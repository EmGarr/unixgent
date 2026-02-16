//! Testable REPL display output following the Linus forward-flow design.
//!
//! `ReplRenderer<W: Write>` centralizes all stderr formatting for the REPL,
//! following the same `BatchOutput<W>` pattern from `batch.rs`. Every emit
//! method clears the spinner first, preventing spinner bleed into PTY output.

use std::io::Write;

use crate::policy::RiskLevel;
use crate::style::{format_tokens, Style};

/// Braille spinner frames.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Encapsulates all stderr formatting for REPL mode output.
///
/// Every `emit_*` method (except spinner methods) calls `clear_spinner()` first.
/// This is the single fix for spinner bleed and job notification overlap.
pub struct ReplRenderer<W: Write> {
    pub writer: W,
    style: Style,
    #[allow(dead_code)]
    term_width: u16,
    spinner_active: bool,
}

impl<W: Write> ReplRenderer<W> {
    pub fn new(writer: W, style: Style) -> Self {
        let term_width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
        Self {
            writer,
            style,
            term_width,
            spinner_active: false,
        }
    }

    pub fn new_with_width(writer: W, style: Style, width: u16) -> Self {
        Self {
            writer,
            style,
            term_width: width,
            spinner_active: false,
        }
    }

    /// Access the style (needed by callers that format their own strings).
    pub fn style(&self) -> &Style {
        &self.style
    }

    // ── Core spinner protocol ───────────────────────────────────────────

    /// Clear the spinner line if one is active.
    /// Every persistent emit method calls this first.
    pub fn clear_spinner(&mut self) {
        if self.spinner_active {
            let _ = write!(self.writer, "\r\x1b[K");
            self.spinner_active = false;
        }
    }

    /// Show the initial spinner (with leading newline).
    pub fn emit_spinner_initial(&mut self) {
        let _ = write!(
            self.writer,
            "\r\n\x1b[K{}{} thinking...{}",
            self.style.cyan_start(),
            SPINNER_FRAMES[0],
            self.style.reset()
        );
        let _ = self.writer.flush();
        self.spinner_active = true;
    }

    /// Update the spinner to the next frame.
    pub fn emit_spinner_tick(&mut self, frame: usize) {
        let _ = write!(
            self.writer,
            "\r\x1b[K{}{} thinking...{}",
            self.style.cyan_start(),
            SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
            self.style.reset()
        );
        let _ = self.writer.flush();
        self.spinner_active = true;
    }

    // ── Persistent output methods ───────────────────────────────────────

    /// Show the first line of thinking as a dim `# comment`.
    pub fn emit_thinking_line(&mut self, text: &str) {
        self.clear_spinner();
        let _ = write!(
            self.writer,
            "\r\x1b[K{}# {}{}\r\n",
            self.style.dim_start(),
            text,
            self.style.reset()
        );
        let _ = self.writer.flush();
    }

    /// Emit streamed text (with `\n` → `\r\n` conversion).
    pub fn emit_text(&mut self, text: &str) {
        self.clear_spinner();
        let raw_safe = text.replace('\n', "\r\n");
        let _ = write!(self.writer, "{raw_safe}");
        let _ = self.writer.flush();
    }

    /// Clear the spinner line without emitting content (transition from spinner to text).
    pub fn emit_clear_line(&mut self) {
        self.clear_spinner();
        let _ = write!(self.writer, "\r\x1b[K");
        let _ = self.writer.flush();
    }

    /// Trailing newline after streaming completes.
    pub fn emit_stream_end(&mut self) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r");
    }

    /// Show a read-only auto-approved command: `  ❯ cmd  ▐ safe`
    pub fn emit_command_safe(&mut self, cmd: &str) {
        self.clear_spinner();
        let safe = cmd.replace('\n', "\r\n");
        let _ = writeln!(
            self.writer,
            "\r  ❯ {safe}  {}▐ safe{}",
            self.style.dim_start(),
            self.style.reset(),
        );
    }

    /// Show a command with its risk label: `  ❯ cmd  ▐ label`
    pub fn emit_command_risk(&mut self, cmd: &str, risk: &RiskLevel) {
        self.clear_spinner();
        let safe = cmd.replace('\n', "\r\n");
        let color = risk_color(risk, &self.style);
        let label = risk.label();
        let _ = writeln!(
            self.writer,
            "\r  ❯ {safe}  {color}▐ {label}{}",
            self.style.reset()
        );
    }

    /// Show a denied command: `  ❯ cmd  ▐ DENIED`
    pub fn emit_denied(&mut self, cmd: &str) {
        self.clear_spinner();
        let safe = cmd.replace('\n', "\r\n");
        let _ = writeln!(
            self.writer,
            "\r  ❯ {safe}  {}▐ DENIED{}",
            self.style.red_start(),
            self.style.reset(),
        );
    }

    /// Show an argument safety warning: `  ⚠ reason`
    pub fn emit_arg_warning(&mut self, reason: &str) {
        self.clear_spinner();
        let _ = writeln!(
            self.writer,
            "\r  {}⚠ {}{}",
            self.style.yellow_start(),
            reason,
            self.style.reset()
        );
    }

    /// Show the first line of a judge warning, plus dim continuation lines.
    pub fn emit_judge_warning(&mut self, first: &str, rest: &[&str]) {
        self.clear_spinner();
        let _ = writeln!(
            self.writer,
            "\r\x1b[K{}⚠ {}{}",
            self.style.yellow_start(),
            first,
            self.style.reset()
        );
        for line in rest {
            let _ = writeln!(
                self.writer,
                "\r  {}{}{}",
                self.style.dim_start(),
                line,
                self.style.reset()
            );
        }
    }

    /// Show a judge note/error: `  judge: msg`
    pub fn emit_judge_note(&mut self, msg: &str) {
        self.clear_spinner();
        let _ = writeln!(
            self.writer,
            "\r\x1b[K{}judge: {}{}",
            self.style.dim_start(),
            msg,
            self.style.reset()
        );
    }

    /// Show the approval prompt: `[y] run  [n] skip  [e] edit` or `Type 'yes' to approve: `
    pub fn emit_approval_prompt(&mut self, privileged: bool) {
        self.clear_spinner();
        if privileged {
            let _ = write!(self.writer, "\rType 'yes' to approve: ");
        } else {
            let _ = write!(self.writer, "\r[y] run  [n] skip  [e] edit ");
        }
        let _ = self.writer.flush();
    }

    /// Show the footer stats line: `1.2k↑ 500↓  2 cmds  3s`
    pub fn emit_footer(&mut self, input_tokens: u32, output_tokens: u32, cmds: u32, secs: u64) {
        self.clear_spinner();
        let _ = writeln!(
            self.writer,
            "\r\x1b[K{}{}↑ {}↓  {} cmds  {}s{}",
            self.style.dim_start(),
            format_tokens(input_tokens),
            format_tokens(output_tokens),
            cmds,
            secs,
            self.style.reset(),
        );
    }

    /// Show a child agent started line (pre-formatted by agents.rs).
    pub fn emit_child_started(&mut self, line: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\x1b[K{line}");
    }

    /// Show a child agent done line (pre-formatted by agents.rs).
    pub fn emit_child_done(&mut self, line: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\x1b[K{line}");
    }

    /// Show an error: `[ua] error: msg`
    pub fn emit_error(&mut self, msg: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\n[ua] error: {msg}");
    }

    /// Show cancellation: `[ua] cancelled`
    pub fn emit_cancelled(&mut self) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\n[ua] cancelled\r");
    }

    /// Show skipped: `[ua] skipped` with optional reason.
    pub fn emit_skipped(&mut self, reason: Option<&str>) {
        self.clear_spinner();
        if let Some(r) = reason {
            let _ = writeln!(self.writer, "\r\n[ua] skipped ({r})\r");
        } else {
            let _ = writeln!(self.writer, "\r\n[ua] skipped\r");
        }
    }

    /// Show blocked: `[ua] command blocked by policy`
    pub fn emit_blocked(&mut self) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r[ua] command blocked by policy\r");
    }

    /// Show PTY write error.
    pub fn emit_pty_error(&mut self, err: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\n[ua] pty write error: {err}");
    }

    /// Show ephemeral judging indicator.
    pub fn emit_judging(&mut self) {
        self.clear_spinner();
        let _ = write!(self.writer, "\r[ua] evaluating safety...");
        let _ = self.writer.flush();
    }

    /// Show judge error.
    pub fn emit_judge_error(&mut self, err: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r[ua] judge error: {err}");
    }

    /// Show a debug message (only used with debug_osc).
    pub fn emit_debug(&mut self, msg: &str) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r{msg}");
    }

    /// Show command failed message.
    pub fn emit_command_failed(&mut self, code: i32) {
        self.clear_spinner();
        let _ = writeln!(
            self.writer,
            "\r[ua] command failed (exit code {code}), stopping"
        );
    }

    /// Show "edit not yet implemented" message.
    pub fn emit_edit_not_implemented(&mut self) {
        self.clear_spinner();
        let _ = writeln!(self.writer, "\r\n  (edit not yet implemented)\r");
    }

    /// Write a single character (for yes-buffer echo).
    pub fn emit_char(&mut self, c: char) {
        let _ = write!(self.writer, "{c}");
        let _ = self.writer.flush();
    }

    /// Backspace character for yes-buffer editing.
    pub fn emit_backspace(&mut self) {
        let _ = write!(self.writer, "\x08 \x08");
        let _ = self.writer.flush();
    }

    /// Whether the spinner is currently active (for testing).
    pub fn spinner_active(&self) -> bool {
        self.spinner_active
    }
}

/// Format risk label with color.
pub fn risk_color<'a>(risk: &RiskLevel, style: &'a Style) -> &'a str {
    match risk {
        RiskLevel::ReadOnly | RiskLevel::BuildTest => style.green_start(),
        RiskLevel::Write | RiskLevel::Network => style.yellow_start(),
        RiskLevel::Destructive | RiskLevel::Privileged | RiskLevel::Denied => style.red_start(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_renderer(style: Style) -> ReplRenderer<Vec<u8>> {
        ReplRenderer::new_with_width(Vec::new(), style, 80)
    }

    fn output_str(r: &ReplRenderer<Vec<u8>>) -> String {
        String::from_utf8_lossy(&r.writer).to_string()
    }

    // ── Spinner protocol tests ──────────────────────────────────────────

    #[test]
    fn spinner_cleared_before_persistent_output() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        assert!(r.spinner_active());
        r.emit_text("hello");
        assert!(!r.spinner_active());
    }

    #[test]
    fn spinner_clear_is_idempotent() {
        let mut r = make_renderer(Style::disabled());
        // No spinner active — clear should produce no output
        r.clear_spinner();
        assert!(output_str(&r).is_empty());
        r.clear_spinner();
        assert!(output_str(&r).is_empty());
    }

    #[test]
    fn spinner_tick_keeps_active() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_tick(3);
        assert!(r.spinner_active());
        r.emit_spinner_tick(4);
        assert!(r.spinner_active());
    }

    #[test]
    fn no_spinner_bleed_on_child_status() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        assert!(r.spinner_active());
        r.emit_child_started("[123] task  ···");
        assert!(!r.spinner_active());
        let s = output_str(&r);
        assert!(
            s.contains("\r\x1b[K"),
            "should clear spinner before child line"
        );
        assert!(s.contains("[123] task"));
    }

    // ── Scenario 1: Simple read-only ────────────────────────────────────

    #[test]
    fn simple_read_only_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        r.emit_thinking_line("analyzing the directory structure");
        r.emit_text("Here are the files:\n- foo.rs\n- bar.rs\n");
        r.emit_stream_end();
        r.emit_command_safe("ls -la");
        r.emit_footer(1200, 500, 1, 3);

        let s = output_str(&r);
        assert!(s.contains("# "), "should have thinking comment");
        assert!(s.contains("❯ ls -la"), "should have command");
        assert!(s.contains("safe"), "should show safe label");
        assert!(s.contains("1.2k↑"), "should have input tokens");
        assert!(s.contains("500↓"), "should have output tokens");
        assert!(s.contains("1 cmds"), "should show command count");
        assert!(s.contains("3s"), "should show elapsed time");
        assert!(!s.contains("[ua] executing"), "must not have executing");
        assert!(!s.contains("observing output"), "must not have observing");
    }

    #[test]
    fn simple_read_only_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_spinner_initial();
        r.emit_thinking_line("analyzing");
        r.emit_text("result\n");
        r.emit_stream_end();
        r.emit_command_safe("ls");
        r.emit_footer(800, 300, 1, 2);

        let s = output_str(&r);
        assert!(s.contains("\x1b[2m"), "should have dim for thinking");
        assert!(s.contains("\x1b[36m"), "should have cyan for spinner");
        assert!(s.contains("\x1b[0m"), "should have reset");
    }

    // ── Scenario 2: Approval required ───────────────────────────────────

    #[test]
    fn approval_required_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        r.emit_stream_end();
        r.emit_command_risk("rm -rf build/", &RiskLevel::Destructive);
        r.emit_approval_prompt(false);

        let s = output_str(&r);
        assert!(s.contains("❯ rm -rf build/"), "should have command");
        assert!(s.contains("destructive"), "should have risk label");
        assert!(s.contains("[y] run"), "should have approval prompt");
    }

    #[test]
    fn approval_required_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_command_risk("rm -rf build/", &RiskLevel::Destructive);
        r.emit_approval_prompt(false);

        let s = output_str(&r);
        assert!(s.contains("\x1b[31m"), "destructive should be red");
    }

    // ── Scenario 3: Privileged + judge ──────────────────────────────────

    #[test]
    fn privileged_judge_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_command_risk("sudo apt install", &RiskLevel::Privileged);
        r.emit_judge_warning(
            "This command requires root privileges",
            &["It will modify system packages"],
        );
        r.emit_approval_prompt(true);

        let s = output_str(&r);
        assert!(s.contains("⚠"), "should have warning symbol");
        assert!(s.contains("root privileges"), "should have warning text");
        assert!(
            s.contains("Type 'yes' to approve"),
            "should have privileged prompt"
        );
    }

    #[test]
    fn privileged_judge_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_command_risk("sudo apt install", &RiskLevel::Privileged);
        r.emit_judge_warning("dangerous", &["details"]);
        r.emit_approval_prompt(true);

        let s = output_str(&r);
        assert!(s.contains("\x1b[33m"), "warning should be yellow");
        assert!(s.contains("\x1b[31m"), "privileged should be red");
    }

    // ── Scenario 4: Multi-iteration ─────────────────────────────────────

    #[test]
    fn multi_iteration_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        // Iteration 1
        r.emit_spinner_initial();
        r.emit_thinking_line("first pass");
        r.emit_stream_end();
        r.emit_command_safe("ls");
        // Iteration 2
        r.emit_spinner_initial();
        r.emit_thinking_line("second pass");
        r.emit_stream_end();
        r.emit_command_safe("cat foo.rs");
        r.emit_footer(2000, 1000, 2, 5);

        let s = output_str(&r);
        let thinking_count = s.matches("# ").count();
        assert_eq!(thinking_count, 2, "should have two thinking lines");
        let cmd_count = s.matches("❯ ").count();
        assert_eq!(cmd_count, 2, "should have two command lines");
        assert!(
            !s.contains("observing output"),
            "must not have observing output between iterations"
        );
        // Only one footer
        let footer_count = s.matches("cmds").count();
        assert_eq!(footer_count, 1, "should have exactly one footer");
    }

    #[test]
    fn multi_iteration_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_spinner_initial();
        r.emit_thinking_line("pass 1");
        r.emit_stream_end();
        r.emit_command_safe("ls");
        r.emit_spinner_initial();
        r.emit_thinking_line("pass 2");
        r.emit_stream_end();
        r.emit_command_safe("pwd");
        r.emit_footer(1500, 800, 2, 4);

        let s = output_str(&r);
        assert!(s.contains("\x1b[2m"), "should have dim codes");
    }

    // ── Scenario 5: Subagent delegation ─────────────────────────────────

    #[test]
    fn subagent_delegation_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        r.emit_child_started("[123] find TODOs  ···");
        r.emit_child_done("[123] find TODOs  done  700 tok  3 cmds  5s");
        r.emit_footer(500, 200, 0, 6);

        let s = output_str(&r);
        assert!(
            s.contains("[123] find TODOs  ···"),
            "should have child started"
        );
        assert!(
            s.contains("[123] find TODOs  done"),
            "should have child done"
        );
    }

    #[test]
    fn subagent_delegation_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_spinner_initial();
        r.emit_child_started("[123] task  ···");
        r.emit_child_done("[123] task  done");

        let s = output_str(&r);
        // Lines are pre-formatted by agents.rs so just check they're there
        assert!(s.contains("[123]"));
    }

    // ── Scenario 6: Error ───────────────────────────────────────────────

    #[test]
    fn error_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        assert!(r.spinner_active());
        r.emit_error("API key not found");

        let s = output_str(&r);
        assert!(s.contains("error:"), "should have error prefix");
        assert!(s.contains("API key not found"), "should have error message");
        assert!(!r.spinner_active(), "spinner should be cleared");
    }

    #[test]
    fn error_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_spinner_initial();
        r.emit_error("connection failed");

        let s = output_str(&r);
        assert!(s.contains("error:"));
    }

    // ── Scenario 7: Denied ──────────────────────────────────────────────

    #[test]
    fn denied_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_denied("rm -rf /");

        let s = output_str(&r);
        assert!(s.contains("DENIED"), "should have DENIED label");
        assert!(s.contains("rm -rf /"), "should have command");
    }

    #[test]
    fn denied_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_denied("rm -rf /");

        let s = output_str(&r);
        assert!(s.contains("\x1b[31m"), "DENIED should be red");
    }

    // ── Scenario 8: Cancelled ───────────────────────────────────────────

    #[test]
    fn cancelled_no_ansi() {
        let mut r = make_renderer(Style::disabled());
        r.emit_spinner_initial();
        assert!(r.spinner_active());
        r.emit_cancelled();

        let s = output_str(&r);
        assert!(s.contains("cancelled"), "should have cancelled");
        assert!(!r.spinner_active(), "spinner should be cleared");
    }

    #[test]
    fn cancelled_with_ansi() {
        let mut r = make_renderer(Style::force_enabled());
        r.emit_spinner_initial();
        r.emit_cancelled();

        let s = output_str(&r);
        assert!(s.contains("cancelled"));
    }

    // ── Additional unit tests ───────────────────────────────────────────

    #[test]
    fn risk_color_mapping() {
        let style = Style::force_enabled();
        assert_eq!(risk_color(&RiskLevel::ReadOnly, &style), "\x1b[32m");
        assert_eq!(risk_color(&RiskLevel::BuildTest, &style), "\x1b[32m");
        assert_eq!(risk_color(&RiskLevel::Write, &style), "\x1b[33m");
        assert_eq!(risk_color(&RiskLevel::Network, &style), "\x1b[33m");
        assert_eq!(risk_color(&RiskLevel::Destructive, &style), "\x1b[31m");
        assert_eq!(risk_color(&RiskLevel::Privileged, &style), "\x1b[31m");
        assert_eq!(risk_color(&RiskLevel::Denied, &style), "\x1b[31m");
    }

    #[test]
    fn risk_color_disabled() {
        let style = Style::disabled();
        assert_eq!(risk_color(&RiskLevel::ReadOnly, &style), "");
        assert_eq!(risk_color(&RiskLevel::Destructive, &style), "");
    }

    #[test]
    fn emit_skipped_with_reason() {
        let mut r = make_renderer(Style::disabled());
        r.emit_skipped(Some("type 'yes' to approve"));
        let s = output_str(&r);
        assert!(s.contains("skipped"));
        assert!(s.contains("type 'yes' to approve"));
    }

    #[test]
    fn emit_skipped_without_reason() {
        let mut r = make_renderer(Style::disabled());
        r.emit_skipped(None);
        let s = output_str(&r);
        assert!(s.contains("skipped"));
        assert!(!s.contains("("));
    }

    #[test]
    fn emit_blocked_message() {
        let mut r = make_renderer(Style::disabled());
        r.emit_blocked();
        let s = output_str(&r);
        assert!(s.contains("command blocked by policy"));
    }

    #[test]
    fn emit_footer_formats_tokens() {
        let mut r = make_renderer(Style::disabled());
        r.emit_footer(1200, 500, 2, 3);
        let s = output_str(&r);
        assert!(s.contains("1.2k↑"));
        assert!(s.contains("500↓"));
        assert!(s.contains("2 cmds"));
        assert!(s.contains("3s"));
    }

    #[test]
    fn emit_text_converts_newlines() {
        let mut r = make_renderer(Style::disabled());
        r.emit_text("line1\nline2\n");
        let s = output_str(&r);
        assert!(s.contains("line1\r\nline2\r\n"));
    }

    #[test]
    fn emit_judge_warning_multiline() {
        let mut r = make_renderer(Style::disabled());
        r.emit_judge_warning("First warning", &["detail 1", "detail 2"]);
        let s = output_str(&r);
        assert!(s.contains("⚠ First warning"));
        assert!(s.contains("detail 1"));
        assert!(s.contains("detail 2"));
    }

    #[test]
    fn emit_judge_note_display() {
        let mut r = make_renderer(Style::disabled());
        r.emit_judge_note("timeout connecting to judge");
        let s = output_str(&r);
        assert!(s.contains("judge: timeout connecting to judge"));
    }
}
