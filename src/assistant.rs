use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use crate::primitives::ProjectId;

pub const EXO_SYSTEM_PROMPT: &str = "\
You are ExO, the executive orchestrator of a multi-agent command center. \
You operate a three-tier hierarchy: User → ExO → PM(s) → Tasks.

## Role
You are a strategic co-pilot. You think alongside the user — discuss, clarify, \
surface trade-offs. Then delegate execution downward, never doing implementation yourself.

## Workflow
For non-trivial work, create a project and brief its PM:
```
clat project create <name>
```
Then switch to the project tab and tell the PM what to build. \
The PM autonomously spawns engineers, researchers, monitors — you don't micromanage.

For simple command-center fixes or one-off tasks, spawn directly:
```
clat spawn \"<name>\" -p task=\"<description>\"
```

## What you do yourself
- Discuss architecture, trade-offs, and priorities with the user
- Create projects and brief PMs on goals
- Check status (`clat list`, `clat list --project <name>`)
- Answer direct questions about the codebase
- Merge completed work, manage branches
- Anything the user explicitly asks you to do directly

## What you delegate
- All implementation, research, and review work
- Codebase exploration for tasks you're about to spawn — put investigation \
instructions in the task description instead";

pub fn project_system_prompt(project_name: &str) -> String {
    format!(
        "You are PM, the autonomous project manager for '{project_name}'. \
You own execution end-to-end: understand the goal, plan the approach, spawn agents, \
monitor progress, and deliver results.

## Bias toward action
When given a goal, start executing immediately. Don't ask permission to spawn tasks — \
that's your job. Break work down, assign it, and report progress.

## NEVER do implementation work yourself
You are a manager, not an engineer. **Never run cargo, nix, git, or any long commands \
directly.** If you run a Bash command that takes more than a few seconds, you block \
yourself from processing messages — ExO and agents cannot reach you. \
Always spawn an engineer or researcher task instead. The only commands you should run \
are short `clat` commands (list, log, send, spawn, close).

## Skills (use `-s` flag)
- `engineer` (default) — implementation, bug fixes, features. Commits code.
- `researcher` — exploration, feasibility, RnD. Reports findings, no commits.
- `reviewer` — code review, PR audits. Reviews and comments.
- `monitor` — watches for conditions (timers, commands). Fires notifications.
- `reporter` — generates reports and summaries from data/logs.
- `security-auditor` — security review, vulnerability assessment.

## Spawning tasks
```
clat spawn \"<name>\" --project {project_name} -s <skill> -p task=\"<description>\"
```
- Task descriptions must be **self-contained** — agents don't see this conversation
- Spawn multiple tasks in parallel for independent work
- For cross-repo work: `--repo <path>`
- For standalone/scratch work: `--scratch`
- For existing branches: `--branch <branch>`

## Coordination
- `clat list --project {project_name}` — check task status
- `clat log <id>` — read an agent's message history
- `clat send <id> <message>` — send instructions to a running agent
- `clat close <id>` — close a completed task

## Feedback loop
Agents report back to you via `send_message(target=\"pm\")` when they finish, \
get blocked, or need clarification. When you receive an agent message, act on it \
immediately — check results, spawn follow-up tasks, or report to the user. \
You can also proactively check with `clat log <id>` or `clat send <id> <message>`.

## Communicating with ExO
ExO sends you messages via this chat. Your text responses appear here too, \
but ExO must switch to your project tab to see them. When ExO asks for a \
status update or report, respond with `clat send exo \"<summary>\"` so it \
appears directly in ExO's chat — don't just print it here.

## Execution loop
1. Understand the goal (ask if truly ambiguous, but prefer reasonable assumptions)
2. Break into tasks, spawn agents
3. React to agent callbacks as they report back
4. Coordinate — unblock stuck agents, spawn follow-ups
5. Report results to ExO via `clat send exo \"<summary>\"`

## CI monitoring
When an engineer opens a draft PR, immediately spawn a monitor task to watch CI. \
If any checks fail, spawn an engineer to investigate and fix. Don't wait for ExO \
to tell you — this is your default post-PR workflow."
    )
}

/// Identifies which session an event belongs to.
#[derive(Clone, Debug)]
pub enum SessionKey {
    Exo,
    Project(ProjectId),
}

pub enum AssistantEvent {
    TextDelta(String),
    ToolStart(String),
    SessionId(String),
    TurnDone,
    ProcessExited,
    Error(String),
}

/// Persistent claude session. The claude process stays alive across turns
/// — messages are sent on stdin and responses streamed back on stdout without
/// any respawn between turns. Used for both ExO and PM sessions.
pub struct AssistantSession {
    stdin: Option<ChildStdin>,
    cancel: Arc<AtomicBool>,
    /// When true, the reader task forwards content events. Set to false
    /// during --resume startup so replayed history is ignored.
    active: Arc<AtomicBool>,
    event_tx: mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
    session_key: SessionKey,
    session_id: Option<String>,
    system_prompt: String,
    skip_permissions: bool,
}

impl AssistantSession {
    /// Create a new session and eagerly spawn the claude process so it
    /// warms up in the background. `Command::spawn` returns immediately
    /// (fork+exec) so this does not block the caller.
    pub fn new(
        session_key: SessionKey,
        session_id: Option<&str>,
        cancel: Arc<AtomicBool>,
        system_prompt: &str,
        event_tx: mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
        skip_permissions: bool,
    ) -> Self {
        let mut session = AssistantSession {
            stdin: None,
            cancel,
            active: Arc::new(AtomicBool::new(false)),
            event_tx,
            session_key,
            session_id: session_id.map(|s| s.to_string()),
            system_prompt: system_prompt.to_string(),
            skip_permissions,
        };
        session.spawn_process(session_id.is_some());
        session
    }

    fn spawn_process(&mut self, resume: bool) {
        let mut args = vec![
            "-p".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
        ];
        if self.skip_permissions {
            args.push("--dangerously-skip-permissions".to_string());
        } else {
            args.extend([
                "--allowedTools".to_string(),
                "Read,Grep,Glob,Bash,Edit,Write".to_string(),
            ]);
        }
        args.extend([
            "--append-system-prompt".to_string(),
            self.system_prompt.clone(),
        ]);

        if resume && let Some(ref sid) = self.session_id {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }

        let mut child = match Command::new("claude")
            .args(&args)
            .env_remove("CLAUDECODE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = self.event_tx.send((
                    self.session_key.clone(),
                    AssistantEvent::Error(format!("Failed to spawn claude: {e}")),
                ));
                return;
            }
        };

        self.stdin = child.stdin.take();
        if self.stdin.is_none() {
            let _ = self.event_tx.send((
                self.session_key.clone(),
                AssistantEvent::Error("Failed to open stdin pipe".into()),
            ));
            let _ = child.kill();
            return;
        }

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = self.event_tx.send((
                    self.session_key.clone(),
                    AssistantEvent::Error("Failed to open stdout pipe".into()),
                ));
                let _ = child.kill();
                self.stdin = None;
                return;
            }
        };

        let tx = self.event_tx.clone();
        let cancel = Arc::clone(&self.cancel);
        let active = Arc::clone(&self.active);
        let key = self.session_key.clone();
        tokio::task::spawn_blocking(move || {
            read_stdout(child, stdout, tx, cancel, active, key);
        });
    }

    /// Send a message to the running claude process.
    /// If the process has exited, spawns a new one first (with --resume).
    pub fn send_message(&mut self, message: &str, session_id: Option<&str>) {
        // Update session_id if provided
        if let Some(sid) = session_id {
            self.session_id = Some(sid.to_string());
        }

        // Respawn if process is gone (with --resume for context)
        if self.stdin.is_none() {
            self.spawn_process(true);
        }

        // Mark active so the reader task forwards events for this turn
        self.active.store(true, Ordering::Relaxed);

        let stdin = match self.stdin.as_mut() {
            Some(s) => s,
            None => {
                // spawn_process already sent an error
                return;
            }
        };

        let sid = self.session_id.as_deref().unwrap_or("default");
        let msg_json = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": message,
            },
            "session_id": sid,
            "parent_tool_use_id": null,
        });

        if let Err(e) = writeln!(stdin, "{}", msg_json) {
            let _ = self.event_tx.send((
                self.session_key.clone(),
                AssistantEvent::Error(format!("Failed to write to claude stdin: {e}")),
            ));
            self.stdin = None;
            return;
        }
        if let Err(e) = stdin.flush() {
            let _ = self.event_tx.send((
                self.session_key.clone(),
                AssistantEvent::Error(format!("Failed to flush claude stdin: {e}")),
            ));
            self.stdin = None;
        }
    }

    /// Update the session_id (called when we receive one from the process).
    pub fn set_session_id(&mut self, id: String) {
        self.session_id = Some(id);
    }

    /// Mark the process as gone (called when ProcessExited is received).
    pub fn mark_exited(&mut self) {
        self.stdin = None;
    }
}

/// Background reader: parses stream-json stdout and sends AssistantEvents.
///
/// With `--include-partial-messages`, streaming events arrive as:
///   `{"type": "stream_event", "event": {"type": "content_block_delta", ...}}`
/// The inner `event` is unwrapped and matched for text deltas and tool starts.
///
/// The `assistant` event (full message) is ignored since text was already
/// streamed via deltas. The `result` event signals turn completion.
///
/// Content events are only forwarded when `active` is true, so session replay
/// during `--resume` startup is silently discarded.
fn read_stdout(
    mut child: Child,
    stdout: std::process::ChildStdout,
    tx: mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    key: SessionKey,
) {
    let reader = std::io::BufReader::new(stdout);

    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            return;
        }

        let line = match line {
            Ok(l) => l,
            Err(e) => {
                let _ = tx.send((
                    key.clone(),
                    AssistantEvent::Error(format!("Error reading claude output: {e}")),
                ));
                break;
            }
        };

        if line.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

        let is_active = active.load(Ordering::Relaxed);

        match event_type {
            // Session metadata — always forwarded (even during startup replay)
            "system" => {
                if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                    let _ = tx.send((key.clone(), AssistantEvent::SessionId(sid.to_string())));
                }
            }

            // Streaming events from --include-partial-messages
            // Wrapped as: {"type": "stream_event", "event": {<API event>}}
            "stream_event" if is_active => {
                if let Some(inner) = parsed.get("event") {
                    handle_stream_event(inner, &tx, &key);
                }
            }

            // Result event — signals turn completion
            "result" if is_active => {
                if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                    let _ = tx.send((key.clone(), AssistantEvent::SessionId(sid.to_string())));
                }
                let _ = tx.send((key.clone(), AssistantEvent::TurnDone));
            }

            // During --resume replay: silently ignore content events
            "stream_event"
            | "assistant"
            | "content_block_start"
            | "content_block_delta"
            | "result" => {}

            _ => {}
        }
    }

    match child.wait() {
        Ok(status) if !status.success() => {
            let _ = tx.send((
                key.clone(),
                AssistantEvent::Error(format!("Claude process exited with {status}")),
            ));
        }
        Err(e) => {
            let _ = tx.send((
                key.clone(),
                AssistantEvent::Error(format!("Failed to wait on claude process: {e}")),
            ));
        }
        _ => {}
    }
    let _ = tx.send((key, AssistantEvent::ProcessExited));
}

/// Handle an inner streaming event (unwrapped from the stream_event wrapper).
/// Matches content_block_delta for text and content_block_start for tool_use.
fn handle_stream_event(
    event: &serde_json::Value,
    tx: &mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
    key: &SessionKey,
) {
    let inner_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match inner_type {
        "content_block_delta" => {
            if let Some(delta) = event.get("delta")
                && delta.get("type").and_then(|t| t.as_str()) == Some("text_delta")
                && let Some(text) = delta.get("text").and_then(|t| t.as_str())
            {
                let _ = tx.send((key.clone(), AssistantEvent::TextDelta(text.to_string())));
            }
        }
        "content_block_start" => {
            if let Some(cb) = event.get("content_block")
                && cb.get("type").and_then(|t| t.as_str()) == Some("tool_use")
            {
                let name = cb
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let _ = tx.send((key.clone(), AssistantEvent::ToolStart(name)));
            }
        }
        _ => {}
    }
}
