//! Downsamples a decoded pet frame into sextant terminal cells,
//! choosing each cell's best two-color split in linear light.

use ratatui::style::Color;

use crate::config::CellAspect;

use crate::sidebar_pane::pixel::RgbaImage;

pub(crate) type PetCellRow = Vec<PetCell>;
pub(crate) type PetCellGrid = Vec<PetCellRow>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PetCell {
    pub(crate) ch: char,
    pub(crate) fg: Color,
    pub(crate) bg: Color,
}

type Rgb = (u8, u8, u8);
type LinearRgb = (f32, f32, f32);
/// A downsampled sub-cell: average sprite color in linear light plus coverage
/// (alpha) in `0.0..=1.0`. Coverage at or above [`INK_THRESHOLD`] is "ink"; the
/// rest is transparent and renders as the terminal background.
type Sample = (LinearRgb, f32);

const INK_THRESHOLD: f32 = 0.5;
const SEXTANT_COLS: u32 = 2;
const SEXTANT_ROWS: u32 = 3;
const TRANSPARENT_SAMPLE: Sample = ((0.0, 0.0, 0.0), 0.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SampleRect {
    width: u32,
    height: u32,
    left: u32,
    top: u32,
}

/// Read terminal cell height/width from the pty's pixel and cell dimensions.
pub fn probe_cell_aspect() -> Option<CellAspect> {
    let size = ratatui::crossterm::terminal::window_size().ok()?;
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return None;
    }
    let ratio = (f32::from(size.height) * f32::from(size.columns))
        / (f32::from(size.rows) * f32::from(size.width));
    CellAspect::from_ratio(ratio)
}

pub(crate) fn render_frame(
    frame: &RgbaImage,
    cols: u16,
    rows: u16,
    aspect: CellAspect,
) -> PetCellGrid {
    let sample_w = u32::from(cols) * SEXTANT_COLS;
    let sample_h = u32::from(rows) * SEXTANT_ROWS;
    let rect = fitted_sample_rect(frame, sample_w, sample_h, aspect);
    let samples = downsample(frame, rect.width, rect.height);
    let mut grid = Vec::with_capacity(usize::from(rows));
    for cell_y in 0..u32::from(rows) {
        let mut row = Vec::with_capacity(usize::from(cols));
        for cell_x in 0..u32::from(cols) {
            let mut sub = [TRANSPARENT_SAMPLE; 6];
            let count = (SEXTANT_COLS * SEXTANT_ROWS) as usize;
            for dy in 0..SEXTANT_ROWS {
                for dx in 0..SEXTANT_COLS {
                    let x = cell_x * SEXTANT_COLS + dx;
                    let y = cell_y * SEXTANT_ROWS + dy;
                    if x >= rect.left
                        && x < rect.left + rect.width
                        && y >= rect.top
                        && y < rect.top + rect.height
                    {
                        sub[(dy * SEXTANT_COLS + dx) as usize] =
                            sample_at(&samples, rect.width, x - rect.left, y - rect.top);
                    }
                }
            }
            row.push(glyph_cell(&sub[..count]));
        }
        grid.push(row);
    }
    grid
}

fn fitted_sample_rect(
    frame: &RgbaImage,
    max_width: u32,
    max_height: u32,
    aspect: CellAspect,
) -> SampleRect {
    if frame.width == 0 || frame.height == 0 || max_width == 0 || max_height == 0 {
        return SampleRect {
            width: 0,
            height: 0,
            left: 0,
            top: max_height,
        };
    }
    let source_aspect = frame.height as f32 / frame.width as f32;
    let fitted_height =
        (max_width as f32 * 3.0 * source_aspect / (2.0 * aspect.ratio())).round() as u32;
    let (width, height) = if fitted_height <= max_height {
        (max_width, fitted_height.max(1))
    } else {
        let fitted_width =
            (max_height as f32 * 2.0 * aspect.ratio() / (3.0 * source_aspect)).round() as u32;
        (fitted_width.clamp(1, max_width), max_height)
    };
    SampleRect {
        width,
        height,
        left: (max_width - width) / 2,
        top: max_height - height,
    }
}

/// A sextant cell. A fully-opaque cell keeps the two-color partition for
/// interior detail; a cell straddling the sprite edge paints its ink subcells
/// in one color over a transparent background; a fully-transparent cell is
/// blank.
fn glyph_cell(sub: &[Sample]) -> PetCell {
    let ink_mask = sub.iter().enumerate().fold(0u8, |mask, (index, sample)| {
        if sample.1 >= INK_THRESHOLD {
            mask | (1 << index)
        } else {
            mask
        }
    });
    if ink_mask == 0 {
        return transparent_cell();
    }
    let full_mask = ((1u16 << sub.len()) - 1) as u8;
    // A solid interior keeps the opaque two-color partition rather than
    // dropping detail.
    if ink_mask == full_mask {
        let colors = sub.iter().map(|sample| sample.0).collect::<Vec<_>>();
        let (mask, fg, bg) = best_partition(&colors);
        return PetCell {
            ch: sextant_char(mask),
            fg: rgb_color(linear_to_rgb(fg)),
            bg: rgb_color(linear_to_rgb(bg)),
        };
    }
    PetCell {
        ch: sextant_char(ink_mask),
        fg: rgb_color(linear_to_rgb(mean_ink(sub))),
        bg: Color::Reset,
    }
}

fn transparent_cell() -> PetCell {
    PetCell {
        ch: ' ',
        fg: Color::Reset,
        bg: Color::Reset,
    }
}

/// Mean linear color of the ink subcells, ignoring transparent ones so an edge's
/// color is not muddied toward black by the empty side.
fn mean_ink(sub: &[Sample]) -> LinearRgb {
    let mut sum = (0.0, 0.0, 0.0);
    let mut count = 0.0;
    for (color, alpha) in sub {
        if *alpha >= INK_THRESHOLD {
            sum.0 += color.0;
            sum.1 += color.1;
            sum.2 += color.2;
            count += 1.0;
        }
    }
    if count > 0.0 {
        (sum.0 / count, sum.1 / count, sum.2 / count)
    } else {
        (0.0, 0.0, 0.0)
    }
}

fn downsample(frame: &RgbaImage, width: u32, height: u32) -> Vec<Sample> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            out.push(sample_region(frame, x, y, width, height));
        }
    }
    out
}

fn sample_region(frame: &RgbaImage, x: u32, y: u32, out_w: u32, out_h: u32) -> Sample {
    let left = x as f32 * frame.width as f32 / out_w as f32;
    let right = (x + 1) as f32 * frame.width as f32 / out_w as f32;
    let top = y as f32 * frame.height as f32 / out_h as f32;
    let bottom = (y + 1) as f32 * frame.height as f32 / out_h as f32;
    let mut color = (0.0, 0.0, 0.0);
    let mut coverage = 0.0;
    let mut weight_total = 0.0;
    for py in top.floor() as u32..bottom.ceil() as u32 {
        if py >= frame.height {
            continue;
        }
        let y_overlap = overlap(top, bottom, py as f32, py as f32 + 1.0);
        if y_overlap <= 0.0 {
            continue;
        }
        for px in left.floor() as u32..right.ceil() as u32 {
            if px >= frame.width {
                continue;
            }
            let x_overlap = overlap(left, right, px as f32, px as f32 + 1.0);
            let weight = x_overlap * y_overlap;
            if weight <= 0.0 {
                continue;
            }
            let [red, green, blue, alpha] = frame.pixel(px, py);
            let alpha = f32::from(alpha) / 255.0;
            let linear = rgb_to_linear((red, green, blue));
            // Weight color by coverage so a transparent pixel lends no color.
            color.0 += linear.0 * alpha * weight;
            color.1 += linear.1 * alpha * weight;
            color.2 += linear.2 * alpha * weight;
            coverage += alpha * weight;
            weight_total += weight;
        }
    }
    if coverage <= 0.0 || weight_total <= 0.0 {
        return ((0.0, 0.0, 0.0), 0.0);
    }
    (
        (color.0 / coverage, color.1 / coverage, color.2 / coverage),
        coverage / weight_total,
    )
}

fn overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

fn sample_at(samples: &[Sample], width: u32, x: u32, y: u32) -> Sample {
    samples[(y * width + x) as usize]
}

fn best_partition(sub: &[LinearRgb]) -> (u8, LinearRgb, LinearRgb) {
    let mut best_error = f32::MAX;
    let mut best = (0, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
    for mask in 0_u32..(1_u32 << sub.len()) {
        let (fg, bg) = partition_means(sub, mask);
        let mut error = 0.0;
        for (index, sample) in sub.iter().enumerate() {
            let target = if (mask >> index) & 1 == 1 { fg } else { bg };
            error += square(sample.0 - target.0);
            error += square(sample.1 - target.1);
            error += square(sample.2 - target.2);
        }
        if error < best_error {
            best_error = error;
            best = (mask as u8, fg, bg);
        }
    }
    best
}

fn partition_means(sub: &[LinearRgb], mask: u32) -> (LinearRgb, LinearRgb) {
    let mut fg = (0.0, 0.0, 0.0);
    let mut bg = (0.0, 0.0, 0.0);
    let mut fg_count = 0.0;
    let mut bg_count = 0.0;
    for (index, sample) in sub.iter().enumerate() {
        if (mask >> index) & 1 == 1 {
            fg.0 += sample.0;
            fg.1 += sample.1;
            fg.2 += sample.2;
            fg_count += 1.0;
        } else {
            bg.0 += sample.0;
            bg.1 += sample.1;
            bg.2 += sample.2;
            bg_count += 1.0;
        }
    }
    let mean = |sum: LinearRgb, count: f32| {
        if count > 0.0 {
            (sum.0 / count, sum.1 / count, sum.2 / count)
        } else {
            (0.0, 0.0, 0.0)
        }
    };
    (mean(fg, fg_count), mean(bg, bg_count))
}

fn square(value: f32) -> f32 {
    value * value
}

fn sextant_char(pattern: u8) -> char {
    match pattern {
        0 => ' ',
        0b111111 => '\u{2588}',
        0b010101 => '\u{258C}',
        0b101010 => '\u{2590}',
        _ => {
            let mut index = u32::from(pattern);
            if pattern > 0b010101 {
                index -= 1;
            }
            if pattern > 0b101010 {
                index -= 1;
            }
            char::from_u32(0x1FB00 + index - 1).unwrap_or('?')
        }
    }
}

fn rgb_color((red, green, blue): Rgb) -> Color {
    Color::Rgb(red, green, blue)
}

fn rgb_to_linear((red, green, blue): Rgb) -> LinearRgb {
    (
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
    )
}

fn linear_to_rgb((red, green, blue): LinearRgb) -> Rgb {
    (
        linear_to_srgb(red),
        linear_to_srgb(green),
        linear_to_srgb(blue),
    )
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let value = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_frame(left: [u8; 4], right: [u8; 4]) -> RgbaImage {
        let mut data = Vec::with_capacity(6 * 13 * 4);
        for _ in 0..13 {
            for _ in 0..3 {
                data.extend_from_slice(&left);
            }
            for _ in 0..3 {
                data.extend_from_slice(&right);
            }
        }
        RgbaImage {
            width: 6,
            height: 13,
            data,
        }
    }

    #[test]
    fn sextant_chars_map_patterns_with_legacy_block_skips() {
        for (pattern, ch) in [
            (0, ' '),
            (1, '\u{1FB00}'),
            (20, '\u{1FB13}'),
            (0b010101, '▌'),
            (22, '\u{1FB14}'),
            (41, '\u{1FB27}'),
            (0b101010, '▐'),
            (43, '\u{1FB28}'),
            (62, '\u{1FB3B}'),
            (0b111111, '█'),
        ] {
            assert_eq!(sextant_char(pattern), ch);
        }
    }

    #[test]
    fn render_frame_maps_coverage_to_ink_and_transparency() {
        let transparent = neutral_frame([255, 0, 0, 0], [255, 0, 0, 0]);
        assert_eq!(
            render_frame(&transparent, 1, 1, CellAspect::NEUTRAL)[0][0],
            PetCell {
                ch: ' ',
                fg: Color::Reset,
                bg: Color::Reset,
            }
        );

        let opaque_two_color = neutral_frame([255, 0, 0, 255], [0, 0, 255, 255]);
        assert_eq!(
            render_frame(&opaque_two_color, 1, 1, CellAspect::NEUTRAL)[0][0],
            PetCell {
                ch: '▌',
                fg: Color::Rgb(255, 0, 0),
                bg: Color::Rgb(0, 0, 255),
            }
        );

        let half_covered = neutral_frame([255, 0, 0, 255], [0, 0, 0, 0]);
        assert_eq!(
            render_frame(&half_covered, 1, 1, CellAspect::NEUTRAL)[0][0],
            PetCell {
                ch: '▌',
                fg: Color::Rgb(255, 0, 0),
                bg: Color::Reset,
            }
        );
    }

    #[test]
    fn fitted_rect_letterboxes_at_subcell_granularity() {
        let frame = RgbaImage {
            width: 192,
            height: 208,
            data: vec![255; 192 * 208 * 4],
        };
        assert_eq!(
            fitted_sample_rect(&frame, 36, 27, CellAspect::NEUTRAL),
            SampleRect {
                width: 36,
                height: 27,
                left: 0,
                top: 0
            }
        );
        let tall = fitted_sample_rect(
            &frame,
            36,
            27,
            CellAspect::from_ratio(2.5).expect("valid aspect"),
        );
        assert_eq!(
            tall,
            SampleRect {
                width: 36,
                height: 23,
                left: 0,
                top: 4
            }
        );

        let short = fitted_sample_rect(
            &frame,
            36,
            27,
            CellAspect::from_ratio(2.0).expect("valid aspect"),
        );
        assert_eq!(
            short,
            SampleRect {
                width: 33,
                height: 27,
                left: 1,
                top: 0
            }
        );

        let tall_grid = render_frame(
            &frame,
            18,
            9,
            CellAspect::from_ratio(2.5).expect("valid aspect"),
        );
        assert!(tall_grid[0].iter().all(|cell| cell == &transparent_cell()));

        let short_grid = render_frame(
            &frame,
            18,
            9,
            CellAspect::from_ratio(2.0).expect("valid aspect"),
        );
        assert!(short_grid.iter().all(|row| row[17] == transparent_cell()));
    }
}
