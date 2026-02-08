//! Mock provider for testing.
//!
//! Produces the exact same `StreamEvent` sequence as the real Anthropic adapter,
//! allowing tests at every layer to use the mock instead of real HTTP.

use std::time::Duration;

use async_stream::stream;
use futures::Stream;
use tokio::time::sleep;
use ua_protocol::StreamEvent;

/// Configurable mock responses for testing.
#[derive(Debug, Clone)]
pub enum MockResponse {
    /// Emit a thinking delta.
    Thinking { content: String },
    /// Emit a text delta.
    Text { content: String },
    /// Emit usage information.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// Emit an error.
    Error { message: String },
    /// Delay before next event (for timing tests).
    Delay { ms: u64 },
}

/// Configuration for mock stream.
#[derive(Debug, Clone, Default)]
pub struct MockConfig {
    /// Sequence of responses to emit.
    pub responses: Vec<MockResponse>,
    /// Optional delay between each event (ms).
    pub chunk_delay_ms: Option<u64>,
}

impl MockConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_responses(mut self, responses: Vec<MockResponse>) -> Self {
        self.responses = responses;
        self
    }

    pub fn with_chunk_delay(mut self, ms: u64) -> Self {
        self.chunk_delay_ms = Some(ms);
        self
    }
}

/// Create a stream of StreamEvents from mock config.
pub fn mock_stream(config: MockConfig) -> impl Stream<Item = StreamEvent> {
    stream! {
        for response in config.responses {
            // Apply inter-event delay if configured
            if let Some(delay_ms) = config.chunk_delay_ms {
                sleep(Duration::from_millis(delay_ms)).await;
            }

            match response {
                MockResponse::Thinking { content } => {
                    yield StreamEvent::ThinkingDelta(content);
                }
                MockResponse::Text { content } => {
                    yield StreamEvent::TextDelta(content);
                }
                MockResponse::Usage { input_tokens, output_tokens } => {
                    yield StreamEvent::Usage { input_tokens, output_tokens };
                }
                MockResponse::Error { message } => {
                    yield StreamEvent::Error(message);
                }
                MockResponse::Delay { ms } => {
                    sleep(Duration::from_millis(ms)).await;
                    // Delay doesn't emit an event
                }
            }
        }

        yield StreamEvent::Done;
    }
}

/// Built-in test fixtures for common scenarios.
pub mod fixtures {
    use super::*;

    /// Create a mock config for a text response with embedded commands.
    pub fn text_with_commands(explanation: &str, commands: &[&str]) -> MockConfig {
        let mut responses = vec![MockResponse::Text {
            content: format!("{explanation}\n\n"),
        }];

        for cmd in commands {
            responses.push(MockResponse::Text {
                content: format!("```\n{cmd}\n```\n\n"),
            });
        }

        MockConfig::new().with_responses(responses)
    }

    /// Create a mock config with thinking followed by text with commands.
    pub fn thinking_then_commands(
        thinking: &str,
        explanation: &str,
        commands: &[&str],
    ) -> MockConfig {
        let mut responses = vec![
            MockResponse::Thinking {
                content: thinking.to_string(),
            },
            MockResponse::Text {
                content: format!("{explanation}\n\n"),
            },
        ];

        for cmd in commands {
            responses.push(MockResponse::Text {
                content: format!("```\n{cmd}\n```\n\n"),
            });
        }

        MockConfig::new().with_responses(responses)
    }

    /// Create a mock config that streams text in chunks.
    pub fn streaming_text(chunks: &[&str]) -> MockConfig {
        let responses = chunks
            .iter()
            .map(|chunk| MockResponse::Text {
                content: (*chunk).to_string(),
            })
            .collect();

        MockConfig::new().with_responses(responses)
    }

    /// Create a mock config that errors mid-stream.
    pub fn error_mid_stream(text_before: &str, error: &str) -> MockConfig {
        MockConfig::new().with_responses(vec![
            MockResponse::Text {
                content: text_before.to_string(),
            },
            MockResponse::Error {
                message: error.to_string(),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_stream_emits_events() {
        let config = MockConfig::new().with_responses(vec![
            MockResponse::Text {
                content: "Hello".to_string(),
            },
            MockResponse::Text {
                content: " world".to_string(),
            },
        ]);

        let events: Vec<_> = mock_stream(config).collect().await;

        assert_eq!(events.len(), 3); // 2 text + Done
        assert_eq!(events[0], StreamEvent::TextDelta("Hello".to_string()));
        assert_eq!(events[1], StreamEvent::TextDelta(" world".to_string()));
        assert_eq!(events[2], StreamEvent::Done);
    }

    #[tokio::test]
    async fn mock_stream_thinking() {
        let config = MockConfig::new().with_responses(vec![MockResponse::Thinking {
            content: "Let me think...".to_string(),
        }]);

        let events: Vec<_> = mock_stream(config).collect().await;

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            StreamEvent::ThinkingDelta("Let me think...".to_string())
        );
        assert_eq!(events[1], StreamEvent::Done);
    }

    #[tokio::test]
    async fn mock_stream_error() {
        let config = MockConfig::new().with_responses(vec![MockResponse::Error {
            message: "API error".to_string(),
        }]);

        let events: Vec<_> = mock_stream(config).collect().await;

        assert_eq!(events[0], StreamEvent::Error("API error".to_string()));
    }

    #[tokio::test]
    async fn fixture_text_with_commands() {
        let config = fixtures::text_with_commands("I'll list files", &["ls /tmp", "cat foo.txt"]);
        let events: Vec<_> = mock_stream(config).collect().await;

        // explanation + 2 command blocks + Done
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0],
            StreamEvent::TextDelta("I'll list files\n\n".to_string())
        );
        assert_eq!(
            events[1],
            StreamEvent::TextDelta("```\nls /tmp\n```\n\n".to_string())
        );
        assert_eq!(
            events[2],
            StreamEvent::TextDelta("```\ncat foo.txt\n```\n\n".to_string())
        );
    }

    #[tokio::test]
    async fn fixture_thinking_then_commands() {
        let config = fixtures::thinking_then_commands(
            "The user wants to see files",
            "I'll list the directory",
            &["ls /tmp"],
        );
        let events: Vec<_> = mock_stream(config).collect().await;

        // thinking + explanation + 1 command block + Done
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0],
            StreamEvent::ThinkingDelta("The user wants to see files".to_string())
        );
    }

    #[tokio::test]
    async fn fixture_streaming_text() {
        let config = fixtures::streaming_text(&["Think", "ing", "..."]);
        let events: Vec<_> = mock_stream(config).collect().await;

        assert_eq!(events.len(), 4); // 3 text + Done
        assert_eq!(events[0], StreamEvent::TextDelta("Think".to_string()));
        assert_eq!(events[1], StreamEvent::TextDelta("ing".to_string()));
        assert_eq!(events[2], StreamEvent::TextDelta("...".to_string()));
    }

    #[tokio::test]
    async fn fixture_error_mid_stream() {
        let config = fixtures::error_mid_stream("Processing...", "Rate limited");
        let events: Vec<_> = mock_stream(config).collect().await;

        assert_eq!(events.len(), 3); // text + error + Done
        assert_eq!(
            events[0],
            StreamEvent::TextDelta("Processing...".to_string())
        );
        assert_eq!(events[1], StreamEvent::Error("Rate limited".to_string()));
    }
}
