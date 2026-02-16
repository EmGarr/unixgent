# UnixAgent Development Guide

## Project Overview

UnixAgent is an AI-powered Unix shell agent. It connects to LLM backends
(Anthropic Claude, etc.) and executes shell commands on the user's behalf
through a PTY-based terminal session.

**Current state:** Phase 2 complete — SSE backends, context management,
plain text command extraction, and OSC 133 sequenced auto-execution.

## PLAN.md — Living Implementation Tracker

**`PLAN.md` is the single source of truth for implementation progress.**
It must be kept up to date at all times.

Rules:
1. **Update PLAN.md** whenever you start, finish, or change a task.
2. **Never remove completed items.** Mark them DONE, don't delete them.
3. **If something looks stale or contradicts the code**, ask the user
   before modifying or removing it. Do not silently "fix" the plan.
4. **Add new issues/bugs** to the "Known Issues" table as you find them.
5. **Add architecture decisions** to the "Architecture Decisions Log"
   when making non-obvious choices.

## Build / Test / Lint

```bash
# Full check (fmt + clippy + test)
make check

# Individual targets
make build          # cargo build --workspace
make test           # cargo test --workspace
make fmt            # cargo fmt --all -- --check
make clippy         # cargo clippy --workspace -- -D warnings
make fmt-fix        # cargo fmt --all (auto-fix)
make clean          # cargo clean

# Docker (Linux testing)
make docker-test    # build + test + clippy inside Debian Bookworm
```

### Running the binary

```bash
export ANTHROPIC_API_KEY=$(security find-generic-password -s "anthropic_api_key" -w)
cargo run -p ua-core
```

## API Key Setup

```bash
# Option 1: env var
export ANTHROPIC_API_KEY="sk-ant-..."

# Option 2: config.toml (recommended)
# In ~/.config/unixagent/config.toml:
# [backend.anthropic]
# api_key_cmd = "security find-generic-password -s anthropic_api_key -w"
```

## Docker Workflow

The `Dockerfile.test` image is based on `rust:1.91-bookworm` and includes
bash, zsh, and fish for shell compatibility testing. Use `make docker-test`
to run the full build + test + clippy suite on Linux.

## Crate Structure

```
crates/
  ua-protocol/   Shared types, message definitions (no internal deps)
  ua-backend/    LLM provider adapters (depends on ua-protocol)
  ua-core/       Binary + agent logic (depends on ua-protocol, ua-backend)
```

Dependency flow:

```
ua-core → ua-backend → ua-protocol
      └───────────────┘
```

### Key modules

```
ua-protocol/src/
  context.rs       ShellContext, TerminalHistory, ConversationMessage, AgentRequest
  message.rs       StreamEvent

ua-backend/src/
  sse.rs           Generic SSE stream parser
  anthropic.rs     Anthropic API client with SSE streaming + extended thinking
  mock.rs          Mock provider for testing (StreamEvent-level)

ua-core/src/
  main.rs          Entry point, CLI args, tokio runtime
  repl.rs          REPL loop, # detection, command extraction, OSC 133 dispatch
  pty.rs           PTY session management
  osc.rs           OSC 133 parser + terminal state machine
  config.rs        Config loading (shell, backend, context, journal)
  context.rs       OutputHistory ring buffer, ANSI stripping, context assembly
  journal.rs       Append-only session journal (JSONL), context builder from journal
  renderer.rs      ReplRenderer<W>: testable REPL display output (Linus forward-flow design)
  display.rs       Response stream accumulator (PlanDisplay)
  process.rs       Process introspection: depth counting, child CWD resolution
  shell_scripts.rs Shell integration scripts (bash/zsh/fish)
```

## SECURITY.md — Security Architecture

**`SECURITY.md` is the security specification for the project.** It defines
the threat model, defense-in-depth layers, OS-level sandbox architecture,
command classification, approval model, and a 45-entry annotated bibliography
of LLM agent security research.

All security-related implementation decisions must reference SECURITY.md.

## Dependency Strategy

Self-contained workspace. No path dependencies on external projects.

Reference (not copy) patterns from `~/Documents/programming/zen-cli/` when needed:
- `llm-client/src/providers/anthropic.rs` — Anthropic SSE event format
- `native-shell/src/shell.rs` — shell session patterns

## Development Workflow

1. **Commit often.** Small, focused commits.
2. **Test always.** Run `make check` before committing.
3. **Update PLAN.md** when starting, finishing, or changing any task.
4. **Update CLAUDE.md** when adding crates, commands, or changing architecture.
5. **No dead code.** If it's unused, delete it.
6. **No over-engineering.** Build what's needed now.
