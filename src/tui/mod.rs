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
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::permission::PermissionRequest;
use crate::runtime::Runtime;
use crate::service::TaskService;
use app::{ActivePermission, App, Focus};

const EXO_PERM_KEY: &str = "exo";
use chat::ExoState;
use claude::ExoEvent;

pub fn run<R: Runtime>(service: &TaskService<R>, resume_session: Option<&str>) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tasks = service.list_active()?;
    let mut app = App::new(tasks);
    let mut exo = ExoState::new();
    if let Some(sid) = resume_session {
        exo.session_id = Some(sid.to_string());
    }
    if let Ok(messages) = service.exo_messages() {
        exo.load_history(messages);
    }
    let (tx, rx) = mpsc::channel::<ExoEvent>();
    let cancel = Arc::new(AtomicBool::new(false));

    // Permission socket listener
    let (perm_tx, perm_rx) = mpsc::channel::<(UnixStream, PermissionRequest)>();
    let perm_cancel = Arc::clone(&cancel);
    let listener = crate::permission::start_socket_listener()?;
    std::thread::spawn(move || {
        while !perm_cancel.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = String::new();
                    if std::io::Read::read_to_string(&mut stream, &mut buf).is_ok()
                        && let Some(req) = crate::permission::parse_request_json(&buf)
                    {
                        let _ = perm_tx.send((stream, req));
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
        service,
        &rx,
        &tx,
        &cancel,
        &perm_rx,
    );

    cancel.store(true, Ordering::Relaxed);

    // Deny all pending permissions on exit
    for (_, mut queue) in app.pending_permissions.drain() {
        for perm in queue.drain(..) {
            let _ = write_response_to_stream(perm.stream, false);
        }
    }
    let _ = std::fs::remove_file(crate::permission::socket_path());

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn write_response_to_stream(mut stream: UnixStream, allow: bool) -> std::io::Result<()> {
    use std::io::Write;
    let response = crate::permission::make_response_json(allow, None);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

#[allow(clippy::too_many_arguments)]
fn run_loop<R: Runtime>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    exo: &mut ExoState,
    service: &TaskService<R>,
    rx: &mpsc::Receiver<ExoEvent>,
    tx: &mpsc::Sender<ExoEvent>,
    cancel: &Arc<AtomicBool>,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
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
                ExoEvent::TextDelta(text) => exo.append_text(&text),
                ExoEvent::ToolStart(name) => exo.add_tool_activity(name),
                ExoEvent::SessionId(id) => exo.session_id = Some(id),
                ExoEvent::Done => {
                    exo.finish_streaming();
                    if let Some(msg) = exo.messages.last()
                        && matches!(msg.role, chat::Role::Assistant)
                        && !msg.content.is_empty()
                    {
                        let _ = service.insert_exo_message("assistant", &msg.content);
                    }
                }
                ExoEvent::Error(e) => {
                    exo.append_text(&format!("\n[Error: {e}]"));
                    exo.finish_streaming();
                    if let Some(msg) = exo.messages.last()
                        && matches!(msg.role, chat::Role::Assistant)
                        && !msg.content.is_empty()
                    {
                        let _ = service.insert_exo_message("assistant", &msg.content);
                    }
                }
            }
        }

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // Global: Ctrl+C quits
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                app.should_quit = true;
            // Global: Ctrl+Z suspends
            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('z')
            {
                terminal::disable_raw_mode()?;
                crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                terminal.show_cursor()?;
                // SAFETY: raise(SIGTSTP) is safe to call; it suspends the process
                // and returns when the process is resumed via SIGCONT (fg).
                unsafe {
                    libc::raise(libc::SIGTSTP);
                }
                terminal::enable_raw_mode()?;
                crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                terminal.hide_cursor()?;
                terminal.clear()?;
            // Global: Ctrl+P cycles to next task with pending permissions
            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('p')
            {
                let names = app.tasks_with_permissions();
                if !names.is_empty() {
                    let current = app.focused_perm_key();
                    let idx = names
                        .iter()
                        .position(|n| n == &current)
                        .map(|i| (i + 1) % names.len())
                        .unwrap_or(0);
                    let name = names[idx].clone();
                    if name == EXO_PERM_KEY {
                        app.show_detail = false;
                    } else if let Some(pos) = app.tasks.iter().position(|t| t.name == name) {
                        app.list_state.select(Some(pos));
                        app.show_detail = true;
                        app.detail_scroll = 0;
                    }
                    app.focus = Focus::ChatInput;
                } else if app.list_state.selected().is_some() {
                    app.show_detail = true;
                    app.detail_scroll = 0;
                    app.focus = Focus::ChatInput;
                }
            // Global: Ctrl+E returns to ExO chat
            } else if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('e')
            {
                app.show_detail = false;
                app.focus = Focus::ChatInput;
            } else {
                match &app.focus {
                    Focus::TaskList => match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Esc => {
                            app.show_detail = false;
                            app.focus = Focus::ChatInput;
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            app.next();
                            app.detail_scroll = 0;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            app.previous();
                            app.detail_scroll = 0;
                        }
                        KeyCode::PageDown => {
                            app.detail_scroll = app.detail_scroll.saturating_add(10);
                        }
                        KeyCode::PageUp => {
                            app.detail_scroll = app.detail_scroll.saturating_sub(10);
                        }
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.detail_scroll = app.detail_scroll.saturating_add(10);
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.detail_scroll = app.detail_scroll.saturating_sub(10);
                        }
                        KeyCode::Enter => {
                            app.show_detail = true;
                            app.focus = Focus::ChatInput;
                        }
                        KeyCode::Char('g') => {
                            if let Some(task) = app.selected_task() {
                                if task.status.is_running() {
                                    if let Some(window_id) = &task.tmux_window {
                                        service.goto_window(window_id);
                                    }
                                } else {
                                    let id = task.id.as_str().to_string();
                                    if let Ok(window_id) = service.reopen(&id) {
                                        if let Ok(tasks) = service.list_active() {
                                            app.refresh_tasks(tasks);
                                        }
                                        service.goto_window(&window_id);
                                    }
                                }
                            }
                        }
                        KeyCode::Char('x') => {
                            if let Some(task) = app.selected_task()
                                && task.status.is_running()
                            {
                                let id = task.id.as_str().to_string();
                                let _ = service.close(&id);
                                if let Ok(tasks) = service.list_active() {
                                    app.refresh_tasks(tasks);
                                }
                            }
                        }
                        KeyCode::Char('n') => {
                            app.input.take();
                            app.focus = Focus::SpawnInput;
                        }
                        KeyCode::Backspace => {
                            if let Some(task) = app.selected_task() {
                                let id = task.id.clone();
                                app.focus = Focus::ConfirmDelete(id);
                            }
                        }
                        KeyCode::Tab => {
                            app.focus = Focus::ChatInput;
                        }
                        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.focus = Focus::ChatInput;
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
                                    let _ = service.spawn(
                                        &name,
                                        "engineer",
                                        vec![("task".to_string(), name.clone())],
                                    );
                                    if let Ok(tasks) = service.list_active() {
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
                    Focus::ChatInput => {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Esc => {
                                if exo.streaming {
                                    exo.finish_streaming();
                                }
                                app.show_detail = false;
                            }
                            KeyCode::Tab => {
                                app.focus = Focus::TaskList;
                            }
                            KeyCode::Char('l') if ctrl => {
                                app.focus = Focus::TaskList;
                            }
                            KeyCode::Enter => {
                                if !app.input.is_empty() {
                                    let perm_key = app.focused_perm_key();
                                    let buf = app.input.buffer();
                                    if app.peek_permission(&perm_key).is_some()
                                        && (buf == "1" || buf == "2")
                                    {
                                        let allow = buf == "1";
                                        app.input.take();
                                        if let Some(perm) = app.take_permission(&perm_key) {
                                            let _ = write_response_to_stream(perm.stream, allow);
                                        }
                                    } else if app.show_detail {
                                        // Send to task agent
                                        let msg = app.input.take();
                                        if let Some(task) = app.selected_task()
                                            && let Some(pane) = task.tmux_pane.as_deref()
                                        {
                                            service.send_by_id(task.id.as_str(), pane, &msg);
                                        }
                                    } else if !exo.streaming {
                                        // Send to ExO
                                        let msg = app.input.take();
                                        claude::spawn_claude(
                                            &msg,
                                            exo.session_id.as_deref(),
                                            Arc::clone(cancel),
                                            tx.clone(),
                                        );
                                        let _ = service.insert_exo_message("user", &msg);
                                        exo.add_user_message(msg);
                                    }
                                }
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
                    Focus::ConfirmDelete(task_id) => match key.code {
                        KeyCode::Char('y') => {
                            let id = task_id.clone();
                            let _ = service.delete(id.as_str());
                            if let Ok(tasks) = service.list_active() {
                                app.refresh_tasks(tasks);
                            }
                            app.focus = Focus::TaskList;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.focus = Focus::TaskList;
                        }
                        _ => {}
                    },
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if let Ok(tasks) = service.list_active() {
                app.refresh_tasks(tasks);
            }
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
            };
            app.add_permission(perm);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
