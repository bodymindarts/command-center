mod app;
mod chat;
mod claude;
mod widgets;

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
use crate::service::TaskService;
use app::{ActivePermission, App, Focus};

const EXO_PERM_KEY: &str = "exo";
use chat::ExoState;
use claude::{ExoEvent, ExoSession};

pub fn run<R: Runtime>(service: &TaskService<R>, resume_session: Option<&str>) -> Result<()> {
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
    let mut exo_session =
        ExoSession::start(exo.session_id.as_deref(), Arc::clone(&cancel), tx.clone());

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
    if let Ok(tasks) = service.list_visible(None) {
        let work_dirs: Vec<String> = tasks.iter().filter_map(|t| t.work_dir.clone()).collect();
        let sock_str = socket_path.to_string_lossy().to_string();
        crate::runtime::reembed_socket_in_worktrees(&work_dirs, &sock_str);
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

    let result = run_loop(
        &mut terminal,
        &mut app,
        &mut exo,
        &mut exo_session,
        service,
        &rx,
        &perm_rx,
        &resolved_rx,
        &idle_rx,
    );

    cancel.store(true, Ordering::Relaxed);

    // Deny all pending permissions on exit
    for (_, mut queue) in app.pending_permissions.drain() {
        for perm in queue.drain(..) {
            let _ = write_response_to_stream(perm.stream, false, None);
        }
    }
    let _ = std::fs::remove_file(&socket_path);
    crate::permission::remove_socket_breadcrumb(service.project_root());

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

/// Resolve and consume the active permission request, returning the
/// stream and permission suggestions so the caller can send the response.
fn resolve_permission(
    app: &mut App,
    allow: bool,
) -> Option<(UnixStream, bool, Vec<serde_json::Value>)> {
    let perm_key = app.active_permission_key()?;
    let perm = app.take_permission(&perm_key)?;
    Some((perm.stream, allow, perm.permission_suggestions))
}

#[allow(clippy::too_many_arguments)]
fn run_loop<R: Runtime>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    service: &TaskService<R>,
    rx: &mpsc::Receiver<ExoEvent>,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
    resolved_rx: &mpsc::Receiver<String>,
    idle_rx: &mpsc::Receiver<String>,
) -> Result<()> {
    let mut last_tick = Instant::now();

    loop {
        let tick_rate = if exo.streaming {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(500)
        };

        terminal.draw(|frame| widgets::ui(frame, app, exo))?;

        // Drain channel events
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ExoEvent::TextDelta(text) => {
                    if exo.streaming {
                        exo.append_text(&text);
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
                                app.show_detail = false;
                            } else if let Some(pos) = app.tasks.iter().position(|t| t.name == name)
                            {
                                app.list_state.select(Some(pos));
                                app.show_detail = true;
                                app.detail_scroll = 0;
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
                        if let Some((stream, allow, _suggestions)) = resolve_permission(app, true) {
                            let _ = write_response_to_stream(stream, allow, None);
                        }
                    // Ctrl+T trust / always-allow (with updatedPermissions)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('t')
                        && app.show_detail
                        && app.peek_permission(&app.focused_perm_key()).is_some()
                    {
                        if let Some((stream, allow, suggestions)) = resolve_permission(app, true) {
                            let _ = write_response_to_stream(stream, allow, Some(&suggestions));
                        }
                    // Ctrl+N denies permission (only when focused task has one)
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('n')
                        && app.show_detail
                        && app.peek_permission(&app.focused_perm_key()).is_some()
                    {
                        if let Some((stream, allow, _suggestions)) = resolve_permission(app, false)
                        {
                            let _ = write_response_to_stream(stream, allow, None);
                        }
                    // Global: Ctrl+O returns to ExO chat
                    } else if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('o')
                    {
                        app.save_current_input();
                        app.show_detail = false;
                        app.focus = Focus::ChatInput;
                        app.chat_scroll = 0;
                        app.restore_input();
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
                                                    if let Ok(tasks) = service.list_visible(None) {
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
                                    if let Some(task) = app.selected_task() {
                                        if task.status.is_running() {
                                            if let Some(window_id) = &task.tmux_window {
                                                service.goto_window(window_id);
                                            }
                                        } else {
                                            let id = task.id.as_str().to_string();
                                            match service.reopen(&id) {
                                                Ok(window_id) => {
                                                    if let Ok(tasks) = service.list_visible(None) {
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
                                                if let Ok(tasks) = service.list_visible(None) {
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
                                KeyCode::Char('/') => {
                                    app.search_query.clear();
                                    app.search_selection = 0;
                                    app.update_search_filter();
                                    app.focus = Focus::TaskSearch;
                                }
                                _ => {}
                            },
                            Focus::TaskSearch => {
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        app.search_query.clear();
                                        app.filtered_indices.clear();
                                        app.search_selection = 0;
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Enter => {
                                        if let Some(&real_idx) =
                                            app.filtered_indices.get(app.search_selection)
                                        {
                                            app.list_state.select(Some(real_idx));
                                            app.show_detail = true;
                                            app.detail_scroll = 0;
                                            app.focus = Focus::ChatInput;
                                            app.restore_input();
                                        } else {
                                            app.focus = Focus::TaskList;
                                        }
                                        app.search_query.clear();
                                        app.filtered_indices.clear();
                                        app.search_selection = 0;
                                    }
                                    KeyCode::Down | KeyCode::Tab => {
                                        app.search_next();
                                    }
                                    KeyCode::Up | KeyCode::BackTab => {
                                        app.search_prev();
                                    }
                                    KeyCode::Char('j') if ctrl => {
                                        app.search_next();
                                    }
                                    KeyCode::Char('k') if ctrl => {
                                        app.search_prev();
                                    }
                                    KeyCode::Backspace => {
                                        app.search_query.pop();
                                        app.update_search_filter();
                                    }
                                    KeyCode::Char(c) if !ctrl => {
                                        app.search_query.push(c);
                                        app.update_search_filter();
                                    }
                                    _ => {}
                                }
                            }
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
                                            let _ = service.spawn(
                                                &name,
                                                "engineer",
                                                vec![("task".to_string(), name.clone())],
                                                None,
                                                None,
                                                None,
                                            );
                                            if let Ok(tasks) = service.list_visible(None) {
                                                app.refresh_tasks(tasks);
                                            }
                                        }
                                        app.focus = Focus::TaskList;
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('k') if ctrl => app.input.kill_line(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
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
                                                        if let Ok(tasks) =
                                                            service.list_visible(None)
                                                        {
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
                                            if let Some(task) = app.selected_task()
                                                && let Some(pane) = task.tmux_pane.as_deref()
                                            {
                                                let id = task.id.as_str().to_string();
                                                service.forward_literal(pane, &msg);
                                                service.forward_key(pane, "Enter");
                                                app.acknowledge_fresh(&id);
                                            }
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
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
                                // ExO chat mode
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                match key.code {
                                    KeyCode::Esc => {
                                        if exo.streaming {
                                            exo.finish_streaming();
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
                                            // Finish any in-progress streaming from a previous turn
                                            if exo.streaming {
                                                exo.finish_streaming();
                                                // Persist the previous assistant response
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
                                            let msg = app.input.take();
                                            let _ =
                                                service.insert_exo_message(MessageRole::User, &msg);
                                            exo.add_user_message(msg.clone());
                                            exo_session
                                                .send_message(&msg, exo.session_id.as_deref());
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => app.input.kill_before(),
                                    KeyCode::Char('w') if ctrl => app.input.kill_word(),
                                    KeyCode::Char('a') if ctrl => app.input.home(),
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
                                    if let Ok(tasks) = service.list_visible(None) {
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
                                    if let Ok(tasks) = service.list_visible(None) {
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
                            Focus::ConfirmCloseProject => match key.code {
                                KeyCode::Char('y') => {
                                    app.active_project = None;
                                    app.save_current_input();
                                    app.focus = Focus::TaskList;
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
            if let Ok(tasks) = service.list_visible(None) {
                app.refresh_tasks(tasks);
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
            last_tick = Instant::now();
        }

        // Handle "resolved" notifications from PostToolUse hooks.
        // When a tool executes (approved in agent pane or elsewhere), clear
        // the matching pending permission and respond to unblock the hook.
        while let Ok(cwd) = resolved_rx.try_recv() {
            let resolved_cwd =
                std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
            let task_name = app
                .tasks
                .iter()
                .find(|t| {
                    t.work_dir.as_deref().is_some_and(|wd| {
                        let canon = std::fs::canonicalize(wd)
                            .unwrap_or_else(|_| std::path::PathBuf::from(wd));
                        resolved_cwd.starts_with(&canon)
                    })
                })
                .map(|t| t.name.clone());
            if let Some(name) = task_name {
                // Drain ALL pending permissions for this task — respond with allow
                // so the PermissionRequest hook processes can exit cleanly.
                while let Some(perm) = app.take_permission(&name) {
                    let _ = write_response_to_stream(perm.stream, true, None);
                }
                // Agent is actively working — clear idle indicator
                if let Some(task) = app.tasks.iter().find(|t| t.name == name) {
                    app.fresh_tasks.remove(task.id.as_str());
                }
            }
        }

        // Handle idle notifications from Stop hooks.
        // Mark the matching task as fresh so the ● indicator appears.
        while let Ok(cwd) = idle_rx.try_recv() {
            let idle_cwd =
                std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
            let task_id = app
                .tasks
                .iter()
                .find(|t| {
                    t.work_dir.as_deref().is_some_and(|wd| {
                        let canon = std::fs::canonicalize(wd)
                            .unwrap_or_else(|_| std::path::PathBuf::from(wd));
                        idle_cwd.starts_with(&canon)
                    })
                })
                .map(|t| t.id.as_str().to_string());
            if let Some(id) = task_id {
                app.fresh_tasks.insert(id);
            }
        }

        // Drain permission requests from socket — non-blocking, no focus change
        while let Ok((stream, req)) = perm_rx.try_recv() {
            let req_cwd = std::fs::canonicalize(&req.cwd)
                .unwrap_or_else(|_| std::path::PathBuf::from(&req.cwd));
            let task_name = app
                .tasks
                .iter()
                .find(|t| {
                    t.work_dir.as_deref().is_some_and(|wd| {
                        let canon = std::fs::canonicalize(wd)
                            .unwrap_or_else(|_| std::path::PathBuf::from(wd));
                        req_cwd.starts_with(&canon)
                    })
                })
                .map(|t| t.name.clone());

            let task_name = task_name.unwrap_or_else(|| EXO_PERM_KEY.to_string());
            let perm = ActivePermission {
                stream,
                task_name: task_name.clone(),
                tool_name: req.tool_name,
                tool_input_summary: req.tool_input_summary,
                permission_suggestions: req.permission_suggestions,
            };
            app.add_permission(perm);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
