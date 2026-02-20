//! ua-protocol: Shared types and message definitions for UnixAgent.
//!
//! This crate defines the protocol types used between the core agent,
//! LLM backends, and any future frontends.

pub mod context;
pub mod message;

pub use context::{
    AgentRequest, Attachment, ConversationMessage, Role, ShellContext, TerminalHistory,
    ToolResultRecord, ToolUseRecord,
};
pub use message::StreamEvent;
