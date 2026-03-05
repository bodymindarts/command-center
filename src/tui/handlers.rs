use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc;

use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::ClatApp;
use crate::permission::PermissionRequest;
use crate::primitives::{ChatId, MessageRole, ProjectId, TaskName};
use crate::runtime::Runtime;

use super::ProjectContext;
use super::permissions::ActivePermission;
use super::screen_state::{Focus, ScreenState};
use super::telegram;
use crate::assistant::{AssistantEvent, AssistantSession, ProjectEvent, project_system_prompt};

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
        if text.contains('\n') || text.contains('\r') {
            state.input.set_paste(text);
        } else {
            for c in text.chars() {
                state.input.insert(c);
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
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
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
        return handle_cycle_permissions(state, app, project_contexts, project_tx);
    }

    // Permission keys — global so they work regardless of focus (ChatInput, ChatHistory, TaskList)
    // while a task detail is showing. Guarded by show_detail + permissions.peek().
    // Ctrl+Y one-time allow
    if ctrl
        && key.code == KeyCode::Char('y')
        && state.show_detail
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
        && state.show_detail
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
        && state.show_detail
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
        && state.show_detail
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
        handle_goto_project(state, app, project_contexts, project_tx);
        return true;
    }

    false
}

fn handle_cycle_permissions<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
) -> bool {
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
                state.show_detail = false;
            }
        } else if let Some(pos) = state.tasks.iter().position(|t| t.name == name) {
            // Task is in the current project view
            state.save_current_input();
            state.list_state.select(Some(pos));
            state.show_detail = true;
            state.detail_scroll = 0;
            state.focus = Focus::ChatInput;
            state.chat_scroll = 0;
            state.restore_input();
        } else {
            // Task is in a different project — switch to it
            let target_pid = state.global_task_projects.get(&name).cloned().flatten();
            if let Some(pid) = target_pid {
                if let Ok(projects) = app.list_projects() {
                    state.projects = projects;
                }
                let proj_name = state
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
                super::ensure_project_context(project_contexts, state, app, &pid, project_tx);
            } else if let Ok(tasks) = app.list_visible(None) {
                state.switch_to_project(None, tasks, Some(&name));
            }
        }
    } else if state.list_state.selected().is_some() {
        state.save_current_input();
        state.show_detail = true;
        state.detail_scroll = 0;
        state.focus = Focus::ChatInput;
        state.chat_scroll = 0;
        state.restore_input();
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

fn handle_goto_project<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
) {
    // If in a project's task detail, go back to PM chat
    if state.show_detail && state.active_project_id.is_some() {
        state.save_current_input();
        state.show_detail = false;
        state.focus = Focus::ChatInput;
        state.chat_scroll = 0;
        state.restore_input();
    // If in ExO view, restore last active project (or first project)
    } else if state.active_project_id.is_none() {
        if let Ok(projects) = app.list_projects() {
            state.projects = projects;
        }
        let target = state
            .last_project
            .take()
            .map(|s| (s.name, s.id, s.show_detail, s.selected_task_name))
            .or_else(|| {
                state
                    .projects
                    .first()
                    .map(|p| (p.name.clone(), p.id.clone(), false, None))
            });
        if let Some((name, id, saved_show_detail, saved_task_name)) = target {
            let focus = if saved_show_detail {
                saved_task_name.as_ref()
            } else {
                None
            };
            if let Ok(tasks) = app.list_visible(Some(&id)) {
                state.switch_to_project(Some((name, id.clone())), tasks, focus);
            }
            super::ensure_project_context(project_contexts, state, app, &id, project_tx);
        }
    // If in a project PM view, cycle to next project
    } else if state.active_project_id.is_some() && !state.show_detail {
        if let Ok(projects) = app.list_projects() {
            state.projects = projects;
        }
        let cur_idx = state
            .active_project_id
            .as_ref()
            .and_then(|pid| state.projects.iter().position(|p| p.id == *pid));
        if let Some(ci) = cur_idx {
            let next_idx = (ci + 1) % state.projects.len();
            let next = &state.projects[next_idx];
            let next_id = next.id.clone();
            let next_name = next.name.clone();
            if let Ok(tasks) = app.list_visible(Some(&next_id)) {
                state.switch_to_project(Some((next_name, next_id.clone())), tasks, None);
            }
            super::ensure_project_context(project_contexts, state, app, &next_id, project_tx);
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
    project_tx: &mpsc::Sender<ProjectEvent>,
) {
    match state.current_focus() {
        Focus::TaskList => handle_task_list_key(state, key, app),
        Focus::TaskSearch => handle_task_search_key(state, key),
        Focus::ProjectList => {
            handle_project_list_key(state, key, app, project_contexts, project_tx)
        }
        Focus::ChatInput if state.show_detail => handle_task_chat_input_key(state, key, app),
        Focus::ChatInput => {
            handle_chat_input_key(state, key, app, exo_session, project_contexts, project_tx)
        }
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
            state.show_detail = false;
            state.focus = Focus::ChatInput;
            state.restore_input();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            state.next();
            state.show_detail = true;
            state.detail_scroll = 0;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            state.previous();
            state.show_detail = true;
            state.detail_scroll = 0;
        }
        KeyCode::PageDown => {
            state.detail_scroll = state.detail_scroll.saturating_add(10);
        }
        KeyCode::PageUp => {
            state.detail_scroll = state.detail_scroll.saturating_sub(10);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.detail_scroll = state.detail_scroll.saturating_add(10);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.detail_scroll = state.detail_scroll.saturating_sub(10);
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            goto_task_window(state, app);
        }
        KeyCode::Enter => {
            if state.selected_task().is_some() {
                state.show_detail = true;
                state.detail_scroll = 0;
                state.focus = Focus::ChatInput;
                state.chat_scroll = 0;
                state.restore_input();
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
            state.search_input.take();
            state.update_search_filter();
            state.focus = Focus::TaskSearch;
        }
        KeyCode::Tab => {
            state.focus = Focus::ChatInput;
            state.restore_input();
        }
        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.focus = Focus::ChatInput;
            state.restore_input();
        }
        KeyCode::Char('p') => {
            if let Ok(projects) = app.list_projects() {
                state.projects = projects;
                if !state.projects.is_empty() {
                    state.project_list_state.select(Some(0));
                }
            }
            state.show_projects = true;
            state.focus = Focus::ProjectList;
        }
        _ => {}
    }
}

fn handle_task_search_key(state: &mut ScreenState, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let searching_projects = state.show_projects;
    let do_filter = |state: &mut ScreenState| {
        if state.show_projects {
            state.update_project_search_filter();
        } else {
            state.update_search_filter();
        }
    };
    match key.code {
        KeyCode::Esc => {
            state.search_input.take();
            if searching_projects {
                state.filtered_project_indices.clear();
                if !state.projects.is_empty() {
                    let sel = state
                        .project_list_state
                        .selected()
                        .unwrap_or(0)
                        .min(state.projects.len() - 1);
                    state.project_list_state.select(Some(sel));
                }
                state.focus = Focus::ProjectList;
            } else {
                state.filtered_indices.clear();
                if !state.tasks.is_empty() {
                    let sel = state
                        .list_state
                        .selected()
                        .unwrap_or(0)
                        .min(state.tasks.len() - 1);
                    state.list_state.select(Some(sel));
                }
                state.focus = Focus::TaskList;
            }
        }
        KeyCode::Enter => {
            if searching_projects {
                if let Some(real_idx) = state.selected_filtered_project_index() {
                    state.project_list_state.select(Some(real_idx));
                }
                state.search_input.take();
                state.filtered_project_indices.clear();
                state.focus = Focus::ProjectList;
            } else {
                if let Some(real_idx) = state.selected_filtered_task_index() {
                    state.list_state.select(Some(real_idx));
                    state.show_detail = true;
                    state.detail_scroll = 0;
                    state.focus = Focus::ChatInput;
                    state.chat_scroll = 0;
                    state.restore_input();
                } else {
                    state.focus = Focus::TaskList;
                }
                state.search_input.take();
                state.filtered_indices.clear();
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if searching_projects {
                state.search_next_project();
            } else {
                state.search_next();
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if searching_projects {
                state.search_prev_project();
            } else {
                state.search_prev();
            }
        }
        KeyCode::Char('n') if ctrl => {
            if searching_projects {
                state.search_next_project();
            } else {
                state.search_next();
            }
        }
        KeyCode::Char('p') if ctrl => {
            if searching_projects {
                state.search_prev_project();
            } else {
                state.search_prev();
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

fn handle_project_list_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
) {
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
                super::ensure_project_context(
                    project_contexts,
                    state,
                    app,
                    &project_id,
                    project_tx,
                );
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
            state.save_current_input();
            state.show_detail = false;
            state.chat_scroll = 0;
            state.restore_input();
        }
        KeyCode::Tab => {
            state.save_current_input();
            state.chat_scroll = 0;
            let current = state.list_state.selected().unwrap_or(0);
            if current + 1 < state.tasks.len() {
                state.list_state.select(Some(current + 1));
                state.detail_scroll = 0;
            } else {
                state.show_detail = false;
            }
            state.restore_input();
        }
        KeyCode::BackTab => {
            state.save_current_input();
            state.chat_scroll = 0;
            let current = state.list_state.selected().unwrap_or(0);
            if current > 0 {
                state.list_state.select(Some(current - 1));
                state.detail_scroll = 0;
            } else {
                state.show_detail = false;
            }
            state.restore_input();
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
            state.save_current_input();
            state.focus = Focus::TaskList;
        }
        KeyCode::Char('g') if ctrl => {
            goto_task_window(state, app);
        }
        KeyCode::Enter => {
            if !state.input.is_empty() {
                let msg = state.input.take();
                if let Some(task) = state.selected_task() {
                    let task_id = task.id.as_str().to_string();
                    let pane = task.tmux_pane.clone();
                    match app.send(&task_id, &msg) {
                        Ok(_) => {
                            if let Some(pane) = pane {
                                state.idle_panes.remove(&pane);
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
            handle_input_editing(&mut state.input, &key);
        }
    }
}

fn handle_chat_input_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            if state.exo_chat.streaming {
                state.exo_chat.finish_streaming();
            }
            if let Some(ref pid) = state.active_project_id
                && let Some(chat) = state.project_chats.get_mut(pid)
                && chat.streaming
            {
                chat.finish_streaming();
            }
            state.chat_scroll = 0;
        }
        KeyCode::Tab => {
            state.save_current_input();
            state.chat_scroll = 0;
            if !state.tasks.is_empty() {
                state.list_state.select(Some(0));
                state.show_detail = true;
                state.detail_scroll = 0;
            }
            state.restore_input();
        }
        KeyCode::BackTab => {
            state.save_current_input();
            state.chat_scroll = 0;
            if !state.tasks.is_empty() {
                state.list_state.select(Some(state.tasks.len() - 1));
                state.show_detail = true;
                state.detail_scroll = 0;
            }
            state.restore_input();
        }
        KeyCode::Char('k') if ctrl => {
            state.focus = Focus::ChatHistory;
        }
        KeyCode::Char('x') if ctrl && state.active_project.is_some() => {
            state.focus = Focus::ConfirmCloseProject;
        }
        KeyCode::Char('l') if ctrl => {
            state.save_current_input();
            state.focus = Focus::TaskList;
            if state.list_state.selected().is_some() {
                state.show_detail = true;
                state.detail_scroll = 0;
            }
        }
        KeyCode::Enter => {
            handle_chat_enter(state, app, exo_session, project_contexts, project_tx);
        }
        _ => {
            handle_input_editing(&mut state.input, &key);
        }
    }
}

fn handle_chat_enter<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    project_tx: &mpsc::Sender<ProjectEvent>,
) {
    state.chat_scroll = 0;
    if state.input.is_empty() {
        return;
    }
    if let Some(pid) = state.active_project_id.clone() {
        if !project_contexts.contains_key(&pid) {
            project_contexts.insert(pid.clone(), ProjectContext::new(&pid, project_tx));
        }
        let chat = state
            .project_chats
            .entry(pid.clone())
            .or_insert_with(super::chat::AssistantChat::new);
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
        let msg = state.input.take();
        let _ = app.insert_project_message(&pid, MessageRole::User, &msg);
        chat.add_user_message(msg.clone());
        let ctx = project_contexts.get_mut(&pid).unwrap();
        if ctx.session.is_none() {
            let sid = app
                .read_project_session_id(&pid)
                .or_else(|| chat.session_id.clone());
            let proj_name = state
                .active_project
                .as_ref()
                .map(|p| p.as_str())
                .unwrap_or("unknown");
            let prompt = project_system_prompt(proj_name);
            ctx.session = Some(AssistantSession::new(
                sid.as_deref(),
                Arc::clone(&ctx.cancel),
                ctx.bridge_tx.clone(),
                &prompt,
            ));
            if let Some(ref s) = sid {
                chat.session_id = Some(s.clone());
            }
        }
        if let Some(sess) = &mut ctx.session {
            sess.send_message(&msg, chat.session_id.as_deref());
            state.chat_scroll = 0;
        }
    } else {
        if state.exo_chat.streaming {
            state.exo_chat.finish_streaming();
            if let Some(msg) = state.exo_chat.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
            }
        }
        let msg = state.input.take();
        let _ = app.insert_exo_message(MessageRole::User, &msg);
        state.exo_chat.add_user_message(msg.clone());
        exo_session.send_message(&msg, state.exo_chat.session_id.as_deref());
        state.chat_scroll = 0;
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
            state.show_detail = false;
            state.focus = Focus::ChatInput;
            state.restore_input();
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus = if state.show_detail {
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
                state.projects = projects;
                if state.projects.is_empty() {
                    state.project_list_state.select(None);
                } else {
                    let sel = state
                        .project_list_state
                        .selected()
                        .unwrap_or(0)
                        .min(state.projects.len().saturating_sub(1));
                    state.project_list_state.select(Some(sel));
                }
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
        match ev {
            AssistantEvent::TextDelta(text) => {
                if state.exo_chat.streaming {
                    state.exo_chat.append_text(&text);
                    state.chat_scroll = 0;
                    if let Some(tx) = tg_tx {
                        let _ = tx.send(telegram::TgOutbound::ExoTextDelta { text: text.clone() });
                    }
                }
            }
            AssistantEvent::ToolStart(name) => {
                if state.exo_chat.streaming {
                    state.exo_chat.add_tool_activity(name);
                }
            }
            AssistantEvent::SessionId(id) => {
                app.write_exo_session_id(&id);
                state.exo_chat.session_id = Some(id.clone());
                exo_session.set_session_id(id);
            }
            AssistantEvent::TurnDone => {
                state.exo_chat.finish_streaming();
                if let Some(msg) = state.exo_chat.messages.last()
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
                state.exo_chat.had_process_error = false;
                exo_session.mark_exited();
                if state.exo_chat.streaming {
                    state
                        .exo_chat
                        .add_error("Claude process exited unexpectedly");
                }
            }
            AssistantEvent::Error(e) => {
                state.exo_chat.had_process_error = true;
                state.exo_chat.add_error(&e);
                if let Some(msg) = state.exo_chat.messages.last()
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
        let Some(chat) = state.project_chats.get_mut(&project_id) else {
            continue;
        };
        match ev.inner {
            AssistantEvent::TextDelta(text) => {
                if chat.streaming {
                    chat.append_text(&text);
                    if is_active_project {
                        state.chat_scroll = 0;
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
                if let Some(ctx) = project_contexts.get_mut(&project_id)
                    && let Some(sess) = &mut ctx.session
                {
                    sess.set_session_id(id);
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
                if let Some(ctx) = project_contexts.get_mut(&project_id)
                    && let Some(sess) = &mut ctx.session
                {
                    sess.mark_exited();
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
            if let Some(pane_id) = state
                .tasks
                .iter()
                .find(|t| t.name == *name)
                .and_then(|t| t.tmux_pane.as_ref())
            {
                state.idle_panes.remove(pane_id);
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
            && let Some(pane_id) = state
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            state.idle_panes.insert(pane_id.clone());
        }
    }
}

/// Drain active notifications from Notification hooks — mark pane as not idle.
pub(super) fn drain_active(state: &mut ScreenState, active_rx: &mpsc::Receiver<String>) {
    while let Ok(cwd) = active_rx.try_recv() {
        let cwd_path =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        if let Some(task_name) = find_task_name_by_cwd(&state.global_task_work_dirs, &cwd_path)
            && let Some(pane_id) = state
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            state.idle_panes.remove(pane_id);
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
        if let Some(pane_id) = state
            .tasks
            .iter()
            .find(|t| t.name == task_name)
            .and_then(|t| t.tmux_pane.as_ref())
        {
            state.idle_panes.remove(pane_id);
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
                state.chat_scroll = 0;
                if state.exo_chat.streaming {
                    state.exo_chat.finish_streaming();
                    if let Some(msg) = state.exo_chat.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ = app.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                    }
                }
                let _ = app.insert_exo_message(MessageRole::User, &text);
                state.exo_chat.add_user_message(text.clone());
                exo_session.send_message(&text, state.exo_chat.session_id.as_deref());
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
    state.window_numbers = app.window_numbers();
    // Update selected messages and live output for detail view
    if let Some(task) = state.selected_task() {
        let chat = ChatId::Task(task.id.clone());
        let is_running = task.status.is_running();
        let pane = task.tmux_pane.clone();
        if let Ok(messages) = app.messages(&chat) {
            state.selected_messages = messages;
        }
        if is_running {
            state.detail_live_output = pane
                .as_ref()
                .map(|p| p.as_str())
                .and_then(|p| app.capture_pane(p));
        } else {
            state.detail_live_output = None;
        }
    } else {
        state.selected_messages.clear();
        state.detail_live_output = None;
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
