//! Emits kitty graphics payloads for pixel pets and tracks pane-local image
//! placement so sprites stay resident until renderer teardown.

pub(crate) mod probe;

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

use super::PetPixelView;
use super::frames::RgbaImage;

const ESC: u8 = 0x1b;
pub(crate) const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
pub(crate) const END_SYNC: &[u8] = b"\x1b[?2026l";
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
    resident: BTreeSet<usize>,
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
            resident: BTreeSet::new(),
        }
    }

    pub(crate) fn runtime_id_base() -> u32 {
        runtime_image_id_base()
    }

    pub(crate) fn id_base(&self) -> u32 {
        self.id_base
    }

    pub(crate) fn ensure_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        pixel: &PetPixelView,
        frame: &RgbaImage,
    ) -> io::Result<()> {
        let pet_changed = self.pet_id.as_deref() != Some(pixel.pet_id.as_str());
        if pet_changed {
            // The same sprite ids are re-transmitted with the new sheet and
            // replace image data in place; no delete APC is emitted mid-session.
            self.transmitted.clear();
            self.pet_id = Some(pixel.pet_id.clone());
        }

        let image_id = pixel.image_id;
        debug_assert_eq!(image_id, self.image_id(pixel.sprite_index));
        if self.transmitted.insert(pixel.sprite_index) {
            self.resident.insert(pixel.sprite_index);
            for chunk in transmit_chunks(image_id, frame) {
                writer.write_all(&self.wrap_payload(&chunk))?;
            }
            // Kitty virtual placements persist; placeholder cells re-materialize the
            // image on redraw, so per-frame placement APCs only add cursor flicker.
            writer.write_all(&self.wrap_payload(&virtual_place(
                image_id,
                pixel.size.cols,
                pixel.size.rows,
                2,
            )))?;
        }
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write_synchronized_pixel_output(writer, |writer| self.delete_transmitted(writer))?;
        self.pet_id = None;
        writer.flush()
    }

    fn delete_transmitted<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let sprite_indexes = std::mem::take(&mut self.resident);
        for sprite_index in sprite_indexes {
            writer.write_all(&self.wrap_payload(&delete(self.image_id(sprite_index))))?;
        }
        self.transmitted.clear();
        Ok(())
    }

    fn image_id(&self, sprite_index: usize) -> u32 {
        sprite_image_id(self.id_base, sprite_index)
    }

    fn wrap_payload(&self, payload: &[u8]) -> Vec<u8> {
        wrap_pixel_payload(payload, self.wrap)
    }
}

pub fn write_synchronized_pixel_output<W: Write>(
    writer: &mut W,
    body: impl FnOnce(&mut W) -> io::Result<()>,
) -> io::Result<()> {
    writer.write_all(BEGIN_SYNC)?;
    let body_result = body(writer);
    let end_result = writer.write_all(END_SYNC);
    body_result.and(end_result)
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

pub(crate) fn sprite_image_id(id_base: u32, sprite_index: usize) -> u32 {
    let id = id_base.wrapping_add(sprite_index as u32) & IMAGE_ID_COLOR_MASK;
    id.max(1)
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

pub fn virtual_place(image_id: u32, cols: u16, rows: u16, quiet: u8) -> Vec<u8> {
    kitty_escape(
        &format!("a=p,U=1,i={image_id},c={cols},r={rows},q={quiet}"),
        &[],
    )
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

pub fn inline_placeholder_row(image_id: u32, row: u16, cols: u16) -> Vec<u8> {
    let mut out = Vec::new();
    push_image_id_color(&mut out, image_id);
    for col in 0..cols {
        out.extend_from_slice(placeholder_cluster(row, col).as_bytes());
    }
    out.extend_from_slice(b"\x1b[0m");
    out
}

pub(crate) fn placeholder_cluster(row: u16, col: u16) -> String {
    let mut out = String::with_capacity(10);
    out.push(PLACEHOLDER);
    out.push(diacritic(row));
    out.push(diacritic(col));
    out
}

pub(crate) fn image_id_rgb(image_id: u32) -> (u8, u8, u8) {
    (
        ((image_id >> 16) & 0xff) as u8,
        ((image_id >> 8) & 0xff) as u8,
        (image_id & 0xff) as u8,
    )
}

pub(crate) fn image_id_color(image_id: u32) -> Color {
    let (red, green, blue) = image_id_rgb(image_id);
    Color::Rgb(red, green, blue)
}

fn push_image_id_color(out: &mut Vec<u8>, image_id: u32) {
    let (red, green, blue) = image_id_rgb(image_id);
    push_fmt(out, format_args!("\x1b[38;2;{red};{green};{blue}m"));
}

fn diacritic(value: u16) -> char {
    ROW_COLUMN_DIACRITICS[usize::from(value).min(ROW_COLUMN_DIACRITICS.len() - 1)]
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

    fn pixel_view(pet_id: &str, sprite_index: usize, cols: u16, rows: u16) -> PetPixelView {
        PetPixelView {
            pet_id: pet_id.to_owned(),
            sprite_index,
            image_id: sprite_image_id(0x120000, sprite_index),
            size: super::super::PetGridSize { cols, rows },
        }
    }

    fn assert_sync_bracketed(bytes: &[u8]) {
        assert!(bytes.starts_with(BEGIN_SYNC));
        assert!(bytes.ends_with(END_SYNC));
        assert!(!bytes_contains(bytes, b"\x1bPtmux;\x1b\x1b[?2026h"));
        assert!(!bytes_contains(bytes, b"\x1bPtmux;\x1b\x1b[?2026l"));
    }

    fn assert_not_sync_bracketed(bytes: &[u8]) {
        assert!(!bytes.starts_with(BEGIN_SYNC));
        assert!(!bytes.ends_with(END_SYNC));
    }

    fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
            virtual_place(42, 12, 6, 2),
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
    fn placeholder_cluster_uses_row_col_diacritics() {
        assert_eq!(placeholder_cluster(0, 1), "\u{10eeee}\u{0305}\u{030d}");
        assert_eq!(placeholder_cluster(1, 0), "\u{10eeee}\u{030d}\u{0305}");
        assert_eq!(image_id_rgb(0x123456), (18, 52, 86));
        assert_eq!(sprite_image_id(0x00ff_ffff, 1), 1);
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
    fn clear_deletes_cached_images_without_blanking_cells() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        painter.pet_id = Some("codex".to_owned());
        painter.transmitted.insert(3);
        painter.resident.insert(3);
        let mut bytes = Vec::new();

        painter.clear(&mut bytes).expect("clear");
        assert_sync_bracketed(&bytes);
        let text = String::from_utf8(bytes).expect("utf8 clear");

        assert!(text.contains("\x1bPtmux;\x1b\x1b_Ga=d,d=i,i=1179651,q=2;"));
        assert!(!text.contains("\u{10eeee}"));
        assert!(!text.contains("\x1b[3;2H"));
        assert!(painter.transmitted.is_empty());
        assert!(painter.resident.is_empty());
    }

    #[test]
    fn ensure_transmitted_places_each_sprite_once_then_writes_nothing_on_repeat() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 3, 2);
        let frame = image(vec![0, 1, 2, 3]);

        let mut first = Vec::new();
        painter
            .ensure_transmitted(&mut first, &pixel, &frame)
            .expect("first transmit");
        assert_not_sync_bracketed(&first);
        let text = String::from_utf8(first).expect("utf8 first transmit");
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1179648,q=2;AAECAw=="));
        assert!(text.contains("a=p,U=1,i=1179648,c=3,r=2,q=2"));
        assert!(!text.contains("\u{10eeee}"));
        assert!(!text.contains("\x1b["));

        let mut repeat = Vec::new();
        painter
            .ensure_transmitted(&mut repeat, &pixel, &frame)
            .expect("repeat transmit");
        assert!(repeat.is_empty());
    }

    #[test]
    fn pet_change_replaces_same_id_without_delete_and_keeps_old_slots_for_teardown() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let codex = pixel_view("codex", 0, 2, 1);
        let codex_other_sprite = pixel_view("codex", 1, 2, 1);
        let claude = pixel_view("claude", 0, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &codex, &frame)
            .expect("first sprite");
        painter
            .ensure_transmitted(&mut Vec::new(), &codex_other_sprite, &frame)
            .expect("second sprite");
        let mut bytes = Vec::new();
        painter
            .ensure_transmitted(&mut bytes, &claude, &frame)
            .expect("pet changed");
        let text = String::from_utf8(bytes).expect("utf8 pet change");

        assert_not_sync_bracketed(text.as_bytes());
        assert!(!text.contains("a=d,d=i"));
        assert!(text.contains("a=t,f=32,s=1,v=1,i=1179648,q=2;AAECAw=="));
        assert!(text.contains("a=p,U=1,i=1179648,c=2,r=1,q=2"));
        assert!(painter.transmitted.contains(&0));
        assert!(!painter.transmitted.contains(&1));
        assert!(painter.resident.contains(&0));
        assert!(painter.resident.contains(&1));

        let mut clear = Vec::new();
        painter.clear(&mut clear).expect("clear");
        let clear_text = String::from_utf8(clear).expect("utf8 clear");
        assert!(clear_text.contains("a=d,d=i,i=1179648,q=2"));
        assert!(clear_text.contains("a=d,d=i,i=1179649,q=2"));
    }

    #[test]
    fn ensure_transmitted_places_new_sprite_and_reuses_resident_sprite() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let other_pixel = pixel_view("codex", 1, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame)
            .expect("first transmit");

        let mut steady = Vec::new();
        painter
            .ensure_transmitted(&mut steady, &pixel, &frame)
            .expect("steady transmit");
        assert!(steady.is_empty());

        let mut new_sprite = Vec::new();
        painter
            .ensure_transmitted(&mut new_sprite, &other_pixel, &frame)
            .expect("new sprite transmit");
        assert!(bytes_contains(
            &new_sprite,
            b"a=t,f=32,s=1,v=1,i=1179649,q=2"
        ));
        assert!(bytes_contains(
            &new_sprite,
            b"a=p,U=1,i=1179649,c=2,r=1,q=2"
        ));

        let mut reused_sprite = Vec::new();
        painter
            .ensure_transmitted(&mut reused_sprite, &pixel, &frame)
            .expect("reused sprite transmit");
        assert!(reused_sprite.is_empty());
    }

    #[test]
    fn ensure_transmitted_wraps_each_transmit_chunk_in_its_own_tmux_passthrough() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let mut bytes = Vec::new();

        painter
            .ensure_transmitted(&mut bytes, &pixel, &image(vec![1; 4096]))
            .expect("transmit");

        assert_not_sync_bracketed(&bytes);
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
    fn ensure_transmitted_can_emit_unwrapped_native_kitty_graphics() {
        let mut painter = PixelPainter::with_id_base(0x120000, false);
        let pixel = pixel_view("codex", 0, 2, 1);
        let mut bytes = Vec::new();

        painter
            .ensure_transmitted(&mut bytes, &pixel, &image(vec![0, 1, 2, 3]))
            .expect("transmit");
        assert_not_sync_bracketed(&bytes);
        let text = String::from_utf8(bytes).expect("utf8 paint");

        assert!(!text.contains("\x1bPtmux;"));
        assert!(text.contains("\x1b_Ga=t,f=32,s=1,v=1,i=1179648,q=2;AAECAw=="));
        assert!(text.contains("\x1b_Ga=p,U=1,i=1179648,c=2,r=1,q=2;"));
    }
}
