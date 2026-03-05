use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::primitives::MessageRole;

use super::super::chat::{ChatMessage, ContentBlock};
use super::super::screen::{ChatViewState, Focus, ProjectListState, TaskListState};

pub(in crate::tui) fn render_chat(
    frame: &mut ratatui::Frame,
    focus: &Focus,
    task_list: &TaskListState,
    project_list: &ProjectListState,
    chat_view: &ChatViewState,
    area: Rect,
) {
    let searching = matches!(focus, Focus::TaskSearch);
    let in_task_chat = !searching && task_list.show_detail && task_list.selected_task().is_some();

    let title = if in_task_chat {
        let name = task_list
            .selected_task()
            .map(|t| t.name.as_str())
            .unwrap_or("?");
        format!(" Chat: {name} ")
    } else if let Some(ref name) = project_list.active_project {
        format!(" PM: {} ", name.as_str())
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
        if task_list.selected_messages.is_empty() {
            lines.push(Line::from(Span::styled(
                "No messages yet.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for msg in &task_list.selected_messages {
                let (label, label_color) = match msg.role {
                    MessageRole::System => ("PROMPT", Color::Cyan),
                    MessageRole::User => ("YOU", Color::Green),
                    MessageRole::Assistant => ("ASSISTANT", Color::White),
                };

                lines.push(Line::from(Span::styled(
                    format!("{label}:"),
                    Style::default()
                        .fg(label_color)
                        .add_modifier(Modifier::BOLD),
                )));
                for l in msg.content.lines() {
                    lines.push(Line::from(l.to_string()));
                }
                lines.push(Line::from(""));
            }
        }

        if let Some(output) = &task_list.detail_live_output {
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
    } else if let Some(ref pid) = project_list.active_project_id {
        // Render PM chat
        if let Some(pm) = chat_view.project_chats.get(pid) {
            render_chat_messages(&mut lines, &pm.messages, "PM", pm.streaming);
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Chat with PM to plan and coordinate project work",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        // Render ExO chat
        render_chat_messages(
            &mut lines,
            &chat_view.exo_chat.messages,
            "ExO",
            chat_view.exo_chat.streaming,
        );
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "Press Tab to chat with ExO",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let inner_height = inner.height as usize;
    let inner_width = inner.width as usize;

    // Account for line wrapping when calculating scroll
    let rendered_lines: usize = lines
        .iter()
        .map(|line| {
            let w = line.width();
            if w == 0 || inner_width == 0 {
                1
            } else {
                w.div_ceil(inner_width)
            }
        })
        .sum();

    let max_scroll = rendered_lines.saturating_sub(inner_height) as u16;
    let effective_scroll = chat_view.chat_scroll.min(max_scroll);
    let scroll = max_scroll.saturating_sub(effective_scroll);

    let chat = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

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
            MessageRole::User => ("You", Color::Green),
            MessageRole::Assistant => (assistant_label, Color::Cyan),
            MessageRole::System => ("System", Color::DarkGray),
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
                        for l in text.trim_start_matches('\n').lines() {
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
