use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;

use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use tokio::sync::mpsc;

use crate::app::ClatApp;
use crate::assistant::{AssistantEvent, AssistantSession, SessionKey};
use crate::permission::{HookEvent, PermissionRequest};
use crate::primitives::{ChatId, MessageRole, ProjectId, TaskName};
use crate::runtime::Runtime;

use super::ProjectContext;
use super::permissions::ActivePermission;
use super::state::{Focus, ScreenState};
use super::telegram;

const EXO_PERM_KEY: &str = "exo";

// ── Helper functions ────────────────────────────────────────────────

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
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
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
    state.accept_paste(text);
}

// ── Global key handler ──────────────────────────────────────────────

/// Handle global key shortcuts. Returns true if the key was consumed.
pub(super) async fn handle_global_keys<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
) -> bool {
    let kb = &state.keybindings.global;

    if kb.quit.matches(&key) {
        state.request_quit();
        return true;
    }

    if kb.cycle_permissions.matches(&key) {
        return handle_cycle_permissions(state, app).await;
    }

    // Permission keys — global so they work regardless of focus (ChatInput, ChatHistory, TaskList)
    // while a task detail is showing. Guarded by show_detail + permissions.peek().
    let show_detail = state.active_state().task_list.is_detail_visible();

    if kb.perm_approve.matches(&key)
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(state, true) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "✅ Approved locally");
        }
        return true;
    }

    if kb.perm_trust.matches(&key)
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, suggestions, perm_id)) = resolve_permission(state, true) {
            let _ = write_response_to_stream(stream, allow, Some(&suggestions));
            notify_tg_resolved(tg_tx, perm_id, "✅ Trusted locally");
        }
        return true;
    }

    if kb.perm_deny.matches(&key)
        && show_detail
        && state.permissions.peek(&state.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(state, false) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "❌ Denied locally");
        }
        return true;
    }

    // Number keys 1-4 answer an AskUser prompt (hardcoded — dynamic by nature)
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

    if kb.focus_exo.matches(&key) {
        if let Ok(tasks) = app.list_visible(None).await {
            state.switch_to_project(None, tasks, None);
        }
        return true;
    }

    if kb.cycle_projects.matches(&key) {
        handle_goto_project(state, app).await;
        return true;
    }

    false
}

async fn handle_cycle_permissions<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) -> bool {
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
                if let Ok(tasks) = app.list_visible(None).await {
                    state.switch_to_project(None, tasks, None);
                }
            } else {
                state.hide_active_detail();
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
            let target_pid = state.global_task_project(&name).cloned().flatten();
            if let Some(pid) = target_pid {
                if let Ok(projects) = app.list_projects().await {
                    state.project_list.set_projects(projects);
                }
                let proj_name = state
                    .project_list
                    .projects()
                    .iter()
                    .find(|p| p.id == pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| {
                        crate::primitives::ProjectName::from(pid.as_str().to_string())
                    });
                if let Ok(tasks) = app.list_visible(Some(&pid)).await {
                    state.switch_to_project(Some((proj_name, pid.clone())), tasks, Some(&name));
                }
            } else if let Ok(tasks) = app.list_visible(None).await {
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
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
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

async fn handle_goto_project<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    // If in a project's task detail, go back to PM chat
    if state.active_state().task_list.is_detail_visible() && state.active_project_id.is_some() {
        state.close_task_detail();
    // If in ExO view, restore last active project (or first project)
    } else if state.active_project_id.is_none() {
        if let Ok(projects) = app.list_projects().await {
            state.project_list.set_projects(projects);
        }
        // Try last_project_id first, then fall back to first project
        let target = state
            .last_project_id()
            .and_then(|pid| {
                state
                    .project_list
                    .projects()
                    .iter()
                    .find(|p| p.id == *pid)
                    .map(|p| (p.name.clone(), p.id.clone()))
            })
            .or_else(|| {
                state
                    .project_list
                    .projects()
                    .first()
                    .map(|p| (p.name.clone(), p.id.clone()))
            });
        if let Some((name, id)) = target
            && let Ok(tasks) = app.list_visible(Some(&id)).await
        {
            state.switch_to_project(Some((name, id.clone())), tasks, None);
        }
    // If in a project PM view, cycle to next project
    } else if state.active_project_id.is_some()
        && !state.active_state().task_list.is_detail_visible()
    {
        if let Ok(projects) = app.list_projects().await {
            state.project_list.set_projects(projects);
        }
        let cur_idx = state.active_project_id.as_ref().and_then(|pid| {
            state
                .project_list
                .projects()
                .iter()
                .position(|p| p.id == *pid)
        });
        if let Some(ci) = cur_idx {
            let next_idx = (ci + 1) % state.project_list.projects().len();
            let next = &state.project_list.projects()[next_idx];
            let next_id = next.id.clone();
            let next_name = next.name.clone();
            if let Ok(tasks) = app.list_visible(Some(&next_id)).await {
                state.switch_to_project(Some((next_name, next_id.clone())), tasks, None);
            }
        }
    }
}

// ── Per-focus key handlers ──────────────────────────────────────────

pub(super) async fn handle_focus_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    match state.current_focus() {
        Focus::TaskList => handle_task_list_key(state, key, app).await,
        Focus::ListSearch => handle_task_search_key(state, key),
        Focus::ProjectList => handle_project_list_key(state, key, app).await,
        Focus::ChatInput if state.active_state().task_list.is_detail_visible() => {
            handle_task_chat_input_key(state, key, app).await
        }
        Focus::ChatInput => {
            handle_chat_input_key(state, key, app, exo_session, project_contexts).await
        }
        Focus::ChatHistory => handle_chat_history_key(state, key),
        Focus::ConfirmDelete(_) => handle_confirm_delete_key(state, key, app).await,
        Focus::ConfirmCloseTask(_) => handle_confirm_close_task_key(state, key, app).await,
        Focus::ConfirmDeleteProject(_) => handle_confirm_delete_project_key(state, key, app).await,
        Focus::ConfirmCloseProject => {
            handle_confirm_close_project_key(state, key, app, project_contexts).await
        }
    }
}

async fn handle_task_list_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let kb = &state.keybindings.task_list;
    if kb.close_detail.matches(&key) {
        state.close_task_detail();
    } else if kb.navigate_down.matches(&key) {
        state.next_task_with_detail();
    } else if kb.navigate_up.matches(&key) {
        state.previous_task_with_detail();
    } else if kb.scroll_down.matches(&key) {
        state.scroll_down_tasks();
    } else if kb.scroll_up.matches(&key) {
        state.scroll_up_tasks();
    } else if kb.goto_window.matches(&key) {
        goto_task_window(state, app).await;
    } else if kb.open_detail.matches(&key) {
        state.open_selected_task();
    } else if kb.close_task.matches(&key) {
        state.confirm_close_selected_task();
    } else if kb.reopen_task.matches(&key) {
        reopen_task(state, app).await;
    } else if kb.delete_task.matches(&key) {
        state.confirm_delete_selected_task();
    } else if kb.search.matches(&key) {
        state.enter_search_mode();
    } else if kb.focus_chat.matches(&key) {
        state.focus_left();
    } else if kb.show_projects.matches(&key) {
        state.show_project_list(app.list_projects().await.unwrap_or_default());
    }
}

fn handle_task_search_key(state: &mut ScreenState, key: KeyEvent) {
    let kb = &state.keybindings.task_search;
    if kb.cancel.matches(&key) {
        state.exit_search();
    } else if kb.confirm.matches(&key) {
        state.confirm_search_selection();
    } else if kb.next.matches(&key) {
        state.search_next();
    } else if kb.prev.matches(&key) {
        state.search_prev();
    } else if handle_input_editing(&mut state.search_input, &key) {
        state.update_search_filter();
    }
}

async fn handle_project_list_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let kb = &state.keybindings.project_list;
    if kb.navigate_down.matches(&key) {
        state.next_project();
    } else if kb.navigate_up.matches(&key) {
        state.previous_project();
    } else if kb.search.matches(&key) {
        state.enter_search_mode();
    } else if kb.select.matches(&key) {
        if let Some(project) = state.selected_project() {
            let project_id = project.id.clone();
            let project_name = project.name.clone();
            if let Ok(tasks) = app.list_visible(Some(&project_id)).await {
                state.switch_to_project(Some((project_name, project_id.clone())), tasks, None);
            }
        }
    } else if kb.delete.matches(&key) {
        if let Some(project) = state.selected_project() {
            let name = project.name.clone();
            state.set_focus(Focus::ConfirmDeleteProject(name));
        }
    } else if kb.back.matches(&key) {
        state.focus_on_tasks();
    }
}

async fn handle_task_chat_input_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let kb = &state.keybindings.task_chat;
    if kb.close_detail.matches(&key) {
        state.close_task_detail();
    } else if kb.next_task.matches(&key) {
        state.navigate_to_adjacent_task(true);
    } else if kb.prev_task.matches(&key) {
        state.navigate_to_adjacent_task(false);
    } else if kb.focus_history.matches(&key) {
        state.set_focus(Focus::ChatHistory);
    } else if kb.close_task.matches(&key) {
        if let Some(task) = state.selected_task()
            && task.status.is_running()
        {
            let id = task.id.clone();
            state.set_focus(Focus::ConfirmCloseTask(id));
        }
    } else if kb.focus_tasks.matches(&key) {
        state.focus_on_tasks();
    } else if kb.goto_window.matches(&key) {
        goto_task_window(state, app).await;
    } else if kb.send.matches(&key) {
        let active = state.active_state_mut();
        if !active.input.is_empty() {
            let msg = active.input.take();
            if let Some(task) = active.task_list.selected_task() {
                let task_id = task.id.as_str().to_string();
                let pane = task.tmux_pane.clone();
                match app.send(&task_id, &msg).await {
                    Ok(_) => {
                        if let Some(pane) = pane {
                            active.task_list.mark_pane_active(pane);
                        }
                    }
                    Err(e) => {
                        state.set_status_error(format!("send: {e}"));
                    }
                }
            }
        }
    } else {
        handle_input_editing(&mut state.active_state_mut().input, &key);
    }
}

async fn handle_chat_input_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    let kb = &state.keybindings.chat_input;
    if kb.cancel_streaming.matches(&key) {
        state.cancel_streaming();
    } else if kb.open_first_task.matches(&key) {
        state.open_first_task_detail();
    } else if kb.open_last_task.matches(&key) {
        state.open_last_task_detail();
    } else if kb.focus_up.matches(&key) {
        state.move_focus_up();
    } else if kb.close_project.matches(&key) && state.is_project_selected() {
        state.confirm_close_project();
    } else if kb.focus_task_list.matches(&key) {
        state.focus_task_list_with_detail();
    } else if kb.send.matches(&key) {
        handle_chat_enter(state, app, exo_session, project_contexts).await;
    } else {
        handle_input_editing(&mut state.active_state_mut().input, &key);
    }
}

async fn handle_chat_enter<R: Runtime>(
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
            state.set_status_error("PM session not initialized".to_string());
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
                let _ = app
                    .insert_session_message(Some(&pid), MessageRole::Assistant, &msg.text_content())
                    .await;
            }
        }
        let msg = active.input.take();
        let _ = app
            .insert_session_message(Some(&pid), MessageRole::User, &msg)
            .await;
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
                let _ = app
                    .insert_session_message(None, MessageRole::Assistant, &msg.text_content())
                    .await;
            }
        }
        let msg = active.input.take();
        let _ = app
            .insert_session_message(None, MessageRole::User, &msg)
            .await;
        chat.add_user_message(msg.clone());
        exo_session.send_message(&msg, chat.session_id.as_deref());
        active.chat_view.reset_scroll();
    }
}

fn handle_chat_history_key(state: &mut ScreenState, key: KeyEvent) {
    let kb = &state.keybindings.chat_history;
    if kb.navigate_down.matches(&key) {
        state.navigate_focus_down();
    } else if kb.scroll_up.matches(&key) {
        state.scroll_chat_up();
    } else if kb.scroll_down.matches(&key) {
        state.scroll_chat_down();
    } else if kb.navigate_right.matches(&key) {
        state.navigate_focus_right();
    }
}

async fn handle_confirm_delete_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
) {
    let task_id = match state.current_focus() {
        Focus::ConfirmDelete(id) => id.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = app.delete(task_id.as_str()).await;
            if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()).await {
                state.refresh_tasks(tasks);
            }
            state.focus_on_tasks();
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.focus_on_tasks();
        }
        _ => {}
    }
}

async fn handle_confirm_close_task_key<R: Runtime>(
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
            let _ = app.close(task_id.as_str()).await;
            if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()).await {
                state.refresh_tasks(tasks);
            }
            state.close_task_detail();
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            let f = if state.active_state().task_list.is_detail_visible() {
                Focus::ChatInput
            } else {
                Focus::TaskList
            };
            state.set_focus(f);
        }
        _ => {}
    }
}

async fn handle_confirm_delete_project_key<R: Runtime>(
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
            let _ = app.delete_project(project_name.as_str()).await;
            if let Ok(projects) = app.list_projects().await {
                state.refresh_projects(projects);
            }
            state.set_focus(Focus::ProjectList);
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.set_focus(Focus::ProjectList);
        }
        _ => {}
    }
}

async fn handle_confirm_close_project_key<R: Runtime>(
    state: &mut ScreenState,
    key: KeyEvent,
    app: &ClatApp<R>,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
) {
    match key.code {
        KeyCode::Char('y') => {
            let closed_pid = state.active_project_id.clone();
            if let Ok(tasks) = app.list_visible(None).await {
                state.switch_to_project(None, tasks, None);
                state.focus_on_tasks();
            }
            if let Some(pid) = closed_pid {
                super::cancel_project_context(project_contexts, state, &pid);
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            state.set_focus(Focus::ChatInput);
        }
        _ => {}
    }
}

/// Shared: go to the selected task's tmux window (or reopen if closed).
async fn goto_task_window<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    if let Some(task) = state.selected_task() {
        if task.status.is_running() {
            if let Some(window_id) = &task.tmux_window {
                app.goto_window(window_id);
            }
        } else {
            let id = task.id.as_str().to_string();
            match app.reopen(&id).await {
                Ok(window_id) => {
                    if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()).await {
                        state.refresh_tasks(tasks);
                    }
                    app.goto_window(&window_id);
                }
                Err(e) => {
                    state.set_status_error(format!("reopen: {e}"));
                }
            }
        }
    }
}

/// Shared: reopen a closed task.
async fn reopen_task<R: Runtime>(state: &mut ScreenState, app: &ClatApp<R>) {
    if let Some(task) = state.selected_task()
        && !task.status.is_running()
    {
        let id = task.id.as_str().to_string();
        match app.reopen(&id).await {
            Ok(_) => {
                if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()).await {
                    state.refresh_tasks(tasks);
                }
            }
            Err(e) => {
                state.set_status_error(format!("reopen: {e}"));
            }
        }
    }
}

// ── Channel draining ────────────────────────────────────────────────

/// Dispatch a single assistant event (from the shared channel) to the
/// appropriate session handler based on the session key.
pub(super) async fn dispatch_assistant_event<R: Runtime>(
    key: &SessionKey,
    event: AssistantEvent,
    state: &mut ScreenState,
    exo_session: &mut AssistantSession,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
) {
    match key {
        SessionKey::Exo => {
            let is_exo_viewing = state.active_project_id.is_none();
            handle_session_event(
                app,
                &mut state.exo,
                is_exo_viewing,
                exo_session,
                None,
                tg_tx,
                event,
            )
            .await;
        }
        SessionKey::Project(pid) => {
            let is_viewing = state.active_project_id.as_ref() == Some(pid);
            if let Some(ctx) = project_contexts.get_mut(pid)
                && let Some(ps) = state.projects.get_mut(pid)
            {
                handle_session_event(
                    app,
                    ps,
                    is_viewing,
                    &mut ctx.session,
                    Some(pid),
                    tg_tx,
                    event,
                )
                .await;
            }
        }
    }
}

async fn handle_session_event<R: Runtime>(
    app: &ClatApp<R>,
    ps: &mut super::state::ProjectState,
    is_viewing: bool,
    session: &mut AssistantSession,
    project_id: Option<&ProjectId>,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
    ev: AssistantEvent,
) {
    match ev {
        AssistantEvent::TextDelta(text) => {
            if ps.chat_view.assistant.streaming {
                ps.chat_view.assistant.append_text(&text);
                if is_viewing {
                    ps.chat_view.reset_scroll();
                }
                if project_id.is_none()
                    && let Some(tx) = tg_tx
                {
                    let _ = tx.send(telegram::TgOutbound::ExoTextDelta { text: text.clone() });
                }
            }
        }
        AssistantEvent::ToolStart(name) => {
            if ps.chat_view.assistant.streaming {
                ps.chat_view.assistant.add_tool_activity(name);
            }
        }
        AssistantEvent::SessionId(id) => {
            match project_id {
                None => app.write_exo_session_id(&id),
                Some(pid) => app.write_project_session_id(pid, &id),
            }
            ps.chat_view.assistant.session_id = Some(id.clone());
            session.set_session_id(id);
        }
        AssistantEvent::TurnDone => {
            ps.chat_view.assistant.finish_streaming();
            if let Some(msg) = ps.chat_view.assistant.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = app
                    .insert_session_message(project_id, MessageRole::Assistant, &msg.text_content())
                    .await;
            }
            if project_id.is_none()
                && let Some(tx) = tg_tx
            {
                let _ = tx.send(telegram::TgOutbound::ExoTurnDone);
            }
        }
        AssistantEvent::ProcessExited => {
            ps.chat_view.assistant.had_process_error = false;
            session.mark_exited();
            if ps.chat_view.assistant.streaming {
                let label = if project_id.is_none() { "Claude" } else { "PM" };
                ps.chat_view
                    .assistant
                    .add_error(&format!("{label} process exited unexpectedly"));
            }
        }
        AssistantEvent::Error(e) => {
            ps.chat_view.assistant.had_process_error = true;
            ps.chat_view.assistant.add_error(&e);
            if let Some(msg) = ps.chat_view.assistant.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = app
                    .insert_session_message(project_id, MessageRole::Assistant, &msg.text_content())
                    .await;
            }
        }
    }
}

/// Mutable state for tracking Telegram permission IDs.
pub(super) struct TgPermState {
    pub ids: HashSet<u64>,
    pub counter: u64,
}

/// Handle a single hook event from the permission socket.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_hook_event<R: Runtime>(
    state: &mut ScreenState,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>,
    event: HookEvent,
    stream: UnixStream,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
    tg_perm: &mut TgPermState,
) {
    match event {
        HookEvent::Resolved { cwd } => {
            handle_hook_resolved(state, &cwd, tg_tx, &mut tg_perm.ids);
        }
        HookEvent::Idle { cwd } => {
            handle_hook_idle(state, &cwd, tg_tx);
        }
        HookEvent::Active { cwd } => {
            handle_hook_active(state, &cwd, tg_tx);
        }
        HookEvent::Permission(request) => {
            handle_hook_permission(
                state,
                stream,
                request,
                tg_tx,
                &mut tg_perm.ids,
                &mut tg_perm.counter,
            );
        }
        // New hook events — received and dropped for now.
        // No response needed; stream is dropped which closes the connection.
        HookEvent::PreToolUse { cwd, .. }
        | HookEvent::UserPromptSubmit { cwd, .. }
        | HookEvent::SubagentStop { cwd, .. } => {
            handle_hook_active(state, &cwd, tg_tx);
        }
        HookEvent::Stop { cwd, .. } => {
            handle_hook_idle(state, &cwd, tg_tx);
            drop(stream);
        }
        HookEvent::PmMessage { project, message } => {
            handle_hook_pm_message(state, project_contexts, app, stream, &project, &message).await;
        }
        HookEvent::Unknown(_) => {}
    }
}

fn handle_hook_resolved(
    state: &mut ScreenState,
    cwd: &str,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
) {
    if let Some(perm) = state.resolve_permission(cwd) {
        let _ = write_response_to_stream(perm.stream, false, None);
        if tg_perm_ids.remove(&perm.perm_id) {
            notify_tg_resolved(tg_tx, perm.perm_id, "✅ Resolved in pane");
        }
    }
}

fn handle_hook_idle(
    state: &mut ScreenState,
    cwd: &str,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
) {
    if let Some(task_name) = state.mark_task_idle(cwd)
        && let Some(tx) = tg_tx
    {
        let _ = tx.send(telegram::TgOutbound::Notify {
            text: format!("💤 Task idle: {task_name}"),
        });
    }
}

fn handle_hook_active(
    state: &mut ScreenState,
    cwd: &str,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
) {
    if let Some(task_name) = state.mark_task_active(cwd)
        && let Some(tx) = tg_tx
    {
        let _ = tx.send(telegram::TgOutbound::Notify {
            text: format!("⚡ Task active: {task_name}"),
        });
    }
}

async fn handle_hook_pm_message<R: Runtime>(
    state: &mut ScreenState,
    project_contexts: &mut HashMap<ProjectId, ProjectContext>,
    app: &ClatApp<R>,
    stream: UnixStream,
    project_name: &str,
    message: &str,
) {
    use std::io::Write;

    let project = match app.resolve_project(project_name).await {
        Ok(p) => p,
        Err(_) => {
            let resp = serde_json::json!({"error": format!("unknown project '{project_name}'")});
            let _ = write!(&stream, "{resp}");
            return;
        }
    };

    let pid = project.id;
    let Some(ctx) = project_contexts.get_mut(&pid) else {
        let resp = serde_json::json!({"error": format!("PM session not initialized for '{project_name}'")});
        let _ = write!(&stream, "{resp}");
        return;
    };

    let Some(ps) = state.projects.get_mut(&pid) else {
        let resp =
            serde_json::json!({"error": format!("project state not found for '{project_name}'")});
        let _ = write!(&stream, "{resp}");
        return;
    };

    let chat = &mut ps.chat_view.assistant;
    if chat.streaming {
        chat.finish_streaming();
        if let Some(msg) = chat.messages.last()
            && matches!(msg.role, MessageRole::Assistant)
            && msg.has_text()
        {
            let _ = app
                .insert_session_message(Some(&pid), MessageRole::Assistant, &msg.text_content())
                .await;
        }
    }
    let _ = app
        .insert_session_message(Some(&pid), MessageRole::User, message)
        .await;
    chat.add_user_message(message.to_string());
    ctx.session
        .send_message(message, chat.session_id.as_deref());
    ps.chat_view.reset_scroll();

    let resp = serde_json::json!({"ok": true});
    let _ = write!(&stream, "{resp}");
}

fn handle_hook_permission(
    state: &mut ScreenState,
    stream: UnixStream,
    req: PermissionRequest,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
    perm_id_counter: &mut u64,
) {
    let task_name = state.task_name_for_cwd_or(&req.cwd, TaskName::from(EXO_PERM_KEY.to_string()));
    state.mark_task_active_by_name(&task_name);
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

/// Handle a single telegram inbound event.
pub(super) async fn dispatch_telegram_event<R: Runtime>(
    state: &mut ScreenState,
    exo_session: &mut AssistantSession,
    app: &ClatApp<R>,
    tg_msg: telegram::TgInbound,
) {
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
                    let _ = app
                        .insert_session_message(None, MessageRole::Assistant, &msg.text_content())
                        .await;
                }
            }
            let _ = app
                .insert_session_message(None, MessageRole::User, &text)
                .await;
            chat.add_user_message(text.clone());
            exo_session.send_message(&text, chat.session_id.as_deref());
        }
    }
}

pub(super) async fn tick_refresh<R: Runtime>(
    state: &mut ScreenState,
    app: &ClatApp<R>,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
) {
    if let Ok(tasks) = app.list_visible(state.active_project_id.as_ref()).await {
        state.refresh_tasks(tasks);
    }
    // Update global task→project mapping and drain stale permissions.
    let all_active = app.list_active().await.unwrap_or_default();
    let all_running_names: HashSet<TaskName> = all_active.iter().map(|t| t.name.clone()).collect();
    let projects_map = all_active
        .iter()
        .map(|t| (t.name.clone(), t.project_id.clone()))
        .collect();
    let work_dirs = all_active
        .iter()
        .filter_map(|t| t.work_dir.as_ref().map(|wd| (t.name.clone(), wd.clone())))
        .collect();
    state.update_global_task_mappings(projects_map, work_dirs);
    for perm in state.permissions.drain_stale(&all_running_names) {
        notify_tg_resolved(tg_tx, perm.perm_id, "⚪ Expired (task ended)");
        let _ = write_response_to_stream(perm.stream, false, None);
    }
    let active = state.active_state_mut();
    active.task_list.update_window_numbers(app.window_numbers());
    // Update selected messages and live output for detail view
    if let Some(task) = active.task_list.selected_task() {
        let chat = ChatId::Task(task.id.clone());
        let is_running = task.status.is_running();
        let pane = task.tmux_pane.clone();
        if let Ok(messages) = app.messages(&chat).await {
            active.task_list.set_selected_messages(messages);
        }
        if is_running {
            active.task_list.set_live_output(
                pane.as_ref()
                    .map(|p| p.as_str())
                    .and_then(|p| app.capture_pane(p)),
            );
        } else {
            active.task_list.set_live_output(None);
        }
    } else {
        active.task_list.clear_selected_messages();
        active.task_list.set_live_output(None);
    }
}

pub(super) fn detect_vanished_perms(
    state: &ScreenState,
    tg_tx: Option<&mpsc::UnboundedSender<telegram::TgOutbound>>,
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
