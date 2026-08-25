//! Shared terminal-mode lifecycle for inline and pane-resident TUI surfaces.

use std::io;
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::crossterm::execute;
use ratatui::crossterm::queue;
use ratatui::crossterm::style::Print;
use ratatui::crossterm::{cursor, terminal};
use tracing_subscriber::fmt::MakeWriter;

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;
type SharedPanicHook = Arc<Mutex<Option<PanicHook>>>;
const LOG_CAPTURE_CAP: usize = 64 * 1024;

#[derive(Default)]
struct LogCapture {
    depth: usize,
    bytes: Vec<u8>,
}

fn log_capture() -> &'static Mutex<LogCapture> {
    static CAPTURE: OnceLock<Mutex<LogCapture>> = OnceLock::new();
    CAPTURE.get_or_init(|| Mutex::new(LogCapture::default()))
}

fn begin_log_capture() {
    let mut capture = log_capture()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if capture.depth == 0 {
        capture.bytes.clear();
    }
    capture.depth += 1;
}

fn end_log_capture() -> Option<Vec<u8>> {
    let mut capture = log_capture()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if capture.depth == 0 {
        return None;
    }
    capture.depth -= 1;
    (capture.depth == 0).then(|| std::mem::take(&mut capture.bytes))
}

fn end_log_capture_flush() {
    let Some(bytes) = end_log_capture() else {
        return;
    };
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(&bytes);
    let _ = stderr.flush();
}

/// Captured tracing lines for a full-screen surface to render without writing
/// over its active terminal frame.
pub fn captured_log_lines() -> Vec<String> {
    let bytes = log_capture()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .bytes
        .clone();
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(strip_ansi)
        .collect()
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if let Some('[') = chars.next() {
                for escaped in chars.by_ref() {
                    if matches!(escaped, '\x40'..='\x7e') {
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Tracing writer that diverts stderr into the active terminal surface.
pub struct TuiLogWriter;

struct CapturedLogOutput;

impl Write for CapturedLogOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut capture = log_capture()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if buf.len() >= LOG_CAPTURE_CAP {
            capture.bytes.clear();
            capture
                .bytes
                .extend_from_slice(&buf[buf.len() - LOG_CAPTURE_CAP..]);
        } else {
            let overflow = capture
                .bytes
                .len()
                .saturating_add(buf.len())
                .saturating_sub(LOG_CAPTURE_CAP);
            if overflow > 0 {
                capture.bytes.drain(..overflow);
            }
            capture.bytes.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for TuiLogWriter {
    type Writer = Box<dyn Write + 'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let captured = log_capture()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .depth
            > 0;
        if captured {
            Box::new(CapturedLogOutput)
        } else {
            Box::new(io::stderr())
        }
    }
}

/// Click + wheel tracking (?1000) with SGR encoding (?1006) — the only modes
/// the sidebar consumes (`sidebar_pane::app::input::encode_mouse`). Crossterm's
/// `EnableMouseCapture` also requests ?1002/?1003/?1015; ?1003 (all-motion)
/// forces tmux to upgrade the outer terminal to MOUSE_ALL and drop it back
/// whenever the window mode union changes, and Ghostty reacts to that churn
/// by converting wheel ticks into arrow keys aimed at the active pane
/// (ghostty-org/ghostty discussions 4617 and 7630).
const ENABLE_CLICK_WHEEL_CAPTURE: &str = "\x1b[?1000h\x1b[?1006h";
const DISABLE_CLICK_WHEEL_CAPTURE: &str = "\x1b[?1006l\x1b[?1000l";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseCapture {
    Off,
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Main,
    Alternate,
}

pub struct TerminalModeGuard {
    mouse: MouseCapture,
    screen: Screen,
    saved_hook: Option<SharedPanicHook>,
}

/// `NO_COLOR` opt-out shared by every terminal surface. The value is process
/// static by convention, so cache it once rather than probing the env per frame.
pub fn no_color() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()))
}

/// Terminal capability signals behind the truecolor decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TruecolorSignals {
    /// Raw `COLORTERM`, when set.
    pub colorterm: Option<String>,
    /// Raw `TERM`, when set.
    pub term: Option<String>,
    /// The terminfo entry for `$TERM` declares 24-bit color.
    pub terminfo: bool,
}

impl TruecolorSignals {
    /// Read the live signals fresh from the environment and terminfo database.
    pub fn detect() -> Self {
        Self {
            colorterm: non_empty_env("COLORTERM"),
            term: non_empty_env("TERM"),
            terminfo: terminfo_truecolor(),
        }
    }

    /// Whether 24-bit color is advertised by `COLORTERM` or terminfo.
    pub fn truecolor(&self) -> bool {
        matches!(self.colorterm.as_deref(), Some("truecolor" | "24bit")) || self.terminfo
    }
}

/// 24-bit color advertised by the active terminal. The value is process static
/// by convention, so cache it once rather than probing terminfo per frame.
pub fn truecolor() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| TruecolorSignals::detect().truecolor())
}

/// Write a buffered terminal frame with raw-mode-safe, self-clearing lines.
fn write_crlf(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line_end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        w.write_all(&bytes[start..line_end])?;
        w.write_all(b"\x1b[K\r\n")?;
        start = index + 1;
    }
    if start < bytes.len() {
        w.write_all(&bytes[start..])?;
        w.write_all(b"\x1b[K")?;
    }
    Ok(())
}

/// Replace the visible terminal frame with one buffered write and flush.
pub fn replace_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    queue!(w, cursor::MoveTo(0, 0))?;
    write_crlf(w, bytes)?;
    queue!(w, terminal::Clear(terminal::ClearType::FromCursorDown))?;
    w.flush()
}

fn terminfo_truecolor() -> bool {
    let Some(term) = std::env::var_os("TERM").filter(|term| !term.is_empty()) else {
        return false;
    };
    // `tput -x` exposes ncurses extended capabilities such as `Tc`; older
    // ncurses builds, including macOS 5.7, may miss them, so `mode =
    // "truecolor"` remains the override when COLORTERM is also absent.
    terminfo_capability(&term, "Tc", &[])
        || terminfo_capability(&term, "RGB", &[])
        || (terminfo_capability(&term, "setrgbf", &["1", "2", "3"])
            && terminfo_capability(&term, "setrgbb", &["1", "2", "3"]))
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn terminfo_capability(term: &std::ffi::OsStr, capability: &str, params: &[&str]) -> bool {
    tput_capability(term, capability, params, true)
        || tput_capability(term, capability, params, false)
}

fn tput_capability(
    term: &std::ffi::OsStr,
    capability: &str,
    params: &[&str],
    extended: bool,
) -> bool {
    let mut command = Command::new("tput");
    if extended {
        command.arg("-x");
    }
    command
        .arg("-T")
        .arg(term)
        .arg(capability)
        .args(params)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

impl TerminalModeGuard {
    pub fn enable(mouse: MouseCapture, screen: Screen) -> io::Result<Self> {
        begin_log_capture();
        if let Err(err) = terminal::enable_raw_mode() {
            end_log_capture_flush();
            return Err(err);
        }
        if screen == Screen::Alternate
            && let Err(err) = execute!(
                io::stdout(),
                terminal::EnterAlternateScreen,
                cursor::Hide,
                terminal::DisableLineWrap
            )
        {
            restore_terminal(MouseCapture::Off, screen);
            return Err(err);
        }
        if let Err(err) = enable_mouse(mouse) {
            restore_terminal(mouse, screen);
            return Err(err);
        }
        let saved_hook = Arc::new(Mutex::new(Some(panic::take_hook())));
        let hook_for_panic = Arc::clone(&saved_hook);
        panic::set_hook(Box::new(move |info| {
            restore_terminal(mouse, screen);
            if let Some(previous) = hook_for_panic
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
            {
                previous(info);
            }
        }));
        Ok(Self {
            mouse,
            screen,
            saved_hook: Some(saved_hook),
        })
    }

    /// Consume the guard leaving every terminal mode in place for a reload
    /// handoff. The replacement process re-enables the same modes, while
    /// restoring here opens a mouse-reporting gap that outer terminals can
    /// observe and turn wheel input into arrow keys.
    pub fn preserve_for_reexec(self) {
        // The process exits immediately after this handoff, so keeping the
        // panic hook installed and skipping the terminal restore are both
        // intentional.
        // ponytail: the capture buffer dies with this process; transfer it if
        // reexec handoffs ever need to preserve diagnostics.
        std::mem::forget(self);
    }

    /// Hand the terminal to another full-screen application without clearing
    /// the current screen or dropping mouse reporting. The successor paints
    /// over the held frame and takes ownership of terminal modes; keeping mouse
    /// capture armed closes the alternate-screen gap in which terminals turn a
    /// residual wheel tick into an arrow key for the successor.
    pub fn handoff_keep_screen(mut self) -> io::Result<()> {
        if self.screen == Screen::Alternate {
            execute!(io::stdout(), terminal::EnableLineWrap, cursor::Show)?;
        }
        terminal::disable_raw_mode()?;
        let hook = self.saved_hook.take().and_then(|saved_hook| {
            saved_hook
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        });
        if let Some(hook) = hook {
            panic::set_hook(hook);
        }
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        restore_terminal(self.mouse, self.screen);
        let hook = self.saved_hook.take().and_then(|saved_hook| {
            saved_hook
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        });
        if let Some(hook) = hook {
            panic::set_hook(hook);
        }
    }
}

fn enable_mouse(mouse: MouseCapture) -> io::Result<()> {
    match mouse {
        MouseCapture::Off => Ok(()),
        MouseCapture::Stdout => execute!(io::stdout(), Print(ENABLE_CLICK_WHEEL_CAPTURE)),
        MouseCapture::Stderr => execute!(io::stderr(), Print(ENABLE_CLICK_WHEEL_CAPTURE)),
    }
}

fn disable_mouse(mouse: MouseCapture) -> io::Result<()> {
    match mouse {
        MouseCapture::Off => Ok(()),
        MouseCapture::Stdout => execute!(io::stdout(), Print(DISABLE_CLICK_WHEEL_CAPTURE)),
        MouseCapture::Stderr => execute!(io::stderr(), Print(DISABLE_CLICK_WHEEL_CAPTURE)),
    }
}

pub(crate) fn restore_terminal(mouse: MouseCapture, screen: Screen) {
    let _ = disable_mouse(mouse);
    if screen == Screen::Alternate {
        let _ = execute!(
            io::stdout(),
            terminal::EnableLineWrap,
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
    }
    let _ = terminal::disable_raw_mode();
    end_log_capture_flush();
}

/// Complete a terminal handoff whose guard preserved the active screen and
/// mouse mode. Cleanup remains best-effort because the successor already ran.
pub fn finish_handoff(mouse: MouseCapture, screen: Screen) {
    restore_terminal(mouse, screen);
}

#[cfg(test)]
mod tests {
    use super::{
        DISABLE_CLICK_WHEEL_CAPTURE, ENABLE_CLICK_WHEEL_CAPTURE, LOG_CAPTURE_CAP, TruecolorSignals,
        TuiLogWriter, begin_log_capture, captured_log_lines, end_log_capture, replace_frame,
        strip_ansi, write_crlf,
    };
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn signals(colorterm: Option<&str>, terminfo: bool) -> TruecolorSignals {
        TruecolorSignals {
            colorterm: colorterm.map(str::to_owned),
            term: None,
            terminfo,
        }
    }

    #[test]
    fn truecolor_signals_accept_colorterm_or_terminfo() {
        assert!(signals(Some("truecolor"), false).truecolor());
        assert!(signals(Some("24bit"), false).truecolor());
        assert!(signals(None, true).truecolor());
        assert!(signals(Some("8bit"), true).truecolor());
        assert!(!signals(Some("8bit"), false).truecolor());
        assert!(!signals(Some(""), false).truecolor());
        assert!(!signals(None, false).truecolor());
    }

    #[test]
    fn raw_mode_writer_uses_crlf_line_endings_and_clears_each_line() {
        let mut out = Vec::new();

        write_crlf(&mut out, b"head\nrow\r\ntail").expect("write frame");

        assert_eq!(out, b"head\x1b[K\r\nrow\x1b[K\r\ntail\x1b[K");
    }

    #[test]
    fn frame_replacement_orders_multiline_output_and_flushes_once() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head\nrow").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1Hhead\x1b[K\r\nrow\x1b[K\x1b[J");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn frame_replacement_preserves_trailing_newline() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head\n").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1Hhead\x1b[K\r\n\x1b[J");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn frame_replacement_clears_unterminated_final_line() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1Hhead\x1b[K\x1b[J");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn mouse_capture_requests_only_click_wheel_and_sgr_modes() {
        assert_eq!(ENABLE_CLICK_WHEEL_CAPTURE, "\x1b[?1000h\x1b[?1006h");
        assert_eq!(DISABLE_CLICK_WHEEL_CAPTURE, "\x1b[?1006l\x1b[?1000l");
    }

    #[test]
    fn tracing_writer_captures_and_cleans_full_screen_logs() {
        let writer = TuiLogWriter;
        begin_log_capture();
        let mut output = writer.make_writer();
        output
            .write_all(b"\x1b[31mwarning\x1b[0m\nnext\n")
            .expect("capture log");
        assert_eq!(captured_log_lines(), ["warning", "next"]);
        assert_eq!(
            end_log_capture().expect("drain capture"),
            b"\x1b[31mwarning\x1b[0m\nnext\n"
        );
        assert!(captured_log_lines().is_empty());
        assert_eq!(strip_ansi("a\x1b[1;31mb\x1b[0mc"), "abc");

        begin_log_capture();
        let oversized = vec![b'x'; LOG_CAPTURE_CAP + 1];
        writer
            .make_writer()
            .write_all(&oversized)
            .expect("capture oversized log");
        assert_eq!(
            end_log_capture().expect("drain bounded capture").len(),
            LOG_CAPTURE_CAP
        );
    }
}
