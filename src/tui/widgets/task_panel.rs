use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::task::{DisplayStatus, Project};

use super::super::screen_state::{Focus, ScreenState};

fn task_list_item(state: &ScreenState, task: &crate::task::Task) -> ListItem<'static> {
    let ds = task.display_status(&state.task_list.idle_panes);
    let status_char = ds.indicator();
    let color = display_status_color(&ds);
    let dim = if ds.is_dim() {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    let fresh_mod = if ds == DisplayStatus::Active {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let time = task.started_at.format("%H:%M");
    let win_num = task
        .tmux_window
        .as_ref()
        .and_then(|w| state.task_list.window_numbers.get(w))
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
    if let Some(queue) = state.permissions.get(&task.name)
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
    if let Some(queue) = state.permissions.get(&task.name)
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

pub(in crate::tui) fn render_task_list(
    frame: &mut ratatui::Frame,
    state: &mut ScreenState,
    area: Rect,
    focused: bool,
) {
    if state.show_projects {
        render_project_list(frame, state, area, focused);
        return;
    }

    let searching = matches!(state.current_focus(), Focus::TaskSearch);

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
        state
            .task_list
            .filtered_indices
            .iter()
            .filter_map(|&i| state.task_list.tasks.get(i))
            .map(|task| task_list_item(state, task))
            .collect()
    } else {
        state
            .task_list
            .tasks
            .iter()
            .map(|task| task_list_item(state, task))
            .collect()
    };

    let border_color = if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };

    let total_askuser = state.current_project_askuser_count();
    let total_perm = state
        .current_project_perm_count()
        .saturating_sub(total_askuser);
    let other_perms = state.other_project_perm_counts();
    let mut title_spans: Vec<Span> = Vec::new();
    if searching {
        title_spans.push(Span::raw(format!(
            " Tasks ({}/{}) ",
            state.task_list.filtered_indices.len(),
            state.task_list.tasks.len()
        )));
    } else if let Some(ref name) = state.active_project {
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

    let show_highlight = state.task_list.show_detail || focused;

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

    frame.render_stateful_widget(list, list_area, &mut state.task_list.list_state);

    if let Some(search_area) = search_area {
        render_search_bar(frame, &state.search_input.buffer(), search_area);
    }
}

fn render_project_list(
    frame: &mut ratatui::Frame,
    state: &mut ScreenState,
    area: Rect,
    focused: bool,
) {
    let searching = matches!(state.current_focus(), Focus::TaskSearch) && state.show_projects;

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
        state
            .filtered_project_indices
            .iter()
            .filter_map(|&i| state.projects.get(i))
            .map(project_item)
            .collect()
    } else {
        state.projects.iter().map(project_item).collect()
    };

    let title = if searching {
        format!(
            " Projects ({}/{}) ",
            state.filtered_project_indices.len(),
            state.projects.len()
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

    frame.render_stateful_widget(list, list_area, &mut state.project_list_state);

    if let Some(search_area) = search_area {
        render_search_bar(frame, &state.search_input.buffer(), search_area);
    }
}

fn render_search_bar(frame: &mut ratatui::Frame, query: &str, area: Rect) {
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
    frame.render_widget(search_bar, area);
}

fn display_status_color(ds: &DisplayStatus) -> Color {
    match ds {
        DisplayStatus::Active | DisplayStatus::Idle => Color::Yellow,
        DisplayStatus::Completed => Color::Green,
        DisplayStatus::Failed => Color::Red,
        DisplayStatus::Closed => Color::Magenta,
    }
}
