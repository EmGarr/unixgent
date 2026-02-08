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
}

impl ConversationMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
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
}
