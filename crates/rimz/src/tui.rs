//! Shared terminal-mode lifecycle for inline and pane-resident TUI surfaces.

use std::io;
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::{cursor, terminal};

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;
type SharedPanicHook = Arc<Mutex<Option<PanicHook>>>;

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
    saved_hook: SharedPanicHook,
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
pub fn write_crlf(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
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
        terminal::enable_raw_mode()?;
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
            saved_hook,
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
        std::mem::forget(self);
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        restore_terminal(self.mouse, self.screen);
        if let Some(hook) = self
            .saved_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            panic::set_hook(hook);
        }
    }
}

fn enable_mouse(mouse: MouseCapture) -> io::Result<()> {
    match mouse {
        MouseCapture::Off => Ok(()),
        MouseCapture::Stdout => execute!(io::stdout(), EnableMouseCapture),
        MouseCapture::Stderr => execute!(io::stderr(), EnableMouseCapture),
    }
}

pub(crate) fn restore_terminal(mouse: MouseCapture, screen: Screen) {
    match mouse {
        MouseCapture::Off => {}
        MouseCapture::Stdout => {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        MouseCapture::Stderr => {
            let _ = execute!(io::stderr(), DisableMouseCapture);
        }
    }
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
    use super::{TruecolorSignals, write_crlf};

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
}
