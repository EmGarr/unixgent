#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    Osc133A,
    Osc133B,
    Osc133C,
    Osc133D { exit_code: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    Idle,
    Prompt,
    Input,
    Executing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Ground,
    Escape,
    OscStart,
    OscParam,
}

pub struct OscParser {
    state: ParseState,
    param_buf: Vec<u8>,
    pub terminal_state: TerminalState,
}

impl Default for OscParser {
    fn default() -> Self {
        Self::new()
    }
}

impl OscParser {
    pub fn new() -> Self {
        Self {
            state: ParseState::Ground,
            param_buf: Vec::with_capacity(64),
            terminal_state: TerminalState::Idle,
        }
    }

    /// Feed a single byte. Returns an event if one was completed.
    /// All bytes pass through — this is non-consuming.
    pub fn feed(&mut self, byte: u8) -> Option<OscEvent> {
        match self.state {
            ParseState::Ground => {
                if byte == 0x1b {
                    self.state = ParseState::Escape;
                }
                None
            }
            ParseState::Escape => {
                if byte == b']' {
                    self.state = ParseState::OscStart;
                    self.param_buf.clear();
                } else {
                    self.state = ParseState::Ground;
                }
                None
            }
            ParseState::OscStart => {
                // Accumulate until we see if it starts with "133;"
                if byte == b';' && self.param_buf == b"133" {
                    self.param_buf.clear();
                    self.state = ParseState::OscParam;
                } else if byte == 0x07 || byte == 0x1b {
                    // BEL or ESC (start of ST) — not a 133 sequence
                    self.state = if byte == 0x1b {
                        ParseState::Escape
                    } else {
                        ParseState::Ground
                    };
                    self.param_buf.clear();
                } else {
                    self.param_buf.push(byte);
                    // If we have more than 3 chars and haven't matched "133", abort
                    if self.param_buf.len() > 3 {
                        self.state = ParseState::OscParam;
                        // Continue collecting until terminator, but it won't match 133
                        // Actually, switch to a non-133 OSC collection mode
                        // Simplify: just keep collecting until terminator
                    }
                }
                None
            }
            ParseState::OscParam => {
                // BEL (0x07) terminates, or ESC \ (ST) terminates
                if byte == 0x07 {
                    let event = self.parse_133_param();
                    self.state = ParseState::Ground;
                    self.param_buf.clear();
                    if let Some(ref evt) = event {
                        self.update_terminal_state(evt);
                    }
                    event
                } else if byte == 0x1b {
                    // Could be start of ST (ESC \) — but we'd need to see the next byte.
                    // For simplicity, treat ESC as terminator (the \ that follows is harmless).
                    let event = self.parse_133_param();
                    self.state = ParseState::Escape;
                    self.param_buf.clear();
                    if let Some(ref evt) = event {
                        self.update_terminal_state(evt);
                    }
                    event
                } else {
                    self.param_buf.push(byte);
                    if self.param_buf.len() > 256 {
                        // Prevent unbounded growth on malformed sequences
                        self.state = ParseState::Ground;
                        self.param_buf.clear();
                    }
                    None
                }
            }
        }
    }

    /// Feed a slice of bytes, collecting any events.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Vec<OscEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            if let Some(evt) = self.feed(b) {
                events.push(evt);
            }
        }
        events
    }

    fn parse_133_param(&self) -> Option<OscEvent> {
        let param = &self.param_buf;
        if param.is_empty() {
            return None;
        }
        match param[0] {
            b'A' => Some(OscEvent::Osc133A),
            b'B' => Some(OscEvent::Osc133B),
            b'C' => Some(OscEvent::Osc133C),
            b'D' => {
                let exit_code = if param.len() > 1 && param[1] == b';' {
                    std::str::from_utf8(&param[2..])
                        .ok()
                        .and_then(|s| s.parse::<i32>().ok())
                } else {
                    None
                };
                Some(OscEvent::Osc133D { exit_code })
            }
            _ => None,
        }
    }

    fn update_terminal_state(&mut self, event: &OscEvent) {
        self.terminal_state = match event {
            OscEvent::Osc133A => TerminalState::Prompt,
            OscEvent::Osc133B => TerminalState::Input,
            OscEvent::Osc133C => TerminalState::Executing,
            OscEvent::Osc133D { .. } => TerminalState::Idle,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc_bytes(param: &str) -> Vec<u8> {
        // ESC ] 133 ; <param> BEL
        let mut v = vec![0x1b, b']'];
        v.extend_from_slice(b"133;");
        v.extend_from_slice(param.as_bytes());
        v.push(0x07);
        v
    }

    fn osc_bytes_st(param: &str) -> Vec<u8> {
        // ESC ] 133 ; <param> ESC backslash (ST)
        let mut v = vec![0x1b, b']'];
        v.extend_from_slice(b"133;");
        v.extend_from_slice(param.as_bytes());
        v.extend_from_slice(&[0x1b, b'\\']);
        v
    }

    #[test]
    fn parse_133_a() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("A"));
        assert_eq!(events, vec![OscEvent::Osc133A]);
        assert_eq!(parser.terminal_state, TerminalState::Prompt);
    }

    #[test]
    fn parse_133_b() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("B"));
        assert_eq!(events, vec![OscEvent::Osc133B]);
        assert_eq!(parser.terminal_state, TerminalState::Input);
    }

    #[test]
    fn parse_133_c() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("C"));
        assert_eq!(events, vec![OscEvent::Osc133C]);
        assert_eq!(parser.terminal_state, TerminalState::Executing);
    }

    #[test]
    fn parse_133_d_with_exit_code() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("D;0"));
        assert_eq!(events, vec![OscEvent::Osc133D { exit_code: Some(0) }]);
        assert_eq!(parser.terminal_state, TerminalState::Idle);
    }

    #[test]
    fn parse_133_d_with_nonzero_exit() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("D;127"));
        assert_eq!(
            events,
            vec![OscEvent::Osc133D {
                exit_code: Some(127)
            }]
        );
    }

    #[test]
    fn parse_133_d_without_exit_code() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("D"));
        assert_eq!(events, vec![OscEvent::Osc133D { exit_code: None }]);
    }

    #[test]
    fn parse_st_terminator() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes_st("A"));
        assert_eq!(events, vec![OscEvent::Osc133A]);
    }

    #[test]
    fn interleaved_with_normal_output() {
        let mut parser = OscParser::new();
        let mut data = b"hello world ".to_vec();
        data.extend_from_slice(&osc_bytes("A"));
        data.extend_from_slice(b"some prompt text");
        data.extend_from_slice(&osc_bytes("C"));
        data.extend_from_slice(b"command output");
        let events = parser.feed_bytes(&data);
        assert_eq!(events, vec![OscEvent::Osc133A, OscEvent::Osc133C]);
    }

    #[test]
    fn state_machine_full_cycle() {
        let mut parser = OscParser::new();
        assert_eq!(parser.terminal_state, TerminalState::Idle);

        parser.feed_bytes(&osc_bytes("D;0"));
        assert_eq!(parser.terminal_state, TerminalState::Idle);

        parser.feed_bytes(&osc_bytes("A"));
        assert_eq!(parser.terminal_state, TerminalState::Prompt);

        parser.feed_bytes(&osc_bytes("B"));
        assert_eq!(parser.terminal_state, TerminalState::Input);

        parser.feed_bytes(&osc_bytes("C"));
        assert_eq!(parser.terminal_state, TerminalState::Executing);

        parser.feed_bytes(&osc_bytes("D;0"));
        assert_eq!(parser.terminal_state, TerminalState::Idle);

        parser.feed_bytes(&osc_bytes("A"));
        assert_eq!(parser.terminal_state, TerminalState::Prompt);
    }

    #[test]
    fn ignores_non_133_osc() {
        let mut parser = OscParser::new();
        // OSC 7 (cwd notification)
        let mut data = vec![0x1b, b']'];
        data.extend_from_slice(b"7;file:///tmp");
        data.push(0x07);
        let events = parser.feed_bytes(&data);
        assert!(events.is_empty());
    }

    #[test]
    fn ignores_unknown_133_subparam() {
        let mut parser = OscParser::new();
        let events = parser.feed_bytes(&osc_bytes("Z"));
        assert!(events.is_empty());
    }
}
