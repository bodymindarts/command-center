use std::collections::{HashMap, HashSet};

use ratatui::widgets::ListState;

use crate::primitives::{ChatId, PaneId, ProjectId, ProjectName, TaskId, TaskName, WindowId};
use crate::task::{Project, Task, TaskMessage};

use super::chat::AssistantChat;

pub use super::input::InputState;
pub use super::permissions::PermissionStore;

pub enum Focus {
    TaskList,
    TaskSearch,
    ProjectList,
    ChatInput,
    ChatHistory,
    ConfirmDelete(TaskId),
    ConfirmDeleteProject(ProjectName),
    ConfirmCloseTask(TaskId),
    ConfirmCloseProject,
}

/// Saved UI state for a project, restored on Ctrl+R.
pub struct SavedProjectState {
    pub name: ProjectName,
    pub id: ProjectId,
    pub show_detail: bool,
    pub selected_task_name: Option<TaskName>,
}

// ── Task list component ─────────────────────────────────────────────

pub struct TaskListState {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    pub show_detail: bool,
    pub selected_messages: Vec<TaskMessage>,
    pub detail_scroll: u16,
    pub detail_live_output: Option<String>,
    pub window_numbers: HashMap<WindowId, String>,
    /// Pane IDs that appear idle (shell prompt visible), refreshed periodically.
    pub idle_panes: HashSet<PaneId>,
    /// Indices into `tasks` that match the current search query.
    pub filtered_indices: Vec<usize>,
}

impl TaskListState {
    fn new(tasks: Vec<Task>) -> Self {
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            tasks,
            list_state,
            show_detail: false,
            selected_messages: Vec::new(),
            detail_scroll: 0,
            detail_live_output: None,
            window_numbers: HashMap::new(),
            idle_panes: HashSet::new(),
            filtered_indices: Vec::new(),
        }
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.list_state.selected().and_then(|i| self.tasks.get(i))
    }

    pub fn next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.tasks.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.tasks.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn refresh_tasks(&mut self, tasks: Vec<Task>) {
        let selected_id = self.selected_task().map(|t| t.id.clone());

        self.tasks = tasks;
        if let Some(id) = selected_id {
            if let Some(pos) = self.tasks.iter().position(|t| t.id == id) {
                self.list_state.select(Some(pos));
            } else if !self.tasks.is_empty() {
                self.detail_scroll = 0;
                let clamped = self
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.tasks.len() - 1);
                self.list_state.select(Some(clamped));
            } else {
                self.detail_scroll = 0;
                self.list_state.select(None);
            }
        } else if !self.tasks.is_empty() && self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
    }

    pub fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(10);
    }

    pub fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(10);
    }

    /// Resolve the currently selected filtered index back to the real task index.
    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_indices.get(i).copied())
    }

    /// Move to the next item in filtered search results.
    pub fn search_next(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.filtered_indices.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move to the previous item in filtered search results.
    pub fn search_prev(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

// ── Project list component ──────────────────────────────────────────

pub struct ProjectListState {
    /// Cached list of projects for rendering.
    pub projects: Vec<Project>,
    /// Selection state for the project list.
    pub list_state: ListState,
    /// Whether the right panel shows the project list instead of the task list.
    pub show_projects: bool,
    /// Currently active project name (for display). None = default (ExO).
    pub active_project: Option<ProjectName>,
    /// Currently active project ID (for queries). None = default (ExO).
    pub active_project_id: Option<ProjectId>,
    /// Last active project state — remembered when Ctrl+O leaves a project.
    pub last_project: Option<SavedProjectState>,
    /// Indices into `projects` that match the current search query.
    pub filtered_project_indices: Vec<usize>,
}

impl ProjectListState {
    fn new() -> Self {
        Self {
            projects: Vec::new(),
            list_state: ListState::default(),
            show_projects: false,
            active_project: None,
            active_project_id: None,
            last_project: None,
            filtered_project_indices: Vec::new(),
        }
    }

    pub fn next_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.projects.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.projects.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.list_state
            .selected()
            .and_then(|i| self.projects.get(i))
    }

    /// Refresh the project list and clamp the selection after changes (e.g. delete).
    pub fn refresh_projects(&mut self, projects: Vec<Project>) {
        self.projects = projects;
        if self.projects.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.projects.len().saturating_sub(1));
            self.list_state.select(Some(sel));
        }
    }

    pub fn selected_filtered_project_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_project_indices.get(i).copied())
    }

    pub fn search_next_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.filtered_project_indices.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn search_prev_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_project_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

// ── Chat view component ─────────────────────────────────────────────

pub struct ChatViewState {
    /// Per-chat input buffers, saved/restored on focus changes.
    pub chat_buffers: HashMap<ChatId, String>,
    pub chat_scroll: u16,
    pub chat_viewport_height: u16,
    /// ExO assistant chat state (messages, streaming flag).
    pub exo_chat: AssistantChat,
    /// Per-project PM assistant chat states.
    pub project_chats: HashMap<ProjectId, AssistantChat>,
}

impl ChatViewState {
    fn new() -> Self {
        Self {
            chat_buffers: HashMap::new(),
            chat_scroll: 0,
            chat_viewport_height: 0,
            exo_chat: AssistantChat::new(),
            project_chats: HashMap::new(),
        }
    }

    pub fn update_chat_viewport_height(&mut self, area_height: u16) {
        self.chat_viewport_height = area_height.saturating_sub(2);
    }

    pub fn scroll_chat_up(&mut self) {
        let half = (self.chat_viewport_height / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_add(half);
    }

    pub fn scroll_chat_down(&mut self) {
        let half = (self.chat_viewport_height / 2).max(1);
        self.chat_scroll = self.chat_scroll.saturating_sub(half);
    }
}

/// Determine which chat buffer corresponds to the current view.
fn current_chat_id(tl: &TaskListState, pl: &ProjectListState) -> ChatId {
    if tl.show_detail {
        tl.selected_task()
            .map(|t| ChatId::Task(t.id.clone()))
            .unwrap_or(ChatId::Exo)
    } else if let Some(ref pid) = pl.active_project_id {
        ChatId::Project(pid.clone())
    } else {
        ChatId::Exo
    }
}

// ── Top-level screen state ──────────────────────────────────────────

pub struct ScreenState {
    pub task_list: TaskListState,
    pub project_list: ProjectListState,
    pub chat_view: ChatViewState,
    pub should_quit: bool,
    pub focus: Focus,
    pub input: InputState,
    pub permissions: PermissionStore,
    /// Transient error message shown in the prompt bar. Cleared on next keypress.
    pub status_error: Option<String>,
    /// Input state for the task search filter.
    pub search_input: InputState,
    /// Global map of task_name → project_id for all running tasks.
    /// Updated every tick from the full (unscoped) active task list.
    pub global_task_projects: HashMap<TaskName, Option<ProjectId>>,
    /// Global list of (task_name, work_dir) for all running tasks.
    /// Used for CWD→task matching in permission/resolved/idle handlers
    /// so lookups work regardless of which project is currently displayed.
    pub global_task_work_dirs: Vec<(TaskName, String)>,
}

impl ScreenState {
    pub fn new(tasks: Vec<Task>) -> Self {
        Self {
            task_list: TaskListState::new(tasks),
            project_list: ProjectListState::new(),
            chat_view: ChatViewState::new(),
            should_quit: false,
            focus: Focus::ChatInput,
            input: InputState::new(),
            permissions: PermissionStore::new(),
            status_error: None,
            search_input: InputState::new(),
            global_task_projects: HashMap::new(),
            global_task_work_dirs: Vec::new(),
        }
    }

    // ── Delegates to TaskListState ──────────────────────────────────

    pub fn selected_task(&self) -> Option<&Task> {
        self.task_list.selected_task()
    }

    pub fn next(&mut self) {
        self.task_list.next();
    }

    pub fn previous(&mut self) {
        self.task_list.previous();
    }

    pub fn refresh_tasks(&mut self, tasks: Vec<Task>) {
        self.task_list.refresh_tasks(tasks);
    }

    pub fn scroll_detail_down(&mut self) {
        self.task_list.scroll_detail_down();
    }

    pub fn scroll_detail_up(&mut self) {
        self.task_list.scroll_detail_up();
    }

    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.task_list.selected_filtered_task_index()
    }

    // ── Delegates to ProjectListState ───────────────────────────────

    pub fn selected_project(&self) -> Option<&Project> {
        self.project_list.selected_project()
    }

    pub fn refresh_projects(&mut self, projects: Vec<Project>) {
        self.project_list.refresh_projects(projects);
    }

    pub fn selected_filtered_project_index(&self) -> Option<usize> {
        self.project_list.selected_filtered_project_index()
    }

    // ── Focus ───────────────────────────────────────────────────────

    pub fn current_focus(&self) -> &Focus {
        &self.focus
    }

    pub fn navigate_focus_down(&mut self) {
        self.focus = Focus::ChatInput;
    }

    pub fn navigate_focus_right(&mut self) {
        self.focus = Focus::TaskList;
    }

    // ── Delegates to ChatViewState ─────────────────────────────────

    pub fn update_chat_viewport_height(&mut self, area_height: u16) {
        self.chat_view.update_chat_viewport_height(area_height);
    }

    pub fn scroll_chat_up(&mut self) {
        self.chat_view.scroll_chat_up();
    }

    pub fn scroll_chat_down(&mut self) {
        self.chat_view.scroll_chat_down();
    }

    // ── Permissions ─────────────────────────────────────────────────

    /// Count pending permissions only for tasks in the current project.
    pub fn current_project_perm_count(&self) -> usize {
        self.permissions.count_for_project(
            self.project_list.active_project_id.as_ref(),
            &self.global_task_projects,
        )
    }

    /// Count pending permissions for tasks NOT in the current project.
    pub fn other_project_perm_counts(&self) -> Vec<(String, usize)> {
        self.permissions.other_project_counts(
            self.project_list.active_project_id.as_ref(),
            &self.global_task_projects,
            &self.project_list.projects,
        )
    }

    /// Count pending AskUser permissions in the current project.
    pub fn current_project_askuser_count(&self) -> usize {
        self.permissions.askuser_count_for_project(
            self.project_list.active_project_id.as_ref(),
            &self.global_task_projects,
        )
    }

    /// Returns the permission key for the currently visible pane.
    /// Task name if viewing a task's detail, "exo" otherwise.
    pub fn focused_perm_key(&self) -> TaskName {
        if self.task_list.show_detail {
            self.selected_task()
                .map(|t| t.name.clone())
                .unwrap_or_else(|| TaskName::from("exo".to_string()))
        } else {
            TaskName::from("exo".to_string())
        }
    }

    /// Returns the permission key to display in the overlay and act on
    /// with global keybindings. Prefers the focused task's key; falls
    /// back to any task with pending permissions.
    pub fn active_permission_key(&self) -> Option<TaskName> {
        let focused = self.focused_perm_key();
        if self.permissions.peek(&focused).is_some() {
            return Some(focused);
        }
        self.permissions
            .task_names_with_pending()
            .into_iter()
            .next()
    }

    // ── Input save/restore ──────────────────────────────────────────

    pub fn save_current_input(&mut self) {
        let chat_id = current_chat_id(&self.task_list, &self.project_list);
        let text = self.input.buffer();
        if text.is_empty() {
            self.chat_view.chat_buffers.remove(&chat_id);
        } else {
            self.chat_view.chat_buffers.insert(chat_id, text);
        }
    }

    pub fn restore_input(&mut self) {
        let chat_id = current_chat_id(&self.task_list, &self.project_list);
        let text = self
            .chat_view
            .chat_buffers
            .get(&chat_id)
            .cloned()
            .unwrap_or_default();
        self.input.take();
        self.input.set(&text);
    }

    // ── Detail view ─────────────────────────────────────────────────

    /// Open a task's detail view: select it, show detail panel, reset scrolls,
    /// focus chat input, and restore that task's input buffer.
    /// Callers that need to preserve the *current* input should call
    /// `save_current_input()` **before** this method.
    pub fn open_task_detail(&mut self, index: usize) {
        self.task_list.list_state.select(Some(index));
        self.task_list.show_detail = true;
        self.task_list.detail_scroll = 0;
        self.focus = Focus::ChatInput;
        self.chat_view.chat_scroll = 0;
        self.restore_input();
    }

    /// Leave the task detail view and return to the main chat.
    /// Resets chat scroll and restores the main chat input buffer.
    /// Callers that need to preserve the *current* input should call
    /// `save_current_input()` **before** this method.
    pub fn close_task_detail(&mut self) {
        self.task_list.show_detail = false;
        self.chat_view.chat_scroll = 0;
        self.focus = Focus::ChatInput;
        self.restore_input();
    }

    /// Move to the next (forward=true) or previous (forward=false) task
    /// from within the task chat input. Returns `true` if navigation stayed
    /// within bounds, `false` if it wrapped past the edge (detail is hidden).
    pub fn navigate_to_adjacent_task(&mut self, forward: bool) -> bool {
        self.save_current_input();
        self.chat_view.chat_scroll = 0;
        let current = self.task_list.list_state.selected().unwrap_or(0);
        let in_bounds = if forward {
            if current + 1 < self.task_list.tasks.len() {
                self.task_list.list_state.select(Some(current + 1));
                self.task_list.detail_scroll = 0;
                true
            } else {
                self.task_list.show_detail = false;
                false
            }
        } else if current > 0 {
            self.task_list.list_state.select(Some(current - 1));
            self.task_list.detail_scroll = 0;
            true
        } else {
            self.task_list.show_detail = false;
            false
        };
        self.restore_input();
        in_bounds
    }

    // ── Project switching ───────────────────────────────────────────

    /// Switch to a project (or ExO when `project` is `None`).
    /// Saves current project state, swaps project context, loads tasks,
    /// and optionally focuses a specific task by name.
    pub fn switch_to_project(
        &mut self,
        project: Option<(ProjectName, ProjectId)>,
        tasks: Vec<Task>,
        focus_task: Option<&TaskName>,
    ) {
        // Save current project state for Ctrl+R restore
        let selected_task_name = self.selected_task().map(|t| t.name.clone());
        if let (Some(name), Some(id)) = (
            self.project_list.active_project.take(),
            self.project_list.active_project_id.take(),
        ) {
            self.project_list.last_project = Some(SavedProjectState {
                name,
                id,
                show_detail: self.task_list.show_detail,
                selected_task_name,
            });
        }

        // Switch context
        self.save_current_input();
        self.project_list.active_project = project.as_ref().map(|(n, _)| n.clone());
        self.project_list.active_project_id = project.map(|(_, id)| id);
        self.project_list.show_projects = false;
        self.task_list.show_detail = false;
        self.focus = Focus::ChatInput;
        self.chat_view.chat_scroll = 0;

        // Load tasks and restore input
        self.refresh_tasks(tasks);
        self.restore_input();

        // Focus a specific task if requested
        if let Some(name) = focus_task
            && let Some(pos) = self.task_list.tasks.iter().position(|t| &t.name == name)
        {
            self.task_list.list_state.select(Some(pos));
            self.task_list.show_detail = true;
            self.task_list.detail_scroll = 0;
        }
    }

    // ── Project list ────────────────────────────────────────────────

    /// Show the project list overlay, replacing the task list.
    pub fn show_project_list(&mut self, projects: Vec<Project>) {
        self.project_list.projects = projects;
        if !self.project_list.projects.is_empty() {
            self.project_list.list_state.select(Some(0));
        }
        self.project_list.show_projects = true;
        self.focus = Focus::ProjectList;
    }

    pub fn next_project(&mut self) {
        self.project_list.next_project();
    }

    pub fn previous_project(&mut self) {
        self.project_list.previous_project();
    }

    // ── Search ──────────────────────────────────────────────────────

    /// Enter search mode: clear the search input and focus the search bar.
    pub fn enter_search_mode(&mut self) {
        self.search_input.take();
        self.update_search_filter();
        self.focus = Focus::TaskSearch;
    }

    /// Exit search mode: clear filters, clamp selection, restore focus.
    pub fn exit_search(&mut self) {
        self.search_input.take();
        if self.project_list.show_projects {
            self.project_list.filtered_project_indices.clear();
            if !self.project_list.projects.is_empty() {
                let sel = self
                    .project_list
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.project_list.projects.len() - 1);
                self.project_list.list_state.select(Some(sel));
            }
            self.focus = Focus::ProjectList;
        } else {
            self.task_list.filtered_indices.clear();
            if !self.task_list.tasks.is_empty() {
                let sel = self
                    .task_list
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.task_list.tasks.len() - 1);
                self.task_list.list_state.select(Some(sel));
            }
            self.focus = Focus::TaskList;
        }
    }

    /// Confirm the current search selection. For tasks, opens the detail view.
    /// For projects, selects the project. Returns `true` if a selection was made.
    pub fn confirm_search_selection(&mut self) -> bool {
        if self.project_list.show_projects {
            if let Some(real_idx) = self.selected_filtered_project_index() {
                self.project_list.list_state.select(Some(real_idx));
            }
            self.search_input.take();
            self.project_list.filtered_project_indices.clear();
            self.focus = Focus::ProjectList;
            true
        } else {
            let selected = if let Some(real_idx) = self.selected_filtered_task_index() {
                self.open_task_detail(real_idx);
                true
            } else {
                self.focus = Focus::TaskList;
                false
            };
            self.search_input.take();
            self.task_list.filtered_indices.clear();
            selected
        }
    }

    /// Recompute `filtered_indices` based on `search_query`.
    /// Fuzzy match: each query char must appear in order (e.g. "res" matches "r.*e.*s.*").
    pub fn update_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        self.task_list.filtered_indices = self
            .task_list
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if query.is_empty() {
                    return true;
                }
                let name = t.name.as_str().to_lowercase();
                let mut qi = 0;
                for c in name.chars() {
                    if c == query[qi] {
                        qi += 1;
                        if qi == query.len() {
                            return true;
                        }
                    }
                }
                false
            })
            .map(|(i, _)| i)
            .collect();
        // Clamp selection to filtered range
        if self.task_list.filtered_indices.is_empty() {
            self.task_list.list_state.select(None);
        } else {
            let sel = self.task_list.list_state.selected().unwrap_or(0);
            if let Some(filtered_pos) = self
                .task_list
                .filtered_indices
                .iter()
                .position(|&i| i == sel)
            {
                self.task_list.list_state.select(Some(filtered_pos));
            } else {
                self.task_list.list_state.select(Some(0));
            }
        }
    }

    pub fn update_project_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        self.project_list.filtered_project_indices = self
            .project_list
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                if query.is_empty() {
                    return true;
                }
                let name = p.name.as_str().to_lowercase();
                let mut qi = 0;
                for c in name.chars() {
                    if c == query[qi] {
                        qi += 1;
                        if qi == query.len() {
                            return true;
                        }
                    }
                }
                false
            })
            .map(|(i, _)| i)
            .collect();
        if self.project_list.filtered_project_indices.is_empty() {
            self.project_list.list_state.select(None);
        } else {
            let sel = self.project_list.list_state.selected().unwrap_or(0);
            if let Some(pos) = self
                .project_list
                .filtered_project_indices
                .iter()
                .position(|&i| i == sel)
            {
                self.project_list.list_state.select(Some(pos));
            } else {
                self.project_list.list_state.select(Some(0));
            }
        }
    }

    pub fn search_next_project(&mut self) {
        self.project_list.search_next_project();
    }

    pub fn search_prev_project(&mut self) {
        self.project_list.search_prev_project();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{ProjectId, ProjectName, TaskId, TaskName};
    use crate::task::{Project, Task};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::Path;

    fn make_task(name: &str) -> Task {
        Task::new(
            TaskId::generate(),
            TaskName::from(name.to_string()),
            "engineer",
            &HashMap::new(),
            Path::new("/tmp"),
            None,
        )
    }

    fn make_project(name: &str) -> Project {
        Project {
            id: ProjectId::generate(),
            name: ProjectName::from(name.to_string()),
            description: String::new(),
            created_at: Utc::now(),
        }
    }

    fn state_with_tasks(n: usize) -> ScreenState {
        let tasks: Vec<Task> = (0..n).map(|i| make_task(&format!("task-{i}"))).collect();
        ScreenState::new(tasks)
    }

    // ── scroll_detail_up / scroll_detail_down ───────────────────────

    #[test]
    fn scroll_detail_down_increments() {
        let mut s = state_with_tasks(0);
        s.scroll_detail_down();
        assert_eq!(s.task_list.detail_scroll, 10);
        s.scroll_detail_down();
        assert_eq!(s.task_list.detail_scroll, 20);
    }

    #[test]
    fn scroll_detail_up_decrements() {
        let mut s = state_with_tasks(0);
        s.task_list.detail_scroll = 25;
        s.scroll_detail_up();
        assert_eq!(s.task_list.detail_scroll, 15);
    }

    #[test]
    fn scroll_detail_up_saturates_at_zero() {
        let mut s = state_with_tasks(0);
        s.task_list.detail_scroll = 5;
        s.scroll_detail_up();
        assert_eq!(s.task_list.detail_scroll, 0);
    }

    // ── open_task_detail ────────────────────────────────────────────

    #[test]
    fn open_task_detail_sets_expected_fields() {
        let mut s = state_with_tasks(3);
        s.task_list.show_detail = false;
        s.task_list.detail_scroll = 99;
        s.chat_view.chat_scroll = 42;
        s.focus = Focus::TaskList;

        s.open_task_detail(2);

        assert!(s.task_list.show_detail);
        assert_eq!(s.task_list.list_state.selected(), Some(2));
        assert_eq!(s.task_list.detail_scroll, 0);
        assert_eq!(s.chat_view.chat_scroll, 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── close_task_detail ───────────────────────────────────────────

    #[test]
    fn close_task_detail_hides_and_resets() {
        let mut s = state_with_tasks(2);
        s.task_list.show_detail = true;
        s.chat_view.chat_scroll = 10;
        s.focus = Focus::TaskList;

        s.close_task_detail();

        assert!(!s.task_list.show_detail);
        assert_eq!(s.chat_view.chat_scroll, 0);
        assert!(matches!(s.focus, Focus::ChatInput));
    }

    // ── navigate_to_adjacent_task ───────────────────────────────────

    #[test]
    fn navigate_forward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.task_list.show_detail = true;
        s.task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(true);

        assert!(result);
        assert_eq!(s.task_list.list_state.selected(), Some(1));
        assert!(s.task_list.show_detail);
        assert_eq!(s.task_list.detail_scroll, 0);
    }

    #[test]
    fn navigate_forward_past_end_hides_detail() {
        let mut s = state_with_tasks(2);
        s.task_list.show_detail = true;
        s.task_list.list_state.select(Some(1));

        let result = s.navigate_to_adjacent_task(true);

        assert!(!result);
        assert!(!s.task_list.show_detail);
    }

    #[test]
    fn navigate_backward_within_bounds() {
        let mut s = state_with_tasks(3);
        s.task_list.show_detail = true;
        s.task_list.list_state.select(Some(2));

        let result = s.navigate_to_adjacent_task(false);

        assert!(result);
        assert_eq!(s.task_list.list_state.selected(), Some(1));
        assert!(s.task_list.show_detail);
    }

    #[test]
    fn navigate_backward_past_start_hides_detail() {
        let mut s = state_with_tasks(2);
        s.task_list.show_detail = true;
        s.task_list.list_state.select(Some(0));

        let result = s.navigate_to_adjacent_task(false);

        assert!(!result);
        assert!(!s.task_list.show_detail);
    }

    // ── show_project_list ───────────────────────────────────────────

    #[test]
    fn show_project_list_sets_state() {
        let mut s = state_with_tasks(0);
        let projects = vec![make_project("alpha"), make_project("beta")];

        s.show_project_list(projects);

        assert!(s.project_list.show_projects);
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), Some(0));
        assert_eq!(s.project_list.projects.len(), 2);
    }

    #[test]
    fn show_project_list_empty() {
        let mut s = state_with_tasks(0);

        s.show_project_list(vec![]);

        assert!(s.project_list.show_projects);
        assert!(matches!(s.focus, Focus::ProjectList));
        assert_eq!(s.project_list.list_state.selected(), None);
    }

    // ── refresh_projects ────────────────────────────────────────────

    #[test]
    fn refresh_projects_clamps_selection() {
        let mut s = state_with_tasks(0);
        s.project_list.projects = vec![make_project("a"), make_project("b"), make_project("c")];
        s.project_list.list_state.select(Some(2));

        s.refresh_projects(vec![make_project("a"), make_project("b")]);

        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    #[test]
    fn refresh_projects_empty_clears_selection() {
        let mut s = state_with_tasks(0);
        s.project_list.list_state.select(Some(1));

        s.refresh_projects(vec![]);

        assert_eq!(s.project_list.list_state.selected(), None);
    }

    // ── enter_search_mode ───────────────────────────────────────────

    #[test]
    fn enter_search_mode_focuses_search() {
        let mut s = state_with_tasks(2);
        s.focus = Focus::TaskList;
        s.search_input.set("old query");

        s.enter_search_mode();

        assert!(matches!(s.focus, Focus::TaskSearch));
        assert!(s.search_input.buffer().is_empty());
    }

    // ── exit_search ─────────────────────────────────────────────────

    #[test]
    fn exit_search_tasks_restores_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.focus = Focus::TaskSearch;
        s.task_list.filtered_indices = vec![0, 2];
        s.task_list.list_state.select(Some(0));

        s.exit_search();

        assert!(matches!(s.focus, Focus::TaskList));
        assert!(s.task_list.filtered_indices.is_empty());
    }

    #[test]
    fn exit_search_projects_restores_project_list() {
        let mut s = state_with_tasks(0);
        s.project_list.show_projects = true;
        s.project_list.projects = vec![make_project("a"), make_project("b")];
        s.focus = Focus::TaskSearch;
        s.project_list.filtered_project_indices = vec![0, 1];
        s.project_list.list_state.select(Some(1));

        s.exit_search();

        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_project_indices.is_empty());
        assert_eq!(s.project_list.list_state.selected(), Some(1));
    }

    // ── confirm_search_selection ────────────────────────────────────

    #[test]
    fn confirm_search_selection_opens_task_detail() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.task_list.filtered_indices = vec![0, 2];
        s.task_list.list_state.select(Some(1)); // points to filtered_indices[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        assert!(s.task_list.show_detail);
        assert_eq!(s.task_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ChatInput));
        assert!(s.task_list.filtered_indices.is_empty());
    }

    #[test]
    fn confirm_search_selection_no_match_goes_to_task_list() {
        let mut s = state_with_tasks(3);
        s.project_list.show_projects = false;
        s.task_list.filtered_indices = vec![];
        s.task_list.list_state.select(None);

        let result = s.confirm_search_selection();

        assert!(!result);
        assert!(matches!(s.focus, Focus::TaskList));
    }

    #[test]
    fn confirm_search_selection_project() {
        let mut s = state_with_tasks(0);
        s.project_list.show_projects = true;
        s.project_list.projects = vec![make_project("a"), make_project("b"), make_project("c")];
        s.project_list.filtered_project_indices = vec![0, 2];
        s.project_list.list_state.select(Some(1)); // points to filtered[1] = 2

        let result = s.confirm_search_selection();

        assert!(result);
        assert_eq!(s.project_list.list_state.selected(), Some(2)); // real index
        assert!(matches!(s.focus, Focus::ProjectList));
        assert!(s.project_list.filtered_project_indices.is_empty());
    }
}
