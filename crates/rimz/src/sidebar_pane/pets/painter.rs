//! Tracks pane-local pixel-pet image residency so dropped or evicted sprites self-heal.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;

use super::PetPixelView;
use super::frames::{RgbaImage, encode_png};
use crate::sidebar_pane::pixel::{
    IMAGE_ID_COLOR_MASK, MIN_RESEND_SPACING_MS, RESIDENT_REFRESH_MS, delete, runtime_image_id_base,
    sprite_image_id, transmit_png_chunks, virtual_place, wrap_pixel_payload,
    write_synchronized_pixel_output,
};

/// Pet sprite indexes occupy the image IDs immediately above `id_base`.
/// Other pixel surfaces reserve offsets outside the bounded sprite-sheet range.
#[derive(Debug)]
pub(crate) struct PixelPainter {
    id_base: u32,
    wrap: bool,
    pub(crate) pet_id: Option<String>,
    pub(crate) transmitted: BTreeMap<usize, u64>,
    pub(crate) png: BTreeMap<usize, Arc<[u8]>>,
    pub(crate) resident: BTreeSet<usize>,
    last_resend_ms: Option<u64>,
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
            transmitted: BTreeMap::new(),
            png: BTreeMap::new(),
            resident: BTreeSet::new(),
            last_resend_ms: None,
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
        now_ms: u64,
    ) -> io::Result<()> {
        let pet_changed = self.pet_id.as_deref() != Some(pixel.pet_id.as_str());
        if pet_changed {
            // The same sprite ids are re-transmitted with the new sheet and
            // replace image data in place; no delete APC is emitted mid-session.
            self.transmitted.clear();
            self.png.clear();
            self.pet_id = Some(pixel.pet_id.clone());
        }

        let image_id = pixel.image_id;
        debug_assert_eq!(image_id, self.image_id(pixel.sprite_index));
        let first = !self.transmitted.contains_key(&pixel.sprite_index);
        let stale = self
            .transmitted
            .get(&pixel.sprite_index)
            .is_some_and(|last| now_ms.saturating_sub(*last) >= RESIDENT_REFRESH_MS);
        let resend = stale
            && self
                .last_resend_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= MIN_RESEND_SPACING_MS);
        if first || resend {
            self.resident.insert(pixel.sprite_index);
            let png = self
                .png
                .entry(pixel.sprite_index)
                .or_insert_with(|| Arc::from(encode_png(frame.width, frame.height, &frame.data)))
                .clone();
            for chunk in transmit_png_chunks(image_id, &png) {
                writer.write_all(&self.wrap_payload(&chunk))?;
            }
            writer.write_all(&self.wrap_payload(&virtual_place(
                image_id,
                pixel.size.cols,
                pixel.size.rows,
                2,
            )))?;
            self.transmitted.insert(pixel.sprite_index, now_ms);
            if resend {
                self.last_resend_ms = Some(now_ms);
            }
        }
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write_synchronized_pixel_output(writer, |writer| self.delete_transmitted(writer))?;
        self.pet_id = None;
        writer.flush()
    }

    /// Release compressed and resend bookkeeping when pets are disabled. Keep
    /// only the tiny resident-id set so renderer teardown can still delete
    /// images already installed in the terminal.
    pub(crate) fn release_process_payload(&mut self) {
        self.pet_id = None;
        self.transmitted.clear();
        self.png.clear();
        self.last_resend_ms = None;
    }

    fn delete_transmitted<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let sprite_indexes = std::mem::take(&mut self.resident);
        for sprite_index in sprite_indexes {
            writer.write_all(&self.wrap_payload(&delete(self.image_id(sprite_index))))?;
        }
        self.transmitted.clear();
        self.png.clear();
        Ok(())
    }

    pub(crate) fn image_id(&self, sprite_index: usize) -> u32 {
        sprite_image_id(self.id_base, sprite_index)
    }

    fn wrap_payload(&self, payload: &[u8]) -> Vec<u8> {
        wrap_pixel_payload(payload, self.wrap)
    }
}
