use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::primitives::TaskName;

use super::super::keybindings::{GlobalBindings, Keybindings};
use super::super::permissions::PermissionStore;
use super::super::state::{Focus, InputState, TaskListState};

#[allow(clippy::too_many_arguments)]
pub(in crate::tui) fn render_input(
    frame: &mut ratatui::Frame,
    focus: &Focus,
    task_list: &TaskListState,
    active_project_name: Option<&str>,
    permissions: &PermissionStore,
    perm_key: &TaskName,
    input: &InputState,
    area: Rect,
    focused: bool,
) {
    let front_input_perm = permissions.peek(perm_key);
    let focused_has_perms =
        task_list.is_detail_visible() && front_input_perm.is_some_and(|p| !p.is_askuser());
    let focused_has_askuser =
        task_list.is_detail_visible() && front_input_perm.is_some_and(|p| p.is_askuser());
    let border_color = if focused && focused_has_perms {
        Color::Yellow
    } else if focused && focused_has_askuser {
        Color::Green
    } else if focused {
        Color::Blue
    } else {
        Color::DarkGray
    };
    let searching = matches!(focus, Focus::ListSearch);
    let prefix = if !searching && task_list.is_detail_visible() {
        let name = task_list
            .selected_task()
            .map(|t| t.name.as_str())
            .unwrap_or("?");
        format!("[{name}] > ")
    } else if let Some(name) = active_project_name {
        format!("[{name}] > ")
    } else {
        "[ExO] > ".to_string()
    };
    let prefix = prefix.as_str();
    let prefix_len = prefix.len() as u16;
    // Visible width inside borders
    let visible_width = area.width.saturating_sub(2);

    let display_buf = input.display_text();
    let cursor_pos = prefix_len + input.display_cursor() as u16;
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

fn perm_hint_spans(global: &GlobalBindings) -> Vec<Span<'static>> {
    let g = |s: String| Span::styled(s, Style::default().fg(Color::Green));
    vec![
        Span::raw("  "),
        g(global.perm_approve.to_string()),
        Span::raw(" ok  "),
        g(global.perm_trust.to_string()),
        Span::raw(" trust  "),
        g(global.perm_deny.to_string()),
        Span::raw(" deny  "),
        g(global.cycle_permissions.to_string()),
        Span::raw(" next"),
    ]
}

fn askuser_hint_spans(n_opts: usize, global: &GlobalBindings) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw("  ")];
    let labels = ["1", "2", "3", "4"];
    for (i, label) in labels.iter().enumerate().take(n_opts.min(4)) {
        if i > 0 {
            spans.push(Span::raw("/"));
        }
        spans.push(Span::styled(*label, Style::default().fg(Color::Green)));
    }
    spans.push(Span::raw(" select  "));
    spans.push(Span::styled(
        global.cycle_permissions.to_string(),
        Style::default().fg(Color::Green),
    ));
    spans.push(Span::raw(" next"));
    spans
}

#[allow(clippy::too_many_arguments)]
pub(in crate::tui) fn render_prompt_bar(
    frame: &mut ratatui::Frame,
    focus: &Focus,
    show_detail: bool,
    permissions: &PermissionStore,
    perm_key: &TaskName,
    status_error: Option<&str>,
    keybindings: &Keybindings,
    area: Rect,
) {
    // Show transient error in red, replacing normal keybinding hints
    if let Some(err) = status_error {
        let bar = Paragraph::new(Line::from(vec![Span::styled(
            format!(" {err}"),
            Style::default().fg(Color::Red),
        )]));
        frame.render_widget(bar, area);
        return;
    }

    let front_p = permissions.peek(perm_key);
    let has_perms = show_detail && front_p.is_some_and(|p| !p.is_askuser());
    let kb = keybindings;
    let key = |s: String| Span::styled(s, Style::default().fg(Color::Yellow));
    let mut spans = match focus {
        Focus::ChatInput if show_detail => {
            vec![
                key(format!(" {}", kb.task_chat.send)),
                Span::raw(" send  "),
                key(format!(
                    "{}/{}",
                    kb.task_chat.next_task, kb.task_chat.prev_task
                )),
                Span::raw(" task  "),
                key(kb.task_chat.focus_history.to_string()),
                Span::raw(" scroll  "),
                key(kb.task_chat.goto_window.to_string()),
                Span::raw(" goto  "),
                key(kb.task_chat.close_task.to_string()),
                Span::raw(" close  "),
                key(kb.task_chat.focus_tasks.to_string()),
                Span::raw(" list  "),
                key(kb.global.focus_exo.to_string()),
                Span::raw(" ExO  "),
                key(kb.global.cycle_projects.to_string()),
                Span::raw(" proj  "),
                key(kb.task_chat.close_detail.to_string()),
                Span::raw(" back"),
            ]
        }
        Focus::ChatInput => {
            vec![
                key(format!(" {}", kb.chat_input.send)),
                Span::raw(" send  "),
                key(kb.chat_input.open_first_task.to_string()),
                Span::raw(" task  "),
                key(kb.chat_input.focus_up.to_string()),
                Span::raw(" scroll  "),
                key(kb.chat_input.focus_task_list.to_string()),
                Span::raw(" list  "),
                key(kb.global.focus_exo.to_string()),
                Span::raw(" ExO  "),
                key(kb.global.cycle_projects.to_string()),
                Span::raw(" proj  "),
                key(kb.chat_input.cancel_streaming.to_string()),
                Span::raw(" stop"),
            ]
        }
        Focus::ChatHistory => {
            vec![
                key(format!(" {}", kb.chat_history.navigate_down)),
                Span::raw(" input  "),
                key(kb.chat_history.scroll_up.to_string()),
                Span::raw(" up  "),
                key(kb.chat_history.scroll_down.to_string()),
                Span::raw(" down  "),
                key(kb.chat_history.navigate_right.to_string()),
                Span::raw(" tasks  "),
                key(kb.global.focus_exo.to_string()),
                Span::raw(" ExO  "),
                key(kb.global.cycle_projects.to_string()),
                Span::raw(" proj"),
            ]
        }
        Focus::ConfirmCloseTask(_)
        | Focus::ConfirmCloseProject
        | Focus::ConfirmDeleteProject(_) => {
            vec![
                key(" y".to_string()),
                Span::raw(" confirm  "),
                key("n/Esc".to_string()),
                Span::raw(" cancel"),
            ]
        }
        Focus::ListSearch => {
            vec![
                key(format!(" {}", kb.task_search.confirm)),
                Span::raw(" select  "),
                key(kb.task_search.cancel.to_string()),
                Span::raw(" cancel  "),
                key(format!("{}/{}", kb.task_search.next, kb.task_search.prev)),
                Span::raw(" navigate"),
            ]
        }
        Focus::ProjectList => {
            vec![
                key(format!(
                    " {}/{}",
                    kb.project_list.navigate_down, kb.project_list.navigate_up
                )),
                Span::raw(" navigate  "),
                key(kb.project_list.select.to_string()),
                Span::raw(" select  "),
                key(kb.project_list.search.to_string()),
                Span::raw(" search  "),
                key(kb.project_list.delete.to_string()),
                Span::raw(" delete  "),
                key(kb.project_list.back.hint_all()),
                Span::raw(" back"),
            ]
        }
        Focus::ConfirmDelete(_) => {
            vec![
                key(" y".to_string()),
                Span::raw(" delete  "),
                key("n/Esc".to_string()),
                Span::raw(" cancel"),
            ]
        }
        Focus::TaskList => {
            vec![
                key(format!(
                    " {}/{}",
                    kb.task_list.navigate_down, kb.task_list.navigate_up
                )),
                Span::raw(" navigate  "),
                key(kb.task_list.open_detail.to_string()),
                Span::raw(" open  "),
                key(kb.task_list.focus_chat.to_string()),
                Span::raw(" input  "),
                key(kb.task_list.close_detail.to_string()),
                Span::raw(" back  "),
                key(kb.task_list.goto_window.to_string()),
                Span::raw(" goto  "),
                key(kb.task_list.search.to_string()),
                Span::raw(" search  "),
                key(kb.task_list.show_projects.to_string()),
                Span::raw(" projects  "),
                key(kb.task_list.reopen_task.to_string()),
                Span::raw(" reopen  "),
                key(kb.task_list.close_task.to_string()),
                Span::raw(" close  "),
                key(kb.task_list.delete_task.to_string()),
                Span::raw(" delete  "),
                key(kb.global.focus_exo.to_string()),
                Span::raw(" exo  "),
                key(kb.global.cycle_projects.to_string()),
                Span::raw(" cycle"),
            ]
        }
    };
    let has_askuser = show_detail && front_p.is_some_and(|p| p.is_askuser());
    if has_perms {
        spans.extend(perm_hint_spans(&kb.global));
    } else if has_askuser {
        let n_opts = front_p.map(|p| p.askuser_options.len()).unwrap_or(0);
        spans.extend(askuser_hint_spans(n_opts, &kb.global));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray));

    frame.render_widget(bar, area);
}
