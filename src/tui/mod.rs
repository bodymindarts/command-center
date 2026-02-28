mod app;
mod chat;
mod claude;
mod widgets;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::store::Store;
use app::{App, Focus};
use chat::ExoState;
use claude::ExoEvent;

pub fn run(store: &Store, resume_session: Option<&str>) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tasks = store.list_tasks()?;
    let mut app = App::new(tasks);
    let mut exo = ExoState::new();
    if let Some(sid) = resume_session {
        exo.session_id = Some(sid.to_string());
    }
    let (tx, rx) = mpsc::channel::<ExoEvent>();
    let cancel = Arc::new(AtomicBool::new(false));

    let result = run_loop(&mut terminal, &mut app, &mut exo, store, &rx, &tx, &cancel);

    cancel.store(true, Ordering::Relaxed);

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    exo: &mut ExoState,
    store: &Store,
    rx: &mpsc::Receiver<ExoEvent>,
    tx: &mpsc::Sender<ExoEvent>,
    cancel: &Arc<AtomicBool>,
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
                ExoEvent::Done => exo.finish_streaming(),
                ExoEvent::Error(e) => {
                    exo.append_text(&format!("\n[Error: {e}]"));
                    exo.finish_streaming();
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
            } else {
                match &app.focus {
                    Focus::TaskList => match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Char('j') | KeyCode::Down => app.next(),
                        KeyCode::Char('k') | KeyCode::Up => app.previous(),
                        KeyCode::Enter => app.goto_selected(),
                        KeyCode::Char('d') => app.show_detail = !app.show_detail,
                        KeyCode::Tab | KeyCode::Char('i') => {
                            app.focus = Focus::ChatInput;
                        }
                        _ => {}
                    },
                    Focus::ChatInput => {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Esc | KeyCode::Tab => {
                                app.focus = Focus::TaskList;
                            }
                            KeyCode::Enter => {
                                if !app.input.is_empty() && !exo.streaming {
                                    let msg = app.input.take();
                                    claude::spawn_claude(
                                        &msg,
                                        exo.session_id.as_deref(),
                                        Arc::clone(cancel),
                                        tx.clone(),
                                    );
                                    exo.add_user_message(msg);
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
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.refresh(store);
            last_tick = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
