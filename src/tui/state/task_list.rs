use std::collections::{HashMap, HashSet};

use ratatui::widgets::ListState;

use crate::primitives::{PaneId, TaskName, WindowId};
use crate::task::{Task, TaskMessage};

pub struct TaskListState {
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    show_detail: bool,
    selected_messages: Vec<TaskMessage>,
    detail_scroll: u16,
    detail_live_output: Option<String>,
    window_numbers: HashMap<WindowId, String>,
    /// Pane IDs that are actively working (hook reported activity).
    /// Absence from this set means idle (the safe default).
    active_panes: HashSet<PaneId>,
    /// Indices into `tasks` that match the current search query.
    filtered_indices: Vec<usize>,
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
            active_panes: HashSet::new(),
            filtered_indices: Vec::new(),
        }
    }

    // ── Detail view ──────────────────────────────────────────────────

    pub fn show_detail(&mut self) {
        self.show_detail = true;
        self.detail_scroll = 0;
    }

    pub fn hide_detail(&mut self) {
        self.show_detail = false;
    }

    pub fn is_detail_visible(&self) -> bool {
        self.show_detail
    }

    pub fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }

    // ── Messages & live output ───────────────────────────────────────

    pub fn set_selected_messages(&mut self, messages: Vec<TaskMessage>) {
        self.selected_messages = messages;
    }

    pub fn clear_selected_messages(&mut self) {
        self.selected_messages.clear();
    }

    pub fn selected_messages(&self) -> &[TaskMessage] {
        &self.selected_messages
    }

    pub fn set_live_output(&mut self, output: Option<String>) {
        self.detail_live_output = output;
    }

    pub fn live_output(&self) -> Option<&str> {
        self.detail_live_output.as_deref()
    }

    // ── Window numbers ───────────────────────────────────────────────

    pub fn update_window_numbers(&mut self, numbers: HashMap<WindowId, String>) {
        self.window_numbers = numbers;
    }

    pub fn window_number(&self, id: &WindowId) -> Option<&str> {
        self.window_numbers.get(id).map(|s| s.as_str())
    }

    // ── Active panes ────────────────────────────────────────────────

    /// Mark a pane as idle (remove from active set).
    /// Returns `true` if the pane was previously active.
    pub fn mark_pane_idle(&mut self, pane: &PaneId) -> bool {
        self.active_panes.remove(pane)
    }

    /// Mark a pane as active (add to active set).
    /// Returns `true` if the pane was not already active.
    pub fn mark_pane_active(&mut self, pane: PaneId) -> bool {
        self.active_panes.insert(pane)
    }

    pub fn active_panes(&self) -> &HashSet<PaneId> {
        &self.active_panes
    }

    /// Mark the pane of the named task as active.
    /// Returns `true` if the pane was newly marked active.
    pub fn activate_task_pane(&mut self, name: &TaskName) -> bool {
        if let Some(pane_id) = self
            .tasks
            .iter()
            .find(|t| t.name == *name)
            .and_then(|t| t.tmux_pane.clone())
        {
            self.mark_pane_active(pane_id)
        } else {
            false
        }
    }

    /// Mark the pane of the named task as idle.
    /// Returns `true` if the pane was newly marked idle (was previously active).
    pub fn idle_task_pane(&mut self, name: &TaskName) -> bool {
        if let Some(pane_id) = self
            .tasks
            .iter()
            .find(|t| t.name == *name)
            .and_then(|t| t.tmux_pane.clone())
        {
            self.mark_pane_idle(&pane_id)
        } else {
            false
        }
    }

    /// Clear all active panes (everything defaults to idle).
    pub fn reset_tasks_to_idle(&mut self) {
        self.active_panes.clear();
    }

    // ── Filtered indices ─────────────────────────────────────────────

    pub fn clear_filter(&mut self) {
        self.filtered_indices.clear();
    }

    #[cfg(test)]
    pub fn set_filtered_indices(&mut self, indices: Vec<usize>) {
        self.filtered_indices = indices;
    }

    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered_indices
    }

    // ── Selection & navigation ───────────────────────────────────────

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
        let selected_id = self.selected_task().map(|t| t.id);

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

    pub fn scroll_down_tasks(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(10);
    }

    pub fn scroll_up_tasks(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(10);
    }

    /// Resolve the currently selected filtered index back to the real task index.
    pub fn selected_filtered_task_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_indices.get(i).copied())
    }

    /// Fuzzy-filter tasks by name and clamp the selection.
    pub fn filter(&mut self, query: &[char]) {
        let indices: Vec<usize> = self
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
        self.filtered_indices = indices;
        if self.filtered_indices.is_empty() {
            self.list_state.select(None);
        } else {
            let sel = self.list_state.selected().unwrap_or(0);
            if let Some(pos) = self.filtered_indices.iter().position(|&i| i == sel) {
                self.list_state.select(Some(pos));
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
}
