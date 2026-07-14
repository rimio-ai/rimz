//! Pixel-precise context-meter rasterization and pane-local image residency.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crate::sidebar_pane::pets::{RgbaImage, encode_png};
use ratatui::style::Color;
use ratatui::text::Line;

use super::{
    BEGIN_SYNC, END_SYNC, IMAGE_ID_COLOR_MASK, MIN_RESEND_SPACING_MS, RESIDENT_REFRESH_MS, delete,
    transmit_png_chunks, virtual_place, wrap_pixel_payload, write_synchronized_pixel_output,
};

const CELL_W: u32 = 8;
const CELL_H: u32 = 16;
/// Pet sprites use IDs directly above the shared base; their sheets are bounded
/// far below this offset. Meter slots occupy the range beginning here.
pub(crate) const METER_ID_OFFSET: u32 = 0x4000;
const METER_ID_CAPACITY: u32 = 512;

/// The exact pixel shape of one context meter. Quantizing before interning keeps
/// visually identical sub-pixel updates on the same terminal image id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MeterRaster {
    pub(crate) width_cells: u16,
    fill_px: u32,
    pub(crate) health: [u8; 3],
    runs: Vec<(u32, [u8; 3])>,
    pub(crate) track: [u8; 3],
}

impl MeterRaster {
    pub(crate) fn new(
        width_cells: u16,
        fill: f64,
        mut health: [u8; 3],
        segments: Vec<(u64, [u8; 3])>,
        mut track: [u8; 3],
    ) -> Self {
        let width = u32::from(width_cells).saturating_mul(CELL_W).max(1);
        let fill = fill.clamp(0.0, 1.0);
        let fill_px = (fill * f64::from(width)).round() as u32;
        let fill_px = if fill > 0.0 { fill_px.max(1) } else { 0 };
        let active = segments
            .into_iter()
            .filter(|(weight, _)| *weight > 0)
            .collect::<Vec<_>>();
        let mut runs = active
            .iter()
            .zip(apportion_pixels(&active, fill_px))
            .filter_map(|((_, color), width)| (width > 0).then_some((width, *color)))
            .collect::<Vec<_>>();
        if fill_px == 0 {
            health = [0; 3];
        }
        if let Some((_, color)) = runs.first_mut() {
            *color = health;
        }
        for (width, color) in runs.iter_mut().skip(1) {
            if *width <= 2 {
                *color = [0; 3];
            }
        }
        if fill_px == width {
            track = [0; 3];
        }
        Self {
            width_cells,
            fill_px,
            health,
            runs,
            track,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    index: u32,
    touched: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MeterPixels {
    pub(crate) id_base: u32,
    table: BTreeMap<MeterRaster, Entry>,
    clock: u64,
    /// Image ids referenced by the terminal's previous frame. They stay
    /// protected until composition identifies the next visible set.
    visible: BTreeSet<u32>,
    next_index: u32,
}

impl MeterPixels {
    pub(crate) fn new(id_base: u32) -> Self {
        Self {
            id_base,
            table: BTreeMap::new(),
            clock: 0,
            visible: BTreeSet::new(),
            next_index: 0,
        }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.clock = self.clock.wrapping_add(1);
    }

    /// Return the content-stable image id, or fall back to the cell bar when the
    /// fixed id window is entirely protected by either terminal-visible images
    /// or placeholders already composed in this frame.
    pub(crate) fn intern(&mut self, raster: MeterRaster) -> Option<u32> {
        if let Some(entry) = self.table.get_mut(&raster) {
            entry.touched = self.clock;
            return Some(meter_image_id(self.id_base, entry.index));
        }

        let index = if self.next_index < METER_ID_CAPACITY {
            let index = self.next_index;
            self.next_index += 1;
            index
        } else {
            let evicted = self
                .table
                .iter()
                .filter(|(_, entry)| {
                    entry.touched != self.clock
                        && !self
                            .visible
                            .contains(&meter_image_id(self.id_base, entry.index))
                })
                .min_by_key(|(_, entry)| (entry.touched, entry.index))
                .map(|(raster, entry)| (raster.clone(), entry.index));
            let (evicted, index) = evicted?;
            self.table.remove(&evicted);
            index
        };
        self.table.insert(
            raster,
            Entry {
                index,
                touched: self.clock,
            },
        );
        Some(meter_image_id(self.id_base, index))
    }

    /// Replace the previous-frame protection set with ids referenced by the
    /// final viewport. This pass observes placeholders without rewriting them.
    pub(crate) fn observe_visible(&mut self, lines: &[Line<'static>]) {
        let interned = self
            .table
            .values()
            .map(|entry| meter_image_id(self.id_base, entry.index))
            .collect::<BTreeSet<_>>();
        let mut visible = BTreeSet::new();
        for line in lines {
            for span in &line.spans {
                if !span.content.contains('\u{10eeee}') {
                    continue;
                }
                let Some(Color::Rgb(red, green, blue)) = span.style.fg else {
                    continue;
                };
                let image_id = u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue);
                if interned.contains(&image_id) {
                    visible.insert(image_id);
                }
            }
        }
        for entry in self.table.values_mut() {
            if visible.contains(&meter_image_id(self.id_base, entry.index)) {
                entry.touched = self.clock;
            }
        }
        self.visible = visible;
    }

    pub(crate) fn visible_rasters(&self) -> impl Iterator<Item = (u32, &MeterRaster)> {
        self.table.iter().filter_map(|(raster, entry)| {
            let image_id = meter_image_id(self.id_base, entry.index);
            self.visible
                .contains(&image_id)
                .then_some((image_id, raster))
        })
    }
}

pub(crate) fn meter_image_id(id_base: u32, index: u32) -> u32 {
    (id_base.wrapping_add(METER_ID_OFFSET).wrapping_add(index) & IMAGE_ID_COLOR_MASK).max(1)
}

pub(crate) fn rasterize(raster: &MeterRaster) -> RgbaImage {
    let width = u32::from(raster.width_cells).saturating_mul(CELL_W).max(1);
    let mut image = RgbaImage {
        width,
        height: CELL_H,
        data: vec![0; width as usize * CELL_H as usize * 4],
    };
    paint_rect(&mut image, 0, 7, width, 1, raster.track);
    if raster.fill_px == 0 {
        return image;
    }

    if raster.runs.is_empty() {
        paint_rect(&mut image, 0, 6, raster.fill_px, 3, raster.health);
        return image;
    }

    let mut x = 0;
    for (index, (run_width, color)) in raster.runs.iter().copied().enumerate() {
        paint_rect(&mut image, x, 6, run_width, 3, color);
        if index > 0 {
            clear_rect(&mut image, x, 6, run_width.min(2), 3);
        }
        x += run_width;
    }
    image
}

fn apportion_pixels(segments: &[(u64, [u8; 3])], total: u32) -> Vec<u32> {
    let weight: u128 = segments.iter().map(|(value, _)| u128::from(*value)).sum();
    if weight == 0 {
        return vec![0; segments.len()];
    }
    let mut shares: Vec<(usize, u32, u128)> = segments
        .iter()
        .enumerate()
        .map(|(index, (value, _))| {
            let scaled = u128::from(*value) * u128::from(total);
            (index, (scaled / weight) as u32, scaled % weight)
        })
        .collect();
    let assigned: u32 = shares.iter().map(|(_, share, _)| *share).sum();
    shares.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    for (_, share, _) in shares.iter_mut().take((total - assigned) as usize) {
        *share += 1;
    }
    shares.sort_by_key(|(index, _, _)| *index);
    shares.into_iter().map(|(_, share, _)| share).collect()
}

fn paint_rect(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: [u8; 3]) {
    for row in y..(y + height).min(image.height) {
        for col in x..(x + width).min(image.width) {
            let offset = ((row * image.width + col) * 4) as usize;
            image.data[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
}

fn clear_rect(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32) {
    for row in y..(y + height).min(image.height) {
        for col in x..(x + width).min(image.width) {
            let offset = ((row * image.width + col) * 4) as usize;
            image.data[offset..offset + 4].fill(0);
        }
    }
}

#[derive(Debug)]
pub(crate) struct MeterPainter {
    wrap: bool,
    images: BTreeMap<u32, (MeterRaster, u64)>,
    resident: BTreeSet<u32>,
    last_resend_ms: Option<u64>,
}

impl MeterPainter {
    pub(crate) fn new(wrap: bool) -> Self {
        Self {
            wrap,
            images: BTreeMap::new(),
            resident: BTreeSet::new(),
            last_resend_ms: None,
        }
    }

    pub(crate) fn ensure_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        image_id: u32,
        raster: &MeterRaster,
        now_ms: u64,
    ) -> io::Result<()> {
        let changed = self
            .images
            .get(&image_id)
            .is_none_or(|(previous, _)| previous != raster);
        let stale = self
            .images
            .get(&image_id)
            .is_some_and(|(_, last)| now_ms.saturating_sub(*last) >= RESIDENT_REFRESH_MS);
        let resend = stale
            && self.last_resend_ms.is_none_or(|last| {
                last == now_ms || now_ms.saturating_sub(last) >= MIN_RESEND_SPACING_MS
            });
        if !changed && !resend {
            return Ok(());
        }

        let image = rasterize(raster);
        let png = encode_png(image.width, image.height, &image.data);
        writer.write_all(&wrap_pixel_payload(BEGIN_SYNC, self.wrap))?;
        let transmit_result = (|| {
            for chunk in transmit_png_chunks(image_id, &png) {
                writer.write_all(&wrap_pixel_payload(&chunk, self.wrap))?;
            }
            writer.write_all(&wrap_pixel_payload(
                &virtual_place(image_id, raster.width_cells, 1, 2),
                self.wrap,
            ))
        })();
        let end_result = writer.write_all(&wrap_pixel_payload(END_SYNC, self.wrap));
        transmit_result.and(end_result)?;
        self.images.insert(image_id, (raster.clone(), now_ms));
        self.resident.insert(image_id);
        if resend {
            self.last_resend_ms = Some(now_ms);
        }
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write_synchronized_pixel_output(writer, |writer| {
            for image_id in std::mem::take(&mut self.resident) {
                writer.write_all(&wrap_pixel_payload(&delete(image_id), self.wrap))?;
            }
            self.images.clear();
            Ok(())
        })?;
        writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Span;

    fn raster(fill: f64) -> MeterRaster {
        MeterRaster::new(2, fill, [1, 2, 3], Vec::new(), [4, 5, 6])
    }

    fn distinct_raster(index: u32) -> MeterRaster {
        MeterRaster::new(
            2,
            0.5,
            [
                ((index >> 16) & 0xff) as u8,
                ((index >> 8) & 0xff) as u8,
                (index & 0xff) as u8,
            ],
            Vec::new(),
            [4, 5, 6],
        )
    }

    fn placeholder(image_id: u32) -> Span<'static> {
        Span::styled(
            super::super::placeholder_cluster(0, 0),
            Style::default().fg(super::super::image_id_color(image_id)),
        )
    }

    fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        image.pixel(x, y)
    }

    #[test]
    fn rasterize_rounds_fill_to_exact_pixel_columns() {
        for (fill, columns) in [(0.0, 0), (f64::EPSILON, 1), (0.25, 4), (0.51, 8), (1.0, 16)] {
            let image = rasterize(&raster(fill));
            let painted = (0..image.width)
                .filter(|x| pixel(&image, *x, 6)[3] == 255)
                .count();
            assert_eq!(painted, columns, "fill={fill}");
        }
    }

    #[test]
    fn rasterize_uses_health_then_segment_colors_and_carves_notch() {
        let image = rasterize(&MeterRaster::new(
            2,
            1.0,
            [1, 2, 3],
            vec![(1, [9, 9, 9]), (1, [7, 8, 9])],
            [4, 5, 6],
        ));
        assert_eq!(pixel(&image, 0, 6), [1, 2, 3, 255]);
        assert_eq!(pixel(&image, 8, 6), [0, 0, 0, 0]);
        assert_eq!(pixel(&image, 9, 8), [0, 0, 0, 0]);
        assert_eq!(pixel(&image, 10, 6), [7, 8, 9, 255]);
        assert_eq!(pixel(&image, 15, 7), [7, 8, 9, 255]);
    }

    #[test]
    fn raster_key_ignores_colors_hidden_by_quantized_pixels() {
        assert_eq!(
            MeterRaster::new(2, 0.0, [1, 2, 3], Vec::new(), [4, 5, 6]),
            MeterRaster::new(2, 0.0, [9, 8, 7], Vec::new(), [4, 5, 6]),
            "an empty fill does not paint the health color"
        );
        assert_eq!(
            MeterRaster::new(2, 1.0, [1, 2, 3], Vec::new(), [4, 5, 6]),
            MeterRaster::new(2, 1.0, [1, 2, 3], Vec::new(), [9, 8, 7]),
            "a full fill covers the track color"
        );
        assert_eq!(
            MeterRaster::new(
                2,
                0.125,
                [1, 2, 3],
                vec![(1, [9, 9, 9]), (1, [4, 5, 6])],
                [7, 8, 9],
            ),
            MeterRaster::new(
                2,
                0.125,
                [1, 2, 3],
                vec![(1, [5, 5, 5]), (1, [8, 8, 8])],
                [7, 8, 9],
            ),
            "the first run uses health and a two-pixel later run is all notch"
        );
    }

    #[test]
    fn interning_and_painting_deduplicate_visual_content() {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        let first = raster(0.5);
        let same_id = pixels.intern(first.clone()).expect("first id");
        assert_eq!(pixels.intern(first.clone()), Some(same_id));

        let subpixel = raster(0.5001);
        assert_eq!(subpixel, first);
        assert_eq!(pixels.intern(subpixel.clone()), Some(same_id));

        let distinct = raster(0.75);
        let distinct_id = pixels.intern(distinct.clone()).expect("distinct id");
        assert_ne!(distinct_id, same_id);

        let mut painter = MeterPainter::new(false);
        let mut first_bytes = Vec::new();
        painter
            .ensure_transmitted(&mut first_bytes, same_id, &first, 0)
            .expect("first");
        let text = String::from_utf8_lossy(&first_bytes);
        assert!(text.contains("a=t"));
        assert!(text.contains("a=p"));
        assert!(text.contains(&format!("i={same_id}")));
        assert!(first_bytes.starts_with(BEGIN_SYNC));
        assert!(first_bytes.ends_with(END_SYNC));

        let mut repeat = Vec::new();
        painter
            .ensure_transmitted(&mut repeat, same_id, &subpixel, 1)
            .expect("same");
        assert!(repeat.is_empty());

        let mut distinct_bytes = Vec::new();
        painter
            .ensure_transmitted(&mut distinct_bytes, distinct_id, &distinct, 2)
            .expect("distinct");
        assert!(!distinct_bytes.is_empty());

        let mut clear = Vec::new();
        painter.clear(&mut clear).expect("clear");
        assert!(String::from_utf8_lossy(&clear).contains("a=d"));
    }

    #[test]
    fn painter_wraps_each_payload_for_tmux() {
        let expected_payloads = {
            let image = rasterize(&raster(0.5));
            transmit_png_chunks(
                meter_image_id(0x120000, 0),
                &encode_png(image.width, image.height, &image.data),
            )
            .len()
                + 3
        };
        let mut painter = MeterPainter::new(true);
        let mut bytes = Vec::new();
        painter
            .ensure_transmitted(&mut bytes, meter_image_id(0x120000, 0), &raster(0.5), 0)
            .expect("paint");
        assert_eq!(
            bytes
                .windows(b"\x1bPtmux;".len())
                .filter(|window| *window == b"\x1bPtmux;")
                .count(),
            expected_payloads
        );
        assert!(!bytes.starts_with(super::super::BEGIN_SYNC));
        assert!(bytes.starts_with(&wrap_pixel_payload(BEGIN_SYNC, true)));
        assert!(bytes.ends_with(&wrap_pixel_payload(END_SYNC, true)));

        let mut no_op = Vec::new();
        painter
            .ensure_transmitted(&mut no_op, meter_image_id(0x120000, 0), &raster(0.5), 1)
            .expect("unchanged");
        assert!(no_op.is_empty(), "a no-op frame writes no sync bytes");
    }

    #[test]
    fn painter_resends_stale_images_with_global_spacing() {
        let first_id = meter_image_id(0x120000, 0);
        let second_id = meter_image_id(0x120000, 1);
        let third_id = meter_image_id(0x120000, 2);
        let mut painter = MeterPainter::new(false);
        painter
            .ensure_transmitted(&mut Vec::new(), first_id, &raster(0.5), 0)
            .expect("first resident");
        painter
            .ensure_transmitted(&mut Vec::new(), second_id, &raster(0.75), 0)
            .expect("second resident");
        painter
            .ensure_transmitted(&mut Vec::new(), third_id, &raster(1.0), 0)
            .expect("third resident");

        let mut first_stale = Vec::new();
        painter
            .ensure_transmitted(
                &mut first_stale,
                first_id,
                &raster(0.5),
                RESIDENT_REFRESH_MS,
            )
            .expect("first stale");
        assert!(!first_stale.is_empty());

        let mut same_frame = Vec::new();
        painter
            .ensure_transmitted(
                &mut same_frame,
                second_id,
                &raster(0.75),
                RESIDENT_REFRESH_MS,
            )
            .expect("same-frame batch");
        assert!(!same_frame.is_empty());

        let mut too_soon = Vec::new();
        painter
            .ensure_transmitted(
                &mut too_soon,
                third_id,
                &raster(1.0),
                RESIDENT_REFRESH_MS + 1,
            )
            .expect("globally spaced");
        assert!(too_soon.is_empty());

        let mut spaced = Vec::new();
        painter
            .ensure_transmitted(
                &mut spaced,
                third_id,
                &raster(1.0),
                RESIDENT_REFRESH_MS + MIN_RESEND_SPACING_MS,
            )
            .expect("spacing elapsed");
        assert!(!spaced.is_empty());
    }

    #[test]
    fn capacity_reuses_lru_without_touching_visible_or_current_ids() {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        let ids = (0..METER_ID_CAPACITY)
            .map(|index| pixels.intern(distinct_raster(index)).expect("free id"))
            .collect::<Vec<_>>();
        pixels.observe_visible(&[Line::from(placeholder(ids[0]))]);

        pixels.begin_frame();
        assert_eq!(pixels.intern(distinct_raster(1)), Some(ids[1]));
        let replacement = distinct_raster(METER_ID_CAPACITY);
        let reused = pixels.intern(replacement.clone()).expect("reused id");
        assert_eq!(reused, ids[2], "the oldest unprotected id is reused");
        assert_ne!(reused, ids[0], "the previous visible id stays immutable");
        assert_ne!(reused, ids[1], "this frame's placeholder stays immutable");

        let mut painter = MeterPainter::new(false);
        painter
            .ensure_transmitted(&mut Vec::new(), reused, &distinct_raster(2), 0)
            .expect("old raster");
        let mut changed = Vec::new();
        painter
            .ensure_transmitted(&mut changed, reused, &replacement, 1)
            .expect("replacement raster");
        assert!(!changed.is_empty(), "id reuse transmits the new raster");
    }

    #[test]
    fn capacity_falls_back_when_every_id_is_current() {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        for index in 0..METER_ID_CAPACITY {
            assert!(pixels.intern(distinct_raster(index)).is_some());
        }
        assert_eq!(pixels.intern(distinct_raster(METER_ID_CAPACITY)), None);
    }

    #[test]
    fn observe_visible_collects_only_allocated_meter_placeholders() {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        let visible_id = pixels.intern(raster(0.5)).expect("visible id");
        let hidden_id = pixels.intern(raster(0.75)).expect("hidden id");
        let unallocated_meter_id = meter_image_id(0x120000, 10);
        let pet_id = 0x120000;
        let lines = vec![
            Line::from(placeholder(visible_id)),
            Line::from(Span::styled(
                "ordinary",
                Style::default().fg(super::super::image_id_color(hidden_id)),
            )),
            Line::from(placeholder(unallocated_meter_id)),
            Line::from(placeholder(pet_id)),
        ];

        pixels.observe_visible(&lines);

        assert_eq!(
            pixels
                .visible_rasters()
                .map(|(image_id, _)| image_id)
                .collect::<Vec<_>>(),
            vec![visible_id]
        );
    }
}
