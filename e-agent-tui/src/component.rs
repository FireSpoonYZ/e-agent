use crate::{
    input::InputEvent,
    render::{InvalidationReason, Rect, SemanticFrame, Style},
    state::AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComponentId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAnchor {
    pub component: ComponentId,
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub ime: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputResult {
    Consumed,
    Bubble,
}

pub struct RenderContext<'a> {
    pub frame: &'a mut SemanticFrame,
    pub clip: Rect,
    pub style: Style,
    pub(crate) cursor: &'a mut Option<CursorAnchor>,
}
impl RenderContext<'_> {
    pub fn set_cursor(&mut self, cursor: CursorAnchor) {
        *self.cursor = Some(cursor);
    }
}

pub trait Component {
    fn id(&self) -> ComponentId;
    fn render(&mut self, state: &AppState, context: &mut RenderContext<'_>, area: Rect);
    fn input(&mut self, _state: &mut AppState, _event: &InputEvent) -> InputResult {
        InputResult::Bubble
    }
    fn focus_changed(&mut self, _focused: bool) {}
    fn invalidate(&mut self, _reason: InvalidationReason) {}
}

pub struct EditorComponent {
    id: ComponentId,
}
impl EditorComponent {
    pub fn new(id: ComponentId) -> Self {
        Self { id }
    }
}
impl Component for EditorComponent {
    fn id(&self) -> ComponentId {
        self.id
    }
    fn input(&mut self, state: &mut AppState, event: &InputEvent) -> InputResult {
        match event {
            InputEvent::Paste(text) | InputEvent::Text(text) => {
                for ch in text.chars() {
                    state.insert(ch);
                }
                InputResult::Consumed
            }
            _ => InputResult::Bubble,
        }
    }
    fn render(&mut self, state: &AppState, context: &mut RenderContext<'_>, area: Rect) {
        let (mut x, mut y) = (area.x, area.y);
        for ch in state.editor.chars() {
            if ch == '\n' {
                x = area.x;
                y = y.saturating_add(1);
            } else {
                context.frame.put(context.clip, x, y, ch, context.style);
                x = x
                    .saturating_add(unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16);
            }
        }
        let (row, column) = crate::cursor_position(&state.editor, state.cursor);
        context.set_cursor(CursorAnchor {
            component: self.id,
            x: area.x.saturating_add(column as u16),
            y: area.y.saturating_add(row as u16),
            visible: true,
            ime: true,
        });
    }
}

#[derive(Debug, Default)]
pub struct FocusManager {
    focused: Option<ComponentId>,
    mounted: std::collections::BTreeSet<ComponentId>,
    history: Vec<ComponentId>,
}
impl FocusManager {
    pub fn mount(&mut self, id: ComponentId) {
        self.mounted.insert(id);
    }
    pub fn remove(&mut self, id: ComponentId) {
        self.mounted.remove(&id);
        self.history.retain(|item| *item != id);
        if self.focused == Some(id) {
            self.focused = self
                .history
                .iter()
                .rev()
                .copied()
                .find(|item| self.mounted.contains(item));
        }
    }
    pub fn focus(&mut self, id: ComponentId) -> bool {
        if !self.mounted.contains(&id) {
            return false;
        }
        if let Some(previous) = self.focused.filter(|previous| *previous != id) {
            self.history.push(previous);
        }
        self.focused = Some(id);
        true
    }
    pub fn focused(&self) -> Option<ComponentId> {
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removing_focus_restores_previous_mounted_component() {
        let mut focus = FocusManager::default();
        focus.mount(ComponentId(1));
        focus.mount(ComponentId(2));
        assert!(focus.focus(ComponentId(1)));
        assert!(focus.focus(ComponentId(2)));
        focus.remove(ComponentId(2));
        assert_eq!(focus.focused(), Some(ComponentId(1)));
        assert!(!focus.focus(ComponentId(3)));
    }
}
