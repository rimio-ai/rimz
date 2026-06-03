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
use std::time::Instant;

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
    /// `focus_mirror` and any active `selection_override`. Keying on the pane
    /// means a status-churn reorder re-anchors the highlight to the same pane
    /// instead of sliding it onto a neighbour.
    pub selected_pane: Option<PaneId>,
    /// The last agent row the mux client was observed focused on — the
    /// level-triggered baseline and the *default* highlight. It advances on every
    /// valid focus report and HOLDS its value across a `None` / sidebar-self /
    /// undiscoverable / non-row report, so a momentary "no agent focus" gap never
    /// blanks or moves the highlight. With no active override the highlight simply
    /// follows this, so the sidebar always reconverges on the real focus.
    pub(crate) focus_mirror: Option<PaneId>,
    /// The active local override of the focus mirror, or `None` to follow the
    /// mirror. Set by every local selection action and cleared by
    /// `app::reconcile_selection` once it resolves (see [`OverrideKind`]).
    pub(crate) selection_override: Option<SelectionOverride>,
}

/// A local selection that overrides the [`UiState::focus_mirror`] for a bounded
/// window. `pane` is the identity it pins; `kind` decides when it yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SelectionOverride {
    pub(crate) pane: PaneId,
    pub(crate) kind: OverrideKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OverrideKind {
    /// Arrow-key browse: pins `pane` WITHOUT moving focus. Holds until the user
    /// acts again or the client focus moves off `baseline` — the mirror value
    /// captured when browsing began. A `None`/inert report does not advance the
    /// mirror, so it is not "moving off baseline" and the browse holds.
    Browse { baseline: Option<PaneId> },
    /// Click / `↵` / digit / `␣`: optimistically pins the jumped `pane`. Resolves
    /// (clears, reverting to the mirror) when the mirror confirms focus reached
    /// `pane`, when a genuine external move lands on a pane that is neither `pane`
    /// nor in `lagging`, or when `now >= settle_deadline` (a jump that never
    /// landed). `lagging` is the pre-jump focus plus every pane this click-burst
    /// already jumped to: a mirror value in this set is our own in-flight focus
    /// catching up — never an external move — so a fast burst never bounces the
    /// highlight through an intermediate self-jump.
    Jump {
        settle_deadline: Instant,
        lagging: Vec<PaneId>,
    },
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

/// Whether any visible row is in an animated state — a running agent (working
/// or plan-mode thinking), a resolver mid-flight, an active process spinning on
/// real work (a build, a test, a `sudo` install), an attention row whose `?`/`!`
/// glyph breathes, or an idle agent showing the loading-dots cue. The serve
/// loop uses this to switch to the fast animation tick only while there is live
/// motion to paint; a fully settled sidebar (only quiet idle/done rows) keeps
/// idling on the slow data tick. A stalled agent is projected to `failed`
/// upstream, so it reads as a breathing `!` here. The cockpit's today-spend
/// count-up rides a separate gate (`UiState::tally`), so a finished-turn climb
/// keeps the tick alive even when every row is otherwise static.
pub fn has_live_animation(snapshot: &SidebarSnapshot) -> bool {
    use rimz::feed::AgentStatus;
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .any(|row| match row.row_kind {
            SidebarRowKind::Agent => {
                row.resolver.is_some()
                    || row.status == Some(AgentStatus::Running)
                    // `?`/`!` breathe to pull the eye back to an unanswered row.
                    || matches!(row.status, Some(AgentStatus::Waiting | AgentStatus::Failed))
                    // The idle "waiting for a prompt" loading-dots cue.
                    || sections::shows_loading_dots(row)
            }
            SidebarRowKind::Process => row.process_active,
        })
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
    // The cockpit summary, two lines: line 1 is `¤` live agents on the left with
    // today's accumulated token breakdown pinned right; line 2 is `◎` sessions
    // today on the left with today's spend pinned right. The counts read from the
    // live fleet and the JSONL `value_tally`'s today window, so the cockpit
    // reflects all of today's sessions rather than only the live statusline sum.
    let today = snapshot.value_tally.as_ref().map(|tally| &tally.today);
    let live_agents = fleet_size(&snapshot.worktree_groups).0;
    header.push(cockpit_summary_line(theme, live_agents, today, inner));
    // Line 2 is always present — sessions read `◎ 0` in an empty room — with the
    // spend joining the right edge and counting up as a turn lands.
    let sessions = today.map(|window| window.sessions).unwrap_or(0);
    let today_usd = today.map(|window| window.usd).unwrap_or(0.0);
    header.push(cockpit_spend_line(
        theme,
        sessions,
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
    // The configurable neglect window (seconds an unanswered `?`/`!` stays
    // yellow before it reddens) rides in on the snapshot like `project_root`
    // does, so the renderer stays a pure consumer. Clamp into the `i64` age space.
    let redden_secs = i64::try_from(snapshot.sidebar.attention_redden_secs).unwrap_or(i64::MAX);
    extend_inert(
        &mut lines,
        &mut map,
        fleet_header_lines(theme, &snapshot.worktree_groups, inner, redden_secs),
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
                redden_secs,
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
        && (rimz::agents::KNOWN_AGENTS.contains(&row.name.as_str()) || row.name == "node")
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
        Line::styled("posture: plan · auto · yolo", dim),
    ]
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rimz::agents::{
        AgentContext, AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage,
        RateLimitWindow,
    };
    use rimz::feed::{AgentState, AgentStatus, FeedKind, PaneRef, PermissionPosture};
    use rimz::ids::{MuxName, PaneId, ViewKind};
    use rimz::{EventEnvelope, FeedItem, FeedStatus, SidebarSnapshot, Surface, WorkspaceId};
    use serde_json::json;
    use std::time::Duration;

    use super::*;

    fn fixed_workspace() -> WorkspaceId {
        WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
    }

    fn fixed_now() -> Timestamp {
        // Pin every test to one timestamp so the redaction filter has a
        // deterministic input to scrub.
        Timestamp::now()
    }

    fn snapshot_to_screen(snapshot: &SidebarSnapshot, width: u16, height: u16) -> String {
        snapshot_to_screen_with_alert(snapshot, None, width, height)
    }

    fn snapshot_to_screen_with_alert(
        snapshot: &SidebarSnapshot,
        alert: Option<&Alert>,
        width: u16,
        height: u16,
    ) -> String {
        snapshot_to_screen_with_alert_and_ui(snapshot, alert, &UiState::default(), width, height)
    }

    fn snapshot_to_screen_with_alert_and_ui(
        snapshot: &SidebarSnapshot,
        alert: Option<&Alert>,
        ui: &UiState,
        width: u16,
        height: u16,
    ) -> String {
        let mut bytes = Vec::new();
        let backend = CrosstermBackend::new(&mut bytes);
        let viewport = Viewport::Fixed(Rect::new(0, 0, width, height));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        terminal.clear().unwrap();
        let mut ui = ui.clone();
        draw_to_terminal_with_ui(&mut terminal, snapshot, alert, &mut ui).unwrap();
        drop(terminal);
        let mut parser = vt100::Parser::new(height, width, 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    fn snapshot_text(screen: &str) -> String {
        screen
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_snapshot(name: &str, screen: String) {
        // Row ages and degraded elapsed values are intentionally relative.
        let screen = snapshot_text(&screen);
        insta::with_settings!({
            filters => vec![
                (r"degraded for \d+[smhd]", "degraded for <elapsed>"),
                // Budget-bar reset countdowns are a live two-unit duration in the
                // bar's right value column (`3h12m`, `3d3h`); scrub them so the
                // card snapshot stays stable across time. Single-unit ages and
                // the `5h`/`7d` labels fall to the age scrub below.
                (r"\b\d+[dhms]\d+[dhms]\b", "<reset>"),
                (r"\b\d+[smhd]\b", "<t>"),
            ],
        }, {
            insta::assert_snapshot!(name, screen);
        });
    }

    #[test]
    fn no_color_theme_suppresses_color_not_shape_modifiers() {
        let style = Theme::fixed(true).style(Color::Red, Modifier::BOLD);

        assert_eq!(style.fg, None);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn remote_control_host_pane_is_filtered_not_rendered() {
        // A `claude remote-control` pane is host infrastructure, not a coding
        // agent: the snapshot reducer filters it out, so it never reads as a
        // `claude` row. Remote control surfaces as a `⇅ rc` flag on the provider
        // dashboard (covered by the section tests), never as its own row.
        let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
            vec![
                pane("%1", "zsh", "/repo/main"),
                pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
            ],
            None,
        );
        let screen = snapshot_to_screen(&snapshot, 32, 24);
        assert!(
            screen.contains("○ zsh"),
            "the plain shell still renders:\n{screen}"
        );
        assert!(
            !screen.contains("○ claude"),
            "the rc host must not read as a claude process row:\n{screen}",
        );
        assert!(
            !screen.contains("remote control"),
            "the rc host is filtered, not a pinned row:\n{screen}",
        );
    }

    fn snapshot_with(items: Vec<FeedItem>, agents: Vec<AgentState>) -> SidebarSnapshot {
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(fixed_workspace(), items, Vec::new(), agents);
        snapshot.display_name = "query-engine".to_owned();
        snapshot
    }

    fn agent(
        id: &str,
        kind: &str,
        status: AgentStatus,
        permission_posture: PermissionPosture,
        worktree_path: Option<&str>,
        branch: Option<&str>,
        task: Option<&str>,
    ) -> AgentState {
        let now = fixed_now();
        AgentState {
            agent_id: id.to_owned(),
            kind: kind.to_owned(),
            status,
            permission_posture,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: worktree_path.map(ToOwned::to_owned),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: task.map(ToOwned::to_owned),
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            parked_on_background: false,
            last_seen: now,
            last_activity: now,
        }
    }

    fn pane(raw: &str, command: &str, cwd: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some("@0".to_owned()),
            view_kind: Some(ViewKind::Window),
            view_name: None,
            is_focused: false,
            client_focused: false,
            command: Some(command.to_owned()),
            cwd: Some(cwd.to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    /// A full Claude statusline enrichment for the rich-row tests. Reset instants
    /// are placed days/hours ahead so the live countdown renders at a stable
    /// length (the value itself is scrubbed by `assert_snapshot`).
    fn claude_context(now: Timestamp) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
            session_name: Some("ledger refactor".to_owned()),
            model_id: Some("claude-opus-4-8".to_owned()),
            model_display_name: Some("Opus 4.8 (1M context)".to_owned()),
            effort: Some("high".to_owned()),
            thinking_enabled: Some(false),
            output_style: None,
            vim_mode: None,
            agent_version: None,
            exceeds_200k_tokens: Some(false),
            cost: Some(AgentCost {
                total_cost_usd: Some(1.27),
                total_duration_ms: Some(12 * 60 * 1_000),
                total_api_duration_ms: None,
                total_lines_added: Some(214),
                total_lines_removed: Some(31),
            }),
            tokens: Some(AgentTokenUsage {
                total_input_tokens: Some(64_200),
                total_output_tokens: Some(12_300),
                context_window_size: Some(200_000),
                used_percentage: Some(38),
                remaining_percentage: Some(62),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(8_500),
                    output_tokens: Some(1_200),
                    cache_creation_input_tokens: Some(20_000),
                    cache_read_input_tokens: Some(48_000),
                }),
            }),
            rate_limits: Some(AgentRateLimits {
                windows: vec![
                    RateLimitWindow {
                        used_percentage: Some(30),
                        resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                        duration_mins: Some(5 * 60),
                    },
                    RateLimitWindow {
                        used_percentage: Some(60),
                        resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                        duration_mins: Some(7 * 24 * 60),
                    },
                ],
            }),
            pr: None,
            account: None,
            observed_at: now,
        }
    }

    /// The Codex app-server enrichment: a 5-hour and a 7-day rate-limit window,
    /// the official model display name, effort, and version — but no token usage or
    /// cost (the app-server exposes neither read-only, so those stay `None` and
    /// the gauge falls back to the rollout scalars). The mirror of `claude_context`
    /// for the other transport.
    fn codex_context(now: Timestamp) -> AgentContext {
        AgentContext {
            source: "codex".to_owned(),
            session_name: None,
            model_id: Some("gpt-5.5-codex".to_owned()),
            model_display_name: Some("GPT-5.5 Codex".to_owned()),
            effort: Some("xhigh".to_owned()),
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: Some("0.135.0".to_owned()),
            exceeds_200k_tokens: None,
            cost: None,
            tokens: None,
            rate_limits: Some(AgentRateLimits {
                windows: vec![
                    RateLimitWindow {
                        used_percentage: Some(42),
                        resets_at: Some(now + Duration::from_secs(3 * 3_600 + 12 * 60)),
                        duration_mins: Some(5 * 60),
                    },
                    RateLimitWindow {
                        used_percentage: Some(7),
                        resets_at: Some(now + Duration::from_secs(3 * 86_400 + 4 * 3_600)),
                        duration_mins: Some(7 * 24 * 60),
                    },
                ],
            }),
            pr: None,
            account: None,
            observed_at: now,
        }
    }

    #[test]
    fn render_worktree_attention_map() {
        let workspace = fixed_workspace();
        let mut native = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "psql DROP TABLE invoices",
            "claude",
            "agent-hook",
        );
        native.worktree_path = Some("/home/me/query-engine".to_owned());
        native.updated_at = fixed_now() - Duration::from_secs(12 * 60);
        let mut script = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "Deploy staging?",
            "deploy.sh",
            "cli",
        );
        script.options = vec!["yes".to_owned(), "no".to_owned()];
        script.updated_at = fixed_now() - Duration::from_secs(5 * 60);
        let mut running = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/home/me/query-engine"),
            Some("main"),
            Some("add tests"),
        );
        running.model = Some("GPT-5.5".to_owned());
        running.effort = Some("high".to_owned());
        running.last_activity = fixed_now() - Duration::from_secs(8);

        let snapshot = snapshot_with(vec![native, script], vec![running]);

        assert_snapshot(
            "worktree_attention_map",
            snapshot_to_screen(&snapshot, 38, 20),
        );
    }

    #[test]
    fn render_agent_capability_and_posture() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Failed,
            PermissionPosture::Yolo,
            Some("/repo/feature-migration"),
            Some("feature-migration"),
            Some("db migrate"),
        );
        claude.model = Some("Opus".to_owned());
        claude.effort = Some("xhigh".to_owned());
        claude.last_activity = fixed_now() - Duration::from_secs(4 * 60);
        let snapshot = snapshot_with(Vec::new(), vec![claude]);

        assert_snapshot("agent_capability", snapshot_to_screen(&snapshot, 34, 12));
    }

    #[test]
    fn render_enriched_selected_agent_card() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/feature-migration"),
            Some("feature-migration"),
            Some("db migrate"),
        );
        // Transcript scalars are the coarse fallback; the statusline context
        // below supersedes them (`Opus` → `Opus 4.8 (1M)`, `xhigh` → `high`).
        claude.model = Some("Opus".to_owned());
        claude.effort = Some("xhigh".to_owned());
        claude.context_pct = Some(38);
        claude.total_tokens = Some(12_400);
        claude.todo_done = Some(3);
        claude.todo_total = Some(5);
        claude.context = Some(claude_context(fixed_now()));
        let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
        snapshot.worktree_groups[0].diff_added = Some(127);
        snapshot.worktree_groups[0].diff_removed = Some(43);

        let rendered = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: false,
                animation_phase: 0,
                line_map: Vec::new(),
                ..Default::default()
            },
            54,
            14,
        );

        // The worktree-total diff sits on the group header.
        assert!(rendered.contains("+127 -43"));
        // Line 1 carries identity + capability + cost; line 2 is the session
        // name; the model display name is shortened (`(1M context)` → `(1M)`).
        assert!(rendered.contains("Opus 4.8 (1M)"));
        assert!(!rendered.contains("context"));
        assert!(rendered.contains("high"));
        assert!(rendered.contains("auto"));
        // Per-row cost now reads at full cent resolution, like every other spend.
        assert!(rendered.contains("$1.27"));
        // Line 2 is the full-width description; todo dots inline at L2.
        assert!(rendered.contains("ledger refactor"));
        assert!(rendered.contains("●●●○○ 3/5"));
        // The context bar carries the `▣` label and the percent used as its
        // value (always — the window size moved to the token line below); the
        // fill carries the same reading.
        assert!(rendered.contains("▣ "));
        // The account-scoped 5h/7d budgets are gone from the row — they live in
        // the provider dashboard now.
        assert!(!rendered.contains("5h↻"));
        assert!(!rendered.contains("7d↻"));
        // The card carries the token line at rest. Tokens read the integer split
        // ◇ total ↘ input ↗ output ◍ cache-write ◌ cache-read; the window size no
        // longer rides this line.
        assert!(rendered.contains("◇ 76k"));
        assert!(rendered.contains("↘ 64k"));
        assert!(rendered.contains("↗ 12k"));
        assert!(rendered.contains("◍ 20k"), "cache-write split:\n{rendered}");
        assert!(rendered.contains("◌ 48k"), "cache-read split:\n{rendered}");
        assert!(
            !rendered.contains("ctx"),
            "window size left the token line:\n{rendered}"
        );
        assert_snapshot("enriched_selected_agent_card", rendered);
    }

    #[test]
    fn render_selected_card_shows_subagent_description_tokens_and_elapsed() {
        // A selected parent expands its `⧉ subagents` list. Each child reads two
        // lines: the type and its `subagentStatusLine` description, then the token
        // spend `◇` left with the elapsed work `◷` pinned right. The child is
        // finished, so its elapsed is frozen at `last_activity − started` (exactly
        // 60s here, independent of wall-clock) — a deterministic `1m`.
        let mut parent = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        parent.context = Some(claude_context(fixed_now()));

        let mut child = agent(
            "child-1",
            "claude",
            AgentStatus::Success,
            PermissionPosture::Default,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".to_owned());
        child.subagent_description = Some("locate the render seam".to_owned());
        child.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
        child.last_activity = fixed_now() - Duration::from_secs(30);
        child.last_seen = fixed_now() - Duration::from_secs(30);
        child.total_tokens = Some(12_400);

        let snapshot = snapshot_with(Vec::new(), vec![parent, child]);
        let rendered = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                ..Default::default()
            },
            54,
            18,
        );

        assert!(
            rendered.contains("⧉ subagents (1)"),
            "the expanded card lists its child:\n{rendered}"
        );
        // Line 1: type + the description of what the parent asked it to do.
        assert!(
            rendered.contains("Explore — locate the render seam"),
            "line 1 carries the description:\n{rendered}"
        );
        // Line 2: token spend left, elapsed work right-pinned. 12_400 → `12.4k`,
        // 60s frozen → `1m`.
        assert!(
            rendered.contains("◇ 12.4k"),
            "line 2 carries the token spend:\n{rendered}"
        );
        assert!(
            rendered.contains("◷ 1m"),
            "line 2 carries the frozen elapsed:\n{rendered}"
        );
        assert_snapshot("subagent_two_line_entry", rendered);
    }

    #[test]
    fn line_one_prefers_session_name_over_task() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let rendered = snapshot_to_screen(&snapshot, 44, 12);

        assert!(rendered.contains("ledger refactor"));
        assert!(!rendered.contains("db migrate"));
    }

    /// An unnamed session whose turn has ended (the activity-bound `task` cleared)
    /// keeps its latest prompt on line two instead of falling to an em dash, until
    /// a real session name exists.
    #[test]
    fn line_two_falls_back_to_the_latest_prompt_when_unnamed() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            None, // idle cleared the task; no session name (no context)
        );
        claude.prompt = Some("wire the bridge".to_owned());
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let rendered = snapshot_to_screen(&snapshot, 44, 12);

        assert!(rendered.contains("wire the bridge"));
        assert!(
            !rendered.contains('—'),
            "the prompt stands in for the em dash"
        );
    }

    #[test]
    fn selected_agent_without_context_keeps_bare_token_total() {
        // An agent with no context sidecar yet (a Codex session before its first
        // app-server refresh, or any agent that publishes none) degrades to the
        // simple selected-row token total — no cost, no usage windows.
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("add tests"),
        );
        codex.model = Some("GPT-5.5".to_owned());
        codex.total_tokens = Some(5_000);
        assert!(codex.context.is_none());
        let snapshot = snapshot_with(Vec::new(), vec![codex]);
        let rendered = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: false,
                animation_phase: 0,
                line_map: Vec::new(),
                ..Default::default()
            },
            44,
            14,
        );

        assert!(rendered.contains("◇ 5k"));
        assert!(!rendered.contains('↻'));
        assert!(!rendered.contains('$'));
    }

    #[test]
    fn codex_app_server_context_links_to_rich_card() {
        // Codex's app-server enrichment rides the same `AgentContext` field as
        // Claude's statusline, so it lights up the rich card with no renderer
        // change: the official display name and effort on the capability line,
        // and both usage windows in the selected detail block. Token usage and
        // cost have no read-only source, so the gauge and detail fall back to the
        // rollout scalars.
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("add tests"),
        );
        // Rollout scalars are the coarse fallback the app-server context upgrades.
        codex.model = Some("gpt-5.5-codex".to_owned());
        codex.context_pct = Some(21);
        codex.total_tokens = Some(48_000);
        codex.context = Some(codex_context(fixed_now()));
        let snapshot = snapshot_with(Vec::new(), vec![codex]);
        let rendered = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: false,
                animation_phase: 0,
                line_map: Vec::new(),
                ..Default::default()
            },
            54,
            14,
        );

        // The app-server display name supersedes the raw catalog id, and effort
        // surfaces — neither was on the rollout-only row.
        assert!(rendered.contains("GPT-5.5 Codex"));
        assert!(!rendered.contains("gpt-5.5-codex"));
        assert!(rendered.contains("xhigh"));
        // The 5h/7d windows are account-scoped now: they leave the row for the
        // provider dashboard, so no reset mark rides a row.
        assert!(!rendered.contains('↻'));
        assert!(!rendered.contains("5h"));
        assert!(!rendered.contains("7d"));
        // No read-only token usage or cost: the bare rollout total (`◇ 48k`,
        // integer form) stands in for the token line, and no cost pins to the row.
        assert!(rendered.contains("◇ 48k"));
        assert!(!rendered.contains('↗'));
        assert!(!rendered.contains('$'));
    }

    #[test]
    fn render_omits_history_sections() {
        let workspace = fixed_workspace();
        let mut answered = FeedItem::new(
            workspace.clone(),
            Surface::Script,
            FeedKind::Question,
            "Deploy staging?",
            "deploy.sh",
            "cli",
        );
        answered.status = FeedStatus::Resolved;
        let event = EventEnvelope::new(
            workspace.clone(),
            "rimz-test",
            "rimz",
            "cli",
            "event.emit",
            json!({ "kind": "build.started", "title": "Building web" }),
        );
        let mut snapshot =
            SidebarSnapshot::build_with_carryover(workspace, vec![answered], vec![event], vec![]);
        snapshot.display_name = "query-engine".to_owned();
        let rendered = snapshot_to_screen(&snapshot, 38, 10);

        assert!(!rendered.contains("all clear"));
        assert!(!rendered.contains("Recent activity"));
        assert!(!rendered.contains("Recently answered"));
    }

    #[test]
    fn render_active_alert_shows_banner_below_snapshot() {
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let alert = Alert {
            reason: "snapshot failed: ledger not found".to_owned(),
            since: fixed_now() - Duration::from_secs(8),
            recovered_at: None,
        };

        assert_snapshot(
            "degraded_banner",
            snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18),
        );
    }

    #[test]
    fn render_recovered_alert_lingers_with_dismiss_hint() {
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let alert = Alert {
            reason: "snapshot failed: ledger not found".to_owned(),
            since: fixed_now() - Duration::from_secs(20),
            recovered_at: Some(fixed_now() - Duration::from_secs(8)),
        };
        let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

        assert!(rendered.contains("last alert"), "{rendered}");
        assert!(rendered.contains("x dismiss"), "{rendered}");
        // Recovered means the room is live again: the first-run hint returns.
        assert!(rendered.contains("rimz hooks install"), "{rendered}");
    }

    #[test]
    fn render_no_alert_omits_banner() {
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let rendered = snapshot_to_screen_with_alert(&snapshot, None, 80, 18);
        assert!(
            !rendered.contains("Sidebar degraded"),
            "no alert must not render the banner:\n{rendered}"
        );
    }

    #[test]
    fn render_first_run_nudge_points_at_install_when_unwired() {
        // No hooks wired (the default): running an agent registers nothing, so
        // the hint must point at `rimz hooks install`, not "run claude or codex".
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        assert!(!snapshot.agent_hooks_ready);
        let rendered = snapshot_to_screen(&snapshot, 80, 18);

        assert!(!rendered.contains("all clear"));
        assert!(rendered.contains("rimz hooks install"));
        assert!(!rendered.contains("run claude or codex"));
        assert_snapshot("first_run_nudge", rendered);
    }

    #[test]
    fn render_process_row_keeps_first_run_hint() {
        let snapshot = snapshot_with(Vec::new(), Vec::new())
            .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
        let rendered = snapshot_to_screen(&snapshot, 80, 18);

        assert!(rendered.contains("○ zsh"));
        assert!(rendered.contains("rimz hooks install"));
    }

    #[test]
    fn render_agent_process_rows_suppress_first_run_hint() {
        let snapshot = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
            vec![
                pane("%1", "claude", "/repo/main"),
                pane("%2", "node", "/repo/main"),
            ],
            None,
        );
        let rendered = snapshot_to_screen(&snapshot, 80, 18);

        assert!(rendered.contains("○ claude"));
        assert!(rendered.contains("○ node"));
        assert!(!rendered.contains("no agents yet"));
        assert!(!rendered.contains("rimz hooks install"));
        assert!(!rendered.contains("run claude or codex"));
    }

    #[test]
    fn active_process_row_keeps_the_animation_tick_alive() {
        // A pane doing real work spins a braille frame, so the serve loop must hold
        // the fast animation tick for it just as it does for a running agent —
        // otherwise the spin crawls on the slow data tick.
        let busy = snapshot_with(Vec::new(), Vec::new()).with_live_panes(
            vec![pane("%1", "cargo build --release", "/repo/main")],
            None,
        );
        assert!(has_live_animation(&busy));

        // A bare shell is presence, not motion: it stays on the calm data tick.
        let idle = snapshot_with(Vec::new(), Vec::new())
            .with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);
        assert!(!has_live_animation(&idle));
    }

    #[test]
    fn render_footer_and_help_overlay() {
        let workspace = fixed_workspace();
        let mut native = FeedItem::new(
            workspace,
            Surface::NativeUi,
            FeedKind::Permission,
            "allow?",
            "codex",
            "agent-hook",
        );
        native.worktree_branch = Some("main".to_owned());
        let snapshot = snapshot_with(vec![native], Vec::new());
        let rendered = snapshot_to_screen(&snapshot, 80, 18);
        // A waiting permission is an attention row, so the footer carries the
        // triage key alongside the resting help hint.
        assert!(rendered.contains("␣ next ?!"), "{rendered}");
        assert!(rendered.contains("? for help"), "{rendered}");

        let help = snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                selected_index: 0,
                help_visible: true,
                animation_phase: 0,
                line_map: Vec::new(),
                ..Default::default()
            },
            80,
            20,
        );
        assert!(help.contains("keys & legend"));
        assert!(help.contains("? waiting"));
        assert!(help.contains("○ idle"));
        assert!(help.contains("dim = process"));
        assert!(help.contains("posture: plan · auto · yolo"));
    }

    #[test]
    fn render_first_run_nudge_invites_launch_when_wired() {
        // Hooks wired but no agent launched yet: the hint invites running one.
        let mut snapshot = snapshot_with(Vec::new(), Vec::new());
        snapshot.agent_hooks_ready = true;
        let rendered = snapshot_to_screen(&snapshot, 80, 18);

        assert!(!rendered.contains("all clear"));
        assert!(rendered.contains("run claude or codex"));
        assert!(!rendered.contains("rimz hooks install"));
        assert_snapshot("first_run_nudge_wired", rendered);
    }

    #[test]
    fn render_active_alert_empty_suppresses_first_run_nudge() {
        // An empty body under an active alert is a failed snapshot, not an
        // empty room — the nudge would misreport. The banner speaks instead.
        let snapshot = snapshot_with(Vec::new(), Vec::new());
        let alert = Alert::active("snapshot failed: ledger not found");
        let rendered = snapshot_to_screen_with_alert(&snapshot, Some(&alert), 80, 18);

        assert!(!rendered.contains("run claude or codex"));
        assert!(!rendered.contains("rimz hooks install"));
    }

    #[test]
    fn render_group_cap_shows_overflow_indicator() {
        let agents = (0..9)
            .map(|i| {
                let mut agent = agent(
                    &format!("codex-{i}"),
                    "codex",
                    AgentStatus::Running,
                    PermissionPosture::Default,
                    Some("/repo/main"),
                    Some("main"),
                    Some(&format!("task-{i}")),
                );
                agent.last_activity = fixed_now() - Duration::from_secs(i);
                agent
            })
            .collect::<Vec<_>>();
        let snapshot = snapshot_with(Vec::new(), agents);

        // Tall enough that the six capped rows (3 compact lines each, stacked
        // with no inter-card gap) plus the `+3 more` overflow all fit, so the
        // indicator the test is named for actually renders.
        let rendered = snapshot_to_screen(&snapshot, 36, 38);
        assert!(rendered.contains("+3 more"), "{rendered}");
        assert_snapshot("group_cap_with_overflow", rendered);
    }

    /// L0 density (~24 columns): line 1 still names the row by status glyph
    /// and clipped name, and label-less meter chrome from line 2 is dropped
    /// when capability data is absent.
    #[test]
    fn render_l0_density_keeps_identity_when_narrow() {
        let mut codex = agent(
            "codex-1",
            "codex",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("compile"),
        );
        codex.last_activity = fixed_now() - Duration::from_secs(3);
        let snapshot = snapshot_with(Vec::new(), vec![codex]);
        // Tall enough that the card clears the bottom-pinned footer after the
        // cockpit's blank-line + two-line summary header (the agent row is what we
        // measure).
        let rendered = snapshot_to_screen(&snapshot, 24, 11);

        assert!(
            // phase 0 of the working spinner is the first frame `⣾`.
            rendered.contains("⣾ codex"),
            "L0 keeps status glyph + name:\n{rendered}"
        );
        assert!(
            rendered.contains("main"),
            "L0 keeps the worktree label:\n{rendered}"
        );
        assert!(
            !rendered.contains("auto") && !rendered.contains("yolo"),
            "default posture stays the omitted baseline:\n{rendered}"
        );
        assert_snapshot("l0_density_minimal_row", rendered);
    }

    fn ui_at_phase(phase: u64) -> UiState {
        UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: phase,
            line_map: Vec::new(),
            ..Default::default()
        }
    }

    /// Honesty test: a running agent silent past the stall window is projected
    /// to the attention bucket, so its cell reads as the attention `!` rather than
    /// the working spinner — a wedged agent stops spinning and asks for a look.
    /// (The `!` slowly blinks to draw the eye, but does not cycle the working
    /// braille; phases 0 and 2 both fall in the blink's shown window.)
    #[test]
    fn render_stalled_agent_reads_as_static_attention() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("waiting on tools"),
        );
        claude.last_activity =
            fixed_now() - Duration::from_secs(rimz::feed::STALL_WINDOW_SECS as u64 + 60);
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 10);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 40, 10);

        assert_eq!(first, second, "a stalled agent's cell must not spin");
        assert!(
            first.contains("! claude"),
            "stalled reads as attention:\n{first}"
        );
    }

    /// A running agent animates: advancing the phase advances the working fill,
    /// regardless of how recently it last reported (the freshness freeze is
    /// gone — staleness escalates to `!` instead of stopping the spinner).
    #[test]
    fn render_running_head_spins_with_the_phase() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("compiling"),
        );
        claude.last_activity = fixed_now() - Duration::from_secs(30);
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 40, 10);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 40, 10);

        assert_ne!(
            first, second,
            "a running agent's head must advance with the phase"
        );
    }

    /// An idle agent on a spent account projects to rate-limited: the row leads
    /// with the `⏸` pause and the cockpit gains an `⏸` bucket. It is static —
    /// parked, with nothing to do but wait for the reset.
    #[test]
    fn rate_limited_agent_reads_as_a_static_pause() {
        let now = fixed_now();
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Idle,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            None,
        );
        claude.context = Some(AgentContext {
            rate_limits: Some(AgentRateLimits {
                windows: vec![RateLimitWindow {
                    used_percentage: Some(100),
                    resets_at: Some(now + Duration::from_secs(2 * 3_600)),
                    duration_mins: Some(5 * 60),
                }],
            }),
            ..claude_context(now)
        });
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 10);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 10);
        assert_eq!(first, second, "a parked agent's head must not animate");
        assert!(
            first.contains('⏸'),
            "the rate-limited row and cockpit show the pause:\n{first}"
        );
    }

    /// A running agent mid-compaction shows the pulsing compacting head instead
    /// of the working spinner: it animates, and the working braille never
    /// appears (the overlay replaced it). Short-lived, so it never enters the
    /// cockpit tally.
    #[test]
    fn compacting_head_pulses_over_the_working_spinner() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("condensing context"),
        );
        claude.compacting_since = Some(fixed_now());
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(0), 44, 10);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(1), 44, 10);
        assert_ne!(first, second, "the compacting head animates");
        // The pulse bar (`▁` at phase 0) leads the row — unique to the compacting
        // head, so its presence proves the overlay replaced the working spinner.
        // (The cockpit's working *bucket* still shows `⢿`, which is expected.)
        assert!(
            first.contains('▁'),
            "the compacting head shows the pulse bar:\n{first}"
        );
    }

    /// A running parent with a live subagent shows the quiet delegated-wait head,
    /// not the working spinner — the work is in the child below. It animates, and
    /// the working braille never appears on the parent's collapsed row.
    #[test]
    fn waiting_on_subagents_head_replaces_the_working_spinner() {
        let parent = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("orchestrating"),
        );
        let mut kid = agent(
            "kid-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            None,
            None,
            Some("Explore"),
        );
        kid.parent_agent_id = Some("claude-1".to_owned());
        let snapshot = snapshot_with(Vec::new(), vec![parent, kid]);
        // Phase 2 of the wave is a distinctive backtick, unique to the
        // delegated-wait head (the cockpit's working bucket still shows `⢿`).
        let first = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(2), 44, 10);
        let second = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui_at_phase(4), 44, 10);
        assert_ne!(first, second, "the delegated-wait head animates");
        assert!(
            first.contains('`'),
            "the parent shows the delegated-wait wave, not the working spinner:\n{first}"
        );
    }

    /// A fully-enriched single-agent group, rendered as raw card lines at a
    /// fixed width. Returns the group lines (header first), each flattened to its
    /// text — the seam the structural card tests share.
    fn card_lines(selected_index: usize) -> Vec<String> {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        let snapshot = snapshot_with(Vec::new(), vec![claude]);
        let theme = Theme::fixed(true);
        let mut row_index = 0;
        let mut lines = Vec::new();
        let mut map = Vec::new();
        worktree_group_lines(
            &theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            54,
            30 * 60,
            &mut row_index,
            selected_index,
            0,
            &mut lines,
            &mut map,
        );
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The load-bearing no-flicker guarantee: selecting a row only *appends*
    /// lines beneath the card — the resting fold lines (identity, description,
    /// ctx bar, token line) keep their exact content, differing only by the
    /// selection gutter.
    #[test]
    fn selecting_a_row_only_appends_never_reshapes_the_fold_lines() {
        let unselected = card_lines(usize::MAX);
        let selected = card_lines(0);

        // Selecting the worktree adds the lane gutter and the dotted seal to its
        // header — chrome, not a card line — but never touches the label itself.
        assert!(unselected[0].contains("main"), "{:?}", unselected[0]);
        assert!(selected[0].contains("main"), "{:?}", selected[0]);
        assert!(
            !unselected[0].contains('┄'),
            "an unselected worktree header is unsealed: {:?}",
            unselected[0]
        );
        assert!(
            selected[0].contains('┄'),
            "the selected worktree header is sealed: {:?}",
            selected[0]
        );
        // Row lines differ only by the leading one-cell gutter; strip it.
        let strip = |line: &String| line.chars().skip(1).collect::<String>();
        let fold: Vec<String> = unselected[1..].iter().map(strip).collect();
        let full: Vec<String> = selected[1..].iter().map(strip).collect();
        // The resting fold is identity + description + ctx bar + the token line.
        assert_eq!(
            fold.len(),
            4,
            "the fold is four card lines (incl. the token line): {fold:?}"
        );
        // The token line rides the resting fold, not a reveal-on-select detail.
        assert!(
            fold.iter().any(|line| line.contains("◇ ")),
            "the token line is part of the resting fold: {fold:?}"
        );
        // This card has no subagents, so selection appends nothing — it only
        // lights the gutter (already stripped), never reshaping a fold line.
        assert_eq!(
            fold, full,
            "selection only appends; it never reshapes the fold lines"
        );
    }

    /// The expanded card lists the agent's subagents (status glyph + type),
    /// nested under the parent and shown only when the row is selected — the
    /// resting card never reveals them, preserving the no-reflow invariant.
    #[test]
    fn expanded_card_lists_subagents_only_when_selected() {
        let parent = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        // A paneless child of the parent, still running — it nests onto the
        // parent's card during snapshot projection.
        let mut kid = agent(
            "kid-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            None,
            None,
            Some("Explore"),
        );
        kid.parent_agent_id = Some("claude-1".to_owned());
        let snapshot = snapshot_with(Vec::new(), vec![parent, kid]);
        let theme = Theme::fixed(true);
        let render = |selected_index: usize| {
            let mut row_index = 0;
            let mut lines = Vec::new();
            let mut map = Vec::new();
            worktree_group_lines(
                &theme,
                &snapshot.worktree_groups[0],
                &snapshot.providers,
                54,
                30 * 60,
                &mut row_index,
                selected_index,
                0,
                &mut lines,
                &mut map,
            );
            lines
                .into_iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let selected = render(0);
        assert!(
            selected.contains("subagents"),
            "expanded card lists subagents:\n{selected}"
        );
        assert!(
            selected.contains("Explore"),
            "the subagent type is shown:\n{selected}"
        );

        let resting = render(usize::MAX);
        assert!(
            !resting.contains("subagents"),
            "the resting card hides the subagent list:\n{resting}"
        );
    }

    /// The resting card is four lines (identity, description, ctx bar, token
    /// line); selecting it appends only the subagent list, so the deepest data is
    /// one keystroke away without ever reshaping a resting line.
    #[test]
    fn resting_card_is_four_lines_and_selection_only_appends() {
        // Card lines, excluding the group header.
        let resting = card_lines(usize::MAX).len() - 1;
        let selected = card_lines(0).len() - 1;
        assert_eq!(resting, 4, "identity, description, ctx, token line");
        // This single-agent fixture has no subagents, so selection appends
        // nothing — the resting height already carries every per-row stat.
        assert_eq!(selected, 4);
    }

    /// Render one worktree group's lines, asserting the hit-test map stays in
    /// lockstep so callers can read either the spans or their text.
    fn group_lines(
        snapshot: &SidebarSnapshot,
        theme: &Theme,
        selected_index: usize,
    ) -> Vec<Line<'static>> {
        let mut row_index = 0;
        let mut lines = Vec::new();
        let mut map = Vec::new();
        worktree_group_lines(
            theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            54,
            30 * 60,
            &mut row_index,
            selected_index,
            0,
            &mut lines,
            &mut map,
        );
        assert_eq!(map.len(), lines.len(), "map stays in lockstep with lines");
        lines
    }

    fn line_texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// A just-started idle agent — idle, on the `Some(0)` baseline gauge with no
    /// usage behind it — sheds the 0% context bar and the zeroed stats, resting at
    /// identity + description alone with nothing to append on selection. The same
    /// 0% reading while *running* still paints the bar, so the suppression is gated
    /// on idle, not merely on a zero percent.
    #[test]
    fn just_started_idle_agent_sheds_the_gauge_and_zeroed_stats() {
        let theme = Theme::fixed(true);
        let mk = |status| {
            let state = agent(
                "claude-1",
                "claude",
                status,
                PermissionPosture::Default,
                Some("/repo/main"),
                Some("main"),
                Some("warm up"),
            );
            snapshot_with(Vec::new(), vec![state])
        };

        let idle = mk(AgentStatus::Idle);
        let resting = line_texts(&group_lines(&idle, &theme, usize::MAX));
        let expanded = line_texts(&group_lines(&idle, &theme, 0));

        assert!(
            resting
                .iter()
                .all(|line| !line.contains('▣') && !line.contains('▢')),
            "fresh idle card hides the context bar:\n{}",
            resting.join("\n")
        );
        // Header + identity + description — no gauge or stats at rest.
        assert_eq!(resting.len(), 3, "{resting:?}");
        let joined = expanded.join("\n");
        assert!(
            !joined.contains('▣') && !joined.contains('◇'),
            "expanded fresh idle card hides the bar and the zeroed stats:\n{joined}"
        );
        // A fresh idle card has nothing to append on selection — no stats, no age,
        // no subagents — so expanding it adds no line.
        assert_eq!(expanded.len(), 3, "{expanded:?}");

        let running = line_texts(&group_lines(&mk(AgentStatus::Running), &theme, usize::MAX));
        assert!(
            running.iter().any(|line| line.contains('▢')),
            "a running 0% agent keeps its bar (the hollow ▢ at 0%):\n{}",
            running.join("\n")
        );
    }

    /// Consecutive cards in a group are separated by one blank line. The group is
    /// unselected here, so the separator carries the plain-space gutter (a lane
    /// spine would tint it) — exactly one all-blank line, never more.
    #[test]
    fn consecutive_cards_get_one_blank_separator() {
        let theme = Theme::fixed(true);
        let one = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("task one"),
        );
        let two = agent(
            "claude-2",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("task two"),
        );
        let snapshot = snapshot_with(Vec::new(), vec![one, two]);
        let rendered = line_texts(&group_lines(&snapshot, &theme, usize::MAX));

        let names: Vec<usize> = rendered
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains("claude"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            names.len(),
            2,
            "two cards in the group:\n{}",
            rendered.join("\n")
        );
        let blanks: Vec<usize> = rendered
            .iter()
            .enumerate()
            .filter(|(_, line)| line.trim().is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            blanks,
            vec![names[1] - 1],
            "one blank line sits between the cards:\n{}",
            rendered.join("\n")
        );
    }

    /// The agent name wears its provider's brand color (Claude's clay), tying the
    /// card to the provider dashboard. Read the expected index off the snapshot's
    /// own panel so the test follows config overrides.
    #[test]
    fn agent_name_wears_its_provider_brand_color() {
        let theme = Theme::fixed(false); // color on, so the brand tone survives
        let state = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        let mut snapshot = snapshot_with(Vec::new(), vec![state]);
        // Provider panels are producer-only (`with_provider_aggregates`), so the
        // reducer-built snapshot carries none — set one as the producer would.
        snapshot.providers = vec![provider_panel(
            "claude",
            "Claude Code",
            173,
            true,
            true,
            None,
        )];
        let expected = snapshot.providers[0].color;

        let lines = group_lines(&snapshot, &theme, usize::MAX);
        let name = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "claude")
            .expect("the agent name span");
        assert_eq!(
            name.style.fg,
            Some(Color::Indexed(expected)),
            "the agent name wears the provider color"
        );
    }

    /// Build a metered provider panel from two rate-limit windows, for the
    /// dashboard alignment and golden tests.
    fn provider_panel(
        kind: &str,
        product_name: &str,
        color: u8,
        metered: bool,
        remote_control: bool,
        windows: Option<(u8, u8)>,
    ) -> rimz::SidebarProviderPanel {
        let now = fixed_now();
        let window = |used: u8, mins: u32, resets_in: Duration| RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(now + resets_in),
            duration_mins: Some(mins),
        };
        rimz::SidebarProviderPanel {
            kind: kind.to_owned(),
            product_name: product_name.to_owned(),
            art: vec![
                " ▐▛███▜▌".to_owned(),
                "▝▜█████▛▘".to_owned(),
                "  ▘▘ ▝▝".to_owned(),
            ],
            color,
            version: Some("2.1.158".to_owned()),
            plan: Some("Claude Max".to_owned()),
            metered,
            remote_control,
            spending: Some(rimz::SpendTally {
                today: rimz::SpendWindow {
                    usd: 3.5,
                    tokens: 486_000,
                    input: 422_000,
                    output: 64_000,
                    cache_write: 12_000,
                    cache_read: 68_000,
                    sessions: 12,
                },
                ..Default::default()
            }),
            windows: windows
                .map(|(five, seven)| {
                    vec![
                        window(five, 5 * 60, Duration::from_secs(3 * 3_600 + 12 * 60)),
                        window(
                            seven,
                            7 * 24 * 60,
                            Duration::from_secs(3 * 86_400 + 4 * 3_600),
                        ),
                    ]
                })
                .unwrap_or_default(),
        }
    }

    /// Every provider bar — `5h`, `7d` across blocks, and the unmetered `∞` —
    /// shares one front (bar-start) column and one end (bar-end) column, so the
    /// whole dashboard reads as one aligned grid. The structural payoff of the
    /// shared bar grammar, now that the budgets live in the panel.
    #[test]
    fn provider_bars_share_one_front_and_end_column() {
        let theme = Theme::fixed(true);
        let panels = vec![
            provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
            provider_panel("codex", "Codex", 33, true, false, Some((55, 8))),
            provider_panel("pi", "Pi", 28, false, false, None),
        ];
        // Rendered narrow so the art column is dropped and the bar lines carry no
        // stray block glyphs from the emblem — the bar grid is what we measure.
        let lines: Vec<String> = provider_panel_lines(&theme, &panels, 30)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .filter(|line| line.contains('▰') || line.contains('▱') || line.contains('▒'))
            .collect();
        assert!(lines.len() >= 5, "two metered providers + one ∞: {lines:?}");
        // Bar start: the first bar cell (tick or shade), by char column.
        let start = |line: &str| {
            line.chars()
                .position(|c| matches!(c, '▰' | '▱' | '▒'))
                .unwrap()
        };
        let starts: Vec<usize> = lines.iter().map(|line| start(line)).collect();
        assert!(
            starts.iter().all(|&s| s == starts[0]),
            "provider bars share a front column: {starts:?}"
        );
        // Bar end: the last bar cell column.
        let end = |line: &str| {
            line.char_indices()
                .filter(|(_, c)| matches!(c, '▰' | '▱' | '▒'))
                .count()
                + start(line)
        };
        let ends: Vec<usize> = lines.iter().map(|line| end(line)).collect();
        assert!(
            ends.iter().all(|&e| e == ends[0]),
            "provider bars share an end column: {ends:?}"
        );
    }

    /// The metered bar rows of one panel (5h then 7d), rendered narrow so the art
    /// column drops and each row's first span is its label. Filters to the lines
    /// carrying bar glyphs.
    fn metered_bar_rows(theme: &Theme, panel: &rimz::SidebarProviderPanel) -> Vec<Line<'static>> {
        provider_panel_lines(theme, std::slice::from_ref(panel), 30)
            .into_iter()
            .filter(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains('▰') || span.content.contains('▱'))
            })
            .collect()
    }

    /// The label foreground, the first bar-glyph foreground, and whether the row
    /// carries a `↻` reset countdown — the three things req 1/2 turn on.
    fn bar_row_facts(line: &Line<'static>) -> (Option<Color>, Option<Color>, bool) {
        let label_fg = line.spans.first().and_then(|span| span.style.fg);
        let glyph_fg = line
            .spans
            .iter()
            .find(|span| span.content.contains('▰') || span.content.contains('▱'))
            .and_then(|span| span.style.fg);
        let has_reset = line.spans.iter().any(|span| span.content.contains('↻'));
        (label_fg, glyph_fg, has_reset)
    }

    /// Each `5h`/`7d` label mirrors its own bar's severity color, so a green and a
    /// yellow window read as two differently-toned rows, not one dim slab.
    #[test]
    fn provider_label_mirrors_its_bar_color() {
        let theme = Theme::fixed(false);
        // 5h: 25% used → 75% left → green. 7d: 70% used → 30% left → yellow.
        let panel = provider_panel("claude", "Claude", 173, true, false, Some((25, 70)));
        let rows = metered_bar_rows(&theme, &panel);
        assert_eq!(rows.len(), 2, "a metered panel draws a 5h and a 7d row");
        let (five_label, five_glyph, _) = bar_row_facts(&rows[0]);
        let (seven_label, seven_glyph, _) = bar_row_facts(&rows[1]);
        assert_eq!(five_label, five_glyph, "5h label mirrors its bar");
        assert_eq!(seven_label, seven_glyph, "7d label mirrors its bar");
        assert_ne!(
            five_label, seven_label,
            "a green 5h and a yellow 7d label differ in tone"
        );
    }

    /// A spent weekly cap gates the short window: with 7d exhausted the 5h row is
    /// painted exhausted — red, a full empty track, and no reset countdown —
    /// regardless of the 5h window's own (here untouched) reading.
    #[test]
    fn seven_day_exhaustion_reddens_and_silences_the_five_hour_row() {
        let theme = Theme::fixed(false);
        // 5h is untouched (would be green with a countdown); 7d is fully spent.
        let panel = provider_panel("claude", "Claude", 173, true, false, Some((0, 100)));
        let rows = metered_bar_rows(&theme, &panel);
        assert_eq!(rows.len(), 2);
        let (five_label, _, five_has_reset) = bar_row_facts(&rows[0]);
        let (seven_label, _, _) = bar_row_facts(&rows[1]);
        assert!(!five_has_reset, "the cascaded 5h row drops its countdown");
        assert!(
            !rows[0].spans.iter().any(|span| span.content.contains('▰')),
            "the cascaded 5h bar is a full empty track, no fill"
        );
        assert_eq!(
            five_label, seven_label,
            "the cascaded 5h label reddens to match the exhausted 7d"
        );
    }

    /// A provider that reports a single window draws exactly one bar, labeled by
    /// the window's own length — the model isn't pinned to a fixed set. (A
    /// transient Codex server bug once widened its window to ~30 days; this is what
    /// rendered, instead of mislabeling it `7d`.)
    #[test]
    fn single_window_panel_draws_one_bar_labeled_by_length() {
        let theme = Theme::fixed(false);
        let now = fixed_now();
        let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
        codex.windows = vec![RateLimitWindow {
            used_percentage: Some(7),
            resets_at: Some(now + Duration::from_secs(28 * 86_400 + 4 * 3_600)),
            duration_mins: Some(43_800),
        }];
        let rows = metered_bar_rows(&theme, &codex);
        assert_eq!(rows.len(), 1, "one window → one bar");
        let label = rows[0]
            .spans
            .first()
            .expect("a label span")
            .content
            .trim()
            .to_owned();
        assert_eq!(label, "30d", "the ~30-day window is labeled 30d");
        let (_, _, has_reset) = bar_row_facts(&rows[0]);
        assert!(has_reset, "the bar carries its reset countdown");
    }

    /// A not-started window drops its countdown — these budgets begin counting
    /// only on the first token, so until then the provider keeps `resets_at` slid a
    /// full window-length ahead. It's detected by the reset distance, not a 0%
    /// reading: the real Codex shape is `usedPercent: 1` with the reset still ~a
    /// full 5h out (`4h59m`). Its bar shows near-full with no countdown.
    #[test]
    fn not_started_window_drops_its_countdown() {
        let theme = Theme::fixed(false);
        let now = fixed_now();
        let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
        // The real not-started shape: ~1% used, reset slid a full 5h ahead (a hair
        // under, here 4h59m30s, the way a live reading reads).
        claude.windows = vec![RateLimitWindow {
            used_percentage: Some(1),
            resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
            duration_mins: Some(5 * 60),
        }];
        let rows = metered_bar_rows(&theme, &claude);
        assert_eq!(rows.len(), 1);
        let (_, _, has_reset) = bar_row_facts(&rows[0]);
        assert!(
            !has_reset,
            "a not-started window (reset ~ full 5h) shows no countdown"
        );
        assert!(
            rows[0].spans.iter().any(|span| span.content.contains('▰')),
            "the not-started window shows a near-full bar, not an empty/exhausted track"
        );
    }

    /// The full provider stats line (all spans joined) of one rendered panel.
    fn stats_line(theme: &Theme, panel: &rimz::SidebarProviderPanel) -> String {
        provider_panel_lines(theme, std::slice::from_ref(panel), 40)
            .into_iter()
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect()
    }

    /// The provider stats line reads today's transcript-history spend *and* token
    /// burn from the JSONL `spending`, never the live active-session sum — the one
    /// figure that also holds for a token-only provider (Codex) with no live cost.
    #[test]
    fn provider_stats_read_todays_jsonl_spend_and_tokens() {
        let theme = Theme::fixed(false);
        let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
        codex.spending = Some(rimz::SpendTally {
            today: rimz::SpendWindow {
                usd: 4.20,
                tokens: 486_000,
                input: 422_000,
                output: 64_000,
                cache_write: 0,
                cache_read: 68_000,
                sessions: 5,
            },
            ..Default::default()
        });
        let stats = stats_line(&theme, &codex);
        assert!(stats.contains("$4.20"), "today's JSONL spend: {stats:?}");
        // The today line reads the coarse integer form (`◇ 486k`), with the split.
        assert!(stats.contains("486k"), "today's JSONL tokens: {stats:?}");
        assert!(stats.contains("↗ 64k"), "the output split: {stats:?}");
    }

    /// A started window — its reset has ticked well below the full window — keeps
    /// its countdown, even at the same low 1% usage as a not-started one. Usage
    /// alone can't tell them apart; the reset distance does.
    #[test]
    fn started_window_keeps_its_countdown() {
        let theme = Theme::fixed(false);
        let now = fixed_now();
        let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
        claude.windows = vec![RateLimitWindow {
            used_percentage: Some(1),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            duration_mins: Some(5 * 60),
        }];
        let rows = metered_bar_rows(&theme, &claude);
        assert_eq!(rows.len(), 1);
        let (_, _, has_reset) = bar_row_facts(&rows[0]);
        assert!(
            has_reset,
            "a started window (reset well below full) shows its countdown"
        );
    }

    /// Usage above the ~1% not-started floor means the window has started — keep its
    /// countdown even when the reset still reads a near-full window. The reset-distance
    /// grace only applies to a window at or below the floor (0–1% used); any real
    /// usage short-circuits to "started".
    #[test]
    fn used_window_keeps_countdown_despite_near_full_reset() {
        let theme = Theme::fixed(false);
        let now = fixed_now();
        let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
        // 5% used with the reset slid a full 5h out: usage above the floor wins, so
        // this counts as started despite the near-full reset.
        claude.windows = vec![RateLimitWindow {
            used_percentage: Some(5),
            resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
            duration_mins: Some(5 * 60),
        }];
        let rows = metered_bar_rows(&theme, &claude);
        assert_eq!(rows.len(), 1);
        let (_, _, has_reset) = bar_row_facts(&rows[0]);
        assert!(
            has_reset,
            "usage above ~1% shows the countdown even with a near-full reset"
        );
    }

    /// The pinned per-provider dashboard: a metered block (header with version
    /// and plan on the left, the `⇅ rc` flag pinned top-right; the brand emblem;
    /// 5h/7d "mana" bars draining toward their resets) above an unmetered block
    /// (the `∞` icon at the front, an empty `▱` track, no countdown).
    #[test]
    fn render_provider_dashboard_pins_panel_with_bars_and_rc_flag() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
        snapshot.providers = vec![
            provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
            {
                let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
                codex.plan = Some("ChatGPT Pro".to_owned());
                codex.version = Some("0.135.0".to_owned());
                codex.spending = Some(rimz::SpendTally {
                    today: rimz::SpendWindow {
                        usd: 1.2,
                        tokens: 88_000,
                        input: 76_000,
                        output: 12_000,
                        cache_write: 0,
                        cache_read: 8_000,
                        sessions: 3,
                    },
                    ..Default::default()
                });
                codex
            },
        ];
        let rendered = snapshot_to_screen(&snapshot, 54, 34);

        // The metered Claude block: header carries the version and plan on the
        // left with the `⇅ rc` remote-control flag pinned to the top-right corner,
        // then drains its 5h/7d budget bars.
        assert!(
            rendered.contains("Claude v2.1.158 · Claude Max"),
            "{rendered}"
        );
        assert!(
            rendered.contains("⇅ rc"),
            "rc flag pinned right:\n{rendered}"
        );
        assert!(rendered.contains("5h"), "{rendered}");
        assert!(rendered.contains("7d"), "{rendered}");
        assert!(rendered.contains('▰'), "a draining mana bar:\n{rendered}");
        assert!(rendered.contains('↻'), "a reset countdown:\n{rendered}");
        // The unmetered Codex block: the `∞` icon rides the front, an empty `▱`
        // track fills, and no countdown follows it.
        assert!(
            rendered.contains("Codex v0.135.0 · ChatGPT Pro"),
            "{rendered}"
        );
        assert!(rendered.contains('∞'), "infinity at the front:\n{rendered}");
        assert!(rendered.contains('▱'), "the empty ∞ track:\n{rendered}");
        assert_snapshot("provider_dashboard", rendered);
    }

    /// The fleet ledger pinned at the bottom of the dashboard: the static
    /// `W:` (week) and `M:` (month) rows, each reading `◎ sessions  ◇ ↘ ↗ ◌
    /// $spend` across every provider — precise one-decimal token figures and the
    /// bold money-green spend, right-aligned into one aligned grid. Today's
    /// headline stays in the cockpit's animated `$`, never repeated here.
    #[test]
    fn render_fleet_ledger_pins_week_month_rows_under_the_dashboard() {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Auto,
            Some("/repo/main"),
            Some("main"),
            Some("db migrate"),
        );
        claude.context = Some(claude_context(fixed_now()));
        let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
        snapshot.providers = vec![provider_panel(
            "claude",
            "Claude",
            173,
            true,
            true,
            Some((25, 40)),
        )];
        snapshot.value_tally = Some(rimz::SpendTally {
            today: rimz::SpendWindow {
                usd: 40.23,
                tokens: 3_300_000,
                input: 300_000,
                output: 3_000_000,
                cache_write: 120_000,
                cache_read: 6_800_000,
                sessions: 12,
            },
            week: rimz::SpendWindow {
                usd: 312.40,
                tokens: 21_000_000,
                input: 2_300_000,
                output: 18_700_000,
                cache_write: 900_000,
                cache_read: 51_000_000,
                sessions: 92,
            },
            month: rimz::SpendWindow {
                usd: 1_240.57,
                tokens: 33_000_000,
                input: 4_300_000,
                output: 28_700_000,
                cache_write: 1_900_000,
                cache_read: 121_000_000,
                sessions: 212,
            },
            year: rimz::SpendWindow {
                usd: 4_821.90,
                tokens: 47_200_000,
                input: 7_200_000,
                output: 40_000_000,
                cache_write: 3_000_000,
                cache_read: 210_000_000,
                sessions: 980,
            },
        });
        let rendered = snapshot_to_screen(&snapshot, 60, 34);

        // The `W:` and `M:` rows: each labelled left, the `$` spend pinned right.
        assert!(rendered.contains("W:"), "the week ledger row:\n{rendered}");
        assert!(rendered.contains("M:"), "the month ledger row:\n{rendered}");
        assert!(
            rendered.contains("$312.40"),
            "this week's spend:\n{rendered}"
        );
        assert!(
            rendered.contains("$1,240.57"),
            "this month's spend:\n{rendered}"
        );
        // Session counts and the precise (one-decimal) token total.
        assert!(rendered.contains("212"), "month session count:\n{rendered}");
        assert!(
            rendered.contains("33.0M"),
            "month token total, precise form:\n{rendered}"
        );
        // The `year` window is no longer surfaced — the ledger tops out at month.
        assert!(
            !rendered.contains("$4,821.90"),
            "the year pile is gone from the ledger:\n{rendered}"
        );
        assert_snapshot("fleet_ledger", rendered);
    }

    /// The borderless repo header (dashboard L1): the workspace name behind `⌘`
    /// on the left, then the project path pinned to the right edge of the same
    /// line — no `⌂` glyph, the dim path opposite the name reads as a path.
    #[test]
    fn repo_header_shows_name_then_path() {
        let mut snapshot = snapshot_with(Vec::new(), Vec::new());
        snapshot.project_root = Some(std::path::PathBuf::from("/srv/code/query-engine"));
        let rendered = snapshot_to_screen(&snapshot, 44, 12);
        let first = rendered.lines().next().unwrap_or_default();
        let name_at = first.find("⌘ query-engine").expect("name on line 1");
        let path_at = first
            .find("/srv/code/query-engine")
            .expect("path on line 1");
        assert!(name_at < path_at, "name leads, path pins right: {first:?}");
        assert!(
            !rendered.contains('⌂'),
            "the ⌂ path glyph is gone:\n{rendered}"
        );
    }

    #[test]
    fn home_abbreviation_collapses_only_a_home_prefix() {
        assert_eq!(
            abbreviate_under("/home/dev/code/query-engine", Some("/home/dev")),
            "~/code/query-engine"
        );
        assert_eq!(abbreviate_under("/home/dev", Some("/home/dev")), "~");
        // A path that merely shares a textual prefix is not under home.
        assert_eq!(
            abbreviate_under("/home/developer/x", Some("/home/dev")),
            "/home/developer/x"
        );
        // Outside home, or no home, passes through.
        assert_eq!(
            abbreviate_under("/srv/code", Some("/home/dev")),
            "/srv/code"
        );
        assert_eq!(abbreviate_under("/srv/code", None), "/srv/code");
    }

    /// The cockpit summary carries the fleet count (`¤ N`, under the name); the
    /// cockpit below it splits the make-up at a fixed height — the left cluster
    /// (`? ! ○ ⏸`, each glyph spaced from its count, a zero reading `? 0`) and the
    /// busy/done tail (`✽ ⢿ ✓`) — so the body never shifts as agents change state.
    #[test]
    fn fleet_header_is_fixed_and_splits_the_make_up() {
        // Borderless layout: row 0 is the name, row 1 a blank line, row 2 the `¤`
        // summary, row 3 the `◎` summary, row 4 the hairline rule. An empty room
        // reads `¤ 0` on row 2 with no make-up beneath, so the body never moves.
        let empty = snapshot_with(Vec::new(), Vec::new());
        let empty_screen = snapshot_to_screen(&empty, 40, 12);
        assert!(
            empty_screen.lines().nth(2).unwrap().contains("¤ 0"),
            "{empty_screen}"
        );
        assert!(
            empty_screen.lines().nth(3).unwrap().contains("◎ 0"),
            "{empty_screen}"
        );

        let working = agent(
            "w",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Default,
            Some("/repo/main"),
            Some("main"),
            Some("a"),
        );
        let thinking = agent(
            "t",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Plan,
            Some("/repo/main"),
            Some("main"),
            Some("b"),
        );
        let snapshot = snapshot_with(Vec::new(), vec![working, thinking]);
        let screen = snapshot_to_screen(&snapshot, 40, 12);
        // Row 2 is the `¤` summary, row 3 the `◎` summary; row 5 is the bucket
        // make-up (row 1 is the blank line, row 4 the hairline rule).
        assert!(screen.lines().nth(2).unwrap().contains("¤ 2"), "{screen}");
        let buckets = screen.lines().nth(5).unwrap();
        // Left cluster: waiting/failed/idle and the parked rate-limited each show
        // their count (a zero reads `? 0`); the running pair splits one working
        // (⢿) one thinking (✽) into the right cluster.
        assert!(buckets.contains("? 0"), "{buckets}");
        assert!(buckets.contains("! 0"), "{buckets}");
        assert!(buckets.contains("○ 0"), "{buckets}");
        // The rate-limited glyph carries the U+FE0E text-presentation selector.
        assert!(buckets.contains("⏸\u{FE0E} 0"), "{buckets}");
        assert!(buckets.contains("⢿ 1"), "{buckets}");
        assert!(buckets.contains("✽ 1"), "{buckets}");
        // The default selection lands on the first row, so its worktree reads as
        // one lane: the header gains the dotted seal and a `▏` lane spine.
        assert!(
            screen.lines().any(|line| line.contains("main")),
            "fleet header wrapped or shifted:\n{screen}"
        );
        assert!(
            screen.contains('▏'),
            "the selected worktree shows the lane spine:\n{screen}"
        );
    }

    /// A compacting agent in plan posture counts as **working** (`⢿`), not
    /// thinking (`✽`). Compaction is active context work regardless of posture.
    #[test]
    fn compacting_agent_in_plan_mode_counts_as_working() {
        let mut compacting = agent(
            "c",
            "claude",
            AgentStatus::Running,
            PermissionPosture::Plan,
            Some("/repo/main"),
            Some("main"),
            Some("t"),
        );
        compacting.compacting_since = Some(fixed_now());
        let snapshot = snapshot_with(Vec::new(), vec![compacting]);
        let screen = snapshot_to_screen(&snapshot, 40, 12);
        // Row 5 is the make-up: name(0), blank(1), `¤`(2), `◎`(3), hairline(4).
        let buckets = screen.lines().nth(5).unwrap();
        assert!(
            buckets.contains("⢿ 1"),
            "compacting counts as working: {buckets}"
        );
        assert!(
            buckets.contains("✽ 0"),
            "compacting does not count as thinking: {buckets}"
        );
    }
}
