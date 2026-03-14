use std::collections::HashMap;

use crate::primitives::{MessageRole, TaskId, TaskName};
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
    saved_chat_input: Option<String>,
    /// Per-task input buffers, preserved across task detail navigations.
    task_inputs: HashMap<TaskId, String>,
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

    /// Returns a mutable reference to the task list if it contains a task with the given name.
    pub fn task_list_for_name(&mut self, name: &TaskName) -> Option<&mut TaskListState> {
        if self.task_list.tasks.iter().any(|t| t.name == *name) {
            Some(&mut self.task_list)
        } else {
            None
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
    /// If `saved_chat_input` is None (e.g., detail was shown via `^L` without
    /// entering task detail), the current input is left unchanged.
    pub fn leave_task_detail(&mut self, task_id: &TaskId) {
        if let Some(main) = self.saved_chat_input.take() {
            let text = self.input.take();
            if !text.is_empty() {
                self.task_inputs.insert(*task_id, text);
            }
            self.input.set(&main);
        }
    }

    /// Returns the effective scroll offset for the chat panel, choosing
    /// between task detail scroll and chat scroll based on current mode.
    pub fn chat_panel_scroll(&self) -> usize {
        if self.task_list.is_detail_visible() && self.task_list.selected_task().is_some() {
            self.task_list.detail_scroll() as usize
        } else {
            self.chat_view.chat_scroll() as usize
        }
    }

    /// Scroll the chat panel up, routing to the correct scroll state.
    pub fn scroll_chat_panel_up(&mut self) {
        if self.task_list.is_detail_visible() && self.task_list.selected_task().is_some() {
            self.task_list.scroll_up_tasks();
        } else {
            self.chat_view.scroll_chat_up();
        }
    }

    /// Scroll the chat panel down, routing to the correct scroll state.
    pub fn scroll_chat_panel_down(&mut self) {
        if self.task_list.is_detail_visible() && self.task_list.selected_task().is_some() {
            self.task_list.scroll_down_tasks();
        } else {
            self.chat_view.scroll_chat_down();
        }
    }

    /// Switch from one task's detail to another's.
    pub fn switch_task_detail(&mut self, old_id: &TaskId, new_id: &TaskId) {
        let text = self.input.take();
        if !text.is_empty() {
            self.task_inputs.insert(*old_id, text);
        }
        let new_text = self.task_inputs.remove(new_id).unwrap_or_default();
        self.input.set(&new_text);
    }

    // ── AssistantChat delegates ─────────────────────────────────────

    pub fn is_streaming(&self) -> bool {
        self.chat_view.assistant.streaming
    }

    pub fn append_text(&mut self, text: &str) {
        self.chat_view.assistant.append_text(text);
    }

    pub fn add_tool_activity(&mut self, tool: String) {
        self.chat_view.assistant.add_tool_activity(tool);
    }

    pub fn set_session_id(&mut self, id: String) {
        self.chat_view.assistant.session_id = Some(id);
    }

    pub fn session_id(&self) -> Option<&str> {
        self.chat_view.assistant.session_id.as_deref()
    }

    pub fn set_process_error(&mut self) {
        self.chat_view.assistant.had_process_error = true;
    }

    /// Finish streaming and return the last assistant message text
    /// if it should be persisted.
    pub fn finish_streaming(&mut self) -> Option<String> {
        self.chat_view.assistant.finish_streaming();
        self.last_assistant_text()
    }

    /// Add an error to the chat and return the last assistant message text
    /// if it should be persisted.
    pub fn add_error(&mut self, error: &str) -> Option<String> {
        self.chat_view.assistant.add_error(error);
        self.last_assistant_text()
    }

    /// Handle process exit: clear error flag, and if streaming, add an
    /// error message.
    pub fn mark_process_exited(&mut self, label: &str) {
        self.chat_view.assistant.had_process_error = false;
        if self.chat_view.assistant.streaming {
            self.chat_view
                .assistant
                .add_error(&format!("{label} process exited unexpectedly"));
        }
    }

    /// Add a user message to the chat and reset scroll.
    pub fn add_user_message(&mut self, content: String) {
        self.chat_view.assistant.add_user_message(content);
        self.chat_view.reset_scroll();
    }

    /// Take the current input, add it as a user message, reset scroll,
    /// and return the message text.
    pub fn add_user_message_and_take_input(&mut self) -> String {
        let msg = self.input.take();
        self.add_user_message(msg.clone());
        msg
    }

    /// Prepare for sending a chat message.
    /// Resets scroll, finishes streaming if needed, takes input,
    /// adds user message. Returns `(user_message, optional_persist_message)`.
    /// Returns `None` if input is empty.
    pub fn prepare_chat_send(&mut self) -> Option<(String, Option<(MessageRole, String)>)> {
        self.chat_view.reset_scroll();
        if self.input.is_empty() {
            return None;
        }
        let to_persist = if self.is_streaming() {
            self.finish_streaming()
                .map(|text| (MessageRole::Assistant, text))
        } else {
            None
        };
        let msg = self.add_user_message_and_take_input();
        Some((msg, to_persist))
    }

    /// Return the text content of the last message if it's an assistant
    /// message with text.
    fn last_assistant_text(&self) -> Option<String> {
        self.chat_view
            .assistant
            .messages
            .last()
            .filter(|msg| matches!(msg.role, MessageRole::Assistant) && msg.has_text())
            .map(|msg| msg.text_content())
    }
}
