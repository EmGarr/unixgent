use std::io::{self, IsTerminal, Read};
use std::path::Path;

use crossterm::terminal;
use ua_core::attachment::load_attachment;
use ua_core::batch::run_batch;
use ua_core::config::Config;
use ua_core::process;
use ua_core::repl::run_repl;
use ua_core::shell_scripts::{detect_shell, ShellKind};

struct TerminalGuard {
    was_raw: bool,
}

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        let was_raw = terminal::is_raw_mode_enabled()?;
        if !was_raw {
            terminal::enable_raw_mode()?;
        }
        Ok(Self { was_raw })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.was_raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn print_help() {
    println!("unixagent — AI-powered Unix shell agent");
    println!();
    println!("Usage:");
    println!("  unixagent                   Interactive REPL mode");
    println!("  unixagent \"instruction\"      Batch mode (non-interactive)");
    println!("  echo \"instruction\" | unixagent  Batch mode via stdin pipe");
    println!("  unixagent -p \"prompt\" --attachments img.png  Multimodal batch mode");
    println!("  unixagent --listen                          Record mic → transcribe → execute");
    println!();
    println!("Options:");
    println!("  -p, --prompt <text>          Instruction text for batch mode");
    println!(
        "  --listen                     Record from microphone, transcribe, run as instruction"
    );
    println!();
    println!("REPL voice input:");
    println!("  #v / #voice / #listen        Start voice recording at the prompt");
    println!("  Ctrl+V                       Push-to-talk (empty prompt only)");
    println!("  --attachments <files...>     Image files to attach (png, jpg, gif, webp)");
    println!("  --system-prompt-file <path>   Prepend file contents to system prompt (batch mode)");
    println!("  --debug-osc                  Print OSC 133 events to stderr");
    println!("  --no-integration             Disable shell integration (OSC 133 injection)");
    println!("  --version                    Print version");
    println!("  --help                       Print this help");
    println!();
    println!("Environment:");
    println!(
        "  UNIXAGENT_COMPUTER_USE=macos  Enable computer-use mode (forces judge in Block mode)"
    );
    println!();
    println!("Internal:");
    println!("  --sandbox-exec <cmd> [args...]  Apply sandbox and exec (used by agent)");
}

/// Parsed CLI arguments.
struct CliArgs {
    debug_osc: bool,
    no_integration: bool,
    listen: bool,
    prompt: Option<String>,
    system_prompt_file: Option<String>,
    attachment_paths: Vec<String>,
    positional: Vec<String>,
}

fn parse_args(args: &[String]) -> CliArgs {
    let mut result = CliArgs {
        debug_osc: false,
        no_integration: false,
        listen: false,
        prompt: None,
        system_prompt_file: None,
        attachment_paths: Vec::new(),
        positional: Vec::new(),
    };

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--debug-osc" => result.debug_osc = true,
            "--no-integration" => result.no_integration = true,
            "--listen" => result.listen = true,
            "-p" | "--prompt" => {
                i += 1;
                if i < args.len() {
                    result.prompt = Some(args[i].clone());
                } else {
                    eprintln!("error: {arg} requires a value");
                    std::process::exit(1);
                }
            }
            "--system-prompt-file" => {
                i += 1;
                if i < args.len() {
                    result.system_prompt_file = Some(args[i].clone());
                } else {
                    eprintln!("error: --system-prompt-file requires a path");
                    std::process::exit(1);
                }
            }
            "--attachments" => {
                // Consume all subsequent non-flag args as attachment paths
                i += 1;
                while i < args.len() && !args[i].starts_with('-') {
                    result.attachment_paths.push(args[i].clone());
                    i += 1;
                }
                if result.attachment_paths.is_empty() {
                    eprintln!("error: --attachments requires at least one file path");
                    std::process::exit(1);
                }
                continue; // don't increment i again
            }
            _ if !arg.starts_with('-') => {
                result.positional.push(arg.clone());
            }
            _ => {} // ignore unknown flags (already handled: --help, --version, --sandbox-exec)
        }
        i += 1;
    }

    result
}

fn main() {
    // --sandbox-exec: apply sandbox and exec command. Must happen before
    // anything else — no panic hook, no config, no tokio runtime.
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() >= 3 && args[1] == "--sandbox-exec" {
            ua_sandbox::exec_sandboxed(&args[2..]);
        }
    }

    // Install panic handler that restores terminal
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        default_hook(info);
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("unixagent {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let cli = parse_args(&args);

    let mut config = Config::load_or_default();

    // Detect computer-use mode
    let computer_use = std::env::var("UNIXAGENT_COMPUTER_USE").is_ok();
    if computer_use {
        config.security.judge_enabled = true;
        config.security.judge_mode = Some(ua_core::config::JudgeMode::Block);
    }

    // Read --system-prompt-file contents if provided
    let system_prompt_file: Option<String> = cli.system_prompt_file.as_ref().map(|path| {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("error: --system-prompt-file: {e}");
            std::process::exit(1);
        })
    });

    // Apply sandbox to agent process (children inherit).
    // Must happen AFTER config load (needs to read ~/.config/unixagent/config.toml)
    // and BEFORE any LLM-driven execution.
    let sandbox_active = if config.sandbox.enabled {
        let policy = config.sandbox.to_policy();
        match ua_sandbox::apply(&policy) {
            Ok(()) => {
                eprintln!("[ua:sandbox] active");
                true
            }
            Err(e) => {
                eprintln!("[ua:sandbox] warning: failed to apply: {e}");
                false
            }
        }
    } else {
        false
    };

    // Determine instruction: --listen, -p flag, positional arg, or stdin pipe
    let stdin_is_pipe = !io::stdin().is_terminal();

    if cli.prompt.is_some() && !cli.positional.is_empty() {
        eprintln!("error: cannot use both -p/--prompt and a positional instruction");
        std::process::exit(1);
    }

    if cli.listen && (cli.prompt.is_some() || !cli.positional.is_empty()) {
        eprintln!(
            "error: --listen cannot be combined with -p/--prompt or a positional instruction"
        );
        std::process::exit(1);
    }

    let instruction = if cli.listen {
        match ua_core::audio::listen(&config.audio) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("error: audio input failed: {e}");
                std::process::exit(1);
            }
        }
    } else if let Some(ref p) = cli.prompt {
        Some(p.clone())
    } else if let Some(arg) = cli.positional.first() {
        Some(arg.clone())
    } else if stdin_is_pipe {
        let mut buf = String::new();
        if io::stdin().read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
            Some(buf.trim().to_string())
        } else {
            None
        }
    } else {
        None
    };

    // --attachments requires an instruction (batch mode)
    if !cli.attachment_paths.is_empty() && instruction.is_none() {
        eprintln!("error: --attachments requires an instruction (-p or positional arg)");
        std::process::exit(1);
    }

    // Batch mode
    if let Some(instruction) = instruction {
        // Load attachments
        let attachments: Vec<ua_protocol::Attachment> = cli
            .attachment_paths
            .iter()
            .map(|path| {
                load_attachment(Path::new(path)).unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                })
            })
            .collect();

        let max_depth = config.security.max_agent_depth;
        let depth = match process::check_depth(max_depth) {
            Ok(d) => d,
            Err(d) => {
                eprintln!(
                    "[ua:batch] error: depth limit reached ({d} >= {max_depth}), refusing to start"
                );
                std::process::exit(1);
            }
        };

        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[ua:batch] error: failed to create async runtime: {e}");
                std::process::exit(1);
            }
        };

        let code = runtime.block_on(run_batch(
            &config,
            &instruction,
            depth,
            sandbox_active,
            attachments,
            system_prompt_file.as_deref(),
            computer_use,
        ));
        std::process::exit(code);
    }

    // REPL mode
    if !cli.attachment_paths.is_empty() {
        eprintln!("error: --attachments is only supported in batch mode (provide an instruction)");
        std::process::exit(1);
    }

    if cli.no_integration {
        config.shell.integration = false;
    }

    // Warn if shell integration is not available
    if config.shell.integration {
        let shell_cmd = config.shell_command();
        let kind = detect_shell(&shell_cmd);
        if kind == ShellKind::Unknown {
            eprintln!(
                "warning: unknown shell '{}', shell integration disabled",
                shell_cmd
            );
            eprintln!("hint: use bash, zsh, or fish for full integration");
            config.shell.integration = false;
        }
    }

    // Create tokio runtime for async operations
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create async runtime: {e}");
            std::process::exit(1);
        }
    };

    let guard = match TerminalGuard::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: failed to configure terminal: {e}");
            std::process::exit(1);
        }
    };

    let result = run_repl(&config, cli.debug_osc, runtime.handle(), sandbox_active);

    drop(guard);

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
