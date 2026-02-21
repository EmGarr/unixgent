//! Attachment loading: file validation, MIME detection, base64 encoding.

use std::path::Path;

use base64::Engine;
use ua_protocol::Attachment;

/// Maximum file size in bytes (20 MB — Anthropic's limit).
const MAX_FILE_SIZE: u64 = 20 * 1024 * 1024;

/// Detect media type from magic bytes at the start of raw output.
/// Returns the MIME type if recognized, `None` for text/unknown.
pub fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some("audio/wav");
    }
    None
}

/// Map file extension to MIME type. Returns `None` for unsupported formats.
fn mime_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Load and validate an image attachment from a file path.
pub fn load_attachment(path: &Path) -> Result<Attachment, String> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| format!("no file extension: {}", path.display()))?;

    let media_type = mime_type_for_extension(ext).ok_or_else(|| {
        format!("unsupported image format '.{ext}' (supported: png, jpg, jpeg, gif, webp)")
    })?;

    let metadata =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if metadata.len() > MAX_FILE_SIZE {
        return Err(format!(
            "file too large: {} bytes (max {} MB)",
            metadata.len(),
            MAX_FILE_SIZE / 1024 / 1024
        ));
    }

    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(Attachment {
        filename,
        media_type: media_type.to_string(),
        data: encoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mime_types() {
        assert_eq!(mime_type_for_extension("png"), Some("image/png"));
        assert_eq!(mime_type_for_extension("PNG"), Some("image/png"));
        assert_eq!(mime_type_for_extension("jpg"), Some("image/jpeg"));
        assert_eq!(mime_type_for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(mime_type_for_extension("gif"), Some("image/gif"));
        assert_eq!(mime_type_for_extension("webp"), Some("image/webp"));
        assert_eq!(mime_type_for_extension("bmp"), None);
        assert_eq!(mime_type_for_extension("txt"), None);
    }

    #[test]
    fn load_valid_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"\x89PNG\r\n\x1a\n").unwrap();

        let att = load_attachment(&path).unwrap();
        assert_eq!(att.filename, "test.png");
        assert_eq!(att.media_type, "image/png");
        assert!(!att.data.is_empty());
    }

    #[test]
    fn load_valid_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        for ext in &["jpg", "jpeg"] {
            let path = dir.path().join(format!("test.{ext}"));
            std::fs::write(&path, b"fake jpeg data").unwrap();
            let att = load_attachment(&path).unwrap();
            assert_eq!(att.media_type, "image/jpeg");
        }
    }

    #[test]
    fn load_missing_file() {
        let result = load_attachment(Path::new("/nonexistent/image.png"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("file not found"));
    }

    #[test]
    fn load_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, b"pdf data").unwrap();
        let result = load_attachment(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported image format"));
    }

    #[test]
    fn load_no_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noext");
        std::fs::write(&path, b"data").unwrap();
        let result = load_attachment(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no file extension"));
    }

    #[test]
    fn base64_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("round.png");
        let original = b"hello world image data";
        std::fs::write(&path, original).unwrap();

        let att = load_attachment(&path).unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&att.data)
            .unwrap();
        assert_eq!(decoded, original);
    }

    // --- detect_media_type tests ---

    #[test]
    fn detect_png() {
        assert_eq!(
            detect_media_type(b"\x89PNG\r\n\x1a\nmore data"),
            Some("image/png")
        );
    }

    #[test]
    fn detect_jpeg() {
        assert_eq!(
            detect_media_type(b"\xff\xd8\xff\xe0more data"),
            Some("image/jpeg")
        );
    }

    #[test]
    fn detect_gif87a() {
        assert_eq!(detect_media_type(b"GIF87amore data"), Some("image/gif"));
    }

    #[test]
    fn detect_gif89a() {
        assert_eq!(detect_media_type(b"GIF89amore data"), Some("image/gif"));
    }

    #[test]
    fn detect_webp() {
        assert_eq!(
            detect_media_type(b"RIFF\x00\x00\x00\x00WEBPmore"),
            Some("image/webp")
        );
    }

    #[test]
    fn detect_wav() {
        assert_eq!(
            detect_media_type(b"RIFF\x00\x00\x00\x00WAVEmore"),
            Some("audio/wav")
        );
    }

    #[test]
    fn detect_text_returns_none() {
        assert_eq!(detect_media_type(b"hello world\n"), None);
    }

    #[test]
    fn detect_empty_returns_none() {
        assert_eq!(detect_media_type(b""), None);
    }

    #[test]
    fn detect_short_bytes_returns_none() {
        assert_eq!(detect_media_type(b"RI"), None);
    }
}
