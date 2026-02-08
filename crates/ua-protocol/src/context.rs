//! Context types for agent requests.

use serde::{Deserialize, Serialize};

/// Represents the current shell environment context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellContext {
    pub cwd: String,
    pub shell: String,
    pub platform: String,
    pub arch: String,
    pub env_vars: Vec<(String, String)>,
    pub terminal_size: (u16, u16),
}

impl Default for ShellContext {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            shell: String::new(),
            platform: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            env_vars: Vec::new(),
            terminal_size: (80, 24),
        }
    }
}

/// Recent terminal output history.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TerminalHistory {
    pub lines: Vec<String>,
}

impl TerminalHistory {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    pub fn from_lines(lines: Vec<String>) -> Self {
        Self { lines }
    }
}

/// A tool_use block from an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolUseRecord {
    pub id: String,
    pub name: String,
    pub input_json: String,
}

/// A tool_result block from a user message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultRecord {
    pub tool_use_id: String,
    pub content: String,
}

/// Role in a conversation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_uses: Vec<ToolUseRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResultRecord>,
}

impl ConversationMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn assistant_with_tool_use(
        content: impl Into<String>,
        tool_uses: Vec<ToolUseRecord>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_uses,
            tool_results: Vec::new(),
        }
    }

    pub fn tool_result(results: Vec<ToolResultRecord>) -> Self {
        Self {
            role: Role::User,
            content: String::new(),
            tool_uses: Vec::new(),
            tool_results: results,
        }
    }
}

/// A complete request to the agent backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRequest {
    pub instruction: String,
    pub context: ShellContext,
    pub terminal_history: TerminalHistory,
    pub conversation: Vec<ConversationMessage>,
}

impl AgentRequest {
    pub fn new(instruction: impl Into<String>, context: ShellContext) -> Self {
        Self {
            instruction: instruction.into(),
            context,
            terminal_history: TerminalHistory::new(),
            conversation: Vec::new(),
        }
    }

    pub fn with_history(mut self, history: TerminalHistory) -> Self {
        self.terminal_history = history;
        self
    }

    pub fn with_conversation(mut self, conversation: Vec<ConversationMessage>) -> Self {
        self.conversation = conversation;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_context_default() {
        let ctx = ShellContext::default();
        assert!(ctx.cwd.is_empty());
        assert!(ctx.shell.is_empty());
        assert!(!ctx.platform.is_empty());
        assert!(!ctx.arch.is_empty());
        assert_eq!(ctx.terminal_size, (80, 24));
    }

    #[test]
    fn shell_context_roundtrip() {
        let ctx = ShellContext {
            cwd: "/tmp".to_string(),
            shell: "bash".to_string(),
            platform: "linux".to_string(),
            arch: "x86_64".to_string(),
            env_vars: vec![("PATH".to_string(), "/usr/bin".to_string())],
            terminal_size: (120, 40),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let ctx2: ShellContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, ctx2);
    }

    #[test]
    fn conversation_message_helpers() {
        let user = ConversationMessage::user("hello");
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content, "hello");

        let assistant = ConversationMessage::assistant("hi there");
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(assistant.content, "hi there");
    }

    #[test]
    fn agent_request_builder() {
        let ctx = ShellContext::default();
        let history = TerminalHistory::from_lines(vec!["line1".to_string()]);
        let conversation = vec![ConversationMessage::user("test")];

        let request = AgentRequest::new("do something", ctx.clone())
            .with_history(history.clone())
            .with_conversation(conversation.clone());

        assert_eq!(request.instruction, "do something");
        assert_eq!(request.context, ctx);
        assert_eq!(request.terminal_history, history);
        assert_eq!(request.conversation, conversation);
    }

    #[test]
    fn role_serialization() {
        let user = Role::User;
        let json = serde_json::to_string(&user).unwrap();
        assert_eq!(json, "\"user\"");

        let assistant = Role::Assistant;
        let json = serde_json::to_string(&assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
    }

    #[test]
    fn tool_use_record_roundtrip() {
        let record = ToolUseRecord {
            id: "toolu_123".to_string(),
            name: "shell".to_string(),
            input_json: r#"{"command":"ls"}"#.to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let record2: ToolUseRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, record2);
    }

    #[test]
    fn tool_result_record_roundtrip() {
        let record = ToolResultRecord {
            tool_use_id: "toolu_123".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let record2: ToolResultRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, record2);
    }

    #[test]
    fn conversation_message_with_tool_use() {
        let tool_uses = vec![ToolUseRecord {
            id: "toolu_abc".to_string(),
            name: "shell".to_string(),
            input_json: r#"{"command":"pwd"}"#.to_string(),
        }];
        let msg = ConversationMessage::assistant_with_tool_use("I'll check.", tool_uses.clone());
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, "I'll check.");
        assert_eq!(msg.tool_uses, tool_uses);
        assert!(msg.tool_results.is_empty());
    }

    #[test]
    fn conversation_message_tool_result() {
        let results = vec![ToolResultRecord {
            tool_use_id: "toolu_abc".to_string(),
            content: "/home/user".to_string(),
        }];
        let msg = ConversationMessage::tool_result(results.clone());
        assert_eq!(msg.role, Role::User);
        assert!(msg.content.is_empty());
        assert!(msg.tool_uses.is_empty());
        assert_eq!(msg.tool_results, results);
    }

    #[test]
    fn conversation_message_roundtrip_with_tools() {
        let msg = ConversationMessage::assistant_with_tool_use(
            "checking",
            vec![ToolUseRecord {
                id: "toolu_1".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"ls"}"#.to_string(),
            }],
        );
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("tool_uses"));
        let msg2: ConversationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, msg2);
    }

    #[test]
    fn conversation_message_roundtrip_without_tools() {
        let msg = ConversationMessage::user("hello");
        let json = serde_json::to_string(&msg).unwrap();
        // skip_serializing_if means these fields are omitted
        assert!(!json.contains("tool_uses"));
        assert!(!json.contains("tool_results"));
        let msg2: ConversationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, msg2);
    }

    #[test]
    fn conversation_message_user_has_empty_vecs() {
        let msg = ConversationMessage::user("test");
        assert!(msg.tool_uses.is_empty());
        assert!(msg.tool_results.is_empty());
    }

    #[test]
    fn conversation_message_assistant_has_empty_vecs() {
        let msg = ConversationMessage::assistant("test");
        assert!(msg.tool_uses.is_empty());
        assert!(msg.tool_results.is_empty());
    }
}
