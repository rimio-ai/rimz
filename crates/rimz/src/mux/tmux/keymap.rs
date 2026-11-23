use std::path::Path;

use crate::config::keys::{
    KeyAction, KeyActionName, KeyChord, KeyDirection, KeyMode, KeyScrollback, KeySplitDirection,
    RoomKeymap,
};

use super::TmuxBackend;
use crate::mux::Result;

impl TmuxBackend {
    pub(super) fn apply_keymap(&self, keymap: &RoomKeymap, rimz_bin: &Path) -> Result<()> {
        let output = self
            .batch_spec(&[
                words(&["show-options", "-s"]),
                words(&["show-options", "-gw", "mode-keys"]),
                words(&["show-options", "-g", "prefix"]),
                words(&["show-options", "-g", "prefix2"]),
            ])
            .run()?;
        let options: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(shlex::split)
            .collect();
        self.run_script(&keymap_commands(keymap, rimz_bin, &options))?;
        Ok(())
    }
}

/// The value of one `show-options` row, `<name> <value>`.
fn option<'a>(options: &'a [Vec<String>], key: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|row| row.first().is_some_and(|name| name == key))
        .and_then(|row| row.get(1))
        .map(String::as_str)
}

fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn table(mode: KeyMode) -> &'static str {
    match mode {
        KeyMode::Normal => "root",
        KeyMode::Prefix => "rimz-prefix",
        KeyMode::Pane => "rimz-pane",
        KeyMode::Tab => "rimz-tab",
        KeyMode::Resize => "rimz-resize",
        KeyMode::Move => "rimz-move",
    }
}

fn keys(key: KeyChord) -> Vec<String> {
    match key {
        KeyChord::Alt(key) if key.is_ascii_uppercase() || key == '?' => {
            vec![format!("M-{key}"), format!("M-S-{key}")]
        }
        KeyChord::Alt(key) => vec![format!("M-{key}")],
        KeyChord::Ctrl(key) => vec![format!("C-{key}")],
        KeyChord::Character(key) if key.is_ascii_uppercase() => {
            vec![key.to_string(), format!("S-{key}")]
        }
        KeyChord::Character(key) => vec![key.to_string()],
        KeyChord::Esc => vec!["Escape".into()],
        KeyChord::Enter => vec!["Enter".into()],
        KeyChord::Arrow(direction) => vec![arrow_key(direction).into()],
        KeyChord::AltArrow(direction) => vec![format!("M-{}", arrow_key(direction))],
        KeyChord::CtrlArrow(direction) => vec![format!("C-{}", arrow_key(direction))],
    }
}

fn arrow_key(direction: KeyDirection) -> &'static str {
    match direction {
        KeyDirection::Left => "Left",
        KeyDirection::Right => "Right",
        KeyDirection::Up => "Up",
        KeyDirection::Down => "Down",
    }
}

fn keymap_commands(
    keymap: &RoomKeymap,
    rimz_bin: &Path,
    options: &[Vec<String>],
) -> Vec<Vec<Vec<String>>> {
    let prior_mode_keys = option(options, "@rimz_mode_keys");
    let global_mode_keys = option(options, "mode-keys").unwrap_or_default();
    let mut commands = Vec::new();
    for key in option(options, "@rimz_keys")
        .unwrap_or_default()
        .split_whitespace()
    {
        commands.push(words(&["unbind-key", "-T", "root", key]));
    }
    for mode in [
        KeyMode::Prefix,
        KeyMode::Pane,
        KeyMode::Tab,
        KeyMode::Resize,
        KeyMode::Move,
    ] {
        commands.push(words(&["unbind-key", "-a", "-T", table(mode)]));
    }
    let prefix = keymap.style.map(|style| style.prefix);
    if keymap
        .style
        .is_some_and(|style| style.scrollback == KeyScrollback::Vi)
    {
        if prior_mode_keys.is_none() {
            commands.push(words(&[
                "set-option",
                "-s",
                "@rimz_mode_keys",
                global_mode_keys,
            ]));
        }
        commands.push(words(&["set-option", "-gw", "mode-keys", "vi"]));
    } else if let Some(prior) = prior_mode_keys {
        commands.push(if prior.is_empty() {
            words(&["set-option", "-gwu", "mode-keys"])
        } else {
            words(&["set-option", "-gw", "mode-keys", prior])
        });
        commands.push(words(&["set-option", "-su", "@rimz_mode_keys"]));
    }
    let mut root_keys = Vec::new();
    for (&mode, bindings) in &keymap.modes {
        for (&key, action) in bindings {
            let body = action_command(action, rimz_bin, prefix);
            for key in keys(key) {
                let mut bind = words(&["bind-key", "-T", table(mode), &key]);
                bind.extend(body.iter().map(|word| bind_body_word(word)));
                if matches!(
                    mode,
                    KeyMode::Pane | KeyMode::Tab | KeyMode::Resize | KeyMode::Move
                ) && matches!(action, KeyAction::Named(name) if name.is_motion())
                {
                    bind.extend(words(&[";", "switch-client", "-T", table(mode)]));
                }
                commands.push(bind);
                if mode == KeyMode::Normal {
                    root_keys.push(key);
                }
            }
        }
    }
    commands.push(words(&[
        "set-option",
        "-s",
        "@rimz_keys",
        &root_keys.join(" "),
    ]));
    for (name, marker) in [("prefix", "@rimz_prefix"), ("prefix2", "@rimz_prefix2")] {
        commands.extend(prefix_option_commands(
            name,
            marker,
            option(options, name),
            option(options, marker),
            &root_keys,
        ));
    }
    commands.into_iter().map(|command| vec![command]).collect()
}

/// tmux checks `prefix` and `prefix2` before any key table, so a root key RimZ
/// binds that equals one would never fire. Turn that option off, recording its
/// value once behind `marker`, and restore it once no root key collides.
fn prefix_option_commands(
    name: &str,
    marker: &str,
    current: Option<&str>,
    recorded: Option<&str>,
    root_keys: &[String],
) -> Vec<Vec<String>> {
    let bound = |key: &str| root_keys.iter().any(|root| root == key);
    if let Some(current) = current.filter(|current| bound(current)) {
        let mut commands = Vec::new();
        if recorded.is_none() {
            commands.push(words(&["set-option", "-s", marker, current]));
        }
        commands.push(words(&["set-option", "-g", name, "None"]));
        return commands;
    }
    match recorded {
        Some(recorded) if !bound(recorded) => vec![
            words(&["set-option", "-g", name, recorded]),
            words(&["set-option", "-su", marker]),
        ],
        _ => Vec::new(),
    }
}

/// `bind-key` reparses its body words as a command list, where a word ending in `;` ends the
/// command (`send-keys M-;` binds `send-keys M-`), so that `;` is escaped as `\;`.
fn bind_body_word(word: &str) -> String {
    match word.strip_suffix(';') {
        Some(head) => format!("{head}\\;"),
        None => word.to_owned(),
    }
}

/// True in a RimZ sidebar pane: the format twin of the listing's title-or-spawn-command
/// classification (`options::spawn_command_is_sidebar_serve`). Split, close and swap
/// bodies test it so a key pressed on the sidebar never acts on it.
const ON_SIDEBAR: &str =
    "#{||:#{==:#{pane_title},rimz-sidebar},#{m:* sidebar serve*,#{pane_start_command}}}";

/// The pressed pane, as a command's `--from-pane`.
const FROM_PANE: &str = " '--from-pane' 'tmux:#{pane_id}'";

/// `command` as one command string that `if-shell` parses when it runs; it expands no
/// formats first, so each nested command expands its own.
fn nested(command: &[String]) -> String {
    command
        .iter()
        .map(|word| super::status::quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn action_command(action: &KeyAction, rimz_bin: &Path, prefix: Option<KeyChord>) -> Vec<String> {
    let name = match action {
        KeyAction::Run { args } => {
            return run_command(rimz_bin, args, false, "");
        }
        KeyAction::Named(name) => *name,
    };
    match name {
        KeyActionName::Command { args, .. } => run_command(rimz_bin, &words(args), true, ""),
        KeyActionName::Palette(mode) => run_command(
            rimz_bin,
            &words(&mode.command()),
            true,
            &format!(" '--client' '#{{client_name}}'{FROM_PANE}"),
        ),
        KeyActionName::PaneFocus(direction) => words(&["select-pane", direction_flag(direction)]),
        KeyActionName::PaneFocusNext => words(&["select-pane", "-t", ":.+"]),
        // Native style resolves no prefix and binds no `pane.send_prefix`.
        KeyActionName::PaneSendPrefix => std::iter::once("send-keys".to_owned())
            .chain(prefix.and_then(|prefix| keys(prefix).into_iter().next()))
            .collect(),
        KeyActionName::PaneSwap(direction) => {
            let target = format!("{{{direction}-of}}");
            let neighbour_is_sidebar = format!("#{{&&:#{{pane_format}},{ON_SIDEBAR}}}");
            let swap = nested(&words(&["swap-pane", "-s", &target]));
            let inner = words(&[
                "if-shell",
                "-F",
                "-t",
                &target,
                &neighbour_is_sidebar,
                "",
                &swap,
            ]);
            words(&["if-shell", "-F", ON_SIDEBAR, "", &nested(&inner)])
        }
        KeyActionName::PaneSplit(direction) => {
            let (side, flag) = match direction {
                KeySplitDirection::Right => ("right", "-h"),
                KeySplitDirection::Down => ("down", "-v"),
            };
            let child = run_command(
                rimz_bin,
                &words(&["sidebar", "split", side]),
                true,
                FROM_PANE,
            );
            let split = words(&["split-window", flag, "-c", "#{pane_current_path}"]);
            words(&[
                "if-shell",
                "-F",
                ON_SIDEBAR,
                &nested(&child),
                &nested(&split),
            ])
        }
        KeyActionName::PaneResize(direction) => words(&["resize-pane", direction_flag(direction)]),
        KeyActionName::PaneClose => words(&["if-shell", "-F", ON_SIDEBAR, "", "kill-pane"]),
        KeyActionName::CardClose => {
            run_command(rimz_bin, &words(&["sidebar", "close"]), true, FROM_PANE)
        }
        KeyActionName::TabNew => words(&["new-window", "-c", "#{pane_current_path}"]),
        KeyActionName::TabNext => words(&["next-window"]),
        KeyActionName::TabPrev => words(&["previous-window"]),
        KeyActionName::Mode(mode) => words(&["switch-client", "-T", table(mode)]),
        KeyActionName::Scrollback => words(&["copy-mode"]),
        KeyActionName::SessionDetach => words(&["detach-client"]),
        KeyActionName::KeysHelp => words(&[
            "display-popup",
            "-E",
            "-d",
            "#{session_path}",
            &shell_argv(rimz_bin, &words(&["keys", "--wait"])),
        ]),
    }
}

fn direction_flag(direction: KeyDirection) -> &'static str {
    match direction {
        KeyDirection::Left => "-L",
        KeyDirection::Right => "-R",
        KeyDirection::Up => "-U",
        KeyDirection::Down => "-D",
    }
}

/// `run-shell -b` of `rimz --mux tmux <args>`, followed by `context`: shell words left
/// unescaped so the pressing client's formats expand in them.
fn run_command(rimz_bin: &Path, args: &[String], session: bool, context: &str) -> Vec<String> {
    let mut command = shell_argv(rimz_bin, args);
    if !session {
        // tmux 3.5 expands command formats, but not run-shell's -c argument.
        command = format!("cd -- #{{q:session_path}} && {command}");
    }
    command.push_str(context);
    if session {
        command.push_str(" '--session-name' '#{session_name}'");
    }
    words(&["run-shell", "-b", &command])
}

fn shell_argv(rimz_bin: &Path, args: &[String]) -> String {
    std::iter::once(rimz_bin.to_string_lossy().as_ref())
        .chain(["--mux", "tmux"])
        .chain(args.iter().map(String::as_str))
        .map(|arg| format!("'{}'", arg.replace('#', "##").replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
