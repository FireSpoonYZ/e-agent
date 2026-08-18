use std::collections::{BTreeMap, VecDeque};

use e_agent_core::{Message, SessionStatus};

use crate::{
    input::{CommandId, InputEvent, KeyCode},
    ui_protocol::{DialogRequest, ExtensionId, Generation, Notification, OverlayId, RequestId},
};

#[derive(Debug, Clone)]
pub struct ToolState {
    pub name: String,
    pub status: String,
    pub input: String,
    pub update: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct PendingDialog {
    pub request: RequestId,
    pub dialog: DialogRequest,
    pub text: String,
    pub cursor: usize,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct OverlayState {
    pub extension: ExtensionId,
    pub id: OverlayId,
    pub generation: Generation,
    pub content: String,
    pub hidden: bool,
    pub capturing: bool,
}

#[derive(Debug, Default)]
pub struct UiState {
    pub dialog: Option<PendingDialog>,
    pub contributions: BTreeMap<(ExtensionId, String, String), String>,
    pub frames: BTreeMap<
        (ExtensionId, String),
        (
            crate::render::SemanticFrame,
            Option<crate::component::CursorAnchor>,
        ),
    >,
    pub notifications: VecDeque<Notification>,
    pub overlays: Vec<OverlayState>,
    pub custom_editor: Option<(ExtensionId, String, String)>,
    pub theme_generation: u64,
    pub keybindings: BTreeMap<String, CommandId>,
    pub keybinding_conflicts: Vec<String>,
    pub terminal_size: (u16, u16),
    pub terminal_focused: bool,
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
    pub ui: UiState,
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
            ui: UiState::default(),
        }
    }

    pub fn reduce(&mut self, event: e_agent_core::AgentEvent) {
        crate::reducer::reduce(self, crate::reducer::AppEvent::Session(event));
    }

    pub(crate) fn insert(&mut self, ch: char) {
        insert_text(&mut self.editor, &mut self.cursor, &ch.to_string());
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        insert_text(&mut self.editor, &mut self.cursor, text);
    }

    pub(crate) fn backspace(&mut self) {
        backspace(&mut self.editor, &mut self.cursor);
    }

    pub(crate) fn delete(&mut self) {
        delete(&mut self.editor, self.cursor);
    }

    pub(crate) fn move_vertical(&mut self, direction: isize) {
        move_vertical(&self.editor, &mut self.cursor, direction);
    }

    pub(crate) fn scroll_up(&mut self, amount: u16) {
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub(crate) fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub(crate) fn take_editor(&mut self) -> Option<String> {
        if self.editor.trim().is_empty() {
            return None;
        }
        self.cursor = 0;
        Some(std::mem::take(&mut self.editor))
    }

    pub fn contribution_lines(&self, slot: &str) -> impl Iterator<Item = &str> {
        self.ui
            .contributions
            .iter()
            .filter(move |((_, item_slot, _), _)| item_slot == slot)
            .flat_map(|(_, content)| content.lines())
    }

    pub fn resolve_command(&self, event: &InputEvent) -> Option<&CommandId> {
        self.ui.keybindings.get(&key_name(event)?)
    }
}

pub(crate) fn insert_text(text: &mut String, cursor: &mut usize, inserted: &str) {
    let byte = text
        .char_indices()
        .nth(*cursor)
        .map_or(text.len(), |(index, _)| index);
    text.insert_str(byte, inserted);
    *cursor += inserted.chars().count();
}

pub(crate) fn backspace(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let mut chars = text.chars().collect::<Vec<_>>();
    chars.remove(*cursor - 1);
    *cursor -= 1;
    *text = chars.into_iter().collect();
}

pub(crate) fn delete(text: &mut String, cursor: usize) {
    let mut chars = text.chars().collect::<Vec<_>>();
    if cursor < chars.len() {
        chars.remove(cursor);
        *text = chars.into_iter().collect();
    }
}

pub(crate) fn move_vertical(text: &str, cursor: &mut usize, direction: isize) {
    let chars = text.chars().collect::<Vec<_>>();
    let line_start = |index: usize| {
        chars[..index]
            .iter()
            .rposition(|ch| *ch == '\n')
            .map_or(0, |pos| pos + 1)
    };
    let current_start = line_start(*cursor);
    let column = *cursor - current_start;
    if direction < 0 {
        if current_start == 0 {
            return;
        }
        let previous_end = current_start - 1;
        let previous_start = line_start(previous_end);
        *cursor = previous_start + column.min(previous_end - previous_start);
    } else if let Some(current_end) = chars[*cursor..]
        .iter()
        .position(|ch| *ch == '\n')
        .map(|offset| *cursor + offset)
    {
        let next_start = current_end + 1;
        let next_end = chars[next_start..]
            .iter()
            .position(|ch| *ch == '\n')
            .map_or(chars.len(), |offset| next_start + offset);
        *cursor = next_start + column.min(next_end - next_start);
    }
}

fn key_name(event: &InputEvent) -> Option<String> {
    let InputEvent::Key(event) = event else {
        return None;
    };
    let mut value = String::new();
    if event.modifiers.ctrl {
        value.push_str("ctrl+");
    }
    if event.modifiers.alt {
        value.push_str("alt+");
    }
    if event.modifiers.shift {
        value.push_str("shift+");
    }
    value.push_str(match event.code {
        KeyCode::Enter => "enter",
        KeyCode::Esc => "esc",
        KeyCode::Char(ch) => return Some(format!("{value}{ch}")),
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pageup",
        KeyCode::PageDown => "pagedown",
    });
    Some(value)
}
