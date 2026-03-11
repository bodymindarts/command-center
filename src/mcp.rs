use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, ErrorData, Implementation, ProtocolVersion, ServerCapabilities,
    ServerInfo,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::app::{ClatApp, PromptMode, SpawnRequest, WorkDirMode};
use crate::runtime::TmuxRuntime;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnParams {
    #[schemars(description = "Short descriptive name for the task")]
    pub name: String,

    #[schemars(description = "Self-contained task description with ALL context the agent needs")]
    pub task: String,

    #[schemars(description = "Skill to use: engineer (default), researcher, reviewer, reporter")]
    pub skill: Option<String>,

    #[schemars(description = "Project name to assign to")]
    pub project: Option<String>,

    #[schemars(description = "Git repo path to work in")]
    pub repo: Option<String>,

    #[schemars(description = "Check out an existing branch in the worktree")]
    pub branch: Option<String>,

    #[schemars(description = "Use a scratch directory instead of a git worktree")]
    pub scratch: Option<bool>,
}

#[derive(Clone)]
pub struct McpServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl McpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Spawn a new Claude Code agent task with a dedicated worktree")]
    async fn clat_spawn(
        &self,
        Parameters(params): Parameters<SpawnParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let inner = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            let SpawnParams {
                name,
                task,
                skill,
                project,
                repo,
                branch,
                scratch,
            } = params;

            let skill_name = skill.unwrap_or_else(|| "engineer".to_string());
            let scratch = scratch.unwrap_or(false);

            let app = ClatApp::try_new(TmuxRuntime)
                .map_err(|e| format!("Failed to initialize app: {e}"))?;

            let repo_pathbuf = repo
                .map(PathBuf::from)
                .unwrap_or_else(|| app.project_root().to_path_buf());

            let project = project.or_else(|| {
                std::fs::read_to_string(".claude/project")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

            let task_params = vec![("task".to_string(), task)];

            let (work_dir_mode, prompt_mode) = if scratch {
                (WorkDirMode::Scratch, PromptMode::Full)
            } else {
                (
                    WorkDirMode::Worktree {
                        repo: &repo_pathbuf,
                        branch: branch.as_deref(),
                    },
                    PromptMode::Full,
                )
            };

            let result = app
                .spawn(SpawnRequest {
                    task_name: &name,
                    skill_name: &skill_name,
                    params: task_params,
                    work_dir_mode,
                    prompt_mode,
                    project,
                })
                .map_err(|e| format!("Spawn failed: {e}"))?;

            Ok(serde_json::json!({
                "task_id": result.task_id.as_str(),
                "name": result.task_name.as_str(),
                "skill": result.skill_name,
                "status": "running",
                "window_id": result.window_id.as_str(),
            }))
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("Task join error: {e}"), None))?;

        match inner {
            Ok(json) => Ok(CallToolResult::success(vec![Content::text(
                json.to_string(),
            )])),
            Err(msg) => Ok(CallToolResult::error(vec![Content::text(msg)])),
        }
    }
}

#[tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info.instructions =
            Some("Spawn and manage Claude Code agent tasks via the Command Center.".to_string());
        info
    }
}

/// MCP server URL breadcrumb path (in temp dir).
fn mcp_url_breadcrumb_path() -> PathBuf {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let base = std::fs::canonicalize(&tmpdir).unwrap_or_else(|_| PathBuf::from(&tmpdir));
    base.join("cc-mcp-url")
}

/// Read the MCP server URL from the breadcrumb file.
pub fn read_mcp_url() -> Option<String> {
    std::fs::read_to_string(mcp_url_breadcrumb_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Start the MCP HTTP server in a background thread with its own tokio runtime.
///
/// Writes a breadcrumb file so that [`crate::runtime::setup_worktree_config`]
/// can inject the MCP server URL into spawned agents' settings.
///
/// Returns the URL the server is listening on.
pub fn start_mcp_server(port: u16) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}/mcp");

    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

    let url_for_thread = url.clone();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(Err(format!("Failed to create tokio runtime: {e}")));
                return;
            }
        };
        rt.block_on(async move {
            use rmcp::transport::streamable_http_server::{
                session::local::LocalSessionManager, tower::StreamableHttpService,
            };

            let service = StreamableHttpService::new(
                || Ok(McpServer::new()),
                Arc::new(LocalSessionManager::default()),
                Default::default(),
            );

            let router = axum::Router::new().nest_service("/mcp", service);

            match tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await {
                Ok(listener) => {
                    let _ = tx.send(Ok(()));
                    let _ = axum::serve(listener, router).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Failed to bind port {port}: {e}")));
                }
            }
        });
        drop(url_for_thread);
    });

    // Wait for the server to start or fail
    match rx.recv() {
        Ok(Ok(())) => {
            std::fs::write(mcp_url_breadcrumb_path(), &url)?;
            Ok(url)
        }
        Ok(Err(e)) => anyhow::bail!("MCP server failed to start: {e}"),
        Err(_) => anyhow::bail!("MCP server thread terminated unexpectedly"),
    }
}

/// Remove the MCP URL breadcrumb file on shutdown.
pub fn remove_mcp_breadcrumb() {
    let _ = std::fs::remove_file(mcp_url_breadcrumb_path());
}
