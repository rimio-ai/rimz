//! Ratatui rendering for the sidebar snapshot model.
//!
//! `draw_to_terminal` is the live entry point; `render_fixed` is the
//! offscreen variant used by the vt100-backed snapshot tests. Section
//! composition lives in `sections`; vocabulary labels in `labels`;
//! pure formatting helpers in `fmt`.
//!
//! `draw_to_terminal` takes an optional `Alert` alongside the snapshot; the
//! fixed renders carry none. The alert is the sticky health line pinned to the
//! bottom of the sidebar: while the refresh loop is unhealthy it shows the
//! reason and elapsed time, and after recovery it lingers as a dismissable
//! "last alert" notice. This is the reload-recovery contract documented in
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

use self::animation::{AnimationCadence, animation_cadence};
use self::ansi::{infallible, write_buffer_line_ansi};
use self::chrome::{hairline_rule, help_lines};
#[cfg(test)]
pub(in crate::sidebar_pane) use self::compose::compose_lines;
use self::compose::compose_lines_with_meter;
#[cfg(test)]
use self::compose::lead_unread;
#[cfg(test)]
use self::compose::{
    auto_scroll_reveal_group, auto_scroll_to_selection, build_bottom_chrome, scroll_thumb,
};
use self::interaction::RenderedBlock;
pub(in crate::sidebar_pane) use self::interaction::{FrameInteractions, HitRegion, HitTarget};
pub(in crate::sidebar_pane) use self::sections::agent_card_cost_usd;
pub(in crate::sidebar_pane) use self::ui_state::cockpit_spend_target;
pub(in crate::sidebar_pane) use self::ui_state::{Alert, UiState};
pub(in crate::sidebar_pane) use self::ui_state::{
    Browse, DashboardTab, FrozenOrder, FrozenRow, GateNotice, ManualScroll, OrderHold,
};
pub(in crate::sidebar_pane) use crate::sidebar_pane::view::BodyFilter;
use odometer::CLICK_PHASES;
pub(in crate::sidebar_pane) use odometer::{CostRolls, TallyAnim};
pub(in crate::sidebar_pane) use scrollbar::ScrollbarFade;

use std::io::{self, Write};
use std::num::NonZeroU16;
use std::time::Duration;

use crate::agents::{AgentStatus, TurnPhase};
#[cfg(any(test, feature = "testkit"))]
use crate::config::GlyphRole;
use crate::config::{AnimationRole, CardDensityMode};
use crate::sidebar_pane::pets::PetAction;
use crate::sidebar_pane::view::VisibleRoster;
use crate::store::snapshot::{ProcessState, SidebarRow, SidebarSnapshot};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend};
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use self::animation::ResolvedAnimations;
use self::sections::DashboardMode;
use self::theme::Theme;

#[cfg(test)]
fn age_heat_amount_for_test(age_secs: i64, ceiling_secs: i64) -> f32 {
    let first_quarter = ceiling_secs / 4;
    debug_assert!(age_secs > first_quarter);
    let heat_span = ceiling_secs - first_quarter;
    ((age_secs - first_quarter) as f32 / heat_span as f32).min(1.0)
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
    prune_delegation_state(snapshot, ui);
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
            crate::config::SidebarConfig::key_label(&snapshot.sidebar.focus_key),
            crate::config::SidebarConfig::key_label(&snapshot.sidebar.zoom_key),
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
    zoom_key: Option<&str>,
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

    let lines = help_lines(theme, focus_key, zoom_key, keys, usize::from(area.width));
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

#[cfg(any(test, feature = "testkit"))]
pub(in crate::sidebar_pane) struct GalleryColumn<'a> {
    pub(in crate::sidebar_pane) snapshot: &'a SidebarSnapshot,
    pub(in crate::sidebar_pane) ui: &'a mut UiState,
}

/// Terminal width above which the gallery packs a fourth fixture column.
#[cfg(any(test, feature = "testkit"))]
const GALLERY_FOUR_COLUMN_MIN_WIDTH: u16 = 240;

/// Returns the number of gallery columns that fit at the launch width.
#[cfg(any(test, feature = "testkit"))]
pub(in crate::sidebar_pane) fn gallery_column_cap(width: u16) -> usize {
    if width > GALLERY_FOUR_COLUMN_MIN_WIDTH {
        4
    } else {
        3
    }
}

#[cfg(any(test, feature = "testkit"))]
pub(in crate::sidebar_pane) fn draw_gallery_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    columns: &mut [GalleryColumn<'_>],
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_gallery(frame, columns))
        .map(|_| ())
}

#[cfg(any(test, feature = "testkit"))]
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

#[cfg(any(test, feature = "testkit"))]
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

/// An override lives as long as its row; the `+K older` history also expires
/// when the parent's user turn moves.
fn prune_delegation_state(snapshot: &SidebarSnapshot, ui: &mut UiState) {
    let agent = |id: &str| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .find(|row| row.id == id)
            .and_then(SidebarRow::as_agent)
    };
    ui.delegation_overrides.retain(|id, _| agent(id).is_some());
    ui.delegation_history
        .retain(|id, turn| agent(id).is_some_and(|agent| agent.user_turn_started_at == *turn));
}

fn selected_row<'a>(snapshot: &'a SidebarSnapshot, ui: &UiState) -> Option<&'a SidebarRow> {
    ui.visible_roster(snapshot).row(ui.selected_index)
}

/// Selection expands a bare, not-yet-prompted idle card whose compose
/// affordance needs the breath animation grid. This can be the selected row
/// itself or a visible named teammate expanded alongside it.
fn expanded_row_awaiting_first_prompt(snapshot: &SidebarSnapshot, ui: &UiState) -> bool {
    let roster = ui.visible_roster(snapshot);
    roster.groups().iter().any(|group| {
        group
            .range()
            .zip(group.rows(&roster).iter().copied())
            .any(|(row_index, row)| {
                sections::row_selection_reach(&roster, group, row_index, ui.selected_index)
                    .opens_card()
                    && sections::awaiting_first_prompt_affordance(row)
            })
    })
}

/// Whether a visible card's open delegation section holds something in motion:
/// a live child's head or a running shell job's working animation. Either
/// needs the fast grid whatever the parent's own status, so a sleeping parent
/// waiting on its children still animates them.
fn visible_delegation_motion(snapshot: &SidebarSnapshot, ui: &UiState) -> bool {
    let roster = ui.visible_roster(snapshot);
    let density = snapshot.theme.display.card_density;
    roster.groups().iter().any(|group| {
        group
            .range()
            .zip(group.rows(&roster).iter().copied())
            .any(|(row_index, row)| {
                let expansion = sections::CardExpansion::resolve(
                    &ui.delegation_overrides,
                    &ui.delegation_history,
                    density,
                    row,
                    sections::row_selection_reach(&roster, group, row_index, ui.selected_index),
                );
                sections::delegation_motion(row, density, expansion)
            })
    })
}

/// The provider kind the dashboard's tab focus derives from the selection: the
/// selected row's agent kind (agent rows carry the kind in `SidebarRow::name`),
/// or `None` for a process row or an empty room — the caller falls back to the
/// first tab. Reads the same filtered universe `selected_index` is an ordinal
/// of, so the dashboard's follow-the-selection stays honest under a make-up
/// filter.
pub(in crate::sidebar_pane) fn selected_agent_kind(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
) -> Option<crate::ids::AgentKind> {
    selected_row(snapshot, ui)
        .filter(|row| row.is_agent())
        .map(|row| crate::ids::AgentKind::new_unchecked(&row.name))
}

pub(in crate::sidebar_pane) fn selected_pet_action(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
) -> PetAction {
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
                .any(|child| child.holds_parent_turn())
        {
            return PetAction::Waiting;
        }
        return match (status, agent.phase) {
            (AgentStatus::Running, TurnPhase::Reasoning) => PetAction::Thinking,
            (AgentStatus::Running, _) => PetAction::Running,
            (AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping, _) => {
                PetAction::Idle
            }
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

pub(in crate::sidebar_pane) fn unread_pet_row_ids(
    snapshot: &SidebarSnapshot,
) -> impl Iterator<Item = String> + '_ {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.unread)
        .map(|row| row.id.clone())
}

/// The provider block the dashboard shows: the manual tab pick while its
/// panel is still on the dashboard, else the block of the live
/// selection-derived kind ([`selected_agent_kind`]), whichever account the
/// card runs on, else the block of the last agent kind the dashboard followed
/// while one is still present, else the first panel. `None` only when the
/// dashboard is empty.
pub(in crate::sidebar_pane) fn active_dashboard_tab(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
) -> Option<crate::ids::LoginKey> {
    let panels = &snapshot.providers;
    if let Some(tab) = &ui.dashboard_tab
        && dashboard_has_tab(snapshot, &tab.login)
    {
        return Some(tab.login.clone());
    }
    selected_agent_kind(snapshot, ui)
        .and_then(|kind| dashboard_tab_of_kind(snapshot, &kind))
        .or_else(|| {
            ui.last_agent_kind
                .as_ref()
                .and_then(|kind| dashboard_tab_of_kind(snapshot, kind))
        })
        .or_else(|| panels.first().map(|panel| panel.login_key()))
}

/// The block a card of `kind` follows to.
pub(in crate::sidebar_pane) fn dashboard_tab_of_kind(
    snapshot: &SidebarSnapshot,
    kind: &crate::ids::AgentKind,
) -> Option<crate::ids::LoginKey> {
    snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == kind.as_str())
        .map(|panel| panel.login_key())
}

pub(in crate::sidebar_pane) fn dashboard_tabs(
    snapshot: &SidebarSnapshot,
) -> Vec<crate::ids::LoginKey> {
    snapshot
        .providers
        .iter()
        .map(|panel| panel.login_key())
        .collect::<Vec<_>>()
}

fn dashboard_has_tab(snapshot: &SidebarSnapshot, login: &crate::ids::LoginKey) -> bool {
    snapshot
        .providers
        .iter()
        .any(|panel| panel.login_key() == *login)
}

/// Whether the dashboard paints a tab rail. Pets keep the dashboard tabbed so
/// the pet overlay rides one provider block at a time; without pets, a single
/// provider keeps the historical bare block.
pub(in crate::sidebar_pane) fn dashboard_tabbed(snapshot: &SidebarSnapshot) -> bool {
    dashboard_mode(snapshot) != DashboardMode::Stacked
}

fn dashboard_mode(snapshot: &SidebarSnapshot) -> DashboardMode {
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

fn dashboard_present(snapshot: &SidebarSnapshot, alert_active: bool) -> bool {
    !alert_active && (!snapshot.providers.is_empty() || snapshot.theme.pets.enabled)
}

pub(in crate::sidebar_pane) fn pets_on_dashboard(
    snapshot: &SidebarSnapshot,
    alert_active: bool,
) -> bool {
    snapshot.theme.pets.enabled && dashboard_present(snapshot, alert_active)
}

pub(in crate::sidebar_pane) fn pet_motion_enabled(
    animations: &ResolvedAnimations,
    action: PetAction,
) -> bool {
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

/// The repaint grid while something on screen moves, `None` when nothing does.
/// `phase` is the observed animation phase, which the serve loop may take from
/// the wall clock ahead of `ui.animation_phase`.
pub(in crate::sidebar_pane) fn animation_interval(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    phase: u64,
    alert_active: bool,
) -> Option<Duration> {
    let refresh_ms = snapshot.theme.display.resolved_refresh_ms();
    let base = crate::sidebar::timing::animation_frame(refresh_ms);
    let Some(theme) = ui.cached_theme(&snapshot.theme) else {
        return Some(base);
    };
    // A scrollbar fade needs the fast grid to read as motion; it is brief and
    // self-terminating, so the cost is bounded to the settle window. Continuous
    // row pulse rides the breath cadence below.
    if ui.help_visible || ui.scrollbar.fading(phase) {
        return Some(base);
    }
    let cadence = animation_cadence(snapshot, &theme.animations);
    if visible_delegation_motion(snapshot, ui) || cadence == AnimationCadence::Fast {
        return Some(base);
    }
    // The money rolls click once per `CLICK_PHASES` phases, so a rolling room
    // samples on the matching money grid — one paint per distinct click, and
    // the one-click settle flash can never fall between samples. A fast room
    // (a working spinner) keeps the fast grid; the roll's painted value simply
    // holds across the extra frames. A slow-cadence room drops to the money
    // grid while a climb is in flight — the cosmetic breath repaints
    // idempotently, and the climb window bounds the extra paints.
    let money_rolling = ui.tally.any_rolling(phase) || ui.cost_rolls.any_rolling(phase);
    let money_grid = || crate::sidebar::timing::money_animation_frame(refresh_ms, CLICK_PHASES);
    // The dashboard pet paints on its track cadence, but a money climb in the
    // still-visible cockpit must keep sampling on the money grid, so a rolling
    // room takes the faster of the two.
    if pets_on_dashboard(snapshot, alert_active)
        && let Some(pet_interval) = ui.pet.as_ref().and_then(|pet| pet.frame_interval)
    {
        return Some(if money_rolling {
            pet_interval.min(money_grid())
        } else {
            pet_interval
        });
    }
    if money_rolling {
        return Some(money_grid());
    }
    if cadence == AnimationCadence::Breath || expanded_row_awaiting_first_prompt(snapshot, ui) {
        return Some(crate::sidebar::timing::breath_animation_frame(refresh_ms));
    }
    None
}

pub(in crate::sidebar_pane) fn draw_to_terminal<B: Backend>(
    terminal: &mut Terminal<B>,
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &mut UiState,
) -> Result<(), B::Error> {
    terminal
        .draw(|frame| draw_into(frame, snapshot, alert, ui, frame.area()))
        .map(|_| ())
}

pub fn render_fixed<W: Write>(
    writer: W,
    snapshot: &SidebarSnapshot,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = CrosstermBackend::new(writer);
    let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
    Backend::clear_region(terminal.backend_mut(), ClearType::All)?;
    draw_to_terminal(&mut terminal, snapshot, None, &mut UiState::default())?;
    Ok(())
}

pub fn render_fixed_line_ansi<W: Write>(
    mut writer: W,
    snapshot: &SidebarSnapshot,
    width: u16,
    height: u16,
) -> io::Result<()> {
    let backend = TestBackend::new(width, height);
    let mut terminal = infallible(Terminal::new(backend));
    infallible(terminal.clear());
    infallible(draw_to_terminal(
        &mut terminal,
        snapshot,
        None,
        &mut UiState::default(),
    ));
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
        delegation_history: snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .filter_map(|row| {
                row.as_agent()
                    .filter(|agent| agent.sub_agent_count > 0)
                    .map(|agent| (row.id.clone(), agent.user_turn_started_at))
            })
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
    infallible(draw_to_terminal(&mut terminal, &snapshot, None, &mut ui));
    write_buffer_line_ansi(&mut writer, terminal.backend().buffer())
}

#[cfg(test)]
mod tests;
