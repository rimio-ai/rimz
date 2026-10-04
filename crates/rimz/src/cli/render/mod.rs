//! Human-facing CLI presentation: one styled stdout path plus terminal prose,
//! borderless auto-fit tables, and aligned key/value blocks, so every `rimz`
//! command reads consistently and in the room's palette.
//!
//! `--json` output and snapshot tests stay byte-clean: [`out`] writes through
//! `anstream`, which strips ANSI when stdout is not a terminal or color is
//! disabled (`NO_COLOR`/`CLICOLOR`, or `--color never`). Writes go through
//! `writeln!`, not the `print!` macros, matching the `print_json` stdout path —
//! the `print_stdout` lint still guards the protocol surface.

pub(crate) mod diff;
pub(crate) mod palette;
pub(crate) mod prose;
pub(crate) mod room;
mod roster;
pub(crate) mod status;

pub(crate) use roster::{Roster, RosterRow, RosterSignal};

use std::io::Write;

use jiff::Timestamp;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Prefix every written line with the loop output gutter.
pub(crate) struct GutterWriter<W: Write> {
    inner: W,
    at_line_start: bool,
    prefix: String,
}

impl<W: Write> GutterWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            at_line_start: true,
            prefix: format!("  {}", paint(palette::faint(), "│ ")),
        }
    }
}

impl<W: Write> Write for GutterWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.at_line_start {
            self.inner.write_all(self.prefix.as_bytes())?;
            self.at_line_start = false;
        }
        let end = buf
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buf.len(), |idx| idx + 1);
        let written = self.inner.write(&buf[..end])?;
        if written > 0 {
            self.at_line_start = buf[written - 1] == b'\n';
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Styled stdout for human command output. Lock it once and write the whole
/// block through it.
pub(crate) fn out() -> anstream::AutoStream<std::io::StdoutLock<'static>> {
    anstream::AutoStream::auto(std::io::stdout().lock())
}

/// Styled stderr for human progress and consent output — the [`out`] sibling
/// for surfaces that must not touch the stdout protocol channel. ANSI is
/// stripped when stderr is not a terminal or color is disabled.
pub(crate) fn err() -> anstream::AutoStream<std::io::StderrLock<'static>> {
    anstream::AutoStream::auto(std::io::stderr().lock())
}

pub(crate) fn warn_unreadable_lanes(lifetimes: &rimz::agents::attribution::LaneLifetimes) {
    for (path, reason) in lifetimes.unreadable() {
        let _ = writeln!(
            err(),
            "{} {}: {reason}; its sessions are left out of seat totals",
            paint(palette::warn().bold(), "warning:"),
            path.display()
        );
    }
}

/// Render best-effort browser client customization warnings on stderr.
pub(crate) fn web_warnings(warnings: &[rimz::web::WebWarning]) {
    let mut stderr = err();
    for warning in warnings {
        let skipped = match warning {
            rimz::web::WebWarning::BrowserClientSkipped(detail) => {
                Some(("browser terminal fixes", detail))
            }
            rimz::web::WebWarning::BrowserFontSkipped(detail) => Some(("browser font", detail)),
            rimz::web::WebWarning::BrowserThemeSkipped(detail) => Some(("browser theme", detail)),
            rimz::web::WebWarning::HeaderAuthUnprotected(detail) => {
                let _ = writeln!(stderr, "rimz: warning: {detail}");
                None
            }
            rimz::web::WebWarning::BroadcastUnauthenticated(detail) => {
                let _ = writeln!(stderr, "rimz: warning: {detail}");
                None
            }
        };
        if let Some((surface, detail)) = skipped {
            let _ = writeln!(stderr, "rimz: skipping {surface}: {detail}");
        }
    }
}

/// Render a command failure for a human, suppressing source messages already
/// embedded in their parent error. A stderr write failure cannot replace the
/// command failure that brought us here.
pub(crate) fn report(error: &anyhow::Error) {
    let _ = write_report(&mut err(), error);
}

fn write_report(w: &mut impl Write, error: &anyhow::Error) -> std::io::Result<()> {
    let mut messages = distinct_messages(error).into_iter();
    let Some(message) = messages.next() else {
        return Ok(());
    };
    let mut lines = message.lines();
    match lines.next() {
        Some(line) => writeln!(w, "{} {line}", paint(palette::alarm().bold(), "error:"))?,
        None => writeln!(w, "{}", paint(palette::alarm().bold(), "error:"))?,
    }
    for line in lines {
        writeln!(w, "  {line}")?;
    }

    for message in messages {
        for line in message.lines() {
            writeln!(w, "  {line}")?;
        }
    }
    Ok(())
}

/// An error's messages outermost first, each trimmed, without any source the
/// message kept before it already contains.
fn distinct_messages(error: &anyhow::Error) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        let message = message.trim();
        if kept.last().is_some_and(|last| last.contains(message)) {
            continue;
        }
        kept.push(message.to_owned());
    }
    kept
}

/// The error chain as plain text for a durable record: what `report` prints,
/// joined by `: ` in place of its prefix, indentation, and color.
pub(crate) fn error_line(error: &anyhow::Error) -> String {
    distinct_messages(error).join(": ")
}

/// Finish a stdout emission, treating a consumer that stopped reading as a
/// clean end rather than a fault. Any other write error propagates.
///
/// A broken pipe calls `std::process::exit(0)` on the spot, so call this
/// after every side effect the command owes, never inside a loop that still
/// has work to do.
pub(crate) fn finish(write: std::io::Result<()>) -> anyhow::Result<()> {
    match write {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(err) => Err(err.into()),
    }
}

/// Emit one compact JSON document followed by one newline.
pub(crate) fn json<T: serde::Serialize + ?Sized>(value: &T) -> anyhow::Result<()> {
    write_json(&mut std::io::stdout().lock(), value, false)
}

/// Emit one pretty-printed JSON document followed by one newline.
pub(crate) fn json_pretty<T: serde::Serialize + ?Sized>(value: &T) -> anyhow::Result<()> {
    write_json(&mut std::io::stdout().lock(), value, true)
}

fn write_json<W: Write, T: serde::Serialize + ?Sized>(
    writer: &mut W,
    value: &T,
    pretty: bool,
) -> anyhow::Result<()> {
    let serialized = if pretty {
        serde_json::to_writer_pretty(&mut *writer, value)
    } else {
        serde_json::to_writer(&mut *writer, value)
    };
    match serialized {
        Ok(()) => {}
        Err(error) if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) => {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    match writer.write_all(b"\n") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Wrap `text` in `style`'s ANSI for inline use inside a larger line — the
/// `anstream` stream strips it when color is off. Cells in [`Table`]/[`KeyVals`]
/// carry their own style; reach for this only when one styled span sits within
/// an otherwise plain `writeln!`.
pub(crate) fn paint(style: anstyle::Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

/// A shape-readable verdict glyph paired with its typed state tone.
pub(crate) fn verdict(role: status::StateRole) -> (&'static str, anstyle::Style) {
    let glyph = match role {
        status::StateRole::Success => "✓",
        status::StateRole::Working => "▸",
        status::StateRole::Waiting => "!",
        status::StateRole::Paused => "⏸",
        status::StateRole::Failed | status::StateRole::Unavailable => "✗",
        status::StateRole::Neutral => "·",
    };
    (glyph, status::role(role))
}

/// Frame captured pane text in quiet terminal chrome, with `title` embedded in
/// the top border. ANSI inside the capture remains intact and does not affect
/// the frame or its padding.
pub(crate) fn pane_frame(w: &mut impl Write, title: &str, text: &str) -> std::io::Result<()> {
    let title_width = title.width();
    let inner_width = text
        .split_terminator('\n')
        .map(|line| anstream::adapter::strip_str(line).to_string().width())
        .max()
        // The leading dash and spaces around the title need one more column
        // than the content itself for every edge to stay aligned.
        .unwrap_or(0)
        .max(title_width + 1);
    let top_fill = inner_width - title_width - 1;
    let border_fill = inner_width + 2;

    write!(w, "{}", paint(palette::faint(), "╭─ "))?;
    write!(w, "{}", paint(palette::muted(), title))?;
    writeln!(
        w,
        "{}",
        paint(palette::faint(), &format!(" {}╮", "─".repeat(top_fill)))
    )?;

    for line in text.split_terminator('\n') {
        let line_width = anstream::adapter::strip_str(line).to_string().width();
        write!(w, "{}", paint(palette::faint(), "│ "))?;
        write!(w, "{line}{}", anstyle::Reset.render())?;
        write!(w, "{:width$}", "", width = inner_width - line_width)?;
        writeln!(w, "{}", paint(palette::faint(), " │"))?;
    }

    writeln!(
        w,
        "{}",
        paint(palette::faint(), &format!("╰{}╯", "─".repeat(border_fill)))
    )
}

/// Render an absolute path relative to `$HOME` as `~`/`~/rest`, so cwd columns
/// read at a glance. Leaves any path outside `$HOME` (or when `$HOME` is unset)
/// untouched.
pub(crate) fn home_relative(path: &str) -> String {
    let home = std::env::var_os("HOME");
    home_relative_to(home.as_ref().and_then(|home| home.to_str()), path)
}

/// Render a path a command holds as a [`Path`]: `..` folded away, then the home
/// directory abbreviated. The worktree directory template is `../{repo}-worktrees`
/// by default, so a configured tree reaches its printer unfolded: the two
/// surfaces that print one from the template, the create report and the agent
/// exit hint, come through here to agree with `rimz worktree list`. The sweep
/// and cleanup rows print git-resolved paths and do not.
pub(crate) fn home_relative_path(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME");
    home_relative_path_to(home.as_ref().and_then(|home| home.to_str()), path)
}

/// [`home_relative_path`] against a home the caller supplies.
pub(crate) fn home_relative_path_to(home: Option<&str>, path: &std::path::Path) -> String {
    home_relative_to(
        home,
        &rimz::utils::path::normalize_path_lexical(path).to_string_lossy(),
    )
}

pub(crate) fn agent_activity_line(
    agent: &rimz::agents::AgentState,
    card: Option<&rimz::store::snapshot::AgentCard>,
) -> Option<String> {
    card.and_then(rimz::store::snapshot::AgentCard::activity_description)
        .and_then(rimz::agents::single_line_description)
        .or_else(|| agent.activity_line())
}

/// Collapse a diagnostic into one terminal-friendly line.
pub(crate) fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Render an error's actionable source without repeating its outer context.
pub(crate) fn one_line_error(error: &(dyn std::error::Error + 'static)) -> String {
    one_line(
        &error
            .source()
            .map(ToString::to_string)
            .unwrap_or_else(|| error.to_string()),
    )
}

pub(crate) use rimz::theme::fmt::fmt_bytes;

/// Format large counts compactly for token-oriented CLI surfaces.
pub(crate) fn compact_count(value: u64) -> String {
    rimz::theme::fmt::compact_count(value)
}

pub(crate) fn rel_age(ts: Timestamp, now: Timestamp) -> String {
    let age = now.duration_since(ts);
    if age.is_negative() {
        return "now".to_owned();
    }
    let secs = age.as_secs().max(0) as u64;
    format!("{} ago", age_label(secs))
}

/// The state word `lsp list`, `lsp status`, and `doctor` print for a registry state.
pub(crate) fn lsp_state_label(state: &rimz::lsp::registry::State) -> String {
    use rimz::lsp::registry::State;
    match state {
        State::Starting => "starting".into(),
        State::Indexing => "indexing".into(),
        State::Ready => "ready".into(),
        State::Dormant { reason, .. } => reason.map_or_else(
            || "not started".into(),
            |reason| format!("dormant: {reason}"),
        ),
        State::Stopped { reason, .. } => format!("stopped: {reason}"),
    }
}

pub(crate) fn age_label(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Descending non-zero units from days to seconds, no separators: `4m12s`, `1h3m`, `0s`.
pub(crate) fn format_compact_duration(mut seconds: u64) -> String {
    let mut rendered = String::new();
    for (unit_seconds, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        let amount = seconds / unit_seconds;
        if amount > 0 {
            rendered.push_str(&format!("{amount}{suffix}"));
            seconds %= unit_seconds;
        }
    }
    if seconds > 0 || rendered.is_empty() {
        rendered.push_str(&format!("{seconds}s"));
    }
    rendered
}

pub(crate) fn age_short(ts: Timestamp, now: Timestamp) -> String {
    let age = now.duration_since(ts);
    age_label(age.as_secs().max(0) as u64)
}

/// What is left of a window, in the tone the sidebar's budget bar has there.
pub(crate) fn percent_left_cell(left: u8) -> Cell {
    cell(format!("{left}%")).fg(palette::budget(left))
}

/// A rate-limit window's table cell in what is left: `∞` when lifted,
/// `N% · ready` before its clock starts, else the percentage with its reset
/// countdown when known. `None` without a reading.
pub(crate) fn window_cell(window: &rimz::agents::RateLimitWindow, now: Timestamp) -> Option<Cell> {
    if window.lifted {
        return Some(cell("∞"));
    }
    let percent = percent_left_cell(window.remaining_percentage(now)?);
    if window.not_started(now) {
        return Some(percent.suffix("· ready", palette::body()));
    }
    Some(match window.resets_at {
        Some(deadline) => percent.suffix(
            format!("· {}", rimz::theme::fmt::reset_countdown(deadline, now)),
            palette::body(),
        ),
        None => percent,
    })
}

pub(crate) fn terminal_columns(fallback: usize) -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(width), _)| usize::from(width))
        .unwrap_or(fallback)
}

pub(crate) fn terminal_rows(fallback: usize) -> usize {
    terminal_size::terminal_size()
        .map(|(_, terminal_size::Height(height))| usize::from(height))
        .unwrap_or(fallback)
}

pub(crate) fn rel_until(ts: Timestamp, now: Timestamp) -> String {
    let until = ts.duration_since(now);
    if until.is_negative() || until.is_zero() {
        return "due".to_owned();
    }
    let secs = until.as_secs().max(0) as u64;
    if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3_600 {
        format!("in {}m", secs / 60)
    } else if secs < 86_400 {
        let (hours, minutes) = (secs / 3_600, secs % 3_600 / 60);
        if minutes == 0 {
            format!("in {hours}h")
        } else {
            format!("in {hours}h {minutes}m")
        }
    } else {
        format!("in {}d", secs / 86_400)
    }
}

pub(crate) fn until_label(ts: Timestamp, now: Timestamp) -> String {
    let until = ts.duration_since(now);
    if until.is_negative() || until.is_zero() {
        return "due".to_owned();
    }
    age_label(until.as_secs().max(0) as u64)
}

fn home_relative_to(home: Option<&str>, path: &str) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return path.to_owned();
    };
    if path == home {
        return "~".to_owned();
    }
    match path
        .strip_prefix(home)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(rest) => format!("~/{rest}"),
        None => path.to_owned(),
    }
}

/// One column's horizontal alignment within an auto-fit [`Table`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Right,
}

/// A single table or key/value cell: plain text plus an optional palette style.
#[derive(Clone)]
pub(crate) struct Cell {
    text: String,
    style: Option<anstyle::Style>,
    suffix: Option<(String, anstyle::Style)>,
}

/// Start a plain (unstyled) cell from any text.
pub(crate) fn cell(text: impl Into<String>) -> Cell {
    Cell {
        text: text.into(),
        style: None,
        suffix: None,
    }
}

impl Cell {
    /// Paint this cell with a palette style.
    pub(crate) fn fg(mut self, style: anstyle::Style) -> Self {
        self.style = Some(style);
        self
    }

    /// Append a separately styled label after this cell's primary text.
    pub(crate) fn suffix(mut self, text: impl Into<String>, style: anstyle::Style) -> Self {
        self.suffix = Some((text.into(), style));
        self
    }

    /// Render a placeholder dash faintly; a no-op for any other text. Lets
    /// optional columns recede their empty `-` without a branch at each call.
    pub(crate) fn dash(self) -> Self {
        if self.text == "-" {
            self.fg(palette::faint())
        } else {
            self
        }
    }

    fn width(&self) -> usize {
        self.text.width() + self.suffix.as_ref().map_or(0, |(text, _)| 1 + text.width())
    }

    fn write_styled(&self, w: &mut impl Write) -> std::io::Result<()> {
        match self.style {
            Some(style) => write!(w, "{}{}{}", style.render(), self.text, style.render_reset()),
            None => write!(w, "{}", self.text),
        }?;
        if let Some((text, style)) = &self.suffix {
            write!(w, " {}{}{}", style.render(), text, style.render_reset())?;
        }
        Ok(())
    }

    fn clipped(&self, width: usize) -> Self {
        let suffix_width = self.suffix.as_ref().map_or(0, |(text, _)| 1 + text.width());
        Cell {
            text: clip_to_width(&self.text, width.saturating_sub(suffix_width)),
            style: self.style,
            suffix: self.suffix.clone(),
        }
    }

    fn write_padded(&self, w: &mut impl Write, width: usize, align: Align) -> std::io::Result<()> {
        let pad = width.saturating_sub(self.width());
        match align {
            Align::Left => {
                self.write_styled(w)?;
                write!(w, "{:pad$}", "", pad = pad)
            }
            Align::Right => {
                write!(w, "{:pad$}", "", pad = pad)?;
                self.write_styled(w)
            }
        }
    }
}

/// One body entry: a dense row, an atomic card with optional detail, or a
/// section label heading a group of following rows.
enum Body {
    Row(Vec<Cell>),
    Card {
        cells: Vec<Cell>,
        detail: Option<Cell>,
    },
    Section(Vec<Cell>),
    Blank,
}

impl Body {
    fn row_cells(&self) -> Option<&[Cell]> {
        match self {
            Self::Row(cells) | Self::Card { cells, .. } => Some(cells),
            Self::Section(_) | Self::Blank => None,
        }
    }

    fn is_card(&self) -> bool {
        matches!(self, Self::Card { .. })
    }
}

const CARD_DETAIL_MAX_LINES: usize = 3;

/// A borderless table whose columns auto-fit their widest cell. Headers render
/// in the [`palette::header()`] tone; every body cell keeps its own style. Cells
/// are joined with a two-space gap and the trailing column is never padded, so
/// lines carry no trailing whitespace. [`Table::section`] groups rows under a
/// spanning label while every row shares one width computation, so groups stay
/// column-aligned.
pub(crate) struct Table {
    headers: Vec<String>,
    align: Vec<Align>,
    rows: Vec<Body>,
    indent: usize,
    max_width: Option<usize>,
}

impl Table {
    pub(crate) fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let headers: Vec<String> = headers.into_iter().map(Into::into).collect();
        let align = vec![Align::Left; headers.len()];
        Table {
            headers,
            align,
            rows: Vec::new(),
            indent: 0,
            max_width: None,
        }
    }

    /// Mark these column indexes right-aligned, for numeric columns.
    pub(crate) fn right(mut self, cols: &[usize]) -> Self {
        for &col in cols {
            if let Some(slot) = self.align.get_mut(col) {
                *slot = Align::Right;
            }
        }
        self
    }

    /// Indent the header row and body rows by `n` spaces.
    pub(crate) fn indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    pub(crate) fn row<I: IntoIterator<Item = Cell>>(&mut self, cells: I) {
        self.rows.push(Body::Row(cells.into_iter().collect()));
    }

    /// Add one card row and its optional full-width detail. Detail text must be
    /// pre-collapsed to one line; [`Self::max_width`] wraps it when configured.
    pub(crate) fn card<I: IntoIterator<Item = Cell>>(&mut self, cells: I, detail: Option<Cell>) {
        self.rows.push(Body::Card {
            cells: cells.into_iter().collect(),
            detail,
        });
    }

    /// Bound every rendered line to `max_total_width` where the table shape
    /// permits it. Plain rows clip their trailing column; card details wrap.
    pub(crate) fn max_width(mut self, max_total_width: usize) -> Self {
        self.max_width = Some(max_total_width);
        self
    }

    /// Open a group: a blank line then `label` in the shared heading treatment,
    /// row pushed until the next section.
    pub(crate) fn section(&mut self, label: impl Into<String>) {
        self.section_cells(vec![cell(label).fg(palette::header())]);
    }

    /// Separate groups of rows with an empty line; the columns stay shared.
    pub(crate) fn blank(&mut self) {
        self.rows.push(Body::Blank);
    }

    /// Open a group with styled spans joined by one space.
    pub(crate) fn section_cells(&mut self, cells: Vec<Cell>) {
        self.rows.push(Body::Section(cells));
    }

    pub(crate) fn render(&self, w: &mut impl Write) -> std::io::Result<()> {
        let widths = self.column_widths();
        let header_cells: Vec<Cell> = self
            .headers
            .iter()
            .map(|h| cell(h.clone()).fg(palette::header()))
            .collect();
        self.write_row(w, &header_cells, &widths)?;
        let mut previous_was_card = false;
        for body in &self.rows {
            self.write_body(w, body, &widths, previous_was_card)?;
            previous_was_card = body.is_card();
        }
        Ok(())
    }

    fn column_widths(&self) -> Vec<usize> {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.width()).collect();
        for row in self.rows.iter().filter_map(Body::row_cells) {
            for (col, cell) in row.iter().enumerate().take(cols) {
                widths[col] = widths[col].max(cell.width());
            }
        }
        if let Some(max_total_width) = self.max_width
            && cols > 0
        {
            let last = cols - 1;
            let gaps = 2 * last;
            let fixed: usize = widths.iter().take(last).sum::<usize>() + gaps + self.indent;
            let available = max_total_width.saturating_sub(fixed);
            if available < widths[last] {
                widths[last] = available.max(1).min(widths[last]);
            }
        }
        widths
    }

    fn write_body(
        &self,
        w: &mut impl Write,
        body: &Body,
        widths: &[usize],
        previous_was_card: bool,
    ) -> std::io::Result<()> {
        match body {
            Body::Row(row) => self.write_row(w, row, widths),
            Body::Card { cells, detail } => {
                if previous_was_card {
                    writeln!(w)?;
                }
                self.write_row(w, cells, widths)?;
                if let Some(detail) = detail {
                    self.write_card_detail(w, detail)?;
                }
                Ok(())
            }
            Body::Section(cells) => self.write_section(w, cells),
            Body::Blank => writeln!(w),
        }
    }

    fn write_section(&self, w: &mut impl Write, cells: &[Cell]) -> std::io::Result<()> {
        writeln!(w)?;
        for (idx, cell) in cells.iter().enumerate() {
            if idx > 0 {
                write!(w, " ")?;
            }
            cell.write_styled(w)?;
        }
        writeln!(w)
    }

    fn write_card_detail(&self, w: &mut impl Write, cell: &Cell) -> std::io::Result<()> {
        let indent = self.indent + 2;
        let Some(max_width) = self.max_width else {
            write!(w, "{:indent$}", "", indent = indent)?;
            cell.write_styled(w)?;
            writeln!(w)?;
            return Ok(());
        };
        let budget = max_width.saturating_sub(indent);
        let mut lines = wrap_words(&cell.text, budget);
        let truncated = lines.len() > CARD_DETAIL_MAX_LINES;
        lines.truncate(CARD_DETAIL_MAX_LINES);
        if truncated
            && budget > 0
            && let Some(last) = lines.last_mut()
        {
            while last.width() > budget - 1 {
                last.pop();
            }
            last.push('…');
        }
        for line in lines {
            write!(w, "{:indent$}", "", indent = indent)?;
            Cell {
                text: line,
                style: cell.style,
                suffix: None,
            }
            .write_styled(w)?;
            writeln!(w)?;
        }
        Ok(())
    }

    fn write_row(
        &self,
        w: &mut impl Write,
        cells: &[Cell],
        widths: &[usize],
    ) -> std::io::Result<()> {
        let cols = self.headers.len();
        let blank = cell("");
        write!(w, "{:indent$}", "", indent = self.indent)?;
        for (col, (&width, &align)) in widths.iter().zip(&self.align).enumerate() {
            if col > 0 {
                write!(w, "  ")?;
            }
            let c = cells.get(col).unwrap_or(&blank);
            let clipped;
            let c = if self.max_width.is_some() && col + 1 == cols {
                clipped = c.clipped(width);
                &clipped
            } else {
                c
            };
            // The last left-aligned column needs no padding, keeping line ends clean.
            if col + 1 == cols && align == Align::Left {
                c.write_styled(w)?;
            } else {
                c.write_padded(w, width, align)?;
            }
        }
        writeln!(w)
    }
}

/// Greedily wrap pre-collapsed single-line text to `width` display columns.
/// Tokens wider than the budget are hard-split on character boundaries.
pub(crate) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.width() + 1 + word.width() <= width {
            current.push(' ');
            current.push_str(word);
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if word.width() <= width {
            current.push_str(word);
            continue;
        }

        let mut used = 0;
        for ch in word.chars() {
            let char_width = ch.width().unwrap_or(0);
            if char_width > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    used = 0;
                }
                lines.push(clip_to_width(&ch.to_string(), width));
                continue;
            }
            if !current.is_empty() && used + char_width > width {
                lines.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(ch);
            used += char_width;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub(crate) fn clip_to_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    let body_width = max_width - 1;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width > body_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

/// A block of aligned `key: value` lines. Keys render in [`palette::muted()`]; the
/// value column aligns to the widest key, and each value keeps its own style.
/// Reports that nest pairs under a heading set an [`KeyVals::indent`].
pub(crate) struct KeyVals {
    rows: Vec<(String, Vec<Vec<Cell>>)>,
    indent: usize,
}

impl KeyVals {
    pub(crate) fn new() -> Self {
        KeyVals {
            rows: Vec::new(),
            indent: 0,
        }
    }

    /// Indent every line by `n` spaces, nesting the block under a heading.
    pub(crate) fn indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    pub(crate) fn push(&mut self, key: impl Into<String>, value: Cell) {
        self.push_spans(key, [value]);
    }

    /// Add one value line composed of independently styled adjacent spans.
    pub(crate) fn push_spans(
        &mut self,
        key: impl Into<String>,
        spans: impl IntoIterator<Item = Cell>,
    ) {
        self.rows
            .push((key.into(), vec![spans.into_iter().collect()]));
    }

    /// Add a value block whose follow-on lines align to the value column.
    pub(crate) fn push_lines(
        &mut self,
        key: impl Into<String>,
        lines: impl IntoIterator<Item = Vec<Cell>>,
    ) {
        self.rows.push((key.into(), lines.into_iter().collect()));
    }

    pub(crate) fn render(&self, w: &mut impl Write) -> std::io::Result<()> {
        // Align values one column past the widest `key:` label.
        let label_w = self
            .rows
            .iter()
            .map(|(key, _)| key.width() + 1)
            .max()
            .unwrap_or(0);
        for (key, lines) in &self.rows {
            let label = format!("{key}:");
            let pad = label_w.saturating_sub(label.width());
            for (line_index, spans) in lines.iter().enumerate() {
                write!(w, "{:indent$}", "", indent = self.indent)?;
                if line_index == 0 {
                    cell(label.clone()).fg(palette::muted()).write_styled(w)?;
                    write!(w, "{:pad$} ", "", pad = pad)?;
                } else {
                    write!(w, "{:value_indent$}", "", value_indent = label_w + 1)?;
                }
                for span in spans {
                    span.write_styled(w)?;
                }
                writeln!(w)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
