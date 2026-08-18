pub mod broker;
pub mod component;
pub mod input;
pub mod reducer;
pub mod render;
pub mod runner;
pub mod state;
pub mod terminal;
pub mod ui_protocol;

use anyhow::Result;
use crossterm::event::EventStream;
use e_agent_core::{Message, MessageContent, SessionAttachment, SessionStatus, StopReason};
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
use std::{borrow::Cow, io, time::Duration};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub use state::{AppState, ToolState};

struct RatatuiRenderer {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    components: render::ComponentRegistry,
}

impl RatatuiRenderer {
    fn new() -> Result<Self> {
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            components: render::ComponentRegistry::default(),
        })
    }
}

impl crate::render::Renderer for RatatuiRenderer {
    fn components(&mut self) -> &mut render::ComponentRegistry {
        &mut self.components
    }

    fn render(
        &mut self,
        snapshot: &mut crate::render::RenderSnapshot<'_>,
        _damage: crate::render::Damage,
    ) -> Result<()> {
        let size = self.terminal.size()?;
        snapshot.size = crate::render::Size {
            width: size.width,
            height: size.height,
        };
        let _semantic = self.components.render(snapshot.state, snapshot.size);
        self.terminal
            .draw(|frame| render(frame, &mut *snapshot.state))?;
        Ok(())
    }
    fn shutdown(&mut self, _preserve_screen: bool) -> Result<()> {
        Ok(())
    }
}

pub async fn run(attachment: SessionAttachment) -> Result<()> {
    run_with_broker(attachment, None).await
}

pub async fn run_with_broker(
    attachment: SessionAttachment,
    broker: Option<crate::broker::UiBrokerServer>,
) -> Result<()> {
    run_with_options(attachment, broker, crate::render::ScreenMode::Alternate).await
}

pub async fn run_with_options(
    mut attachment: SessionAttachment,
    broker: Option<crate::broker::UiBrokerServer>,
    screen_mode: crate::render::ScreenMode,
) -> Result<()> {
    let renderer = Box::new(RatatuiRenderer::new()?);
    run_with_renderer_and_broker(&mut attachment, renderer, broker, screen_mode).await
}

async fn run_with_renderer_and_broker(
    attachment: &mut SessionAttachment,
    mut renderer: Box<dyn crate::render::Renderer>,
    mut broker: Option<crate::broker::UiBrokerServer>,
    screen_mode: crate::render::ScreenMode,
) -> Result<()> {
    let _terminal =
        terminal::TerminalSession::start(terminal::CrosstermDriver::default(), screen_mode)?;
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut app = AppState::new(std::mem::take(&mut attachment.messages), attachment.status);
    let mut scheduler = render::RenderScheduler::default();
    scheduler.request(render::Damage::Full);
    draw(&mut *renderer, &mut app, scheduler.take())?;

    'event_loop: loop {
        tokio::select! {
            event = attachment.events.recv() => match event {
                Ok(event) => {
                    reducer::reduce(&mut app, reducer::AppEvent::Session(event));
                    scheduler.request(render::Damage::Components);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    reducer::reduce(&mut app, reducer::AppEvent::ObserverLagged(skipped));
                    scheduler.request(render::Damage::Full);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            event = input.next() => {
                let Some(Ok(event)) = event else { break };
                if let Some(event) = input::InputEvent::from_crossterm(event) {
                    let consumed = broker.as_ref().is_some_and(|server| {
                        server.reply_input(event.clone())
                            || (app.ui.custom_editor.is_some()
                                && server.publish_input(event.clone()))
                    });
                    if !consumed {
                        if let Some(server) = broker.as_ref() {
                            server.publish_input(event.clone());
                        }
                        let effects = reducer::reduce(&mut app, reducer::AppEvent::Input(event));
                        if runner::execute_effects(&attachment.handle, broker.as_ref(), effects).await? { break 'event_loop; }
                    }
                    scheduler.request(render::Damage::Components);
                }
            }
            envelope = async { broker.as_mut().expect("guarded").recv().await }, if broker.is_some() => {
                if let Some(envelope) = envelope {
                    if matches!(
                        envelope.operation,
                        crate::ui_protocol::UiOperation::TerminalInput { .. }
                    ) {
                        broker
                            .as_ref()
                            .expect("guarded")
                            .queue_input_poll(envelope.request);
                    } else {
                        let default = broker.as_ref().expect("guarded").default_reply(&envelope);
                        if matches!(default, crate::ui_protocol::UiReply::Ack) {
                            let effects = reducer::reduce(&mut app, reducer::AppEvent::Ui(envelope));
                            runner::execute_effects(&attachment.handle, broker.as_ref(), effects).await?;
                            renderer.components().invalidate_all(crate::render::InvalidationReason::State);
                            scheduler.request(render::Damage::Components);
                        } else {
                            broker.as_ref().expect("guarded").reply(envelope.request, default);
                        }
                    }
                }
            }
            _ = tick.tick(), if scheduler.has_damage() => draw(&mut *renderer, &mut app, scheduler.take())?,
        }
    }
    let effects = reducer::reduce(&mut app, reducer::AppEvent::Shutdown);
    runner::execute_effects(&attachment.handle, broker.as_ref(), effects).await?;
    renderer.shutdown(false)?;
    Ok(())
}

fn draw(
    renderer: &mut dyn crate::render::Renderer,
    app: &mut AppState,
    damage: crate::render::Damage,
) -> Result<()> {
    renderer.render(
        &mut crate::render::RenderSnapshot {
            size: crate::render::Size {
                width: 0,
                height: 0,
            },
            state: app,
        },
        damage,
    )
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut AppState) {
    let area = frame.area();
    let header_height = app.contribution_lines("header").count() as u16;
    let above_height = app.contribution_lines("widget").count() as u16;
    let footer_height = app.contribution_lines("footer").count() as u16;
    let editor_height =
        (app.editor.split('\n').count() as u16 + 2).clamp(3, area.height.saturating_sub(2).max(3));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(above_height),
            Constraint::Length(editor_height),
            Constraint::Length(footer_height),
            Constraint::Length(1),
        ])
        .split(area);

    if header_height > 0 {
        frame.render_widget(
            Paragraph::new(
                app.contribution_lines("header")
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            chunks[0],
        );
    }
    let transcript = transcript(app, chunks[1].width as usize);
    let transcript = Paragraph::new(transcript).wrap(Wrap { trim: false });
    let line_count = transcript.line_count(chunks[1].width) as u16;
    let max_scroll = line_count.saturating_sub(chunks[1].height);
    if app.follow {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
        if app.scroll == max_scroll {
            app.follow = true;
        }
    }
    frame.render_widget(transcript.scroll((app.scroll, 0)), chunks[1]);

    if above_height > 0 {
        frame.render_widget(
            Paragraph::new(
                app.contribution_lines("widget")
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            chunks[2],
        );
    }
    let editor_text = app
        .ui
        .custom_editor
        .as_ref()
        .map_or(app.editor.as_str(), |(_, _, content)| content.as_str());
    let editor = Paragraph::new(editor_text).block(Block::default().borders(Borders::TOP));
    frame.render_widget(editor, chunks[3]);
    let (row, col) = cursor_position(&app.editor, app.cursor);
    let x = chunks[3]
        .x
        .saturating_add(col as u16)
        .min(chunks[1].right().saturating_sub(1));
    let y = chunks[3]
        .y
        .saturating_add(1 + row as u16)
        .min(chunks[1].bottom().saturating_sub(1));
    frame.set_cursor_position(Position::new(x, y));

    if footer_height > 0 {
        frame.render_widget(
            Paragraph::new(
                app.contribution_lines("footer")
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            chunks[4],
        );
    }
    let mut status = app.fatal_error.clone().unwrap_or_else(|| match app.status {
        SessionStatus::Idle => "Idle".into(),
        SessionStatus::Running => "Running".into(),
        SessionStatus::Fatal => "Fatal".into(),
        SessionStatus::Closed => "Closed".into(),
    });
    let extension_status = app
        .contribution_lines("status")
        .collect::<Vec<_>>()
        .join(" · ");
    if !extension_status.is_empty() {
        status.push_str(" · ");
        status.push_str(&extension_status);
    }
    let style = if app.status == SessionStatus::Fatal {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(Paragraph::new(Line::styled(status, style)), chunks[5]);

    if let Some(notification) = app.ui.notifications.back() {
        let width = area.width.saturating_sub(4).min(60);
        let popup = ratatui::layout::Rect::new(
            area.right().saturating_sub(width + 1),
            1,
            width,
            3.min(area.height),
        );
        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.render_widget(
            Paragraph::new(notification.message.as_str()).block(Block::bordered()),
            popup,
        );
    }
    if let Some(dialog) = &app.ui.dialog {
        let width = area.width.saturating_sub(4).min(60);
        let height = match dialog.dialog {
            crate::ui_protocol::DialogRequest::Select { ref options, .. } => {
                (options.len() as u16 + 2).min(area.height)
            }
            _ => 5.min(area.height),
        };
        let popup = ratatui::layout::Rect::new(
            area.x + (area.width.saturating_sub(width) / 2),
            area.y + (area.height.saturating_sub(height) / 2),
            width,
            height,
        );
        let text = match &dialog.dialog {
            crate::ui_protocol::DialogRequest::Select { title, options } => format!(
                "{title}\n{}",
                options
                    .iter()
                    .enumerate()
                    .map(|(index, value)| format!(
                        "{} {value}",
                        if index == dialog.selected { '›' } else { ' ' }
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            crate::ui_protocol::DialogRequest::Confirm { title, message } => {
                format!("{title}\n{message}\n[y/n]")
            }
            crate::ui_protocol::DialogRequest::Input { title, placeholder } => format!(
                "{title}\n{}",
                if dialog.text.is_empty() {
                    placeholder
                } else {
                    &dialog.text
                }
            ),
            crate::ui_protocol::DialogRequest::Editor { title, .. } => {
                format!("{title}\n{}", dialog.text)
            }
        };
        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.render_widget(Paragraph::new(text).block(Block::bordered()), popup);
    }
    for (index, overlay) in app
        .ui
        .overlays
        .iter()
        .filter(|overlay| !overlay.hidden)
        .enumerate()
    {
        let width = area.width.saturating_sub(4 + index as u16 * 2).min(60);
        let height = 5.min(area.height);
        let popup = ratatui::layout::Rect::new(
            area.x + (area.width.saturating_sub(width) / 2) + index as u16,
            area.y + (area.height.saturating_sub(height) / 2) + index as u16,
            width,
            height,
        );
        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.render_widget(
            Paragraph::new(overlay.content.as_str()).block(Block::bordered()),
            popup,
        );
    }
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
    for (frame, _) in app.ui.frames.values() {
        lines.extend(semantic_frame_lines(frame));
    }
    Text::from(lines)
}

fn semantic_frame_lines(frame: &crate::render::SemanticFrame) -> Vec<Line<'static>> {
    (0..frame.size.height)
        .map(|row| {
            let start = usize::from(row) * usize::from(frame.size.width);
            let end = start + usize::from(frame.size.width);
            let spans = frame.cells[start..end]
                .iter()
                .filter(|cell| !cell.symbol.is_empty())
                .map(|cell| {
                    let mut style = Style::default();
                    if let Some(color) = cell.style.foreground {
                        style = style.fg(semantic_color(color));
                    }
                    if let Some(color) = cell.style.background {
                        style = style.bg(semantic_color(color));
                    }
                    for (enabled, modifier) in [
                        (cell.style.bold, Modifier::BOLD),
                        (cell.style.italic, Modifier::ITALIC),
                        (cell.style.underline, Modifier::UNDERLINED),
                        (cell.style.inverse, Modifier::REVERSED),
                        (cell.style.strikethrough, Modifier::CROSSED_OUT),
                    ] {
                        if enabled {
                            style = style.add_modifier(modifier);
                        }
                    }
                    Span::styled(cell.symbol.clone(), style)
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

fn semantic_color(value: u32) -> Color {
    let palette = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
    ];
    match value & 0xff00_0000 {
        0x0100_0000 => palette[(value & 7) as usize],
        0x0200_0000 => match value & 7 {
            0 => Color::DarkGray,
            1 => Color::LightRed,
            2 => Color::LightGreen,
            3 => Color::LightYellow,
            4 => Color::LightBlue,
            5 => Color::LightMagenta,
            6 => Color::LightCyan,
            _ => Color::White,
        },
        _ => Color::Rgb(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        ),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use e_agent_core::{AgentEvent, MessageDelta, UserMessage};
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
