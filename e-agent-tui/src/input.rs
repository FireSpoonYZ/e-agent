#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Enter,
    Esc,
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Press,
    Repeat,
    Release,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
    pub kind: KeyKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Up,
    Drag,
    Move,
    ScrollUp,
    ScrollDown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
    pub button: Option<MouseButton>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    Text(String),
    Paste(String),
    Mouse(MouseEvent),
    Resize { columns: u16, rows: u16 },
    FocusGained,
    FocusLost,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandId(pub String);

impl InputEvent {
    pub fn from_crossterm(event: crossterm::event::Event) -> Option<Self> {
        use crossterm::event::{
            Event, KeyCode as C, KeyEventKind as K, MouseButton as B, MouseEventKind as M,
        };
        match event {
            Event::Resize(columns, rows) => Some(Self::Resize { columns, rows }),
            Event::FocusGained => Some(Self::FocusGained),
            Event::FocusLost => Some(Self::FocusLost),
            Event::Paste(text) => Some(Self::Paste(text)),
            Event::Mouse(event) => Some(Self::Mouse(crate::input::MouseEvent {
                kind: match event.kind {
                    M::ScrollUp => MouseKind::ScrollUp,
                    M::ScrollDown => MouseKind::ScrollDown,
                    M::Down(_) => MouseKind::Down,
                    M::Up(_) => MouseKind::Up,
                    M::Drag(_) => MouseKind::Drag,
                    M::Moved => MouseKind::Move,
                    _ => return None,
                },
                column: event.column,
                row: event.row,
                button: match event.kind {
                    M::Down(button) | M::Up(button) | M::Drag(button) => Some(match button {
                        B::Left => MouseButton::Left,
                        B::Right => MouseButton::Right,
                        B::Middle => MouseButton::Middle,
                    }),
                    _ => None,
                },
            })),
            Event::Key(event) => Some(Self::Key(KeyEvent {
                code: match event.code {
                    C::Enter => KeyCode::Enter,
                    C::Esc => KeyCode::Esc,
                    C::Char(ch) => KeyCode::Char(ch),
                    C::Backspace => KeyCode::Backspace,
                    C::Delete => KeyCode::Delete,
                    C::Left => KeyCode::Left,
                    C::Right => KeyCode::Right,
                    C::Up => KeyCode::Up,
                    C::Down => KeyCode::Down,
                    C::Home => KeyCode::Home,
                    C::End => KeyCode::End,
                    C::PageUp => KeyCode::PageUp,
                    C::PageDown => KeyCode::PageDown,
                    _ => return None,
                },
                modifiers: Modifiers {
                    ctrl: event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL),
                    alt: event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::ALT),
                    shift: event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::SHIFT),
                },
                kind: match event.kind {
                    K::Press => KeyKind::Press,
                    K::Repeat => KeyKind::Repeat,
                    K::Release => KeyKind::Release,
                },
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputEvent, MouseButton, MouseKind};

    #[test]
    fn mouse_button_survives_crossterm_normalization() {
        let event = crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
            column: 4,
            row: 7,
            modifiers: crossterm::event::KeyModifiers::CONTROL,
        });
        assert_eq!(
            InputEvent::from_crossterm(event),
            Some(InputEvent::Mouse(super::MouseEvent {
                kind: MouseKind::Down,
                column: 4,
                row: 7,
                button: Some(MouseButton::Right),
            }))
        );
    }
}
