use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;

/// Environment variable used to transport sandbox policy to the child process.
pub const SANDBOX_ENV_VAR: &str = "__UA_SANDBOX_POLICY";

/// Filesystem sandbox policy.
///
/// Defines which paths the sandboxed process may read, write, or must be denied.
/// Default-deny: any path not listed in `writable` or `readable` is inaccessible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SandboxPolicy {
    /// Paths the child may read and write.
    pub writable: Vec<PathBuf>,
    /// Paths the child may read (but not write).
    pub readable: Vec<PathBuf>,
    /// Paths explicitly denied (overrides readable/writable on platforms that support it).
    pub denied: Vec<PathBuf>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::from_config(
            &[
                "$CWD".to_string(),
                "/tmp".to_string(),
                "$HOME/.local/share/unixagent".to_string(),
            ],
            &{
                let mut r = vec![
                    "/usr".to_string(),
                    "/bin".to_string(),
                    "/sbin".to_string(),
                    "/lib".to_string(),
                    "/lib64".to_string(),
                    "/etc".to_string(),
                    "/opt".to_string(),
                    "/dev/null".to_string(),
                    "/dev/urandom".to_string(),
                    "/dev/tty".to_string(),
                ];
                if cfg!(target_os = "macos") {
                    r.extend([
                        "/System".to_string(),
                        "/Library".to_string(),
                        "/private/tmp".to_string(),
                        "/private/var/db".to_string(),
                    ]);
                }
                r
            },
            &[
                "$HOME/.ssh".to_string(),
                "$HOME/.gnupg".to_string(),
                "$HOME/.aws".to_string(),
            ],
        )
    }
}

impl SandboxPolicy {
    /// Build a policy from config strings, resolving `$CWD` and `$HOME` placeholders.
    ///
    /// Paths are canonicalized where possible to handle symlinks (e.g., macOS
    /// `/tmp` → `/private/tmp`). Both the original and canonical paths are
    /// included in writable/readable lists to handle either form.
    pub fn from_config(writable: &[String], readable: &[String], denied: &[String]) -> Self {
        Self {
            writable: resolve_and_canonicalize(writable),
            readable: resolve_and_canonicalize(readable),
            denied: denied.iter().map(|s| resolve_path(s)).collect(),
        }
    }

    /// Serialize to JSON for transport via environment variable.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SandboxPolicy serialization cannot fail")
    }

    /// Deserialize from JSON (e.g., from environment variable).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Deserialize from the `__UA_SANDBOX_POLICY` environment variable.
    pub fn from_env() -> Option<Self> {
        env::var(SANDBOX_ENV_VAR)
            .ok()
            .and_then(|json| Self::from_json(&json).ok())
    }
}

/// Resolve paths and include canonical forms for symlink handling.
///
/// For each path, includes both the original resolved form and the
/// canonicalized form (if different). This ensures that macOS symlinks
/// like `/tmp` → `/private/tmp` are handled correctly.
fn resolve_and_canonicalize(paths: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for s in paths {
        let resolved = resolve_path(s);
        if let Ok(canonical) = resolved.canonicalize() {
            if canonical != resolved {
                result.push(canonical);
            }
        }
        result.push(resolved);
    }
    result
}

/// Resolve path placeholders: `$CWD` → current_dir(), `$HOME` → $HOME env var.
fn resolve_path(s: &str) -> PathBuf {
    match s {
        "$CWD" => env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        "$HOME" => env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(".")),
        other => {
            // Handle paths starting with $HOME/
            if let Some(rest) = other.strip_prefix("$HOME/") {
                let home = env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("."));
                home.join(rest)
            } else {
                PathBuf::from(other)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_has_cwd_writable() {
        let policy = SandboxPolicy::default();
        let cwd = env::current_dir().unwrap();
        assert!(policy.writable.contains(&cwd));
        assert!(policy.writable.contains(&PathBuf::from("/tmp")));
        let home = env::var("HOME").unwrap();
        assert!(policy
            .writable
            .contains(&PathBuf::from(format!("{home}/.local/share/unixagent"))));
    }

    #[test]
    fn default_policy_denies_ssh() {
        let policy = SandboxPolicy::default();
        let home = env::var("HOME").unwrap();
        assert!(policy
            .denied
            .contains(&PathBuf::from(format!("{home}/.ssh"))));
    }

    #[test]
    fn default_policy_has_system_readable() {
        let policy = SandboxPolicy::default();
        assert!(policy.readable.contains(&PathBuf::from("/usr")));
        assert!(policy.readable.contains(&PathBuf::from("/bin")));
        assert!(policy.readable.contains(&PathBuf::from("/etc")));
    }

    #[test]
    fn json_round_trip() {
        let policy = SandboxPolicy::default();
        let json = policy.to_json();
        let restored = SandboxPolicy::from_json(&json).unwrap();
        assert_eq!(policy, restored);
    }

    #[test]
    fn from_config_resolves_cwd() {
        let policy = SandboxPolicy::from_config(
            &["$CWD".to_string(), "/tmp".to_string()],
            &["/usr".to_string()],
            &["$HOME/.ssh".to_string()],
        );
        let cwd = env::current_dir().unwrap();
        assert!(policy.writable.contains(&cwd));
        assert!(policy.writable.contains(&PathBuf::from("/tmp")));
        assert!(policy.readable.contains(&PathBuf::from("/usr")));
        let home = env::var("HOME").unwrap();
        assert!(policy
            .denied
            .contains(&PathBuf::from(format!("{home}/.ssh"))));
    }

    #[test]
    fn from_config_resolves_home() {
        let policy = SandboxPolicy::from_config(&["$HOME".to_string()], &[], &[]);
        let home = env::var("HOME").map(PathBuf::from).unwrap();
        assert!(policy.writable.contains(&home));
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        // Ensure the env var is not set
        env::remove_var(SANDBOX_ENV_VAR);
        assert!(SandboxPolicy::from_env().is_none());
    }
}
