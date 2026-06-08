//! Shared terminal-mode lifecycle for inline and pane-resident TUI surfaces.

use std::io;
use std::panic::{self, PanicHookInfo};
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
