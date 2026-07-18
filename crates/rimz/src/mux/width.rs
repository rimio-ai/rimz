//! Sidebar sizing math: a birth seed for panes that materialize before their
//! view geometry is known, and the canonical live target for panes in a sized
//! view. Pure domain arithmetic — the backends spell seeds into layouts,
//! splits, and hooks, then converge live panes here; this file only computes it.

use std::num::NonZeroU16;

use super::SplitDirection;

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

/// Resolve the canonical width for one live view: the room-runtime override
/// verbatim when present, otherwise the configured percentage policy capped at
/// `max_cols`.
pub(crate) fn live_target_cols(
    width: SidebarWidth,
    width_override: Option<NonZeroU16>,
    view_cols: u64,
) -> u64 {
    width_override.map_or_else(
        || width.target_cols(view_cols),
        |cols| u64::from(cols.get()),
    )
}

/// Sidebar pane width: the configured percentage policy for each live view,
/// capped at `max_cols` columns (`theme.display.max_cols`).
/// [`SidebarWidth::birth_size`] seeds panes whose view geometry is not yet
/// known; once live, the canonical width is [`live_target_cols`] of the current
/// view and room-runtime override. A `width_percent` or `max_cols` edit
/// therefore applies on the next convergence.
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

    /// The width verdict a launch resolves on a terminal `detected_cols`
    /// wide: [`Self::target_cols`] of the probe — the percentage capped at
    /// `max_cols` — as columns for tmux and as a percentage spelling for
    /// Zellij layouts. An unknown width (`None` — launch outside a tty)
    /// resolves to the bare cap with the explicit percentage, the pets wide
    /// default, or the automatic policy's narrow fallback.
    pub fn birth_size(self, detected_cols: Option<u16>) -> BirthSize {
        let fallback_percent = self.percent.resolve(None);
        match detected_cols {
            Some(total) if total > 0 => {
                let target = self.target_cols(u64::from(total));
                // target_cols is ≥ 1 and ≤ max_cols, so the fallback chain is
                // unreachable; spelled without panicking per the error rules.
                let cols = u16::try_from(target)
                    .ok()
                    .and_then(NonZeroU16::new)
                    .unwrap_or(self.max_cols);
                // Round the percentage spelling to the nearest whole percent,
                // so materializing it on the probed terminal lands within
                // roughly half a percent of the verdict on either side. The
                // reconcile tolerance absorbs that residue. `target ≤ total`
                // bounds it at 100, and at least 1% keeps the spelling valid.
                let total = u64::from(total);
                let derived = ((target * 100 + total / 2) / total).clamp(1, 100);
                BirthSize {
                    cols,
                    percent: u16::try_from(derived).unwrap_or(fallback_percent),
                }
            }
            _ => BirthSize {
                cols: self.max_cols,
                percent: fallback_percent,
            },
        }
    }

    /// Pin a room-runtime override while retaining a percentage spelling for
    /// Zellij births whose geometry is not known until attach.
    pub fn birth_size_with_override(
        self,
        detected_cols: Option<u16>,
        width_override: Option<NonZeroU16>,
    ) -> BirthSize {
        let Some(cols) = width_override else {
            return self.birth_size(detected_cols);
        };
        let percent = match detected_cols {
            Some(total) if total > 0 => {
                let total = u64::from(total);
                let derived = ((u64::from(cols.get()) * 100 + total / 2) / total).clamp(1, 100);
                u16::try_from(derived).unwrap_or_else(|_| self.percent.resolve(Some(total)))
            }
            _ => self.percent.resolve(None),
        };
        BirthSize { cols, percent }
    }
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self::from_config(&crate::config::ThemeConfig::default())
    }
}

/// The width seed a launch uses before a pane's live view geometry is known.
/// `cols` seeds tmux split and hook paths; `percent` keeps every Zellij
/// layout resizable and safe on detached background geometry. Live repair
/// replaces either seed with the per-view target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BirthSize {
    /// The verdict in columns: `min(percent × probed width, max_cols)`, the
    /// bare cap when no terminal was probed.
    pub cols: NonZeroU16,
    /// The verdict as a share of the probed width (rounded to nearest, ≥ 1%)
    /// — the unknown-geometry spelling; the configured percentage when no
    /// terminal was probed; pets use the wide default and automatic policy
    /// uses its narrow fallback.
    pub percent: u16,
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
/// that can birth the session; the width feeds [`SidebarWidth::birth_size`]
/// and the pair sizes a detached tmux birth.
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
        };
        let exact = WidthStep {
            cols: 2,
            exact: true,
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
        assert_eq!(width.birth_size(None).percent, 10);

        theme.display.width_percent = Some(95);
        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.target_cols(60), 54);
        assert_eq!(width.birth_size(None).percent, 90);
    }

    #[test]
    fn pets_enabled_widens_the_automatic_default() {
        let mut theme = crate::config::ThemeConfig::default();
        theme.pets.enabled = true;

        let width = SidebarWidth::from_config(&theme);
        assert_eq!(width.percent, WidthPercent::Fixed(30));
        assert_eq!(width.target_cols(120), 36);
        assert_eq!(width.target_cols(300), 72);
        assert_eq!(width.birth_size(None).percent, 30);

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
    fn birth_size_seeds_each_launch_from_detected_geometry() {
        let width = SidebarWidth::default();
        let birth = |cols: u16, percent: u16| BirthSize {
            cols: NonZeroU16::new(cols).expect("nonzero"),
            percent,
        };
        // Narrow views use the 25% branch below the cap.
        assert_eq!(width.birth_size(Some(120)), birth(30, 25));
        assert_eq!(width.birth_size(Some(200)), birth(50, 25));
        // The breakpoint remains on the narrow branch.
        assert_eq!(width.birth_size(Some(240)), birth(60, 25));
        // Above it the 30% branch applies, then the cap bites.
        assert_eq!(width.birth_size(Some(300)), birth(72, 24));
        // Past it the cap bites, and the percentage spelling rounds to the
        // nearest share of the probed width: round(72·100/340) = 21.
        assert_eq!(width.birth_size(Some(340)), birth(72, 21));
        assert_eq!(width.birth_size(Some(250)), birth(72, 29));
        assert_eq!(width.birth_size(Some(460)), birth(72, 16));
        // The percentage spelling never rounds below 1%, however wide the view.
        assert_eq!(width.birth_size(Some(7300)), birth(72, 1));
        // Unknown width (no tty, or a zero-width probe) resolves to the bare
        // cap with the narrow fallback percentage for unknown-geometry panes.
        assert_eq!(width.birth_size(None), birth(72, 25));
        assert_eq!(width.birth_size(Some(0)), birth(72, 25));
    }

    #[test]
    fn room_override_pins_cols_and_derives_birth_percent() {
        let width = SidebarWidth::default();
        let cols = NonZeroU16::new(90).expect("nonzero");

        assert_eq!(
            width.birth_size_with_override(Some(300), Some(cols)),
            BirthSize { cols, percent: 30 },
        );
        assert_eq!(
            width.birth_size_with_override(Some(240), Some(cols)),
            BirthSize { cols, percent: 38 },
        );
        assert_eq!(
            width.birth_size_with_override(None, Some(cols)),
            BirthSize { cols, percent: 25 },
        );
        assert_eq!(
            width.birth_size_with_override(Some(120), None),
            width.birth_size(Some(120)),
        );
    }

    #[test]
    fn live_target_prefers_override_then_tracks_capped_view_share() {
        let runtime = NonZeroU16::new(90).expect("nonzero");
        let width = SidebarWidth::default();

        assert_eq!(live_target_cols(width, Some(runtime), 120), 90);
        assert_eq!(live_target_cols(width, None, 120), 30);
        assert_eq!(live_target_cols(width, None, 300), 72);
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
    fn live_targets_can_differ_from_the_birth_seed() {
        let width = SidebarWidth::default();
        let birth = width.birth_size(Some(300));

        assert_eq!(birth.cols.get(), 72);
        assert_eq!(
            width.target_cols(190),
            47,
            "live geometry can differ from the launch seed",
        );
        assert_ne!(u64::from(birth.cols.get()), width.target_cols(190));
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
