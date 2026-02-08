use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use futures::StreamExt;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use ua_backend::AnthropicClient;
use ua_protocol::{ConversationMessage, StreamEvent, ToolResultRecord, ToolUseRecord};

use crate::audit::AuditLogger;
use crate::config::Config;
use crate::context::{
    build_agent_request, scrub_injection_markers, OutputHistory, TOOL_RESULT_PREFIX,
};
use crate::display::PlanDisplay;
use crate::osc::{OscEvent, OscParser, TerminalState};
use crate::policy::{analyze_pipe_chain, validate_arguments, ArgumentSafety, RiskLevel};
use crate::pty::PtySession;

enum Event {
    Stdin(Vec<u8>),
    PtyOutput(Vec<u8>),
    PtyEof,
    Resize(u16, u16),
    /// A streaming event from the backend (forwarded from a spawned tokio task).
    BackendChunk(StreamEvent),
    /// The backend stream has finished (either completed or was cancelled).
    BackendDone,
}

/// Agent state machine — drives the main event loop.
enum AgentState {
    /// Waiting for user input. Normal shell operation.
    Idle,
    /// Streaming response from the LLM backend.
    Streaming {
        /// Send on this channel to cancel the backend task.
        cancel_tx: Option<oneshot::Sender<()>>,
        /// Accumulates the streamed response.
        display: PlanDisplay,
        /// Whether we're currently in thinking mode (for display).
        is_thinking: bool,
        /// Current agentic loop iteration (0-based).
        iteration: usize,
        /// Commands captured from tool_use events.
        tool_commands: Vec<String>,
        /// Full tool_use records for conversation history.
        tool_uses: Vec<ToolUseRecord>,
    },
    /// Awaiting user approval of proposed commands.
    Approving {
        /// Commands extracted from the LLM response.
        commands: Vec<String>,
        /// Current agentic loop iteration.
        iteration: usize,
        /// Tool use IDs for building tool_result messages.
        tool_use_ids: Vec<String>,
        /// Whether any command in the batch is Privileged.
        has_privileged: bool,
        /// Buffer for typing "yes" on privileged commands.
        yes_buffer: String,
    },
    /// Commands are being executed in the PTY.
    Executing {
        /// Current agentic loop iteration.
        iteration: usize,
        /// Captures output during execution for observation.
        capture: OutputHistory,
        /// Tool use IDs for building tool_result messages.
        tool_use_ids: Vec<String>,
    },
}

/// Result of processing an OSC event through the command queue.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QueueEvent {
    /// Send this command to the PTY.
    Dispatch(String),
    /// All queued commands have finished executing.
    AllDone,
    /// A command failed with a non-zero exit code. Queue is cleared.
    Failed(i32),
    /// Nothing to do.
    None,
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
    /// True while commands are being executed (from enqueue until AllDone).
    executing: bool,
    /// Exit code from the most recent 133;D event.
    last_exit_code: Option<i32>,
}

impl CommandQueue {
    fn new() -> Self {
        Self {
            commands: VecDeque::new(),
            awaiting_ready: false,
            executing: false,
            last_exit_code: None,
        }
    }

    /// Queue commands for execution and mark as executing.
    fn enqueue(&mut self, commands: impl IntoIterator<Item = String>) {
        self.commands.extend(commands);
        if !self.commands.is_empty() {
            self.executing = true;
        }
    }

    /// Pop the first command for immediate dispatch (shell is already at prompt).
    fn pop_immediate(&mut self) -> Option<String> {
        self.commands.pop_front()
    }

    /// Process an OSC event. Returns a `QueueEvent` indicating what to do.
    ///
    /// - `133;D` (command done): stores exit code
    /// - `133;A` (prompt start): when executing, ALWAYS sets awaiting_ready
    ///   (even if queue is empty — this is how we detect last command done)
    /// - `133;B` (prompt ready): checks exit code — if non-zero, clears queue
    ///   and returns Failed; otherwise dispatches next command or signals AllDone
    fn handle_osc_event(&mut self, event: &OscEvent) -> QueueEvent {
        match event {
            OscEvent::Osc133D { exit_code } => {
                self.last_exit_code = *exit_code;
                QueueEvent::None
            }
            OscEvent::Osc133A => {
                if self.executing {
                    self.awaiting_ready = true;
                }
                QueueEvent::None
            }
            OscEvent::Osc133B => {
                if self.awaiting_ready {
                    self.awaiting_ready = false;

                    // Check if last command failed
                    if let Some(code) = self.last_exit_code {
                        if code != 0 {
                            let remaining = self.commands.len();
                            self.clear();
                            if remaining > 0 {
                                return QueueEvent::Failed(code);
                            }
                            // Last command failed but queue was already empty —
                            // treat as AllDone (the caller sees the failure in output)
                            return QueueEvent::AllDone;
                        }
                    }

                    match self.commands.pop_front() {
                        Some(cmd) => QueueEvent::Dispatch(cmd),
                        None => {
                            // Queue empty + was executing = all done
                            self.executing = false;
                            QueueEvent::AllDone
                        }
                    }
                } else {
                    QueueEvent::None
                }
            }
            _ => QueueEvent::None,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn clear(&mut self) {
        self.commands.clear();
        self.awaiting_ready = false;
        self.executing = false;
        self.last_exit_code = None;
    }
}

/// Maximum number of agentic loop iterations before stopping.
const MAX_AGENT_ITERATIONS: usize = 10;

pub fn run_repl(config: &Config, debug_osc: bool, rt_handle: &Handle) -> io::Result<()> {
    let shell_cmd = config.shell_command();
    let (mut session, pty_reader) = PtySession::spawn(&shell_cmd, config.shell.integration)?;
    let mut parser = OscParser::new();
    let mut line_buf = String::new();
    let mut output_history = OutputHistory::new(config.context.max_terminal_lines);
    let mut conversation: Vec<ConversationMessage> = Vec::new();
    let mut terminal_size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut command_queue = CommandQueue::new();
    let mut state = AgentState::Idle;
    // Instruction text saved across the state transition (Idle → Streaming).
    let mut pending_instruction: Option<String> = None;

    // Initialize audit logger
    let mut audit = if config.security.audit_enabled {
        let path = config.security.resolve_audit_path();
        match AuditLogger::new(&path) {
            Ok(logger) => logger,
            Err(e) => {
                eprintln!(
                    "[ua] warning: failed to open audit log {}: {e}",
                    path.display()
                );
                AuditLogger::noop()
            }
        }
    } else {
        AuditLogger::noop()
    };

    let (tx, rx) = mpsc::channel::<Event>();

    // Keep one sender alive for start_streaming() to clone from.
    let tx_for_streaming = tx.clone();

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

    // PTY reader thread (store handle for join on exit)
    let tx_pty = tx.clone();
    let pty_reader_handle: JoinHandle<()> = thread::spawn(move || {
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

    // SIGWINCH handler thread (replaces 250ms polling)
    #[cfg(unix)]
    {
        use signal_hook::consts::SIGWINCH;
        use signal_hook::iterator::Signals;

        let tx_sig = tx.clone();
        thread::spawn(move || {
            let mut signals = match Signals::new([SIGWINCH]) {
                Ok(s) => s,
                Err(_) => return,
            };
            for _ in signals.forever() {
                if let Ok(size) = crossterm::terminal::size() {
                    if tx_sig.send(Event::Resize(size.0, size.1)).is_err() {
                        break;
                    }
                }
            }
        });
    }

    drop(tx);

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr();

    while let Ok(event) = rx.recv() {
        match event {
            Event::Stdin(data) => {
                match state {
                    AgentState::Idle => {
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
                                                pending_instruction = Some(instruction.to_string());

                                                // Clear shell readline (removes the # text)
                                                let _ = session.write_all(b"\x15");

                                                // Start streaming from the backend
                                                state = start_streaming(
                                                    rt_handle,
                                                    config,
                                                    instruction,
                                                    &output_history,
                                                    &conversation,
                                                    terminal_size,
                                                    0,
                                                    &tx_for_streaming,
                                                    &mut stderr,
                                                );
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
                            if debug_osc {
                                for &b in &data {
                                    if b == b'#' {
                                        let _ = writeln!(
                                            stderr,
                                            "\r[ua:osc] '#' ignored (state={:?})",
                                            parser.terminal_state
                                        );
                                    }
                                }
                            }
                            line_buf.clear();
                        }

                        // Forward input to PTY unless we handled an instruction
                        if !handled_instruction {
                            if let Err(e) = session.write_all(&data) {
                                if debug_osc {
                                    let _ = writeln!(stderr, "\r[ua] pty write error: {e}");
                                }
                                break;
                            }
                        }
                    }
                    AgentState::Streaming {
                        ref mut cancel_tx, ..
                    } => {
                        // Check for Ctrl+C (0x03)
                        if data.contains(&0x03) {
                            // Cancel the backend task
                            if let Some(tx) = cancel_tx.take() {
                                let _ = tx.send(());
                            }
                            let _ = writeln!(stderr, "\r\n[ua] cancelled\r");
                            state = AgentState::Idle;
                            pending_instruction = None;
                        }
                        // Other input is ignored during streaming
                    }
                    AgentState::Approving {
                        ref has_privileged,
                        ref mut yes_buffer,
                        ..
                    } => {
                        let need_yes =
                            *has_privileged && config.security.require_yes_for_privileged;

                        if need_yes {
                            // Privileged: require typing "yes" + Enter
                            for &b in &data {
                                match b {
                                    b'\r' | b'\n' => {
                                        if yes_buffer.trim() == "yes" {
                                            // Approved
                                            if let AgentState::Approving {
                                                commands,
                                                iteration,
                                                tool_use_ids,
                                                ..
                                            } = std::mem::replace(&mut state, AgentState::Idle)
                                            {
                                                audit.log_approved(
                                                    iteration,
                                                    "typed_yes",
                                                    "user typed yes",
                                                );
                                                let _ = writeln!(stderr, "\r\n[ua] executing...\r");

                                                command_queue.enqueue(commands);
                                                if let Some(cmd) = command_queue.pop_immediate() {
                                                    let cmd = format!("{cmd}\n");
                                                    if let Err(e) =
                                                        session.write_all(cmd.as_bytes())
                                                    {
                                                        let _ = writeln!(
                                                            stderr,
                                                            "\r\n[ua] pty write error: {e}"
                                                        );
                                                        command_queue.clear();
                                                    } else {
                                                        state = AgentState::Executing {
                                                            iteration,
                                                            capture: OutputHistory::new(200),
                                                            tool_use_ids,
                                                        };
                                                    }
                                                }
                                            }
                                        } else {
                                            if let AgentState::Approving { iteration, .. } = &state
                                            {
                                                audit.log_denied(
                                                    *iteration,
                                                    "typed_no",
                                                    "user did not type yes",
                                                );
                                            }
                                            let _ = writeln!(
                                                stderr,
                                                "\r\n[ua] skipped (type 'yes' to approve)\r"
                                            );
                                            state = AgentState::Idle;
                                        }
                                        break;
                                    }
                                    0x7f | 0x08 => {
                                        // Backspace
                                        if yes_buffer.pop().is_some() {
                                            let _ = write!(stderr, "\x08 \x08");
                                            let _ = stderr.flush();
                                        }
                                    }
                                    0x03 => {
                                        // Ctrl-C
                                        if let AgentState::Approving { iteration, .. } = &state {
                                            audit.log_denied(
                                                *iteration,
                                                "ctrl_c",
                                                "user cancelled",
                                            );
                                        }
                                        let _ = writeln!(stderr, "\r\n[ua] cancelled\r");
                                        state = AgentState::Idle;
                                        break;
                                    }
                                    b if b >= 0x20 => {
                                        yes_buffer.push(b as char);
                                        let _ = write!(stderr, "{}", b as char);
                                        let _ = stderr.flush();
                                    }
                                    _ => {}
                                }
                            }
                        } else {
                            // Normal: single keystroke approval
                            for &b in &data {
                                match b {
                                    b'y' | b'Y' | b'\r' | b'\n' => {
                                        if let AgentState::Approving {
                                            commands,
                                            iteration,
                                            tool_use_ids,
                                            ..
                                        } = std::mem::replace(&mut state, AgentState::Idle)
                                        {
                                            audit.log_approved(
                                                iteration,
                                                "keystroke",
                                                "user pressed y",
                                            );
                                            let _ = writeln!(stderr, "\r\n[ua] executing...\r");

                                            command_queue.enqueue(commands);
                                            if let Some(cmd) = command_queue.pop_immediate() {
                                                let cmd = format!("{cmd}\n");
                                                if let Err(e) = session.write_all(cmd.as_bytes()) {
                                                    let _ = writeln!(
                                                        stderr,
                                                        "\r\n[ua] pty write error: {e}"
                                                    );
                                                    command_queue.clear();
                                                } else {
                                                    state = AgentState::Executing {
                                                        iteration,
                                                        capture: OutputHistory::new(200),
                                                        tool_use_ids,
                                                    };
                                                }
                                            }
                                        }
                                        break;
                                    }
                                    b'n' | b'N' | b'q' | b'Q' | 0x03 => {
                                        if let AgentState::Approving { iteration, .. } = &state {
                                            audit.log_denied(
                                                *iteration,
                                                "keystroke",
                                                "user pressed n",
                                            );
                                        }
                                        let _ = writeln!(stderr, "\r\n[ua] skipped\r");
                                        state = AgentState::Idle;
                                        break;
                                    }
                                    _ => {
                                        // Ignore other keys
                                    }
                                }
                            }
                        }
                    }
                    AgentState::Executing { .. } => {
                        // Check for Ctrl+C — forward to PTY and abort agent loop
                        if data.contains(&0x03) {
                            let _ = session.write_all(&[0x03]);
                            command_queue.clear();
                            let _ = writeln!(stderr, "\r\n[ua] cancelled\r");
                            state = AgentState::Idle;
                            pending_instruction = None;
                        } else {
                            // Forward other input to PTY (user may interact with commands)
                            let _ = session.write_all(&data);
                        }
                    }
                }
            }
            Event::BackendChunk(stream_event) => {
                if let AgentState::Streaming {
                    ref mut display,
                    ref mut is_thinking,
                    ref mut tool_commands,
                    ref mut tool_uses,
                    ..
                } = state
                {
                    match &stream_event {
                        StreamEvent::ThinkingDelta(text) => {
                            if !*is_thinking {
                                *is_thinking = true;
                                let _ = write!(stderr, "\r\x1b[K\x1b[2m");
                                let _ = stderr.flush();
                            }
                            let raw_safe = text.replace('\n', "\r\n");
                            let _ = write!(stderr, "\x1b[2m{raw_safe}\x1b[0m");
                            let _ = stderr.flush();
                        }
                        StreamEvent::TextDelta(text) => {
                            if *is_thinking {
                                *is_thinking = false;
                                let _ = write!(stderr, "\x1b[0m\r\n");
                            } else if display.streaming_text.is_empty() {
                                let _ = write!(stderr, "\r\x1b[K");
                            }
                            let raw_safe = text.replace('\n', "\r\n");
                            let _ = write!(stderr, "{raw_safe}");
                            let _ = stderr.flush();
                        }
                        StreamEvent::ToolUse {
                            id,
                            name,
                            input_json,
                        } => {
                            // Track full record for conversation history
                            tool_uses.push(ToolUseRecord {
                                id: id.clone(),
                                name: name.clone(),
                                input_json: input_json.clone(),
                            });
                            // Parse the tool input to extract the command
                            if let Ok(input) = serde_json::from_str::<serde_json::Value>(input_json)
                            {
                                if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                                    tool_commands.push(cmd.to_string());
                                }
                            }
                        }
                        StreamEvent::Error(_) => {
                            if display.streaming_text.is_empty() {
                                let _ = write!(stderr, "\r\x1b[K");
                                let _ = stderr.flush();
                            }
                        }
                        _ => {}
                    }
                    display.handle_event(&stream_event);
                }
            }
            Event::BackendDone => {
                if let AgentState::Streaming {
                    display,
                    iteration,
                    tool_commands,
                    tool_uses,
                    ..
                } = std::mem::replace(&mut state, AgentState::Idle)
                {
                    // Trailing newline
                    let _ = writeln!(stderr, "\r");

                    match display.status {
                        crate::display::DisplayStatus::Error(ref msg) => {
                            let _ = writeln!(stderr, "\r\n[ua] error: {msg}");
                            pending_instruction = None;
                        }
                        _ => {
                            let response_text = display.streaming_text.clone();
                            let commands = tool_commands;

                            // Push conversation history
                            if let Some(instruction) = pending_instruction.take() {
                                conversation.push(ConversationMessage::user(&instruction));
                            }

                            // Push assistant message with tool_use records if present
                            if !tool_uses.is_empty() {
                                conversation.push(ConversationMessage::assistant_with_tool_use(
                                    &response_text,
                                    tool_uses.clone(),
                                ));
                            } else if !response_text.is_empty() {
                                conversation.push(ConversationMessage::assistant(&response_text));
                            }

                            // Extract tool_use_ids for Approving/Executing states
                            let tool_use_ids: Vec<String> =
                                tool_uses.iter().map(|t| t.id.clone()).collect();

                            // Evict oldest turns if over limit
                            let max = config.context.max_conversation_turns;
                            if conversation.len() > max {
                                conversation.drain(..conversation.len() - max);
                            }

                            if commands.is_empty() {
                                // No commands — done
                                state = AgentState::Idle;
                            } else {
                                // Classify each command
                                let risk_levels: Vec<RiskLevel> =
                                    commands.iter().map(|cmd| analyze_pipe_chain(cmd)).collect();
                                let risk_labels: Vec<&str> =
                                    risk_levels.iter().map(|r| r.as_str()).collect();

                                // Log proposed commands
                                audit.log_proposed(iteration, &commands, &risk_labels, "llm");

                                // Check for denied commands — block them
                                let mut blocked = false;
                                for (i, risk) in risk_levels.iter().enumerate() {
                                    if *risk == RiskLevel::Denied {
                                        let _ = writeln!(
                                            stderr,
                                            "\r  \x1b[31m[DENIED]\x1b[0m {}",
                                            commands[i].replace('\n', "\r\n")
                                        );
                                        audit.log_blocked(
                                            &commands[i],
                                            risk.as_str(),
                                            "denied by policy",
                                        );
                                        blocked = true;
                                    }
                                }

                                if blocked {
                                    // Push a tool_result explaining the denial
                                    if !tool_use_ids.is_empty() {
                                        let denial_msg = "Command was blocked by the security policy. \
                                            The command is on the deny list and cannot be executed. \
                                            Please suggest a safer alternative.";
                                        let tool_results: Vec<ToolResultRecord> = tool_use_ids
                                            .iter()
                                            .map(|id| ToolResultRecord {
                                                tool_use_id: id.clone(),
                                                content: denial_msg.to_string(),
                                            })
                                            .collect();
                                        conversation
                                            .push(ConversationMessage::tool_result(tool_results));
                                    }
                                    let _ = writeln!(stderr, "\r[ua] command blocked by policy\r");
                                    state = AgentState::Idle;
                                    continue;
                                }

                                // Check for dangerous arguments
                                for cmd in &commands {
                                    if let ArgumentSafety::Dangerous(reason) =
                                        validate_arguments(cmd)
                                    {
                                        let _ = writeln!(
                                            stderr,
                                            "\r  \x1b[33m[WARNING]\x1b[0m {reason}"
                                        );
                                    }
                                }

                                // Auto-approve if all read-only
                                let all_read_only =
                                    risk_levels.iter().all(|r| *r == RiskLevel::ReadOnly);
                                let has_privileged = risk_levels.contains(&RiskLevel::Privileged);

                                if all_read_only && config.security.auto_approve_read_only {
                                    audit.log_approved(iteration, "auto", "all commands read-only");
                                    let _ = writeln!(stderr, "\r[ua] auto-approved (read-only)\r");

                                    command_queue.enqueue(commands);
                                    if let Some(cmd) = command_queue.pop_immediate() {
                                        let cmd = format!("{cmd}\n");
                                        if let Err(e) = session.write_all(cmd.as_bytes()) {
                                            let _ =
                                                writeln!(stderr, "\r\n[ua] pty write error: {e}");
                                            command_queue.clear();
                                        } else {
                                            state = AgentState::Executing {
                                                iteration,
                                                capture: OutputHistory::new(200),
                                                tool_use_ids,
                                            };
                                        }
                                    }
                                } else {
                                    // Show risk-aware prompt
                                    let _ = writeln!(stderr, "\r[ua] proposed:");
                                    for (i, cmd) in commands.iter().enumerate() {
                                        let safe = cmd.replace('\n', "\r\n");
                                        let label = risk_levels[i].label();
                                        let _ = writeln!(
                                            stderr,
                                            "\r  {}. \x1b[33m[{label}]\x1b[0m {safe}",
                                            i + 1
                                        );
                                    }

                                    if has_privileged && config.security.require_yes_for_privileged
                                    {
                                        let _ = write!(stderr, "\rType 'yes' to approve: ");
                                    } else {
                                        let _ = write!(stderr, "\r[y] run  [n] skip  [q] quit ");
                                    }
                                    let _ = stderr.flush();

                                    state = AgentState::Approving {
                                        commands,
                                        iteration,
                                        tool_use_ids,
                                        has_privileged,
                                        yes_buffer: String::new(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
            Event::PtyOutput(data) => {
                stdout.write_all(&data)?;
                stdout.flush()?;

                // Feed to output history
                output_history.feed(&data);

                // Also feed to execution capture if we're executing
                if let AgentState::Executing {
                    ref mut capture, ..
                } = state
                {
                    capture.feed(&data);
                }

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
                    match command_queue.handle_osc_event(evt) {
                        QueueEvent::Dispatch(cmd) => {
                            let cmd = format!("{cmd}\n");
                            if let Err(e) = session.write_all(cmd.as_bytes()) {
                                let _ = writeln!(stderr, "\r\n[ua] pty write error: {e}");
                                command_queue.clear();
                                state = AgentState::Idle;
                            }
                        }
                        QueueEvent::AllDone => {
                            // Commands finished executing
                            if let AgentState::Executing {
                                iteration,
                                capture,
                                tool_use_ids,
                                ..
                            } = std::mem::replace(&mut state, AgentState::Idle)
                            {
                                let captured_lines = capture.lines();
                                if !captured_lines.is_empty() && iteration < MAX_AGENT_ITERATIONS {
                                    // Build observation with scrubbing
                                    let raw_output = captured_lines.join("\n");
                                    let scrubbed = scrub_injection_markers(&raw_output);
                                    let observation =
                                        format!("{}{}\n", TOOL_RESULT_PREFIX, scrubbed);

                                    // Push as tool_result if we have tool_use_ids,
                                    // otherwise fall back to plain user message.
                                    if !tool_use_ids.is_empty() {
                                        let tool_results: Vec<ToolResultRecord> = tool_use_ids
                                            .iter()
                                            .map(|id| ToolResultRecord {
                                                tool_use_id: id.clone(),
                                                content: observation.clone(),
                                            })
                                            .collect();
                                        conversation
                                            .push(ConversationMessage::tool_result(tool_results));
                                    } else {
                                        conversation.push(ConversationMessage::user(&observation));
                                    }

                                    // Clear readline before next LLM call
                                    let _ = session.write_all(b"\x15");

                                    let next_iteration = iteration + 1;
                                    if next_iteration >= MAX_AGENT_ITERATIONS {
                                        let _ = writeln!(
                                            stderr,
                                            "\r[ua] max iterations ({MAX_AGENT_ITERATIONS}) reached"
                                        );
                                    } else {
                                        let _ = writeln!(
                                            stderr,
                                            "\r[ua] observing output ({next_iteration}/{MAX_AGENT_ITERATIONS})..."
                                        );
                                        // Pass empty instruction — tool_result is
                                        // already in conversation history.
                                        state = start_streaming(
                                            rt_handle,
                                            config,
                                            "",
                                            &output_history,
                                            &conversation,
                                            terminal_size,
                                            next_iteration,
                                            &tx_for_streaming,
                                            &mut stderr,
                                        );
                                    }
                                }
                                // else: no output or max iterations — stay Idle
                            }
                        }
                        QueueEvent::Failed(code) => {
                            let _ = writeln!(
                                stderr,
                                "\r[ua] command failed (exit code {code}), stopping"
                            );
                            state = AgentState::Idle;
                        }
                        QueueEvent::None => {}
                    }
                }
            }
            Event::PtyEof => {
                // PTY closed — check exit code for diagnostics
                if debug_osc {
                    if let Ok(Some(code)) = session.try_wait() {
                        let _ = writeln!(stderr, "\r[ua] child exited with code {code}");
                    }
                }
                break;
            }
            Event::Resize(cols, rows) => {
                terminal_size = (cols, rows);
                if let Err(e) = session.resize(cols, rows) {
                    if debug_osc {
                        let _ = writeln!(stderr, "\r[ua] resize error: {e}");
                    }
                }
            }
        }
    }

    // Join PTY reader thread (stdin thread blocks on read — can't join portably)
    let _ = pty_reader_handle.join();

    Ok(())
}

/// Spawn a tokio task to stream from the backend, forwarding events through the mpsc channel.
/// Returns the initial AgentState::Streaming.
#[allow(clippy::too_many_arguments)]
fn start_streaming(
    rt_handle: &Handle,
    config: &Config,
    instruction: &str,
    history: &OutputHistory,
    conversation: &[ConversationMessage],
    terminal_size: (u16, u16),
    iteration: usize,
    tx: &mpsc::Sender<Event>,
    stderr: &mut io::Stderr,
) -> AgentState {
    // Resolve API key
    let api_key = match config.backend.anthropic.resolve_api_key() {
        Ok(key) => key,
        Err(e) => {
            let _ = writeln!(stderr, "\r\n[ua] error: {e}");
            return AgentState::Idle;
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

    // Create client and stream
    let client = AnthropicClient::with_model(&api_key, &config.backend.anthropic.model);
    let stream = client.send(&request);

    // Show immediate feedback
    let _ = write!(stderr, "\r\n[ua] thinking...");
    let _ = stderr.flush();

    // Cancellation channel
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    // Spawn tokio task that forwards stream events through the mpsc channel
    let tx_clone = tx.clone();
    rt_handle.spawn(async move {
        let mut stream = std::pin::pin!(stream);
        tokio::select! {
            _ = async {
                while let Some(event) = stream.next().await {
                    if tx_clone.send(Event::BackendChunk(event)).is_err() {
                        break;
                    }
                }
            } => {}
            _ = cancel_rx => {
                // Cancelled — stop streaming
            }
        }
        let _ = tx_clone.send(Event::BackendDone);
    });

    AgentState::Streaming {
        cancel_tx: Some(cancel_tx),
        display: PlanDisplay::new(),
        is_thinking: false,
        iteration,
        tool_commands: Vec::new(),
        tool_uses: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Tool use command extraction tests ---

    /// Helper: parse a tool_use input JSON to extract the command, same as REPL does.
    fn extract_tool_command(input_json: &str) -> Option<String> {
        let input: serde_json::Value = serde_json::from_str(input_json).ok()?;
        input
            .get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
    }

    #[test]
    fn tool_use_extracts_command() {
        let json = r#"{"command":"ls /tmp"}"#;
        assert_eq!(extract_tool_command(json), Some("ls /tmp".to_string()));
    }

    #[test]
    fn tool_use_extracts_chained_command() {
        let json = r#"{"command":"cat Cargo.toml && head -30 PLAN.md"}"#;
        assert_eq!(
            extract_tool_command(json),
            Some("cat Cargo.toml && head -30 PLAN.md".to_string())
        );
    }

    #[test]
    fn tool_use_missing_command_field() {
        let json = r#"{"cmd":"ls"}"#;
        assert_eq!(extract_tool_command(json), None);
    }

    #[test]
    fn tool_use_invalid_json() {
        assert_eq!(extract_tool_command("not json"), None);
    }

    #[test]
    fn tool_use_empty_command() {
        let json = r#"{"command":""}"#;
        assert_eq!(extract_tool_command(json), Some(String::new()));
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
        assert_eq!(result, QueueEvent::None);
        assert!(queue.awaiting_ready);
    }

    #[test]
    fn command_queue_dispatches_on_133b() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["pwd".to_string()]);

        queue.handle_osc_event(&OscEvent::Osc133A);
        let result = queue.handle_osc_event(&OscEvent::Osc133B);
        assert_eq!(result, QueueEvent::Dispatch("pwd".to_string()));
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
            QueueEvent::None
        );
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), QueueEvent::None);
        assert!(queue.awaiting_ready);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::Dispatch("cmd2".to_string())
        );

        // cmd2 finishes → same cycle
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            QueueEvent::None
        );
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), QueueEvent::None);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::Dispatch("cmd3".to_string())
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
        assert_eq!(result, QueueEvent::None);
        assert!(!queue.is_empty());
    }

    #[test]
    fn command_queue_empty_is_noop() {
        let mut queue = CommandQueue::new();

        // All events are no-ops when queue is empty (not executing)
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), QueueEvent::None);
        assert!(!queue.awaiting_ready);
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133B), QueueEvent::None);
    }

    #[test]
    fn command_queue_clear_resets_state() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd1".to_string(), "cmd2".to_string()]);
        queue.handle_osc_event(&OscEvent::Osc133A);

        queue.clear();
        assert!(queue.is_empty());
        assert!(!queue.awaiting_ready);
        assert!(!queue.executing);
    }

    #[test]
    fn command_queue_ignores_133c_and_133d() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd".to_string()]);
        queue.handle_osc_event(&OscEvent::Osc133A);

        // 133;C and 133;D should not dispatch
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133C), QueueEvent::None);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            QueueEvent::None
        );
        assert!(queue.awaiting_ready); // Still waiting for 133;B
    }

    #[test]
    fn command_queue_single_command_all_done() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["ls".to_string()]);
        assert!(queue.executing);

        // First command dispatched immediately
        assert_eq!(queue.pop_immediate(), Some("ls".to_string()));

        // ls finishes → 133;D, 133;A, 133;B → AllDone (queue empty)
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) }),
            QueueEvent::None
        );
        assert_eq!(queue.handle_osc_event(&OscEvent::Osc133A), QueueEvent::None);
        assert!(queue.awaiting_ready); // executing=true, so 133;A sets awaiting
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::AllDone
        );
    }

    #[test]
    fn command_queue_multi_command_all_done() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd1".to_string(), "cmd2".to_string()]);

        // First command dispatched immediately
        assert_eq!(queue.pop_immediate(), Some("cmd1".to_string()));

        // cmd1 finishes → dispatches cmd2
        queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) });
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::Dispatch("cmd2".to_string())
        );

        // cmd2 finishes → AllDone
        queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) });
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::AllDone
        );
    }

    #[test]
    fn command_queue_all_done_resets_executing() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["ls".to_string()]);
        assert!(queue.executing);

        queue.pop_immediate();

        // ls finishes → AllDone
        queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) });
        queue.handle_osc_event(&OscEvent::Osc133A);
        queue.handle_osc_event(&OscEvent::Osc133B);

        assert!(!queue.executing);

        // Subsequent 133;A should NOT set awaiting_ready (not executing)
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert!(!queue.awaiting_ready);
    }

    #[test]
    fn command_queue_failed_exit_code_stops_queue() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec![
            "cmd1".to_string(),
            "cmd2".to_string(),
            "cmd3".to_string(),
        ]);

        // First command dispatched immediately
        assert_eq!(queue.pop_immediate(), Some("cmd1".to_string()));

        // cmd1 fails with exit code 1
        queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(1) });
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::Failed(1)
        );

        // Queue should be cleared
        assert!(queue.is_empty());
        assert!(!queue.executing);
    }

    #[test]
    fn command_queue_last_command_fails_is_all_done() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["only_cmd".to_string()]);

        // Dispatch the only command
        assert_eq!(queue.pop_immediate(), Some("only_cmd".to_string()));

        // Command fails — but queue is already empty, so AllDone (not Failed)
        queue.handle_osc_event(&OscEvent::Osc133D {
            exit_code: Some(127),
        });
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::AllDone
        );
    }

    #[test]
    fn command_queue_success_continues() {
        let mut queue = CommandQueue::new();
        queue.enqueue(vec!["cmd1".to_string(), "cmd2".to_string()]);

        assert_eq!(queue.pop_immediate(), Some("cmd1".to_string()));

        // cmd1 succeeds
        queue.handle_osc_event(&OscEvent::Osc133D { exit_code: Some(0) });
        queue.handle_osc_event(&OscEvent::Osc133A);
        assert_eq!(
            queue.handle_osc_event(&OscEvent::Osc133B),
            QueueEvent::Dispatch("cmd2".to_string())
        );
    }

    // --- End-to-end tests: mock StreamEvent with tool_use ---

    #[test]
    fn mock_tool_use_single_command() {
        // Text + tool_use: command comes via ToolUse event, not code blocks
        let events = vec![
            StreamEvent::TextDelta("I'll list the files.".to_string()),
            StreamEvent::ToolUse {
                id: "toolu_123".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"ls /tmp"}"#.to_string(),
            },
            StreamEvent::Done,
        ];

        let mut commands = Vec::new();
        for event in &events {
            if let StreamEvent::ToolUse { input_json, .. } = event {
                if let Some(cmd) = extract_tool_command(input_json) {
                    commands.push(cmd);
                }
            }
        }

        assert_eq!(commands, vec!["ls /tmp"]);
    }

    #[test]
    fn mock_tool_use_with_thinking() {
        let events = vec![
            StreamEvent::ThinkingDelta("Let me analyze...".to_string()),
            StreamEvent::TextDelta("I'll check the project.".to_string()),
            StreamEvent::ToolUse {
                id: "toolu_456".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"cat Cargo.toml && head -30 PLAN.md"}"#.to_string(),
            },
            StreamEvent::Done,
        ];

        let mut display = PlanDisplay::new();
        let mut commands = Vec::new();
        for event in &events {
            display.handle_event(event);
            if let StreamEvent::ToolUse { input_json, .. } = event {
                if let Some(cmd) = extract_tool_command(input_json) {
                    commands.push(cmd);
                }
            }
        }

        assert_eq!(commands, vec!["cat Cargo.toml && head -30 PLAN.md"]);
        // Text and thinking are separate from commands
        assert!(!display.thinking_text.is_empty());
        assert!(display.streaming_text.contains("check the project"));
        // No code blocks in the text
        assert!(!display.streaming_text.contains("```"));
    }

    #[test]
    fn mock_text_only_no_tool_use() {
        // Final answer: text only, no tool_use
        let events = vec![
            StreamEvent::TextDelta("The directory is empty. Nothing to do.".to_string()),
            StreamEvent::Done,
        ];

        let mut commands = Vec::new();
        for event in &events {
            if let StreamEvent::ToolUse { input_json, .. } = event {
                if let Some(cmd) = extract_tool_command(input_json) {
                    commands.push(cmd);
                }
            }
        }

        assert!(commands.is_empty());
    }

    #[test]
    fn mock_error_yields_no_commands() {
        let events = vec![
            StreamEvent::TextDelta("I'll help with".to_string()),
            StreamEvent::Error("Rate limited".to_string()),
        ];

        let mut commands = Vec::new();
        for event in &events {
            if let StreamEvent::ToolUse { input_json, .. } = event {
                if let Some(cmd) = extract_tool_command(input_json) {
                    commands.push(cmd);
                }
            }
        }

        assert!(commands.is_empty());
    }

    // --- Tool use record tracking tests ---

    #[test]
    fn tool_use_captures_full_record() {
        let events = vec![StreamEvent::ToolUse {
            id: "toolu_abc".to_string(),
            name: "shell".to_string(),
            input_json: r#"{"command":"ls /tmp"}"#.to_string(),
        }];

        let mut tool_commands = Vec::new();
        let mut tool_uses: Vec<ToolUseRecord> = Vec::new();

        for event in &events {
            if let StreamEvent::ToolUse {
                id,
                name,
                input_json,
            } = event
            {
                tool_uses.push(ToolUseRecord {
                    id: id.clone(),
                    name: name.clone(),
                    input_json: input_json.clone(),
                });
                if let Some(cmd) = extract_tool_command(input_json) {
                    tool_commands.push(cmd);
                }
            }
        }

        assert_eq!(tool_commands, vec!["ls /tmp"]);
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].id, "toolu_abc");
        assert_eq!(tool_uses[0].name, "shell");
    }

    #[test]
    fn tool_use_multiple_captures_ids() {
        let events = vec![
            StreamEvent::ToolUse {
                id: "toolu_1".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"ls"}"#.to_string(),
            },
            StreamEvent::ToolUse {
                id: "toolu_2".to_string(),
                name: "shell".to_string(),
                input_json: r#"{"command":"pwd"}"#.to_string(),
            },
        ];

        let mut tool_uses: Vec<ToolUseRecord> = Vec::new();
        let mut tool_commands = Vec::new();

        for event in &events {
            if let StreamEvent::ToolUse {
                id,
                name,
                input_json,
            } = event
            {
                tool_uses.push(ToolUseRecord {
                    id: id.clone(),
                    name: name.clone(),
                    input_json: input_json.clone(),
                });
                if let Some(cmd) = extract_tool_command(input_json) {
                    tool_commands.push(cmd);
                }
            }
        }

        assert_eq!(tool_uses.len(), 2);
        assert_eq!(tool_commands, vec!["ls", "pwd"]);
        let ids: Vec<&str> = tool_uses.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["toolu_1", "toolu_2"]);
    }

    #[test]
    fn tool_result_built_from_ids() {
        let tool_use_ids = vec!["toolu_a".to_string(), "toolu_b".to_string()];
        let observation = "file1.txt\nfile2.txt".to_string();

        let tool_results: Vec<ToolResultRecord> = tool_use_ids
            .iter()
            .map(|id| ToolResultRecord {
                tool_use_id: id.clone(),
                content: observation.clone(),
            })
            .collect();

        assert_eq!(tool_results.len(), 2);
        assert_eq!(tool_results[0].tool_use_id, "toolu_a");
        assert_eq!(tool_results[1].tool_use_id, "toolu_b");
        assert_eq!(tool_results[0].content, observation);

        let msg = ConversationMessage::tool_result(tool_results);
        assert_eq!(msg.role, ua_protocol::Role::User);
        assert_eq!(msg.tool_results.len(), 2);
    }
}
