//! Tracks pane-local pixel-pet image residency so dropped or evicted sprites self-heal.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Arc;

use super::PetPixelView;
use crate::sidebar_pane::pixel::{
    IMAGE_ID_COLOR_MASK, ImageRequest, ImageResidency, RgbaImage, encode_png,
    runtime_image_id_base, sprite_image_id,
};

/// Pet sprite indexes occupy the image IDs immediately above `id_base`.
/// Other pixel surfaces reserve offsets outside the bounded sprite-sheet range.
#[derive(Debug)]
pub(crate) struct PixelPainter {
    id_base: u32,
    pub(crate) pet_id: Option<String>,
    pub(crate) png: BTreeMap<usize, Arc<[u8]>>,
    residency: ImageResidency<usize, String>,
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
            pet_id: None,
            png: BTreeMap::new(),
            residency: ImageResidency::new(wrap),
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
            self.residency.invalidate();
            self.png.clear();
            self.pet_id = Some(pixel.pet_id.clone());
        }

        let image_id = pixel.image_id;
        debug_assert_eq!(image_id, self.image_id(pixel.sprite_index));
        let png = &mut self.png;
        self.residency.ensure(
            writer,
            ImageRequest {
                key: pixel.sprite_index,
                image_id,
                content: pixel.pet_id.clone(),
                now_ms,
                cols: pixel.size.cols,
                rows: pixel.size.rows,
                synchronized: false,
            },
            || {
                png.entry(pixel.sprite_index)
                    .or_insert_with(|| {
                        Arc::from(encode_png(frame.width, frame.height, &frame.data))
                    })
                    .clone()
            },
        )?;
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.residency.clear(writer)?;
        self.pet_id = None;
        self.png.clear();
        Ok(())
    }

    /// Release compressed and resend bookkeeping when pets are disabled. Keep
    /// only the tiny resident-id set so renderer teardown can still delete
    /// images already installed in the terminal.
    pub(crate) fn release_process_payload(&mut self) {
        self.pet_id = None;
        self.residency.invalidate();
        self.png.clear();
    }

    pub(crate) fn image_id(&self, sprite_index: usize) -> u32 {
        sprite_image_id(self.id_base, sprite_index)
    }

    #[cfg(test)]
    pub(crate) fn transmitted_contains(&self, sprite_index: usize) -> bool {
        self.residency.contains_key(&sprite_index)
    }

    #[cfg(test)]
    pub(crate) fn transmitted_is_empty(&self) -> bool {
        self.residency.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn resident_contains(&self, sprite_index: usize) -> bool {
        self.residency
            .resident_contains(self.image_id(sprite_index))
    }

    #[cfg(test)]
    pub(crate) fn resident_is_empty(&self) -> bool {
        self.residency.resident_is_empty()
    }

    #[cfg(test)]
    pub(crate) fn mark_resident_for_test(
        &mut self,
        pet_id: &str,
        sprite_index: usize,
        png: Vec<u8>,
    ) {
        let image_id = self.image_id(sprite_index);
        self.pet_id = Some(pet_id.to_owned());
        self.png.insert(sprite_index, Arc::from(png));
        self.residency
            .mark_resident(sprite_index, image_id, pet_id.to_owned(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::pets::PetGridSize;
    use crate::sidebar_pane::pixel::{
        BEGIN_SYNC, END_SYNC, MIN_RESEND_SPACING_MS, RESIDENT_REFRESH_MS, sprite_image_id,
    };

    fn image() -> RgbaImage {
        RgbaImage {
            width: 1,
            height: 1,
            data: vec![0, 1, 2, 3],
        }
    }

    fn view(pet_id: &str, sprite_index: usize) -> PetPixelView {
        PetPixelView {
            pet_id: pet_id.to_owned(),
            sprite_index,
            image_id: sprite_image_id(0x120000, sprite_index),
            size: PetGridSize { cols: 2, rows: 1 },
        }
    }

    fn contains(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.windows(needle.len()).any(|window| window == needle)
    }

    #[test]
    fn first_transmit_is_lazy_and_repeat_writes_nothing() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let pixel = view("codex", 0);
        let mut first = Vec::new();
        painter
            .ensure_transmitted(&mut first, &pixel, &image(), 0)
            .expect("first transmit");

        assert!(contains(&first, b"a=t,f=100,i=1179648,q=2"));
        assert!(contains(&first, b"a=p,U=1,i=1179648,c=2,r=1,q=2"));
        assert!(!first.starts_with(BEGIN_SYNC));
        let png = painter.png.get(&0).expect("lazy PNG").clone();
        let mut repeat = Vec::new();
        painter
            .ensure_transmitted(&mut repeat, &pixel, &image(), 1)
            .expect("repeat transmit");
        assert!(repeat.is_empty());
        assert!(Arc::ptr_eq(&png, painter.png.get(&0).expect("cached PNG")));
    }

    #[test]
    fn stale_sprites_share_frame_resends_then_obey_spacing() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        let first = view("codex", 0);
        let second = view("codex", 1);
        let third = view("codex", 2);
        for pixel in [&first, &second, &third] {
            painter
                .ensure_transmitted(&mut Vec::new(), pixel, &image(), 0)
                .expect("initial transmit");
        }

        for pixel in [&first, &second] {
            let mut stale = Vec::new();
            painter
                .ensure_transmitted(&mut stale, pixel, &image(), RESIDENT_REFRESH_MS)
                .expect("same-frame resend");
            assert!(!stale.is_empty());
        }
        let mut too_soon = Vec::new();
        painter
            .ensure_transmitted(
                &mut too_soon,
                &third,
                &image(),
                RESIDENT_REFRESH_MS + MIN_RESEND_SPACING_MS - 1,
            )
            .expect("throttled resend");
        assert!(too_soon.is_empty());
        let mut spaced = Vec::new();
        painter
            .ensure_transmitted(
                &mut spaced,
                &third,
                &image(),
                RESIDENT_REFRESH_MS + MIN_RESEND_SPACING_MS,
            )
            .expect("spaced resend");
        assert!(!spaced.is_empty());
    }

    #[test]
    fn pet_change_reuses_ids_without_delete_and_clear_deletes_every_slot() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        for pixel in [view("codex", 0), view("codex", 1)] {
            painter
                .ensure_transmitted(&mut Vec::new(), &pixel, &image(), 0)
                .expect("initial pet");
        }
        let mut changed = Vec::new();
        painter
            .ensure_transmitted(&mut changed, &view("claude", 0), &image(), 0)
            .expect("pet change");
        assert!(!contains(&changed, b"a=d,d=i"));
        assert!(painter.transmitted_contains(0));
        assert!(!painter.transmitted_contains(1));
        assert!(painter.resident_contains(0));
        assert!(painter.resident_contains(1));

        let mut clear = Vec::new();
        painter.clear(&mut clear).expect("clear");
        assert!(clear.starts_with(BEGIN_SYNC));
        assert!(clear.ends_with(END_SYNC));
        assert!(contains(&clear, b"a=d,d=i,i=1179648,q=2"));
        assert!(contains(&clear, b"a=d,d=i,i=1179649,q=2"));
        assert!(painter.transmitted_is_empty());
        assert!(painter.resident_is_empty());
    }

    #[test]
    fn disabled_pets_release_payload_but_keep_terminal_ids() {
        let mut painter = PixelPainter::with_id_base(0x120000, true);
        painter.mark_resident_for_test("codex", 3, vec![1]);

        painter.release_process_payload();

        assert!(painter.pet_id.is_none());
        assert!(painter.transmitted_is_empty());
        assert!(painter.png.is_empty());
        assert!(painter.resident_contains(3));
    }
}
