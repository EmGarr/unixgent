//! Integration tests for sandbox enforcement.
//!
//! These tests spawn a child process that applies the sandbox and attempts
//! file operations, verifying that the sandbox correctly allows/denies access.
//!
//! The tests use the `unixagent` binary's `--sandbox-exec` subcommand.

use std::env;
use std::process::Command;
use ua_sandbox::policy::SANDBOX_ENV_VAR;
use ua_sandbox::SandboxPolicy;

/// Find the unixagent binary in the target directory.
fn unixagent_bin() -> std::path::PathBuf {
    // When running tests, the binary is in target/debug/
    let mut path = env::current_exe().unwrap();
    // Go up from the test binary to target/debug/
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("unixagent");
    if !path.exists() {
        panic!(
            "unixagent binary not found at {}. Run `cargo build -p ua-core` first.",
            path.display()
        );
    }
    path
}

#[test]
fn sandbox_allows_ls_tmp() {
    let policy = SandboxPolicy::default();
    let json = policy.to_json();

    let output = Command::new(unixagent_bin())
        .arg("--sandbox-exec")
        .arg("ls")
        .arg("/tmp")
        .env(SANDBOX_ENV_VAR, &json)
        .output()
        .expect("failed to execute");

    assert!(
        output.status.success(),
        "ls /tmp should succeed in sandbox. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_denies_read_ssh() {
    let home = env::var("HOME").unwrap();
    let ssh_dir = format!("{home}/.ssh");

    // Skip if ~/.ssh doesn't exist
    if !std::path::Path::new(&ssh_dir).exists() {
        eprintln!("skipping test: ~/.ssh does not exist");
        return;
    }

    let policy = SandboxPolicy::default();
    let json = policy.to_json();

    let output = Command::new(unixagent_bin())
        .arg("--sandbox-exec")
        .arg("ls")
        .arg(&ssh_dir)
        .env(SANDBOX_ENV_VAR, &json)
        .output()
        .expect("failed to execute");

    assert!(
        !output.status.success(),
        "ls ~/.ssh should fail in sandbox. stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_allows_write_to_tmp() {
    let policy = SandboxPolicy::default();
    let json = policy.to_json();

    let test_file = format!("/tmp/ua-sandbox-test-{}", std::process::id());

    let output = Command::new(unixagent_bin())
        .arg("--sandbox-exec")
        .arg("sh")
        .arg("-c")
        .arg(format!(
            "echo test > {test_file} && cat {test_file} && rm {test_file}"
        ))
        .env(SANDBOX_ENV_VAR, &json)
        .output()
        .expect("failed to execute");

    assert!(
        output.status.success(),
        "writing to /tmp should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test"),
        "should have written and read back 'test'"
    );
}

#[test]
fn sandbox_missing_env_exits_126() {
    let output = Command::new(unixagent_bin())
        .arg("--sandbox-exec")
        .arg("ls")
        .arg("/tmp")
        // Deliberately not setting SANDBOX_ENV_VAR
        .env_remove(SANDBOX_ENV_VAR)
        .output()
        .expect("failed to execute");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code,
        126,
        "missing env var should exit 126. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sandbox_stderr_shows_active() {
    let policy = SandboxPolicy::default();
    let json = policy.to_json();

    let output = Command::new(unixagent_bin())
        .arg("--sandbox-exec")
        .arg("true")
        .env(SANDBOX_ENV_VAR, &json)
        .output()
        .expect("failed to execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[ua:sandbox] active"),
        "stderr should contain sandbox active message. stderr: {stderr}"
    );
}
