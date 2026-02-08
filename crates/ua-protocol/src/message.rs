//! Message types for agent responses and streaming events.

/// Events emitted during streaming response.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A chunk of thinking/reasoning text from the model.
    ThinkingDelta(String),

    /// A chunk of response text.
    TextDelta(String),

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_event_variants() {
        let events = vec![
            StreamEvent::ThinkingDelta("hmm".to_string()),
            StreamEvent::TextDelta("hello".to_string()),
            StreamEvent::Usage {
                input_tokens: 100,
                output_tokens: 50,
            },
            StreamEvent::Done,
            StreamEvent::Error("something went wrong".to_string()),
        ];

        assert_eq!(events.len(), 5);
    }
}
