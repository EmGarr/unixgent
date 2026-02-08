#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Unknown,
}

pub fn detect_shell(cmd: &str) -> ShellKind {
    // Extract the basename from the command path
    let basename = cmd.rsplit('/').next().unwrap_or(cmd);
    // Strip leading dash (login shell convention)
    let name = basename.strip_prefix('-').unwrap_or(basename);
    match name {
        "bash" => ShellKind::Bash,
        "zsh" => ShellKind::Zsh,
        "fish" => ShellKind::Fish,
        _ => ShellKind::Unknown,
    }
}

/// Returns the integration script for the given shell kind.
///
/// The script is written to a temp file and sourced silently via
/// shell-specific env vars — never typed into the PTY.
pub fn integration_script(kind: ShellKind) -> Option<&'static str> {
    match kind {
        ShellKind::Bash => Some(BASH_INTEGRATION),
        ShellKind::Zsh => Some(ZSH_INTEGRATION),
        ShellKind::Fish => Some(FISH_INTEGRATION),
        ShellKind::Unknown => None,
    }
}

// OSC 133 markers:
//   A = prompt start (fresh prompt ready for input)
//   B = command start (user pressed Enter, about to execute)
//   C = command output start (execution begins)
//   D;N = command finished with exit code N
//
// Sequence: D;$? → A → [user types] → B → C → [output] → D;$? → A → ...

// These scripts are sourced from a temp file, so they can use normal
// multi-line shell syntax — no need for eval or quoting gymnastics.

const BASH_INTEGRATION: &str = r#"
__ua_prompt_command() {
    local exit_code=$?
    printf '\x1b]133;D;%d\x07' "$exit_code"
    printf '\x1b]133;A\x07'
}
[[ "${PROMPT_COMMAND[*]}" =~ __ua_prompt_command ]] || PROMPT_COMMAND=("__ua_prompt_command" "${PROMPT_COMMAND[@]}")

# Append 133;B to PS1 to mark end of prompt (start of user input).
# This fires after PS1 is rendered and readline is ready for input.
case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${PS1}\[\e]133;B\a\]" ;;
esac

__ua_debug_trap() {
    [[ "$BASH_COMMAND" != "__ua_prompt_command" ]] && printf '\x1b]133;C\x07'
}
trap '__ua_debug_trap' DEBUG
clear
"#;

const ZSH_INTEGRATION: &str = r#"
__ua_precmd() {
    local exit_code=$?
    printf '\x1b]133;D;%d\x07' "$exit_code"
    printf '\x1b]133;A\x07'
}
__ua_preexec() {
    printf '\x1b]133;C\x07'
}
# Emit 133;B after prompt is rendered and ZLE is initialized.
# This ensures injected commands arrive when the terminal is in raw mode,
# preventing double-echo from canonical mode.
__ua_zle_line_init() {
    printf '\x1b]133;B\x07'
}
zle -N zle-line-init __ua_zle_line_init
(( ${precmd_functions[(Ie)__ua_precmd]} )) || precmd_functions=(__ua_precmd $precmd_functions)
(( ${preexec_functions[(Ie)__ua_preexec]} )) || preexec_functions=(__ua_preexec $preexec_functions)
clear
"#;

const FISH_INTEGRATION: &str = r#"
function __ua_fish_prompt --on-event fish_prompt
    set -l exit_code $status
    printf \x1b]133\;D\;%d\x07 $exit_code
    printf \x1b]133\;A\x07
end
function __ua_fish_preexec --on-event fish_preexec
    printf \x1b]133\;B\x07
    printf \x1b]133\;C\x07
end
clear
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_shells() {
        assert_eq!(detect_shell("/bin/bash"), ShellKind::Bash);
        assert_eq!(detect_shell("/usr/bin/zsh"), ShellKind::Zsh);
        assert_eq!(detect_shell("/usr/local/bin/fish"), ShellKind::Fish);
        assert_eq!(detect_shell("/bin/sh"), ShellKind::Unknown);
        assert_eq!(detect_shell("bash"), ShellKind::Bash);
        assert_eq!(detect_shell("-zsh"), ShellKind::Zsh);
    }

    #[test]
    fn integration_scripts_exist() {
        assert!(integration_script(ShellKind::Bash).is_some());
        assert!(integration_script(ShellKind::Zsh).is_some());
        assert!(integration_script(ShellKind::Fish).is_some());
        assert!(integration_script(ShellKind::Unknown).is_none());
    }

    #[test]
    fn scripts_contain_osc_markers() {
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let script = integration_script(kind).unwrap();
            // Fish uses escaped semicolons (133\;A), others use 133;A
            let has_marker =
                |m: &str| script.contains(m) || script.contains(&m.replace(';', "\\;"));
            assert!(has_marker("133;A"), "{kind:?} missing 133;A");
            assert!(has_marker("133;B"), "{kind:?} missing 133;B");
            assert!(has_marker("133;D"), "{kind:?} missing 133;D");
            assert!(has_marker("133;C"), "{kind:?} missing 133;C");
        }
    }

    #[test]
    fn scripts_end_with_clear() {
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish] {
            let script = integration_script(kind).unwrap();
            assert!(
                script.trim().ends_with("clear"),
                "{kind:?} script should end with 'clear'"
            );
        }
    }
}
