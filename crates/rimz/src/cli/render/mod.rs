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
pub(crate) mod status;

use std::io::Write;

use jiff::Timestamp;
use unicode_width::UnicodeWidthStr;

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

/// Finish a stdout emission, treating a consumer that stopped reading as a
/// clean end rather than a fault. Any other write error propagates.
pub(crate) fn finish(write: std::io::Result<()>) -> anyhow::Result<()> {
    match write {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(err) => Err(err.into()),
    }
}

/// Wrap `text` in `style`'s ANSI for inline use inside a larger line — the
/// `anstream` stream strips it when color is off. Cells in [`Table`]/[`KeyVals`]
/// carry their own style; reach for this only when one styled span sits within
/// an otherwise plain `writeln!`.
pub(crate) fn paint(style: anstyle::Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

/// Render an absolute path relative to `$HOME` as `~`/`~/rest`, so cwd columns
/// read at a glance. Leaves any path outside `$HOME` (or when `$HOME` is unset)
/// untouched.
pub(crate) fn home_relative(path: &str) -> String {
    let home = std::env::var_os("HOME");
    home_relative_to(home.as_ref().and_then(|home| home.to_str()), path)
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

pub(crate) fn rel_age(ts: Timestamp, now: Timestamp) -> String {
    let age = now.duration_since(ts);
    if age.is_negative() {
        return "now".to_owned();
    }
    let secs = age.as_secs().max(0) as u64;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
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
            self.fg(palette::FAINT)
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

/// One body entry: a row of cells, or a section label spanning the table to
/// head a group of following rows.
enum Body {
    Row(Vec<Cell>),
    Section(String),
}

/// A borderless table whose columns auto-fit their widest cell. Headers render
/// in the [`palette::HEADER`] tone; every body cell keeps its own style. Cells
/// are joined with a two-space gap and the trailing column is never padded, so
/// lines carry no trailing whitespace. [`Table::section`] groups rows under a
/// spanning label while every row shares one width computation, so groups stay
/// column-aligned.
pub(crate) struct Table {
    headers: Vec<String>,
    align: Vec<Align>,
    rows: Vec<Body>,
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

    pub(crate) fn row<I: IntoIterator<Item = Cell>>(&mut self, cells: I) {
        self.rows.push(Body::Row(cells.into_iter().collect()));
    }

    /// Open a group: a blank line then `label` in the accent tone, heading every
    /// row pushed until the next section.
    pub(crate) fn section(&mut self, label: impl Into<String>) {
        self.rows.push(Body::Section(label.into()));
    }

    pub(crate) fn render(&self, w: &mut impl Write) -> std::io::Result<()> {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.width()).collect();
        for body in &self.rows {
            if let Body::Row(row) = body {
                for (col, cell) in row.iter().enumerate().take(cols) {
                    widths[col] = widths[col].max(cell.width());
                }
            }
        }
        let header_cells: Vec<Cell> = self
            .headers
            .iter()
            .map(|h| cell(h.clone()).fg(palette::HEADER))
            .collect();
        self.write_row(w, &header_cells, &widths)?;
        for body in &self.rows {
            match body {
                Body::Row(row) => self.write_row(w, row, &widths)?,
                Body::Section(label) => {
                    writeln!(w)?;
                    cell(label.clone())
                        .fg(palette::ACCENT.bold())
                        .write_styled(w)?;
                    writeln!(w)?;
                }
            }
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
        for (col, (&width, &align)) in widths.iter().zip(&self.align).enumerate() {
            if col > 0 {
                write!(w, "  ")?;
            }
            let c = cells.get(col).unwrap_or(&blank);
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

/// A block of aligned `key: value` lines. Keys render in [`palette::MUTED`]; the
/// value column aligns to the widest key, and each value keeps its own style.
/// Reports that nest pairs under a heading set an [`KeyVals::indent`].
pub(crate) struct KeyVals {
    rows: Vec<(String, Cell)>,
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
        self.rows.push((key.into(), value));
    }

    pub(crate) fn render(&self, w: &mut impl Write) -> std::io::Result<()> {
        // Align values one column past the widest `key:` label.
        let label_w = self
            .rows
            .iter()
            .map(|(key, _)| key.width() + 1)
            .max()
            .unwrap_or(0);
        for (key, value) in &self.rows {
            let label = format!("{key}:");
            let pad = label_w.saturating_sub(label.width());
            write!(w, "{:indent$}", "", indent = self.indent)?;
            cell(label).fg(palette::MUTED).write_styled(w)?;
            write!(w, "{:pad$} ", "", pad = pad)?;
            value.write_styled(w)?;
            writeln!(w)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    #[test]
    fn finish_passes_success_through() {
        assert!(finish(Ok(())).is_ok());
    }

    #[test]
    fn finish_propagates_a_non_broken_pipe_error() {
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(finish(Err(err)).is_err());
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
    fn keyvals_aligns_values_and_indents() {
        let mut kv = KeyVals::new().indent(2);
        kv.push("name", cell("right-yard"));
        kv.push("session", cell("0d52"));
        assert_eq!(
            strip(|w| kv.render(w)),
            "  name:    right-yard\n  session: 0d52\n"
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
    fn styled_cells_emit_ansi_and_strip_cleanly() {
        let mut table = Table::new(["S"]);
        table.row([cell("running").fg(palette::GOOD)]);
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
