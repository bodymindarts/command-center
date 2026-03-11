# Time Primitives v2: Revisions to Original Spec

**Date:** 2026-03-11
**Context:** Revisions based on design review of the v1 implementation. Two areas need rework: the `on-complete` trigger semantics and the `watch` primitive design.

---

## 1. Trigger Rework: Add `on-idle`, Fix `on-complete` Semantics

### Problem with current `on-complete`

The current task status lifecycle has a semantic gap that makes `on-complete` less useful than it appears:

```
                 ┌──────────┐
      spawn ──►  │ Running  │
                 └────┬─────┘
                      │
            ┌─────────┼──────────┐
            ▼         ▼          ▼
     ┌───────────┐ ┌────────┐ ┌────────┐
     │ Completed │ │ Failed │ │ Closed │
     │ (exit 0)  │ │(exit≠0)│ │(manual)│
     └───────────┘ └────────┘ └────────┘
```

- **`completed`** (exit 0): set by `Store::complete_task()`, called from `clat agent complete <id> <exit_code>` via the Stop hook. Means the Claude Code **process exited cleanly**.
- **`failed`** (exit ≠ 0): same path, non-zero exit code.
- **`closed`**: set by `Store::close_task()`, triggered by user pressing `x` in TUI. Means **the user manually killed it**.

**The problem:** Many agent tasks never reach `completed`. They do their work, then idle at the Claude Code prompt waiting for further instructions. The user eventually closes them manually. So `on-complete` only fires for fire-and-forget tasks where the agent exits on its own — a subset of real workflows.

### Solution: `on-idle` trigger

The system already has idle detection infrastructure that isn't being leveraged for triggers:

1. **Hook-based idle detection:** Claude Code fires `Notification` hooks with `idle_prompt` matcher when waiting for input. The `notification-idle.sh` hook forwards `{"_idle":true,"cwd":"..."}` to the TUI's Unix socket.

2. **Handler:** `handle_hook_idle()` in `src/tui/handlers.rs` (line 1008-1020) receives these events, calls `state.mark_task_idle(cwd)`, and sends a Telegram notification (`💤 Task idle: {task_name}`).

3. **Display:** `DisplayStatus::Idle` already exists in `src/task.rs` — it's the `◉` indicator, derived from `TaskStatus::Running` + pane not actively working.

**What's missing:** Idle transitions are observed but never acted upon beyond UI updates. The idle hook handler should also check a triggers/schedules table and fire actions.

### Revised trigger set

| Trigger | When it fires | Agent state after | Primary use case |
|---------|--------------|-------------------|-----------------|
| **`on-idle`** | Agent finishes work, waiting at prompt | Alive, full context preserved | "Phase 1 done, start monitoring" / "Agent ready, begin next step" |
| **`on-complete`** | Agent process exits with code 0 | Dead, session resumable | Fire-and-forget chains |
| **`on-fail`** | Agent process exits with code ≠ 0 | Dead | Error recovery, retry, alerting |

`on-close` (user manual close) should **not** trigger anything — it's an intentional human action that could mean "done" or "abandon", and auto-triggering on it would be surprising.

### Why `on-idle` is more valuable than `on-complete`

**Scenario: PR review agent**
1. Agent opens PR, writes review comments → goes **idle**
2. `on-idle` trigger enables a schedule: "poll PR for responses every 5 min"
3. When new comments arrive, `clat send` wakes the agent (still alive, full context)
4. Agent responds to comments → goes idle again → cycle repeats

With `on-complete`, the agent would need to exit, losing all context. Re-spawning requires reconstructing the full PR review context from scratch.

**Scenario: Build → Test chain**
1. Build agent finishes, goes **idle**
2. `on-idle` trigger spawns test agent with `{{ parent_output }}` context
3. Build agent stays alive in case tests reveal build issues that need fixing

### Implementation guidance

**Where to hook in:** `handle_hook_idle()` in `src/tui/handlers.rs` (line 1008). After the existing `mark_task_idle` + Telegram notification, add a check against the schedules/triggers table:

```rust
fn handle_hook_idle(
    state: &mut ScreenState,
    cwd: &str,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    // NEW: also needs app reference to check/fire triggers
) {
    if let Some(task_name) = state.mark_task_idle(cwd)
        && let Some(tx) = tg_tx
    {
        let _ = tx.send(telegram::TgOutbound::Notify {
            text: format!("💤 Task idle: {task_name}"),
        });
    }
    // NEW: check if this task has on-idle triggers and fire them
}
```

**For `on-complete`/`on-fail`:** Hook into `Store::complete_task()` in `src/store.rs` (line 129) or in `cmd_complete()` in `src/main.rs` (line 336). After recording the exit code, check `on_complete_json` and fire if conditions met.

### Schema addition

The `on_complete_json` column from the original spec is still valid. Add `on_idle_json` alongside it:

```sql
ALTER TABLE tasks ADD COLUMN on_idle_json TEXT;
ALTER TABLE tasks ADD COLUMN on_complete_json TEXT;
```

Both store a JSON action descriptor:
```json
{
  "action": "spawn",
  "skill": "engineer",
  "params": {"task": "..."},
  "condition": "any"
}
```

For `on_idle_json`, add a `fire_once` boolean (default true) to prevent re-firing on every idle transition. Once fired, null out the column or mark it as fired.

```json
{
  "action": "send",
  "message": "Begin phase 2: run the test suite",
  "fire_once": true
}
```

### CLI

```sh
# On idle: send message to the same agent
clat spawn "pr-review" -p task="Review PR #123" \
  --on-idle send "Start polling PR #123 for author responses every 5 min"

# On idle: spawn a follow-up task
clat spawn "build-feature" -p task="Build feature X" \
  --on-idle spawn -s engineer -p task="Run smoke tests"

# On complete: chain to next task (fire-and-forget style)
clat spawn "data-migration" -p task="Run migration script" \
  --on-complete-success spawn -s engineer -p task="Verify migration succeeded"

# On fail: alert
clat spawn "deploy" -p task="Deploy to prod" \
  --on-fail spawn -s reporter -p task="Deployment failed. Investigate and report."
```

---

## 2. Watch Rework: Generic `--check` Command Instead of Hardcoded Types

### Problem with original design

The original spec proposed typed watch targets:

```sh
clat watch pr "owner/repo#123" --every 5m --on-change ...
clat watch url "https://ci.example.com/..." --every 2m --on-change ...
```

This hardcodes a fixed set of watchable things (`pr`, `url`, etc.) into clat. Every new watch type requires new code. This is unnecessarily limiting — the user already knows how to check whatever they want to check via shell commands.

### Solution: `--check` as an arbitrary shell command

The watch primitive should be: **run a shell command on an interval, compare output to last run, fire action if changed.**

```sh
# Generic form — the primitive
clat watch "name" \
  --every 5m \
  --check "gh pr view 123 --json comments --jq '.comments | length'" \
  --on-change spawn -s engineer -p task="PR comment count changed from {{ prev }} to {{ curr }}"

# The user brings their own check command — clat doesn't need to know about PRs
clat watch "ci-pipeline" \
  --every 2m \
  --check "curl -s https://ci.example.com/api/pipelines/123 | jq -r .status" \
  --on-change spawn -s engineer -p task="CI status changed to {{ curr }} (was {{ prev }})"

# Health check (fire on non-zero exit code)
clat watch "api-health" \
  --every 1m \
  --check "curl -sf https://api.example.com/health" \
  --diff exit_code \
  --on-change spawn -s engineer -p task="Health check state changed. Output: {{ output }}"

# Watch a file
clat watch "config-changes" \
  --every 30s \
  --check "sha256sum /etc/nginx/nginx.conf" \
  --on-change spawn -s engineer -p task="nginx config changed"
```

### Watch is not a separate primitive — it's a schedule variant

A watch is just a schedule with a `check_command`. The existing `schedules` table needs two additional columns:

```sql
-- Add to schedules table:
check_command TEXT,                    -- shell command to run (NULL = unconditional schedule)
diff_mode TEXT NOT NULL DEFAULT 'string',  -- 'string' | 'exit_code'
-- poll_state_json already exists in v1 spec — reuse it for storing last output
```

**Behavior:**
- Schedule **without** `check_command` → fires unconditionally on interval/cron (timer behavior, already implemented)
- Schedule **with** `check_command` → fires only when check output changes (watch behavior)

One table, one scheduler loop, two behaviors. `clat watch` is syntactic sugar for `clat schedule create --check`.

### Scheduler loop changes for watch-type schedules

When a schedule has `check_command` set, the scheduler loop does:

```
1. Run check_command as shell subprocess, capture stdout + exit code
2. Compare against poll_state (using diff_mode):
   - 'string': fire if stdout ≠ stored poll_state
   - 'exit_code': fire if exit code changed (e.g., 0→1 or 1→0)
3. If changed:
   a. Render action template with variables:
      {{ prev }}   = previous poll_state
      {{ curr }}   = current stdout
      {{ output }} = current stdout (alias)
      {{ exit_code }} = current exit code
   b. Fire action (spawn / send / reopen)
   c. Update poll_state = current stdout
4. If unchanged: do nothing, update last_fired_at
```

### Template variables

The action's `params_json` / `message` should support these template variables (Jinja2 rendering is already used for skill templates):

| Variable | Value |
|----------|-------|
| `{{ prev }}` | Previous check output (from `poll_state`) |
| `{{ curr }}` | Current check output |
| `{{ output }}` | Alias for `{{ curr }}` |
| `{{ exit_code }}` | Exit code of the check command |
| `{{ schedule_name }}` | Name of this schedule |
| `{{ fire_count }}` | How many times this schedule has fired |

### Convenience wrappers (future, not MVP)

Typed shortcuts can be added *later* as thin wrappers that generate `--check` commands:

```sh
# These two would be equivalent:
clat watch "pr-123" --pr "owner/repo#123" --field comments --every 5m --on-change ...
# ↓ internally becomes ↓
clat watch "pr-123" --check "gh api repos/owner/repo/pulls/123/comments --jq 'length'" --every 5m --on-change ...
```

These are pure CLI sugar — no scheduler changes needed. Ship them when specific patterns become repetitive, not upfront.

---

## 3. Revised Build Order

### Ship 1: Schedules table + scheduler loop + CLI (already done in v1)

No changes needed to what was already implemented.

### Ship 2: Add `check_command` + `diff_mode` to schedules (watch support)

Add two columns to the schedules table. Update the scheduler loop to run check commands and diff output. Add `clat watch` as a CLI alias for `clat schedule create --check`. This turns every schedule into an optional watch.

**Effort:** ~1 day on top of existing schedule implementation.

### Ship 3: `on-idle` trigger

Add `on_idle_json` column to tasks. Hook into `handle_hook_idle()` to check and fire triggers. Add `--on-idle` flag to `clat spawn`.

**Effort:** ~1 day. Most of the work is plumbing the trigger check into the idle handler and ensuring fire-once semantics.

### Ship 4: `on-complete` / `on-fail` triggers

Add `on_complete_json` column to tasks. Hook into `complete_task()` / `cmd_complete()`. Add `--on-complete`, `--on-complete-success`, `--on-fail` flags to `clat spawn`.

**Effort:** ~0.5 day.

### Ship 5: MCP tool for agent self-scheduling

Unchanged from original spec.

---

## 4. Summary of Changes from v1

| Area | v1 Spec | v2 Revision |
|------|---------|-------------|
| **Triggers** | `on-complete` only | Add `on-idle` (primary), keep `on-complete`, add `on-fail`, skip `on-close` |
| **Watch types** | Hardcoded `pr`, `url` variants | Generic `--check <command>` — arbitrary shell command |
| **Watch as primitive** | Separate `clat watch` concept | Watch = schedule + `check_command` column (not a separate table) |
| **Delta detection** | Implied but underspecified | Explicit `diff_mode` (`string` or `exit_code`), template variables (`prev`, `curr`, `output`) |
| **Build order** | on-complete before watch | on-idle before on-complete, watch integrated into schedules early |
