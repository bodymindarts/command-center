use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::primitives::TaskName;

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

pub(in crate::tui) fn render_prompt_bar(
    frame: &mut ratatui::Frame,
    focus: &Focus,
    show_detail: bool,
    permissions: &PermissionStore,
    perm_key: &TaskName,
    status_error: Option<&str>,
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
    let mut spans = match focus {
        Focus::ChatInput if show_detail => {
            vec![
                Span::styled(" Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" send  "),
                Span::styled("Tab/S-Tab", Style::default().fg(Color::Yellow)),
                Span::raw(" task  "),
                Span::styled("^K", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll  "),
                Span::styled("^G", Style::default().fg(Color::Yellow)),
                Span::raw(" goto  "),
                Span::styled("^X", Style::default().fg(Color::Yellow)),
                Span::raw(" close  "),
                Span::styled("^L", Style::default().fg(Color::Yellow)),
                Span::raw(" list  "),
                Span::styled("^O", Style::default().fg(Color::Yellow)),
                Span::raw(" ExO  "),
                Span::styled("^R", Style::default().fg(Color::Yellow)),
                Span::raw(" proj  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back"),
            ]
        }
        Focus::ChatInput => {
            vec![
                Span::styled(" Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" send  "),
                Span::styled("Tab", Style::default().fg(Color::Yellow)),
                Span::raw(" task  "),
                Span::styled("^K", Style::default().fg(Color::Yellow)),
                Span::raw(" scroll  "),
                Span::styled("^L", Style::default().fg(Color::Yellow)),
                Span::raw(" list  "),
                Span::styled("^O", Style::default().fg(Color::Yellow)),
                Span::raw(" ExO  "),
                Span::styled("^R", Style::default().fg(Color::Yellow)),
                Span::raw(" proj  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" stop"),
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
                Span::styled("^O", Style::default().fg(Color::Yellow)),
                Span::raw(" ExO  "),
                Span::styled("^R", Style::default().fg(Color::Yellow)),
                Span::raw(" proj"),
            ]
        }
        Focus::ConfirmCloseTask(_)
        | Focus::ConfirmCloseProject
        | Focus::ConfirmDeleteProject(_) => {
            vec![
                Span::styled(" y", Style::default().fg(Color::Yellow)),
                Span::raw(" confirm  "),
                Span::styled("n/Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel"),
            ]
        }
        Focus::ListSearch => {
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
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(" search  "),
                Span::styled("\u{232b}", Style::default().fg(Color::Yellow)),
                Span::raw(" delete  "),
                Span::styled("p/Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ]
        }
        Focus::ConfirmDelete(_) => {
            vec![
                Span::styled(" y", Style::default().fg(Color::Yellow)),
                Span::raw(" delete  "),
                Span::styled("n/Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel"),
            ]
        }
        Focus::TaskList => {
            vec![
                Span::styled(" j/k", Style::default().fg(Color::Yellow)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" open  "),
                Span::styled("Tab", Style::default().fg(Color::Yellow)),
                Span::raw(" input  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back  "),
                Span::styled("^G", Style::default().fg(Color::Yellow)),
                Span::raw(" goto  "),
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(" search  "),
                Span::styled("p", Style::default().fg(Color::Yellow)),
                Span::raw(" projects  "),
                Span::styled("r", Style::default().fg(Color::Yellow)),
                Span::raw(" reopen  "),
                Span::styled("x", Style::default().fg(Color::Yellow)),
                Span::raw(" close  "),
                Span::styled("\u{232b}", Style::default().fg(Color::Yellow)),
                Span::raw(" delete  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ]
        }
    };
    let has_askuser = show_detail && front_p.is_some_and(|p| p.is_askuser());
    if has_perms {
        spans.extend(perm_hint_spans());
    } else if has_askuser {
        let n_opts = front_p.map(|p| p.askuser_options.len()).unwrap_or(0);
        spans.extend(askuser_hint_spans(n_opts));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray));

    frame.render_widget(bar, area);
}
