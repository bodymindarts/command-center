use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::ClatApp;
use crate::permission::PermissionRequest;
use crate::primitives::{ChatId, MessageRole, ProjectId, TaskName};
use crate::runtime::Runtime;

use super::ProjectContext;
use super::permissions::ActivePermission;
use super::state::{Focus, ScreenState};
use super::telegram;
use crate::assistant::{AssistantEvent, AssistantSession, ProjectEvent};

const EXO_PERM_KEY: &str = "exo";

// ── Helper functions ────────────────────────────────────────────────

/// Find the task name whose work_dir is a prefix of the given CWD.
pub(super) fn find_task_name_by_cwd(
    work_dirs: &[(TaskName, String)],
    cwd: &std::path::Path,
) -> Option<TaskName> {
    work_dirs
        .iter()
        .find(|(_, wd)| {
            let canon = std::fs::canonicalize(wd).unwrap_or_else(|_| std::path::PathBuf::from(wd));
            cwd.starts_with(&canon)
        })
        .map(|(name, _)| name.clone())
}

pub(super) fn write_response_to_stream(
    mut stream: UnixStream,
    allow: bool,
    permission_suggestions: Option<&[serde_json::Value]>,
) -> std::io::Result<()> {
    use std::io::Write;
    let response = crate::permission::make_response_json(allow, None, permission_suggestions);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn write_response_with_message(
    mut stream: UnixStream,
    allow: bool,
    message: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let response = crate::permission::make_response_json(allow, Some(message), None);
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Extract the first question and its options from an AskUserQuestion tool_input.
fn parse_ask_user_options(
    tool_input: Option<&serde_json::Value>,
) -> Option<(String, Vec<(String, String)>)> {
    let input = tool_input?;
    let questions = input.get("questions")?.as_array()?;
    let first = questions.first()?;
    let question = first.get("question")?.as_str()?.to_string();
    let options = first.get("options")?.as_array()?;
    let parsed: Vec<(String, String)> = options
        .iter()
        .filter_map(|opt| {
            let label = opt.get("label")?.as_str()?.to_string();
            let desc = opt
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            Some((label, desc))
        })
        .collect();
    if parsed.is_empty() {
        return None;
    }
    Some((question, parsed))
}

/// Resolve and consume the active permission request.
fn resolve_permission(
    state: &mut ScreenState,
    allow: bool,
) -> Option<(UnixStream, bool, Vec<serde_json::Value>, u64)> {
    let perm_key = state.active_permission_key()?;
    let perm = state.permissions.take(&perm_key)?;
    Some((
        perm.stream,
        allow,
        perm.permission_suggestions,
        perm.perm_id,
    ))
}

/// Notify Telegram that a permission was resolved (if the bot is active).
pub(super) fn notify_tg_resolved(
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    perm_id: u64,
    outcome: &str,
) {
    if let Some(tx) = tg_tx {
        let _ = tx.send(telegram::TgOutbound::Resolved {
            perm_id,
            outcome: outcome.to_string(),
        });
    }
}

// ── Shared input editing ────────────────────────────────────────────

/// Handle common text-editing key events on an InputState.
/// Returns true if the key was consumed.
fn handle_input_editing(input: &mut super::input::InputState, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('u') if ctrl => {
            input.kill_before();
            true
        }
        KeyCode::Char('w') if ctrl => {
            input.kill_word();
            true
        }
        KeyCode::Char('a') if ctrl => {
            input.home();
            true
        }
        KeyCode::Char('e') if ctrl => {
            input.end();
            true
        }
        KeyCode::Char(c) if !ctrl => {
            input.insert(c);
            true
        }
        KeyCode::Backspace => {
            input.backspace();
            true
        }
        KeyCode::Delete => {
            input.delete();
            true
        }
        KeyCode::Left => {
            input.left();
            true
        }
        KeyCode::Right => {
            input.right();
            true
        }
        KeyCode::Home => {
            input.home();
            true
        }
        KeyCode::End => {
            input.end();
            true
        }
        _ => false,
    }
}

// ── Paste handling ──────────────────────────────────────────────────

pub(super) fn handle_paste(state: &mut ScreenState, text: String) {
    if matches!(state.current_focus(), Focus::ChatInput) {
        let input = &mut state.active_state_mut().input;
        if text.contains('\n') || text.contains('\r') {
            input.set_paste(text);
        } else {
            for c in text.chars() {
                input.insert(c);
            }
        }
    }
}

// ── Global key handler ──────────────────────────────────────────────

/// Handle global key shortcuts (Ctrl+C, Ctrl+P, Ctrl+Y/T/N, number keys, Ctrl+O, Ctrl+R).
/// Returns true if the key was consumed.
pub(super) fn handle_global_keys<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Ctrl+C quits
    if ctrl && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return true;
    }

    // Ctrl+P cycles to next task with pending permissions
    if ctrl && key.code == KeyCode::Char('p') {
        return handle_cycle_permissions(state, app);
    }

    // Permission keys — global so they work regardless of focus (ChatInput, ChatHistory, TaskList)
    // while a task detail is showing. Guarded by show_detail + permissions.peek().
    let show_detail = state.active_state().task_list.show_detail;
    // Ctrl+Y one-time allow
    if ctrl
        && key.code == KeyCode::Char('y')
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(state, true) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "✅ Approved locally");
        }
        return true;
    }

    // Ctrl+T trust / always-allow
    if ctrl
        && key.code == KeyCode::Char('t')
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, suggestions, perm_id)) = resolve_permission(state, true) {
            let _ = write_response_to_stream(stream, allow, Some(&suggestions));
            notify_tg_resolved(tg_tx, perm_id, "✅ Trusted locally");
        }
        return true;
    }

    // Ctrl+N denies permission
    if ctrl
        && key.code == KeyCode::Char('n')
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(state, false) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "❌ Denied locally");
        }
        return true;
    }

    // Number keys 1-4 answer an AskUser prompt
    if matches!(key.code, KeyCode::Char('1'..='4'))
        && key.modifiers.is_empty()
        && show_detail
        && state
            .permissions
            .peek(&state.focused_perm_key())
            .is_some_and(|p| p.is_askuser())
    {
        handle_askuser_select(state, key, tg_tx);
        return true;
    }

    // Ctrl+O returns to ExO chat
    if ctrl && key.code == KeyCode::Char('o') {
        if let Ok(tasks) = app.list_visible(None) {
            state.switch_to_project(None, tasks, None);
        }
        return true;
    }

    // Ctrl+R returns to PM (or last active project from ExO)
    if ctrl && key.code == KeyCode::Char('r') {
        handle_goto_project(state, app);
        return true;
    }

    false
}

fn handle_cycle_permissions<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) -> bool {
    let names = state.permissions.task_names_with_pending();
    if !names.is_empty() {
        let current = state.focused_perm_key();
        let idx = names
            .iter()
            .position(|n| n == &current)
            .map(|i| (i + 1) % names.len())
            .unwrap_or(0);
        let name = names[idx].clone();
        if name == EXO_PERM_KEY {
            // Navigate to ExO view
            if state.active_project_id.is_some() {
                if let Ok(tasks) = app.list_visible(None) {
                    state.switch_to_project(None, tasks, None);
                }
            } else {
                state.active_state_mut().task_list.show_detail = false;
            }
        } else if let Some(pos) = state
            .active_state()
            .task_list
            .tasks
            .iter()
            .position(|t| t.name == name)
        {
            // Task is in the current project view
            state.open_task_detail(pos);
        } else {
            // Task is in a different project — switch to it
            let target_pid = state.global_task_projects.get(&name).cloned().flatten();
            if let Some(pid) = target_pid {
                if let Ok(projects) = app.list_projects() {
                    state.project_list.projects = projects;
                }
                let proj_name = state
                    .project_list
                    .projects
                    .iter()
                    .find(|p| p.id == pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| {
                        crate::primitives::ProjectName::from(pid.as_str().to_string())
                    });
                if let Ok(tasks) = app.list_visible(Some(&pid)) {
                    state.switch_to_project(Some((proj_name, pid.clone())), tasks, Some(&name));
                }
            } else if let Ok(tasks) = app.list_visible(None) {
                state.switch_to_project(None, tasks, Some(&name));
            }
        }
    } else if let Some(idx) = state.active_state().task_list.list_state.selected() {
        state.open_task_detail(idx);
    }
    true
}

fn handle_askuser_select(
    state: &mut ScreenState,
    key: KeyEvent,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    let digit = match key.code {
        KeyCode::Char(c) => c.to_digit(10).unwrap_or(1) as usize,
        _ => 1,
    };
    let perm_key = state.focused_perm_key();
    if let Some(perm) = state.permissions.peek(&perm_key) {
        let idx = digit - 1;
        if idx < perm.askuser_options.len() {
            let label = perm.askuser_options[idx].0.clone();
            let perm_id = perm.perm_id;
            if let Some(perm) = state.permissions.take(&perm_key) {
                let _ = write_response_with_message(perm.stream, true, &label);
                notify_tg_resolved(tg_tx, perm_id, &format!("✅ Selected: {label}"));
            }
        }
    }
}

fn handle_goto_project<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    // If in a project's task detail, go back to PM chat
    if state.active_state().task_list.show_detail && state.active_project_id.is_some() {
        state.close_task_detail();
    // If in ExO view, restore last active project (or first project)
    } else if state.active_project_id.is_none() {
        if let Ok(projects) = app.list_projects() {
            state.project_list.projects = projects;
        }
        // Try last_project_id first, then fall back to first project
        let target = state
            .last_project_id
            .as_ref()
            .and_then(|pid| {
                state
                    .project_list
                    .projects
                    .iter()
                    .find(|p| p.id == *pid)
                    .map(|p| (p.name.clone(), p.id.clone()))
            })
            .or_else(|| {
                state
                    .project_list
                    .projects
                    .first()
                    .map(|p| (p.name.clone(), p.id.clone()))
            });
        if let Some((name, id)) = target
            && let Ok(tasks) = app.list_visible(Some(&id))
        {
            state.switch_to_project(Some((name, id.clone())), tasks, None);
        }
    // If in a project PM view, cycle to next project
    } else if state.active_project_id.is_some() && !state.active_state().task_list.show_detail {
        if let Ok(projects) = app.list_projects() {
            state.project_list.projects = projects;
        }
        let cur_idx = state.active_project_id.as_ref().and_then(|pid| {
            state
                .project_list
                .projects
                .iter()
                .position(|p| p.id == *pid)
        });
        if let Some(ci) = cur_idx {
            let next_idx = (ci + 1) % state.project_list.projects.len();
            let next = &state.project_list.projects[next_idx];
            let next_id = next.id.clone();
            let next_name = next.name.clone();
            if let Ok(tasks) = app.list_visible(Some(&next_id)) {
                state.switch_to_project(Some((next_name, next_id.clone())), tasks, None);
            }
        }
    }
}

// ── Per-focus key handlers ──────────────────────────────────────────

pub(super) fn handle_focus_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    match state.current_focus() {
        Focus::TaskList => handle_task_list_key(state, key, app),
        Focus::TaskSearch => handle_task_search_key(state, key),
        Focus::ProjectList => handle_project_list_key(state, key, app),
        Focus::ChatInput if state.active_state().task_list.show_detail => {
            handle_task_chat_input_key(state, key, app)
        }
        Focus::ChatInput => handle_chat_input_key(state, key, app, exo_session, project_contexts),
        Focus::ChatHistory => handle_chat_history_key(state, key),
        Focus::ConfirmDelete(_) => handle_confirm_delete_key(state, key, app),
        Focus::ConfirmCloseTask(_) => handle_confirm_close_task_key(state, key, app),
        Focus::ConfirmDeleteProject(_) => handle_confirm_delete_project_key(state, key, app),
        Focus::ConfirmCloseProject => {
            handle_confirm_close_project_key(state, key, app, project_contexts)
        }
    }
}

fn handle_task_list_key<R: Runtime>(state: &mut ScreenState, key: KeyEvent, app: &ClatApp<R>) {
    match key.code {
        KeyCode::Char('q') => state.should_quit = true,
        KeyCode::Esc => {
            state.close_task_detail();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.next();
            let active = state.active_state_mut();
            active.task_list.show_detail = true;
            active.task_list.detail_scroll = 0;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.previous();
            let active = state.active_state_mut();
            active.task_list.show_detail = true;
            active.task_list.detail_scroll = 0;
        }
        KeyCode::PageDown => {
            state.scroll_detail_down();
        }
        KeyCode::PageUp => {
            state.scroll_detail_up();
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_detail_down();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.scroll_detail_up();
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            goto_task_window(state, app);
        }
        KeyCode::Enter => {
            if let Some(idx) = state.active_state().task_list.list_state.selected() {
                state.open_task_detail(idx);
            }
        }
        KeyCode::Char('x') => {
            if let Some(task) = state.selected_task()
                && task.status.is_running()
            {
                let id = task.id.clone();
                state.focus = Focus::ConfirmCloseTask(id);
            }
        }
        KeyCode::Char('r') => {
            reopen_task(state, app);
        }
        KeyCode::Backspace => {
            if let Some(task) = state.selected_task() {
                let id = task.id.clone();
                state.focus = Focus::ConfirmDelete(id);
            }
        }
        KeyCode::Char('/') => {
            state.enter_search_mode();
        }
        KeyCode::Tab => {
            state.focus = Focus::ChatInput;
        }
        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = Focus::ChatInput;
        }
        KeyCode::Char('p') => {
            if let Ok(projects) = app.list_projects() {
                state.show_project_list(projects);
            } else {
                state.project_list.show_projects = true;
                state.focus = Focus::ProjectList;
            }
        }
        _ => {}
    }
}

fn handle_task_search_key(state: &mut ScreenState, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let searching_projects = state.project_list.show_projects;
    let do_filter = |state: &mut ScreenState| {
        if state.project_list.show_projects {
            state.update_project_search_filter();
        } else {
            state.update_search_filter();
        }
    };
    match key.code {
        KeyCode::Esc => {
            state.exit_search();
        }
        KeyCode::Enter => {
            state.confirm_search_selection();
        }
        KeyCode::Down | KeyCode::Tab => {
            if searching_projects {
                state.search_next_project();
            } else {
                state.active_state_mut().task_list.search_next();
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if searching_projects {
                state.search_prev_project();
            } else {
                state.active_state_mut().task_list.search_prev();
            }
        }
        KeyCode::Char('n') if ctrl => {
            if searching_projects {
                state.search_next_project();
            } else {
                state.active_state_mut().task_list.search_next();
            }
        }
        KeyCode::Char('p') if ctrl => {
            if searching_projects {
                state.search_prev_project();
            } else {
                state.active_state_mut().task_list.search_prev();
            }
        }
        KeyCode::Char('k') if ctrl => {
            state.search_input.kill_line();
            do_filter(state);
        }
        _ => {
            if handle_input_editing(&mut state.search_input, &key) {
                do_filter(state);
            }
        }
    }
}

fn handle_project_list_key<R: Runtime>(state: &mut ScreenState, key: KeyEvent, app: &ClatApp<R>) {
    match key.code {
        KeyCode::Char('q') => state.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => state.next_project(),
        KeyCode::Char('k') | KeyCode::Up => state.previous_project(),
        KeyCode::Char('/') => {
            state.search_input.take();
            state.update_project_search_filter();
            state.focus = Focus::TaskSearch;
        }
        KeyCode::Enter => {
            if let Some(project) = state.selected_project() {
                let project_id = project.id.clone();
                let project_name = project.name.clone();
                if let Ok(tasks) = app.list_visible(Some(&project_id)) {
                    state.switch_to_project(Some((project_name, project_id.clone())), tasks, None);
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(project) = state.selected_project() {
                let name = project.name.clone();
                state.focus = Focus::ConfirmDeleteProject(name);
            }
        }
        KeyCode::Char('p') | KeyCode::Esc => {
            if let Ok(tasks) = app.list_visible(None) {
                state.switch_to_project(None, tasks, None);
                state.focus = Focus::TaskList;
            }
        }
        _ => {}
    }
}

fn handle_task_chat_input_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            state.close_task_detail();
        }
        KeyCode::Tab => {
            state.navigate_to_adjacent_task(true);
        }
        KeyCode::BackTab => {
            state.navigate_to_adjacent_task(false);
        }
        KeyCode::Char('k') if ctrl => {
            state.focus = Focus::ChatHistory;
        }
        KeyCode::Char('x') if ctrl => {
            if let Some(task) = state.selected_task()
                && task.status.is_running()
            {
                let id = task.id.clone();
                state.focus = Focus::ConfirmCloseTask(id);
            }
        }
        KeyCode::Char('l') if ctrl => {
            state.focus = Focus::TaskList;
        }
        KeyCode::Char('g') if ctrl => {
            goto_task_window(state, app);
        }
        KeyCode::Enter => {
            let active = state.active_state_mut();
            if !active.input.is_empty() {
                let msg = active.input.take();
                if let Some(task) = active.task_list.selected_task() {
                    let task_id = task.id.as_str().to_string();
                    let pane = task.tmux_pane.clone();
                    match app.send(&task_id, &msg) {
                        Ok(_) => {
                            if let Some(pane) = pane {
                                active.task_list.idle_panes.remove(&pane);
                            }
                        }
                        Err(e) => {
                            state.status_error = Some(format!("send: {e}"));
                        }
                    }
                }
            }
        }
        _ => {
            handle_input_editing(&mut state.active_state_mut().input, &key);
        }
    }
}

fn handle_chat_input_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            let active = state.active_state_mut();
            if active.chat_view.assistant.streaming {
                active.chat_view.assistant.finish_streaming();
            }
            active.chat_view.reset_scroll();
        }
        KeyCode::Tab => {
            let has_tasks = !state.active_state().task_list.tasks.is_empty();
            if has_tasks {
                state.open_task_detail(0);
            } else {
                state.active_state_mut().chat_view.reset_scroll();
            }
        }
        KeyCode::BackTab => {
            let last = state.active_state().task_list.tasks.len().checked_sub(1);
            if let Some(last) = last {
                state.open_task_detail(last);
            } else {
                state.active_state_mut().chat_view.reset_scroll();
            }
        }
        KeyCode::Char('k') if ctrl => {
            state.focus = Focus::ChatHistory;
        }
        KeyCode::Char('x') if ctrl && state.active_project_name.is_some() => {
            state.focus = Focus::ConfirmCloseProject;
        }
        KeyCode::Char('l') if ctrl => {
            state.focus = Focus::TaskList;
            let active = state.active_state_mut();
            if active.task_list.list_state.selected().is_some() {
                active.task_list.show_detail = true;
                active.task_list.detail_scroll = 0;
            }
        }
        KeyCode::Enter => {
            handle_chat_enter(state, app, exo_session, project_contexts);
        }
        _ => {
            handle_input_editing(&mut state.active_state_mut().input, &key);
        }
    }
}

fn handle_chat_enter<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    let active = state.active_state_mut();
    active.chat_view.reset_scroll();
    if active.input.is_empty() {
        return;
    }
    if let Some(pid) = state.active_project_id.clone() {
        let Some(ctx) = project_contexts.get_mut(&pid) else {
            state.status_error = Some("PM session not initialized".to_string());
            return;
        };
        let active = state.active_state_mut();
        let chat = &mut active.chat_view.assistant;
        if chat.streaming {
            chat.finish_streaming();
            if let Some(msg) = chat.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ =
                    app.insert_project_message(&pid, MessageRole::Assistant, &msg.text_content());
            }
        }
        let msg = active.input.take();
        let _ = app.insert_project_message(&pid, MessageRole::User, &msg);
        chat.add_user_message(msg.clone());
        ctx.session.send_message(&msg, chat.session_id.as_deref());
        active.chat_view.reset_scroll();
    } else {
        let active = state.active_state_mut();
        let chat = &mut active.chat_view.assistant;
        if chat.streaming {
            chat.finish_streaming();
            if let Some(msg) = chat.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
            }
        }
        let msg = active.input.take();
        let _ = app.insert_exo_message(MessageRole::User, &msg);
        chat.add_user_message(msg.clone());
        exo_session.send_message(&msg, chat.session_id.as_deref());
        active.chat_view.reset_scroll();
    }
}

fn handle_chat_history_key(state: &mut ScreenState, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('j') if ctrl => {
            state.navigate_focus_down();
        }
        KeyCode::Char('u') if ctrl => {
            state.scroll_chat_up();
        }
        KeyCode::Char('d') if ctrl => {
            state.scroll_chat_down();
        }
        KeyCode::Char('l') if ctrl => {
            state.navigate_focus_right();
        }
        _ => {}
    }
}

fn handle_confirm_delete_key<R: Runtime>(state: &mut ScreenState, key: KeyEvent, app: &ClatApp<R>) {
    let task_id = match state.current_focus() {
        Focus::ConfirmDelete(id) => id.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = app.delete(task_id.as_str());
            if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()) {
                state.refresh_tasks(tasks);
            }
            state.focus = Focus::TaskList;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus = Focus::TaskList;
        }
        _ => {}
    }
}

fn handle_confirm_close_task_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let task_id = match state.current_focus() {
        Focus::ConfirmCloseTask(id) => id.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = app.close(task_id.as_str());
            if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()) {
                state.refresh_tasks(tasks);
            }
            state.close_task_detail();
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus = if state.active_state().task_list.show_detail {
                Focus::ChatInput
            } else {
                Focus::TaskList
            };
        }
        _ => {}
    }
}

fn handle_confirm_delete_project_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let project_name = match state.current_focus() {
        Focus::ConfirmDeleteProject(name) => name.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = app.delete_project(project_name.as_str());
            if let Ok(projects) = app.list_projects() {
                state.refresh_projects(projects);
            }
            state.focus = Focus::ProjectList;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus = Focus::ProjectList;
        }
        _ => {}
    }
}

fn handle_confirm_close_project_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    match key.code {
        KeyCode::Char('y') => {
            let closed_pid = state.active_project_id.clone();
            if let Ok(tasks) = app.list_visible(None) {
                state.switch_to_project(None, tasks, None);
                state.focus = Focus::TaskList;
            }
            if let Some(pid) = closed_pid {
                super::cancel_project_context(project_contexts, state, &pid);
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus = Focus::ChatInput;
        }
        _ => {}
    }
}

/// Shared: go to the selected task's tmux window (or reopen if closed).
fn goto_task_window<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    if let Some(task) = state.selected_task() {
        if task.status.is_running() {
            if let Some(window_id) = &task.tmux_window {
                app.goto_window(window_id);
            }
        } else {
            let id = task.id.as_str().to_string();
            match app.reopen(&id) {
                Ok(window_id) => {
                    if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()) {
                        state.refresh_tasks(tasks);
                    }
                    app.goto_window(&window_id);
                }
                Err(e) => {
                    state.status_error = Some(format!("reopen: {e}"));
                }
            }
        }
    }
}

/// Shared: reopen a closed task.
fn reopen_task<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    if let Some(task) = state.selected_task()
        && !task.status.is_running()
    {
        let id = task.id.as_str().to_string();
        match app.reopen(&id) {
            Ok(_) => {
                if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()) {
                    state.refresh_tasks(tasks);
                }
            }
            Err(e) => {
                state.status_error = Some(format!("reopen: {e}"));
            }
        }
    }
}

// ── Channel draining ────────────────────────────────────────────────

pub(super) fn drain_exo_events<R: Runtime>(
    exo_session: &mut AssistantSession,
    app: &ClatApp<R>,
    rx: &mpsc::Receiver<AssistantEvent>,
    state: &mut ScreenState,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    while let Ok(ev) = rx.try_recv() {
        let chat = &mut state.exo.chat_view.assistant;
        match ev {
            AssistantEvent::TextDelta(text) => {
                if chat.streaming {
                    chat.append_text(&text);
                    if state.active_project_id.is_none() {
                        state.exo.chat_view.reset_scroll();
                    }
                    if let Some(tx) = tg_tx {
                        let _ = tx.send(telegram::TgOutbound::ExoTextDelta { text: text.clone() });
                    }
                }
            }
            AssistantEvent::ToolStart(name) => {
                if chat.streaming {
                    chat.add_tool_activity(name);
                }
            }
            AssistantEvent::SessionId(id) => {
                app.write_exo_session_id(&id);
                chat.session_id = Some(id.clone());
                exo_session.set_session_id(id);
            }
            AssistantEvent::TurnDone => {
                chat.finish_streaming();
                if let Some(msg) = chat.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                }
                if let Some(tx) = tg_tx {
                    let _ = tx.send(telegram::TgOutbound::ExoTurnDone);
                }
            }
            AssistantEvent::ProcessExited => {
                chat.had_process_error = false;
                exo_session.mark_exited();
                if chat.streaming {
                    chat.add_error("Claude process exited unexpectedly");
                }
            }
            AssistantEvent::Error(e) => {
                chat.had_process_error = true;
                chat.add_error(&e);
                if let Some(msg) = chat.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                }
            }
        }
    }
}

pub(super) fn drain_project_events<R: Runtime>(
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>,
    project_rx: &mpsc::Receiver<ProjectEvent>,
    state: &mut ScreenState,
) {
    while let Ok(ev) = project_rx.try_recv() {
        let project_id = ev.project_id;
        let is_active_project = state.active_project_id.as_ref() == Some(&project_id);
        let Some(project_state) = state.projects.get_mut(&project_id) else {
            continue;
        };
        let chat = &mut project_state.chat_view.assistant;
        match ev.inner {
            AssistantEvent::TextDelta(text) => {
                if chat.streaming {
                    chat.append_text(&text);
                    if is_active_project {
                        project_state.chat_view.reset_scroll();
                    }
                }
            }
            AssistantEvent::ToolStart(name) => {
                if chat.streaming {
                    chat.add_tool_activity(name);
                }
            }
            AssistantEvent::SessionId(id) => {
                app.write_project_session_id(&project_id, &id);
                chat.session_id = Some(id.clone());
                if let Some(ctx) = project_contexts.get_mut(&project_id) {
                    ctx.session.set_session_id(id);
                }
            }
            AssistantEvent::TurnDone => {
                chat.finish_streaming();
                if let Some(msg) = chat.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = app.insert_project_message(
                        &project_id,
                        MessageRole::Assistant,
                        &msg.text_content(),
                    );
                }
            }
            AssistantEvent::ProcessExited => {
                chat.had_process_error = false;
                if let Some(ctx) = project_contexts.get_mut(&project_id) {
                    ctx.session.mark_exited();
                }
                if chat.streaming {
                    chat.add_error("PM process exited unexpectedly");
                }
            }
            AssistantEvent::Error(e) => {
                chat.had_process_error = true;
                chat.add_error(&e);
                if let Some(msg) = chat.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = app.insert_project_message(
                        &project_id,
                        MessageRole::Assistant,
                        &msg.text_content(),
                    );
                }
            }
        }
    }
}

/// Find the project state that contains a task with the given name.
fn find_project_state_for_task<'a>(
    state: &'a mut ScreenState,
    task_name: &TaskName,
) -> Option<&'a mut super::state::TaskListState> {
    // Check ExO tasks
    if state
        .exo
        .task_list
        .tasks
        .iter()
        .any(|t| t.name == *task_name)
    {
        return Some(&mut state.exo.task_list);
    }
    // Check project tasks
    for ps in state.projects.values_mut() {
        if ps.task_list.tasks.iter().any(|t| t.name == *task_name) {
            return Some(&mut ps.task_list);
        }
    }
    None
}

pub(super) fn drain_resolved(
    state: &mut ScreenState,
    resolved_rx: &mpsc::Receiver<String>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
) {
    while let Ok(cwd) = resolved_rx.try_recv() {
        let resolved_cwd =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        let task_name = find_task_name_by_cwd(&state.global_task_work_dirs, &resolved_cwd);
        if let Some(ref name) = task_name {
            if let Some(task_list) = find_project_state_for_task(state, name)
                && let Some(pane_id) = task_list
                    .tasks
                    .iter()
                    .find(|t| t.name == *name)
                    .and_then(|t| t.tmux_pane.as_ref())
            {
                task_list.idle_panes.remove(pane_id);
            }
            if let Some(perm) = state.permissions.take(name) {
                let _ = write_response_to_stream(perm.stream, false, None);
                if tg_perm_ids.remove(&perm.perm_id) {
                    notify_tg_resolved(tg_tx, perm.perm_id, "✅ Resolved in pane");
                }
            }
        }
    }
}

pub(super) fn drain_idle(state: &mut ScreenState, idle_rx: &mpsc::Receiver<String>) {
    while let Ok(cwd) = idle_rx.try_recv() {
        let cwd_path =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        if let Some(task_name) = find_task_name_by_cwd(&state.global_task_work_dirs, &cwd_path)
            && let Some(task_list) = find_project_state_for_task(state, &task_name)
            && let Some(pane_id) = task_list
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            task_list.idle_panes.insert(pane_id.clone());
        }
    }
}

/// Drain active notifications from Notification hooks — mark pane as not idle.
pub(super) fn drain_active(state: &mut ScreenState, active_rx: &mpsc::Receiver<String>) {
    while let Ok(cwd) = active_rx.try_recv() {
        let cwd_path =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        if let Some(task_name) = find_task_name_by_cwd(&state.global_task_work_dirs, &cwd_path)
            && let Some(task_list) = find_project_state_for_task(state, &task_name)
            && let Some(pane_id) = task_list
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            task_list.idle_panes.remove(pane_id);
        }
    }
}

pub(super) fn drain_permissions(
    state: &mut ScreenState,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
    perm_id_counter: &mut u64,
) {
    while let Ok((stream, req)) = perm_rx.try_recv() {
        let req_cwd =
            std::fs::canonicalize(&req.cwd).unwrap_or_else(|_| std::path::PathBuf::from(&req.cwd));
        let task_name = find_task_name_by_cwd(&state.global_task_work_dirs, &req_cwd)
            .unwrap_or_else(|| TaskName::from(EXO_PERM_KEY.to_string()));
        if let Some(task_list) = find_project_state_for_task(state, &task_name)
            && let Some(pane_id) = task_list
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            task_list.idle_panes.remove(pane_id);
        }
        *perm_id_counter += 1;
        let perm_id = *perm_id_counter;
        if let Some(tx) = tg_tx {
            if req.tool_name == "AskUserQuestion"
                && let Some((question, options)) = parse_ask_user_options(req.tool_input.as_ref())
            {
                let _ = tx.send(telegram::TgOutbound::NewQuestion {
                    perm_id,
                    task_name: task_name.to_string(),
                    question,
                    options,
                });
            } else {
                let _ = tx.send(telegram::TgOutbound::NewPermission {
                    perm_id,
                    task_name: task_name.to_string(),
                    tool_name: req.tool_name.clone(),
                    tool_input_summary: req.tool_input_summary.clone(),
                });
            }
            tg_perm_ids.insert(perm_id);
        }
        let (askuser_question, askuser_options) = if req.tool_name == "AskUserQuestion"
            && let Some((q, opts)) = parse_ask_user_options(req.tool_input.as_ref())
        {
            (Some(q), opts)
        } else {
            (None, Vec::new())
        };
        let perm = ActivePermission {
            perm_id,
            stream,
            task_name: task_name.clone(),
            tool_name: req.tool_name,
            tool_input_summary: req.tool_input_summary,
            permission_suggestions: req.permission_suggestions,
            askuser_question,
            askuser_options,
        };
        state.permissions.add(perm);
    }
}

pub(super) fn drain_telegram<R: Runtime>(
    state: &mut ScreenState,
    exo_session: &mut AssistantSession,
    app: &ClatApp<R>,
    tg_rx: Option<&mpsc::Receiver<telegram::TgInbound>>,
) {
    let Some(rx) = tg_rx else { return };
    while let Ok(tg_msg) = rx.try_recv() {
        match tg_msg {
            telegram::TgInbound::PermissionDecision { perm_id, action } => {
                let task_name = state
                    .permissions
                    .iter()
                    .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                    .map(|(name, _)| name.clone());
                if let Some(name) = task_name
                    && state
                        .permissions
                        .peek(&name)
                        .is_some_and(|front| front.perm_id == perm_id)
                    && let Some(perm) = state.permissions.take(&name)
                {
                    let (allow, suggestions) = match action {
                        telegram::PermAction::Approve => (true, None),
                        telegram::PermAction::Trust => {
                            (true, Some(perm.permission_suggestions.clone()))
                        }
                        telegram::PermAction::Deny => (false, None),
                    };
                    let _ = write_response_to_stream(perm.stream, allow, suggestions.as_deref());
                }
            }
            telegram::TgInbound::QuestionAnswer { perm_id, answer } => {
                let task_name = state
                    .permissions
                    .iter()
                    .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                    .map(|(name, _)| name.clone());
                if let Some(name) = task_name
                    && state
                        .permissions
                        .peek(&name)
                        .is_some_and(|front| front.perm_id == perm_id)
                    && let Some(perm) = state.permissions.take(&name)
                {
                    let _ = write_response_with_message(perm.stream, true, &answer);
                }
            }
            telegram::TgInbound::ExoMessage { text } => {
                state.exo.chat_view.reset_scroll();
                let chat = &mut state.exo.chat_view.assistant;
                if chat.streaming {
                    chat.finish_streaming();
                    if let Some(msg) = chat.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                    }
                }
                let _ = app.insert_exo_message(MessageRole::User, &text);
                chat.add_user_message(text.clone());
                exo_session.send_message(&text, chat.session_id.as_deref());
            }
        }
    }
}

pub(super) fn tick_refresh<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()) {
        state.refresh_tasks(tasks);
    }
    // Update global task→project mapping and drain stale permissions.
    let all_active = app.list_active().unwrap_or_default();
    let all_running_names: HashSet<TaskName> = all_active.iter().map(|t| t.name.clone()).collect();
    state.global_task_projects = all_active
        .iter()
        .map(|t| (t.name.clone(), t.project_id.clone()))
        .collect();
    state.global_task_work_dirs = all_active
        .iter()
        .filter_map(|t| t.work_dir.as_ref().map(|wd| (t.name.clone(), wd.clone())))
        .collect();
    for perm in state.permissions.drain_stale(&all_running_names) {
        notify_tg_resolved(tg_tx, perm.perm_id, "⚪ Expired (task ended)");
        let _ = write_response_to_stream(perm.stream, false, None);
    }
    let active = state.active_state_mut();
    active.task_list.window_numbers = app.window_numbers();
    // Update selected messages and live output for detail view
    if let Some(task) = active.task_list.selected_task() {
        let chat = ChatId::Task(task.id.clone());
        let is_running = task.status.is_running();
        let pane = task.tmux_pane.clone();
        if let Ok(messages) = app.messages(&chat) {
            active.task_list.selected_messages = messages;
        }
        if is_running {
            active.task_list.detail_live_output = pane
                .as_ref()
                .map(|p| p.as_str())
                .and_then(|p| app.capture_pane(p));
        } else {
            active.task_list.detail_live_output = None;
        }
    } else {
        active.task_list.selected_messages.clear();
        active.task_list.detail_live_output = None;
    }
}

pub(super) fn detect_vanished_perms(
    state: &ScreenState,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
) {
    if tg_perm_ids.is_empty() {
        return;
    }
    let still_pending = state.permissions.all_perm_ids();
    let vanished: Vec<u64> = tg_perm_ids
        .iter()
        .filter(|id| !still_pending.contains(id))
        .copied()
        .collect();
    for id in &vanished {
        notify_tg_resolved(tg_tx, *id, "✅ Resolved in pane");
        tg_perm_ids.remove(id);
    }
}
