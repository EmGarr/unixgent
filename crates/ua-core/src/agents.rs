//! Child agent discovery: read task and summary from descendant agent journals.

use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::journal::JournalEntry;

/// Read the end-of-session Summary from the last line of a child agent's journal.
///
/// Returns `None` if the file doesn't exist, is empty, or the last entry
/// isn't a Summary.
pub fn read_child_summary(path: &Path) -> Option<JournalEntry> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last_line = None;
    for line in reader.lines() {
        let line = line.ok()?;
        if !line.trim().is_empty() {
            last_line = Some(line);
        }
    }
    let last = last_line?;
    let entry: JournalEntry = serde_json::from_str(&last).ok()?;
    if matches!(entry, JournalEntry::Summary { .. }) {
        Some(entry)
    } else {
        None
    }
}

/// Read the task description from the first Instruction entry in a child journal.
///
/// Returns the instruction text, truncated to `max_len` characters.
/// Returns `None` if the file doesn't exist or has no Instruction entry.
pub fn read_child_task(path: &Path, max_len: usize) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(JournalEntry::Instruction { text, .. }) = serde_json::from_str(&line) {
            if text.len() > max_len {
                return Some(format!("{}...", &text[..max_len]));
            }
            return Some(text);
        }
    }
    None
}

/// Format a child agent status line for display.
///
/// For a completed child: `[PID] task  done  stats`
/// For a failed child: `[PID] task  fail  stats`
pub fn format_child_done(
    pid: u32,
    task: &str,
    summary: &JournalEntry,
    style: &crate::style::Style,
) -> String {
    if let JournalEntry::Summary {
        input_tokens,
        output_tokens,
        commands_run,
        exit_code,
        elapsed_secs,
        ..
    } = summary
    {
        let status_label = if *exit_code == 0 { "done" } else { "fail" };
        let status_color = if *exit_code == 0 {
            style.green_start()
        } else {
            style.red_start()
        };
        let tok = crate::style::format_tokens(input_tokens + output_tokens);
        format!(
            "{}[{pid}]{} {task}  {status_color}{status_label}{}  {}{tok} tok  {commands_run} cmds  {elapsed_secs:.0}s{}",
            style.dim_start(),
            style.reset(),
            style.reset(),
            style.dim_start(),
            style.reset(),
        )
    } else {
        format!(
            "{}[{pid}]{} {task}  {}???{}",
            style.dim_start(),
            style.reset(),
            style.yellow_start(),
            style.reset(),
        )
    }
}

/// Format a child agent discovery line.
///
/// `[PID] task ···`
pub fn format_child_started(pid: u32, task: &str, style: &crate::style::Style) -> String {
    format!(
        "{}[{pid}]{} {task}  {}···{}",
        style.dim_start(),
        style.reset(),
        style.dim_start(),
        style.reset(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    #[test]
    fn read_child_summary_with_summary_last() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-123.jsonl");
        let instruction = r#"{"type":"instruction","ts":1,"text":"find TODOs"}"#;
        let summary = r#"{"type":"summary","ts":2,"input_tokens":500,"output_tokens":200,"commands_run":3,"commands_denied":0,"exit_code":0,"elapsed_secs":5.2,"task":"find TODOs"}"#;
        std::fs::write(&path, format!("{instruction}\n{summary}\n")).unwrap();

        let result = read_child_summary(&path);
        assert!(result.is_some());
        if let Some(JournalEntry::Summary {
            input_tokens,
            exit_code,
            ..
        }) = result
        {
            assert_eq!(input_tokens, 500);
            assert_eq!(exit_code, 0);
        } else {
            panic!("Expected Summary");
        }
    }

    #[test]
    fn read_child_summary_no_summary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-123.jsonl");
        let instruction = r#"{"type":"instruction","ts":1,"text":"find TODOs"}"#;
        std::fs::write(&path, format!("{instruction}\n")).unwrap();

        assert!(read_child_summary(&path).is_none());
    }

    #[test]
    fn read_child_summary_missing_file() {
        assert!(read_child_summary(Path::new("/nonexistent/path.jsonl")).is_none());
    }

    #[test]
    fn read_child_summary_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();

        assert!(read_child_summary(&path).is_none());
    }

    #[test]
    fn read_child_task_extracts_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-123.jsonl");
        let instruction =
            r#"{"type":"instruction","ts":1,"text":"find all TODO comments in the codebase"}"#;
        std::fs::write(&path, format!("{instruction}\n")).unwrap();

        let task = read_child_task(&path, 40);
        assert_eq!(
            task.as_deref(),
            Some("find all TODO comments in the codebase")
        );
    }

    #[test]
    fn read_child_task_truncates_long_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-123.jsonl");
        let long_text = "x".repeat(100);
        let instruction = format!(r#"{{"type":"instruction","ts":1,"text":"{long_text}"}}"#);
        std::fs::write(&path, format!("{instruction}\n")).unwrap();

        let task = read_child_task(&path, 40);
        assert!(task.is_some());
        let task = task.unwrap();
        assert_eq!(task.len(), 43); // 40 + "..."
        assert!(task.ends_with("..."));
    }

    #[test]
    fn read_child_task_short_no_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-123.jsonl");
        let instruction = r#"{"type":"instruction","ts":1,"text":"hello"}"#;
        std::fs::write(&path, format!("{instruction}\n")).unwrap();

        let task = read_child_task(&path, 40);
        assert_eq!(task.as_deref(), Some("hello"));
    }

    #[test]
    fn read_child_task_missing_file() {
        assert!(read_child_task(Path::new("/nonexistent/path.jsonl"), 40).is_none());
    }

    #[test]
    fn format_child_done_success() {
        let style = Style::disabled();
        let summary = JournalEntry::Summary {
            ts: 1,
            input_tokens: 500,
            output_tokens: 200,
            commands_run: 3,
            commands_denied: 0,
            exit_code: 0,
            elapsed_secs: 5.0,
            task: "find TODOs".to_string(),
        };
        let line = format_child_done(123, "find TODOs", &summary, &style);
        assert!(line.contains("[123]"));
        assert!(line.contains("find TODOs"));
        assert!(line.contains("done"));
        assert!(line.contains("700"));
        assert!(line.contains("3 cmds"));
    }

    #[test]
    fn format_child_done_failure() {
        let style = Style::disabled();
        let summary = JournalEntry::Summary {
            ts: 1,
            input_tokens: 300,
            output_tokens: 100,
            commands_run: 1,
            commands_denied: 2,
            exit_code: 1,
            elapsed_secs: 3.0,
            task: "broken task".to_string(),
        };
        let line = format_child_done(456, "broken task", &summary, &style);
        assert!(line.contains("[456]"));
        assert!(line.contains("fail"));
    }

    #[test]
    fn format_child_done_no_ansi_when_disabled() {
        let style = Style::disabled();
        let summary = JournalEntry::Summary {
            ts: 1,
            input_tokens: 100,
            output_tokens: 50,
            commands_run: 1,
            commands_denied: 0,
            exit_code: 0,
            elapsed_secs: 1.0,
            task: "test".to_string(),
        };
        let line = format_child_done(1, "test", &summary, &style);
        assert!(!line.contains("\x1b"));
    }

    #[test]
    fn format_child_started_display() {
        let style = Style::disabled();
        let line = format_child_started(789, "run lint", &style);
        assert!(line.contains("[789]"));
        assert!(line.contains("run lint"));
        assert!(line.contains("···"));
    }
}
