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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_error_creates_assistant_message_when_none_exists() {
        let mut exo = ExoState::new();
        exo.add_error("connection lost");

        assert_eq!(exo.messages.len(), 1);
        assert!(matches!(exo.messages[0].role, MessageRole::Assistant));
        assert!(exo.messages[0].content.contains("[Error: connection lost]"));
        assert!(!exo.streaming);
    }

    #[test]
    fn add_error_appends_to_existing_assistant_message() {
        let mut exo = ExoState::new();
        exo.add_user_message("hello".into());
        assert!(exo.streaming);

        // Simulate some text arriving
        exo.append_text("partial response");

        exo.add_error("pipe broke");

        // user + assistant = 2 messages (no extra created)
        assert_eq!(exo.messages.len(), 2);
        let assistant = &exo.messages[1];
        assert!(matches!(assistant.role, MessageRole::Assistant));
        assert!(assistant.content.contains("partial response"));
        assert!(assistant.content.contains("[Error: pipe broke]"));
        assert!(!exo.streaming);
    }

    #[test]
    fn add_error_after_user_message_with_no_text_yet() {
        let mut exo = ExoState::new();
        exo.add_user_message("hello".into());

        // Error before any text arrives — should append to the empty assistant msg
        exo.add_error("spawn failed");

        assert_eq!(exo.messages.len(), 2);
        assert!(exo.messages[1].content.contains("[Error: spawn failed]"));
        assert!(!exo.streaming);
    }

    #[test]
    fn add_error_does_not_append_to_user_message() {
        let mut exo = ExoState::new();
        // Manually push just a user message (no assistant follows)
        exo.messages.push(ChatMessage {
            role: MessageRole::User,
            content: "test".into(),
            tool_activity: Vec::new(),
        });

        exo.add_error("bad state");

        // Should have created a new assistant message, not appended to user
        assert_eq!(exo.messages.len(), 2);
        assert!(matches!(exo.messages[1].role, MessageRole::Assistant));
        assert!(exo.messages[1].content.contains("[Error: bad state]"));
    }

    #[test]
    fn finish_streaming_clears_flag() {
        let mut exo = ExoState::new();
        exo.add_user_message("hi".into());
        assert!(exo.streaming);
        exo.finish_streaming();
        assert!(!exo.streaming);
    }
}
