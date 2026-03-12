use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::app::ClatApp;
use crate::runtime::Runtime;

const SERVER_NAME: &str = "clat";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP server over stdio, reading JSON-RPC requests from stdin and
/// writing responses to stdout.
pub async fn run<R: Runtime>(app: ClatApp<R>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                write_response(&mut stdout, &err_resp).await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Notifications (no id) — acknowledge silently
        if id.is_none() {
            // MCP notifications like notifications/initialized — no response needed
            continue;
        }

        let response = match method {
            "initialize" => handle_initialize(&id),
            "tools/list" => handle_tools_list(&id),
            "tools/call" => handle_tools_call(&id, &params, &app).await,
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {method}") }
            }),
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

async fn write_response(stdout: &mut tokio::io::Stdout, response: &Value) -> anyhow::Result<()> {
    let mut buf = serde_json::to_vec(response)?;
    buf.push(b'\n');
    stdout.write_all(&buf).await?;
    stdout.flush().await?;
    Ok(())
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
        // Truncate to last 50 lines like the CLI does
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

        // Verify each tool has required fields
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
}
