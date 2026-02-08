//! Anthropic Claude API client with SSE streaming support.

use async_stream::stream;
use futures::Stream;
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use ua_protocol::{AgentRequest, StreamEvent};

use crate::sse::{parse_sse_stream, SseEvent};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";

#[derive(Debug, Error)]
pub enum AnthropicError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("API error: {0}")]
    Api(String),
}

/// Anthropic API client.
pub struct AnthropicClient {
    api_key: String,
    model: String,
    http: Client,
}

impl AnthropicClient {
    /// Create a new client with the given API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_string(),
            http: Client::new(),
        }
    }

    /// Create a new client with a custom model.
    pub fn with_model(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            http: Client::new(),
        }
    }

    /// Send a request and return a stream of events.
    pub fn send(&self, request: &AgentRequest) -> impl Stream<Item = StreamEvent> + Send + 'static {
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let http = self.http.clone();
        let request = request.clone();

        stream! {
            match send_request(&http, &api_key, &model, &request).await {
                Ok(response) => {
                    let byte_stream = response.bytes_stream();
                    let mut sse_stream = parse_sse_stream(byte_stream);
                    let mut processor = SseProcessor::new();

                    use futures::StreamExt;

                    while let Some(result) = sse_stream.next().await {
                        match result {
                            Ok(sse_event) => {
                                for stream_event in processor.process(&sse_event) {
                                    yield stream_event;
                                }
                            }
                            Err(e) => {
                                yield StreamEvent::Error(format!("Stream error: {e}"));
                                return;
                            }
                        }
                    }

                    yield StreamEvent::Done;
                }
                Err(e) => {
                    yield StreamEvent::Error(e.to_string());
                }
            }
        }
    }
}

async fn send_request(
    http: &Client,
    api_key: &str,
    model: &str,
    request: &AgentRequest,
) -> Result<reqwest::Response, AnthropicError> {
    let system_prompt = build_system_prompt(request);
    let messages = build_messages(request);

    let body = ApiRequest {
        model: model.to_string(),
        max_tokens: 16000,
        stream: true,
        system: system_prompt,
        messages,
        tools: vec![build_shell_tool()],
        thinking: ApiThinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 10000,
        },
    };

    let response = http
        .post(API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AnthropicError::Api(format!("{status}: {body}")));
    }

    Ok(response)
}

fn build_shell_tool() -> ApiTool {
    ApiTool {
        name: "shell".to_string(),
        description: "Execute a shell command. The command runs in the user's terminal via PTY."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute. Chain multiple commands with && if needed."
                }
            },
            "required": ["command"]
        }),
    }
}

fn build_system_prompt(request: &AgentRequest) -> String {
    let ctx = &request.context;
    let (cols, rows) = ctx.terminal_size;

    let mut prompt = format!(
        "You are a Unix shell agent. The user is working in a terminal.

Working directory: {}
Shell: {}
Platform: {} ({})
Terminal: {}x{}",
        ctx.cwd, ctx.shell, ctx.platform, ctx.arch, cols, rows
    );

    if !ctx.env_vars.is_empty() {
        prompt.push_str("\n\nEnvironment variables:");
        for (key, value) in &ctx.env_vars {
            prompt.push_str(&format!("\n  {key}={value}"));
        }
    }

    if !request.terminal_history.lines.is_empty() {
        prompt.push_str("\n\nRecent terminal output:");
        for line in &request.terminal_history.lines {
            prompt.push_str(&format!("\n{line}"));
        }
    }

    prompt.push_str(
        "\n\nUse the shell tool to execute commands. \
         Each command runs and you will see the output. \
         You may then run more commands or provide your final answer. \
         When you are done, respond with text only (no tool calls).",
    );

    prompt
}

fn build_messages(request: &AgentRequest) -> Vec<ApiMessage> {
    let mut messages = Vec::new();

    // Add conversation history
    for msg in &request.conversation {
        messages.push(ApiMessage {
            role: match msg.role {
                ua_protocol::Role::User => "user".to_string(),
                ua_protocol::Role::Assistant => "assistant".to_string(),
            },
            content: ApiContent::Text(msg.content.clone()),
        });
    }

    // Add current instruction
    messages.push(ApiMessage {
        role: "user".to_string(),
        content: ApiContent::Text(request.instruction.clone()),
    });

    messages
}

/// Tracks state across SSE events for tool_use accumulation.
///
/// Tool use blocks arrive as:
///   content_block_start (type=tool_use, id, name)
///   content_block_delta* (input_json_delta chunks)
///   content_block_stop
struct SseProcessor {
    /// Active tool_use block being accumulated.
    active_tool: Option<ToolAccumulator>,
}

struct ToolAccumulator {
    id: String,
    name: String,
    input_json: String,
}

impl SseProcessor {
    fn new() -> Self {
        Self { active_tool: None }
    }

    fn process(&mut self, event: &SseEvent) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        let data: Value = match serde_json::from_str(&event.data) {
            Ok(v) => v,
            Err(_) => return events,
        };

        let event_type = event.event_type.as_deref().unwrap_or("");

        match event_type {
            "message_start" => {
                if let Some(usage) = data.get("message").and_then(|m| m.get("usage")) {
                    if let (Some(input), Some(output)) = (
                        usage.get("input_tokens").and_then(|v| v.as_u64()),
                        usage.get("output_tokens").and_then(|v| v.as_u64()),
                    ) {
                        events.push(StreamEvent::Usage {
                            input_tokens: input as u32,
                            output_tokens: output as u32,
                        });
                    }
                }
            }
            "content_block_start" => {
                // Check if this is a tool_use block
                if let Some(content_block) = data.get("content_block") {
                    let block_type = content_block
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    if block_type == "tool_use" {
                        let id = content_block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = content_block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.active_tool = Some(ToolAccumulator {
                            id,
                            name,
                            input_json: String::new(),
                        });
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = data.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::TextDelta(text.to_string()));
                            }
                        }
                        "thinking_delta" => {
                            if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                                events.push(StreamEvent::ThinkingDelta(text.to_string()));
                            }
                        }
                        "input_json_delta" => {
                            // Accumulate tool input JSON chunks
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                if let Some(ref mut tool) = self.active_tool {
                                    tool.input_json.push_str(partial);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                // If we were accumulating a tool_use, emit it now
                if let Some(tool) = self.active_tool.take() {
                    events.push(StreamEvent::ToolUse {
                        id: tool.id,
                        name: tool.name,
                        input_json: tool.input_json,
                    });
                }
            }
            "message_delta" => {
                if let Some(usage) = data.get("usage") {
                    if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                        events.push(StreamEvent::Usage {
                            input_tokens: 0,
                            output_tokens: output as u32,
                        });
                    }
                }
            }
            "error" => {
                let error_msg = data
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                events.push(StreamEvent::Error(error_msg));
            }
            _ => {}
        }

        events
    }
}

// API request/response types

#[derive(Debug, Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    stream: bool,
    system: String,
    messages: Vec<ApiMessage>,
    tools: Vec<ApiTool>,
    thinking: ApiThinking,
}

#[derive(Debug, Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct ApiThinking {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: u32,
}

#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    content: ApiContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ApiContent {
    Text(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ua_protocol::{ConversationMessage, ShellContext, TerminalHistory};

    #[test]
    fn build_system_prompt_basic() {
        let request = AgentRequest {
            instruction: "list files".to_string(),
            context: ShellContext {
                cwd: "/home/user".to_string(),
                shell: "bash".to_string(),
                platform: "linux".to_string(),
                arch: "x86_64".to_string(),
                env_vars: vec![],
                terminal_size: (80, 24),
            },
            terminal_history: TerminalHistory::new(),
            conversation: vec![],
        };

        let prompt = build_system_prompt(&request);
        assert!(prompt.contains("Working directory: /home/user"));
        assert!(prompt.contains("Shell: bash"));
        assert!(prompt.contains("Platform: linux (x86_64)"));
        assert!(prompt.contains("Terminal: 80x24"));
        assert!(prompt.contains("shell tool"));
    }

    #[test]
    fn build_system_prompt_with_env() {
        let request = AgentRequest {
            instruction: "test".to_string(),
            context: ShellContext {
                cwd: "/tmp".to_string(),
                shell: "zsh".to_string(),
                platform: "darwin".to_string(),
                arch: "aarch64".to_string(),
                env_vars: vec![
                    ("PATH".to_string(), "/usr/bin".to_string()),
                    ("HOME".to_string(), "/home/user".to_string()),
                ],
                terminal_size: (120, 40),
            },
            terminal_history: TerminalHistory::new(),
            conversation: vec![],
        };

        let prompt = build_system_prompt(&request);
        assert!(prompt.contains("Environment variables:"));
        assert!(prompt.contains("PATH=/usr/bin"));
        assert!(prompt.contains("HOME=/home/user"));
    }

    #[test]
    fn build_system_prompt_with_history() {
        let request = AgentRequest {
            instruction: "test".to_string(),
            context: ShellContext::default(),
            terminal_history: TerminalHistory::from_lines(vec![
                "$ ls".to_string(),
                "file1.txt  file2.txt".to_string(),
            ]),
            conversation: vec![],
        };

        let prompt = build_system_prompt(&request);
        assert!(prompt.contains("Recent terminal output:"));
        assert!(prompt.contains("$ ls"));
        assert!(prompt.contains("file1.txt  file2.txt"));
    }

    #[test]
    fn build_messages_basic() {
        let request = AgentRequest {
            instruction: "list files".to_string(),
            context: ShellContext::default(),
            terminal_history: TerminalHistory::new(),
            conversation: vec![],
        };

        let messages = build_messages(&request);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
    }

    #[test]
    fn build_messages_with_conversation() {
        let request = AgentRequest {
            instruction: "now show hidden files".to_string(),
            context: ShellContext::default(),
            terminal_history: TerminalHistory::new(),
            conversation: vec![
                ConversationMessage::user("list files"),
                ConversationMessage::assistant("I'll use ls to list files"),
            ],
        };

        let messages = build_messages(&request);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn process_text_delta() {
        let event = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello"}}"#
                .to_string(),
        };

        let mut processor = SseProcessor::new();
        let events = processor.process(&event);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::TextDelta("Hello".to_string()));
    }

    #[test]
    fn process_thinking_delta() {
        let event = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#
                .to_string(),
        };

        let mut processor = SseProcessor::new();
        let events = processor.process(&event);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::ThinkingDelta("Let me think...".to_string())
        );
    }

    #[test]
    fn process_error_event() {
        let event = SseEvent {
            event_type: Some("error".to_string()),
            data:
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limited"}}"#
                    .to_string(),
        };

        let mut processor = SseProcessor::new();
        let events = processor.process(&event);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Error("Rate limited".to_string()));
    }

    #[test]
    fn process_tool_use_block() {
        let mut processor = SseProcessor::new();

        // content_block_start with tool_use
        let start = SseEvent {
            event_type: Some("content_block_start".to_string()),
            data: r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_123","name":"shell","input":{}}}"#.to_string(),
        };
        assert!(processor.process(&start).is_empty());
        assert!(processor.active_tool.is_some());

        // input_json_delta chunks
        let delta1 = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#.to_string(),
        };
        assert!(processor.process(&delta1).is_empty());

        let delta2 = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"ls /tmp\"}"}}"#.to_string(),
        };
        assert!(processor.process(&delta2).is_empty());

        // content_block_stop emits the ToolUse event
        let stop = SseEvent {
            event_type: Some("content_block_stop".to_string()),
            data: r#"{"type":"content_block_stop","index":1}"#.to_string(),
        };
        let events = processor.process(&stop);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::ToolUse {
                id: "toolu_123".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"ls /tmp"}"#.to_string(),
            }
        );
    }

    #[test]
    fn process_text_then_tool_use() {
        let mut processor = SseProcessor::new();

        // Text block first
        let text = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"I'll check the files."}}"#.to_string(),
        };
        let events = processor.process(&text);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::TextDelta("I'll check the files.".to_string())
        );

        // Then tool_use
        let start = SseEvent {
            event_type: Some("content_block_start".to_string()),
            data: r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_456","name":"shell","input":{}}}"#.to_string(),
        };
        processor.process(&start);

        let delta = SseEvent {
            event_type: Some("content_block_delta".to_string()),
            data: r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"command\":\"cat Cargo.toml\"}"}}"#.to_string(),
        };
        processor.process(&delta);

        let stop = SseEvent {
            event_type: Some("content_block_stop".to_string()),
            data: r#"{"type":"content_block_stop","index":1}"#.to_string(),
        };
        let events = processor.process(&stop);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::ToolUse {
                id: "toolu_456".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"cat Cargo.toml"}"#.to_string(),
            }
        );
    }

    #[test]
    fn build_shell_tool_structure() {
        let tool = build_shell_tool();
        assert_eq!(tool.name, "shell");
        assert!(tool.input_schema.get("properties").is_some());
        assert!(tool
            .input_schema
            .get("properties")
            .unwrap()
            .get("command")
            .is_some());
    }
}
