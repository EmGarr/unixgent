//! Integration tests for shell integration with OSC 133 sequences.
//!
//! These tests spawn real shells in a PTY with shell integration scripts
//! and verify that OSC 133 A/B/C/D sequences are emitted correctly.

use std::io::Read;
use std::process::Command;
use std::thread;
use std::time::Duration;

use ua_core::osc::{OscEvent, OscParser};
use ua_core::pty::PtySession;

/// Check if a shell is available on the system.
fn shell_available(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Spawn a shell with integration, run a command, and collect OSC events.
fn collect_osc_events(shell: &str, command: &str, timeout_ms: u64) -> Vec<OscEvent> {
    let (mut session, mut reader) =
        PtySession::spawn(shell, true, None).expect("failed to spawn shell");

    let mut parser = OscParser::new();
    let mut events = Vec::new();
    let mut buf = [0u8; 4096];

    // Wait for shell startup + integration script to load
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);

    // Drain initial output (startup, clear, prompt)
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        match reader.read(&mut buf) {
            Ok(n) if n > 0 => {
                let osc_events = parser.feed_bytes(&buf[..n]);
                events.extend(osc_events);
            }
            _ => {}
        }
        // Wait until we see the first prompt cycle (133;A + 133;B)
        if events.iter().any(|e| *e == OscEvent::Osc133B) {
            break;
        }
    }

    // Clear events from startup
    events.clear();

    // Send the test command
    session
        .write_all(format!("{command}\n").as_bytes())
        .expect("write failed");

    // Collect events from command execution
    let cmd_deadline = std::time::Instant::now() + Duration::from_millis(3000);
    let mut saw_done = false;
    while std::time::Instant::now() < cmd_deadline {
        thread::sleep(Duration::from_millis(100));
        match reader.read(&mut buf) {
            Ok(n) if n > 0 => {
                let osc_events = parser.feed_bytes(&buf[..n]);
                events.extend(osc_events);
            }
            _ => {}
        }
        // Wait until we see 133;D (command done) followed by 133;B (next prompt ready)
        if events.iter().any(|e| matches!(e, OscEvent::Osc133D { .. })) {
            saw_done = true;
        }
        if saw_done && events.iter().filter(|e| *e == &OscEvent::Osc133B).count() > 0 {
            break;
        }
    }

    // Clean exit
    let _ = session.write_all(b"exit\n");
    thread::sleep(Duration::from_millis(200));

    events
}

#[test]
fn bash_osc133_sequences() {
    if !shell_available("bash") {
        eprintln!("skipping: bash not available");
        return;
    }

    let events = collect_osc_events("bash", "echo hello_bash_test", 5000);

    // Should see: C (preexec), D (command done), A (prompt start), B (prompt ready)
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133C),
        "expected 133;C in bash events: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, OscEvent::Osc133D { .. })),
        "expected 133;D in bash events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133A),
        "expected 133;A in bash events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133B),
        "expected 133;B in bash events: {events:?}"
    );
}

#[test]
fn zsh_osc133_sequences() {
    if !shell_available("zsh") {
        eprintln!("skipping: zsh not available");
        return;
    }

    let events = collect_osc_events("zsh", "echo hello_zsh_test", 5000);

    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133C),
        "expected 133;C in zsh events: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, OscEvent::Osc133D { .. })),
        "expected 133;D in zsh events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133A),
        "expected 133;A in zsh events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133B),
        "expected 133;B in zsh events: {events:?}"
    );
}

#[test]
fn fish_osc133_sequences() {
    if !shell_available("fish") {
        eprintln!("skipping: fish not available");
        return;
    }

    let events = collect_osc_events("fish", "echo hello_fish_test", 5000);

    assert!(
        events.iter().any(|e| matches!(e, OscEvent::Osc133D { .. })),
        "expected 133;D in fish events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133A),
        "expected 133;A in fish events: {events:?}"
    );
    assert!(
        events.iter().any(|e| *e == OscEvent::Osc133B),
        "expected 133;B in fish events: {events:?}"
    );
}

#[test]
fn bash_exit_code_in_133d() {
    if !shell_available("bash") {
        eprintln!("skipping: bash not available");
        return;
    }

    let events = collect_osc_events("bash", "false", 5000);

    // `false` returns exit code 1 — should appear in 133;D
    let has_nonzero_exit = events.iter().any(|e| {
        if let OscEvent::Osc133D { exit_code } = e {
            *exit_code == Some(1)
        } else {
            false
        }
    });
    assert!(
        has_nonzero_exit,
        "expected 133;D with exit code 1 for `false` command: {events:?}"
    );
}

#[test]
fn bash_exit_code_zero() {
    if !shell_available("bash") {
        eprintln!("skipping: bash not available");
        return;
    }

    let events = collect_osc_events("bash", "true", 5000);

    // `true` returns exit code 0
    let has_zero_exit = events.iter().any(|e| {
        if let OscEvent::Osc133D { exit_code } = e {
            *exit_code == Some(0)
        } else {
            false
        }
    });
    assert!(
        has_zero_exit,
        "expected 133;D with exit code 0 for `true` command: {events:?}"
    );
}
