use e_agent_tui::{
    component::{ComponentId, CursorAnchor},
    render::{Rect, SemanticFrame, Size, Style},
};
use unicode_width::UnicodeWidthChar;

use crate::ansi::{AnsiFrame, AnsiStyle, parse_frame};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiCachedFrame {
    pub frame: SemanticFrame,
    pub cursor: Option<CursorAnchor>,
}

impl PiCachedFrame {
    pub fn from_ansi_lines(
        component: ComponentId,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
        width: u16,
    ) -> Self {
        let parsed = parse_frame(lines, usize::from(width));
        Self::from_ansi_frame(component, parsed)
    }

    fn from_ansi_frame(component: ComponentId, parsed: AnsiFrame) -> Self {
        let size = Size {
            width: u16::try_from(parsed.width).unwrap_or(u16::MAX),
            height: u16::try_from(parsed.lines.len()).unwrap_or(u16::MAX),
        };
        let clip = Rect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        };
        let mut frame = SemanticFrame::new(size);
        let mut cursor = None;

        for (line_index, line) in parsed.lines.into_iter().enumerate() {
            let Ok(y) = u16::try_from(line_index) else {
                break;
            };
            if let Some(x) = line.cursor.and_then(|x| u16::try_from(x).ok()) {
                cursor = Some(CursorAnchor {
                    component,
                    x,
                    y,
                    visible: true,
                    ime: true,
                });
            }
            let mut x = 0u16;
            for cell in line.cells {
                let width = UnicodeWidthChar::width(cell.symbol).unwrap_or(0) as u16;
                frame.put_cell(
                    clip,
                    x,
                    y,
                    cell.symbol,
                    native_style(&cell.style),
                    cell.hyperlink,
                );
                x = x.saturating_add(width);
            }
        }
        Self { frame, cursor }
    }
}

fn native_style(style: &AnsiStyle) -> Style {
    Style {
        foreground: style.foreground,
        background: style.background,
        bold: style.bold,
        italic: style.italic,
        underline: style.underline,
        inverse: style.inverse,
        strikethrough: style.strikethrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_frame_preserves_style_link_cjk_and_cursor() {
        let cached = PiCachedFrame::from_ansi_lines(
            ComponentId(7),
            ["\x1b[1;3;4;38;2;1;2;3m你\x1b]8;;https://e.example\x07a\x1b]8;;\x07\x1b_pi:c\x07"],
            4,
        );
        assert_eq!(
            cached.frame.size,
            Size {
                width: 4,
                height: 1
            }
        );
        assert_eq!(cached.frame.cells[0].symbol, "你");
        assert_eq!(cached.frame.cells[0].style.foreground, Some(0x010203));
        assert!(cached.frame.cells[0].style.bold);
        assert!(cached.frame.cells[0].style.italic);
        assert!(cached.frame.cells[0].style.underline);
        assert_eq!(cached.frame.cells[2].symbol, "a");
        assert_eq!(
            cached.frame.cells[2].hyperlink.as_deref(),
            Some("https://e.example")
        );
        assert_eq!(
            cached.cursor,
            Some(CursorAnchor {
                component: ComponentId(7),
                x: 3,
                y: 0,
                visible: true,
                ime: true,
            })
        );
    }
}
