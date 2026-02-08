//! Testing harness for TUI components.
//!
//! Provides TestTui for rendering and asserting on display output.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::Terminal;
use ua_protocol::StreamEvent;

use super::PlanDisplay;

/// Test harness for TUI components.
pub struct TestTui {
    pub display: PlanDisplay,
    terminal: Terminal<TestBackend>,
}

impl TestTui {
    /// Create a new test TUI with the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        let backend = TestBackend::new(width, height);
        let terminal = Terminal::new(backend).expect("failed to create terminal");

        Self {
            display: PlanDisplay::new(),
            terminal,
        }
    }

    /// Create a test TUI with default dimensions (80x24).
    pub fn default_size() -> Self {
        Self::new(80, 24)
    }

    /// Apply a stream event to the display.
    pub fn apply_event(&mut self, event: &StreamEvent) {
        self.display.handle_event(event);
    }

    /// Apply multiple events.
    pub fn apply_events(&mut self, events: &[StreamEvent]) {
        for event in events {
            self.display.handle_event(event);
        }
    }

    /// Render and return as string for assertions.
    pub fn render(&mut self) -> String {
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                self.display.render(area, frame.buffer_mut());
            })
            .expect("failed to draw");

        buffer_to_string(self.terminal.backend().buffer())
    }

    /// Assert rendered output contains text.
    pub fn assert_contains(&mut self, expected: &str) {
        let rendered = self.render();
        assert!(
            rendered.contains(expected),
            "Expected to find '{}' in:\n{}",
            expected,
            rendered
        );
    }

    /// Assert rendered output does not contain text.
    pub fn assert_not_contains(&mut self, unexpected: &str) {
        let rendered = self.render();
        assert!(
            !rendered.contains(unexpected),
            "Expected NOT to find '{}' in:\n{}",
            unexpected,
            rendered
        );
    }

    /// Get the raw buffer for detailed inspection.
    pub fn buffer(&mut self) -> &Buffer {
        // Render first to ensure buffer is up to date
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                self.display.render(area, frame.buffer_mut());
            })
            .expect("failed to draw");

        self.terminal.backend().buffer()
    }

    /// Get the terminal area.
    pub fn area(&self) -> Rect {
        let size = self.terminal.size().unwrap_or_default();
        Rect::new(0, 0, size.width, size.height)
    }
}

/// Convert a buffer to a string representation.
fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut result = String::new();

    for y in 0..area.height {
        for x in 0..area.width {
            let cell = buffer.cell((x, y)).unwrap();
            result.push_str(cell.symbol());
        }
        // Trim trailing spaces and add newline
        result = result.trim_end().to_string();
        result.push('\n');
    }

    // Trim trailing empty lines
    while result.ends_with("\n\n") {
        result.pop();
    }

    result
}

/// Key event helpers for testing.
pub mod keys {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    pub fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    pub fn esc() -> KeyEvent {
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    }

    pub fn up() -> KeyEvent {
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
    }

    pub fn down() -> KeyEvent {
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)
    }

    pub fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
}

/// Test fixtures using StreamEvent.
pub mod fixtures {
    use super::*;

    /// Create events for a text response with embedded commands.
    pub fn text_with_commands_events() -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta("I'll list the files.\n\n".to_string()),
            StreamEvent::TextDelta("```\nls /tmp\n```\n\n".to_string()),
            StreamEvent::TextDelta("And show contents:\n\n".to_string()),
            StreamEvent::TextDelta("```\ncat file.txt\n```\n".to_string()),
            StreamEvent::Done,
        ]
    }

    /// Create error events.
    pub fn error_events(msg: &str) -> Vec<StreamEvent> {
        vec![StreamEvent::Error(msg.to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayStatus;

    #[test]
    fn test_tui_creation() {
        let tui = TestTui::new(40, 10);
        assert_eq!(tui.area().width, 40);
        assert_eq!(tui.area().height, 10);
    }

    #[test]
    fn test_tui_default_size() {
        let tui = TestTui::default_size();
        assert_eq!(tui.area().width, 80);
        assert_eq!(tui.area().height, 24);
    }

    #[test]
    fn test_text_with_commands_display() {
        let mut tui = TestTui::default_size();

        let events = fixtures::text_with_commands_events();
        tui.apply_events(&events);

        tui.assert_contains("ls /tmp");
        tui.assert_contains("cat file.txt");
        assert_eq!(tui.display.status, DisplayStatus::Streaming);
    }

    #[test]
    fn test_streaming_text_display() {
        let mut tui = TestTui::default_size();

        tui.apply_event(&StreamEvent::TextDelta("Thinking".to_string()));
        tui.assert_contains("Thinking");

        tui.apply_event(&StreamEvent::TextDelta("...".to_string()));
        tui.assert_contains("Thinking...");
    }

    #[test]
    fn test_thinking_display() {
        let mut tui = TestTui::default_size();

        tui.apply_event(&StreamEvent::ThinkingDelta("Reasoning...".to_string()));
        tui.assert_contains("Reasoning...");
        assert_eq!(tui.display.status, DisplayStatus::Thinking);
    }

    #[test]
    fn test_error_display() {
        let mut tui = TestTui::default_size();

        tui.apply_event(&StreamEvent::Error("API rate limited".to_string()));
        tui.assert_contains("Error");
        tui.assert_contains("rate limited");
    }

    #[test]
    fn test_idle_display_is_empty() {
        let mut tui = TestTui::default_size();
        let rendered = tui.render();

        // Should be mostly empty (whitespace)
        let non_whitespace: String = rendered.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            non_whitespace.is_empty(),
            "Expected empty display, got: {}",
            rendered
        );
    }

    #[test]
    fn test_apply_events() {
        let mut tui = TestTui::default_size();

        let events = vec![
            StreamEvent::TextDelta("Hello ".to_string()),
            StreamEvent::TextDelta("World".to_string()),
        ];
        tui.apply_events(&events);

        assert_eq!(tui.display.streaming_text, "Hello World");
    }

    #[test]
    fn test_fixtures_text_with_commands() {
        let events = fixtures::text_with_commands_events();
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn test_fixtures_error() {
        let events = fixtures::error_events("test error");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StreamEvent::Error(msg) if msg == "test error"));
    }
}
