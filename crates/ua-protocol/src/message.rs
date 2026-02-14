//! Message types for agent responses and streaming events.

/// Events emitted during streaming response.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A chunk of thinking/reasoning text from the model.
    ThinkingDelta(String),

    /// A chunk of response text.
    TextDelta(String),

    /// The model is calling a tool. Contains the tool call ID and accumulated input JSON.
    ToolUse {
        id: String,
        name: String,
        input_json: String,
    },

    /// Token usage information.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },

    /// Stream has completed successfully.
    Done,

    /// An error occurred during streaming.
    Error(String),
}
