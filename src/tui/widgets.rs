use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::primitives::TaskStatus;

use super::app::{App, Focus};
use super::chat::{ExoState, Role};

pub fn ui(frame: &mut ratatui::Frame, app: &mut App, exo: &ExoState) {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(frame.area());

    // Left side: chat + input + prompt bar
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(outer[0]);

    render_chat(frame, app, exo, left[0]);

    let focused_input = matches!(app.focus, Focus::ChatInput | Focus::SpawnInput);
    render_input(frame, app, left[1], focused_input);
    render_prompt_bar(frame, app, left[2]);

    // Right side: always task list
    let focused_task_list = matches!(app.focus, Focus::TaskList);
    render_task_list(frame, app, outer[1], focused_task_list);
}

fn render_task_list(frame: &mut ratatui::Frame, app: &mut App, area: Rect, focused: bool) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|task| {
            let status_char = match task.status {
                TaskStatus::Running => "r",
                TaskStatus::Completed => "c",
                TaskStatus::Failed => "f",
                TaskStatus::Closed => "x",
            };
            let color = status_color(&task.status);
            let time = task.started_at.format("%H:%M");
            let line = Line::from(vec![
                Span::styled(format!("{status_char} "), Style::default().fg(color)),
                Span::raw(format!("{:<10} ", task.name)),
                Span::styled(format!("{time}"), Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Tasks ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_chat(frame: &mut ratatui::Frame, app: &App, exo: &ExoState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let in_task_chat = app.show_detail && app.selected_task().is_some();

    let title = if in_task_chat {
        let name = app.selected_task().map(|t| t.name.as_str()).unwrap_or("?");
        format!(" Chat: {name} ")
    } else {
        " ExO Chat ".to_string()
    };

    if in_task_chat {
        // Render task messages
        if app.selected_messages.is_empty() {
            lines.push(Line::from(Span::styled(
                "No messages yet.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in &app.selected_messages {
                let (label, label_color) = match msg.role.as_str() {
                    "system" => ("PROMPT", Color::Cyan),
                    "user" => ("YOU", Color::Green),
                    _ => (&*msg.role, Color::White),
                };

                lines.push(Line::from(Span::styled(
                    format!("{label}:"),
                    Style::default()
                        .fg(label_color)
                        .add_modifier(Modifier::BOLD),
                )));
                for l in msg.content.lines() {
                    lines.push(Line::from(l.to_string()));
                }
                lines.push(Line::from(""));
            }
        }

        if let Some(output) = &app.detail_live_output {
            lines.push(Line::from(Span::styled(
                "--- Live (last 50 lines) ---",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            let tail: Vec<&str> = output.lines().collect();
            let start = tail.len().saturating_sub(50);
            for l in &tail[start..] {
                lines.push(Line::from(l.to_string()));
            }
        }
    } else {
        // Render ExO chat
        for msg in &exo.messages {
            let (label, label_color) = match msg.role {
                Role::User => ("You", Color::Green),
                Role::Assistant => ("ExO", Color::Cyan),
            };

            lines.push(Line::from(Span::styled(
                format!("{label}:"),
                Style::default()
                    .fg(label_color)
                    .add_modifier(Modifier::BOLD),
            )));

            if msg.content.is_empty() && matches!(msg.role, Role::Assistant) && exo.streaming {
                lines.push(Line::from(Span::styled(
                    "...",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for l in msg.content.lines() {
                    lines.push(Line::from(l.to_string()));
                }
            }

            if !msg.tool_activity.is_empty() {
                let tools: Vec<Span> = msg
                    .tool_activity
                    .iter()
                    .map(|t| Span::styled(format!("[{t}] "), Style::default().fg(Color::Yellow)))
                    .collect();
                lines.push(Line::from(tools));
            }

            lines.push(Line::from(""));
        }

        // Show pending permission request from spawned tasks
        if let Some(req) = &app.current_permission {
            lines.push(Line::from(Span::styled(
                format!(
                    "{} wants to use {}: {}",
                    req.task_name, req.tool_name, req.tool_input_summary
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Press Tab to chat with ExO",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let inner_height = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;

    // Account for line wrapping when calculating scroll
    let rendered_lines: usize = lines
        .iter()
        .map(|line| {
            let w = line.width();
            if w == 0 || inner_width == 0 {
                1
            } else {
                w.div_ceil(inner_width)
            }
        })
        .sum();

    let scroll = rendered_lines.saturating_sub(inner_height) as u16;

    let chat = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(chat, area);
}

fn render_input(frame: &mut ratatui::Frame, app: &App, area: Rect, focused: bool) {
    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let prefix = if matches!(app.focus, Focus::SpawnInput) {
        "[spawn:noop] > ".to_string()
    } else if app.show_detail {
        let name = app.selected_task().map(|t| t.name.as_str()).unwrap_or("?");
        format!("[agent:{name}] > ")
    } else {
        "[ExO] > ".to_string()
    };
    let prefix = prefix.as_str();
    let buf = app.input.buffer();
    let prefix_len = prefix.len() as u16;
    // Visible width inside borders
    let visible_width = area.width.saturating_sub(2);
    let cursor_pos = prefix_len + app.input.cursor as u16;

    // Horizontal scroll: keep cursor within visible area
    let scroll = if cursor_pos >= visible_width {
        cursor_pos - visible_width + 1
    } else {
        0
    };

    let display = format!("{prefix}{buf}");

    let input = Paragraph::new(display)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .scroll((0, scroll));

    frame.render_widget(input, area);

    if focused {
        let x = area.x + 1 + cursor_pos - scroll;
        let y = area.y + 1;
        frame.set_cursor_position((x, y));
    }
}

fn render_prompt_bar(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let spans = match &app.focus {
        Focus::PermissionPrompt => vec![
            Span::styled(" y", Style::default().fg(Color::Green)),
            Span::raw(" approve  "),
            Span::styled("n", Style::default().fg(Color::Red)),
            Span::raw(" deny"),
        ],
        Focus::SpawnInput => vec![
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" send  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ],
        Focus::ChatInput => vec![
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" send  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" tasks  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ],
        Focus::TaskList => vec![
            Span::styled(" j/k", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" chat  "),
            Span::styled("g", Style::default().fg(Color::Yellow)),
            Span::raw(" goto  "),
            Span::styled("n", Style::default().fg(Color::Yellow)),
            Span::raw(" new  "),
            Span::styled("x", Style::default().fg(Color::Yellow)),
            Span::raw(" close  "),
            Span::styled("Tab", Style::default().fg(Color::Yellow)),
            Span::raw(" exo  "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(" quit"),
        ],
    };

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray));

    frame.render_widget(bar, area);
}

fn status_color(status: &TaskStatus) -> Color {
    match status {
        TaskStatus::Running => Color::Yellow,
        TaskStatus::Completed => Color::Green,
        TaskStatus::Failed => Color::Red,
        TaskStatus::Closed => Color::Magenta,
    }
}
