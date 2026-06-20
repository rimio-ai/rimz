//! Shared terminal-mode lifecycle for inline and pane-resident TUI surfaces.

use std::io;
use std::panic::{self, PanicHookInfo};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal;

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;
type SharedPanicHook = Arc<Mutex<Option<PanicHook>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseCapture {
    Off,
    Stdout,
    Stderr,
}

pub struct TerminalModeGuard {
    mouse: MouseCapture,
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
    pub fn enable(mouse: MouseCapture) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(err) = enable_mouse(mouse) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        let saved_hook = Arc::new(Mutex::new(Some(panic::take_hook())));
        let hook_for_panic = Arc::clone(&saved_hook);
        panic::set_hook(Box::new(move |info| {
            restore_terminal(mouse);
            if let Some(previous) = hook_for_panic
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
            {
                previous(info);
            }
        }));
        Ok(Self { mouse, saved_hook })
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        restore_terminal(self.mouse);
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

fn restore_terminal(mouse: MouseCapture) {
    match mouse {
        MouseCapture::Off => {}
        MouseCapture::Stdout => {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        MouseCapture::Stderr => {
            let _ = execute!(io::stderr(), DisableMouseCapture);
        }
    }
    let _ = terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::TruecolorSignals;

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
}
