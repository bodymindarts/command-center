use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

const EXO_SYSTEM_PROMPT: &str = "\
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

pub enum ExoEvent {
    TextDelta(String),
    ToolStart(String),
    SessionId(String),
    TurnDone,
    ProcessExited,
    Error(String),
}

/// Persistent ExO claude session. Keeps the process alive across messages
/// so subsequent sends skip process startup latency entirely.
pub struct ExoSession {
    stdin: Option<ChildStdin>,
    cancel: Arc<AtomicBool>,
    /// When true, the reader thread forwards content events. Set to false
    /// during pre-spawn warmup so replayed history is ignored.
    active: Arc<AtomicBool>,
    tx: mpsc::Sender<ExoEvent>,
    session_id: Option<String>,
}

impl ExoSession {
    /// Start a new persistent claude process and background reader thread.
    pub fn start(
        session_id: Option<&str>,
        cancel: Arc<AtomicBool>,
        tx: mpsc::Sender<ExoEvent>,
    ) -> Self {
        let mut session = ExoSession {
            stdin: None,
            cancel,
            active: Arc::new(AtomicBool::new(true)),
            tx,
            session_id: session_id.map(|s| s.to_string()),
        };
        session.spawn_process();
        session
    }

    fn spawn_process(&mut self) {
        let mut args = vec![
            "-p".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--allowedTools".to_string(),
            "Read,Grep,Glob,Bash,Edit,Write".to_string(),
            "--append-system-prompt".to_string(),
            EXO_SYSTEM_PROMPT.to_string(),
        ];

        if let Some(ref sid) = self.session_id {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }

        let mut child = match Command::new("claude")
            .args(&args)
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
    /// If the process has exited, spawns a new one first.
    pub fn send_message(&mut self, message: &str, session_id: Option<&str>) {
        // Update session_id if provided
        if let Some(sid) = session_id {
            self.session_id = Some(sid.to_string());
        }

        // Respawn if process is gone
        if self.stdin.is_none() {
            self.spawn_process();
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

    /// Pre-spawn the process if it's not running.
    /// Called after a clean exit so the process is warm for the next message.
    /// The reader thread ignores content events until send_message activates it.
    pub fn ensure_alive(&mut self) {
        if self.stdin.is_none() {
            self.active.store(false, Ordering::Relaxed);
            self.spawn_process();
        }
    }
}

/// Background reader: parses stream-json stdout and sends ExoEvents.
/// Content events (text, tools, turn-done) are only forwarded when `active`
/// is true, so pre-spawn warmup / session replay is silently discarded.
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
            // Session metadata — always forwarded (even during warmup)
            "system" => {
                if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                    let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                }
            }
            // Content events — only forwarded when active (not during pre-spawn warmup)
            "assistant" if is_active => {
                if let Some(content) = parsed
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    let _ = tx.send(ExoEvent::TextDelta(text.to_string()));
                                }
                            }
                            Some("tool_use") => {
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("tool")
                                    .to_string();
                                let _ = tx.send(ExoEvent::ToolStart(name));
                            }
                            _ => {}
                        }
                    }
                }
            }
            "content_block_start" if is_active => {
                if let Some(cb) = parsed.get("content_block")
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
            "content_block_delta" if is_active => {
                if let Some(delta) = parsed.get("delta")
                    && delta.get("type").and_then(|t| t.as_str()) == Some("text_delta")
                    && let Some(text) = delta.get("text").and_then(|t| t.as_str())
                {
                    let _ = tx.send(ExoEvent::TextDelta(text.to_string()));
                }
            }
            "result" if is_active => {
                if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                    let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                }
                let _ = tx.send(ExoEvent::TurnDone);
            }
            // During warmup: silently ignore replayed content events
            "assistant" | "content_block_start" | "content_block_delta" | "result" => {}
            other => {
                if !other.is_empty() {
                    let debug_path = std::env::temp_dir().join("cc-exo-debug.jsonl");
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&debug_path)
                    {
                        let _ = writeln!(f, "{}", line);
                    }
                }
            }
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
