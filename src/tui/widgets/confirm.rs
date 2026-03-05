use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::super::screen::{Focus, Screen};

pub(in crate::tui) fn render_permission_panel(
    frame: &mut ratatui::Frame,
    dash: &Screen,
    area: Rect,
) {
    let perm_key = dash.focused_perm_key();
    let Some(req) = dash.permissions.peek(&perm_key) else {
        return;
    };
    let extra = dash
        .permissions
        .get(&perm_key)
        .map(|q| q.len().saturating_sub(1))
        .unwrap_or(0);
    let more = if extra > 0 {
        format!(" (+{extra} more)")
    } else {
        String::new()
    };
    let summary = if req.tool_input_summary.is_empty() {
        req.tool_name.clone()
    } else {
        format!("{}: {}", req.tool_name, req.tool_input_summary)
    };
    let lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                summary,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(more, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "^Y",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ok   "),
            Span::styled(
                "^T",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" trust   "),
            Span::styled(
                "^N",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" deny"),
        ]),
    ];
    let block = Block::default()
        .title(" Permission ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(in crate::tui) fn render_askuser_panel(frame: &mut ratatui::Frame, dash: &Screen, area: Rect) {
    let perm_key = dash.focused_perm_key();
    let Some(perm) = dash.permissions.peek(&perm_key) else {
        return;
    };
    if !perm.is_askuser() {
        return;
    }
    let question = perm.askuser_question.as_deref().unwrap_or("?");
    let extra = dash
        .permissions
        .get(&perm_key)
        .map(|q| q.len().saturating_sub(1))
        .unwrap_or(0);
    let more = if extra > 0 {
        format!(" (+{extra} more)")
    } else {
        String::new()
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                question.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(more, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
    ];
    for (i, (label, description)) in perm.askuser_options.iter().enumerate().take(4) {
        let num = i + 1;
        let desc = if description.is_empty() {
            String::new()
        } else {
            format!(" - {description}")
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("{num}"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(". {label}{desc}")),
        ]));
    }
    let block = Block::default()
        .title(" AskUser ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(in crate::tui) fn render_delete_confirm_panel(
    frame: &mut ratatui::Frame,
    dash: &Screen,
    area: Rect,
) {
    let Focus::ConfirmDelete(ref id) = dash.focus else {
        return;
    };
    let name = dash
        .tasks
        .iter()
        .find(|t| t.id == *id)
        .map(|t| t.name.as_str())
        .unwrap_or("?");
    let lines = vec![
        Line::from(vec![
            Span::raw(" Delete task "),
            Span::styled(
                name,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "y",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" delete   "),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ]),
    ];
    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(in crate::tui) fn render_close_task_panel(
    frame: &mut ratatui::Frame,
    dash: &Screen,
    area: Rect,
) {
    let Focus::ConfirmCloseTask(ref id) = dash.focus else {
        return;
    };
    let name = dash
        .tasks
        .iter()
        .find(|t| t.id == *id)
        .map(|t| t.name.as_str())
        .unwrap_or("?");
    let lines = vec![
        Line::from(vec![
            Span::raw(" Close task "),
            Span::styled(
                name,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "y",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" close   "),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ]),
    ];
    let block = Block::default()
        .title(" Confirm Close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(in crate::tui) fn render_close_project_panel(
    frame: &mut ratatui::Frame,
    dash: &Screen,
    area: Rect,
) {
    let name = dash
        .active_project
        .as_ref()
        .map(|n| n.as_str())
        .unwrap_or("?");
    let lines = vec![
        Line::from(vec![
            Span::raw(" Close project "),
            Span::styled(
                name,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "y",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" close   "),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ]),
    ];
    let block = Block::default()
        .title(" Confirm Close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(in crate::tui) fn render_delete_project_panel(
    frame: &mut ratatui::Frame,
    dash: &Screen,
    area: Rect,
) {
    let Focus::ConfirmDeleteProject(ref name) = dash.focus else {
        return;
    };
    let lines = vec![
        Line::from(vec![
            Span::raw(" Delete project "),
            Span::styled(
                name.as_str(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "y",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" delete   "),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" cancel"),
        ]),
    ];
    let block = Block::default()
        .title(" Confirm Delete ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
