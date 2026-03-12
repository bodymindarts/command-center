use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::schemars;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::app::{ClatApp, PromptMode, SpawnOutput, SpawnRequest, WorkDirMode};
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
        Parameters(params): Parameters<SpawnParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let result = self
            .app
            .spawn(McpSpawnParams {
                name: params.name,
                task: params.task,
                skill: params.skill,
                project: params.project,
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

/// Breadcrumb file name for the MCP server URL.
const MCP_URL_BREADCRUMB: &str = "cc-mcp-url";

/// Write the MCP server URL to a breadcrumb file in TMPDIR.
pub fn write_mcp_url_breadcrumb(url: &str) {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&tmpdir).join(MCP_URL_BREADCRUMB);
    let _ = std::fs::write(&path, url);
}

/// Remove the breadcrumb file on shutdown.
pub fn remove_mcp_url_breadcrumb() {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&tmpdir).join(MCP_URL_BREADCRUMB);
    let _ = std::fs::remove_file(&path);
}

/// Read the MCP server URL from the breadcrumb file.
pub fn read_mcp_url_breadcrumb() -> Option<String> {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&tmpdir).join(MCP_URL_BREADCRUMB);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

/// Start the MCP HTTP server as a background tokio task.
///
/// Returns the URL the server is listening on.
pub async fn start_mcp_server<R: Runtime + Send + Sync + 'static>(
    app: Arc<ClatApp<R>>,
    port: u16,
) -> anyhow::Result<String> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    };

    let app_dyn: Arc<dyn McpApp> = app;
    let service = StreamableHttpService::new(
        move || Ok(ClatMcpServer::new(Arc::clone(&app_dyn))),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let url = format!("http://127.0.0.1:{port}/mcp");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("MCP server error: {e}");
        }
    });

    Ok(url)
}
