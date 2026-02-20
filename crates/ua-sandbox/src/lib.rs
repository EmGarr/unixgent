//! OS-level filesystem sandbox for UnixAgent.
//!
//! Provides kernel-enforced filesystem isolation using Landlock (Linux) or
//! Seatbelt (macOS). The sandbox is applied to the current process and is
//! irreversible — designed to be used in a child process before exec.
//!
//! # Architecture
//!
//! The parent process (REPL/batch loop) remains unsandboxed. Child commands
//! run via `unixagent --sandbox-exec`, which:
//! 1. Deserializes the policy from `__UA_SANDBOX_POLICY` env var
//! 2. Applies the OS sandbox (Landlock or Seatbelt)
//! 3. Execs the requested command
//!
//! # Usage
//!
//! ```no_run
//! use ua_sandbox::{SandboxPolicy, apply};
//!
//! let policy = SandboxPolicy::default();
//! apply(&policy).expect("sandbox application failed");
//! // Process is now sandboxed — cannot access paths outside the policy
//! ```

pub mod policy;

#[cfg(target_os = "linux")]
pub mod landlock;

#[cfg(target_os = "macos")]
pub mod seatbelt;

pub use policy::SandboxPolicy;

use std::os::unix::process::CommandExt;
use std::process::Command;

/// Errors from sandbox application.
#[derive(Debug)]
pub enum SandboxError {
    /// Platform-specific sandbox error (Landlock or Seatbelt).
    Platform(String),
    /// Policy not found in environment.
    NoPolicyInEnv,
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::Platform(msg) => write!(f, "sandbox error: {msg}"),
            SandboxError::NoPolicyInEnv => {
                write!(f, "sandbox error: __UA_SANDBOX_POLICY env var not set")
            }
        }
    }
}

impl std::error::Error for SandboxError {}

/// Apply the filesystem sandbox to the current process. Irreversible.
///
/// On macOS, uses Seatbelt (`sandbox_init`).
/// On Linux, uses Landlock.
/// On other platforms, returns an error.
pub fn apply(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    #[cfg(target_os = "macos")]
    {
        seatbelt::apply_seatbelt(policy)
    }
    #[cfg(target_os = "linux")]
    {
        landlock::apply_landlock(policy)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = policy;
        Err(SandboxError::Platform(
            "no sandbox implementation for this platform".to_string(),
        ))
    }
}

/// Deserialize policy from env, apply sandbox, exec command. Does not return on success.
///
/// This is the entry point for `unixagent --sandbox-exec <args...>`.
/// On failure, prints an error to stderr and exits with code 126.
pub fn exec_sandboxed(args: &[String]) -> ! {
    // Deserialize policy from environment
    let policy = match SandboxPolicy::from_env() {
        Some(p) => p,
        None => {
            eprintln!("[ua:sandbox] error: {} not set", policy::SANDBOX_ENV_VAR);
            std::process::exit(126);
        }
    };

    // Apply the sandbox (irreversible)
    if let Err(e) = apply(&policy) {
        eprintln!("[ua:sandbox] error: {e}");
        std::process::exit(126);
    }

    eprintln!("[ua:sandbox] active");

    if args.is_empty() {
        eprintln!("[ua:sandbox] error: no command specified");
        std::process::exit(126);
    }

    // Exec the requested command — replaces this process
    let err = Command::new(&args[0]).args(&args[1..]).exec();

    // exec() only returns on error
    eprintln!("[ua:sandbox] exec failed: {err}");
    std::process::exit(126);
}
