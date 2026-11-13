//! The provider budget bar's raster: the `▰▱` text bar drawn as one continuous
//! slanted bar, cut at the exact percent.
//!
//! The geometry is Cascadia Code's `▰` outline in font units, a cell 1200 wide
//! and 2380 tall with its baseline 1900 below the top, so the image the
//! terminal stretches over the bar's cells keeps the glyph's height and lean.
//! The spent part is a solid strip inset from the bar's top and bottom, so it
//! reads quieter than the budget left.

use super::super::RgbaImage;

/// Raster pixels per cell. Fine enough that the strip's inset is about a
/// pixel and a half and the lean several; coverage sampling smooths the
/// slanted edges.
const CELL_W: u32 = 24;
const CELL_H: u32 = 48;
const SAMPLES: u32 = 4;

const UNITS_W: f64 = 1200.0;
const UNITS_H: f64 = 2380.0;
const ASCENT: f64 = 1900.0;
/// The bar's band and its rightward lean per unit of height.
const BAR_BOTTOM: f64 = 410.0;
const BAR_TOP: f64 = 1010.0;
const LEAN: f64 = 170.0 / 600.0;
/// How far the spent strip sits in from the bar's top and bottom, leaving it
/// three quarters of the bar's height.
const TRACK_INSET: f64 = 75.0;
/// A tick's vertical reach, its ink half-width, and the half-width of the gap
/// it clears through the bar beneath it.
const TICK_BOTTOM: f64 = 50.0;
const TICK_TOP: f64 = 1370.0;
const TICK_HALF: f64 = 80.0;
const TICK_GAP_HALF: f64 = 233.0;
/// A ceiling's dashes: three dashes of two parts each, split by one-part gaps.
const CEILING_PARTS: f64 = 8.0;

/// The exact pixel shape of one budget bar.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sidebar_pane) struct BudgetRaster {
    pub(in crate::sidebar_pane) width_cells: u16,
    remaining: u8,
    fill: [u8; 3],
    track: [u8; 3],
    ticks: Vec<BudgetTick>,
}

/// A tick at its own remaining percent: a slanted sub-cap, or a dashed upright
/// ceiling drawn above every sub-cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::sidebar_pane) struct BudgetTick {
    pub(in crate::sidebar_pane) ceiling: bool,
    pub(in crate::sidebar_pane) remaining: u8,
    pub(in crate::sidebar_pane) color: [u8; 3],
}

impl BudgetRaster {
    /// A bar with `remaining` percent left in `fill` and the spent strip in
    /// `spent`; `None` is an unknown reading, the whole strip in `spent`. A
    /// spent bar is the whole strip in `fill` with no ticks, and a full bar
    /// has no strip left to show. Colors and ticks the bar cannot show stay
    /// out of the key, so equal pixels share one image.
    pub(in crate::sidebar_pane) fn new(
        width_cells: u16,
        remaining: Option<u8>,
        fill: [u8; 3],
        spent: [u8; 3],
        mut ticks: Vec<BudgetTick>,
    ) -> Self {
        let (remaining, fill, track) = match remaining.map(|pct| pct.min(100)) {
            None => (0, [0; 3], spent),
            Some(0) => (0, [0; 3], fill),
            Some(100) => (100, fill, [0; 3]),
            Some(pct) => (pct, fill, spent),
        };
        if remaining == 0 {
            ticks.clear();
        }
        ticks.sort_unstable();
        ticks.dedup();
        Self {
            width_cells,
            remaining,
            fill,
            track,
            ticks,
        }
    }
}

enum TickHit {
    Ink,
    Gap,
    Clear,
}

pub(super) fn rasterize(raster: &BudgetRaster) -> RgbaImage {
    let cells = raster.width_cells.max(1);
    let width = u32::from(cells) * CELL_W;
    let end = f64::from(cells) * UNITS_W - (BAR_TOP - BAR_BOTTOM) * LEAN;
    let mut data = vec![0; width as usize * CELL_H as usize * 4];
    let per_pixel = SAMPLES * SAMPLES;
    for py in 0..CELL_H {
        for px in 0..width {
            let mut sum = [0u32; 3];
            let mut covered = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (f64::from(px) + (f64::from(sx) + 0.5) / f64::from(SAMPLES)) * UNITS_W
                        / f64::from(CELL_W);
                    let y = ASCENT
                        - (f64::from(py) + (f64::from(sy) + 0.5) / f64::from(SAMPLES)) * UNITS_H
                            / f64::from(CELL_H);
                    if let Some(color) = sample(raster, end, x, y) {
                        covered += 1;
                        for (total, channel) in sum.iter_mut().zip(color) {
                            *total += u32::from(channel);
                        }
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let offset = ((py * width + px) * 4) as usize;
            for (slot, total) in data[offset..offset + 3].iter_mut().zip(sum) {
                *slot = ((total + covered / 2) / covered) as u8;
            }
            data[offset + 3] = ((covered * 255 + per_pixel / 2) / per_pixel) as u8;
        }
    }
    RgbaImage {
        width,
        height: CELL_H,
        data,
    }
}

/// The color at one sample point, ticks on top: each tick clears a gap through
/// the bar beneath it, so it reads apart from a fill of the same tone. A gap
/// never clears another tick's ink.
fn sample(raster: &BudgetRaster, end: f64, x: f64, y: f64) -> Option<[u8; 3]> {
    let mut in_gap = false;
    for tick in raster.ticks.iter().rev() {
        match tick_hit(tick, end, x, y) {
            TickHit::Ink => return Some(tick.color),
            TickHit::Gap => in_gap = true,
            TickHit::Clear => {}
        }
    }
    if in_gap {
        return None;
    }
    if !(BAR_BOTTOM..=BAR_TOP).contains(&y) {
        return None;
    }
    let lean = (y - BAR_BOTTOM) * LEAN;
    if x < lean || x > end + lean {
        return None;
    }
    let cut = end * f64::from(raster.remaining) / 100.0;
    if raster.remaining > 0 && x <= cut + lean {
        return Some(raster.fill);
    }
    (BAR_BOTTOM + TRACK_INSET..=BAR_TOP - TRACK_INSET)
        .contains(&y)
        .then_some(raster.track)
}

fn tick_hit(tick: &BudgetTick, end: f64, x: f64, y: f64) -> TickHit {
    if !(TICK_BOTTOM..=TICK_TOP).contains(&y) {
        return TickHit::Clear;
    }
    let mid = end * f64::from(tick.remaining) / 100.0 + (BAR_TOP - BAR_BOTTOM) * LEAN / 2.0;
    let centre = if tick.ceiling {
        mid
    } else {
        mid + (y - (BAR_BOTTOM + BAR_TOP) / 2.0) * LEAN
    };
    let offset = (x - centre).abs();
    let dash = ((y - TICK_BOTTOM) / (TICK_TOP - TICK_BOTTOM) * CEILING_PARTS) as u32 % 3 != 2;
    if offset <= TICK_HALF && (!tick.ceiling || dash) {
        TickHit::Ink
    } else if offset <= TICK_GAP_HALF {
        TickHit::Gap
    } else {
        TickHit::Clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILL: [u8; 3] = [40, 200, 60];
    const SPENT: [u8; 3] = [70, 70, 80];
    const SUB_CAP: [u8; 3] = [210, 180, 20];
    const CEILING: [u8; 3] = [20, 120, 230];
    /// The pixel row through the bar's middle.
    const MID_ROW: u32 = 24;

    fn bar(remaining: Option<u8>, ticks: Vec<BudgetTick>) -> BudgetRaster {
        BudgetRaster::new(20, remaining, FILL, SPENT, ticks)
    }

    fn tick(ceiling: bool, remaining: u8, color: [u8; 3]) -> BudgetTick {
        BudgetTick {
            ceiling,
            remaining,
            color,
        }
    }

    fn solid(image: &RgbaImage, row: u32, color: [u8; 3]) -> Vec<u32> {
        (0..image.width)
            .filter(|x| image.pixel(*x, row) == [color[0], color[1], color[2], 255])
            .collect()
    }

    fn opaque_colors(image: &RgbaImage) -> std::collections::BTreeSet<[u8; 3]> {
        (0..image.height)
            .flat_map(|y| (0..image.width).map(move |x| (x, y)))
            .map(|(x, y)| image.pixel(x, y))
            .filter(|pixel| pixel[3] == 255)
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect()
    }

    /// The pixel column a tick's centre crosses `row` at, from the same font
    /// geometry the raster draws.
    fn tick_column(width_cells: u16, remaining: u8, ceiling: bool, row: u32) -> f64 {
        let end = f64::from(width_cells) * UNITS_W - 170.0;
        let y = ASCENT - (f64::from(row) + 0.5) * UNITS_H / f64::from(CELL_H);
        let mid = end * f64::from(remaining) / 100.0 + 85.0;
        let centre = if ceiling {
            mid
        } else {
            mid + (y - 710.0) * LEAN
        };
        centre * f64::from(CELL_W) / UNITS_W - 0.5
    }

    #[test]
    fn small_usage_moves_the_cut_by_whole_percents() {
        let edges = [93, 95, 97].map(|pct| {
            *solid(&rasterize(&bar(Some(pct), Vec::new())), MID_ROW, FILL)
                .last()
                .expect("the fill shows")
        });
        assert!(edges[0] < edges[1] && edges[1] < edges[2], "{edges:?}");
        assert!(
            edges[1] - edges[0] >= 4,
            "two percent spans pixels: {edges:?}"
        );

        let mut pixels = super::super::MeterPixels::new(0x120000);
        pixels.begin_frame();
        let ids = [93, 95, 97, 95].map(|pct| pixels.intern(bar(Some(pct), Vec::new())).unwrap());
        assert_eq!(ids[1], ids[3], "equal bars share one image");
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    }

    #[test]
    fn spent_full_and_unknown_bars_keep_their_shapes() {
        let spent = rasterize(&bar(Some(0), Vec::new()));
        assert_eq!(
            opaque_colors(&spent),
            [FILL].into(),
            "a spent strip wears the fill"
        );
        assert_eq!(
            spent.pixel(spent.width / 2, MID_ROW),
            [FILL[0], FILL[1], FILL[2], 255],
            "and is solid"
        );

        let full = rasterize(&bar(Some(100), Vec::new()));
        let row = solid(&full, MID_ROW, FILL);
        let (first, last) = (row[0], *row.last().unwrap());
        assert_eq!(row.len() as u32, last - first + 1, "a full bar is solid");
        assert_eq!(opaque_colors(&full), [FILL].into());

        let unknown = rasterize(&bar(None, Vec::new()));
        assert_eq!(
            opaque_colors(&unknown),
            [SPENT].into(),
            "unknown is the strip only"
        );
        assert_eq!(
            unknown.pixel(unknown.width / 2, MID_ROW),
            [SPENT[0], SPENT[1], SPENT[2], 255]
        );

        let ticks = vec![tick(false, 40, SUB_CAP), tick(true, 60, CEILING)];
        assert_eq!(bar(Some(0), ticks.clone()), bar(Some(0), Vec::new()));
        assert_eq!(bar(None, ticks), bar(None, Vec::new()));
    }

    #[test]
    fn the_spent_strip_is_solid_and_inset_from_the_fills_height() {
        let image = rasterize(&bar(Some(50), Vec::new()));
        assert_eq!(
            image.pixel(image.width * 3 / 4, MID_ROW),
            [SPENT[0], SPENT[1], SPENT[2], 255],
            "the strip is solid past the cut"
        );
        let row_bounds = |row: u32| {
            let top = ASCENT - f64::from(row) * UNITS_H / f64::from(CELL_H);
            (top - UNITS_H / f64::from(CELL_H), top)
        };
        let inset_rows = (0..CELL_H)
            .filter(|row| {
                let (bottom, top) = row_bounds(*row);
                (bottom >= BAR_BOTTOM && top <= BAR_BOTTOM + TRACK_INSET)
                    || (bottom >= BAR_TOP - TRACK_INSET && top <= BAR_TOP)
            })
            .collect::<Vec<_>>();
        assert_eq!(inset_rows.len(), 2, "one row inside each band");
        for row in inset_rows {
            assert!(!solid(&image, row, FILL).is_empty(), "row {row}: the fill");
            assert!(solid(&image, row, SPENT).is_empty(), "row {row}: no strip");
            assert_eq!(image.pixel(image.width * 3 / 4, row)[3], 0, "row {row}");
        }
    }

    #[test]
    fn a_spent_bar_and_a_nearly_spent_bar_take_their_own_images() {
        let mut pixels = super::super::MeterPixels::new(0x120000);
        pixels.begin_frame();
        let spent = pixels.intern(bar(Some(0), Vec::new())).unwrap();
        let nearly = pixels.intern(bar(Some(3), Vec::new())).unwrap();
        assert_ne!(spent, nearly);
    }

    #[test]
    fn ticks_sit_at_their_exact_percent() {
        let image = rasterize(&BudgetRaster::new(
            10,
            Some(80),
            FILL,
            SPENT,
            vec![tick(true, 60, CEILING), tick(false, 40, SUB_CAP)],
        ));
        let mut ceiling_rows = 0;
        for row in 0..CELL_H {
            let sub_cap = solid(&image, row, SUB_CAP);
            let y = ASCENT - (f64::from(row) + 0.5) * UNITS_H / f64::from(CELL_H);
            if (TICK_BOTTOM + 60.0..=TICK_TOP - 60.0).contains(&y) {
                assert!(!sub_cap.is_empty(), "row {row}");
            }
            for column in sub_cap {
                let expected = tick_column(10, 40, false, row);
                assert!(
                    (f64::from(column) - expected).abs() <= 2.0,
                    "row {row}: {column} vs {expected}"
                );
            }
            let ceiling = solid(&image, row, CEILING);
            ceiling_rows += usize::from(!ceiling.is_empty());
            for column in ceiling {
                let expected = tick_column(10, 60, true, row);
                assert!(
                    (f64::from(column) - expected).abs() <= 2.0,
                    "row {row}: {column} vs {expected}"
                );
            }
        }
        assert!(
            ceiling_rows > 10,
            "the ceiling dashes span the bar: {ceiling_rows}"
        );
        assert!(
            ceiling_rows < (TICK_TOP - TICK_BOTTOM) as usize * CELL_H as usize / UNITS_H as usize,
            "with breaks: {ceiling_rows}"
        );
    }

    #[test]
    fn a_ceiling_leaves_a_sub_cap_at_its_percent_showing() {
        let image = rasterize(&bar(
            Some(80),
            vec![tick(true, 40, CEILING), tick(false, 40, SUB_CAP)],
        ));
        let rows = |color| {
            (0..CELL_H)
                .filter(|row| !solid(&image, *row, color).is_empty())
                .count()
        };
        assert!(rows(SUB_CAP) >= 8, "sub-cap rows: {}", rows(SUB_CAP));
        assert!(rows(CEILING) > 10, "ceiling rows: {}", rows(CEILING));
    }
}
