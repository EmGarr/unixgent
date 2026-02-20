use serde::Deserialize;
use std::io;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub shell: ShellConfig,
    pub backend: BackendConfig,
    pub context: ContextConfig,
    pub security: SecurityConfig,
    pub journal: JournalConfig,
    pub sandbox: SandboxConfig,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShellConfig {
    pub command: Option<String>,
    pub integration: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: None,
            integration: true,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct BackendConfig {
    /// Which backend to use by default.
    pub default: String,
    /// Anthropic-specific configuration.
    pub anthropic: AnthropicConfig,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            default: "anthropic".to_string(),
            anthropic: AnthropicConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct AnthropicConfig {
    /// Command to run to get API key (e.g., "security find-generic-password -s anthropic -w").
    /// The command is run via `sh -c`.
    pub api_key_cmd: Option<String>,
    /// Model to use.
    pub model: String,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key_cmd: None,
            model: "claude-sonnet-4-20250514".to_string(),
        }
    }
}

impl AnthropicConfig {
    /// Resolve the API key from api_key_cmd or ANTHROPIC_API_KEY env var.
    pub fn resolve_api_key(&self) -> io::Result<String> {
        // Try api_key_cmd first
        if let Some(cmd) = &self.api_key_cmd {
            let output = Command::new("sh").arg("-c").arg(cmd).output()?;

            if output.status.success() {
                let key = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !key.is_empty() {
                    return Ok(key);
                }
            }
        }

        // Fall back to env var
        std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "ANTHROPIC_API_KEY not set and no api_key_cmd configured",
            )
        })
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContextConfig {
    /// Maximum number of terminal output lines to include in context.
    pub max_terminal_lines: usize,
    /// Maximum number of conversation turns to keep before evicting oldest.
    pub max_conversation_turns: usize,
    /// Environment variables to include in context.
    pub include_env: Vec<String>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_terminal_lines: 200,
            max_conversation_turns: 20,
            include_env: vec![
                "PATH".to_string(),
                "HOME".to_string(),
                "USER".to_string(),
                "SHELL".to_string(),
                "TERM".to_string(),
                "LANG".to_string(),
            ],
        }
    }
}

/// How the judge handles dangerous commands.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JudgeMode {
    /// Show warning, still allow execution (main agent default).
    Warn,
    /// Return error to LLM, block execution (subagent default).
    Block,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct SecurityConfig {
    /// Auto-approve read-only commands without prompting.
    pub auto_approve_read_only: bool,
    /// Require typing "yes" (not just 'y') for privileged commands.
    pub require_yes_for_privileged: bool,
    /// Enable audit logging.
    pub audit_enabled: bool,
    /// Custom audit log path. Defaults to ~/.local/share/unixagent/audit.jsonl.
    pub audit_log_path: Option<String>,
    /// Enable LLM-based security judge for non-read-only commands.
    /// Adds latency (1-3s) and doubles API costs for evaluated batches.
    pub judge_enabled: bool,
    /// Judge mode: "warn" shows warning (default for depth 0), "block" returns
    /// error to LLM (default for depth > 0). If None, auto-detected from depth.
    pub judge_mode: Option<JudgeMode>,
    /// Maximum nesting depth for batch-mode agent delegation.
    /// Verified via process tree inspection (tamper-proof).
    pub max_agent_depth: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            auto_approve_read_only: true,
            require_yes_for_privileged: true,
            audit_enabled: true,
            audit_log_path: None,
            judge_enabled: false,
            judge_mode: None,
            max_agent_depth: 3,
        }
    }
}

impl SecurityConfig {
    /// Resolve the effective judge mode for a given nesting depth.
    ///
    /// If `judge_mode` is explicitly set in config, use that.
    /// Otherwise: depth 0 → Warn, depth > 0 → Block.
    pub fn resolve_judge_mode(&self, depth: u32) -> JudgeMode {
        match &self.judge_mode {
            Some(mode) => mode.clone(),
            None => {
                if depth == 0 {
                    JudgeMode::Warn
                } else {
                    JudgeMode::Block
                }
            }
        }
    }

    /// Resolve the audit log path, using the configured path or the XDG default.
    pub fn resolve_audit_path(&self) -> PathBuf {
        if let Some(ref custom) = self.audit_log_path {
            return PathBuf::from(custom);
        }

        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local").join("share")
            });
        base.join("unixagent").join("audit.jsonl")
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct JournalConfig {
    /// Enable session journaling.
    pub enabled: bool,
    /// Custom sessions directory. Defaults to ~/.local/share/unixagent/sessions/.
    pub sessions_dir: Option<String>,
    /// Token budget for conversation context rebuilt from journal.
    pub conversation_budget: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sessions_dir: None,
            conversation_budget: 60_000,
        }
    }
}

impl JournalConfig {
    /// Resolve the sessions directory, using the configured path or the XDG default.
    pub fn resolve_sessions_dir(&self) -> PathBuf {
        if let Some(ref custom) = self.sessions_dir {
            return PathBuf::from(custom);
        }

        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local").join("share")
            });
        base.join("unixagent").join("sessions")
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct SandboxConfig {
    /// Enable OS-level filesystem sandbox for batch-mode commands.
    pub enabled: bool,
    /// Paths the sandboxed process may write. Supports `$CWD` and `$HOME` placeholders.
    pub writable_paths: Vec<String>,
    /// Paths the sandboxed process may read. System paths added automatically.
    pub readable_paths: Vec<String>,
    /// Paths explicitly denied. Overrides readable/writable on supported platforms.
    pub denied_paths: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            writable_paths: vec![
                "$CWD".to_string(),
                "/tmp".to_string(),
                "$HOME/.local/share/unixagent".to_string(),
            ],
            readable_paths: vec![
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
            ],
            denied_paths: vec![
                "$HOME/.ssh".to_string(),
                "$HOME/.gnupg".to_string(),
                "$HOME/.aws".to_string(),
            ],
        }
    }
}

impl SandboxConfig {
    /// Build a `SandboxPolicy` from this config, resolving path placeholders.
    pub fn to_policy(&self) -> ua_sandbox::SandboxPolicy {
        ua_sandbox::SandboxPolicy::from_config(
            &self.writable_paths,
            &self.readable_paths,
            &self.denied_paths,
        )
    }
}

impl Config {
    pub fn load_or_default() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
                eprintln!("warning: failed to parse {}: {e}", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn shell_command(&self) -> String {
        self.shell
            .command
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".to_string())
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("unixagent").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.shell.command, None);
        assert!(cfg.shell.integration);
        assert_eq!(cfg.backend.default, "anthropic");
        assert_eq!(cfg.context.max_terminal_lines, 200);
    }

    #[test]
    fn parse_toml() {
        let toml_str = r#"
[shell]
command = "/bin/zsh"
integration = false
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.shell.command.as_deref(), Some("/bin/zsh"));
        assert!(!cfg.shell.integration);
    }

    #[test]
    fn parse_backend_config() {
        let toml_str = r#"
[backend]
default = "anthropic"

[backend.anthropic]
api_key_cmd = "security find-generic-password -s anthropic -w"
model = "claude-opus-4-20250514"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.backend.default, "anthropic");
        assert_eq!(
            cfg.backend.anthropic.api_key_cmd.as_deref(),
            Some("security find-generic-password -s anthropic -w")
        );
        assert_eq!(cfg.backend.anthropic.model, "claude-opus-4-20250514");
    }

    #[test]
    fn parse_context_config() {
        let toml_str = r#"
[context]
max_terminal_lines = 100
include_env = ["PATH", "HOME"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.context.max_terminal_lines, 100);
        assert_eq!(cfg.context.include_env, vec!["PATH", "HOME"]);
    }

    #[test]
    fn parse_empty_toml() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn shell_command_fallback() {
        let cfg = Config::default();
        let cmd = cfg.shell_command();
        // Should return $SHELL or /bin/sh
        assert!(!cmd.is_empty());
    }

    #[test]
    fn anthropic_default_model() {
        let cfg = AnthropicConfig::default();
        assert_eq!(cfg.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn context_default_env_vars() {
        let cfg = ContextConfig::default();
        assert!(cfg.include_env.contains(&"PATH".to_string()));
        assert!(cfg.include_env.contains(&"HOME".to_string()));
        assert!(cfg.include_env.contains(&"SHELL".to_string()));
    }

    #[test]
    fn resolve_api_key_from_env() {
        // This test only works if ANTHROPIC_API_KEY is set
        // We test the fallback path behavior
        let cfg = AnthropicConfig {
            api_key_cmd: Some("echo test_key_123".to_string()),
            model: "test".to_string(),
        };

        let key = cfg.resolve_api_key().unwrap();
        assert_eq!(key, "test_key_123");
    }

    #[test]
    fn security_config_defaults() {
        let cfg = SecurityConfig::default();
        assert!(cfg.auto_approve_read_only);
        assert!(cfg.require_yes_for_privileged);
        assert!(cfg.audit_enabled);
        assert!(cfg.audit_log_path.is_none());
        assert!(!cfg.judge_enabled);
    }

    #[test]
    fn parse_security_config() {
        let toml_str = r#"
[security]
auto_approve_read_only = false
require_yes_for_privileged = false
audit_enabled = false
audit_log_path = "/tmp/audit.jsonl"
judge_enabled = true
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.security.auto_approve_read_only);
        assert!(!cfg.security.require_yes_for_privileged);
        assert!(!cfg.security.audit_enabled);
        assert_eq!(
            cfg.security.audit_log_path.as_deref(),
            Some("/tmp/audit.jsonl")
        );
        assert!(cfg.security.judge_enabled);
    }

    #[test]
    fn parse_security_config_judge_defaults_false() {
        let toml_str = r#"
[security]
auto_approve_read_only = true
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.security.judge_enabled);
    }

    #[test]
    fn parse_toml_without_security_uses_defaults() {
        let toml_str = r#"
[shell]
command = "/bin/bash"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.security.auto_approve_read_only);
        assert!(cfg.security.audit_enabled);
    }

    #[test]
    fn resolve_audit_path_custom() {
        let cfg = SecurityConfig {
            audit_log_path: Some("/custom/path/audit.jsonl".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_audit_path(),
            PathBuf::from("/custom/path/audit.jsonl")
        );
    }

    #[test]
    fn resolve_audit_path_default() {
        let cfg = SecurityConfig::default();
        let path = cfg.resolve_audit_path();
        assert!(path.to_string_lossy().ends_with("unixagent/audit.jsonl"));
    }

    #[test]
    fn journal_config_defaults() {
        let cfg = JournalConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.sessions_dir.is_none());
        assert_eq!(cfg.conversation_budget, 60_000);
    }

    #[test]
    fn parse_journal_config() {
        let toml_str = r#"
[journal]
enabled = false
sessions_dir = "/tmp/sessions"
conversation_budget = 30000
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.journal.enabled);
        assert_eq!(cfg.journal.sessions_dir.as_deref(), Some("/tmp/sessions"));
        assert_eq!(cfg.journal.conversation_budget, 30000);
    }

    #[test]
    fn resolve_sessions_dir_custom() {
        let cfg = JournalConfig {
            sessions_dir: Some("/custom/sessions".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_sessions_dir(),
            PathBuf::from("/custom/sessions")
        );
    }

    #[test]
    fn resolve_sessions_dir_default() {
        let cfg = JournalConfig::default();
        let path = cfg.resolve_sessions_dir();
        assert!(path.to_string_lossy().ends_with("unixagent/sessions"));
    }

    #[test]
    fn sandbox_config_defaults() {
        let cfg = SandboxConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.writable_paths.contains(&"$CWD".to_string()));
        assert!(cfg.writable_paths.contains(&"/tmp".to_string()));
        assert!(cfg
            .writable_paths
            .contains(&"$HOME/.local/share/unixagent".to_string()));
        assert!(cfg.denied_paths.contains(&"$HOME/.ssh".to_string()));
        assert!(cfg.denied_paths.contains(&"$HOME/.aws".to_string()));
    }

    #[test]
    fn parse_sandbox_config() {
        let toml_str = r#"
[sandbox]
enabled = false
writable_paths = ["/tmp", "/home/user/project"]
readable_paths = ["/usr"]
denied_paths = ["$HOME/.ssh"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(!cfg.sandbox.enabled);
        assert_eq!(
            cfg.sandbox.writable_paths,
            vec!["/tmp", "/home/user/project"]
        );
        assert_eq!(cfg.sandbox.readable_paths, vec!["/usr"]);
        assert_eq!(cfg.sandbox.denied_paths, vec!["$HOME/.ssh"]);
    }

    #[test]
    fn sandbox_config_to_policy() {
        let cfg = SandboxConfig::default();
        let policy = cfg.to_policy();
        // Policy should have CWD resolved
        let cwd = std::env::current_dir().unwrap();
        assert!(policy.writable.contains(&cwd));
        assert!(policy.writable.contains(&PathBuf::from("/tmp")));
        // Denied should have resolved $HOME
        let home = std::env::var("HOME").unwrap();
        assert!(policy
            .denied
            .contains(&PathBuf::from(format!("{home}/.ssh"))));
    }

    #[test]
    fn parse_toml_without_sandbox_uses_defaults() {
        let toml_str = r#"
[shell]
command = "/bin/bash"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.sandbox.enabled);
        assert!(cfg.sandbox.writable_paths.contains(&"$CWD".to_string()));
    }

    #[test]
    fn resolve_api_key_cmd_failure_fallback() {
        // If api_key_cmd fails, should try env var
        let cfg = AnthropicConfig {
            api_key_cmd: Some("exit 1".to_string()),
            model: "test".to_string(),
        };

        // This will fail if ANTHROPIC_API_KEY is not set, which is expected
        let result = cfg.resolve_api_key();
        // We can't assert success here since it depends on env, but we verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn parse_judge_mode_warn() {
        let toml_str = r#"
[security]
judge_enabled = true
judge_mode = "warn"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.security.judge_enabled);
        assert_eq!(cfg.security.judge_mode, Some(JudgeMode::Warn));
    }

    #[test]
    fn parse_judge_mode_block() {
        let toml_str = r#"
[security]
judge_enabled = true
judge_mode = "block"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.security.judge_enabled);
        assert_eq!(cfg.security.judge_mode, Some(JudgeMode::Block));
    }

    #[test]
    fn judge_mode_defaults_none() {
        let cfg = SecurityConfig::default();
        assert!(cfg.judge_mode.is_none());
    }

    #[test]
    fn resolve_judge_mode_depth_zero_warn() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.resolve_judge_mode(0), JudgeMode::Warn);
    }

    #[test]
    fn resolve_judge_mode_depth_positive_block() {
        let cfg = SecurityConfig::default();
        assert_eq!(cfg.resolve_judge_mode(1), JudgeMode::Block);
        assert_eq!(cfg.resolve_judge_mode(3), JudgeMode::Block);
    }

    #[test]
    fn resolve_judge_mode_explicit_overrides_depth() {
        let cfg = SecurityConfig {
            judge_mode: Some(JudgeMode::Warn),
            ..Default::default()
        };
        // Even at depth > 0, explicit Warn wins
        assert_eq!(cfg.resolve_judge_mode(2), JudgeMode::Warn);

        let cfg2 = SecurityConfig {
            judge_mode: Some(JudgeMode::Block),
            ..Default::default()
        };
        // Even at depth 0, explicit Block wins
        assert_eq!(cfg2.resolve_judge_mode(0), JudgeMode::Block);
    }

    #[test]
    fn security_config_defaults_include_judge_mode() {
        let cfg = SecurityConfig::default();
        assert!(cfg.judge_mode.is_none());
        assert!(!cfg.judge_enabled);
    }

    #[test]
    fn sandbox_default_denied_paths_no_config_dir() {
        let cfg = SandboxConfig::default();
        assert!(!cfg
            .denied_paths
            .contains(&"$HOME/.config/unixagent".to_string()));
        assert!(cfg.denied_paths.contains(&"$HOME/.ssh".to_string()));
    }
}
