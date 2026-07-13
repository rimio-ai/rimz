//! Pixel-precise context-meter rasterization and pane-local image residency.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crate::sidebar_pane::pets::{RgbaImage, encode_png};
use ratatui::style::Color;
use ratatui::text::Line;

use super::{
    IMAGE_ID_COLOR_MASK, MIN_RESEND_SPACING_MS, RESIDENT_REFRESH_MS, delete, transmit_png_chunks,
    virtual_place, wrap_pixel_payload, write_synchronized_pixel_output,
};

const CELL_W: u32 = 8;
const CELL_H: u32 = 16;
/// Pet sprites use IDs directly above the shared base; their sheets are bounded
/// far below this offset. Meter slots occupy the range beginning here.
pub(crate) const METER_ID_OFFSET: u32 = 0x4000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MeterBarSpec {
    pub(crate) width_cells: u16,
    /// Filled fraction of the bar width, clamped to `0.0..=1.0` at rasterization.
    pub(crate) fill: f64,
    pub(crate) health: [u8; 3],
    pub(crate) segments: Vec<(u64, [u8; 3])>,
    pub(crate) track: [u8; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MeterPixels {
    pub(crate) id_base: u32,
    pub(crate) specs: Vec<MeterBarSpec>,
}

impl MeterPixels {
    pub(crate) fn new(id_base: u32) -> Self {
        Self {
            id_base,
            specs: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, spec: MeterBarSpec) -> usize {
        let slot = self.specs.len();
        self.specs.push(spec);
        slot
    }

    /// Composition builds the whole scrollable roster before slicing its visible
    /// window. Compact provisional slots after slicing so residency stays bounded
    /// by the gauges whose placeholders actually reach the terminal buffer.
    pub(crate) fn retain_visible(&mut self, lines: &mut [Line<'static>]) {
        let mut visible = Vec::new();
        let mut remap = BTreeMap::new();
        for line in lines {
            for span in &mut line.spans {
                if !span.content.contains('\u{10eeee}') {
                    continue;
                }
                let Some(Color::Rgb(red, green, blue)) = span.style.fg else {
                    continue;
                };
                let image_id = u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue);
                let Some(old_slot) = (0..self.specs.len())
                    .find(|slot| meter_image_id(self.id_base, *slot) == image_id)
                else {
                    continue;
                };
                let new_slot = *remap.entry(old_slot).or_insert_with(|| {
                    let slot = visible.len();
                    visible.push(self.specs[old_slot].clone());
                    slot
                });
                span.style.fg = Some(super::image_id_color(meter_image_id(
                    self.id_base,
                    new_slot,
                )));
            }
        }
        self.specs = visible;
    }
}

pub(crate) fn meter_image_id(id_base: u32, slot: usize) -> u32 {
    (id_base
        .wrapping_add(METER_ID_OFFSET)
        .wrapping_add(slot as u32)
        & IMAGE_ID_COLOR_MASK)
        .max(1)
}

pub(crate) fn rasterize(spec: &MeterBarSpec) -> RgbaImage {
    let width = u32::from(spec.width_cells).saturating_mul(CELL_W).max(1);
    let mut image = RgbaImage {
        width,
        height: CELL_H,
        data: vec![0; width as usize * CELL_H as usize * 4],
    };
    paint_rect(&mut image, 0, 7, width, 2, spec.track);

    let fill_px = (spec.fill.clamp(0.0, 1.0) * f64::from(width)).round() as u32;
    if fill_px == 0 {
        return image;
    }

    let active: Vec<(u64, [u8; 3])> = spec
        .segments
        .iter()
        .copied()
        .filter(|(weight, _)| *weight > 0)
        .collect();
    if active.is_empty() {
        paint_rect(&mut image, 0, 6, fill_px, 4, spec.health);
        return image;
    }

    let widths = apportion_pixels(&active, fill_px);
    let mut x = 0;
    let mut painted_runs = 0;
    for ((_, color), run_width) in active.into_iter().zip(widths) {
        if run_width == 0 {
            continue;
        }
        let color = if painted_runs == 0 {
            spec.health
        } else {
            color
        };
        paint_rect(&mut image, x, 6, run_width, 4, color);
        if painted_runs > 0 {
            clear_rect(&mut image, x, 6, run_width.min(2), 4);
        }
        x += run_width;
        painted_runs += 1;
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
    id_base: u32,
    wrap: bool,
    slots: BTreeMap<usize, (MeterBarSpec, u64)>,
    resident: BTreeSet<usize>,
    last_resend_ms: Option<u64>,
}

impl MeterPainter {
    pub(crate) fn new(id_base: u32, wrap: bool) -> Self {
        Self {
            id_base: id_base & IMAGE_ID_COLOR_MASK,
            wrap,
            slots: BTreeMap::new(),
            resident: BTreeSet::new(),
            last_resend_ms: None,
        }
    }

    pub(crate) fn ensure_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        slot: usize,
        spec: &MeterBarSpec,
        now_ms: u64,
    ) -> io::Result<()> {
        let changed = self
            .slots
            .get(&slot)
            .is_none_or(|(previous, _)| previous != spec);
        let stale = self
            .slots
            .get(&slot)
            .is_some_and(|(_, last)| now_ms.saturating_sub(*last) >= RESIDENT_REFRESH_MS);
        let resend = stale
            && self
                .last_resend_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= MIN_RESEND_SPACING_MS);
        if !changed && !resend {
            return Ok(());
        }

        let image_id = meter_image_id(self.id_base, slot);
        let image = rasterize(spec);
        let png = encode_png(image.width, image.height, &image.data);
        for chunk in transmit_png_chunks(image_id, &png) {
            writer.write_all(&wrap_pixel_payload(&chunk, self.wrap))?;
        }
        writer.write_all(&wrap_pixel_payload(
            &virtual_place(image_id, spec.width_cells, 1, 2),
            self.wrap,
        ))?;
        self.slots.insert(slot, (spec.clone(), now_ms));
        self.resident.insert(slot);
        if resend {
            self.last_resend_ms = Some(now_ms);
        }
        Ok(())
    }

    pub(crate) fn clear<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        write_synchronized_pixel_output(writer, |writer| {
            for slot in std::mem::take(&mut self.resident) {
                writer.write_all(&wrap_pixel_payload(
                    &delete(meter_image_id(self.id_base, slot)),
                    self.wrap,
                ))?;
            }
            self.slots.clear();
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

    fn spec(fill: f64) -> MeterBarSpec {
        MeterBarSpec {
            width_cells: 2,
            fill,
            health: [1, 2, 3],
            segments: Vec::new(),
            track: [4, 5, 6],
        }
    }

    fn pixel(image: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        image.pixel(x, y)
    }

    #[test]
    fn rasterize_rounds_fill_to_exact_pixel_columns() {
        for (fill, columns) in [(0.0, 0), (0.25, 4), (0.51, 8), (1.0, 16)] {
            let image = rasterize(&spec(fill));
            let painted = (0..image.width)
                .filter(|x| pixel(&image, *x, 6)[3] == 255)
                .count();
            assert_eq!(painted, columns, "fill={fill}");
        }
    }

    #[test]
    fn rasterize_uses_health_then_segment_colors_and_carves_notch() {
        let image = rasterize(&MeterBarSpec {
            segments: vec![(1, [9, 9, 9]), (1, [7, 8, 9])],
            ..spec(1.0)
        });
        assert_eq!(pixel(&image, 0, 6), [1, 2, 3, 255]);
        assert_eq!(pixel(&image, 8, 6), [0, 0, 0, 0]);
        assert_eq!(pixel(&image, 9, 8), [0, 0, 0, 0]);
        assert_eq!(pixel(&image, 10, 6), [7, 8, 9, 255]);
        assert_eq!(pixel(&image, 15, 7), [7, 8, 9, 255]);
    }

    #[test]
    fn painter_diffs_specs_replaces_data_and_clears() {
        let mut painter = MeterPainter::new(0x120000, false);
        let mut bytes = Vec::new();
        painter
            .ensure_transmitted(&mut bytes, 0, &spec(0.5), 0)
            .expect("first");
        let first_len = bytes.len();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("a=t"));
        assert!(text.contains("a=p"));
        assert!(text.contains(&format!("i={}", meter_image_id(0x120000, 0))));

        painter
            .ensure_transmitted(&mut bytes, 0, &spec(0.5), 1)
            .expect("same");
        assert_eq!(bytes.len(), first_len);
        painter
            .ensure_transmitted(&mut bytes, 0, &spec(0.75), 2)
            .expect("changed");
        assert!(bytes.len() > first_len);

        let mut clear = Vec::new();
        painter.clear(&mut clear).expect("clear");
        assert!(String::from_utf8_lossy(&clear).contains("a=d"));
    }

    #[test]
    fn painter_wraps_each_payload_for_tmux() {
        let expected_payloads = {
            let image = rasterize(&spec(0.5));
            transmit_png_chunks(
                meter_image_id(0x120000, 0),
                &encode_png(image.width, image.height, &image.data),
            )
            .len()
                + 1
        };
        let mut painter = MeterPainter::new(0x120000, true);
        let mut bytes = Vec::new();
        painter
            .ensure_transmitted(&mut bytes, 0, &spec(0.5), 0)
            .expect("paint");
        assert_eq!(
            bytes
                .windows(b"\x1bPtmux;".len())
                .filter(|window| *window == b"\x1bPtmux;")
                .count(),
            expected_payloads
        );
        assert!(!bytes.starts_with(super::super::BEGIN_SYNC));
    }

    #[test]
    fn visible_compaction_rebases_draw_order_slots() {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.push(spec(0.1));
        pixels.push(spec(0.2));
        pixels.push(spec(0.3));
        let old_id = meter_image_id(pixels.id_base, 2);
        let mut lines = vec![Line::from(Span::styled(
            super::super::placeholder_cluster(0, 0),
            Style::default().fg(super::super::image_id_color(old_id)),
        ))];

        pixels.retain_visible(&mut lines);

        assert_eq!(pixels.specs, vec![spec(0.3)]);
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(super::super::image_id_color(meter_image_id(
                pixels.id_base,
                0,
            )))
        );
    }
}
