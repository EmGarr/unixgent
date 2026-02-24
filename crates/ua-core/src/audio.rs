//! Audio input: record from microphone and transcribe to text.
//!
//! This module provides a two-stage pipeline:
//! 1. **Record** — capture audio from the default input device via `sox` (rec).
//! 2. **Transcribe** — convert audio to text via a configurable STT backend
//!    (default: `whisper-cpp`).
//!
//! The agent stays text-in/text-out; audio is a preprocessing step.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::AudioConfig;

/// Record audio from the default input device.
///
/// Uses `sox`'s `rec` command to capture a WAV file. The recording stops
/// after `duration_secs` seconds, or when silence is detected (2s of silence
/// below 1% volume triggers stop).
///
/// Returns the path to the recorded WAV file.
pub fn record(config: &AudioConfig, out_dir: &Path) -> io::Result<PathBuf> {
    let out_path = out_dir.join("recording.wav");

    // Check that the recorder binary exists
    let recorder = config.recorder_cmd();
    which_check(&recorder)?;

    let duration = config.max_duration_secs.to_string();

    // rec -q out.wav rate 16000 channels 1 trim 0 <duration> silence 1 0.1 1% 1 2.0 1%
    // -q: quiet (no progress)
    // rate 16000: 16kHz sample rate (optimal for speech)
    // channels 1: mono
    // trim 0 <duration>: max recording length
    // silence ...: stop on 2s of silence below 1%
    let status = Command::new(&recorder)
        .args([
            "-q",
            out_path.to_str().unwrap_or("recording.wav"),
            "rate",
            "16000",
            "channels",
            "1",
            "trim",
            "0",
            &duration,
            "silence",
            "1",
            "0.1",
            "1%",
            "1",
            "2.0",
            "1%",
        ])
        .status()
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to run '{}': {e}. Install sox: https://sox.sourceforge.net",
                    recorder
                ),
            )
        })?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "recorder exited with status: {status}"
        )));
    }

    if !out_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "recording file not created",
        ));
    }

    // Check file has actual content (not just a WAV header)
    let metadata = std::fs::metadata(&out_path)?;
    if metadata.len() < 100 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recording too short — no audio captured",
        ));
    }

    Ok(out_path)
}

/// Transcribe an audio file to text using the configured STT backend.
///
/// Default backend is `whisper-cpp` (the CLI from whisper.cpp project).
/// The transcriber command is run with the audio file path appended.
///
/// Returns the transcribed text (trimmed).
pub fn transcribe(config: &AudioConfig, audio_path: &Path) -> io::Result<String> {
    let transcriber = config.transcriber_cmd();
    let model = &config.whisper_model;

    which_check(&transcriber)?;

    // whisper-cpp -m <model> -f <audio> --no-timestamps -nt
    // Output goes to stdout as plain text.
    let output = Command::new(&transcriber)
        .args([
            "-m",
            model,
            "-f",
            audio_path.to_str().unwrap_or("recording.wav"),
            "--no-timestamps",
            "-nt",
        ])
        .output()
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "failed to run '{}': {e}. Install whisper.cpp: https://github.com/ggerganov/whisper.cpp",
                    transcriber
                ),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!("transcriber failed: {stderr}")));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if text.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transcription produced no text",
        ));
    }

    Ok(text)
}

/// Record from microphone and transcribe to text in one step.
///
/// Creates a temp directory, records audio, transcribes it, and returns
/// the resulting text instruction.
pub fn listen(config: &AudioConfig) -> io::Result<String> {
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| io::Error::new(e.kind(), format!("failed to create temp dir: {e}")))?;

    eprintln!("[ua:audio] listening... (speak now, silence stops recording)");
    let audio_path = record(config, tmp_dir.path())?;

    eprintln!("[ua:audio] transcribing...");
    let text = transcribe(config, &audio_path)?;

    eprintln!("[ua:audio] heard: \"{}\"", truncate_display(&text, 80));
    Ok(text)
}

/// Check that a command exists on PATH.
fn which_check(cmd: &str) -> io::Result<()> {
    let status = Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("command not found: '{cmd}'"),
        )),
    }
}

/// Truncate a string for display.
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() > max {
        let mut t: String = s.chars().take(max - 3).collect();
        t.push_str("...");
        t
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_display_short() {
        assert_eq!(truncate_display("hello", 10), "hello");
    }

    #[test]
    fn truncate_display_long() {
        let long = "a".repeat(100);
        let result = truncate_display(&long, 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_display_exact() {
        let s = "abcde";
        assert_eq!(truncate_display(s, 5), "abcde");
    }

    #[test]
    fn which_check_finds_sh() {
        // /bin/sh should exist on all Unix systems
        assert!(which_check("sh").is_ok());
    }

    #[test]
    fn which_check_missing_command() {
        assert!(which_check("nonexistent_command_xyz_12345").is_err());
    }

    #[test]
    fn record_fails_without_sox() {
        let config = AudioConfig {
            recorder: Some("nonexistent_recorder_xyz".to_string()),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let result = record(&config, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("command not found"));
    }

    #[test]
    fn transcribe_fails_without_whisper() {
        let config = AudioConfig {
            transcriber: Some("nonexistent_transcriber_xyz".to_string()),
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let audio_path = dir.path().join("test.wav");
        std::fs::write(&audio_path, b"fake audio data").unwrap();
        let result = transcribe(&config, &audio_path);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("command not found"));
    }

    #[test]
    fn audio_config_defaults() {
        let config = AudioConfig::default();
        assert_eq!(config.recorder_cmd(), "rec");
        assert_eq!(config.transcriber_cmd(), "whisper-cpp");
        assert_eq!(config.max_duration_secs, 30);
    }
}
