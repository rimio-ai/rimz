//! Palette popup sizing: one rule from the room's pane area and the
//! `[theme.display.popup]` settings. Each backend reads its own area and
//! spells the result as its own size arguments.

use std::time::Duration;

use crate::config::PopupConfig;

/// Bound on the one area read a popup open makes before it opens.
pub(super) const AREA_READ_TIMEOUT: Duration = Duration::from_secs(3);

const FLOOR_COLS: u16 = 100;
const FLOOR_ROWS: u16 = 24;
const PERCENT_RANGE: (u16, u16) = (10, 100);
const MIN_MAX_COLS: u16 = 34;
const MIN_MAX_ROWS: u16 = 10;

/// The room's pane area a popup opens over, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PopupArea {
    pub(super) cols: u16,
    pub(super) rows: u16,
}

/// A popup size, border included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PopupSize {
    /// Cells per axis, from a known area.
    Cells { cols: u16, rows: u16 },
    /// Percentages of whatever the multiplexer measures against, when the
    /// area is unknown.
    Percent { width: u16, height: u16 },
}

impl PopupSize {
    /// Per axis, `min(area, ceiling, max(floor, floor(area * percent / 100)))`
    /// with the settings clamped: the area outranks the ceiling, the ceiling
    /// the floor, and the floor the percentage.
    pub(super) fn resolve(config: &PopupConfig, area: Option<PopupArea>) -> Self {
        let width = config.width_percent.clamp(PERCENT_RANGE.0, PERCENT_RANGE.1);
        let height = config
            .height_percent
            .clamp(PERCENT_RANGE.0, PERCENT_RANGE.1);
        let Some(area) = area else {
            return Self::Percent { width, height };
        };
        Self::Cells {
            cols: axis(
                area.cols,
                width,
                config.max_cols.max(MIN_MAX_COLS),
                FLOOR_COLS,
            ),
            rows: axis(
                area.rows,
                height,
                config.max_rows.max(MIN_MAX_ROWS),
                FLOOR_ROWS,
            ),
        }
    }

    /// The width and height words both backends take: a bare integer is
    /// cells, a `%` suffix a percentage.
    pub(super) fn words(self) -> [String; 2] {
        match self {
            Self::Cells { cols, rows } => [cols.to_string(), rows.to_string()],
            Self::Percent { width, height } => [format!("{width}%"), format!("{height}%")],
        }
    }
}

fn axis(area: u16, percent: u16, ceiling: u16, floor: u16) -> u16 {
    let target = u32::from(area) * u32::from(percent) / 100;
    let target = u16::try_from(target).unwrap_or(area).max(floor);
    area.min(ceiling).min(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(cols: u16, rows: u16) -> Option<PopupArea> {
        Some(PopupArea { cols, rows })
    }

    #[test]
    fn default_settings_size_each_area_by_the_rule() {
        let config = PopupConfig::default();
        for ((cols, rows), expected) in [
            ((255, 77), (153, 38)),
            ((185, 52), (111, 26)),
            ((90, 52), (90, 26)),
            ((80, 24), (80, 24)),
            ((400, 120), (160, 40)),
        ] {
            assert_eq!(
                PopupSize::resolve(&config, area(cols, rows)),
                PopupSize::Cells {
                    cols: expected.0,
                    rows: expected.1
                },
                "area {cols}x{rows}"
            );
        }
    }

    #[test]
    fn a_ceiling_below_the_floor_wins_and_settings_clamp_at_use() {
        let sized = |config: PopupConfig| PopupSize::resolve(&config, area(255, 77));
        let defaults = PopupConfig::default();
        assert_eq!(
            sized(PopupConfig {
                max_cols: 80,
                ..defaults
            }),
            PopupSize::Cells { cols: 80, rows: 38 }
        );
        assert_eq!(
            sized(PopupConfig {
                width_percent: 0,
                height_percent: 250,
                max_cols: 1000,
                max_rows: 1000,
            }),
            PopupSize::Cells {
                cols: 100,
                rows: 77
            },
            "10% of 255 is under the floor; 250% clamps to the whole area"
        );
        assert_eq!(
            sized(PopupConfig {
                width_percent: 250,
                max_cols: 0,
                max_rows: 1,
                ..defaults
            }),
            PopupSize::Cells { cols: 34, rows: 10 }
        );
    }

    #[test]
    fn an_unknown_area_gives_the_clamped_percentages() {
        let defaults = PopupConfig::default();
        assert_eq!(
            PopupSize::resolve(&defaults, None),
            PopupSize::Percent {
                width: 60,
                height: 50
            }
        );
        let outside = PopupConfig {
            width_percent: 0,
            height_percent: 250,
            ..defaults
        };
        assert_eq!(
            PopupSize::resolve(&outside, None).words(),
            ["10%".to_owned(), "100%".to_owned()]
        );
        assert_eq!(
            PopupSize::Cells {
                cols: 153,
                rows: 38
            }
            .words(),
            ["153".to_owned(), "38".to_owned()]
        );
    }
}
