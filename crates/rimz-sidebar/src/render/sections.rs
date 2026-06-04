//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; this module only maps the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! todo progress, diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar.md](../../../docs/internals/sidebar.md).

use jiff::{SignedDuration, Timestamp};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::agents::{AgentContext, RateLimitWindow};
use rimz::config::ContextSeverityConfig;
use rimz::feed::AgentStatus;
use rimz::{
    SidebarProviderPanel, SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarSubAgent,
    SidebarWorktreeGroup, SidebarWorktreeKind, SpendTally, SpendWindow,
};

use super::TallyAnim;

use super::fmt::{
    activity_short, age_secs, age_short, clip, compact_seconds, dollars2, model_label, pct_label,
    reset_countdown, time_remaining, tokens_int, tokens_short, window_label, window_short,
};
use super::labels::{
    SEGMENT_CACHE_READ, SEGMENT_CACHE_WRITE, SEGMENT_INPUT, TOKENS_CACHED, TOKENS_IN, TOKENS_OUT,
    TOKENS_TOTAL, activity_age_style, age_heat, agent_glyph, agent_style, attention_glyph_style,
    branch_delta_spans, compacting_glyph, compacting_style, context_breakdown_spans,
    context_severity_color, context_total_spans, diff_spans, elapsed_glyph, gauge_spans,
    infinite_bar_spans, loading_dots, mana_bar_spans, mana_color, resolver_glyph,
    segmented_gauge_spans, status_glyph, status_style, subagent_glyph, subagent_style, todo_spans,
    token_breakdown_spans, tokens_total_spans, window_style, working_glyph,
};
use super::theme::Theme;

/// The context-meter label — a framed square reading as "the window", replacing
/// the `ctx` word now that it is the row's one bar (the account-scoped budget
/// bars moved to the provider dashboard). A fresh, unfilled window reads as the
/// hollow [`CONTEXT_EMPTY_GLYPH`].
const CONTEXT_GLYPH: &str = "▣";

/// The context-meter label for an empty (0%) window: a hollow square, the
/// unfilled sibling of `▣`, so a just-started window reads "nothing in it yet".
const CONTEXT_EMPTY_GLYPH: &str = "▢";

/// The cockpit count glyphs: `¤` for the live agents in the room right now, `◎`
/// for the sessions (threads) that have run today. `◎` is shared with the W/M
/// ledger rows, so a session count reads the same in both places.
const ACTIVE_AGENTS_GLYPH: &str = "¤";
const SESSIONS_GLYPH: &str = "◎";

/// The expanded card's subagent-section glyph: stacked panes for the children an
/// agent spawned this turn.
const SUBAGENTS_GLYPH: &str = "⧉";

/// The selected card's left accent: a bold half-block `▌` running the card's
/// full height — the one loud lane marker on screen.
const SELECTED_SPINE: &str = "▌";

/// The selected *worktree's* resting lane spine: a thin `▏` (lighter than the
/// selected card's `▌`) down the whole selected group — header, every row, and
/// the inter-card gaps — so the worktree holding the selection reads as one
/// bracketed lane. Non-selected worktrees carry no spine at all.
const LANE_SPINE: &str = "▏";

/// Inner content width: the sidebar width less the one-cell left gutter and the
/// one-cell right margin. Every line is built to this width and then opened with
/// a gutter cell (blank, lane `▏`, or selected `▌`), leaving the trailing column
/// as the matching right margin — so the whole sidebar reads inside a one-cell
/// frame and selecting a row only swaps the gutter glyph, never shifts a column.
pub(super) fn content_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

/// Width band that drives the ambient row density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tier {
    /// Identity + a bare gauge, no labels (~24 columns).
    L0,
    /// Default: line 1 cue + capability + context gauge (~30 columns).
    L1,
    /// Wide: line 2 also inlines todo / extra meters (~44+).
    L2,
}

impl Tier {
    pub(super) fn for_width(width: usize) -> Self {
        if width >= 44 {
            Tier::L2
        } else if width >= 30 {
            Tier::L1
        } else {
            Tier::L0
        }
    }
}

/// The fixed fleet header — the cockpit's make-up line, below the repo
/// dashboard's identity and `¤`/`◎`/spend lines. One line when the room has
/// agents, nothing when it does not (the `¤ 0` count lives on the summary above),
/// so the body below never shifts vertically as agents change *state*:
///
/// ```text
/// ? 2   ! 1   ○ 2   ⏸ 0                        ⢿ 3   ✓ 4   make-up: left · right
/// ```
///
/// The line splits the make-up by who might want you. The left cluster is the
/// rows worth a glance — `waiting` `?` and `failed` `!` (each wearing its
/// oldest row's age heat over a yellow floor), a free `idle` `○` (calm
/// green, but grouped left because a free agent wants work), then a parked
/// `rate-limited` `⏸` (held amber, never heating) closing the cluster. The right
/// cluster is the busy/done tail — working `⢿` (every running agent; the
/// thinking sparkle is a per-row animation head, not a bucket), then `success`
/// `✓`. Every bucket renders, so a zero reads a faint `? 0`. Counts span the
/// capped agents (`status_counts`). The fleet's live time / token / commit
/// totals are gone — the summary line's today-accumulated breakdown carries the
/// fleet's resource read.
pub(super) fn fleet_header_lines(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    width: usize,
) -> Vec<Line<'static>> {
    let working = status_total(groups, AgentStatus::Running);
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    let rate_limited = status_total(groups, AgentStatus::RateLimited);
    let idle = status_total(groups, AgentStatus::Idle);
    let success = status_total(groups, AgentStatus::Success);
    let total = working + waiting + failed + rate_limited + idle + success;

    // An empty (or process-only) room has no make-up line — the `¤ 0  ◎ 0` summary
    // lives on the dashboard above. The make-up line is reserved for a room that
    // has agents to summarize.
    if total == 0 {
        return Vec::new();
    }

    // Top line — the make-up split by who might want you. The left cluster gathers
    // the rows worth a glance: `waiting` `?` and `failed` `!` (the oldest row's
    // heat over a yellow floor), a free `idle` `○` — calm green, but grouped
    // left because a free agent wants work — then a parked `rate-limited` `⏸`
    // closing the cluster. The right cluster is the busy/done tail: working,
    // then success. Every bucket shows its count.
    let mut left: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Waiting),
        waiting,
        attention_bucket_style(theme, groups, AgentStatus::Waiting),
    );
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Failed),
        failed,
        attention_bucket_style(theme, groups, AgentStatus::Failed),
    );
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Idle),
        idle,
        status_style(theme, AgentStatus::Idle),
    );
    // Rate-limited closes the left cluster, after the free `○` idle agent:
    // attention-class but parked. It renders like every other bucket — a faint
    // `⏸ 0` when empty — so the make-up stays a fixed dashboard, scannable by
    // position. It takes the held-amber resting tone (`status_style`), never the
    // heating `attention_bucket_style`, since there is nothing to do but wait
    // for the window to reset.
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::RateLimited),
        rate_limited,
        status_style(theme, AgentStatus::RateLimited),
    );
    let mut right: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Running),
        working,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Success),
        success,
        status_style(theme, AgentStatus::Success),
    );

    // Split left / right when both clusters fit; on a narrow sidebar (the right
    // cluster alone can outrun the width) fall back to one left-packed line so the
    // attention buckets stay intact and the busy tail clips, rather than crushing
    // `? 0  ! 0` down to a stub.
    let buckets = if spans_width(&left) + 1 + spans_width(&right) <= width {
        pin_right(left, right, width)
    } else {
        if !left.is_empty() && !right.is_empty() {
            left.push(Span::raw("   "));
        }
        left.extend(right);
        Line::from(trim_spans_to_width(left, width))
    };

    vec![buckets]
}

/// The fleet head-count read by the dashboard's L2: `(main, subs)` — the main
/// agents you launched (the sum of the capped per-worktree `status_counts`, so it
/// matches the cockpit make-up below) and the subagents they spawned this turn.
pub(super) fn fleet_size(groups: &[SidebarWorktreeGroup]) -> (usize, usize) {
    let main = groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .map(|count| count.count)
        .sum();
    let subs = groups
        .iter()
        .flat_map(|group| &group.rows)
        .map(|row| row.sub_agents.len())
        .sum();
    (main, subs)
}

/// The cockpit attention bucket's tone: bold, wearing the oldest contributing
/// row's [`age_heat`] over the same yellow floor as the per-row glyph — the
/// aggregate echo of [`attention_glyph_style`]'s escalation. Reads the rendered
/// rows (capped-away agents are excluded — the bucket count still spans them,
/// but a hidden agent never drives the visible heat).
fn attention_bucket_style(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
) -> Style {
    let oldest = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status == Some(status))
        .map(|row| age_secs(row.last_activity))
        .max()
        .unwrap_or(0);
    theme.style(age_heat(oldest).unwrap_or(Color::Yellow), Modifier::BOLD)
}

/// Append a `glyph n` bucket to a header line, spaced from the previous one. The
/// glyph and its count are always separated by a single space (`? 2`, never
/// `?2`); successive buckets are separated by three. Every bucket renders, so a
/// zero reads `? 0` — the cockpit is a fixed dashboard, scannable by position —
/// but a zero bucket drops to faint chrome so the eye lands on the live counts,
/// not the empty ones.
fn push_count(
    theme: &Theme,
    spans: &mut Vec<Span<'static>>,
    glyph: &str,
    count: usize,
    style: Style,
) {
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    let style = if count == 0 { theme.faint() } else { style };
    spans.push(Span::styled(format!("{glyph} {count}"), style));
}

/// The cockpit's first summary line, directly beneath the repo identity:
/// `¤ {live}` — the agents in the room right now — on the left, with today's
/// accumulated token breakdown `◇ ↘ ↗ ◍ ◌` (integer magnitudes, the live coarse
/// form) pinned to the right edge. The count reads from the live fleet; the
/// breakdown reads the JSONL `value_tally`'s today window and drops when today
/// recorded no tokens, leaving `¤ {live}` alone. Sessions and spend ride the
/// second line ([`cockpit_spend_line`]).
pub(super) fn cockpit_summary_line(
    theme: &Theme,
    live_agents: usize,
    today: Option<&SpendWindow>,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(
        theme,
        ACTIVE_AGENTS_GLYPH,
        Color::Cyan,
        &live_agents.to_string(),
    );
    let right = today
        .filter(|w| w.tokens > 0 || w.cache_write > 0 || w.cache_read > 0)
        .map(|window| {
            token_breakdown_spans(
                theme,
                window.tokens,
                window.input,
                window.output,
                window.cache_write,
                window.cache_read,
                tokens_int,
                true,
            )
        })
        .unwrap_or_default();
    pin_right(left, right, width)
}

/// The cockpit's second summary line: `◎ {sessions}` — the threads that have run
/// today — on the left, with today's fleet spend pinned to the right edge,
/// climbing in a smooth count-up as a turn lands. The figure eases toward the
/// `value_tally` today total via the shared [`TallyAnim`] roll and brightens for a
/// beat the instant it settles — the cockpit's one animated number (the W/M
/// ledger rows below stay static). Always present — sessions read `◎ 0` in an
/// empty room; the bold money-green `$` joins the right edge once today records
/// spend.
pub(super) fn cockpit_spend_line(
    theme: &Theme,
    sessions: u32,
    today_usd: f64,
    anim: &TallyAnim,
    phase: u64,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(theme, SESSIONS_GLYPH, Color::Cyan, &sessions.to_string());
    let right = if today_usd > 0.0 {
        let usd = anim.today_usd.display(today_usd, phase);
        let style = if anim.today_usd.flashing(phase) {
            theme.style(VALUE_FLASH, Modifier::BOLD)
        } else {
            theme.style(Color::Green, Modifier::BOLD)
        };
        vec![Span::styled(dollars2(usd), style)]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}

/// A stats metric as a colored icon glyph + dim value (`◷ 2h34m`, `¤ 5`): the
/// glyph carries a semantic accent (time teal, commits green; the `◇` token
/// total goes violet via [`tokens_label`]) while the number stays neutral, so
/// the stats read as a tidy icon column instead of a wall of one tone.
fn metric_spans(theme: &Theme, glyph: &str, color: Color, value: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(glyph.to_owned(), theme.style(color, Modifier::empty())),
        Span::styled(format!(" {value}"), theme.dim()),
    ]
}

/// Dim getting-started hint for a healthy room with no agent or feed rows.
/// Shell/editor process rows can still be present; the renderer suppresses
/// this cue once an agent-like process or product row appears.
///
/// The cue names the *real* next step. Until hooks are wired, running
/// claude/codex registers nothing, so an un-wired room points at `rimz hooks
/// install`; once wired (`hooks_ready`), it invites launching an agent.
pub(super) fn first_run_hint_lines(theme: &Theme, hooks_ready: bool) -> Vec<Line<'static>> {
    let dim = theme.dim();
    let lines: [&str; 3] = if hooks_ready {
        ["no agents yet", "run claude or codex", "in a pane to begin"]
    } else {
        [
            "no agents yet",
            "install hooks:",
            "rimz hooks install claude",
        ]
    };
    lines
        .into_iter()
        .map(|text| Line::styled(text, dim))
        .collect()
}

/// Compose one worktree group's lines, appending to `lines`, and tag each
/// content line in the parallel `map` with the visible row index it belongs to
/// (or `None` for the group header and the `+K more` hidden-count line). `map`
/// stays exactly as long as `lines`, so the hit-test can look a screen line up
/// to a row with no separate geometry. The row index captured for a row's lines
/// is the value *before* `row_index` advances, matching `app::visible_rows()`.
#[allow(clippy::too_many_arguments)]
pub(super) fn worktree_group_lines(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    providers: &[SidebarProviderPanel],
    width: usize,
    bands: &ContextSeverityConfig,
    row_index: &mut usize,
    selected_index: usize,
    animation_phase: u64,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
) {
    // Does the selection live in this worktree? If so the whole group reads as one
    // bracketed lane: the resting `▏` spine on the header, every row, and the
    // inter-card gaps, with the selected card itself lit bold `▌`. The `external`
    // catch-all is never a lane.
    let first_row = *row_index;
    let group_selected = group.kind != SidebarWorktreeKind::Workspace
        && (first_row..first_row + group.rows.len()).contains(&selected_index);
    let lane = if group_selected {
        Gutter::Lane
    } else {
        Gutter::Blank
    };

    // The header carries the lane gutter when its worktree is selected (blank
    // otherwise), and its dotted `┄` seal shows only then, so an unselected
    // worktree is just its bold label. The `external` divider is full-bleed
    // chrome with a blank gutter.
    let header = group_header(theme, group, width, group_selected);
    lines.push(with_gutter(theme, header, lane));
    // The worktree name is itself a click target: it lands on the group's first
    // row — the agent adjacent to the header — so clicking the pod name jumps
    // straight into it. The `external` divider is not a worktree name, so it
    // stays inert chrome.
    let header_target = (group.kind != SidebarWorktreeKind::Workspace && !group.rows.is_empty())
        .then_some(*row_index);
    map.push(header_target);
    let tier = Tier::for_width(content_width(width));
    // A blank line separates consecutive cards; it carries the group's lane
    // gutter so a selected worktree's spine runs unbroken through the gap, and
    // maps to `None` as structural chrome (never a jump target).
    for (index, row) in group.rows.iter().enumerate() {
        if index > 0 {
            lines.push(with_gutter(theme, Line::from(""), lane));
            map.push(None);
        }
        let selected = *row_index == selected_index;
        let this_row = *row_index;
        *row_index += 1;
        let gutter = if selected { Gutter::Selected } else { lane };
        let row_lines = row_lines(
            theme,
            row,
            providers,
            width,
            tier,
            selected,
            animation_phase,
            bands,
            gutter,
        );
        map.extend(std::iter::repeat_n(Some(this_row), row_lines.len()));
        lines.extend(row_lines);
    }
    if group.hidden_count > 0 {
        lines.push(with_gutter(
            theme,
            Line::styled(format!("  +{} more", group.hidden_count), theme.dim()),
            lane,
        ));
        map.push(None);
    }
}

fn status_total(groups: &[SidebarWorktreeGroup], status: AgentStatus) -> usize {
    groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .filter(|count| count.status == status)
        .map(|count| count.count)
        .sum()
}

fn group_header(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    width: usize,
    sealed: bool,
) -> Line<'static> {
    // The catch-all is not a worktree — render it as a dim divider, not a bold
    // pod header, so out-of-project sessions read as "outside the project."
    if group.kind == SidebarWorktreeKind::Workspace {
        return workspace_divider(theme, group, width);
    }
    // The lane spine (added by the caller) opens the header, so the label leads
    // here in bold teal — no inline `▌`, the spine carries the lane. The header
    // builds to the content width left after the gutter cell.
    let cw = content_width(width);
    // The worktree's git story pins right: the `⇡/⇣` commit delta ahead of
    // the `+/-` churn, zero components omitted. The per-worktree status tally
    // is gone: the cockpit owns the fleet make-up and each row carries its own
    // status glyph, so repeating it here was noise. The label clips to
    // whatever's left after the stats claim their width, always leaving a cell
    // so the header never shrinks to zero on extreme narrowness.
    let right = group_git_spans(theme, group);
    let right_width: usize = right.iter().map(|span| span.content.chars().count()).sum();
    let label_width = cw.saturating_sub(right_width + 1).max(1);
    let label_with_prefix = format!("⑂ {}", group.label);
    let left = clip(&label_with_prefix, label_width);
    // The dotted `┄` seal caps only the *selected* worktree's header, so the lane
    // reads as one bracketed block; every other header is just its bold label and
    // right-pinned stats, with plain space filling the gap. Sized to land the line
    // exactly on the content width — a space frames the dotted run from the text
    // on each side it touches.
    let middle = cw.saturating_sub(left.chars().count() + right_width);
    let fill = if sealed {
        match (right.is_empty(), middle) {
            (false, m) if m >= 2 => format!(" {} ", "┄".repeat(m - 2)),
            (true, m) if m >= 1 => format!(" {}", "┄".repeat(m - 1)),
            (_, m) => " ".repeat(m),
        }
    } else {
        " ".repeat(middle)
    };

    let mut spans = vec![
        Span::styled(left, theme.style(Color::Cyan, Modifier::BOLD)),
        Span::styled(fill, theme.faint()),
    ];
    spans.extend(right);
    Line::from(spans)
}

/// The header's right-pinned git cluster. The `⇡/⇣` commit delta leads the
/// `+/-` churn, zero components omitted. Empty when no git read reached this
/// group.
fn group_git_spans(theme: &Theme, group: &SidebarWorktreeGroup) -> Vec<Span<'static>> {
    let mut spans = branch_delta_spans(
        theme,
        group.commits_ahead.unwrap_or(0),
        group.commits_behind.unwrap_or(0),
    );
    let diff = group
        .diff_added
        .zip(group.diff_removed)
        .filter(|(added, removed)| *added + *removed > 0);
    if let Some((added, removed)) = diff {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.extend(diff_spans(theme, added, removed));
    }
    spans
}

/// The `workspace` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// It keeps an *attention-only* tally (`? n` / `! n`) so a waiting script ask
/// still surfaces; the calm counts stay with the cockpit.
fn workspace_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let cw = content_width(width);
    let tally = attention_tally(&group.status_counts);
    let head = format!("┄ {} ", group.label);
    let tail = if tally.is_empty() {
        String::new()
    } else {
        format!(" {tally}")
    };
    let fill = cw
        .saturating_sub(head.chars().count() + tail.chars().count())
        .max(1);
    let mut spans = vec![
        Span::styled(head, theme.faint()),
        Span::styled("┄".repeat(fill), theme.faint()),
    ];
    if !tally.is_empty() {
        spans.push(Span::styled(tail, theme.dim()));
    }
    Line::from(spans)
}

/// An attention-only status tally — a spaced `? n` / `! n` for the waiting and
/// failed counts only. The glyph and count are separated by a space (`? 1`,
/// never `1?`); the calm states are omitted (the cockpit owns the fleet
/// make-up). Empty when nothing in the group needs a human.
fn attention_tally(counts: &[SidebarStatusCount]) -> String {
    counts
        .iter()
        .filter(|count| matches!(count.status, AgentStatus::Waiting | AgentStatus::Failed))
        .filter(|count| count.count > 0)
        .map(|count| format!("{} {}", status_glyph(count.status), count.count))
        .collect::<Vec<_>>()
        .join("  ")
}

/// A just-started agent: idle, sitting on the `Some(0)` baseline context gauge
/// with no real usage behind it yet. Its 0% bar and zeroed stat lines are noise,
/// so the card collapses to identity + description (+ the last-activity age).
fn idle_unstarted(row: &SidebarRow) -> bool {
    matches!(row.status.unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && gauge_percent(row).unwrap_or(0) == 0
}

#[allow(clippy::too_many_arguments)]
fn row_lines(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    width: usize,
    tier: Tier,
    selected: bool,
    animation_phase: u64,
    bands: &ContextSeverityConfig,
    gutter: Gutter,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // The resting (unselected) card is line 1 (identity), line 2 (description),
    // the ctx bar, and the token line. Selection only *appends* the subagent
    // list; it never reshapes a line already on screen, so the card never reflows
    // on expand. The budgets are account-scoped, so they live in the pinned
    // provider dashboard, never on a row.
    let mut inner = vec![identity_line(
        theme,
        row,
        providers,
        tier,
        cw,
        animation_phase,
    )];
    // An active process row carries its full command on a dim second line under
    // the shell anchor — the build or `sudo` install reads in full while line 1
    // stays the stable shell label. Idle process rows have no detail to add.
    if row.row_kind == SidebarRowKind::Process
        && let Some(line) = process_detail_line(theme, row, cw)
    {
        inner.push(line);
    }
    if row.row_kind == SidebarRowKind::Agent {
        inner.push(description_line(theme, row, tier, cw, animation_phase));
        // A just-started idle agent sits on the 0% baseline gauge with nothing
        // behind it, so it rests at identity + description alone. Once an agent
        // has real context, the bar and the context line — the per-card
        // `▤ · ◌ ◍ ↘ ↗` breakdown with the clock-fill last-activity age — join
        // the resting card.
        if !idle_unstarted(row) {
            if let Some(line) = gauge_line(theme, row, bands, cw) {
                inner.push(line);
            }
            if let Some(line) = context_tokens_line(theme, row, bands, cw) {
                inner.push(line);
            }
        }
        // The subagents this agent spawned this turn, listed only in the expanded
        // card — appended after the stats so the resting card never reflows
        // (selection only ever adds lines).
        if selected && !row.sub_agents.is_empty() {
            inner.extend(sub_agent_lines(theme, &row.sub_agents, cw));
        }
    }
    inner
        .into_iter()
        .map(|line| with_gutter(theme, line, gutter))
        .collect()
}

/// The expanded card's subagent list: a dim `⧉ subagents (N)` header, then up to
/// two indented lines per child. Line 1 is the status glyph, the type, and the
/// description of what the parent asked it to do; line 2 (deeper indent) is its
/// token spend `◇` and elapsed work `◷`, pinned right under the parent's own
/// stats. Children are subordinate to the parent card, so every line is dim and
/// indented past the parent's stat lines. The enrichment (description, tokens,
/// elapsed) rides in from Claude's `subagentStatusLine`; a Codex child or one
/// before its first render degrades to the bare type line, with line 2 dropped.
fn sub_agent_lines(
    theme: &Theme,
    sub_agents: &[SidebarSubAgent],
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(trim_spans_to_width(
        vec![Span::styled(
            format!("  {SUBAGENTS_GLYPH} subagents ({})", sub_agents.len()),
            theme.dim(),
        )],
        width,
    ))];
    for sub in sub_agents {
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(status_glyph(sub.status), status_style(theme, sub.status)),
            Span::raw(" "),
            Span::styled(sub.name.clone(), theme.dim()),
        ];
        // Prefer the `subagentStatusLine` description; fall back to the task
        // descriptor, shown only when it differs from the name (the name already
        // is the type for most children) so the line never reads `Explore —
        // Explore`.
        let detail = sub
            .description
            .as_deref()
            .or(sub.task.as_deref().filter(|task| *task != sub.name));
        if let Some(detail) = detail {
            spans.push(Span::styled(format!(" — {detail}"), theme.dim()));
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));

        // Line 2: token spend (left) and elapsed work (right-pinned), drawn only
        // when `subagentStatusLine` reported a positive figure. A deeper indent
        // sets it below the type line; the clock-fill glyph lands under the
        // parent's age and fills with the child's worked span.
        let tokens = sub.total_tokens.filter(|total| *total > 0);
        let elapsed = sub.elapsed_secs.filter(|secs| *secs > 0);
        if tokens.is_some() || elapsed.is_some() {
            let mut left = vec![Span::raw("      ")];
            if let Some(total) = tokens {
                left.extend(tokens_total_spans(theme, total, tokens_short));
            }
            let right = elapsed
                .map(|secs| {
                    metric_spans(
                        theme,
                        elapsed_glyph(secs),
                        Color::Cyan,
                        &compact_seconds(secs),
                    )
                })
                .unwrap_or_default();
            lines.push(pin_right(left, right, width));
        }
    }
    lines
}

/// Width budget for the agent name on line 1: short agent kinds (`claude`,
/// `codex`) fit comfortably, and a longer name clips with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 12;

/// The agent name's style: its provider's brand color (Claude clay, Codex blue,
/// Provider match: the brand color at full weight so the name ties to the
/// provider dashboard. Falls back to mid-gray chrome (no DIM modifier) when no
/// provider matches the kind.
fn agent_name_style(theme: &Theme, providers: &[SidebarProviderPanel], kind: &str) -> Style {
    providers
        .iter()
        .find(|panel| panel.kind == kind)
        .map(|panel| theme.style(Color::Indexed(panel.color), Modifier::empty()))
        .unwrap_or_else(|| theme.style(Color::DarkGray, Modifier::empty()))
}

fn identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    if row.row_kind == SidebarRowKind::Process {
        return process_row_line(theme, row, width, animation_phase);
    }

    if let Some(resolver) = &row.resolver {
        let resolver_name = resolver
            .display_name
            .as_deref()
            .unwrap_or_else(|| resolver.resolver_id.as_str());
        let remaining = resolver
            .budget_until
            .map(time_remaining)
            .unwrap_or_else(|| "?".to_owned());
        // A resolver mid-flight is the one "waiting for an answer" motion: a
        // braille spinner while the resolver composes the decision, bounded by
        // its budget. The resolver + budget fill the slot a task would.
        return composed_row(
            theme,
            Span::styled(
                resolver_glyph(animation_phase),
                status_style(theme, AgentStatus::Waiting),
            ),
            &row.name,
            &format!("{resolver_name} {remaining}"),
            row.last_activity,
            width,
        );
    }

    let status = row.status.unwrap_or(AgentStatus::Idle);
    agent_identity_line(theme, row, providers, status, tier, width, animation_phase)
}

/// The leading status cell for an agent row, applying the two transient render
/// overlays before the base status glyph: a **compacting** head (a violet bar
/// pulsing as the context window condenses) and a **waiting-on-subagents** head
/// (a quiet clay wave while a live child runs). Both are short-lived and stay
/// out of the cockpit tally — they ride over the row's base status here. A
/// human-blocked `?`/`!` always wins, so the overlays defer to those; otherwise
/// the cell is the animated working/thinking fill or the static status glyph.
fn agent_lead_cell(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    animation_phase: u64,
) -> Span<'static> {
    let actionable = matches!(status, AgentStatus::Waiting | AgentStatus::Failed);
    if !actionable && row.compacting {
        return Span::styled(compacting_glyph(animation_phase), compacting_style(theme));
    }
    if status == AgentStatus::Running
        && row
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running)
    {
        return Span::styled(subagent_glyph(animation_phase), subagent_style(theme));
    }
    // A blocked `?`/`!` breathes — a slow brightness pulse via
    // `attention_glyph_style` — to pull the eye back to an unanswered row. It
    // never blanks, so the one-cell column never shifts as it swells and fades.
    Span::styled(
        agent_glyph(status, row.thinking, animation_phase),
        attention_glyph_style(theme, status, age_secs(row.last_activity), animation_phase),
    )
}

/// Line 1 for an agent: the leading cell (the working fill or thinking sparkle
/// while active; a blocked `?`/`!` breathes a slow brightness pulse), the agent
/// name, then the dim capability tokens (`· model · effort · window`) with the
/// bold `$cost` (money-green) pinned right. The window token is the model's
/// context window (`258k`, `1M`) — the statusline/app-server reading first, the
/// hook-derived fallback second, omitted when neither has named it. Capability
/// tokens degrade by width tier: L2 carries model + effort + window, L1 drops
/// effort, L0 keeps just the name — cost always pins right. A blocked `?`/`!`
/// glyph heats through amber to red on the age clock's quarter-hour ramp, so a
/// long-ignored ask escalates without a timestamp.
fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    status: AgentStatus,
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    // Right cluster, built first so the left trims to whatever's left: the
    // session cost, bold in money-green.
    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(cost) = ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
        .map(dollars2)
    {
        right.push(Span::styled(
            cost,
            theme.style(Color::Green, Modifier::BOLD),
        ));
    }

    // Left cluster: glyph + name + dim capability tokens. The glyph heats with
    // the age clock once a `waiting`/`failed` row sits unanswered. The kind name
    // reads at normal weight in the provider's brand color (or mid-gray chrome
    // for unknown kinds); the bright slot is saved for the task below.
    let mut left: Vec<Span<'static>> = vec![
        agent_lead_cell(theme, row, status, animation_phase),
        Span::raw(" "),
        Span::styled(
            clip(&row.name, NAME_MAX),
            agent_name_style(theme, providers, &row.name),
        ),
    ];
    if tier != Tier::L0 {
        if let Some(model) = display_model(row) {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(model, theme.dim()));
        }
        if tier == Tier::L2
            && let Some(effort) = display_effort(row)
        {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(effort.to_owned(), theme.dim()));
        }
        // The window token is always dim chrome — metadata, not a status
        // signal; the context-meter severity ramp owns the loud color slot.
        if let Some(window) = display_context_window(row) {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(
                window_short(window),
                window_style(theme, window),
            ));
        }
    }
    pin_right(left, right, width)
}

/// The model's context window for the identity line (`258k`, `1m`). Prefers the
/// out-of-band runtime reading (Claude's statusline / Codex's app-server — the
/// live truth), falls back to the hook-derived scalar, and omits when neither
/// source has named it.
fn display_context_window(row: &SidebarRow) -> Option<u64> {
    ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.context_window_size)
        .or(row.context_window)
        .filter(|window| *window > 0)
}

/// Line 2 for an agent: the description (the user's session name, else the task,
/// else the prompt) on its own full-width line. An idle agent with nothing to
/// show yet paints the animated loading-dots cue instead; any other empty
/// description falls to an em dash. A turn that died on a provider API error
/// takes the line over the fall-through — the dim upstream error text
/// (`turn_error_label`, quoted verbatim) is the row's most important fact while
/// the `!` escalation holds, and the fall-through returns once it clears. At L2
/// the todo progress (`●●●○○ 3/5`) pins to a right column, aligning under the
/// cost/age above so the dots read as a tidy gutter instead of floating after
/// the text.
fn description_line(
    theme: &Theme,
    row: &SidebarRow,
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    let body = if let Some(label) = row.turn_error_label.as_deref() {
        Span::styled(label.to_owned(), theme.dim())
    } else {
        match descriptor(row) {
            Some(text) => Span::raw(text.to_owned()),
            None if shows_loading_dots(row) => {
                Span::styled(loading_dots(animation_phase).to_owned(), theme.dim())
            }
            None => Span::raw("—".to_owned()),
        }
    };
    let mut left = vec![Span::raw("  "), body];
    // The agent parked its turn on still-in-flight background work: keep the
    // real activity above and add a distinct, faint secondary marker rather than
    // overwriting the description with a synthetic "N background tasks" count.
    if row.parked_on_background {
        left.push(Span::styled("  ⋯ bg", theme.faint()));
    }
    if tier == Tier::L2 && row.todo_total.unwrap_or(0) > 0 {
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        return pin_right(left, todo_spans(theme, done, total), width);
    }
    Line::from(trim_spans_to_width(left, width))
}

/// Pack `left` from the start and pin `right` flush to the trailing edge: trim
/// the left to leave room for the right plus a one-cell gap, then pad the gap so
/// the right cluster ends at `width`. Shared by the identity line and the meter
/// rows so every right-anchored column lands on one edge.
fn pin_right(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    if right.is_empty() {
        return Line::from(trim_spans_to_width(left, width));
    }
    let right_width = spans_width(&right);
    let mut spans = trim_spans_to_width(left, width.saturating_sub(right_width + 1));
    let padding = width
        .saturating_sub(spans_width(&spans) + right_width)
        .max(1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    Line::from(spans)
}

/// The line-2 description: the user's session name when they set one (`--name` /
/// `/rename`), else the agent's live task, else the latest prompt. The name is
/// what a human chose to call this session, so it reads better than the task. The
/// activity-bound `task` clears on idle, so the persisted prompt keeps an unnamed
/// session labelled past its turn until it earns a name. `None` when the session
/// has nothing to show — the caller paints the idle loading-dots or an em dash.
fn descriptor(row: &SidebarRow) -> Option<&str> {
    // The producer sanitizes prompt/task before they reach the row; this is a
    // last-ditch backstop so a harness control turn (`<task-notification>…`)
    // can never paint the description even if a future producer regressed.
    let usable = |value: &str| !value.is_empty() && !looks_like_control_text(value);
    ctx(row)
        .and_then(|context| context.session_name.as_deref())
        .filter(|name| usable(name))
        .or(row.task.as_deref().filter(|task| usable(task)))
        .or(row.prompt.as_deref().filter(|prompt| usable(prompt)))
}

/// Whether an agent row paints the idle loading-dots cue in place of a
/// description — an idle agent with nothing to show yet (no session name, task,
/// or prompt), the "waiting for your first prompt" state. Shared by the renderer
/// (to paint the animated dots) and the serve loop's [`super::has_live_animation`]
/// (to keep the animation tick alive while they cycle).
pub(super) fn shows_loading_dots(row: &SidebarRow) -> bool {
    row.row_kind == SidebarRowKind::Agent
        && matches!(row.status.unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && descriptor(row).is_none()
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text — it leads with one of the synthetic-turn tags. A
/// renderer backstop only; the real guard is `sanitize_user_prompt` in the
/// producer.
fn looks_like_control_text(value: &str) -> bool {
    const CONTROL_TAG_PREFIXES: &[&str] = &[
        "<task-notification>",
        "<system-reminder>",
        "<command-message>",
        "<command-name>",
        "<local-command-stdout>",
    ];
    let trimmed = value.trim_start();
    CONTROL_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
}

/// The session's statusline enrichment, when it published any.
fn ctx(row: &SidebarRow) -> Option<&AgentContext> {
    row.context.as_ref()
}

/// Model name preferred from the statusline (`Opus 4.8 (1M context)`) over the
/// coarser transcript scalar (`Opus`), then shortened for the row
/// (`Opus 4.8 (1M)`); never synthesized.
fn display_model(row: &SidebarRow) -> Option<String> {
    ctx(row)
        .and_then(|context| context.model_display_name.as_deref())
        .or(row.model.as_deref())
        .filter(|model| !model.is_empty())
        .map(model_label)
}

/// Reasoning effort: the hook/ledger value (what the user configured) is
/// preferred; the statusline falls back for sessions that haven't seen a
/// hook-Stop yet. This means a configured `xhigh` shows even when the model
/// caps its effective level to `high` in the statusline.
fn display_effort(row: &SidebarRow) -> Option<&str> {
    row.effort
        .as_deref()
        .or_else(|| ctx(row).and_then(|context| context.effort.as_deref()))
        .filter(|effort| !effort.is_empty())
}

/// The one-cell left gutter that opens every line — blank for chrome and resting
/// worktrees, the resting lane `▏` for the selected worktree, the bold `▌` accent
/// for the selected card itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gutter {
    /// No marker — chrome and non-selected worktrees.
    Blank,
    /// The selected worktree's resting lane spine (`▏`, dim teal).
    Lane,
    /// The selected card's bold accent spine (`▌`).
    Selected,
}

/// Open a line with its one-cell gutter (see [`Gutter`]). The cell is always one
/// column, so changing it never shifts content; the trailing column the content
/// leaves free is the matching right margin. Applied to every line of a worktree
/// group so the lane spans the whole selected worktree as one block, with the
/// selected card lit `▌` inside it. Under `NO_COLOR` the `▏`/`▌` shapes carry the
/// lane and the selection without color.
fn with_gutter(theme: &Theme, line: Line<'static>, gutter: Gutter) -> Line<'static> {
    let cell = match gutter {
        Gutter::Blank => Span::raw(" "),
        Gutter::Lane => Span::styled(LANE_SPINE, theme.style(Color::Cyan, Modifier::DIM)),
        Gutter::Selected => Span::styled(SELECTED_SPINE, theme.selection()),
    };
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(cell);
    spans.extend(line.spans);
    Line::from(spans)
}

fn process_row_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    let dim = theme.dim();
    let label = clip(&row.name, width.saturating_sub(2).max(1));
    // An active pane (a build, a test, a script) gets the running braille spinner
    // so live work reads at a glance; an idle shell or a TUI the user just sits in
    // rests on the same hollow `○` an idle agent shows, so the lead column reads
    // and aligns alike across the two. Both stay in the dim chrome tone, never the
    // agent's clay, so a process stays secondary to an agent.
    let lead = if row.process_active {
        working_glyph(animation_phase)
    } else {
        status_glyph(AgentStatus::Idle)
    };
    Line::from(vec![
        Span::styled(lead, dim),
        Span::raw(" "),
        Span::styled(label, dim),
    ])
}

/// Line 2 for an *active* process row: the full foreground command, dim and
/// indented under the shell anchor, so a build or a `sudo` install reads in full
/// while the primary line keeps the stable shell label. `None` when the producer
/// left no detail (an idle pane, or a command already shown whole on line 1).
fn process_detail_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let detail = row.command_detail.as_deref()?;
    let left = vec![
        Span::raw("  "),
        Span::styled(detail.to_owned(), theme.dim()),
    ];
    Some(Line::from(trim_spans_to_width(left, width)))
}

fn composed_row(
    theme: &Theme,
    lead: Span<'static>,
    name: &str,
    task: &str,
    last_activity: jiff::Timestamp,
    width: usize,
) -> Line<'static> {
    let age = age_short(last_activity);
    let lead_width = 2;
    let name_width = 7;
    let age_width = age.chars().count();
    let fixed = lead_width + name_width + 2 + age_width;
    let task_width = width.saturating_sub(fixed).max(1);
    let name = format!("{:<name_width$}", clip(name, name_width));
    let task = clip(task, task_width);
    let padding = width
        .saturating_sub(lead_width + name.chars().count() + 1 + task.chars().count() + age_width)
        .max(1);

    Line::from(vec![
        lead,
        Span::raw(" "),
        Span::raw(name),
        Span::raw(" "),
        Span::raw(task),
        Span::raw(" ".repeat(padding)),
        Span::styled(age, theme.dim()),
    ])
}

/// Column widths for the per-row context meter: a one-cell lead-glyph label
/// (`▣`, sharing the column with the `◇`/`◷` glyphs on the lines below it) and a
/// fixed 5-cell right value, with the bar filling the middle. The value
/// (`78.2%`) fits five cells. The provider dashboard's budget bars carry their
/// own label/value widths but the same shape.
const BAR_LABEL_WIDTH: usize = 1;
const BAR_VALUE_WIDTH: usize = 5;

/// One aligned meter row: `<indent><label:3> <bar> <value:5>`. The caller's
/// `make_bar` builds the colored bar spans to the supplied width and supplies the
/// `label_style` for the lead glyph (the context meter tints its `▣` with the
/// bar's severity); this helper owns the indent, the fixed label and value
/// columns, and the gaps — so every row built through it shares one bar-start
/// column and one value-end column by construction, with no per-call alignment
/// math. The value column stays dim chrome.
fn bar_row(
    theme: &Theme,
    label: &str,
    label_style: Style,
    value: &str,
    make_bar: impl FnOnce(usize) -> Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    // "  "(2) + label(3) + " "(1) + bar + " "(1) + value(5)
    let bar_width = width
        .saturating_sub(2 + BAR_LABEL_WIDTH + 1 + 1 + BAR_VALUE_WIDTH)
        .max(1);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{label:<BAR_LABEL_WIDTH$}"), label_style),
        Span::raw(" "),
    ];
    spans.extend(make_bar(bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>BAR_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Line::from(trim_spans_to_width(spans, width))
}

/// The context meter — the resting card's one bar. `ctx` on the left, the
/// **percent used** on the right (always — the window *size* moves to the
/// expanded token line), the bar between. The fill amount and its calm-blue →
/// yellow → amber → red severity ([`context_severity_color`], bands from
/// `[sidebar.context]`) come from the used percentage and the absolute tokens;
/// when the statusline reports the per-message token breakdown a *calm* fill is
/// split into colored segments (cache writes / cache reads / fresh input) that
/// add up to exactly that percentage, and a warmed bar goes one solid severity
/// run. The `▣` glyph wears the same severity, so glyph, bar, and the `▤` line
/// below speak one urgency. The value prefers a one-decimal precise fraction
/// (`78.2%`) over the integer gauge. An empty (0%) window reads the hollow
/// `▢`; any usage fills it to `▣`.
fn gauge_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    width: usize,
) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = pct_label(precise_context_pct(row), percent);
    let used = context_used_tokens(row);
    let severity = context_severity_color(percent, used, bands);
    // The severity decides composition-vs-solid: the segments (where the window
    // went) paint only while the meter rests calm-blue; once it warms the bar
    // goes solid severity.
    let segments = (severity == Color::Blue)
        .then(|| gauge_segments(row))
        .flatten();
    let glyph = if percent == 0 {
        CONTEXT_EMPTY_GLYPH
    } else {
        CONTEXT_GLYPH
    };
    Some(bar_row(
        theme,
        glyph,
        theme.style(severity, Modifier::empty()),
        &value,
        |bar_width| match &segments {
            Some(segments) => segmented_gauge_spans(theme, segments, severity, percent, bar_width),
            None => gauge_spans(theme, severity, percent, bar_width),
        },
        width,
    ))
}

/// A precise context-used fraction (0..=100) from the current-message token
/// composition over the window size, so the `ctx` value can read a decimal
/// (`78.2%`). The composition (`input + cache_creation + cache_read`) is exactly
/// what `used_percentage` measures, so the decimal refines the same number.
/// `None` (no breakdown, or no window size) falls the value back to the integer
/// gauge percent.
fn precise_context_pct(row: &SidebarRow) -> Option<f64> {
    let window = ctx(row)?.tokens.as_ref()?.context_window_size? as f64;
    if window <= 0.0 {
        return None;
    }
    let used = context_used_tokens(row)? as f64;
    Some((used / window * 100.0).clamp(0.0, 100.0))
}

/// Tokens currently occupying the context window — the current message's `input
/// + cache_creation + cache_read`. The numerator behind both
/// [`precise_context_pct`] and the absolute-token severity overlay in
/// [`gauge_line`]. `None` when no per-message breakdown was reported.
fn context_used_tokens(row: &SidebarRow) -> Option<u64> {
    let usage = ctx(row)?.tokens.as_ref()?.current_usage.as_ref()?;
    Some(
        usage.input_tokens.unwrap_or(0)
            + usage.cache_creation_input_tokens.unwrap_or(0)
            + usage.cache_read_input_tokens.unwrap_or(0),
    )
}

/// The context bar's value: the statusline's authoritative `used_percentage`
/// when present, else the transcript-derived gauge.
fn gauge_percent(row: &SidebarRow) -> Option<u8> {
    ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.used_percentage)
        .or(row.context_pct)
}

/// The context bar's color segments, when the per-message breakdown is known,
/// left to right: cache writes (yellow), cache reads (blue), fresh `input`
/// (red) — the shared `SEGMENT_*` tones the context line's markers also wear,
/// so the line legends the bar by construction. `None` when no breakdown was
/// reported (a fresh session, post-compact, or a non-Claude agent), so the bar
/// falls back to a single-color ramp.
fn gauge_segments(row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    let usage = ctx(row)?.tokens.as_ref()?.current_usage.as_ref()?;
    let input = usage.input_tokens.unwrap_or(0);
    let writes = usage.cache_creation_input_tokens.unwrap_or(0);
    let reads = usage.cache_read_input_tokens.unwrap_or(0);
    (input + writes + reads > 0).then_some([
        (writes, SEGMENT_CACHE_WRITE),
        (reads, SEGMENT_CACHE_READ),
        (input, SEGMENT_INPUT),
    ])
}

/// The card's context line — `▤` the filled part of the window (integer
/// magnitudes) with the last-activity age pinned right. `▤` is
/// `input + cache_write + cache_read` of the latest API call — exactly the
/// numerator the `▣` meter scales — so the bar's percent and this absolute
/// figure read as one measurement, and the `▤` head wears the bar's severity
/// tone to seal that pairing. A `·` seam separates the headline from the
/// latest call's composition, ordered by how the window filled: `◌` read back
/// from cache, `◍` newly written to it, `↘` fresh input, `↗` output generated
/// (which joins the window next turn) — each marker in its bar-segment color,
/// so the line doubles as the bar's legend. The `◇` totals stay the cockpit /
/// fleet-ledger / subagent vocabulary — this line answers "what is in the
/// window", not "what did today burn". Falls back to the bare `▤` rollup
/// total for an agent whose context carries no per-call token split (Codex's
/// app-server exposes none, and Claude reports none before the first API call
/// and right after `/compact`), so the line shows *something* for every
/// agent. The age rides the right edge only once it crosses a full minute
/// — a just-active agent shows the breakdown alone, left-aligned, rather than
/// a misleading `1m` — as the clock-fill glyph ([`elapsed_glyph`]) over the
/// quarter-stepping age tone ([`activity_age_style`]): dim warm, yellow from
/// the second quarter, amber past the half hour, red from the hour, when
/// resuming would likely re-read the whole context uncached.
fn context_tokens_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    width: usize,
) -> Option<Line<'static>> {
    let age = activity_short(row.last_activity)
        .map(|label| {
            let secs = age_secs(row.last_activity);
            vec![Span::styled(
                format!("{} {label}", elapsed_glyph(secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default();
    // The `▤` head mirrors the bar's severity — same inputs, same ramp — so the
    // absolute figure and the meter above it read at one urgency. A row with no
    // gauge percent folds to 0 and lets the token overlay alone speak.
    let severity = context_severity_color(
        gauge_percent(row).unwrap_or(0),
        context_used_tokens(row),
        bands,
    );
    if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_usage.as_ref())
    {
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let mut left = vec![Span::raw("  ")];
        left.extend(context_breakdown_spans(
            theme,
            severity,
            input + cache_write + cache_read,
            cache_read,
            cache_write,
            input,
            output,
            tokens_int,
        ));
        return Some(pin_right(left, age, width));
    }
    let total = row.total_tokens?;
    let mut left = vec![Span::raw("  ")];
    left.extend(context_total_spans(theme, severity, total, tokens_int));
    Some(pin_right(left, age, width))
}

/// Total display width of a span run, in terminal cells.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

fn trim_spans_to_width(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut remaining = width;
    let mut trimmed = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let span_width = span.content.chars().count();
        if span_width <= remaining {
            remaining -= span_width;
            trimmed.push(span);
            continue;
        }
        let content = span.content.chars().take(remaining).collect::<String>();
        if !content.is_empty() {
            trimmed.push(Span::styled(content, span.style));
        }
        break;
    }
    trimmed
}

/// The provider dashboard's fixed art column width: the brand emblem is padded
/// to this many cells so the stats/bar column to its right starts at one shared
/// cell for every provider block — the bars align across providers by
/// construction. Dropped (bars run full-width) below [`PROVIDER_ART_MIN_WIDTH`].
const PROVIDER_ART_WIDTH: usize = 9;

/// Narrowest sidebar that still affords the art column beside a bar; below it
/// the emblem is dropped so the bar keeps a legible length.
const PROVIDER_ART_MIN_WIDTH: usize = 34;

/// The provider bar's label slot (`5h` / `7d` / `30d` / `∞`) and reset-value
/// column, shared by every provider bar so they align front and back. The label
/// fits three cells (`30d`); the value holds `↻ ` plus a two-unit reset countdown
/// (up to `↻ 30d10h`).
const PROVIDER_LABEL_WIDTH: usize = 3;
const PROVIDER_VALUE_WIDTH: usize = 8;

/// How close to a full window-length a reset must read to count as "not started".
/// A not-started window keeps its reset slid to `now + duration`, but a live
/// reading lands a hair under (minute-flooring + read latency) and a cached one
/// drifts down until the next refresh — so allow this margin below the full
/// window. A *started* window's reset has ticked well below full, so it clears
/// the margin easily.
const NOT_STARTED_GRACE: SignedDuration = SignedDuration::from_secs(120);

/// A brighter sage than the resting money-green, held for a couple of frames as a
/// figure lands — the quiet "ka-chunk" of the climb. Drops to plain bold under
/// `NO_COLOR` like every other tone.
const VALUE_FLASH: Color = Color::Indexed(150);

/// The fleet ledger rows pinned to the bottom of the dashboard: the trailing
/// week (`W:`) and month (`M:`), each reading `◎ sessions  ◇ ↘ ↗ ◌  $spend`
/// across every provider (today's headline lives in the cockpit, so these climb
/// `week → month`). The token figures read the precise one-decimal form (`16.5k`)
/// — the ledger is the exact record next to the cockpit's coarse live read — with
/// the `◇` total in violet (matching the cards) and the `$` bold money-green; the
/// spend deliberately does **not** animate (only today's headline does). Both
/// rows share one set of right-aligned column widths so the labels stack and
/// every number column lines up. Empty (dropped) until something is recorded.
pub(super) fn fleet_ledger_lines(
    theme: &Theme,
    tally: Option<&SpendTally>,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(tally) = tally.filter(|t| !t.is_zero()) else {
        return Vec::new();
    };
    let cols = WmColumns::measure(&tally.week, &tally.month);
    vec![
        wm_row(theme, "W", &tally.week, &cols, width),
        wm_row(theme, "M", &tally.month, &cols, width),
    ]
}

/// The shared right-aligned column widths for the `W:`/`M:` ledger rows, measured
/// across both windows so a 2- and a 3-digit figure stack on one right edge.
struct WmColumns {
    sessions: usize,
    total: usize,
    input: usize,
    output: usize,
    cache_read: usize,
    usd: usize,
}

impl WmColumns {
    fn measure(week: &SpendWindow, month: &SpendWindow) -> Self {
        let max2 = |a: String, b: String| a.chars().count().max(b.chars().count());
        Self {
            sessions: max2(week.sessions.to_string(), month.sessions.to_string()),
            total: max2(tokens_short(week.tokens), tokens_short(month.tokens)),
            input: max2(tokens_short(week.input), tokens_short(month.input)),
            output: max2(tokens_short(week.output), tokens_short(month.output)),
            cache_read: max2(
                tokens_short(week.cache_read),
                tokens_short(month.cache_read),
            ),
            usd: max2(dollars2(week.usd), dollars2(month.usd)),
        }
    }
}

/// One ledger row — `W: ◎ {sessions}  ◇ {total} ↘ {in} ↗ {out} ◌ {cache_read}`
/// left-clustered, the `$ {spend}` pinned to the right edge. Every numeric field
/// is right-aligned to the shared [`WmColumns`] width, so the `W:` and `M:` rows
/// stack into one tidy grid. The `◍` cache-write field is intentionally omitted
/// here — the ledger keeps to the four headline figures the all-time read needs.
fn wm_row(
    theme: &Theme,
    label: &str,
    window: &SpendWindow,
    cols: &WmColumns,
    width: usize,
) -> Line<'static> {
    let dim = theme.dim();
    let left = vec![
        Span::styled(format!("{label}: "), theme.faint()),
        Span::styled(SESSIONS_GLYPH, theme.style(Color::Cyan, Modifier::empty())),
        Span::styled(format!(" {:>w$}", window.sessions, w = cols.sessions), dim),
        Span::raw("  "),
        Span::styled(TOKENS_TOTAL, theme.style(Color::Magenta, Modifier::empty())),
        Span::styled(
            format!(" {:>w$}", tokens_short(window.tokens), w = cols.total),
            dim,
        ),
        Span::styled(
            format!(
                " {TOKENS_IN} {:>w$}",
                tokens_short(window.input),
                w = cols.input
            ),
            dim,
        ),
        Span::styled(
            format!(
                " {TOKENS_OUT} {:>w$}",
                tokens_short(window.output),
                w = cols.output
            ),
            dim,
        ),
        Span::styled(
            format!(
                " {TOKENS_CACHED} {:>w$}",
                tokens_short(window.cache_read),
                w = cols.cache_read
            ),
            dim,
        ),
    ];
    let right = vec![Span::styled(
        format!("{:>w$}", dollars2(window.usd), w = cols.usd),
        theme.style(Color::Green, Modifier::BOLD),
    )];
    pin_right(left, right, width)
}

/// The pinned per-provider dashboard: one block per provider (`Claude`,
/// `Codex`, …), each a header line then the brand emblem zipped against the
/// aggregate stats and the account-scoped budget bars. A metered account drains
/// one "mana" bar per budget window toward its reset; an unmetered (API-key)
/// account shows the `∞` "infinite power" bar in the label slot with no countdown. The bars
/// share one start and one end column across every block, so the whole
/// dashboard reads as one aligned grid. Bottom chrome — never a jump target.
pub(super) fn provider_panel_lines(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, panel) in providers.iter().enumerate() {
        // A blank line sets each provider block apart, so two providers read as
        // two distinct cards rather than one dense slab.
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(provider_header_line(theme, panel, width));
        // A blank line below the provider name sets the identity apart from the
        // emblem + stats body, matching the cockpit's breathing room.
        lines.push(Line::from(""));
        lines.extend(provider_body_lines(theme, panel, width));
    }
    lines
}

/// `Claude v2.1.158 · Claude Max          ⇅ rc`: the product name in the
/// brand color and the version + plan dim on the left, with the violet `⇅ rc`
/// flag pinned to the top-right corner of the block when remote control is on for
/// the provider. Fields drop out when unknown.
fn provider_header_line(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Line<'static> {
    let mut left = vec![Span::styled(
        panel.product_name.clone(),
        theme.style(Color::Indexed(panel.color), Modifier::BOLD),
    )];
    if let Some(version) = panel.version.as_deref() {
        left.push(Span::styled(format!(" v{version}"), theme.dim()));
    }
    if let Some(plan) = panel.plan.as_deref() {
        left.push(Span::styled(" · ", theme.faint()));
        left.push(Span::styled(plan.to_owned(), theme.dim()));
    }
    let right = if panel.remote_control {
        vec![Span::styled(
            "⇅ rc",
            theme.style(Color::Magenta, Modifier::BOLD),
        )]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}

/// The block beneath the header: the brand emblem in a fixed left column zipped
/// against the right column — aggregate stats on the first line, the budget bars
/// below. The art is dropped (and the bars run full width) when the sidebar is
/// too narrow to fit both.
fn provider_body_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Vec<Line<'static>> {
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_column = if show_art { PROVIDER_ART_WIDTH + 1 } else { 0 };
    let bar_region = width.saturating_sub(art_column);

    // The right column, top to bottom: aggregate stats then the budget bars,
    // packed directly so the three rows line up against the three-line emblem and
    // the bars sit right under the numbers (no separator row).
    let mut rights: Vec<Vec<Span<'static>>> = vec![provider_stats_spans(theme, panel, bar_region)];
    rights.extend(provider_bar_rows(theme, panel, bar_region));

    let rows = panel.art.len().max(rights.len());
    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_art {
            let art_line = panel.art.get(index).map(String::as_str).unwrap_or("");
            spans.push(Span::styled(
                pad_to(art_line, PROVIDER_ART_WIDTH),
                theme.style(Color::Indexed(panel.color), Modifier::empty()),
            ));
            spans.push(Span::raw(" "));
        }
        if let Some(right) = rights.get(index) {
            spans.extend(right.iter().cloned());
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));
    }
    lines
}

/// The provider's aggregate stats line beside the emblem: today's token
/// breakdown `◇ ↘ ↗ ◍ ◌` (integer magnitudes) on the left, the bold money-green
/// spend pinned to the right edge of the bar `region`. Always rendered — an idle
/// account reads `◇ 0 …  $0.00` so the line above the budget bars is never blank.
/// Every figure is today's transcript-history burn for this provider, summed
/// across every session from the JSONL — the accurate cross-session total, and
/// the only cost source for token-only providers like Codex. The summed `+/-`
/// churn is intentionally absent — a noisy per-account aggregate; per-worktree
/// churn lives on the group headers and per-agent churn on the work line.
fn provider_stats_spans(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
) -> Vec<Span<'static>> {
    let today = panel
        .spending
        .as_ref()
        .map(|spending| spending.today)
        .unwrap_or_default();
    let left = token_breakdown_spans(
        theme,
        today.tokens,
        today.input,
        today.output,
        today.cache_write,
        today.cache_read,
        tokens_int,
        true,
    );
    let right = vec![Span::styled(
        dollars2(today.usd),
        theme.style(Color::Green, Modifier::BOLD),
    )];
    pin_right(left, right, region).spans
}

/// The provider's budget bars within `region`: a metered account drains one
/// "mana" bar per reported window (`5h`, `7d`, `30d`, …, ordered short→long);
/// an unmetered account shows the single `∞` bar. Each reset reads a two-unit
/// countdown scaled to its magnitude. Each row aligns front and back within
/// `region`, so they line up across providers too.
fn provider_bar_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
) -> Vec<Vec<Span<'static>>> {
    if !panel.metered {
        return vec![infinite_bar_row(theme, panel.color, region)];
    }
    panel
        .windows
        .iter()
        .filter_map(|window| {
            metered_bar_row(theme, window, region, longer_window_spent(panel, window))
        })
        .collect()
}

/// Whether a window with a strictly longer duration is spent — a higher-level cap
/// being exhausted gates this shorter window (its budget is unusable until the
/// longer one resets), so the renderer paints the shorter row exhausted too (e.g.
/// a spent 7-day cap gating the 5-hour bar).
fn longer_window_spent(panel: &SidebarProviderPanel, window: &RateLimitWindow) -> bool {
    let mins = window.duration_mins.unwrap_or(0);
    panel.windows.iter().any(|other| {
        other.duration_mins.unwrap_or(0) > mins
            && other.used_percentage.is_some_and(|used| used >= 100)
    })
}

/// Whether a window has not started its clock. These budgets are sliding windows:
/// the provider keeps `resets_at` slid a full window-length ahead until the first
/// token, so a reset still within [`NOT_STARTED_GRACE`] of the full window means
/// the clock hasn't begun — the displayed countdown would be a placeholder.
///
/// The not-started floor is ~1% used (a fresh 5h window reads `usedPercent: 1`,
/// never 0), so detection keys on the reset distance, not a 0% reading. Any usage
/// **above** that floor means the window has clearly started — its reset is then a
/// real countdown — so >1% short-circuits to "started" regardless of the reset
/// (this also covers a spent window at 100%). An absent reset or duration can't be
/// judged, so it isn't flagged.
fn window_not_started(window: &RateLimitWindow) -> bool {
    if window.used_percentage > Some(1) {
        return false;
    }
    let (Some(reset), Some(mins)) = (window.resets_at, window.duration_mins) else {
        return false;
    };
    let full = SignedDuration::from_secs(i64::from(mins) * 60);
    reset.duration_since(Timestamp::now()) >= full - NOT_STARTED_GRACE
}

/// One metered budget bar row: the window's label (`5h`/`7d`/`30d`), the draining
/// mana bar (filled = remaining), and the `↻ <reset>` countdown right-aligned in
/// the value column. The label mirrors its bar's severity color. `force_exhausted`
/// paints the row as fully spent — red, no countdown — regardless of the window's
/// own reading (a longer spent window gates it). `None` when the window reported
/// no usage percentage and is not force-exhausted.
///
/// A window that has **not started** drops its countdown — a full bar with no
/// `↻` reads "send a message to start it" rather than a misleading ticking reset.
/// These are sliding windows that begin counting only on the first token, so until
/// then the provider keeps `resets_at` slid a full window-length ahead. Detect that
/// by the reset distance ([`window_not_started`]), not a 0% reading — a fresh 5h
/// window still reports ~1% used, never 0. Codex reports a placeholder usedPercent
/// (~99) with no `resets_at` before the first token; that variant is caught by the
/// absent-reset + known-duration check in the `remaining` computation below.
fn metered_bar_row(
    theme: &Theme,
    window: &RateLimitWindow,
    region: usize,
    force_exhausted: bool,
) -> Option<Vec<Span<'static>>> {
    let not_started = !force_exhausted && window_not_started(window);
    let remaining = if force_exhausted {
        0
    } else {
        let raw = 100u8.saturating_sub(window.used_percentage?);
        // Codex reports a placeholder usedPercent (≈99) with no resetsAt before the
        // first token and a known duration — normalise to full so the bar matches
        // the empty countdown.
        if not_started || (window.resets_at.is_none() && window.duration_mins.is_some() && raw > 0)
        {
            100
        } else {
            raw
        }
    };
    let label = window_label(window.duration_mins);
    let value = if force_exhausted || not_started {
        String::new()
    } else {
        window
            .resets_at
            .map(|at| format!("↻ {}", reset_countdown(at)))
            .unwrap_or_default()
    };
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{label:<PROVIDER_LABEL_WIDTH$}"),
            theme.style(mana_color(remaining), Modifier::empty()),
        ),
        Span::raw(" "),
    ];
    spans.extend(mana_bar_spans(theme, remaining, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>PROVIDER_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Some(spans)
}

/// The unmetered `∞` bar row: the infinity icon rides the label slot (aligned
/// with `5h`/`7d`), then the full brand-colored infinite bar. The value column
/// is reserved but empty — no countdown — so the bar's right edge still aligns
/// with the metered bars'.
fn infinite_bar_row(theme: &Theme, color: u8, region: usize) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{:<PROVIDER_LABEL_WIDTH$}", "∞"),
            theme.style(Color::Indexed(color), Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(infinite_bar_spans(theme, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(" ".repeat(PROVIDER_VALUE_WIDTH)));
    spans
}

/// The bar's cell width inside a provider `region`: the region less the label,
/// the value column, and the two single-cell gaps that frame the bar. At least
/// one cell, so a narrow sidebar still paints a (short) bar.
fn provider_bar_width(region: usize) -> usize {
    region
        .saturating_sub(PROVIDER_LABEL_WIDTH + 1 + 1 + PROVIDER_VALUE_WIDTH)
        .max(1)
}

/// Pad (or clip) a string to exactly `width` terminal cells — the fixed art
/// column, so the right column starts at one shared cell for every block.
fn pad_to(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count >= width {
        value.chars().take(width).collect()
    } else {
        let mut padded = value.to_owned();
        padded.extend(std::iter::repeat_n(' ', width - count));
        padded
    }
}
