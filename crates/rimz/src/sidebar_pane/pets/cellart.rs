use crate::config::PetsGlyphMode;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphTier {
    Half,
    Sextant,
    Octant,
}

impl GlyphTier {
    fn from_config(mode: PetsGlyphMode) -> Self {
        match mode {
            PetsGlyphMode::Auto | PetsGlyphMode::Sextant => Self::Sextant,
            PetsGlyphMode::Half => Self::Half,
            PetsGlyphMode::Octant => Self::Octant,
        }
    }

    fn subcells(self) -> (u32, u32) {
        match self {
            Self::Half => (1, 2),
            Self::Sextant => (2, 3),
            Self::Octant => (2, 4),
        }
    }

    fn glyph(self, mask: u8) -> char {
        match self {
            Self::Half => '▀',
            Self::Sextant => sextant_char(mask),
            Self::Octant => octant_char(mask),
        }
    }
}

type Rgb = (u8, u8, u8);
type LinearRgb = (f32, f32, f32);
/// A downsampled sub-cell: average sprite color in linear light plus coverage
/// (alpha) in `0.0..=1.0`. Coverage at or above [`INK_THRESHOLD`] is "ink"; the
/// rest is transparent and renders as the terminal background.
type Sample = (LinearRgb, f32);

const INK_THRESHOLD: f32 = 0.5;

pub(crate) fn render_frame(
    frame: &RgbaImage,
    cols: u16,
    rows: u16,
    glyphs: PetsGlyphMode,
) -> PetCellGrid {
    let tier = GlyphTier::from_config(glyphs);
    let (sub_w, sub_h) = tier.subcells();
    let sample_w = u32::from(cols) * sub_w;
    let sample_h = u32::from(rows) * sub_h;
    let samples = downsample(frame, sample_w, sample_h);
    let mut grid = Vec::with_capacity(usize::from(rows));
    for cell_y in 0..u32::from(rows) {
        let mut row = Vec::with_capacity(usize::from(cols));
        for cell_x in 0..u32::from(cols) {
            if tier == GlyphTier::Half {
                let top = sample_at(&samples, sample_w, cell_x, cell_y * 2);
                let bottom = sample_at(&samples, sample_w, cell_x, cell_y * 2 + 1);
                row.push(half_cell(top, bottom));
                continue;
            }

            let mut sub = [((0.0, 0.0, 0.0), 0.0); 8];
            let count = (sub_w * sub_h) as usize;
            for dy in 0..sub_h {
                for dx in 0..sub_w {
                    sub[(dy * sub_w + dx) as usize] =
                        sample_at(&samples, sample_w, cell_x * sub_w + dx, cell_y * sub_h + dy);
                }
            }
            row.push(glyph_cell(tier, &sub[..count]));
        }
        grid.push(row);
    }
    grid
}

/// A two-tone half-block cell. Each half is ink or transparent: an inked half
/// paints its color, a transparent half drops to the terminal background.
fn half_cell(top: Sample, bottom: Sample) -> PetCell {
    let ink = |sample: Sample| sample.1 >= INK_THRESHOLD;
    let color = |sample: Sample| rgb_color(linear_to_rgb(sample.0));
    match (ink(top), ink(bottom)) {
        (false, false) => transparent_cell(),
        (true, false) => PetCell {
            ch: '▀',
            fg: color(top),
            bg: Color::Reset,
        },
        (false, true) => PetCell {
            ch: '▄',
            fg: color(bottom),
            bg: Color::Reset,
        },
        (true, true) => PetCell {
            ch: '▀',
            fg: color(top),
            bg: color(bottom),
        },
    }
}

/// A sextant/octant cell. A fully-opaque cell keeps the two-color partition for
/// interior detail; a cell straddling the sprite edge paints its ink subcells in
/// one color over a transparent background; a fully-transparent cell is blank.
fn glyph_cell(tier: GlyphTier, sub: &[Sample]) -> PetCell {
    let octant = tier == GlyphTier::Octant;
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
    // A solid interior, or an octant edge whose ink pattern has no glyph, keeps
    // the opaque two-color partition rather than dropping detail.
    if ink_mask == full_mask || (octant && !octant_representable(ink_mask)) {
        let colors = sub.iter().map(|sample| sample.0).collect::<Vec<_>>();
        let (mask, fg, bg) = best_partition(&colors, octant);
        return PetCell {
            ch: tier.glyph(mask),
            fg: rgb_color(linear_to_rgb(fg)),
            bg: rgb_color(linear_to_rgb(bg)),
        };
    }
    PetCell {
        ch: tier.glyph(ink_mask),
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

fn best_partition(sub: &[LinearRgb], octant: bool) -> (u8, LinearRgb, LinearRgb) {
    let mut best_error = f32::MAX;
    let mut best = (0, (0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
    for mask in 0_u32..(1_u32 << sub.len()) {
        if octant && !octant_representable(mask as u8) {
            continue;
        }
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

fn octant_char(pattern: u8) -> char {
    char::from_u32(OCTANT_CP[pattern as usize]).unwrap_or('?')
}

fn octant_representable(pattern: u8) -> bool {
    OCTANT_CP[pattern as usize] != 0
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

const OCTANT_CP: [u32; 256] = [
    0x00020, 0x00000, 0x00000, 0x00000, 0x1CD00, 0x02598, 0x1CD01, 0x1CD02, 0x1CD03, 0x1CD04,
    0x0259D, 0x1CD05, 0x1CD06, 0x1CD07, 0x1CD08, 0x02580, 0x1CD09, 0x1CD0A, 0x1CD0B, 0x1CD0C,
    0x00000, 0x1CD0D, 0x1CD0E, 0x1CD0F, 0x1CD10, 0x1CD11, 0x1CD12, 0x1CD13, 0x1CD14, 0x1CD15,
    0x1CD16, 0x1CD17, 0x1CD18, 0x1CD19, 0x1CD1A, 0x1CD1B, 0x1CD1C, 0x1CD1D, 0x1CD1E, 0x1CD1F,
    0x00000, 0x1CD20, 0x1CD21, 0x1CD22, 0x1CD23, 0x1CD24, 0x1CD25, 0x1CD26, 0x1CD27, 0x1CD28,
    0x1CD29, 0x1CD2A, 0x1CD2B, 0x1CD2C, 0x1CD2D, 0x1CD2E, 0x1CD2F, 0x1CD30, 0x1CD31, 0x1CD32,
    0x1CD33, 0x1CD34, 0x1CD35, 0x00000, 0x00000, 0x1CD36, 0x1CD37, 0x1CD38, 0x1CD39, 0x1CD3A,
    0x1CD3B, 0x1CD3C, 0x1CD3D, 0x1CD3E, 0x1CD3F, 0x1CD40, 0x1CD41, 0x1CD42, 0x1CD43, 0x1CD44,
    0x02596, 0x1CD45, 0x1CD46, 0x1CD47, 0x1CD48, 0x0258C, 0x1CD49, 0x1CD4A, 0x1CD4B, 0x1CD4C,
    0x0259E, 0x1CD4D, 0x1CD4E, 0x1CD4F, 0x1CD50, 0x0259B, 0x1CD51, 0x1CD52, 0x1CD53, 0x1CD54,
    0x1CD55, 0x1CD56, 0x1CD57, 0x1CD58, 0x1CD59, 0x1CD5A, 0x1CD5B, 0x1CD5C, 0x1CD5D, 0x1CD5E,
    0x1CD5F, 0x1CD60, 0x1CD61, 0x1CD62, 0x1CD63, 0x1CD64, 0x1CD65, 0x1CD66, 0x1CD67, 0x1CD68,
    0x1CD69, 0x1CD6A, 0x1CD6B, 0x1CD6C, 0x1CD6D, 0x1CD6E, 0x1CD6F, 0x1CD70, 0x00000, 0x1CD71,
    0x1CD72, 0x1CD73, 0x1CD74, 0x1CD75, 0x1CD76, 0x1CD77, 0x1CD78, 0x1CD79, 0x1CD7A, 0x1CD7B,
    0x1CD7C, 0x1CD7D, 0x1CD7E, 0x1CD7F, 0x1CD80, 0x1CD81, 0x1CD82, 0x1CD83, 0x1CD84, 0x1CD85,
    0x1CD86, 0x1CD87, 0x1CD88, 0x1CD89, 0x1CD8A, 0x1CD8B, 0x1CD8C, 0x1CD8D, 0x1CD8E, 0x1CD8F,
    0x02597, 0x1CD90, 0x1CD91, 0x1CD92, 0x1CD93, 0x0259A, 0x1CD94, 0x1CD95, 0x1CD96, 0x1CD97,
    0x02590, 0x1CD98, 0x1CD99, 0x1CD9A, 0x1CD9B, 0x0259C, 0x1CD9C, 0x1CD9D, 0x1CD9E, 0x1CD9F,
    0x1CDA0, 0x1CDA1, 0x1CDA2, 0x1CDA3, 0x1CDA4, 0x1CDA5, 0x1CDA6, 0x1CDA7, 0x1CDA8, 0x1CDA9,
    0x1CDAA, 0x1CDAB, 0x00000, 0x1CDAC, 0x1CDAD, 0x1CDAE, 0x1CDAF, 0x1CDB0, 0x1CDB1, 0x1CDB2,
    0x1CDB3, 0x1CDB4, 0x1CDB5, 0x1CDB6, 0x1CDB7, 0x1CDB8, 0x1CDB9, 0x1CDBA, 0x1CDBB, 0x1CDBC,
    0x1CDBD, 0x1CDBE, 0x1CDBF, 0x1CDC0, 0x1CDC1, 0x1CDC2, 0x1CDC3, 0x1CDC4, 0x1CDC5, 0x1CDC6,
    0x1CDC7, 0x1CDC8, 0x1CDC9, 0x1CDCA, 0x1CDCB, 0x1CDCC, 0x1CDCD, 0x1CDCE, 0x1CDCF, 0x1CDD0,
    0x1CDD1, 0x1CDD2, 0x1CDD3, 0x1CDD4, 0x1CDD5, 0x1CDD6, 0x1CDD7, 0x1CDD8, 0x1CDD9, 0x1CDDA,
    0x02584, 0x1CDDB, 0x1CDDC, 0x1CDDD, 0x1CDDE, 0x02599, 0x1CDDF, 0x1CDE0, 0x1CDE1, 0x1CDE2,
    0x0259F, 0x1CDE3, 0x00000, 0x1CDE4, 0x1CDE5, 0x02588,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sextant_special_cases_reuse_legacy_blocks() {
        assert_eq!(sextant_char(0), ' ');
        assert_eq!(sextant_char(0b111111), '█');
        assert_eq!(sextant_char(0b010101), '▌');
        assert_eq!(sextant_char(0b101010), '▐');
    }

    #[test]
    fn octant_matcher_skips_unrepresentable_patterns() {
        assert!(!octant_representable(1));
        let sub = [
            rgb_to_linear((255, 0, 0)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
            rgb_to_linear((0, 0, 255)),
        ];
        let (mask, _, _) = best_partition(&sub, true);
        assert_ne!(mask, 1);
        assert!(octant_representable(mask));
    }

    #[test]
    fn render_half_block_uses_top_as_fg_and_bottom_as_bg() {
        let image = RgbaImage {
            width: 1,
            height: 2,
            data: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        let grid = render_frame(&image, 1, 1, PetsGlyphMode::Half);
        assert_eq!(grid[0][0].ch, '▀');
        assert_eq!(grid[0][0].fg, Color::Rgb(255, 0, 0));
        assert_eq!(grid[0][0].bg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn transparent_pixels_drop_to_terminal_background() {
        let image = RgbaImage {
            width: 1,
            height: 2,
            data: vec![255, 0, 0, 0, 255, 0, 0, 0],
        };
        let grid = render_frame(&image, 1, 1, PetsGlyphMode::Half);
        assert_eq!(grid[0][0].ch, ' ');
        assert_eq!(grid[0][0].fg, Color::Reset);
        assert_eq!(grid[0][0].bg, Color::Reset);
    }

    #[test]
    fn half_edge_keeps_terminal_background_under_the_sprite() {
        let image = RgbaImage {
            width: 1,
            height: 2,
            // Opaque red top, fully transparent bottom.
            data: vec![255, 0, 0, 255, 0, 0, 0, 0],
        };
        let grid = render_frame(&image, 1, 1, PetsGlyphMode::Half);
        assert_eq!(grid[0][0].ch, '▀');
        assert_eq!(grid[0][0].fg, Color::Rgb(255, 0, 0));
        assert_eq!(grid[0][0].bg, Color::Reset);
    }
}
