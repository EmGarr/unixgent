//! Append-only session journal backed by a JSONL file.
//!
//! Each session writes one JSON object per line, recording all events:
//! user shell commands, LLM instructions, responses, tool results, and
//! policy blocks. The journal replaces in-memory conversation accumulation
//! and reactive compaction — each LLM call rebuilds context fresh from the
//! journal, trimmed to a token budget.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ua_protocol::{ConversationMessage, ToolResultRecord, ToolUseRecord};

// ---------------------------------------------------------------------------
// Shared utilities (also used by audit.rs)
// ---------------------------------------------------------------------------

/// Seconds since Unix epoch.
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a short session ID from PID and timestamp.
pub fn generate_session_id() -> String {
    let pid = std::process::id();
    let ts = epoch_secs();
    format!("s{:x}", pid ^ (ts as u32))
}

// ---------------------------------------------------------------------------
// JournalEntry — serde-tagged JSONL format
// ---------------------------------------------------------------------------

/// A single entry in the session journal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum JournalEntry {
    /// User typed a command at the shell prompt (no `#` prefix).
    #[serde(rename = "shell_command")]
    ShellCommand {
        ts: u64,
        command: String,
        exit_code: Option<i32>,
    },
    /// User typed `# instruction` for the LLM.
    #[serde(rename = "instruction")]
    Instruction { ts: u64, text: String },
    /// LLM response (text + optional tool_use blocks).
    #[serde(rename = "response")]
    Response {
        ts: u64,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_uses: Vec<ToolUseRecord>,
    },
    /// Command execution output fed back to LLM.
    #[serde(rename = "tool_result")]
    ToolResult {
        ts: u64,
        results: Vec<ToolResultRecord>,
    },
    /// Policy-blocked commands (denial message).
    #[serde(rename = "blocked")]
    Blocked {
        ts: u64,
        results: Vec<ToolResultRecord>,
    },
    /// Summary checkpoint for very long sessions.
    #[serde(rename = "checkpoint")]
    Checkpoint { ts: u64, summary: String },
}

// ---------------------------------------------------------------------------
// SessionJournal — append-only JSONL file
// ---------------------------------------------------------------------------

/// Append-only session journal backed by a JSONL file.
pub struct SessionJournal {
    writer: BufWriter<File>,
    path: PathBuf,
}

impl SessionJournal {
    /// Create/open a JSONL journal file. Creates parent directories.
    pub fn new(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            path,
        })
    }

    /// Append one entry, flush immediately.
    pub fn append(&mut self, entry: &JournalEntry) {
        if let Ok(line) = serde_json::to_string(entry) {
            let _ = writeln!(self.writer, "{line}");
            let _ = self.writer.flush();
        }
    }

    /// Read all entries from the journal file.
    pub fn read_all(&self) -> Vec<JournalEntry> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let reader = BufReader::new(file);
        reader
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                if line.trim().is_empty() {
                    return None;
                }
                serde_json::from_str(&line).ok()
            })
            .collect()
    }

    /// Get the journal file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Context builder — journal entries → ConversationMessage
// ---------------------------------------------------------------------------

/// Approximate token count for a string (chars / 4).
fn approx_tokens(s: &str) -> usize {
    s.len() / 4
}

/// Approximate token cost of a single journal entry.
fn entry_tokens(entry: &JournalEntry) -> usize {
    match entry {
        JournalEntry::ShellCommand { command, .. } => approx_tokens(command) + 10,
        JournalEntry::Instruction { text, .. } => approx_tokens(text),
        JournalEntry::Response {
            text, tool_uses, ..
        } => {
            let mut t = approx_tokens(text);
            for tu in tool_uses {
                t += approx_tokens(&tu.name) + approx_tokens(&tu.input_json);
            }
            t
        }
        JournalEntry::ToolResult { results, .. } | JournalEntry::Blocked { results, .. } => {
            results.iter().map(|r| approx_tokens(&r.content)).sum()
        }
        JournalEntry::Checkpoint { summary, .. } => approx_tokens(summary),
    }
}

/// Build a conversation from journal entries, respecting a token budget.
///
/// 1. Find most recent `Checkpoint` (if any), start from there
/// 2. Walk backward from end, accumulate token cost until budget exhausted
/// 3. Convert included entries to `Vec<ConversationMessage>`
/// 4. Merge consecutive user-role entries for API alternation
pub fn build_conversation_from_journal(
    entries: &[JournalEntry],
    budget_tokens: usize,
) -> Vec<ConversationMessage> {
    if entries.is_empty() {
        return Vec::new();
    }

    // Find most recent Checkpoint
    let start_idx = entries
        .iter()
        .rposition(|e| matches!(e, JournalEntry::Checkpoint { .. }))
        .unwrap_or(0);
    let relevant = &entries[start_idx..];

    // Walk backward from end, include entries while budget allows.
    // Always include at least the last entry.
    let mut include_from = relevant.len();
    let mut tokens_used = 0;
    for (i, entry) in relevant.iter().enumerate().rev() {
        let cost = entry_tokens(entry);
        if tokens_used + cost > budget_tokens && include_from < relevant.len() {
            break;
        }
        tokens_used += cost;
        include_from = i;
    }

    let included = &relevant[include_from..];
    convert_entries_to_messages(included)
}

/// Convert journal entries to ConversationMessages with strict user/assistant alternation.
pub fn convert_entries_to_messages(entries: &[JournalEntry]) -> Vec<ConversationMessage> {
    let mut messages: Vec<ConversationMessage> = Vec::new();

    for entry in entries {
        match entry {
            JournalEntry::ShellCommand {
                command, exit_code, ..
            } => {
                let exit_str = match exit_code {
                    Some(code) => format!("exit {code}"),
                    None => "unknown exit".to_string(),
                };
                let text = format!("[ran: {command} -> {exit_str}]");
                merge_or_push_user(&mut messages, text, Vec::new());
            }
            JournalEntry::Instruction { text, .. } => {
                if !text.is_empty() {
                    merge_or_push_user(&mut messages, text.clone(), Vec::new());
                }
            }
            JournalEntry::Response {
                text, tool_uses, ..
            } => {
                if !tool_uses.is_empty() {
                    messages.push(ConversationMessage::assistant_with_tool_use(
                        text,
                        tool_uses.clone(),
                    ));
                } else if !text.is_empty() {
                    messages.push(ConversationMessage::assistant(text));
                }
            }
            JournalEntry::ToolResult { results, .. } | JournalEntry::Blocked { results, .. } => {
                merge_or_push_user(&mut messages, String::new(), results.clone());
            }
            JournalEntry::Checkpoint { summary, .. } => {
                let text = format!("Previous context summary: {summary}");
                merge_or_push_user(&mut messages, text, Vec::new());
            }
        }
    }

    // If conversation starts with assistant message, prepend synthetic user message
    if let Some(first) = messages.first() {
        if first.role == ua_protocol::Role::Assistant {
            messages.insert(0, ConversationMessage::user("[session continues]"));
        }
    }

    messages
}

/// Merge user-role content into the last message if it's also user-role,
/// otherwise push a new user message. Maintains API alternation.
fn merge_or_push_user(
    messages: &mut Vec<ConversationMessage>,
    text: String,
    tool_results: Vec<ToolResultRecord>,
) {
    if let Some(last) = messages.last_mut() {
        if last.role == ua_protocol::Role::User {
            if !text.is_empty() {
                if !last.content.is_empty() {
                    last.content.push('\n');
                }
                last.content.push_str(&text);
            }
            last.tool_results.extend(tool_results);
            return;
        }
    }
    let mut msg = ConversationMessage::user(&text);
    msg.tool_results = tool_results;
    messages.push(msg);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Serde roundtrip tests ---

    #[test]
    fn serde_roundtrip_shell_command() {
        let entry = JournalEntry::ShellCommand {
            ts: 1000,
            command: "ls -la".to_string(),
            exit_code: Some(0),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_instruction() {
        let entry = JournalEntry::Instruction {
            ts: 1000,
            text: "what files are here".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_response_with_tool_uses() {
        let entry = JournalEntry::Response {
            ts: 1000,
            text: "Let me check.".to_string(),
            tool_uses: vec![ToolUseRecord {
                id: "toolu_1".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"ls"}"#.to_string(),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_response_without_tool_uses() {
        let entry = JournalEntry::Response {
            ts: 1000,
            text: "The answer is 42.".to_string(),
            tool_uses: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(
            !json.contains("tool_uses"),
            "empty tool_uses should be skipped"
        );
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_tool_result() {
        let entry = JournalEntry::ToolResult {
            ts: 1000,
            results: vec![ToolResultRecord {
                tool_use_id: "toolu_1".to_string(),
                content: "file.txt\n".to_string(),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_blocked() {
        let entry = JournalEntry::Blocked {
            ts: 1000,
            results: vec![ToolResultRecord {
                tool_use_id: "toolu_1".to_string(),
                content: "Command blocked".to_string(),
            }],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn serde_roundtrip_checkpoint() {
        let entry = JournalEntry::Checkpoint {
            ts: 1000,
            summary: "User ran ls and checked files.".to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JournalEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    // --- SessionJournal tests ---

    #[test]
    fn journal_append_and_read_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut journal = SessionJournal::new(path).unwrap();

        journal.append(&JournalEntry::Instruction {
            ts: 1000,
            text: "hello".to_string(),
        });
        journal.append(&JournalEntry::Response {
            ts: 1001,
            text: "Hi there!".to_string(),
            tool_uses: vec![],
        });

        let entries = journal.read_all();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], JournalEntry::Instruction { text, .. } if text == "hello"));
        assert!(matches!(&entries[1], JournalEntry::Response { text, .. } if text == "Hi there!"));
    }

    #[test]
    fn journal_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("dir").join("test.jsonl");
        let _journal = SessionJournal::new(path.clone()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn journal_empty_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        let journal = SessionJournal::new(path).unwrap();
        assert!(journal.read_all().is_empty());
    }

    // --- build_conversation_from_journal tests ---

    #[test]
    fn empty_journal_empty_conversation() {
        let msgs = build_conversation_from_journal(&[], 60000);
        assert!(msgs.is_empty());
    }

    #[test]
    fn instruction_response_roundtrip() {
        let entries = vec![
            JournalEntry::Instruction {
                ts: 1,
                text: "what files?".to_string(),
            },
            JournalEntry::Response {
                ts: 2,
                text: "Let me check.".to_string(),
                tool_uses: vec![ToolUseRecord {
                    id: "toolu_1".to_string(),
                    name: "shell".to_string(),
                    input_json: r#"{"command":"ls"}"#.to_string(),
                }],
            },
            JournalEntry::ToolResult {
                ts: 3,
                results: vec![ToolResultRecord {
                    tool_use_id: "toolu_1".to_string(),
                    content: "file.txt\n".to_string(),
                }],
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, ua_protocol::Role::User);
        assert_eq!(msgs[0].content, "what files?");
        assert_eq!(msgs[1].role, ua_protocol::Role::Assistant);
        assert_eq!(msgs[1].tool_uses.len(), 1);
        assert_eq!(msgs[2].role, ua_protocol::Role::User);
        assert_eq!(msgs[2].tool_results.len(), 1);
    }

    #[test]
    fn shell_command_becomes_user_message() {
        let entries = vec![
            JournalEntry::ShellCommand {
                ts: 1,
                command: "ls -la".to_string(),
                exit_code: Some(0),
            },
            JournalEntry::Instruction {
                ts: 2,
                text: "what did I just run?".to_string(),
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        // Consecutive user entries merge into one message
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("[ran: ls -la -> exit 0]"));
        assert!(msgs[0].content.contains("what did I just run?"));
    }

    #[test]
    fn consecutive_user_entries_merged() {
        let entries = vec![
            JournalEntry::ShellCommand {
                ts: 1,
                command: "cd /tmp".to_string(),
                exit_code: Some(0),
            },
            JournalEntry::ShellCommand {
                ts: 2,
                command: "ls".to_string(),
                exit_code: Some(0),
            },
            JournalEntry::Instruction {
                ts: 3,
                text: "explain".to_string(),
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("[ran: cd /tmp -> exit 0]"));
        assert!(msgs[0].content.contains("[ran: ls -> exit 0]"));
        assert!(msgs[0].content.contains("explain"));
    }

    #[test]
    fn checkpoint_starts_context() {
        let entries = vec![
            JournalEntry::Instruction {
                ts: 1,
                text: "old instruction".to_string(),
            },
            JournalEntry::Response {
                ts: 2,
                text: "old response".to_string(),
                tool_uses: vec![],
            },
            JournalEntry::Checkpoint {
                ts: 3,
                summary: "User ran some commands.".to_string(),
            },
            JournalEntry::Instruction {
                ts: 4,
                text: "new instruction".to_string(),
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        // Should start from checkpoint
        assert!(msgs[0].content.contains("Previous context summary"));
        assert!(msgs[0].content.contains("new instruction"));
        // Should NOT include old instruction
        let all_text: String = msgs.iter().map(|m| m.content.clone()).collect();
        assert!(!all_text.contains("old instruction"));
    }

    #[test]
    fn token_budget_truncation() {
        let big_text = "x".repeat(4000); // ~1000 tokens
        let entries = vec![
            JournalEntry::Instruction {
                ts: 1,
                text: big_text.clone(),
            },
            JournalEntry::Response {
                ts: 2,
                text: big_text,
                tool_uses: vec![],
            },
            JournalEntry::Instruction {
                ts: 3,
                text: "recent".to_string(),
            },
            JournalEntry::Response {
                ts: 4,
                text: "recent response".to_string(),
                tool_uses: vec![],
            },
        ];

        // Budget of 100 tokens — should only include recent entries
        let msgs = build_conversation_from_journal(&entries, 100);
        assert!(!msgs.is_empty());
        let all_text: String = msgs.iter().map(|m| m.content.clone()).collect();
        assert!(all_text.contains("recent"));
    }

    #[test]
    fn blocked_entry_becomes_user_message() {
        let entries = vec![
            JournalEntry::Instruction {
                ts: 1,
                text: "run something".to_string(),
            },
            JournalEntry::Response {
                ts: 2,
                text: "OK".to_string(),
                tool_uses: vec![ToolUseRecord {
                    id: "toolu_1".to_string(),
                    name: "shell".to_string(),
                    input_json: r#"{"command":"rm -rf /"}"#.to_string(),
                }],
            },
            JournalEntry::Blocked {
                ts: 3,
                results: vec![ToolResultRecord {
                    tool_use_id: "toolu_1".to_string(),
                    content: "Command blocked".to_string(),
                }],
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[2].role, ua_protocol::Role::User);
        assert_eq!(msgs[2].tool_results.len(), 1);
    }

    #[test]
    fn assistant_first_gets_synthetic_user() {
        let entries = vec![JournalEntry::Response {
            ts: 2,
            text: "Continuing...".to_string(),
            tool_uses: vec![],
        }];

        let msgs = build_conversation_from_journal(&entries, 60000);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, ua_protocol::Role::User);
        assert!(msgs[0].content.contains("[session continues]"));
        assert_eq!(msgs[1].role, ua_protocol::Role::Assistant);
    }

    #[test]
    fn shell_command_none_exit_code() {
        let entries = vec![JournalEntry::ShellCommand {
            ts: 1,
            command: "sleep 100 &".to_string(),
            exit_code: None,
        }];

        let msgs = build_conversation_from_journal(&entries, 60000);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("unknown exit"));
    }

    #[test]
    fn budget_always_includes_at_least_last_entry() {
        let entries = vec![JournalEntry::Instruction {
            ts: 1,
            text: "x".repeat(4000), // ~1000 tokens
        }];

        // Budget of 1 token — still includes the single entry
        let msgs = build_conversation_from_journal(&entries, 1);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn alternation_after_tool_result() {
        // Response → ToolResult → Response → ToolResult
        // Should produce: user(synthetic) → assistant → user(tool_result) → assistant → user(tool_result)
        let entries = vec![
            JournalEntry::Instruction {
                ts: 1,
                text: "do it".to_string(),
            },
            JournalEntry::Response {
                ts: 2,
                text: "step 1".to_string(),
                tool_uses: vec![ToolUseRecord {
                    id: "t1".to_string(),
                    name: "shell".to_string(),
                    input_json: r#"{"command":"ls"}"#.to_string(),
                }],
            },
            JournalEntry::ToolResult {
                ts: 3,
                results: vec![ToolResultRecord {
                    tool_use_id: "t1".to_string(),
                    content: "file.txt".to_string(),
                }],
            },
            JournalEntry::Response {
                ts: 4,
                text: "step 2".to_string(),
                tool_uses: vec![ToolUseRecord {
                    id: "t2".to_string(),
                    name: "shell".to_string(),
                    input_json: r#"{"command":"cat file.txt"}"#.to_string(),
                }],
            },
            JournalEntry::ToolResult {
                ts: 5,
                results: vec![ToolResultRecord {
                    tool_use_id: "t2".to_string(),
                    content: "hello world".to_string(),
                }],
            },
        ];

        let msgs = build_conversation_from_journal(&entries, 60000);
        // Verify strict alternation: user, assistant, user, assistant, user
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].role, ua_protocol::Role::User);
        assert_eq!(msgs[1].role, ua_protocol::Role::Assistant);
        assert_eq!(msgs[2].role, ua_protocol::Role::User);
        assert_eq!(msgs[3].role, ua_protocol::Role::Assistant);
        assert_eq!(msgs[4].role, ua_protocol::Role::User);
    }
}
