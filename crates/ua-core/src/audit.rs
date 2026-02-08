//! Append-only JSONL audit logger for command execution events.
//!
//! Writes one JSON object per line to a log file, recording proposed commands,
//! approvals, denials, blocks, and executions.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Append-only JSONL audit logger.
pub struct AuditLogger {
    writer: Option<BufWriter<File>>,
    session_id: String,
}

impl AuditLogger {
    /// Create a new audit logger that writes to the given path.
    /// Creates parent directories if they don't exist.
    pub fn new(path: &PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self {
            writer: Some(BufWriter::new(file)),
            session_id: generate_session_id(),
        })
    }

    /// Create a no-op logger that discards all events.
    pub fn noop() -> Self {
        Self {
            writer: None,
            session_id: generate_session_id(),
        }
    }

    /// Log a proposed command set from the LLM.
    pub fn log_proposed(
        &mut self,
        iteration: usize,
        commands: &[String],
        risk_levels: &[&str],
        source: &str,
    ) {
        self.write_event(serde_json::json!({
            "ts": epoch_secs(),
            "session": self.session_id,
            "type": "proposed",
            "iteration": iteration,
            "commands": commands,
            "risk_levels": risk_levels,
            "source": source,
        }));
    }

    /// Log that a command set was approved.
    pub fn log_approved(&mut self, iteration: usize, method: &str, reason: &str) {
        self.write_event(serde_json::json!({
            "ts": epoch_secs(),
            "session": self.session_id,
            "type": "approved",
            "iteration": iteration,
            "method": method,
            "reason": reason,
        }));
    }

    /// Log that a command set was denied by the user.
    pub fn log_denied(&mut self, iteration: usize, method: &str, reason: &str) {
        self.write_event(serde_json::json!({
            "ts": epoch_secs(),
            "session": self.session_id,
            "type": "denied",
            "iteration": iteration,
            "method": method,
            "reason": reason,
        }));
    }

    /// Log that a command was blocked by the policy engine.
    pub fn log_blocked(&mut self, command: &str, risk_level: &str, reason: &str) {
        self.write_event(serde_json::json!({
            "ts": epoch_secs(),
            "session": self.session_id,
            "type": "blocked",
            "command": command,
            "risk_level": risk_level,
            "reason": reason,
        }));
    }

    /// Log a command execution result.
    pub fn log_executed(&mut self, command: &str, exit_code: Option<i32>, duration_ms: u64) {
        self.write_event(serde_json::json!({
            "ts": epoch_secs(),
            "session": self.session_id,
            "type": "executed",
            "command": command,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
        }));
    }

    fn write_event(&mut self, value: serde_json::Value) {
        if let Some(ref mut writer) = self.writer {
            if let Ok(line) = serde_json::to_string(&value) {
                let _ = writeln!(writer, "{line}");
                let _ = writer.flush();
            }
        }
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn generate_session_id() -> String {
    let pid = std::process::id();
    let ts = epoch_secs();
    format!("s{:x}", pid ^ (ts as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_log_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
        let content = std::fs::read_to_string(path).unwrap();
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn new_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("dir").join("audit.jsonl");
        let _logger = AuditLogger::new(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn noop_logger_discards() {
        let mut logger = AuditLogger::noop();
        logger.log_proposed(0, &["ls".to_string()], &["read_only"], "llm");
        // No panic, no output — just works
    }

    #[test]
    fn log_proposed_writes_valid_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_proposed(
            0,
            &["ls /tmp".to_string(), "cat file".to_string()],
            &["read_only", "read_only"],
            "llm",
        );

        let lines = read_log_lines(&path);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["type"], "proposed");
        assert_eq!(lines[0]["iteration"], 0);
        assert_eq!(lines[0]["commands"][0], "ls /tmp");
        assert_eq!(lines[0]["source"], "llm");
    }

    #[test]
    fn log_approved_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_approved(1, "keystroke", "user pressed y");

        let lines = read_log_lines(&path);
        assert_eq!(lines[0]["type"], "approved");
        assert_eq!(lines[0]["method"], "keystroke");
    }

    #[test]
    fn log_denied_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_denied(0, "keystroke", "user pressed n");

        let lines = read_log_lines(&path);
        assert_eq!(lines[0]["type"], "denied");
        assert_eq!(lines[0]["reason"], "user pressed n");
    }

    #[test]
    fn log_blocked_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_blocked("rm -rf /", "denied", "denied by policy");

        let lines = read_log_lines(&path);
        assert_eq!(lines[0]["type"], "blocked");
        assert_eq!(lines[0]["command"], "rm -rf /");
        assert_eq!(lines[0]["risk_level"], "denied");
    }

    #[test]
    fn log_executed_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_executed("ls /tmp", Some(0), 42);

        let lines = read_log_lines(&path);
        assert_eq!(lines[0]["type"], "executed");
        assert_eq!(lines[0]["command"], "ls /tmp");
        assert_eq!(lines[0]["exit_code"], 0);
        assert_eq!(lines[0]["duration_ms"], 42);
    }

    #[test]
    fn multiple_entries_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_proposed(0, &["ls".to_string()], &["read_only"], "llm");
        logger.log_approved(0, "keystroke", "y");
        logger.log_executed("ls", Some(0), 10);

        let lines = read_log_lines(&path);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "proposed");
        assert_eq!(lines[1]["type"], "approved");
        assert_eq!(lines[2]["type"], "executed");
    }

    #[test]
    fn session_id_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_proposed(0, &["ls".to_string()], &["read_only"], "llm");
        logger.log_approved(0, "keystroke", "y");

        let lines = read_log_lines(&path);
        assert_eq!(lines[0]["session"], lines[1]["session"]);
    }

    #[test]
    fn timestamp_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_proposed(0, &["ls".to_string()], &["read_only"], "llm");

        let lines = read_log_lines(&path);
        assert!(lines[0]["ts"].is_u64());
        assert!(lines[0]["ts"].as_u64().unwrap() > 0);
    }

    #[test]
    fn log_executed_null_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let mut logger = AuditLogger::new(&path).unwrap();

        logger.log_executed("ls", None, 10);

        let lines = read_log_lines(&path);
        assert!(lines[0]["exit_code"].is_null());
    }
}
