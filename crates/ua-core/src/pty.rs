use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{self, Read, Write};

use crate::shell_scripts::{detect_shell, integration_script};

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    /// Temp file for integration script — kept alive so the file isn't deleted.
    _integration_file: Option<tempfile::NamedTempFile>,
}

impl PtySession {
    /// Spawn a shell in a PTY. Returns the session and a reader for the PTY output.
    /// The reader is returned separately so it can be moved to a different thread.
    pub fn spawn(shell_cmd: &str, integration: bool) -> io::Result<(Self, Box<dyn Read + Send>)> {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;

        let kind = detect_shell(shell_cmd);

        // Write integration script to a temp file if enabled.
        // The script ends with `clear` to wipe any startup artifacts.
        let integration_file = if integration {
            if let Some(script) = integration_script(kind) {
                let mut tmpfile = tempfile::NamedTempFile::new()?;
                tmpfile.write_all(script.as_bytes())?;
                tmpfile.flush()?;
                Some(tmpfile)
            } else {
                None
            }
        } else {
            None
        };

        let mut cmd = CommandBuilder::new(shell_cmd);
        // Pass -l for login shell behavior (profile sourcing).
        cmd.arg("-l");
        // Start in the user's current directory.
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd.env(
            "TERM",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
        );

        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;

        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;

        let mut writer = pair.master.take_writer().map_err(io::Error::other)?;

        // Source the integration script by writing a short command to the PTY.
        // Leading space prevents it from being added to shell history.
        // The script itself ends with `clear`, which wipes this source line
        // from the visible terminal.
        if let Some(ref tmpfile) = integration_file {
            let path = tmpfile.path().to_string_lossy();
            // `source` works in bash, zsh, and fish.
            let source_cmd = format!(" source {path}\n");
            writer.write_all(source_cmd.as_bytes())?;
            writer.flush()?;
        }

        let session = Self {
            master: pair.master,
            child,
            writer,
            _integration_file: integration_file,
        };

        Ok((session, reader))
    }

    /// Return the PID of the child shell process (if available).
    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)
    }

    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<u32>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(status.exit_code())),
            Ok(None) => Ok(None),
            Err(e) => Err(io::Error::other(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn spawn_and_echo() {
        let (mut session, mut reader) = PtySession::spawn("/bin/sh", false).expect("spawn failed");

        // Give the shell time to start
        thread::sleep(Duration::from_millis(200));

        // Send a command
        session
            .write_all(b"echo hello_pty_test\n")
            .expect("write failed");

        // Read output
        let mut output = Vec::new();
        let mut buf = [0u8; 1024];
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => output.extend_from_slice(&buf[..n]),
                _ => {}
            }
            if String::from_utf8_lossy(&output).contains("hello_pty_test") {
                break;
            }
        }

        let out_str = String::from_utf8_lossy(&output);
        assert!(
            out_str.contains("hello_pty_test"),
            "expected 'hello_pty_test' in output, got: {out_str}"
        );

        // Exit the shell
        session.write_all(b"exit\n").expect("write exit failed");

        // Wait for child to exit, polling with retries
        let mut exited = false;
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(100));
            // Drain any pending output to prevent the child from blocking on write
            let _ = reader.read(&mut buf);
            if let Ok(Some(_)) = session.try_wait() {
                exited = true;
                break;
            }
        }
        assert!(exited, "child should have exited");
    }
}
