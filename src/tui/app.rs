use std::collections::{HashMap, VecDeque};
use std::os::unix::net::UnixStream;

use ratatui::widgets::ListState;

use crate::primitives::TaskId;
use crate::task::{Task, TaskMessage};

pub enum Focus {
    TaskList,
    ChatInput,
    ChatHistory,
    SpawnInput,
    ConfirmDelete(TaskId),
}

pub struct ActivePermission {
    pub stream: UnixStream,
    pub task_name: String,
    pub tool_name: String,
    pub tool_input_summary: String,
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

    fn clear_paste(&mut self) {
        self.pasted = None;
    }

    pub fn insert(&mut self, c: char) {
        self.clear_paste();
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.pasted.is_some() {
            self.clear_paste();
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.pasted.is_some() {
            self.clear_paste();
            return;
        }
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    pub fn kill_line(&mut self) {
        self.clear_paste();
        self.chars.truncate(self.cursor);
    }

    pub fn kill_before(&mut self) {
        self.clear_paste();
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    pub fn kill_word(&mut self) {
        self.clear_paste();
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
}
