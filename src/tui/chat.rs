use crate::primitives::MessageRole;
use crate::task::TaskMessage;

pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_activity: Vec<String>,
}

pub struct ExoState {
    pub session_id: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub streaming: bool,
}

impl ExoState {
    pub fn new() -> Self {
        Self {
            session_id: None,
            messages: Vec::new(),
            streaming: false,
        }
    }

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(ChatMessage {
            role: MessageRole::User,
            content,
            tool_activity: Vec::new(),
        });
        self.messages.push(ChatMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_activity: Vec::new(),
        });
        self.streaming = true;
    }

    pub fn append_text(&mut self, text: &str) {
        if let Some(msg) = self
            .messages
            .last_mut()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
        {
            msg.content.push_str(text);
        }
    }

    pub fn add_tool_activity(&mut self, tool: String) {
        if let Some(msg) = self
            .messages
            .last_mut()
            .filter(|m| matches!(m.role, MessageRole::Assistant))
        {
            msg.tool_activity.push(tool);
        }
    }

    /// Surface an error in the chat history. If there's already a pending
    /// assistant message (streaming), append to it; otherwise create one.
    pub fn add_error(&mut self, error: &str) {
        let has_assistant = self
            .messages
            .last()
            .is_some_and(|m| matches!(m.role, MessageRole::Assistant));
        if !has_assistant {
            self.messages.push(ChatMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_activity: Vec::new(),
            });
        }
        self.append_text(&format!("\n[Error: {error}]"));
        self.streaming = false;
    }

    pub fn finish_streaming(&mut self) {
        self.streaming = false;
    }

    pub fn load_history(&mut self, messages: Vec<TaskMessage>) {
        for msg in messages {
            match msg.role {
                MessageRole::User | MessageRole::Assistant => {
                    self.messages.push(ChatMessage {
                        role: msg.role,
                        content: msg.content,
                        tool_activity: Vec::new(),
                    });
                }
                MessageRole::System => {}
            }
        }
    }
}
