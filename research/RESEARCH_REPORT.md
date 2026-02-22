# Research Report: Unix/macOS Control Agents — Landscape Analysis

**Date:** 2026-02-22
**Context:** Competitive analysis for the UnixAgent project

---

## Executive Summary

We cloned and analyzed 10 open-source projects that provide AI-powered Unix, macOS, or desktop control through agent architectures. This report compares their approaches to computer control, LLM integration, security, and identifies novel ideas and limitations relevant to UnixAgent's design.

### Key Finding

No project in the landscape combines **PTY-based shell sessions + OS-level sandboxing (Seatbelt/Landlock) + streaming SSE architecture** the way UnixAgent does. The closest architectural cousins are Butterfish (PTY wrapping) and Goose (Rust + MCP extensibility), but each has significant gaps that UnixAgent's design addresses.

---

## Comparison Matrix

| Project | Language | Shell Execution | Sandbox | PTY | Streaming | Stars |
|---------|----------|----------------|---------|-----|-----------|-------|
| **UnixAgent** | Rust | PTY session | Seatbelt + Landlock | Yes | SSE | — |
| **Open Interpreter** | Python | subprocess.Popen | None (semgrep optional) | No | Generator | 50K+ |
| **Goose** (Block) | Rust | tokio::process | Container optional | No | SSE | 30K+ |
| **Butterfish** | Go | PTY (creack/pty) | None | **Yes** | SSE | ~3K |
| **gptme** | Python | subprocess.Popen | Allowlist + deny rules | No | Generator | ~5K |
| **ShellGPT** | Python | os.system / Popen | None | No | OpenAI streaming | ~15K |
| **macOS-use** | Python | Accessibility API | None | No | LangChain | ~2K |
| **Agent-S** | Python | PyAutoGUI + subprocess | None (eval!) | No | Non-streaming | ~10K |
| **CUA** | Python/Swift/TS | WebSocket → server | VM isolation | No | WebSocket | ~5K |
| **Aider** | Python | N/A (file edits) | None | No | LiteLLM streaming | 39K+ |
| **OpenCode** | Go | exec.Cmd persistent | Permission system | No | Provider events | ~8K |

---

## Detailed Project Analyses

---

### 1. Open Interpreter

**GitHub:** [openinterpreter/open-interpreter](https://github.com/openinterpreter/open-interpreter)
**Architecture:** Python, ~19,500 LOC

#### How It Controls the Computer

- **Multi-language execution** via persistent subprocess sessions (Python, Shell, JS, Ruby, R, Java, PowerShell, AppleScript)
- Uses `subprocess.Popen` with PIPE mode (not PTY) — one persistent process per language
- **Active line markers**: Injects `echo "##active_line1##"` between commands for real-time execution tracking
- **End marker**: `echo "##end_of_execution##"` signals completion
- Shell commands have no terminal awareness (no colors, no interactive programs)
- Python uses **Jupyter kernel** (stateful, persistent)

#### Command Extraction

- **Tool-calling LLMs** (Claude, GPT-4): Uses OpenAI tool_calls schema with single `execute(language, code)` tool
- **Text LLMs**: Markdown code block extraction via state machine parser
- Lots of regex-based hallucination patching (fragile, LLM-specific)

#### LLM Integration

- **LiteLLM** for multi-provider support (OpenAI, Anthropic, local models)
- Custom **LMC (Language Model Communication) format** for unified message handling
- Token trimming via `tokentrim` library — intelligent conversation truncation

#### Security

- **Minimal**: No sandboxing, no filesystem restrictions, no network isolation
- Optional semgrep static analysis (experimental)
- User confirmation before each code execution (can be disabled with `-y`)
- **Critical gap**: LLM jailbreaking bypasses all safety; code runs with full user permissions

#### Novel Ideas

1. **LMC format** — unified abstraction for text, code, images, console output across all interaction types
2. **Language plugin system** — clean `BaseLanguage` class, just implement `run()` generator
3. **Computer API introspection** — auto-generates system message from Python docstrings of available tools
4. **Confirmation-based execution** — yields confirmation chunks so UI can intercept before running

#### Limitations

- No PTY = no terminal features (colors, interactive programs, screen clearing)
- Context window management is basic (truncation, not summarization)
- Jupyter kernel for Python is heavyweight
- No error recovery (LLM must interpret errors and retry manually)

---

### 2. Goose (Block)

**GitHub:** [block/goose](https://github.com/block/goose)
**Architecture:** Rust, multi-crate workspace (v1.24.0)

#### How It Controls the Computer

- **Developer MCP server** provides shell access via `tokio::process::Command`
- Detects OS and shell (bash/zsh/fish on Unix; PowerShell/cmd on Windows)
- Uses `-c` flag to execute — **not persistent sessions**
- **Text editor tool** with view/write/str_replace/insert/undo operations
- **Computer controller** uses `xcap` crate for screen capture (Metal on macOS, Direct3D on Windows)

#### MCP Integration (Key Innovation)

- Deep MCP consumer AND producer
- Built-in extensions run **in-process** via `tokio::io::DuplexStream` (no subprocess overhead)
- External extensions via stdio or streamable HTTP
- Lazy tool initialization with version caching
- Built-in extensions: developer, autovisualiser, computercontroller, memory, tutorial
- `rmcp` macros auto-generate tool schemas from JsonSchema-annotated Rust structs

#### LLM Integration

- **21+ providers** including Anthropic, OpenAI, Google, Azure, AWS Bedrock, Ollama, llama.cpp local
- Provider trait: `stream()` returns `MessageStream`
- Anthropic extended thinking support
- Token-efficient tools API (2025-02-19)
- Auto-compaction at 85% context window via LLM summarization

#### Security

- **Multi-layer**: Tool inspection → Permission manager → Security classification → Container sandbox
- `SecurityInspector` detects prompt injection and code execution risks
- `RepetitionInspector` prevents tool call loops
- Permission persistence in SQLite (AllowOnce / AlwaysAllow / Decline)
- Optional container execution mode

#### Novel Ideas

1. **MCP extension polymorphism** — same tool interface for child process, HTTP, and in-process
2. **Session persistence** with SQLite journal and auto-compaction
3. **YAML/JSON recipes** as composable task templates with MiniJinja templating
4. **ToolStream** combines notification stream + result future (tools can emit progress)
5. **Declarative provider config** — custom LLM providers via config YAML

#### Limitations

- Single active provider per session (can't mix Anthropic + OpenAI)
- Sequential tool execution (no parallel tool dispatch)
- Shell commands are one-shot (no persistent session state)
- Context compaction is lossy

---

### 3. Butterfish

**GitHub:** [bakks/butterfish](https://github.com/bakks/butterfish)
**Architecture:** Go, ~4000 LOC

#### How It Controls the Computer (Most Similar to UnixAgent)

- **PTY shell wrapping** via `creack/pty` library
- Creates PTY, runs user's shell as child process, acts as transparent middleware
- User keeps existing shell configuration and history
- **I/O multiplexing** via Go channels: child output, parent input, LLM responses, cursor position, resize events
- **Raw terminal mode** with SIGWINCH handling for resize propagation
- **Custom PS1 markers** (`\033Q` prefix, `\033R` suffix) for prompt detection

#### Goal Mode — Autonomous Execution

- Triggered by `!` prefix (e.g., `!deploy the app`)
- `!!` prefix = unsafe mode (skip confirmation)
- LLM has 3 functions: `command(cmd)`, `user_input(question)`, `finish(success)`
- Commands **injected directly into PTY master side** (`fmt.Fprintf(this.ChildIn, "%s", command)`)
- Output captured by watching for PS1 markers (2 consecutive prompts = command done)
- Newer models (GPT-5.1+) use OpenAI Responses API shell tool

#### Terminal State Machine

```
stateNormal → stateShell (lowercase char)
stateNormal → statePrompting (uppercase or !)
statePrompting → statePromptResponse (Enter)
statePromptResponse → stateNormal (response complete)
```

#### Novel Ideas

1. **Shell wrapping vs shell replacement** — transparent middleware preserving user's shell
2. **PS1 marker injection** for robust command completion detection (far better than heuristics)
3. **TUI passthrough** — detects interactive apps (vim, less) and bypasses AI context
4. **Token-budgeted autosuggest** — separate goroutine with cancellable requests to save API costs
5. **ShellBuffer** — full terminal editing simulation tracking cursor position, escape sequences

#### Limitations

- Command parsing uses brittle regex for JSON extraction
- Only executes first shell call if LLM returns multiple
- No sandbox or safety constraints (Goal Mode executes anything)
- Token counting assumes exact model behavior
- History truncation loses long command output

---

### 4. gptme

**GitHub:** [ErikBjare/gptme](https://github.com/ErikBjare/gptme)
**Architecture:** Python, ~1200+ files, v0.31.0

#### How It Controls the Computer

- **Shell tool** (82KB) using `subprocess.Popen` with configurable timeout (20 min default)
- **bashlex** for proper bash script parsing (pipes, redirects, heredocs)
- **Background job support**: `bg`, `jobs`, `output`, `kill` commands with 1MB memory-bounded buffers
- **Safety**: Allowlist of 275 safe commands + deny groups for dangerous operations

#### Tool System (30+ tools)

Extensive tool ecosystem: shell, python (IPython), save, append, patch, read, browser (3 backends), computer (screen/keyboard), vision, gh (GitHub), tmux, subagent, MCP, precommit, morph, lessons, youtube, rag, tts, form, choice, todo

#### Novel Ideas

1. **Comprehensive hook system** (138KB) — 15+ event types (STEP_PRE/POST, TURN_PRE/POST, TOOL_EXECUTE_PRE/POST, FILE_SAVE_PRE, etc.) with priority ordering
2. **Streaming code block break** — breaks LLM generation when complete code block detected, executes before continuing
3. **Multi-level config** — system defaults → user config → project config → env vars → CLI args
4. **Context compression** — adaptive compressor with task analyzer for intelligent summarization
5. **ToolSpec with hooks** — each tool can register its own lifecycle hooks

#### Limitations

- Token limits are soft suggestions (warnings, not hard stops)
- No OS-level sandboxing
- Code block detection is regex-based (~300 lines of complex nesting logic)
- No rollback mechanism for failed file operations

---

### 5. ShellGPT

**GitHub:** [TheR1D/shell_gpt](https://github.com/TheR1D/shell_gpt)
**Architecture:** Python, ~2000 LOC

#### How It Controls the Computer

- **Two modes**: Interactive shell (`--shell -s`) with user confirmation, and function calling (autonomous)
- Shell execution: `os.system(f"{shell} -c {shlex.quote(command)}")` — simple but effective
- **Function calling**: LLM can invoke `execute_shell_command()` autonomously via OpenAI tool_calls
- **AppleScript function** on macOS: `osascript -e` for GUI automation

#### Novel Ideas

1. **Role-based system prompts** — Shell/Code/Describe/Default roles with OS/shell template variables
2. **Shell integration scripts** — Ctrl+L in ZSH auto-completes incomplete commands via sgpt
3. **REPL mode** with `e` (execute), `d` (describe), or new prompts

#### Limitations

- **Critical security gap**: Function calling is enabled by default without user approval
- Uses `shell=True` in subprocess (injection risk)
- No command validation or allowlist
- No sandbox of any kind

---

### 6. macOS-use

**GitHub:** [browser-use/macOS-use](https://github.com/browser-use/macOS-use)
**Architecture:** Python, LangChain-based

#### How It Controls macOS (Unique Approach)

- **Accessibility API** via PyObjC bridges — direct element-level control, not mouse/keyboard simulation
- `AXUIElementCreateApplication(pid)` → traverse UI hierarchy → index interactive elements
- **Semantic UI tree** as text: `1[:]<AXButton title="Click me">` with numeric indices for LLM reference
- Actions: `AXUIElementPerformAction()` for clicks, `AXUIElementSetAttributeValue()` for typing
- **AppleScript fallback** for operations beyond Accessibility API

#### Action System

| Action | Method |
|--------|--------|
| click_element | AXPress via Accessibility API |
| input_text | AXSetValue attribute |
| scroll_element | AXScrollUpByPage |
| open_app | NSWorkspace / subprocess |
| run_apple_script | osascript subprocess |

#### Novel Ideas

1. **Text-based semantic UI** instead of screenshots — token-efficient, works with any LLM (even text-only)
2. **Dynamic Pydantic action models** — actions registered at runtime, automatically exposed to LLM
3. **Multi-action batching** — up to 10 actions per step for stable UIs (Calculator example)
4. **Context vs interactive elements** — non-interactive text marked with `_`, interactive numbered

#### Limitations

- Only works with accessible apps (proper AX metadata required)
- No visual feedback or screenshot analysis (vision param exists but isn't used)
- No direct mouse/keyboard control
- UI tree rebuilt from scratch each step (no caching)
- No session persistence or learning

---

### 7. Agent-S

**GitHub:** [simular-ai/Agent-S](https://github.com/simular-ai/Agent-S)
**Architecture:** Python, 3 progressive versions (S1→S2→S3)

#### How It Controls the Computer

- **Visual grounding model** → screenshot analysis → coordinate extraction → **PyAutoGUI code generation**
- Grounding: LLM receives screenshot + element description, outputs `[x, y]` coordinates
- All actions convert to Python strings: `"import pyautogui; pyautogui.click(1024, 768)"`
- Code executed via `eval()` (direct, no sandboxing)
- 16 parameterized primitive actions (click, drag, type, hotkey, scroll, etc.)
- **Code Agent** for structured Python/Bash execution (spreadsheets via LibreOffice UNO bridge)

#### What Made It SOTA (72.6% on OSWorld)

1. **Behavior Best-of-N (BoN)**: Run agent 3× independently, VLM judge selects best trajectory
2. **Flat architecture** (S3): Simplified from hierarchical S2, reduced complexity
3. **Reflection agent**: Per-turn trajectory analysis detecting cycles and validating progress
4. **Extended thinking**: 4K thinking budget for reasoning before action
5. **Procedural memory as code**: System prompts dynamically generated from Python introspection

#### Multi-Platform

- macOS: Cmd+Space spotlight for app launching
- Windows: Win+D desktop + search
- Linux: wmctrl + xdotool for window management

#### Novel Ideas

1. **Unified grounding model** for all actions (single prompt format, single output)
2. **Integrated code agent** as part of action space (`call_code_agent()`)
3. **PyAutoGUI code generation chain** — auditable, debuggable, language-agnostic
4. **Screenshot-only interface** — no DOM parsing, generalizes across any GUI

#### Limitations

- Coordinate hallucination (no validation of generated coordinates)
- OCR text grounding uses aggressive regex cleaning (corrupts special chars)
- `eval()` on LLM-generated code = arbitrary code execution
- Context limited to last 8 screenshots
- No error recovery (failures silently skip turns)

---

### 8. CUA (Computer-Use Agents)

**GitHub:** [trycua/cua](https://github.com/trycua/cua)
**Architecture:** Python/Swift/TypeScript, multi-layer

#### How It Controls the Computer

- **VM-based isolation**: Lume (Apple Virtualization), Lumier (Docker), Cloud, QEMU, Windows Sandbox
- **WebSocket + REST protocol** between agent and computer-server
- Server-side handlers: mouse, keyboard, screenshots, files, windows, accessibility, shell
- Platform-specific handlers: Linux (pyautogui+DBus), macOS (PyObjC), Windows (UIAutomation), Android (ADB)
- **~60 hardcoded commands** in server handler factory

#### SDK

```python
async with Computer(os_type="linux", provider_type="cloud") as computer:
    agent = ComputerAgent(model="anthropic/claude-sonnet-4-5", tools=[computer])
    async for result in agent.run(history):
        history += result["output"]
```

#### Novel Ideas

1. **Multi-provider VM abstraction** — same agent code works across Lume VMs, Docker, Cloud, QEMU
2. **Set-of-Mark (SOM) visual grounding** — numbered bounding boxes on screenshots
3. **DHCP lease parsing** for VM IP detection (Swift)
4. **CuaBot multi-agent sandboxing** — HTTP server spawning Docker containers with window proxying
5. **MCP integration** — computer-server exposes MCP endpoint at `/mcp`
6. **Trajectory saving** — JSONL format with cost tracking for RL training

#### Limitations

- Each action = separate WebSocket round-trip (~100-200ms overhead)
- No batch operation support
- Screenshots only (no DOM access for web)
- 7 independent platform handler implementations (feature parity unknown)
- No cross-VM coordination

---

### 9. Aider

**GitHub:** [paul-gauthier/aider](https://github.com/paul-gauthier/aider)
**Architecture:** Python, 39K+ stars

#### How It Controls the Computer

- **File editing only** — does not execute shell commands
- Multiple edit formats: whole file, SEARCH/REPLACE, unified diff, architect, patch
- **Flexible search and replace** with progressive strategies: exact match → git cherry-pick → diff-match-patch
- **RelativeIndenter** — converts absolute indentation to relative using Unicode markers for indentation-agnostic matching

#### Repository Mapping (Core Innovation)

1. **Tree-sitter** parser extracts function/class definitions and references across 100+ languages
2. **NetworkX PageRank** ranks files by dependency graph relevance
3. Token-budgeted (1024 default, up to 8192 when chat empty)
4. DiskCache (SQLite) with mtime-based invalidation

#### Novel Ideas

1. **PageRank-based repo mapping** — semantic code understanding without token explosion
2. **Multi-format edit system** — model selects best format for its capabilities
3. **Git cherry-pick as search strategy** — uses git itself for merge conflict resolution
4. **Prompt caching** — stable context cached, background warming thread every 5 min
5. **Lazy LiteLLM loading** — reduces startup from ~4s to <0.5s
6. **ChatChunks dataclass** — explicit message composition structure

#### Limitations

- No shell execution (purely file editing)
- Token limits force lossy summarization
- Tree-sitter queries need maintenance per language
- Large repos: initial tag extraction slow

---

### 10. OpenCode

**GitHub:** [opencode-ai/opencode](https://github.com/opencode-ai/opencode)
**Architecture:** Go 1.24+, Bubble Tea TUI

#### How It Controls the Computer

- **Persistent shell session** model — `exec.Cmd` with stdin/stdout/stderr pipes
- Shell state persists between commands (env vars, CWD, virtual envs)
- Timeout: 1 min default, 10 min max
- Output truncation: 30KB max (first/last 15KB with middle omitted)
- **Banned commands**: curl, wget, nc, telnet, browser commands
- **Safe whitelist**: ls, git, go, npm auto-approved

#### TUI (Bubble Tea)

- Elm-inspired component model with Update()/View()
- Pages: Chat, Logs. Dialogs: Permission, Session Switcher, Model Selector, File Picker, etc.
- Vim-like editor mode
- Event-driven via pub-sub broker

#### Novel Ideas

1. **Persistent shell model** — maintains state across commands (unlike one-shot execution)
2. **Pub-sub event architecture** — clean separation between services and UI
3. **LSP integration** — diagnostics exposed to AI via tool
4. **Auto-compaction** at 95% context window (creates new session with summary)
5. **Session lineage** — parent_session_id tracks compaction history
6. **File change tracking** — all file versions tracked during session with diff display

#### Limitations

- No OS-level sandboxing (application-level permissions only)
- Sequential tool execution
- Simple head/tail output truncation loses middle context
- No git-backed rollback

---

## Cross-Cutting Analysis

### How They Execute Shell Commands

| Approach | Projects | Pros | Cons |
|----------|----------|------|------|
| **PTY session** | Butterfish, **UnixAgent** | Real terminal (colors, interactive), persistent state | Complex multiplexing |
| **Persistent subprocess** | Open Interpreter, OpenCode | Stateful, simpler than PTY | No terminal features |
| **One-shot subprocess** | Goose, gptme, ShellGPT | Simple, isolated | No state persistence |
| **os.system()** | ShellGPT | Simplest | No output capture |
| **VM + WebSocket** | CUA | Full isolation | High latency |
| **Accessibility API** | macOS-use | Semantic, not pixel-based | Only accessible apps |
| **PyAutoGUI code gen** | Agent-S | Auditable, cross-platform | Coordinate hallucination |

### Security Spectrum

```
No Security ←————————————————————————————→ Full Isolation
  ShellGPT    Open Interpreter   gptme    Goose    OpenCode   CUA     UnixAgent
  Agent-S     Butterfish                  (perms)  (perms)   (VMs)   (Seatbelt/
  (eval!)     (no sandbox)       (allow-  (multi-  (banned           Landlock)
                                  list)    layer)   cmds)
```

### Command Extraction from LLM Output

| Method | Projects | Reliability |
|--------|----------|-------------|
| **Tool/function calling** | Goose, ShellGPT, Open Interpreter (tool-calling models) | High (structured) |
| **Markdown code blocks** | Open Interpreter (text models), gptme | Medium (parsing errors) |
| **JSON parsing** | Butterfish, macOS-use | Medium (escaping issues) |
| **Plain text (prompt engineering)** | ShellGPT (shell role), **UnixAgent** | Low-medium (depends on model) |
| **PyAutoGUI code generation** | Agent-S | Medium (eval risk) |

### Context Management Strategies

| Strategy | Projects |
|----------|----------|
| **Token trimming** (truncate oldest) | Open Interpreter |
| **LLM summarization** | Goose (85% threshold), OpenCode (95% threshold) |
| **Ring buffer** | Butterfish, **UnixAgent** |
| **PageRank-based selection** | Aider (repo map) |
| **Adaptive compression** | gptme (task analyzer) |
| **Image-only pruning** | Agent-S (keep last 8 screenshots) |

---

## Ideas Worth Stealing for UnixAgent

### High Priority

1. **MCP protocol support** (from Goose) — standard tool extensibility, huge ecosystem
2. **Allowlist/denylist for commands** (from gptme) — 275 safe commands + deny groups as defense-in-depth layer on top of Seatbelt/Landlock
3. **PS1 marker injection** (from Butterfish) — robust command completion detection (UnixAgent already has OSC 133, which is better)
4. **Persistent shell state** (from OpenCode) — env vars, CWD, virtual envs persist across commands

### Medium Priority

5. **Hook system** (from gptme) — event-driven lifecycle hooks for extensibility
6. **Session persistence with auto-compaction** (from Goose/OpenCode) — SQLite journal with LLM summarization at threshold
7. **Repo mapping with PageRank** (from Aider) — if UnixAgent ever does code editing
8. **Multi-action batching** (from macOS-use) — batch stable operations to reduce round-trips

### Worth Watching

9. **Behavior Best-of-N** (from Agent-S) — run N trajectories, VLM judge selects best
10. **VM-based sandbox infrastructure** (from CUA) — for cloud deployment scenarios
11. **Accessibility API integration** (from macOS-use) — semantic app control as complement to shell
12. **Set-of-Mark visual grounding** (from CUA) — if UnixAgent adds GUI capabilities

---

## Architectural Lessons

### What Works

- **Structured tool calling** > markdown parsing > plain text extraction (reliability)
- **Persistent sessions** > one-shot commands (developer experience)
- **OS-level sandboxing** > application-level permissions (real security)
- **PTY wrapping** > subprocess PIPE (terminal fidelity)
- **Streaming** > blocking (responsiveness)

### What Doesn't Work

- **eval()** on LLM output (Agent-S) — arbitrary code execution risk
- **No safety at all** (ShellGPT function calling) — LLM can run anything autonomously
- **Regex-based command extraction** (Butterfish, Open Interpreter) — breaks on edge cases
- **Screenshots-only** without semantic understanding — fragile, model-dependent
- **Monolithic handlers** — gptme's 82KB shell.py, Aider's 61KB commands.py become maintenance burdens

### UnixAgent's Unique Position

UnixAgent is the **only project** that combines:
1. **Rust** for performance and memory safety
2. **True PTY sessions** for full terminal fidelity
3. **OS-level sandboxing** (Seatbelt on macOS, Landlock on Linux)
4. **OSC 133 terminal protocol** for robust prompt/command detection
5. **Streaming SSE** for real-time LLM communication

This is a defensible architectural position that no other project currently matches.

---

## Appendix: Individual Analysis Files

Detailed per-project analyses are available in:
- `research/open_interpreter_analysis.txt`
- `research/goose_analysis.txt`
- `research/butterfish_analysis.txt`
- `research/gptme_analysis.txt`
- `research/shellgpt_analysis.txt`
- `research/macos_use_analysis.txt`
- `research/agent_s_analysis.txt`
- `research/cua_analysis.txt`
- `research/aider_analysis.txt`
- `research/opencode_analysis.txt`
