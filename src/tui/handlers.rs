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

use super::PmContext;
use super::chat::ExoState;
use super::claude::{ExoEvent, ExoSession, PmEvent, pm_system_prompt};
use super::dashboard::{Dashboard, Focus};
use super::permissions::ActivePermission;
use super::telegram;

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
    app: &mut Dashboard,
    allow: bool,
) -> Option<(UnixStream, bool, Vec<serde_json::Value>, u64)> {
    let perm_key = app.active_permission_key()?;
    let perm = app.permissions.take(&perm_key)?;
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

pub(super) fn handle_paste(app: &mut Dashboard, text: String) {
    if matches!(app.focus, Focus::ChatInput) {
        if text.contains('\n') || text.contains('\r') {
            app.input.set_paste(text);
        } else {
            for c in text.chars() {
                app.input.insert(c);
            }
        }
    }
}

// ── Global key handler ──────────────────────────────────────────────

/// Handle global key shortcuts (Ctrl+C, Ctrl+P, Ctrl+Y/T/N, number keys, Ctrl+O, Ctrl+R).
/// Returns true if the key was consumed.
pub(super) fn handle_global_keys<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Ctrl+C quits
    if ctrl && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return true;
    }

    // Ctrl+P cycles to next task with pending permissions
    if ctrl && key.code == KeyCode::Char('p') {
        return handle_cycle_permissions(app, service, pm_contexts, pm_tx);
    }

    // Ctrl+Y one-time allow
    if ctrl
        && key.code == KeyCode::Char('y')
        && app.show_detail
        && app.permissions.peek(&app.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(app, true) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "✅ Approved locally");
        }
        return true;
    }

    // Ctrl+T trust / always-allow
    if ctrl
        && key.code == KeyCode::Char('t')
        && app.show_detail
        && app.permissions.peek(&app.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, suggestions, perm_id)) = resolve_permission(app, true) {
            let _ = write_response_to_stream(stream, allow, Some(&suggestions));
            notify_tg_resolved(tg_tx, perm_id, "✅ Trusted locally");
        }
        return true;
    }

    // Ctrl+N denies permission
    if ctrl
        && key.code == KeyCode::Char('n')
        && app.show_detail
        && app.permissions.peek(&app.focused_perm_key()).is_some()
    {
        if let Some((stream, allow, _suggestions, perm_id)) = resolve_permission(app, false) {
            let _ = write_response_to_stream(stream, allow, None);
            notify_tg_resolved(tg_tx, perm_id, "❌ Denied locally");
        }
        return true;
    }

    // Number keys 1-4 answer an AskUser prompt
    if matches!(key.code, KeyCode::Char('1'..='4'))
        && key.modifiers.is_empty()
        && app.show_detail
        && app
            .permissions
            .peek(&app.focused_perm_key())
            .is_some_and(|p| p.is_askuser())
    {
        handle_askuser_select(app, key, tg_tx);
        return true;
    }

    // Ctrl+O returns to ExO chat
    if ctrl && key.code == KeyCode::Char('o') {
        app.save_project_state();
        app.switch_to_project(None);
        if let Ok(tasks) = service.list_visible(None) {
            app.finish_project_switch(tasks);
        }
        return true;
    }

    // Ctrl+R returns to PM (or last active project from ExO)
    if ctrl && key.code == KeyCode::Char('r') {
        handle_goto_pm(app, service, pm_contexts, pm_tx);
        return true;
    }

    false
}

fn handle_cycle_permissions<R: Runtime>(
    app: &mut Dashboard,
    service: &ClatApp<R>,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) -> bool {
    let names = app.permissions.task_names_with_pending();
    if !names.is_empty() {
        let current = app.focused_perm_key();
        let idx = names
            .iter()
            .position(|n| n == &current)
            .map(|i| (i + 1) % names.len())
            .unwrap_or(0);
        let name = names[idx].clone();
        if name == EXO_PERM_KEY {
            // Navigate to ExO view
            if app.active_project_id.is_some() {
                app.save_project_state();
                app.switch_to_project(None);
                if let Ok(tasks) = service.list_visible(None) {
                    app.finish_project_switch(tasks);
                }
            } else {
                app.show_detail = false;
            }
        } else if let Some(pos) = app.tasks.iter().position(|t| t.name == name) {
            // Task is in the current project view
            app.save_current_input();
            app.list_state.select(Some(pos));
            app.show_detail = true;
            app.detail_scroll = 0;
            app.focus = Focus::ChatInput;
            app.chat_scroll = 0;
            app.restore_input();
        } else {
            // Task is in a different project — switch to it
            let target_pid = app.global_task_projects.get(&name).cloned().flatten();
            app.save_project_state();
            if let Some(pid) = target_pid {
                if let Ok(projects) = service.list_projects() {
                    app.projects = projects;
                }
                let proj_name = app
                    .projects
                    .iter()
                    .find(|p| p.id == pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| {
                        crate::primitives::ProjectName::from(pid.as_str().to_string())
                    });
                app.switch_to_project(Some((proj_name, pid.clone())));
                if let Ok(tasks) = service.list_visible(Some(&pid)) {
                    app.finish_project_switch(tasks);
                }
                if let Some(pos) = app.tasks.iter().position(|t| t.name == name) {
                    app.list_state.select(Some(pos));
                    app.show_detail = true;
                    app.detail_scroll = 0;
                }
                super::ensure_pm_context(pm_contexts, app, service, &pid, pm_tx);
            } else {
                app.switch_to_project(None);
                if let Ok(tasks) = service.list_visible(None) {
                    app.finish_project_switch(tasks);
                }
                if let Some(pos) = app.tasks.iter().position(|t| t.name == name) {
                    app.list_state.select(Some(pos));
                    app.show_detail = true;
                    app.detail_scroll = 0;
                }
            }
        }
    } else if app.list_state.selected().is_some() {
        app.save_current_input();
        app.show_detail = true;
        app.detail_scroll = 0;
        app.focus = Focus::ChatInput;
        app.chat_scroll = 0;
        app.restore_input();
    }
    true
}

fn handle_askuser_select(
    app: &mut Dashboard,
    key: KeyEvent,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    let digit = match key.code {
        KeyCode::Char(c) => c.to_digit(10).unwrap_or(1) as usize,
        _ => 1,
    };
    let perm_key = app.focused_perm_key();
    if let Some(perm) = app.permissions.peek(&perm_key) {
        let idx = digit - 1;
        if idx < perm.askuser_options.len() {
            let label = perm.askuser_options[idx].0.clone();
            let perm_id = perm.perm_id;
            if let Some(perm) = app.permissions.take(&perm_key) {
                let _ = write_response_with_message(perm.stream, true, &label);
                notify_tg_resolved(tg_tx, perm_id, &format!("✅ Selected: {label}"));
            }
        }
    }
}

fn handle_goto_pm<R: Runtime>(
    app: &mut Dashboard,
    service: &ClatApp<R>,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    // If in a project's task detail, go back to PM chat
    if app.show_detail && app.active_project_id.is_some() {
        app.save_current_input();
        app.show_detail = false;
        app.focus = Focus::ChatInput;
        app.chat_scroll = 0;
        app.restore_input();
    // If in ExO view, restore last active project (or first project)
    } else if app.active_project_id.is_none() {
        if let Ok(projects) = service.list_projects() {
            app.projects = projects;
        }
        let target = app
            .last_project
            .take()
            .map(|s| (s.name, s.id, s.show_detail, s.selected_task_name))
            .or_else(|| {
                app.projects
                    .first()
                    .map(|p| (p.name.clone(), p.id.clone(), false, None))
            });
        if let Some((name, id, saved_show_detail, saved_task_name)) = target {
            app.switch_to_project(Some((name, id.clone())));
            if let Ok(tasks) = service.list_visible(Some(&id)) {
                app.finish_project_switch(tasks);
            }
            if saved_show_detail
                && let Some(ref task_name) = saved_task_name
                && let Some(idx) = app.tasks.iter().position(|t| &t.name == task_name)
            {
                app.list_state.select(Some(idx));
                app.show_detail = true;
                app.detail_scroll = 0;
            }
            super::ensure_pm_context(pm_contexts, app, service, &id, pm_tx);
        }
    // If in a project PM view, cycle to next project
    } else if app.active_project_id.is_some() && !app.show_detail {
        if let Ok(projects) = service.list_projects() {
            app.projects = projects;
        }
        let cur_idx = app
            .active_project_id
            .as_ref()
            .and_then(|pid| app.projects.iter().position(|p| p.id == *pid));
        if let Some(ci) = cur_idx {
            let next_idx = (ci + 1) % app.projects.len();
            let next = &app.projects[next_idx];
            let next_id = next.id.clone();
            let next_name = next.name.clone();
            app.switch_to_project(Some((next_name, next_id.clone())));
            if let Ok(tasks) = service.list_visible(Some(&next_id)) {
                app.finish_project_switch(tasks);
            }
            super::ensure_pm_context(pm_contexts, app, service, &next_id, pm_tx);
        }
    }
}

// ── Per-focus key handlers ──────────────────────────────────────────

pub(super) fn handle_focus_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    match &app.focus {
        Focus::TaskList => handle_task_list_key(app, key, service),
        Focus::TaskSearch => handle_task_search_key(app, key),
        Focus::ProjectList => handle_project_list_key(app, key, service, pm_contexts, pm_tx),
        Focus::ProjectNameInput => handle_project_name_input_key(app, key, service),
        Focus::ChatInput if app.show_detail => handle_task_chat_input_key(app, key, service),
        Focus::ChatInput => {
            handle_chat_input_key(app, key, service, exo, exo_session, pm_contexts, pm_tx)
        }
        Focus::ChatHistory => handle_chat_history_key(app, key),
        Focus::ConfirmDelete(_) => handle_confirm_delete_key(app, key, service),
        Focus::ConfirmCloseTask(_) => handle_confirm_close_task_key(app, key, service),
        Focus::ConfirmDeleteProject(_) => handle_confirm_delete_project_key(app, key, service),
        Focus::ConfirmCloseProject => {
            handle_confirm_close_project_key(app, key, service, pm_contexts)
        }
    }
}

fn handle_task_list_key<R: Runtime>(app: &mut Dashboard, key: KeyEvent, service: &ClatApp<R>) {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => {
            app.show_detail = false;
            app.focus = Focus::ChatInput;
            app.restore_input();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.next();
            app.show_detail = true;
            app.detail_scroll = 0;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.previous();
            app.show_detail = true;
            app.detail_scroll = 0;
        }
        KeyCode::PageDown => {
            app.detail_scroll = app.detail_scroll.saturating_add(10);
        }
        KeyCode::PageUp => {
            app.detail_scroll = app.detail_scroll.saturating_sub(10);
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.detail_scroll = app.detail_scroll.saturating_add(10);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.detail_scroll = app.detail_scroll.saturating_sub(10);
        }
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            goto_task_window(app, service);
        }
        KeyCode::Enter => {
            if app.selected_task().is_some() {
                app.show_detail = true;
                app.detail_scroll = 0;
                app.focus = Focus::ChatInput;
                app.chat_scroll = 0;
                app.restore_input();
            }
        }
        KeyCode::Char('x') => {
            if let Some(task) = app.selected_task()
                && task.status.is_running()
            {
                let id = task.id.clone();
                app.focus = Focus::ConfirmCloseTask(id);
            }
        }
        KeyCode::Char('r') => {
            reopen_task(app, service);
        }
        KeyCode::Backspace => {
            if let Some(task) = app.selected_task() {
                let id = task.id.clone();
                app.focus = Focus::ConfirmDelete(id);
            }
        }
        KeyCode::Char('/') => {
            app.search_input.take();
            app.update_search_filter();
            app.focus = Focus::TaskSearch;
        }
        KeyCode::Tab => {
            app.focus = Focus::ChatInput;
            app.restore_input();
        }
        KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.focus = Focus::ChatInput;
            app.restore_input();
        }
        KeyCode::Char('p') => {
            if let Ok(projects) = service.list_projects() {
                app.projects = projects;
                if !app.projects.is_empty() {
                    app.project_list_state.select(Some(0));
                }
            }
            app.show_projects = true;
            app.focus = Focus::ProjectList;
        }
        _ => {}
    }
}

fn handle_task_search_key(app: &mut Dashboard, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let searching_projects = app.show_projects;
    let do_filter = |app: &mut Dashboard| {
        if app.show_projects {
            app.update_project_search_filter();
        } else {
            app.update_search_filter();
        }
    };
    match key.code {
        KeyCode::Esc => {
            app.search_input.take();
            if searching_projects {
                app.filtered_project_indices.clear();
                if !app.projects.is_empty() {
                    let sel = app
                        .project_list_state
                        .selected()
                        .unwrap_or(0)
                        .min(app.projects.len() - 1);
                    app.project_list_state.select(Some(sel));
                }
                app.focus = Focus::ProjectList;
            } else {
                app.filtered_indices.clear();
                if !app.tasks.is_empty() {
                    let sel = app
                        .list_state
                        .selected()
                        .unwrap_or(0)
                        .min(app.tasks.len() - 1);
                    app.list_state.select(Some(sel));
                }
                app.focus = Focus::TaskList;
            }
        }
        KeyCode::Enter => {
            if searching_projects {
                if let Some(real_idx) = app.selected_filtered_project_index() {
                    app.project_list_state.select(Some(real_idx));
                }
                app.search_input.take();
                app.filtered_project_indices.clear();
                app.focus = Focus::ProjectList;
            } else {
                if let Some(real_idx) = app.selected_filtered_task_index() {
                    app.list_state.select(Some(real_idx));
                    app.show_detail = true;
                    app.detail_scroll = 0;
                    app.focus = Focus::ChatInput;
                    app.chat_scroll = 0;
                    app.restore_input();
                } else {
                    app.focus = Focus::TaskList;
                }
                app.search_input.take();
                app.filtered_indices.clear();
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if searching_projects {
                app.search_next_project();
            } else {
                app.search_next();
            }
        }
        KeyCode::Up | KeyCode::BackTab => {
            if searching_projects {
                app.search_prev_project();
            } else {
                app.search_prev();
            }
        }
        KeyCode::Char('n') if ctrl => {
            if searching_projects {
                app.search_next_project();
            } else {
                app.search_next();
            }
        }
        KeyCode::Char('p') if ctrl => {
            if searching_projects {
                app.search_prev_project();
            } else {
                app.search_prev();
            }
        }
        KeyCode::Char('k') if ctrl => {
            app.search_input.kill_line();
            do_filter(app);
        }
        _ => {
            if handle_input_editing(&mut app.search_input, &key) {
                do_filter(app);
            }
        }
    }
}

fn handle_project_list_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.next_project(),
        KeyCode::Char('k') | KeyCode::Up => app.previous_project(),
        KeyCode::Char('/') => {
            app.search_input.take();
            app.update_project_search_filter();
            app.focus = Focus::TaskSearch;
        }
        KeyCode::Enter => {
            if let Some(project) = app.selected_project() {
                let project_id = project.id.clone();
                let project_name = project.name.clone();
                app.switch_to_project(Some((project_name, project_id.clone())));
                if let Ok(tasks) = service.list_visible(Some(&project_id)) {
                    app.finish_project_switch(tasks);
                }
                super::ensure_pm_context(pm_contexts, app, service, &project_id, pm_tx);
            }
        }
        KeyCode::Char('n') => {
            app.input.take();
            app.focus = Focus::ProjectNameInput;
        }
        KeyCode::Backspace => {
            if let Some(project) = app.selected_project() {
                let name = project.name.clone();
                app.focus = Focus::ConfirmDeleteProject(name);
            }
        }
        KeyCode::Char('p') | KeyCode::Esc => {
            app.switch_to_project(None);
            app.focus = Focus::TaskList;
            if let Ok(tasks) = service.list_visible(None) {
                app.finish_project_switch(tasks);
            }
        }
        _ => {}
    }
}

fn handle_project_name_input_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.input.take();
            app.focus = Focus::ProjectList;
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let name = app.input.take();
                match service.create_project(&name, "") {
                    Ok(project) => {
                        let project_id = project.id.clone();
                        app.switch_to_project(Some((project.name.clone(), project_id.clone())));
                        if let Ok(tasks) = service.list_visible(Some(&project_id)) {
                            app.finish_project_switch(tasks);
                        }
                    }
                    Err(e) => {
                        app.status_error = Some(format!("create project: {e}"));
                        app.focus = Focus::ProjectList;
                    }
                }
            } else {
                app.focus = Focus::ProjectList;
            }
        }
        KeyCode::Char('k') if ctrl => app.input.kill_line(),
        _ => {
            handle_input_editing(&mut app.input, &key);
        }
    }
}

fn handle_task_chat_input_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.save_current_input();
            app.show_detail = false;
            app.chat_scroll = 0;
            app.restore_input();
        }
        KeyCode::Tab => {
            app.save_current_input();
            app.chat_scroll = 0;
            let current = app.list_state.selected().unwrap_or(0);
            if current + 1 < app.tasks.len() {
                app.list_state.select(Some(current + 1));
                app.detail_scroll = 0;
            } else {
                app.show_detail = false;
            }
            app.restore_input();
        }
        KeyCode::BackTab => {
            app.save_current_input();
            app.chat_scroll = 0;
            let current = app.list_state.selected().unwrap_or(0);
            if current > 0 {
                app.list_state.select(Some(current - 1));
                app.detail_scroll = 0;
            } else {
                app.show_detail = false;
            }
            app.restore_input();
        }
        KeyCode::Char('k') if ctrl => {
            app.focus = Focus::ChatHistory;
        }
        KeyCode::Char('x') if ctrl => {
            if let Some(task) = app.selected_task()
                && task.status.is_running()
            {
                let id = task.id.clone();
                app.focus = Focus::ConfirmCloseTask(id);
            }
        }
        KeyCode::Char('l') if ctrl => {
            app.save_current_input();
            app.focus = Focus::TaskList;
        }
        KeyCode::Char('g') if ctrl => {
            goto_task_window(app, service);
        }
        KeyCode::Enter => {
            if !app.input.is_empty() {
                let msg = app.input.take();
                if let Some(task) = app.selected_task() {
                    let task_id = task.id.as_str().to_string();
                    let pane = task.tmux_pane.clone();
                    match service.send(&task_id, &msg) {
                        Ok(_) => {
                            if let Some(pane) = pane {
                                app.idle_panes.remove(&pane);
                            }
                        }
                        Err(e) => {
                            app.status_error = Some(format!("send: {e}"));
                        }
                    }
                }
            }
        }
        _ => {
            handle_input_editing(&mut app.input, &key);
        }
    }
}

fn handle_chat_input_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            if exo.streaming {
                exo.finish_streaming();
            }
            if let Some(ref pid) = app.active_project_id
                && let Some(ctx) = pm_contexts.get_mut(pid)
                && ctx.state.streaming
            {
                ctx.state.finish_streaming();
            }
            app.chat_scroll = 0;
        }
        KeyCode::Tab => {
            app.save_current_input();
            app.chat_scroll = 0;
            if !app.tasks.is_empty() {
                app.list_state.select(Some(0));
                app.show_detail = true;
                app.detail_scroll = 0;
            }
            app.restore_input();
        }
        KeyCode::BackTab => {
            app.save_current_input();
            app.chat_scroll = 0;
            if !app.tasks.is_empty() {
                app.list_state.select(Some(app.tasks.len() - 1));
                app.show_detail = true;
                app.detail_scroll = 0;
            }
            app.restore_input();
        }
        KeyCode::Char('k') if ctrl => {
            app.focus = Focus::ChatHistory;
        }
        KeyCode::Char('x') if ctrl && app.active_project.is_some() => {
            app.focus = Focus::ConfirmCloseProject;
        }
        KeyCode::Char('l') if ctrl => {
            app.save_current_input();
            app.focus = Focus::TaskList;
            if app.list_state.selected().is_some() {
                app.show_detail = true;
                app.detail_scroll = 0;
            }
        }
        KeyCode::Enter => {
            handle_chat_enter(app, service, exo, exo_session, pm_contexts, pm_tx);
        }
        _ => {
            handle_input_editing(&mut app.input, &key);
        }
    }
}

fn handle_chat_enter<R: Runtime>(
    app: &mut Dashboard,
    service: &ClatApp<R>,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    pm_tx: &mpsc::Sender<PmEvent>,
) {
    app.chat_scroll = 0;
    if app.input.is_empty() {
        return;
    }
    if let Some(ref pid) = app.active_project_id {
        if !pm_contexts.contains_key(pid) {
            pm_contexts.insert(pid.clone(), PmContext::new(pid, pm_tx));
        }
        let ctx = pm_contexts.get_mut(pid).unwrap();
        if ctx.state.streaming {
            ctx.state.finish_streaming();
            if let Some(msg) = ctx.state.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = service.insert_pm_message(pid, MessageRole::Assistant, &msg.text_content());
            }
        }
        let msg = app.input.take();
        let _ = service.insert_pm_message(pid, MessageRole::User, &msg);
        ctx.state.add_user_message(msg.clone());
        if ctx.session.is_none() {
            let sid = service
                .read_pm_session_id(pid)
                .or_else(|| ctx.state.session_id.clone());
            let proj_name = app
                .active_project
                .as_ref()
                .map(|p| p.as_str())
                .unwrap_or("unknown");
            let prompt = pm_system_prompt(proj_name);
            ctx.session = Some(ExoSession::new(
                sid.as_deref(),
                Arc::clone(&ctx.cancel),
                ctx.bridge_tx.clone(),
                &prompt,
            ));
            if let Some(ref s) = sid {
                ctx.state.session_id = Some(s.clone());
            }
        }
        if let Some(sess) = &mut ctx.session {
            sess.send_message(&msg, ctx.state.session_id.as_deref());
            app.chat_scroll = 0;
        }
    } else {
        if exo.streaming {
            exo.finish_streaming();
            if let Some(msg) = exo.messages.last()
                && matches!(msg.role, MessageRole::Assistant)
                && msg.has_text()
            {
                let _ = service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
            }
        }
        let msg = app.input.take();
        let _ = service.insert_exo_message(MessageRole::User, &msg);
        exo.add_user_message(msg.clone());
        exo_session.send_message(&msg, exo.session_id.as_deref());
        app.chat_scroll = 0;
    }
}

fn handle_chat_history_key(app: &mut Dashboard, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('j') if ctrl => {
            app.focus = Focus::ChatInput;
        }
        KeyCode::Char('u') if ctrl => {
            let half = (app.chat_viewport_height / 2).max(1);
            app.chat_scroll = app.chat_scroll.saturating_add(half);
        }
        KeyCode::Char('d') if ctrl => {
            let half = (app.chat_viewport_height / 2).max(1);
            app.chat_scroll = app.chat_scroll.saturating_sub(half);
        }
        KeyCode::Char('l') if ctrl => {
            app.focus = Focus::TaskList;
        }
        KeyCode::Esc => {
            app.focus = Focus::ChatInput;
            app.chat_scroll = 0;
        }
        _ => {}
    }
}

fn handle_confirm_delete_key<R: Runtime>(app: &mut Dashboard, key: KeyEvent, service: &ClatApp<R>) {
    let task_id = match &app.focus {
        Focus::ConfirmDelete(id) => id.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = service.delete(task_id.as_str());
            if let Ok(tasks) = service.list_visible(app.active_project_id.as_ref()) {
                app.refresh_tasks(tasks);
            }
            app.focus = Focus::TaskList;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.focus = Focus::TaskList;
        }
        _ => {}
    }
}

fn handle_confirm_close_task_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
) {
    let task_id = match &app.focus {
        Focus::ConfirmCloseTask(id) => id.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = service.close(task_id.as_str());
            if let Ok(tasks) = service.list_visible(app.active_project_id.as_ref()) {
                app.refresh_tasks(tasks);
            }
            app.show_detail = false;
            app.focus = Focus::ChatInput;
            app.restore_input();
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.focus = if app.show_detail {
                Focus::ChatInput
            } else {
                Focus::TaskList
            };
        }
        _ => {}
    }
}

fn handle_confirm_delete_project_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
) {
    let project_name = match &app.focus {
        Focus::ConfirmDeleteProject(name) => name.clone(),
        _ => return,
    };
    match key.code {
        KeyCode::Char('y') => {
            let _ = service.delete_project(project_name.as_str());
            if let Ok(projects) = service.list_projects() {
                app.projects = projects;
                if app.projects.is_empty() {
                    app.project_list_state.select(None);
                } else {
                    let sel = app
                        .project_list_state
                        .selected()
                        .unwrap_or(0)
                        .min(app.projects.len().saturating_sub(1));
                    app.project_list_state.select(Some(sel));
                }
            }
            app.focus = Focus::ProjectList;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.focus = Focus::ProjectList;
        }
        _ => {}
    }
}

fn handle_confirm_close_project_key<R: Runtime>(
    app: &mut Dashboard,
    key: KeyEvent,
    service: &ClatApp<R>,
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
) {
    match key.code {
        KeyCode::Char('y') => {
            let closed_pid = app.active_project_id.clone();
            app.switch_to_project(None);
            app.focus = Focus::TaskList;
            if let Some(pid) = closed_pid {
                super::cancel_pm_context(pm_contexts, &pid);
            }
            if let Ok(tasks) = service.list_visible(None) {
                app.finish_project_switch(tasks);
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.focus = Focus::ChatInput;
        }
        _ => {}
    }
}

/// Shared: go to the selected task's tmux window (or reopen if closed).
fn goto_task_window<R: Runtime>(app: &mut Dashboard, service: &ClatApp<R>) {
    if let Some(task) = app.selected_task() {
        if task.status.is_running() {
            if let Some(window_id) = &task.tmux_window {
                service.goto_window(window_id);
            }
        } else {
            let id = task.id.as_str().to_string();
            match service.reopen(&id) {
                Ok(window_id) => {
                    if let Ok(tasks) = service.list_visible(app.active_project_id.as_ref()) {
                        app.refresh_tasks(tasks);
                    }
                    service.goto_window(&window_id);
                }
                Err(e) => {
                    app.status_error = Some(format!("reopen: {e}"));
                }
            }
        }
    }
}

/// Shared: reopen a closed task.
fn reopen_task<R: Runtime>(app: &mut Dashboard, service: &ClatApp<R>) {
    if let Some(task) = app.selected_task()
        && !task.status.is_running()
    {
        let id = task.id.as_str().to_string();
        match service.reopen(&id) {
            Ok(_) => {
                if let Ok(tasks) = service.list_visible(app.active_project_id.as_ref()) {
                    app.refresh_tasks(tasks);
                }
            }
            Err(e) => {
                app.status_error = Some(format!("reopen: {e}"));
            }
        }
    }
}

// ── Channel draining ────────────────────────────────────────────────

pub(super) fn drain_exo_events<R: Runtime>(
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    service: &ClatApp<R>,
    rx: &mpsc::Receiver<ExoEvent>,
    app: &mut Dashboard,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    while let Ok(ev) = rx.try_recv() {
        match ev {
            ExoEvent::TextDelta(text) => {
                if exo.streaming {
                    exo.append_text(&text);
                    app.chat_scroll = 0;
                    if let Some(tx) = tg_tx {
                        let _ = tx.send(telegram::TgOutbound::ExoTextDelta { text: text.clone() });
                    }
                }
            }
            ExoEvent::ToolStart(name) => {
                if exo.streaming {
                    exo.add_tool_activity(name);
                }
            }
            ExoEvent::SessionId(id) => {
                service.write_exo_session_id(&id);
                exo.session_id = Some(id.clone());
                exo_session.set_session_id(id);
            }
            ExoEvent::TurnDone => {
                exo.finish_streaming();
                if let Some(msg) = exo.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                }
                if let Some(tx) = tg_tx {
                    let _ = tx.send(telegram::TgOutbound::ExoTurnDone);
                }
            }
            ExoEvent::ProcessExited => {
                exo.had_process_error = false;
                exo_session.mark_exited();
                if exo.streaming {
                    exo.add_error("Claude process exited unexpectedly");
                }
            }
            ExoEvent::Error(e) => {
                exo.had_process_error = true;
                exo.add_error(&e);
                if let Some(msg) = exo.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                }
            }
        }
    }
}

pub(super) fn drain_pm_events<R: Runtime>(
    pm_contexts: &mut HashMap<ProjectId, PmContext>,
    service: &ClatApp<R>,
    pm_rx: &mpsc::Receiver<PmEvent>,
    app: &mut Dashboard,
) {
    while let Ok(pm_ev) = pm_rx.try_recv() {
        let project_id = pm_ev.project_id;
        let Some(ctx) = pm_contexts.get_mut(&project_id) else {
            continue;
        };
        let is_active_pm = app.active_project_id.as_ref() == Some(&project_id);
        match pm_ev.inner {
            ExoEvent::TextDelta(text) => {
                if ctx.state.streaming {
                    ctx.state.append_text(&text);
                    if is_active_pm {
                        app.chat_scroll = 0;
                    }
                }
            }
            ExoEvent::ToolStart(name) => {
                if ctx.state.streaming {
                    ctx.state.add_tool_activity(name);
                }
            }
            ExoEvent::SessionId(id) => {
                service.write_pm_session_id(&project_id, &id);
                ctx.state.session_id = Some(id.clone());
                if let Some(sess) = &mut ctx.session {
                    sess.set_session_id(id);
                }
            }
            ExoEvent::TurnDone => {
                ctx.state.finish_streaming();
                if let Some(msg) = ctx.state.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = service.insert_pm_message(
                        &project_id,
                        MessageRole::Assistant,
                        &msg.text_content(),
                    );
                }
            }
            ExoEvent::ProcessExited => {
                ctx.state.had_process_error = false;
                if let Some(sess) = &mut ctx.session {
                    sess.mark_exited();
                }
                if ctx.state.streaming {
                    ctx.state.add_error("PM process exited unexpectedly");
                }
            }
            ExoEvent::Error(e) => {
                ctx.state.had_process_error = true;
                ctx.state.add_error(&e);
                if let Some(msg) = ctx.state.messages.last()
                    && matches!(msg.role, MessageRole::Assistant)
                    && msg.has_text()
                {
                    let _ = service.insert_pm_message(
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
    app: &mut Dashboard,
    resolved_rx: &mpsc::Receiver<String>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
) {
    while let Ok(cwd) = resolved_rx.try_recv() {
        let resolved_cwd =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        let task_name = find_task_name_by_cwd(&app.global_task_work_dirs, &resolved_cwd);
        if let Some(ref name) = task_name {
            if let Some(pane_id) = app
                .tasks
                .iter()
                .find(|t| t.name == *name)
                .and_then(|t| t.tmux_pane.as_ref())
            {
                app.idle_panes.remove(pane_id);
            }
            if let Some(perm) = app.permissions.take(name) {
                let _ = write_response_to_stream(perm.stream, false, None);
                if tg_perm_ids.remove(&perm.perm_id) {
                    notify_tg_resolved(tg_tx, perm.perm_id, "✅ Resolved in pane");
                }
            }
        }
    }
}

pub(super) fn drain_idle(app: &mut Dashboard, idle_rx: &mpsc::Receiver<String>) {
    while let Ok(cwd) = idle_rx.try_recv() {
        let cwd_path =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        if let Some(task_name) = find_task_name_by_cwd(&app.global_task_work_dirs, &cwd_path)
            && let Some(pane_id) = app
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            app.idle_panes.insert(pane_id.clone());
        }
    }
}

/// Drain active notifications from Notification hooks — mark pane as not idle.
pub(super) fn drain_active(app: &mut Dashboard, active_rx: &mpsc::Receiver<String>) {
    while let Ok(cwd) = active_rx.try_recv() {
        let cwd_path =
            std::fs::canonicalize(&cwd).unwrap_or_else(|_| std::path::PathBuf::from(&cwd));
        if let Some(task_name) = find_task_name_by_cwd(&app.global_task_work_dirs, &cwd_path)
            && let Some(pane_id) = app
                .tasks
                .iter()
                .find(|t| t.name == task_name)
                .and_then(|t| t.tmux_pane.as_ref())
        {
            app.idle_panes.remove(pane_id);
        }
    }
}

pub(super) fn drain_permissions(
    app: &mut Dashboard,
    perm_rx: &mpsc::Receiver<(UnixStream, PermissionRequest)>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
    perm_id_counter: &mut u64,
) {
    while let Ok((stream, req)) = perm_rx.try_recv() {
        let req_cwd =
            std::fs::canonicalize(&req.cwd).unwrap_or_else(|_| std::path::PathBuf::from(&req.cwd));
        let task_name = find_task_name_by_cwd(&app.global_task_work_dirs, &req_cwd)
            .unwrap_or_else(|| TaskName::from(EXO_PERM_KEY.to_string()));
        if let Some(pane_id) = app
            .tasks
            .iter()
            .find(|t| t.name == task_name)
            .and_then(|t| t.tmux_pane.as_ref())
        {
            app.idle_panes.remove(pane_id);
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
        app.permissions.add(perm);
    }
}

pub(super) fn drain_telegram<R: Runtime>(
    app: &mut Dashboard,
    exo: &mut ExoState,
    exo_session: &mut ExoSession,
    service: &ClatApp<R>,
    tg_rx: Option<&mpsc::Receiver<telegram::TgInbound>>,
) {
    let Some(rx) = tg_rx else { return };
    while let Ok(tg_msg) = rx.try_recv() {
        match tg_msg {
            telegram::TgInbound::PermissionDecision { perm_id, action } => {
                let task_name = app
                    .permissions
                    .iter()
                    .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                    .map(|(name, _)| name.clone());
                if let Some(name) = task_name
                    && app
                        .permissions
                        .peek(&name)
                        .is_some_and(|front| front.perm_id == perm_id)
                    && let Some(perm) = app.permissions.take(&name)
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
                let task_name = app
                    .permissions
                    .iter()
                    .find(|(_, queue)| queue.iter().any(|p| p.perm_id == perm_id))
                    .map(|(name, _)| name.clone());
                if let Some(name) = task_name
                    && app
                        .permissions
                        .peek(&name)
                        .is_some_and(|front| front.perm_id == perm_id)
                    && let Some(perm) = app.permissions.take(&name)
                {
                    let _ = write_response_with_message(perm.stream, true, &answer);
                }
            }
            telegram::TgInbound::ExoMessage { text } => {
                app.chat_scroll = 0;
                if exo.streaming {
                    exo.finish_streaming();
                    if let Some(msg) = exo.messages.last()
                        && matches!(msg.role, MessageRole::Assistant)
                        && msg.has_text()
                    {
                        let _ =
                            service.insert_exo_message(MessageRole::Assistant, &msg.text_content());
                    }
                }
                let _ = service.insert_exo_message(MessageRole::User, &text);
                exo.add_user_message(text.clone());
                exo_session.send_message(&text, exo.session_id.as_deref());
            }
        }
    }
}

pub(super) fn tick_refresh<R: Runtime>(
    app: &mut Dashboard,
    service: &ClatApp<R>,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
) {
    if let Ok(tasks) = service.list_visible(app.active_project_id.as_ref()) {
        app.refresh_tasks(tasks);
    }
    // Update global task→project mapping and drain stale permissions.
    let all_active = service.list_active().unwrap_or_default();
    let all_running_names: HashSet<TaskName> = all_active.iter().map(|t| t.name.clone()).collect();
    app.global_task_projects = all_active
        .iter()
        .map(|t| (t.name.clone(), t.project_id.clone()))
        .collect();
    app.global_task_work_dirs = all_active
        .iter()
        .filter_map(|t| t.work_dir.as_ref().map(|wd| (t.name.clone(), wd.clone())))
        .collect();
    for perm in app.permissions.drain_stale(&all_running_names) {
        notify_tg_resolved(tg_tx, perm.perm_id, "⚪ Expired (task ended)");
        let _ = write_response_to_stream(perm.stream, false, None);
    }
    app.window_numbers = service.window_numbers();
    // Update selected messages and live output for detail view
    if let Some(task) = app.selected_task() {
        let chat = ChatId::Task(task.id.clone());
        let is_running = task.status.is_running();
        let pane = task.tmux_pane.clone();
        if let Ok(messages) = service.messages(&chat) {
            app.selected_messages = messages;
        }
        if is_running {
            app.detail_live_output = pane
                .as_ref()
                .map(|p| p.as_str())
                .and_then(|p| service.capture_pane(p));
        } else {
            app.detail_live_output = None;
        }
    } else {
        app.selected_messages.clear();
        app.detail_live_output = None;
    }
    // Refresh PM messages for active project
    if let Some(ref pid) = app.active_project_id {
        if let Ok(messages) = service.pm_messages(pid) {
            app.pm_messages = messages;
        }
    } else {
        app.pm_messages.clear();
    }
}

pub(super) fn detect_vanished_perms(
    app: &Dashboard,
    tg_tx: Option<&mpsc::Sender<telegram::TgOutbound>>,
    tg_perm_ids: &mut HashSet<u64>,
) {
    if tg_perm_ids.is_empty() {
        return;
    }
    let still_pending = app.permissions.all_perm_ids();
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
