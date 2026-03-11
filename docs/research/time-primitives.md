# Time-Based Primitives for clat: Feasibility & Design Report

**Date:** 2026-03-11
**Status:** Research complete — ready for design decisions

---

## 1. Problem Statement

Claude Code agents are ephemeral and stateless regarding time. They can't say "check back in 5 minutes" or "when X finishes, do Y." But real workflows require temporal coordination:

- **Polling**: watch a PR for comments, monitor CI status
- **Chaining**: when task A completes, spawn task B
- **Scheduled checks**: daily digest, hourly health check
- **Monitoring loops**: watch pipeline, notify on failure

The clat system already manages agent lifecycle (spawn/close/reopen/delete), persists state in SQLite, and runs a TUI event loop. The question is: what's the minimal, incremental path to adding time awareness?

---

## 2. What Exists Today

### In clat
- **Task lifecycle**: running → completed/failed/closed, with reopen
- **SQLite store**: tasks, messages, projects — durable across restarts
- **Session IDs**: every task gets a UUID session ID, enabling `claude --resume`
- **TUI event loop**: tick-based (50ms streaming, 500ms idle), polls terminal + Unix socket + Telegram channels
- **Hook system**: agents emit lifecycle events (Stop, Idle, Active, etc.) via Unix socket → TUI
- **`clat agent complete`**: hook-invoked on agent exit, records exit code + output
- **No scheduling**: zero timer/cron/polling infrastructure today

### In Claude Code itself
- **`/loop` and `CronCreate`**: session-scoped scheduling (check every N minutes). Max 50 tasks, 3-day expiry, destroyed on process exit.
- **`--resume <session-id>`**: restores full conversation history. Directory-scoped. Session permissions must be re-approved.
- **MCP tools**: agents can call arbitrary tools exposed by MCP servers — a natural extension point.

### Key insight
Claude Code's built-in `/loop` is useful for within-session polling but dies with the process. clat needs *durable* scheduling that survives agent exits, TUI restarts, and machine reboots.

---

## 3. Landscape Survey

### How other frameworks handle time

| Framework | Timers | Event triggers | State across gaps | Task chaining |
|-----------|--------|---------------|-------------------|---------------|
| **Temporal.io** | Durable timers (persisted, millions concurrent, zero overhead) | Signals, queries | Event sourcing (full replay) | Workflow → Activity → Child workflow |
| **Inngest** | `step.sleep()` / `step.sleepUntil()` (durable, survives restarts) | `step.waitForEvent()` with property matching | Step results persisted | Event-driven fan-out |
| **LangGraph** | Cron jobs (platform feature) | Webhooks, conditional graph edges | Checkpointing (SQLite/Postgres) | Graph edges with conditions |
| **Windmill** | Extended cron syntax | Webhooks, CDC, Kafka, SQS | Stateful polling (JSON state persisted between runs) | Flow steps |
| **GitHub Actions** | Cron (5min minimum) | `workflow_run`, `repository_dispatch` | Stateless (artifacts for data passing) | `needs:` dependencies, `workflow_run` |
| **CrewAI** | None | `@listen` decorators, `or_`/`and_` combinators | Optional Pydantic state persistence | Event-driven via `@listen` |
| **AutoGen** | None | Actor message passing | In-memory only | Message-driven |

### Key patterns worth stealing

1. **Temporal's durable timers**: timers are *persisted state*, not OS timers. A scheduler reads from the database, fires what's due. Survives crashes, restarts, even machine migration.

2. **Inngest's `step.waitForEvent()`**: "pause this workflow until event X arrives, with a timeout." Models "wait for CI to pass" perfectly.

3. **Windmill's stateful polling**: each poll carries forward a `state` JSON blob — last known status, last check time. Only acts on *changes*. Prevents redundant work.

4. **LangGraph's SQLite checkpointing**: snapshots graph state at every step. Enables resume-after-failure. Directly applicable since clat already uses SQLite.

---

## 4. Architecture Options

### Option A: Scheduler loop in clat (recommended)

A background tokio task (or thread) in the TUI process that:
1. Polls a `schedules` table in SQLite every ~10 seconds
2. Fires due items by spawning tasks or sending messages to running agents
3. Updates `last_fired_at` and `next_fire_at`

```
┌─────────────────────────────────┐
│          clat TUI               │
│  ┌───────────┐ ┌─────────────┐  │
│  │ Event loop│ │  Scheduler  │  │
│  │ (UI/hooks)│ │  (bg task)  │  │
│  └─────┬─────┘ └──────┬──────┘  │
│        │               │        │
│        └───────┬───────┘        │
│                ▼                │
│           SQLite DB             │
│     [tasks, schedules, ...]     │
└─────────────────────────────────┘
```

**Pros:**
- Single process, no external dependencies
- SQLite provides durability (survives TUI restart — just re-reads schedules on start)
- Already have a tick-based event loop to hook into
- Full control over firing logic (jitter, backoff, overlap policies)

**Cons:**
- Schedules only fire when TUI is running (but this is fine — agents need tmux anyway)
- Scheduler logic adds complexity to the main process
- Need to handle concurrent schedule evaluation carefully

**Mitigation for "TUI must be running":** clat is designed to run as a persistent session (`clat start` creates a tmux session). The TUI is the daemon. If truly persistent scheduling is needed (machine reboots), a systemd timer can `clat start` on boot.

### Option B: External scheduler (systemd/cron)

System cron or systemd timers call `clat spawn` or `clat send` on a schedule.

**Pros:**
- Zero code in clat (pure CLI composition)
- Survives TUI crashes and restarts
- Battle-tested scheduling infrastructure

**Cons:**
- Poor UX: schedules live outside clat, invisible in TUI
- No dynamic scheduling (agent can't create a schedule at runtime)
- Cron has 1-minute minimum granularity
- Managing schedule lifecycle (create/delete/list) requires separate tooling
- State passing between scheduled runs is manual

**Verdict:** Useful as a *system-scoped* fallback (e.g., "start clat TUI on boot") but not suitable as the primary scheduling mechanism. Agents need to create schedules dynamically.

### Option C: Webhook/event-driven only

Instead of polling, set up webhook receivers for GitHub, CI systems, etc.

**Pros:**
- Real-time (no polling delay)
- Efficient (no wasted checks)
- Natural for GitHub PR events, CI notifications

**Cons:**
- Requires inbound HTTP server (NAT/firewall issues for local dev)
- Not all services support webhooks
- Doesn't cover time-based scheduling (daily digest, "check in 5 minutes")
- Significant new infrastructure (HTTP server, webhook verification, routing)

**Verdict:** Valuable *complement* but not a complete solution. Many use cases are purely time-based, not event-based.

### Option D: Hybrid (recommended approach)

Combine A + C selectively:
- **Scheduler loop** for time-based triggers (cron, intervals, delayed tasks)
- **Webhook receiver** added later as an optimization for supported services
- **External cron** as optional system-level backup

This is what most production systems converge on (GitHub Actions = cron + webhooks, Windmill = cron + webhooks + CDC).

---

## 5. Proposed Primitives

### Minimal viable set (ship first)

#### 5.1 `schedule` — Cron/interval task spawning

```sql
CREATE TABLE schedules (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    -- What to do
    action_type TEXT NOT NULL,        -- 'spawn' | 'send'
    skill_name TEXT,                  -- for spawn actions
    params_json TEXT,                 -- for spawn actions
    message TEXT,                     -- for send actions
    target_task_prefix TEXT,          -- for send actions (find running task)
    -- When to do it
    schedule_type TEXT NOT NULL,      -- 'cron' | 'interval' | 'once'
    cron_expr TEXT,                   -- '0 9 * * *' (cron type)
    interval_secs INTEGER,           -- 300 (interval type)
    fire_at TEXT,                     -- RFC3339 (once type)
    -- State
    enabled INTEGER NOT NULL DEFAULT 1,
    last_fired_at TEXT,
    next_fire_at TEXT NOT NULL,
    fire_count INTEGER NOT NULL DEFAULT 0,
    max_fires INTEGER,               -- NULL = unlimited
    -- Metadata
    project_id TEXT,
    created_by_task TEXT,             -- task ID that created this schedule
    created_at TEXT NOT NULL,
    -- Polling state (for watch-type schedules)
    poll_state_json TEXT              -- last known state for delta detection
);
```

**CLI:**
```sh
# Cron: daily digest at 9am
clat schedule create "morning-digest" \
  --cron "0 9 * * *" \
  --spawn -s reporter -p task="Summarize overnight activity"

# Interval: check PR every 5 minutes
clat schedule create "watch-pr-123" \
  --every 5m \
  --spawn -s engineer -p task="Check PR #123 for new comments, respond if needed"

# One-shot: delayed spawn
clat schedule create "deploy-check" \
  --at "2026-03-11T15:00:00" \
  --spawn -s engineer -p task="Verify deployment succeeded"

# Send message to running task
clat schedule create "reminder" \
  --every 10m \
  --send "check-pr" "Any new comments on the PR?"

# List / delete
clat schedule list
clat schedule delete <id>
clat schedule pause <id>
clat schedule resume <id>
```

**Why this is the minimal useful primitive:** it covers cron (daily digest), interval polling (watch PR), one-shot delays (check later), and message-to-agent (wake up running task). All built on one database table and one scheduler loop.

#### 5.2 `on-complete` — Task completion trigger

```sql
ALTER TABLE tasks ADD COLUMN on_complete_json TEXT;
-- JSON: { "action": "spawn", "skill": "engineer", "params": {"task": "..."} }
-- or:   { "action": "schedule_id", "id": "..." }  (enable a paused schedule)
```

**CLI:**
```sh
# Chain: when "build" completes, spawn "test"
clat spawn "build-feature" -p task="..." \
  --on-complete spawn -s engineer -p task="Run smoke tests against the build"

# Chain: when "pr-review" completes successfully, spawn "merge"
clat spawn "pr-review" -p task="..." \
  --on-complete-success spawn -s engineer -p task="Merge the PR"
```

**Implementation:** The existing `complete_task()` method in `Store` is the natural hook point. After recording exit code, check `on_complete_json`, fire if conditions met.

**Why this is essential:** Task chaining is the second most-requested temporal pattern after scheduling. It's also the simplest to implement — one column, one check on task completion.

### Next tier (ship second)

#### 5.3 `watch` — Stateful polling with delta detection

A higher-level primitive built on `schedule`:

```sh
# Watch a PR for new comments
clat watch pr "owner/repo#123" \
  --every 5m \
  --on-change spawn -s engineer -p task="Respond to new comment: {{ change }}"

# Watch CI pipeline
clat watch url "https://ci.example.com/api/pipelines/123" \
  --every 2m \
  --on-change spawn -s engineer -p task="CI status changed to {{ new_status }}"
```

Under the hood, `watch` creates a schedule with a built-in poll script that:
1. Checks current state (via `gh api`, `curl`, etc.)
2. Compares against `poll_state_json` from last run
3. Only fires the action if state changed
4. Updates `poll_state_json`

This is Windmill's stateful polling pattern adapted for CLI.

#### 5.4 MCP tool: `request_callback`

Expose scheduling to agents themselves via an MCP server:

```json
{
  "name": "request_callback",
  "description": "Ask the orchestrator to send you a message or spawn a follow-up task after a delay",
  "parameters": {
    "delay_minutes": 5,
    "message": "Check PR #123 for new comments",
    "action": "send_to_self"
  }
}
```

The MCP server writes to the `schedules` table. The scheduler loop fires it. This lets agents schedule their own follow-ups without the user having to anticipate every need.

**Implementation:** A small MCP server process that connects to the same SQLite database. Could run as a sidecar or be embedded in the clat process.

---

## 6. Agent Context Across Time Gaps

The central tension: **keep agent alive (expensive, uses context window) vs. exit and re-spawn (loses context, but cheap).**

### Option 1: Keep alive + send message

Agent stays running in tmux. After delay, `clat send <task> "message"` injects text into the Claude Code pane (already implemented via `send_keys_to_pane`).

| Aspect | Assessment |
|--------|-----------|
| Context preservation | Full — agent has entire conversation history |
| Cost | High — Claude Code process consumes resources while idle |
| Complexity | Low — already works today |
| Risk | Agent may hit context window limits on long-running sessions |
| Best for | Short delays (5-30 minutes), active monitoring |

### Option 2: Exit + resume session

Agent exits. On trigger, `clat reopen <task>` resumes with `claude --resume <session-id>`. Full conversation history restored.

| Aspect | Assessment |
|--------|-----------|
| Context preservation | Good — conversation history restored, but session permissions need re-approval |
| Cost | Low — no resources consumed while waiting |
| Complexity | Medium — need to handle permission re-approval |
| Risk | Session may expire; directory must still exist |
| Best for | Longer delays (hours, days), infrequent checks |

### Option 3: Exit + spawn new task with context blob

Agent exits with structured output. New task spawned with that output as input context.

| Aspect | Assessment |
|--------|-----------|
| Context preservation | Partial — only what was explicitly captured |
| Cost | Low |
| Complexity | Medium — need structured output capture + template injection |
| Risk | Context loss if capture is incomplete |
| Best for | Task chaining (A → B), where tasks are conceptually different |

### Recommendation: Tiered approach

| Delay | Strategy |
|-------|----------|
| < 30 min | Keep alive + send message (Option 1) |
| 30 min – 4 hours | Exit + resume session (Option 2) |
| > 4 hours / chaining | Spawn new task with context (Option 3) |

The scheduler should support all three via the `action_type` field: `send` (Option 1), `reopen` (Option 2), `spawn` (Option 3).

---

## 7. Implementation Sketch

### Phase 1: Scheduler loop + schedules table (1-2 days)

1. **Add `schedules` table** via new SQLite migration
2. **Scheduler loop** in the TUI:
   - New background thread (or integrate into existing tick loop)
   - Every 10 seconds: `SELECT * FROM schedules WHERE enabled=1 AND next_fire_at <= now()`
   - For each due schedule:
     - Execute action (`spawn` → call `App::spawn()`, `send` → call `Runtime::send_keys_to_pane()`)
     - Update `last_fired_at`, compute `next_fire_at`, increment `fire_count`
     - If `max_fires` reached, set `enabled=0`
   - On TUI startup: recompute `next_fire_at` for all enabled schedules (handles restarts)
3. **CLI commands**: `clat schedule create|list|delete|pause|resume`
4. **TUI display**: show active schedules in a new panel or as a section in task list

### Phase 2: Task completion triggers (half day)

1. **Add `on_complete_json` column** to tasks table
2. **Modify `complete_task()`** to check and fire triggers
3. **Add `--on-complete` flag** to `clat spawn`
4. **Success/failure conditions**: `--on-complete-success` (exit code 0 only), `--on-complete-failure` (non-zero only), `--on-complete` (any)

### Phase 3: MCP tool for agent self-scheduling (1 day)

1. **Build MCP server** that exposes `request_callback` tool
2. **Server connects to SQLite** and writes to `schedules` table
3. **Register MCP server** in skill configurations (`.claude/settings.local.json`)
4. Agents can now say "check back on this in 10 minutes" by calling the tool

### Phase 4: Stateful watch (1-2 days)

1. **`clat watch` command** that creates a schedule with a poll script
2. **Poll scripts** for common targets (GitHub PR, CI endpoint, URL)
3. **Delta detection** using `poll_state_json`
4. **Template rendering** for change context in spawned task descriptions

---

## 8. Use Cases Mapped to Primitives

### PR monitoring

```sh
# Agent opens PR, then:
clat schedule create "watch-pr-456" \
  --every 5m \
  --spawn -s engineer -p task="Check github.com/org/repo/pull/456 for new comments. If there are comments since the last check, respond to them. Previous context: [PR description and changes summary]"
```

Or, if the agent is kept alive:
```sh
clat schedule create "pr-reminder" \
  --every 5m \
  --send "pr-agent" "Check for new comments on PR #456"
```

### CI watcher

```sh
clat schedule create "ci-watch" \
  --every 2m \
  --max-fires 30 \
  --spawn -s engineer -p task="Check Concourse pipeline 'deploy-prod' status. If failed, report the failure details and suggest fixes. If succeeded, report success."
```

### Daily digest

```sh
clat schedule create "morning-digest" \
  --cron "0 9 * * 1-5" \
  --spawn -s reporter -p task="Summarize: 1) overnight git commits across all repos, 2) open PRs needing review, 3) failed CI pipelines, 4) any open issues assigned to the team"
```

### Release gate (chaining)

```sh
# Step 1: build
clat spawn "nightly-build" \
  -s engineer \
  -p task="Run the nightly build pipeline" \
  --on-complete-success spawn -s engineer \
    -p task="Nightly build succeeded. Run smoke tests against the staging deployment."
```

### Monitoring loop with escalation

```sh
# Check health every 15 minutes, stop after 96 checks (24 hours)
clat schedule create "health-check" \
  --every 15m \
  --max-fires 96 \
  --spawn -s engineer -p task="Check https://api.example.com/health. If unhealthy, investigate and notify via Telegram."
```

---

## 9. Open Design Questions

### Q1: Should schedules survive TUI restarts?

**Recommendation: Yes.** Store everything in SQLite. On TUI startup, reload all enabled schedules and recompute `next_fire_at`. This is what makes the system durable — the defining difference from Claude Code's built-in `/loop`.

### Q2: Overlap policy — what if a scheduled task is still running when the next fire is due?

**Options:**
- **Skip** (default): if a task spawned by this schedule is still running, skip this firing
- **Allow**: spawn anyway (multiple concurrent instances)
- **Replace**: kill the running task, spawn a new one

**Recommendation:** Default to Skip. Add `--overlap allow|skip|replace` flag. This mirrors Temporal's overlap policies.

### Q3: How to pass context from a completing task to a chained task?

**Options:**
- **Output capture**: `clat agent complete` already captures pane output. Inject into chained task's prompt template as `{{ parent_output }}`.
- **Structured output**: agent writes to a known file (e.g., `.claude/output.json`), chained task reads it.
- **Message history**: chained task gets read access to parent task's message history via `clat log`.

**Recommendation:** Start with output capture (already exists). Add `{{ parent_output }}` template variable in `on_complete` action params. Structured output can come later.

### Q4: Should `clat schedule` work without the TUI running?

**For MVP: No.** The scheduler lives in the TUI process. Without TUI, schedules accumulate missed firings. On next TUI start, fire any that are overdue (with a configurable catch-up policy: fire-all-missed vs. fire-once-to-catch-up vs. skip-missed).

**Future:** A lightweight `clat daemon` process (no TUI) that just runs the scheduler. The TUI connects to it instead of owning the scheduler directly.

### Q5: Notification channel for schedule events?

**Recommendation:** Use the existing Telegram integration. When a schedule fires, optionally send a Telegram notification. When a watched resource changes, send the delta. This requires minimal new code — just emit to the existing `TgOutbound` channel.

---

## 10. Recommended Build Order

### Ship 1: Schedules table + scheduler loop + CLI (smallest useful thing)

**What you get:** `clat schedule create "name" --every 5m --spawn -s skill -p task="..."` — durable, interval-based task spawning that survives restarts. Covers daily digest, PR polling, CI watching.

**Effort:** ~2 days. One migration, one background loop, a few CLI commands.

### Ship 2: Task completion triggers

**What you get:** `clat spawn "build" --on-complete-success spawn ...` — automatic chaining. Covers release gates, multi-stage pipelines.

**Effort:** ~0.5 day. One column, one hook in `complete_task()`.

### Ship 3: MCP tool for agent self-scheduling

**What you get:** Agents can call `request_callback` to schedule their own follow-ups. The agent becomes self-directing: "I'll check back on this PR in 30 minutes."

**Effort:** ~1 day. Small MCP server, SQLite integration.

### Ship 4: Stateful watch + delta detection

**What you get:** `clat watch pr "repo#123" --on-change spawn ...` — higher-level abstraction for monitoring with change detection.

**Effort:** ~1-2 days. Poll scripts, state diffing, template rendering.

---

## 11. What NOT to Build (Yet)

- **Webhook receiver**: Requires HTTP server, complicates networking. Polling is good enough for MVP. Add webhooks as an optimization when specific integrations demand it.
- **Distributed scheduling**: Single-machine SQLite is sufficient. Don't add Postgres, Redis, or message queues until there's a multi-machine use case.
- **Workflow DAGs**: Full graph-based workflow definitions (Temporal/LangGraph style) are overkill. Linear chaining (`on-complete`) covers 90% of cases. Fan-out can be done by spawning multiple tasks in `on-complete`.
- **Sub-second scheduling**: 10-second scheduler poll is fine. Real-time needs should use webhooks (future work).
- **Visual workflow editor**: Premature. CLI-first, TUI-display-second.

---

## 12. Summary

| Primitive | What it does | Implementation complexity | Value |
|-----------|-------------|--------------------------|-------|
| **schedule** | Cron/interval/one-shot task spawning | Medium (new table, scheduler loop, CLI) | High — covers most use cases |
| **on-complete** | Chain: task A finishes → task B starts | Low (one column, one hook) | High — enables pipelines |
| **request_callback (MCP)** | Agent schedules its own follow-ups | Medium (MCP server) | Medium — agent autonomy |
| **watch** | Stateful polling with delta detection | Medium (poll scripts, state diffing) | Medium — cleaner PR/CI monitoring |

The durable scheduler loop is the foundation. Everything else builds on it. Start there.
