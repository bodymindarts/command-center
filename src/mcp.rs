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

use crate::app::{ClatApp, PromptMode, SpawnRequest, WorkDirMode};
use crate::jwt::JwtSigner;
use crate::primitives::{ChatId, MessageRole, ProjectId};
use crate::runtime::Runtime;

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
        router.add_route(Self::send_message_route(Arc::clone(&app)));
        Self {
            app,
            tool_router: router,
        }
    }

    fn create_watch_route(app: Arc<ClatApp<R>>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["label", "check"],
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Short description of what you're watching for (included in the notification)"
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
                    let task_id = ctx
                        .request_context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|parts| parts.extensions.get::<crate::jwt::AgentClaims>())
                        .map(|c| c.sub.as_str())
                        .unwrap_or("unknown")
                        .to_string();

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
                                .create_command(&task_id, label, cmd, delay, context)
                                .await
                        }
                        _ => {
                            app.watch()
                                .create_timer(&task_id, label, delay, context)
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
            "Send a message to another running task or to the PM/ExO session. Messages to tasks are delivered to their tmux pane. Messages to 'pm' are recorded in the project chat for the PM to retrieve. Caller must have a project set, and can only message tasks in the same project.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let app = Arc::clone(&app);
                Box::pin(async move {
                    // Extract JWT claims from request extensions.
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

                    let caller_project = match claims.project {
                        Some(ref p) => p.clone(),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Agents without a project cannot use send_message",
                            )]));
                        }
                    };

                    let args = ctx.arguments.unwrap_or_default();
                    let target = match args.get("target").and_then(|v| v.as_str()) {
                        Some(t) => t.to_string(),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'target'",
                            )]));
                        }
                    };
                    let message = match args.get("message").and_then(|v| v.as_str()) {
                        Some(m) => m.to_string(),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "Missing required field 'message'",
                            )]));
                        }
                    };

                    // Resolve caller's project to a ProjectId.
                    // The JWT project claim may be a project name or a ProjectId string
                    // (the latter happens for reopened tasks).
                    let caller_project_id =
                        match app.resolve_project_id(&caller_project).await.or_else(|_| {
                            caller_project
                                .parse::<ProjectId>()
                                .map_err(anyhow::Error::from)
                        }) {
                            Ok(id) => id,
                            Err(e) => {
                                return Ok(CallToolResult::error(vec![Content::text(format!(
                                    "Failed to resolve caller project '{caller_project}': {e}"
                                ))]));
                            }
                        };

                    if target == "pm" {
                        // Store message in the project chat for PM visibility.
                        let chat = ChatId::Project(caller_project_id);
                        let label = format!("[from agent {} ({})]", claims.sub, claims.role);
                        let content = format!("{label} {message}");
                        match app
                            .insert_chat_message(&chat, MessageRole::User, &content)
                            .await
                        {
                            Ok(_) => {
                                let response = serde_json::json!({
                                    "status": "recorded",
                                    "target": "pm",
                                    "project": caller_project,
                                    "note": "Message recorded in project chat. The PM will see it when reviewing project messages."
                                });
                                Ok(CallToolResult::success(vec![Content::text(
                                    serde_json::to_string_pretty(&response).unwrap(),
                                )]))
                            }
                            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                                "Failed to record message: {e}"
                            ))])),
                        }
                    } else {
                        // Regular task target: resolve, verify project, then send.
                        let task = match app.resolve_task(&target).await {
                            Ok(t) => t,
                            Err(e) => {
                                return Ok(CallToolResult::error(vec![Content::text(format!(
                                    "Failed to resolve target task '{target}': {e}"
                                ))]));
                            }
                        };

                        // Verify same project.
                        if task.project_id != Some(caller_project_id) {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Target task '{}' does not belong to the same project",
                                task.name
                            ))]));
                        }

                        match app.send(&target, &message).await {
                            Ok(output) => {
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
                    }
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
