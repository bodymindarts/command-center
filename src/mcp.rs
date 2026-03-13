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
use crate::runtime::Runtime;
use crate::watch::WatchService;

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
    _watch_service: Arc<WatchService>,
    tool_router: ToolRouter<Self>,
}

impl<R: Runtime + Send + Sync + 'static> ClatMcpServer<R> {
    pub fn new(app: Arc<ClatApp<R>>, watch_service: Arc<WatchService>) -> Self {
        let mut router = Self::tool_router();
        router.add_route(Self::create_watch_route(Arc::clone(&watch_service)));
        Self {
            app,
            _watch_service: watch_service,
            tool_router: router,
        }
    }

    fn create_watch_route(watch_service: Arc<WatchService>) -> ToolRoute<Self> {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["label", "check"],
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Short description of what you're watching for (included in the notification)"
                },
                "check": {
                    "oneOf": [{
                        "type": "object",
                        "title": "timer",
                        "description": "Fire a notification after a delay",
                        "required": ["name"],
                        "properties": {
                            "name": { "const": "timer" }
                        },
                        "additionalProperties": false
                    }]
                },
                "delay_seconds": {
                    "type": "integer",
                    "minimum": 5,
                    "description": "Seconds to wait before firing the notification"
                }
            },
            "additionalProperties": false
        });

        let input_schema = Arc::new(schema.as_object().unwrap().clone());
        let tool = rmcp::model::Tool::new(
            "create_watch",
            "Set a background timer. You'll receive a message when it fires. Uses zero LLM tokens while waiting. Returns immediately with a watch ID.",
            input_schema,
        );

        ToolRoute::new_dyn(
            tool,
            move |ctx: rmcp::handler::server::tool::ToolCallContext<'_, ClatMcpServer<R>>| {
                let ws = Arc::clone(&watch_service);
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
                        .unwrap_or("unnamed timer");
                    let delay = args
                        .get("delay_seconds")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(30);

                    match ws.create_timer(&task_id, label, delay).await {
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
impl<R: Runtime + Send + Sync + 'static> ClatMcpServer<R> {
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
impl<R: Runtime + Send + Sync + 'static> ServerHandler for ClatMcpServer<R> {
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
pub async fn start_mcp_server<R: Runtime + Send + Sync + 'static>(
    app: Arc<ClatApp<R>>,
    port: u16,
) -> anyhow::Result<String> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    let watch_service = Arc::new(WatchService::init(Arc::clone(&app)).await?);
    let jwt_signer = app.jwt_signer().clone();
    let config = StreamableHttpServerConfig {
        stateful_mode: false,
        json_response: true,
        ..Default::default()
    };
    let service = StreamableHttpService::new(
        move || {
            Ok(ClatMcpServer::new(
                Arc::clone(&app),
                Arc::clone(&watch_service),
            ))
        },
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
