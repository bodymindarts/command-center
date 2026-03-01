use ratatui::widgets::ListState;

use crate::store::{Store, Task};

pub enum Focus {
    TaskList,
    ChatInput,
    PermissionPrompt,
}

pub struct InputState {
    chars: Vec<char>,
    pub cursor: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
        }
    }

    pub fn buffer(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
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
        self.chars.truncate(self.cursor);
    }

    pub fn kill_before(&mut self) {
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    pub fn kill_word(&mut self) {
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
        let result: String = self.chars.iter().collect();
        self.chars.clear();
        result
    }
}

pub struct PendingPermission {
    pub req_id: String,
    pub task_name: String,
    pub tool_name: String,
    pub tool_input_summary: String,
}

pub struct App {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    pub should_quit: bool,
    pub focus: Focus,
    pub input: InputState,
    pub show_detail: bool,
    pub pending_permission: Option<PendingPermission>,
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
            pending_permission: None,
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

    pub fn refresh(&mut self, store: &Store) {
        let selected_id = self.selected_task().map(|t| t.id.clone());
        if let Ok(tasks) = store.list_tasks() {
            self.tasks = tasks;
        }
        if let Some(id) = selected_id {
            if let Some(pos) = self.tasks.iter().position(|t| t.id == id) {
                self.list_state.select(Some(pos));
            } else if !self.tasks.is_empty() {
                let clamped = self
                    .list_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.tasks.len() - 1);
                self.list_state.select(Some(clamped));
            } else {
                self.list_state.select(None);
            }
        } else if !self.tasks.is_empty() && self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
    }

    pub fn goto_selected(&self) {
        if let Some(task) = self.selected_task()
            && let Some(window_id) = &task.tmux_window
        {
            let _ = std::process::Command::new("tmux")
                .args(["select-window", "-t", window_id])
                .output();
        }
    }
}
