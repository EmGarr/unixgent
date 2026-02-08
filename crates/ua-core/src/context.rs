//! Context capture and management for agent requests.
//!
//! Handles terminal output history, ANSI escape sequence stripping,
//! and building agent requests with appropriate context.

use std::collections::VecDeque;

use ua_protocol::{AgentRequest, ConversationMessage, ShellContext, TerminalHistory};

use crate::config::{Config, ContextConfig};

/// Ring buffer for terminal output history.
pub struct OutputHistory {
    /// Lines of output (oldest first).
    lines: VecDeque<String>,
    /// Current line being accumulated (not yet complete).
    current_line: String,
    /// Maximum number of lines to keep.
    max_lines: usize,
    /// State for ANSI escape sequence parsing.
    escape_state: EscapeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    Ground,
    Escape,
    Csi,
    Osc,
}

impl OutputHistory {
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_lines),
            current_line: String::new(),
            max_lines,
            escape_state: EscapeState::Ground,
        }
    }

    /// Feed raw bytes from terminal output.
    /// Strips ANSI escape sequences and accumulates lines.
    pub fn feed(&mut self, data: &[u8]) {
        for &byte in data {
            match self.escape_state {
                EscapeState::Ground => {
                    if byte == 0x1b {
                        self.escape_state = EscapeState::Escape;
                    } else if byte == b'\n' {
                        self.push_line();
                    } else if byte == b'\r' {
                        // Ignore carriage returns
                    } else if (0x20..0x7f).contains(&byte) {
                        self.current_line.push(byte as char);
                    }
                    // Ignore other control characters
                }
                EscapeState::Escape => match byte {
                    b'[' => self.escape_state = EscapeState::Csi,
                    b']' => self.escape_state = EscapeState::Osc,
                    _ => self.escape_state = EscapeState::Ground,
                },
                EscapeState::Csi => {
                    // CSI sequence ends with a letter (0x40-0x7E)
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape_state = EscapeState::Ground;
                    }
                    // Otherwise continue accumulating CSI parameters
                }
                EscapeState::Osc => {
                    // OSC sequence ends with BEL (0x07) or ST (ESC \)
                    if byte == 0x07 {
                        self.escape_state = EscapeState::Ground;
                    } else if byte == 0x1b {
                        // Could be start of ST, but we'll handle ESC in next iteration
                        self.escape_state = EscapeState::Escape;
                    }
                }
            }
        }
    }

    fn push_line(&mut self) {
        let line = std::mem::take(&mut self.current_line);
        // Trim trailing whitespace
        let trimmed = line.trim_end().to_string();
        self.lines.push_back(trimmed);

        // Evict oldest if over capacity
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }

    /// Get all lines as a vector.
    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    /// Approximate token count (chars / 4).
    pub fn approx_tokens(&self) -> usize {
        let total_chars: usize = self.lines.iter().map(|l| l.len()).sum();
        total_chars / 4
    }

    /// Trim oldest lines until under the given token limit.
    pub fn trim_to_tokens(&mut self, max_tokens: usize) {
        while self.approx_tokens() > max_tokens && !self.lines.is_empty() {
            self.lines.pop_front();
        }
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.current_line.clear();
    }
}

/// Build a ShellContext from the current environment.
pub fn build_shell_context(config: &Config, terminal_size: (u16, u16)) -> ShellContext {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let shell = config.shell_command();

    let env_vars = collect_env_vars(&config.context);

    ShellContext {
        cwd,
        shell,
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        env_vars,
        terminal_size,
    }
}

/// Collect filtered environment variables based on config.
fn collect_env_vars(config: &ContextConfig) -> Vec<(String, String)> {
    let sensitive_suffixes = ["_KEY", "_SECRET", "_TOKEN", "_PASSWORD", "_CREDENTIALS"];

    config
        .include_env
        .iter()
        .filter_map(|name| {
            // Skip if variable name ends with a sensitive suffix
            let upper = name.to_uppercase();
            if sensitive_suffixes.iter().any(|s| upper.ends_with(s)) {
                return None;
            }

            std::env::var(name).ok().map(|value| {
                // Also skip if the value looks like a secret (long base64-ish string)
                if looks_like_secret(&value) {
                    None
                } else {
                    Some((name.clone(), value))
                }
            })?
        })
        .collect()
}

/// Heuristic to detect if a value looks like a secret.
fn looks_like_secret(value: &str) -> bool {
    // Skip if it's a very long string with no spaces (likely a key/token)
    if value.len() > 100 && !value.contains(' ') {
        return true;
    }
    // Skip if it starts with common secret prefixes
    if value.starts_with("sk-") || value.starts_with("pk-") || value.starts_with("ghp_") {
        return true;
    }
    false
}

/// Build an AgentRequest from the current state.
pub fn build_agent_request(
    instruction: &str,
    config: &Config,
    history: &OutputHistory,
    conversation: Vec<ConversationMessage>,
    terminal_size: (u16, u16),
) -> AgentRequest {
    let context = build_shell_context(config, terminal_size);
    let terminal_history = TerminalHistory::from_lines(history.lines());

    AgentRequest {
        instruction: instruction.to_string(),
        context,
        terminal_history,
        conversation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_history_feed_simple() {
        let mut history = OutputHistory::new(100);
        history.feed(b"hello\nworld\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn output_history_strips_ansi_csi() {
        let mut history = OutputHistory::new(100);
        // CSI sequence: ESC [ 31 m (red text)
        history.feed(b"\x1b[31mred text\x1b[0m\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["red text"]);
    }

    #[test]
    fn output_history_strips_ansi_osc() {
        let mut history = OutputHistory::new(100);
        // OSC sequence: ESC ] 0 ; title BEL
        history.feed(b"\x1b]0;window title\x07normal text\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["normal text"]);
    }

    #[test]
    fn output_history_strips_osc_133() {
        let mut history = OutputHistory::new(100);
        // OSC 133;A (prompt start)
        history.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["$"]);
    }

    #[test]
    fn output_history_ring_buffer() {
        let mut history = OutputHistory::new(3);
        history.feed(b"line1\nline2\nline3\nline4\nline5\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["line3", "line4", "line5"]);
    }

    #[test]
    fn output_history_approx_tokens() {
        let mut history = OutputHistory::new(100);
        // 40 chars = 10 tokens
        history.feed(b"1234567890123456789012345678901234567890\n");

        assert_eq!(history.approx_tokens(), 10);
    }

    #[test]
    fn output_history_trim_to_tokens() {
        let mut history = OutputHistory::new(100);
        // Each line is 10 chars = 2.5 tokens
        history.feed(b"1234567890\n1234567890\n1234567890\n1234567890\n");

        assert_eq!(history.lines().len(), 4);

        // Trim to 5 tokens (should keep about 2 lines)
        history.trim_to_tokens(5);
        assert!(history.approx_tokens() <= 5);
    }

    #[test]
    fn output_history_handles_incomplete_line() {
        let mut history = OutputHistory::new(100);
        history.feed(b"complete line\nincomplete");

        let lines = history.lines();
        // Incomplete line should not be included
        assert_eq!(lines, vec!["complete line"]);
    }

    #[test]
    fn output_history_handles_crlf() {
        let mut history = OutputHistory::new(100);
        history.feed(b"line1\r\nline2\r\n");

        let lines = history.lines();
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn output_history_clear() {
        let mut history = OutputHistory::new(100);
        history.feed(b"line1\nline2\n");
        history.clear();

        assert!(history.lines().is_empty());
    }

    #[test]
    fn looks_like_secret_detects_api_keys() {
        assert!(looks_like_secret("sk-ant-api03-xxxxx"));
        assert!(looks_like_secret(
            "ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        ));
        assert!(!looks_like_secret("/usr/local/bin"));
        assert!(!looks_like_secret("xterm-256color"));
    }

    #[test]
    fn collect_env_vars_filters_sensitive() {
        let config = ContextConfig {
            max_terminal_lines: 100,
            max_conversation_turns: 20,
            include_env: vec![
                "PATH".to_string(),
                "HOME".to_string(),
                "API_KEY".to_string(), // Should be filtered
            ],
        };

        let vars = collect_env_vars(&config);

        // API_KEY should be filtered out
        assert!(!vars.iter().any(|(k, _)| k == "API_KEY"));
    }

    #[test]
    fn build_shell_context_basic() {
        let config = Config::default();
        let ctx = build_shell_context(&config, (80, 24));

        assert!(!ctx.cwd.is_empty());
        assert!(!ctx.shell.is_empty());
        assert!(!ctx.platform.is_empty());
        assert!(!ctx.arch.is_empty());
        assert_eq!(ctx.terminal_size, (80, 24));
    }

    #[test]
    fn build_agent_request_basic() {
        let config = Config::default();
        let mut history = OutputHistory::new(100);
        history.feed(b"$ ls\nfile.txt\n");

        let request =
            build_agent_request("what files are here", &config, &history, vec![], (80, 24));

        assert_eq!(request.instruction, "what files are here");
        assert_eq!(request.terminal_history.lines, vec!["$ ls", "file.txt"]);
    }
}
