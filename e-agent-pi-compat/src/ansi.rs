#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnsiStyle {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCell {
    pub symbol: char,
    pub style: AnsiStyle,
    pub hyperlink: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnsiLine {
    pub cells: Vec<AnsiCell>,
    pub cursor: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiFrame {
    pub width: usize,
    pub lines: Vec<AnsiLine>,
}

fn char_width(ch: char) -> usize {
    if ch.is_control() || ch.is_ascii() {
        return usize::from(!ch.is_control());
    }
    if matches!(
        ch as u32,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
            | 0x20000..=0x3fffd
    ) {
        2
    } else {
        1
    }
}

pub fn parse_frame(lines: impl IntoIterator<Item = impl AsRef<str>>, width: usize) -> AnsiFrame {
    AnsiFrame {
        width,
        lines: lines
            .into_iter()
            .map(|line| parse_line(line.as_ref(), width))
            .collect(),
    }
}

pub fn parse_line(input: &str, width: usize) -> AnsiLine {
    let mut line = AnsiLine::default();
    let mut style = AnsiStyle::default();
    let mut hyperlink = None;
    let mut used = 0usize;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            if ch.is_control() {
                continue;
            }
            let cells = char_width(ch);
            if cells == 0 || used.saturating_add(cells) > width {
                continue;
            }
            line.cells.push(AnsiCell {
                symbol: ch,
                style: style.clone(),
                hyperlink: hyperlink.clone(),
            });
            used += cells;
            continue;
        }

        match chars.next() {
            Some('[') => parse_csi(&mut chars, &mut style),
            Some(']') => parse_osc(&mut chars, &mut hyperlink),
            Some('_') => {
                let payload = read_string_control(&mut chars);
                if payload.as_deref() == Some("pi:c") {
                    line.cursor = Some(used);
                }
            }
            Some('P' | '^' | 'X') => {
                let _ = read_string_control(&mut chars);
            }
            Some(_) | None => {}
        }
    }

    line
}

pub fn plain_line(input: &str, width: usize) -> String {
    parse_line(input, width)
        .cells
        .into_iter()
        .map(|cell| cell.symbol)
        .collect()
}

pub fn plain_lines(lines: impl IntoIterator<Item = impl AsRef<str>>, width: usize) -> String {
    parse_frame(lines, width)
        .lines
        .into_iter()
        .map(|line| line.cells.into_iter().map(|cell| cell.symbol).collect())
        .collect::<Vec<String>>()
        .join("\n")
}

fn parse_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, style: &mut AnsiStyle) {
    let mut sequence = String::new();
    while let Some(ch) = chars.next() {
        if ('@'..='~').contains(&ch) {
            if ch == 'm' {
                parse_sgr(&sequence, style);
            }
            return;
        }
        sequence.push(ch);
    }
}

fn parse_sgr(sequence: &str, style: &mut AnsiStyle) {
    let values = if sequence.is_empty() {
        vec![0]
    } else {
        sequence
            .split(';')
            .map(|value| value.parse::<u16>().unwrap_or(0))
            .collect()
    };
    let mut index = 0;
    while index < values.len() {
        match values[index] {
            0 => *style = AnsiStyle::default(),
            1 => style.bold = true,
            3 => style.italic = true,
            4 => style.underline = true,
            7 => style.inverse = true,
            9 => style.strikethrough = true,
            22 => style.bold = false,
            23 => style.italic = false,
            24 => style.underline = false,
            27 => style.inverse = false,
            29 => style.strikethrough = false,
            30..=37 | 90..=97 => style.foreground = Some(ansi_color(values[index])),
            39 => style.foreground = None,
            40..=47 | 100..=107 => style.background = Some(ansi_color(values[index])),
            49 => style.background = None,
            38 | 48 => {
                let target = values[index] == 38;
                if let Some(color) = parse_extended_color(&values, index + 1) {
                    if target {
                        style.foreground = Some(color.0);
                    } else {
                        style.background = Some(color.0);
                    }
                    index = color.1;
                }
            }
            _ => {}
        }
        index += 1;
    }
}

fn parse_extended_color(values: &[u16], start: usize) -> Option<(u32, usize)> {
    match values.get(start).copied()? {
        5 => Some((u32::from(*values.get(start + 1)?), start + 1)),
        2 => Some((
            (u32::from(*values.get(start + 1)?) << 16)
                | (u32::from(*values.get(start + 2)?) << 8)
                | u32::from(*values.get(start + 3)?),
            start + 3,
        )),
        _ => None,
    }
}

fn ansi_color(code: u16) -> u32 {
    match code {
        30..=37 => 0x01000000 | u32::from(code - 30),
        90..=97 => 0x02000000 | u32::from(code - 90),
        40..=47 => 0x01000000 | u32::from(code - 40),
        100..=107 => 0x02000000 | u32::from(code - 100),
        _ => 0,
    }
}

fn parse_osc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, hyperlink: &mut Option<String>) {
    let Some(payload) = read_string_control(chars) else {
        return;
    };
    let Some(rest) = payload.strip_prefix("8;") else {
        return;
    };
    let Some((_, uri)) = rest.split_once(';') else {
        return;
    };
    *hyperlink = (!uri.is_empty()).then(|| uri.to_owned());
}

fn read_string_control(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut payload = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\u{7}' {
            return Some(payload);
        }
        if ch == '\u{1b}' {
            if chars.next() == Some('\\') {
                return Some(payload);
            }
            continue;
        }
        payload.push(ch);
    }
    Some(payload)
}

#[cfg(test)]
mod tests {
    use super::{parse_frame, parse_line, plain_line, plain_lines};

    #[test]
    fn strips_control_sequences_and_clips_cjk_without_splitting_cells() {
        assert_eq!(plain_line("\x1b[31m你a\x1b[0m", 3), "你a");
        assert_eq!(plain_line("你a", 2), "你");
        assert_eq!(
            plain_line("a\x1b]8;;https://e.example\x1b\\b\x1b]8;;\x1b\\", 2),
            "ab"
        );
        assert_eq!(plain_line("a\x1b_pi:c\x07b", 2), "ab");
    }

    #[test]
    fn parses_styles_links_and_cursor_marker() {
        let line = parse_line(
            "\x1b[1;38;2;1;2;3mA\x1b]8;;https://e.example\x07B\x1b]8;;\x07\x1b_pi:c\x07C",
            8,
        );
        assert_eq!(line.cursor, Some(2));
        assert_eq!(line.cells.len(), 3);
        assert!(line.cells[0].style.bold);
        assert_eq!(line.cells[0].style.foreground, Some(0x010203));
        assert_eq!(
            line.cells[1].hyperlink.as_deref(),
            Some("https://e.example")
        );
        assert!(line.cells[2].hyperlink.is_none());
    }

    #[test]
    fn resets_style_and_hyperlink_at_explicit_boundaries() {
        let frame = parse_frame(
            [
                "\x1b[31mred\x1b[39m plain",
                "\x1b]8;;url\x07link\x1b]8;;\x07x",
            ],
            32,
        );
        assert_eq!(frame.lines.len(), 2);
        assert_ne!(frame.lines[0].cells[0].style.foreground, None);
        assert_eq!(frame.lines[0].cells[4].style.foreground, None);
        assert_eq!(frame.lines[1].cells[0].hyperlink.as_deref(), Some("url"));
        assert_eq!(frame.lines[1].cells[4].hyperlink, None);
    }

    #[test]
    fn isolates_each_line() {
        assert_eq!(plain_lines(["\x1b[1mone", "two\x1b[0m"], 8), "one\ntwo");
    }
}
