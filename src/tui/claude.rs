use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

pub enum ExoEvent {
    TextDelta(String),
    ToolStart(String),
    SessionId(String),
    Done,
    Error(String),
}

pub fn spawn_claude(
    message: &str,
    session_id: Option<&str>,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<ExoEvent>,
) {
    let session_id = session_id.map(|s| s.to_string());
    let message = message.to_string();

    let mut args = vec![
        "-p".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--allowedTools".to_string(),
        "Read,Grep,Glob,Bash,Edit,Write".to_string(),
    ];

    if let Some(ref sid) = session_id {
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
            let _ = tx.send(ExoEvent::Error(format!("Failed to spawn claude: {e}")));
            return;
        }
    };

    // Send initial user message via stream-json protocol, then close stdin
    // so the process knows no more messages are coming.
    {
        let mut stdin = child.stdin.take().unwrap();
        let sid = session_id.as_deref().unwrap_or("default");
        let msg_json = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": message,
            },
            "session_id": sid,
            "parent_tool_use_id": null,
        });
        let _ = writeln!(stdin, "{}", msg_json);
        let _ = stdin.flush();
    }

    let stdout = child.stdout.take().unwrap();

    thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);

        for line in reader.lines() {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                return;
            }

            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };

            if line.is_empty() {
                continue;
            }

            let parsed: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match event_type {
                "system" => {
                    if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                        let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                    }
                }
                "assistant" => {
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
                "result" => {
                    if let Some(sid) = parsed.get("session_id").and_then(|s| s.as_str()) {
                        let _ = tx.send(ExoEvent::SessionId(sid.to_string()));
                    }
                }
                other => {
                    // Log unknown events for protocol discovery
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

        let _ = child.wait();
        let _ = tx.send(ExoEvent::Done);
    });
}
