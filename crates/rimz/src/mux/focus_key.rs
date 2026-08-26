//! Room-wide chords: parsed once, then bound by the active backend. tmux binds
//! them as root-table keys whose commands resolve the pressing session at
//! keypress time; Zellij routes them through the presence plugin.

use std::path::{Path, PathBuf};

/// A modifier-plus-key chord such as `Alt+p`. Only modifiers a terminal can
/// reliably deliver are supported; `Cmd` is intentionally absent because the
/// terminal emulator swallows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusChord {
    modifier: Modifier,
    key: char,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modifier {
    Alt,
    Ctrl,
}

impl FocusChord {
    /// Parse a `Mod+key` (or `Mod-key`) chord. Unsupported shapes return
    /// `None` so room birth can warn and skip the binding.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
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
    pub(crate) fn to_tmux(self) -> String {
        let prefix = match self.modifier {
            Modifier::Alt => 'M',
            Modifier::Ctrl => 'C',
        };
        format!("{prefix}-{}", self.key)
    }
}

/// A resolved room-key binding. Room identity is not baked in — a tmux root
/// binding resolves the pressing session at keypress (`#{session_name}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomKeyBinding {
    pub(crate) chord: FocusChord,
    program: PathBuf,
    args: Vec<String>,
}

impl RoomKeyBinding {
    /// Resolve a command and configured chord against the absolute RimZ binary.
    pub fn resolve(chord: &str, rimz_bin: &Path, args: &[&str]) -> Option<Self> {
        Some(Self {
            chord: FocusChord::parse(chord)?,
            program: rimz_bin.to_path_buf(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        })
    }

    /// Render the root-table command, resolving the pressing session at
    /// keypress and forcing tmux because the child runs off-server.
    pub fn tmux_run_shell_command(&self) -> String {
        let mut argv = vec![self.program.to_string_lossy().into_owned()];
        argv.extend(self.args.iter().cloned());
        argv.extend(["--session-name", "#{session_name}", "--mux", "tmux"].map(str::to_owned));
        argv.iter()
            .map(String::as_str)
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Single-quote one argument for `/bin/sh -c`.
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
        assert_eq!(FocusChord::parse("ctrl-s").unwrap().to_tmux(), "C-s");
        assert_eq!(FocusChord::parse("M-0").unwrap().to_tmux(), "M-0");
        assert_eq!(FocusChord::parse("Alt+`").unwrap().to_tmux(), "M-`");
    }

    #[test]
    fn rejects_unsupported_chords() {
        assert_eq!(FocusChord::parse("p"), None);
        assert_eq!(FocusChord::parse("Super+p"), None);
        assert_eq!(FocusChord::parse("Alt+Space"), None);
        assert_eq!(FocusChord::parse("Alt+"), None);
    }

    #[test]
    fn tmux_commands_target_the_pressing_session_and_force_the_backend() {
        let focus = RoomKeyBinding::resolve(
            "Alt+p",
            Path::new("/usr/bin/rimz"),
            &["sidebar", "focus", "--toggle"],
        )
        .expect("focus binding");
        assert_eq!(focus.chord.to_tmux(), "M-p");
        assert_eq!(
            focus.tmux_run_shell_command(),
            "'/usr/bin/rimz' 'sidebar' 'focus' '--toggle' '--session-name' \
             '#{session_name}' '--mux' 'tmux'"
        );

        let zoom = RoomKeyBinding::resolve("Alt+g", Path::new("/usr/bin/rimz"), &["pane", "zoom"])
            .expect("zoom binding");
        assert_eq!(
            zoom.tmux_run_shell_command(),
            "'/usr/bin/rimz' 'pane' 'zoom' '--session-name' '#{session_name}' '--mux' 'tmux'"
        );
    }

    #[test]
    fn unparseable_chord_resolves_to_none() {
        assert_eq!(
            RoomKeyBinding::resolve(
                "nonsense",
                Path::new("/usr/bin/rimz"),
                &["sidebar", "focus", "--toggle"],
            ),
            None
        );
    }
}
