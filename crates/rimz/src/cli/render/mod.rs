//! Human-facing CLI presentation: one styled stdout path plus borderless,
//! auto-fit tables and aligned key/value blocks, so every `rimz` command reads
//! consistently and in the room's palette.
//!
//! `--json` output and snapshot tests stay byte-clean: [`out`] writes through
//! `anstream`, which strips ANSI when stdout is not a terminal or color is
//! disabled (`NO_COLOR`/`CLICOLOR`, or `--color never`). Writes go through
//! `writeln!`, not the `print!` macros, matching the `print_json` stdout path —
//! the `print_stdout` lint still guards the protocol surface.

pub(crate) mod palette;
pub(crate) mod room;
pub(crate) mod status;

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
    let mut chain = error.chain();
    let Some(error) = chain.next() else {
        return Ok(());
    };
    let message = error.to_string();
    let message = message.trim();
    let mut lines = message.lines();
    match lines.next() {
        Some(line) => writeln!(w, "{} {line}", paint(palette::alarm().bold(), "error:"))?,
        None => writeln!(w, "{}", paint(palette::alarm().bold(), "error:"))?,
    }
    for line in lines {
        writeln!(w, "  {line}")?;
    }

    let mut last_printed = message.to_owned();
    for source in chain {
        let message = source.to_string();
        let message = message.trim();
        if last_printed.contains(message) {
            continue;
        }
        for line in message.lines() {
            writeln!(w, "  {line}")?;
        }
        last_printed = message.to_owned();
    }
    Ok(())
}

/// Finish a stdout emission, treating a consumer that stopped reading as a
/// clean end rather than a fault. Any other write error propagates.
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

/// Format bytes for human CLI reports with 1024-based units.
pub(crate) fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value.fract() == 0.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

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

pub(crate) fn age_short(ts: Timestamp, now: Timestamp) -> String {
    let age = now.duration_since(ts);
    age_label(age.as_secs().max(0) as u64)
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
        format!("in {}h", secs / 3_600)
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
}

/// Start a plain (unstyled) cell from any text.
pub(crate) fn cell(text: impl Into<String>) -> Cell {
    Cell {
        text: text.into(),
        style: None,
    }
}

impl Cell {
    /// Paint this cell with a palette style.
    pub(crate) fn fg(mut self, style: anstyle::Style) -> Self {
        self.style = Some(style);
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
        self.text.width()
    }

    fn write_styled(&self, w: &mut impl Write) -> std::io::Result<()> {
        match self.style {
            Some(style) => write!(w, "{}{}{}", style.render(), self.text, style.render_reset()),
            None => write!(w, "{}", self.text),
        }
    }

    fn clipped(&self, width: usize) -> Self {
        Cell {
            text: clip_to_width(&self.text, width),
            style: self.style,
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
}

impl Body {
    fn row_cells(&self) -> Option<&[Cell]> {
        match self {
            Self::Row(cells) | Self::Card { cells, .. } => Some(cells),
            Self::Section(_) => None,
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
    if max_width == 1 {
        return "…".to_owned();
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
    if out.is_empty() {
        "…".to_owned()
    } else {
        out.push('…');
        out
    }
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
mod tests {
    use super::*;
    use serde::ser::Error as _;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    #[test]
    fn finish_propagates_a_non_broken_pipe_error() {
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(finish(Err(err)).is_err());
    }

    #[test]
    fn compact_json_has_one_trailing_newline() {
        let mut out = Vec::new();

        write_json(&mut out, &serde_json::json!({ "answer": 42 }), false).unwrap();

        assert_eq!(out, b"{\"answer\":42}\n");
    }

    #[test]
    fn pretty_json_has_one_trailing_newline() {
        let mut out = Vec::new();

        write_json(&mut out, &serde_json::json!({ "answer": 42 }), true).unwrap();

        assert_eq!(out, b"{\n  \"answer\": 42\n}\n");
    }

    #[test]
    fn json_propagates_serialization_failure() {
        struct Fails;

        impl serde::Serialize for Fails {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(S::Error::custom("serialization failed"))
            }
        }

        let error = write_json(&mut Vec::new(), &Fails, false).unwrap_err();

        assert!(error.to_string().contains("serialization failed"));
    }

    #[test]
    fn json_propagates_ordinary_writer_failure() {
        let mut writer = FailingWriter(std::io::ErrorKind::PermissionDenied);

        let error = write_json(&mut writer, &true, false).unwrap_err();

        assert!(error.to_string().contains("permission denied"));
    }

    #[test]
    fn json_treats_broken_pipe_as_clean() {
        let mut writer = FailingWriter(std::io::ErrorKind::BrokenPipe);

        assert!(write_json(&mut writer, &true, false).is_ok());
    }

    struct FailingWriter(std::io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn gutter_writer_prefixes_lines_across_partial_writes() {
        let mut out = Vec::new();
        {
            let mut gutter = GutterWriter::new(&mut out);
            gutter.write_all(b"first\nsec").unwrap();
            gutter.write_all(b"ond\nthird").unwrap();
        }

        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains(&paint(palette::faint(), "│ ")));
        assert_eq!(
            anstream::adapter::strip_str(&raw).to_string(),
            "  │ first\n  │ second\n  │ third"
        );
    }

    #[test]
    fn report_prints_an_embedded_source_once() {
        #[derive(Debug, thiserror::Error)]
        #[error("opening config: {source}")]
        struct EmbeddedSource {
            #[source]
            source: std::io::Error,
        }

        let error = anyhow::Error::new(EmbeddedSource {
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
        });

        assert_eq!(
            strip(|w| write_report(w, &error)),
            "error: opening config: permission denied\n"
        );
    }

    #[test]
    fn report_indents_distinct_causes() {
        let error = anyhow::anyhow!("leaf").context("middle").context("top");

        assert_eq!(
            strip(|w| write_report(w, &error)),
            "error: top\n  middle\n  leaf\n"
        );
    }

    #[test]
    fn report_indents_every_multiline_cause_line() {
        let error = anyhow::anyhow!("first detail\nsecond detail").context("top");

        assert_eq!(
            strip(|w| write_report(w, &error)),
            "error: top\n  first detail\n  second detail\n"
        );
    }

    #[test]
    fn report_prints_a_bare_error_on_one_line() {
        let error = anyhow::anyhow!("plain failure");

        assert_eq!(strip(|w| write_report(w, &error)), "error: plain failure\n");
    }

    #[test]
    fn pane_frame_aligns_plain_text() {
        let rendered = strip(|w| pane_frame(w, "tmux:%3", "short\na longer line"));
        let widths: Vec<usize> = rendered.lines().map(UnicodeWidthStr::width).collect();

        assert_eq!(widths, vec![17, 17, 17, 17], "{rendered}");
        assert_eq!(
            rendered,
            "╭─ tmux:%3 ─────╮\n│ short         │\n│ a longer line │\n╰───────────────╯\n"
        );
    }

    #[test]
    fn pane_frame_measures_ansi_styled_content_without_sgr_bytes() {
        let styled = format!(
            "{}ready{}\nlonger",
            palette::good().render(),
            palette::good().render_reset()
        );

        assert_eq!(
            strip(|w| pane_frame(w, "pane", &styled)),
            "╭─ pane ─╮\n│ ready  │\n│ longer │\n╰────────╯\n"
        );
    }

    #[test]
    fn pane_frame_aligns_wide_unicode_content() {
        let rendered = strip(|w| pane_frame(w, "p", "文字\nx"));
        let widths: Vec<usize> = rendered.lines().map(UnicodeWidthStr::width).collect();

        assert_eq!(widths, vec![8, 8, 8, 8], "{rendered}");
        assert!(rendered.contains("│ 文字 │"), "{rendered}");
    }

    #[test]
    fn pane_frame_fits_a_title_wider_than_its_content() {
        assert_eq!(
            strip(|w| pane_frame(w, "zellij:terminal_3", "ok")),
            "╭─ zellij:terminal_3 ╮\n│ ok                 │\n╰────────────────────╯\n"
        );
    }

    #[test]
    fn pane_frame_ignores_a_trailing_content_newline() {
        assert_eq!(
            strip(|w| pane_frame(w, "p", "one\ntwo\n")),
            strip(|w| pane_frame(w, "p", "one\ntwo"))
        );
    }

    #[test]
    fn pane_frame_renders_an_empty_capture_without_a_content_row() {
        assert_eq!(strip(|w| pane_frame(w, "p", "")), "╭─ p ╮\n╰────╯\n");
    }

    #[test]
    fn pane_frame_resets_capture_style_before_padding() {
        let mut raw = Vec::new();

        pane_frame(&mut raw, "p", "\u{1b}[31mred\nlonger").expect("render pane frame");
        let raw = String::from_utf8(raw).expect("utf-8");

        assert!(raw.contains("\u{1b}[31mred\u{1b}[0m   "), "{raw:?}");
    }

    #[test]
    fn table_auto_fits_columns_and_right_aligns() {
        let mut table = Table::new(["NAME", "CTX"]).right(&[1]);
        table.row([cell("right-yard"), cell("100%")]);
        table.row([cell("a"), cell("5%")]);
        // Columns fit the widest cell; CTX is right-aligned; the last column is
        // padded only because it is right-aligned, never trailing whitespace.
        assert_eq!(
            strip(|w| table.render(w)),
            "NAME         CTX\nright-yard  100%\na             5%\n"
        );
    }

    #[test]
    fn table_max_width_limits_trailing_column() {
        let mut table = Table::new(["NAME", "DESC"]).max_width(20);
        table.row([cell("agent"), cell("unicode wide 文字 tail")]);

        let rendered = strip(|w| table.render(w));

        assert_eq!(rendered, "NAME   DESC\nagent  unicode wide…\n");
        assert!(rendered.lines().all(|line| line.width() <= 20));
    }

    #[test]
    fn table_card_detail_wraps_with_an_aligned_indent() {
        let mut table = Table::new(["NAME"]).indent(2).max_width(20);
        table.card([cell("agent")], Some(cell("one two three four five")));

        assert_eq!(
            strip(|w| table.render(w)),
            "  NAME\n  agent\n    one two three\n    four five\n"
        );
    }

    #[test]
    fn table_card_detail_caps_lines_and_marks_truncation() {
        let mut table = Table::new(["NAME"]).max_width(10);
        table.card(
            [cell("agent")],
            Some(cell("one two three four five six seven eight")),
        );

        let rendered = strip(|w| table.render(w));

        assert_eq!(rendered, "NAME\nagent\n  one two\n  three\n  four…\n");
        assert!(rendered.lines().all(|line| line.width() <= 10));
    }

    #[test]
    fn table_card_detail_without_max_width_stays_on_one_line() {
        let mut table = Table::new(["NAME"]);
        table.card([cell("agent")], Some(cell("one two three")));

        assert_eq!(strip(|w| table.render(w)), "NAME\nagent\n  one two three\n");
    }

    #[test]
    fn table_cards_separate_without_section_gaps() {
        let mut table = Table::new(["NAME"]).max_width(20);
        table.section("first");
        table.card([cell("one")], Some(cell("detail")));
        table.card([cell("two")], None);
        table.section("second");
        table.card([cell("three")], None);

        assert_eq!(
            strip(|w| table.render(w)),
            "NAME\n\nfirst\none\n  detail\n\ntwo\n\nsecond\nthree\n"
        );
    }

    #[test]
    fn table_cards_without_detail_still_separate() {
        let mut table = Table::new(["NAME"]);
        table.card([cell("one")], None);
        table.card([cell("two")], None);

        assert_eq!(strip(|w| table.render(w)), "NAME\none\n\ntwo\n");
    }

    #[test]
    fn table_rows_remain_dense_by_default() {
        let mut table = Table::new(["NAME"]);
        table.row([cell("one")]);
        table.row([cell("two")]);

        assert_eq!(strip(|w| table.render(w)), "NAME\none\ntwo\n");
    }

    #[test]
    fn wrap_words_respects_wide_unicode_and_splits_long_tokens() {
        let wrapped = wrap_words("ab 文字列 abcdefgh", 4);

        assert_eq!(wrapped, ["ab", "文字", "列", "abcd", "efgh"]);
        assert!(wrapped.iter().all(|line| line.width() <= 4));
        assert_eq!(wrap_words("文", 1), ["…"]);
    }

    #[test]
    fn table_indent_applies_to_header_and_rows() {
        let mut table = Table::new(["NAME", "CTX"]).indent(2);
        table.row([cell("right-yard"), cell("ok")]);

        assert_eq!(
            strip(|w| table.render(w)),
            "  NAME        CTX\n  right-yard  ok\n"
        );
    }

    #[test]
    fn table_section_cells_join_styled_spans() {
        let mut table = Table::new(["NAME"]);
        table.section_cells(vec![
            cell("⑂ auth-refresh").fg(palette::accent().bold()),
            cell("· forge team").fg(palette::meta()),
        ]);
        table.row([cell("@coder")]);

        assert_eq!(
            strip(|w| table.render(w)),
            "NAME\n\n⑂ auth-refresh · forge team\n@coder\n"
        );
    }

    #[test]
    fn clip_to_width_respects_unicode_width() {
        assert_eq!(clip_to_width("abcd", 4), "abcd");
        assert_eq!(clip_to_width("abcdef", 4), "abc…");
        assert_eq!(clip_to_width("文字abc", 5), "文字…");
    }

    #[test]
    fn keyvals_scopes_span_styles_and_aligns_continuations() {
        let mut kv = KeyVals::new().indent(2);
        kv.push_spans(
            "m",
            [
                cell("used "),
                cell("$12.00").fg(palette::money()),
                cell(" today"),
            ],
        );
        kv.push_lines(
            "reset",
            [
                vec![cell("2 credits")],
                vec![cell("- first")],
                vec![cell("- second")],
            ],
        );
        assert_eq!(
            strip(|w| kv.render(w)),
            "  m:     used $12.00 today\n  reset: 2 credits\n         - first\n         - second\n"
        );
        let mut raw = Vec::new();
        kv.render(&mut raw).unwrap();
        let raw = String::from_utf8(raw).unwrap();
        assert!(
            raw.contains(&format!(
                "used {}$12.00{} today",
                palette::money().render(),
                palette::money().render_reset()
            )),
            "{raw:?}"
        );
    }

    #[test]
    fn home_relative_collapses_only_the_home_prefix() {
        let home = Some("/home/dev");
        assert_eq!(home_relative_to(home, "/home/dev"), "~");
        assert_eq!(
            home_relative_to(home, "/home/dev/code/query-engine"),
            "~/code/query-engine"
        );
        // A sibling that merely shares the prefix string is left untouched.
        assert_eq!(
            home_relative_to(home, "/home/developer/x"),
            "/home/developer/x"
        );
        assert_eq!(home_relative_to(home, "/srv/work"), "/srv/work");
        // No home → identity.
        assert_eq!(home_relative_to(None, "/home/dev/x"), "/home/dev/x");
    }

    #[test]
    fn fmt_bytes_uses_binary_units_and_short_decimals() {
        assert_eq!(fmt_bytes(1023), "1023 B");
        assert_eq!(fmt_bytes(1024), "1 KB");
        assert_eq!(fmt_bytes(13_018), "13 KB");
        assert_eq!(fmt_bytes(1_503_238_553), "1.4 GB");
        assert_eq!(fmt_bytes(18 * 1024 * 1024), "18 MB");
    }

    #[test]
    fn rel_age_uses_seconds_minutes_hours_days_and_clamps_future() {
        let now = Timestamp::from_second(200_000).expect("timestamp");
        assert_eq!(
            rel_age(Timestamp::from_second(199_959).expect("timestamp"), now),
            "41s ago"
        );
        assert_eq!(
            rel_age(Timestamp::from_second(199_880).expect("timestamp"), now),
            "2m ago"
        );
        assert_eq!(
            rel_age(Timestamp::from_second(192_800).expect("timestamp"), now),
            "2h ago"
        );
        assert_eq!(
            rel_age(Timestamp::from_second(27_200).expect("timestamp"), now),
            "2d ago"
        );
        assert_eq!(
            rel_age(Timestamp::from_second(200_001).expect("timestamp"), now),
            "now"
        );
    }

    #[test]
    fn rel_until_uses_seconds_minutes_hours_days_and_marks_past_due() {
        let now = Timestamp::from_second(200_000).expect("timestamp");
        assert_eq!(
            rel_until(Timestamp::from_second(200_041).expect("timestamp"), now),
            "in 41s"
        );
        assert_eq!(
            rel_until(Timestamp::from_second(200_120).expect("timestamp"), now),
            "in 2m"
        );
        assert_eq!(
            rel_until(Timestamp::from_second(207_200).expect("timestamp"), now),
            "in 2h"
        );
        assert_eq!(
            rel_until(Timestamp::from_second(372_800).expect("timestamp"), now),
            "in 2d"
        );
        assert_eq!(
            rel_until(Timestamp::from_second(199_999).expect("timestamp"), now),
            "due"
        );
    }

    #[test]
    fn until_label_uses_bare_durations_and_marks_past_due() {
        let now = Timestamp::from_second(200_000).expect("timestamp");
        assert_eq!(
            until_label(Timestamp::from_second(200_041).expect("timestamp"), now),
            "41s"
        );
        assert_eq!(
            until_label(Timestamp::from_second(200_120).expect("timestamp"), now),
            "2m"
        );
        assert_eq!(
            until_label(Timestamp::from_second(207_200).expect("timestamp"), now),
            "2h"
        );
        assert_eq!(
            until_label(Timestamp::from_second(372_800).expect("timestamp"), now),
            "2d"
        );
        assert_eq!(
            until_label(Timestamp::from_second(199_999).expect("timestamp"), now),
            "due"
        );
        assert_eq!(until_label(now, now), "due");
    }

    #[test]
    fn styled_cells_emit_ansi_and_strip_cleanly() {
        let mut table = Table::new(["S"]);
        table.row([cell("running").fg(palette::good())]);
        let mut raw: Vec<u8> = Vec::new();
        table.render(&mut raw).expect("render to buffer");
        let raw = String::from_utf8(raw).expect("utf-8");
        assert!(
            raw.contains('\u{1b}'),
            "styled output carries ANSI: {raw:?}"
        );
        assert!(
            !strip(|w| table.render(w)).contains('\u{1b}'),
            "an ANSI-stripping stream yields plain text"
        );
    }
}
