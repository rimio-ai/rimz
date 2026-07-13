//! Shared kitty graphics transport for pane-resident pixel surfaces.

pub(crate) mod meter;
pub(crate) mod probe;

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

const ESC: u8 = 0x1b;
pub(crate) const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
pub(crate) const END_SYNC: &[u8] = b"\x1b[?2026l";
const CHUNK_SIZE: usize = 4096;
pub(crate) const IMAGE_ID_COLOR_MASK: u32 = 0x00ff_ffff;
const PLACEHOLDER: char = '\u{10eeee}';
pub(crate) const RESIDENT_REFRESH_MS: u64 = 2000;
pub(crate) const MIN_RESEND_SPACING_MS: u64 = 250;

// Kitty's complete rowcolumn-diacritics list, derived from Unicode 6.0.
const ROW_COLUMN_DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030d}',
    '\u{030e}',
    '\u{0310}',
    '\u{0312}',
    '\u{033d}',
    '\u{033e}',
    '\u{033f}',
    '\u{0346}',
    '\u{034a}',
    '\u{034b}',
    '\u{034c}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035b}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036a}',
    '\u{036b}',
    '\u{036c}',
    '\u{036d}',
    '\u{036e}',
    '\u{036f}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059c}',
    '\u{059d}',
    '\u{059e}',
    '\u{059f}',
    '\u{05a0}',
    '\u{05a1}',
    '\u{05a8}',
    '\u{05a9}',
    '\u{05ab}',
    '\u{05ac}',
    '\u{05af}',
    '\u{05c4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065a}',
    '\u{065b}',
    '\u{065d}',
    '\u{065e}',
    '\u{06d6}',
    '\u{06d7}',
    '\u{06d8}',
    '\u{06d9}',
    '\u{06da}',
    '\u{06db}',
    '\u{06dc}',
    '\u{06df}',
    '\u{06e0}',
    '\u{06e1}',
    '\u{06e2}',
    '\u{06e4}',
    '\u{06e7}',
    '\u{06e8}',
    '\u{06eb}',
    '\u{06ec}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073a}',
    '\u{073d}',
    '\u{073f}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074a}',
    '\u{07eb}',
    '\u{07ec}',
    '\u{07ed}',
    '\u{07ee}',
    '\u{07ef}',
    '\u{07f0}',
    '\u{07f1}',
    '\u{07f3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081b}',
    '\u{081c}',
    '\u{081d}',
    '\u{081e}',
    '\u{081f}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082a}',
    '\u{082b}',
    '\u{082c}',
    '\u{082d}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0f82}',
    '\u{0f83}',
    '\u{0f86}',
    '\u{0f87}',
    '\u{135d}',
    '\u{135e}',
    '\u{135f}',
    '\u{17dd}',
    '\u{193a}',
    '\u{1a17}',
    '\u{1a75}',
    '\u{1a76}',
    '\u{1a77}',
    '\u{1a78}',
    '\u{1a79}',
    '\u{1a7a}',
    '\u{1a7b}',
    '\u{1a7c}',
    '\u{1b6b}',
    '\u{1b6d}',
    '\u{1b6e}',
    '\u{1b6f}',
    '\u{1b70}',
    '\u{1b71}',
    '\u{1b72}',
    '\u{1b73}',
    '\u{1cd0}',
    '\u{1cd1}',
    '\u{1cd2}',
    '\u{1cda}',
    '\u{1cdb}',
    '\u{1ce0}',
    '\u{1dc0}',
    '\u{1dc1}',
    '\u{1dc3}',
    '\u{1dc4}',
    '\u{1dc5}',
    '\u{1dc6}',
    '\u{1dc7}',
    '\u{1dc8}',
    '\u{1dc9}',
    '\u{1dcb}',
    '\u{1dcc}',
    '\u{1dd1}',
    '\u{1dd2}',
    '\u{1dd3}',
    '\u{1dd4}',
    '\u{1dd5}',
    '\u{1dd6}',
    '\u{1dd7}',
    '\u{1dd8}',
    '\u{1dd9}',
    '\u{1dda}',
    '\u{1ddb}',
    '\u{1ddc}',
    '\u{1ddd}',
    '\u{1dde}',
    '\u{1ddf}',
    '\u{1de0}',
    '\u{1de1}',
    '\u{1de2}',
    '\u{1de3}',
    '\u{1de4}',
    '\u{1de5}',
    '\u{1de6}',
    '\u{1dfe}',
    '\u{20d0}',
    '\u{20d1}',
    '\u{20d4}',
    '\u{20d5}',
    '\u{20d6}',
    '\u{20d7}',
    '\u{20db}',
    '\u{20dc}',
    '\u{20e1}',
    '\u{20e7}',
    '\u{20e9}',
    '\u{20f0}',
    '\u{2cef}',
    '\u{2cf0}',
    '\u{2cf1}',
    '\u{2de0}',
    '\u{2de1}',
    '\u{2de2}',
    '\u{2de3}',
    '\u{2de4}',
    '\u{2de5}',
    '\u{2de6}',
    '\u{2de7}',
    '\u{2de8}',
    '\u{2de9}',
    '\u{2dea}',
    '\u{2deb}',
    '\u{2dec}',
    '\u{2ded}',
    '\u{2dee}',
    '\u{2def}',
    '\u{2df0}',
    '\u{2df1}',
    '\u{2df2}',
    '\u{2df3}',
    '\u{2df4}',
    '\u{2df5}',
    '\u{2df6}',
    '\u{2df7}',
    '\u{2df8}',
    '\u{2df9}',
    '\u{2dfa}',
    '\u{2dfb}',
    '\u{2dfc}',
    '\u{2dfd}',
    '\u{2dfe}',
    '\u{2dff}',
    '\u{a66f}',
    '\u{a67c}',
    '\u{a67d}',
    '\u{a6f0}',
    '\u{a6f1}',
    '\u{a8e0}',
    '\u{a8e1}',
    '\u{a8e2}',
    '\u{a8e3}',
    '\u{a8e4}',
    '\u{a8e5}',
    '\u{a8e6}',
    '\u{a8e7}',
    '\u{a8e8}',
    '\u{a8e9}',
    '\u{a8ea}',
    '\u{a8eb}',
    '\u{a8ec}',
    '\u{a8ed}',
    '\u{a8ee}',
    '\u{a8ef}',
    '\u{a8f0}',
    '\u{a8f1}',
    '\u{aab0}',
    '\u{aab2}',
    '\u{aab3}',
    '\u{aab7}',
    '\u{aab8}',
    '\u{aabe}',
    '\u{aabf}',
    '\u{aac1}',
    '\u{fe20}',
    '\u{fe21}',
    '\u{fe22}',
    '\u{fe23}',
    '\u{fe24}',
    '\u{fe25}',
    '\u{fe26}',
    '\u{10a0f}',
    '\u{10a38}',
    '\u{1d185}',
    '\u{1d186}',
    '\u{1d187}',
    '\u{1d188}',
    '\u{1d189}',
    '\u{1d1aa}',
    '\u{1d1ab}',
    '\u{1d1ac}',
    '\u{1d1ad}',
    '\u{1d242}',
    '\u{1d243}',
    '\u{1d244}',
];

pub fn write_synchronized_pixel_output<W: Write>(
    writer: &mut W,
    body: impl FnOnce(&mut W) -> io::Result<()>,
) -> io::Result<()> {
    writer.write_all(BEGIN_SYNC)?;
    let body_result = body(writer);
    let end_result = writer.write_all(END_SYNC);
    body_result.and(end_result)
}

pub(crate) fn runtime_image_id_base() -> u32 {
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

pub fn transmit_png_chunks(image_id: u32, png: &[u8]) -> Vec<Vec<u8>> {
    let payload = base64(png);
    if payload.len() <= CHUNK_SIZE {
        return vec![kitty_escape(
            &format!("a=t,f=100,i={image_id},q=2"),
            payload.as_bytes(),
        )];
    }

    let mut out = Vec::new();
    let chunk_count = payload.len().div_ceil(CHUNK_SIZE);
    for (index, chunk) in payload.as_bytes().chunks(CHUNK_SIZE).enumerate() {
        let more = usize::from(index + 1 < chunk_count);
        let control = if index == 0 {
            format!("a=t,f=100,i={image_id},q=2,m={more}")
        } else {
            format!("m={more}")
        };
        out.push(kitty_escape(&control, chunk));
    }
    out
}

pub fn virtual_place(image_id: u32, cols: u16, rows: u16, quiet: u8) -> Vec<u8> {
    kitty_escape(
        &format!("a=p,U=1,i={image_id},c={cols},r={rows},q={quiet}"),
        &[],
    )
}

pub(crate) fn delete(image_id: u32) -> Vec<u8> {
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

pub(crate) fn placeholder_columns_supported(width: usize) -> bool {
    width <= ROW_COLUMN_DIACRITICS.len() && u16::try_from(width).is_ok()
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
    use crate::sidebar_pane::pets::{
        PetGridSize, PetPixelView, PixelPainter, RgbaImage, encode_png,
    };

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
            size: PetGridSize { cols, rows },
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
    fn transmit_encodes_png_image() {
        let bytes = transmit_png_chunks(42, &encode_png(1, 1, &[0, 1, 2, 3])).concat();
        let text = String::from_utf8(bytes).expect("ascii kitty escapes");

        assert!(text.contains("a=t,f=100,i=42,q=2;iVBORw0KGgo"));
    }

    #[test]
    fn transmit_chunks_large_payload() {
        let chunks = transmit_png_chunks(7, &vec![1_u8; 4096]);
        let text = String::from_utf8(chunks.concat()).expect("ascii kitty escapes");

        assert_eq!(chunks.len(), 2);
        assert!(text.contains("a=t,f=100,i=7,q=2,m=1;"));
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
        assert!(placeholder_columns_supported(ROW_COLUMN_DIACRITICS.len()));
        assert!(!placeholder_columns_supported(
            ROW_COLUMN_DIACRITICS.len() + 1
        ));
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
        painter.transmitted.insert(3, 0);
        painter
            .png
            .insert(3, std::sync::Arc::<[u8]>::from(vec![1_u8]));
        painter.resident.insert(3);
        let mut bytes = Vec::new();

        painter.clear(&mut bytes).expect("clear");
        assert_sync_bracketed(&bytes);
        let text = String::from_utf8(bytes).expect("utf8 clear");

        assert!(text.contains("\x1bPtmux;\x1b\x1b_Ga=d,d=i,i=1179651,q=2;"));
        assert!(!text.contains("\u{10eeee}"));
        assert!(!text.contains("\x1b[3;2H"));
        assert!(painter.transmitted.is_empty());
        assert!(painter.png.is_empty());
        assert!(painter.resident.is_empty());
    }

    #[test]
    fn ensure_transmitted_places_each_sprite_once_then_writes_nothing_on_repeat() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 3, 2);
        let frame = image(vec![0, 1, 2, 3]);

        let mut first = Vec::new();
        painter
            .ensure_transmitted(&mut first, &pixel, &frame, 0)
            .expect("first transmit");
        assert_not_sync_bracketed(&first);
        let text = String::from_utf8(first).expect("utf8 first transmit");
        assert!(text.contains("a=t,f=100,i=1179648,q=2"));
        assert!(text.contains("a=p,U=1,i=1179648,c=3,r=2,q=2"));
        assert!(!text.contains("\u{10eeee}"));
        assert!(!text.contains("\x1b["));

        let mut repeat = Vec::new();
        painter
            .ensure_transmitted(&mut repeat, &pixel, &frame, 0)
            .expect("repeat transmit");
        assert!(repeat.is_empty());
    }

    #[test]
    fn stale_resident_sprite_retransmits_image_and_virtual_placement() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 3, 2);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame, 0)
            .expect("first transmit");

        let mut stale = Vec::new();
        painter
            .ensure_transmitted(&mut stale, &pixel, &frame, RESIDENT_REFRESH_MS)
            .expect("stale transmit");
        let text = String::from_utf8(stale).expect("utf8 stale transmit");

        assert!(text.contains("a=t,f=100,i=1179648,q=2"));
        assert!(text.contains("a=p,U=1,i=1179648,c=3,r=2,q=2"));
    }

    #[test]
    fn stale_sprite_retransmits_are_globally_spaced() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let other_pixel = pixel_view("codex", 1, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame, 0)
            .expect("first transmit");
        painter
            .ensure_transmitted(&mut Vec::new(), &other_pixel, &frame, 0)
            .expect("other first transmit");

        let mut first_stale = Vec::new();
        painter
            .ensure_transmitted(&mut first_stale, &pixel, &frame, RESIDENT_REFRESH_MS)
            .expect("first stale transmit");
        assert!(bytes_contains(&first_stale, b"a=t,f=100,i=1179648,q=2"));

        let mut too_soon = Vec::new();
        painter
            .ensure_transmitted(
                &mut too_soon,
                &other_pixel,
                &frame,
                RESIDENT_REFRESH_MS + MIN_RESEND_SPACING_MS - 1,
            )
            .expect("too soon transmit");
        assert!(too_soon.is_empty());

        let mut spaced = Vec::new();
        painter
            .ensure_transmitted(
                &mut spaced,
                &other_pixel,
                &frame,
                RESIDENT_REFRESH_MS + MIN_RESEND_SPACING_MS,
            )
            .expect("spaced transmit");
        assert!(bytes_contains(&spaced, b"a=t,f=100,i=1179649,q=2"));
    }

    #[test]
    fn new_sprite_transmit_ignores_resend_spacing() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let other_pixel = pixel_view("codex", 1, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame, 0)
            .expect("first transmit");
        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame, RESIDENT_REFRESH_MS)
            .expect("stale transmit");

        let mut first_time = Vec::new();
        painter
            .ensure_transmitted(
                &mut first_time,
                &other_pixel,
                &frame,
                RESIDENT_REFRESH_MS + 1,
            )
            .expect("first-time transmit");

        assert!(bytes_contains(&first_time, b"a=t,f=100,i=1179649,q=2"));
        assert!(bytes_contains(
            &first_time,
            b"a=p,U=1,i=1179649,c=2,r=1,q=2"
        ));
    }

    #[test]
    fn pet_change_replaces_same_id_without_delete_and_keeps_old_slots_for_teardown() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let codex = pixel_view("codex", 0, 2, 1);
        let codex_other_sprite = pixel_view("codex", 1, 2, 1);
        let claude = pixel_view("claude", 0, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &codex, &frame, 0)
            .expect("first sprite");
        painter
            .ensure_transmitted(&mut Vec::new(), &codex_other_sprite, &frame, 0)
            .expect("second sprite");
        let mut bytes = Vec::new();
        painter
            .ensure_transmitted(&mut bytes, &claude, &frame, 0)
            .expect("pet changed");
        let text = String::from_utf8(bytes).expect("utf8 pet change");

        assert_not_sync_bracketed(text.as_bytes());
        assert!(!text.contains("a=d,d=i"));
        assert!(text.contains("a=t,f=100,i=1179648,q=2"));
        assert!(text.contains("a=p,U=1,i=1179648,c=2,r=1,q=2"));
        assert!(painter.transmitted.contains_key(&0));
        assert!(!painter.transmitted.contains_key(&1));
        assert!(painter.png.contains_key(&0));
        assert!(!painter.png.contains_key(&1));
        assert!(painter.resident.contains(&0));
        assert!(painter.resident.contains(&1));

        let mut clear = Vec::new();
        painter.clear(&mut clear).expect("clear");
        let clear_text = String::from_utf8(clear).expect("utf8 clear");
        assert!(clear_text.contains("a=d,d=i,i=1179648,q=2"));
        assert!(clear_text.contains("a=d,d=i,i=1179649,q=2"));
        assert!(painter.png.is_empty());
    }

    #[test]
    fn ensure_transmitted_places_new_sprite_and_reuses_resident_sprite() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let other_pixel = pixel_view("codex", 1, 2, 1);
        let frame = image(vec![0, 1, 2, 3]);

        painter
            .ensure_transmitted(&mut Vec::new(), &pixel, &frame, 0)
            .expect("first transmit");

        let mut steady = Vec::new();
        painter
            .ensure_transmitted(&mut steady, &pixel, &frame, 0)
            .expect("steady transmit");
        assert!(steady.is_empty());

        let mut new_sprite = Vec::new();
        painter
            .ensure_transmitted(&mut new_sprite, &other_pixel, &frame, 0)
            .expect("new sprite transmit");
        assert!(bytes_contains(&new_sprite, b"a=t,f=100,i=1179649,q=2"));
        assert!(bytes_contains(
            &new_sprite,
            b"a=p,U=1,i=1179649,c=2,r=1,q=2"
        ));

        let mut reused_sprite = Vec::new();
        painter
            .ensure_transmitted(&mut reused_sprite, &pixel, &frame, 0)
            .expect("reused sprite transmit");
        assert!(reused_sprite.is_empty());
    }

    #[test]
    fn ensure_transmitted_wraps_each_transmit_chunk_in_its_own_tmux_passthrough() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = pixel_view("codex", 0, 2, 1);
        let mut bytes = Vec::new();
        let width = 64;
        let height = 64;
        let mut state = 0x1234_5678_u32;
        let data = (0..width * height * 4)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect::<Vec<_>>();
        let expected =
            transmit_png_chunks(painter.image_id(0), &encode_png(width, height, &data)).len() + 1;

        painter
            .ensure_transmitted(
                &mut bytes,
                &pixel,
                &RgbaImage {
                    width,
                    height,
                    data,
                },
                0,
            )
            .expect("transmit");

        assert_not_sync_bracketed(&bytes);
        assert!(
            expected >= 3,
            "test image must span multiple transmit chunks plus placement"
        );
        assert_eq!(
            bytes
                .windows(b"\x1bPtmux;".len())
                .filter(|window| *window == b"\x1bPtmux;")
                .count(),
            expected,
            "each transmit chunk plus virtual placement gets a passthrough wrapper"
        );
    }

    #[test]
    fn ensure_transmitted_can_emit_unwrapped_native_kitty_graphics() {
        let mut painter = PixelPainter::with_id_base(0x120000, false);
        let pixel = pixel_view("codex", 0, 2, 1);
        let mut bytes = Vec::new();

        painter
            .ensure_transmitted(&mut bytes, &pixel, &image(vec![0, 1, 2, 3]), 0)
            .expect("transmit");
        assert_not_sync_bracketed(&bytes);
        let text = String::from_utf8(bytes).expect("utf8 paint");

        assert!(!text.contains("\x1bPtmux;"));
        assert!(text.contains("\x1b_Ga=t,f=100,i=1179648,q=2;"));
        assert!(text.contains("\x1b_Ga=p,U=1,i=1179648,c=2,r=1,q=2;"));
    }
}
