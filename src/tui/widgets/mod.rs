mod chat_panel;
mod confirm;
mod input_panel;
mod task_panel;

use ratatui::layout::{Constraint, Direction, Layout};

use super::chat::ExoState;
use super::screen::{Focus, Screen};

pub(in crate::tui) fn ui(
    frame: &mut ratatui::Frame,
    dash: &mut Screen,
    exo: &ExoState,
    pm: Option<&ExoState>,
) {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(frame.area());

    // Left side: chat + (optional mid-panel) + input + prompt bar
    let searching = matches!(dash.focus, Focus::TaskSearch);
    let in_task_chat = !searching && dash.show_detail && dash.selected_task().is_some();
    let focused_perm_key = dash.focused_perm_key();
    let front_perm = dash.permissions.peek(&focused_perm_key);
    let show_perm = in_task_chat && front_perm.is_some_and(|p| !p.is_askuser());
    let show_askuser = in_task_chat && front_perm.is_some_and(|p| p.is_askuser());
    let show_delete = matches!(dash.focus, Focus::ConfirmDelete(_));
    let show_delete_project = matches!(dash.focus, Focus::ConfirmDeleteProject(_));
    let show_close_task = matches!(dash.focus, Focus::ConfirmCloseTask(_));
    let show_close_project = matches!(dash.focus, Focus::ConfirmCloseProject);
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

    chat_panel::render_chat(frame, dash, exo, pm, left[0]);
    if show_close_task {
        confirm::render_close_task_panel(frame, dash, left[1]);
    } else if show_close_project {
        confirm::render_close_project_panel(frame, dash, left[1]);
    } else if show_delete_project {
        confirm::render_delete_project_panel(frame, dash, left[1]);
    } else if show_delete {
        confirm::render_delete_confirm_panel(frame, dash, left[1]);
    } else if show_perm {
        confirm::render_permission_panel(frame, dash, left[1]);
    } else if show_askuser {
        confirm::render_askuser_panel(frame, dash, left[1]);
    }

    let focused_input = matches!(dash.focus, Focus::ChatInput);
    input_panel::render_input(frame, dash, left[2], focused_input);
    input_panel::render_prompt_bar(frame, dash, left[3]);

    // Right side: task list or project list
    let focused_task_list = matches!(
        dash.focus,
        Focus::TaskList | Focus::TaskSearch | Focus::ProjectList
    );
    task_panel::render_task_list(frame, dash, outer[1], focused_task_list);
}
