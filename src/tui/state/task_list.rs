use std::collections::{HashMap, HashSet};

use ratatui::widgets::ListState;

use crate::primitives::{PaneId, WindowId};
use crate::task::{Task, TaskMessage};

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
    pub(super) fn new(tasks: Vec<Task>) -> Self {
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
