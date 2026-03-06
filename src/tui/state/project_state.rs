use std::collections::HashMap;

use crate::primitives::TaskId;
use crate::task::Task;
use crate::tui::chat::AssistantChat;

use super::InputState;
use super::chat_view::ChatViewState;
use super::task_list::TaskListState;

/// Per-project (or ExO) workspace — fully self-contained.
/// Each project gets its own chat, task list, and input state.
pub struct ProjectState {
    pub chat_view: ChatViewState,
    pub task_list: TaskListState,
    pub input: InputState,
    /// Saved main chat input when entering task detail view.
    pub saved_chat_input: Option<String>,
    /// Per-task input buffers, preserved across task detail navigations.
    pub task_inputs: HashMap<TaskId, String>,
}

impl ProjectState {
    pub(in crate::tui) fn new(assistant: AssistantChat, tasks: Vec<Task>) -> Self {
        Self {
            chat_view: ChatViewState::new(assistant),
            task_list: TaskListState::new(tasks),
            input: InputState::new(),
            saved_chat_input: None,
            task_inputs: HashMap::new(),
        }
    }

    /// Mark all running task panes as idle.
    pub fn reset_tasks_to_idle(&mut self) {
        self.task_list.reset_tasks_to_idle();
    }

    /// Save current input before entering task detail.
    /// Stores the main chat input and restores the target task's saved buffer.
    pub fn enter_task_detail(&mut self, task_id: &TaskId) {
        self.saved_chat_input = Some(self.input.take());
        let text = self.task_inputs.remove(task_id).unwrap_or_default();
        self.input.set(&text);
    }

    /// Save current task input and restore main chat input when leaving detail.
    pub fn leave_task_detail(&mut self, task_id: &TaskId) {
        let text = self.input.take();
        if !text.is_empty() {
            self.task_inputs.insert(task_id.clone(), text);
        }
        let main = self.saved_chat_input.take().unwrap_or_default();
        self.input.set(&main);
    }

    /// Switch from one task's detail to another's.
    pub fn switch_task_detail(&mut self, old_id: &TaskId, new_id: &TaskId) {
        let text = self.input.take();
        if !text.is_empty() {
            self.task_inputs.insert(old_id.clone(), text);
        }
        let new_text = self.task_inputs.remove(new_id).unwrap_or_default();
        self.input.set(&new_text);
    }
}
