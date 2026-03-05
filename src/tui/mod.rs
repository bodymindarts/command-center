mod chat;
mod claude;
mod dashboard;
mod handlers;
mod input;
mod permissions;
mod telegram;
mod widgets;

use std::collections::HashMap;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::ClatApp;
use crate::permission::PermissionRequest;
use crate::primitives::ProjectId;
use crate::runtime::Runtime;
use chat::ExoState;
use claude::{EXO_SYSTEM_PROMPT, ExoEvent, ExoSession, PmEvent};
use dashboard::Dashboard;

/// Holds the PM state and session for a single project.
struct PmContext {
    state: ExoState,
    session: Option<ExoSession>,
    cancel: Arc<AtomicBool>,
    /// Sender that wraps ExoEvent → PmEvent with this project's ID.
    bridge_tx: mpsc::Sender<ExoEvent>,
}

impl PmContext {
    fn new(project_id: &ProjectId, pm_tx: &mpsc::Sender<PmEvent>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (bridge_tx, bridge_rx) = mpsc::channel::<ExoEvent>();
        let pid = project_id.clone();
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
fn cancel_pm_context(pm_contexts: &mut HashMap<ProjectId, PmContext>, project_id: &ProjectId) {
    if let Some(ctx) = pm_contexts.remove(project_id) {
        ctx.cancel.store(true, Ordering::Relaxed);
    }
}

/// Ensure a PmContext exists for the project, creating and loading history if needed.
fn ensure_pm_context<R: Runtime>(
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    app: &mut Dashboard,
    service: &ClatApp<R>,
    project_id: &ProjectId,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    if !pm_contexts.contains_key(project_id) {
        let mut ctx = PmContext::new(project_id, pm_tx);
        ctx.state.session_id = service.read_pm_session_id(project_id);
        if let Ok(messages) = service.pm_messages(project_id) {
            let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
            ctx.state.load_history(recent.into_iter().rev().collect());
        }
        pm_contexts.insert(project_id.clone(), ctx);
    }
    if let Ok(msgs) = service.pm_messages(project_id) {
        app.pm_messages = msgs;
    }
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

pub fn run<R: Runtime>(
    service: &ClatApp<R>,
    resume_session: Option<&str>,
    caffeinate: bool,
) -> anyhow::Result<()> {
    let mut caffeinate_child = if caffeinate { spawn_caffeinate() } else { None };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tasks = service.list_visible(None)?;
    let mut app = Dashboard::new(tasks);
    let mut exo = ExoState::new();
    if let Some(sid) = resume_session {
        exo.session_id = Some(sid.to_string());
    } else if let Some(sid) = service.read_exo_session_id() {
        exo.session_id = Some(sid);
    }
    if let Ok(messages) = service.exo_messages() {
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
    let mut pm_contexts: HashMap<ProjectId, PmContext> = HashMap::new();
    let (pm_tx, pm_rx) = mpsc::channel::<PmEvent>();

    // Permission socket listener
    let (perm_tx, perm_rx) = mpsc::channel::<(UnixStream, PermissionRequest)>();
    let (resolved_tx, resolved_rx) = mpsc::channel::<String>();
    let (idle_tx, idle_rx) = mpsc::channel::<String>();
    let (active_tx, active_rx) = mpsc::channel::<String>(); // CWD of active agent
    let perm_cancel = Arc::clone(&cancel);
    let (listener, socket_path) = crate::permission::start_socket_listener()?;
    // SAFETY: called once at startup before spawning threads that read env vars.
    unsafe {
        std::env::set_var(crate::permission::SOCKET_ENV, &socket_path);
    }
    crate::permission::write_socket_breadcrumb(service.project_root(), &socket_path);
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

                        if let Some(cwd) = crate::permission::parse_resolved_json(&buf) {
                            let _ = resolved_tx.send(cwd);
                        } else if let Some(cwd) = crate::permission::parse_idle_json(&buf) {
                            let _ = idle_tx.send(cwd);
                        } else if let Some(cwd) = crate::permission::parse_active_json(&buf) {
                            let _ = active_tx.send(cwd);
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
        &active_rx,
        tg_tx.as_ref(),
        tg_rx.as_ref(),
    );

    cancel.store(true, Ordering::Relaxed);
    for ctx in pm_contexts.values() {
        ctx.cancel.store(true, Ordering::Relaxed);
    }

    // Deny all pending permissions on exit
    for (_, mut queue) in app.permissions.drain_all() {
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

#[allow(clippy::too_many_arguments)]
fn run_loop<R: Runtime>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut Dashboard,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
    service: &ClatApp<R>,
    rx: &mpsc::Receiver<ExoEvent>,
    pm_rx: &mpsc::Receiver<PmEvent>,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
    resolved_rx: &mpsc::Receiver<String>,
    idle_rx: &mpsc::Receiver<String>,
    active_rx: &mpsc::Receiver<String>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_rx: Option<&mpsc::Receiver<telegram::TgInbound>>,
) -> anyhow::Result<()> {
    let mut last_tick = Instant::now();
    let mut perm_id_counter: u64 = 0;
    let mut tg_perm_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

    // Start with all running panes assumed idle. Notification hooks
    // (idle_prompt → idle, permission_prompt/elicitation_dialog → active)
    // and message-sent will flip state — no screen-capture polling needed.
    app.idle_panes = app
        .tasks
        .iter()
        .filter(|t| t.status.is_running())
        .filter_map(|t| t.tmux_pane.clone())
        .collect();

    loop {
        let any_pm_streaming = pm_contexts.values().any(|ctx| ctx.state.streaming);
        let tick_rate = if exo.streaming || any_pm_streaming {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };

        let active_pm_state = app
            .active_project_id
            .as_ref()
            .and_then(|pid| pm_contexts.get(pid))
            .map(|ctx| &ctx.state);
        terminal.draw(|frame| widgets::ui(frame, app, exo, active_pm_state))?;

        // Drain channel events
        handlers::drain_exo_events(exo, exo_session, service, rx, app, tg_tx);
        handlers::drain_pm_events(pm_contexts, service, pm_rx, app);

        // Poll terminal input
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
                Event::Paste(text) => handlers::handle_paste(app, text),
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.status_error = None;
                    // Ctrl+Z suspends — needs terminal access, handled inline
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('z')
                    {
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
                    } else if !handlers::handle_global_keys(
                        app,
                        key,
                        service,
                        pm_contexts,
                        pm_tx,
                        tg_tx,
                    ) {
                        handlers::handle_focus_key(
                            app,
                            key,
                            service,
                            exo,
                            exo_session,
                            pm_contexts,
                            pm_tx,
                        );
                    }
                }
                _ => {}
            }
        }

        // Periodic refresh
        if last_tick.elapsed() >= tick_rate {
            handlers::tick_refresh(app, service, tg_tx);
            last_tick = Instant::now();
        }

        // Drain socket events – permissions first so resolved notifications can find them.
        handlers::drain_permissions(app, perm_rx, tg_tx, &mut tg_perm_ids, &mut perm_id_counter);
        handlers::drain_resolved(app, resolved_rx, tg_tx, &mut tg_perm_ids);
        handlers::drain_idle(app, idle_rx);
        handlers::drain_active(app, active_rx);
        handlers::drain_telegram(app, exo, exo_session, service, tg_rx);
        handlers::detect_vanished_perms(app, tg_tx, &mut tg_perm_ids);

        if app.should_quit {
            return Ok(());
        }
    }
}
