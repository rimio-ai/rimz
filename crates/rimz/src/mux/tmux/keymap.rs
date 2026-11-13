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
mod tests {
    use super::*;
    use crate::config::MachineConfig;

    #[test]
    fn every_named_action_compiles() {
        for name in KeyActionName::vocabulary() {
            let action = KeyAction::Named(KeyActionName::parse(name).expect(name));
            assert_eq!(action.to_string(), name);
            assert!(
                !action_command(&action, Path::new("/bin/rimz"), Some(KeyChord::Ctrl('b')))
                    .is_empty()
            );
        }
    }

    fn commands(config: &str, roots: &str, prior: Option<&str>) -> Vec<Vec<String>> {
        let mut options = vec![("@rimz_keys", roots), ("mode-keys", "emacs")];
        options.extend(prior.map(|prior| ("@rimz_mode_keys", prior)));
        commands_with(config, &options)
    }

    fn commands_with(config: &str, options: &[(&str, &str)]) -> Vec<Vec<String>> {
        let config: MachineConfig = toml::from_str(config).unwrap();
        let options: Vec<_> = options
            .iter()
            .map(|(name, value)| words(&[name, value]))
            .collect();
        keymap_commands(&config.room_keymap(), Path::new("/bin/rimz"), &options)
            .into_iter()
            .flatten()
            .collect()
    }

    fn prefix_commands(commands: &[Vec<String>]) -> Vec<Vec<String>> {
        commands
            .iter()
            .filter(|cmd| cmd[0] == "set-option" && cmd.iter().any(|arg| arg.contains("prefix")))
            .cloned()
            .collect()
    }

    #[test]
    fn a_colliding_tmux_prefix_is_recorded_once_turned_off_and_restored() {
        let ctrl_b = "[room]\nstyle='rimz'\n[keys]\nprefix='Ctrl+b'\n";
        let first = commands_with(ctrl_b, &[("prefix", "C-b"), ("prefix2", "None")]);
        assert_eq!(
            prefix_commands(&first),
            [
                words(&["set-option", "-s", "@rimz_prefix", "C-b"]),
                words(&["set-option", "-g", "prefix", "None"]),
            ]
        );
        let second = commands_with(
            ctrl_b,
            &[
                ("prefix", "None"),
                ("prefix2", "None"),
                ("@rimz_prefix", "C-b"),
            ],
        );
        assert_eq!(prefix_commands(&second), [] as [Vec<String>; 0]);
        let native = commands_with("", &[("prefix", "None"), ("@rimz_prefix", "C-b")]);
        assert_eq!(
            prefix_commands(&native),
            [
                words(&["set-option", "-g", "prefix", "C-b"]),
                words(&["set-option", "-su", "@rimz_prefix"]),
            ]
        );
        let rimz = commands_with(
            "[room]\nstyle='rimz'",
            &[("prefix", "C-b"), ("prefix2", "M-x")],
        );
        assert_eq!(prefix_commands(&rimz), [] as [Vec<String>; 0]);
        let second_prefix = commands_with(ctrl_b, &[("prefix", "C-a"), ("prefix2", "C-b")]);
        assert_eq!(
            prefix_commands(&second_prefix),
            [
                words(&["set-option", "-s", "@rimz_prefix2", "C-b"]),
                words(&["set-option", "-g", "prefix2", "None"]),
            ]
        );
    }

    #[test]
    fn native_binds_only_sidebar_commands_and_removes_recorded_keys() {
        let commands = commands("", "M-x M-; M-?", None);
        for key in ["M-x", "M-;", "M-?"] {
            assert!(
                commands.contains(
                    &["unbind-key", "-T", "root", key]
                        .map(str::to_owned)
                        .to_vec()
                )
            );
        }
        let binds: Vec<_> = commands.iter().filter(|cmd| cmd[0] == "bind-key").collect();
        assert_eq!(binds.len(), 2);
        assert!(
            binds.iter().any(|cmd| cmd[3] == "M-p"
                && cmd.last().unwrap().contains("'sidebar' 'focus' '--toggle'"))
        );
        assert!(
            binds
                .iter()
                .any(|cmd| cmd[3] == "M-g" && cmd.last().unwrap().contains("'pane' 'zoom'"))
        );
        assert!(
            !commands
                .iter()
                .any(|cmd| cmd.iter().any(|arg| arg == "mode-keys"))
        );
    }

    #[test]
    fn rimz_compiles_native_actions_sticky_tables_and_uppercase_aliases() {
        let commands = commands("[room]\nstyle='rimz'", "", None);
        let bind = |table: &str, key: &str| {
            commands
                .iter()
                .find(|cmd| cmd[0] == "bind-key" && cmd[2] == table && cmd[3] == key)
                .unwrap()
        };
        assert_eq!(&bind("root", "M-h")[4..], ["select-pane", "-L"]);
        assert_eq!(
            &bind("root", "M-t")[4..],
            ["new-window", "-c", "#{pane_current_path}"]
        );
        assert_eq!(
            &bind("rimz-resize", "h")[4..],
            [
                "resize-pane",
                "-L",
                ";",
                "switch-client",
                "-T",
                "rimz-resize"
            ]
        );
        assert_eq!(
            &bind("rimz-prefix", "r")[4..],
            ["switch-client", "-T", "rimz-resize"]
        );
        assert_eq!(&bind("root", "M-J")[4..], &bind("root", "M-S-J")[4..]);
        for key in ["H", "J", "K", "L", "X"] {
            assert!(
                commands.iter().any(|command| command[0] == "bind-key"
                    && command[2] == "rimz-prefix"
                    && command[3] == format!("S-{key}")),
                "extended shifted {key} must work on tmux 3.5"
            );
            assert_eq!(
                &bind("rimz-prefix", key)[4..],
                &bind("rimz-prefix", &format!("S-{key}"))[4..],
            );
        }
        assert_eq!(&bind("root", "M-?")[4..], &bind("root", "M-S-?")[4..]);
        assert!(
            commands.contains(
                &["set-option", "-gw", "mode-keys", "vi"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert!(
            commands.contains(
                &["set-option", "-s", "@rimz_mode_keys", "emacs"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
    }

    fn bind<'a>(commands: &'a [Vec<String>], table: &str, key: &str) -> &'a [String] {
        let bind = commands
            .iter()
            .find(|cmd| cmd[0] == "bind-key" && cmd[2] == table && cmd[3] == key);
        assert!(bind.is_some(), "{table} {key} must be bound");
        &bind.unwrap()[4..]
    }

    #[test]
    fn sticky_modes_reenter_only_after_a_motion_and_prefix_stays_one_shot() {
        let commands = commands(
            "[room]\nstyle='rimz'\n[keys.bind.pane]\nh='pane.focus.left'\nz='pane.zoom'\n\
             [keys.bind.tab]\nn='tab.next'\nc='tab.new'\n[keys.bind.resize]\nz='pane.zoom'\n\
             [keys.bind.move]\nz='pane.zoom'\n",
            "",
            None,
        );
        for table in [
            "rimz-prefix",
            "rimz-pane",
            "rimz-tab",
            "rimz-resize",
            "rimz-move",
        ] {
            assert!(
                commands.contains(&words(&["unbind-key", "-a", "-T", table])),
                "{table} must be cleared on every apply"
            );
        }
        let sticky = |table: &str| words(&[";", "switch-client", "-T", table]);
        assert_eq!(
            bind(&commands, "rimz-pane", "h"),
            [words(&["select-pane", "-L"]), sticky("rimz-pane")].concat()
        );
        assert_eq!(
            bind(&commands, "rimz-tab", "n"),
            [words(&["next-window"]), sticky("rimz-tab")].concat()
        );
        let move_h = bind(&commands, "rimz-move", "h");
        assert_eq!(move_h[0], "if-shell");
        assert_eq!(move_h[5..], sticky("rimz-move"));
        for (table, key) in [
            ("rimz-pane", "z"),
            ("rimz-tab", "c"),
            ("rimz-resize", "z"),
            ("rimz-move", "z"),
            ("rimz-prefix", "h"),
        ] {
            assert!(
                !bind(&commands, table, key).contains(&";".to_owned()),
                "{table} {key} must return to normal"
            );
        }
    }

    /// tmux's own parse of one nested command string.
    fn parsed(command: &str) -> Vec<String> {
        shlex::split(command).unwrap()
    }

    #[test]
    fn split_close_and_swap_leave_the_sidebar_alone_in_every_bundled_keymap() {
        let on_sidebar =
            "#{||:#{==:#{pane_title},rimz-sidebar},#{m:* sidebar serve*,#{pane_start_command}}}";
        for keymap in ["rimz", "tmux", "zellij", "vim"] {
            let config = format!("[room]\nstyle='rimz'\n[keys]\nkeymap='{keymap}'\n");
            let commands = commands(&config, "", None);
            let machine: MachineConfig = toml::from_str(&config).unwrap();
            let mut checked = 0;
            for (&mode, bindings) in &machine.room_keymap().modes {
                for (&key, action) in bindings {
                    let KeyAction::Named(
                        name @ (KeyActionName::PaneClose
                        | KeyActionName::PaneSplit(_)
                        | KeyActionName::PaneSwap(_)),
                    ) = action
                    else {
                        continue;
                    };
                    let body = bind(&commands, table(mode), &keys(key)[0]);
                    assert_eq!(
                        body[..3],
                        words(&["if-shell", "-F", on_sidebar]),
                        "{keymap} {key:?}"
                    );
                    match *name {
                        KeyActionName::PaneClose => {
                            assert_eq!(body[3..5], words(&["", "kill-pane"]))
                        }
                        KeyActionName::PaneSplit(direction) => {
                            let (side, flag) = match direction {
                                KeySplitDirection::Right => ("right", "-h"),
                                KeySplitDirection::Down => ("down", "-v"),
                            };
                            let child = parsed(&body[3]);
                            assert_eq!(child[..2], words(&["run-shell", "-b"]));
                            assert_eq!(
                                parsed(&child[2]),
                                words(&[
                                    "/bin/rimz",
                                    "--mux",
                                    "tmux",
                                    "sidebar",
                                    "split",
                                    side,
                                    "--from-pane",
                                    "tmux:#{pane_id}",
                                    "--session-name",
                                    "#{session_name}",
                                ])
                            );
                            assert_eq!(
                                parsed(&body[4]),
                                words(&["split-window", flag, "-c", "#{pane_current_path}"])
                            );
                        }
                        KeyActionName::PaneSwap(direction) => {
                            let target = format!("{{{direction}-of}}");
                            assert_eq!(body[3], "");
                            let inner = parsed(&body[4]);
                            assert_eq!(
                                inner[..6],
                                words(&[
                                    "if-shell",
                                    "-F",
                                    "-t",
                                    &target,
                                    &format!("#{{&&:#{{pane_format}},{on_sidebar}}}"),
                                    "",
                                ])
                            );
                            assert_eq!(parsed(&inner[6]), words(&["swap-pane", "-s", &target]));
                        }
                        _ => unreachable!("filtered above"),
                    }
                    checked += 1;
                }
            }
            assert!(checked >= 7, "{keymap}: close, both splits and four swaps");
        }
    }

    #[test]
    fn send_prefix_focus_next_and_arrow_keys_compile() {
        for (prefix, sent) in [("Ctrl+b", "C-b"), ("Alt+w", "M-w"), ("Alt+;", "M-\\;")] {
            let commands = commands(
                &format!(
                    "[room]\nstyle='rimz'\n[keys]\nprefix='{prefix}'\n[keys.bind.prefix]\n\
                     b='pane.send_prefix'\no='pane.focus.next'\nLeft='pane.focus.left'\n\
                     'Alt+Up'='pane.focus.up'\n'Ctrl+Down'='pane.focus.down'\n"
                ),
                "",
                None,
            );
            assert_eq!(bind(&commands, "rimz-prefix", "b"), ["send-keys", sent]);
            assert_eq!(
                bind(&commands, "rimz-prefix", "o"),
                ["select-pane", "-t", ":.+"]
            );
            assert_eq!(
                bind(&commands, "rimz-prefix", "Left")[..2],
                ["select-pane", "-L"]
            );
            assert_eq!(
                bind(&commands, "rimz-prefix", "M-Up")[..2],
                ["select-pane", "-U"]
            );
            assert_eq!(
                bind(&commands, "rimz-prefix", "C-Down")[..2],
                ["select-pane", "-D"]
            );
        }
    }

    #[test]
    fn native_scrollback_leaves_mode_keys_and_restores_a_recorded_value() {
        let config: MachineConfig = toml::from_str("[room]\nstyle='rimz'").unwrap();
        let mut keymap = config.room_keymap();
        keymap.style.as_mut().unwrap().scrollback = KeyScrollback::Native;
        let run = |options: &[(&str, &str)]| -> Vec<Vec<String>> {
            let options: Vec<_> = options
                .iter()
                .map(|(name, value)| words(&[name, value]))
                .collect();
            keymap_commands(&keymap, Path::new("/bin/rimz"), &options)
                .into_iter()
                .flatten()
                .filter(|cmd| {
                    cmd.iter()
                        .any(|arg| arg.contains("mode-keys") || arg.contains("mode_keys"))
                })
                .collect()
        };
        assert_eq!(run(&[("mode-keys", "emacs")]), [] as [Vec<String>; 0]);
        assert_eq!(
            run(&[("mode-keys", "vi"), ("@rimz_mode_keys", "emacs")]),
            [
                words(&["set-option", "-gw", "mode-keys", "emacs"]),
                words(&["set-option", "-su", "@rimz_mode_keys"]),
            ]
        );
    }

    #[test]
    fn native_restores_mode_keys_only_behind_the_marker() {
        let commands = commands("", "", Some("emacs"));
        assert!(
            commands.contains(
                &["set-option", "-gw", "mode-keys", "emacs"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert!(
            commands.contains(
                &["set-option", "-su", "@rimz_mode_keys"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
    }

    #[test]
    fn user_commands_quote_literal_formats_and_do_not_append_session_name() {
        let commands = commands(
            "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+y\"={run=\"rimz message @coder -- '#{session_name} % ;'\"}",
            "",
            None,
        );
        let bind = commands
            .iter()
            .find(|cmd| cmd[0] == "bind-key" && cmd[3] == "M-y")
            .unwrap();
        assert_eq!(
            &bind[4..],
            [
                "run-shell",
                "-b",
                "cd -- #{q:session_path} && '/bin/rimz' '--mux' 'tmux' 'message' '@coder' \
                 '--' '##{session_name} % ;'"
            ]
        );
        let palette = commands
            .iter()
            .find(|cmd| cmd[0] == "bind-key" && cmd[3] == "M-n")
            .unwrap();
        assert!(
            palette
                .last()
                .unwrap()
                .contains("'--client' '#{client_name}' '--from-pane' 'tmux:#{pane_id}'")
        );
        assert!(
            palette
                .last()
                .unwrap()
                .contains("'--session-name' '#{session_name}'")
        );
    }

    #[test]
    fn alt_digits_run_the_tab_verbs_and_an_overlay_restores_a_slot() {
        let bind = |commands: &[Vec<String>], key: &str| {
            commands
                .iter()
                .find(|cmd| cmd[0] == "bind-key" && cmd[3] == key)
                .map(|cmd| cmd[4..].to_vec())
        };
        let bundled = commands("[room]\nstyle='rimz'", "", None);
        for (key, verb) in [
            ("M-3", "'tab' 'terminal' '3'"),
            ("M-0", "'tab' 'dashboard'"),
        ] {
            assert_eq!(
                bind(&bundled, key).unwrap(),
                [
                    "run-shell".to_owned(),
                    "-b".to_owned(),
                    format!(
                        "'/bin/rimz' '--mux' 'tmux' {verb} '--session-name' '#{{session_name}}'"
                    )
                ]
            );
        }
        let overlay = commands(
            "[room]\nstyle='rimz'\n[keys.bind.normal]\n'Alt+3'='card.slot.3'\n'Alt+0'='none'",
            "",
            None,
        );
        assert!(
            bind(&overlay, "M-3")
                .unwrap()
                .last()
                .unwrap()
                .contains("'sidebar' 'slot' '3'")
        );
        assert_eq!(bind(&overlay, "M-0"), None);
    }

    #[test]
    fn named_commands_do_not_depend_on_the_session_start_directory() {
        for config in ["", "[room]\nstyle='rimz'"] {
            for command in commands(config, "", None) {
                if command[0] == "bind-key"
                    && command.get(4).is_some_and(|word| word == "run-shell")
                {
                    assert!(
                        command[6].starts_with("'/bin/rimz' '--mux' 'tmux' "),
                        "named actions must run even after the start directory is removed: {command:?}"
                    );
                }
            }
        }
    }
}
