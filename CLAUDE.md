# UnixAgent Development Guide

## Project Overview

UnixAgent is an AI-powered Unix shell agent. It connects to LLM backends
(Anthropic Claude, etc.) and executes shell commands on the user's behalf
through a PTY-based terminal session.

**Current state:** Phase 1 — workspace scaffold complete, no functional code yet.

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
cargo run -p ua-core
```

## API Key Setup

Not needed yet. When Phase 2 begins:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
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

## Dependency Strategy

Self-contained workspace. No path dependencies on external projects.

Reference (not copy) patterns from `~/Documents/programming/zen-cli/` when needed:
- `llm-client/src/providers/anthropic.rs` — Anthropic SSE event format
- `llm-client/src/types.rs` — streaming type design
- `zen-cli/src/tui/` — ratatui TUI patterns, TestTui harness
- `native-shell/src/shell.rs` — shell session patterns

## Development Workflow

1. **Commit often.** Small, focused commits.
2. **Test always.** Run `make check` before committing.
3. **Update CLAUDE.md** when adding crates, commands, or changing architecture.
4. **No dead code.** If it's unused, delete it.
5. **No over-engineering.** Build what's needed now.

## Current Phase

**Phase 1: PTY Wrapper + REPL** — not started.

Next steps:
- Implement PTY session management in ua-core
- Build a basic REPL loop
- Add integration tests for shell interaction
