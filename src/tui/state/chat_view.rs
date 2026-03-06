use crate::tui::chat::AssistantChat;

pub struct ChatViewState {
    pub assistant: AssistantChat,
    chat_scroll: u16,
    chat_viewport_height: u16,
}

impl ChatViewState {
    pub(super) fn new(assistant: AssistantChat) -> Self {
        Self {
            assistant,
            chat_scroll: 0,
            chat_viewport_height: 0,
        }
    }

    pub fn chat_scroll(&self) -> u16 {
        self.chat_scroll
    }

    pub fn reset_scroll(&mut self) {
        self.chat_scroll = 0;
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
