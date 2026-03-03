use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

pub const EXO_SYSTEM_PROMPT: &str = "\
You are ExO, the executive orchestrator of a multi-agent command center. \
You are a strategic co-pilot — you deliberate, clarify, and plan before acting.

## Role
You think alongside the user. When something is unclear, ask questions and discuss. \
When the path is clear, delegate execution to worker agents — never do the implementation work yourself.

## Deliberate, then delegate
- If the request is ambiguous or underspecified, talk it through with the user. \
Propose an approach, surface trade-offs, ask clarifying questions.
- Once the what and how are clear (either because the user was specific, or you've discussed it), \
spawn a task immediately. Don't sit on a clear request — delegate it.
- Never explore the codebase to build context for a task you're about to spawn. \
Instead, put investigation instructions in the task description and let the agent do it.

## Spawning tasks
```
clat spawn \"<short-task-name>\" -p task=\"<clear description of what to do>\"
```
Each task runs in its own worktree with an engineer agent. You can spawn multiple tasks in parallel. \
The task description should be self-contained — the agent won't see this conversation. \
If the agent needs to find or understand code, say so in the description rather than looking it up yourself.

## What you do yourself
- Answering the user's direct questions about the codebase (reading code when they ask)
- Checking task status (`clat list`)
- Discussing architecture, trade-offs, and priorities
- Anything the user explicitly asks you to do directly";

pub const PM_SYSTEM_PROMPT: &str = "\
You are PM, the project manager for this project. \
You help the user organize, plan, and coordinate work within the project scope.

## Role
You discuss project goals, break down work into actionable tasks, track progress, \
and help the user make decisions about priorities and approach.

## What you do
- Discuss project scope, goals, and priorities
- Break down features into clear, actionable tasks
- Suggest approaches and surface trade-offs
- Track what's been done and what remains
- Help estimate effort and sequence work

## Spawning tasks
When the user wants to execute work, spawn tasks using:
```
clat spawn \"<short-task-name>\" -p task=\"<clear description of what to do>\"
```
Each task runs in its own worktree with an engineer agent. \
The task description should be self-contained — include all context the agent needs.";

pub enum ExoEvent {
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
pub struct ExoSession {
    stdin: Option<ChildStdin>,
    cancel: Arc<AtomicBool>,
    /// When true, the reader thread forwards content events. Set to false
    /// during --resume startup so replayed history is ignored.
    active: Arc<AtomicBool>,
    tx: mpsc::Sender<ExoEvent>,
    session_id: Option<String>,
    system_prompt: String,
}

impl ExoSession {
    /// Start a new persistent claude process and background reader thread.
    pub fn start(
        session_id: Option<&str>,
        cancel: Arc<AtomicBool>,
        tx: mpsc::Sender<ExoEvent>,
        system_prompt: &str,
    ) -> Self {
        let mut session = ExoSession {
            stdin: None,
            cancel,
            active: Arc::new(AtomicBool::new(false)),
            tx,
            session_id: session_id.map(|s| s.to_string()),
            system_prompt: system_prompt.to_string(),
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
            "--allowedTools".to_string(),
            "Read,Grep,Glob,Bash,Edit,Write".to_string(),
            "--append-system-prompt".to_string(),
            self.system_prompt.clone(),
        ];

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
                let _ = self
                    .tx
                    .send(ExoEvent::Error(format!("Failed to spawn claude: {e}")));
                return;
            }
        };

        self.stdin = child.stdin.take();
        if self.stdin.is_none() {
            let _ = self
                .tx
                .send(ExoEvent::Error("Failed to open stdin pipe".into()));
            let _ = child.kill();
            return;
        }

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = self
                    .tx
                    .send(ExoEvent::Error("Failed to open stdout pipe".into()));
                let _ = child.kill();
                self.stdin = None;
                return;
            }
        };

        let tx = self.tx.clone();
        let cancel = Arc::clone(&self.cancel);
        let active = Arc::clone(&self.active);
        thread::spawn(move || {
            read_stdout(child, stdout, tx, cancel, active);
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

        // Mark active so the reader thread forwards events for this turn
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
            let _ = self.tx.send(ExoEvent::Error(format!(
                "Failed to write to claude stdin: {e}"
            )));
            self.stdin = None;
            return;
        }
        if let Err(e) = stdin.flush() {
            let _ = self.tx.send(ExoEvent::Error(format!(
                "Failed to flush claude stdin: {e}"
            )));
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

/// Background reader: parses stream-json stdout and sends ExoEvents.
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
    tx: mpsc::Sender<ExoEvent>,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
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
                let _ = tx.send(ExoEvent::Error(format!("Error reading claude output: {e}")));
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
                    let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                }
            }

            // Streaming events from --include-partial-messages
            // Wrapped as: {"type": "stream_event", "event": {<API event>}}
            "stream_event" if is_active => {
                if let Some(inner) = parsed.get("event") {
                    handle_stream_event(inner, &tx);
                }
            }

            // Result event — signals turn completion
            "result" if is_active => {
                if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                    let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                }
                let _ = tx.send(ExoEvent::TurnDone);
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
            let _ = tx.send(ExoEvent::Error(format!(
                "Claude process exited with {status}"
            )));
        }
        Err(e) => {
            let _ = tx.send(ExoEvent::Error(format!(
                "Failed to wait on claude process: {e}"
            )));
        }
        _ => {}
    }
    let _ = tx.send(ExoEvent::ProcessExited);
}

/// Handle an inner streaming event (unwrapped from the stream_event wrapper).
/// Matches content_block_delta for text and content_block_start for tool_use.
fn handle_stream_event(event: &serde_json::Value, tx: &mpsc::Sender<ExoEvent>) {
    let inner_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match inner_type {
        "content_block_delta" => {
            if let Some(delta) = event.get("delta")
                && delta.get("type").and_then(|t| t.as_str()) == Some("text_delta")
                && let Some(text) = delta.get("text").and_then(|t| t.as_str())
            {
                let _ = tx.send(ExoEvent::TextDelta(text.to_string()));
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
                let _ = tx.send(ExoEvent::ToolStart(name));
            }
        }
        _ => {}
    }
}
