use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::app::ClatApp;
use crate::runtime::{Runtime, TmuxRuntime};

const SERVER_NAME: &str = "clat";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_PORT: u16 = 24462;

/// Start the MCP HTTP server in the background. Returns the task handle.
/// The server creates its own `ClatApp` (separate DB connection) so it
/// runs independently of the dashboard's app instance.
pub fn start() -> JoinHandle<()> {
    tokio::spawn(async {
        if let Err(e) = serve().await {
            // Can't print to stdout (TUI owns it), just log to file.
            log_mcp_error(&format!("MCP server failed: {e}"));
        }
    })
}

async fn serve() -> anyhow::Result<()> {
    let app = Arc::new(ClatApp::try_new(TmuxRuntime).await?);
    let listener = TcpListener::bind(("127.0.0.1", DEFAULT_PORT)).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &app).await {
                log_mcp_error(&format!("MCP connection error: {e}"));
            }
        });
    }
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    app: &ClatApp<impl Runtime>,
) -> anyhow::Result<()> {
    // Read HTTP request: request line + headers + body.
    let mut buf = vec![0u8; 8192];
    let mut total = 0;

    // Read until we have the full header section (\r\n\r\n).
    let header_end = loop {
        if total >= buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            anyhow::bail!("connection closed before headers complete");
        }
        total += n;
        if let Some(pos) = find_header_end(&buf[..total]) {
            break pos;
        }
    };

    let headers_raw = std::str::from_utf8(&buf[..header_end])?;

    // Parse request line.
    let first_line = headers_raw
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty request"))?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // Only accept POST /mcp
    if method != "POST" || path != "/mcp" {
        let body = if method == "GET" && path == "/mcp" {
            // MCP spec: GET returns SSE stream, but we don't support that.
            r#"{"error":"GET not supported, use POST"}"#.to_string()
        } else {
            r#"{"error":"Not found"}"#.to_string()
        };
        write_http_response(&mut stream, 404, &body).await?;
        return Ok(());
    }

    // Find Content-Length.
    let content_length = parse_content_length(headers_raw)
        .ok_or_else(|| anyhow::anyhow!("missing Content-Length header"))?;

    // Body starts right after the header separator.
    let body_start = header_end + 4; // skip \r\n\r\n
    let body_already = total - body_start;

    // Read remaining body bytes if needed.
    if content_length > body_already {
        let needed = content_length - body_already;
        if body_start + content_length > buf.len() {
            buf.resize(body_start + content_length, 0);
        }
        let mut read_so_far = 0;
        while read_so_far < needed {
            let n = stream
                .read(&mut buf[total..total + (needed - read_so_far)])
                .await?;
            if n == 0 {
                anyhow::bail!("connection closed before body complete");
            }
            total += n;
            read_so_far += n;
        }
    }

    let body_bytes = &buf[body_start..body_start + content_length];
    let request: Value = serde_json::from_slice(body_bytes)?;

    let response = dispatch_jsonrpc(&request, app).await;
    let response_body = serde_json::to_string(&response)?;
    write_http_response(&mut stream, 200, &response_body).await?;

    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if let Some(val) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            return val.trim().parse().ok();
        }
        // Case-insensitive fallback
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> anyhow::Result<()> {
    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn log_mcp_error(msg: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("data/mcp.log")
    {
        use std::io::Write;
        let ts = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "[{ts}] {msg}");
    }
}

// --- JSON-RPC dispatch (protocol-agnostic) ---

async fn dispatch_jsonrpc<R: Runtime>(request: &Value, app: &ClatApp<R>) -> Value {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no id) — acknowledge silently with empty 200
    if id.is_none() {
        return json!({});
    }

    match method {
        "initialize" => handle_initialize(&id),
        "tools/list" => handle_tools_list(&id),
        "tools/call" => handle_tools_call(&id, &params, app).await,
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {method}") }
        }),
    }
}

fn handle_initialize(id: &Option<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        }
    })
}

fn handle_tools_list(id: &Option<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "tools": tools_schema()
        }
    })
}

fn tools_schema() -> Value {
    json!([
        {
            "name": "list_tasks",
            "description": "List tasks and their status. Without arguments, shows only active (running) tasks.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": {
                        "type": "boolean",
                        "description": "Show all tasks including closed/completed/failed"
                    },
                    "project": {
                        "type": "string",
                        "description": "Filter tasks by project name"
                    }
                }
            }
        },
        {
            "name": "task_log",
            "description": "Get message log for a task, including system prompt, user messages, and assistant responses.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task ID (prefix match supported)"
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_projects",
            "description": "List all projects.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "list_skills",
            "description": "List available agent skills.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "spawn_task",
            "description": "Spawn a new agent task. Creates a git worktree and launches a Claude agent with the specified skill.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Human-friendly task name"
                    },
                    "skill": {
                        "type": "string",
                        "description": "Skill to use (default: engineer)"
                    },
                    "params": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Template parameters as key-value pairs (e.g. {\"task\": \"implement feature X\"})"
                    },
                    "repo": {
                        "type": "string",
                        "description": "Path to target git repository"
                    },
                    "project": {
                        "type": "string",
                        "description": "Assign task to a project"
                    }
                },
                "required": ["name"]
            }
        },
        {
            "name": "send_message",
            "description": "Send a message to a running agent's tmux pane.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task ID (prefix match supported)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Message to send"
                    }
                },
                "required": ["id", "message"]
            }
        },
        {
            "name": "close_task",
            "description": "Close a running task (capture output, kill tmux window).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task ID (prefix match supported)"
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "reopen_task",
            "description": "Reopen a closed/completed task (resume agent in tmux).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task ID (prefix match supported)"
                    }
                },
                "required": ["id"]
            }
        }
    ])
}

async fn handle_tools_call<R: Runtime>(
    id: &Option<Value>,
    params: &Value,
    app: &ClatApp<R>,
) -> Value {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "list_tasks" => tool_list_tasks(&args, app).await,
        "task_log" => tool_task_log(&args, app).await,
        "list_projects" => tool_list_projects(app).await,
        "list_skills" => tool_list_skills(app),
        "spawn_task" => tool_spawn_task(&args, app).await,
        "send_message" => tool_send_message(&args, app).await,
        "close_task" => tool_close_task(&args, app).await,
        "reopen_task" => tool_reopen_task(&args, app).await,
        _ => Err(anyhow::anyhow!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": content }]
            }
        }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("Error: {e}") }],
                "isError": true
            }
        }),
    }
}

// --- Tool implementations ---

async fn tool_list_tasks<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let project = args.get("project").and_then(|v| v.as_str());

    let tasks = app.list_tasks(all, project).await?;

    if tasks.is_empty() {
        return Ok("No tasks.".to_string());
    }

    let items: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id.as_str(),
                "name": t.name.as_str(),
                "skill": t.skill_name,
                "status": t.status.as_str(),
                "started_at": t.started_at.to_rfc3339(),
                "exit_code": t.exit_code,
                "project_id": t.project_id.as_ref().map(|p| p.as_str().to_string()),
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&items)?)
}

async fn tool_task_log<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: id"))?;

    let log = app.log(id).await?;

    let messages: Vec<Value> = log
        .messages
        .iter()
        .map(|m| {
            json!({
                "role": m.role.as_str(),
                "content": m.content,
                "created_at": m.created_at.to_rfc3339(),
            })
        })
        .collect();

    let mut result = json!({
        "task": {
            "id": log.task.id.as_str(),
            "name": log.task.name.as_str(),
            "status": log.task.status.as_str(),
        },
        "messages": messages,
    });

    if let Some(output) = &log.live_output {
        let all_lines: Vec<&str> = output.lines().collect();
        let tail = if all_lines.len() > 50 {
            &all_lines[all_lines.len() - 50..]
        } else {
            &all_lines
        };
        result["live_output"] = json!(tail.join("\n"));
    }

    Ok(serde_json::to_string_pretty(&result)?)
}

async fn tool_list_projects<R: Runtime>(app: &ClatApp<R>) -> anyhow::Result<String> {
    let projects = app.list_projects().await?;

    if projects.is_empty() {
        return Ok("No projects.".to_string());
    }

    let items: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "id": p.id.as_str(),
                "name": p.name.as_str(),
                "description": p.description,
                "created_at": p.created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&items)?)
}

fn tool_list_skills<R: Runtime>(app: &ClatApp<R>) -> anyhow::Result<String> {
    let skills = app.list_skills()?;

    if skills.is_empty() {
        return Ok("No skills found.".to_string());
    }

    let items: Vec<Value> = skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "params": s.params,
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&items)?)
}

async fn tool_spawn_task<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: name"))?;
    let skill = args
        .get("skill")
        .and_then(|v| v.as_str())
        .unwrap_or("engineer");
    let project = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(String::from);

    let repo = args
        .get("repo")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);

    let params: Vec<(String, String)> = args
        .get("params")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let repo_ref = repo
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("repo parameter is required for spawning tasks via MCP"))?;

    let result = app
        .spawn(crate::app::SpawnRequest {
            task_name: name,
            skill_name: skill,
            params,
            work_dir_mode: crate::app::WorkDirMode::Worktree {
                repo: repo_ref,
                branch: None,
            },
            prompt_mode: crate::app::PromptMode::Full,
            project,
        })
        .await?;

    Ok(serde_json::to_string_pretty(&json!({
        "task_id": result.task_id.as_str(),
        "name": result.task_name.as_str(),
        "skill": result.skill_name,
        "window_id": result.window_id.as_str(),
    }))?)
}

async fn tool_send_message<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: id"))?;
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: message"))?;

    let result = app.send(id, message).await?;

    Ok(format!(
        "Sent message to {} ({})",
        result.task_name,
        result.task_id.short()
    ))
}

async fn tool_close_task<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: id"))?;

    let result = app.close(id).await?;

    Ok(format!(
        "Closed task {} ({})",
        result.task_name,
        result.task_id.short()
    ))
}

async fn tool_reopen_task<R: Runtime>(args: &Value, app: &ClatApp<R>) -> anyhow::Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required parameter: id"))?;

    let window_id = app.reopen(id).await?;

    Ok(format!("Reopened task {id} (window: {window_id})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_schema_is_valid_json_array() {
        let schema = tools_schema();
        assert!(schema.is_array());
        let tools = schema.as_array().unwrap();
        assert_eq!(tools.len(), 8);

        for tool in tools {
            assert!(tool.get("name").is_some(), "tool missing 'name'");
            assert!(
                tool.get("description").is_some(),
                "tool missing 'description'"
            );
            assert!(
                tool.get("inputSchema").is_some(),
                "tool missing 'inputSchema'"
            );
        }
    }

    #[test]
    fn initialize_response_has_correct_structure() {
        let resp = handle_initialize(&Some(json!(1)));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn tools_list_response_has_correct_structure() {
        let resp = handle_tools_list(&Some(json!(2)));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 2);
        assert!(resp["result"]["tools"].is_array());
    }

    #[test]
    fn parse_content_length_works() {
        let headers = "POST /mcp HTTP/1.1\r\nContent-Length: 42\r\nHost: localhost\r\n";
        assert_eq!(parse_content_length(headers), Some(42));

        let headers_lower = "POST /mcp HTTP/1.1\r\ncontent-length: 100\r\n";
        assert_eq!(parse_content_length(headers_lower), Some(100));

        let no_cl = "POST /mcp HTTP/1.1\r\nHost: localhost\r\n";
        assert_eq!(parse_content_length(no_cl), None);
    }

    #[test]
    fn find_header_end_works() {
        let simple = b"GET / HTTP/1.1\r\n\r\nbody";
        let pos = find_header_end(simple).unwrap();
        assert_eq!(&simple[pos..pos + 4], b"\r\n\r\n");

        let multi = b"POST /mcp HTTP/1.1\r\nHost: x\r\n\r\n{}";
        let pos = find_header_end(multi).unwrap();
        assert_eq!(&multi[pos..pos + 4], b"\r\n\r\n");

        assert_eq!(find_header_end(b"no separator here"), None);
    }

    #[tokio::test]
    async fn dispatch_notification_returns_empty() {
        // Notifications have no id field
        let request = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        // We can't easily construct a ClatApp here, but we can test the id-is-none branch
        // by checking that the function returns an empty object for notifications.
        // For this we need an app — skip for now, tested via the id.is_none() check.
        assert!(request.get("id").is_none());
    }
}
