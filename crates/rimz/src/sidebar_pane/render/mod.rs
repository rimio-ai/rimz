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
//! [`docs/internals/sidebar/sidebar.md`](../../docs/internals/sidebar/sidebar.md).

mod animation;
mod ansi;
mod chrome;
mod compose;
mod effects;
mod embedded_themes;
mod fmt;
pub mod glyph_set;
mod labels;
mod odometer;
mod oklab;
pub mod scheme;
mod scrollbar;
mod sections;
mod theme;
mod ui_state;

use self::ansi::{infallible, write_buffer_line_ansi};
use self::chrome::hairline_rule;
#[cfg(test)]
use self::chrome::{abbreviate_under, center_line, help_lines};
pub(crate) use self::compose::compose_lines;
use self::compose::lead_unread;
#[cfg(test)]
use self::compose::{auto_scroll_to_selection, build_bottom_chrome, pad_chrome, scroll_thumb};
pub use self::ui_state::{Alert, AnimationCadence, UiState};
pub(crate) use self::ui_state::{BodyFilter, Browse, DashboardTab, GateNotice, ManualScroll};
pub(crate) use effects::EffectState;
pub(crate) use odometer::{CLICK_PHASES, CostRolls, TallyAnim};
pub(crate) use scrollbar::ScrollbarFade;

use std::io::{self, Write};

use crate::agents::TurnPhase;
use crate::config::AnimationSpec;
use crate::feed::AgentStatus;
use crate::sidebar_pane::pets::PetAction;
use crate::{ProcessState, SidebarRow, SidebarSnapshot};
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

#[cfg(test)]
pub(crate) use self::sections::{MakeUpHit, ProviderTabHit};
pub(crate) use self::sections::{status_total, unread_total};
use self::theme::Theme;

#[cfg(test)]
fn age_heat_amount_for_test(age_secs: i64) -> f32 {
    let first_quarter = crate::feed::ATTENTION_AGE_CEILING_SECS / 4;
    debug_assert!(age_secs > first_quarter);
    let heat_span = crate::feed::ATTENTION_AGE_CEILING_SECS - first_quarter;
    ((age_secs - first_quarter) as f32 / heat_span as f32).min(1.0)
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &mut UiState::default());
}

/// Whether any visible row is in an animated state — a running agent (working
/// or pre-edit thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), or the single lead unread
/// `?`/`!` row whose configured effect flows. The serve loop uses this as the
/// broad "does anything move?" gate; [`animation_cadence`] decides whether the
/// movement needs the fast frame grid or the breath grid. A fully settled
/// sidebar — quiet read idle/done rows, and every unread row past the lead
/// resting at its static crest — keeps idling on the slow data tick. A stalled
/// agent is projected to `failed` upstream, so it reads as a pulsing `!` here.
/// The cockpit's headline-spend count-up rides a separate gate (`UiState::tally`),
/// so a finished-turn climb keeps the tick alive even when every row is
/// otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    animation_cadence(snapshot) != AnimationCadence::None
}

// Deliberately unfiltered by the make-up filter: the cockpit's attention
// buckets still animate (and the counts still tick) for rows a filter hides,
// so the gate must track the whole room, not the narrowed body.
pub fn animation_cadence(snapshot: &SidebarSnapshot) -> AnimationCadence {
    let mut breath = false;
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
    {
        if row.is_agent() {
            if row.resolver().is_some() || row.status() == Some(AgentStatus::Running) {
                return AnimationCadence::Fast;
            }
            // A read `?`/`!` row honours its configured effect. Unread motion is
            // reserved to the single lead row (checked once below); every other
            // unread row settles to the static `bright` crest and asks nothing
            // of the grid.
            if !row.unread
                && let Some(status) = row.status()
                && status.is_actionable()
            {
                breath |= status_needs_motion(&snapshot.theme.animations, status);
            }
        } else if row.process_is_busy() {
            return AnimationCadence::Fast;
        }
    }
    // The lead unread row wears the continuous unread effect, so it keeps the
    // breath grid warm — but only when that effect actually moves frame to
    // frame, not when it rests at the static `bright` crest or its role is
    // quieted to `static`. The cockpit lead bucket pulses with it, so this one
    // condition covers both the row and its bucket.
    breath |= lead_unread_needs_motion(snapshot);
    if breath || snapshot.theme.animations.has_resting_motion() {
        AnimationCadence::Breath
    } else {
        AnimationCadence::None
    }
}

/// Whether the single lead unread row carries per-frame motion the breath grid
/// must serve. The lead is the oldest actionable unread ask ([`lead_unread`]);
/// it animates when the configured unread effect flows (shimmer or blink, not
/// the held `bright` crest) and the lead's role has not been quieted to
/// `static`.
fn lead_unread_needs_motion(snapshot: &SidebarSnapshot) -> bool {
    let Some((_, status)) = lead_unread(&snapshot.worktree_groups) else {
        return false;
    };
    unread_effect_animates(snapshot.theme.animations.unread)
        && status_needs_motion(&snapshot.theme.animations, status)
}

/// Whether the configured unread effect flows on the phase grid. `shimmer` and
/// `blink` move; the held `bright` crest is static, so a lead row wearing it
/// asks nothing of the breath grid.
fn unread_effect_animates(effect: Option<crate::config::UnreadEffect>) -> bool {
    !matches!(effect, Some(crate::config::UnreadEffect::Bright))
}

fn status_needs_motion(
    animations: &crate::config::ThemeAnimationsConfig,
    status: AgentStatus,
) -> bool {
    let spec = match status {
        AgentStatus::Waiting => animations.waiting.as_ref(),
        AgentStatus::Failed => animations.failed.as_ref(),
        _ => None,
    };
    spec_needs_motion(spec)
}

fn spec_needs_motion(spec: Option<&AnimationSpec>) -> bool {
    match spec {
        Some(spec) if spec.disables_effect_motion() => spec.has_frame_motion(),
        _ => true,
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
    let theme = Theme::for_sidebar(&snapshot.theme);
    // The transition garnish tier: a color-only effects pass over the buffer
    // the paragraph just rendered, geometry-locked to the line map this draw wrote.
    // Gated here rather than inside the pass so a non-truecolor terminal — or a
    // `[theme.display] glow = "never"` opt-out — pays nothing, not even the
    // transition observation.
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
pub(crate) fn row_passes_filter(row: &SidebarRow, filter: Option<BodyFilter>) -> bool {
    match filter {
        None => true,
        Some(BodyFilter::Status(status)) => row.status() == Some(status),
        Some(BodyFilter::Unread) => row.unread,
    }
}

fn selected_row<'a>(snapshot: &'a SidebarSnapshot, ui: &UiState) -> Option<&'a SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row_passes_filter(row, ui.make_up_filter))
        .nth(ui.selected_index)
}

/// The provider kind the dashboard's tab focus derives from the selection: the
/// selected row's agent kind (agent rows carry the kind in `SidebarRow::name`),
/// or `None` for a process row or an empty room — the caller falls back to the
/// first tab. Reads the same filtered universe `selected_index` is an ordinal
/// of, so the dashboard's follow-the-selection stays honest under a make-up
/// filter.
pub(crate) fn selected_agent_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    selected_row(snapshot, ui)
        .filter(|row| row.is_agent())
        .map(|row| row.name.clone())
}

pub(crate) fn selected_pet_action(snapshot: &SidebarSnapshot, ui: &UiState) -> PetAction {
    selected_row(snapshot, ui).map_or(PetAction::Idle, row_pet_action)
}

fn row_pet_action(row: &SidebarRow) -> PetAction {
    if let Some(agent) = row.as_agent() {
        let status = agent.status.unwrap_or(AgentStatus::Idle);
        if agent.compacting {
            return PetAction::Review;
        }
        if status == AgentStatus::Waiting {
            return PetAction::Ask;
        }
        if status == AgentStatus::Failed {
            return PetAction::Failed;
        }
        if status == AgentStatus::Paused {
            return PetAction::Waiting;
        }
        if status == AgentStatus::Running
            && (agent.phase == TurnPhase::Parked
                || agent
                    .sub_agents
                    .iter()
                    .any(|child| child.status == AgentStatus::Running))
        {
            return PetAction::Waiting;
        }
        return match (status, agent.phase) {
            (AgentStatus::Running, TurnPhase::Reasoning) => PetAction::Thinking,
            (AgentStatus::Running, _) => PetAction::Running,
            (AgentStatus::Idle | AgentStatus::Success, _) => PetAction::Idle,
            (AgentStatus::Waiting, _) => PetAction::Ask,
            (AgentStatus::Failed, _) => PetAction::Failed,
            (AgentStatus::Paused, _) => PetAction::Waiting,
        };
    }
    match row.process_state().unwrap_or(ProcessState::Idle) {
        ProcessState::Busy => PetAction::Running,
        ProcessState::Stuck => PetAction::Failed,
        ProcessState::Idle => PetAction::Idle,
    }
}

pub(crate) fn unread_pet_row_ids(snapshot: &SidebarSnapshot) -> impl Iterator<Item = String> + '_ {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.unread)
        .map(|row| row.id.clone())
}

/// The provider kind whose block the dashboard shows: the manual tab pick while
/// its panel is still on the dashboard, else the selection-derived kind
/// ([`selected_agent_kind`]) when a panel exists for it, else the first panel.
/// `None` only when the dashboard is empty.
#[cfg(test)]
pub(crate) fn active_provider_kind(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    active_dashboard_tab(snapshot, ui)
}

pub(crate) fn active_dashboard_tab(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<String> {
    let panels = &snapshot.providers;
    let has_panel = |kind: &str| panels.iter().any(|panel| panel.kind == kind);
    if let Some(tab) = &ui.dashboard_tab
        && dashboard_has_tab(snapshot, &tab.kind)
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

pub(crate) fn active_dashboard_block_rows(snapshot: &SidebarSnapshot, ui: &UiState) -> Option<u16> {
    let active_kind = active_dashboard_tab(snapshot, ui)?;
    snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == active_kind)
        .map(|panel| sections::provider_dashboard_block_rows(panel, snapshot.value_tally.as_ref()))
        .and_then(|rows| u16::try_from(rows).ok())
}

pub(crate) fn dashboard_tabs(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .providers
        .iter()
        .map(|panel| panel.kind.clone())
        .collect::<Vec<_>>()
}

fn dashboard_has_tab(snapshot: &SidebarSnapshot, kind: &str) -> bool {
    snapshot.providers.iter().any(|panel| panel.kind == kind)
}

/// Whether the dashboard paints a tab rail. Pets keep the dashboard tabbed so
/// the pet overlay rides one provider block at a time; without pets, a single
/// provider keeps the historical bare block.
pub(crate) fn dashboard_tabbed(snapshot: &SidebarSnapshot) -> bool {
    if snapshot.pets.enabled {
        return true;
    }
    snapshot
        .theme
        .display
        .provider_tabs
        .tabs(snapshot.providers.len())
}

pub(crate) fn dashboard_present(snapshot: &SidebarSnapshot, alert_active: bool) -> bool {
    !alert_active && (!snapshot.providers.is_empty() || snapshot.pets.enabled)
}

pub(crate) fn pet_body_enabled(snapshot: &SidebarSnapshot) -> bool {
    Theme::for_sidebar(&snapshot.theme).pet_body_enabled()
}

pub(crate) fn pet_motion_enabled(snapshot: &SidebarSnapshot, action: PetAction) -> bool {
    let animations = &snapshot.theme.animations;
    let spec = match action {
        PetAction::Idle => animations.idle.as_ref(),
        PetAction::Thinking => animations.thinking.as_ref(),
        PetAction::Running => animations.working.as_ref(),
        PetAction::Waiting => animations.delegating.as_ref(),
        PetAction::Review => animations.compacting.as_ref(),
        PetAction::Ask => animations.waiting.as_ref(),
        PetAction::Failed => animations.failed.as_ref(),
    };
    spec_needs_motion(spec)
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
