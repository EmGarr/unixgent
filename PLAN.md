# UnixAgent — Implementation Plan

**Last updated**: 2026-02-07

This document tracks the implementation status of every phase and sub-task.
It is the single source of truth for "where are we at". See DESIGN.md for
the full technical spec.

**Convention**:
- **DONE** — shipped, tested, merged
- **WIP** — actively in progress
- **TODO** — not started yet
- **BLOCKED** — waiting on something (noted inline)

---

## Phase 1: PTY Wrapper + REPL — DONE

Spawn child shell in PTY, proxy all I/O, inject OSC 133 shell integration,
intercept `#` lines at the prompt.

| Sub-task | Status | Files |
|----------|--------|-------|
| Workspace scaffold | DONE | `Cargo.toml`, crate layout |
| PTY session management | DONE | `ua-core/src/pty.rs` |
| OSC 133 parser + terminal state machine | DONE | `ua-core/src/osc.rs` |
| Shell integration scripts (bash/zsh/fish) | DONE | `ua-core/src/shell_scripts.rs` |
| REPL loop with `#` detection | DONE | `ua-core/src/repl.rs` |
| Config file loading | DONE | `ua-core/src/config.rs` |
| CLI args + main entry point | DONE | `ua-core/src/main.rs` |
| `make check` passes | DONE | |

**Exit criteria met**: Shell commands work normally. `#` instructions are
captured only at the prompt (not in heredocs, REPLs). Prompt detection works
via OSC 133 in bash and zsh.

---

## Phase 2: SSE Backends + Config + Context Management — DONE

Anthropic adapter with SSE streaming. Config for API keys. Context window
management. Plan display. Agentic loop.

| Sub-task | Status | Files |
|----------|--------|-------|
| Add workspace deps (reqwest, bytes, async-stream, ratatui) | DONE | `Cargo.toml`, crate Cargo.tomls |
| Protocol types (context, message) | DONE | `ua-protocol/src/context.rs`, `ua-protocol/src/message.rs` |
| SSE stream parser | DONE | `ua-backend/src/sse.rs` |
| Mock provider + test fixtures | DONE | `ua-backend/src/mock.rs` |
| Anthropic SSE adapter | DONE | `ua-backend/src/anthropic.rs` |
| Config: BackendConfig, AnthropicConfig, ContextConfig | DONE | `ua-core/src/config.rs` |
| Context capture (OutputHistory, ANSI stripping, env filter) | DONE | `ua-core/src/context.rs` |
| PlanDisplay TUI component | DONE | `ua-core/src/display/mod.rs` |
| TestTui harness | DONE | `ua-core/src/display/testing.rs` |
| Tokio runtime in main.rs | DONE | `ua-core/src/main.rs` |
| Wire backend into REPL | DONE | `ua-core/src/repl.rs` |
| **Auto-execute plan commands via PTY** | DONE | `ua-core/src/repl.rs` |
| Silent shell integration injection (single-line eval + clear) | DONE | `ua-core/src/shell_scripts.rs` |
| **Agentic loop (execute → observe → iterate)** | DONE | `ua-core/src/repl.rs`, `ua-backend/src/anthropic.rs` |
| **Tool use API for command delivery** | DONE | `ua-protocol/src/message.rs`, `ua-backend/src/anthropic.rs`, `ua-backend/src/mock.rs`, `ua-core/src/repl.rs` |
| 110 unit tests pass, `make check` clean | DONE | |

**Exit criteria met**:
- `# what's in /tmp` → backend returns plan with `ls /tmp` ✅
- Plan is displayed to the user ✅
- Context stays within token limits across multiple turns ✅
- Commands are auto-executed after displaying the plan ✅
- Agentic loop: commands execute, output fed back, LLM iterates until done ✅
- Commands delivered via tool_use API (no text parsing) ✅

---

## Phase 3: Second Backend + Common Interface — TODO

OpenAI adapter. Extract common backend trait from what the two adapters
actually share. Subprocess adapter for ollama.

| Sub-task | Status | Files |
|----------|--------|-------|
| OpenAI SSE adapter | TODO | `ua-backend/src/openai.rs` |
| Extract Backend trait | TODO | `ua-backend/src/lib.rs` |
| Subprocess adapter (ollama) | TODO | `ua-backend/src/subprocess.rs` |
| Backend selection in config | TODO | |

**Exit criteria**: Can switch between Anthropic, OpenAI, and ollama.
Backend interface is clean and minimal.

---

## Phase 4: Policy Engine + Hooks — TODO

Parse `policy.toml`. Pre-exec hook runner. Command allow/deny.

| Sub-task | Status | Files |
|----------|--------|-------|
| Policy file parsing | TODO | `ua-core/src/policy.rs` |
| Deny pattern matching | TODO | |
| Hook runner (pre/post exec) | TODO | `ua-core/src/hooks.rs` |
| Audit log writer | TODO | `ua-core/src/audit.rs` |

**Exit criteria**: Hook denies `curl` → agent command blocked.
Policy deny patterns work.

---

## Phase 5: Agent Mode TUI + Interactive Approval — TODO

Full agent loop: context → backend → plan stream → agent mode TUI →
interactive approval/steering → execute. Multi-turn conversation.

This is the big one — the TUI overlay described in DESIGN.md §2.4.

| Sub-task | Status | Files |
|----------|--------|-------|
| Agent mode TUI overlay | TODO | `ua-core/src/display/` |
| Plan approval controls ([a] Allow, [s] Step, [d] Deny, [e] Edit) | TODO | |
| Step-through execution with inline output | TODO | |
| Inline steering (`# new instruction` mid-plan) | TODO | |
| Stream output mode (TTY vs NDJSON) | TODO | `ua-core/src/stream.rs` |

**Exit criteria**: Complete interactive session works end-to-end. User can
step through plans, edit commands, steer with inline `#` instructions.

---

## Phase 6: Vision — TODO

Screen capture and accessibility API integration.

| Sub-task | Status | Files |
|----------|--------|-------|
| macOS screencapture | TODO | `ua-core/src/vision.rs` |
| macOS Accessibility API | TODO | |
| Linux X11/Wayland screenshots | TODO | |
| Linux AT-SPI2 | TODO | |
| OCR | TODO | |

**Exit criteria**: `# what's on screen` → agent describes visible windows.

---

## Phase 7: Audio — TODO

Microphone, system audio, speech-to-text.

**Exit criteria**: User speaks instruction → agent transcribes and executes.

---

## Phase 8: UI Interaction — TODO

Input synthesis, window management, clipboard, accessibility-targeted actions.

**Exit criteria**: `# close the Firefox tab playing music` → agent does it.

---

## Phase 9: Agent Spawning + Telemetry + Static Binary — TODO

Child agents, policy inheritance, trace propagation, musl build, packaging.

**Exit criteria**: Parent spawns child over SSH, trace links them. Static binary ships.

---

## Known Issues / Bugs

| Issue | Severity | Notes |
|-------|----------|-------|
| ~~Plan commands not auto-executed~~ | ~~High~~ | FIXED — commands now written to PTY after plan display |
| ~~Shell integration script echoed on startup~~ | ~~Low~~ | FIXED — temp file + source approach; script ends with `clear` to wipe the short source line |
| ~~Double command display on multi-command plans~~ | ~~Medium~~ | FIXED — commands dispatched on 133;A arrived before ZLE/readline init causing canonical echo + ZLE re-echo. Fix: dispatch on 133;B (after prompt rendered, input ready). Zsh uses `zle-line-init` hook, bash embeds 133;B in PS1 |
| ~~No Ctrl+C / interrupt handling~~ | ~~Critical~~ | FIXED — Ctrl+C cancels streaming via oneshot channel, kills execution and clears command queue. State-aware handling per AgentState. See DESIGN.md §19.2.2 |
| ~~No approval gate — auto-executes with zero safety~~ | ~~Critical~~ | FIXED — Commands require explicit user approval (`[y] run [n] skip [q] quit`). Single keystroke in raw mode. See DESIGN.md §19.2.3 |
| ~~`block_on` blocks main event loop during streaming~~ | ~~Critical~~ | FIXED — Replaced with async state machine. Backend streams via spawned tokio task, events flow through mpsc channel. Event loop processes Ctrl+C, PTY output, resize in real time. See DESIGN.md §19.2.1 |
| ~~Command queue ignores exit codes~~ | ~~High~~ | FIXED — CommandQueue tracks last_exit_code from 133;D. Non-zero exit with remaining commands returns QueueEvent::Failed, clearing queue. See DESIGN.md §19.2.4 |
| ~~Conversation history grows unbounded~~ | ~~High~~ | FIXED — max_conversation_turns (default 20) in ContextConfig. Oldest entries evicted after each push. See DESIGN.md §19.2.7 |
| Shell integration sourcing unverified | Medium | No check that `source` succeeded; `clear` hack; temp file leak on panic. See DESIGN.md §19.2.5 |
| ~~`try_wait()` polled on every event~~ | ~~Medium~~ | FIXED — Moved to PtyEof arm only, used for diagnostic logging. See DESIGN.md §19.2.6 |
| ~~Thread handles discarded (fire-and-forget)~~ | ~~Medium~~ | FIXED — PTY reader JoinHandle stored and joined on exit. Stdin thread blocks on read (can't join portably). See DESIGN.md §19.2.8 |
| ~~`looks_like_secret()` insufficient~~ | ~~Medium~~ | FIXED — Expanded with AWS keys (AKIA), JWTs (eyJ), Slack/GitLab/npm tokens, SSH private key content, high-entropy base64 heuristic. See DESIGN.md §19.2.9 |
| ~~SIGWINCH polled every 250ms~~ | ~~Low~~ | FIXED — Replaced with signal_hook SIGWINCH handler. Zero CPU when idle. See DESIGN.md §19.2.10 |
| ~~No integration tests with real shells + OSC 133~~ | ~~Medium~~ | FIXED — 5 integration tests spawning real bash/zsh/fish in PTY, verifying OSC 133 A/B/C/D sequences and exit codes. See DESIGN.md §19.3 |

---

## Architecture Decisions Log

| Decision | Date | Rationale |
|----------|------|-----------|
| ~~Sync/async bridge via `block_on`~~ | ~~2026-02-07~~ | ~~Replaced by async state machine~~ |
| Async state machine with spawned tokio task | 2026-02-08 | Backend streams forwarded through mpsc channel via spawned tokio task. AgentState enum (Idle/Streaming/Approving/Executing) drives the main event loop. Cancellation via oneshot channel. |
| No backend trait yet | 2026-02-07 | Per DESIGN.md §4.5 — extract common interface in Phase 3 after second backend exists |
| ~~Plain text code block parsing~~ | ~~2026-02-08~~ | ~~Replaced by tool_use API~~ |
| Tool use API for commands | 2026-02-08 | Commands delivered via Anthropic tool_use (shell tool) instead of parsing fenced code blocks from text. Truly invisible delimiter — structured API channel, not text. SseProcessor accumulates tool blocks across SSE events. See DESIGN.md §2.10 |
| Extended thinking enabled | 2026-02-08 | Anthropic API with thinking budget (10k tokens), API version 2023-06-01 |
| Dispatch commands on 133;B not 133;A | 2026-02-08 | 133;A fires in precmd before prompt rendering/ZLE init; dispatching there causes double-echo. 133;B fires after prompt is ready (zle-line-init for zsh, PS1 embedded for bash) |
| Agentic loop via inner event loop | 2026-02-08 | After commands execute, output is captured and fed back to LLM. Loop continues until LLM responds without code blocks or max 10 iterations. Inner loop reads from same mpsc channel, keeping event-driven model intact |
| Approximate token counting (chars/4) | 2026-02-07 | Good enough for 200-line terminal history within 200k context window |
| reqwest + rustls-tls | 2026-02-07 | No OpenSSL dependency, integrates with tokio |
| Temp file + source for shell integration | 2026-02-07 | Writing scripts to PTY causes echo; temp file sourced via ` source /path` with `clear` at end |
