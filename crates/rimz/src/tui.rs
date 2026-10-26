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

#[cfg(test)]
fn captured_log_lines() -> Vec<String> {
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

#[cfg(test)]
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
            Box::new(anstream::AutoStream::auto(io::stderr()))
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
    modes: Option<(MouseCapture, Screen)>,
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
        colorterm_truecolor(self.colorterm.as_deref()) || self.terminfo
    }
}

fn colorterm_truecolor(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor" | "24bit"))
}

/// 24-bit color advertised by the active terminal. The value is process static
/// by convention, so cache it once rather than probing terminfo per frame.
/// A truecolor `COLORTERM` settles the answer before terminfo is probed, which
/// spawns `tput` up to eight times on the start-up path.
pub fn truecolor() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        colorterm_truecolor(non_empty_env("COLORTERM").as_deref()) || terminfo_truecolor()
    })
}

/// Write a buffered terminal frame with raw-mode-safe, self-clearing lines.
///
/// Each erase runs while the cursor is still at the start of its line: a line
/// that fills the terminal leaves the cursor on its last cell, and an erase
/// issued there clears that cell.
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
        w.write_all(b"\x1b[K")?;
        w.write_all(&bytes[start..line_end])?;
        w.write_all(b"\r\n")?;
        start = index + 1;
    }
    queue!(w, terminal::Clear(terminal::ClearType::FromCursorDown))?;
    if start < bytes.len() {
        w.write_all(&bytes[start..])?;
    }
    Ok(())
}

/// Replace the visible terminal frame with one buffered write and flush.
pub fn replace_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    queue!(w, cursor::MoveTo(0, 0))?;
    write_crlf(w, bytes)?;
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
            modes: Some((mouse, screen)),
            saved_hook: Some(saved_hook),
        })
    }

    /// Leave tty modes in place while this process continues, without
    /// retaining this guard's log capture or panic hook.
    pub(crate) fn preserve_for_handoff(mut self) {
        end_log_capture_flush();
        self.restore_hook();
        std::mem::forget(self);
    }

    /// Restore terminal modes and the panic hook, but retain log capture until
    /// this guard is dropped or consumed by [`Self::into_captured_logs`].
    pub fn restore_modes_keep_capture(&mut self) {
        if let Some((mouse, screen)) = self.modes.take() {
            restore_terminal_modes(mouse, screen);
        }
        self.restore_hook();
    }

    /// Disarm terminal cleanup and return captured logs without writing them.
    /// The caller owns mode cleanup and the timing of replay. If another guard
    /// still captures logs, its eventual release owns the shared buffer.
    pub fn into_captured_logs(mut self) -> Vec<u8> {
        let bytes = end_log_capture().unwrap_or_default();
        self.restore_hook();
        std::mem::forget(self);
        bytes
    }

    /// Hand the terminal to another full-screen application without clearing
    /// the current screen or dropping mouse reporting. The successor paints
    /// over the held frame and takes ownership of terminal modes; keeping mouse
    /// capture armed closes the alternate-screen gap in which terminals turn a
    /// residual wheel tick into an arrow key for the successor. The returned
    /// guard retains cleanup ownership and restores the terminal when dropped.
    pub fn handoff_keep_screen(mut self) -> io::Result<Self> {
        if let Some((_, Screen::Alternate)) = self.modes {
            execute!(io::stdout(), terminal::EnableLineWrap, cursor::Show)?;
        }
        terminal::disable_raw_mode()?;
        self.restore_hook();
        Ok(self)
    }

    fn restore_hook(&mut self) {
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

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if let Some((mouse, screen)) = self.modes.take() {
            restore_terminal_modes(mouse, screen);
        }
        end_log_capture_flush();
        self.restore_hook();
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
    restore_terminal_modes(mouse, screen);
    end_log_capture_flush();
}

fn restore_terminal_modes(mouse: MouseCapture, screen: Screen) {
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
}

#[cfg(test)]
mod tests {
    use super::{
        Arc, Command, MouseCapture, Screen, TerminalModeGuard, io, log_capture, panic, terminal,
    };
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

        assert_eq!(out, b"\x1b[Khead\r\n\x1b[Krow\r\n\x1b[Jtail");
    }

    #[test]
    fn frame_replacement_orders_multiline_output_and_flushes_once() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head\nrow").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1H\x1b[Khead\r\n\x1b[Jrow");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn frame_replacement_preserves_trailing_newline() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head\n").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1H\x1b[Khead\r\n\x1b[J");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn frame_replacement_clears_unterminated_final_line() {
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"head").expect("replace frame");

        assert_eq!(out.bytes, b"\x1b[1;1H\x1b[Jhead");
        assert_eq!(out.flushes, 1);
    }

    #[test]
    fn frame_replacement_preserves_full_width_lines_and_removes_stale_content() {
        let mut terminal = vt100::Parser::new(4.try_into().unwrap(), 8.try_into().unwrap(), 0);
        terminal.process(b"old-old!\r\nstale...\r\nremove!!\r\nmore...");
        let mut out = RecordingWriter::default();

        replace_frame(&mut out, b"12345678\nabcdefgX").expect("replace full-width frame");
        terminal.process(&out.bytes);
        assert_eq!(terminal.screen().contents(), "12345678\nabcdefgX");
        assert_eq!(out.flushes, 1);

        out.bytes.clear();
        replace_frame(&mut out, b"new\n").expect("replace with shorter frame");
        terminal.process(&out.bytes);
        assert_eq!(terminal.screen().contents(), "new");
        assert_eq!(terminal.screen().cursor_position(), (1, 0));

        out.bytes.clear();
        replace_frame(&mut out, b"").expect("clear frame");
        terminal.process(&out.bytes);
        assert_eq!(terminal.screen().contents(), "");
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

    #[test]
    #[cfg(unix)]
    fn repeated_in_process_handoffs_keep_modes_without_capture_or_hook_leaks() {
        const CHILD: &str = "RIMZ_TEST_TUI_HANDOFF_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let pty = nix::pty::openpty(None, None).unwrap();
            let output = std::fs::File::from(pty.slave);
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tui::tests::repeated_in_process_handoffs_keep_modes_without_capture_or_hook_leaks", "--nocapture"])
                .env(CHILD, "1")
                .stdin(output.try_clone().unwrap())
                .stdout(output.try_clone().unwrap())
                .stderr(output)
                .spawn().unwrap();
            let mut reader = std::fs::File::from(pty.master);
            let capture = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = io::Read::read_to_end(&mut reader, &mut bytes);
                bytes
            });
            let status = child.wait().unwrap();
            let bytes = capture.join().unwrap();
            assert!(status.success(), "{}", String::from_utf8_lossy(&bytes));
            assert!(
                !bytes
                    .windows(DISABLE_CLICK_WHEEL_CAPTURE.len())
                    .any(|bytes| bytes == DISABLE_CLICK_WHEEL_CAPTURE.as_bytes()),
                "handoffs must not drop mouse reporting"
            );
            return;
        }

        let previous = panic::take_hook();
        let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = invoked.clone();
        panic::set_hook(Box::new(move |info| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = writeln!(io::stderr(), "{info}");
        }));
        for _ in 0..3 {
            let guard = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main).unwrap();
            let installed = Arc::downgrade(guard.saved_hook.as_ref().unwrap());
            let modes = nix::sys::termios::tcgetattr(io::stdout()).unwrap();
            guard.preserve_for_handoff();
            let capture_depth = log_capture().lock().unwrap().depth;
            assert_eq!(
                capture_depth, 0,
                "in-process handoff must release its capture"
            );
            assert!(
                installed.upgrade().is_none(),
                "in-process handoff must restore its predecessor hook"
            );
            assert!(terminal::is_raw_mode_enabled().unwrap());
            assert_eq!(nix::sys::termios::tcgetattr(io::stdout()).unwrap(), modes);
        }
        let mut guard = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main).unwrap();
        guard.restore_modes_keep_capture();
        assert!(
            !terminal::is_raw_mode_enabled().unwrap(),
            "step-out must restore modes without ending capture"
        );
        TuiLogWriter
            .make_writer()
            .write_all(b"warning while the room runs\n")
            .unwrap();
        assert_eq!(log_capture().lock().unwrap().depth, 1);
        assert_eq!(
            guard.into_captured_logs(),
            b"warning while the room runs\n",
            "session completion must return logs emitted after step-out"
        );
        assert_eq!(log_capture().lock().unwrap().depth, 0);
        terminal::enable_raw_mode().unwrap();
        assert!(panic::catch_unwind(|| panic!("test restored hook")).is_err());
        assert_eq!(invoked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            terminal::is_raw_mode_enabled().unwrap(),
            "the terminal-restore hook must be gone"
        );
        panic::set_hook(previous);
        terminal::disable_raw_mode().unwrap();
    }
}
