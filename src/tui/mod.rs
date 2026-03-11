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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::ClatApp;
use crate::assistant::{AssistantSession, EXO_SYSTEM_PROMPT};
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
    fn new(session_id: Option<&str>, system_prompt: &str) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let session = AssistantSession::new(session_id, Arc::clone(&cancel), system_prompt);
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
fn build_project_state<R: Runtime>(app: &ClatApp<R>, project_id: &ProjectId) -> ProjectState {
    let mut assistant = chat::AssistantChat::new();
    assistant.session_id = app.read_project_session_id(project_id);
    if let Ok(messages) = app.session_messages(Some(project_id)) {
        let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
        assistant.load_history(recent.into_iter().rev().collect());
    }
    ProjectState::new(assistant, Vec::new())
}

/// Initialize a project: build its ProjectState, add it to ScreenState, return a ProjectContext.
fn init_project_context<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    project_id: &ProjectId,
    project_name: &str,
) -> ProjectContext {
    let project_state = build_project_state(app, project_id);
    let session_id = project_state.chat_view.assistant.session_id.clone();
    state.add_project(project_id.clone(), project_state);
    let prompt = crate::assistant::project_system_prompt(project_name);
    ProjectContext::new(session_id.as_deref(), &prompt)
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
    app: ClatApp<R>,
    resume_session: Option<&str>,
    caffeinate: bool,
) -> anyhow::Result<()> {
    let mut caffeinate_child = if caffeinate { spawn_caffeinate() } else { None };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.close_stale_tasks();
    let tasks = app.list_visible(None)?;
    let exo = {
        let mut assistant = chat::AssistantChat::new();
        if let Some(sid) = resume_session {
            assistant.session_id = Some(sid.to_string());
        } else if let Some(sid) = app.read_exo_session_id() {
            assistant.session_id = Some(sid);
        }
        if let Ok(messages) = app.session_messages(None) {
            let recent: Vec<_> = messages.into_iter().rev().take(20).collect();
            assistant.load_history(recent.into_iter().rev().collect());
        }
        ProjectState::new(assistant, tasks)
    };
    let keybindings = keybindings::Keybindings::load(&app.project_root().join("keybindings.toml"));
    let mut state = ScreenState::new(exo, keybindings);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut exo_session = AssistantSession::new(
        state.exo.chat_view.assistant.session_id.as_deref(),
        Arc::clone(&cancel),
        EXO_SYSTEM_PROMPT,
    );

    // Project contexts: one per project, keyed by project ID
    let mut project_contexts: HashMap<ProjectId, ProjectContext> = HashMap::new();

    // Boot all PM sessions eagerly so they warm up in the background.
    if let Ok(projects) = app.list_projects() {
        for project in &projects {
            project_contexts.insert(
                project.id.clone(),
                init_project_context(&mut state, &app, &project.id, project.name.as_str()),
            );
        }
    }

    // Hook event socket listener — single channel for all hook types
    let (hook_tx, hook_rx) = mpsc::channel::<(HookEvent, UnixStream)>();
    let perm_cancel = Arc::clone(&cancel);
    let (listener, socket_path) = crate::permission::start_socket_listener()?;
    // SAFETY: called once at startup before spawning threads that read env vars.
    unsafe {
        std::env::set_var(crate::permission::SOCKET_ENV, &socket_path);
    }
    crate::permission::write_socket_breadcrumb(app.project_root(), &socket_path);
    {
        let work_dirs: Vec<String> = state
            .exo
            .task_list
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

                        let event: HookEvent = serde_json::from_str(&buf).unwrap_or_else(|_| {
                            HookEvent::Unknown(serde_json::Value::String(buf.clone()))
                        });
                        let _ = hook_tx.send((event, stream));
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
        &mut state,
        &mut exo_session,
        &mut project_contexts,
        &app,
        &hook_rx,
        tg_tx.as_ref(),
        tg_rx.as_ref(),
    );

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
    state: &mut ScreenState,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>, // borrowed from run() which owns it
    hook_rx: &mpsc::Receiver<(HookEvent, UnixStream)>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_rx: Option<&mpsc::Receiver<telegram::TgInbound>>,
) -> anyhow::Result<()> {
    let mut last_tick = Instant::now();
    let mut perm_id_counter: u64 = 0;
    let mut tg_perm_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();

    state.render_loop_starting();

    loop {
        let tick_rate = if state.any_streaming() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };

        terminal.draw(|frame| widgets::ui(frame, state))?;

        // Drain channel events
        handlers::drain_events(exo_session, project_contexts, app, state, tg_tx);

        // Poll terminal input
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            match event::read()? {
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
                    } else if !handlers::handle_global_keys(state, key, app, tg_tx) {
                        handlers::handle_focus_key(state, key, app, exo_session, project_contexts);
                    }
                }
                _ => {}
            }
        }

        // Periodic refresh
        if last_tick.elapsed() >= tick_rate {
            handlers::tick_refresh(state, app, tg_tx);
            last_tick = Instant::now();
        }

        // Drain hook events from the socket listener
        handlers::drain_hooks(
            state,
            project_contexts,
            app,
            hook_rx,
            tg_tx,
            &mut tg_perm_ids,
            &mut perm_id_counter,
        );
        handlers::drain_telegram(state, exo_session, app, tg_rx);
        handlers::detect_vanished_perms(state, tg_tx, &mut tg_perm_ids);

        if state.should_quit() {
            return Ok(());
        }
    }
}
