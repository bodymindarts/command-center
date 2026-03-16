use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::primitives::MessageRole;

use super::super::chat::{ChatMessage, ContentBlock};
use super::super::state::{Focus, ProjectState};

pub(in crate::tui) fn render_chat(
    frame: &mut ratatui::Frame,
    focus: &Focus,
    active: &ProjectState,
    active_project_name: Option<&str>,
    project_list_visible: bool,
    area: Rect,
) {
    let task_list = &active.task_list;
    let chat_view = &active.chat_view;
    let searching = matches!(focus, Focus::ListSearch);
    let in_task_chat = !project_list_visible
        && !searching
        && task_list.is_detail_visible()
        && task_list.selected_task().is_some();

    let title = if in_task_chat {
        let name = task_list
            .selected_task()
            .map(|t| t.name.as_str())
            .unwrap_or("?");
        format!(" Chat: {name} ")
    } else if let Some(name) = active_project_name {
        format!(" PM: {name} ")
    } else {
        " ExO Chat ".to_string()
    };

    let chat_border_color = if matches!(focus, Focus::ChatHistory) {
        Color::Blue
    } else {
        Color::DarkGray
    };

    // Render the outer border block
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(chat_border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build chat content lines
    let mut lines: Vec<Line> = Vec::new();

    if in_task_chat {
        // Render task messages
        if task_list.selected_messages().is_empty() {
            lines.push(Line::from(Span::styled(
                "No messages yet.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in task_list.selected_messages() {
                let (label, label_color, display_content) = match msg.role {
                    MessageRole::System => {
                        ("PROMPT".to_string(), Color::Cyan, msg.content.as_str())
                    }
                    MessageRole::Assistant => {
                        ("ASSISTANT".to_string(), Color::White, msg.content.as_str())
                    }
                    MessageRole::User => {
                        if let Some((name, body)) = parse_task_msg_sender(&msg.content) {
                            (name, Color::Yellow, body)
                        } else {
                            ("YOU".to_string(), Color::Green, msg.content.as_str())
                        }
                    }
                };

                lines.push(Line::from(Span::styled(
                    format!("{label}:"),
                    Style::default()
                        .fg(label_color)
                        .add_modifier(Modifier::BOLD),
                )));
                for l in display_content.lines() {
                    lines.push(Line::from(l.to_string()));
                }
                lines.push(Line::from(""));
            }
        }

        if let Some(output) = task_list.live_output() {
            let tail: Vec<&str> = output.lines().collect();
            let start = tail.len().saturating_sub(500);
            lines.push(Line::from(Span::styled(
                format!("--- Live (last {} lines) ---", tail.len() - start),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
            for l in &tail[start..] {
                lines.push(Line::from(l.to_string()));
            }
        }
    } else {
        // Render assistant chat (ExO or PM — both use the same single chat)
        let label = if active_project_name.is_some() {
            "PM"
        } else {
            "ExO"
        };
        render_chat_messages(
            &mut lines,
            &chat_view.assistant.messages,
            label,
            chat_view.assistant.streaming,
        );
        if lines.is_empty() {
            let hint = if active_project_name.is_some() {
                "Chat with PM to plan and coordinate project work"
            } else {
                "Press Tab to chat with ExO"
            };
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let inner_height = inner.height as usize;
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let rendered_lines = paragraph.line_count(inner.width);

    let scroll_offset = active.chat_panel_scroll();
    let max_scroll = rendered_lines.saturating_sub(inner_height);
    let effective_scroll = scroll_offset.min(max_scroll);
    // Ratatui's Paragraph::scroll takes u16; clamp to prevent silent wrapping.
    let scroll = max_scroll
        .saturating_sub(effective_scroll)
        .min(u16::MAX as usize) as u16;

    let chat = paragraph.scroll((scroll, 0));
    frame.render_widget(chat, inner);
}

/// Render streaming chat messages (used for both ExO and PM chats).
fn render_chat_messages(
    lines: &mut Vec<Line<'static>>,
    messages: &[ChatMessage],
    assistant_label: &str,
    streaming: bool,
) {
    for msg in messages {
        let (label, label_color) = match msg.role {
            MessageRole::User => {
                if let Some(sender) = parse_agent_sender(msg) {
                    (sender, Color::Yellow)
                } else {
                    ("You".to_string(), Color::Green)
                }
            }
            MessageRole::Assistant => (assistant_label.to_string(), Color::Cyan),
            MessageRole::System => ("System".to_string(), Color::DarkGray),
        };

        lines.push(Line::from(Span::styled(
            format!("{label}:"),
            Style::default()
                .fg(label_color)
                .add_modifier(Modifier::BOLD),
        )));

        if msg.blocks.is_empty() && matches!(msg.role, MessageRole::Assistant) && streaming {
            lines.push(Line::from(Span::styled(
                "...",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            let mut tool_spans: Vec<Span> = Vec::new();
            for block in &msg.blocks {
                match block {
                    ContentBlock::Text(text) => {
                        if !tool_spans.is_empty() {
                            lines.push(Line::from(std::mem::take(&mut tool_spans)));
                        }
                        // Strip the "[from agent ...] " prefix since we show it in the label
                        let display_text = strip_agent_prefix(text);
                        for l in display_text.trim_start_matches('\n').lines() {
                            lines.push(Line::from(l.to_string()));
                        }
                    }
                    ContentBlock::ToolUse(name) => {
                        tool_spans.push(Span::styled(
                            format!("[{name}] "),
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                }
            }
            if !tool_spans.is_empty() {
                lines.push(Line::from(tool_spans));
            }
        }

        lines.push(Line::from(""));
    }
}

/// Strip the `[from <name> (<role>)] ` prefix from message text for display.
fn strip_agent_prefix(text: &str) -> &str {
    crate::primitives::parse_sender_prefix(text).map_or(text, |(_, body)| body)
}

/// Parse `[from <name> (<role>)] <body>` from raw message content (e.g. TaskMessage).
/// Returns `(name, body)` if the prefix is found.
fn parse_task_msg_sender(content: &str) -> Option<(String, &str)> {
    let (name, body) = crate::primitives::parse_sender_prefix(content)?;
    Some((name.to_string(), body))
}

/// Extract sender label from agent messages formatted as `[from <name> (<role>)] <msg>`.
/// Returns the display label (e.g. "exo-task-monitor") if the prefix is found.
fn parse_agent_sender(msg: &ChatMessage) -> Option<String> {
    let text = match msg.blocks.first()? {
        ContentBlock::Text(t) => t,
        _ => return None,
    };
    let (name, _) = crate::primitives::parse_sender_prefix(text)?;
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_msg_sender_with_role() {
        let (name, body) = parse_task_msg_sender("[from my-project (pm)] hello world").unwrap();
        assert_eq!(name, "my-project");
        assert_eq!(body, "hello world");
    }

    #[test]
    fn parse_task_msg_sender_without_role() {
        let (name, body) = parse_task_msg_sender("[from agent-x] done").unwrap();
        assert_eq!(name, "agent-x");
        assert_eq!(body, "done");
    }

    #[test]
    fn parse_task_msg_sender_no_prefix() {
        assert!(parse_task_msg_sender("plain text").is_none());
    }

    #[test]
    fn parse_agent_sender_with_role() {
        let msg = ChatMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text(
                "[from my-project (pm)] hello".to_string(),
            )],
        };
        assert_eq!(parse_agent_sender(&msg).unwrap(), "my-project");
    }

    #[test]
    fn parse_agent_sender_without_role() {
        let msg = ChatMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text("[from agent-name] some msg".to_string())],
        };
        assert_eq!(parse_agent_sender(&msg).unwrap(), "agent-name");
    }

    #[test]
    fn parse_agent_sender_no_prefix() {
        let msg = ChatMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::Text("plain message".to_string())],
        };
        assert!(parse_agent_sender(&msg).is_none());
    }

    #[test]
    fn parse_agent_sender_tool_block_first() {
        let msg = ChatMessage {
            role: MessageRole::User,
            blocks: vec![ContentBlock::ToolUse("Bash".to_string())],
        };
        assert!(parse_agent_sender(&msg).is_none());
    }

    #[test]
    fn strip_agent_prefix_with_sender() {
        assert_eq!(strip_agent_prefix("[from ExO (exo)] hello"), "hello");
    }

    #[test]
    fn strip_agent_prefix_no_sender() {
        assert_eq!(strip_agent_prefix("plain text"), "plain text");
    }
}
