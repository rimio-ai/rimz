//! Downsamples a decoded pet frame into sextant terminal cells,
//! choosing each cell's best two-color split in linear light.

use ratatui::style::Color;

use super::frames::RgbaImage;

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

pub(crate) fn render_frame(frame: &RgbaImage, cols: u16, rows: u16) -> PetCellGrid {
    let sample_w = u32::from(cols) * SEXTANT_COLS;
    let sample_h = u32::from(rows) * SEXTANT_ROWS;
    let samples = downsample(frame, sample_w, sample_h);
    let mut grid = Vec::with_capacity(usize::from(rows));
    for cell_y in 0..u32::from(rows) {
        let mut row = Vec::with_capacity(usize::from(cols));
        for cell_x in 0..u32::from(cols) {
            let mut sub = [((0.0, 0.0, 0.0), 0.0); 6];
            let count = (SEXTANT_COLS * SEXTANT_ROWS) as usize;
            for dy in 0..SEXTANT_ROWS {
                for dx in 0..SEXTANT_COLS {
                    sub[(dy * SEXTANT_COLS + dx) as usize] = sample_at(
                        &samples,
                        sample_w,
                        cell_x * SEXTANT_COLS + dx,
                        cell_y * SEXTANT_ROWS + dy,
                    );
                }
            }
            row.push(glyph_cell(&sub[..count]));
        }
        grid.push(row);
    }
    grid
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
        let transparent = RgbaImage {
            width: 1,
            height: 1,
            data: vec![255, 0, 0, 0],
        };
        assert_eq!(
            render_frame(&transparent, 1, 1)[0][0],
            PetCell {
                ch: ' ',
                fg: Color::Reset,
                bg: Color::Reset,
            }
        );

        let opaque_two_color = RgbaImage {
            width: 2,
            height: 3,
            data: [
                [255, 0, 0, 255],
                [0, 0, 255, 255],
                [255, 0, 0, 255],
                [0, 0, 255, 255],
                [255, 0, 0, 255],
                [0, 0, 255, 255],
            ]
            .concat(),
        };
        assert_eq!(
            render_frame(&opaque_two_color, 1, 1)[0][0],
            PetCell {
                ch: '▌',
                fg: Color::Rgb(255, 0, 0),
                bg: Color::Rgb(0, 0, 255),
            }
        );

        let half_covered = RgbaImage {
            width: 2,
            height: 3,
            data: [
                [255, 0, 0, 255],
                [0, 0, 0, 0],
                [255, 0, 0, 255],
                [0, 0, 0, 0],
                [255, 0, 0, 255],
                [0, 0, 0, 0],
            ]
            .concat(),
        };
        assert_eq!(
            render_frame(&half_covered, 1, 1)[0][0],
            PetCell {
                ch: '▌',
                fg: Color::Rgb(255, 0, 0),
                bg: Color::Reset,
            }
        );
    }
}
