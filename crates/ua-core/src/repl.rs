use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use futures::StreamExt;
use tokio::runtime::Handle;
use ua_backend::AnthropicClient;
use ua_protocol::{ConversationMessage, StreamEvent};

use crate::config::Config;
use crate::context::{build_agent_request, OutputHistory};
use crate::display::PlanDisplay;
use crate::osc::{OscEvent, OscParser, TerminalState};
use crate::pty::PtySession;

enum Event {
    Stdin(Vec<u8>),
    PtyOutput(Vec<u8>),
    PtyEof,
    Resize(u16, u16),
}

/// Manages queued commands for OSC 133 sequenced execution.
///
/// Commands are dispatched one at a time. After the first (immediate) dispatch,
/// subsequent commands wait for the shell to signal readiness via OSC 133;B
/// (prompt rendered, input ready) before sending the next command.
/// This prevents double-echo that occurs when commands are sent before
/// ZLE/readline initialization.
struct CommandQueue {
    commands: VecDeque<String>,
    awaiting_ready: bool,
}

impl CommandQueue {
    fn new() -> Self {
        Self {
            commands: VecDeque::new(),
            awaiting_ready: false,
        }
    }

    /// Queue commands for execution.
    fn enqueue(&mut self, commands: impl IntoIterator<Item = String>) {
        self.commands.extend(commands);
    }

    /// Pop the first command for immediate dispatch (shell is already at prompt).
    fn pop_immediate(&mut self) -> Option<String> {
        self.commands.pop_front()
    }

    /// Process an OSC event. Returns a command to dispatch if the shell is ready.
    ///
    /// - `133;A` (prompt start): sets awaiting flag, does NOT dispatch
    /// - `133;B` (prompt ready): dispatches next command if awaiting
    fn handle_osc_event(&mut self, event: &OscEvent) -> Option<String> {
        match event {
            OscEvent::Osc133A => {
                if !self.commands.is_empty() {
                    self.awaiting_ready = true;
                }
                None
            }
            OscEvent::Osc133B => {
                if self.awaiting_ready {
                    self.awaiting_ready = false;
                    self.commands.pop_front()
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn clear(&mut self) {
        self.commands.clear();
        self.awaiting_ready = false;
    }
}

pub fn run_repl(config: &Config, debug_osc: bool, rt_handle: &Handle) -> io::Result<()> {
    let shell_cmd = config.shell_command();
    let (mut session, pty_reader) = PtySession::spawn(&shell_cmd, config.shell.integration)?;
    let mut parser = OscParser::new();
    let mut line_buf = String::new();
    let mut output_history = OutputHistory::new(config.context.max_terminal_lines);
    let mut conversation: Vec<ConversationMessage> = Vec::new();
    let mut terminal_size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut command_queue = CommandQueue::new();

    let (tx, rx) = mpsc::channel::<Event>();

    // Stdin reader thread
    let tx_stdin = tx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut buf = [0u8; 1024];
        loop {
            match handle.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx_stdin.send(Event::Stdin(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // PTY reader thread
    let tx_pty = tx.clone();
    thread::spawn(move || {
        let mut reader = pty_reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx_pty.send(Event::PtyEof);
                    break;
                }
                Ok(n) => {
                    if tx_pty.send(Event::PtyOutput(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = tx_pty.send(Event::PtyEof);
                    break;
                }
            }
        }
    });

    // Resize poller thread
    #[cfg(unix)]
    {
        let tx_sig = tx.clone();
        thread::spawn(move || {
            let mut last_size = crossterm::terminal::size().unwrap_or((80, 24));
            loop {
                thread::sleep(Duration::from_millis(250));
                if let Ok(size) = crossterm::terminal::size() {
                    if size != last_size {
                        last_size = size;
                        if tx_sig.send(Event::Resize(size.0, size.1)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    drop(tx);

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr();

    for event in rx {
        match event {
            Event::Stdin(data) => {
                let mut handled_instruction = false;

                // Track keystrokes in line buffer during prompt-related states:
                // - Prompt = after 133;A (prompt start)
                // - Input  = after 133;B (prompt rendered, ZLE/readline ready)
                // - Idle   = after 133;D but before 133;A (brief window)
                if matches!(
                    parser.terminal_state,
                    TerminalState::Prompt | TerminalState::Input | TerminalState::Idle
                ) {
                    for &b in &data {
                        match b {
                            b'\r' | b'\n' => {
                                let trimmed = line_buf.trim();
                                if let Some(instruction) = trimmed.strip_prefix('#') {
                                    let instruction = instruction.trim();
                                    if !instruction.is_empty() {
                                        handled_instruction = true;

                                        // Handle instruction via backend
                                        let result = handle_instruction(
                                            rt_handle,
                                            config,
                                            instruction,
                                            &output_history,
                                            &conversation,
                                            terminal_size,
                                            &mut stderr,
                                        );

                                        // Extract commands and queue for execution
                                        if let Ok(Some((commands, response_text))) = result {
                                            conversation
                                                .push(ConversationMessage::user(instruction));
                                            conversation.push(ConversationMessage::assistant(
                                                &response_text,
                                            ));

                                            if !commands.is_empty() {
                                                command_queue.enqueue(commands);

                                                // Clear shell's readline buffer (Ctrl+U),
                                                // then send the first command immediately
                                                // (shell is already at prompt with ZLE ready)
                                                let _ = session.write_all(b"\x15");
                                                if let Some(cmd) = command_queue.pop_immediate() {
                                                    let cmd = format!("{cmd}\n");
                                                    if let Err(e) =
                                                        session.write_all(cmd.as_bytes())
                                                    {
                                                        let _ = writeln!(
                                                            stderr,
                                                            "\r\n[ua] pty write error: {e}"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                line_buf.clear();
                            }
                            0x7f | 0x08 => {
                                line_buf.pop();
                            }
                            0x15 => {
                                // Ctrl+U
                                line_buf.clear();
                            }
                            0x17 => {
                                // Ctrl+W
                                let trimmed = line_buf.trim_end();
                                if let Some(pos) = trimmed.rfind(' ') {
                                    line_buf.truncate(pos + 1);
                                } else {
                                    line_buf.clear();
                                }
                            }
                            b if b >= 0x20 => {
                                line_buf.push(b as char);
                            }
                            _ => {}
                        }
                    }
                } else {
                    line_buf.clear();
                }

                // Forward input to PTY — but skip if we handled an instruction,
                // since we already sent the plan commands and don't want the
                // original Enter key to execute the # comment line.
                if !handled_instruction {
                    if let Err(e) = session.write_all(&data) {
                        if debug_osc {
                            let _ = writeln!(stderr, "\r[ua] pty write error: {e}");
                        }
                        break;
                    }
                }
            }
            Event::PtyOutput(data) => {
                stdout.write_all(&data)?;
                stdout.flush()?;

                // Feed to output history
                output_history.feed(&data);

                let events = parser.feed_bytes(&data);
                if debug_osc {
                    for evt in &events {
                        let _ = writeln!(
                            stderr,
                            "\r[ua:osc] {evt:?} -> state={:?}",
                            parser.terminal_state
                        );
                    }
                }

                for evt in &events {
                    if *evt == OscEvent::Osc133A {
                        line_buf.clear();
                    }

                    // OSC 133 sequencing: dispatch next command on 133;B
                    // (prompt rendered, ZLE/readline ready — no double-echo)
                    if let Some(cmd) = command_queue.handle_osc_event(evt) {
                        let cmd = format!("{cmd}\n");
                        if let Err(e) = session.write_all(cmd.as_bytes()) {
                            let _ = writeln!(stderr, "\r\n[ua] pty write error: {e}");
                            command_queue.clear();
                        }
                    }
                }
            }
            Event::PtyEof => break,
            Event::Resize(cols, rows) => {
                terminal_size = (cols, rows);
                if let Err(e) = session.resize(cols, rows) {
                    if debug_osc {
                        let _ = writeln!(stderr, "\r[ua] resize error: {e}");
                    }
                }
            }
        }

        // Check if child has exited
        if let Ok(Some(code)) = session.try_wait() {
            if debug_osc {
                let _ = writeln!(stderr, "\r[ua] child exited with code {code}");
            }
            break;
        }
    }

    Ok(())
}

/// Extract shell commands from fenced code blocks in text.
///
/// Parses ``` blocks (with optional language tag like ```bash).
/// Returns the content of each code block as a separate command string.
fn extract_commands(text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_block = false;
    let mut current_block = String::new();

    for line in text.lines() {
        if !in_block {
            if line.starts_with("```") {
                in_block = true;
                current_block.clear();
            }
        } else if line.starts_with("```") {
            // End of block
            let trimmed = current_block.trim().to_string();
            if !trimmed.is_empty() {
                commands.push(trimmed);
            }
            in_block = false;
        } else {
            if !current_block.is_empty() {
                current_block.push('\n');
            }
            current_block.push_str(line);
        }
    }

    commands
}

/// Handle an instruction by calling the backend.
/// Returns extracted commands and the full response text on success.
fn handle_instruction(
    rt_handle: &Handle,
    config: &Config,
    instruction: &str,
    history: &OutputHistory,
    conversation: &[ConversationMessage],
    terminal_size: (u16, u16),
    stderr: &mut io::Stderr,
) -> io::Result<Option<(Vec<String>, String)>> {
    // Resolve API key
    let api_key = match config.backend.anthropic.resolve_api_key() {
        Ok(key) => key,
        Err(e) => {
            let _ = writeln!(stderr, "\r\n[ua] error: {e}");
            return Ok(None);
        }
    };

    // Build request
    let request = build_agent_request(
        instruction,
        config,
        history,
        conversation.to_vec(),
        terminal_size,
    );

    // Create client and send request
    let client = AnthropicClient::with_model(&api_key, &config.backend.anthropic.model);
    let stream = client.send(&request);

    // Show immediate feedback
    let _ = write!(stderr, "\r\n[ua] thinking...");
    let _ = stderr.flush();

    // Process stream
    let mut display = PlanDisplay::new();
    let mut is_thinking = false;

    rt_handle.block_on(async {
        let mut stream = std::pin::pin!(stream);
        while let Some(event) = stream.next().await {
            match &event {
                StreamEvent::ThinkingDelta(text) => {
                    if !is_thinking {
                        is_thinking = true;
                        // Clear the early "thinking..." and start dimmed thinking output
                        let _ = write!(stderr, "\r\x1b[K\x1b[2m");
                        let _ = stderr.flush();
                    }
                    let raw_safe = text.replace('\n', "\r\n");
                    let _ = write!(stderr, "\x1b[2m{raw_safe}\x1b[0m");
                    let _ = stderr.flush();
                }
                StreamEvent::TextDelta(text) => {
                    if is_thinking {
                        is_thinking = false;
                        let _ = write!(stderr, "\x1b[0m\r\n");
                    } else if display.streaming_text.is_empty() {
                        // First text delta — clear the "thinking..." indicator
                        let _ = write!(stderr, "\r\x1b[K");
                    }
                    let raw_safe = text.replace('\n', "\r\n");
                    let _ = write!(stderr, "{raw_safe}");
                    let _ = stderr.flush();
                }
                _ => {}
            }

            display.handle_event(&event);
        }
    });

    // Trailing newline
    let _ = writeln!(stderr, "\r");

    match display.status {
        crate::display::DisplayStatus::Error(msg) => {
            let _ = writeln!(stderr, "\r\n[ua] error: {msg}");
            Ok(None)
        }
        _ => {
            let response_text = display.streaming_text.clone();
            let commands = extract_commands(&response_text);

            if !commands.is_empty() {
                let _ = writeln!(stderr, "\r[ua] commands:");
                for (i, cmd) in commands.iter().enumerate() {
                    let safe = cmd.replace('\n', "\r\n");
                    let _ = writeln!(stderr, "\r  {}. {safe}", i + 1);
                }
                let _ = writeln!(stderr, "\r[ua] executing...");
            }

            Ok(Some((commands, response_text)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::PlanDisplay;

    // --- extract_commands tests ---

    #[test]
    fn extract_single_command() {
        let text = "Here's how to list files:\n\n```\nls /tmp\n```\n";
        let commands = extract_commands(text);
        assert_eq!(commands, vec!["ls /tmp"]);
    }

    #[test]
    fn extract_multiple_commands() {
        let text =
            "First list, then show:\n\n```\nls /tmp\n```\n\nThen:\n\n```\ncat foo.txt\n```\n";
        let commands = extract_commands(text);
        assert_eq!(commands, vec!["ls /tmp", "cat foo.txt"]);
    }

    #[test]
    fn extract_with_language_tag() {
        let text = "```bash\nls -la\n```\n";
        let commands = extract_commands(text);
        assert_eq!(commands, vec!["ls -la"]);
    }

    #[test]
    fn extract_multiline_command() {
        let text = "```\nfor f in *.txt; do\n  echo $f\ndone\n```\n";
        let commands = extract_commands(text);
        assert_eq!(commands, vec!["for f in *.txt; do\n  echo $f\ndone"]);
    }

    #[test]
    fn extract_no_commands() {
        let text = "There are no commands to run here. Just an explanation.";
        let commands = extract_commands(text);
        assert!(commands.is_empty());
    }

    #[test]
    fn extract_empty_code_block() {
        let text = "```\n```\n";
        let commands = extract_commands(text);
        assert!(commands.is_empty());
    }

    #[test]
    fn extract_skips_unclosed_block() {
        let text = "```\nls /tmp\n";
        let commands = extract_commands(text);
        assert!(commands.is_empty());
    }

    #[test]
    fn extract_mixed_content() {
        let text = "I'll check the system.\n\n\
                     ```\nuname -a\n```\n\n\
                     This shows the kernel info.\n\n\
                     ```sh\ndf -h\n```\n\n\
                     And that shows disk usage.";
        let commands = extract_commands(text);
        assert_eq!(commands, vec!["uname -a", "df -h"]);
    }

    // --- CommandQueue tests ---

    #[test]
    fn command_queue_pop_immediate() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["ls".to_string(), "pwd".to_string()]);
        assert_eq!(queue.pop_immediate(), Some("ls".to_string()));
        assert!(!queue.is_empty()); // "pwd" still queued
    }

    #[test]
    fn command_queue_does_not_dispatch_on_133a() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["pwd".to_string()]);

        let result = queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(result, None);
        assert!(queue.awaiting_ready);
    }

    #[test]
    fn command_queue_dispatches_on_133b() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["pwd".to_string()]);

        queue.handle_osc_event(&OscEvent::Osc133A);
        let result = queue.handle_osc_event(&OscEvent::Osc133B);
        assert_eq!(result, Some("pwd".to_string()));
        assert!(!queue.awaiting_ready);
    }

    #[test]
    fn command_queue_sequential_dispatch() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec![
            "cmd1".to_string(),
            "cmd2".to_string(),
            "cmd3".to_string(),
        ]);

        // First command: immediate dispatch (shell is at prompt)
        assert_eq!(queue.pop_immediate(), Some("cmd1".to_string()));

        // cmd1 finishes → 133;D, 133;A (prompt start), then 133;B (prompt ready)
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            None
        );
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), None);
        assert!(queue.awaiting_ready);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            Some("cmd2".to_string())
        );

        // cmd2 finishes → same cycle
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            None
        );
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), None);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            Some("cmd3".to_string())
        );

        // No more commands
        assert!(queue.is_empty());
    }

    #[test]
    fn command_queue_133b_without_awaiting_is_noop() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd".to_string()]);

        // 133;B without preceding 133;A should not dispatch
        let result = queue.handle_osc_event(&OscEvent::Osc133B);
        assert_eq!(result, None);
        assert!(!queue.is_empty());
    }

    #[test]
    fn command_queue_empty_is_noop() {
        let mut queue = CommandQueue::new();

        // All events are no-ops when queue is empty
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), None);
        assert!(!queue.awaiting_ready);
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133B), None);
    }

    #[test]
    fn command_queue_clear_resets_state() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd1".to_string(), "cmd2".to_string()]);
        queue.handle_osc_event(&OscEvent::Osc133A);

        queue.clear();
        assert!(queue.is_empty());
        assert!(!queue.awaiting_ready);
    }

    #[test]
    fn command_queue_ignores_133c_and_133d() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd".to_string()]);
        queue.handle_osc_event(&OscEvent::Osc133A);

        // 133;C and 133;D should not dispatch
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133C), None);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            None
        );
        assert!(queue.awaiting_ready); // Still waiting for 133;B
    }

    // --- End-to-end tests: mock StreamEvent deltas → commands → dispatch ---

    #[test]
    fn mock_deltas_to_commands_single() {
        let events = vec![
            StreamEvent::TextDelta("Here's the command:\n\n```\nls /tmp\n```\n".to_string()),
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        let commands = extract_commands(&display.streaming_text);
        assert_eq!(commands, vec!["ls /tmp"]);
    }

    #[test]
    fn mock_deltas_to_commands_with_thinking() {
        let events = vec![
            StreamEvent::ThinkingDelta("Let me analyze...".to_string()),
            StreamEvent::TextDelta("I'll list the files:\n\n".to_string()),
            StreamEvent::TextDelta("```bash\nls -la /tmp\n```\n".to_string()),
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        let commands = extract_commands(&display.streaming_text);
        assert_eq!(commands, vec!["ls -la /tmp"]);
        // Thinking text should not leak into command extraction
        assert!(!display.thinking_text.is_empty());
        assert!(!display.streaming_text.contains("analyze"));
    }

    #[test]
    fn mock_deltas_multiple_commands_sequenced() {
        // Simulate LLM response with multiple commands
        let events = vec![
            StreamEvent::ThinkingDelta("I need to create and verify a file.".to_string()),
            StreamEvent::TextDelta(
                "First, create the file:\n\n```\ntouch /tmp/test.txt\n```\n\n".to_string(),
            ),
            StreamEvent::TextDelta(
                "Then verify it exists:\n\n```\nls -la /tmp/test.txt\n```\n".to_string(),
            ),
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        let commands = extract_commands(&display.streaming_text);
        assert_eq!(
            commands,
            vec!["touch /tmp/test.txt", "ls -la /tmp/test.txt"]
        );

        // Verify sequenced dispatch via CommandQueue
        let mut queue = CommandQueue::new();
        queue.enqueue(commands);

        // First command dispatched immediately (shell at prompt)
        assert_eq!(
            queue.pop_immediate(),
            Some("touch /tmp/test.txt".to_string())
        );

        // Second command waits for 133;A (prompt start) then 133;B (prompt ready)
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), None);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            Some("ls -la /tmp/test.txt".to_string())
        );

        assert!(queue.is_empty());
    }

    #[test]
    fn mock_deltas_no_commands() {
        let events = vec![
            StreamEvent::TextDelta(
                "There are no files to list. The directory is empty.".to_string(),
            ),
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        let commands = extract_commands(&display.streaming_text);
        assert!(commands.is_empty());
    }

    #[test]
    fn mock_deltas_streamed_code_block() {
        // Simulate text arriving in small chunks (like real SSE streaming)
        let events = vec![
            StreamEvent::TextDelta("Here".to_string()),
            StreamEvent::TextDelta("'s the command".to_string()),
            StreamEvent::TextDelta(":\n\n```".to_string()),
            StreamEvent::TextDelta("\nls".to_string()),
            StreamEvent::TextDelta(" /tmp\n".to_string()),
            StreamEvent::TextDelta("```\n".to_string()),
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        let commands = extract_commands(&display.streaming_text);
        assert_eq!(commands, vec!["ls /tmp"]);
    }

    #[test]
    fn mock_deltas_error_yields_no_commands() {
        let events = vec![
            StreamEvent::TextDelta("I'll help with".to_string()),
            StreamEvent::Error("Rate limited".to_string()),
        ];

        let mut display = PlanDisplay::new();
        for event in &events {
            display.handle_event(event);
        }

        // On error, streaming_text is partial — extract_commands should handle gracefully
        let commands = extract_commands(&display.streaming_text);
        assert!(commands.is_empty()); // No complete code block
    }
}
