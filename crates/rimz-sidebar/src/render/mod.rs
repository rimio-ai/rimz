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

mod fmt;
mod labels;
mod odometer;
mod sections;
mod theme;

pub(crate) use odometer::TallyAnim;

use std::io::{self, Write};

use jiff::Timestamp;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
use rimz::ids::PaneId;
use rimz::{SidebarRowKind, SidebarSnapshot};

use self::fmt::age_short;
use self::sections::{
    cockpit_spend_line, cockpit_summary_line, content_width, first_run_hint_lines,
    fleet_header_lines, fleet_ledger_lines, fleet_size, provider_panel_lines, worktree_group_lines,
};
use self::theme::Theme;

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
    /// Wall-clock animation frame counter, advanced by the serve loop's
    /// animation tick. The renderer derives the running-agent spin frame from
    /// it; freshness gating (per row) keeps a quiet agent frozen.
    pub animation_phase: u64,
    /// The cockpit spend's count-up state — one eased roll for today's `$`.
    /// Folded forward on each data refresh (`TallyAnim::observe`) and read by the
    /// renderer at `animation_phase`; the serve loop keeps the fast tick alive
    /// while a roll is in flight. Crate-internal: an implementation detail of the
    /// renderer, not part of the public `UiState` surface.
    pub(crate) tally: TallyAnim,
    /// Hit-test map of the most recently drawn frame: one entry per inner-area
    /// content line, `Some(row)` for a jump-target row line (in
    /// `app::visible_rows()` order) and `None` for chrome. The renderer writes
    /// it as a byproduct of every draw; the mouse hit-test reads it. Empty
    /// before the first draw.
    pub line_map: Vec<Option<usize>>,
    /// The pane the highlight is pinned to — selection keyed by identity, not
    /// position. Re-derived each fold by `app::reconcile_selection` from the
    /// derived `baseline_pane` and any live `browse`. Keying on the pane means
    /// a status-churn reorder re-anchors the highlight to the same pane
    /// instead of sliding it onto a neighbour.
    pub selected_pane: Option<PaneId>,
    /// The hold-last derived baseline: the own view's active working pane from
    /// the last frame that reported one. Selection is *derived* — recomputed
    /// from the queried mux state every fold, so it is same-tab by construction
    /// and can never desynchronize, only lag a frame. It advances on a `Some`
    /// derivation and holds across a `None` (the sidebar itself is the view's
    /// active pane, or the active pane is not a row).
    pub(crate) baseline_pane: Option<PaneId>,
    /// The transient arrow-key browse pick riding above the baseline, or `None`
    /// when not browsing (see [`Browse`]).
    pub(crate) browse: Option<Browse>,
}

/// Arrow-key browse: pins `pane` WITHOUT moving focus, roaming every visible
/// row — other tabs' rows included, so any card is one keystroke from
/// expanding. Holds until the derived baseline genuinely changes from
/// `baseline_at_start` — the value captured when browsing began. A `None`
/// derivation holds the baseline, so an inert frame never ends a browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Browse {
    pub(crate) pane: PaneId,
    pub(crate) baseline_at_start: Option<PaneId>,
}

/// A sticky health alert pinned to the bottom of the sidebar.
///
/// `since` is when the unhealthy episode began, so an active alert can show
/// `for Ns`. `recovered_at` is `None` while the loop is still unhealthy and
/// `Some(t)` once it healed — a recovered alert lingers as a dismissable
/// "last alert" notice rather than vanishing the instant a fetch succeeds.
#[derive(Clone, Debug)]
pub struct Alert {
    pub reason: String,
    pub since: Timestamp,
    pub recovered_at: Option<Timestamp>,
}

impl Alert {
    pub fn active(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            since: Timestamp::now(),
            recovered_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.recovered_at.is_none()
    }
}

pub fn draw(frame: &mut Frame<'_>, snapshot: &SidebarSnapshot, alert: Option<&Alert>) {
    draw_with_ui(frame, snapshot, alert, &mut UiState::default());
}

/// The fastest animation class currently visible in the snapshot. Fast motion
/// changes every frame (working/thinking spinners, resolver work, active process
/// rows). Slow motion is cosmetic attention/loading movement whose visible
/// state is held for several base frames, so the serve loop can redraw it less
/// often without making the sidebar feel stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationCadence {
    None,
    Slow,
    Fast,
}

/// Whether any visible row is in an animated state — a running agent (working
/// or pre-edit thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), an attention row whose `?`/`!`
/// glyph breathes, or an idle agent showing the loading-dots cue. The serve
/// loop uses this as the broad "does anything move?" gate; [`animation_cadence`]
/// decides whether the movement needs the fast frame grid or the slower
/// cosmetic cadence. A fully settled sidebar (only quiet idle/done rows) keeps
/// idling on the slow data tick. A stalled agent is projected to `failed`
/// upstream, so it reads as a breathing `!` here. The cockpit's today-spend
/// count-up rides a separate gate (`UiState::tally`), so a finished-turn climb
/// keeps the tick alive even when every row is otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    animation_cadence(snapshot) != AnimationCadence::None
}

pub fn animation_cadence(snapshot: &SidebarSnapshot) -> AnimationCadence {
    use rimz::feed::AgentStatus;
    let mut slow = false;
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
    {
        match row.row_kind {
            SidebarRowKind::Agent => {
                if row.resolver.is_some() || row.status == Some(AgentStatus::Running) {
                    return AnimationCadence::Fast;
                }
                // `?`/`!` breathe to pull the eye back to an unanswered row —
                // quickening with age up to the red blink, which flips every
                // 300ms by design so it samples cleanly on this grid — and the
                // idle "waiting for a prompt" loading-dots cue cycles in
                // place. None of it needs a 10fps full-frame redraw.
                if matches!(row.status, Some(AgentStatus::Waiting | AgentStatus::Failed))
                    || sections::shows_loading_dots(row)
                {
                    slow = true;
                }
            }
            SidebarRowKind::Process if row.process_active => return AnimationCadence::Fast,
            SidebarRowKind::Process => {}
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
    // The composed map is a byproduct of the draw: store it so the mouse
    // hit-test reads the geometry of the frame the user is actually looking at.
    let (lines, map) = compose_lines(snapshot, alert, ui, area.width, area.height);
    ui.line_map = map;
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

/// Lay out the body, then pin the bottom chrome to the bottom edge of the
/// viewport like a status bar: the centered navigation footer, and beneath it
/// the sticky health alert. Space for the bottom block is always reserved — the
/// body is truncated before it is ever clipped — so the footer and notice can
/// never scroll off the bottom of a full sidebar. While an alert is *active* the
/// body is a stale/empty fetch, so the footer steps aside and the alert speaks
/// alone.
///
/// Returns the composed body *and* a parallel hit-test map of equal length:
/// entry `i` is the visible row index that on-screen content line `i` belongs
/// to (`app::visible_rows()` order), or `None` for structural lines (cockpit
/// header, gaps, the external divider, `+K more`, help, footer, alert); a
/// worktree header routes to the row it jumps into. The map is the single
/// authority on row geometry — built from the same final line vector that is
/// rendered, so it stays 1:1 with what the user sees through every clip.
pub(crate) fn compose_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    // `NO_COLOR` can't change mid-process, so read the palette once per frame
    // and hand the same `Theme` to the body and the bottom chrome.
    let theme = Theme::from_env();
    let cells = usize::from(width.max(1));
    // The whole sidebar sits inside a one-cell frame: chrome is built to the inner
    // width and opened with a blank gutter, leaving the trailing column as the
    // matching right margin — the same frame the cards carry (see `with_gutter`).
    let inner = content_width(cells);
    let (mut body, mut map) = snapshot_lines(snapshot, alert, ui, cells, &theme);

    // Bottom-pinned chrome, top to bottom: the per-provider dashboard (account-
    // scoped budgets + brand emblem), the navigation footer (centered), then the
    // sticky health alert — each bracketed by a faint hairline rule. While an
    // alert is active the body is a stale/empty fetch, so the panel and footer
    // step aside and the alert speaks alone. Every chrome line is gutter-padded so
    // it breathes in the same one-cell frame as the body.
    let active = alert.is_some_and(Alert::is_active);
    let mut bottom: Vec<Line<'static>> = Vec::new();
    let dashboard_present = !active && !snapshot.providers.is_empty();
    if dashboard_present {
        bottom.push(pad_chrome(hairline_rule(&theme, inner)));
        bottom.extend(
            provider_panel_lines(&theme, &snapshot.providers, inner)
                .into_iter()
                .map(pad_chrome),
        );
    }
    // The fleet ledger — the static `W:`/`M:` week/month rows — seals the bottom
    // of the dashboard. It rides under the dashboard's blank-line block separator
    // when an account block is present, else carries its own hairline so it never
    // floats unsealed against the body.
    if !active {
        let corner = fleet_ledger_lines(&theme, snapshot.value_tally.as_ref(), inner);
        if !corner.is_empty() {
            if dashboard_present {
                bottom.push(Line::from(""));
            } else {
                bottom.push(pad_chrome(hairline_rule(&theme, inner)));
            }
            bottom.extend(corner.into_iter().map(pad_chrome));
        }
    }
    if !active {
        let footer = footer_lines(snapshot, &theme, inner);
        if !footer.is_empty() {
            // No rule above the footer — it sits quietly under the dashboard's own
            // top rule, with one blank line of breathing room when a dashboard is
            // present (skipped in an empty room so the footer doesn't float).
            if !bottom.is_empty() {
                bottom.push(Line::from(""));
            }
            bottom.extend(footer.into_iter().map(pad_chrome));
        }
    }
    if let Some(alert) = alert {
        bottom.extend(alert_lines(&theme, alert).into_iter().map(pad_chrome));
    }
    if bottom.is_empty() {
        return (body, map);
    }

    let height = usize::from(height);
    let bottom_height = bottom
        .iter()
        // Every line occupies at least one row — a blank separator has width 0
        // but still takes a row, so `.max(1)` keeps the reservation honest and
        // the footer from being pushed off the frame.
        .map(|line| line.width().div_ceil(cells).max(1))
        .sum::<usize>()
        .min(height);

    let max_body = height.saturating_sub(bottom_height);
    if body.len() > max_body {
        body.truncate(max_body);
        map.truncate(max_body);
    }
    let pad = height.saturating_sub(body.len() + bottom_height);
    body.extend(std::iter::repeat_n(Line::from(""), pad));
    map.extend(std::iter::repeat_n(None, pad));
    // The footer and alert are pinned chrome, never jump targets: one `None`
    // per line.
    map.extend(std::iter::repeat_n(None, bottom.len()));
    body.extend(bottom);
    (body, map)
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

/// Compose the sidebar body and, in lockstep, the hit-test map: every content
/// line gets one map entry, `Some(row)` for an agent/process row line and the
/// worktree header that jumps into it, `None` for structural chrome (cockpit
/// header, gaps, the external divider, first-run hint, help, `+K more`). The
/// footer and alert are pinned to the bottom by [`compose_lines`], not here. The
/// two vectors stay equal length and same order so [`compose_lines`] can hand
/// the map straight to the hit-test.
fn snapshot_lines(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    ui: &UiState,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    // An *active* alert means the body is a stale/empty fetch, not a live room:
    // suppress the first-run hint, footer, and help so the alert speaks alone.
    // A recovered alert is just a lingering notice — the room below it is live.
    let active = alert.is_some_and(Alert::is_active);
    let mut lines = Vec::new();
    let mut map: Vec<Option<usize>> = Vec::new();

    // The whole sidebar is built one cell narrow on each side; chrome lines pick
    // up their blank gutter in `extend_inert`, the cards carry their own.
    let inner = content_width(width);

    // Borderless repo header: the workspace name and its path behind their
    // glyphs, a blank line, then the cockpit summary and spend lines and a faint
    // hairline rule sealing the header from the cockpit. Inert chrome, so every
    // line maps to `None`.
    let mut header = repo_header_lines(theme, snapshot, inner);
    // A blank line sets the repo identity apart from the cockpit summary below.
    header.push(Line::from(""));
    // The cockpit summary, two lines: line 1 is `◎` sessions today on the left
    // with today's accumulated token breakdown pinned right — both halves read
    // today's window; line 2 is `¤` live agents on the left with today's spend
    // pinned right. The counts read from the live fleet and the JSONL
    // `value_tally`'s today window, so the cockpit reflects all of today's
    // sessions rather than only the live statusline sum.
    let today = snapshot.value_tally.as_ref().map(|tally| &tally.today);
    let sessions = today.map(|window| window.sessions).unwrap_or(0);
    header.push(cockpit_summary_line(theme, sessions, today, inner));
    // Line 2 is always present — an empty room reads `¤ 0` — with the spend
    // joining the right edge and counting up as a turn lands.
    let live_agents = fleet_size(&snapshot.worktree_groups).0;
    let today_usd = today.map(|window| window.usd).unwrap_or(0.0);
    header.push(cockpit_spend_line(
        theme,
        live_agents,
        today_usd,
        &ui.tally,
        ui.animation_phase,
        inner,
    ));
    header.push(hairline_rule(theme, inner));
    extend_inert(&mut lines, &mut map, header);

    // The fleet header (the cockpit make-up line) is always present and a fixed
    // height — one line for a populated room, none for an empty one — so the body
    // below never shifts vertically as agents change state. It is chrome, never a
    // jump target, so every header line maps to `None`.
    extend_inert(
        &mut lines,
        &mut map,
        fleet_header_lines(theme, &snapshot.worktree_groups, inner),
    );
    if snapshot.worktree_groups.is_empty() {
        if !active && should_show_first_run_hint(snapshot) {
            push_section_gap(&mut lines, &mut map);
            extend_inert(
                &mut lines,
                &mut map,
                first_run_hint_lines(theme, snapshot.agent_hooks_ready),
            );
        }
    } else {
        push_section_gap(&mut lines, &mut map);
        let mut row_index = 0;
        for (index, group) in snapshot.worktree_groups.iter().enumerate() {
            if index > 0 {
                lines.push(Line::from(""));
                map.push(None);
            }
            worktree_group_lines(
                theme,
                group,
                &snapshot.providers,
                width,
                &snapshot.sidebar.context,
                &mut row_index,
                ui.selected_index,
                ui.animation_phase,
                &mut lines,
                &mut map,
            );
        }
        if !active && should_show_first_run_hint(snapshot) {
            lines.push(Line::from(""));
            map.push(None);
            extend_inert(
                &mut lines,
                &mut map,
                first_run_hint_lines(theme, snapshot.agent_hooks_ready),
            );
        }
        if ui.help_visible && !active {
            lines.push(Line::from(""));
            map.push(None);
            extend_inert(&mut lines, &mut map, help_lines());
        }
    }

    (lines, map)
}

/// Append structural (non-row) lines, tagging each map slot `None` and opening
/// each with a blank one-cell gutter so chrome breathes in the same one-cell
/// frame as the cards (which carry their own gutter via `with_gutter`).
fn extend_inert(
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
    inert: Vec<Line<'static>>,
) {
    map.extend(std::iter::repeat_n(None, inert.len()));
    lines.extend(inert.into_iter().map(pad_chrome));
}

/// Open a chrome line with the same one-cell blank left gutter the cards carry,
/// so the whole sidebar sits inside a one-cell frame — the trailing column the
/// content leaves free is the matching right margin. A genuinely empty line (a
/// blank separator, or the cockpit's reserved-but-empty totals slot) is left as
/// is, so it stays zero-width and never reads as a one-space "content" line that
/// would trip the section-gap heuristic.
fn pad_chrome(line: Line<'static>) -> Line<'static> {
    if line.spans.iter().all(|span| span.content.is_empty()) {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" "));
    spans.extend(line.spans);
    Line::from(spans)
}

/// The borderless repo header (dashboard L1): the workspace name behind a `⌘`
/// glyph in bold on the left, and — when the project root is known — its
/// home-abbreviated path dim on the right edge of the same line. Identity and
/// location at a glance, on one line so the spend line can sit below it. The
/// path left-truncates with a leading `…` (keeping the meaningful tail) when it
/// can't fit, so the name is never crowded out.
fn repo_header_lines(
    theme: &Theme,
    snapshot: &SidebarSnapshot,
    width: usize,
) -> Vec<Line<'static>> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let clip = |text: &str| -> String { text.chars().take(width.max(1)).collect() };
    let name = clip(&format!("⌘ {}", snapshot.display_name));
    let name_width = name.chars().count();

    let Some(root) = snapshot.project_root.as_deref() else {
        return vec![Line::styled(name, bold)];
    };
    let path = abbreviate_home(&root.to_string_lossy());
    let path_budget = width.saturating_sub(name_width + 1);
    if path_budget == 0 {
        return vec![Line::styled(name, bold)];
    }
    let path = truncate_left(&path, path_budget);
    let gap = width
        .saturating_sub(name_width + path.chars().count())
        .max(1);
    vec![Line::from(vec![
        Span::styled(name, bold),
        Span::raw(" ".repeat(gap)),
        Span::styled(path, theme.dim()),
    ])]
}

/// Truncate `text` from its left to fit `budget` cells, marking the cut with a
/// leading `…` so the meaningful tail (`…engine/main`) survives. Shorter text
/// passes through unchanged.
fn truncate_left(text: &str, budget: usize) -> String {
    let len = text.chars().count();
    if len <= budget {
        return text.to_owned();
    }
    if budget <= 1 {
        return "…".chars().take(budget).collect();
    }
    let tail: String = text.chars().skip(len - (budget - 1)).collect();
    format!("…{tail}")
}

/// Abbreviate a leading `$HOME` to `~` for the path line, so a deep home path
/// reads `~/code/query-engine` rather than spilling the absolute prefix.
fn abbreviate_home(path: &str) -> String {
    let home = std::env::var_os("HOME").map(|home| home.to_string_lossy().into_owned());
    abbreviate_under(path, home.as_deref())
}

/// The pure core of [`abbreviate_home`]: collapse a leading `home` prefix to
/// `~`. A path outside `home`, or with no `home`, passes through unchanged.
fn abbreviate_under(path: &str, home: Option<&str>) -> String {
    match home {
        Some(home) if !home.is_empty() && path == home => "~".to_owned(),
        Some(home) if !home.is_empty() => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_owned(),
        },
        _ => path.to_owned(),
    }
}

/// A very faint full-width `─` hairline rule (a step below the dim chrome, so it
/// recedes to about the weight of the dotted `┄` divider). Seals the header from
/// the cockpit and brackets the provider dashboard — the structure the dropped
/// border once carried.
fn hairline_rule(theme: &Theme, width: usize) -> Line<'static> {
    Line::styled("─".repeat(width.max(1)), theme.rule())
}

fn alert_lines(theme: &Theme, alert: &Alert) -> Vec<Line<'static>> {
    if alert.is_active() {
        let elapsed = age_short(alert.since);
        vec![Line::styled(
            format!("! Sidebar degraded for {elapsed}: {}", alert.reason),
            theme.style(Color::Red, Modifier::BOLD),
        )]
    } else {
        let elapsed = alert
            .recovered_at
            .map(age_short)
            .unwrap_or_else(|| "0s".to_owned());
        vec![Line::styled(
            format!("⚠ last alert {elapsed} ago: {}  ·  x dismiss", alert.reason),
            theme.style(Color::Yellow, Modifier::DIM),
        )]
    }
}

fn push_section_gap(lines: &mut Vec<Line<'static>>, map: &mut Vec<Option<usize>>) {
    if lines.last().is_some_and(|line| line.width() > 0) {
        lines.push(Line::from(""));
        map.push(None);
    }
}

fn should_show_first_run_hint(snapshot: &SidebarSnapshot) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .all(|row| row.row_kind == SidebarRowKind::Process && !is_known_agent_process(row))
}

fn is_known_agent_process(row: &rimz::SidebarRow) -> bool {
    // tmux can expose Claude/Codex as the shared Node host before hook
    // enrichment claims the pane, so `node` is agent-like for the empty-room cue.
    row.row_kind == SidebarRowKind::Process
        && (rimz::agents::known_kinds().any(|kind| kind == row.name) || row.name == "node")
}

fn footer_lines(snapshot: &SidebarSnapshot, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let needs_attention = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| {
            matches!(
                row.status,
                Some(rimz::feed::AgentStatus::Waiting | rimz::feed::AgentStatus::Failed)
            )
        });
    // The faintest chrome — quieter than the old dim footer. `? for help` is the
    // resting hint; the `␣ next ?!` triage key joins it only when something
    // actually needs you, so the signature key stays discoverable without
    // shouting at rest. The full key model lives behind the `?` overlay.
    let text = if needs_attention {
        "␣ next ?!   ? for help"
    } else {
        "? for help"
    };
    vec![center_line(
        Line::styled(text.to_owned(), theme.faint()),
        width,
    )]
}

/// Center a single line within `width` by prepending padding — used to pin the
/// navigation footer to the bottom edge, horizontally centered. A line already
/// at or past the width is returned unchanged.
fn center_line(line: Line<'static>, width: usize) -> Line<'static> {
    let content_width: usize = line
        .spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    let pad = width.saturating_sub(content_width) / 2;
    if pad == 0 {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(line.spans);
    Line::from(spans)
}

fn help_lines() -> Vec<Line<'static>> {
    let dim = Style::default()
        .fg(Color::Indexed(244))
        .add_modifier(Modifier::DIM);
    vec![
        Line::styled("keys & legend", dim),
        Line::styled("↑/↓ select   1-9 jump   ↵ jump", dim),
        Line::styled("␣ next ?!   x dismiss   r reload   ? close", dim),
        Line::styled("⢿ working   ✽ thinking   ? waiting", dim),
        Line::styled("! attention   ○ idle   ✓ done   dim = process", dim),
    ]
}

#[cfg(test)]
mod tests;
