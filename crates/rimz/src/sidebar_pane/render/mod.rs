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
mod interaction;
mod labels;
mod layout;
mod odometer;
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
pub(crate) use self::interaction::{FrameInteractions, HitRegion, HitTarget, RenderedBlock};
pub use self::ui_state::{Alert, UiState};
pub(crate) use self::ui_state::{
    Browse, DashboardTab, FrozenOrder, FrozenRow, GateNotice, ManualScroll, OrderHold,
    cockpit_spend_target,
};
pub(crate) use crate::sidebar_pane::view::BodyFilter;
pub(crate) use odometer::{CLICK_PHASES, CostRolls, TallyAnim};
pub(crate) use scrollbar::ScrollbarFade;

use std::io::{self, Write};
use std::num::NonZeroU16;

use crate::agents::AgentStatus;
use crate::agents::TurnPhase;
use crate::config::{AnimationRole, CardDensityMode, GlyphRole};
use crate::sidebar_pane::pets::PetAction;
use crate::sidebar_pane::view::VisibleRoster;
use crate::{ProcessState, SidebarRow, SidebarSnapshot};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use self::animation::ResolvedAnimations;
pub(crate) use self::sections::DashboardMode;
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
    ui.interactions = composed.interactions;
    ui.scrollbar
        .observe(composed.scroll_offset, ui.animation_phase);
    ui.scroll_offset = composed.scroll_offset;
    // One paint consumes the focus reveal; later folds with unchanged selection
    // leave the viewport free to follow the card or scroll by hand.
    ui.focus_group_reveal = false;
    let paragraph = Paragraph::new(Text::from(composed.lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
    paint_hyperlinks(frame, &ui.interactions, area);
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

fn paint_hyperlinks(frame: &mut Frame<'_>, interactions: &FrameInteractions, area: Rect) {
    for (rows, columns, url) in interactions.hyperlinks() {
        let url = crate::osc::osc_text(url);
        if url.is_empty() {
            continue;
        }
        for row in rows.clone() {
            let Ok(row) = u16::try_from(row) else {
                continue;
            };
            if row >= area.height {
                continue;
            }
            for column in columns.clone() {
                if column >= area.width {
                    continue;
                }
                let Some(cell) = frame
                    .buffer_mut()
                    .cell_mut((area.x.saturating_add(column), area.y.saturating_add(row)))
                else {
                    continue;
                };
                if cell.diff_option == CellDiffOption::Skip || cell.symbol().is_empty() {
                    continue;
                }
                let symbol = cell.symbol().to_owned();
                cell.set_symbol(&format!("\x1b]8;;{url}\x1b\\{symbol}\x1b]8;;\x1b\\"));
                cell.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
            }
        }
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

/// Terminal width above which the gallery packs a fourth fixture column.
const GALLERY_FOUR_COLUMN_MIN_WIDTH: u16 = 240;

/// Returns the number of gallery columns that fit at the launch width.
pub fn gallery_column_cap(width: u16) -> usize {
    if width > GALLERY_FOUR_COLUMN_MIN_WIDTH {
        4
    } else {
        3
    }
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

fn prune_expanded_groups(snapshot: &SidebarSnapshot, ui: &mut UiState) {
    let roster = VisibleRoster::baseline(snapshot);
    ui.expanded_groups.retain(|key| {
        roster
            .groups()
            .iter()
            .any(|group| group.source().key == *key && group.natural_hidden_count() > 0)
    });
}

fn selected_row<'a>(snapshot: &'a SidebarSnapshot, ui: &UiState) -> Option<&'a SidebarRow> {
    VisibleRoster::new(
        snapshot,
        ui.make_up_filter,
        &ui.expanded_groups,
        ui.held_visible(),
    )
    .row(ui.selected_index)
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
            && agent
                .sub_agents
                .iter()
                .any(|child| child.status == AgentStatus::Running)
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
    dashboard_mode(snapshot) != DashboardMode::Stacked
}

pub(crate) fn dashboard_mode(snapshot: &SidebarSnapshot) -> DashboardMode {
    if snapshot.theme.pets.enabled {
        return DashboardMode::Pet;
    }
    if snapshot
        .theme
        .display
        .provider_tabs
        .tabs(snapshot.providers.len())
    {
        DashboardMode::Tabbed
    } else {
        DashboardMode::Stacked
    }
}

pub(crate) fn dashboard_present(snapshot: &SidebarSnapshot, alert_active: bool) -> bool {
    !alert_active && (!snapshot.providers.is_empty() || snapshot.theme.pets.enabled)
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

/// Render the one-shot `rimz sidebar frame --expand` view as line-oriented ANSI.
///
/// Every card uses expanded density, every worktree group bypasses its row cap,
/// and the offscreen viewport grows to the full composed height.
pub fn render_expanded_line_ansi<W: Write>(
    mut writer: W,
    snapshot: &SidebarSnapshot,
    width: u16,
) -> io::Result<()> {
    let mut snapshot = snapshot.clone();
    snapshot.theme.display.card_density = CardDensityMode::Expanded;
    let mut ui = UiState {
        expanded_groups: snapshot
            .worktree_groups
            .iter()
            .map(|group| group.key.clone())
            .collect(),
        ..UiState::default()
    };
    let theme = ui.theme(&snapshot.theme);
    let composed =
        compose_lines_with_meter(&snapshot, None, &ui, theme.as_ref(), width, u16::MAX, None);
    let height =
        u16::try_from(composed.content_height.clamp(1, usize::from(u16::MAX))).unwrap_or(u16::MAX);

    let backend = TestBackend::new(width, height);
    let mut terminal = infallible(Terminal::new(backend));
    infallible(terminal.clear());
    infallible(draw_to_terminal_with_ui(
        &mut terminal,
        &snapshot,
        None,
        &mut ui,
    ));
    write_buffer_line_ansi(&mut writer, terminal.backend().buffer())
}

#[cfg(test)]
mod tests;
