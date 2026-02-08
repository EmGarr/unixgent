//! Server-Sent Events (SSE) stream parser.
//!
//! Parses a byte stream into SSE events according to the W3C specification.

use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A parsed SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The event type (from `event:` field). None if not specified.
    pub event_type: Option<String>,
    /// The event data (from `data:` field(s)).
    pub data: String,
}

/// Parser state for SSE stream.
#[derive(Default)]
struct SseParserState {
    /// Buffer for incomplete lines
    line_buf: String,
    /// Current event being accumulated
    current_event_type: Option<String>,
    /// Current data lines being accumulated
    current_data: Vec<String>,
}

impl SseParserState {
    fn new() -> Self {
        Self::default()
    }

    /// Process a complete line. Returns an event if one is complete.
    fn process_line(&mut self, line: &str) -> Option<SseEvent> {
        // Empty line signals end of event
        if line.is_empty() {
            if self.current_data.is_empty() {
                // No data accumulated, nothing to emit
                return None;
            }

            let event = SseEvent {
                event_type: self.current_event_type.take(),
                data: self.current_data.join("\n"),
            };
            self.current_data.clear();
            return Some(event);
        }

        // Parse field:value
        if let Some(colon_pos) = line.find(':') {
            let field = &line[..colon_pos];
            // Value starts after colon, skip optional leading space
            let value_start = colon_pos + 1;
            let value = if line.len() > value_start && line.as_bytes()[value_start] == b' ' {
                &line[value_start + 1..]
            } else {
                &line[value_start..]
            };

            match field {
                "event" => {
                    self.current_event_type = Some(value.to_string());
                }
                "data" => {
                    self.current_data.push(value.to_string());
                }
                // Ignore other fields (id, retry, comments)
                _ => {}
            }
        }
        // Lines without colons are comments or invalid, ignore them

        None
    }
}

/// Stream wrapper that parses SSE events from a byte stream.
pub struct SseStream<S> {
    inner: S,
    state: SseParserState,
    pending_events: Vec<SseEvent>,
}

impl<S> SseStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            state: SseParserState::new(),
            pending_events: Vec::new(),
        }
    }
}

impl<S, E> Stream for SseStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    type Item = Result<SseEvent, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;

        // First, drain any pending events
        if !this.pending_events.is_empty() {
            return Poll::Ready(Some(Ok(this.pending_events.remove(0))));
        }

        // Poll the inner stream
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    // Process bytes into lines
                    let chunk = String::from_utf8_lossy(&bytes);
                    for c in chunk.chars() {
                        if c == '\n' {
                            let line = std::mem::take(&mut this.state.line_buf);
                            // Strip trailing \r if present
                            let line = line.strip_suffix('\r').unwrap_or(&line);
                            if let Some(event) = this.state.process_line(line) {
                                this.pending_events.push(event);
                            }
                        } else {
                            this.state.line_buf.push(c);
                        }
                    }

                    // Return first event if we have any
                    if !this.pending_events.is_empty() {
                        return Poll::Ready(Some(Ok(this.pending_events.remove(0))));
                    }
                    // Otherwise continue polling for more data
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    // Stream ended. Process any remaining buffered data.
                    if !this.state.line_buf.is_empty() {
                        let line = std::mem::take(&mut this.state.line_buf);
                        if let Some(event) = this.state.process_line(&line) {
                            return Poll::Ready(Some(Ok(event)));
                        }
                    }
                    // Emit final event if data was accumulated
                    if !this.state.current_data.is_empty() {
                        let event = SseEvent {
                            event_type: this.state.current_event_type.take(),
                            data: this.state.current_data.join("\n"),
                        };
                        this.state.current_data.clear();
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

/// Create an SSE stream from a byte stream.
pub fn parse_sse_stream<S, E>(stream: S) -> SseStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
{
    SseStream::new(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn bytes_stream(
        chunks: Vec<&'static str>,
    ) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
        futures::stream::iter(chunks.into_iter().map(|s| Ok(Bytes::from(s))))
    }

    #[tokio::test]
    async fn parse_simple_event() {
        let stream = bytes_stream(vec!["data: hello\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, None);
        assert_eq!(event.data, "hello");
        assert!(sse.next().await.is_none());
    }

    #[tokio::test]
    async fn parse_event_with_type() {
        let stream = bytes_stream(vec!["event: message\ndata: hello world\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, Some("message".to_string()));
        assert_eq!(event.data, "hello world");
    }

    #[tokio::test]
    async fn parse_multi_line_data() {
        let stream = bytes_stream(vec!["data: line1\ndata: line2\ndata: line3\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "line1\nline2\nline3");
    }

    #[tokio::test]
    async fn parse_multiple_events() {
        let stream = bytes_stream(vec![
            "event: first\ndata: one\n\nevent: second\ndata: two\n\n",
        ]);
        let mut sse = parse_sse_stream(stream);

        let event1 = sse.next().await.unwrap().unwrap();
        assert_eq!(event1.event_type, Some("first".to_string()));
        assert_eq!(event1.data, "one");

        let event2 = sse.next().await.unwrap().unwrap();
        assert_eq!(event2.event_type, Some("second".to_string()));
        assert_eq!(event2.data, "two");

        assert!(sse.next().await.is_none());
    }

    #[tokio::test]
    async fn parse_chunked_data() {
        // Data split across multiple chunks
        let stream = bytes_stream(vec!["data: hel", "lo wor", "ld\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "hello world");
    }

    #[tokio::test]
    async fn parse_with_crlf() {
        let stream = bytes_stream(vec!["data: hello\r\n\r\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "hello");
    }

    #[tokio::test]
    async fn ignore_comments() {
        let stream = bytes_stream(vec![": this is a comment\ndata: actual data\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "actual data");
    }

    #[tokio::test]
    async fn ignore_unknown_fields() {
        let stream = bytes_stream(vec!["id: 123\nretry: 5000\ndata: hello\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "hello");
    }

    #[tokio::test]
    async fn empty_data_field() {
        let stream = bytes_stream(vec!["data:\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "");
    }

    #[tokio::test]
    async fn multiple_empty_lines_between_events() {
        let stream = bytes_stream(vec!["data: first\n\n\n\ndata: second\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event1 = sse.next().await.unwrap().unwrap();
        assert_eq!(event1.data, "first");

        let event2 = sse.next().await.unwrap().unwrap();
        assert_eq!(event2.data, "second");
    }

    #[tokio::test]
    async fn data_with_colon() {
        let stream = bytes_stream(vec!["data: {\"key\": \"value\"}\n\n"]);
        let mut sse = parse_sse_stream(stream);

        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "{\"key\": \"value\"}");
    }

    #[tokio::test]
    async fn event_at_stream_end_without_trailing_newline() {
        let stream = bytes_stream(vec!["data: final"]);
        let mut sse = parse_sse_stream(stream);

        // Should still emit the event when stream ends
        let event = sse.next().await.unwrap().unwrap();
        assert_eq!(event.data, "final");
    }
}
