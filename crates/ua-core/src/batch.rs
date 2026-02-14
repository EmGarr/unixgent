//! Non-interactive batch mode for agent execution.
//!
//! This is the `python -c` equivalent: receive an instruction, execute it
//! using the LLM + shell tools, print the final answer to stdout, and exit.
//! No PTY, no OSC parsing, no approval UI, no raw mode.

use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::process::Command;
use std::time::Instant;

use futures::StreamExt;
use ua_backend::AnthropicClient;
use ua_protocol::{ConversationMessage, StreamEvent, ToolResultRecord, ToolUseRecord};

use crate::audit::AuditLogger;
use crate::config::Config;
use crate::context::{
    build_agent_request, build_delegation_prompt, scrub_injection_markers, OutputHistory,
    TOOL_RESULT_PREFIX,
};
use crate::policy::{analyze_pipe_chain, RiskLevel};

const MAX_OUTPUT_BYTES: usize = 100_000;
const MAX_CONSECUTIVE_DENIALS: usize = 3;

/// Encapsulates all stderr formatting for batch mode output.
///
/// TTY output uses compact, single-line overwrite between persistent boundaries.
/// Non-TTY output uses plain text with one line per event and no ANSI codes.
pub struct BatchOutput<W: Write> {
    writer: W,
    is_tty: bool,
    depth: u32,
    start_time: Instant,
    task_summary: String,
    step_count: usize,
    term_width: u16,
}

impl<W: Write> BatchOutput<W> {
    pub fn new(writer: W, is_tty: bool, depth: u32, instruction: &str) -> Self {
        let term_width = if is_tty {
            crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
        } else {
            80
        };

        let summary: String = instruction.chars().take(60).collect();
        let task_summary = if instruction.chars().count() > 60 {
            format!("{summary}...")
        } else {
            summary
        };

        Self {
            writer,
            is_tty,
            depth,
            start_time: Instant::now(),
            task_summary,
            step_count: 0,
            term_width,
        }
    }

    /// Format the depth prefix, e.g. `[ua:d0]`.
    fn prefix(&self) -> String {
        format!("[ua:d{}]", self.depth)
    }

    /// Prefix with dim cyan color for TTY mode.
    fn colored_prefix(&self) -> String {
        if self.is_tty {
            format!("\x1b[2m\x1b[36m{}\x1b[0m", self.prefix())
        } else {
            self.prefix()
        }
    }

    /// Truncate a string to fit within terminal width minus the prefix and padding.
    fn truncate_to_width(&self, s: &str) -> String {
        // prefix + space + content
        let prefix_len = self.prefix().len() + 1;
        let max_content = (self.term_width as usize).saturating_sub(prefix_len);
        if s.len() > max_content && max_content > 3 {
            let mut truncated: String = s.chars().take(max_content - 3).collect();
            truncated.push_str("...");
            truncated
        } else {
            s.to_string()
        }
    }

    /// Emit the start boundary line (persists).
    pub fn emit_start(&mut self) {
        if self.is_tty {
            let _ = writeln!(
                self.writer,
                "{} \x1b[36m---\x1b[0m \"{}\"",
                self.colored_prefix(),
                self.task_summary
            );
        } else {
            let _ = writeln!(
                self.writer,
                "{} --- \"{}\"",
                self.prefix(),
                self.task_summary
            );
        }
    }

    /// Emit a thinking/progress indicator (overwritten in TTY mode).
    pub fn emit_thinking(&mut self, iteration: usize) {
        self.step_count = iteration;
        if self.is_tty {
            let _ = write!(
                self.writer,
                "\r\x1b[K{} \x1b[2m({}) thinking...\x1b[0m",
                self.colored_prefix(),
                iteration + 1,
            );
        } else {
            let _ = writeln!(
                self.writer,
                "{} ({}) thinking...",
                self.prefix(),
                iteration + 1,
            );
        }
        let _ = self.writer.flush();
    }

    /// Emit a command being executed (overwritten in TTY mode).
    pub fn emit_command(&mut self, cmd: &str, iteration: usize) {
        self.step_count = iteration + 1;
        let display_cmd = self.truncate_to_width(cmd);
        if self.is_tty {
            let _ = write!(
                self.writer,
                "\r\x1b[K{} \x1b[2m({}) {}\x1b[0m",
                self.colored_prefix(),
                iteration + 1,
                display_cmd,
            );
            let _ = self.writer.flush();
        } else {
            let _ = writeln!(self.writer, "{} $ {}", self.prefix(), display_cmd,);
        }
    }

    /// Emit a denied command (persists — red).
    pub fn emit_denied(&mut self, cmd: &str) {
        let display_cmd = self.truncate_to_width(cmd);
        if self.is_tty {
            let _ = writeln!(
                self.writer,
                "\r\x1b[K{} \x1b[31mDENIED: {}\x1b[0m",
                self.colored_prefix(),
                display_cmd,
            );
        } else {
            let _ = writeln!(self.writer, "{} DENIED: {}", self.prefix(), display_cmd,);
        }
    }

    /// Emit an error (persists — red).
    pub fn emit_error(&mut self, msg: &str) {
        if self.is_tty {
            let _ = writeln!(
                self.writer,
                "\r\x1b[K{} \x1b[31merror: {}\x1b[0m",
                self.colored_prefix(),
                msg,
            );
        } else {
            let _ = writeln!(self.writer, "{} error: {}", self.prefix(), msg,);
        }
    }

    /// Emit the done boundary line (persists).
    pub fn emit_done(&mut self) {
        let elapsed = self.start_time.elapsed().as_secs();
        if self.is_tty {
            let _ = writeln!(
                self.writer,
                "\r\x1b[K{} \x1b[36m---\x1b[0m \x1b[2mdone ({elapsed}s, {} steps)\x1b[0m",
                self.colored_prefix(),
                self.step_count,
            );
        } else {
            let _ = writeln!(
                self.writer,
                "{} --- done ({elapsed}s, {} steps)",
                self.prefix(),
                self.step_count,
            );
        }
    }
}

/// Build the batch-mode system prompt.
fn build_batch_system_prompt(depth: u32, max_depth: u32) -> String {
    let mut prompt = String::from(
        "You are running in non-interactive batch mode.\n\
         \n\
         RULES:\n\
         1. Each shell tool call costs one iteration. Be efficient.\n\
         2. Combine commands with && or ; when possible.\n\
         3. After gathering enough information, STOP using tools and provide \
         your final answer as plain text.\n\
         4. Your stdout is consumed by the caller. Make answers complete and \
         self-contained.\n\
         5. Use as many iterations as required to complete the task thoroughly.",
    );

    if let Some(delegation) = build_delegation_prompt(depth, max_depth) {
        prompt.push_str("\n\n");
        prompt.push_str(&delegation);
    }

    prompt
}

/// Run the agent in non-interactive batch mode.
///
/// Streams LLM responses, executes tool calls via `sh -c`, feeds results back,
/// and prints the final text answer to stdout. Returns the exit code.
pub async fn run_batch(config: &Config, instruction: &str, depth: u32) -> i32 {
    let is_tty = std::io::stderr().is_terminal();
    let mut output = BatchOutput::new(std::io::stderr(), is_tty, depth, instruction);

    let system_extra = build_batch_system_prompt(depth, config.security.max_agent_depth);

    // Resolve API key
    let api_key = match config.backend.anthropic.resolve_api_key() {
        Ok(key) => key,
        Err(e) => {
            output.emit_error(&e.to_string());
            return 1;
        }
    };

    let client = AnthropicClient::with_model(&api_key, &config.backend.anthropic.model);

    // Initialize audit logger
    let mut audit = if config.security.audit_enabled {
        let path = config.security.resolve_audit_path();
        match AuditLogger::new(&path) {
            Ok(logger) => logger,
            Err(e) => {
                let mut msg = String::new();
                let _ = write!(msg, "warning: audit log: {e}");
                output.emit_error(&msg);
                AuditLogger::noop()
            }
        }
    } else {
        AuditLogger::noop()
    };

    let empty_history = OutputHistory::new(0);
    let mut conversation: Vec<ConversationMessage> = Vec::new();
    let mut current_instruction = instruction.to_string();
    let mut consecutive_denials: usize = 0;

    output.emit_start();

    let mut iteration: usize = 0;
    loop {
        // Build request
        let mut request = build_agent_request(
            &current_instruction,
            config,
            &empty_history,
            conversation.clone(),
            (80, 24),
            None, // No PTY child in batch mode
        );
        request.system_prompt_extra = Some(system_extra.clone());

        // Stream response
        let stream = client.send(&request);
        let mut stream = std::pin::pin!(stream);

        let mut text = String::new();
        let mut tool_uses: Vec<ToolUseRecord> = Vec::new();
        let mut tool_commands: Vec<String> = Vec::new();

        output.emit_thinking(iteration);

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::TextDelta(t) => text.push_str(&t),
                StreamEvent::ToolUse {
                    id,
                    name,
                    input_json,
                } => {
                    tool_uses.push(ToolUseRecord {
                        id,
                        name,
                        input_json: input_json.clone(),
                    });
                    if let Ok(input) = serde_json::from_str::<serde_json::Value>(&input_json) {
                        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                            tool_commands.push(cmd.to_string());
                        }
                    }
                }
                StreamEvent::Error(e) => {
                    output.emit_error(&e);
                    return 1;
                }
                _ => {}
            }
        }

        // Push conversation history
        if iteration == 0 {
            conversation.push(ConversationMessage::user(&current_instruction));
        }

        if !tool_uses.is_empty() {
            conversation.push(ConversationMessage::assistant_with_tool_use(
                &text,
                tool_uses.clone(),
            ));
        } else if !text.is_empty() {
            conversation.push(ConversationMessage::assistant(&text));
        }

        // No tool calls = final answer
        if tool_commands.is_empty() {
            output.emit_done();
            print!("{text}");
            return 0;
        }

        // Classify and check deny list
        let tool_use_ids: Vec<String> = tool_uses.iter().map(|t| t.id.clone()).collect();
        let risk_levels: Vec<RiskLevel> = tool_commands
            .iter()
            .map(|cmd| analyze_pipe_chain(cmd))
            .collect();
        let risk_labels: Vec<&str> = risk_levels.iter().map(|r| r.as_str()).collect();

        audit.log_proposed(iteration, &tool_commands, &risk_labels, "llm");

        // Check for denied commands
        let mut any_denied = false;
        for (i, risk) in risk_levels.iter().enumerate() {
            if *risk == RiskLevel::Denied {
                output.emit_denied(&tool_commands[i]);
                audit.log_blocked(&tool_commands[i], risk.as_str(), "denied by policy");
                any_denied = true;
            }
        }

        if any_denied {
            consecutive_denials += 1;
            if consecutive_denials >= MAX_CONSECUTIVE_DENIALS {
                let mut msg = String::new();
                let _ = write!(
                    msg,
                    "{MAX_CONSECUTIVE_DENIALS} consecutive denials, aborting"
                );
                output.emit_error(&msg);
                return 1;
            }
            let denial_msg = "Command blocked by security policy. Suggest a safer alternative.";
            let tool_results: Vec<ToolResultRecord> = tool_use_ids
                .iter()
                .map(|id| ToolResultRecord {
                    tool_use_id: id.clone(),
                    content: denial_msg.to_string(),
                })
                .collect();
            conversation.push(ConversationMessage::tool_result(tool_results));
            current_instruction = String::new();
            continue;
        }

        consecutive_denials = 0;
        audit.log_approved(iteration, "batch", "non-interactive auto-approve");

        // Execute each command
        let mut all_results: Vec<ToolResultRecord> = Vec::new();
        for (i, cmd) in tool_commands.iter().enumerate() {
            output.emit_command(cmd, iteration);
            let start = Instant::now();

            let cmd_output = Command::new("sh").arg("-c").arg(cmd).output();

            let duration_ms = start.elapsed().as_millis() as u64;

            match cmd_output {
                Ok(out) => {
                    let exit_code = out.status.code();
                    audit.log_executed(cmd, exit_code, duration_ms);

                    let mut stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr_text = String::from_utf8_lossy(&out.stderr).to_string();

                    // Cap output size
                    if stdout.len() > MAX_OUTPUT_BYTES {
                        let total = stdout.len();
                        stdout.truncate(MAX_OUTPUT_BYTES);
                        stdout.push_str(&format!("\n[truncated, {total} bytes total]"));
                    }

                    let mut result = String::from(TOOL_RESULT_PREFIX);
                    if !stdout.is_empty() {
                        result.push_str(&stdout);
                    }
                    if !stderr_text.is_empty() {
                        if !stdout.is_empty() {
                            result.push('\n');
                        }
                        result.push_str("STDERR:\n");
                        result.push_str(&stderr_text);
                    }
                    if let Some(code) = exit_code {
                        if code != 0 {
                            result.push_str(&format!("\n[exit code: {code}]"));
                        }
                    }

                    let scrubbed = scrub_injection_markers(&result);
                    all_results.push(ToolResultRecord {
                        tool_use_id: tool_use_ids[i].clone(),
                        content: scrubbed,
                    });
                }
                Err(e) => {
                    audit.log_executed(cmd, None, duration_ms);
                    all_results.push(ToolResultRecord {
                        tool_use_id: tool_use_ids[i].clone(),
                        content: format!("Failed to execute: {e}"),
                    });
                }
            }
        }

        conversation.push(ConversationMessage::tool_result(all_results));
        current_instruction = String::new();

        // Evict oldest turns
        let max = config.context.max_conversation_turns;
        if conversation.len() > max {
            conversation.drain(..conversation.len() - max);
        }

        iteration += 1;
    }
}

// Need IsTerminal for `std::io::stderr().is_terminal()`
use std::io::IsTerminal;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_output(is_tty: bool, depth: u32, instruction: &str) -> BatchOutput<Vec<u8>> {
        BatchOutput::new(Vec::new(), is_tty, depth, instruction)
    }

    fn output_str(output: &BatchOutput<Vec<u8>>) -> String {
        String::from_utf8_lossy(&output.writer).to_string()
    }

    // --- TTY mode tests ---

    #[test]
    fn tty_start_has_ansi_and_boundary() {
        let mut out = make_output(true, 1, "find all TODO comments");
        out.emit_start();
        let s = output_str(&out);
        assert!(s.contains("[ua:d1]"), "should have depth prefix");
        assert!(s.contains("---"), "should have boundary marker");
        assert!(s.contains("find all TODO comments"), "should have summary");
        assert!(s.contains("\x1b["), "TTY should have ANSI codes");
        assert!(s.ends_with('\n'), "start should persist with newline");
    }

    #[test]
    fn tty_thinking_uses_carriage_return() {
        let mut out = make_output(true, 0, "test");
        out.emit_thinking(2);
        let s = output_str(&out);
        assert!(s.starts_with("\r\x1b[K"), "should start with line clear");
        assert!(s.contains("(3)"), "should show iteration count");
        assert!(s.contains("thinking"), "should say thinking");
        assert!(
            !s.ends_with('\n'),
            "TTY thinking should not end with newline"
        );
    }

    #[test]
    fn tty_command_uses_carriage_return() {
        let mut out = make_output(true, 2, "test");
        out.emit_command("grep -rn TODO src/", 3);
        let s = output_str(&out);
        assert!(s.starts_with("\r\x1b[K"), "should start with line clear");
        assert!(s.contains("[ua:d2]"), "should have depth prefix");
        assert!(s.contains("(4)"), "should show iteration");
        assert!(s.contains("grep -rn TODO src/"), "should have command");
        assert!(
            !s.ends_with('\n'),
            "TTY command should not end with newline"
        );
    }

    #[test]
    fn tty_denied_persists_red() {
        let mut out = make_output(true, 0, "test");
        out.emit_denied("rm -rf /");
        let s = output_str(&out);
        assert!(s.contains("\x1b[31m"), "should be red");
        assert!(s.contains("DENIED"), "should say DENIED");
        assert!(s.contains("rm -rf /"), "should have command");
        assert!(s.ends_with('\n'), "denied should persist");
    }

    #[test]
    fn tty_error_persists_red() {
        let mut out = make_output(true, 0, "test");
        out.emit_error("connection failed");
        let s = output_str(&out);
        assert!(s.contains("\x1b[31m"), "should be red");
        assert!(s.contains("error:"), "should say error:");
        assert!(s.contains("connection failed"), "should have message");
        assert!(s.ends_with('\n'), "error should persist");
    }

    #[test]
    fn tty_done_has_boundary_and_elapsed() {
        let mut out = make_output(true, 1, "test");
        out.step_count = 3;
        out.emit_done();
        let s = output_str(&out);
        assert!(s.contains("---"), "should have boundary");
        assert!(s.contains("done"), "should say done");
        assert!(s.contains("3 steps"), "should show step count");
        assert!(s.contains("s,"), "should show elapsed time");
        assert!(s.ends_with('\n'), "done should persist");
    }

    // --- Non-TTY mode tests ---

    #[test]
    fn non_tty_start_no_ansi() {
        let mut out = make_output(false, 1, "find all TODO comments");
        out.emit_start();
        let s = output_str(&out);
        assert!(s.contains("[ua:d1]"), "should have depth prefix");
        assert!(s.contains("---"), "should have boundary");
        assert!(s.contains("find all TODO comments"), "should have summary");
        assert!(!s.contains("\x1b["), "non-TTY should not have ANSI codes");
    }

    #[test]
    fn non_tty_thinking_uses_newline() {
        let mut out = make_output(false, 0, "test");
        out.emit_thinking(2);
        let s = output_str(&out);
        assert!(!s.contains("\r\x1b[K"), "non-TTY should not use line clear");
        assert!(s.contains("(3)"), "should show iteration");
        assert!(s.ends_with('\n'), "non-TTY should use newline");
    }

    #[test]
    fn non_tty_command_uses_dollar_sign() {
        let mut out = make_output(false, 0, "test");
        out.emit_command("ls -la", 0);
        let s = output_str(&out);
        assert!(s.contains("$ ls -la"), "non-TTY should show $ prefix");
        assert!(!s.contains("\x1b["), "non-TTY should not have ANSI codes");
        assert!(s.ends_with('\n'), "non-TTY should use newline");
    }

    #[test]
    fn non_tty_denied_no_ansi() {
        let mut out = make_output(false, 0, "test");
        out.emit_denied("rm -rf /");
        let s = output_str(&out);
        assert!(s.contains("DENIED: rm -rf /"), "should show denial");
        assert!(!s.contains("\x1b["), "non-TTY should not have ANSI codes");
    }

    #[test]
    fn non_tty_error_no_ansi() {
        let mut out = make_output(false, 0, "test");
        out.emit_error("something broke");
        let s = output_str(&out);
        assert!(s.contains("error: something broke"), "should show error");
        assert!(!s.contains("\x1b["), "non-TTY should not have ANSI codes");
    }

    #[test]
    fn non_tty_done_no_ansi() {
        let mut out = make_output(false, 0, "test");
        out.step_count = 5;
        out.emit_done();
        let s = output_str(&out);
        assert!(s.contains("--- done"), "should have boundary");
        assert!(s.contains("5 steps"), "should show step count");
        assert!(!s.contains("\x1b["), "non-TTY should not have ANSI codes");
    }

    // --- Truncation tests ---

    #[test]
    fn command_truncated_to_width() {
        let mut out = make_output(false, 0, "test");
        out.term_width = 30;
        let long_cmd = "a]".repeat(50);
        out.emit_command(&long_cmd, 0);
        let s = output_str(&out);
        // The command portion should be truncated
        assert!(s.contains("..."), "long command should be truncated");
        // Total line width should be bounded
        for line in s.lines() {
            assert!(
                line.len() <= 35,
                "line should be roughly within term width, got {}",
                line.len()
            );
        }
    }

    // --- Depth label tests ---

    #[test]
    fn depth_labels() {
        for depth in 0..3 {
            let out = make_output(false, depth, "test");
            assert_eq!(out.prefix(), format!("[ua:d{depth}]"));
        }
    }

    // --- Task summary truncation ---

    #[test]
    fn task_summary_truncated_at_60_chars() {
        let long_instruction = "x".repeat(100);
        let out = make_output(false, 0, &long_instruction);
        assert!(
            out.task_summary.len() <= 64, // 60 chars + "..."
            "summary should be truncated, got len={}",
            out.task_summary.len()
        );
        assert!(out.task_summary.ends_with("..."));
    }

    #[test]
    fn task_summary_short_instruction_unchanged() {
        let out = make_output(false, 0, "list files");
        assert_eq!(out.task_summary, "list files");
    }

    // --- Step count tracking ---

    #[test]
    fn emit_command_increments_step_count() {
        let mut out = make_output(false, 0, "test");
        assert_eq!(out.step_count, 0);
        out.emit_command("ls", 0);
        assert_eq!(out.step_count, 1);
        out.emit_command("pwd", 1);
        assert_eq!(out.step_count, 2);
    }

    // --- System prompt tests ---

    #[test]
    fn batch_system_prompt_mentions_efficiency() {
        let prompt = build_batch_system_prompt(0, 3);
        assert!(prompt.contains("Be efficient"), "should mention efficiency");
        assert!(
            prompt.contains("Combine commands"),
            "should mention combining"
        );
        assert!(!prompt.contains("budget"), "should not mention a budget");
    }

    #[test]
    fn batch_system_prompt_includes_delegation_when_allowed() {
        let prompt = build_batch_system_prompt(0, 3);
        assert!(
            prompt.contains("delegate subtasks"),
            "should include delegation"
        );
    }

    #[test]
    fn batch_system_prompt_no_delegation_at_depth_limit() {
        let prompt = build_batch_system_prompt(2, 3);
        assert!(
            !prompt.contains("delegate subtasks"),
            "should not include delegation at depth limit"
        );
    }
}
