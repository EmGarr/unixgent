use std::io::{self, IsTerminal, Read};

use crossterm::terminal;
use ua_core::batch::run_batch;
use ua_core::config::Config;
use ua_core::depth;
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
    println!();
    println!("Options:");
    println!("  --debug-osc       Print OSC 133 events and state transitions to stderr");
    println!("  --no-integration  Disable shell integration (OSC 133 injection)");
    println!("  --version         Print version");
    println!("  --help            Print this help");
}

fn main() {
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

    let debug_osc = args.iter().any(|a| a == "--debug-osc");
    let no_integration = args.iter().any(|a| a == "--no-integration");

    let config = Config::load_or_default();

    // Detect batch mode: positional arg (non-flag) or piped stdin
    let non_flag_args: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let stdin_is_pipe = !io::stdin().is_terminal();

    let instruction = if let Some(arg) = non_flag_args.first() {
        Some((*arg).clone())
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

    // Batch mode
    if let Some(instruction) = instruction {
        let max_depth = config.security.max_agent_depth;
        let depth = match depth::check_depth(max_depth) {
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

        let code = runtime.block_on(run_batch(&config, &instruction, depth));
        std::process::exit(code);
    }

    // REPL mode
    let mut config = config;
    if no_integration {
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

    let result = run_repl(&config, debug_osc, runtime.handle());

    drop(guard);

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
