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

pub fn run_repl(config: &Config, debug_osc: bool, rt_handle: &Handle) -> io::Result<()> {
    let shell_cmd = config.shell_command();
    let (mut session, pty_reader) = PtySession::spawn(&shell_cmd, config.shell.integration)?;
    let mut parser = OscParser::new();
    let mut line_buf = String::new();
    let mut output_history = OutputHistory::new(config.context.max_terminal_lines);
    let mut conversation: Vec<ConversationMessage> = Vec::new();
    let mut terminal_size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut pending_commands: VecDeque<String> = VecDeque::new();

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

                // Track keystrokes in line buffer during Prompt or Idle state
                // (Idle = after 133;D but before 133;A, a brief window)
                if matches!(
                    parser.terminal_state,
                    TerminalState::Prompt | TerminalState::Idle
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
                                                // Queue all commands
                                                pending_commands.extend(commands.into_iter());

                                                // Clear shell's readline buffer (Ctrl+U),
                                                // then send the first command
                                                let _ = session.write_all(b"\x15");
                                                if let Some(cmd) = pending_commands.pop_front() {
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

                        // OSC 133 sequencing: if we have pending commands,
                        // send the next one now that the shell is ready
                        if let Some(cmd) = pending_commands.pop_front() {
                            let cmd = format!("{cmd}\n");
                            if let Err(e) = session.write_all(cmd.as_bytes()) {
                                let _ = writeln!(stderr, "\r\n[ua] pty write error: {e}");
                                pending_commands.clear();
                            }
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
}
