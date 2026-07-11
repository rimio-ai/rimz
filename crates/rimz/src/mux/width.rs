//! Sidebar sizing math: a birth seed for panes that materialize before their
//! view geometry is known, and the canonical live target for panes in a sized
//! view. Pure domain arithmetic — the backends spell seeds into layouts,
//! splits, and hooks, then converge live panes here; this file only computes it.

use std::num::NonZeroU16;

use super::SplitDirection;

/// One user-requested sidebar width step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidthAdjust {
    Narrower,
    Wider,
}

/// Resolve the canonical width for one live view: the room-runtime override
/// verbatim when present, otherwise the configured percentage capped at
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

/// Default sidebar width as a percentage of the view. The single source of
/// truth for both the CLI launch paths and the user-wide reload reconcile.
pub(crate) const DEFAULT_SIDEBAR_WIDTH_PERCENT: u16 = 30;

/// Sidebar pane width: a percentage of each live view, capped at `max_cols`
/// columns (`theme.display.max_cols`). [`SidebarWidth::birth_size`] seeds panes
/// whose view geometry is not yet known; once live, the canonical width is
/// [`live_target_cols`] of the current view and room-runtime override. A
/// `max_cols` edit therefore applies on the next convergence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidebarWidth {
    /// Percentage of the view width — tracks terminal size below the cap.
    pub percent: u16,
    /// Column cap the percentage never exceeds (`theme.display.max_cols`).
    pub max_cols: NonZeroU16,
}

impl SidebarWidth {
    /// The width a machine config asks for: the default percentage at the
    /// configured column cap.
    pub fn from_config(display: &crate::config::DisplayConfig) -> Self {
        Self {
            percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
            max_cols: display.max_cols,
        }
    }

    /// The capped target in columns for a view `total_cols` wide:
    /// `min(percent, max_cols)`.
    pub fn target_cols(self, total_cols: u64) -> u64 {
        let percent = (total_cols * u64::from(self.percent.clamp(10, 90)) / 100).max(1);
        percent.min(self.cap_cols())
    }

    /// The column cap alone — the threshold above which a pane is born fixed.
    pub fn cap_cols(self) -> u64 {
        u64::from(self.max_cols.get())
    }

    /// The width verdict a launch resolves on a terminal `detected_cols`
    /// wide: [`Self::target_cols`] of the probe — the percentage capped at
    /// `max_cols` — as fixed columns, plus its percentage spelling for panes
    /// that materialize at unknown geometry. An unknown width (`None` —
    /// launch outside a tty) resolves to the bare cap with the raw
    /// percentage.
    pub fn birth_size(self, detected_cols: Option<u16>) -> BirthSize {
        let percent = self.percent.clamp(10, 90);
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
                    percent: u16::try_from(derived).unwrap_or(percent),
                }
            }
            _ => BirthSize {
                cols: self.max_cols,
                percent,
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
                u16::try_from(derived).unwrap_or(self.percent.clamp(10, 90))
            }
            _ => self.percent.clamp(10, 90),
        };
        BirthSize { cols, percent }
    }
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self::from_config(&crate::config::DisplayConfig::default())
    }
}

/// The width seed a launch uses before a pane's live view geometry is known.
/// `cols` seeds fixed-column split and explicit-layout paths; `percent` keeps a
/// detached Zellij birth safe on its small background geometry. Live repair
/// replaces either spelling with the per-view target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BirthSize {
    /// The verdict in columns: `min(percent × probed width, max_cols)`, the
    /// bare cap when no terminal was probed.
    pub cols: NonZeroU16,
    /// The verdict as a share of the probed width (rounded to nearest, ≥ 1%)
    /// — the unknown-geometry spelling; the configured percentage when no
    /// terminal was probed.
    pub percent: u16,
}

/// Whether a live sidebar's width warrants repair toward the canonical
/// verdict. Width above canonical has only a two-column allowance because it
/// violates the configured cap. Below canonical, one Zellij resize step (5%
/// of the view) lets a repair stop on the near side without re-triggering and
/// lets a small manual narrowing stick.
pub fn sidebar_width_off_spec(cols: u64, canonical_cols: u64, view_cols: u64) -> bool {
    cols > canonical_cols + 2 || canonical_cols.saturating_sub(cols) > 2.max(view_cols / 20)
}

/// The invoking terminal's `(cols, rows)`, when stdout is attached to one.
/// Probed by the command that can birth the session; the width feeds
/// [`SidebarWidth::birth_size`] and the pair sizes a detached tmux birth.
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
    fn sidebar_width_uses_default_percent_and_configured_cap() {
        let mut display = crate::config::DisplayConfig::default();
        let width = SidebarWidth::from_config(&display);
        assert_eq!(width.percent, DEFAULT_SIDEBAR_WIDTH_PERCENT);
        assert_eq!(width.cap_cols(), 72);
        assert_eq!(width.target_cols(120), 36);
        assert_eq!(width.target_cols(300), 72);

        let max = NonZeroU16::new(100).expect("nonzero");
        display.max_cols = max;
        assert_eq!(SidebarWidth::from_config(&display).max_cols, max);
    }

    #[test]
    fn birth_size_seeds_each_launch_from_detected_geometry() {
        let width = SidebarWidth::default();
        let birth = |cols: u16, percent: u16| BirthSize {
            cols: NonZeroU16::new(cols).expect("nonzero"),
            percent,
        };
        // Below the cap the verdict is the percentage share, as fixed columns:
        // 30% of 120 is 36 ≤ 72 — never a raw percentage that re-evaluates
        // against whatever geometry instantiates a later tab.
        assert_eq!(width.birth_size(Some(120)), birth(36, 30));
        // Exactly at the cap: 30% of 240 is 72.
        assert_eq!(width.birth_size(Some(240)), birth(72, 30));
        // Past it the cap bites, and the percentage spelling rounds to the
        // nearest share of the probed width: round(72·100/340) = 21.
        assert_eq!(width.birth_size(Some(340)), birth(72, 21));
        assert_eq!(width.birth_size(Some(250)), birth(72, 29));
        assert_eq!(width.birth_size(Some(460)), birth(72, 16));
        // The percentage spelling never rounds below 1%, however wide the view.
        assert_eq!(width.birth_size(Some(7300)), birth(72, 1));
        // Unknown width (no tty, or a zero-width probe) resolves to the bare
        // cap with the raw percentage for unknown-geometry panes.
        assert_eq!(width.birth_size(None), birth(72, 30));
        assert_eq!(width.birth_size(Some(0)), birth(72, 30));
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
            BirthSize { cols, percent: 30 },
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
        assert_eq!(live_target_cols(width, None, 120), 36);
        assert_eq!(live_target_cols(width, None, 300), 72);
    }

    #[test]
    fn sidebar_width_repair_uses_one_resize_step_as_slack() {
        // A 200-column view gets ten columns of slack below canonical, while
        // a pane more than two columns wider always violates the cap.
        assert!(!sidebar_width_off_spec(62, 72, 200));
        assert!(sidebar_width_off_spec(82, 72, 200));
        assert!(sidebar_width_off_spec(61, 72, 200));
        assert!(sidebar_width_off_spec(83, 72, 200));

        // A raw 30% detached birth against the live browser width exceeds the
        // configured cap, while the first post-shrink crossing stays stable.
        assert!(sidebar_width_off_spec(85, 72, 283));
        assert!(!sidebar_width_off_spec(71, 72, 283));

        // Tiny or unknown-looking views retain a two-column minimum band.
        assert!(!sidebar_width_off_spec(70, 72, 0));
        assert!(!sidebar_width_off_spec(74, 72, 20));
        assert!(sidebar_width_off_spec(69, 72, 20));
        assert!(sidebar_width_off_spec(75, 72, 20));
    }

    #[test]
    fn live_targets_can_differ_from_the_birth_seed() {
        let width = SidebarWidth::default();
        let birth = width.birth_size(Some(300));

        assert_eq!(birth.cols.get(), 72);
        assert_eq!(
            width.target_cols(190),
            57,
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
}
