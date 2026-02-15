//! Streaming response accumulator.

use ua_protocol::StreamEvent;

/// Current display status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayStatus {
    Idle,
    Thinking,
    Streaming,
    Error(String),
}

/// Accumulates streaming response data from the backend.
pub struct PlanDisplay {
    pub thinking_text: String,
    pub streaming_text: String,
    pub status: DisplayStatus,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl Default for PlanDisplay {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanDisplay {
    pub fn new() -> Self {
        Self {
            thinking_text: String::new(),
            streaming_text: String::new(),
            status: DisplayStatus::Idle,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Handle a stream event, updating internal state.
    pub fn handle_event(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::ThinkingDelta(text) => {
                self.status = DisplayStatus::Thinking;
                self.thinking_text.push_str(text);
            }
            StreamEvent::TextDelta(text) => {
                self.status = DisplayStatus::Streaming;
                self.streaming_text.push_str(text);
            }
            StreamEvent::ToolUse { .. } => {}
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                self.input_tokens += input_tokens;
                self.output_tokens += output_tokens;
            }
            StreamEvent::Done => {}
            StreamEvent::Error(msg) => {
                self.status = DisplayStatus::Error(msg.clone());
            }
        }
    }

    /// Reset the display state.
    pub fn reset(&mut self) {
        self.thinking_text.clear();
        self.streaming_text.clear();
        self.status = DisplayStatus::Idle;
        self.input_tokens = 0;
        self.output_tokens = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_starts_idle() {
        let display = PlanDisplay::new();
        assert_eq!(display.status, DisplayStatus::Idle);
        assert!(display.streaming_text.is_empty());
        assert!(display.thinking_text.is_empty());
    }

    #[test]
    fn handle_thinking_delta() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::ThinkingDelta("Hmm".to_string()));

        assert_eq!(display.status, DisplayStatus::Thinking);
        assert_eq!(display.thinking_text, "Hmm");
    }

    #[test]
    fn handle_text_delta() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::TextDelta("Hello".to_string()));

        assert_eq!(display.status, DisplayStatus::Streaming);
        assert_eq!(display.streaming_text, "Hello");
    }

    #[test]
    fn handle_text_delta_accumulates() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::TextDelta("Hello ".to_string()));
        display.handle_event(&StreamEvent::TextDelta("world".to_string()));

        assert_eq!(display.streaming_text, "Hello world");
    }

    #[test]
    fn thinking_then_text() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::ThinkingDelta("thinking...".to_string()));
        assert_eq!(display.status, DisplayStatus::Thinking);

        display.handle_event(&StreamEvent::TextDelta("response".to_string()));
        assert_eq!(display.status, DisplayStatus::Streaming);
        assert_eq!(display.thinking_text, "thinking...");
        assert_eq!(display.streaming_text, "response");
    }

    #[test]
    fn handle_error() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::Error("API error".to_string()));

        assert_eq!(
            display.status,
            DisplayStatus::Error("API error".to_string())
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::ThinkingDelta("think".to_string()));
        display.handle_event(&StreamEvent::TextDelta("text".to_string()));

        display.reset();

        assert_eq!(display.status, DisplayStatus::Idle);
        assert!(display.thinking_text.is_empty());
        assert!(display.streaming_text.is_empty());
    }

    #[test]
    fn handle_usage_accumulates() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 50,
        });
        display.handle_event(&StreamEvent::Usage {
            input_tokens: 200,
            output_tokens: 75,
        });
        assert_eq!(display.input_tokens, 300);
        assert_eq!(display.output_tokens, 125);
    }

    #[test]
    fn reset_clears_tokens() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 50,
        });
        display.reset();
        assert_eq!(display.input_tokens, 0);
        assert_eq!(display.output_tokens, 0);
    }

    #[test]
    fn usage_does_not_change_status() {
        let mut display = PlanDisplay::new();
        display.handle_event(&StreamEvent::Usage {
            input_tokens: 100,
            output_tokens: 50,
        });
        assert_eq!(display.status, DisplayStatus::Idle);
    }
}
