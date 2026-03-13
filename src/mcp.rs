use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::app::{ClatApp, PromptMode, SpawnOutput, SpawnRequest, WorkDirMode};
use crate::jwt::JwtSigner;
use crate::runtime::Runtime;

// ---------------------------------------------------------------------------
// Trait-object interface so the MCP server struct stays non-generic.
// ---------------------------------------------------------------------------

/// Operations the MCP server needs from the application layer.
///
/// Implemented for `ClatApp<R>` so we can erase the Runtime generic.
pub(crate) trait McpApp: Send + Sync {
    fn spawn<'a>(
        &'a self,
        params: McpSpawnParams,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<SpawnOutput>> + Send + 'a>>;
}

/// Owned parameter bundle for [`McpApp::spawn`].
pub(crate) struct McpSpawnParams {
    pub name: String,
    pub task: String,
    pub skill: Option<String>,
    pub project: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub scratch: bool,
}

impl<R: Runtime + Send + Sync + 'static> McpApp for ClatApp<R> {
    fn spawn<'a>(
        &'a self,
        p: McpSpawnParams,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<SpawnOutput>> + Send + 'a>> {
        Box::pin(async move {
            let skill_name = p.skill.as_deref().unwrap_or("engineer");
            let kv_params = vec![("task".to_string(), p.task)];

            let repo_path;
            let (work_dir_mode, prompt_mode) = if p.scratch {
                (WorkDirMode::Scratch, PromptMode::Full)
            } else {
                repo_path = p
                    .repo
                    .as_deref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| self.project_root().to_path_buf());
                (
                    WorkDirMode::Worktree {
                        repo: &repo_path,
                        branch: p.branch.as_deref(),
                    },
                    PromptMode::Full,
                )
            };

            // Inherit project from parent task breadcrumb if not explicitly set.
            let project = p.project.or_else(|| {
                std::fs::read_to_string(".claude/project")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

            ClatApp::spawn(
                self,
                SpawnRequest {
                    task_name: &p.name,
                    skill_name,
                    params: kv_params,
                    work_dir_mode,
                    prompt_mode,
                    project,
                },
            )
            .await
        })
    }
}

// ---------------------------------------------------------------------------
// MCP server (non-generic — macros work)
// ---------------------------------------------------------------------------

/// MCP server that exposes clat commands as tools.
///
/// Runs inside the dashboard process and serves spawned agents
/// over HTTP on localhost.
#[derive(Clone)]
pub struct ClatMcpServer {
    app: Arc<dyn McpApp>,
    tool_router: ToolRouter<Self>,
}

impl ClatMcpServer {
    pub fn new(app: Arc<dyn McpApp>) -> Self {
        Self {
            app,
            tool_router: Self::tool_router(),
        }
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
impl ClatMcpServer {
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

        let result = self
            .app
            .spawn(McpSpawnParams {
                name: params.name,
                task: params.task,
                skill: params.skill,
                project,
                repo: params.repo,
                branch: params.branch,
                scratch: params.scratch.unwrap_or(false),
            })
            .await;

        match result {
            Ok(output) => {
                let response = serde_json::json!({
                    "task_id": output.task_id.as_str(),
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
impl ServerHandler for ClatMcpServer {
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
// Server startup
// ---------------------------------------------------------------------------

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

    let jwt_signer = app.jwt_signer().clone();
    let app_dyn: Arc<dyn McpApp> = app;
    let config = StreamableHttpServerConfig {
        stateful_mode: false,
        json_response: true,
        ..Default::default()
    };
    let service = StreamableHttpService::new(
        move || Ok(ClatMcpServer::new(Arc::clone(&app_dyn))),
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
