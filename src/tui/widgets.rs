use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::primitives::{MessageRole, TaskStatus};
use crate::task::Project;

use super::app::{App, Focus};
use super::chat::{ContentBlock, ExoState};

pub fn ui(frame: &mut ratatui::Frame, app: &mut App, exo: &ExoState, pm: Option<&ExoState>) {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(frame.area());

    // Left side: chat + (optional mid-panel) + input + prompt bar
    let searching = matches!(app.focus, Focus::TaskSearch);
    let in_task_chat = !searching && app.show_detail && app.selected_task().is_some();
    let focused_perm_key = app.focused_perm_key();
    let front_perm = app.peek_permission(&focused_perm_key);
    let show_perm = in_task_chat && front_perm.is_some_and(|p| !p.is_askuser());
    let show_askuser = in_task_chat && front_perm.is_some_and(|p| p.is_askuser());
    let show_delete = matches!(app.focus, Focus::ConfirmDelete(_));
    let show_delete_project = matches!(app.focus, Focus::ConfirmDeleteProject(_));
    let show_close_task = matches!(app.focus, Focus::ConfirmCloseTask(_));
    let show_close_project = matches!(app.focus, Focus::ConfirmCloseProject);
    let show_mid_panel = show_perm
        || show_askuser
        || show_delete
        || show_delete_project
        || show_close_task
        || show_close_project;
    let mid_panel_height = if show_askuser {
        // 2 (border) + 1 (question) + 1 (blank) + up to 4 options
        let n_opts = front_perm.map(|p| p.askuser_options.len()).unwrap_or(2);
        (4 + n_opts) as u16
    } else {
        5
    };
    let left = if show_mid_panel {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(mid_panel_height),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(outer[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(0),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(outer[0])
    };

    render_chat(frame, app, exo, pm, left[0]);
    if show_close_task {
        render_close_task_panel(frame, app, left[1]);
    } else if show_close_project {
        render_close_project_panel(frame, app, left[1]);
    } else if show_delete_project {
        render_delete_project_panel(frame, app, left[1]);
    } else if show_delete {
        render_delete_confirm_panel(frame, app, left[1]);
    } else if show_perm {
        render_permission_panel(frame, app, left[1]);
    } else if show_askuser {
        render_askuser_panel(frame, app, left[1]);
    }

    let focused_input = matches!(
        app.focus,
        Focus::ChatInput | Focus::SpawnInput | Focus::ProjectNameInput
    );
    render_input(frame, app, left[2], focused_input);
    render_prompt_bar(frame, app, left[3]);

    // Right side: task list or project list
    let focused_task_list = matches!(
        app.focus,
        Focus::TaskList | Focus::TaskSearch | Focus::ProjectList
    );
    render_task_list(frame, app, outer[1], focused_task_list);
}

fn task_list_item(app: &App, task: &crate::task::Task) -> ListItem<'static> {
    let is_active = task.status.is_running() && app.is_task_active(task.tmux_pane.as_ref());
    let status_char = match task.status {
        TaskStatus::Running if is_active => "●",
        TaskStatus::Running => "r",
        TaskStatus::Completed => "c",
        TaskStatus::Failed => "f",
        TaskStatus::Closed => "x",
    };
    let is_running = task.status.is_running();
    let color = status_color(&task.status);
    let dim = if is_running || is_active {
        Modifier::empty()
    } else {
        Modifier::DIM
    };
    let fresh_mod = if is_active {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let time = task.started_at.format("%H:%M");
    let win_num = task
        .tmux_window
        .as_ref()
        .and_then(|w| app.window_numbers.get(w))
        .map(|s| s.as_str())
        .unwrap_or("-");
    let main_line = Line::from(vec![
        Span::styled(
            format!("{:<2} ", win_num),
            Style::default().fg(Color::DarkGray).add_modifier(dim),
        ),
        Span::styled(
            format!("{status_char} "),
            Style::default()
                .fg(color)
                .add_modifier(dim)
                .add_modifier(fresh_mod),
        ),
        Span::styled(
            format!("{:<10} ", task.name),
            Style::default().add_modifier(dim).add_modifier(fresh_mod),
        ),
        Span::styled(
            format!("{:<8} ", task.skill_name),
            Style::default().fg(Color::Gray).add_modifier(dim),
        ),
        Span::styled(
            format!("{time}"),
            Style::default().fg(Color::DarkGray).add_modifier(dim),
        ),
    ]);

    // Permission sub-line if this task has pending permissions
    if let Some(queue) = app.pending_permissions.get(&task.name)
        && let Some(front) = queue.front()
    {
        let extra = queue.len().saturating_sub(1);
        let more = if extra > 0 {
            format!(" (+{extra} more)")
        } else {
            String::new()
        };
        let sub_line = Line::from(Span::styled(
            format!(
                "    ! {}: {}{more}",
                front.tool_name, front.tool_input_summary
            ),
            Style::default().fg(Color::Yellow),
        ));
        return ListItem::new(vec![main_line, sub_line]);
    }

    // AskUser sub-line if the front permission for this task is an AskUser question (green)
    if let Some(queue) = app.pending_permissions.get(&task.name)
        && let Some(front) = queue.front()
        && front.is_askuser()
    {
        let question = front.askuser_question.as_deref().unwrap_or("?");
        let truncated: String = question.chars().take(50).collect();
        let sub_line = Line::from(Span::styled(
            format!("    ? {truncated}"),
            Style::default().fg(Color::Green),
        ));
        return ListItem::new(vec![main_line, sub_line]);
    }

    ListItem::new(main_line)
}

fn render_task_list(frame: &mut ratatui::Frame, app: &mut App, area: Rect, focused: bool) {
    if app.show_projects {
        render_project_list(frame, app, area, focused);
        return;
    }

    let searching = matches!(app.focus, Focus::TaskSearch);

    // When searching, reserve a line above the task list for the search input
    let (list_area, search_area) = if searching {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        (chunks[1], Some(chunks[0]))
    } else {
        (area, None)
    };

    let items: Vec<ListItem> = if searching {
        app.filtered_indices
            .iter()
            .filter_map(|&i| app.tasks.get(i))
            .map(|task| task_list_item(app, task))
            .collect()
    } else {
        app.tasks
            .iter()
            .map(|task| task_list_item(app, task))
            .collect()
    };

    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };

    let total_askuser = app.current_project_askuser_count();
    let total_perm = app
        .current_project_perm_count()
        .saturating_sub(total_askuser);
    let other_perms = app.other_project_perm_counts();
    let mut title_spans: Vec<Span> = Vec::new();
    if searching {
        title_spans.push(Span::raw(format!(
            " Tasks ({}/{}) ",
            app.filtered_indices.len(),
            app.tasks.len()
        )));
    } else if let Some(ref name) = app.active_project {
        let name = name.as_str();
        let mut badges = Vec::new();
        if total_perm > 0 {
            badges.push(format!("{total_perm} perm"));
        }
        if total_askuser > 0 {
            badges.push(format!("{total_askuser} ask"));
        }
        if badges.is_empty() {
            title_spans.push(Span::raw(format!(" {name} ")));
        } else {
            title_spans.push(Span::raw(format!(" {name} ({}) ", badges.join(", "))));
        }
    } else {
        let mut badges = Vec::new();
        if total_perm > 0 {
            badges.push(format!("{total_perm} perm"));
        }
        if total_askuser > 0 {
            badges.push(format!("{total_askuser} ask"));
        }
        if badges.is_empty() {
            title_spans.push(Span::raw(" Tasks "));
        } else {
            title_spans.push(Span::raw(format!(" Tasks ({}) ", badges.join(", "))));
        }
    }
    if !other_perms.is_empty() {
        let parts: Vec<String> = other_perms
            .iter()
            .map(|(name, count)| format!("{name}:{count}"))
            .collect();
        title_spans.push(Span::styled(
            format!("[{}]", parts.join(", ")),
            Style::default().fg(Color::Yellow),
        ));
        title_spans.push(Span::raw(" "));
    }
    let title = Line::from(title_spans);

    let show_highlight = app.show_detail || focused;

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .highlight_style(if show_highlight || searching {
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White)
        } else {
            Style::default()
        })
        .highlight_symbol(if show_highlight || searching {
            "> "
        } else {
            "  "
        });

    frame.render_stateful_widget(list, list_area, &mut app.list_state);

    // Render search input bar above the task list
    if let Some(search_area) = search_area {
        let query = app.search_input.buffer();
        let search_line = Line::from(vec![
            Span::styled(" / ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::styled(
                format!(" {query}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);
        let search_bar = Paragraph::new(search_line).style(Style::default().bg(Color::DarkGray));
        frame.render_widget(search_bar, search_area);
    }
}

fn render_project_list(frame: &mut ratatui::Frame, app: &mut App, area: Rect, focused: bool) {
    let searching = matches!(app.focus, Focus::TaskSearch) && app.show_projects;

    let (list_area, search_area) = if searching {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        (chunks[1], Some(chunks[0]))
    } else {
        (area, None)
    };

    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };

    let project_item = |project: &Project| {
        let main_line = Line::from(vec![
            Span::styled(
                format!("{:<16} ", project.name.as_str()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                project.description.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        ListItem::new(main_line)
    };

    let items: Vec<ListItem> = if searching {
        app.filtered_project_indices
            .iter()
            .filter_map(|&i| app.projects.get(i))
            .map(project_item)
            .collect()
    } else {
        app.projects.iter().map(project_item).collect()
    };

    let title = if searching {
        format!(
            " Projects ({}/{}) ",
            app.filtered_project_indices.len(),
            app.projects.len()
        )
    } else {
        " Projects ".to_string()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, list_area, &mut app.project_list_state);

    if let Some(search_area) = search_area {
        let query = app.search_input.buffer();
        let search_line = Line::from(vec![
            Span::styled(" / ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::styled(
                format!(" {query}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("_", Style::default().fg(Color::DarkGray)),
        ]);
        let search_bar = Paragraph::new(search_line).style(Style::default().bg(Color::DarkGray));
        frame.render_widget(search_bar, search_area);
    }
}

fn render_chat(
    frame: &mut ratatui::Frame,
    app: &mut App,
    exo: &ExoState,
    pm: Option<&ExoState>,
    area: Rect,
) {
    let searching = matches!(app.focus, Focus::TaskSearch);
    let in_task_chat = !searching && app.show_detail && app.selected_task().is_some();

    let title = if in_task_chat {
        let name = app.selected_task().map(|t| t.name.as_str()).unwrap_or("?");
        format!(" Chat: {name} ")
    } else if let Some(ref name) = app.active_project {
        format!(" PM: {} ", name.as_str())
    } else {
        " ExO Chat ".to_string()
    };

    let chat_border_color = if matches!(app.focus, Focus::ChatHistory) {
        Color::Blue
    } else {
        Color::DarkGray
    };

    // Render the outer border block
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(chat_border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build chat content lines
    let mut lines: Vec<Line> = Vec::new();

    if in_task_chat {
        // Render task messages
        if app.selected_messages.is_empty() {
            lines.push(Line::from(Span::styled(
                "No messages yet.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in &app.selected_messages {
                let (label, label_color) = match msg.role {
                    MessageRole::System => ("PROMPT", Color::Cyan),
                    MessageRole::User => ("YOU", Color::Green),
                    MessageRole::Assistant => ("ASSISTANT", Color::White),
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
            let tail: Vec<&str> = output.lines().collect();
            let start = tail.len().saturating_sub(500);
            lines.push(Line::from(Span::styled(
                format!("--- Live (last {} lines) ---", tail.len() - start),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for l in &tail[start..] {
                lines.push(Line::from(l.to_string()));
            }
        }
    } else if app.active_project.is_some() {
        // Render PM chat
        if let Some(pm) = pm {
            render_chat_messages(&mut lines, &pm.messages, "PM", pm.streaming);
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Chat with PM to plan and coordinate project work",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        // Render ExO chat
        render_chat_messages(&mut lines, &exo.messages, "ExO", exo.streaming);
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Press Tab to chat with ExO",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let inner_height = inner.height as usize;
    let inner_width = inner.width as usize;

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

    let max_scroll = rendered_lines.saturating_sub(inner_height) as u16;
    app.chat_viewport_height = inner_height as u16;
    app.chat_scroll = app.chat_scroll.min(max_scroll);
    let scroll = max_scroll.saturating_sub(app.chat_scroll);

    let chat = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(chat, inner);
}

/// Render streaming chat messages (used for both ExO and PM chats).
fn render_chat_messages(
    lines: &mut Vec<Line<'static>>,
    messages: &[super::chat::ChatMessage],
    assistant_label: &str,
    streaming: bool,
) {
    for msg in messages {
        let (label, label_color) = match msg.role {
            MessageRole::User => ("You", Color::Green),
            MessageRole::Assistant => (assistant_label, Color::Cyan),
            MessageRole::System => ("System", Color::DarkGray),
        };

        lines.push(Line::from(Span::styled(
            format!("{label}:"),
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        )));

        if msg.blocks.is_empty() && matches!(msg.role, MessageRole::Assistant) && streaming {
            lines.push(Line::from(Span::styled(
                "...",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            let mut tool_spans: Vec<Span> = Vec::new();
            for block in &msg.blocks {
                match block {
                    ContentBlock::Text(text) => {
                        if !tool_spans.is_empty() {
                            lines.push(Line::from(std::mem::take(&mut tool_spans)));
                        }
                        for l in text.trim_start_matches('\n').lines() {
                            lines.push(Line::from(l.to_string()));
                        }
                    }
                    ContentBlock::ToolUse(name) => {
                        tool_spans.push(Span::styled(
                            format!("[{name}] "),
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                }
            }
            if !tool_spans.is_empty() {
                lines.push(Line::from(tool_spans));
            }
        }

        lines.push(Line::from(""));
    }
}

fn render_permission_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let perm_key = app.focused_perm_key();
    let Some(req) = app.peek_permission(&perm_key) else {
        return;
    };
    let extra = app
        .pending_permissions
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

fn render_askuser_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let perm_key = app.focused_perm_key();
    let Some(perm) = app.peek_permission(&perm_key) else {
        return;
    };
    if !perm.is_askuser() {
        return;
    }
    let question = perm.askuser_question.as_deref().unwrap_or("?");
    let extra = app
        .pending_permissions
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

fn render_delete_confirm_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let Focus::ConfirmDelete(ref id) = app.focus else {
        return;
    };
    let name = app
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

fn render_close_task_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let Focus::ConfirmCloseTask(ref id) = app.focus else {
        return;
    };
    let name = app
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

fn render_close_project_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let name = app
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

fn render_delete_project_panel(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    let Focus::ConfirmDeleteProject(ref name) = app.focus else {
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

fn render_input(frame: &mut ratatui::Frame, app: &App, area: Rect, focused: bool) {
    let front_input_perm = app.peek_permission(&app.focused_perm_key());
    let focused_has_perms = app.show_detail && front_input_perm.is_some_and(|p| !p.is_askuser());
    let focused_has_askuser = app.show_detail && front_input_perm.is_some_and(|p| p.is_askuser());
    let border_color = if focused && focused_has_perms {
        Color::Yellow
    } else if focused && focused_has_askuser {
        Color::Green
    } else if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let searching = matches!(app.focus, Focus::TaskSearch);
    let prefix = if matches!(app.focus, Focus::ProjectNameInput) {
        "[new project] > ".to_string()
    } else if matches!(app.focus, Focus::SpawnInput) {
        "[spawn] > ".to_string()
    } else if !searching && app.show_detail {
        let name = app.selected_task().map(|t| t.name.as_str()).unwrap_or("?");
        format!("[{name}] > ")
    } else if let Some(ref name) = app.active_project {
        format!("[{}] > ", name.as_str())
    } else {
        "[ExO] > ".to_string()
    };
    let prefix = prefix.as_str();
    let prefix_len = prefix.len() as u16;
    // Visible width inside borders
    let visible_width = area.width.saturating_sub(2);

    let display_buf = app.input.display_text();
    let cursor_pos = prefix_len + app.input.display_cursor() as u16;
    let scroll = if cursor_pos >= visible_width {
        cursor_pos - visible_width + 1
    } else {
        0
    };
    let display = format!("{prefix}{display_buf}");

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

fn perm_hint_spans() -> Vec<Span<'static>> {
    vec![
        Span::raw("  "),
        Span::styled("^Y", Style::default().fg(Color::Green)),
        Span::raw(" ok  "),
        Span::styled("^T", Style::default().fg(Color::Green)),
        Span::raw(" trust  "),
        Span::styled("^N", Style::default().fg(Color::Green)),
        Span::raw(" deny  "),
        Span::styled("^P", Style::default().fg(Color::Green)),
        Span::raw(" next"),
    ]
}

fn askuser_hint_spans(n_opts: usize) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw("  ")];
    let labels = ["1", "2", "3", "4"];
    for (i, label) in labels.iter().enumerate().take(n_opts.min(4)) {
        if i > 0 {
            spans.push(Span::raw("/"));
        }
        spans.push(Span::styled(*label, Style::default().fg(Color::Green)));
    }
    spans.push(Span::raw(" select  "));
    spans.push(Span::styled("^P", Style::default().fg(Color::Green)));
    spans.push(Span::raw(" next"));
    spans
}

fn render_prompt_bar(frame: &mut ratatui::Frame, app: &App, area: Rect) {
    // Show transient error in red, replacing normal keybinding hints
    if let Some(ref err) = app.status_error {
        let bar = Paragraph::new(Line::from(vec![Span::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red),
        )]));
        frame.render_widget(bar, area);
        return;
    }

    let front_p = app.peek_permission(&app.focused_perm_key());
    let has_perms = app.show_detail && front_p.is_some_and(|p| !p.is_askuser());
    let mut spans = match &app.focus {
        Focus::ProjectNameInput => vec![
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" create  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ],
        Focus::SpawnInput => vec![
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" send  "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(" cancel"),
        ],
        Focus::ChatInput if app.show_detail => {
            vec![
                Span::styled(" ^G", Style::default().fg(Color::Yellow)),
                Span::raw(" goto  "),
                Span::styled("^K", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll  "),
                Span::styled("^N/^P", Style::default().fg(Color::Yellow)),
                Span::raw(" next  "),
                Span::styled("^L", Style::default().fg(Color::Yellow)),
                Span::raw(" tasks  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back"),
            ]
        }
        Focus::ChatInput => {
            vec![
                Span::styled(" Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" send  "),
                Span::styled("^K", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll  "),
                Span::styled("^L", Style::default().fg(Color::Yellow)),
                Span::raw(" tasks  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back"),
            ]
        }
        Focus::ChatHistory => {
            vec![
                Span::styled(" ^J", Style::default().fg(Color::Yellow)),
                Span::raw(" input  "),
                Span::styled("^U", Style::default().fg(Color::Yellow)),
                Span::raw(" up  "),
                Span::styled("^D", Style::default().fg(Color::Yellow)),
                Span::raw(" down  "),
                Span::styled("^L", Style::default().fg(Color::Yellow)),
                Span::raw(" tasks  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back"),
            ]
        }
        Focus::ConfirmCloseTask(_)
        | Focus::ConfirmCloseProject
        | Focus::ConfirmDeleteProject(_) => {
            vec![
                Span::styled(" y", Style::default().fg(Color::Yellow)),
                Span::raw(" close  "),
                Span::styled("n", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel"),
            ]
        }
        Focus::TaskSearch => {
            vec![
                Span::styled(" Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" select  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel  "),
                Span::styled("Tab/S-Tab", Style::default().fg(Color::Yellow)),
                Span::raw(" navigate"),
            ]
        }
        Focus::ProjectList => {
            vec![
                Span::styled(" j/k", Style::default().fg(Color::Yellow)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" select  "),
                Span::styled("n", Style::default().fg(Color::Yellow)),
                Span::raw(" new  "),
                Span::styled("⌫", Style::default().fg(Color::Yellow)),
                Span::raw(" delete  "),
                Span::styled("p/Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ]
        }
        Focus::ConfirmDelete(_) | Focus::TaskList => {
            vec![
                Span::styled(" j/k", Style::default().fg(Color::Yellow)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" chat  "),
                Span::styled("^G", Style::default().fg(Color::Yellow)),
                Span::raw(" goto  "),
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(" search  "),
                Span::styled("p", Style::default().fg(Color::Yellow)),
                Span::raw(" projects  "),
                Span::styled("n", Style::default().fg(Color::Yellow)),
                Span::raw(" new  "),
                Span::styled("x", Style::default().fg(Color::Yellow)),
                Span::raw(" close  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ]
        }
    };
    let has_askuser = app.show_detail && front_p.is_some_and(|p| p.is_askuser());
    if has_perms {
        spans.extend(perm_hint_spans());
    } else if has_askuser {
        let n_opts = front_p.map(|p| p.askuser_options.len()).unwrap_or(0);
        spans.extend(askuser_hint_spans(n_opts));
    }

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
