use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// Ghostty TokyoNight's default foreground, paired with the screenshot canvas
/// background in `xtask/assets/ghostty-tokyonight.json`.
const TOKYONIGHT_DEFAULT_FG: (u8, u8, u8) = (192, 202, 245);

pub(super) fn infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(err) => match err {},
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineAnsiStyle {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

pub(super) fn write_buffer_line_ansi<W: Write>(writer: &mut W, buffer: &Buffer) -> io::Result<()> {
    let width = buffer.area.width as usize;
    for row in buffer.content.chunks(width) {
        let mut current: Option<LineAnsiStyle> = None;
        for cell in row {
            let next = LineAnsiStyle {
                fg: cell.fg,
                bg: cell.bg,
                modifier: cell.modifier,
            };
            if current != Some(next) {
                write_line_sgr(writer, next)?;
                current = Some(next);
            }
            writer.write_all(cell.symbol().as_bytes())?;
        }
        writer.write_all(b"\x1b[0m\n")?;
    }
    Ok(())
}

fn write_line_sgr<W: Write>(writer: &mut W, style: LineAnsiStyle) -> io::Result<()> {
    let mut codes: Vec<String> = vec!["0".to_owned()];
    push_modifier_codes(style.modifier, &mut codes);
    push_fg_code(style.fg, &mut codes);
    push_bg_code(style.bg, &mut codes);
    write!(writer, "\x1b[{}m", codes.join(";"))
}

fn push_modifier_codes(modifier: Modifier, codes: &mut Vec<String>) {
    if modifier.contains(Modifier::BOLD) {
        codes.push("1".to_owned());
    }
    if modifier.contains(Modifier::DIM) {
        codes.push("2".to_owned());
    }
    if modifier.contains(Modifier::ITALIC) {
        codes.push("3".to_owned());
    }
    if modifier.contains(Modifier::UNDERLINED) {
        codes.push("4".to_owned());
    }
    if modifier.contains(Modifier::SLOW_BLINK) {
        codes.push("5".to_owned());
    }
    if modifier.contains(Modifier::RAPID_BLINK) {
        codes.push("6".to_owned());
    }
    if modifier.contains(Modifier::REVERSED) {
        codes.push("7".to_owned());
    }
    if modifier.contains(Modifier::HIDDEN) {
        codes.push("8".to_owned());
    }
    if modifier.contains(Modifier::CROSSED_OUT) {
        codes.push("9".to_owned());
    }
}

fn push_fg_code(color: Color, codes: &mut Vec<String>) {
    push_color_code(color, 30, 90, 38, codes);
}

fn push_bg_code(color: Color, codes: &mut Vec<String>) {
    if color != Color::Reset {
        push_color_code(color, 40, 100, 48, codes);
    }
}

fn push_color_code(color: Color, base: u8, bright_base: u8, extended: u8, codes: &mut Vec<String>) {
    let code = match color {
        Color::Reset => {
            let (red, green, blue) = TOKYONIGHT_DEFAULT_FG;
            codes.push(format!("{extended};2;{red};{green};{blue}"));
            return;
        }
        Color::Black => base,
        Color::Red => base + 1,
        Color::Green => base + 2,
        Color::Yellow => base + 3,
        Color::Blue => base + 4,
        Color::Magenta => base + 5,
        Color::Cyan => base + 6,
        Color::Gray => base + 7,
        Color::DarkGray => bright_base,
        Color::LightRed => bright_base + 1,
        Color::LightGreen => bright_base + 2,
        Color::LightYellow => bright_base + 3,
        Color::LightBlue => bright_base + 4,
        Color::LightMagenta => bright_base + 5,
        Color::LightCyan => bright_base + 6,
        Color::White => bright_base + 7,
        Color::Rgb(red, green, blue) => {
            codes.push(format!("{extended};2;{red};{green};{blue}"));
            return;
        }
        Color::Indexed(index) => {
            codes.push(format!("{extended};5;{index}"));
            return;
        }
    };
    codes.push(code.to_string());
}
