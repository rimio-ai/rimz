//! Post-render transition effects — the truecolor "garnish" tier.
//!
//! After the paragraph renders, this pass mutates buffer cell *colors* in
//! place: brief one-shot flashes mark state transitions — a card entering
//! `waiting`/`failed`, an ask resolving, a paused row lifting, a new card
//! appearing, the spine lighting under a fresh selection. Color only, never a
//! glyph: the composed text is untouched, so the golden frames and the
//! `NO_COLOR` grammar cannot drift (locked by the
//! `effects_pass_never_changes_the_composed_text` golden guard).
//!
//! The pass runs only when [`Theme::effects_enabled`] clears it — the
//! `[sidebar] glow` mode over the terminal's 24-bit advertisement. The
//! continuous row pulse is owned by base composition; this pass only animates
//! the moment of change and decay, and a calm room paints nothing here.
//!
//! Geometry re-resolves every frame from `UiState::line_map` (the hit-test
//! map, the renderer's one row-geometry authority), so an effect follows its
//! row through ranking reorders and scrolling and simply drops when the row
//! leaves the viewport. Time advances by `animation_phase` deltas — never the
//! wall clock — so every effect is deterministic under a pinned phase.

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use crate::feed::AgentStatus;
use crate::ids::PaneId;
use crate::{SidebarRow, SidebarSnapshot};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tachyonfx::pattern::SweepPattern;
use tachyonfx::{CellFilter, Effect, Interpolation, fx};

use super::theme::{Component, Theme};
use super::{BodyFilter, row_passes_filter};

/// Cap on the elapsed time fed into a one-shot per painted frame. A calm room
/// paints rarely, so a raw phase delta can span seconds; clamping means a
/// flash spawned after a quiet stretch still plays out over visible frames
/// instead of expiring inside its first one.
const MAX_STEP_MS: u64 = crate::sidebar::timing::EFFECT_MAX_STEP_MS;

const FLASH_ENTERED_MS: u32 = 250;
const FLASH_RESOLVED_MS: u32 = 300;
const FLASH_LIFTED_MS: u32 = 400;
const FLASH_SELECTED_MS: u32 = 180;
const FLASH_MATERIALIZE_MS: u32 = 250;

/// The sweep gradient span, in columns, for a card flash's wipe-in lead: the
/// width of the soft leading edge as the flash tone enters from the spine. A
/// few cells reads as cast light travelling in, not a hard bar; a card narrower
/// than the span just resolves the wipe more gently.
const SWEEP_SPAN: u16 = 6;

/// The state-transition cues the observer spawns. Each is a one-shot: it
/// plays once over its row and expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionKind {
    EnteredWaiting,
    EnteredFailed,
    AskResolved,
    PausedLifted,
    Completed,
    SelectionLanded,
    Materialized,
}

/// Where a one-shot paints, resolved fresh each frame from the row's line run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The row's full card block (header line included when first in group).
    Card,
    /// The one-cell gutter column over the card — the block edge.
    Spine,
}

/// The last observed per-row facts the transition detector diffs against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observed {
    status: AgentStatus,
}

/// A live transition cue: the row it belongs to, what fired it, and the
/// tachyonfx effect carrying its decay.
#[derive(Clone)]
struct Oneshot {
    key: String,
    kind: TransitionKind,
    target: Target,
    fx: Effect,
    /// Set on the spawn frame so the first process starts the effect at t=0
    /// (full flash) instead of skipping ahead by the frame's elapsed step.
    born: bool,
}

/// The effects pass's whole memory, riding `UiState` like the spend tally's
/// `TallyAnim`: the previous frame's per-row statuses and selection (the
/// transition detector's diff base) and the live one-shots.
#[derive(Clone, Default)]
pub(crate) struct EffectState {
    /// The phase the pass last ran at; elapsed time derives from the delta.
    last_phase: Option<u64>,
    /// Whether a frame has been observed at all. The very first frame records
    /// without spawning — a fresh renderer attaching mid-fleet never opens on
    /// a flash storm — while a later arrival into an already-watched room
    /// earns its materialize cue.
    primed: bool,
    /// Last seen status per row id — the transition detector's diff base.
    prev: HashMap<String, Observed>,
    /// Last seen selection (outer `None` = never observed — the guard that
    /// keeps the first frame from reading as a selection jump).
    last_selection: Option<Option<PaneId>>,
    oneshots: Vec<Oneshot>,
}

impl std::fmt::Debug for EffectState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectState")
            .field("last_phase", &self.last_phase)
            .field("tracked_rows", &self.prev.len())
            .field("oneshots", &self.oneshots.len())
            .finish()
    }
}

impl EffectState {
    /// Whether any one-shot is still decaying — the serve loop's gate hook:
    /// while true the fast tick stays warm so the flash plays smoothly.
    pub(crate) fn any_active(&self) -> bool {
        !self.oneshots.is_empty()
    }

    /// The whole pass: observe transitions against the previous frame, then
    /// paint every live effect onto the freshly composed buffer. Runs after
    /// the paragraph render inside the same draw, with the line map that draw
    /// just wrote — so geometry and content can never disagree.
    ///
    /// Two row universes, deliberately split: transitions observe the *whole*
    /// room, so toggling the make-up filter never reads as a wave of evictions
    /// and arrivals — no flash storm. Geometry resolves against the *filtered*
    /// rows, whose ordinals are what `line_map` carries — an unfiltered index
    /// would land a cue on a stranger's card. A cue for a row the filter hides
    /// finds no run and ends that frame, exactly like a row scrolled off: the
    /// transition is still *recorded* either way, so clearing the filter later
    /// reveals the row already settled rather than replaying a now-stale flash.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply(
        &mut self,
        snapshot: &SidebarSnapshot,
        theme: &Theme,
        filter: Option<BodyFilter>,
        line_map: &[Option<usize>],
        selected_pane: Option<&PaneId>,
        phase: u64,
        buf: &mut Buffer,
        area: Rect,
    ) {
        let elapsed = Duration::from_millis(step_ms(
            self.last_phase,
            phase,
            u64::from(snapshot.sidebar.resolved_refresh_ms()),
        ));
        self.last_phase = Some(phase);

        let rows: Vec<&SidebarRow> = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .collect();

        self.observe(&rows, selected_pane, theme);

        let visible: Vec<&SidebarRow> = rows
            .iter()
            .copied()
            .filter(|row| row_passes_filter(row, filter))
            .collect();

        self.oneshots.retain_mut(|shot| {
            let Some(rect) = target_rect(&visible, line_map, area, &shot.key, shot.target) else {
                // The row left the screen (scrolled out, evicted, reranked
                // off); a cue with nothing to paint on is over, not pending.
                return false;
            };
            let step = if shot.born { Duration::ZERO } else { elapsed };
            shot.born = false;
            shot.fx.process(step, buf, rect);
            shot.fx.running()
        });
    }

    /// Diff the frame's rows and selection against the last observed state and
    /// spawn the matching one-shots. First observation — of the whole room, a
    /// new row, or the selection — records silently; only a *change* earns a
    /// cue. Rows that vanished are evicted so a relaunch reads as new.
    fn observe(&mut self, rows: &[&SidebarRow], selected_pane: Option<&PaneId>, theme: &Theme) {
        let first_frame = !self.primed;
        self.primed = true;
        for row in rows {
            let Some(status) = row.status() else { continue };
            let previous = self.prev.insert(row.id.clone(), Observed { status });
            if first_frame {
                continue;
            }
            let kind = match previous {
                None => Some(TransitionKind::Materialized),
                Some(seen) => transition(seen.status, status),
            };
            if let Some(kind) = kind {
                self.spawn(&row.id, kind, theme);
            }
        }
        let live: std::collections::HashSet<&str> =
            rows.iter().map(|row| row.id.as_str()).collect();
        self.prev.retain(|key, _| live.contains(key.as_str()));

        let current = selected_pane.cloned();
        if let Some(previous) = self.last_selection.replace(current.clone())
            && previous != current
            && let Some(pane) = current
            && let Some(row) = rows.iter().find(|row| {
                row.pane
                    .as_ref()
                    .is_some_and(|pane_ref| pane_ref.pane_id == pane)
            })
        {
            let key = row.id.clone();
            self.spawn(&key, TransitionKind::SelectionLanded, theme);
        }
    }

    /// Push the one-shot for `kind`, replacing a still-decaying cue of the
    /// same kind on the same row so a status flap restarts the flash instead
    /// of stacking a second one.
    fn spawn(&mut self, key: &str, kind: TransitionKind, theme: &Theme) {
        self.oneshots
            .retain(|shot| !(shot.kind == kind && shot.key == key));
        let (target, fx) = build_oneshot(kind, theme);
        self.oneshots.push(Oneshot {
            key: key.to_owned(),
            kind,
            target,
            fx,
            born: true,
        });
    }
}

/// Elapsed milliseconds to feed the effects this frame: the phase delta on the
/// 100ms grid, zero on the very first pass (nothing jumps), capped so a flash
/// spawned after a calm stretch still plays out on screen.
fn step_ms(last: Option<u64>, phase: u64, frame_ms: u64) -> u64 {
    last.map_or(0, |last| phase.saturating_sub(last) * frame_ms)
        .min(MAX_STEP_MS)
}

/// The transition cue a status change earns, if any. Entering an actionable
/// state outranks everything (a `paused → waiting` flap reads as the new
/// ask, not the lift); leaving the paused park and settling an ask each carry
/// their own cue; a fresh turn finishing well (`running`/`idle → success`,
/// where nothing more urgent is in play) gives the eye one gentle completion
/// cue — its announce-once moment before it settles to the static unread crest;
/// everything else is status churn the row's own glyph already tells.
fn transition(seen: AgentStatus, status: AgentStatus) -> Option<TransitionKind> {
    if seen == status {
        return None;
    }
    if status == AgentStatus::Waiting {
        return Some(TransitionKind::EnteredWaiting);
    }
    if status == AgentStatus::Failed {
        return Some(TransitionKind::EnteredFailed);
    }
    if seen == AgentStatus::Paused {
        return Some(TransitionKind::PausedLifted);
    }
    if seen.is_actionable() {
        return Some(TransitionKind::AskResolved);
    }
    if status == AgentStatus::Success {
        return Some(TransitionKind::Completed);
    }
    None
}

/// The one-shot's target and decay for `kind`, toned through the active
/// palette. An actionable status-change card flash *settles*: the cue tone wipes
/// in from the spine, then dissolves back to rest ([`directional_settle`]). A
/// fresh card *develops in*, and a finished turn *announces once* the same gentle
/// way — a uniform tone resolving back to rest ([`single_fade`]): the new card
/// from the dim recede tone ([`Component::CardRecede`]), the completion from the
/// positive [`Component::FlashCompleted`] crest. The spine flick under a landed
/// selection stays a plain fade. Every effect is foreground-only and skips
/// default-foreground (`Reset`) cells, whose true tone the terminal owns.
fn build_oneshot(kind: TransitionKind, theme: &Theme) -> (Target, Effect) {
    let fx = match kind {
        TransitionKind::EnteredWaiting => {
            directional_settle(theme.component(Component::FlashWaiting), FLASH_ENTERED_MS)
        }
        TransitionKind::EnteredFailed => {
            directional_settle(theme.component(Component::FlashFailed), FLASH_ENTERED_MS)
        }
        TransitionKind::AskResolved => {
            directional_settle(theme.component(Component::FlashResolved), FLASH_RESOLVED_MS)
        }
        TransitionKind::PausedLifted => {
            directional_settle(theme.component(Component::FlashLifted), FLASH_LIFTED_MS)
        }
        TransitionKind::Completed => single_fade(
            theme.component(Component::FlashCompleted),
            FLASH_RESOLVED_MS,
        ),
        TransitionKind::SelectionLanded => single_fade(
            theme.component(Component::FlashSelectionLanded),
            FLASH_SELECTED_MS,
        ),
        TransitionKind::Materialized => {
            single_fade(theme.component(Component::CardRecede), FLASH_MATERIALIZE_MS)
        }
    };
    let target = match kind {
        TransitionKind::SelectionLanded => Target::Spine,
        _ => Target::Card,
    };
    (target, fx)
}

/// The cell filter every flash carries: skip default-foreground (`Reset`)
/// cells, whose true tone the terminal owns — tachyonfx would otherwise lerp
/// them through a hardcoded white/black fallback, wrong on a light scheme. A
/// fresh guard per effect (and per `sequence` child) since a container runs
/// each child's own filter.
fn reset_guard() -> CellFilter {
    CellFilter::Not(CellFilter::FgColor(Color::Reset).into())
}

/// A plain foreground fade from `tone` back to each cell's own color — the
/// uniform develop the spine flick and the card's develop-in arrival ride.
fn single_fade(tone: Color, ms: u32) -> Effect {
    fx::fade_from_fg(tone, (ms, Interpolation::QuadOut)).with_filter(reset_guard())
}

/// A directional settle: the flash tone wipes in from the spine column (left
/// edge) across the card on a short lead, then dissolves back to each cell's
/// resting tone. Both phases are foreground fades — the sweep is a spatial
/// *pattern* on the lead fade's alpha, never a cell translation — so the pass
/// stays color-only. The split holds `wipe + settle == ms`, keeping a cue's
/// total decay window (and the fast-grid decay tests) exactly where a plain
/// fade had it.
fn directional_settle(flash: Color, ms: u32) -> Effect {
    let wipe = (ms * 2 / 5).min(ms);
    let settle = ms - wipe;
    fx::sequence(&[
        fx::fade_to_fg(flash, (wipe, Interpolation::CircOut))
            .with_pattern(SweepPattern::left_to_right(SWEEP_SPAN))
            .with_filter(reset_guard()),
        fx::fade_from_fg(flash, (settle, Interpolation::QuadOut)).with_filter(reset_guard()),
    ])
}

/// The contiguous line run `line_map` assigns to visible row `index` — the
/// row's card block, group header included when it leads its group, exactly
/// the lines the hit-test would route to it.
fn row_run(line_map: &[Option<usize>], index: usize) -> Option<Range<usize>> {
    let first = line_map.iter().position(|entry| *entry == Some(index))?;
    let last = line_map
        .iter()
        .rposition(|entry| *entry == Some(index))
        .unwrap_or(first);
    Some(first..last + 1)
}

/// Resolve a one-shot's paint rect for the row keyed `key`, or `None` when the
/// row is gone or off screen this frame. Width spares the right rail column so a
/// flash never tints the scrollbar. That also leaves the static right spine
/// untinted on card flashes; mirroring the flash needs a rail-aware target that
/// knows whether the scrollbar owns the column this frame.
fn target_rect(
    rows: &[&SidebarRow],
    line_map: &[Option<usize>],
    area: Rect,
    key: &str,
    target: Target,
) -> Option<Rect> {
    let index = rows.iter().position(|row| row.id == key)?;
    let run = row_run(line_map, index)?;
    Some(match target {
        Target::Card => card_rect(area, &run),
        Target::Spine => spine_rect(area, &run),
    })
}

fn card_rect(area: Rect, run: &Range<usize>) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(run.start as u16),
        area.width.saturating_sub(1),
        run.len() as u16,
    )
}

fn spine_rect(area: Rect, run: &Range<usize>) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(run.start as u16),
        1,
        run.len() as u16,
    )
}

#[cfg(test)]
mod tests {
    use crate::config::SidebarConfig;
    use crate::feed::PaneRef;
    use crate::ids::{MuxName, ViewKind};
    use crate::{AgentCard, RowCard, SidebarWorktreeGroup, SidebarWorktreeKind, WorkspaceId};
    use jiff::Timestamp;

    use super::*;

    fn pane(raw: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: Some(ViewKind::Window),
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: Some("claude".to_owned()),
            spawn_command: None,
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    fn row(id: &str, status: AgentStatus) -> SidebarRow {
        SidebarRow {
            id: id.to_owned(),
            name: "claude".to_owned(),
            pane: Some(pane(id)),
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            unread: false,
            inactive: false,
            last_activity: Timestamp::now(),
            card: RowCard::Agent(Box::new(AgentCard {
                status: Some(status),
                phase: crate::agents::TurnPhase::Idle,
                request_id: None,
                surface: None,
                task: Some("db migrate".to_owned()),
                prompt: None,
                model: None,
                effort: None,
                context_pct: None,
                context_window: None,
                total_tokens: None,
                cache_read_input_tokens: None,
                cache_write_input_tokens: None,
                fresh_input_tokens: None,
                output_tokens: None,
                todo_done: None,
                todo_total: None,
                context: None,
                context_severity: None,
                registered_at: None,
                resolver: None,
                options: Vec::new(),
                sub_agents: Vec::new(),
                compacting: false,
                compaction_count: 0,
                turn_error_label: None,
            })),
        }
    }

    fn snap(rows: Vec<SidebarRow>) -> SidebarSnapshot {
        let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let now = Timestamp::now();
        SidebarSnapshot {
            workspace_id,
            display_name: "query-engine".to_owned(),
            generated_at: now,
            panes_produced_at_ms: None,
            panes_observed_at_ms: None,
            focus_contested_panes: Vec::new(),
            truth_degraded: None,
            now,
            worktree_groups: vec![SidebarWorktreeGroup {
                key: "/repo/main".to_owned(),
                label: "main".to_owned(),
                kind: SidebarWorktreeKind::Worktree,
                status_counts: Vec::new(),
                rows,
                hidden_count: 0,
                diff_added: None,
                diff_removed: None,
                commits_ahead: None,
                commits_behind: None,
                trunk: None,
                clean: None,
            }],
            needs_attention: Vec::new(),
            resolver_working: Vec::new(),
            agents: Vec::new(),
            wired_lazy_kinds: Vec::new(),
            lazy_agent_default_models: std::collections::BTreeMap::new(),
            agent_panes: Vec::new(),
            own_view: None,
            only_daemon_view_remains: false,
            project_root: None,
            worktree_roots: Vec::new(),
            worktree_home: None,
            root_class: crate::workspace::RootClass::Repo,
            sidebar: SidebarConfig::default(),
            theme: crate::config::ThemeConfig::default(),
            pets: crate::config::PetsConfig::default(),
            attention: crate::config::AttentionConfig::default(),
            providers: Vec::new(),
            value_tally: None,
            workspace_value_tally: None,
            today_spend_live_usd: None,
            link: None,
            reflects_log: None,
        }
    }

    /// Drive one frame through the pass: a 3-line run for row 0 over a small
    /// blank buffer, no selection unless given.
    fn frame(
        state: &mut EffectState,
        snapshot: &SidebarSnapshot,
        selected: Option<&PaneId>,
        phase: u64,
    ) {
        let area = Rect::new(0, 0, 30, 8);
        let mut buf = Buffer::empty(area);
        let line_map = vec![Some(0), Some(0), Some(0), Some(1), Some(1), None];
        state.apply(
            snapshot,
            &Theme::fixed(false),
            None,
            &line_map,
            selected,
            phase,
            &mut buf,
            area,
        );
    }

    #[test]
    fn transition_table_matches_the_cue_vocabulary() {
        use AgentStatus::*;
        use TransitionKind::*;
        assert_eq!(transition(Idle, Waiting), Some(EnteredWaiting));
        assert_eq!(transition(Running, Failed), Some(EnteredFailed));
        assert_eq!(transition(Waiting, Running), Some(AskResolved));
        assert_eq!(transition(Failed, Idle), Some(AskResolved));
        assert_eq!(transition(Paused, Running), Some(PausedLifted));
        // Entering an actionable state outranks the lift.
        assert_eq!(transition(Paused, Waiting), Some(EnteredWaiting));
        // A fresh turn finishing well earns its one announce cue...
        assert_eq!(transition(Running, Success), Some(Completed));
        assert_eq!(transition(Idle, Success), Some(Completed));
        // ...but settling an ask outranks the completion (the ask clearing is
        // the salient change, not the success underneath it).
        assert_eq!(transition(Waiting, Success), Some(AskResolved));
        // Plain work churn carries no cue.
        assert_eq!(transition(Idle, Running), None);
        assert_eq!(transition(Waiting, Waiting), None);
    }

    #[test]
    fn step_is_zero_first_then_the_phase_delta_capped() {
        let frame_ms = u64::from(crate::sidebar::timing::DEFAULT_REFRESH_MS);
        assert_eq!(step_ms(None, 42, frame_ms), 0);
        assert_eq!(step_ms(Some(7), 8, frame_ms), 100);
        assert_eq!(step_ms(Some(5), 8, frame_ms), 300);
        // A calm stretch clamps so a fresh flash still plays out on screen.
        assert_eq!(step_ms(Some(0), 1_000, frame_ms), 300);
        // A restarted phase base saturates instead of jumping.
        assert_eq!(step_ms(Some(50), 10, frame_ms), 0);
    }

    #[test]
    fn row_run_resolves_the_contiguous_line_block() {
        let map = vec![None, Some(0), Some(0), None, Some(1)];
        assert_eq!(row_run(&map, 0), Some(1..3));
        assert_eq!(row_run(&map, 1), Some(4..5));
        assert_eq!(row_run(&map, 2), None);
    }

    #[test]
    fn first_frame_records_without_a_flash_storm() {
        let mut state = EffectState::default();
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            0,
        );
        assert!(!state.any_active(), "a fresh renderer never opens flashing");
    }

    #[test]
    fn status_change_spawns_the_cue_then_decays_on_the_fast_grid() {
        let mut state = EffectState::default();
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Idle)]),
            None,
            0,
        );
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            1,
        );
        assert!(state.any_active());
        assert_eq!(state.oneshots[0].kind, TransitionKind::EnteredWaiting);
        // 100ms per phase against the 250ms fade: alive, alive, done.
        let waiting = snap(vec![row("a", AgentStatus::Waiting)]);
        frame(&mut state, &waiting, None, 2);
        frame(&mut state, &waiting, None, 3);
        assert!(state.any_active(), "mid-decay the flash is still live");
        frame(&mut state, &waiting, None, 4);
        assert!(!state.any_active(), "a finished flash drains from the gate");
    }

    #[test]
    fn arrival_into_a_watched_room_materializes_but_a_flap_never_stacks() {
        let mut state = EffectState::default();
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Idle)]),
            None,
            0,
        );
        let both = snap(vec![
            row("a", AgentStatus::Idle),
            row("b", AgentStatus::Idle),
        ]);
        frame(&mut state, &both, None, 1);
        assert_eq!(state.oneshots.len(), 1);
        assert_eq!(state.oneshots[0].kind, TransitionKind::Materialized);
        assert_eq!(state.oneshots[0].key, "b");

        // A waiting↔running flap restarts the cue instead of stacking copies.
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            2,
        );
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Running)]),
            None,
            3,
        );
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            4,
        );
        let entered = state
            .oneshots
            .iter()
            .filter(|shot| shot.kind == TransitionKind::EnteredWaiting)
            .count();
        assert_eq!(entered, 1);
    }

    #[test]
    fn vanished_row_is_evicted_and_returns_as_a_new_arrival() {
        let mut state = EffectState::default();
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            0,
        );
        frame(&mut state, &snap(Vec::new()), None, 1);
        assert!(
            state.prev.is_empty(),
            "a gone row leaves no stale diff base"
        );
        frame(
            &mut state,
            &snap(vec![row("a", AgentStatus::Waiting)]),
            None,
            2,
        );
        assert_eq!(state.oneshots[0].kind, TransitionKind::Materialized);
    }

    #[test]
    fn selection_change_lights_the_spine_once() {
        let mut state = EffectState::default();
        let two = snap(vec![
            row("a", AgentStatus::Idle),
            row("b", AgentStatus::Idle),
        ]);
        let pane_a = PaneId::from_parts(MuxName::Tmux, "a");
        let pane_b = PaneId::from_parts(MuxName::Tmux, "b");
        // First observation of a selection is a baseline, not a jump.
        frame(&mut state, &two, Some(&pane_a), 0);
        assert!(!state.any_active());
        frame(&mut state, &two, Some(&pane_a), 1);
        assert!(!state.any_active(), "a held selection never re-fires");
        frame(&mut state, &two, Some(&pane_b), 2);
        assert_eq!(state.oneshots[0].kind, TransitionKind::SelectionLanded);
        assert_eq!(state.oneshots[0].target, Target::Spine);
        assert_eq!(state.oneshots[0].key, "b");
    }
}
