//! Sidebar sizing math: the width the session-birth command resolves once and
//! every pane of the session is born with. Commands targeting a live session
//! mirror the verdict the mux already carries and use the probe-less cap only as
//! a fallback. Pure domain arithmetic — the backends spell the verdict into
//! layouts, splits, and hooks; this file only computes it.

use std::num::NonZeroU16;

/// Default sidebar width as a percentage of the view. The single source of
/// truth for both the CLI launch paths and the user-wide reload reconcile.
const DEFAULT_SIDEBAR_WIDTH_PERCENT: u16 = 30;

/// Sidebar pane width: a percentage of the view, capped at `max_cols` columns
/// (`theme.display.max_cols`). The width is resolved once by the command that
/// births the session: the birth path probes its terminal ([`detect_terminal_size`])
/// and [`SidebarWidth::birth_size`] turns the probe into the one [`BirthSize`]
/// verdict every pane of the session is born with — constant for the session's
/// life. Commands targeting a live session mirror that verdict from the mux
/// (Zellij's template or live docked sidebars, tmux's hook) and fall back to
/// `birth_size(None)`. Birth-time only: a `max_cols` edit applies at the next
/// launch.
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
                // Floor keeps the percentage spelling at or under the verdict
                // on the probed terminal; at least 1% so the spelling stays
                // valid. `target ≤ total` bounds it at 100, so the conversion
                // holds.
                let derived = (target * 100 / u64::from(total)).max(1);
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
}

impl Default for SidebarWidth {
    fn default() -> Self {
        Self::from_config(&crate::config::DisplayConfig::default())
    }
}

/// The one width verdict every sidebar pane of a launch is born with —
/// resolved once per command by [`SidebarWidth::birth_size`] from the
/// session-birth terminal, then constant for the session's life. Two spellings of
/// the same verdict: `cols` pins panes that instantiate at known geometry
/// (the Zellij `new_tab_template` an attached client opens tabs from, the
/// tmux `after-new-window` hook), and `percent` covers panes that materialize
/// at unknown geometry — the detached Zellij birth, where a fixed size wider
/// than the background session's default geometry kills the session — and
/// rescales to `cols` when the launching client attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BirthSize {
    /// The verdict in columns: `min(percent × probed width, max_cols)`, the
    /// bare cap when no terminal was probed.
    pub cols: NonZeroU16,
    /// The verdict as a share of the probed width (floor, ≥ 1%) — the
    /// unknown-geometry spelling; the configured percentage when no terminal
    /// was probed.
    pub percent: u16,
}

/// The invoking terminal's `(cols, rows)`, when stdout is attached to one.
/// Probed by the command that can birth the session; the width feeds
/// [`SidebarWidth::birth_size`] and the pair sizes a detached tmux birth.
pub fn detect_terminal_size() -> Option<(u16, u16)> {
    terminal_size::terminal_size().map(|(width, height)| (width.0, height.0))
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
    fn birth_size_resolves_one_fixed_verdict_per_launch() {
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
        // Past it the cap bites, and the percentage spelling floors to the
        // cap's share of the probed width: ⌊72·100/340⌋ = 21.
        assert_eq!(width.birth_size(Some(340)), birth(72, 21));
        // The percentage spelling never floors below 1%, however wide the view.
        assert_eq!(width.birth_size(Some(7300)), birth(72, 1));
        // Unknown width (no tty, or a zero-width probe) resolves to the bare
        // cap with the raw percentage for unknown-geometry panes.
        assert_eq!(width.birth_size(None), birth(72, 30));
        assert_eq!(width.birth_size(Some(0)), birth(72, 30));
    }

    #[test]
    fn live_targets_do_not_change_the_session_birth_verdict() {
        let width = SidebarWidth::default();
        let birth = width.birth_size(Some(300));

        assert_eq!(birth.cols.get(), 72);
        assert_eq!(
            width.target_cols(190),
            57,
            "live geometry can differ from the fixed session verdict",
        );
        assert_ne!(u64::from(birth.cols.get()), width.target_cols(190));
    }
}
