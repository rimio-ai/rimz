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
mod fmt;
mod labels;
mod layout;
mod odometer;
mod oklab;
pub mod scheme;
mod scrollbar;
mod sections;
mod theme;
mod ui_state;

pub(crate) use self::animation::{AnimationCadence, animation_cadence};
use self::ansi::{infallible, write_buffer_line_ansi};
use self::chrome::{hairline_rule, help_lines};
#[cfg(test)]
pub(crate) use self::compose::compose_lines;
use self::compose::compose_lines_with_meter;
#[cfg(test)]
use self::compose::lead_unread;
#[cfg(test)]
use self::compose::{
    auto_scroll_reveal_group, auto_scroll_to_selection, build_bottom_chrome, scroll_thumb,
};
pub(crate) use self::ui_state::MoreHit;
pub use self::ui_state::{Alert, UiState};
pub(crate) use self::ui_state::{
    BodyFilter, Browse, DashboardTab, FrozenOrder, FrozenRow, GateNotice, ManualScroll, OrderHold,
};
pub(crate) use odometer::{CLICK_PHASES, CostRolls, TallyAnim};
pub(crate) use scrollbar::ScrollbarFade;

use std::collections::HashSet;
use std::io::{self, Write};

use crate::agents::AgentStatus;
use crate::agents::TurnPhase;
use crate::config::{AnimationRole, GlyphRole};
use crate::sidebar_pane::pets::PetAction;
use crate::{ProcessState, SidebarRow, SidebarSnapshot};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend};
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use self::animation::ResolvedAnimations;
#[cfg(test)]
pub(crate) use self::sections::{MakeUpHit, ProviderTabHit};
pub(crate) use self::sections::{status_total, unread_total};
use self::theme::Theme;

#[cfg(test)]
fn age_heat_amount_for_test(age_secs: i64) -> f32 {
    let first_quarter = crate::agents::ATTENTION_AGE_CEILING_SECS / 4;
    debug_assert!(age_secs > first_quarter);
    let heat_span = crate::agents::ATTENTION_AGE_CEILING_SECS - first_quarter;
    ((age_secs - first_quarter) as f32 / heat_span as f32).min(1.0)
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &mut UiState::default());
}

/// Resolve the glyph set under `theme`, so non-sidebar surfaces honor the same
/// `[theme] style` / `[theme.glyphs]` config the sidebar reads.
pub fn theme_glyphs(
    theme: &crate::config::ThemeConfig,
) -> impl Fn(crate::config::GlyphRole) -> String {
    let glyphs = theme::GlyphSet::from_theme(theme);
    move |role| glyphs.glyph(role).to_owned()
}

pub fn nerd_font_probe_glyphs() -> [&'static str; 8] {
    theme::nerd_font_probe_glyphs()
}

pub fn nerd_font_probe_gradient(width: usize) -> Vec<(u8, u8, u8)> {
    theme::nerd_font_probe_gradient(width)
}

pub fn draw_with_ui(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) {
    draw_into(frame, snapshot, alert, ui, frame.area());
}

fn draw_into(
    frame: &mut Frame<'_>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
    area: Rect,
) {
    // Borderless: the sidebar already sits inside a framed mux pane, so a second
    // 4-sided border double-frames it and eats two precious columns. The body
    // fills the whole area; a title line and faint hairline rules carry the
    // structure the border used to.
    //
    // The composed maps and the resolved scroll offset are byproducts of the
    // draw: store them so the mouse hit-test and the next frame's viewport read
    // the geometry of the frame the user is actually looking at.
    prune_expanded_groups(snapshot, ui);
    let theme = ui.theme(&snapshot.theme);
    let mut meter_pixels = ui.meter_pixels.take();
    let composed = compose_lines_with_meter(
        snapshot,
        alert,
        ui,
        theme.as_ref(),
        area.width,
        area.height,
        meter_pixels.as_mut(),
    );
    ui.meter_pixels = meter_pixels;
    let top_height = composed.top_height;
    let bottom_height = composed.bottom_height;
    ui.line_map = composed.line_map;
    ui.tab_hits = composed.tab_hits;
    ui.make_up_hits = composed.make_up_hits;
    ui.more_hits = composed.more_hits;
    ui.banner_line = composed.banner_line;
    ui.scrollbar
        .observe(composed.scroll_offset, ui.animation_phase);
    ui.scroll_offset = composed.scroll_offset;
    // One paint consumes the focus reveal; later folds with unchanged selection
    // leave the viewport free to follow the card or scroll by hand.
    ui.focus_group_reveal = false;
    let paragraph = Paragraph::new(Text::from(composed.lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
    if ui.help_visible {
        draw_help_overlay(
            frame,
            theme.as_ref(),
            snapshot.sidebar.focus_key_label(),
            &snapshot.sidebar.keys,
            area,
            (top_height, bottom_height),
        );
    }
}

fn draw_help_overlay(
    frame: &mut Frame<'_>,
    theme: &Theme,
    focus_key: Option<&str>,
    keys: &crate::config::SidebarKeys,
    area: Rect,
    chrome_heights: (usize, usize),
) {
    if area.width == 0 {
        return;
    }

    let area_bottom = area.bottom();
    let (top_height, bottom_height) = chrome_heights;
    let region_top = area
        .y
        .saturating_add(top_height.min(usize::from(u16::MAX)) as u16)
        .min(area_bottom);
    let mut region_bottom = area_bottom
        .saturating_sub(bottom_height.min(usize::from(u16::MAX)) as u16)
        .min(area_bottom);
    if region_bottom < region_top {
        region_bottom = region_top;
    }
    let region_h = region_bottom.saturating_sub(region_top);
    if region_h == 0 {
        return;
    }

    let lines = help_lines(theme, focus_key, keys, usize::from(area.width));
    let box_w = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width)) as u16;
    let box_h = (lines.len() as u16).min(region_h);
    if box_w == 0 || box_h == 0 {
        return;
    }

    let x = area.right().saturating_sub(box_w).max(area.x);
    let y = region_bottom.saturating_sub(box_h).max(region_top);
    let width = box_w.min(area.right().saturating_sub(x));
    if width == 0 {
        return;
    }

    let rect = Rect {
        x,
        y,
        width,
        height: box_h,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(Text::from(lines)), rect);
}

pub struct GalleryColumn<'a> {
    pub snapshot: &'a SidebarSnapshot,
    pub ui: &'a mut UiState,
}

pub fn draw_gallery_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    columns: &mut [GalleryColumn<'_>],
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_gallery(frame, columns))
        .map(|_| ())
}

fn draw_gallery(frame: &mut Frame<'_>, columns: &mut [GalleryColumn<'_>]) {
    let area = frame.area();
    let (column_areas, delimiter_xs) = gallery_layout(area, columns.len());
    for (column, column_area) in columns.iter_mut().zip(column_areas) {
        draw_into(frame, column.snapshot, None, column.ui, column_area);
    }
    let Some(first) = columns.first_mut() else {
        return;
    };
    let theme = first.ui.theme(&first.snapshot.theme);
    let style = theme.rule();
    let delimiter = theme.glyph(GlyphRole::ChromeBoxVertical).to_owned();
    let buffer = frame.buffer_mut();
    for x in delimiter_xs {
        for y in area.y..area.bottom() {
            buffer[(x, y)].set_symbol(&delimiter).set_style(style);
        }
    }
}

fn gallery_layout(area: Rect, column_count: usize) -> (Vec<Rect>, Vec<u16>) {
    if column_count == 0 || area.width == 0 {
        return (Vec::new(), Vec::new());
    }
    let count = column_count.min(usize::from(u16::MAX)) as u16;
    let delimiter_count = count.saturating_sub(1);
    let available_width = area.width.saturating_sub(delimiter_count);
    let base_width = available_width / count;
    let mut remainder = available_width % count;
    let mut x = area.x;
    let mut columns = Vec::with_capacity(column_count);
    let mut delimiters = Vec::with_capacity(column_count.saturating_sub(1));

    for index in 0..column_count {
        let mut width = base_width;
        if remainder > 0 {
            width = width.saturating_add(1);
            remainder -= 1;
        }
        columns.push(Rect::new(x, area.y, width, area.height));
        x = x.saturating_add(width);
        if index + 1 < column_count && x < area.right() {
            delimiters.push(x);
            x = x.saturating_add(1);
        }
    }
    (columns, delimiters)
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

/// Maximum calm rows painted before overflow moves behind `+K more`.
pub const WORKTREE_ROW_CAP: usize = 6;

/// The rows a worktree group paints and the selection model can browse.
///
/// With a make-up filter active, every matching row passes: the cockpit bucket
/// count and the narrowed body stay exact, ignoring any held visibility set.
/// With an expanded group, the full roster passes. A finished group otherwise
/// collapses every row except the focused pane and rows held from the previous
/// order. For active groups the calm idle/process tail trims to
/// [`WORKTREE_ROW_CAP`], always keeping unread rows, non-idle agent rows, and
/// the focused pane. Inactive success rows still stay visible so a renderer
/// never drops an unread stamp before receipts converge; sticky unread idle
/// rows stay visible until the human reads them, and the first live process row
/// stays visible when it is the group's only live member, so capping never
/// turns a live shell's group into an inactive one. An active order hold unions
/// in rows painted in the frozen frame, so cap exemptions settle together with
/// the held order. Ordinary inactive idle rows are the first calm rows hidden
/// behind `+K more`.
pub(crate) fn group_visible_rows<'a>(
    group: &'a crate::SidebarWorktreeGroup,
    filter: Option<BodyFilter>,
    expanded: bool,
    held: Option<&HashSet<String>>,
) -> Vec<&'a SidebarRow> {
    if filter.is_some() {
        return group
            .rows
            .iter()
            .filter(|row| row_passes_filter(row, filter))
            .collect();
    }
    if expanded {
        return group.rows.iter().collect();
    }
    if group.finished {
        return group
            .rows
            .iter()
            .filter(|row| {
                row.pane.as_ref().is_some_and(|pane| pane.is_focused)
                    || held.is_some_and(|ids| ids.contains(&row.id))
            })
            .collect();
    }

    capped_visible_rows(&group.rows, held)
}

/// The rows that survive the calm-tail cap for one worktree group's roster.
///
/// See [`group_visible_rows`] for the full selection model; this is the
/// no-filter, no-expand branch, shared with the sidebar fixture so the
/// visibility rule has one home.
pub fn capped_visible_rows<'a>(
    rows: &'a [SidebarRow],
    held: Option<&HashSet<String>>,
) -> Vec<&'a SidebarRow> {
    let process_is_only_live_member = rows.iter().map(row_band).min() == Some(0)
        && rows
            .iter()
            .filter(|row| row_band(row) == 0)
            .all(SidebarRow::is_process);
    let liveness_process_id = if process_is_only_live_member {
        rows.iter()
            .find(|row| row.is_process() && row_band(row) == 0)
            .map(|row| row.id.as_str())
    } else {
        None
    };

    let mut visible = Vec::new();
    for row in rows {
        if row.unread
            || row
                .status()
                .is_some_and(|status| status != AgentStatus::Idle)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || liveness_process_id == Some(row.id.as_str())
            || held.is_some_and(|ids| ids.contains(&row.id))
            || visible.len() < WORKTREE_ROW_CAP
        {
            visible.push(row);
        }
    }
    visible
}

fn row_band(row: &SidebarRow) -> u8 {
    if row.archived {
        2
    } else if row.inactive {
        1
    } else {
        0
    }
}

fn group_has_hidden_tail(group: &crate::SidebarWorktreeGroup) -> bool {
    group_visible_rows(group, None, false, None).len() < group.rows.len()
}

fn prune_expanded_groups(snapshot: &SidebarSnapshot, ui: &mut UiState) {
    ui.expanded_groups.retain(|key| {
        snapshot
            .worktree_groups
            .iter()
            .any(|group| group.key == *key && group_has_hidden_tail(group))
    });
}

fn selected_row<'a>(snapshot: &'a SidebarSnapshot, ui: &UiState) -> Option<&'a SidebarRow> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| {
            group_visible_rows(
                group,
                ui.make_up_filter,
                ui.expanded_groups.contains(&group.key),
                ui.held_visible(),
            )
        })
        .nth(ui.selected_index)
}

/// The selected row is a bare, not-yet-prompted idle card whose selected form
/// animates the compose affordance.
pub(crate) fn selection_awaiting_first_prompt(snapshot: &SidebarSnapshot, ui: &UiState) -> bool {
    selected_row(snapshot, ui).is_some_and(sections::awaiting_first_prompt_affordance)
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
        let status = agent.status;
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
/// its panel is still on the dashboard, else the live selection-derived kind
/// ([`selected_agent_kind`]) when a panel exists for it, else the last agent
/// kind the dashboard followed while its panel is still present, else the
/// first panel. `None` only when the dashboard is empty.
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
    if let Some(kind) = &ui.last_agent_kind
        && has_panel(kind)
    {
        return Some(kind.clone());
    }
    panels.first().map(|panel| panel.kind.clone())
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
    if snapshot.theme.pets.enabled {
        return true;
    }
    snapshot
        .theme
        .display
        .provider_tabs
        .tabs(snapshot.providers.len())
}

pub(crate) fn dashboard_present(snapshot: &SidebarSnapshot, alert_active: bool) -> bool {
    !alert_active && (!snapshot.providers.is_empty() || snapshot.theme.pets.enabled)
}

pub(crate) fn pet_body_enabled(_snapshot: &SidebarSnapshot) -> bool {
    !crate::tui::no_color()
}

pub(crate) fn pet_motion_enabled(animations: &ResolvedAnimations, action: PetAction) -> bool {
    let role = match action {
        PetAction::Idle => AnimationRole::Idle,
        PetAction::Thinking => AnimationRole::Thinking,
        PetAction::Running => AnimationRole::Working,
        PetAction::Waiting => AnimationRole::Delegating,
        PetAction::Review => AnimationRole::Compacting,
        PetAction::Ask => AnimationRole::Waiting,
        PetAction::Failed => AnimationRole::Failed,
    };
    !animations.role(role).motion_quieted()
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
    Backend::clear_region(terminal.backend_mut(), ClearType::All)?;
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
