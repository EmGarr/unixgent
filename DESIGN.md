# UnixAgent — Technical Design Document

**Status**: Design Phase — Specification Draft
**Date**: February 2026

---

## 0. Premise

The computer already has an agent interface. It's the shell. The shell can do anything the user can do — manage files, control processes, configure the system, talk to the network. But it's blind and deaf. It can't see the screen, hear audio, or interact with the GUI.

We fix that. The agent IS the shell, extended with the full sensory and motor capabilities of the operating system.

One binary. Any shell. Any backend. Any OS. Every action audited. Every action reversible. Every action traced.

---

## 1. Core Design Principle

**The agent is not a new shell. It wraps any existing shell.**

`unixagent` is a PTY proxy — like `tmux`, `script`, or `screen`. It spawns the user's `$SHELL` inside a pseudo-terminal, proxies all I/O, and intercepts agent instructions. Everything else passes through unchanged. All shell features (tab completion, history, job control, prompts, readline) work for free because the real shell is running underneath.

---

## 2. Interaction Protocol: OSC 133

OSC 133 shell integration is the foundation of how the user and agent interact. It's not just prompt detection — it defines the terminal's lifecycle states that the agent hooks into.

### 2.1 OSC 133 Markers

The shell emits invisible escape sequences that mark prompt boundaries:

```
ESC ] 133 ; A ST    — prompt started (shell is waiting for input)
ESC ] 133 ; B ST    — prompt ended (user pressed Enter, command follows)
ESC ] 133 ; C ST    — command started executing
ESC ] 133 ; D ; N ST — command finished (exit code N)
```

On startup, the agent injects a minimal integration script into the child shell:
- **bash**: `PROMPT_COMMAND` hook
- **zsh**: `precmd` / `preexec` hooks
- **fish**: `fish_prompt` / `fish_preexec` events

This is the same mechanism iTerm2 and VSCode terminal integration use. It's opt-out (`shell.integration = false`), not opt-in. The reliability difference between this and heuristic prompt detection is too large to make it optional by default.

### 2.2 Terminal States

OSC 133 gives the agent a state machine for the terminal:

```
+---[133;A]---+     +---[133;B]---+     +---[133;C]---+     +---[133;D]---+
|   PROMPT    | --> |   INPUT     | --> |  EXECUTING  | --> |   IDLE      |
|  (waiting)  |     | (user types)|     | (cmd runs)  |     | (finished)  |
+-------------+     +-------------+     +-------------+     +------+------+
      ^                                                            |
      +------------------------------------------------------------+
```

The agent only intercepts input during **PROMPT** state. During EXECUTING, all I/O passes through transparently. This is how `#` lines inside heredocs, Python REPLs, and vim never get intercepted — the shell is in EXECUTING state, not PROMPT state.

### 2.3 Instruction Prefix: `#`

With reliable prompt detection via OSC 133, single `#` works safely as the instruction prefix.

```
$ ls -la                              # normal shell command (PROMPT state → shell)
# find all Python files over 1MB      # agent instruction (PROMPT state → agent)
# what's on screen right now          # agent uses vision
```

The `#` character is the universal comment prefix in shell. Inside unixagent, it becomes the instruction prefix. This overload is safe because:
- The agent only intercepts `#` lines during PROMPT state (OSC 133;A received, waiting for input).
- During command execution (heredocs, REPLs, editors), `#` passes through — the shell is in EXECUTING state.
- In the rare case of heuristic fallback (no shell integration), `##` (double hash) is required instead. The agent detects which mode it's in and tells the user at startup.

### 2.4 Agent Mode: The Interactive Flow

When the user types a `#` instruction at the prompt, the terminal transitions into **agent mode**. This is a TUI overlay — the agent takes over the terminal and the user interacts with the agent, not the shell. The child shell is paused (waiting at the PTY).

```
PROMPT state
  |
  user types: # find large files and clean up
  |
  v
AGENT MODE (TUI overlay)
  |
  +-- agent streams plan to terminal (user watches it form)
  |     user can: Ctrl+C to cancel while streaming
  |
  +-- plan displayed with interactive controls
  |     [a] Allow all   [s] Step through   [d] Deny   [e] Edit
  |     [n] Add note    [Enter] on a step to edit it
  |
  +-- user picks [s] Step through
  |     step 1: du -sh /tmp/*
  |     [a] Allow  [d] Deny  [e] Edit  [c] Add context  [q] Quit plan
  |
  +-- step executes, output displayed inline
  |     user can: Ctrl+C to stop command (SIGINT)
  |     user can: type additional context while viewing output
  |
  +-- between steps: agent pauses
  |     user can: approve next step, skip it, edit it, inject new instruction
  |     "# actually, only delete files older than 7 days"
  |     → agent revises remaining steps with new context
  |
  +-- plan complete (or user quits)
  |
  v
PROMPT state (shell prompt reappears, OSC 133;A)
```

The key difference from a simple yes/no approval: **the user participates in execution**. They can steer the agent mid-plan, inject context between steps, edit commands before they run, and abort at any point. The agent flow is a conversation, not a gate.

### 2.5 Inline Steering

During agent mode, the user can type `# instruction` to steer the agent mid-flow. This injects new context or replaces the remaining plan:

```
Agent: Step 2 of 4: rm -rf /tmp/cache/*
       This will free ~800MB.
[a] Allow  [d] Deny  [e] Edit  [c] Add context

User types: # wait, keep the nginx cache, only delete app caches

Agent: Revised plan:
       Step 2: rm -rf /tmp/cache/app-*
       Step 3: rm -rf /tmp/cache/build-*
       (removed: rm /tmp/cache/nginx-* — kept per your instruction)
```

The agent sends the user's steering input + the current execution state as new context to the backend, which revises the remaining steps.

### 2.6 Multi-line Instructions

If a `#` line is followed immediately by another `#` line (no prompt in between), they're concatenated into a single instruction:

```
# deploy the app to staging,
# run the test suite,
# and rollback if any test fails
```

A blank line or a non-`#` line terminates the instruction.

### 2.7 Escape Mechanism

`\#` at the start of a line passes through as a literal `#` comment to the shell. In fallback mode (no shell integration), `\##` escapes the double-hash prefix.

### 2.8 Fallback: No Shell Integration

If the child shell doesn't support OSC 133 injection (unusual shell, user disabled it):

- **Instruction prefix**: requires `##` (double hash) instead of single `#`. This is unambiguous without prompt detection.
- **Prompt detection**: heuristic — regex matching common `PS1` patterns + idle detection (no output for N ms).
- **Agent mode**: still works, but the agent cannot reliably detect when a command finishes. It falls back to watching for the prompt pattern to reappear.
- **The agent warns at startup**: "Shell integration unavailable. Using ## prefix. Some features may be less reliable."

### 2.9 Config

```toml
[shell]
integration = true    # inject OSC 133 prompt markers (default: true)
```

### 2.10 Command Protocol: LLM-to-PTY

How the LLM's intent to execute a command reaches the PTY. This is the
critical boundary between "text for the user" and "command for the shell."

#### 2.10.1 Current: API-Level Tool Use

The LLM backend's structured output protocol (Anthropic tool_use, OpenAI
function_calling) provides the delimiter. No text parsing required.

The agent defines a single tool:

```json
{
  "name": "shell",
  "description": "Execute a shell command. The command is sent to the user's shell via PTY.",
  "input_schema": {
    "type": "object",
    "properties": {
      "command": { "type": "string", "description": "Shell command to execute" }
    },
    "required": ["command"]
  }
}
```

The LLM response contains two types of content blocks:
- **Text blocks** → display to user (reasoning, explanations)
- **Tool use blocks** → extract `.input.command`, route to PTY

```
LLM response stream:
  [text]     "I'll check the project structure."
  [tool_use] { "name": "shell", "command": "cat Cargo.toml && head -30 PLAN.md" }
  [text]     "Let me look at the config next."
  [tool_use] { "name": "shell", "command": "cat ~/.config/unixagent/config.toml" }
```

What the user sees on stderr: the text blocks, streamed naturally. Commands
are invisible in the text — they arrive through the API's structured channel
and go directly to the PTY. After each tool_use, the agent captures output
and sends it back as a `tool_result` for the next turn.

This is the "translate to native" approach: the API's tool call boundary
IS the ECMA-48 APC boundary, conceptually. We just map one protocol to
the other.

#### 2.10.2 Future: Native APC Tokens (Custom Tokenizer)

With control over the model's tokenizer, the delimiter moves from the API
protocol layer into the token stream itself, using ECMA-48 APC (Application
Program Command) — the control sequence literally designed for this purpose.

**Two special tokens added to the vocabulary:**

| Token | Bytes | ECMA-48 |
|-------|-------|---------|
| `<cmd>` | `ESC _` (0x1B 0x5F) | APC — Application Program Command |
| `</cmd>` | `ESC \` (0x1B 0x5C) | ST — String Terminator |

**Model output becomes a single stream:**

```
I'll check the project structure.
\x1b_cat Cargo.toml && head -30 PLAN.md\x1b\
Let me look at the config next.
\x1b_cat ~/.config/unixagent/config.toml\x1b\
```

**Terminal behavior:** APC sequences are invisible per ECMA-48. Every
terminal emulator swallows them. The user sees:

```
I'll check the project structure.
Let me look at the config next.
```

**Architecture collapses to almost nothing:**
- Stream LLM output directly to stdout (it IS the terminal output)
- Parse for APC boundaries in the byte stream
- On APC open: buffer command bytes
- On ST: write buffered command to PTY, wait for OSC 133 AllDone
- Resume streaming

No `extract_commands()`. No separate stderr display path. No tool_use
JSON parsing. The LLM's output stream and the terminal are unified.

**Requirements:**
- Open-weight model (Llama, Mistral, Qwen, etc.)
- Custom tokenizer with APC/ST special tokens
- Fine-tuning on conversations that use `<cmd>`/`</cmd>` boundaries
- Inference via vLLM, llama.cpp, or custom serving stack

**Why APC specifically:**
- ECMA-48 §8.3.2: "used as the opening delimiter of a control string
  for application program use" — this is its literal purpose
- Terminals ignore it (invisible by spec, not by hack)
- No collision with existing OSC 133 sequences (different namespace)
- ST (String Terminator) is shared with OSC, so the parser already
  handles `ESC \` termination

---

## 3. Capabilities

### 3.1 Shell (Native Space)

The shell is where the agent is most natural.

| Capability | Description |
|-----------|-------------|
| Execute commands | Run any shell command the user could type |
| Pipe and compose | Chain commands, redirect I/O, use process substitution |
| Environment | Read/set environment variables, change directory |
| Job control | Background processes, signals, suspend/resume |
| Scripting | Generate and execute multi-line scripts |
| Package management | Install, update, remove software |
| Service management | Start, stop, configure system services |
| Container/VM | Manage Docker, Podman, VMs |

### 3.2 Vision (Screen)

The agent can perceive the visual state of the user's session.

| Capability | Description |
|-----------|-------------|
| Screenshot | Capture full screen, specific window, or region |
| Window enumeration | List open windows, their titles, positions, sizes |
| UI element tree | Read accessibility tree (AT-SPI on Linux, Accessibility API on macOS) |
| OCR | Extract text from screen regions where accessibility APIs fail |
| Screen change detection | Detect when screen content changes (for reactive behavior) |

**Platform specifics:**

- **macOS**: `screencapture` CLI, `CGWindowListCreateImage`, Accessibility API (`AXUIElement`), Vision framework for OCR.
- **Linux/X11**: `xdotool`, `xdg-screenshot`, `xwininfo`, AT-SPI2 D-Bus interface, Tesseract for OCR.
- **Linux/Wayland**: `wlr-screencopy` protocol, `wl-copy`/`wl-paste`, portal-based screenshot (`xdg-desktop-portal`), AT-SPI2. Wayland is more restricted by design — each compositor exposes different capabilities. The agent must gracefully degrade.

### 3.3 Audio (Hearing)

| Capability | Description |
|-----------|-------------|
| Microphone | Listen to ambient audio / voice commands |
| System audio | Capture audio output from applications |
| Application audio | Capture audio from a specific application |
| Speech-to-text | Transcribe audio to text for processing |
| Audio events | Detect specific sounds (notifications, alarms, errors) |

**Platform specifics:**

- **macOS**: Core Audio / AVFoundation for capture. No built-in system audio loopback — requires a virtual audio device (BlackHole, Soundflower) or Screen Capture Kit (macOS 13+).
- **Linux**: PulseAudio `parecord` / `parec` for mic and monitor sources. PipeWire for application-level routing. ALSA as fallback.
- **Speech-to-text**: Whisper (local via whisper.cpp) or cloud API. The agent should prefer local STT for privacy.

### 3.4 UI Interaction (Motor)

| Capability | Description |
|-----------|-------------|
| Click | Click at coordinates or on accessibility elements |
| Type | Keyboard input to any focused application |
| Scroll | Scroll in any window |
| Drag and drop | Move elements between applications |
| Window management | Move, resize, minimize, maximize, close windows |
| Menu navigation | Open menus, select items |
| File dialogs | Interact with open/save dialogs |
| Clipboard | Read from and write to system clipboard |
| Notifications | Send system notifications to the user |

**Platform specifics:**

- **macOS**: CGEvent for input synthesis, Accessibility API for element-targeted actions, `osascript`/JXA for window management, `pbcopy`/`pbpaste` for clipboard.
- **Linux/X11**: `xdotool` for input synthesis and window management, `xclip`/`xsel` for clipboard, `xdg-open` for notifications via `notify-send`.
- **Linux/Wayland**: `wtype` / `ydotool` for input synthesis (requires elevated access or portal), `wl-copy`/`wl-paste` for clipboard. Wayland's security model deliberately restricts input injection — the agent must use portals or compositor-specific protocols.

### 3.5 Network

| Capability | Description |
|-----------|-------------|
| HTTP/HTTPS | Make web requests (curl, reqwest) |
| WebSocket | Persistent connections |
| DNS | Resolve and query |
| SSH | Connect to remote machines |
| File transfer | Upload, download |

### 3.6 Filesystem

| Capability | Description |
|-----------|-------------|
| Read / write / delete | Standard file operations |
| Search | Find files by name, content, metadata |
| Watch | Monitor files/directories for changes (inotify, FSEvents, kqueue) |
| Metadata | Permissions, timestamps, extended attributes |

---

## 4. Architecture

### 4.1 Process Model

```
User Session (terminal, desktop, remote, VM, container)
         |
    unixagent (PID N)
    +---------+  +----------+  +------------+
    |  REPL   |  |  Vision  |  |  Backend   |
    | # detect|  |  Screen  |  | Claude SSE |
    | pty proxy  |  A11y API|  | OpenAI SSE |
    +----+----+  +----+-----+  +-----+------+
         |            |              |
         +------------+------+-------+
                             |
                     +-------v-------+
                     |  Agent Core   |
                     | context+plan  |
                     | confirm+exec  |
                     +-------+-------+
                             |
                     +-------v-------+
                     | Policy+Sandbox|
                     |  hooks+audit  |
                     +-------+-------+
                             |
                    Child Shell (bash/zsh/fish)
                         via PTY
```

The agent is a single process. It holds one end of a PTY pair; the user's shell holds the other. The agent sits between the user's terminal emulator and the child shell, reading all I/O.

### 4.2 Stream-as-Interface

The agent's output is a stream of events. The format adapts to context:

- **Terminal (TTY detected)**: Colored text, inline in the shell session.
- **Pipe (non-TTY)**: Structured NDJSON, consumable by any program.
- **Remote (SSH)**: Same stream, carried over the SSH channel.

No separate API server. The shell's `stdin`/`stdout` IS the API. A GUI application spawns `unixagent` as a child process, writes `#` instructions to stdin, reads events from stdout.

```sh
# Human use
$ unixagent
# what's using disk space

# Programmatic use
$ echo '# what processes are running' | unixagent --json
{"type":"plan","steps":[...]}
{"type":"output","text":"..."}

# Remote use
$ ssh server unixagent
```

### 4.3 Data Flow for an Instruction

```
1. Shell emits OSC 133;A (prompt ready)
2. User types: # find large files in /tmp
3. REPL detects the # prefix in PROMPT state, strips it
4. Terminal enters AGENT MODE (TUI overlay)
5. Context module captures:
   - cwd, env, $SHELL, platform
   - Last N lines of terminal output
   - (Optional) screenshot, accessibility tree
6. Agent core builds FullContext + instruction
7. Policy engine checks: is this instruction allowed?
8. Backend receives context + instruction via SSE
9. Backend streams response — plan renders in real-time
10. User interacts with plan:
    - Allow all / Step through / Deny / Edit
    - Inject context: "# also check /var/tmp"
    - Edit individual steps before execution
11. For each approved step:
    a. Hooks run (pre-exec)
    b. Command injected into child shell PTY
    c. Output captured and displayed inline in agent mode
    d. User can steer between steps: approve, skip, edit, add context
    e. Hooks run (post-exec)
    f. Audit log entry written
12. Plan complete (or user quits) → agent mode ends
13. Shell prompt reappears (OSC 133;A)
```

### 4.4 Crate Structure

```
unixagent/
├── Cargo.toml                    # workspace root
├── crates/
│   ├── ua-core/src/
│   │   ├── main.rs               # entry point, CLI args, startup
│   │   ├── repl.rs               # input loop: # detection + passthrough
│   │   ├── pty.rs                # spawn child shell in PTY, proxy I/O
│   │   ├── agent.rs              # context → backend → plan → confirm → execute
│   │   ├── context.rs            # capture terminal output, cwd, env
│   │   ├── vision.rs             # screen capture, accessibility APIs
│   │   ├── audio.rs              # microphone, system audio, STT
│   │   ├── ui.rs                 # input synthesis, window management
│   │   ├── display.rs            # plan display, confirmation UI
│   │   ├── stream.rs             # output: TTY (colored text) or pipe (NDJSON)
│   │   ├── config.rs             # parse ~/.config/unixagent/config.toml
│   │   ├── policy.rs             # parse policy.toml, enforce rules
│   │   ├── hooks.rs              # pre/post execution hook runner
│   │   └── audit.rs              # audit log writer
│   │
│   ├── ua-backend/src/
│   │   ├── lib.rs                # Backend interface (designed after implementing two backends)
│   │   ├── anthropic.rs          # Claude API + SSE streaming
│   │   ├── openai.rs             # OpenAI API + SSE streaming
│   │   ├── sse.rs                # shared SSE stream parser
│   │   └── subprocess.rs         # generic CLI adapter (ollama, etc.)
│   │
│   └── ua-protocol/src/
│       ├── lib.rs                # re-exports
│       ├── message.rs            # AgentRequest, Plan, PlanStep, AgentEvent, StreamChunk
│       └── context.rs            # ShellContext, FullContext, ConversationMessage
│
└── tests/
    └── integration/
```

**Language**: Rust. Single static binary. No runtime dependencies.

### 4.5 Backend Interface

The backend interface will be designed bottom-up: implement the Anthropic adapter first, then the OpenAI adapter, then extract the common interface from what actually overlaps. Designing it top-down before implementation guarantees the wrong abstraction.

What we know the interface must handle (but not yet how):
- Streaming responses (SSE)
- Context + instruction in, plan out
- Multi-turn conversation state
- Token counting for context window management
- Retries and rate limiting
- Model-specific prompt format differences

The interface will emerge from the code, not precede it.

---

## 5. Approval Model

Every agent action goes through one of two gates: **human approval** or **policy authorization**. There is no third option.

### 5.1 Human Approval (Interactive Mode)

The agent proposes an action, the human approves or denies.

```
Agent: I'd like to run: rm -rf /tmp/old_builds
       This will delete 3 directories (240MB).

[Allow] [Allow for session] [Deny] [Edit]
```

**Approval granularity:**

| Level | Description |
|-------|-------------|
| Per-action | Each command/click/interaction requires explicit approval |
| Per-plan | Agent presents a multi-step plan, user approves the whole plan |
| Per-session | User grants a capability for the session ("you can read files in this dir") |
| Per-category | User grants a class of actions ("shell freely, ask before UI") |

### 5.2 Policy Authorization (Autonomous Mode)

A policy file pre-authorizes specific capabilities within defined boundaries.

`~/.config/unixagent/policy.toml`:

```toml
[approval]
default = "ask"    # "ask" | "allow" | "deny"

[approval.shell]
mode = "allow"
deny_patterns = ["rm -rf /", "sudo *", "chmod 777 *"]

[approval.filesystem]
mode = "allow"
read = ["$CWD/**", "$HOME/Documents/**"]
write = ["$CWD/**"]
deny = ["$HOME/.ssh/**", "$HOME/.gnupg/**"]

[approval.vision]
screenshot = "allow"
ui_tree = "allow"

[approval.audio]
microphone = "ask"
system_audio = "allow"
speech_to_text = "allow"

[approval.ui]
mode = "ask"
allow_apps = ["Terminal", "Firefox"]

[approval.network]
mode = "deny"
allow_hosts = ["api.github.com", "*.internal.corp"]
```

The policy file is **protected** — the agent cannot modify it. The agent's filesystem deny list always includes its own config and policy paths.

### 5.3 Hooks (Programmable Gates)

User-defined scripts that intercept agent actions before execution. The hook receives the proposed action as JSON on stdin and returns allow/deny/modify.

```sh
#!/bin/bash
# ~/.config/unixagent/hooks/pre-exec.sh
ACTION=$(cat)
CMD=$(echo "$ACTION" | jq -r '.command')

if echo "$CMD" | grep -q "production"; then
    echo '{"decision": "deny", "reason": "Production commands require manual execution"}'
    exit 0
fi

echo '{"decision": "allow"}'
```

Hooks run for ALL action types — shell, UI, audio, filesystem, network. Same interface.

### 5.4 Decision Cascade

For any action, the agent evaluates in order:

1. **Policy deny list** -> if matched, deny immediately (no hook, no prompt)
2. **Hook** -> if a hook exists, run it. Hook can allow, deny, or modify.
3. **Policy allow/ask/deny** -> if `allow`, proceed. If `deny`, deny. If `ask`, check session grants, then prompt the human.

That's three steps. Session grants are a cache of previous human answers for the current session — they're part of the `ask` path, not a separate step.

### 5.5 Execution Path: From Approval to Kernel

The approval model is a **userspace gate**. It decides whether a command gets typed at all. Once approved, execution is pure Unix — the agent doesn't interpret or run the command itself.

```
Agent process (PID 1000, UID=user)
  |
  |  approval gate: policy + hooks + human
  |
  |  writes "rm -rf /tmp/old_builds\n" into the PTY fd
  |
  v
Child shell (bash, PID 1001, UID=user)
  |
  |  shell parses, forks, execs — standard shell behavior
  |
  v
rm (PID 1002, UID=user)
  |
  |  unlink() syscalls against the filesystem
  |
  v
Kernel: file permissions (UID, file modes)
```

The agent injects keystrokes into the child shell's PTY. From the kernel's perspective, this is identical to the user typing the command by hand. The shell forks, execs, and the child process runs with the user's UID, GID, groups, and capabilities — nothing more.

**The approval prompt is the security boundary.** This is the same model `apt` uses ("Do you want to continue? [Y/n]"), the same model `sudo` uses, the same model `rm -i` uses. It works. It's been working for 30 years.

### 5.6 Terminal Approval Mechanics

Approval happens inside agent mode (section 2.4). The agent's TUI renders directly to the terminal; the child shell is paused and never sees the interaction.

**Single-step approval:**
```
Agent: rm -rf /tmp/old_builds
       Deletes 3 directories (240MB).

[a] Allow  [s] Allow for session  [d] Deny  [e] Edit  [c] Add context
```

**Multi-step plan approval:**
```
Agent: Plan (3 steps):
  1. du -sh /tmp/*                      [show sizes]
  2. rm -rf /tmp/old_builds /tmp/cache  [free 800MB]
  3. df -h /tmp                         [verify]

[a] Allow all  [s] Step through  [d] Deny  [e] Edit plan
               [Enter] on a step to inspect/edit it
```

**Step-through mode** prompts before each step. Between steps, the user sees the previous command's output and can:
- `[a]` approve the next step
- `[e]` edit the next step's command
- `[c]` add context (typed as `# instruction`) — agent revises remaining steps
- `[q]` quit the plan

For `[e] Edit`, the agent opens `$EDITOR` with the proposed command (same pattern as `git commit` or `fc`).

**Pipe mode** (`--json`): approval is structured NDJSON on stdin/stdout:

```jsonl
<- {"type":"approval_request","plan_id":"p001","step":{"command":"rm -rf /tmp/old_builds"}}
-> {"type":"approval","plan_id":"p001","decision":"allow"}
<- {"type":"step_complete","plan_id":"p001","step":1,"exit_code":0,"output":"..."}
-> {"type":"steer","plan_id":"p001","context":"also check /var/tmp"}
```

This is the same protocol any programmatic consumer uses. The terminal TUI and a pipe client are just different renderers for the same approval + steering flow.

### 5.7 Audit Trail

Every action, approved or denied, is logged:

```jsonl
{"ts":"...","type":"shell","command":"rm /tmp/old","approval":"policy","result":"ok"}
{"ts":"...","type":"ui","action":"click","target":"Firefox:Submit","approval":"human","result":"ok"}
{"ts":"...","type":"audio","action":"mic_listen","approval":"denied","reason":"policy"}
```

Audit log path: `~/.local/share/unixagent/audit.jsonl`

---

## 6. Interaction Modes

### 6.1 Interactive Shell (Primary)

```sh
$ ls -la                              # normal shell command
# what processes are using the most memory   # agent instruction
# close the Firefox tab playing music        # agent uses UI
# listen for the doorbell and notify me      # agent uses audio
```

### 6.2 Voice

```
User (spoken): "What's running on port 8080?"
Agent (spoken): "It's your dev server, Node.js, started 2 hours ago."
Agent (shell):  lsof -i :8080
```

Voice is an input/output channel, not a separate mode.

### 6.3 Reactive / Background (Watchers)

```toml
# ~/.config/unixagent/watchers.toml
[[watcher]]
trigger = "file_change"
path = "$CWD/src/**/*.rs"
instruction = "# run cargo test and report failures"

[[watcher]]
trigger = "audio_event"
pattern = "doorbell"
instruction = "# send me a notification: someone is at the door"

[[watcher]]
trigger = "screen_change"
app = "Slack"
pattern = "urgent"
instruction = "# summarize the urgent Slack message and notify me"
```

### 6.4 Remote

```sh
$ ssh server unixagent
$ ssh server '# check disk health'
```

### 6.5 Programmatic (Pipe / API)

```sh
$ echo '# summarize this project' | unixagent --json
{"type":"plan","steps":[...]}
{"type":"output","text":"This project is..."}
```

TTY detection switches between human-readable and machine-readable output.

---

## 7. Agent Identity and Spawning

### 7.1 Identity

The agent runs as the user. It has the user's UID, GID, groups, capabilities. It cannot escalate privileges.

- The agent is a process. It has a PID.
- The agent inherits the user's environment.
- The agent's children inherit its permissions.
- The agent cannot modify its own policy.
- The agent cannot disable its own audit log.

### 7.2 Recursive Agents

An agent can spawn child agents. Each child is its own process with its own shell. Children can spawn grandchildren. This is `fork()`.

```
Agent (PID 1000, shell=bash)
+-- # deploy to staging
|   +-- Agent (PID 1001, shell=bash, ssh staging)
|       +-- # check dependencies
|       +-- # run migrations
|       +-- # restart services
+-- # notify team when done
```

### 7.3 Policy Inheritance

A child's permissions are **at most** the parent's. Permissions only narrow, never widen.

```sh
# Parent spawns child with restricted policy
unixagent --policy child-policy.toml -c '# analyze this log file'
```

---

## 8. Context Window Management

The backend has a finite context window. The agent sends terminal history, env, cwd, screenshot, accessibility tree, and conversation history. This will exceed the context window without active management.

### 8.1 Strategy

| Source | Budget | Approach |
|--------|--------|----------|
| Terminal history | Cap at N lines (default 200) | Oldest lines dropped first. Summarize if backend supports it. |
| Screenshots | Max 1024px wide | Compress, resize before base64 encoding. One screenshot per instruction (active window). |
| Accessibility tree | Prune to relevant subtree | Active window + focused element + ancestors. Not the full tree. |
| Environment | Curated allowlist | Only send variables in `context.include_env`. Never send `*_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`. |
| Conversation history | Sliding window | Last N turns. Older turns summarized into a single context block. |

### 8.2 Token Counting

The agent must track approximate token usage per request and stay within the model's context window. Each backend adapter knows its model's limit. The agent trims context (oldest terminal lines first, then conversation history) to fit.

This is not optional. Without context management, the agent breaks after a few turns. This must work correctly in phase 2.

---

## 9. Shell State Tracking

The agent's context includes `cwd`, `env`, and shell state. This state changes constantly as the user runs commands. Stale context produces wrong plans.

### 9.1 Primary Mechanism: Shell Integration

The OSC 133 shell integration (section 2.4) also reports cwd changes. The integration script hooks into:
- **bash**: `PROMPT_COMMAND` — emits cwd after each command.
- **zsh**: `precmd` — same.
- **fish**: `fish_prompt` — same.

The agent reads these escape sequences from the PTY output and updates its context. This is reliable and zero-cost (the data arrives passively).

### 9.2 Fallback: Query Before Each Instruction

If shell integration is unavailable, the agent silently injects `pwd` into the child shell before each `#` instruction and captures the output. This adds a small visible flicker but ensures correct cwd.

Environment variables are harder — there's no non-invasive way to read the child shell's full env without `env` or `declare -x`. The agent captures env at startup and tracks `export` commands it observes in the PTY output. This is best-effort. The user can configure additional variables to always re-query.

### 9.3 Config

```toml
[context]
max_terminal_lines = 200
include_env = ["PATH", "HOME", "USER", "SHELL", "TERM", "LANG"]
requery_env = []    # additional env vars to re-query before each instruction
```

---

## 10. Telemetry and Tracing

### 10.1 The Process Tree IS the Trace Tree

Unix gives us the hierarchy for free. PID/PPID is the trace tree. We don't build a trace collector — we use the OS and add structured context.

### 10.2 Trace Context Propagation

Environment variables — the Unix way to pass context to child processes:

```sh
UA_TRACE_ID=7f3a2b...          # root trace identifier
UA_SPAN_ID=a1b2c3...           # this agent's span
UA_PARENT_SPAN_ID=d4e5f6...    # parent's span
UA_TRACE_DEPTH=2               # nesting level
UA_TRACE_SINK=file:///var/log/ua/traces.jsonl
```

### 10.3 Trace Events (JSONL)

```jsonl
{"trace_id":"7f3a2b","span_id":"a1b2c3","parent_span":"d4e5f6","pid":1001,"host":"staging-1",
 "start":"2026-02-01T12:00:00Z","end":"2026-02-01T12:00:05Z",
 "instruction":"apply latest release",
 "backend":"anthropic","model":"claude-sonnet-4-20250514",
 "actions":[
   {"type":"shell","command":"git pull","exit":0,"duration_ms":1200},
   {"type":"shell","command":"make install","exit":0,"duration_ms":3400}
 ],
 "tokens_in":1500,"tokens_out":800,
 "approval":"policy",
 "result":"success"}
```

### 10.4 Trace Sinks

| Sink | Description |
|------|-------------|
| File (default) | Append JSONL to `~/.local/share/unixagent/traces.jsonl` |
| Journald | systemd journal |
| Syslog | Works everywhere |
| Stdout | Parent process aggregates |

### 10.5 Data Flywheel

Every session records: instruction -> plan -> outcome -> user feedback. With consent, anonymized traces can be exported for model improvement.

```toml
[telemetry]
enabled = true
sink = "file"
path = "~/.local/share/unixagent/traces.jsonl"
share_with_backend = false
anonymize = true
retain_days = 30
```

---

## 11. Composability with Unix

```sh
# Agent output piped to standard tools
# list all errors | grep CRITICAL | wc -l

# Standard tools piped to agent
tail -f server.log | # alert me if you see an OOM error

# Agent as part of a script
#!/usr/bin/env unixagent
# scan this codebase for security vulnerabilities
# for each vulnerability found, create a GitHub issue

# Agent calling agent (via SSH)
ssh prod '# check health and report'

# Cron job
0 9 * * * echo '# summarize overnight alerts' | unixagent --json >> /var/log/daily-summary.json
```

---

## 12. Configuration

### 12.1 Main Config

`~/.config/unixagent/config.toml`:

```toml
[shell]
command = "/bin/bash"           # or $SHELL
confirm_mode = "plan"           # "auto" | "each" | "plan"
integration = true              # OSC 133 prompt markers

[backend]
default = "anthropic"

[backend.anthropic]
api_key_cmd = "pass show anthropic/api-key"
model = "claude-sonnet-4-20250514"

[backend.openai]
api_key_cmd = "pass show openai/api-key"
model = "gpt-4o"

[backend.subprocess]
command = "ollama run llama3"

[vision]
enabled = true
ocr = true

[audio]
enabled = false
stt_backend = "whisper-local"   # "whisper-local" | "whisper-api"

[context]
max_terminal_lines = 200
include_env = ["PATH", "HOME", "USER", "SHELL", "TERM", "LANG"]
```

### 12.2 Policy Config

`~/.config/unixagent/policy.toml` — see section 5.2 above.

### 12.3 Watchers Config

`~/.config/unixagent/watchers.toml` — see section 6.3 above.

---

## 13. CLI Interface

```
unixagent [OPTIONS] [--] [SHELL_ARGS...]

Options:
  -c, --command <INSTRUCTION>    Run a single instruction and exit
  --json                         Force NDJSON output (even on TTY)
  --backend <NAME>               Override default backend
  --model <MODEL>                Override default model
  --policy <PATH>                Use a specific policy file
  --config <PATH>                Use a specific config file
  --trace                        Print trace ID for this session
  --version                      Print version
  --help                         Print help

Environment:
  UNIXAGENT_CONFIG               Path to config file
  UNIXAGENT_POLICY               Path to policy file
  UNIXAGENT_BACKEND              Default backend name
  UA_TRACE_ID                    Inherited trace ID (for child agents)
  UA_SPAN_ID                     Inherited span ID
  UA_PARENT_SPAN_ID              Parent span ID
  UA_TRACE_DEPTH                 Nesting depth
  UA_TRACE_SINK                  Trace output destination
```

---

## 14. Edge Cases and Failure Modes

### 14.1 Concurrent / Interleaved Actions

During agent mode, the user's keystrokes go to the agent TUI, not the shell. The user can:

- **Steer**: type `# new instruction` to redirect the agent mid-plan (section 2.5).
- **Abort**: Ctrl+C or `[q]` to exit agent mode and return to the shell prompt.
- **Escape to shell**: a dedicated key (`Esc` or `Ctrl+Z`) suspends agent mode and drops the user to the shell. Resuming agent mode continues the plan where it left off.

The user always has priority. Agent mode is interruptible at every point.

### 14.2 Error Recovery and Partial Execution

A multi-step plan where step 3 of 5 fails:

- The agent stops at the failed step.
- Shows the error to the user (and to the AI backend as context).
- Asks the backend for a recovery plan (or asks the user what to do).
- Audit log records the partial execution.

### 14.3 Secrets and Sensitive Data

The agent sends context to a cloud AI backend. That context may contain secrets.

- **Env filtering**: Only send a curated list of env vars (config: `include_env`). Never send `*_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` variables.
- **Output scrubbing**: Regex-based scrubbing of common secret patterns before sending to backend. Configurable patterns.
- **Local backend option**: Use a local model (ollama) to avoid sending any data off-machine.
- **Policy enforcement**: `[approval.network]` can deny all outbound except the AI API endpoint.

### 14.4 Terminal Emulator Compatibility

The agent is a **transparent proxy** for all terminal escape sequences it doesn't understand. It only interprets sequences it needs to (prompt detection via OSC 133, and its own output formatting).

Must handle:
- 256-color and truecolor (degrade gracefully)
- Unicode / ASCII fallback for box-drawing
- SIGWINCH (terminal resize) — propagate to child shell
- Mouse events — pass through
- Bracketed paste — forward to child shell
- OSC sequences (hyperlinks, window titles) — pass through

### 14.5 Resource Exhaustion from Backend

The AI backend might generate a plan with 1000 steps, or a command that runs forever.

- **Plan size limit**: Reject plans with more than N steps (configurable, default 50).
- **Output capture limit**: Truncate command output after N bytes (default 1MB).
- **Timeout**: Each command has a timeout (default 5 minutes). The whole plan has a timeout.

### 14.6 Prompt Injection

The agent sends terminal output and file contents to the AI backend. Malicious content could contain prompt injection attacks.

Defense in depth — the AI is just one layer:
- Policy layer enforces rules regardless of what the backend says.
- Deny patterns still block dangerous commands even if the AI is tricked.
- Hooks still run.
- Human approval still applies.

The system prompt should include anti-injection instructions, but the real defense is that the AI's output is untrusted and passes through the same approval pipeline as everything else.

### 14.7 Interrupt Handling

When the user presses Ctrl+C:

- If the agent is waiting for the backend's response: cancel the request, return to prompt.
- If the agent is displaying a plan: cancel the plan.
- If the agent is executing a command: send SIGINT to the command (like normal shell behavior).
- If the agent is waiting for user confirmation: cancel the pending plan.

Double Ctrl+C within 1 second: kill the current agent operation immediately.

### 14.8 Offline Mode

If the configured backend is unreachable:
- Fall back to the next configured backend.
- If no backend is reachable, the agent says so and offers to queue instructions for later.
- A local subprocess backend (ollama) should always be available as ultimate fallback if installed.

### 14.9 Wayland Limitations

Wayland intentionally prevents one application reading another's window contents, injecting input, or enumerating windows.

**Mitigation**:
- Use `xdg-desktop-portal` APIs (screenshot portal, screen sharing portal) — requires user consent via system dialog.
- Use compositor-specific protocols (wlr-screencopy for wlroots-based compositors).
- Use XWayland for applications running under XWayland.
- Graceful degradation: if the agent can't see the screen, it tells the user and suggests alternatives.

### 14.10 macOS Permissions

macOS requires explicit user consent for screen recording, accessibility API access, microphone, and camera. The agent should detect when permissions are missing and guide the user to grant them in System Settings, rather than silently failing.

### 14.11 Cost Control

AI API calls cost money.

- Token usage is tracked in telemetry (tokens_in, tokens_out per request).
- Config option: `[backend] max_tokens_per_session = 100000` — warn or stop after limit.
- The `unixagent --usage` command shows token breakdown for recent sessions.

---

## 15. Implementation Phases

### Phase 1: PTY Wrapper + REPL

Spawn child shell in PTY, proxy all I/O, inject OSC 133 shell integration, intercept `#` lines at the prompt. All shell features work for free.

**Files**: `main.rs`, `repl.rs`, `pty.rs`
**Exit criteria**: Can type shell commands normally. `#` instructions are captured only at the prompt (not in heredocs, REPLs, etc.). Prompt detection works reliably in bash and zsh via OSC 133.

### Phase 2: SSE Backends + Config + Context Management

Anthropic adapter with SSE streaming. Config for API keys via `api_key_cmd`. Context window management — terminal history capping, env filtering, token counting.

**Files**: `ua-backend/anthropic.rs`, `ua-backend/sse.rs`, `config.rs`, `context.rs`
**Exit criteria**: `# what's in /tmp` -> backend returns a plan with `ls /tmp`. Context stays within token limits across multiple turns.

### Phase 3: Second Backend + Common Interface

OpenAI adapter. Extract common backend interface from what the two adapters actually share. Subprocess adapter for ollama.

**Files**: `ua-backend/openai.rs`, `ua-backend/lib.rs`, `ua-backend/subprocess.rs`
**Exit criteria**: Can switch between Anthropic, OpenAI, and ollama. Backend interface is clean and minimal.

### Phase 4: Policy Engine + Hooks

Parse `policy.toml`. Pre-exec hook runner. Command allow/deny.

**Files**: `policy.rs`, `hooks.rs`
**Exit criteria**: Hook denies `curl` -> agent command blocked. Policy deny patterns work.

### Phase 5: Agent Loop + Agent Mode TUI

Full agent loop: context capture -> backend -> plan stream -> agent mode TUI -> interactive approval/steering -> execute. Multi-turn conversation. Stream output (TTY vs NDJSON). User can steer mid-plan, edit steps, inject context.

**Files**: `agent.rs`, `display.rs`, `stream.rs`
**Exit criteria**: Complete interactive session works end-to-end. User can step through plans, edit commands, steer with inline `#` instructions between steps.

### Phase 6: Vision

Screen capture and accessibility API integration. macOS: screencapture + Accessibility API. Linux: X11/Wayland screenshots + AT-SPI2. OCR via platform APIs.

**Files**: `vision.rs` + platform-specific modules
**Exit criteria**: `# what's on screen` -> agent describes the visible windows.

### Phase 7: Audio

Microphone capture, system audio capture, speech-to-text (whisper.cpp local, cloud API fallback). Voice as input channel.

**Files**: `audio.rs` + platform-specific modules
**Exit criteria**: User speaks instruction -> agent transcribes and executes.

### Phase 8: UI Interaction

Input synthesis (click, type, scroll), window management, clipboard, accessibility-targeted actions. Platform-specific: CGEvent/xdotool/ydotool.

**Files**: `ui.rs` + platform-specific modules
**Exit criteria**: `# close the Firefox tab playing music` -> agent finds the tab and closes it.

### Phase 9: Agent Spawning + Telemetry + Static Binary

Child agent spawning, policy inheritance, trace context propagation. Trace sinks. musl build, cross-compilation, packaging. Homebrew formula.

**Exit criteria**: Parent agent spawns child over SSH, trace links them. Single static binary ships.

---

## 16. Open Questions

1. **Should shell integration be truly opt-out?** It modifies the shell environment. Some users will object. But OSC 133 is the foundation of the interaction model — without it, the agent falls back to `##` prefix and heuristic prompt detection. Current position: opt-out with clear documentation of what you lose.

2. **How should the agent handle long-running commands?** A `# build this project` might run for 20 minutes. Should the agent show a progress indicator? Should it allow the user to escape to the shell while waiting?

3. **How should file editing work?** The agent generates `sed` commands? Produces a diff and applies it? Opens `$EDITOR`? Uses a structured edit protocol?

4. **Should the agent support tool use / function calling?** Instead of generating shell commands, the backend could call structured tools (like MCP). This is more reliable than parsing shell commands from free text but less flexible.

5. **How does the agent handle state across SSH hops?** Agent on machine A spawns agent on machine B via SSH. Machine B's agent needs the backend API key. Does it inherit it from machine A (via the SSH channel)? Does machine B have its own config?

6. **What is the upgrade path?** When a new version ships, how does it handle config format changes? Session file format changes?

---

## 17. Non-Goals (v1)

- **Not a new shell**. Wraps existing shells. bash/zsh/fish work unchanged.
- **Not a chatbot**. It's a system agent that happens to understand natural language.
- **Not a GUI app**. The shell is the interface.
- **Not a daemon** (by default). Interactive process. Can be daemonized for watchers.
- **Not a Windows tool** (initially). Linux and macOS.
- **Not a distributed system**. One agent, one machine. Agent-to-agent coordination via SSH if needed. No message bus, no mesh protocol, no cloud orchestration. Those are separate projects for a separate day.
- **Not a SaaS platform**. Runs locally. Cloud deployment is a different product with different requirements.

---

## 18. Testing Strategy

- **Unit tests**: Each module in isolation. Mock the PTY, mock the backend.
- **Integration tests**: Spawn a real shell in a PTY, send `#` instructions, verify the agent's behavior end-to-end. Use a mock HTTP server for the backend.
- **Golden tests**: Record expected NDJSON output for known instruction/context pairs. Regression test against them.
- **Fuzz testing**: Feed random terminal output and instructions to the REPL parser.
- **Platform tests**: On Linux CI, test real platform APIs. On macOS CI, test real platform APIs. No mocking of OS capabilities in platform tests.

---

## 19. External Review Notes (Linus Torvalds, Feb 2026)

### 19.1 What's Validated

The following design choices and implementations were confirmed as correct:

| Area | Assessment |
|------|-----------|
| Core premise (PTY proxy wrapping existing shell) | Correct process model. Like `script(1)` or `screen(1)`. Nothing special from the kernel's perspective. |
| OSC 133 state machine | Genuinely clever. Real state transitions instead of fragile prompt regex. `#` interception limited to PROMPT state makes heredocs/REPLs/vim safe. |
| SSE parser (`sse.rs`) | Proper W3C-compliant parser. Byte-by-byte, handles CRLF, chunked data across boundaries, BEL and ST terminators. 11 tests. Small, focused, well-tested. |
| OSC parser (`osc.rs`) | Disciplined state machine with clear transitions, 256-byte cap on parameter buffer, proper BEL and ESC-backslash handling. Non-133 sequences silently passed through. 10 tests. |
| ANSI stripping state machine (`context.rs`) | Correct per ECMA-48. Proper state machine (Ground, Escape, CSI, OSC), not regex. CSI sequences consumed until final byte (0x40-0x7e). Ring buffer with eviction is the right data structure. |
| Secret filtering (`looks_like_secret`) | Pragmatic and correct for a first pass (but see §19.2.9). |
| Crate structure | Clean. `ua-protocol` has no internal deps. Dependency arrow points one way. Not over-engineered into 15 crates. |
| Test discipline | 99 tests, `make check` runs fmt + clippy + test. Dockerfile on Debian Bookworm with bash, zsh, fish. |

### 19.2 Critical Issues

#### 19.2.1 `block_on` Sync/Async Bridge Blocks the Event Loop

**Severity**: Critical — breaks interactivity during LLM streaming

The synchronous event loop in `run_repl()` reads from an MPSC channel, then calls `rt_handle.block_on()` to synchronously consume the async Anthropic stream. This **blocks the main event loop** while the LLM streams its response. Consequences:

- User presses Ctrl+C during streaming → nothing happens until the stream finishes.
- Child shell produces output → queues up invisibly.
- PTY buffer fills → child shell blocks.

"Works in demos, breaks in production."

**Required fix** (one of):
1. Make the whole REPL async (`tokio::select!` over stdin, PTY, and the backend stream), or
2. Spawn the backend call onto the runtime and poll its channel alongside the others.

PTY reader and stdin threads buffering into the channel is **not sufficient** — processing events is what matters, not buffering them.

*Ref: Architecture Decisions Log entry "Sync/async bridge via `block_on`"*

#### 19.2.2 No Ctrl+C / Interrupt Handling

**Severity**: Critical — safety bug for an auto-executing agent

The design doc describes interrupt handling in §14.7 (double Ctrl+C, cancel during streaming, cancel during approval, cancel during execution). **None of it is implemented.** Right now:

- Ctrl+C during streaming → goes into MPSC buffer, sits there until `block_on` returns.
- Ctrl+C during execution → goes to child shell (correct, but by accident, not design).
- No signal handler. No SIGINT registration.

For a tool that auto-executes shell commands on behalf of the user, the inability to cancel is a **critical safety bug**, not a TODO.

**Required fix**: Register a signal handler for SIGINT. Cancel the stream and return to prompt during streaming. Let SIGINT reach the child during execution (natural PTY behavior). Clear the command queue during queued execution.

#### 19.2.3 Command Extraction is Naive — No Safety Gates

**Severity**: Critical — auto-execution with zero approval

`extract_commands()` does string-level fenced code block parsing: split on lines starting with triple backticks. If the LLM outputs:

```
Here's a harmless explanation:
```bash
rm -rf /
```‎
```

...the agent extracts `rm -rf /` and queues it for auto-execution. There is:
- No policy engine
- No deny patterns
- No approval prompt
- No human confirmation gate

Commands go straight from the LLM to the PTY. The design doc describes an entire approval model (§5) with policy files, hooks, deny patterns, human confirmation, and step-through mode. **None of it exists yet.** This is not Phase 5 material — this is Phase 0.

**Required fix**: At minimum, print each command before execution and wait for the user to press Enter or `y`. The fancy approval TUI can come later. An auto-executing agent with no human gate is an `rm -rf /` waiting to happen.

#### 19.2.4 Command Queue Ignores Exit Codes

**Severity**: High — blind execution after failure

`CommandQueue.handle_osc_event()` checks for `133;B` and dispatches the next command unconditionally. It never looks at the exit code in `133;D`. A three-step plan where step 1 fails should not blindly execute steps 2 and 3.

**Required fix**: Check the exit code from `133;D`. Stop the queue on non-zero exit code. Tell the user. Let them decide whether to continue.

#### 19.2.5 Shell Integration Sourcing is Fragile

**Severity**: Medium

The integration script is written to a `NamedTempFile`, then ` source /tmp/xxx\n` is sent to the PTY. Issues:

1. **No verification that source succeeded.** If the temp file is cleaned up by the OS (some distros clean `/tmp` aggressively), or if the shell can't read it, shell integration is silently absent.
2. **`clear` as the last command is a hack.** If the terminal is slow, the user sees a flash. If the terminal doesn't support `clear`, they see garbage.
3. **Temp file leak on panic.** The file is kept alive by `_integration_file` in `PtySession`. If the session panics before the guard runs, the file leaks. Minor, but sloppy.

#### 19.2.6 `try_wait()` Polled After Every Event

**Severity**: Medium — unnecessary syscall overhead

Every time a stdin byte, PTY output chunk, or resize event arrives, `session.try_wait()` is called. This is a `waitpid(WNOHANG)` syscall on every keystroke.

**Required fix**: Listen for SIGCHLD instead, or at least only check after PTY EOF.

#### 19.2.7 Conversation History Grows Unbounded

**Severity**: High — will blow context window

`conversation` is a `Vec<ConversationMessage>` that gets pushed to on every instruction. There's no sliding window, no summarization, no eviction. The design doc (§8.1) says "Last N turns. Older turns summarized into a single context block." This isn't implemented. After ~20 turns, the context window overflows and the API returns an error.

**Required fix**: Cap at N turns. This is trivial and prevents context window blow-up.

#### 19.2.8 Thread Spawning Without Join Handles

**Severity**: Medium — silent panic swallowing

The stdin, PTY reader, and resize threads in `run_repl()` are spawned and the handles are thrown away. If the PTY reader thread panics, the main loop eventually sees no more events and exits, but the panic is swallowed.

**Required fix**: Join the threads on cleanup.

#### 19.2.9 `looks_like_secret()` is Insufficient for Production

**Severity**: Medium — false sense of security

Checking for `sk-`, `pk-`, `ghp_` prefixes and "longer than 100 chars with no spaces" catches a handful of API keys. It misses:
- AWS keys (`AKIA...`)
- JWTs
- Base64-encoded tokens
- SSH private keys
- Anything that doesn't match the three hard-coded prefixes

**Required fix**: Either do it properly (comprehensive pattern list) or document that it's best-effort and make the user explicitly opt-in to which env vars get sent.

#### 19.2.10 SIGWINCH Handling via Polling

**Severity**: Low — wasteful but functional

Terminal size is polled every 250ms in a thread. Every terminal program since `vi` in 1976 uses a signal handler for SIGWINCH instead.

**Required fix**: Use a SIGWINCH signal handler.

### 19.3 Required Actions (Priority Order)

Per the review, these must be addressed before the agent is usable:

1. **Implement Ctrl+C handling** — register SIGINT handler, cancel streams, clear queue. Non-negotiable for an auto-executing agent.
2. **Add minimal approval gate** — print each command, wait for Enter/`y`. Not Phase 5 — Phase 0.
3. **Fix sync/async bridge** — user must be able to interact while LLM streams.
4. **Check exit codes between queued commands** — stop queue on non-zero `133;D`.
5. **Implement conversation history eviction** — cap at N turns.
6. **Join threads** — don't fire-and-forget spawned threads.
7. **Handle SIGWINCH properly** — signal handler, not polling.
8. **Integration tests with real shells** — bash/zsh/fish with OSC 133. Unit tests on the state machine are necessary but not sufficient.

### 19.4 Overall Assessment

> "The bones are good. Now put some safety on it."

The foundation is correct: PTY proxy model, OSC 133 state machine, crate structure, SSE parser. The code is readable, focused, and tested. No frameworks, no excess dependencies.

The critical gap is between the **design doc's safety promises** (policy engines, hooks, approval cascades, audit trails) and the **code's actual behavior** (auto-executes LLM-generated commands with no gate). Shipping to users before closing that gap means someone loses data.
