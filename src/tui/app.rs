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

enum Segment {
    Typed(Vec<char>),
    Pasted(String),
}

impl Segment {
    fn display_len(&self) -> usize {
        match self {
            Segment::Typed(chars) => chars.len(),
            Segment::Pasted(content) => paste_summary(content).len(),
        }
    }
}

fn paste_summary(content: &str) -> String {
    // Normalize \r\n and bare \r to \n, then count lines
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let n = normalized.lines().count().max(1);
    format!("[{n} lines pasted]")
}

pub struct InputState {
    /// Alternating sequence: always starts and ends with `Typed`, with
    /// `Pasted` segments interleaved: `[T, (P, T)*]`.
    segments: Vec<Segment>,
    /// Index into `segments` — always points to a `Typed` variant.
    cursor_seg: usize,
    /// Character offset within the current `Typed` segment.
    cursor_off: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            segments: vec![Segment::Typed(Vec::new())],
            cursor_seg: 0,
            cursor_off: 0,
        }
    }

    /// Returns the full content for submission, expanding pasted blocks.
    pub fn buffer(&self) -> String {
        self.segments
            .iter()
            .map(|seg| match seg {
                Segment::Typed(chars) => chars.iter().collect::<String>(),
                Segment::Pasted(content) => content.clone(),
            })
            .collect()
    }

    /// Returns the display representation: typed text shown verbatim,
    /// paste blocks shown as `[N lines pasted]` summaries.
    pub fn display_text(&self) -> String {
        self.segments
            .iter()
            .map(|seg| match seg {
                Segment::Typed(chars) => chars.iter().collect::<String>(),
                Segment::Pasted(content) => paste_summary(content),
            })
            .collect()
    }

    /// Cursor position within the display string returned by [`display_text`].
    pub fn display_cursor(&self) -> usize {
        let mut pos = 0;
        for (i, seg) in self.segments.iter().enumerate() {
            if i == self.cursor_seg {
                return pos + self.cursor_off;
            }
            pos += seg.display_len();
        }
        pos + self.cursor_off
    }

    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(|seg| match seg {
            Segment::Typed(chars) => chars.is_empty(),
            Segment::Pasted(_) => false,
        })
    }

    #[cfg(test)]
    fn has_paste(&self) -> bool {
        self.segments
            .iter()
            .any(|seg| matches!(seg, Segment::Pasted(_)))
    }

    /// Insert a multi-line paste at the current cursor position.
    /// Splits the current `Typed` segment and inserts a `Pasted` block between
    /// the two halves.  The cursor moves to the start of the second half so
    /// subsequent typing appears *after* the paste summary.
    pub fn set_paste(&mut self, text: String) {
        // Normalize line endings: \r\n → \n, bare \r → \n
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (before, after) = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => (
                chars[..self.cursor_off].to_vec(),
                chars[self.cursor_off..].to_vec(),
            ),
            Segment::Pasted(_) => (vec![], vec![]),
        };
        self.segments[self.cursor_seg] = Segment::Typed(before);
        let paste_idx = self.cursor_seg + 1;
        self.segments.insert(paste_idx, Segment::Pasted(text));
        self.segments.insert(paste_idx + 1, Segment::Typed(after));
        self.cursor_seg = paste_idx + 1;
        self.cursor_off = 0;
    }

    pub fn insert(&mut self, c: char) {
        if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
            chars.insert(self.cursor_off, c);
            self.cursor_off += 1;
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor_off > 0 {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                self.cursor_off -= 1;
                chars.remove(self.cursor_off);
            }
        } else if self.cursor_seg >= 2 {
            // At start of Typed preceded by Pasted — delete the paste block.
            let prev_typed_idx = self.cursor_seg - 2;
            let prev_len = match &self.segments[prev_typed_idx] {
                Segment::Typed(chars) => chars.len(),
                _ => 0,
            };
            let current_chars = match &self.segments[self.cursor_seg] {
                Segment::Typed(chars) => chars.clone(),
                _ => vec![],
            };
            self.segments.remove(self.cursor_seg);
            self.segments.remove(self.cursor_seg - 1);
            if let Segment::Typed(ref mut chars) = self.segments[prev_typed_idx] {
                chars.extend(current_chars);
            }
            self.cursor_seg = prev_typed_idx;
            self.cursor_off = prev_len;
        }
    }

    pub fn delete(&mut self) {
        let at_end = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => self.cursor_off >= chars.len(),
            _ => true,
        };
        if !at_end {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                chars.remove(self.cursor_off);
            }
        } else if self.cursor_seg + 2 < self.segments.len() {
            // At end of Typed followed by Pasted — delete the paste block.
            let next_typed_chars = match &self.segments[self.cursor_seg + 2] {
                Segment::Typed(chars) => chars.clone(),
                _ => vec![],
            };
            self.segments.remove(self.cursor_seg + 2);
            self.segments.remove(self.cursor_seg + 1);
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                chars.extend(next_typed_chars);
            }
        }
    }

    pub fn left(&mut self) {
        if self.cursor_off > 0 {
            self.cursor_off -= 1;
        } else if self.cursor_seg >= 2 {
            self.cursor_seg -= 2;
            self.cursor_off = match &self.segments[self.cursor_seg] {
                Segment::Typed(chars) => chars.len(),
                _ => 0,
            };
        }
    }

    pub fn right(&mut self) {
        let at_end = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => self.cursor_off >= chars.len(),
            _ => true,
        };
        if !at_end {
            self.cursor_off += 1;
        } else if self.cursor_seg + 2 < self.segments.len() {
            self.cursor_seg += 2;
            self.cursor_off = 0;
        }
    }

    pub fn home(&mut self) {
        self.cursor_seg = 0;
        self.cursor_off = 0;
    }

    pub fn end(&mut self) {
        self.cursor_seg = self.segments.len() - 1;
        self.cursor_off = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => chars.len(),
            _ => 0,
        };
    }

    pub fn kill_line(&mut self) {
        if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
            chars.truncate(self.cursor_off);
        }
        self.segments.truncate(self.cursor_seg + 1);
    }

    pub fn kill_before(&mut self) {
        let remaining = match &self.segments[self.cursor_seg] {
            Segment::Typed(chars) => chars[self.cursor_off..].to_vec(),
            _ => vec![],
        };
        let after: Vec<Segment> = self.segments.drain(self.cursor_seg + 1..).collect();
        self.segments.clear();
        self.segments.push(Segment::Typed(remaining));
        self.segments.extend(after);
        self.cursor_seg = 0;
        self.cursor_off = 0;
    }

    pub fn kill_word(&mut self) {
        if self.cursor_off > 0 {
            if let Segment::Typed(ref mut chars) = self.segments[self.cursor_seg] {
                let mut i = self.cursor_off;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                chars.drain(i..self.cursor_off);
                self.cursor_off = i;
            }
        } else if self.cursor_seg >= 2 {
            // Delete preceding paste block (treat it as one "word").
            self.backspace();
        }
    }

    pub fn take(&mut self) -> String {
        let result = self.buffer();
        self.segments = vec![Segment::Typed(Vec::new())];
        self.cursor_seg = 0;
        self.cursor_off = 0;
        result
    }

    pub fn set(&mut self, text: &str) {
        self.segments = vec![Segment::Typed(text.chars().collect())];
        self.cursor_seg = 0;
        self.cursor_off = text.chars().count();
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

    /// Save the current project's UI state so it can be restored later (e.g. Ctrl+R).
    /// Takes ownership of `active_project` and `active_project_id`, leaving them `None`.
    pub fn save_project_state(&mut self) {
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

    // ── basic typing ──────────────────────────────────────────────

    #[test]
    fn single_char_insert() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('b');
        input.insert('c');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.display_cursor(), 3);
    }

    #[test]
    fn insert_at_cursor() {
        let mut input = InputState::new();
        input.insert('a');
        input.insert('c');
        input.left();
        input.insert('b');
        assert_eq!(input.buffer(), "abc");
        assert_eq!(input.display_cursor(), 2);
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

    // ── paste inserts at cursor, doesn't replace ──────────────────

    #[test]
    fn paste_inserts_at_cursor_position() {
        let mut input = InputState::new();
        for c in "hello world".chars() {
            input.insert(c);
        }
        // Move cursor back to after "hello "
        for _ in 0..5 {
            input.left();
        }
        input.set_paste("fn foo() {\n    bar();\n}".to_string());
        // Typed text before and after paste is preserved
        assert_eq!(input.buffer(), "hello fn foo() {\n    bar();\n}world");
        assert_eq!(input.display_text(), "hello [3 lines pasted]world");
    }

    #[test]
    fn paste_at_beginning_preserves_typed() {
        let mut input = InputState::new();
        for c in "suffix".chars() {
            input.insert(c);
        }
        input.home();
        input.set_paste("a\nb".to_string());
        assert_eq!(input.buffer(), "a\nbsuffix");
        assert_eq!(input.display_text(), "[2 lines pasted]suffix");
    }

    #[test]
    fn paste_at_end_preserves_typed() {
        let mut input = InputState::new();
        for c in "prefix ".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        assert_eq!(input.buffer(), "prefix a\nb");
        assert_eq!(input.display_text(), "prefix [2 lines pasted]");
    }

    // ── typing after paste keeps summary stable ───────────────────

    #[test]
    fn typing_after_paste_keeps_summary() {
        let mut input = InputState::new();
        input.set_paste("line1\nline2\nline3".to_string());
        assert!(input.has_paste());

        // Typing appends *after* the paste summary — summary stays
        input.insert('!');
        assert!(input.has_paste());
        assert_eq!(input.buffer(), "line1\nline2\nline3!");
        assert_eq!(input.display_text(), "[3 lines pasted]!");
    }

    #[test]
    fn multiple_inserts_after_paste() {
        let mut input = InputState::new();
        input.set_paste("base\nline".to_string());
        input.insert('!');
        input.insert('!');
        input.insert('!');
        assert_eq!(input.buffer(), "base\nline!!!");
        assert_eq!(input.display_text(), "[2 lines pasted]!!!");
    }

    // ── backspace / delete at paste boundary ──────────────────────

    #[test]
    fn backspace_at_paste_boundary_removes_paste_block() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor is right after paste; backspace deletes the whole block
        input.backspace();
        assert!(!input.has_paste());
        assert!(input.is_empty());
    }

    #[test]
    fn backspace_after_paste_with_typed_preserves_typed() {
        let mut input = InputState::new();
        for c in "before ".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        // Backspace deletes the paste block, not "before "
        input.backspace();
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "before ");
    }

    #[test]
    fn delete_at_paste_boundary_removes_paste_block() {
        let mut input = InputState::new();
        for c in "before".chars() {
            input.insert(c);
        }
        input.set_paste("a\nb".to_string());
        for c in " after".chars() {
            input.insert(c);
        }
        // Move cursor to end of "before" (just before paste)
        input.home();
        input.right(); // b
        input.right(); // e
        input.right(); // f
        input.right(); // o
        input.right(); // r
        input.right(); // e — end of first Typed, still in seg 0
        // Now delete should remove the paste block
        input.delete();
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "before after");
    }

    #[test]
    fn backspace_on_empty_after_paste_clear() {
        let mut input = InputState::new();
        input.set_paste("x\ny".to_string());
        input.backspace(); // removes the paste block
        assert!(input.is_empty());
        // Another backspace is safe
        input.backspace();
        assert!(input.is_empty());
    }

    #[test]
    fn paste_then_delete_at_end_is_noop() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor after paste, no following content — delete is noop
        input.delete();
        assert_eq!(input.buffer(), "hello\nworld");
    }

    // ── cursor movement skips paste blocks ────────────────────────

    #[test]
    fn cursor_skips_paste_on_left_right() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Display: "ab[2 lines pasted]cd"
        let summary_len = "[2 lines pasted]".len();
        assert_eq!(input.display_cursor(), 2 + summary_len + 2); // end

        // Move left twice into "cd"
        input.left();
        assert_eq!(input.display_cursor(), 2 + summary_len + 1);
        input.left();
        assert_eq!(input.display_cursor(), 2 + summary_len);

        // Next left skips the paste block → end of "ab"
        input.left();
        assert_eq!(input.display_cursor(), 2);

        // Right skips paste → start of "cd"
        input.right();
        assert_eq!(input.display_cursor(), 2 + summary_len);
    }

    #[test]
    fn home_and_end_span_all_segments() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        let total = input.display_text().len();

        input.home();
        assert_eq!(input.display_cursor(), 0);
        input.end();
        assert_eq!(input.display_cursor(), total);
    }

    // ── kill operations ───────────────────────────────────────────

    #[test]
    fn kill_line_from_before_paste() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Move home then right once → cursor at 'b'
        input.home();
        input.right();
        // Kill line: keep "a", remove rest including paste
        input.kill_line();
        assert_eq!(input.buffer(), "a");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_line_from_end_is_noop() {
        let mut input = InputState::new();
        input.set_paste("hello\nworld".to_string());
        // Cursor at end (after paste, in empty Typed)
        input.kill_line();
        assert_eq!(input.buffer(), "hello\nworld");
    }

    #[test]
    fn kill_before_from_after_paste() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Cursor at end of "cd". Kill before removes "ab" + paste + "cd"
        input.kill_before();
        assert_eq!(input.buffer(), "");
        assert_eq!(input.display_cursor(), 0);
    }

    #[test]
    fn kill_before_preserves_segments_after_cursor() {
        let mut input = InputState::new();
        for c in "ab".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        for c in "cd".chars() {
            input.insert(c);
        }
        // Move to start of "cd" (just after paste)
        input.left();
        input.left();
        // Kill before: removes "ab" and paste, keeps "cd"
        input.kill_before();
        assert_eq!(input.buffer(), "cd");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_word_at_paste_boundary() {
        let mut input = InputState::new();
        for c in "hello ".chars() {
            input.insert(c);
        }
        input.set_paste("x\ny".to_string());
        // Cursor right after paste, in empty Typed. kill_word deletes paste.
        input.kill_word();
        assert_eq!(input.buffer(), "hello ");
        assert!(!input.has_paste());
    }

    #[test]
    fn kill_word_within_typed_after_paste() {
        let mut input = InputState::new();
        input.set_paste("x\ny".to_string());
        for c in "hello world".chars() {
            input.insert(c);
        }
        // Cursor at end of "hello world" typed segment
        input.kill_word();
        assert_eq!(input.buffer(), "x\nyhello ");
        assert_eq!(input.display_text(), "[2 lines pasted]hello ");
    }

    // ── take / set / buffer ───────────────────────────────────────

    #[test]
    fn take_returns_expanded_content() {
        let mut input = InputState::new();
        for c in "before ".chars() {
            input.insert(c);
        }
        input.set_paste("pasted\ntext".to_string());
        for c in " after".chars() {
            input.insert(c);
        }
        let taken = input.take();
        assert_eq!(taken, "before pasted\ntext after");
        assert!(input.is_empty());
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
        input.set_paste("pasted\nstuff".to_string());
        input.set("replaced");
        assert!(!input.has_paste());
        assert_eq!(input.buffer(), "replaced");
    }

    // ── multiple pastes ───────────────────────────────────────────

    #[test]
    fn multiple_pastes_both_shown_as_summaries() {
        let mut input = InputState::new();
        input.set_paste("a\nb".to_string());
        for c in " ".chars() {
            input.insert(c);
        }
        input.set_paste("c\nd".to_string());
        assert_eq!(input.display_text(), "[2 lines pasted] [2 lines pasted]");
        assert_eq!(input.buffer(), "a\nb c\nd");
    }

    #[test]
    fn backspace_removes_only_adjacent_paste() {
        let mut input = InputState::new();
        input.set_paste("a\nb".to_string());
        for c in " ".chars() {
            input.insert(c);
        }
        input.set_paste("c\nd".to_string());
        // Backspace removes second paste only
        input.backspace();
        assert!(input.has_paste()); // first paste still there
        assert_eq!(input.buffer(), "a\nb ");
        assert_eq!(input.display_text(), "[2 lines pasted] ");
    }
}
