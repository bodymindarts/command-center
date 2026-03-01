use crate::task::TaskMessage;

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
            session_id: None,
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

    pub fn load_history(&mut self, messages: Vec<TaskMessage>) {
        for msg in messages {
            match msg.role.as_str() {
                "user" => {
                    self.messages.push(ChatMessage {
                        role: Role::User,
                        content: msg.content,
                        tool_activity: Vec::new(),
                    });
                }
                "assistant" => {
                    self.messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: msg.content,
                        tool_activity: Vec::new(),
                    });
                }
                _ => {}
            }
        }
    }
}
