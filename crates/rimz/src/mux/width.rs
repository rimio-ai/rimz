//! Sidebar sizing math: keep one room-wide share, spell it for backend
//! layouts, and converge live panes toward it.

use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

use super::SplitDirection;
use crate::ids::MuxName;

const AUTO_WIDTH_BREAKPOINT_COLS: u64 = 240;
const AUTO_WIDTH_NARROW_PERCENT: u16 = 25;
const AUTO_WIDTH_WIDE_PERCENT: u16 = 30;

pub(crate) const MIN_ADJUSTABLE_WIDTH: u16 = 24;

/// One user-requested sidebar width step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthAdjust {
    Narrower,
    Wider,
}

/// One backend-native sidebar width step and whether it can land on an exact
/// column target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidthStep {
    pub cols: u16,
    pub exact: bool,
    /// Full width of the pane's view, or zero when the backend cannot resolve
    /// geometry for this request.
    pub view_cols: u16,
}

/// Resolve the validated absolute target requested by one width keypress.
/// Inexact backends reject a narrower step that would cross the minimum;
/// exact backends clamp that step to the minimum instead.
pub(crate) fn adjust_target_cols(
    base: u16,
    dir: WidthAdjust,
    step: WidthStep,
    min_cols: u16,
) -> Option<NonZeroU16> {
    match dir {
        WidthAdjust::Wider => NonZeroU16::new(base.saturating_add(step.cols)),
        WidthAdjust::Narrower if base <= min_cols => None,
        WidthAdjust::Narrower => {
            let target = base.saturating_sub(step.cols);
            if target >= min_cols {
                NonZeroU16::new(target)
            } else if step.exact {
                NonZeroU16::new(min_cols)
            } else {
                None
            }
        }
    }
}

/// Sidebar width policy from `theme.display.width_percent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthPercent {
    /// An explicit configured percentage, clamped to 10-90 when used.
    Fixed(u16),
    /// The width-keyed default: 30% above 240 columns and 25% at or below.
    Auto,
}

impl WidthPercent {
    /// Resolve the policy for a known view width, using the narrow branch when
    /// geometry is unavailable.
    pub fn resolve(self, view_cols: Option<u64>) -> u16 {
        match self {
            Self::Fixed(percent) => percent.clamp(10, 90),
            Self::Auto => match view_cols {
                Some(cols) if cols > AUTO_WIDTH_BREAKPOINT_COLS => AUTO_WIDTH_WIDE_PERCENT,
                _ => AUTO_WIDTH_NARROW_PERCENT,
            },
        }
    }
}

/// Tenths of a percent of the full view, in the inclusive range `1..=1000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct WidthPermille(u16);

impl WidthPermille {
    const MIN: u16 = 1;
    const MAX: u16 = 1000;
    const ZELLIJ_RUNG: u16 = 50;

    /// Convert a whole percentage into a valid share.
    pub fn from_percent(percent: u16) -> Self {
        Self(percent.saturating_mul(10).clamp(Self::MIN, Self::MAX))
    }

    /// Convert an absolute width into the smallest share that renders back to
    /// that width. The ceiling preserves tmux's column-exact intent.
    pub fn from_cols(cols: NonZeroU16, view_cols: NonZeroU16) -> Self {
        let cols = u64::from(cols.get());
        let view_cols = u64::from(view_cols.get());
        let permille = (cols * 1000).div_ceil(view_cols);
        Self(
            u16::try_from(permille)
                .unwrap_or(Self::MAX)
                .clamp(Self::MIN, Self::MAX),
        )
    }

    /// Render this share as absolute columns for one view, preserving the
    /// minimum adjustable width.
    pub fn cols(self, view_cols: NonZeroU16) -> NonZeroU16 {
        let cols = (u64::from(self.0) * u64::from(view_cols.get()) / 1000)
            .max(u64::from(MIN_ADJUSTABLE_WIDTH));
        NonZeroU16::new(u16::try_from(cols).unwrap_or(u16::MAX)).unwrap_or(NonZeroU16::MAX)
    }

    /// Spell this share as a whole percentage for Zellij layout KDL.
    pub fn to_percent_rounded(self) -> u16 {
        ((self.0 + 5) / 10).clamp(1, 100)
    }

    /// Snap to the nearest backend-native share rung.
    pub fn snap_to_rung(self, mux: MuxName) -> Self {
        if mux != MuxName::Zellij {
            return self;
        }
        let snapped = ((self.0 + Self::ZELLIJ_RUNG / 2) / Self::ZELLIJ_RUNG)
            .saturating_mul(Self::ZELLIJ_RUNG)
            .clamp(Self::ZELLIJ_RUNG, Self::MAX);
        Self(snapped)
    }
}

impl TryFrom<u16> for WidthPermille {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err("width permille must be between 1 and 1000")
        }
    }
}

impl<'de> Deserialize<'de> for WidthPermille {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// One resolved room-wide share rendered against each backend view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarTarget {
    pub share: WidthPermille,
    pub max_cols: NonZeroU16,
    pub pinned: bool,
}

impl SidebarTarget {
    /// Render the target for one view, applying the configured cap only to an
    /// unpinned default. Without geometry the cap is the safe birth seed.
    pub fn cols(self, view_cols: Option<u16>) -> NonZeroU16 {
        let Some(view_cols) = view_cols.and_then(NonZeroU16::new) else {
            return self.max_cols;
        };
        let cols = self.share.cols(view_cols);
        if self.pinned {
            cols
        } else {
            // `MIN_ADJUSTABLE_WIDTH` is a nonzero pane-width constant.
            let floor =
                NonZeroU16::new(MIN_ADJUSTABLE_WIDTH).expect("minimum pane width is nonzero");
            cols.min(self.max_cols).max(floor)
        }
    }

    /// Spell the resolved share as a whole percentage for Zellij layout KDL.
    pub fn percent(self) -> u16 {
        self.share.to_percent_rounded()
    }
}

/// Sidebar pane width: the configured percentage policy for each live view,
/// capped at `max_cols` columns (`theme.display.max_cols`).
/// A `width_percent` or `max_cols` edit applies on the next unpinned
/// room-target resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarWidth {
    /// Percentage policy for the view width — tracks terminal size below the cap.
    pub percent: WidthPercent,
    /// Column cap the percentage never exceeds (`theme.display.max_cols`).
    pub max_cols: NonZeroU16,
}

impl SidebarWidth {
    /// The width a machine config asks for: its explicit percentage, the wide
    /// default when pets are enabled, or the width-keyed default, plus the
    /// column cap. Percentage bounds are enforced when used.
    pub fn from_config(theme: &crate::config::ThemeConfig) -> Self {
        let display = &theme.display;
        let percent = match display.width_percent {
            Some(percent) => WidthPercent::Fixed(percent),
            // The pet dashboard needs the wide default at any view width.
            None if theme.pets.enabled => WidthPercent::Fixed(AUTO_WIDTH_WIDE_PERCENT),
            None => WidthPercent::Auto,
        };
        Self {
            percent,
            max_cols: display.max_cols,
        }
    }

    /// The capped target in columns for a view `total_cols` wide:
    /// `min(percent × total_cols, max_cols)`.
    pub fn target_cols(self, total_cols: u64) -> u64 {
        let percent = self.percent.resolve(Some(total_cols));
        let cols = (total_cols * u64::from(percent) / 100).max(1);
        cols.min(self.cap_cols())
    }

    /// The configured column cap.
    pub fn cap_cols(self) -> u64 {
        u64::from(self.max_cols.get())
    }
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self::from_config(&crate::config::ThemeConfig::default())
    }
}

/// Carries the connecting client's terminal `<cols>x<rows>` across SSH for a
/// remote room birth that has no pty of its own.
pub const CLIENT_SIZE_ENV: &str = "RIMZ_CLIENT_SIZE";

fn parse_client_size(value: &str) -> Option<(u16, u16)> {
    let (cols, rows) = value.split_once('x')?;
    let cols = cols.parse::<u16>().ok().filter(|cols| *cols > 0)?;
    let rows = rows.parse::<u16>().ok().filter(|rows| *rows > 0)?;
    Some((cols, rows))
}

/// The connecting client's terminal size shipped across SSH, when valid.
pub fn client_size_from_env() -> Option<(u16, u16)> {
    std::env::var(CLIENT_SIZE_ENV)
        .ok()
        .as_deref()
        .and_then(parse_client_size)
}

/// Whether a live sidebar's drift warrants repair toward the canonical width.
/// Drift beyond half the backend's resize resolution (with a one-column
/// minimum) can be moved closer. The band is symmetric because the canonical
/// target already carries either the user's recorded width or the configured
/// cap.
pub(crate) fn sidebar_width_off_spec(cols: u64, canonical_cols: u64, step_cols: u64) -> bool {
    cols.abs_diff(canonical_cols) > (step_cols / 2).max(1)
}

/// Zellij's built-in resize increment is approximately 5% of the view width.
pub(crate) fn zellij_resize_step_cols(view_cols: u64) -> u64 {
    (view_cols / 20).max(1)
}

/// The invoking terminal's `(cols, rows)`, when any standard stream is
/// attached to one (stdout, then stderr, then stdin). Probed by the command
/// that can birth the session; the pair sizes a detached tmux birth.
pub fn detect_terminal_size() -> Option<(u16, u16)> {
    terminal_size::terminal_size().map(|(width, height)| (width.0, height.0))
}

/// Split side-by-side when the pane is visually wider than tall, otherwise
/// stack. This is a cell-count heuristic for detached CLI/tmux paths; Zellij's
/// native no-direction key path uses the terminal's real pixel ratio.
pub fn split_along_longer_edge(cols: u16, rows: u16) -> SplitDirection {
    const CELL_ASPECT: u32 = 2;
    if u32::from(cols) > u32::from(rows) * CELL_ASPECT {
        SplitDirection::Right
    } else {
        SplitDirection::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_adjustment_resolves_absolute_targets_and_floor() {
        let inexact = WidthStep {
            cols: 10,
            exact: false,
            view_cols: 200,
        };
        let exact = WidthStep {
            cols: 2,
            exact: true,
            view_cols: 200,
        };

        assert_eq!(
            adjust_target_cols(30, WidthAdjust::Wider, inexact, 24),
            NonZeroU16::new(40)
        );
        assert_eq!(
            adjust_target_cols(40, WidthAdjust::Narrower, inexact, 24),
            NonZeroU16::new(30)
        );
        assert_eq!(
            adjust_target_cols(30, WidthAdjust::Narrower, inexact, 24),
            None
        );
        assert_eq!(
            adjust_target_cols(25, WidthAdjust::Narrower, exact, 24),
            NonZeroU16::new(24)
        );
        assert_eq!(
            adjust_target_cols(24, WidthAdjust::Narrower, exact, 24),
            None
        );
    }

    #[test]
    fn sidebar_width_uses_configured_percent_and_cap() {
        let mut theme = crate::config::ThemeConfig::default();
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.percent, WidthPercent::Auto);
        assert_eq!(width.cap_cols(), 72);
        assert_eq!(width.target_cols(120), 30);
        assert_eq!(width.target_cols(300), 72);

        theme.display.width_percent = Some(25);
        let max = NonZeroU16::new(100).expect("nonzero");
        theme.display.max_cols = max;
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.percent, WidthPercent::Fixed(25));
        assert_eq!(width.max_cols, max);
        assert_eq!(width.target_cols(120), 30);

        theme.display.width_percent = Some(5);
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(
            width.percent,
            WidthPercent::Fixed(5),
            "config stays raw until use",
        );
        assert_eq!(width.target_cols(120), 12);
        assert_eq!(width.percent.resolve(None), 10);

        theme.display.width_percent = Some(95);
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.target_cols(60), 54);
        assert_eq!(width.percent.resolve(None), 90);
    }

    #[test]
    fn pets_enabled_widens_the_automatic_default() {
        let mut theme = crate::config::ThemeConfig::default();
        theme.pets.enabled = true;

        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.percent, WidthPercent::Fixed(30));
        assert_eq!(width.target_cols(120), 36);
        assert_eq!(width.target_cols(300), 72);
        assert_eq!(width.percent.resolve(None), 30);

        theme.display.width_percent = Some(20);
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.percent, WidthPercent::Fixed(20));
    }

    #[test]
    fn automatic_width_switches_at_240_view_columns() {
        let width = SidebarWidth::default();

        assert_eq!(width.target_cols(200), 50);
        assert_eq!(width.target_cols(240), 60);
        assert_eq!(width.target_cols(241), 72);
    }

    #[test]
    fn width_permille_round_trips_columns_and_holds_the_floor() {
        let cols = |cols| NonZeroU16::new(cols).expect("nonzero");

        for (view, target) in [(120, 30), (127, 31), (213, 72), (400, 82)] {
            let view = cols(view);
            let target = cols(target);
            assert_eq!(WidthPermille::from_cols(target, view).cols(view), target);
        }
        assert_eq!(
            WidthPermille::from_percent(1).cols(cols(120)),
            cols(MIN_ADJUSTABLE_WIDTH),
        );
    }

    #[test]
    fn width_permille_snaps_only_zellij_to_share_rungs() {
        let cols = |cols| NonZeroU16::new(cols).expect("nonzero");
        let share = WidthPermille::from_cols(cols(81), cols(200));

        assert_eq!(
            share.snap_to_rung(MuxName::Zellij),
            WidthPermille::from_percent(40),
        );
        assert_eq!(share.snap_to_rung(MuxName::Tmux), share);
        assert_eq!(share.snap_to_rung(MuxName::Zellij).to_percent_rounded(), 40,);
    }

    #[test]
    fn sidebar_target_renders_each_view_and_applies_only_the_default_cap() {
        let target = SidebarTarget {
            share: WidthPermille::from_percent(25),
            max_cols: NonZeroU16::new(72).expect("nonzero"),
            pinned: false,
        };
        assert_eq!(target.cols(Some(120)), NonZeroU16::new(30).unwrap());
        assert_eq!(target.cols(Some(400)), NonZeroU16::new(72).unwrap());
        assert_eq!(target.cols(None), NonZeroU16::new(72).unwrap());

        let pinned = SidebarTarget {
            pinned: true,
            ..target
        };
        assert_eq!(pinned.cols(Some(400)), NonZeroU16::new(100).unwrap());
        assert_eq!(pinned.percent(), 25);

        let below_floor = SidebarTarget {
            max_cols: NonZeroU16::new(3).unwrap(),
            ..target
        };
        assert_eq!(
            below_floor.cols(Some(120)),
            NonZeroU16::new(MIN_ADJUSTABLE_WIDTH).unwrap(),
        );
    }

    #[test]
    fn sidebar_width_repair_uses_half_a_resize_step_as_slack() {
        // Exact backends retain only the one-column minimum band.
        assert!(!sidebar_width_off_spec(71, 72, 1));
        assert!(!sidebar_width_off_spec(73, 72, 1));
        assert!(sidebar_width_off_spec(70, 72, 1));
        assert!(sidebar_width_off_spec(74, 72, 1));

        // A 213-column Zellij view has a ten-column step and a symmetric
        // five-column band around the canonical width.
        let step = zellij_resize_step_cols(213);
        assert_eq!(step, 10);
        assert!(!sidebar_width_off_spec(58, 63, step));
        assert!(!sidebar_width_off_spec(68, 63, step));
        assert!(sidebar_width_off_spec(57, 63, step));
        assert!(sidebar_width_off_spec(69, 63, step));

        // Regression: a full Zellij step and one tmux keypress both propagate.
        assert!(sidebar_width_off_spec(53, 63, zellij_resize_step_cols(213)));
        assert!(sidebar_width_off_spec(61, 63, 1));

        // Zero and tiny views still produce a one-column minimum band.
        assert_eq!(zellij_resize_step_cols(0), 1);
        assert_eq!(zellij_resize_step_cols(19), 1);
    }

    #[test]
    fn resolved_targets_can_differ_when_view_geometry_changes() {
        let width = SidebarWidth::default();
        let wide = width.target_cols(300);
        let narrow = width.target_cols(190);
        assert_eq!(wide, 72);
        assert_eq!(narrow, 47);
        assert_ne!(wide, narrow);
    }

    #[test]
    fn split_along_longer_edge_uses_cell_aspect_boundary() {
        assert_eq!(split_along_longer_edge(121, 60), SplitDirection::Right);
        assert_eq!(split_along_longer_edge(80, 60), SplitDirection::Down);
        assert_eq!(split_along_longer_edge(120, 60), SplitDirection::Down);
    }

    #[test]
    fn client_size_parser_accepts_only_nonzero_u16_pairs() {
        assert_eq!(parse_client_size("120x40"), Some((120, 40)));
        for invalid in [
            "junk",
            "120",
            "120x",
            "x40",
            "0x40",
            "120x0",
            "65536x40",
            "120x65536",
            "120x40x20",
        ] {
            assert_eq!(parse_client_size(invalid), None, "{invalid}");
        }
    }
}
