use std::collections::HashMap;

use crate::primitives::{ChatId, ProjectId};

use super::project_list::ProjectListState;
use super::task_list::TaskListState;
use crate::tui::chat::AssistantChat;

pub struct ChatViewState {
    /// Per-chat input buffers, saved/restored on focus changes.
    pub chat_buffers: HashMap<ChatId, String>,
    pub chat_scroll: u16,
    pub chat_viewport_height: u16,
    /// ExO assistant chat state (messages, streaming flag).
    pub exo_chat: AssistantChat,
    /// Per-project PM assistant chat states.
    pub project_chats: HashMap<ProjectId, AssistantChat>,
}

impl ChatViewState {
    pub(super) fn new() -> Self {
        Self {
            chat_buffers: HashMap::new(),
            chat_scroll: 0,
            chat_viewport_height: 0,
            exo_chat: AssistantChat::new(),
            project_chats: HashMap::new(),
        }
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
}

/// Determine which chat buffer corresponds to the current view.
pub(super) fn current_chat_id(tl: &TaskListState, pl: &ProjectListState) -> ChatId {
    if tl.show_detail {
        tl.selected_task()
            .map(|t| ChatId::Task(t.id.clone()))
            .unwrap_or(ChatId::Exo)
    } else if let Some(ref pid) = pl.active_project_id {
        ChatId::Project(pid.clone())
    } else {
        ChatId::Exo
    }
}
