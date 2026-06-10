//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw` is the entry point a Ratatui frame calls; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in [`sections`]; vocabulary labels in [`labels`];
//! pure formatting helpers in [`fmt`].
//!
//! Every entry point takes an optional [`Alert`] alongside the snapshot. The
//! alert is the sticky health line pinned to the bottom of the sidebar: while
//! the refresh loop is unhealthy it shows the reason and elapsed time, and
//! after recovery it lingers as a dismissable "last alert" notice. This is the
//! reload-recovery contract documented in
//! [`docs/internals/sidebar.md`](../../docs/internals/sidebar.md).

mod ansi;
mod chrome;
mod compose;
mod effects;
mod fmt;
mod labels;
mod odometer;
mod scrollbar;
mod sections;
mod theme;
mod ui_state;
mod unread;

use self::ansi::{infallible, write_buffer_line_ansi};
use self::chrome::hairline_rule;
#[cfg(test)]
use self::chrome::{abbreviate_under, center_line, help_lines};
pub(crate) use self::compose::compose_lines;
#[cfg(test)]
use self::compose::{auto_scroll_to_selection, build_bottom_chrome, pad_chrome, scroll_thumb};
pub use self::ui_state::{Alert, AnimationCadence, UiState};
pub(crate) use self::ui_state::{Browse, DashboardTab, GateNotice, ManualScroll};
pub(crate) use effects::EffectState;
pub(crate) use odometer::{CLICK_PHASES, CostRolls, TallyAnim};
pub(crate) use scrollbar::ScrollbarFade;
pub(crate) use unread::UnreadTracker;

use std::io::{self, Write};

use crate::config::ProviderTabsMode;
use crate::feed::AgentStatus;
use crate::{SidebarRow, SidebarSnapshot};
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

pub(crate) use self::sections::status_total;
#[cfg(test)]
pub(crate) use self::sections::{MakeUpHit, ProviderTabHit};
use self::theme::Theme;

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &mut UiState::default());
}

/// Whether any visible row is in an animated state — a running agent (working
/// or pre-edit thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), or a row whose lead glyph
/// animates (`?`/`!` breathe, unread rows hard-blink). The serve loop uses this
/// as the broad "does anything move?" gate; [`animation_cadence`] decides whether
/// the movement needs the fast frame grid or the slower cosmetic cadence. A
/// fully settled sidebar (only quiet read idle/done rows) keeps idling on the
/// slow data tick. A stalled agent is projected to `failed` upstream, so it
/// reads as a breathing `!` here. The cockpit's today-spend count-up rides a
/// separate gate
/// (`UiState::tally`), so a finished-turn climb keeps the tick alive even when
/// every row is otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    animation_cadence(snapshot) != AnimationCadence::None
}

// Deliberately unfiltered by the make-up filter: the cockpit's attention
// buckets still animate (and the counts still tick) for rows a filter hides,
// so the gate must track the whole room, not the narrowed body.
pub fn animation_cadence(snapshot: &SidebarSnapshot) -> AnimationCadence {
    let mut slow = false;
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
    {
        if row.is_agent() {
            if row.resolver().is_some() || row.status() == Some(AgentStatus::Running) {
                return AnimationCadence::Fast;
            }
            // `?`/`!` keep breathing until resolved. Unread rows hard-blink
            // until the pane is focused.
            if row.unread || row.status().is_some_and(AgentStatus::is_actionable) {
                slow = true;
            }
        } else if row.process_is_busy() {
            return AnimationCadence::Fast;
        }
    }
    if slow {
        AnimationCadence::Slow
    } else {
        AnimationCadence::None
    }
}

pub fn draw_with_ui(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) {
    let area = frame.area();
    // Borderless: the sidebar already sits inside a framed mux pane, so a second
    // 4-sided border double-frames it and eats two precious columns. The body
    // fills the whole area; a title line and faint hairline rules carry the
    // structure the border used to.
    //
    // The composed maps and the resolved scroll offset are byproducts of the
    // draw: store them so the mouse hit-test and the next frame's viewport read
    // the geometry of the frame the user is actually looking at.
    let composed = compose_lines(snapshot, alert, ui, area.width, area.height);
    ui.line_map = composed.line_map;
    ui.tab_hits = composed.tab_hits;
    ui.make_up_hits = composed.make_up_hits;
    ui.scrollbar
        .observe(composed.scroll_offset, ui.animation_phase);
    ui.scroll_offset = composed.scroll_offset;
    let paragraph = Paragraph::new(Text::from(composed.lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
    // The truecolor garnish tier: a color-only effects pass over the buffer the
    // paragraph just rendered, geometry-locked to the line map this draw wrote.
    // Gated here rather than inside the pass so a non-truecolor terminal — or a
    // `[sidebar] glow = "never"` opt-out — pays nothing, not even the
    // transition observation.
    let theme = Theme::for_sidebar(&snapshot.sidebar);
    if theme.effects_enabled() {
        ui.effects.apply(
            snapshot,
            &theme,
            ui.make_up_filter,
            &ui.line_map,
            ui.selected_pane.as_ref(),
            ui.animation_phase,
            frame.buffer_mut(),
            area,
        );
    }
}

/// The one row-visibility predicate the make-up filter narrows the body by —
/// the single authority the body composer ([`worktree_group_lines`]) and the
/// selection model (`app::visible_rows` and friends) share, so the row
/// ordinals in `line_map` can never drift from the indices selection counts.
/// With no filter every row passes; with a bucket active only agent rows of
/// that status pass, so process rows (status `None`) drop out entirely.
pub(crate) fn row_passes_filter(row: &SidebarRow, filter: Option<AgentStatus>) -> bool {
    filter.is_none_or(|status| row.status() == Some(status))
}

/// The provider kind the dashboard's tab focus derives from the selection: the
/// selected row's agent kind (agent rows carry the kind in `SidebarRow::name`),
/// or `None` for a process row or an empty room — the caller falls back to the
/// first tab. Reads the same filtered universe `selected_index` is an ordinal
/// of, so the dashboard's follow-the-selection stays honest under a make-up
/// filter.
pub(crate) fn selected_agent_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row_passes_filter(row, ui.make_up_filter))
        .nth(ui.selected_index)
        .filter(|row| row.is_agent())
        .map(|row| row.name.clone())
}

/// The provider kind whose block the dashboard shows: the manual tab pick while
/// its panel is still on the dashboard, else the selection-derived kind
/// ([`selected_agent_kind`]) when a panel exists for it, else the first panel.
/// `None` only when the dashboard is empty.
pub(crate) fn active_provider_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    let panels = &snapshot.providers;
    let has_panel = |kind: &str| panels.iter().any(|panel| panel.kind == kind);
    if let Some(tab) = &ui.dashboard_tab
        && has_panel(&tab.kind)
    {
        return Some(tab.kind.clone());
    }
    if let Some(kind) = selected_agent_kind(snapshot, ui)
        && has_panel(&kind)
    {
        return Some(kind);
    }
    panels.first().map(|panel| panel.kind.clone())
}

/// Whether the provider dashboard paints a tab rail. A single provider always
/// paints as a bare block; the configured mode only matters once multiple
/// provider panels are present.
pub(crate) fn dashboard_tabbed(snapshot: &SidebarSnapshot) -> bool {
    match snapshot.sidebar.provider_tabs {
        ProviderTabsMode::Auto => snapshot.providers.len() >= 3,
        ProviderTabsMode::Always => snapshot.providers.len() > 1,
        ProviderTabsMode::Never => false,
    }
}

pub fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
) -> Result<(), B::Error> {
    draw_to_terminal_with_ui(terminal, snapshot, alert, &mut UiState::default())
}

pub fn draw_to_terminal_with_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_with_ui(frame, snapshot, alert, ui))
        .map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    terminal.clear()?;
    draw_to_terminal(&mut terminal, snapshot, alert)?;
    Ok(())
}

pub fn render_fixed_line_ansi<W: Write>(
    mut writer: W,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = TestBackend::new(width, height);
    let mut terminal = infallible(Terminal::new(backend));
    infallible(terminal.clear());
    infallible(draw_to_terminal(&mut terminal, snapshot, alert));
    write_buffer_line_ansi(&mut writer, terminal.backend().buffer())
}

#[cfg(test)]
mod tests;
