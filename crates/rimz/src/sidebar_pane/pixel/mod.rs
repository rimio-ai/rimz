//! Shared kitty graphics transport for pane-resident pixel surfaces.

pub(crate) mod meter;
mod pacing;
pub(crate) mod probe;
pub(crate) mod tty;

pub use pacing::LiveGraphicsPacer;
pub(crate) use probe::detect as detect_pixel_render_caps;
pub use probe::{PixelRenderCaps, detect_env as detect_pixel_render_env};

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

const ESC: u8 = 0x1b;
pub(crate) const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
pub(crate) const END_SYNC: &[u8] = b"\x1b[?2026l";
const CHUNK_SIZE: usize = 4096;
pub(crate) const IMAGE_ID_COLOR_MASK: u32 = 0x00ff_ffff;
pub(crate) const PLACEHOLDER: char = '\u{10eeee}';
pub(crate) const RESIDENT_REFRESH_MS: u64 = 2000;
pub(crate) const MIN_RESEND_SPACING_MS: u64 = 250;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RgbaImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

impl RgbaImage {
    pub(crate) fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * self.width + x) * 4) as usize;
        self.data[offset..offset + 4].try_into().unwrap_or_default()
    }
}

pub fn encode_png(width: u32, height: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fastest);
        let mut writer = encoder
            .write_header()
            .expect("encoding in-memory PNG header cannot fail");
        writer
            .write_image_data(data)
            .expect("encoding valid RGBA frame data cannot fail");
    }
    bytes
}

#[derive(Debug)]
struct Resident<C> {
    image_id: u32,
    content: C,
    sent_at_ms: u64,
}

/// Generic terminal image residency. Content bytes stay caller-owned and are
/// produced only after first-send/change/stale policy requests transmission.
#[derive(Debug)]
pub(crate) struct ImageResidency<K, C> {
    wrap: bool,
    images: BTreeMap<K, Resident<C>>,
    resident_ids: BTreeSet<u32>,
    last_resend_ms: Option<u64>,
}

impl<K: Ord, C: Eq> ImageResidency<K, C> {
    pub(crate) fn new(wrap: bool) -> Self {
        Self {
            wrap,
            images: BTreeMap::new(),
            resident_ids: BTreeSet::new(),
            last_resend_ms: None,
        }
    }

    pub(crate) fn ensure<W: Write>(
        &mut self,
        writer: &mut W,
        request: ImageRequest<K, C>,
        png: impl FnOnce() -> Arc<[u8]>,
    ) -> io::Result<bool> {
        let ImageRequest {
            key,
            image_id,
            content,
            now_ms,
            cols,
            rows,
            synchronized,
        } = request;
        let changed = self
            .images
            .get(&key)
            .is_none_or(|resident| resident.image_id != image_id || resident.content != content);
        let stale = self.images.get(&key).is_some_and(|resident| {
            now_ms.saturating_sub(resident.sent_at_ms) >= RESIDENT_REFRESH_MS
        });
        let resend = stale
            && self.last_resend_ms.is_none_or(|last| {
                last == now_ms || now_ms.saturating_sub(last) >= MIN_RESEND_SPACING_MS
            });
        if !changed && !resend {
            return Ok(false);
        }

        let write_image = |writer: &mut W| -> io::Result<()> {
            let png = png();
            for chunk in transmit_png_chunks(image_id, &png) {
                writer.write_all(&wrap_pixel_payload(&chunk, self.wrap))?;
            }
            writer.write_all(&wrap_pixel_payload(
                &virtual_place(image_id, cols, rows, 2),
                self.wrap,
            ))
        };
        if synchronized {
            writer.write_all(&wrap_pixel_payload(BEGIN_SYNC, self.wrap))?;
            let body = write_image(writer);
            let end = writer.write_all(&wrap_pixel_payload(END_SYNC, self.wrap));
            body.and(end)?;
        } else {
            write_image(writer)?;
        }
        self.images.insert(
            key,
            Resident {
                image_id,
                content,
                sent_at_ms: now_ms,
            },
        );
        self.resident_ids.insert(image_id);
        if resend {
            self.last_resend_ms = Some(now_ms);
        }
        Ok(true)
    }

    /// Forget content without deleting terminal IDs, allowing in-place image
    /// replacement for a new pet and payload release while disabled.
    pub(crate) fn invalidate(&mut self) {
        self.images.clear();
        self.last_resend_ms = None;
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write_synchronized_pixel_output(writer, |writer| {
            for image_id in std::mem::take(&mut self.resident_ids) {
                writer.write_all(&wrap_pixel_payload(&delete(image_id), self.wrap))?;
            }
            self.invalidate();
            Ok(())
        })?;
        writer.flush()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &K) -> bool {
        self.images.contains_key(key)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn mark_resident(&mut self, key: K, image_id: u32, content: C, now_ms: u64) {
        self.images.insert(
            key,
            Resident {
                image_id,
                content,
                sent_at_ms: now_ms,
            },
        );
        self.resident_ids.insert(image_id);
    }

    #[cfg(test)]
    pub(crate) fn resident_contains(&self, image_id: u32) -> bool {
        self.resident_ids.contains(&image_id)
    }

    #[cfg(test)]
    pub(crate) fn resident_is_empty(&self) -> bool {
        self.resident_ids.is_empty()
    }
}

pub(crate) struct ImageRequest<K, C> {
    pub(crate) key: K,
    pub(crate) image_id: u32,
    pub(crate) content: C,
    pub(crate) now_ms: u64,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) synchronized: bool,
}

// Kitty's complete rowcolumn-diacritics list, derived from Unicode 6.0.
pub(crate) const ROW_COLUMN_DIACRITICS: [char; 297] = [
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

fn tmux_passthrough(payload: &[u8]) -> Vec<u8> {
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

fn image_id_rgb(image_id: u32) -> (u8, u8, u8) {
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
}
