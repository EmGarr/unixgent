# UnixAgent

**The shell is the agent. The agent is the shell.**

Unix, Linux, macOS — these are not just terminals. They are operating systems with displays, windows, accessibility APIs, and user sessions. An agent that only lives in a terminal is blind. A shell that can see what the user sees — that is the real interface.

## The Idea

```sh
$ ls -la                            # shell command — runs as-is
# find all Python files over 1MB    # agent instruction — AI generates and runs commands
# what's in this screenshot         # agent sees what the user sees
```

The `#` comment character becomes the agent instruction prefix. Everything else passes through to your existing shell (bash, zsh, fish). The agent wraps any shell — it does not replace it.

This works because the agent knows when the shell is at a prompt — via OSC 133 shell integration, the same protocol iTerm2 and VSCode use. At the prompt, `#` means "talk to the agent." Inside a running command (heredoc, Python REPL, vim), `#` passes through normally.

But the agent is not trapped in the terminal. It can:

- **See the screen** — via OS accessibility APIs, screenshots, window state
- **Hear audio** — via microphone capture, system audio, speech-to-text
- **Control the UI** — via input synthesis, window management, accessibility actions
- **See the terminal** — via PTY capture of recent output
- **See the filesystem** — it's a shell, it has full access
- **See the network** — same user permissions, same machine

The shell is the perfect agent interface because it is already the universal control plane of every Unix system. Every server, every laptop, every container has a shell. Now every shell has an agent.

## How It Works

1. `unixagent` spawns your shell (`$SHELL`) inside a PTY
2. Shell integration (OSC 133) injected for reliable prompt detection
3. You type commands — they pass through to your shell normally
4. You type `# instruction` at the prompt — the agent intercepts it
5. The terminal enters **agent mode** — an interactive TUI overlay
6. The agent streams its plan in real-time — you watch it form
7. You interact: approve all, step through, edit steps, add context
8. Commands execute, output displayed inline
9. Between steps, you can steer: `# actually, only delete files older than 7 days`
10. Plan completes — agent mode ends, shell prompt returns

## Agent Mode

When you type a `#` instruction, the terminal enters agent mode. This is an interactive TUI — not just a yes/no gate. You participate in the execution:

```
# clean up /tmp

Agent: Plan (3 steps):
  1. du -sh /tmp/*                      [show sizes]
  2. rm -rf /tmp/old_builds /tmp/cache  [free 800MB]
  3. df -h /tmp                         [verify]

[a] Allow all  [s] Step through  [d] Deny  [e] Edit plan

> s  (step through)

Step 1: du -sh /tmp/*
[a] Allow  [d] Deny  [e] Edit

> a

240M  /tmp/old_builds
560M  /tmp/cache
4.0K  /tmp/nginx-cache

Step 2: rm -rf /tmp/old_builds /tmp/cache
[a] Allow  [d] Deny  [e] Edit  [c] Add context

> # keep the nginx cache, only delete app caches

Agent: Revised step 2: rm -rf /tmp/old_builds /tmp/cache/app-*
[a] Allow  [d] Deny  [e] Edit

> a
```

You can steer the agent mid-plan with `#` instructions, edit individual commands, or abort at any point. Ctrl+C exits agent mode and returns to your shell.

## Architecture

```
+---------------------------------------------+
|              User Session                    |
|  (terminal, desktop, remote, VM, container)  |
+----------------------+-----------------------+
                       |
+----------------------v-----------------------+
|             unixagent                        |
|                                              |
|  +---------+  +----------+  +------------+   |
|  |  REPL   |  |  Vision  |  |  Backend   |   |
|  | # detect|  |  Screen  |  | Claude SSE |   |
|  | pty proxy  |  A11y API|  | OpenAI SSE |   |
|  +----+----+  +----+-----+  +-----+------+   |
|       |            |              |           |
|       +------------+------+-------+           |
|                           |                   |
|                   +-------v-------+           |
|                   |  Agent Core   |           |
|                   | context+plan  |           |
|                   | confirm+exec  |           |
|                   +-------+-------+           |
|                           |                   |
|                   +-------v-------+           |
|                   | Policy+Hooks  |           |
|                   |  audit trail  |           |
|                   +---------------+           |
+----------------------+------------------------+
                       |
            +----------v----------+
            |   Child Shell       |
            |  (bash/zsh/fish)    |
            |   via PTY           |
            +---------------------+
```

### The stream is the interface

The agent produces a stream of events:

- **Terminal** -> colored text + agent mode TUI
- **Pipe** -> structured NDJSON, consumable by any program
- **Remote** -> SSH carries the stream, same as any shell session

```sh
# Terminal use (human)
$ unixagent
# explain this error

# Programmatic use (script)
$ echo '# explain this error' | unixagent --json

# Remote use
$ ssh server unixagent
# check disk usage
```

## Project Structure

```
unixagent/
├── Cargo.toml
├── crates/
│   ├── ua-core/src/
│   │   ├── main.rs          # entry point
│   │   ├── repl.rs          # input loop: # detection + passthrough
│   │   ├── pty.rs           # spawn child shell, proxy I/O
│   │   ├── agent.rs         # context -> backend -> plan -> agent mode -> execute
│   │   ├── context.rs       # capture terminal output, cwd, env
│   │   ├── vision.rs        # screen capture, accessibility APIs
│   │   ├── audio.rs         # microphone, system audio, STT
│   │   ├── ui.rs            # input synthesis, window management
│   │   ├── display.rs       # agent mode TUI, plan display, approval flow
│   │   ├── stream.rs        # output stream: TTY (text) or pipe (NDJSON)
│   │   ├── config.rs        # ~/.config/unixagent/config.toml
│   │   ├── policy.rs        # policy engine
│   │   ├── hooks.rs         # pre/post execution hooks
│   │   └── audit.rs         # audit log
│   │
│   ├── ua-backend/src/
│   │   ├── lib.rs           # backend interface
│   │   ├── anthropic.rs     # Claude API + SSE
│   │   ├── openai.rs        # OpenAI API + SSE
│   │   ├── sse.rs           # shared SSE stream parser
│   │   └── subprocess.rs    # generic CLI adapter (ollama, etc.)
│   │
│   └── ua-protocol/src/
│       ├── lib.rs
│       ├── message.rs       # request, plan, event types
│       └── context.rs       # shell + vision context serialization
│
└── tests/
    └── integration/
```

## Configuration

`~/.config/unixagent/config.toml`:
```toml
[shell]
command = "/bin/bash"
confirm_mode = "plan"       # auto | each | plan
integration = true          # OSC 133 prompt markers (required for # prefix)

[backend]
default = "anthropic"

[backend.anthropic]
api_key_cmd = "pass show anthropic/api-key"
model = "claude-sonnet-4-20250514"

[backend.openai]
api_key_cmd = "pass show openai/api-key"
model = "gpt-4o"

[vision]
enabled = true

[audio]
enabled = false
stt_backend = "whisper-local"
```

## Implementation Phases

1. **PTY Wrapper + REPL** — Spawn child shell, proxy I/O, OSC 133 shell integration, intercept `#` lines at the prompt.
2. **First Backend + Context** — Anthropic adapter with SSE streaming. Context window management.
3. **Second Backend + Interface** — OpenAI adapter. Extract common interface from two implementations.
4. **Policy + Hooks** — Policy engine, pre-exec hooks, command allow/deny.
5. **Agent Mode TUI** — Full interactive flow: plan streaming, step-through, inline steering, edit.
6. **Vision** — Screen capture, accessibility APIs, OCR.
7. **Audio** — Microphone, system audio, speech-to-text.
8. **UI Interaction** — Input synthesis, window management, clipboard.
9. **Spawning + Telemetry + Distribution** — Child agents, tracing, static binary, packaging.

## Security

- Agent-generated commands are **untrusted** — the approval prompt is the security boundary
- Policy engine enforces rules regardless of what the AI says
- Pre-exec hooks provide programmable gates
- API keys via `api_key_cmd` (keychain/pass, never stored in plaintext)
- Vision/audio require explicit opt-in
- Agent has the same permissions as the user — no escalation
- Audit log: `~/.local/share/unixagent/audit.jsonl`

## Building

```sh
cargo build --release
# Static binary (Linux):
# cargo build --release --target x86_64-unknown-linux-musl
```

## Usage

```sh
# Interactive — wraps your shell
./unixagent

# Run single instruction
echo '# list running services' | ./unixagent

# Remote
ssh server ./unixagent

# JSON output for scripts/UIs
echo '# summarize this project' | ./unixagent --json
```

## License

MIT
