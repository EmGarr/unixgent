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

## Execution model

- Commands via `std::process::Command` (not PTY) — the "hands"
- LLM proposes via tool_use — the "brain"
- Policy gate between brain and hands: deny list enforced, everything else auto-approved
- No raw mode, no interactive approval, no OSC parsing
- Startup banner → stderr: `[ua:sub pid=<PID> depth=<N>]` (parent LLM can grep audit log for this PID)
- Streaming thinking/progress → stderr (captured by parent but not in stdout)
- Final text answer (no tool calls) → stdout
- Exit codes: 0 = success, 1 = error, 2 = depth limit

## Agentic loop

```
loop {
    LLM call (instruction or tool_result in conversation)
    collect response (text + tool_uses)
    if no tool_uses → print text to stdout, exit 0
    for each command:
        classify (deny list check)
        if denied → push denial tool_result, continue loop
    execute via Command::new("sh").arg("-c").arg(cmd).env("UA_DEPTH", depth+1)
    scrub output, push as tool_result
    iteration++
    if iteration >= MAX → exit 1
}
```

## Depth control

`UA_DEPTH` env var. Root REPL = unset (treated as 0). Each child increments.
Binary refuses to start when `UA_DEPTH >= max_depth` (default 3). Propagated
to child commands automatically via `.env("UA_DEPTH", depth + 1)`.

## Files changed

### 1. `crates/ua-core/src/main.rs` — add subagent mode branch

Detect positional args. If stdin is not a TTY and no positional args, read
instruction from stdin. Parse `UA_DEPTH` from env. If instruction present:
skip TerminalGuard/raw mode, check depth limit, call `run_subagent()`,
exit with its return code.

### 2. `crates/ua-core/src/subagent.rs` — NEW: non-interactive agent loop

```rust
pub fn run_subagent(config: &Config, instruction: &str, depth: usize, rt_handle: &Handle) -> i32
```

- `stream_response()` — async helper, streams LLM, collects text + tool_uses
- `classify_and_gate_subagent()` — deny list enforced, else auto-approved
- `execute_command()` — `Command::new("sh").arg("-c")` with `UA_DEPTH` propagation
- Output scrubbed via `scrub_injection_markers`, pushed as tool_result
- Max 10 iterations
- Uses `rt_handle.block_on()` (synchronous — no event loop needed)

### 3. `crates/ua-core/src/config.rs` — add depth limit to SecurityConfig

```rust
// In SecurityConfig (existing struct):
pub max_agent_depth: usize,  // default: 3
```

No new config type. Depth limiting is a security concern — it belongs with the
deny list and audit settings.

### 4. `crates/ua-backend/src/anthropic.rs` — system prompt + depth params

Don't touch `AgentRequest` (protocol type shouldn't know about subagent depth).
Instead, pass depth info directly to `build_system_prompt`:

```rust
fn build_system_prompt(request: &AgentRequest, depth: Option<usize>, max_depth: usize) -> String
```

The caller (REPL or subagent) passes its depth. The prompt builder uses it
locally. Protocol types stay clean.

Two conditional sections in `build_system_prompt()`:

**Subagent identity** (when `subagent_depth.is_some()`):
> You are running as a subagent. You were invoked to complete a focused subtask.
> Provide your final answer as plain text (no tool calls). Your stdout goes to
> the parent agent.

**Delegation capability** (when depth + 1 < max_depth):
> You can delegate subtasks by running: `unixagent "instruction"`
> The subagent writes to the shared audit log at <audit_path>. You can inspect
> what a subagent did by searching the log for its PID (visible in stderr output).

### 5. `crates/ua-core/src/context.rs` — no depth changes needed

`build_agent_request` stays unchanged. Depth is passed separately to the
backend's `build_system_prompt`, not embedded in the request.

### 6. `crates/ua-core/src/lib.rs` — `pub mod subagent;`

### 7. `crates/ua-core/src/audit.rs` — add `log_subagent_start` / `log_subagent_end`

### 8. `crates/ua-core/src/repl.rs` — extract `extract_tool_command` as shared

## What is reused vs new

| Reused unchanged | Modified | New |
|---|---|---|
| AnthropicClient | build_system_prompt (add depth params) | subagent.rs |
| PlanDisplay | SecurityConfig (add max_agent_depth) | run_subagent() |
| AgentRequest (untouched) | main.rs (add subagent branch) | stream_response() |
| policy (classify, deny, validate) | audit.rs (add 2 methods) | classify_and_gate_subagent() |
| scrub_injection_markers | | execute_command() |
| TOOL_RESULT_PREFIX | | |

NOT used by subagent: PTY, OSC, CommandQueue, TerminalGuard, approval UI, judge.
Protocol crate (`ua-protocol`) is untouched.

## Security

- **Recursion bomb**: `UA_DEPTH` + max_depth. Exit 2 at limit.
- **Privilege escalation**: Deny list always enforced. No bypass.
- **Output injection**: `scrub_injection_markers` on all tool_results.
- **Audit as inter-agent observability**: Every subagent logs to shared audit file.
  Parent can `grep` for child PID to see everything it did. No special API —
  just the filesystem. Subagent prints `pid=<PID>` to stderr on start so the
  parent LLM knows which session to look up.
- **No hidden state**: Process-scoped. Dies when done. The audit log is the only
  persistent artifact, and it's inspectable by design.

## Tests

- `subagent.rs`: classify_and_gate_subagent (deny blocks, write auto-approves, audit)
- `config.rs`: max_agent_depth default and TOML parsing
- `anthropic.rs`: system prompt delegation text present/absent by depth
- Integration: depth limit rejection (`UA_DEPTH=3 unixagent "test"` → exit 2)

## Implementation order

1. config.rs (add `max_agent_depth` to SecurityConfig)
2. ua-backend anthropic.rs (depth params on build_system_prompt)
3. audit.rs (subagent start/end events)
4. subagent.rs (new module + tests)
5. lib.rs (register module)
6. main.rs (subagent mode branch + stdin pipe support)
7. repl.rs (extract shared helper, pass depth to backend)
8. make check
9. PLAN.md update
