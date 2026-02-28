pub enum Role {
    User,
    Assistant,
}

pub struct ChatMessage {
    pub role: Role,
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
            session_id: Some("ab53d707-9df5-45b6-a004-e510ae1dad77".to_string()),
            messages: Vec::new(),
            streaming: false,
        }
    }

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(ChatMessage {
            role: Role::User,
            content,
            tool_activity: Vec::new(),
        });
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            tool_activity: Vec::new(),
        });
        self.streaming = true;
    }

    pub fn append_text(&mut self, text: &str) {
        if let Some(msg) = self
            .messages
            .last_mut()
            .filter(|m| matches!(m.role, Role::Assistant))
        {
            msg.content.push_str(text);
        }
    }

    pub fn add_tool_activity(&mut self, tool: String) {
        if let Some(msg) = self
            .messages
            .last_mut()
            .filter(|m| matches!(m.role, Role::Assistant))
        {
            msg.tool_activity.push(tool);
        }
    }

    pub fn finish_streaming(&mut self) {
        self.streaming = false;
    }
}
