//! The focus-sidebar chord (`[sidebar] focus_key`): parsed once, then bound by
//! the active backend. The chord reaches the sidebar pane from any pane in the
//! room — tmux binds it as a root-table `bind-key` whose command resolves the
//! pressing session at keypress time, Zellij through the presence plugin.

use std::path::{Path, PathBuf};

/// A modifier-plus-key chord such as `Alt+p`. Only modifiers a terminal can
/// reliably deliver are supported — `Cmd` is intentionally absent because the
/// terminal emulator swallows it, and `Ctrl+B` would collide with tmux's prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusChord {
    modifier: Modifier,
    key: char,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modifier {
    Alt,
    Ctrl,
}

impl FocusChord {
    /// Parse a `Mod+key` (or `Mod-key`) chord such as `Alt+p` or `Ctrl+s`. The
    /// modifier is case-insensitive; the key is one printable character.
    /// Returns `None` for an unsupported shape so the caller can warn and skip
    /// the binding rather than register a broken one.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (modifier, key) = raw.split_once(['+', '-'])?;
        let modifier = match modifier.trim().to_ascii_lowercase().as_str() {
            "alt" | "meta" | "m" => Modifier::Alt,
            "ctrl" | "control" | "c" => Modifier::Ctrl,
            _ => return None,
        };
        let mut chars = key.trim().chars();
        let key = chars.next()?;
        if chars.next().is_some() || !key.is_ascii_graphic() {
            return None;
        }
        Some(Self { modifier, key })
    }

    /// tmux root-table key spec: `M-p` / `C-p`.
    pub fn to_tmux(self) -> String {
        let prefix = match self.modifier {
            Modifier::Alt => 'M',
            Modifier::Ctrl => 'C',
        };
        format!("{prefix}-{}", self.key)
    }
}

/// A resolved focus-key binding: the chord plus the rimz binary the keybind
/// runs. Room identity is not baked in — a tmux root binding is server-global,
/// so the command resolves the pressing session at keypress (`#{session_name}`),
/// keeping one binding correct for every room sharing the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusKeyBinding {
    pub(crate) chord: FocusChord,
    /// The rimz binary the keybind runs (absolute, so the keybind never depends
    /// on the pressing pane's `PATH`).
    program: PathBuf,
}

impl FocusKeyBinding {
    /// Resolve from the configured chord label (`None`/`off` already filtered by
    /// [`crate::config::SidebarConfig::focus_key_label`]) and the rimz binary.
    /// `None` when the chord cannot be parsed, so the caller warns and registers
    /// nothing.
    pub fn resolve(chord: &str, rimz_bin: &Path) -> Option<Self> {
        Some(Self {
            chord: FocusChord::parse(chord)?,
            program: rimz_bin.to_path_buf(),
        })
    }

    /// The tmux `run-shell` command: `rimz sidebar focus --toggle` with the
    /// pressing session resolved at keypress via the `#{session_name}` format
    /// (tmux expands `run-shell` arguments before running them) and the backend
    /// forced — a root binding fires off-server, where backend auto-detect has
    /// no `$TMUX` to read. Quoted as a single POSIX-`sh` word list; tmux expands
    /// the format inside the quotes, then `/bin/sh` strips them.
    pub fn tmux_run_shell_command(&self) -> String {
        [
            self.program.to_string_lossy().as_ref(),
            "sidebar",
            "focus",
            "--toggle",
            "--session-name",
            "#{session_name}",
            "--mux",
            "tmux",
        ]
        .into_iter()
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// Single-quote one argument for `/bin/sh -c`, escaping embedded single quotes.
fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alt_and_ctrl_chords_into_tmux_syntax() {
        let alt = FocusChord::parse("Alt+p").expect("alt chord");
        assert_eq!(alt.to_tmux(), "M-p");

        // Case-insensitive modifier, `-` separator, and the `M-`/`C-` aliases.
        assert_eq!(FocusChord::parse("ctrl-s").unwrap().to_tmux(), "C-s");
        assert_eq!(FocusChord::parse("M-0").unwrap().to_tmux(), "M-0");
        assert_eq!(FocusChord::parse("Alt+`").unwrap().to_tmux(), "M-`");
    }

    #[test]
    fn rejects_unsupported_chords() {
        // No modifier, an unknown modifier, a multi-char key, and an empty key.
        assert_eq!(FocusChord::parse("p"), None);
        assert_eq!(FocusChord::parse("Super+p"), None);
        assert_eq!(FocusChord::parse("Alt+Space"), None);
        assert_eq!(FocusChord::parse("Alt+"), None);
    }

    #[test]
    fn tmux_command_resolves_the_session_at_keypress_and_forces_the_backend() {
        let binding =
            FocusKeyBinding::resolve("Alt+p", Path::new("/usr/bin/rimz")).expect("binding");
        assert_eq!(binding.chord.to_tmux(), "M-p");
        // No room identity is baked in: the session is the runtime `#{session_name}`
        // format so the one server-global binding follows whichever room pressed it,
        // and `--mux tmux` forces the backend the off-server `run-shell` can't detect.
        assert_eq!(
            binding.tmux_run_shell_command(),
            "'/usr/bin/rimz' 'sidebar' 'focus' '--toggle' '--session-name' \
             '#{session_name}' '--mux' 'tmux'"
        );
    }

    #[test]
    fn unparseable_chord_resolves_to_none() {
        assert_eq!(
            FocusKeyBinding::resolve("nonsense", Path::new("/usr/bin/rimz")),
            None
        );
    }
}
