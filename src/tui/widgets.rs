use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::store::Task;

use super::app::{App, Focus};
use super::chat::{ExoState, Role};

pub fn ui(frame: &mut ratatui::Frame, app: &mut App, exo: &ExoState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let main_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[0]);

    let focused_task_list = matches!(app.focus, Focus::TaskList);
    render_task_list(frame, app, main_area[0], focused_task_list);

    if app.show_detail && focused_task_list {
        render_detail(frame, app, main_area[1]);
    } else {
        render_chat(frame, exo, main_area[1]);
    }

    let focused_input = matches!(app.focus, Focus::ChatInput);
    render_input(frame, app, outer[1], focused_input);
    render_prompt_bar(frame, outer[2]);
}

fn render_task_list(frame: &mut ratatui::Frame, app: &mut App, area: Rect, focused: bool) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|task| {
            let status_char = match task.status.as_str() {
                "running" => "r",
                "completed" => "c",
                "failed" => "f",
                _ => "?",
            };
            let color = status_color(&task.status);
            let time = task.started_at.format("%H:%M");
            let line = Line::from(vec![
                Span::styled(format!("{status_char} "), Style::default().fg(color)),
                Span::raw(format!("{:<10} ", task.skill_name)),
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

fn render_detail(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let Some(task) = app.selected_task() else {
        let empty = Paragraph::new("No task selected")
            .block(
                Block::default()
                    .title(" Detail ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    };

    let color = status_color(&task.status);
    let short_id = &task.id[..8.min(task.id.len())];

    let prompt_text = read_prompt_file(&task.id);
    let output_text = read_task_output(task);

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("Skill:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(&task.skill_name),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&task.status, Style::default().fg(color)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "--- Prompt ---",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
    ];

    for l in prompt_text.lines() {
        lines.push(Line::from(l.to_string()));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "--- Output ---",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    let tail = tail_lines(&output_text, 50);
    for l in tail.lines() {
        lines.push(Line::from(l.to_string()));
    }

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .title(format!(" Detail: {short_id} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(detail, area);
}

fn render_chat(frame: &mut ratatui::Frame, exo: &ExoState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

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

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Press Tab to chat with ExO",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let inner_height = area.height.saturating_sub(2);
    let scroll = (lines.len() as u16).saturating_sub(inner_height);

    let chat = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" ExO Chat ")
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
    let prefix = "[ExO] > ";
    let display = format!("{prefix}{}", app.input.buffer());

    let input = Paragraph::new(display).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );

    frame.render_widget(input, area);

    if focused {
        let x = area.x + 1 + prefix.len() as u16 + app.input.cursor as u16;
        let y = area.y + 1;
        frame.set_cursor_position((x, y));
    }
}

fn render_prompt_bar(frame: &mut ratatui::Frame, area: Rect) {
    let bar = Paragraph::new(Line::from(vec![
        Span::styled(" j/k", Style::default().fg(Color::Yellow)),
        Span::raw(" navigate  "),
        Span::styled("Enter", Style::default().fg(Color::Yellow)),
        Span::raw(" goto  "),
        Span::styled("d", Style::default().fg(Color::Yellow)),
        Span::raw(" detail  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" chat  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]))
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(bar, area);
}

fn read_prompt_file(task_id: &str) -> String {
    let path = std::env::temp_dir().join(format!("cc-prompt-{task_id}.txt"));
    std::fs::read_to_string(path).unwrap_or_default()
}

fn read_task_output(task: &Task) -> String {
    if task.status == "running" {
        let path = std::env::temp_dir().join(format!("cc-task-{}.out", task.id));
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        task.output.clone().unwrap_or_default()
    }
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= n {
        text.to_string()
    } else {
        lines[lines.len() - n..].join("\n")
    }
}

fn status_color(status: &str) -> Color {
    match status {
        "running" => Color::Yellow,
        "completed" => Color::Green,
        "failed" => Color::Red,
        _ => Color::White,
    }
}
