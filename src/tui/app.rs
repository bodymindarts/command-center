use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::net::UnixStream;

use ratatui::widgets::ListState;

use crate::primitives::{TaskId, TaskStatus};
use crate::task::{Project, Task, TaskMessage};

pub enum Focus {
    TaskList,
    TaskSearch,
    ProjectList,
    ProjectNameInput,
    ChatInput,
    ChatHistory,
    SpawnInput,
    ConfirmDelete(TaskId),
    ConfirmDeleteProject(String),
    ConfirmCloseTask(TaskId),
    ConfirmCloseProject,
}

pub struct ActivePermission {
    pub stream: UnixStream,
    pub task_name: String,
    pub tool_name: String,
    pub tool_input_summary: String,
    pub permission_suggestions: Vec<serde_json::Value>,
}

/// Saved UI state for a project, restored on Ctrl+R.
pub struct SavedProjectState {
    pub name: String,
    pub id: String,
    pub show_detail: bool,
    pub selected_task_name: Option<String>,
}

pub struct InputState {
    chars: Vec<char>,
    pub cursor: usize,
    /// Stores multi-line pasted content. When set, the input widget shows
    /// "[N lines pasted]" instead of the raw text.
    pasted: Option<String>,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
            pasted: None,
        }
    }

    pub fn buffer(&self) -> String {
        if let Some(ref p) = self.pasted {
            p.clone()
        } else {
            self.chars.iter().collect()
        }
    }

    pub fn is_empty(&self) -> bool {
        if self.pasted.is_some() {
            false
        } else {
            self.chars.is_empty()
        }
    }

    pub fn has_paste(&self) -> bool {
        self.pasted.is_some()
    }

    pub fn paste_line_count(&self) -> usize {
        self.pasted.as_ref().map(|p| p.lines().count()).unwrap_or(0)
    }

    /// Store a multi-line paste, replacing any current input.
    pub fn set_paste(&mut self, text: String) {
        self.chars.clear();
        self.cursor = 0;
        self.pasted = Some(text);
    }

    /// Transfer pasted content into the char buffer for normal editing.
    /// After this call, `pasted` is `None` and `chars` + `cursor` reflect
    /// the full content with the cursor at the end.
    fn materialize_paste(&mut self) {
        if let Some(p) = self.pasted.take() {
            self.chars = p.chars().collect();
            self.cursor = self.chars.len();
        }
    }

    pub fn insert(&mut self, c: char) {
        self.materialize_paste();
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        self.materialize_paste();
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        self.materialize_paste();
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.materialize_paste();
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn right(&mut self) {
        self.materialize_paste();
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.materialize_paste();
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.materialize_paste();
        self.cursor = self.chars.len();
    }

    pub fn kill_line(&mut self) {
        self.materialize_paste();
        self.chars.truncate(self.cursor);
    }

    pub fn kill_before(&mut self) {
        self.materialize_paste();
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    pub fn kill_word(&mut self) {
        self.materialize_paste();
        if self.cursor == 0 {
            return;
        }
        let mut i = self.cursor;
        // Skip whitespace
        while i > 0 && self.chars[i - 1] == ' ' {
            i -= 1;
        }
        // Skip word chars
        while i > 0 && self.chars[i - 1] != ' ' {
            i -= 1;
        }
        self.chars.drain(i..self.cursor);
        self.cursor = i;
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        if let Some(p) = self.pasted.take() {
            self.chars.clear();
            p
        } else {
            let result: String = self.chars.iter().collect();
            self.chars.clear();
            result
        }
    }

    pub fn set(&mut self, text: &str) {
        self.pasted = None;
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }
}

pub struct App {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    pub should_quit: bool,
    pub focus: Focus,
    pub input: InputState,
    pub show_detail: bool,
    pub pending_permissions: HashMap<String, VecDeque<ActivePermission>>,
    pub selected_messages: Vec<TaskMessage>,
    pub detail_scroll: u16,
    pub detail_live_output: Option<String>,
    pub window_numbers: HashMap<String, String>,
    pub chat_buffers: HashMap<String, String>,
    pub chat_scroll: u16,
    pub chat_viewport_height: u16,
    /// Task IDs that recently transitioned from Running to Completed/Failed.
    /// Cleared when the user views the task detail.
    pub fresh_tasks: HashSet<String>,
    /// Transient error message shown in the prompt bar. Cleared on next keypress.
    pub status_error: Option<String>,
    /// Currently active project name (for display). None = default (ExO).
    pub active_project: Option<String>,
    /// Currently active project ID (for queries). None = default (ExO).
    pub active_project_id: Option<String>,
    /// Last active project state — remembered when Ctrl+O leaves a project.
    pub last_project: Option<SavedProjectState>,
    /// Cached PM messages for the active project.
    pub pm_messages: Vec<TaskMessage>,
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
    pub global_task_projects: HashMap<String, Option<String>>,
    /// Global list of (task_name, work_dir) for all running tasks.
    /// Used for CWD→task matching in permission/resolved/idle handlers
    /// so lookups work regardless of which project is currently displayed.
    pub global_task_work_dirs: Vec<(String, String)>,
}

impl App {
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
            pending_permissions: HashMap::new(),
            selected_messages: Vec::new(),
            detail_scroll: 0,
            detail_live_output: None,
            window_numbers: HashMap::new(),
            chat_buffers: HashMap::new(),
            chat_scroll: 0,
            chat_viewport_height: 0,
            fresh_tasks: HashSet::new(),
            status_error: None,
            active_project: None,
            active_project_id: None,
            last_project: None,
            pm_messages: Vec::new(),
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

        // Detect tasks that transitioned from Running to Completed/Failed
        for new_task in &tasks {
            if matches!(new_task.status, TaskStatus::Completed | TaskStatus::Failed) {
                let was_running = self
                    .tasks
                    .iter()
                    .find(|old| old.id == new_task.id)
                    .is_some_and(|old| old.status == TaskStatus::Running);
                if was_running {
                    self.fresh_tasks.insert(new_task.id.as_str().to_string());
                }
            }
        }

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

    /// Remove a task from the fresh set (user has acknowledged it).
    pub fn acknowledge_fresh(&mut self, task_id: &str) {
        self.fresh_tasks.remove(task_id);
    }

    pub fn add_permission(&mut self, perm: ActivePermission) {
        self.pending_permissions
            .entry(perm.task_name.clone())
            .or_default()
            .push_back(perm);
    }

    pub fn take_permission(&mut self, name: &str) -> Option<ActivePermission> {
        let queue = self.pending_permissions.get_mut(name)?;
        let perm = queue.pop_front();
        if queue.is_empty() {
            self.pending_permissions.remove(name);
        }
        perm
    }

    pub fn peek_permission(&self, name: &str) -> Option<&ActivePermission> {
        self.pending_permissions.get(name)?.front()
    }

    pub fn tasks_with_permissions(&self) -> Vec<String> {
        let mut names: Vec<String> = self.pending_permissions.keys().cloned().collect();
        names.sort();
        names
    }

    /// Count pending permissions only for tasks in the current project.
    /// "exo" key belongs to the default (no-project) scope.
    pub fn current_project_perm_count(&self) -> usize {
        let current_pid = self.active_project_id.as_deref();
        self.pending_permissions
            .iter()
            .filter(|(task_name, _)| {
                if task_name.as_str() == "exo" {
                    current_pid.is_none()
                } else {
                    let task_pid = self
                        .global_task_projects
                        .get(task_name.as_str())
                        .and_then(|pid| pid.as_deref());
                    task_pid == current_pid
                }
            })
            .map(|(_, queue)| queue.len())
            .sum()
    }

    /// Count pending permissions for tasks NOT in the current project.
    /// Returns a vec of (project_name_or_default, count) for display.
    /// Uses `global_task_projects` (updated every tick) so lookups work
    /// even when `self.tasks` is scoped to a single project.
    pub fn other_project_perm_counts(&self) -> Vec<(String, usize)> {
        let current_pid = self.active_project_id.as_deref();
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for (task_name, queue) in &self.pending_permissions {
            let task_pid = self
                .global_task_projects
                .get(task_name)
                .and_then(|pid| pid.as_deref());
            if task_pid != current_pid {
                let label = if let Some(pid) = task_pid {
                    self.projects
                        .iter()
                        .find(|p| p.id == pid)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| "?".to_string())
                } else {
                    "default".to_string()
                };
                *counts.entry(label).or_default() += queue.len();
            }
        }
        counts.into_iter().collect()
    }

    /// Remove and return all permissions for task names that don't correspond
    /// to any globally running task. The "exo" key is always preserved.
    /// `all_running_names` must contain names of ALL running tasks across all
    /// projects, not just the currently displayed ones.
    pub fn drain_stale_permissions(
        &mut self,
        all_running_names: &HashSet<String>,
    ) -> Vec<ActivePermission> {
        let stale_keys: Vec<String> = self
            .pending_permissions
            .keys()
            .filter(|k| *k != "exo" && !all_running_names.contains(k.as_str()))
            .cloned()
            .collect();

        let mut stale = Vec::new();
        for key in stale_keys {
            if let Some(queue) = self.pending_permissions.remove(&key) {
                stale.extend(queue);
            }
        }
        stale
    }

    fn current_chat_key(&self) -> String {
        if self.show_detail {
            self.selected_task()
                .map(|t| t.id.as_str().to_string())
                .unwrap_or_else(|| "exo".to_string())
        } else if let Some(ref pid) = self.active_project_id {
            format!("pm:{pid}")
        } else {
            "exo".to_string()
        }
    }

    pub fn save_current_input(&mut self) {
        let key = self.current_chat_key();
        let text = self.input.buffer();
        if text.is_empty() {
            self.chat_buffers.remove(&key);
        } else {
            self.chat_buffers.insert(key, text);
        }
    }

    pub fn restore_input(&mut self) {
        let key = self.current_chat_key();
        let text = self.chat_buffers.get(&key).cloned().unwrap_or_default();
        self.input.take();
        self.input.set(&text);
    }

    /// Returns the permission key for the currently visible pane.
    /// Task name if viewing a task's detail, "exo" otherwise.
    pub fn focused_perm_key(&self) -> String {
        if self.show_detail {
            self.selected_task()
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "exo".to_string())
        } else {
            "exo".to_string()
        }
    }

    /// Returns the permission key to display in the overlay and act on
    /// with global keybindings. Prefers the focused task's key; falls
    /// back to any task with pending permissions.
    pub fn active_permission_key(&self) -> Option<String> {
        let focused = self.focused_perm_key();
        if self.peek_permission(&focused).is_some() {
            return Some(focused);
        }
        self.tasks_with_permissions().into_iter().next()
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
                let name = t.name.to_lowercase();
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
                let name = p.name.to_lowercase();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_char_insert() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn insert_at_cursor() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('c');
        input.left();
        input.insert('b');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn paste_then_insert_preserves_content() {
        let mut input = InputState::new();
        input.set_paste("line1\nline2\nline3".to_string());
        assert!(input.has_paste());
        assert_eq!(input.paste_line_count(), 3);

        // Typing a char should append to the pasted content, not replace it
        input.insert('!');
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "line1\nline2\nline3!");
    }

    #[test]
    fn paste_then_backspace_removes_last_char() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        input.backspace();
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "hello\nworl");
    }

    #[test]
    fn paste_then_delete_at_end_is_noop() {
        let mut input = InputState::new();
        input.set_paste("hello".to_string());
        input.delete();
        assert_eq!(input.buffer(), "hello");
    }

    #[test]
    fn paste_then_home_delete_removes_first_char() {
        let mut input = InputState::new();
        input.set_paste("hello".to_string());
        input.home();
        assert_eq!(input.cursor, 0);
        input.delete();
        assert_eq!(input.buffer(), "ello");
    }

    #[test]
    fn paste_then_cursor_movement() {
        let mut input = InputState::new();
        input.set_paste("abc".to_string());
        // After materialize, cursor should be at end (3)
        input.left();
        assert_eq!(input.cursor, 2);
        input.left();
        assert_eq!(input.cursor, 1);
        input.right();
        assert_eq!(input.cursor, 2);
        input.home();
        assert_eq!(input.cursor, 0);
        input.end();
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn paste_then_kill_line() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Materialize puts cursor at end; kill_line truncates at cursor (noop)
        input.kill_line();
        assert_eq!(input.buffer(), "hello\nworld");

        // Now move cursor and kill
        input.home();
        input.kill_line();
        assert_eq!(input.buffer(), "");
    }

    #[test]
    fn paste_then_kill_before() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor at end after materialize; kill_before clears everything
        input.kill_before();
        assert_eq!(input.buffer(), "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn paste_then_kill_word() {
        let mut input = InputState::new();
        input.set_paste("hello world".to_string());
        input.kill_word();
        assert_eq!(input.buffer(), "hello ");
    }

    #[test]
    fn paste_take_returns_pasted_content() {
        let mut input = InputState::new();
        input.set_paste("pasted\ntext".to_string());
        let taken = input.take();
        assert_eq!(taken, "pasted\ntext");
        assert!(input.is_empty());
    }

    #[test]
    fn paste_buffer_returns_pasted_content() {
        let mut input = InputState::new();
        input.set_paste("multi\nline".to_string());
        assert_eq!(input.buffer(), "multi\nline");
    }

    #[test]
    fn set_after_paste_replaces() {
        let mut input = InputState::new();
        input.set_paste("pasted".to_string());
        input.set("replaced");
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "replaced");
    }

    #[test]
    fn multiple_inserts_after_paste() {
        let mut input = InputState::new();
        input.set_paste("base".to_string());
        input.insert('!');
        input.insert('!');
        input.insert('!');
        assert_eq!(input.buffer(), "base!!!");
    }

    #[test]
    fn single_line_paste_chars_then_type() {
        // Simulates single-line paste (inserted char-by-char in event loop)
        let mut input = InputState::new();
        for c in "pasted".chars() {
            input.insert(c);
        }
        assert_eq!(input.buffer(), "pasted");
        input.insert('!');
        assert_eq!(input.buffer(), "pasted!");
    }

    #[test]
    fn backspace_on_empty_after_paste_clear() {
        let mut input = InputState::new();
        input.set_paste("x".to_string());
        input.backspace(); // materializes "x", cursor=1, then removes 'x'
        assert!(input.is_empty());
        // Another backspace should be safe
        input.backspace();
        assert!(input.is_empty());
    }
}
