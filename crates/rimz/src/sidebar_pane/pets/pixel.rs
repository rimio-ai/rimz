pub(crate) mod probe;

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;

use super::PetPixelView;
use super::frames::RgbaImage;

const ESC: u8 = 0x1b;
const CHUNK_SIZE: usize = 4096;
const IMAGE_ID_COLOR_MASK: u32 = 0x00ff_ffff;
const PLACEHOLDER: char = '\u{10eeee}';

const ROW_COLUMN_DIACRITICS: [char; 32] = [
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
];

#[derive(Debug)]
pub(crate) struct PixelPainter {
    id_base: u32,
    wrap: bool,
    pet_id: Option<String>,
    transmitted: BTreeSet<usize>,
    last_rect: Option<Rect>,
}

impl Default for PixelPainter {
    fn default() -> Self {
        Self::new(true)
    }
}

impl PixelPainter {
    pub(crate) fn new(wrap: bool) -> Self {
        Self::with_id_base(runtime_image_id_base(), wrap)
    }

    pub(crate) fn with_id_base(id_base: u32, wrap: bool) -> Self {
        Self {
            id_base: id_base & IMAGE_ID_COLOR_MASK,
            wrap,
            pet_id: None,
            transmitted: BTreeSet::new(),
            last_rect: None,
        }
    }

    pub(crate) fn runtime_id_base() -> u32 {
        runtime_image_id_base()
    }

    pub(crate) fn needs_full_redraw(&self, next: Option<(Rect, &PetPixelView)>) -> bool {
        let Some(last_rect) = self.last_rect else {
            return false;
        };
        match next {
            Some((rect, pixel)) => {
                last_rect != rect || self.pet_id.as_deref() != Some(pixel.pet_id.as_str())
            }
            None => true,
        }
    }

    pub(crate) fn paint<W: Write>(
        &mut self,
        writer: &mut W,
        rect: Rect,
        pixel: &PetPixelView,
        frame: &RgbaImage,
    ) -> io::Result<()> {
        let pet_changed = self.pet_id.as_deref() != Some(pixel.pet_id.as_str());
        let rect_changed = self.last_rect.is_some_and(|last| last != rect);
        if pet_changed || rect_changed {
            self.delete_transmitted(writer)?;
        }
        if pet_changed {
            self.pet_id = Some(pixel.pet_id.clone());
        }

        let image_id = self.image_id(pixel.sprite_index);
        if self.transmitted.insert(pixel.sprite_index) {
            for chunk in transmit_chunks(image_id, frame) {
                writer.write_all(&self.wrap_payload(&chunk))?;
            }
        }
        writer.write_all(&self.wrap_payload(&virtual_place(image_id, rect.width, rect.height)))?;
        writer.write_all(&placeholder_grid(image_id, rect))?;
        writer.flush()?;
        self.last_rect = Some(rect);
        Ok(())
    }

    pub(crate) fn hide_after_draw<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        // This runs after ratatui draws the frame. Delete only kitty images here;
        // blanking cells would desync ratatui's diff buffer from the terminal.
        self.last_rect = None;
        self.delete_transmitted(writer)?;
        self.pet_id = None;
        writer.flush()
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if let Some(rect) = self.last_rect.take() {
            clear_rect(writer, rect)?;
        }
        self.delete_transmitted(writer)?;
        self.pet_id = None;
        writer.flush()
    }

    fn delete_transmitted<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let sprite_indexes = std::mem::take(&mut self.transmitted);
        for sprite_index in sprite_indexes {
            writer.write_all(&self.wrap_payload(&delete(self.image_id(sprite_index))))?;
        }
        Ok(())
    }

    fn image_id(&self, sprite_index: usize) -> u32 {
        let id = self.id_base.wrapping_add(sprite_index as u32) & IMAGE_ID_COLOR_MASK;
        id.max(1)
    }

    fn wrap_payload(&self, payload: &[u8]) -> Vec<u8> {
        wrap_pixel_payload(payload, self.wrap)
    }
}

fn runtime_image_id_base() -> u32 {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let mixed = pid.rotate_left(8) ^ nanos;
    let base = mixed & IMAGE_ID_COLOR_MASK;
    if base == 0 { 0x520000 } else { base }
}

pub fn transmit_rgba_chunks(image_id: u32, width: u32, height: u32, data: &[u8]) -> Vec<Vec<u8>> {
    let payload = base64(data);
    if payload.len() <= CHUNK_SIZE {
        return vec![kitty_escape(
            &format!("a=t,f=32,s={},v={},i={},q=2", width, height, image_id),
            payload.as_bytes(),
        )];
    }

    let mut out = Vec::new();
    let chunk_count = payload.len().div_ceil(CHUNK_SIZE);
    for (index, chunk) in payload.as_bytes().chunks(CHUNK_SIZE).enumerate() {
        let more = usize::from(index + 1 < chunk_count);
        let control = if index == 0 {
            format!(
                "a=t,f=32,s={},v={},i={},q=2,m={more}",
                width, height, image_id
            )
        } else {
            format!("m={more}")
        };
        out.push(kitty_escape(&control, chunk));
    }
    out
}

fn transmit_chunks(image_id: u32, image: &RgbaImage) -> Vec<Vec<u8>> {
    transmit_rgba_chunks(image_id, image.width, image.height, &image.data)
}

pub fn virtual_place(image_id: u32, cols: u16, rows: u16) -> Vec<u8> {
    kitty_escape(&format!("a=p,U=1,i={image_id},c={cols},r={rows},q=2"), &[])
}

pub(super) fn delete(image_id: u32) -> Vec<u8> {
    kitty_escape(&format!("a=d,d=i,i={image_id},q=2"), &[])
}

fn kitty_escape(control: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1b_G".to_vec();
    out.extend_from_slice(control.as_bytes());
    out.push(b';');
    out.extend_from_slice(payload);
    out.extend_from_slice(b"\x1b\\");
    out
}

pub(super) fn tmux_passthrough(payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for byte in payload {
        if *byte == ESC {
            out.push(ESC);
        }
        out.push(*byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

pub fn wrap_pixel_payload(payload: &[u8], wrap: bool) -> Vec<u8> {
    if wrap {
        tmux_passthrough(payload)
    } else {
        payload.to_vec()
    }
}

fn placeholder_grid(image_id: u32, rect: Rect) -> Vec<u8> {
    let mut out = Vec::new();
    push_image_id_color(&mut out, image_id);
    for row in 0..rect.height {
        push_fmt(
            &mut out,
            format_args!("\x1b[{};{}H", rect.y + row + 1, rect.x + 1),
        );
        for col in 0..rect.width {
            push_placeholder_cell(&mut out, row, col);
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

pub fn inline_placeholder_row(image_id: u32, row: u16, cols: u16) -> Vec<u8> {
    let mut out = Vec::new();
    push_image_id_color(&mut out, image_id);
    for col in 0..cols {
        push_placeholder_cell(&mut out, row, col);
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

fn push_image_id_color(out: &mut Vec<u8>, image_id: u32) {
    let red = (image_id >> 16) & 0xff;
    let green = (image_id >> 8) & 0xff;
    let blue = image_id & 0xff;
    push_fmt(out, format_args!("\x1b[38;2;{red};{green};{blue}m"));
}

fn push_placeholder_cell(out: &mut Vec<u8>, row: u16, col: u16) {
    let mut buf = [0; 4];
    out.extend_from_slice(PLACEHOLDER.encode_utf8(&mut buf).as_bytes());
    let mut buf = [0; 4];
    out.extend_from_slice(diacritic(row).encode_utf8(&mut buf).as_bytes());
    let mut buf = [0; 4];
    out.extend_from_slice(diacritic(col).encode_utf8(&mut buf).as_bytes());
}

fn diacritic(value: u16) -> char {
    ROW_COLUMN_DIACRITICS[usize::from(value).min(ROW_COLUMN_DIACRITICS.len() - 1)]
}

fn clear_rect<W: Write>(writer: &mut W, rect: Rect) -> io::Result<()> {
    writer.write_all(b"\x1b[0m")?;
    let blank = " ".repeat(usize::from(rect.width));
    for row in 0..rect.height {
        write!(writer, "\x1b[{};{}H{blank}", rect.y + row + 1, rect.x + 1)?;
    }
    Ok(())
}

fn push_fmt(out: &mut Vec<u8>, args: std::fmt::Arguments<'_>) {
    out.write_fmt(args)
        .expect("writing formatted bytes to Vec cannot fail");
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(data: Vec<u8>) -> RgbaImage {
        RgbaImage {
            width: 1,
            height: 1,
            data,
        }
    }

    #[test]
    fn transmit_encodes_rgba_image() {
        let bytes = transmit_chunks(42, &image(vec![0, 1, 2, 3])).concat();

        assert_eq!(
            bytes,
            b"\x1b_Ga=t,f=32,s=1,v=1,i=42,q=2;AAECAw==\x1b\\".to_vec()
        );
    }

    #[test]
    fn transmit_chunks_large_payload() {
        let chunks = transmit_chunks(7, &image(vec![1; 4096]));
        let text = String::from_utf8(chunks.concat()).expect("ascii kitty escapes");

        assert_eq!(chunks.len(), 2);
        assert!(text.contains("a=t,f=32,s=1,v=1,i=7,q=2,m=1;"));
        assert!(text.contains("\u{1b}_Gm=0;"));
    }

    #[test]
    fn place_delete_and_passthrough_encode_protocol_bytes() {
        assert_eq!(
            virtual_place(42, 12, 6),
            b"\x1b_Ga=p,U=1,i=42,c=12,r=6,q=2;\x1b\\".to_vec()
        );
        assert_eq!(delete(42), b"\x1b_Ga=d,d=i,i=42,q=2;\x1b\\".to_vec());
        assert_eq!(
            tmux_passthrough(b"\x1b_Ga=p;\x1b\\"),
            b"\x1bPtmux;\x1b\x1b_Ga=p;\x1b\x1b\\\x1b\\".to_vec()
        );
        assert_eq!(
            wrap_pixel_payload(b"\x1b_Ga=p;\x1b\\", false),
            b"\x1b_Ga=p;\x1b\\".to_vec()
        );
    }

    #[test]
    fn placeholder_grid_uses_truecolor_id_and_row_col_diacritics() {
        let bytes = placeholder_grid(0x123456, Rect::new(2, 3, 2, 1));
        let text = String::from_utf8(bytes).expect("utf8 placeholders");

        assert!(text.starts_with("\x1b[38;2;18;52;86m\x1b[4;3H"));
        assert!(text.contains("\u{10eeee}\u{0305}\u{0305}"));
        assert!(text.contains("\u{10eeee}\u{0305}\u{030d}"));
        assert!(text.ends_with("\x1b[0m"));
    }

    #[test]
    fn inline_placeholder_row_uses_image_color_without_cursor_moves() {
        let bytes = inline_placeholder_row(0x123456, 1, 2);
        let text = String::from_utf8(bytes).expect("utf8 placeholders");

        assert!(text.starts_with("\x1b[38;2;18;52;86m"));
        assert!(!text.contains('H'));
        assert!(text.contains("\u{10eeee}\u{030d}\u{0305}"));
        assert!(text.contains("\u{10eeee}\u{030d}\u{030d}"));
        assert!(text.ends_with("\x1b[0m"));
    }

    #[test]
    fn row_column_diacritics_match_kitty_placeholder_table() {
        assert_eq!(
            ROW_COLUMN_DIACRITICS,
            [
                '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}',
                '\u{033f}', '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}',
                '\u{0352}', '\u{0357}', '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}',
                '\u{0367}', '\u{0368}', '\u{0369}', '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}',
                '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
            ]
        );
        assert_eq!(diacritic(3), '\u{0310}');
        assert_ne!(diacritic(3), '\u{030f}');
    }

    #[test]
    fn clear_deletes_cached_images_and_blanks_last_rect() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        painter.pet_id = Some("codex".to_owned());
        painter.transmitted.insert(3);
        painter.last_rect = Some(Rect::new(1, 2, 2, 1));
        let mut bytes = Vec::new();

        painter.clear(&mut bytes).expect("clear");
        let text = String::from_utf8(bytes).expect("utf8 clear");

        assert!(text.contains("\x1b[3;2H  "));
        assert!(text.contains("\x1bPtmux;\x1b\x1b_Ga=d,d=i,i=1179651,q=2;"));
        assert!(painter.transmitted.is_empty());
        assert!(painter.last_rect.is_none());
    }

    #[test]
    fn full_redraw_needed_when_pixel_rect_pet_or_presence_changes() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        painter.pet_id = Some("codex".to_owned());
        painter.last_rect = Some(Rect::new(1, 2, 2, 1));
        let same = PetPixelView {
            pet_id: "codex".to_owned(),
            sprite_index: 0,
            size: super::super::PetGridSize { cols: 2, rows: 1 },
        };
        let other_pet = PetPixelView {
            pet_id: "claude".to_owned(),
            ..same.clone()
        };

        assert!(!painter.needs_full_redraw(Some((Rect::new(1, 2, 2, 1), &same))));
        assert!(painter.needs_full_redraw(Some((Rect::new(1, 3, 2, 1), &same))));
        assert!(painter.needs_full_redraw(Some((Rect::new(1, 2, 2, 1), &other_pet))));
        assert!(painter.needs_full_redraw(None));

        painter.last_rect = None;
        assert!(!painter.needs_full_redraw(Some((Rect::new(1, 3, 2, 1), &same))));
    }

    #[test]
    fn hide_after_draw_deletes_cached_images_without_blanking_last_rect() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        painter.pet_id = Some("codex".to_owned());
        painter.transmitted.insert(3);
        painter.last_rect = Some(Rect::new(1, 2, 2, 1));
        let mut bytes = Vec::new();

        painter.hide_after_draw(&mut bytes).expect("hide");
        let text = String::from_utf8(bytes).expect("utf8 hide");

        assert!(!text.contains("\x1b[3;2H  "));
        assert!(text.contains("\x1bPtmux;\x1b\x1b_Ga=d,d=i,i=1179651,q=2;"));
        assert!(painter.transmitted.is_empty());
        assert!(painter.last_rect.is_none());
    }

    #[test]
    fn paint_rect_shift_deletes_images_without_blanking_old_cells() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = PetPixelView {
            pet_id: "codex".to_owned(),
            sprite_index: 0,
            size: super::super::PetGridSize { cols: 3, rows: 2 },
        };
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .paint(&mut Vec::new(), Rect::new(5, 7, 3, 2), &pixel, &frame)
            .expect("first paint");
        let mut bytes = Vec::new();
        painter
            .paint(&mut bytes, Rect::new(1, 1, 3, 2), &pixel, &frame)
            .expect("shifted paint");
        let text = String::from_utf8(bytes).expect("utf8 shifted paint");

        assert!(!text.contains("\x1b[8;6H   "));
        assert!(!text.contains("\x1b[9;6H   "));
        assert!(text.contains("a=d,d=i,i=1179648,q=2"));
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1179648,q=2;AAECAw=="));
        assert!(text.contains("\x1b[2;2H"));
    }

    #[test]
    fn paint_wraps_each_transmit_chunk_in_its_own_tmux_passthrough() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = PetPixelView {
            pet_id: "codex".to_owned(),
            sprite_index: 0,
            size: super::super::PetGridSize { cols: 2, rows: 1 },
        };
        let mut bytes = Vec::new();

        painter
            .paint(
                &mut bytes,
                Rect::new(0, 0, 2, 1),
                &pixel,
                &image(vec![1; 4096]),
            )
            .expect("paint");

        assert_eq!(
            bytes
                .windows(b"\x1bPtmux;".len())
                .filter(|window| *window == b"\x1bPtmux;")
                .count(),
            3,
            "two transmit chunks plus one virtual placement each get a passthrough wrapper"
        );
    }

    #[test]
    fn paint_can_emit_unwrapped_native_kitty_graphics() {
        let mut painter = PixelPainter::with_id_base(0x120000, false);
        let pixel = PetPixelView {
            pet_id: "codex".to_owned(),
            sprite_index: 0,
            size: super::super::PetGridSize { cols: 2, rows: 1 },
        };
        let mut bytes = Vec::new();

        painter
            .paint(
                &mut bytes,
                Rect::new(0, 0, 2, 1),
                &pixel,
                &image(vec![0, 1, 2, 3]),
            )
            .expect("paint");
        let text = String::from_utf8(bytes).expect("utf8 paint");

        assert!(!text.contains("\x1bPtmux;"));
        assert!(text.contains("\x1b_Ga=t,f=32,s=1,v=1,i=1179648,q=2;AAECAw=="));
        assert!(text.contains("\x1b_Ga=p,U=1,i=1179648,c=2,r=1,q=2;"));
    }
}
