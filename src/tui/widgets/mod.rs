mod chat_panel;
mod confirm;
mod input_panel;
mod task_panel;

use ratatui::layout::{Constraint, Direction, Layout};

use super::state::{Focus, ScreenState};

pub(in crate::tui) fn ui(frame: &mut ratatui::Frame, state: &mut ScreenState) {
    let outer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(frame.area());

    // Left side: chat + (optional mid-panel) + input + prompt bar
    let searching = matches!(state.current_focus(), Focus::ListSearch);
    let active = state.active_state();
    let show_detail = active.task_list.is_detail_visible();
    let in_task_chat = !searching && show_detail && active.task_list.selected_task().is_some();
    let focused_perm_key = state.focused_perm_key();
    let front_perm = state.permissions.peek(&focused_perm_key);
    let show_perm = in_task_chat && front_perm.is_some_and(|p| !p.is_askuser());
    let show_askuser = in_task_chat && front_perm.is_some_and(|p| p.is_askuser());
    let show_delete = matches!(state.current_focus(), Focus::ConfirmDelete(_));
    let show_delete_project = matches!(state.current_focus(), Focus::ConfirmDeleteProject(_));
    let show_close_task = matches!(state.current_focus(), Focus::ConfirmCloseTask(_));
    let show_close_project = matches!(state.current_focus(), Focus::ConfirmCloseProject);
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

    state.update_chat_viewport_height(left[0].height);

    let active = state.active_state();
    let active_project_name = state.active_project_name.as_ref().map(|n| n.as_str());
    chat_panel::render_chat(
        frame,
        state.current_focus(),
        &active.task_list,
        &active.chat_view,
        active_project_name,
        left[0],
    );
    if show_close_task {
        confirm::render_close_task_panel(
            frame,
            state.current_focus(),
            &state.active_state().task_list.tasks,
            left[1],
        );
    } else if show_close_project {
        let name = state
            .active_project_name
            .as_ref()
            .map(|n| n.as_str())
            .unwrap_or("?");
        confirm::render_close_project_panel(frame, name, left[1]);
    } else if show_delete_project {
        confirm::render_delete_project_panel(frame, state.current_focus(), left[1]);
    } else if show_delete {
        confirm::render_delete_confirm_panel(
            frame,
            state.current_focus(),
            &state.active_state().task_list.tasks,
            left[1],
        );
    } else if show_perm {
        confirm::render_permission_panel(frame, &state.permissions, &focused_perm_key, left[1]);
    } else if show_askuser {
        confirm::render_askuser_panel(frame, &state.permissions, &focused_perm_key, left[1]);
    }

    let focused_input = matches!(state.current_focus(), Focus::ChatInput);
    let active = state.active_state();
    input_panel::render_input(
        frame,
        state.current_focus(),
        &active.task_list,
        active_project_name,
        &state.permissions,
        &focused_perm_key,
        &active.input,
        left[2],
        focused_input,
    );
    input_panel::render_prompt_bar(
        frame,
        state.current_focus(),
        show_detail,
        &state.permissions,
        &focused_perm_key,
        state.status_error(),
        &state.keybindings,
        left[3],
    );

    // Right side: task list or project list
    let focused_task_list = matches!(
        state.current_focus(),
        Focus::TaskList | Focus::ListSearch | Focus::ProjectList
    );
    let search_query = state.search_input.buffer();
    if state.project_list.is_visible() {
        task_panel::render_project_list(
            frame,
            &state.focus,
            &mut state.project_list,
            &search_query,
            outer[1],
            focused_task_list,
        );
    } else {
        let total_askuser = state.current_project_askuser_count();
        let total_perm = state
            .current_project_perm_count()
            .saturating_sub(total_askuser);
        let other_perms = state.other_project_perm_counts();
        // Use direct field access to avoid borrow conflict with active_project_name
        let active_name = state
            .active_project_name
            .as_ref()
            .map(|n| n.as_str().to_string());
        let active_name_ref = active_name.as_deref();
        let task_list = match &state.active_project_id {
            Some(pid) => {
                let pid = *pid;
                &mut state
                    .projects
                    .get_mut(&pid)
                    .unwrap_or(&mut state.exo)
                    .task_list
            }
            None => &mut state.exo.task_list,
        };
        task_panel::render_task_list(
            frame,
            &state.focus,
            task_list,
            &state.permissions,
            active_name_ref,
            total_perm,
            total_askuser,
            &other_perms,
            &search_query,
            outer[1],
            focused_task_list,
        );
    }
}
