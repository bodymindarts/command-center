use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::net::UnixStream;

use ratatui::widgets::ListState;

use crate::primitives::{TaskId, TaskStatus};
use crate::task::{Task, TaskMessage};

pub enum Focus {
    TaskList,
    TaskSearch,
    ChatInput,
    ChatHistory,
    SpawnInput,
    ConfirmDelete(TaskId),
    ConfirmCloseTask(TaskId),
    #[allow(dead_code)]
    ConfirmCloseProject,
}

pub struct ActivePermission {
    pub stream: UnixStream,
    pub task_name: String,
    pub tool_name: String,
    pub tool_input_summary: String,
    pub permission_suggestions: Vec<serde_json::Value>,
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
    /// Currently active project name. None = default (ExO).
    pub active_project: Option<String>,
    /// Current search query for task list filtering.
    pub search_query: String,
    /// Indices into `tasks` that match the current search query.
    pub filtered_indices: Vec<usize>,
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
            search_query: String::new(),
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

    pub fn total_pending_permissions(&self) -> usize {
        self.pending_permissions.values().map(|q| q.len()).sum()
    }

    fn current_chat_key(&self) -> String {
        if self.show_detail {
            self.selected_task()
                .map(|t| t.id.as_str().to_string())
                .unwrap_or_else(|| "exo".to_string())
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

    /// Recompute `filtered_indices` based on `search_query`.
    /// Fuzzy match: each query char must appear in order (e.g. "res" matches "r.*e.*s.*").
    pub fn update_search_filter(&mut self) {
        let query: Vec<char> = self.search_query.to_lowercase().chars().collect();
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
