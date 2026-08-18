use std::collections::BTreeSet;
use unicode_width::UnicodeWidthChar;

use crate::{
    component::{Component, ComponentId, CursorAnchor, EditorComponent, RenderContext},
    state::AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    Main,
    Alternate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Rect {
    fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    pub symbol: String,
    pub style: Style,
    pub hyperlink: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticFrame {
    pub size: Size,
    pub cells: Vec<Cell>,
}
impl SemanticFrame {
    pub fn new(size: Size) -> Self {
        Self {
            size,
            cells: vec![Cell::default(); size.width as usize * size.height as usize],
        }
    }
    pub fn put(&mut self, clip: Rect, x: u16, y: u16, symbol: char, style: Style) {
        self.put_cell(clip, x, y, symbol, style, None);
    }
    pub fn put_cell(
        &mut self,
        clip: Rect,
        x: u16,
        y: u16,
        symbol: char,
        style: Style,
        hyperlink: Option<String>,
    ) {
        if !clip.contains(x, y) || x >= self.size.width || y >= self.size.height {
            return;
        }
        let width = UnicodeWidthChar::width(symbol).unwrap_or(0) as u16;
        if width == 0
            || x.saturating_add(width) > clip.x.saturating_add(clip.width)
            || x.saturating_add(width) > self.size.width
        {
            return;
        }
        self.cells[y as usize * self.size.width as usize + x as usize] = Cell {
            symbol: symbol.to_string(),
            style,
            hyperlink,
        };
        for offset in 1..width {
            self.cells[y as usize * self.size.width as usize + (x + offset) as usize] = Cell {
                symbol: String::new(),
                style,
                hyperlink: None,
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Damage {
    #[default]
    None,
    Components,
    Full,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    State,
    Resize,
    Theme,
    Capabilities,
    Full,
}
#[derive(Debug)]
pub struct RenderSnapshot<'a> {
    pub size: Size,
    pub state: &'a mut AppState,
}
#[derive(Debug, Clone, Default)]
pub struct RendererPrivateState {
    pub family: Option<String>,
}

#[derive(Debug, Default)]
pub struct RenderScheduler {
    dirty: Damage,
    components: BTreeSet<ComponentId>,
}
impl RenderScheduler {
    pub fn request(&mut self, damage: Damage) {
        self.dirty = match (self.dirty, damage) {
            (Damage::Full, _) | (_, Damage::Full) => Damage::Full,
            (Damage::Components, _) | (_, Damage::Components) => Damage::Components,
            _ => Damage::None,
        };
    }
    pub fn invalidate(&mut self, id: ComponentId, _reason: InvalidationReason) {
        self.components.insert(id);
        self.request(Damage::Components);
    }
    pub fn has_damage(&self) -> bool {
        self.dirty != Damage::None
    }
    pub fn take(&mut self) -> Damage {
        self.components.clear();
        std::mem::take(&mut self.dirty)
    }
}

pub trait Renderer {
    fn components(&mut self) -> &mut ComponentRegistry;
    fn render(&mut self, snapshot: &mut RenderSnapshot<'_>, damage: Damage) -> anyhow::Result<()>;
    fn suspend(&mut self) -> anyhow::Result<RendererPrivateState> {
        Ok(RendererPrivateState::default())
    }
    fn resume(&mut self, _state: Option<RendererPrivateState>) -> anyhow::Result<()> {
        Ok(())
    }
    fn shutdown(&mut self, preserve_screen: bool) -> anyhow::Result<()>;
}

pub struct ComponentRegistry {
    components: Vec<Box<dyn Component>>,
    focus: crate::component::FocusManager,
}
impl Default for ComponentRegistry {
    fn default() -> Self {
        let id = ComponentId(1);
        let mut focus = crate::component::FocusManager::default();
        focus.mount(id);
        focus.focus(id);
        Self {
            components: vec![Box::new(EditorComponent::new(id))],
            focus,
        }
    }
}
impl ComponentRegistry {
    pub fn focus(&self) -> &crate::component::FocusManager {
        &self.focus
    }
    pub fn dispatch(&mut self, _state: &mut AppState, _event: &crate::input::InputEvent) -> bool {
        false
    }
    pub fn invalidate_all(&mut self, reason: InvalidationReason) {
        for component in &mut self.components {
            component.invalidate(reason);
        }
    }
    pub fn render(
        &mut self,
        state: &AppState,
        size: Size,
    ) -> (SemanticFrame, Option<CursorAnchor>) {
        let mut frame = SemanticFrame::new(size);
        let mut cursor = None;
        let area = Rect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        };
        for component in &mut self.components {
            let mut context = RenderContext {
                frame: &mut frame,
                clip: area,
                style: Style::default(),
                cursor: &mut cursor,
            };
            component.render(state, &mut context, area);
        }
        (frame, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scheduler_coalesces_damage() {
        let mut scheduler = RenderScheduler::default();
        scheduler.request(Damage::Components);
        scheduler.request(Damage::Full);
        assert_eq!(scheduler.take(), Damage::Full);
        assert_eq!(scheduler.take(), Damage::None);
    }
    #[test]
    fn semantic_frame_clips_and_accounts_for_cjk_width() {
        let mut frame = SemanticFrame::new(Size {
            width: 4,
            height: 1,
        });
        let clip = Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        };
        frame.put(clip, 1, 0, '你', Style::default());
        frame.put(clip, 3, 0, 'x', Style::default());
        assert_eq!(frame.cells[1].symbol, "你");
        assert_eq!(frame.cells[2].symbol, "");
        assert_eq!(frame.cells[3].symbol, "");
    }
}
