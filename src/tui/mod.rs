mod app;
mod chat;
mod claude;
mod telegram;
mod widgets;

use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::permission::PermissionRequest;
use crate::primitives::MessageRole;
use crate::runtime::Runtime;
use crate::service::{PromptMode, SpawnRequest, TaskService, WorkDirMode};
use app::{ActivePermission, App, Focus};

const EXO_PERM_KEY: &str = "exo";

/// Find the task name whose work_dir is a prefix of the given CWD.
/// Uses the global (project-independent) work_dir list so lookups
/// work regardless of which project is currently displayed.
fn find_task_name_by_cwd(work_dirs: &[(String, String)], cwd: &std::path::Path) -> Option<String> {
    work_dirs
        .iter()
        .find(|(_, wd)| {
            let canon = std::fs::canonicalize(wd).unwrap_or_else(|_| std::path::PathBuf::from(wd));
            cwd.starts_with(&canon)
        })
        .map(|(name, _)| name.clone())
}
use chat::ExoState;
use claude::{EXO_SYSTEM_PROMPT, ExoEvent, ExoSession, PmEvent, pm_system_prompt};

/// Holds the PM state and session for a single project.
struct PmContext {
    state: ExoState,
    session: Option<ExoSession>,
    cancel: Arc<AtomicBool>,
    /// Sender that wraps ExoEvent → PmEvent with this project's ID.
    bridge_tx: mpsc::Sender<ExoEvent>,
}

impl PmContext {
    fn new(project_id: &str, pm_tx: &mpsc::Sender<PmEvent>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (bridge_tx, bridge_rx) = mpsc::channel::<ExoEvent>();
        let pid = project_id.to_string();
        let pm_tx = pm_tx.clone();
        std::thread::spawn(move || {
            for event in bridge_rx {
                if pm_tx
                    .send(PmEvent {
                        project_id: pid.clone(),
                        inner: event,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        PmContext {
            state: ExoState::new(),
            session: None,
            cancel,
            bridge_tx,
        }
    }
}

/// Cancel and remove a specific project's PM context.
fn cancel_pm_context(pm_contexts: &mut HashMap<String, PmContext>, project_id: &str) {
    if let Some(ctx) = pm_contexts.remove(project_id) {
        ctx.cancel.store(true, Ordering::Relaxed);
    }
}

/// Ensure a PmContext exists for the project, creating and loading history if needed.
fn ensure_pm_context<R: Runtime>(
    pm_contexts: &mut HashMap<String, PmContext>,
    app: &mut App,
    service: &TaskService<R>,
    project_id: &str,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    if !pm_contexts.contains_key(project_id) {
        let mut ctx = PmContext::new(project_id, pm_tx);
        ctx.state.session_id = service.read_pm_session_id(project_id);
        if let Ok(messages) = service.pm_messages(project_id) {
            let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
            ctx.state.load_history(recent.into_iter().rev().collect());
        }
        pm_contexts.insert(project_id.to_string(), ctx);
    }
    if let Ok(msgs) = service.pm_messages(project_id) {
        app.pm_messages = msgs;
    }
}

/// Spawn `caffeinate -s` to prevent system sleep (macOS only).
/// Returns the child handle if successful, or None with a warning on failure.
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

pub fn run<R: Runtime>(
    service: &TaskService<R>,
    resume_session: Option<&str>,
    caffeinate: bool,
) -> Result<()> {
    let mut caffeinate_child = if caffeinate { spawn_caffeinate() } else { None };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tasks = service.list_visible(None)?;
    let mut app = App::new(tasks);
    let mut exo = ExoState::new();
    if let Some(sid) = resume_session {
        exo.session_id = Some(sid.to_string());
    } else if let Some(sid) = service.read_exo_session_id() {
        exo.session_id = Some(sid);
    }
    if let Ok(messages) = service.exo_messages() {
        // Load only the last few messages to provide context without
        // flooding the chat view with hundreds of old messages.
        let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
        exo.load_history(recent.into_iter().rev().collect());
    }
    let (tx, rx) = mpsc::channel::<ExoEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut exo_session = ExoSession::new(
        exo.session_id.as_deref(),
        Arc::clone(&cancel),
        tx.clone(),
        EXO_SYSTEM_PROMPT,
    );

    // PM (project manager) contexts: one per project, keyed by project ID
    let mut pm_contexts: HashMap<String, PmContext> = HashMap::new();
    let (pm_tx, pm_rx) = mpsc::channel::<PmEvent>();

    // Permission socket listener
    let (perm_tx, perm_rx) = mpsc::channel::<(UnixStream, PermissionRequest)>();
    let (resolved_tx, resolved_rx) = mpsc::channel::<String>(); // CWD of resolved tool
    let (idle_tx, idle_rx) = mpsc::channel::<String>(); // CWD of idle agent
    let perm_cancel = Arc::clone(&cancel);
    let (listener, socket_path) = crate::permission::start_socket_listener()?;
    // SAFETY: called once at startup before spawning threads that read env vars.
    // Spawned tasks need CC_PERM_SOCKET to route permission requests here.
    unsafe {
        std::env::set_var(crate::permission::SOCKET_ENV, &socket_path);
    }
    // Write breadcrumb so CLI-spawned tasks (which don't inherit our env)
    // can discover the active socket path.
    crate::permission::write_socket_breadcrumb(service.project_root(), &socket_path);
    // Re-embed socket path in active worktrees so hooks from pre-existing
    // tasks connect to this dashboard's socket after a restart.
    // Reuse the task list from above and run in a background thread to
    // avoid blocking the first render.
    {
        let work_dirs: Vec<String> = app
            .tasks
            .iter()
            .filter_map(|t| t.work_dir.clone())
            .collect();
        let sock_str = socket_path.to_string_lossy().to_string();
        std::thread::spawn(move || {
            crate::runtime::reembed_socket_in_worktrees(&work_dirs, &sock_str);
        });
    }
    std::thread::spawn(move || {
        while !perm_cancel.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = String::new();
                    if std::io::Read::read_to_string(&mut stream, &mut buf).is_ok() {
                        if let Some(cwd) = crate::permission::parse_resolved_json(&buf) {
                            let _ = resolved_tx.send(cwd);
                        } else if let Some(cwd) = crate::permission::parse_idle_json(&buf) {
                            let _ = idle_tx.send(cwd);
                        } else if let Some(req) = crate::permission::parse_request_json(&buf) {
                            let _ = perm_tx.send((stream, req));
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });

    // Optional Telegram bot for remote permission approval.
    let (tg_tx, tg_rx) = if let (Ok(token), Ok(chat_id)) = (
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
        &mut app,
        &mut exo,
        &mut exo_session,
        &mut pm_contexts,
        &pm_tx,
        service,
        &rx,
        &pm_rx,
        &perm_rx,
        &resolved_rx,
        &idle_rx,
        tg_tx.as_ref(),
        tg_rx.as_ref(),
    );

    cancel.store(true, Ordering::Relaxed);
    for ctx in pm_contexts.values() {
        ctx.cancel.store(true, Ordering::Relaxed);
    }

    // Deny all pending permissions on exit
    for (_, mut queue) in app.pending_permissions.drain() {
        for perm in queue.drain(..) {
            if let Some(tx) = tg_tx.as_ref() {
                let _ = tx.send(telegram::TgOutbound::Resolved {
                    perm_id: perm.perm_id,
                    outcome: "⚪ Denied (dashboard closed)".to_string(),
                });
            }
            let _ = write_response_to_stream(perm.stream, false, None);
        }
    }
    let _ = std::fs::remove_file(&socket_path);
    crate::permission::remove_socket_breadcrumb(service.project_root());

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

fn write_response_to_stream(
    mut stream: UnixStream,
    allow: bool,
    permission_suggestions: Option<&[serde_json::Value]>,
) -> std::io::Result<()> {
    use std::io::Write;
    let response = crate::permission::make_response_json(allow, None, permission_suggestions);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn write_response_with_message(
    mut stream: UnixStream,
    allow: bool,
    message: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let response = crate::permission::make_response_json(allow, Some(message), None);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Extract the first question and its options from an AskUserQuestion tool_input.
/// Returns `(question_text, [(label, description)])`.
fn parse_ask_user_options(
    tool_input: Option<&serde_json::Value>,
) -> Option<(String, Vec<(String, String)>)> {
    let input = tool_input?;
    let questions = input.get("questions")?.as_array()?;
    let first = questions.first()?;
    let question = first.get("question")?.as_str()?.to_string();
    let options = first.get("options")?.as_array()?;
    let parsed: Vec<(String, String)> = options
        .iter()
        .filter_map(|opt| {
            let label = opt.get("label")?.as_str()?.to_string();
            let desc = opt
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            Some((label, desc))
        })
        .collect();
    if parsed.is_empty() {
        return None;
    }
    Some((question, parsed))
}

/// Resolve and consume the active permission request, returning the
/// stream, permission suggestions, and perm_id so the caller can send
/// the response and notify Telegram.
fn resolve_permission(
    app: &mut App,
    allow: bool,
) -> Option<(UnixStream, bool, Vec<serde_json::Value>, u64)> {
    let perm_key = app.active_permission_key()?;
    let perm = app.take_permission(&perm_key)?;
    Some((
        perm.stream,
        allow,
        perm.permission_suggestions,
        perm.perm_id,
    ))
}

/// Notify Telegram that a permission was resolved (if the bot is active).
fn notify_tg_resolved(
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    perm_id: u64,
    outcome: &str,
) {
    if let Some(tx) = tg_tx {
        let _ = tx.send(telegram::TgOutbound::Resolved {
            perm_id,
            outcome: outcome.to_string(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop<R: Runtime>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    pm_contexts: &mut HashMap<String, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
    service: &TaskService<R>,
    rx: &mpsc::Receiver<ExoEvent>,
    pm_rx: &mpsc::Receiver<PmEvent>,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
    resolved_rx: &mpsc::Receiver<String>,
    idle_rx: &mpsc::Receiver<String>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_rx: Option<&mpsc::Receiver<telegram::TgInbound>>,
) -> Result<()> {
    let mut last_tick = Instant::now();
    let mut perm_id_counter: u64 = 0;
    // Perm IDs that were sent to Telegram and are still pending.
    // Used to detect permissions resolved in-pane (disappeared from
    // pending_permissions without an explicit dashboard resolution).
    let mut tg_perm_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

    // Seed idle pane detection before first render.
    let running_pane_ids: Vec<String> = app
        .tasks
        .iter()
        .filter(|t| t.status.is_running())
        .filter_map(|t| t.tmux_pane.clone())
        .collect();
    let pane_refs: Vec<&str> = running_pane_ids.iter().map(|s| s.as_str()).collect();
    app.idle_panes = crate::runtime::idle_panes(&pane_refs);
    let mut last_activity_check = Instant::now();

    loop {
        let any_pm_streaming = pm_contexts.values().any(|ctx| ctx.state.streaming);
        let tick_rate = if exo.streaming || any_pm_streaming {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };

        let active_pm_state = app
            .active_project_id
            .as_deref()
            .and_then(|pid| pm_contexts.get(pid))
            .map(|ctx| &ctx.state);
        terminal.draw(|frame| widgets::ui(frame, app, exo, active_pm_state))?;

        // Drain channel events
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ExoEvent::TextDelta(text) => {
                    if exo.streaming {
                        exo.append_text(&text);
                        app.chat_scroll = 0;
                        if let Some(tx) = tg_tx {
                            let _ =
                                tx.send(telegram::TgOutbound::ExoTextDelta { text: text.clone() });
                        }
                    }
                }
                ExoEvent::ToolStart(name) => {
                    if exo.streaming {
                        exo.add_tool_activity(name);
                    }
                }
                ExoEvent::SessionId(id) => {
                    service.write_exo_session_id(&id);
                    exo.session_id = Some(id.clone());
                    exo_session.set_session_id(id);
                }
                ExoEvent::TurnDone => {
                    exo.finish_streaming();
                    if let Some(msg) = exo.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ =
                            service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                    }
                    if let Some(tx) = tg_tx {
                        let _ = tx.send(telegram::TgOutbound::ExoTurnDone);
                    }
                }
                ExoEvent::ProcessExited => {
                    exo.had_process_error = false;
                    exo_session.mark_exited();
                    if exo.streaming {
                        exo.add_error("Claude process exited unexpectedly");
                    }
                    // Process stays alive across turns, so exit means it
                    // truly died. Next send_message() will respawn with --resume.
                }
                ExoEvent::Error(e) => {
                    exo.had_process_error = true;
                    exo.add_error(&e);
                    if let Some(msg) = exo.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ =
                            service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                    }
                }
            }
        }

        // Drain PM channel events — route by project_id to the correct PmContext
        while let Ok(pm_ev) = pm_rx.try_recv() {
            let project_id = pm_ev.project_id;
            let Some(ctx) = pm_contexts.get_mut(&project_id) else {
                continue;
            };
            let is_active_pm = app.active_project_id.as_deref() == Some(project_id.as_str());
            match pm_ev.inner {
                ExoEvent::TextDelta(text) => {
                    if ctx.state.streaming {
                        ctx.state.append_text(&text);
                        if is_active_pm {
                            app.chat_scroll = 0;
                        }
                    }
                }
                ExoEvent::ToolStart(name) => {
                    if ctx.state.streaming {
                        ctx.state.add_tool_activity(name);
                    }
                }
                ExoEvent::SessionId(id) => {
                    service.write_pm_session_id(&project_id, &id);
                    ctx.state.session_id = Some(id.clone());
                    if let Some(sess) = &mut ctx.session {
                        sess.set_session_id(id);
                    }
                }
                ExoEvent::TurnDone => {
                    ctx.state.finish_streaming();
                    if let Some(msg) = ctx.state.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ = service.insert_pm_message(
                            &project_id,
                            MessageRole::Assistant,
                            &msg.text_content(),
                        );
                    }
                }
                ExoEvent::ProcessExited => {
                    ctx.state.had_process_error = false;
                    if let Some(sess) = &mut ctx.session {
                        sess.mark_exited();
                    }
                    if ctx.state.streaming {
                        ctx.state.add_error("PM process exited unexpectedly");
                    }
                }
                ExoEvent::Error(e) => {
                    ctx.state.had_process_error = true;
                    ctx.state.add_error(&e);
                    if let Some(msg) = ctx.state.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ = service.insert_pm_message(
                            &project_id,
                            MessageRole::Assistant,
                            &msg.text_content(),
                        );
                    }
                }
            }
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                // Handle bracketed paste events
                Event::Paste(text) => {
                    if matches!(app.focus, Focus::ChatInput | Focus::SpawnInput) {
                        if text.contains('\n') || text.contains('\r') {
                            app.input.set_paste(text);
                        } else {
                            // Single-line paste: insert character by character
                            for c in text.chars() {
                                app.input.insert(c);
                            }
                        }
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Clear transient status error on any keypress
                    app.status_error = None;
                    // Global: Ctrl+C quits
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        app.should_quit = true;
                    // Global: Ctrl+Z suspends
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('z')
                    {
                        terminal::disable_raw_mode()?;
                        crossterm::execute!(
                            terminal.backend_mut(),
                            DisableBracketedPaste,
                            LeaveAlternateScreen
                        )?;
                        terminal.show_cursor()?;
                        // SAFETY: raise(SIGTSTP) is safe to call; it suspends the process
                        // and returns when the process is resumed via SIGCONT (fg).
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
                    // Global: Ctrl+P cycles to next task with pending permissions
                    // (including AskUser prompts, which are stored as permissions)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('p')
                    {
                        let names = app.tasks_with_permissions();
                        if !names.is_empty() {
                            app.save_current_input();
                            let current = app.focused_perm_key();
                            let idx = names
                                .iter()
                                .position(|n| n == &current)
                                .map(|i| (i + 1) % names.len())
                                .unwrap_or(0);
                            let name = names[idx].clone();
                            if name == EXO_PERM_KEY {
                                // Navigate to ExO view
                                if app.active_project_id.is_some() {
                                    app.save_project_state();
                                    app.pm_messages.clear();
                                    if let Ok(tasks) = service.list_visible(None) {
                                        app.refresh_tasks(tasks);
                                    }
                                }
                                app.show_detail = false;
                            } else if let Some(pos) = app.tasks.iter().position(|t| t.name == name)
                            {
                                // Task is in the current project view
                                app.list_state.select(Some(pos));
                                app.show_detail = true;
                                app.detail_scroll = 0;
                            } else {
                                // Task is in a different project — switch to it
                                let target_pid =
                                    app.global_task_projects.get(&name).cloned().flatten();
                                // Save current project state before switching
                                app.save_project_state();
                                if let Some(pid) = target_pid {
                                    // Switch to the target project
                                    if let Ok(projects) = service.list_projects() {
                                        app.projects = projects;
                                    }
                                    let proj_name = app
                                        .projects
                                        .iter()
                                        .find(|p| p.id == pid)
                                        .map(|p| p.name.clone())
                                        .unwrap_or_else(|| pid.clone());
                                    app.active_project = Some(proj_name);
                                    app.active_project_id = Some(pid.clone());
                                    app.show_projects = false;
                                    if let Ok(tasks) = service.list_visible(Some(&pid)) {
                                        app.refresh_tasks(tasks);
                                    }
                                    if let Some(pos) = app.tasks.iter().position(|t| t.name == name)
                                    {
                                        app.list_state.select(Some(pos));
                                        app.show_detail = true;
                                        app.detail_scroll = 0;
                                    }
                                    // Set up PM context for the target project
                                    ensure_pm_context(pm_contexts, app, service, &pid, pm_tx);
                                } else {
                                    // Target is the default (ExO) view
                                    app.active_project = None;
                                    app.active_project_id = None;
                                    app.show_projects = false;
                                    app.pm_messages.clear();
                                    if let Ok(tasks) = service.list_visible(None) {
                                        app.refresh_tasks(tasks);
                                    }
                                    if let Some(pos) = app.tasks.iter().position(|t| t.name == name)
                                    {
                                        app.list_state.select(Some(pos));
                                        app.show_detail = true;
                                        app.detail_scroll = 0;
                                    }
                                }
                            }
                            app.focus = Focus::ChatInput;
                            app.chat_scroll = 0;
                            app.restore_input();
                        } else if app.list_state.selected().is_some() {
                            app.save_current_input();
                            app.show_detail = true;
                            app.detail_scroll = 0;
                            app.focus = Focus::ChatInput;
                            app.chat_scroll = 0;
                            app.restore_input();
                        }
                    // Ctrl+Y one-time allow (no updatedPermissions)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('y')
                        && app.show_detail
                        && app.peek_permission(&app.focused_perm_key()).is_some()
                    {
                        if let Some((stream, allow, _suggestions, perm_id)) =
                            resolve_permission(app, true)
                        {
                            let _ = write_response_to_stream(stream, allow, None);
                            notify_tg_resolved(tg_tx, perm_id, "✅ Approved locally");
                        }
                    // Ctrl+T trust / always-allow (with updatedPermissions)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('t')
                        && app.show_detail
                        && app.peek_permission(&app.focused_perm_key()).is_some()
                    {
                        if let Some((stream, allow, suggestions, perm_id)) =
                            resolve_permission(app, true)
                        {
                            let _ = write_response_to_stream(stream, allow, Some(&suggestions));
                            notify_tg_resolved(tg_tx, perm_id, "✅ Trusted locally");
                        }
                    // Ctrl+N denies permission (only when focused task has one)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('n')
                        && app.show_detail
                        && app.peek_permission(&app.focused_perm_key()).is_some()
                    {
                        if let Some((stream, allow, _suggestions, perm_id)) =
                            resolve_permission(app, false)
                        {
                            let _ = write_response_to_stream(stream, allow, None);
                            notify_tg_resolved(tg_tx, perm_id, "❌ Denied locally");
                        }
                    // Number keys 1-4 answer an AskUser prompt (front permission is AskUser)
                    } else if matches!(key.code, KeyCode::Char('1'..='4'))
                        && key.modifiers.is_empty()
                        && app.show_detail
                        && app
                            .peek_permission(&app.focused_perm_key())
                            .is_some_and(|p| p.is_askuser())
                    {
                        let digit = match key.code {
                            KeyCode::Char(c) => c.to_digit(10).unwrap_or(1) as usize,
                            _ => 1,
                        };
                        let perm_key = app.focused_perm_key();
                        if let Some(perm) = app.peek_permission(&perm_key) {
                            let idx = digit - 1;
                            if idx < perm.askuser_options.len() {
                                let label = perm.askuser_options[idx].0.clone();
                                let perm_id = perm.perm_id;
                                if let Some(perm) = app.take_permission(&perm_key) {
                                    let _ = write_response_with_message(perm.stream, true, &label);
                                    notify_tg_resolved(
                                        tg_tx,
                                        perm_id,
                                        &format!("✅ Selected: {label}"),
                                    );
                                }
                            }
                        }
                    // Global: Ctrl+O returns to ExO chat
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('o')
                    {
                        app.save_current_input();
                        // Remember last project + view state so Ctrl+R can restore it
                        app.save_project_state();
                        app.show_detail = false;
                        app.show_projects = false;
                        app.pm_messages.clear();
                        if let Ok(tasks) = service.list_visible(None) {
                            app.refresh_tasks(tasks);
                        }
                        app.focus = Focus::ChatInput;
                        app.chat_scroll = 0;
                        app.restore_input();
                    // Global: Ctrl+R returns to PM (or last active project from ExO)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('r')
                    {
                        // If in a project's task detail, go back to PM chat
                        if app.show_detail && app.active_project_id.is_some() {
                            app.save_current_input();
                            app.show_detail = false;
                            app.focus = Focus::ChatInput;
                            app.chat_scroll = 0;
                            app.restore_input();
                        // If in ExO view, restore last active project (or first project)
                        } else if app.active_project_id.is_none() {
                            // Refresh project list
                            if let Ok(projects) = service.list_projects() {
                                app.projects = projects;
                            }
                            // Pick target: last visited project, or first available
                            let target = app
                                .last_project
                                .take()
                                .map(|s| (s.name, s.id, s.show_detail, s.selected_task_name))
                                .or_else(|| {
                                    app.projects
                                        .first()
                                        .map(|p| (p.name.clone(), p.id.clone(), false, None))
                                });
                            if let Some((name, id, saved_show_detail, saved_task_name)) = target {
                                app.save_current_input();
                                app.active_project = Some(name);
                                app.active_project_id = Some(id.clone());
                                app.show_projects = false;
                                if let Ok(tasks) = service.list_visible(Some(&id)) {
                                    app.refresh_tasks(tasks);
                                }
                                // Restore the view state the user had when they left
                                if saved_show_detail {
                                    if let Some(ref task_name) = saved_task_name
                                        && let Some(idx) =
                                            app.tasks.iter().position(|t| &t.name == task_name)
                                    {
                                        app.list_state.select(Some(idx));
                                        app.show_detail = true;
                                        app.detail_scroll = 0;
                                    } else {
                                        app.show_detail = false;
                                    }
                                } else {
                                    app.show_detail = false;
                                }
                                // Restore PM state
                                ensure_pm_context(pm_contexts, app, service, &id, pm_tx);
                                app.focus = Focus::ChatInput;
                                app.chat_scroll = 0;
                                app.restore_input();
                            }
                        // If in a project PM view, cycle to next project
                        } else if app.active_project_id.is_some() && !app.show_detail {
                            app.save_current_input();
                            // Refresh project list and find the next one
                            if let Ok(projects) = service.list_projects() {
                                app.projects = projects;
                            }
                            let cur_idx = app
                                .active_project_id
                                .as_deref()
                                .and_then(|pid| app.projects.iter().position(|p| p.id == pid));
                            if let Some(ci) = cur_idx {
                                let next_idx = (ci + 1) % app.projects.len();
                                let next = &app.projects[next_idx];
                                let next_id = next.id.clone();
                                let next_name = next.name.clone();
                                app.active_project = Some(next_name);
                                app.active_project_id = Some(next_id.clone());
                                app.show_detail = false;
                                app.show_projects = false;
                                if let Ok(tasks) = service.list_visible(Some(&next_id)) {
                                    app.refresh_tasks(tasks);
                                }
                                ensure_pm_context(pm_contexts, app, service, &next_id, pm_tx);
                                app.focus = Focus::ChatInput;
                                app.chat_scroll = 0;
                                app.restore_input();
                            }
                        }
                    } else {
                        match &app.focus {
                            Focus::TaskList => match key.code {
                                KeyCode::Char('q') => app.should_quit = true,
                                KeyCode::Esc => {
                                    app.show_detail = false;
                                    app.focus = Focus::ChatInput;
                                    app.restore_input();
                                }
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.next();
                                    app.show_detail = true;
                                    app.detail_scroll = 0;
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.previous();
                                    app.show_detail = true;
                                    app.detail_scroll = 0;
                                }
                                KeyCode::PageDown => {
                                    app.detail_scroll = app.detail_scroll.saturating_add(10);
                                }
                                KeyCode::PageUp => {
                                    app.detail_scroll = app.detail_scroll.saturating_sub(10);
                                }
                                KeyCode::Char('d')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    app.detail_scroll = app.detail_scroll.saturating_add(10);
                                }
                                KeyCode::Char('u')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    app.detail_scroll = app.detail_scroll.saturating_sub(10);
                                }
                                KeyCode::Char('g')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    if let Some(task) = app.selected_task() {
                                        if task.status.is_running() {
                                            if let Some(window_id) = &task.tmux_window {
                                                service.goto_window(window_id);
                                            }
                                        } else {
                                            let id = task.id.as_str().to_string();
                                            match service.reopen(&id) {
                                                Ok(window_id) => {
                                                    if let Ok(tasks) = service.list_visible(
                                                        app.active_project_id.as_deref(),
                                                    ) {
                                                        app.refresh_tasks(tasks);
                                                    }
                                                    service.goto_window(&window_id);
                                                }
                                                Err(e) => {
                                                    app.status_error = Some(format!("reopen: {e}"));
                                                }
                                            }
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if app.selected_task().is_some() {
                                        app.show_detail = true;
                                        app.detail_scroll = 0;
                                        app.focus = Focus::ChatInput;
                                        app.chat_scroll = 0;
                                        app.restore_input();
                                    }
                                }
                                KeyCode::Char('x') => {
                                    if let Some(task) = app.selected_task()
                                        && task.status.is_running()
                                    {
                                        let id = task.id.clone();
                                        app.focus = Focus::ConfirmCloseTask(id);
                                    }
                                }
                                KeyCode::Char('n') => {
                                    app.input.take();
                                    app.focus = Focus::SpawnInput;
                                }
                                KeyCode::Char('r') => {
                                    if let Some(task) = app.selected_task()
                                        && !task.status.is_running()
                                    {
                                        let id = task.id.as_str().to_string();
                                        match service.reopen(&id) {
                                            Ok(_) => {
                                                if let Ok(tasks) = service
                                                    .list_visible(app.active_project_id.as_deref())
                                                {
                                                    app.refresh_tasks(tasks);
                                                }
                                            }
                                            Err(e) => {
                                                app.status_error = Some(format!("reopen: {e}"));
                                            }
                                        }
                                    }
                                }
                                KeyCode::Backspace => {
                                    if let Some(task) = app.selected_task() {
                                        let id = task.id.clone();
                                        app.focus = Focus::ConfirmDelete(id);
                                    }
                                }
                                KeyCode::Char('/') => {
                                    app.search_input.take();
                                    app.update_search_filter();
                                    app.focus = Focus::TaskSearch;
                                }
                                KeyCode::Tab => {
                                    app.focus = Focus::ChatInput;
                                    app.restore_input();
                                }
                                KeyCode::Char('h')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    app.focus = Focus::ChatInput;
                                    app.restore_input();
                                }
                                KeyCode::Char('p') => {
                                    if let Ok(projects) = service.list_projects() {
                                        app.projects = projects;
                                        if !app.projects.is_empty() {
                                            app.project_list_state.select(Some(0));
                                        }
                                    }
                                    app.show_projects = true;
                                    app.focus = Focus::ProjectList;
                                }
                                _ => {}
                            },
                            Focus::TaskSearch => {
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                let searching_projects = app.show_projects;
                                let do_filter = |app: &mut App| {
                                    if app.show_projects {
                                        app.update_project_search_filter();
                                    } else {
                                        app.update_search_filter();
                                    }
                                };
                                match key.code {
                                    KeyCode::Esc => {
                                        app.search_input.take();
                                        if searching_projects {
                                            app.filtered_project_indices.clear();
                                            if !app.projects.is_empty() {
                                                let sel = app
                                                    .project_list_state
                                                    .selected()
                                                    .unwrap_or(0)
                                                    .min(app.projects.len() - 1);
                                                app.project_list_state.select(Some(sel));
                                            }
                                            app.focus = Focus::ProjectList;
                                        } else {
                                            app.filtered_indices.clear();
                                            if !app.tasks.is_empty() {
                                                let sel = app
                                                    .list_state
                                                    .selected()
                                                    .unwrap_or(0)
                                                    .min(app.tasks.len() - 1);
                                                app.list_state.select(Some(sel));
                                            }
                                            app.focus = Focus::TaskList;
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if searching_projects {
                                            if let Some(real_idx) =
                                                app.selected_filtered_project_index()
                                            {
                                                app.project_list_state.select(Some(real_idx));
                                            }
                                            app.search_input.take();
                                            app.filtered_project_indices.clear();
                                            app.focus = Focus::ProjectList;
                                        } else {
                                            if let Some(real_idx) =
                                                app.selected_filtered_task_index()
                                            {
                                                app.list_state.select(Some(real_idx));
                                                app.show_detail = true;
                                                app.detail_scroll = 0;
                                                app.focus = Focus::ChatInput;
                                                app.chat_scroll = 0;
                                                app.restore_input();
                                            } else {
                                                app.focus = Focus::TaskList;
                                            }
                                            app.search_input.take();
                                            app.filtered_indices.clear();
                                        }
                                    }
                                    KeyCode::Down | KeyCode::Tab => {
                                        if searching_projects {
                                            app.search_next_project();
                                        } else {
                                            app.search_next();
                                        }
                                    }
                                    KeyCode::Up | KeyCode::BackTab => {
                                        if searching_projects {
                                            app.search_prev_project();
                                        } else {
                                            app.search_prev();
                                        }
                                    }
                                    KeyCode::Char('n') if ctrl => {
                                        if searching_projects {
                                            app.search_next_project();
                                        } else {
                                            app.search_next();
                                        }
                                    }
                                    KeyCode::Char('p') if ctrl => {
                                        if searching_projects {
                                            app.search_prev_project();
                                        } else {
                                            app.search_prev();
                                        }
                                    }
                                    // Standard input controls
                                    KeyCode::Backspace => {
                                        app.search_input.backspace();
                                        do_filter(app);
                                    }
                                    KeyCode::Delete => {
                                        app.search_input.delete();
                                        do_filter(app);
                                    }
                                    KeyCode::Left => app.search_input.left(),
                                    KeyCode::Right => app.search_input.right(),
                                    KeyCode::Home | KeyCode::Char('a') if ctrl => {
                                        app.search_input.home();
                                    }
                                    KeyCode::End | KeyCode::Char('e') if ctrl => {
                                        app.search_input.end();
                                    }
                                    KeyCode::Char('u') if ctrl => {
                                        app.search_input.kill_before();
                                        do_filter(app);
                                    }
                                    KeyCode::Char('k') if ctrl => {
                                        app.search_input.kill_line();
                                        do_filter(app);
                                    }
                                    KeyCode::Char('w') if ctrl => {
                                        app.search_input.kill_word();
                                        do_filter(app);
                                    }
                                    KeyCode::Char(c) if !ctrl => {
                                        app.search_input.insert(c);
                                        do_filter(app);
                                    }
                                    _ => {}
                                }
                            }
                            Focus::ProjectList => match key.code {
                                KeyCode::Char('q') => app.should_quit = true,
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.next_project();
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.previous_project();
                                }
                                KeyCode::Char('/') => {
                                    app.search_input.take();
                                    app.update_project_search_filter();
                                    app.focus = Focus::TaskSearch;
                                }
                                KeyCode::Enter => {
                                    if let Some(project) = app.selected_project() {
                                        let project_id = project.id.clone();
                                        let project_name = project.name.clone();
                                        app.active_project = Some(project_name);
                                        app.active_project_id = Some(project_id.clone());
                                        app.show_projects = false;
                                        app.show_detail = false;
                                        app.focus = Focus::ChatInput;
                                        app.chat_scroll = 0;
                                        // Load tasks for this project
                                        if let Ok(tasks) = service.list_visible(Some(&project_id)) {
                                            app.refresh_tasks(tasks);
                                        }
                                        // Set up PM state for this project
                                        ensure_pm_context(
                                            pm_contexts,
                                            app,
                                            service,
                                            &project_id,
                                            pm_tx,
                                        );
                                        app.restore_input();
                                    }
                                }
                                KeyCode::Char('n') => {
                                    app.input.take();
                                    app.focus = Focus::ProjectNameInput;
                                }
                                KeyCode::Backspace => {
                                    if let Some(project) = app.selected_project() {
                                        let name = project.name.clone();
                                        app.focus = Focus::ConfirmDeleteProject(name);
                                    }
                                }
                                KeyCode::Char('p') | KeyCode::Esc => {
                                    // Go back to default task view
                                    app.show_projects = false;
                                    app.active_project = None;
                                    app.active_project_id = None;
                                    app.focus = Focus::TaskList;
                                    if let Ok(tasks) = service.list_visible(None) {
                                        app.refresh_tasks(tasks);
                                    }
                                }
                                _ => {}
                            },
                            Focus::SpawnInput => {
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        app.input.take();
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() {
                                            let name = app.input.take();
                                            let root = service.project_root().to_path_buf();
                                            let _ = service.spawn(SpawnRequest {
                                                task_name: &name,
                                                skill_name: "engineer",
                                                params: vec![("task".to_string(), name.clone())],
                                                work_dir_mode: WorkDirMode::Worktree {
                                                    repo: &root,
                                                    branch: None,
                                                },
                                                prompt_mode: PromptMode::Full,
                                                project_id: app.active_project_id.clone(),
                                            });
                                            if let Ok(tasks) = service
                                                .list_visible(app.active_project_id.as_deref())
                                            {
                                                app.refresh_tasks(tasks);
                                            }
                                        }
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('k') if ctrl => app.input.kill_line(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
                                    KeyCode::Char('e') if ctrl => app.input.end(),
                                    KeyCode::Char(c) => app.input.insert(c),
                                    KeyCode::Backspace => app.input.backspace(),
                                    KeyCode::Delete => app.input.delete(),
                                    KeyCode::Left => app.input.left(),
                                    KeyCode::Right => app.input.right(),
                                    KeyCode::Home => app.input.home(),
                                    KeyCode::End => app.input.end(),
                                    _ => {}
                                }
                            }
                            Focus::ProjectNameInput => {
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        app.input.take();
                                        app.focus = Focus::ProjectList;
                                    }
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() {
                                            let name = app.input.take();
                                            match service.create_project(&name, "") {
                                                Ok(project) => {
                                                    // Enter the new project
                                                    let project_id = project.id.clone();
                                                    app.active_project = Some(project.name.clone());
                                                    app.active_project_id =
                                                        Some(project_id.clone());
                                                    app.show_projects = false;
                                                    app.show_detail = false;
                                                    app.chat_scroll = 0;
                                                    if let Ok(tasks) =
                                                        service.list_visible(Some(&project_id))
                                                    {
                                                        app.refresh_tasks(tasks);
                                                    }
                                                    // New project — PM context created lazily on first message
                                                    app.focus = Focus::ChatInput;
                                                    app.restore_input();
                                                }
                                                Err(e) => {
                                                    app.status_error =
                                                        Some(format!("create project: {e}"));
                                                    app.focus = Focus::ProjectList;
                                                }
                                            }
                                        } else {
                                            app.focus = Focus::ProjectList;
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('k') if ctrl => app.input.kill_line(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
                                    KeyCode::Char('e') if ctrl => app.input.end(),
                                    KeyCode::Char(c) => app.input.insert(c),
                                    KeyCode::Backspace => app.input.backspace(),
                                    KeyCode::Delete => app.input.delete(),
                                    KeyCode::Left => app.input.left(),
                                    KeyCode::Right => app.input.right(),
                                    KeyCode::Home => app.input.home(),
                                    KeyCode::End => app.input.end(),
                                    _ => {}
                                }
                            }
                            Focus::ChatInput if app.show_detail => {
                                // Task chat mode — buffer input, send on Enter
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        app.save_current_input();
                                        app.show_detail = false;
                                        app.chat_scroll = 0;
                                        app.restore_input();
                                    }
                                    KeyCode::Tab => {
                                        app.save_current_input();
                                        app.chat_scroll = 0;
                                        let current = app.list_state.selected().unwrap_or(0);
                                        if current + 1 < app.tasks.len() {
                                            app.list_state.select(Some(current + 1));
                                            app.detail_scroll = 0;
                                        } else {
                                            app.show_detail = false;
                                        }
                                        app.restore_input();
                                    }
                                    KeyCode::BackTab => {
                                        app.save_current_input();
                                        app.chat_scroll = 0;
                                        let current = app.list_state.selected().unwrap_or(0);
                                        if current > 0 {
                                            app.list_state.select(Some(current - 1));
                                            app.detail_scroll = 0;
                                        } else {
                                            app.show_detail = false;
                                        }
                                        app.restore_input();
                                    }
                                    KeyCode::Char('k') if ctrl => {
                                        app.focus = Focus::ChatHistory;
                                    }
                                    KeyCode::Char('x') if ctrl => {
                                        if let Some(task) = app.selected_task()
                                            && task.status.is_running()
                                        {
                                            let id = task.id.clone();
                                            app.focus = Focus::ConfirmCloseTask(id);
                                        }
                                    }
                                    KeyCode::Char('l') if ctrl => {
                                        app.save_current_input();
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Char('g') if ctrl => {
                                        if let Some(task) = app.selected_task() {
                                            if task.status.is_running() {
                                                if let Some(window_id) = &task.tmux_window {
                                                    service.goto_window(window_id);
                                                }
                                            } else {
                                                let id = task.id.as_str().to_string();
                                                match service.reopen(&id) {
                                                    Ok(window_id) => {
                                                        if let Ok(tasks) = service.list_visible(
                                                            app.active_project_id.as_deref(),
                                                        ) {
                                                            app.refresh_tasks(tasks);
                                                        }
                                                        service.goto_window(&window_id);
                                                    }
                                                    Err(e) => {
                                                        app.status_error =
                                                            Some(format!("reopen: {e}"));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() {
                                            let msg = app.input.take();
                                            let pane_id = app
                                                .selected_task()
                                                .and_then(|t| t.tmux_pane.clone());
                                            if let Some(pane) = pane_id.as_deref() {
                                                service.forward_literal(pane, &msg);
                                                service.forward_key(pane, "Enter");
                                                // Message sent → agent will start working.
                                                app.idle_panes.remove(pane);
                                            }
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
                                    KeyCode::Char('e') if ctrl => app.input.end(),
                                    KeyCode::Char(c) => app.input.insert(c),
                                    KeyCode::Backspace => app.input.backspace(),
                                    KeyCode::Delete => app.input.delete(),
                                    KeyCode::Left => app.input.left(),
                                    KeyCode::Right => app.input.right(),
                                    KeyCode::Home => app.input.home(),
                                    KeyCode::End => app.input.end(),
                                    _ => {}
                                }
                            }
                            Focus::ChatInput => {
                                // ExO / PM chat mode
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        if exo.streaming {
                                            exo.finish_streaming();
                                        }
                                        if let Some(ref pid) = app.active_project_id
                                            && let Some(ctx) = pm_contexts.get_mut(pid.as_str())
                                            && ctx.state.streaming
                                        {
                                            ctx.state.finish_streaming();
                                        }
                                        app.chat_scroll = 0;
                                    }
                                    KeyCode::Tab => {
                                        app.save_current_input();
                                        app.chat_scroll = 0;
                                        if !app.tasks.is_empty() {
                                            app.list_state.select(Some(0));
                                            app.show_detail = true;
                                            app.detail_scroll = 0;
                                        }
                                        app.restore_input();
                                    }
                                    KeyCode::BackTab => {
                                        app.save_current_input();
                                        app.chat_scroll = 0;
                                        if !app.tasks.is_empty() {
                                            app.list_state.select(Some(app.tasks.len() - 1));
                                            app.show_detail = true;
                                            app.detail_scroll = 0;
                                        }
                                        app.restore_input();
                                    }
                                    KeyCode::Char('k') if ctrl => {
                                        app.focus = Focus::ChatHistory;
                                    }
                                    KeyCode::Char('x') if ctrl && app.active_project.is_some() => {
                                        app.focus = Focus::ConfirmCloseProject;
                                    }
                                    KeyCode::Char('l') if ctrl => {
                                        app.save_current_input();
                                        app.focus = Focus::TaskList;
                                        if app.list_state.selected().is_some() {
                                            app.show_detail = true;
                                            app.detail_scroll = 0;
                                        }
                                    }
                                    KeyCode::Enter => {
                                        app.chat_scroll = 0;
                                        if !app.input.is_empty() {
                                            if let Some(ref pid) = app.active_project_id {
                                                let pid = pid.clone();
                                                // Ensure PM context exists
                                                if !pm_contexts.contains_key(&pid) {
                                                    pm_contexts.insert(
                                                        pid.clone(),
                                                        PmContext::new(&pid, pm_tx),
                                                    );
                                                }
                                                let ctx = pm_contexts.get_mut(&pid).unwrap();
                                                // PM chat: finish any in-progress streaming
                                                if ctx.state.streaming {
                                                    ctx.state.finish_streaming();
                                                    if let Some(msg) = ctx.state.messages.last()
                                                        && matches!(
                                                            msg.role,
                                                            MessageRole::Assistant
                                                        )
                                                        && msg.has_text()
                                                    {
                                                        let _ = service.insert_pm_message(
                                                            &pid,
                                                            MessageRole::Assistant,
                                                            &msg.text_content(),
                                                        );
                                                    }
                                                }
                                                let msg = app.input.take();
                                                let _ = service.insert_pm_message(
                                                    &pid,
                                                    MessageRole::User,
                                                    &msg,
                                                );
                                                ctx.state.add_user_message(msg.clone());
                                                // Lazily create PM session
                                                if ctx.session.is_none() {
                                                    let sid = service
                                                        .read_pm_session_id(&pid)
                                                        .or_else(|| ctx.state.session_id.clone());
                                                    let proj_name = app
                                                        .active_project
                                                        .as_deref()
                                                        .unwrap_or("unknown");
                                                    let prompt = pm_system_prompt(proj_name);
                                                    ctx.session = Some(ExoSession::new(
                                                        sid.as_deref(),
                                                        Arc::clone(&ctx.cancel),
                                                        ctx.bridge_tx.clone(),
                                                        &prompt,
                                                    ));
                                                    if let Some(ref s) = sid {
                                                        ctx.state.session_id = Some(s.clone());
                                                    }
                                                }
                                                if let Some(sess) = &mut ctx.session {
                                                    sess.send_message(
                                                        &msg,
                                                        ctx.state.session_id.as_deref(),
                                                    );
                                                    app.chat_scroll = 0;
                                                }
                                            } else {
                                                // ExO chat: stream to ExO
                                                // Finish any in-progress streaming from a previous turn
                                                if exo.streaming {
                                                    exo.finish_streaming();
                                                    // Persist the previous assistant response
                                                    if let Some(msg) = exo.messages.last()
                                                        && matches!(
                                                            msg.role,
                                                            MessageRole::Assistant
                                                        )
                                                        && msg.has_text()
                                                    {
                                                        let _ = service.insert_exo_message(
                                                            MessageRole::Assistant,
                                                            &msg.text_content(),
                                                        );
                                                    }
                                                }
                                                let msg = app.input.take();
                                                let _ = service
                                                    .insert_exo_message(MessageRole::User, &msg);
                                                exo.add_user_message(msg.clone());
                                                exo_session
                                                    .send_message(&msg, exo.session_id.as_deref());
                                                app.chat_scroll = 0;
                                            }
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
                                    KeyCode::Char('e') if ctrl => app.input.end(),
                                    KeyCode::Char(c) => app.input.insert(c),
                                    KeyCode::Backspace => app.input.backspace(),
                                    KeyCode::Delete => app.input.delete(),
                                    KeyCode::Left => app.input.left(),
                                    KeyCode::Right => app.input.right(),
                                    KeyCode::Home => app.input.home(),
                                    KeyCode::End => app.input.end(),
                                    _ => {}
                                }
                            }
                            Focus::ChatHistory => {
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Char('j') if ctrl => {
                                        app.focus = Focus::ChatInput;
                                    }
                                    KeyCode::Char('u') if ctrl => {
                                        let half = (app.chat_viewport_height / 2).max(1);
                                        app.chat_scroll = app.chat_scroll.saturating_add(half);
                                    }
                                    KeyCode::Char('d') if ctrl => {
                                        let half = (app.chat_viewport_height / 2).max(1);
                                        app.chat_scroll = app.chat_scroll.saturating_sub(half);
                                    }
                                    KeyCode::Char('l') if ctrl => {
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Esc => {
                                        app.focus = Focus::ChatInput;
                                        app.chat_scroll = 0;
                                    }
                                    _ => {}
                                }
                            }
                            Focus::ConfirmDelete(task_id) => match key.code {
                                KeyCode::Char('y') => {
                                    let id = task_id.clone();
                                    let _ = service.delete(id.as_str());
                                    if let Ok(tasks) =
                                        service.list_visible(app.active_project_id.as_deref())
                                    {
                                        app.refresh_tasks(tasks);
                                    }
                                    app.focus = Focus::TaskList;
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    app.focus = Focus::TaskList;
                                }
                                _ => {}
                            },
                            Focus::ConfirmCloseTask(task_id) => match key.code {
                                KeyCode::Char('y') => {
                                    let id = task_id.clone();
                                    let _ = service.close(id.as_str());
                                    if let Ok(tasks) =
                                        service.list_visible(app.active_project_id.as_deref())
                                    {
                                        app.refresh_tasks(tasks);
                                    }
                                    app.show_detail = false;
                                    app.focus = Focus::ChatInput;
                                    app.restore_input();
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    app.focus = if app.show_detail {
                                        Focus::ChatInput
                                    } else {
                                        Focus::TaskList
                                    };
                                }
                                _ => {}
                            },
                            Focus::ConfirmDeleteProject(project_name) => match key.code {
                                KeyCode::Char('y') => {
                                    let name = project_name.clone();
                                    let _ = service.delete_project(&name);
                                    // Refresh project list
                                    if let Ok(projects) = service.list_projects() {
                                        app.projects = projects;
                                        if app.projects.is_empty() {
                                            app.project_list_state.select(None);
                                        } else {
                                            let sel = app
                                                .project_list_state
                                                .selected()
                                                .unwrap_or(0)
                                                .min(app.projects.len().saturating_sub(1));
                                            app.project_list_state.select(Some(sel));
                                        }
                                    }
                                    app.focus = Focus::ProjectList;
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    app.focus = Focus::ProjectList;
                                }
                                _ => {}
                            },
                            Focus::ConfirmCloseProject => match key.code {
                                KeyCode::Char('y') => {
                                    let closed_pid = app.active_project_id.take();
                                    app.active_project = None;
                                    app.save_current_input();
                                    if let Some(pid) = closed_pid {
                                        cancel_pm_context(pm_contexts, &pid);
                                    }
                                    app.focus = Focus::TaskList;
                                    if let Ok(tasks) = service.list_visible(None) {
                                        app.refresh_tasks(tasks);
                                    }
                                }
                                KeyCode::Char('n') | KeyCode::Esc => {
                                    app.focus = Focus::ChatInput;
                                }
                                _ => {}
                            },
                        }
                    }
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if let Ok(tasks) = service.list_visible(app.active_project_id.as_deref()) {
                app.refresh_tasks(tasks);
            }
            // Refresh idle pane detection every 2 seconds (single list-panes call).
            if last_activity_check.elapsed() >= Duration::from_secs(2) {
                let running_pane_ids: Vec<String> = app
                    .tasks
                    .iter()
                    .filter(|t| t.status.is_running())
                    .filter_map(|t| t.tmux_pane.clone())
                    .collect();
                let pane_refs: Vec<&str> = running_pane_ids.iter().map(|s| s.as_str()).collect();
                app.idle_panes = crate::runtime::idle_panes(&pane_refs);
                last_activity_check = Instant::now();
            }
            // Update global task→project mapping and drain stale permissions.
            // Uses the full (unscoped) active task list so permissions for tasks
            // in other projects aren't incorrectly drained or miscounted.
            let all_active = service.list_active().unwrap_or_default();
            let all_running_names: std::collections::HashSet<String> =
                all_active.iter().map(|t| t.name.clone()).collect();
            app.global_task_projects = all_active
                .iter()
                .map(|t| (t.name.clone(), t.project_id.clone()))
                .collect();
            app.global_task_work_dirs = all_active
                .iter()
                .filter_map(|t| t.work_dir.as_ref().map(|wd| (t.name.clone(), wd.clone())))
                .collect();
            for perm in app.drain_stale_permissions(&all_running_names) {
                notify_tg_resolved(tg_tx, perm.perm_id, "⚪ Expired (task ended)");
                let _ = write_response_to_stream(perm.stream, false, None);
            }
            app.window_numbers = crate::runtime::tmux_window_numbers();
            // Update selected messages and live output for detail view
            if let Some(task) = app.selected_task() {
                let task_id = task.id.as_str().to_string();
                let is_running = task.status.is_running();
                let pane = task.tmux_pane.clone();
                if let Ok(messages) = service.messages(&task_id) {
                    app.selected_messages = messages;
                }
                if is_running {
                    app.detail_live_output = pane.as_deref().and_then(|p| service.capture_pane(p));
                } else {
                    app.detail_live_output = None;
                }
            } else {
                app.selected_messages.clear();
                app.detail_live_output = None;
            }
            // Refresh PM messages for active project
            if let Some(ref pid) = app.active_project_id {
                if let Ok(messages) = service.pm_messages(pid) {
                    app.pm_messages = messages;
                }
            } else {
                app.pm_messages.clear();
            }
            last_tick = Instant::now();
        }

        // Handle "resolved" notifications from PostToolUse hooks.
        // When a tool executes (approved in agent pane or elsewhere), clear
        // the matching pending permission and respond to unblock the hook.
        while let Ok(cwd) = resolved_rx.try_recv() {
            let resolved_cwd =
                std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
            let task_name = find_task_name_by_cwd(&app.global_task_work_dirs, &resolved_cwd);
            if let Some(name) = task_name {
                // Tool executed in-pane → task is active.
                if let Some(pane_id) = app
                    .tasks
                    .iter()
                    .find(|t| t.name == name)
                    .and_then(|t| t.tmux_pane.as_deref())
                {
                    app.idle_panes.remove(pane_id);
                }
                // Drain ALL pending permissions for this task — respond with allow
                // so the PermissionRequest hook processes can exit cleanly.
                while let Some(perm) = app.take_permission(&name) {
                    notify_tg_resolved(tg_tx, perm.perm_id, "✅ Resolved (tool executed)");
                    let _ = write_response_to_stream(perm.stream, true, None);
                }
            }
        }

        // Drain idle notifications from Stop hooks — immediately mark pane as idle.
        while let Ok(cwd) = idle_rx.try_recv() {
            let cwd_path =
                std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
            if let Some(task_name) = find_task_name_by_cwd(&app.global_task_work_dirs, &cwd_path)
                && let Some(pane_id) = app
                    .tasks
                    .iter()
                    .find(|t| t.name == task_name)
                    .and_then(|t| t.tmux_pane.as_deref())
            {
                app.idle_panes.insert(pane_id.to_string());
            }
        }

        // Drain permission requests from socket — non-blocking, no focus change
        while let Ok((stream, req)) = perm_rx.try_recv() {
            let req_cwd = std::fs::canonicalize(&req.cwd)
                .unwrap_or_else(|_| std::path::PathBuf::from(&req.cwd));
            let task_name = find_task_name_by_cwd(&app.global_task_work_dirs, &req_cwd)
                .unwrap_or_else(|| EXO_PERM_KEY.to_string());
            // Permission request → task is actively working.
            if let Some(pane_id) = app
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_deref())
            {
                app.idle_panes.remove(pane_id);
            }
            perm_id_counter += 1;
            let perm_id = perm_id_counter;
            if let Some(tx) = tg_tx {
                // AskUserQuestion: send inline buttons for each option.
                if req.tool_name == "AskUserQuestion"
                    && let Some((question, options)) =
                        parse_ask_user_options(req.tool_input.as_ref())
                {
                    let _ = tx.send(telegram::TgOutbound::NewQuestion {
                        perm_id,
                        task_name: task_name.clone(),
                        question,
                        options,
                    });
                } else {
                    let _ = tx.send(telegram::TgOutbound::NewPermission {
                        perm_id,
                        task_name: task_name.clone(),
                        tool_name: req.tool_name.clone(),
                        tool_input_summary: req.tool_input_summary.clone(),
                    });
                }
                tg_perm_ids.insert(perm_id);
            }
            // Parse AskUser options when tool_name is AskUserQuestion
            let (askuser_question, askuser_options) = if req.tool_name == "AskUserQuestion"
                && let Some((q, opts)) = parse_ask_user_options(req.tool_input.as_ref())
            {
                (Some(q), opts)
            } else {
                (None, Vec::new())
            };
            let perm = ActivePermission {
                perm_id,
                stream,
                task_name: task_name.clone(),
                tool_name: req.tool_name,
                tool_input_summary: req.tool_input_summary,
                permission_suggestions: req.permission_suggestions,
                askuser_question,
                askuser_options,
            };
            app.add_permission(perm);
        }

        // Handle Telegram inbound messages (permissions + ExO chat).
        if let Some(rx) = tg_rx {
            while let Ok(tg_msg) = rx.try_recv() {
                match tg_msg {
                    telegram::TgInbound::PermissionDecision { perm_id, action } => {
                        // Find which task this perm_id belongs to.
                        let task_name = app
                            .pending_permissions
                            .iter()
                            .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                            .map(|(name, _)| name.clone());
                        if let Some(name) = task_name
                            && app
                                .peek_permission(&name)
                                .is_some_and(|front| front.perm_id == perm_id)
                            && let Some(perm) = app.take_permission(&name)
                        {
                            let (allow, suggestions) = match action {
                                telegram::PermAction::Approve => (true, None),
                                telegram::PermAction::Trust => {
                                    (true, Some(perm.permission_suggestions.clone()))
                                }
                                telegram::PermAction::Deny => (false, None),
                            };
                            let _ = write_response_to_stream(
                                perm.stream,
                                allow,
                                suggestions.as_deref(),
                            );
                        }
                    }
                    telegram::TgInbound::QuestionAnswer { perm_id, answer } => {
                        // Find which task this perm_id belongs to.
                        let task_name = app
                            .pending_permissions
                            .iter()
                            .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                            .map(|(name, _)| name.clone());
                        if let Some(name) = task_name
                            && app
                                .peek_permission(&name)
                                .is_some_and(|front| front.perm_id == perm_id)
                            && let Some(perm) = app.take_permission(&name)
                        {
                            let _ = write_response_with_message(perm.stream, true, &answer);
                        }
                    }
                    telegram::TgInbound::ExoMessage { text } => {
                        // Same flow as Enter-key ExO chat submission.
                        app.chat_scroll = 0;
                        if exo.streaming {
                            exo.finish_streaming();
                            if let Some(msg) = exo.messages.last()
                                && matches!(msg.role, MessageRole::Assistant)
                                && msg.has_text()
                            {
                                let _ = service.insert_exo_message(
                                    MessageRole::Assistant,
                                    &msg.text_content(),
                                );
                            }
                        }
                        let _ = service.insert_exo_message(MessageRole::User, &text);
                        exo.add_user_message(text.clone());
                        exo_session.send_message(&text, exo.session_id.as_deref());
                    }
                }
            }
        }

        // Detect permissions that disappeared from pending_permissions
        // without an explicit dashboard/Telegram resolution.  This catches
        // in-pane approvals where the PostToolUse hook didn't fire.
        // Sending a duplicate Resolved is safe — the bot thread's msg_map
        // deduplicates via remove().
        if !tg_perm_ids.is_empty() {
            let still_pending: std::collections::HashSet<u64> = app
                .pending_permissions
                .values()
                .flat_map(|q| q.iter().map(|p| p.perm_id))
                .collect();
            let vanished: Vec<u64> = tg_perm_ids
                .iter()
                .filter(|id| !still_pending.contains(id))
                .copied()
                .collect();
            for id in &vanished {
                notify_tg_resolved(tg_tx, *id, "✅ Resolved in pane");
                tg_perm_ids.remove(id);
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
