# Subagent Architecture Plan

## Context

The agent needs to delegate work to subagents. The subagent is just the
`unixagent` binary invoked in non-interactive mode. The LLM uses the existing
`shell` tool to run `unixagent "subtask"` like any other command.

## Design principles (from Unix-Native Agent reference)

What we're adopting:
- **Agents are processes.** stdin/stdout/stderr/exit codes. Composable via pipes.
- **Brain/hands split.** LLM proposes commands (brain), runner validates + executes
  with policy (hands). Already exists: `classify_and_gate → execute`.
- **No hidden state.** Subagent is stateless between invocations. No daemon, no
  session files, no sockets. Process starts, does work, exits.
- **Pipe composability.** `echo "instruction" | unixagent` works. Reads stdin when
  not a TTY and no positional args.
- **Observability is mandatory.** Audit trail for every command. `subagent_start`
  and `subagent_end` events link the trace.
- **Policy engine is the boundary.** Deny list always enforced. That's the kernel
  boundary — not the approval UI, which is a userspace convenience.

What we're NOT adopting yet (future phases):
- Sandbox (namespaces, cgroups, seccomp) → Phase 4.5, already planned
- Separate runner binary → unnecessary indirection when it's one process
- Structured command JSON (`{"cmd":"git","args":["status"]}`) → tool_use API
  already gives us structure. The shell tool's `{"command":"git status"}` is enough.
- Resource limits → Phase 4.5

## Invocation

```
# Positional arg (primary):
unixagent "find all TODO comments and categorize them"

# Pipe (stdin when not a TTY):
echo "summarize this file" | unixagent

# Recursive (child delegates to grandchild):
unixagent "analyze test coverage in crates/ua-core"
```

No positional args + stdin is a TTY = REPL mode (existing behavior).

Empty stdin (e.g. `unixagent < /dev/null` with no args) prints a usage error
to stderr and exits 1. We check for empty/whitespace-only instruction before
entering the agent loop.

## Execution model

- Commands via `std::process::Command` (not PTY) — the "hands"
- LLM proposes via tool_use — the "brain"
- Policy gate between brain and hands: deny list enforced, everything else auto-approved
- No raw mode, no interactive approval, no OSC parsing
- Startup banner → stderr: `[ua:sub pid=<PID> depth=<N>]` (parent LLM can grep audit log for this PID)
- LLM thinking/progress → audit log (not stderr — stderr is for errors only)
- Final text answer (no tool calls) → stdout
- Exit codes: 0 = success, 1 = error (including depth limit)

Depth limit is communicated via a structured stderr line:
`[ua:depth-limit depth=<N> max=<M>]` so the parent can distinguish it from
other failures. But the exit code is just 1 — we don't claim special exit
codes that collide with standard Unix conventions.

## Agentic loop

```
loop {
    LLM call (instruction or tool_result in conversation)
    collect response (text + tool_uses)
    if no tool_uses → print text to stdout, exit 0
    for each command:
        classify_and_gate(cmd, ApprovalMode::AutoApprove)
        if denied → push denial tool_result, continue loop
    execute via Command::new("sh").arg("-c").arg(cmd)
    scrub output, push as tool_result
    iteration++
    if iteration >= max_iterations → exit 1
}
```

`UA_DEPTH` is only set when spawning `unixagent` subprocesses, not on every
command. General commands (ls, grep, git, etc.) don't get the env var —
it's not their concern.

## Depth control

`UA_DEPTH` env var. Root REPL = unset (treated as 0). Each child increments.
Binary refuses to start when `UA_DEPTH >= max_depth` (default 3).

Only propagated when spawning `unixagent` children — the subagent's
`execute_command()` sets `UA_DEPTH` on commands only when the command
invokes `unixagent`. For all other commands, the env var is not set.

## `sh -c` and the deny list gap

The deny list operates on the command string. `sh -c` re-interprets that
string — variable expansion, globbing, semicolons, subshells. This means
`echo $(cat /etc/shadow)` could bypass a deny list that checks for `cat`
at the top level.

This is a known limitation. The deny list is a first-pass filter, not a
sandbox. The real fix is OS-level sandboxing (Phase 4.5: namespaces,
seccomp, cgroups). Until then, the deny list catches the common case
(LLM proposing `rm -rf /`) but does not defend against a determined
adversarial LLM crafting shell injection. This is explicitly documented
so nobody mistakes the deny list for a security boundary against
adversarial input.

## Async model

`run_subagent` is an `async fn`. The tokio runtime is already running in
`main.rs`. You cannot call `block_on()` from within an async context —
tokio will panic. So the subagent loop is async all the way through:

```rust
pub async fn run_subagent(config: &Config, instruction: &str, depth: usize) -> i32
```

`main.rs` calls it from `#[tokio::main]` the same way it calls the REPL.
No `block_on`, no `spawn_blocking`, no second runtime.

## Files changed

### 1. `crates/ua-core/src/main.rs` — add subagent mode branch

Detect positional args. If stdin is not a TTY and no positional args, read
instruction from stdin. Reject empty instruction (print usage, exit 1).
Parse `UA_DEPTH` from env. If instruction present: skip TerminalGuard/raw
mode, check depth limit (print structured stderr line, exit 1), call
`run_subagent().await`, exit with its return code.

### 2. `crates/ua-core/src/subagent.rs` — NEW: non-interactive agent loop

```rust
pub async fn run_subagent(config: &Config, instruction: &str, depth: usize) -> i32
```

- `stream_response()` — async, streams LLM, collects text + tool_uses
- `execute_command()` — `Command::new("sh").arg("-c")`, sets `UA_DEPTH`
  only when spawning `unixagent` children
- Output scrubbed via `scrub_injection_markers`, pushed as tool_result
- Max iterations from `config.security.max_agent_iterations`
- Fully async — no `block_on`

### 3. `crates/ua-core/src/config.rs` — add limits to SecurityConfig

```rust
// In SecurityConfig (existing struct):
pub max_agent_depth: usize,       // default: 3
pub max_agent_iterations: usize,  // default: 10
```

Both depth and iteration limits are configurable. If one is worth
configuring, both are.

### 4. `crates/ua-core/src/repl.rs` — add `ApprovalMode` to `classify_and_gate`

Instead of duplicating `classify_and_gate` for the subagent, add an
`ApprovalMode` enum:

```rust
pub enum ApprovalMode {
    Interactive,   // existing behavior: prompt user for write/admin commands
    AutoApprove,   // subagent: deny list enforced, everything else auto-approved
}

pub fn classify_and_gate(cmd: &str, mode: ApprovalMode, ...) -> GateResult
```

One code path. One place to audit. One place to fix. The existing REPL
calls it with `Interactive`, the subagent calls it with `AutoApprove`.

### 5. `crates/ua-core/src/context.rs` — system prompt composition

System prompt composition moves to `ua-core`. The `build_system_prompt`
helper lives in a context/prompt module in `ua-core`, not in the backend
crate. It composes the prompt string — including subagent identity and
delegation capability sections — and passes the finished string to the
backend. The backend crate just sends what it's given. It doesn't know
what a "subagent" or "depth" is.

Two conditional sections composed in ua-core:

**Subagent identity** (when depth > 0):
> You are running as a subagent. You were invoked to complete a focused subtask.
> Provide your final answer as plain text (no tool calls). Your stdout goes to
> the parent agent.

**Delegation capability** (when depth + 1 < max_depth):
> You can delegate subtasks by running: `unixagent "instruction"`
> The subagent writes to the shared audit log at <audit_path>. You can inspect
> what a subagent did by searching the log for its PID (visible in stderr output).

### 6. `crates/ua-backend/src/anthropic.rs` — accept system prompt string

`build_system_prompt` in the backend loses the depth parameters. It
receives the pre-composed system prompt from ua-core or takes a simpler
form that doesn't know about subagent concerns. The backend's job is
HTTP + SSE, not prompt policy.

### 7. `crates/ua-core/src/lib.rs` — `pub mod subagent;`

### 8. `crates/ua-core/src/audit.rs` — add `log_subagent_start` / `log_subagent_end`

Audit writes use `O_APPEND` mode. On Linux/macOS, writes under `PIPE_BUF`
(4096 bytes) to an `O_APPEND` fd are atomic — no interleaving when
multiple subagents write concurrently. Audit lines must stay under 4096
bytes. This is already true for the existing audit format (structured
single-line JSON), but we enforce it: truncate the command/output field
if the serialized line would exceed 4000 bytes.

## What is reused vs new

| Reused unchanged | Modified | New |
|---|---|---|
| AnthropicClient | classify_and_gate (add ApprovalMode) | subagent.rs |
| PlanDisplay | SecurityConfig (add max_agent_depth, max_agent_iterations) | run_subagent() |
| AgentRequest (untouched) | main.rs (add subagent branch) | stream_response() |
| scrub_injection_markers | audit.rs (add 2 methods, enforce O_APPEND) | execute_command() |
| TOOL_RESULT_PREFIX | context.rs (system prompt composition) | ApprovalMode enum |
| | anthropic.rs (accept composed prompt) | |

NOT used by subagent: PTY, OSC, CommandQueue, TerminalGuard, approval UI, judge.
Protocol crate (`ua-protocol`) is untouched.

## Security

- **Recursion bomb**: `UA_DEPTH` + max_depth. Structured stderr message +
  exit 1 at limit. No special exit codes.
- **Privilege escalation**: Deny list always enforced via single
  `classify_and_gate` code path. No separate copy to drift.
- **Shell injection via `sh -c`**: Known limitation. Deny list is a
  first-pass filter, not a sandbox. Documented explicitly. Real fix
  is OS-level sandboxing in Phase 4.5.
- **Output injection**: `scrub_injection_markers` on all tool_results.
- **Audit as inter-agent observability**: Every subagent logs to shared
  audit file. `O_APPEND` writes, lines under PIPE_BUF for atomicity.
  Parent can `grep` for child PID to see everything it did. No special
  API — just the filesystem.
- **No env pollution**: `UA_DEPTH` only set on `unixagent` child
  processes, not on general commands.
- **No hidden state**: Process-scoped. Dies when done. The audit log is
  the only persistent artifact, and it's inspectable by design.
- **Empty input**: Rejected with usage error before entering agent loop.

## Tests

- `repl.rs`: classify_and_gate with AutoApprove mode (deny blocks, write auto-approves)
- `config.rs`: max_agent_depth and max_agent_iterations defaults and TOML parsing
- `context.rs`: system prompt delegation text present/absent by depth
- `audit.rs`: O_APPEND mode, line length enforcement
- Integration: depth limit rejection (`UA_DEPTH=3 unixagent "test"` → exit 1 + structured stderr)
- Integration: empty stdin rejection (`echo "" | unixagent` → exit 1)

## Implementation order

1. config.rs (add `max_agent_depth` and `max_agent_iterations` to SecurityConfig)
2. repl.rs (add `ApprovalMode` enum, refactor `classify_and_gate`)
3. context.rs (system prompt composition with depth-aware sections)
4. ua-backend anthropic.rs (accept composed system prompt, remove depth knowledge)
5. audit.rs (subagent start/end events, enforce O_APPEND + line length)
6. subagent.rs (new async module + tests)
7. lib.rs (register module)
8. main.rs (subagent mode branch + stdin pipe support + empty input check)
9. make check
10. PLAN.md update
