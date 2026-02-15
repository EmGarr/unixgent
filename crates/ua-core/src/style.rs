//! Terminal styling helpers with NO_COLOR support.

/// Check if color output is enabled (respects `NO_COLOR` env var).
pub fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

/// Terminal style helper that respects NO_COLOR.
pub struct Style {
    enabled: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self::new()
    }
}

impl Style {
    pub fn new() -> Self {
        Self {
            enabled: color_enabled(),
        }
    }

    /// Create a style with colors explicitly enabled (for tests).
    pub fn force_enabled() -> Self {
        Self { enabled: true }
    }

    /// Create a style with colors explicitly disabled.
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    pub fn dim_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[2m"
        } else {
            ""
        }
    }

    pub fn bold_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[1m"
        } else {
            ""
        }
    }

    pub fn red_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[31m"
        } else {
            ""
        }
    }

    pub fn yellow_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[33m"
        } else {
            ""
        }
    }

    pub fn green_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[32m"
        } else {
            ""
        }
    }

    pub fn cyan_start(&self) -> &'static str {
        if self.enabled {
            "\x1b[36m"
        } else {
            ""
        }
    }

    pub fn reset(&self) -> &'static str {
        if self.enabled {
            "\x1b[0m"
        } else {
            ""
        }
    }
}

/// Compute visible width of a string, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            width += unicode_display_width(c);
        }
    }
    width
}

/// Approximate display width of a character.
/// CJK characters and some symbols are double-width.
fn unicode_display_width(c: char) -> usize {
    // Control characters have zero width
    if c < ' ' {
        return 0;
    }
    // ASCII is single-width
    if c.is_ascii() {
        return 1;
    }
    // CJK Unified Ideographs and common double-width ranges
    let cp = c as u32;
    if (0x1100..=0x115F).contains(&cp)       // Hangul Jamo
        || (0x2E80..=0x303E).contains(&cp)   // CJK Radicals
        || (0x3040..=0x33BF).contains(&cp)   // Hiragana, Katakana, CJK
        || (0x3400..=0x4DBF).contains(&cp)   // CJK Extension A
        || (0x4E00..=0x9FFF).contains(&cp)   // CJK Unified
        || (0xF900..=0xFAFF).contains(&cp)   // CJK Compatibility
        || (0xFE30..=0xFE6F).contains(&cp)   // CJK Compatibility Forms
        || (0xFF01..=0xFF60).contains(&cp)    // Fullwidth Forms
        || (0x20000..=0x2FFFF).contains(&cp)
    // CJK Extension B+
    {
        2
    } else {
        1
    }
}

/// Format a token count for display: `340` for small, `1.2k` for 1000+.
pub fn format_tokens(n: u32) -> String {
    if n >= 1000 {
        let k = n as f64 / 1000.0;
        format!("{k:.1}k")
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_disabled_returns_empty_codes() {
        let style = Style::disabled();
        assert_eq!(style.dim_start(), "");
        assert_eq!(style.bold_start(), "");
        assert_eq!(style.red_start(), "");
        assert_eq!(style.yellow_start(), "");
        assert_eq!(style.green_start(), "");
        assert_eq!(style.cyan_start(), "");
        assert_eq!(style.reset(), "");
    }

    #[test]
    fn color_enabled_returns_escape_codes() {
        let style = Style::force_enabled();
        assert_eq!(style.dim_start(), "\x1b[2m");
        assert_eq!(style.reset(), "\x1b[0m");
        assert_eq!(style.red_start(), "\x1b[31m");
    }

    #[test]
    fn visible_width_plain_text() {
        assert_eq!(visible_width("hello"), 5);
    }

    #[test]
    fn visible_width_strips_ansi() {
        assert_eq!(visible_width("\x1b[31mhello\x1b[0m"), 5);
    }

    #[test]
    fn visible_width_multibyte_utf8() {
        // ❯ = 1 width, space, cmd, spaces, ▐ = 1 width, space, write
        assert_eq!(visible_width("❯ cmd  ▐ write"), 14);
    }

    #[test]
    fn visible_width_empty() {
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(340), "340");
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1200), "1.2k");
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(15600), "15.6k");
    }
}
