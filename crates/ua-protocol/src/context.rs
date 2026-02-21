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

/// Reference to a media file on disk (serialized to journal).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaRef {
    pub media_type: String,
    pub filename: String,
}

/// Resolved media ready for API call (NOT serialized to journal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMedia {
    pub media_type: String,
    pub data: String, // base64-encoded
}

/// A tool_result block from a user message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultRecord {
    pub tool_use_id: String,
    pub content: String,
    /// References to media files on disk (serialized to journal).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<MediaRef>,
    /// Resolved media with loaded data (NOT serialized — populated at runtime).
    #[serde(skip)]
    pub resolved_media: Vec<ResolvedMedia>,
}

impl ToolResultRecord {
    /// Create a text-only tool result (no media).
    pub fn text(tool_use_id: String, content: String) -> Self {
        Self {
            tool_use_id,
            content,
            media: Vec::new(),
            resolved_media: Vec::new(),
        }
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

/// A resolved image attachment: file already read and base64-encoded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    /// Original file name (e.g., "screenshot.png").
    pub filename: String,
    /// MIME type (e.g., "image/png").
    pub media_type: String,
    /// Base64-encoded file data.
    pub data: String,
}

/// A complete request to the agent backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRequest {
    pub instruction: String,
    pub context: ShellContext,
    pub terminal_history: TerminalHistory,
    pub conversation: Vec<ConversationMessage>,
    /// Extra text appended to the system prompt (e.g. batch-mode instructions).
    /// The backend appends this if present; callers compose it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_extra: Option<String>,
    /// Image attachments to include with the instruction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl AgentRequest {
    pub fn new(instruction: impl Into<String>, context: ShellContext) -> Self {
        Self {
            instruction: instruction.into(),
            context,
            terminal_history: TerminalHistory::new(),
            conversation: Vec::new(),
            system_prompt_extra: None,
            attachments: Vec::new(),
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

    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.attachments = attachments;
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
        let record =
            ToolResultRecord::text("toolu_123".to_string(), "file1.txt\nfile2.txt".to_string());
        let json = serde_json::to_string(&record).unwrap();
        let record2: ToolResultRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, record2);
    }

    #[test]
    fn tool_result_record_with_media_roundtrip() {
        let record = ToolResultRecord {
            tool_use_id: "toolu_abc".to_string(),
            content: "[binary: 204800 bytes, image/png]".to_string(),
            media: vec![MediaRef {
                media_type: "image/png".to_string(),
                filename: "toolu_abc.png".to_string(),
            }],
            resolved_media: vec![ResolvedMedia {
                media_type: "image/png".to_string(),
                data: "base64data".to_string(),
            }],
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"media\""));
        // resolved_media should NOT appear in JSON
        assert!(!json.contains("resolved_media"));
        let record2: ToolResultRecord = serde_json::from_str(&json).unwrap();
        // media refs should survive roundtrip
        assert_eq!(record2.media, record.media);
        // resolved_media should be empty after deserialization
        assert!(record2.resolved_media.is_empty());
    }

    #[test]
    fn tool_result_record_backward_compat() {
        // Old journal entries without media field should parse fine
        let json = r#"{"tool_use_id":"toolu_1","content":"output"}"#;
        let record: ToolResultRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.tool_use_id, "toolu_1");
        assert_eq!(record.content, "output");
        assert!(record.media.is_empty());
        assert!(record.resolved_media.is_empty());
    }

    #[test]
    fn tool_result_record_empty_media_skipped() {
        let record = ToolResultRecord::text("toolu_1".to_string(), "output".to_string());
        let json = serde_json::to_string(&record).unwrap();
        assert!(
            !json.contains("media"),
            "empty media should be skipped in serialization"
        );
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
        let results = vec![ToolResultRecord::text(
            "toolu_abc".to_string(),
            "/home/user".to_string(),
        )];
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

    #[test]
    fn attachment_roundtrip() {
        let att = Attachment {
            filename: "test.png".to_string(),
            media_type: "image/png".to_string(),
            data: "iVBOR...".to_string(),
        };
        let json = serde_json::to_string(&att).unwrap();
        let att2: Attachment = serde_json::from_str(&json).unwrap();
        assert_eq!(att, att2);
    }

    #[test]
    fn agent_request_with_attachments_roundtrip() {
        let ctx = ShellContext::default();
        let attachments = vec![Attachment {
            filename: "img.png".to_string(),
            media_type: "image/png".to_string(),
            data: "base64data".to_string(),
        }];
        let request = AgentRequest::new("describe this", ctx).with_attachments(attachments);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("attachments"));
        let request2: AgentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, request2);
    }

    #[test]
    fn agent_request_without_attachments_omits_field() {
        let ctx = ShellContext::default();
        let request = AgentRequest::new("hello", ctx);
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("attachments"));
    }
}
