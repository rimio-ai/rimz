//! Post-render color effects — the truecolor "garnish" tier.
//!
//! After the paragraph renders, this pass mutates buffer cell *colors* in
//! place: the attention glow (a smooth lightness swell on a `?`/`!` row's
//! glyph, name, and gutter spine, phase-locked to the modifier breath in
//! [`super::labels::attention_breath`]) and brief one-shot flashes on state
//! transitions — a card entering `waiting`/`failed`, an ask resolving, a
//! rate-limit lifting, a new card appearing, the spine lighting under a fresh
//! selection. Color only, never a glyph: the composed text is untouched, so
//! the golden frames and the `NO_COLOR` grammar cannot drift (locked by the
//! `effects_pass_never_changes_the_composed_text` golden guard).
//!
//! The pass runs only when [`Theme::effects_enabled`] says the terminal speaks
//! 24-bit color — smooth interpolation quantizes into banding on a 256-color
//! palette — and it obeys the design law: the glow rides rows that already
//! breathe, the one-shots animate the moment of change and decay, and a calm
//! room paints nothing here.
//!
//! Geometry re-resolves every frame from `UiState::line_map` (the hit-test
//! map, the renderer's one row-geometry authority), so an effect follows its
//! row through ranking reorders and scrolling and simply drops when the row
//! leaves the viewport. Time advances by `animation_phase` deltas — never the
//! wall clock — so every effect is deterministic under a pinned phase.

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use rimz::feed::AgentStatus;
use rimz::ids::PaneId;
use rimz::{SidebarRow, SidebarRowKind, SidebarSnapshot};
use tachyonfx::{CellFilter, Effect, EffectTimer, Interpolation, fx};

use super::fmt::age_secs;
use super::labels::age_heat;
use super::theme::{ORANGE, Theme};

/// The glow's peak lightness lift (HSL points over the painted tone). Strong
/// enough to read as a swell on the muted palette, gentle enough to stay a
/// breath rather than a strobe.
const GLOW_MAX_LIGHTNESS: f32 = 16.0;

/// One animation tick in milliseconds — mirrors `app::ANIMATION_FRAME`, the
/// grid `animation_phase` counts on.
const FRAME_MS: u64 = 100;

/// Cap on the elapsed time fed into a one-shot per painted frame. A calm room
/// paints rarely, so a raw phase delta can span seconds; clamping means a
/// flash spawned after a quiet stretch still plays out over visible frames
/// instead of expiring inside its first one.
const MAX_STEP_MS: u64 = 300;

const FLASH_ENTERED_MS: u32 = 250;
const FLASH_RESOLVED_MS: u32 = 300;
const FLASH_LIFTED_MS: u32 = 400;
const FLASH_SELECTED_MS: u32 = 180;
const FLASH_MATERIALIZE_MS: u32 = 250;

/// The state-transition cues the observer spawns. Each is a one-shot: it
/// plays once over its row and expires; the continuous attention glow is not
/// one of these (it is rebuilt per frame from the phase, stateless).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionKind {
    EnteredWaiting,
    EnteredFailed,
    AskResolved,
    RateLimitLifted,
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
/// transition detector's diff base) and the live one-shots. The continuous
/// glow holds no state here — it is a pure function of (age heat, phase).
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
    /// while true the fast tick stays warm so the flash plays smoothly. The
    /// continuous glow deliberately does not count; it rides the slow
    /// cosmetic cadence the attention breath already keeps alive.
    pub(crate) fn any_active(&self) -> bool {
        !self.oneshots.is_empty()
    }

    /// The whole pass: observe transitions against the previous frame, then
    /// paint every live effect onto the freshly composed buffer. Runs after
    /// the paragraph render inside the same draw, with the line map that draw
    /// just wrote — so geometry and content can never disagree.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply(
        &mut self,
        snapshot: &SidebarSnapshot,
        theme: &Theme,
        line_map: &[Option<usize>],
        selected_pane: Option<&PaneId>,
        phase: u64,
        buf: &mut Buffer,
        area: Rect,
    ) {
        let elapsed = Duration::from_millis(step_ms(self.last_phase, phase));
        self.last_phase = Some(phase);

        let rows: Vec<&SidebarRow> = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .collect();

        self.observe(&rows, selected_pane, theme);

        // One-shots first, glyph glow second, so a card-wide flash never
        // flattens the breath on the attention glyph it overlaps.
        self.oneshots.retain_mut(|shot| {
            let Some(rect) = target_rect(&rows, line_map, area, &shot.key, shot.target) else {
                // The row left the screen (scrolled out, evicted, reranked
                // off); a cue with nothing to paint on is over, not pending.
                return false;
            };
            let step = if shot.born { Duration::ZERO } else { elapsed };
            shot.born = false;
            shot.fx.process(step, buf, rect);
            shot.fx.running()
        });

        for (index, row) in rows.iter().enumerate() {
            let Some(delta) = glow_delta(row, phase) else {
                continue;
            };
            let Some(run) = row_run(line_map, index) else {
                continue;
            };
            if let Some(word) = word_rect(buf, area, &run, row) {
                shift_lightness(delta, buf, word);
            }
            shift_lightness(delta, buf, spine_rect(area, &run));
        }
    }

    /// Diff the frame's rows and selection against the last observed state and
    /// spawn the matching one-shots. First observation — of the whole room, a
    /// new row, or the selection — records silently; only a *change* earns a
    /// cue. Rows that vanished are evicted so a relaunch reads as new.
    fn observe(&mut self, rows: &[&SidebarRow], selected_pane: Option<&PaneId>, theme: &Theme) {
        let first_frame = !self.primed;
        self.primed = true;
        for row in rows {
            if row.row_kind != SidebarRowKind::Agent {
                continue;
            }
            let Some(status) = row.status else { continue };
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
fn step_ms(last: Option<u64>, phase: u64) -> u64 {
    last.map_or(0, |last| phase.saturating_sub(last) * FRAME_MS)
        .min(MAX_STEP_MS)
}

/// The transition cue a status change earns, if any. Entering an actionable
/// state outranks everything (a `rate_limited → waiting` flap reads as the new
/// ask, not the lift); leaving the rate-limit park and settling an ask each
/// carry their own cue; everything else is status churn the row's own glyph
/// already tells.
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
    if seen == AgentStatus::RateLimited {
        return Some(TransitionKind::RateLimitLifted);
    }
    if seen.is_actionable() {
        return Some(TransitionKind::AskResolved);
    }
    None
}

/// The one-shot's target and decay for `kind`, toned through the active
/// palette. Every flash is a foreground-only fade from the cue tone back to
/// each cell's own color — and skips default-foreground (`Reset`) cells,
/// whose true tone the terminal owns (tachyonfx would lerp them via a
/// hardcoded white fallback, wrong on a light scheme).
fn build_oneshot(kind: TransitionKind, theme: &Theme) -> (Target, Effect) {
    let (target, tone, ms) = match kind {
        TransitionKind::EnteredWaiting => (Target::Card, Color::Yellow, FLASH_ENTERED_MS),
        TransitionKind::EnteredFailed => (Target::Card, Color::Red, FLASH_ENTERED_MS),
        TransitionKind::AskResolved => (Target::Card, Color::Green, FLASH_RESOLVED_MS),
        TransitionKind::RateLimitLifted => (Target::Card, Color::Green, FLASH_LIFTED_MS),
        TransitionKind::SelectionLanded => (Target::Spine, Color::Cyan, FLASH_SELECTED_MS),
        TransitionKind::Materialized => (Target::Card, Color::DarkGray, FLASH_MATERIALIZE_MS),
    };
    let mut fx = fx::fade_from_fg(theme.tone(tone), (ms, Interpolation::QuadOut));
    fx.filter(CellFilter::Not(CellFilter::FgColor(Color::Reset).into()));
    (target, fx)
}

/// The glow's lightness lift for `row` at `phase`, or `None` when the row
/// holds no glow: only an unresolved actionable row breathes, the resolver
/// spinner means the ask is being handled, and at red heat the hard modifier
/// blink owns the cell — a smooth swell under a strobe would mush both.
/// The wave is the breath's own triangle (same cycle, same amber double-time,
/// see [`super::labels::attention_breath`]), so color and modifier swell as
/// one motion.
fn glow_delta(row: &SidebarRow, phase: u64) -> Option<f32> {
    if row.row_kind != SidebarRowKind::Agent || row.resolver.is_some() {
        return None;
    }
    if !row.status.is_some_and(AgentStatus::is_actionable) {
        return None;
    }
    let heat = age_heat(age_secs(row.last_activity));
    if heat == Some(Color::Red) {
        return None;
    }
    let level = breath_level(phase, heat == Some(ORANGE));
    (level > 0.0).then_some(level * GLOW_MAX_LIGHTNESS)
}

/// One step of the breath triangle as a 0..1 level — the continuous twin of
/// `labels::breath_wave`, on the same 24-tick cycle (12 at amber double-time)
/// so the color swell peaks exactly when the modifier does.
fn breath_level(phase: u64, double_time: bool) -> f32 {
    const CYCLE: u64 = 24;
    let phase = if double_time {
        phase.wrapping_mul(2)
    } else {
        phase
    };
    let pos = phase % CYCLE;
    let level = if pos <= CYCLE / 2 { pos } else { CYCLE - pos };
    level as f32 / (CYCLE / 2) as f32
}

/// Lift the foreground lightness of every cell in `rect` by `delta` HSL
/// points, this frame only: an instantaneous shift (a 1ms shader driven to
/// completion), rebuilt next frame at the next phase's delta — stateless by
/// construction.
fn shift_lightness(delta: f32, buf: &mut Buffer, rect: Rect) {
    let mut fx = fx::hsl_shift_fg(
        [0.0, 0.0, delta],
        EffectTimer::from_ms(1, Interpolation::Linear),
    );
    fx.process(Duration::from_millis(1), buf, rect);
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

/// Resolve a one-shot's paint rect for the row keyed `key`, or `None` when
/// the row is gone or off screen this frame. Width spares the right-margin
/// column so a flash never tints the scrollbar.
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

/// The glow's word rect: the gutter cell, the attention glyph, and the agent
/// name on the card's identity line — found by scanning the row's run for the
/// composed `?`/`!` in the glyph column, which also skips the group header
/// line sharing the run. `None` when the identity line is scrolled out or the
/// glyph is not on screen this frame.
fn word_rect(buf: &Buffer, area: Rect, run: &Range<usize>, row: &SidebarRow) -> Option<Rect> {
    for line in run.clone() {
        let y = area.y.saturating_add(line as u16);
        let Some(cell) = buf.cell((area.x + 1, y)) else {
            continue;
        };
        if matches!(cell.symbol(), "?" | "!") {
            let label = 3 + row.name.chars().count() as u16;
            return Some(Rect::new(
                area.x,
                y,
                label.min(area.width.saturating_sub(1)),
                1,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rimz::config::SidebarConfig;
    use rimz::feed::PaneRef;
    use rimz::ids::{MuxName, ResolverId, ViewKind};
    use rimz::{SidebarResolverState, SidebarWorktreeGroup, SidebarWorktreeKind, WorkspaceId};

    use super::*;

    fn pane(raw: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: Some(ViewKind::Window),
            view_name: None,
            is_focused: false,
            command: Some("claude".to_owned()),
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }
    }

    fn row(id: &str, status: AgentStatus) -> SidebarRow {
        SidebarRow {
            row_kind: SidebarRowKind::Agent,
            id: id.to_owned(),
            name: "claude".to_owned(),
            status: Some(status),
            phase: rimz::agents::TurnPhase::Idle,
            pane: Some(pane(id)),
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
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            context_severity: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
            registered_at: None,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            process_active: false,
            command_detail: None,
            compacting: false,
            turn_error_label: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }
    }

    fn snap(rows: Vec<SidebarRow>) -> SidebarSnapshot {
        let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let now = Timestamp::now();
        SidebarSnapshot {
            workspace_id,
            display_name: "query-engine".to_owned(),
            generated_at: now,
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
            }],
            needs_attention: Vec::new(),
            resolver_working: Vec::new(),
            agents: Vec::new(),
            agent_hooks_ready: true,
            wired_lazy_kinds: Vec::new(),
            own_view: None,
            only_daemon_view_remains: false,
            project_root: None,
            worktree_roots: Vec::new(),
            root_class: rimz::workspace::RootClass::Repo,
            sidebar: SidebarConfig::default(),
            providers: Vec::new(),
            value_tally: None,
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
        assert_eq!(transition(RateLimited, Running), Some(RateLimitLifted));
        // Entering an actionable state outranks the lift.
        assert_eq!(transition(RateLimited, Waiting), Some(EnteredWaiting));
        // Plain work churn carries no cue.
        assert_eq!(transition(Idle, Running), None);
        assert_eq!(transition(Running, Success), None);
        assert_eq!(transition(Waiting, Waiting), None);
    }

    #[test]
    fn step_is_zero_first_then_the_phase_delta_capped() {
        assert_eq!(step_ms(None, 42), 0);
        assert_eq!(step_ms(Some(7), 8), 100);
        assert_eq!(step_ms(Some(5), 8), 300);
        // A calm stretch clamps so a fresh flash still plays out on screen.
        assert_eq!(step_ms(Some(0), 1_000), 300);
        // A restarted phase base saturates instead of jumping.
        assert_eq!(step_ms(Some(50), 10), 0);
    }

    #[test]
    fn breath_level_is_the_breaths_own_triangle() {
        assert_eq!(breath_level(0, false), 0.0);
        assert_eq!(breath_level(12, false), 1.0);
        assert_eq!(breath_level(24, false), 0.0);
        assert_eq!(breath_level(6, false), 0.5);
        // Amber double-time peaks twice as fast.
        assert_eq!(breath_level(6, true), 1.0);
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

    #[test]
    fn glow_rides_only_an_unhandled_actionable_row_below_red() {
        let mid_breath = 6;
        let waiting = row("a", AgentStatus::Waiting);
        assert_eq!(
            glow_delta(&waiting, mid_breath),
            Some(GLOW_MAX_LIGHTNESS / 2.0)
        );
        assert_eq!(
            glow_delta(&waiting, 0),
            None,
            "the swell's trough paints nothing"
        );

        let calm = row("a", AgentStatus::Running);
        assert_eq!(glow_delta(&calm, mid_breath), None);

        let mut handled = row("a", AgentStatus::Waiting);
        handled.resolver = Some(SidebarResolverState {
            resolver_id: ResolverId::new_unchecked("opus-policy"),
            display_name: None,
            budget_until: None,
        });
        assert_eq!(
            glow_delta(&handled, mid_breath),
            None,
            "a resolver owns the ask"
        );

        let mut red = row("a", AgentStatus::Waiting);
        red.last_activity = Timestamp::now() - jiff::SignedDuration::from_secs(2 * 3_600);
        assert_eq!(
            glow_delta(&red, mid_breath),
            None,
            "the red blink owns the cell"
        );

        let mut amber = row("a", AgentStatus::Waiting);
        amber.last_activity = Timestamp::now() - jiff::SignedDuration::from_secs(2_000);
        assert_eq!(
            glow_delta(&amber, 3),
            Some(GLOW_MAX_LIGHTNESS / 2.0),
            "amber doubles the tempo, peaking at phase 6"
        );
    }

    #[test]
    fn lightness_shift_touches_the_color_never_the_glyph() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        let cell = buf.cell_mut((1, 0)).unwrap();
        cell.set_symbol("?");
        cell.set_fg(Color::Indexed(179));
        shift_lightness(GLOW_MAX_LIGHTNESS, &mut buf, area);
        let cell = buf.cell((1, 0)).unwrap();
        assert_eq!(cell.symbol(), "?", "the shift is color-only");
        assert_ne!(
            cell.fg,
            Color::Indexed(179),
            "a full-delta shift visibly lifts the painted tone"
        );
    }
}
