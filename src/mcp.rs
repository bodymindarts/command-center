use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::app::{AgentSendOutput, ClatApp, PromptMode, SpawnRequest, WorkDirMode};
use crate::jwt::JwtSigner;
use crate::runtime::Runtime;

use agent_memory::memory::NewMemory;

// ---------------------------------------------------------------------------
// MCP server — generic over R: Runtime
// ---------------------------------------------------------------------------

/// MCP server that exposes clat commands as tools.
///
/// Runs inside the dashboard process and serves spawned agents
/// over HTTP on localhost.
#[derive(Clone)]
pub struct ClatMcpServer<R: Runtime> {
    app: Arc<ClatApp<R>>,
    tool_router: ToolRouter<Self>,
}

impl<R: Runtime> ClatMcpServer<R> {
    pub fn new(app: Arc<ClatApp<R>>) -> Self {
        let mut router = Self::tool_router();
        router.add_route(Self::create_watch_route(Arc::clone(&app)));
        router.add_route(Self::cancel_watch_route(Arc::clone(&app)));
        router.add_route(Self::list_watches_route(Arc::clone(&app)));
        router.add_route(Self::send_message_route(Arc::clone(&app)));
        router.add_route(Self::list_tasks_route(Arc::clone(&app)));
        router.add_route(Self::task_log_route(Arc::clone(&app)));
        router.add_route(Self::store_memory_route(Arc::clone(&app)));
        router.add_route(Self::search_memory_route(Arc::clone(&app)));
        router.add_route(Self::list_memories_route(Arc::clone(&app)));
        Self {
            app,
            tool_router: router,
        }
    }

    fn create_watch_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["label", "check", "name"],
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Short description of what you're watching for (included in the notification)"
                },
                "name": {
                    "type": "string",
                    "description": "Uniquely identifies the watch. Creating a watch with an existing name reschedules it. Use the same name in polling loops to prevent watch stacking."
                },
                "check": {
                    "oneOf": [
                        {
                            "type": "object",
                            "title": "timer",
                            "description": "Fire a notification after a delay",
                            "required": ["name"],
                            "properties": {
                                "name": { "const": "timer" }
                            },
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "title": "command",
                            "description": "Run a command after the delay and include its output in the notification",
                            "required": ["name", "cmd"],
                            "properties": {
                                "name": { "const": "command" },
                                "cmd": {
                                    "type": "string",
                                    "description": "Shell command to execute. Only allowed CLIs: gh, curl, git. Example: 'gh run view 123 --json status'"
                                }
                            },
                            "additionalProperties": false
                        }
                    ]
                },
                "delay_seconds": {
                    "type": "integer",
                    "minimum": 5,
                    "description": "Seconds to wait before firing the notification"
                },
                "context": {
                    "description": "Arbitrary JSON echoed back in the notification. Use to persist state across sleep cycles.",
                    "type": "object"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "create_watch",
            "Set a background watch that fires after a delay. Uses zero LLM tokens while waiting. Returns immediately with a watch ID. Use this to implement polling loops: set a watch, stop, and when the notification arrives, check status and decide whether to watch again or finish.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    // Extract task_id from JWT claims via request extensions.
                    let task_id_str = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .map(|c| c.sub.as_str())
                        .unwrap_or("unknown");
                    let task_id: crate::primitives::TaskId = match task_id_str.parse() {
                        Ok(id) => id,
                        Err(_) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Invalid task_id: {task_id_str}"
                            ))]));
                        }
                    };

                    let args = ctx.arguments.unwrap_or_default();
                    let label = args
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed watch");
                    let delay = args
                        .get("delay_seconds")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(30);
                    let context = args.get("context").cloned();
                    let watch_name = match args.get("name").and_then(|v| v.as_str()) {
                        Some(n) => n,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'name'",
                            )]));
                        }
                    };

                    let check_name = args
                        .get("check")
                        .and_then(|v| v.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("timer");

                    let result = match check_name {
                        "command" => {
                            let cmd = match args
                                .get("check")
                                .and_then(|v| v.get("cmd"))
                                .and_then(|v| v.as_str())
                            {
                                Some(cmd) => cmd,
                                None => {
                                    return Ok(CallToolResult::error(vec![Content::text(
                                        "Missing required field 'cmd' in command check",
                                    )]));
                                }
                            };
                            app.watch()
                                .create_command(task_id, label, cmd, delay, context, watch_name)
                                .await
                        }
                        _ => {
                            app.watch()
                                .create_timer(task_id, label, delay, context, watch_name)
                                .await
                        }
                    };

                    match result {
                        Ok(watch_id) => {
                            let response = serde_json::json!({
                                "watch_id": watch_id,
                                "status": "active",
                                "fires_in_seconds": delay,
                            });
                            Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string_pretty(&response).unwrap(),
                            )]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to create watch: {e}"
                        ))])),
                    }
                })
            },
        )
    }
    fn cancel_watch_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["watch_id"],
            "properties": {
                "watch_id": {
                    "type": "string",
                    "description": "The watch ID (or prefix) to cancel"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "cancel_watch",
            "Cancel an active watch. No notification will be sent.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let task_id_str = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .map(|c| c.sub.as_str())
                        .unwrap_or("unknown");
                    let task_id: crate::primitives::TaskId = match task_id_str.parse() {
                        Ok(id) => id,
                        Err(_) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Invalid task_id: {task_id_str}"
                            ))]));
                        }
                    };

                    let args = ctx.arguments.unwrap_or_default();
                    let watch_id = match args.get("watch_id").and_then(|v| v.as_str()) {
                        Some(id) => id,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'watch_id'",
                            )]));
                        }
                    };

                    match app.watch().cancel_watch(task_id, watch_id).await {
                        Ok(()) => {
                            let response = serde_json::json!({
                                "status": "cancelled",
                                "watch_id": watch_id,
                            });
                            Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string_pretty(&response).unwrap(),
                            )]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to cancel watch: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn list_watches_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "list_watches",
            "List all active watches for your task. Returns watch IDs, names, labels, and scheduled fire times.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let task_id_str = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .map(|c| c.sub.as_str())
                        .unwrap_or("unknown");
                    let task_id: crate::primitives::TaskId = match task_id_str.parse() {
                        Ok(id) => id,
                        Err(_) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Invalid task_id: {task_id_str}"
                            ))]));
                        }
                    };

                    match app.watch().list_watches(task_id).await {
                        Ok(watches) => Ok(CallToolResult::success(vec![Content::text(
                            serde_json::to_string_pretty(&watches).unwrap(),
                        )])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to list watches: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn send_message_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["target", "message"],
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Task ID (or prefix) of the target task to send the message to. Use 'pm' to send to the PM/ExO session."
                },
                "message": {
                    "type": "string",
                    "description": "The message to send to the target task"
                }
            }
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "send_message",
            "Send a message to another running task or to the PM/ExO session. Messages to tasks are delivered to their tmux pane. Target 'pm' routes to the project PM chat if the caller has a project, or to the ExO chat if it doesn't. Task targets require the caller and target to be in the same project.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let claims = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .cloned();
                    let claims = match claims {
                        Some(c) => c,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Authentication required: no JWT claims found",
                            )]));
                        }
                    };

                    let args = ctx.arguments.unwrap_or_default();
                    let target = match args.get("target").and_then(|v| v.as_str()) {
                        Some(t) => t,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'target'",
                            )]));
                        }
                    };
                    let message = match args.get("message").and_then(|v| v.as_str()) {
                        Some(m) => m,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'message'",
                            )]));
                        }
                    };

                    match app.send_from_agent(&claims, target, message).await {
                        Ok(AgentSendOutput::Pm { project }) => {
                            let response = serde_json::json!({
                                "status": "delivered",
                                "target": "pm",
                                "project": project,
                                "note": "Message delivered to PM session."
                            });
                            Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string_pretty(&response).unwrap(),
                            )]))
                        }
                        Ok(AgentSendOutput::Exo) => {
                            let response = serde_json::json!({
                                "status": "delivered",
                                "target": "exo",
                                "note": "Message delivered to ExO chat."
                            });
                            Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string_pretty(&response).unwrap(),
                            )]))
                        }
                        Ok(AgentSendOutput::Task(output)) => {
                            let response = serde_json::json!({
                                "status": "sent",
                                "target_task_id": output.task_id.to_string(),
                                "target_task_name": output.task_name.as_str(),
                            });
                            Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string_pretty(&response).unwrap(),
                            )]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to send message: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn list_tasks_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": ["string", "null"],
                    "description": "Filter tasks by project name. If omitted, lists all active tasks."
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "list_tasks",
            "List active tasks. Optionally filter by project name. Returns task id, name, skill, status, and activity.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    // Prefer project from JWT claims, fall back to explicit parameter.
                    let claims = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .cloned();
                    let args = ctx.arguments.unwrap_or_default();
                    let explicit_project = args.get("project").and_then(|v| v.as_str());
                    let project = claims
                        .as_ref()
                        .and_then(|c| c.project.as_deref())
                        .or(explicit_project);

                    let tasks = match app.list_tasks(false, project).await {
                        Ok(t) => t,
                        Err(e) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Failed to list tasks: {e}"
                            ))]));
                        }
                    };

                    // Determine idle/active status for running tasks.
                    let running_pane_ids: Vec<crate::primitives::PaneId> = tasks
                        .iter()
                        .filter(|t| t.status.is_running())
                        .filter_map(|t| t.tmux_pane.clone())
                        .collect();
                    let pane_refs: Vec<&crate::primitives::PaneId> =
                        running_pane_ids.iter().collect();
                    let idle = crate::runtime::idle_panes(&pane_refs);

                    let items: Vec<serde_json::Value> = tasks
                        .iter()
                        .map(|t| {
                            let activity = if !t.status.is_running() {
                                "-"
                            } else if let Some(ref pane) = t.tmux_pane {
                                if idle.contains(pane) {
                                    "idle"
                                } else {
                                    "active"
                                }
                            } else {
                                "-"
                            };
                            serde_json::json!({
                                "id": t.id.short(),
                                "name": t.name.as_str(),
                                "skill": t.skill_name,
                                "status": t.status.to_string(),
                                "activity": activity,
                            })
                        })
                        .collect();

                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&items).unwrap(),
                    )]))
                })
            },
        )
    }

    fn store_memory_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["title", "content"],
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Descriptive title for the memory"
                },
                "content": {
                    "type": "string",
                    "description": "Full content (findings, decisions, patterns, etc.)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Lowercase tags for categorization"
                },
                "project": {
                    "type": ["string", "null"],
                    "description": "Project scope for this memory"
                },
                "memory_type": {
                    "type": ["string", "null"],
                    "description": "'memory' (default) or 'report'"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "store_memory",
            "Store a research finding, decision, or piece of knowledge for future agents. Always store important findings before completing a task.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let claims = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .cloned();
                    let task_id_str = claims.as_ref().map(|c| c.sub.as_str());

                    let args = ctx.arguments.unwrap_or_default();
                    let title = match args.get("title").and_then(|v| v.as_str()) {
                        Some(t) => t.to_string(),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'title'",
                            )]));
                        }
                    };
                    let content = match args.get("content").and_then(|v| v.as_str()) {
                        Some(c) => c.to_string(),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'content'",
                            )]));
                        }
                    };
                    let tags: Vec<String> = args
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let project = args
                        .get("project")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| claims.as_ref().and_then(|c| c.project.clone()));
                    // Treat memory_type=report as persistent for backwards compat.
                    let persistent =
                        args.get("memory_type").and_then(|v| v.as_str()) == Some("report");
                    let source_task = task_id_str.map(|s| s.to_string());

                    let new = NewMemory {
                        title: title.clone(),
                        content,
                        tags: tags.clone(),
                        project,
                        source_task,
                        source_type: "agent".to_string(),
                        persistent,
                    };
                    match app.memory().store(new).await {
                        Ok(memory) => {
                            let short_id = &memory.id[..8.min(memory.id.len())];
                            let tags_display = if tags.is_empty() {
                                String::from("(none)")
                            } else {
                                tags.join(", ")
                            };
                            let label = if memory.persistent {
                                "persistent memory"
                            } else {
                                "memory"
                            };
                            let text = format!(
                                "Stored {label}: \"{}\" (id: {})\nTags: {}",
                                title, short_id, tags_display
                            );
                            Ok(CallToolResult::success(vec![Content::text(text)]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to store memory: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn search_memory_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (keywords or natural language)"
                },
                "project": {
                    "type": ["string", "null"],
                    "description": "Filter results by project"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of results to return (default: 10)"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "search_memory",
            "Search stored memories and research reports. Always search before starting research — someone may have already investigated your topic.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let args = ctx.arguments.unwrap_or_default();
                    let query = match args.get("query").and_then(|v| v.as_str()) {
                        Some(q) => q,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'query'",
                            )]));
                        }
                    };
                    let project = args.get("project").and_then(|v| v.as_str());
                    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

                    match app.memory().search(query, project, limit).await {
                        Ok(results) => {
                            if results.is_empty() {
                                return Ok(CallToolResult::success(vec![Content::text(
                                    "No results found.",
                                )]));
                            }

                            let mut text = format!("## Results ({} found)\n", results.len());

                            for (i, r) in results.iter().enumerate() {
                                text.push_str(&format!(
                                    "\n### {}. {} (score: {:.2}, decay: {:.0}%",
                                    i + 1,
                                    r.title,
                                    r.score,
                                    r.decay_factor * 100.0,
                                ));
                                if r.persistent {
                                    text.push_str(", persistent");
                                }
                                if r.pinned {
                                    text.push_str(", pinned");
                                }
                                text.push(')');

                                if !r.tags.is_empty() {
                                    text.push_str(&format!("\nTags: {}", r.tags.join(", ")));
                                }
                                if let Some(ref proj) = r.project {
                                    text.push_str(&format!("\nProject: {proj}"));
                                }

                                // Content snippet (first 300 chars).
                                let snippet: String = r.content.chars().take(300).collect();
                                text.push_str(&format!("\n\n{snippet}"));
                                if r.content.len() > 300 {
                                    text.push_str("...");
                                }
                                text.push_str("\n\n---");
                            }

                            Ok(CallToolResult::success(vec![Content::text(text)]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Search failed: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn list_memories_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": ["string", "null"],
                    "description": "Filter by project"
                },
                "memory_type": {
                    "type": ["string", "null"],
                    "description": "Filter by type: 'memory' or 'report'"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of results to return (default: 20)"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "list_memories",
            "List stored memories and research reports, optionally filtered by project or type.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    let args = ctx.arguments.unwrap_or_default();
                    let project = args.get("project").and_then(|v| v.as_str());
                    // Treat memory_type=report as persistent filter for backwards compat.
                    let memory_type = args.get("memory_type").and_then(|v| v.as_str());
                    let persistent_filter = match memory_type {
                        Some("report") => Some(true),
                        Some("memory") => Some(false),
                        _ => None,
                    };
                    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

                    match app.memory().list(project, persistent_filter, limit).await {
                        Ok(memories) => {
                            if memories.is_empty() {
                                return Ok(CallToolResult::success(vec![Content::text(
                                    "No memories found.",
                                )]));
                            }

                            let mut text = String::new();
                            for m in &memories {
                                let short_id = &m.id[..8.min(m.id.len())];
                                let tags_display = if m.tags.is_empty() {
                                    String::new()
                                } else {
                                    format!("\n  Tags: {}", m.tags.join(", "))
                                };
                                let project_display = m
                                    .project
                                    .as_deref()
                                    .map(|p| format!("\n  Project: {p}"))
                                    .unwrap_or_default();
                                let flags = if m.persistent { " [persistent]" } else { "" };
                                text.push_str(&format!(
                                    "- [{}] **{}** (created: {}{}){}{}\n",
                                    short_id,
                                    m.title,
                                    m.created_at.format("%Y-%m-%d"),
                                    flags,
                                    tags_display,
                                    project_display,
                                ));
                            }

                            let header = format!("## Memories ({} found)\n\n", memories.len());
                            Ok(CallToolResult::success(vec![Content::text(format!(
                                "{header}{text}"
                            ))]))
                        }
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                            "Failed to list memories: {e}"
                        ))])),
                    }
                })
            },
        )
    }

    fn task_log_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task ID or prefix to look up"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of messages to return (default: 20)"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "task_log",
            "Show the message log for a task. Returns recent messages with role, content, and timestamp.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    // Extract caller's project from JWT claims for scoping.
                    let claims = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .cloned();

                    let args = ctx.arguments.unwrap_or_default();
                    let task_id = match args.get("task_id").and_then(|v| v.as_str()) {
                        Some(id) => id,
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'task_id'",
                            )]));
                        }
                    };
                    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

                    let log = match app.log(task_id).await {
                        Ok(l) => l,
                        Err(e) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Failed to fetch task log: {e}"
                            ))]));
                        }
                    };

                    // Enforce project scoping: if caller has a project, the
                    // target task must belong to the same project.
                    if let Some(ref claims) = claims
                        && let Some(ref caller_project) = claims.project
                    {
                        let caller_project_id = match app.resolve_project_id(caller_project).await {
                            Ok(id) => Some(id),
                            Err(_) => caller_project.parse::<crate::primitives::ProjectId>().ok(),
                        };
                        if caller_project_id.is_some() && log.task.project_id != caller_project_id {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Access denied: task does not belong to your project",
                            )]));
                        }
                    }

                    // Take only the last `limit` messages.
                    let messages = if log.messages.len() > limit {
                        &log.messages[log.messages.len() - limit..]
                    } else {
                        &log.messages
                    };

                    let items: Vec<serde_json::Value> = messages
                        .iter()
                        .map(|m| {
                            serde_json::json!({
                                "role": m.role.as_str(),
                                "content": m.content,
                                "timestamp": m.created_at.to_rfc3339(),
                            })
                        })
                        .collect();

                    let response = serde_json::json!({
                        "task_id": task_id,
                        "task_name": log.task.name.as_str(),
                        "message_count": items.len(),
                        "messages": items,
                    });

                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&response).unwrap(),
                    )]))
                })
            },
        )
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpawnParams {
    /// Name for the task (used as branch name and identifier)
    name: String,
    /// The task description / prompt to send to the agent
    task: String,
    /// Skill to use (default: engineer). Options: engineer, researcher, reviewer
    skill: Option<String>,
    /// Project to assign the task to
    project: Option<String>,
    /// Path to the git repo (defaults to command-center)
    repo: Option<String>,
    /// Existing git branch to check out in the worktree
    branch: Option<String>,
    /// If true, create a scratch directory instead of a git worktree
    scratch: Option<bool>,
}

#[tool_router]
impl<R: Runtime> ClatMcpServer<R> {
    #[tool(
        description = "Spawn a new task agent. Creates a git worktree, loads the skill template, and launches a Claude Code session."
    )]
    async fn clat_spawn(
        &self,
        Extension(parts): Extension<axum::http::request::Parts>,
        Parameters(params): Parameters<SpawnParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let caller_claims = parts.extensions.get::<crate::jwt::AgentClaims>();

        // Priority: explicit param > JWT project claim > breadcrumb fallback
        let project = params
            .project
            .or_else(|| caller_claims.and_then(|c| c.project.clone()));

        let skill_name = params.skill.as_deref().unwrap_or("engineer");
        let kv_params = vec![("task".to_string(), params.task)];

        let repo_path;
        let scratch = params.scratch.unwrap_or(false);
        let (work_dir_mode, prompt_mode) = if scratch {
            (WorkDirMode::Scratch, PromptMode::Full)
        } else {
            repo_path = params
                .repo
                .as_deref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| self.app.project_root().to_path_buf());
            (
                WorkDirMode::Worktree {
                    repo: &repo_path,
                    branch: params.branch.as_deref(),
                },
                PromptMode::Full,
            )
        };

        // Inherit project from parent task breadcrumb if not explicitly set.
        let project = project.or_else(|| {
            std::fs::read_to_string(".claude/project")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

        let result = self
            .app
            .spawn(SpawnRequest {
                task_name: &params.name,
                skill_name,
                params: kv_params,
                work_dir_mode,
                prompt_mode,
                project,
            })
            .await;

        match result {
            Ok(output) => {
                let response = serde_json::json!({
                    "task_id": output.task_id.to_string(),
                    "task_name": output.task_name.as_str(),
                    "skill": output.skill_name,
                    "window": output.window_id.as_str(),
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&response).unwrap(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to spawn task: {e}"
            ))])),
        }
    }
}

#[tool_handler]
impl<R: Runtime> ServerHandler for ClatMcpServer<R> {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("clat", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Command Center MCP server. Use clat_spawn to create new task agents.",
            )
    }
}

// ---------------------------------------------------------------------------
// Breadcrumb helpers
// ---------------------------------------------------------------------------

/// Breadcrumb file written by the dashboard so that CLI-spawned tasks
/// can discover the MCP server URL.
const MCP_URL_BREADCRUMB: &str = ".claude/mcp-url";

/// Write the MCP server URL to a breadcrumb file in the project root.
pub fn write_mcp_url_breadcrumb(project_root: &std::path::Path, url: &str) {
    let path = project_root.join(MCP_URL_BREADCRUMB);
    let _ = std::fs::write(&path, url);
}

/// Remove the breadcrumb file on shutdown.
pub fn remove_mcp_url_breadcrumb(project_root: &std::path::Path) {
    let _ = std::fs::remove_file(project_root.join(MCP_URL_BREADCRUMB));
}

/// Read the MCP server URL from the breadcrumb file.
pub fn read_mcp_url_breadcrumb(project_root: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(project_root.join(MCP_URL_BREADCRUMB))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// JWT authentication middleware
// ---------------------------------------------------------------------------

/// Axum middleware that extracts and verifies JWT tokens from incoming requests.
///
/// Checks `Authorization: Bearer <token>` header first, then falls back to
/// `?token=<token>` query parameter. Verified claims are inserted into
/// request extensions for downstream handlers.
///
/// During migration, requests without tokens are allowed through with a warning.
async fn jwt_auth_middleware(
    axum::extract::State(signer): axum::extract::State<JwtSigner>,
    mut req: Request,
    next: Next,
) -> Response {
    // Try Authorization header first.
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string());

    // Fall back to query parameter.
    let token = token.or_else(|| {
        req.uri().query().and_then(|q| {
            q.split('&')
                .find_map(|pair| pair.strip_prefix("token=").map(|v| v.to_string()))
        })
    });

    match token {
        Some(t) => match signer.verify(&t) {
            Ok(claims) => {
                tracing::debug!(
                    task_id = %claims.sub,
                    role = %claims.role,
                    "authenticated MCP request"
                );
                req.extensions_mut().insert(claims);
            }
            Err(e) => {
                tracing::warn!("invalid JWT token: {e}");
                return Response::builder()
                    .status(401)
                    .body(axum::body::Body::from("invalid token"))
                    .unwrap();
            }
        },
        None => {
            // Graceful degradation: allow unauthenticated requests during migration.
            tracing::debug!("unauthenticated MCP request (no token)");
        }
    }

    next.run(req).await
}

/// Start the MCP HTTP server as a background tokio task.
///
/// Returns the URL the server is listening on.
pub async fn start_mcp_server<R: Runtime>(
    app: Arc<ClatApp<R>>,
    port: u16,
) -> anyhow::Result<String> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    app.init_watch().await?;
    let jwt_signer = app.jwt_signer().clone();
    let config = StreamableHttpServerConfig {
        stateful_mode: false,
        json_response: true,
        ..Default::default()
    };
    let service = StreamableHttpService::new(
        move || Ok(ClatMcpServer::new(Arc::clone(&app))),
        LocalSessionManager::default().into(),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service).layer(
        axum::middleware::from_fn_with_state(jwt_signer, jwt_auth_middleware),
    );
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let url = format!("http://127.0.0.1:{port}/mcp");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("MCP server error: {e}");
        }
    });

    Ok(url)
}
