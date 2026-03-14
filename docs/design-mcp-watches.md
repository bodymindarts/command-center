# Design: MCP Watch System

**Status:** Draft
**Date:** 2025-03-12

## Problem

An agent working on a task needs to wait for external conditions — a CI build
completing, a PR being merged, new review comments arriving. Today the agent
either polls manually (burning LLM tokens every iteration) or loses track
entirely.

We need a system where the agent says "watch for X" and the infrastructure
does the dumb polling in a loop — zero LLM tokens — then pages the agent
once the condition is met.

## MCP Constraints

MCP tool calls are synchronous request/response. The spec provides no
reliable mechanism for a server to inject a message into the agent's
conversation after a tool call has returned:

| Mechanism | Direction | Usable? |
|---|---|---|
| `sampling/createMessage` | Server → Client (LLM inference) | Claude Code does not support it |
| `notifications/message` | Server → Client (logging) | Appears in logs, not in agent conversation |
| `notifications/resources/updated` | Server → Client (resource change) | Client may not surface to agent reasoning |
| Tool call response | Server → Client | Synchronous — blocks until done |

**Implication:** The MCP server cannot deliver notifications. A side channel
is required. `clat send <task_id>` (tmux `send-keys`) already exists and
injects messages directly into the agent's conversation.

## Design: Watch & Report with Check Catalog

### Core Idea

1. **Checks** are a curated catalog of pollable things (Concourse builds,
   GitHub PRs, HTTP endpoints). Each check is whitelisted by definition —
   agents cannot poll arbitrary commands or URLs.
2. The MCP server exposes a `create_watch` tool. The agent picks a check
   from the catalog, fills in typed parameters, and sets a polling interval.
   The call returns immediately with a watch ID.
3. A **watch engine** (long-lived, runs in the dashboard's tokio loop or as
   a standalone service) executes checks in a loop without LLM involvement.
4. When a check's condition is met, the engine delivers the result to the
   agent via `clat send`. The agent wakes up once, with the answer in hand.

### Architecture

```
Agent (Claude Code)
  ↕ stdio or HTTP (MCP)
MCP Server (clat mcp-serve)
  ↓ validates check + role, writes watch record
Store (SQLite / Postgres)
  ↑ reads active watches, executes checks
Watch Engine (dashboard tokio loop or standalone service)
  ↓ condition met → deliver result
clat send → Agent tmux pane
```

**Separation of concerns:**

- **MCP server** — short-lived (stdio) or long-lived (HTTP). Validates
  requests, persists intent. Does not execute watches.
- **Watch engine** — long-lived. Executes checks, evaluates conditions,
  delivers results.
- **Store** — durable. Watches survive MCP server and watch engine restarts.
- **Agent** — the orchestrator. Receives results, decides what to do.

## Check Catalog

### Concept

Checks are the unit of "things the system knows how to poll." Each check
defines:

- **Identity**: name, description, allowed roles
- **Parameters**: typed inputs the agent provides
- **Execution**: how to perform the check (shell command, HTTP request, or
  compiled Rust function)
- **Mode**: how to determine "done" (value match, change detection, or
  first-success)
- **Probe/Report split**: lightweight probe runs every interval; richer
  report runs once when the probe fires

### Three Modes

| Mode | Fires when | Example |
|---|---|---|
| `match` | Probe output matches a regex | Build status becomes `succeeded\|failed` |
| `change` | Probe output differs from previous poll | Comment count goes from 3 → 4 |
| `any` | Probe succeeds (any output) | Timer equivalent — always fires on first check |

### Probe/Report Split

Some checks need a cheap probe (run frequently) and a richer report (run
once when fired):

- **Concourse build** — probe returns status string, report is the same
  (no separate report needed)
- **PR comments** — probe returns comment count (cheap), report returns
  last N comment bodies (richer, only on change)

If `report` is omitted, probe output is delivered directly.

### Encoding: Two Options

#### Option A: TOML Files (local, extensible without recompilation)

```
checks/
├── concourse_build.toml
├── github_pr.toml
└── github_pr_comments.toml
```

```toml
# checks/concourse_build.toml
[check]
name = "concourse_build"
description = "Poll a Concourse build until it reaches a terminal status"
roles = ["engineer", "devops"]

[[check.params]]
name = "target"
required = true
description = "Concourse target name"

[[check.params]]
name = "pipeline"
required = true

[[check.params]]
name = "job"
required = true

[[check.params]]
name = "build_id"
required = true

[check.probe]
type = "command"
command = "fly -t {{ target }} builds -j {{ pipeline }}/{{ job }} -c {{ build_id }} --json | jq -r '.[0].status'"
mode = "match"
match = "succeeded|failed|errored|aborted"
```

```toml
# checks/github_pr_comments.toml
[check]
name = "github_pr_comments"
description = "Watch for new comments on a GitHub PR"
roles = ["engineer", "reviewer", "devops"]

[[check.params]]
name = "repo"
required = true
description = "owner/repo"

[[check.params]]
name = "pr_number"
required = true

[check.probe]
type = "command"
command = "gh api repos/{{ repo }}/issues/{{ pr_number }}/comments --jq 'length'"
mode = "change"

[check.report]
type = "command"
command = "gh api repos/{{ repo }}/issues/{{ pr_number }}/comments --jq '[.[-3:] | .[] | {user: .user.login, body: .body[:200]}]'"
```

**Pros:** Add a check by adding a file. No recompilation.
**Cons:** Shell execution — requires CLI tools on the host. Not suitable for
distributed/high-security environments.

#### Option B: Compiled Rust (distributed, high-security)

```rust
#[async_trait]
trait Check: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn params_schema(&self) -> serde_json::Value;
    fn allowed_roles(&self) -> &[&str];
    fn mode(&self) -> CheckMode;
    fn default_condition(&self) -> Option<&str>;

    async fn probe(&self, params: &serde_json::Value) -> Result<ProbeOutput>;
    async fn report(&self, params: &serde_json::Value) -> Result<String>;
}

struct ProbeOutput {
    value: String,             // the thing to match/compare
    raw: serde_json::Value,    // structured result for the report
}
```

Checks are registered at startup with injected, authenticated clients:

```rust
let mut registry = CheckRegistry::new();
registry.register(ConcourseCheck::new(concourse_client));
registry.register(GitHubPrCheck::new(github_client));
registry.register(GitHubPrCommentsCheck::new(github_client));
```

**Pros:** No shell, no ambient CLI dependencies. Secrets managed at
construction (injected clients). Type-safe. Testable. Shared connection
pools.
**Cons:** Adding a check requires code + recompile (in high-security
contexts, this is a feature — checks go through code review).

#### Choosing

| Concern | TOML + shell | Compiled Rust |
|---|---|---|
| Adding a check | Add file, restart | Add impl, recompile |
| Execution | Shell out | Async Rust function |
| Secrets | Env vars | Injected clients |
| Dependencies | CLI tools on host | Compiled in |
| Security | Whitelisted but shells out | No shell, no injection risk |
| Testing | Integration only | Unit testable with mocked clients |

Both options produce the same agent-facing MCP tool schema. The choice is
an implementation detail invisible to agents.

## MCP Tool Definitions

Three tools. The `create_watch` schema is **generated dynamically** from the
check catalog at server startup.

### `create_watch`

```json
{
  "name": "create_watch",
  "description": "Register a background watch. The system polls the specified check at the given interval without using LLM tokens. You'll receive a message when the condition is met. Returns immediately with a watch ID.",
  "inputSchema": {
    "type": "object",
    "required": ["label", "check"],
    "properties": {
      "label": {
        "type": "string",
        "description": "Short description of what you're watching for (included in the notification when it fires)"
      },
      "check": {
        "oneOf": [
          {
            "type": "object",
            "title": "concourse_build",
            "description": "Poll a Concourse build until terminal status",
            "required": ["name", "target", "pipeline", "job", "build_id"],
            "properties": {
              "name": { "const": "concourse_build" },
              "target": { "type": "string", "description": "Concourse target name" },
              "pipeline": { "type": "string" },
              "job": { "type": "string" },
              "build_id": { "type": "integer" }
            },
            "additionalProperties": false
          },
          {
            "type": "object",
            "title": "github_pr",
            "description": "Poll a GitHub PR until MERGED or CLOSED",
            "required": ["name", "repo", "pr_number"],
            "properties": {
              "name": { "const": "github_pr" },
              "repo": { "type": "string", "description": "owner/repo" },
              "pr_number": { "type": "integer" }
            },
            "additionalProperties": false
          },
          {
            "type": "object",
            "title": "github_pr_comments",
            "description": "Watch for new comments on a GitHub PR",
            "required": ["name", "repo", "pr_number"],
            "properties": {
              "name": { "const": "github_pr_comments" },
              "repo": { "type": "string", "description": "owner/repo" },
              "pr_number": { "type": "integer" }
            },
            "additionalProperties": false
          }
        ]
      },
      "condition": {
        "type": "string",
        "description": "Override default done-condition (regex against check output). Only applies to match-mode checks. Omit to use the check's built-in default."
      },
      "interval_seconds": {
        "type": "integer",
        "minimum": 5,
        "default": 30,
        "description": "How often to poll (seconds)"
      },
      "timeout_seconds": {
        "type": "integer",
        "minimum": 10,
        "default": 3600,
        "description": "Give up after this many seconds and notify with a timeout"
      }
    },
    "additionalProperties": false
  }
}
```

### `cancel_watch`

```json
{
  "name": "cancel_watch",
  "description": "Cancel an active watch. No notification will be sent.",
  "inputSchema": {
    "type": "object",
    "required": ["watch_id"],
    "properties": {
      "watch_id": { "type": "string" }
    },
    "additionalProperties": false
  }
}
```

### `list_watches`

```json
{
  "name": "list_watches",
  "description": "List all watches for the current task (active, fired, and expired)",
  "inputSchema": {
    "type": "object",
    "properties": {},
    "additionalProperties": false
  }
}
```

## Agent Invocation Examples

### Concourse build (match mode)

```json
create_watch({
  "label": "CI build #42",
  "check": {
    "name": "concourse_build",
    "target": "main",
    "pipeline": "core",
    "job": "test",
    "build_id": 42
  },
  "interval_seconds": 30
})
```

Returns: `{ "watch_id": "019d-abc", "status": "active" }`

Agent receives (when build completes):
```
[Watch: CI build #42] Result: succeeded
```

### PR comments (change mode)

```json
create_watch({
  "label": "PR #42 review comments",
  "check": {
    "name": "github_pr_comments",
    "repo": "org/my-repo",
    "pr_number": 42
  },
  "interval_seconds": 60
})
```

Agent receives (when new comment appears):
```
[Watch: PR #42 review comments] New activity detected.
---
[{"user":"reviewer","body":"Looks good but can you add a test for the edge case..."}]
```

### PR merge (match mode, custom condition)

```json
create_watch({
  "label": "PR #42 merge",
  "check": {
    "name": "github_pr",
    "repo": "org/my-repo",
    "pr_number": 42
  },
  "condition": "MERGED",
  "interval_seconds": 60,
  "timeout_seconds": 7200
})
```

## Role-Based Filtering

The check catalog is filtered per agent based on role. An engineer agent
sees all three checks; a reviewer agent sees only `github_pr_comments`.

### How the role reaches the server

**Stdio transport (local):** Role passed as a launch argument. `clat`
controls the spawn and knows the skill/role:

```json
{
  "mcpServers": {
    "watches": {
      "command": "clat",
      "args": ["mcp-serve", "--task-id", "019d...", "--role", "engineer"]
    }
  }
}
```

Wired into `setup_worktree_config` — role derived from skill name.

**HTTP/SSE transport (distributed):** JWT in the auth header:

```
Authorization: Bearer eyJ...
→ { "sub": "task-019d", "role": "engineer", "project": "core-platform" }
```

### Filtering

The `tools/list` response includes only checks allowed for the caller's
role. `create_watch` also validates the role at call time (defense in
depth).

Engineer sees:
```json
"oneOf": [
  { "title": "concourse_build", ... },
  { "title": "github_pr", ... },
  { "title": "github_pr_comments", ... }
]
```

Reviewer sees:
```json
"oneOf": [
  { "title": "github_pr_comments", ... }
]
```

Same tool, different menu. The agent doesn't know it's being filtered.

## Storage

### Watches Table

```sql
CREATE TABLE watches (
    id              TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    label           TEXT NOT NULL,
    check_name      TEXT NOT NULL,
    check_params    TEXT NOT NULL,       -- JSON
    mode            TEXT NOT NULL,       -- 'match' | 'change' | 'any'
    condition       TEXT,                -- regex (match mode only)
    interval_secs   INTEGER NOT NULL,
    timeout_secs    INTEGER NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active',
                                        -- 'active' | 'fired' | 'expired' | 'cancelled'
    previous_probe  TEXT,                -- last probe output (for change detection)
    created_at      TEXT NOT NULL,
    next_check_at   TEXT NOT NULL,
    fired_at        TEXT,
    result          TEXT,                -- delivered payload
    delivered       BOOLEAN NOT NULL DEFAULT 0,
    FOREIGN KEY (task_id) REFERENCES tasks(id)
);
```

### Watch Record Lifecycle

```
active → fired       (condition met, result delivered)
active → expired     (timeout reached, timeout notification delivered)
active → cancelled   (agent called cancel_watch)
```

## Watch Engine

Runs in the dashboard's `tokio::select!` loop alongside existing hook and
assistant event handling.

### Execution Loop

```
every 5 seconds:
    load active watches where next_check_at <= now
    for each watch:
        execute check.probe(watch.check_params)
        evaluate mode:
            match  → does output match condition regex?
            change → does output differ from watch.previous_probe?
            any    → always true
        if condition met:
            if check has report → execute check.report(params)
            else → use probe output as result
            update watch: status=fired, result=report_output
            deliver to agent
        else:
            update watch: previous_probe=output, next_check_at=now+interval
```

### Delivery

**Agent is running (task has tmux pane):**
`send_keys_to_pane(task.tmux_pane, formatted_message)`

**Agent is closed/completed/failed:**
Store result with `delivered=false`. Options (configurable):

1. **Notify ExO** — send to ExO pane: "Watch fired for closed task X: ..."
   ExO decides whether to reopen the task or handle it. *(Recommended
   default — matches "deliberate, then delegate" principle.)*
2. **Queue for reopen** — when `clat reopen <task_id>` is called, deliver
   pending watches as the first message.
3. **Spawn new task** — `clat spawn "handle-watch-<id>"` with the result
   and original task context.

## Integration with Existing Architecture

### MCP Server as `clat mcp-serve`

New subcommand. Runs MCP protocol on stdio (or HTTP for distributed).
Configured in worktree's `settings.local.json` by `setup_worktree_config`:

```json
{
  "mcpServers": {
    "watches": {
      "command": "/path/to/clat",
      "args": ["mcp-serve", "--task-id", "019d...", "--role", "engineer", "--db", "/path/to/data/cc.db"]
    }
  }
}
```

### Store Extensions

New migration step (step 4) adds the `watches` table. New methods:

- `insert_watch()`
- `list_active_watches()`
- `list_watches_for_task(task_id)`
- `update_watch_status(id, status, result)`
- `update_watch_probe(id, previous_probe, next_check_at)`
- `list_undelivered_watches()`
- `mark_delivered(id)`

### Dashboard Integration

Watch engine spawned as a tokio task at dashboard startup. Communicates
via `mpsc::unbounded_channel<WatchEvent>`. Watch events handled in the
existing `tokio::select!` loop alongside hook events, assistant events,
and Telegram events.

### TUI

Active watches shown in task detail view (alongside permissions and
messages). Badge on task list for tasks with active watches.

## End-to-End Flow: Concourse CI Watch

```
1. Agent (engineer task, working on PR #42):
   "I've pushed changes. Let me watch for CI."

2. Agent calls create_watch via MCP:
   check: concourse_build, target: main, pipeline: core, job: test, build_id: 42
   interval: 30s, timeout: 30min, label: "CI build #42 for PR"

3. MCP server (clat mcp-serve):
   - validates check name exists in catalog
   - validates role "engineer" is allowed for concourse_build
   - validates params against check schema
   - writes watch record to SQLite: status=active, next_check_at=now+30s
   - returns { watch_id: "019d-abc", status: "active" }

4. Agent continues working on docs (or goes idle). Zero token spend on CI.

5. Dashboard watch engine (every 5s scan):
   - finds watch 019d-abc, next_check_at has passed
   - executes concourse_build.probe({target: "main", ...})
   - probe returns "started" — doesn't match "succeeded|failed|errored"
   - updates next_check_at = now + 30s

6. [Repeats for ~15 minutes, zero LLM tokens]

7. Probe returns "succeeded" — matches condition.
   - No separate report defined → probe output is the result
   - Updates watch: status=fired, result="succeeded"
   - Delivers via send_keys_to_pane:
     "[Watch: CI build #42 for PR] Result: succeeded"

8. Agent sees new message in conversation:
   "CI passed! Let me merge the PR."
   → proceeds with next steps
```

## Future Extensions (not v1)

- **Event-based triggers**: Webhook endpoint receives external events
  (GitHub webhook, Concourse resource check) instead of polling. Requires
  a long-running HTTP listener.
- **Recurring watches**: Re-arm after firing (for ongoing monitoring).
  Watch stays active, fires multiple times.
- **Cross-task watches**: Watch another task's status (completed, failed).
  Useful for task dependency chains.
- **MCP resource subscriptions**: If Claude Code adds support for
  `notifications/resources/updated` that surfaces in agent reasoning,
  use it as an alternative delivery channel alongside `clat send`.
- **Watch chaining**: "When this watch fires, create another watch."
  Not needed — the agent handles sequencing naturally.

## Design Decisions Log

| # | Decision | Rationale |
|---|---|---|
| 1 | Watch & report (Option C), not self-referential tool calls (Option B) | Agent is the orchestrator. It doesn't need pre-encoded actions — it receives results and decides contextually. Simpler, more flexible, no combinatorial explosion. |
| 2 | Check catalog (not arbitrary commands/URLs) | Security. Whitelisted by definition. Secrets stay in check definitions, never exposed to agents. Params validated against schema. |
| 3 | `clat send` for delivery, not MCP notifications | MCP has no reliable server→agent push mechanism that surfaces in conversation. `clat send` already exists and injects directly into the agent's reasoning. |
| 4 | Probe/report split | Probe is cheap (runs every interval). Report is rich (runs once on fire). Avoids expensive operations on every poll cycle. |
| 5 | Watch engine in dashboard, not in MCP server | MCP server is short-lived (stdio = tied to agent session). Watch engine must be long-lived. Dashboard is already a long-running tokio process with Store access. |
| 6 | Dynamic `oneOf` schema generation from catalog | Agent discovers available checks directly in the tool schema. No separate `list_checks` call needed. Adding a check automatically updates the schema. |
| 7 | Role-based filtering via launch args (local) or JWT (distributed) | Least privilege. Agents only see and can use checks appropriate to their role. |
