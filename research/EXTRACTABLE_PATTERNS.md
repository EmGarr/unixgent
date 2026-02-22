# Extractable Patterns: CUA + Butterfish → UnixAgent

**Linus's Rule: Respect the computer. Don't bend the OS to the LLM — bend the LLM to the OS.**

**Shibui**: simple, subtle, unobtrusive beauty. The best abstraction is the
one you don't notice because it follows the grain of the system it inhabits.

---

## The Question

What cool abstractions from CUA (Claude's Computer Use) and Butterfish can
we directly integrate into UnixAgent — without violating the Unix philosophy
or over-engineering?

## The Answer: Three Patterns Worth Stealing, Two Anti-Patterns to Avoid

---

## PATTERN 1: TUI Detection + Passthrough Mode (from Butterfish)

**Priority: HIGH — This is the single biggest improvement available.**

### The Problem

UnixAgent doesn't know when an interactive TUI app (vim, less, htop, nano,
top) is running inside the PTY. When the user runs `vim`, the terminal fills
with cursor movement CSI sequences — screen painting, not command output.
The `OutputHistory` ring buffer dutifully accumulates this garbage. Next time
the LLM sees "terminal history", it gets pages of `ESC[2J ESC[1;1H` nonsense
instead of useful context.

OSC 133 tells us "a command is executing" (state C), but it can't
distinguish `ls` (meaningful output) from `vim` (screen painting).

### How Butterfish Solves It

`shell.go:816-841` — `likelyTUIControlSequence()`:

```go
func likelyTUIControlSequence(data []byte) bool {
    for i := 0; i+2 < len(data); i++ {
        if data[i] != 0x1b || data[i+1] != '[' { continue }
        for j := i + 2; j < len(data); j++ {
            b := data[j]
            if b < 0x40 || b > 0x7e { continue }
            switch b {
            case 'm':  // Color/style — ignore, not conclusive
            case 'A', 'B', 'C', 'D', 'H', 'J', 'K':  // Cursor movement
                return true  // This looks like a TUI
            }
        }
    }
    return false
}
```

When detected + running child process confirmed:
- **Stop feeding OutputHistory** (the output is screen painting, not data)
- **Pass all output straight to the terminal** (user sees the TUI normally)
- **Keep a small tail buffer** (~4KB) so when the TUI exits, we have some
  context about what happened

### How to Integrate into UnixAgent

Location: `ua-core/src/context.rs` (OutputHistory) + `ua-core/src/repl.rs`
(event loop).

```rust
// In OutputHistory or as a standalone function:
fn is_likely_tui_output(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b'[' {
            let mut j = i + 2;
            while j < data.len() && (data[j] < 0x40 || data[j] > 0x7e) {
                j += 1;
            }
            if j < data.len() {
                match data[j] {
                    b'A' | b'B' | b'H' | b'J' | b'K' => return true,
                    _ => {}
                }
            }
        }
        i += 1;
    }
    false
}
```

In the event loop, during `AgentState::Idle` + `TerminalState::Executing`:
- If `is_likely_tui_output(&data)` is true for N consecutive chunks,
  set a `tui_passthrough: bool` flag
- While in passthrough: skip `output_history.feed()`, keep a small
  `tui_tail: VecDeque<u8>` (4KB cap)
- On `Osc133D` (command done): clear `tui_passthrough`, optionally feed
  the sanitized tail into history

**Why this is shibui**: It observes what the terminal is doing and adapts.
No configuration, no lists of "known TUI programs." The terminal tells you
what it is — you just have to listen.

---

## PATTERN 2: Accurate Command Line Tracking (from Butterfish's ShellBuffer)

**Priority: MEDIUM — Improves `# instruction` detection reliability.**

### The Problem

UnixAgent's `line_buf` in `repl.rs` is a simple `String` that accumulates
printable bytes and clears on Enter. It doesn't handle:
- Backspace (0x7f / 0x08) — the buffer accumulates deleted characters
- Arrow keys (CSI A/B/C/D) — cursor movement doesn't update the buffer
- Ctrl-A/Ctrl-E (home/end) — ignored
- Ctrl-W (delete word) — ignored
- Ctrl-U (clear line) — only handled explicitly for the `# instruction` case

This means if the user types `# fix th` then backspaces and types `the bug`,
the line_buf contains `# fix ththe bug` instead of `# fix the bug`.

### How Butterfish Solves It

`shellbuffer.go:13-107` — `ShellBuffer`:

```go
type ShellBuffer struct {
    buffer       []rune
    cursor       int
    termWidth    int
    promptLength int
}
```

It processes every escape sequence: arrow keys move the cursor, backspace
removes characters at cursor position, Ctrl-A jumps to start, Ctrl-E to
end. The buffer always reflects **what the user actually sees on the
terminal line**.

### How to Integrate into UnixAgent

Replace `line_buf: String` with a `LineBuffer` struct:

```rust
struct LineBuffer {
    chars: Vec<char>,
    cursor: usize,
}

impl LineBuffer {
    fn feed_byte(&mut self, byte: u8) { /* handle printable, BS, DEL */ }
    fn feed_csi(&mut self, final_byte: u8) { /* handle arrows, home/end */ }
    fn feed_ctrl(&mut self, byte: u8) { /* handle Ctrl-U, Ctrl-W, Ctrl-A, Ctrl-E */ }
    fn as_str(&self) -> String { self.chars.iter().collect() }
    fn clear(&mut self) { self.chars.clear(); self.cursor = 0; }
}
```

The key insight from Butterfish: you need a **mini-state machine** to parse
the stdin byte stream, because escape sequences arrive as multi-byte
sequences (ESC [ C for right arrow). The existing `OscParser` already shows
the pattern — this would be a smaller, simpler version for CSI-only parsing
on the stdin side.

**Why this is shibui**: The terminal has a well-defined editing model. The
LineBuffer mirrors it exactly — no more, no less. It doesn't try to be
readline. It just tracks what readline is doing.

---

## PATTERN 3: CUA's Structured Action Pipeline (Adapted for Terminal)

**Priority: MEDIUM-LOW — Architectural improvement for future extensibility.**

### What CUA Does Well

CUA models computer interaction as a pipeline:

```
LLM Decision → Action Type → Platform Dispatch → Execution → Result Capture → LLM Feedback
```

Each action has a structured type (click, type, screenshot, scroll,
key_press) with validated parameters. The platform handler (`MacOSHandler`,
`LinuxHandler`) implements the action using OS-native APIs. Results are
captured in a structured format (screenshot image, accessibility tree state)
and returned to the LLM.

The key insight: **the LLM never sees raw OS state**. It sees a structured
representation. And **the OS never receives raw LLM output**. It receives
validated, typed actions.

### Where UnixAgent Already Does This

UnixAgent's flow is:

```
LLM Response → Tool Use (bash command) → Risk Classification → Approval → PTY Execution → Output Capture → Tool Result
```

This is already the right shape. But the "action type" is implicit — it's
always "execute this bash string." The risk classification (`RiskLevel`)
provides some structure, but it's post-hoc analysis of a free-form string.

### What to Extract (Carefully)

**Don't** add a formal `Action` trait or `ComputerHandler` abstraction.
That's GUI thinking. The terminal is the interface. Shell commands are the
action type. This is correct.

**Do** consider CUA's pattern of **result structuring**. Right now,
`OutputHistory` captures raw text. CUA captures structured state
(accessibility tree, screenshot coordinates). The terminal equivalent would
be structured output metadata:

```rust
struct CommandResult {
    exit_code: i32,
    stdout_lines: Vec<String>,    // from OutputHistory
    duration_ms: u64,             // how long it ran
    was_tui: bool,                // from Pattern 1
    cwd_after: Option<String>,    // from process.rs::cwd_of_pid
    files_modified: Vec<String>,  // optional: from inotify/kqueue
}
```

This gives the LLM richer feedback without changing the execution model.
The computer tells us more — we just pass it along.

**Why this is shibui**: Don't change how commands run. Change how we
*describe* what happened. The OS already knows. We just weren't asking.

---

## ANTI-PATTERN 1: Don't Import GUI Abstractions

CUA has `get_accessibility_tree()`, `get_screenshot()`, `click(x, y)`.
These are the right abstractions for a GUI agent. They are the **wrong
abstractions** for a terminal agent.

The terminal's "accessibility tree" is its text content — we already
have that in OutputHistory. The terminal's "screenshot" is its current
buffer — we already have that. The terminal's "click" is sending
characters to the PTY — we already have that.

Importing CUA's handler abstraction would add a layer of indirection
between "the shell" and "the LLM" that doesn't earn its keep. The
shell is already a perfectly good computer interface. OSC 133 is
already a perfectly good protocol for tracking state.

**Linus's take**: "The whole point of Unix is that the shell IS the API.
Don't wrap it in another API."

---

## ANTI-PATTERN 2: Don't Import Butterfish's Capital Letter Heuristic

Butterfish intercepts user input by checking if the first character is
uppercase. Clever hack. But it's a hack — it breaks for commands like
`SSH`, `PATH=foo`, `GIT_...`. It also means the shell never sees these
keystrokes until Butterfish decides to forward them.

UnixAgent's `# prefix` is better: it uses a character (`#`) that bash
treats as a comment, so even if the detection fails, the worst case is
a harmless comment in the shell. The `#` prefix is self-documenting
("this is a comment/instruction") and doesn't collide with any command.

**Keep what we have.** It's the right design.

---

## IMPLEMENTATION PRIORITY

| Pattern | Effort | Impact | Priority |
|---------|--------|--------|----------|
| TUI Detection + Passthrough | Small | High | P0 |
| LineBuffer (accurate tracking) | Medium | Medium | P1 |
| Structured CommandResult | Medium | Medium-Low | P2 |

### P0: TUI Detection

~50 lines of code. Add `is_likely_tui_output()` to `context.rs`, add
`tui_passthrough` flag and `tui_tail` buffer to the event loop. Wire up
on `Osc133D` to clear the flag. The hardest part is deciding the threshold
(how many consecutive TUI chunks before switching to passthrough — 3-5
chunks of 4KB seems right).

### P1: LineBuffer

~100 lines of code. New struct in `repl.rs` (or a small module). Replace
`line_buf: String` with `LineBuffer`. Requires a mini CSI parser for stdin
bytes — can reuse the approach from `OscParser` but for CSI only.

### P2: Structured CommandResult

~60 lines of code. Add timing and metadata to the tool_result construction.
The `cwd_after` is already available via `cwd_of_pid()`. Duration can be
measured from `Osc133C` to `Osc133D`. The `was_tui` flag comes from
Pattern 1.

---

## BUTTERFISH: WHAT THEY GOT RIGHT (AND WHY)

Butterfish's core insight is the **transparent multiplexer** pattern:

```
User Terminal ←→ [Multiplexer] ←→ Child Shell
                     ↕
                 LLM Backend
```

The multiplexer sits between the user and the shell. It observes
everything but modifies as little as possible. When the LLM has
nothing to say, it's invisible. When the LLM has a command to run,
it types it into the shell the same way a user would.

UnixAgent follows this exact pattern. The difference is that Butterfish
is Go with goroutines and channels; UnixAgent is Rust with threads and
mpsc. Same architecture, different dialect.

**What Butterfish does that we should note but not copy:**

- **tiktoken-based token counting**: Butterfish uses actual BPE
  tokenization for history windowing. UnixAgent uses `chars/4`. The
  accuracy difference matters for large context windows, but tiktoken
  in Rust is a heavier dependency. Consider for Phase 3+.

- **PS1 custom markers**: Butterfish injects invisible markers (ESC Q /
  ESC R) into PS1 for prompt detection. UnixAgent uses OSC 133, which
  is the *standard* terminal protocol for this. OSC 133 is better —
  it's what terminals and shells are converging on (iTerm2, WezTerm,
  bash 5.1+, zsh). Don't regress to custom markers.

- **Goal mode with function calling**: Butterfish's "goal mode" wraps
  the LLM in a function-calling loop with `command`, `user_input`, and
  `finish` tools. UnixAgent does the same thing with tool_use blocks.
  Same pattern, same idea.

---

## CUA: WHAT THEY GOT RIGHT (AND WHY)

CUA's core insight is **platform-native APIs over simulation**:

Instead of "move mouse to coordinates and click", CUA uses macOS
Accessibility APIs to directly interact with UI elements by role and
attribute. Instead of OCR to read the screen, it reads the accessibility
tree. Instead of screenshot-matching to find buttons, it queries
`AXUIElementCopyAttributeValue`.

For a GUI agent, this is correct. The accessibility tree IS the
computer's self-description. Using it respects the OS's own model of
its UI.

**The terminal equivalent**: OSC 133 is our accessibility tree. The
shell's prompt/command/output cycle, marked by standard escape
sequences, is the terminal's self-description. We already use it.

**What CUA does that we should note:**

- **Three-platform dispatch**: CUA has separate handlers for macOS,
  Linux, and Windows, each using native APIs. UnixAgent already does
  this in `process.rs` with `#[cfg(target_os)]` modules. The pattern
  is the same — conditional compilation for platform-specific syscalls.

- **Structured result capture**: CUA returns structured data to the
  LLM (accessibility tree JSON, screenshot dimensions, element
  positions). This is Pattern 3 above — richer feedback about what
  happened.

- **Permission automation**: CUA detects and handles macOS permission
  prompts (Accessibility, Screen Recording) automatically. The
  terminal equivalent would be detecting `sudo` password prompts or
  SSH host key confirmations — something to consider for future work.

---

## SUMMARY: The Shibui Path

1. **Listen to the terminal** (TUI detection) — the terminal tells you
   what it's doing. Stop, look, and listen before feeding garbage to
   the LLM.

2. **Mirror the line editor** (LineBuffer) — the shell has a model of
   what the user typed. Mirror it accurately instead of approximating.

3. **Describe what happened** (structured results) — the OS knows the
   exit code, the duration, the CWD, whether it was a TUI. Pass this
   knowledge to the LLM.

4. **Don't add layers** — no GUI abstractions, no platform handler
   traits, no action type enums. The shell is the interface. OSC 133
   is the protocol. Landlock/Seatbelt is the sandbox. These are the
   right tools. Use them better, don't replace them.

The computer already knows what's happening. Our job is to listen
more carefully and waste less of what it tells us.
