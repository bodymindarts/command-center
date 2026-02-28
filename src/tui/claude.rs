use std::io::BufRead;
use std::process::Command;
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
    let message = message.to_string();
    let session_id = session_id.map(|s| s.to_string());

    thread::spawn(move || {
        let mut args = vec![
            "-p".to_string(),
            message,
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        if let Some(ref sid) = session_id {
            args.push("--resume".to_string());
            args.push(sid.clone());
        }

        let child = Command::new("claude")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ExoEvent::Error(format!("Failed to spawn claude: {e}")));
                return;
            }
        };

        let stdout = child.stdout.take().unwrap();
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
                _ => {}
            }
        }

        let _ = child.wait();
        let _ = tx.send(ExoEvent::Done);
    });
}
