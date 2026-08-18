use std::{borrow::Cow, collections::BTreeMap, io, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event as TerminalEvent, EventStream, KeyCode,
        KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use e_agent_core::{
    AgentEvent, Message, MessageContent, MessageDelta, SessionAttachment, SessionClient,
    SessionHandle, SessionStatus, StopReason, UserMessage,
};
use futures_util::StreamExt;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone)]
pub struct ToolState {
    pub name: String,
    pub status: String,
    pub input: String,
    pub update: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub is_error: bool,
}

#[derive(Debug)]
pub struct AppState {
    pub messages: Vec<Message>,
    pub partial_id: Option<String>,
    pub partial: String,
    pub thinking: String,
    pub tool_call_input: String,
    pub partial_unpersisted: bool,
    pub tools: BTreeMap<String, ToolState>,
    pub status: SessionStatus,
    pub editor: String,
    pub cursor: usize,
    pub scroll: u16,
    pub follow: bool,
    pub fatal_error: Option<String>,
}

impl AppState {
    pub fn new(messages: Vec<Message>, status: SessionStatus) -> Self {
        Self {
            messages,
            partial_id: None,
            partial: String::new(),
            thinking: String::new(),
            tool_call_input: String::new(),
            partial_unpersisted: false,
            tools: BTreeMap::new(),
            status,
            editor: String::new(),
            cursor: 0,
            scroll: 0,
            follow: true,
            fatal_error: None,
        }
    }

    pub fn reduce(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AgentStart { .. } => self.status = SessionStatus::Running,
            AgentEvent::MessageStart {
                message_id,
                message: Message::Assistant(_),
            } => {
                self.partial_id = Some(message_id);
                self.partial.clear();
                self.thinking.clear();
                self.tool_call_input.clear();
                self.partial_unpersisted = false;
            }
            AgentEvent::MessageUpdate {
                message_id,
                delta: MessageDelta::Thinking(text),
                ..
            } if self.partial_id.as_deref() == Some(&message_id) => self.thinking.push_str(&text),
            AgentEvent::MessageUpdate {
                message_id,
                delta: MessageDelta::ToolCallInput(input),
                ..
            } if self.partial_id.as_deref() == Some(&message_id) => {
                self.tool_call_input.push_str(&input)
            }
            AgentEvent::MessageUpdate {
                message_id,
                delta: MessageDelta::Text(text),
                ..
            } if self.partial_id.as_deref() == Some(&message_id) => self.partial.push_str(&text),
            AgentEvent::MessageEnd {
                message_id,
                message,
            } => {
                if self.partial_id.as_deref() == Some(&message_id) {
                    self.partial_id = None;
                    self.partial.clear();
                    self.thinking.clear();
                    self.tool_call_input.clear();
                    self.partial_unpersisted = false;
                }
                self.messages.push(message);
            }
            AgentEvent::ToolExecutionStart { id, name, input } => {
                self.tools.insert(
                    id,
                    ToolState {
                        name,
                        status: "running".into(),
                        input,
                        update: None,
                        result: None,
                        is_error: false,
                    },
                );
            }
            AgentEvent::ToolExecutionUpdate { id, update } => {
                if let Some(tool) = self.tools.get_mut(&id) {
                    tool.status = "updating".into();
                    tool.update = Some(update);
                }
            }
            AgentEvent::ToolExecutionEnd {
                id,
                result,
                is_error,
                ..
            } => {
                if let Some(tool) = self.tools.get_mut(&id) {
                    tool.status = if is_error { "error" } else { "done" }.into();
                    tool.result = Some(result);
                    tool.is_error = is_error;
                }
            }
            AgentEvent::AgentSettled { .. } => self.status = SessionStatus::Idle,
            AgentEvent::SessionFatal { error } => {
                self.status = SessionStatus::Fatal;
                self.partial_unpersisted = !self.partial.is_empty();
                self.fatal_error = Some(error);
                for tool in self.tools.values_mut() {
                    if tool.status == "running" || tool.status == "updating" {
                        tool.status = "interrupted".into();
                    }
                }
            }
            AgentEvent::SessionShutdown if self.status != SessionStatus::Fatal => {
                self.status = SessionStatus::Closed
            }
            _ => {}
        }
    }

    fn insert(&mut self, ch: char) {
        let mut chars = self.editor.chars().collect::<Vec<_>>();
        chars.insert(self.cursor, ch);
        self.cursor += 1;
        self.editor = chars.into_iter().collect();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars = self.editor.chars().collect::<Vec<_>>();
        chars.remove(self.cursor - 1);
        self.cursor -= 1;
        self.editor = chars.into_iter().collect();
    }

    fn delete(&mut self) {
        let mut chars = self.editor.chars().collect::<Vec<_>>();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.editor = chars.into_iter().collect();
        }
    }

    fn move_vertical(&mut self, direction: isize) {
        let chars = self.editor.chars().collect::<Vec<_>>();
        let line_start = |index: usize| {
            chars[..index]
                .iter()
                .rposition(|ch| *ch == '\n')
                .map_or(0, |pos| pos + 1)
        };
        let current_start = line_start(self.cursor);
        let column = self.cursor - current_start;
        if direction < 0 {
            if current_start == 0 {
                return;
            }
            let previous_end = current_start - 1;
            let previous_start = line_start(previous_end);
            self.cursor = previous_start + column.min(previous_end - previous_start);
        } else {
            let current_end = chars[self.cursor..]
                .iter()
                .position(|ch| *ch == '\n')
                .map(|offset| self.cursor + offset);
            let Some(current_end) = current_end else {
                return;
            };
            let next_start = current_end + 1;
            let next_end = chars[next_start..]
                .iter()
                .position(|ch| *ch == '\n')
                .map_or(chars.len(), |offset| next_start + offset);
            self.cursor = next_start + column.min(next_end - next_start);
        }
    }

    fn scroll_up(&mut self, amount: u16) {
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(amount);
    }

    fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    fn take_editor(&mut self) -> Option<String> {
        if self.editor.trim().is_empty() {
            return None;
        }
        self.cursor = 0;
        Some(std::mem::take(&mut self.editor))
    }
}

pub async fn run(mut attachment: SessionAttachment) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut app = AppState::new(attachment.messages, attachment.status);
    let mut dirty = false;

    terminal.draw(|frame| render(frame, &mut app))?;
    loop {
        tokio::select! {
            event = attachment.events.recv() => match event {
                Ok(event) => {
                    app.reduce(event);
                    dirty = true;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    app.status = SessionStatus::Fatal;
                    app.fatal_error = Some(format!("event observer lagged by {skipped} records"));
                    dirty = true;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            event = input.next() => {
                let Some(Ok(event)) = event else { break };
                if handle_terminal_event(event, &mut app, &attachment.handle).await? { break; }
                dirty = true;
            }
            _ = tick.tick(), if dirty => {
                terminal.draw(|frame| render(frame, &mut app))?;
                dirty = false;
            }
        }
    }
    attachment.handle.abort().await?;
    attachment.handle.close().await?;
    Ok(())
}

async fn handle_terminal_event(
    event: TerminalEvent,
    app: &mut AppState,
    handle: &SessionClient,
) -> Result<bool> {
    if let TerminalEvent::Mouse(event) = event {
        match event.kind {
            MouseEventKind::ScrollUp => app.scroll_up(3),
            MouseEventKind::ScrollDown => app.scroll_down(3),
            _ => {}
        }
        return Ok(false);
    }
    let TerminalEvent::Key(KeyEvent {
        code,
        modifiers,
        kind,
        ..
    }) = event
    else {
        return Ok(false);
    };
    if !matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return Ok(false);
    }
    if code == KeyCode::Esc {
        return Ok(true);
    }
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        if app.status == SessionStatus::Running {
            handle.abort().await?;
            return Ok(false);
        }
        return Ok(true);
    }
    if app.status == SessionStatus::Fatal {
        return Ok(false);
    }
    match code {
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => app.insert('\n'),
        KeyCode::Enter => {
            if let Some(text) = app.take_editor() {
                let handle = handle.clone();
                tokio::task::spawn_local(async move {
                    let _ = handle.prompt(UserMessage::text(text)).await;
                });
            }
        }
        KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => app.insert(ch),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete(),
        KeyCode::Left => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Right => app.cursor = (app.cursor + 1).min(app.editor.chars().count()),
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.editor.chars().count(),
        KeyCode::Up if app.editor.is_empty() => app.scroll_up(1),
        KeyCode::Down if app.editor.is_empty() => app.scroll_down(1),
        KeyCode::Up => app.move_vertical(-1),
        KeyCode::Down => app.move_vertical(1),
        KeyCode::PageUp => app.scroll_up(10),
        KeyCode::PageDown => app.scroll_down(10),
        _ => {}
    }
    Ok(false)
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut AppState) {
    let area = frame.area();
    let editor_height =
        (app.editor.split('\n').count() as u16 + 2).clamp(3, area.height.saturating_sub(2).max(3));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(editor_height),
            Constraint::Length(1),
        ])
        .split(area);

    let transcript = transcript(app, chunks[0].width as usize);
    let transcript = Paragraph::new(transcript).wrap(Wrap { trim: false });
    let line_count = transcript.line_count(chunks[0].width) as u16;
    let max_scroll = line_count.saturating_sub(chunks[0].height);
    if app.follow {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
        if app.scroll == max_scroll {
            app.follow = true;
        }
    }
    frame.render_widget(transcript.scroll((app.scroll, 0)), chunks[0]);

    let editor = Paragraph::new(app.editor.as_str()).block(Block::default().borders(Borders::TOP));
    frame.render_widget(editor, chunks[1]);
    let (row, col) = cursor_position(&app.editor, app.cursor);
    let x = chunks[1]
        .x
        .saturating_add(col as u16)
        .min(chunks[1].right().saturating_sub(1));
    let y = chunks[1]
        .y
        .saturating_add(1 + row as u16)
        .min(chunks[1].bottom().saturating_sub(1));
    frame.set_cursor_position(Position::new(x, y));

    let status = app.fatal_error.as_deref().unwrap_or(match app.status {
        SessionStatus::Idle => "Idle",
        SessionStatus::Running => "Running",
        SessionStatus::Fatal => "Fatal",
        SessionStatus::Closed => "Closed",
    });
    let style = if app.status == SessionStatus::Fatal {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(Paragraph::new(Line::styled(status, style)), chunks[2]);
}

fn transcript(app: &AppState, width: usize) -> Text<'static> {
    let mut lines = Vec::new();
    for message in &app.messages {
        let (label, style) = match message {
            Message::User(_) => (
                "You",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Message::Assistant(_) => (
                "Assistant",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Message::ToolResult(_) => (
                "Tool",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        };
        lines.push(Line::styled(label, style));
        lines.extend(message_lines(message, width));
        if let Message::Assistant(message) = message {
            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                lines.push(Line::styled(
                    format!("{:?}", message.stop_reason),
                    Style::default().fg(Color::Red),
                ));
            }
        }
        lines.push(Line::default());
    }
    if !app.thinking.is_empty() {
        lines.push(Line::styled(
            "Thinking",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_text(&app.thinking, width).lines);
    }
    if !app.tool_call_input.is_empty() {
        lines.push(Line::styled(
            "Tool source",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(
            markdown_text(
                &format!("```typescript\n{}\n```", tool_source(&app.tool_call_input)),
                width,
            )
            .lines,
        );
    }
    if !app.partial.is_empty() {
        lines.push(Line::styled(
            "Assistant",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(markdown_text(&app.partial, width).lines);
        if app.partial_unpersisted {
            lines.push(Line::styled("Unpersisted", Style::default().fg(Color::Red)));
        }
    }
    for tool in app
        .tools
        .values()
        .filter(|tool| matches!(tool.status.as_str(), "running" | "updating"))
    {
        let style = if tool.is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        lines.push(Line::styled(
            format!("{}: {}", tool.name, tool.status),
            style,
        ));
        lines.extend(
            markdown_text(
                &format!("```typescript\n{}\n```", tool_source(&tool.input)),
                width,
            )
            .lines,
        );
        if let Some(update) = &tool.update {
            lines.extend(markdown_text(&format!("```json\n{}\n```", update), width).lines);
        }
        if let Some(result) = &tool.result
            && !result.is_null()
            && result != &serde_json::json!({})
        {
            lines.extend(markdown_text(&format!("```json\n{}\n```", result), width).lines);
        }
    }
    Text::from(lines)
}

fn message_lines(message: &Message, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for content in message.content() {
        match content {
            MessageContent::Text { text } => lines.extend(markdown_text(text, width).lines),
            MessageContent::Thinking { thinking, .. } => {
                lines.push(Line::styled(
                    "Thinking",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
                lines.extend(markdown_text(thinking, width).lines);
            }
            MessageContent::ToolUse { name, input, .. } => {
                lines.push(Line::styled(
                    format!("Tool source: {name}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
                lines.extend(
                    markdown_text(
                        &format!("```typescript\n{}\n```", tool_source(input)),
                        width,
                    )
                    .lines,
                );
            }
        }
    }
    lines
}

fn tool_source(input: &str) -> Cow<'_, str> {
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| value["code"].as_str().map(str::to_owned))
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(input))
}

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
}

pub fn markdown_text(source: &str, width: usize) -> Text<'static> {
    let mut lines = vec![Line::default()];
    let mut style = Style::default();
    let mut table: Option<TableState> = None;

    for event in Parser::new_ext(source, Options::all()) {
        if let Some(state) = table.as_mut() {
            match event {
                Event::Start(Tag::Table(_)) => {}
                Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {}
                Event::Start(Tag::TableCell) => {
                    state.cell.clear();
                    state.in_cell = true;
                }
                Event::End(TagEnd::TableCell) => {
                    state.row.push(std::mem::take(&mut state.cell));
                    state.in_cell = false;
                }
                Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                    if !state.row.is_empty() {
                        state.rows.push(std::mem::take(&mut state.row));
                    }
                }
                Event::End(TagEnd::Table) => {
                    let state = table.take().unwrap();
                    render_table(&mut lines, &state.rows, width);
                }
                Event::Text(text) | Event::Code(text) if state.in_cell => {
                    state.cell.push_str(&text);
                }
                Event::SoftBreak | Event::HardBreak if state.in_cell => state.cell.push(' '),
                _ if state.in_cell => {}
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(Tag::Table(_)) => table = Some(TableState::default()),
            Event::Start(Tag::Heading { .. }) => {
                style = Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan)
            }
            Event::End(TagEnd::Heading(_)) => {
                style = Style::default();
                lines.push(Line::default());
            }
            Event::Start(Tag::Strong) => style = style.add_modifier(Modifier::BOLD),
            Event::End(TagEnd::Strong) => style = style.remove_modifier(Modifier::BOLD),
            Event::Start(Tag::Emphasis) => style = style.add_modifier(Modifier::ITALIC),
            Event::End(TagEnd::Emphasis) => style = style.remove_modifier(Modifier::ITALIC),
            Event::Start(Tag::CodeBlock(_)) => style = Style::default().fg(Color::Yellow),
            Event::End(TagEnd::CodeBlock) => {
                style = Style::default();
                lines.push(Line::default());
            }
            Event::Text(text) | Event::Code(text) => lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(text.into_string(), style)),
            Event::SoftBreak | Event::HardBreak => lines.push(Line::default()),
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::Item) => lines.push(Line::default()),
            Event::Start(Tag::Item) => lines.last_mut().unwrap().spans.push(Span::raw("• ")),
            _ => {}
        }
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    Text::from(lines)
}

fn render_table(lines: &mut Vec<Line<'static>>, rows: &[Vec<String>], width: usize) {
    if rows.is_empty() {
        return;
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return;
    }

    let mut widths = vec![1usize; columns];
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.width().max(1));
        }
    }
    let max_content = width.saturating_sub(columns + 1 + columns * 2);
    while widths.iter().sum::<usize>() > max_content {
        let Some(index) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| **value)
            .map(|(index, _)| index)
        else {
            break;
        };
        if widths[index] <= 1 {
            break;
        }
        widths[index] -= 1;
    }

    let border = |left: char, middle: char, right: char| {
        let mut text = String::new();
        text.push(left);
        for (index, column_width) in widths.iter().enumerate() {
            text.push_str(&"─".repeat(column_width + 2));
            text.push(if index + 1 == columns { right } else { middle });
        }
        text
    };
    lines.push(Line::from(border('┌', '┬', '┐')));
    for (row_index, row) in rows.iter().enumerate() {
        let cells = (0..columns)
            .map(|index| row.get(index).map(String::as_str).unwrap_or(""))
            .collect::<Vec<_>>();
        let wrapped = cells
            .iter()
            .enumerate()
            .map(|(index, cell)| wrap_cell(cell, widths[index]))
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let mut text = String::from("│");
            for (index, cell_lines) in wrapped.iter().enumerate() {
                let cell = cell_lines.get(line_index).map(String::as_str).unwrap_or("");
                text.push(' ');
                text.push_str(cell);
                text.push_str(&" ".repeat(widths[index].saturating_sub(cell.width()) + 1));
                text.push('│');
            }
            lines.push(Line::from(text));
        }
        if row_index + 1 < rows.len() {
            lines.push(Line::from(border('├', '┼', '┤')));
        }
    }
    lines.push(Line::from(border('└', '┴', '┘')));
    lines.push(Line::default());
}

fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.width() + 1 + word.width() > width {
            result.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        for ch in word.chars() {
            if line.width() + UnicodeWidthChar::width(ch).unwrap_or(0) > width && !line.is_empty() {
                result.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
    }
    if !line.is_empty() || result.is_empty() {
        result.push(line);
    }
    result
}

fn cursor_position(text: &str, cursor: usize) -> (usize, usize) {
    let mut row = 0;
    let mut col = 0;
    for ch in text.chars().take(cursor) {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else if ch == '\t' {
            col += 4 - (col % 4);
        } else {
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    (row, col)
}

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use e_agent_core::{AssistantMessage, Usage};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn reducer_restores_streams_and_marks_fatal_partial() {
        let mut app = AppState::new(
            vec![Message::User(UserMessage::text("# hello"))],
            SessionStatus::Idle,
        );
        app.reduce(AgentEvent::MessageStart {
            message_id: "a".into(),
            message: Message::Assistant(AssistantMessage {
                content: vec![],
                stop_reason: StopReason::Stop,
                usage: None,
                error_message: None,
            }),
        });
        app.reduce(AgentEvent::MessageUpdate {
            message_id: "a".into(),
            block_index: 0,
            delta: MessageDelta::Text("## Head\n```ru".into()),
            usage: Some(Usage::default()),
        });
        app.reduce(AgentEvent::MessageUpdate {
            message_id: "a".into(),
            block_index: 0,
            delta: MessageDelta::Thinking("reasoning".into()),
            usage: None,
        });
        assert_eq!(app.thinking, "reasoning");
        app.reduce(AgentEvent::SessionFatal {
            error: "disk full".into(),
        });
        assert_eq!(app.thinking, "reasoning");
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn renders_markdown_cjk_and_narrow_terminal() {
        let mut app = AppState::new(
            vec![Message::User(UserMessage::text(
                "# 标题\n\n- 项目\n\n```rust\nfn main() {}\n```",
            ))],
            SessionStatus::Idle,
        );
        app.editor = "多行\n输入".into();
        app.cursor = app.editor.chars().count();
        for (width, height) in [(24, 10), (80, 24), (140, 30)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert!(buffer.content.iter().any(|cell| cell.symbol() == "标"));
        }
    }

    #[test]
    fn cursor_position_uses_terminal_width() {
        assert_eq!(cursor_position("你a", 1), (0, 2));
        assert_eq!(cursor_position("你\n好", 3), (1, 2));
        assert_eq!(cursor_position("\t字", 2), (0, 6));
    }

    #[test]
    fn renders_tables_with_borders_and_cjk_width() {
        let table = "| 项目 | 今日新增 | 简介 |\n| --- | ---: | --- |\n| cordis | 719 | 时空可组合性元框架 |\n| OpenCut | 134 | 开源视频编辑器 |";
        let rendered = markdown_text(table, 32);
        let lines = rendered
            .lines
            .iter()
            .map(|line| line.width())
            .collect::<Vec<_>>();
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.to_string().contains('┌'))
        );
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.to_string().contains('┼'))
        );
        assert!(rendered.lines.iter().all(|line| line.width() <= 32));
        assert!(lines.len() >= 7);
    }
    #[test]
    fn incomplete_markdown_never_panics() {
        for source in ["## Head", "ing\n\n```ru", "st\nfn main() {}\n```"] {
            assert!(!markdown_text(source, 80).lines.is_empty());
        }
    }

    #[test]
    fn transcript_scroll_disables_follow_and_moves_both_directions() {
        let mut app = AppState::new(Vec::new(), SessionStatus::Idle);
        app.scroll = 20;
        app.scroll_up(1);
        assert_eq!(app.scroll, 19);
        assert!(!app.follow);
        app.scroll_down(1);
        assert_eq!(app.scroll, 20);
        assert!(!app.follow);
    }

    #[test]
    fn follow_uses_wrapped_height_and_manual_scroll_sticks() {
        let mut app = AppState::new(
            vec![Message::User(UserMessage::text("长".repeat(200)))],
            SessionStatus::Idle,
        );
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.scroll > 0, "follow must scroll to wrapped bottom");

        app.follow = false;
        app.scroll = app.scroll.saturating_sub(3);
        let manual = app.scroll;
        app.reduce(AgentEvent::MessageEnd {
            message_id: "user".into(),
            message: Message::User(UserMessage::text("new output")),
        });
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(!app.follow);
        assert_eq!(app.scroll, manual);

        app.scroll = u16::MAX;
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.follow, "scrolling back to bottom must resume follow");
    }

    #[test]
    fn finalized_thinking_and_tool_source_remain_visible() {
        let mut app = AppState::new(Vec::new(), SessionStatus::Running);
        app.reduce(AgentEvent::ToolExecutionStart {
            id: "call".into(),
            name: "node".into(),
            input: serde_json::json!({"code":"console.log('kept')"}).to_string(),
        });
        app.reduce(AgentEvent::ToolExecutionEnd {
            id: "call".into(),
            name: "node".into(),
            result: serde_json::json!({}),
            is_error: false,
        });
        app.reduce(AgentEvent::MessageEnd {
            message_id: "assistant".into(),
            message: Message::Assistant(AssistantMessage {
                content: vec![
                    MessageContent::Thinking {
                        thinking: "kept reasoning".into(),
                        signature: None,
                    },
                    MessageContent::ToolUse {
                        id: "call".into(),
                        name: "node".into(),
                        input: serde_json::json!({"code":"console.log('kept')"}).to_string(),
                        custom: false,
                        item_id: None,
                    },
                ],
                stop_reason: StopReason::ToolUse,
                usage: None,
                error_message: None,
            }),
        });
        let rendered = transcript(&app, 80)
            .lines
            .into_iter()
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<String>();
        assert!(rendered.contains("kept reasoning"));
        assert!(rendered.contains("console.log('kept')"));
        assert!(!rendered.contains("node: done"));
    }
    #[test]
    fn editor_preserves_markdown_and_supports_delete_vertical_motion() {
        let mut app = AppState::new(Vec::new(), SessionStatus::Idle);
        app.editor = "# 标题\n第二行".into();
        app.cursor = 3;
        app.move_vertical(1);
        assert_eq!(app.cursor, 8);
        app.cursor = 7;
        app.delete();
        assert_eq!(app.editor, "# 标题\n第二");
        app.cursor = 1;
        let submitted = app.take_editor().unwrap();
        assert_eq!(submitted, "# 标题\n第二");
        assert!(app.editor.is_empty());
    }
}
