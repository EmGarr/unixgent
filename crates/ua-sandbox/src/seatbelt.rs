//! macOS Seatbelt (sandbox_init) implementation.
//!
//! Generates an SBPL (Sandbox Profile Language) policy string and applies it
//! via the `sandbox_init()` FFI call. Once applied, the sandbox is irreversible.
//!
//! Strategy: allow all non-file operations and file reads broadly, then
//! deny file writes by default and selectively allow them for policy paths.
//! Sensitive paths (e.g., ~/.ssh) get explicit deny rules for both read and
//! write. In SBPL, more-specific deny rules beat same-specificity allows.

use std::ffi::{CStr, CString};
use std::ptr;

use crate::policy::SandboxPolicy;
use crate::SandboxError;

extern "C" {
    fn sandbox_init(
        profile: *const libc::c_char,
        flags: u64,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;

    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}

/// `kSBXProfileString` — interpret the profile parameter as a string.
const SBPL_PROFILE_STRING: u64 = 0;

/// Apply the Seatbelt sandbox to the current process. Irreversible.
pub fn apply_seatbelt(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    let sbpl = generate_sbpl(policy);
    let c_profile = CString::new(sbpl.as_str())
        .map_err(|e| SandboxError::Platform(format!("SBPL contains null byte: {e}")))?;

    let mut errorbuf: *mut libc::c_char = ptr::null_mut();
    let ret = unsafe { sandbox_init(c_profile.as_ptr(), SBPL_PROFILE_STRING, &mut errorbuf) };

    if ret != 0 {
        let msg = if !errorbuf.is_null() {
            let err = unsafe { CStr::from_ptr(errorbuf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(errorbuf) };
            err
        } else {
            "unknown sandbox_init error".to_string()
        };
        return Err(SandboxError::Platform(format!(
            "sandbox_init failed: {msg}"
        )));
    }

    Ok(())
}

/// Generate an SBPL (Sandbox Profile Language) string from a SandboxPolicy.
///
/// The profile:
/// 1. Denies everything by default
/// 2. Allows all non-file operations (process, mach, ipc, signal, sysctl, network)
/// 3. Allows all file operations, then denies file-write* globally
/// 4. Selectively re-allows file-write* for policy writable paths
/// 5. Explicitly denies file-read* and file-write* for sensitive paths
///
/// This gives: read everywhere (except denied), write only to whitelisted paths.
pub fn generate_sbpl(policy: &SandboxPolicy) -> String {
    let mut sbpl = String::new();

    sbpl.push_str("(version 1)\n");
    sbpl.push_str("(deny default)\n");

    // --- Non-file operations: allow broadly ---
    sbpl.push_str("(allow process*)\n");
    sbpl.push_str("(allow mach*)\n");
    sbpl.push_str("(allow ipc*)\n");
    sbpl.push_str("(allow signal)\n");
    sbpl.push_str("(allow sysctl*)\n");
    sbpl.push_str("(allow network*)\n"); // restricted in Phase 4.5b
    sbpl.push_str("(allow pseudo-tty)\n");

    // --- File operations: allow reads, deny writes by default ---
    sbpl.push_str("(allow file*)\n");
    sbpl.push_str("(deny file-write*)\n");

    // --- Writable paths: re-allow file-write* ---
    // /dev is always writable (stdout, stderr, pty)
    sbpl.push_str("(allow file-write* (subpath \"/dev\"))\n");
    // Process temp dirs (dyld cache, libSystem)
    sbpl.push_str("(allow file-write* (subpath \"/private/var/folders\"))\n");

    for path in &policy.writable {
        let p = path.display();
        sbpl.push_str(&format!("(allow file-write* (subpath \"{p}\"))\n"));
    }

    // --- Explicit deny for sensitive paths (read AND write) ---
    // These are more specific than `(allow file*)` so they take precedence.
    for path in &policy.denied {
        let p = path.display();
        sbpl.push_str(&format!("(deny file-read* (subpath \"{p}\"))\n"));
        sbpl.push_str(&format!("(deny file-write* (subpath \"{p}\"))\n"));
    }

    sbpl
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sbpl_has_deny_default() {
        let policy = SandboxPolicy {
            writable: vec![],
            readable: vec![],
            denied: vec![],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(version 1)"));
        assert!(sbpl.contains("(deny default)"));
    }

    #[test]
    fn sbpl_allows_non_file_operations() {
        let policy = SandboxPolicy {
            writable: vec![],
            readable: vec![],
            denied: vec![],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(allow process*)"));
        assert!(sbpl.contains("(allow mach*)"));
        assert!(sbpl.contains("(allow ipc*)"));
        assert!(sbpl.contains("(allow signal)"));
        assert!(sbpl.contains("(allow sysctl*)"));
        assert!(sbpl.contains("(allow network*)"));
    }

    #[test]
    fn sbpl_allows_file_reads_denies_writes() {
        let policy = SandboxPolicy {
            writable: vec![],
            readable: vec![],
            denied: vec![],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(allow file*)"));
        assert!(sbpl.contains("(deny file-write*)"));
    }

    #[test]
    fn sbpl_allows_writable_paths() {
        let policy = SandboxPolicy {
            writable: vec![PathBuf::from("/tmp"), PathBuf::from("/home/user/project")],
            readable: vec![],
            denied: vec![],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(sbpl.contains("(allow file-write* (subpath \"/home/user/project\"))"));
    }

    #[test]
    fn sbpl_denies_sensitive_paths() {
        let policy = SandboxPolicy {
            writable: vec![],
            readable: vec![],
            denied: vec![PathBuf::from("/home/user/.ssh")],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(deny file-read* (subpath \"/home/user/.ssh\"))"));
        assert!(sbpl.contains("(deny file-write* (subpath \"/home/user/.ssh\"))"));
    }

    #[test]
    fn sbpl_allows_dev_writes() {
        let policy = SandboxPolicy {
            writable: vec![],
            readable: vec![],
            denied: vec![],
        };
        let sbpl = generate_sbpl(&policy);
        assert!(sbpl.contains("(allow file-write* (subpath \"/dev\"))"));
        assert!(sbpl.contains("(allow file-write* (subpath \"/private/var/folders\"))"));
    }
}
