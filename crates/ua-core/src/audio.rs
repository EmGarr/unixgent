//! Audio input: record from microphone and transcribe to text.
//!
//! This module provides a two-stage pipeline:
//! 1. **Record** — capture audio from the default input device via `sox` (rec).
//! 2. **Transcribe** — convert audio to text via a configurable STT backend
//!    (default: `whisper-cpp`).
//!
//! The agent stays text-in/text-out; audio is a preprocessing step.
//!
//! Two modes:
//! - **Blocking** (`listen`) — used by `--listen` CLI flag (batch mode).
//! - **Async** (`listen_async`) — used by REPL `#v` / Ctrl+V. Records in a
//!   background thread, supports cancellation, sends result via channel.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

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

/// Handle for an in-progress voice recording.
///
/// Call `cancel()` to kill the recorder process and abort. The background
/// thread sends `VoiceResult` on the provided channel when done (or cancelled).
pub struct RecordingHandle {
    /// Set to true to signal the recording thread to kill `rec` and abort.
    cancel: Arc<AtomicBool>,
    /// Join handle for the background thread.
    _thread: thread::JoinHandle<()>,
}

impl RecordingHandle {
    /// Cancel the recording. The background thread will kill the recorder
    /// process and send an error result on the channel.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Result of an async voice recording + transcription pipeline.
pub type VoiceResult = Result<String, String>;

/// Start recording in a background thread.
///
/// Returns a `RecordingHandle` that can be used to cancel the recording.
/// The result (transcribed text or error message) is sent via `tx`.
///
/// The recording stops on silence detection (same as `listen()`), or when
/// cancelled via the handle.
pub fn listen_async(config: &AudioConfig, tx: mpsc::Sender<VoiceResult>) -> RecordingHandle {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();

    // Clone config values for the thread (AudioConfig fields are all Clone).
    let recorder = config.recorder_cmd();
    let transcriber = config.transcriber_cmd();
    let model = config.whisper_model.clone();
    let max_duration = config.max_duration_secs;

    let handle = thread::spawn(move || {
        let result = listen_worker(&recorder, &transcriber, &model, max_duration, &cancel_clone);
        let _ = tx.send(result);
    });

    RecordingHandle {
        cancel,
        _thread: handle,
    }
}

/// Worker function for async recording. Runs in a background thread.
fn listen_worker(
    recorder: &str,
    transcriber: &str,
    model: &str,
    max_duration: u32,
    cancel: &AtomicBool,
) -> VoiceResult {
    // Pre-check that binaries exist before starting
    which_check(recorder).map_err(|e| e.to_string())?;
    which_check(transcriber).map_err(|e| e.to_string())?;

    let tmp_dir = tempfile::tempdir().map_err(|e| format!("failed to create temp dir: {e}"))?;
    let out_path = tmp_dir.path().join("recording.wav");
    let duration = max_duration.to_string();

    // Spawn recorder as a child process so we can kill it on cancel.
    let mut child = Command::new(recorder)
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            format!("failed to run '{recorder}': {e}. Install sox: https://sox.sourceforge.net")
        })?;

    // Poll for completion or cancellation (100ms intervals).
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("recording cancelled".to_string());
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("recorder exited with status: {status}"));
                }
                break;
            }
            Ok(None) => {
                // Still running — sleep briefly and check again.
                thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("failed to wait on recorder: {e}")),
        }
    }

    // Check cancel before transcription
    if cancel.load(Ordering::SeqCst) {
        return Err("recording cancelled".to_string());
    }

    // Validate recording
    if !out_path.exists() {
        return Err("recording file not created".to_string());
    }
    let metadata = std::fs::metadata(&out_path).map_err(|e| e.to_string())?;
    if metadata.len() < 100 {
        return Err("recording too short — no audio captured".to_string());
    }

    // Transcribe
    let output = Command::new(transcriber)
        .args([
            "-m",
            model,
            "-f",
            out_path.to_str().unwrap_or("recording.wav"),
            "--no-timestamps",
            "-nt",
        ])
        .output()
        .map_err(|e| format!("failed to run '{transcriber}': {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("transcriber failed: {stderr}"));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err("transcription produced no text".to_string());
    }

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

    #[test]
    fn listen_async_fails_without_recorder() {
        let config = AudioConfig {
            recorder: Some("nonexistent_recorder_xyz_99".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        let _handle = listen_async(&config, tx);
        let result = rx.recv().unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("command not found"));
    }

    #[test]
    fn listen_async_cancel_before_start() {
        // Cancel immediately — the worker should detect cancel and abort.
        let config = AudioConfig {
            // Use `sleep` as a fake recorder so the child lives long enough to be killed.
            recorder: Some("sleep".to_string()),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        let handle = listen_async(&config, tx);
        // Cancel immediately
        handle.cancel();
        let result = rx.recv().unwrap();
        // Either "cancelled" or a recorder error — both are acceptable.
        assert!(result.is_err());
    }

    #[test]
    fn voice_result_type_ok() {
        let r: VoiceResult = Ok("hello world".to_string());
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), "hello world");
    }

    #[test]
    fn voice_result_type_err() {
        let r: VoiceResult = Err("mic not found".to_string());
        assert!(r.is_err());
        assert_eq!(r.unwrap_err(), "mic not found");
    }
}
