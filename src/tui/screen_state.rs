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

pub struct ScreenState {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    pub should_quit: bool,
    pub focus: Focus,
    pub input: InputState,
    pub show_detail: bool,
    pub permissions: PermissionStore,
    pub selected_messages: Vec<TaskMessage>,
    pub detail_scroll: u16,
    pub detail_live_output: Option<String>,
    pub window_numbers: HashMap<WindowId, String>,
    pub chat_buffers: HashMap<ChatId, String>,
    pub chat_scroll: u16,
    pub chat_viewport_height: u16,
    /// Pane IDs that appear idle (shell prompt visible), refreshed periodically.
    pub idle_panes: HashSet<PaneId>,
    /// Transient error message shown in the prompt bar. Cleared on next keypress.
    pub status_error: Option<String>,
    /// Currently active project name (for display). None = default (ExO).
    pub active_project: Option<ProjectName>,
    /// Currently active project ID (for queries). None = default (ExO).
    pub active_project_id: Option<ProjectId>,
    /// Last active project state — remembered when Ctrl+O leaves a project.
    pub last_project: Option<SavedProjectState>,
    /// ExO assistant chat state (messages, streaming flag).
    pub exo_chat: AssistantChat,
    /// Per-project PM assistant chat states.
    pub project_chats: HashMap<ProjectId, AssistantChat>,
    /// Whether the right panel shows the project list instead of the task list.
    pub show_projects: bool,
    /// Cached list of projects for rendering.
    pub projects: Vec<Project>,
    /// Selection state for the project list.
    pub project_list_state: ListState,
    /// Input state for the task search filter.
    pub search_input: InputState,
    /// Indices into `tasks` that match the current search query.
    pub filtered_indices: Vec<usize>,
    /// Indices into `projects` that match the current search query.
    pub filtered_project_indices: Vec<usize>,
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
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            tasks,
            list_state,
            should_quit: false,
            focus: Focus::ChatInput,
            input: InputState::new(),
            show_detail: false,
            permissions: PermissionStore::new(),
            selected_messages: Vec::new(),
            detail_scroll: 0,
            detail_live_output: None,
            window_numbers: HashMap::new(),
            chat_buffers: HashMap::new(),
            chat_scroll: 0,
            chat_viewport_height: 0,
            idle_panes: HashSet::new(),
            status_error: None,
            active_project: None,
            active_project_id: None,
            last_project: None,
            exo_chat: AssistantChat::new(),
            project_chats: HashMap::new(),
            show_projects: false,
            projects: Vec::new(),
            project_list_state: ListState::default(),
            search_input: InputState::new(),
            filtered_indices: Vec::new(),
            filtered_project_indices: Vec::new(),
            global_task_projects: HashMap::new(),
            global_task_work_dirs: Vec::new(),
        }
    }

    pub fn current_focus(&self) -> &Focus {
        &self.focus
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

    pub fn navigate_focus_down(&mut self) {
        self.focus = Focus::ChatInput;
    }

    pub fn navigate_focus_right(&mut self) {
        self.focus = Focus::TaskList;
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
                // Selection changed — reset scroll
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

    /// Count pending permissions only for tasks in the current project.
    pub fn current_project_perm_count(&self) -> usize {
        self.permissions
            .count_for_project(self.active_project_id.as_ref(), &self.global_task_projects)
    }

    /// Count pending permissions for tasks NOT in the current project.
    pub fn other_project_perm_counts(&self) -> Vec<(String, usize)> {
        self.permissions.other_project_counts(
            self.active_project_id.as_ref(),
            &self.global_task_projects,
            &self.projects,
        )
    }

    /// Count pending AskUser permissions in the current project.
    pub fn current_project_askuser_count(&self) -> usize {
        self.permissions
            .askuser_count_for_project(self.active_project_id.as_ref(), &self.global_task_projects)
    }

    fn current_chat_id(&self) -> ChatId {
        if self.show_detail {
            self.selected_task()
                .map(|t| ChatId::Task(t.id.clone()))
                .unwrap_or(ChatId::Exo)
        } else if let Some(ref pid) = self.active_project_id {
            ChatId::Project(pid.clone())
        } else {
            ChatId::Exo
        }
    }

    pub fn save_current_input(&mut self) {
        let chat_id = self.current_chat_id();
        let text = self.input.buffer();
        if text.is_empty() {
            self.chat_buffers.remove(&chat_id);
        } else {
            self.chat_buffers.insert(chat_id, text);
        }
    }

    pub fn restore_input(&mut self) {
        let chat_id = self.current_chat_id();
        let text = self.chat_buffers.get(&chat_id).cloned().unwrap_or_default();
        self.input.take();
        self.input.set(&text);
    }

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
        if let (Some(name), Some(id)) = (self.active_project.take(), self.active_project_id.take())
        {
            self.last_project = Some(SavedProjectState {
                name,
                id,
                show_detail: self.show_detail,
                selected_task_name,
            });
        }

        // Switch context
        self.save_current_input();
        self.active_project = project.as_ref().map(|(n, _)| n.clone());
        self.active_project_id = project.map(|(_, id)| id);
        self.show_projects = false;
        self.show_detail = false;
        self.focus = Focus::ChatInput;
        self.chat_scroll = 0;

        // Load tasks and restore input
        self.refresh_tasks(tasks);
        self.restore_input();

        // Focus a specific task if requested
        if let Some(name) = focus_task
            && let Some(pos) = self.tasks.iter().position(|t| &t.name == name)
        {
            self.list_state.select(Some(pos));
            self.show_detail = true;
            self.detail_scroll = 0;
        }
    }

    /// Returns the permission key for the currently visible pane.
    /// Task name if viewing a task's detail, "exo" otherwise.
    pub fn focused_perm_key(&self) -> TaskName {
        if self.show_detail {
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

    pub fn next_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.project_list_state.selected() {
            Some(i) => (i + 1) % self.projects.len(),
            None => 0,
        };
        self.project_list_state.select(Some(i));
    }

    pub fn previous_project(&mut self) {
        if self.projects.is_empty() {
            return;
        }
        let i = match self.project_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.projects.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.project_list_state.select(Some(i));
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.project_list_state
            .selected()
            .and_then(|i| self.projects.get(i))
    }

    /// Recompute `filtered_indices` based on `search_query`.
    /// Fuzzy match: each query char must appear in order (e.g. "res" matches "r.*e.*s.*").
    pub fn update_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        self.filtered_indices = self
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
        if self.filtered_indices.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self.list_state.selected().unwrap_or(0);
            if let Some(filtered_pos) = self.filtered_indices.iter().position(|&i| i == sel) {
                self.list_state.select(Some(filtered_pos));
            } else {
                self.list_state.select(Some(0));
            }
        }
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

    pub fn update_project_search_filter(&mut self) {
        let query: Vec<char> = self.search_input.buffer().to_lowercase().chars().collect();
        self.filtered_project_indices = self
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
        if self.filtered_project_indices.is_empty() {
            self.project_list_state.select(None);
        } else {
            let sel = self.project_list_state.selected().unwrap_or(0);
            if let Some(pos) = self.filtered_project_indices.iter().position(|&i| i == sel) {
                self.project_list_state.select(Some(pos));
            } else {
                self.project_list_state.select(Some(0));
            }
        }
    }

    pub fn search_next_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.project_list_state.selected() {
            Some(i) => (i + 1) % self.filtered_project_indices.len(),
            None => 0,
        };
        self.project_list_state.select(Some(i));
    }

    pub fn search_prev_project(&mut self) {
        if self.filtered_project_indices.is_empty() {
            return;
        }
        let i = match self.project_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_project_indices.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.project_list_state.select(Some(i));
    }

    pub fn selected_filtered_project_index(&self) -> Option<usize> {
        self.project_list_state
            .selected()
            .and_then(|i| self.filtered_project_indices.get(i).copied())
    }

    /// Resolve the currently selected filtered index back to the real task index.
    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_indices.get(i).copied())
    }
}
