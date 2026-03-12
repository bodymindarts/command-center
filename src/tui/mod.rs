mod chat;
mod handlers;
mod input;
mod keybindings;
mod permissions;
mod state;
mod telegram;
mod widgets;

use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::app::ClatApp;
use crate::assistant::{AssistantEvent, AssistantSession, EXO_SYSTEM_PROMPT, SessionKey};
use crate::permission::HookEvent;
use crate::primitives::ProjectId;
use crate::runtime::Runtime;
use state::{ProjectState, ScreenState};

/// Holds the project session and bridge for a single project.
/// Chat state (messages, streaming) lives in `ProjectState::chat_view`.
struct ProjectContext {
    session: AssistantSession,
    cancel: Arc<AtomicBool>,
}

impl ProjectContext {
    fn new(
        session_key: SessionKey,
        session_id: Option<&str>,
        system_prompt: &str,
        event_tx: mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
        skip_permissions: bool,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let session = AssistantSession::new(
            session_key,
            session_id,
            Arc::clone(&cancel),
            system_prompt,
            event_tx,
            skip_permissions,
        );
        ProjectContext { session, cancel }
    }
}

/// Cancel and remove a specific project's context and chat state.
fn cancel_project_context(
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    state: &mut ScreenState,
    project_id: &ProjectId,
) {
    if let Some(ctx) = project_contexts.remove(project_id) {
        ctx.cancel.store(true, Ordering::Relaxed);
    }
    state.projects.remove(project_id);
}

/// Build a fully-initialized ProjectState: loads session ID and chat history from the DB.
async fn build_project_state<R: Runtime>(app: &ClatApp<R>, project_id: &ProjectId) -> ProjectState {
    let mut assistant = chat::AssistantChat::new();
    assistant.session_id = app.read_project_session_id(project_id);
    if let Ok(messages) = app.session_messages(Some(project_id)).await {
        let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
        assistant.load_history(recent.into_iter().rev().collect());
    }
    ProjectState::new(assistant, Vec::new())
}

/// Initialize a project: build its ProjectState, add it to ScreenState, return a ProjectContext.
async fn init_project_context<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    project_id: &ProjectId,
    project_name: &str,
    event_tx: mpsc::UnboundedSender<(SessionKey, AssistantEvent)>,
    skip_permissions: bool,
) -> ProjectContext {
    let project_state = build_project_state(app, project_id).await;
    let session_id = project_state.chat_view.assistant.session_id.clone();
    state.add_project(*project_id, project_state);
    let prompt = crate::assistant::project_system_prompt(project_name);
    let session_key = SessionKey::Project(*project_id);
    ProjectContext::new(
        session_key,
        session_id.as_deref(),
        &prompt,
        event_tx,
        skip_permissions,
    )
}

/// Spawn `caffeinate -s` to prevent system sleep (macOS only).
fn spawn_caffeinate() -> Option<std::process::Child> {
    match std::process::Command::new("caffeinate")
        .arg("-s")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("warning: failed to spawn caffeinate: {e}");
            None
        }
    }
}

pub async fn run<R: Runtime + Send + Sync + 'static>(
    app: Arc<ClatApp<R>>,
    resume_session: Option<&str>,
    caffeinate: bool,
) -> anyhow::Result<()> {
    let skip_permissions = app.skip_permissions();
    let mut caffeinate_child = if caffeinate { spawn_caffeinate() } else { None };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.close_stale_tasks().await;
    let tasks = app.list_visible(None).await?;
    let exo = {
        let mut assistant = chat::AssistantChat::new();
        if let Some(sid) = resume_session {
            assistant.session_id = Some(sid.to_string());
        } else if let Some(sid) = app.read_exo_session_id() {
            assistant.session_id = Some(sid);
        }
        if let Ok(messages) = app.session_messages(None).await {
            let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
            assistant.load_history(recent.into_iter().rev().collect());
        }
        ProjectState::new(assistant, tasks)
    };
    let keybindings = keybindings::Keybindings::load(&app.project_root().join("keybindings.toml"));
    let mut state = ScreenState::new(exo, keybindings);
    let cancel = Arc::new(AtomicBool::new(false));

    // Shared channel for all assistant session events (ExO + PM sessions).
    let (assistant_tx, mut assistant_rx) =
        mpsc::unbounded_channel::<(SessionKey, AssistantEvent)>();

    let mut exo_session = AssistantSession::new(
        SessionKey::Exo,
        state.exo.chat_view.assistant.session_id.as_deref(),
        Arc::clone(&cancel),
        EXO_SYSTEM_PROMPT,
        assistant_tx.clone(),
        skip_permissions,
    );

    // Project contexts: one per project, keyed by project ID
    let mut project_contexts: HashMap<ProjectId, ProjectContext> = HashMap::new();

    // Boot all PM sessions eagerly so they warm up in the background.
    if let Ok(projects) = app.list_projects().await {
        for project in &projects {
            project_contexts.insert(
                project.id,
                init_project_context(
                    &mut state,
                    &app,
                    &project.id,
                    project.name.as_str(),
                    assistant_tx.clone(),
                    skip_permissions,
                )
                .await,
            );
        }
    }

    // Hook event socket listener — async via tokio::net::UnixListener
    let (hook_tx, mut hook_rx) = mpsc::unbounded_channel::<(HookEvent, UnixStream)>();
    let perm_cancel = Arc::clone(&cancel);
    let socket_path = crate::permission::session_socket_path();
    let _ = std::fs::remove_file(&socket_path);
    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    // SAFETY: called once at startup before spawning tasks that read env vars.
    unsafe {
        std::env::set_var(crate::permission::SOCKET_ENV, &socket_path);
    }
    crate::permission::write_socket_breadcrumb(app.project_root(), &socket_path);
    if skip_permissions {
        crate::permission::write_skip_permissions_breadcrumb(app.project_root());
    }
    {
        let work_dirs: Vec<String> = state
            .exo
            .task_list
            .tasks
            .iter()
            .filter_map(|t| t.work_dir.clone())
            .collect();
        let sock_str = socket_path.to_string_lossy().to_string();
        tokio::task::spawn_blocking(move || {
            crate::runtime::reembed_socket_in_worktrees(&work_dirs, &sock_str);
        });
    }
    // Spawn async hook listener task
    tokio::spawn(async move {
        while !perm_cancel.load(Ordering::Relaxed) {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };

            let hook_tx = hook_tx.clone();
            let perm_cancel = perm_cancel.clone();

            // Handle each connection in its own task so slow reads don't
            // block accepting new connections.
            tokio::spawn(async move {
                if perm_cancel.load(Ordering::Relaxed) {
                    return;
                }
                let mut buf = String::new();
                let mut reader = stream;
                if tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut buf)
                    .await
                    .is_ok()
                {
                    // Log every incoming hook message to data/hooks.log
                    if let Ok(mut log) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("data/hooks.log")
                    {
                        use std::io::Write;
                        let ts = chrono::Local::now().format("%H:%M:%S%.3f");
                        let _ = writeln!(log, "[{ts}] {}", buf.trim());
                    }

                    let event: HookEvent = serde_json::from_str(&buf).unwrap_or_else(|_| {
                        HookEvent::Unknown(serde_json::Value::String(buf.clone()))
                    });

                    // Convert tokio stream to std for later synchronous response writes.
                    if let Ok(std_stream) = reader.into_std() {
                        let _ = std_stream.set_nonblocking(false);
                        let _ = hook_tx.send((event, std_stream));
                    }
                }
            });
        }
    });

    // Optional Telegram bot for remote permission approval.
    let (tg_tx, mut tg_rx) = if let (Ok(token), Ok(chat_id)) = (
        std::env::var("TELEGRAM_BOT_TOKEN"),
        std::env::var("TELEGRAM_CHAT_ID"),
    ) {
        let (tx, rx) = telegram::start(token, chat_id, Arc::clone(&cancel));
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let result = run_loop(
        &mut terminal,
        &mut state,
        &mut exo_session,
        &mut project_contexts,
        &app,
        &mut assistant_rx,
        &mut hook_rx,
        tg_tx.as_ref(),
        &mut tg_rx,
    )
    .await;

    cancel.store(true, Ordering::Relaxed);
    for ctx in project_contexts.values() {
        ctx.cancel.store(true, Ordering::Relaxed);
    }

    // Deny all pending permissions on exit
    for (_, mut queue) in state.permissions.drain_all() {
        for perm in queue.drain(..) {
            if let Some(tx) = tg_tx.as_ref() {
                let _ = tx.send(telegram::TgOutbound::Resolved {
                    perm_id: perm.perm_id,
                    outcome: "⚪ Denied (dashboard closed)".to_string(),
                });
            }
            let _ = handlers::write_response_to_stream(perm.stream, false, None);
        }
    }
    let _ = std::fs::remove_file(&socket_path);
    crate::permission::remove_socket_breadcrumb(app.project_root());
    crate::permission::remove_skip_permissions_breadcrumb(app.project_root());

    if let Some(ref mut child) = caffeinate_child {
        let _ = child.kill();
        let _ = child.wait();
    }

    terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_loop<R: Runtime>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut ScreenState,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>,
    assistant_rx: &mut mpsc::UnboundedReceiver<(SessionKey, AssistantEvent)>,
    hook_rx: &mut mpsc::UnboundedReceiver<(HookEvent, UnixStream)>,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
    tg_rx: &mut Option<mpsc::UnboundedReceiver<telegram::TgInbound>>,
) -> anyhow::Result<()> {
    let mut event_stream = crossterm::event::EventStream::new();
    let mut last_tick = Instant::now();
    let mut tg_perm = handlers::TgPermState {
        ids: std::collections::HashSet::new(),
        counter: 0,
    };

    state.render_loop_starting();

    // Populate global_task_work_dirs before the main loop so that hook events
    // arriving early can resolve CWDs to task names correctly.
    handlers::tick_refresh(state, app, tg_tx).await;

    loop {
        let tick_rate = if state.any_streaming() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };

        terminal.draw(|frame| widgets::ui(frame, state))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());

        tokio::select! {
            // Terminal input events (crossterm EventStream)
            event = event_stream.next() => {
                if let Some(event) = event {
                    let event = event?;
                    match event {
                        Event::Paste(text) => handlers::handle_paste(state, text),
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            state.clear_status_error();
                            // Suspend — needs terminal access, handled inline
                            if state.keybindings.global.suspend.matches(&key) {
                                terminal::disable_raw_mode()?;
                                crossterm::execute!(
                                    terminal.backend_mut(),
                                    DisableBracketedPaste,
                                    LeaveAlternateScreen
                                )?;
                                terminal.show_cursor()?;
                                unsafe {
                                    libc::raise(libc::SIGTSTP);
                                }
                                terminal::enable_raw_mode()?;
                                crossterm::execute!(
                                    terminal.backend_mut(),
                                    EnterAlternateScreen,
                                    EnableBracketedPaste
                                )?;
                                terminal.hide_cursor()?;
                                terminal.clear()?;
                            } else if !handlers::handle_global_keys(state, key, app, tg_tx).await {
                                handlers::handle_focus_key(state, key, app, exo_session, project_contexts).await;
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Assistant session events (shared channel for ExO + all PM sessions)
            event = assistant_rx.recv() => {
                if let Some((key, ev)) = event {
                    handlers::dispatch_assistant_event(
                        &key, ev, state, exo_session, project_contexts, app, tg_tx,
                    ).await;
                }
            }

            // Hook events from the permission socket
            event = hook_rx.recv() => {
                if let Some((hook_event, stream)) = event {
                    handlers::dispatch_hook_event(
                        state,
                        project_contexts,
                        app,
                        hook_event,
                        stream,
                        tg_tx,
                        &mut tg_perm,
                    ).await;
                }
            }

            // Telegram inbound events (optional)
            event = recv_optional(tg_rx) => {
                if let Some(tg_msg) = event {
                    handlers::dispatch_telegram_event(state, exo_session, app, tg_msg).await;
                }
            }

            // Periodic tick (timeout-based)
            _ = tokio::time::sleep(timeout) => {}
        }

        // Periodic refresh
        if last_tick.elapsed() >= tick_rate {
            handlers::tick_refresh(state, app, tg_tx).await;
            handlers::detect_vanished_perms(state, tg_tx, &mut tg_perm.ids);
            last_tick = Instant::now();
        }

        if state.should_quit() {
            return Ok(());
        }
    }
}

/// Receive from an optional channel. Returns `pending` when the channel is `None`.
async fn recv_optional<T>(rx: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending::<Option<T>>().await,
    }
}
