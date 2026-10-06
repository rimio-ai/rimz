//! Shared kitty graphics transport for pane-resident pixel surfaces.

pub(super) mod meter;
mod pacing;
pub(super) mod probe;
mod tty;

pub use pacing::LiveGraphicsPacer;
pub use probe::{PixelRenderCaps, detect_env as detect_pixel_render_env};

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;

use ratatui::style::Color;

use crate::pixel_wire::{PLACEHOLDER, ROW_COLUMN_DIACRITICS};

const ESC: u8 = 0x1b;
pub(super) const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
pub(super) const END_SYNC: &[u8] = b"\x1b[?2026l";
const CHUNK_SIZE: usize = 4096;
const MAX_PIXEL_SLOTS: usize = 256;
const SLOT_ORIGIN: u32 = 0x520000;
const SLOT_STRIDE: u32 = 0x200;
pub(super) const METER_ID_OFFSET: u32 = 0x100;
pub(super) const METER_ID_CAPACITY: u32 = 256;
pub(super) const IMAGE_ID_COLOR_MASK: u32 = 0x00ff_ffff;
pub(super) const RESIDENT_REFRESH_MS: u64 = 2000;
pub(super) const MIN_RESEND_SPACING_MS: u64 = 250;

/// Cross-version image-id layout. Moving it orphans images from older workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PixelSlot(u8);

impl PixelSlot {
    pub(super) fn new(index: u8) -> Self {
        Self(index)
    }

    pub(super) fn base(self) -> u32 {
        SLOT_ORIGIN + u32::from(self.0) * SLOT_STRIDE
    }

    /// Unbracketed: the caller's frame carries the synchronized-output bracket.
    pub(super) fn sweep<W: Write>(self, writer: &mut W, wrap: bool) -> io::Result<()> {
        for image_id in self.base()..self.base() + SLOT_STRIDE {
            writer.write_all(&wrap_pixel_payload(&delete(image_id), wrap))?;
        }
        Ok(())
    }
}

pub(super) struct PixelLease {
    pub(super) slot: PixelSlot,
    _guard: crate::disk::lock::WorkspaceLock,
}

impl PixelLease {
    pub(super) fn acquire(
        runtime: &crate::disk::paths::RuntimePaths,
    ) -> crate::disk::lock::Result<Option<Self>> {
        for index in 0..MAX_PIXEL_SLOTS {
            let slot = PixelSlot::new(index as u8);
            if let Some(guard) = crate::disk::lock::WorkspaceLock::try_acquire(
                &runtime.shared_pixel_slot_lock(slot.0),
            )? {
                return Ok(Some(Self {
                    slot,
                    _guard: guard,
                }));
            }
        }
        Ok(None)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RgbaImage {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) data: Vec<u8>,
}

impl RgbaImage {
    pub(super) fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
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
pub(super) struct ImageResidency<K, C> {
    wrap: bool,
    images: BTreeMap<K, Resident<C>>,
    resident_ids: BTreeSet<u32>,
    last_resend_ms: Option<u64>,
}

impl<K: Ord, C: Eq> ImageResidency<K, C> {
    pub(super) fn new(wrap: bool) -> Self {
        Self {
            wrap,
            images: BTreeMap::new(),
            resident_ids: BTreeSet::new(),
            last_resend_ms: None,
        }
    }

    pub(super) fn ensure<W: Write>(
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
            let mut payload = transmit_png(image_id, &png);
            payload.extend_from_slice(&resident_place(image_id, cols, rows));
            writer.write_all(&wrap_pixel_payload(&payload, self.wrap))
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
    pub(super) fn invalidate(&mut self) {
        self.images.clear();
        self.last_resend_ms = None;
    }

    /// Unbracketed: the caller's frame or teardown carries the
    /// synchronized-output bracket, since mode 2026 does not nest.
    pub(super) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        for image_id in std::mem::take(&mut self.resident_ids) {
            writer.write_all(&wrap_pixel_payload(&delete(image_id), self.wrap))?;
        }
        self.invalidate();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.images.contains_key(key)
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    #[cfg(test)]
    pub(super) fn mark_resident(&mut self, key: K, image_id: u32, content: C, now_ms: u64) {
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
    pub(super) fn resident_contains(&self, image_id: u32) -> bool {
        self.resident_ids.contains(&image_id)
    }

    #[cfg(test)]
    pub(super) fn resident_is_empty(&self) -> bool {
        self.resident_ids.is_empty()
    }
}

pub(super) struct ImageRequest<K, C> {
    pub(super) key: K,
    pub(super) image_id: u32,
    pub(super) content: C,
    pub(super) now_ms: u64,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) synchronized: bool,
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

pub(super) fn meter_image_id(id_base: u32, index: u32) -> u32 {
    id_base + METER_ID_OFFSET + index
}

pub(super) fn sprite_image_id(id_base: u32, sprite_index: usize) -> u32 {
    let id = id_base.wrapping_add(sprite_index as u32) & IMAGE_ID_COLOR_MASK;
    id.max(1)
}

pub fn transmit_png(image_id: u32, png: &[u8]) -> Vec<u8> {
    let payload = base64(png);
    if payload.len() <= CHUNK_SIZE {
        return kitty_escape(&format!("a=t,f=100,i={image_id},q=2"), payload.as_bytes());
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
        out.extend_from_slice(&kitty_escape(&control, chunk));
    }
    out
}

pub fn virtual_place(image_id: u32, cols: u16, rows: u16, quiet: u8) -> Vec<u8> {
    kitty_escape(
        &format!("a=p,U=1,i={image_id},c={cols},r={rows},q={quiet}"),
        &[],
    )
}

fn resident_place(image_id: u32, cols: u16, rows: u16) -> Vec<u8> {
    kitty_escape(
        &format!("a=p,U=1,i={image_id},p=1,c={cols},r={rows},q=2"),
        &[],
    )
}

fn delete(image_id: u32) -> Vec<u8> {
    kitty_escape(&format!("a=d,d=I,i={image_id},q=2"), &[])
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

pub(super) fn placeholder_cluster(row: u16, col: u16) -> String {
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

pub(super) fn image_id_color(image_id: u32) -> Color {
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

pub(super) fn placeholder_columns_supported(width: usize) -> bool {
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
        let bytes = transmit_png(42, &encode_png(1, 1, &[0, 1, 2, 3]));
        let text = String::from_utf8(bytes).expect("ascii kitty escapes");

        assert!(text.contains("a=t,f=100,i=42,q=2;iVBORw0KGgo"));
    }

    #[test]
    fn transmit_chunks_large_payload() {
        let png = vec![1_u8; 8192];
        let text = String::from_utf8(transmit_png(7, &png)).expect("ascii kitty escapes");
        let chunks: Vec<_> = text.split_terminator("\x1b\\").collect();
        assert_eq!(chunks.len(), 3);
        let mut payload = String::new();
        for (chunk, control) in
            chunks
                .iter()
                .zip(["\x1b_Ga=t,f=100,i=7,q=2,m=1", "\x1b_Gm=1", "\x1b_Gm=0"])
        {
            let (actual_control, data) = chunk.split_once(';').expect("kitty APC");
            assert_eq!(actual_control, control);
            assert!(data.len() <= 4096);
            payload.push_str(data);
        }
        assert_eq!(payload, base64(&png));
    }

    #[test]
    fn residency_wraps_whole_image_for_tmux() {
        let png: Arc<[u8]> = vec![1_u8; 6144].into();
        let expected = format!(
            "\x1b_Ga=t,f=100,i=7,q=2,m=1;{}\x1b\\\x1b_Gm=0;{}\x1b\\\x1b_Ga=p,U=1,i=7,p=1,c=12,r=6,q=2;\x1b\\",
            "AQEB".repeat(1024),
            "AQEB".repeat(1024),
        );
        for wrap in [false, true] {
            let mut residency = ImageResidency::new(wrap);
            let mut bytes = Vec::new();
            residency
                .ensure(
                    &mut bytes,
                    ImageRequest {
                        key: 7,
                        image_id: 7,
                        content: (),
                        now_ms: 0,
                        cols: 12,
                        rows: 6,
                        synchronized: false,
                    },
                    || png.clone(),
                )
                .expect("transmit");
            let text = String::from_utf8(bytes).expect("ascii kitty escapes");
            assert_eq!(text.matches("\x1bPtmux;").count(), usize::from(wrap));
            let unwrapped = if wrap {
                text.strip_prefix("\x1bPtmux;")
                    .expect("envelope")
                    .strip_suffix("\x1b\\")
                    .expect("envelope end")
                    .replace("\x1b\x1b", "\x1b")
            } else {
                text
            };
            assert_eq!(unwrapped, expected);
        }
    }

    #[test]
    fn place_delete_and_passthrough_encode_protocol_bytes() {
        assert_eq!(
            resident_place(42, 12, 6),
            b"\x1b_Ga=p,U=1,i=42,p=1,c=12,r=6,q=2;\x1b\\".to_vec()
        );
        assert_eq!(delete(42), b"\x1b_Ga=d,d=I,i=42,q=2;\x1b\\".to_vec());
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
