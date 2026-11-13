use std::path::Path;

use crate::config::keys::{
    KeyAction, KeyActionName, KeyChord, KeyDirection, KeyMode, KeyScrollback, KeySplitDirection,
    RoomKeymap,
};
use serde_json::{Value, json};

pub(super) fn encode(keymap: &RoomKeymap, session: &str, root: &Path) -> String {
    let rimz = keymap.style.is_some();
    let prefix = keymap.style.map(|style| style.prefix);
    let mut commands = Vec::new();
    let mut modes = Vec::new();
    for (&mode, bindings) in &keymap.modes {
        let binds: Vec<_> = bindings
            .iter()
            .map(|(&key, action)| {
                let mut steps = vec![step(action, prefix, session, root, &mut commands)];
                if returns_to_normal(mode, action) {
                    steps.push(json!({"mode":"locked"}));
                }
                json!({"key": key_label(key), "steps": steps})
            })
            .collect();
        modes.push(json!({"mode": mode_word(mode), "clear": rimz, "binds": binds}));
        if !rimz {
            modes.push(json!({"mode":"normal", "clear":false, "binds":binds}));
        }
    }
    match keymap.style.map(|style| style.scrollback) {
        Some(KeyScrollback::Vi) => modes.extend(scroll_modes()),
        Some(KeyScrollback::Native) => modes.extend(native_scroll_modes()),
        None => {}
    }
    json!({
        "report_modes": rimz,
        "commands": commands,
        "modes": modes,
        "sidebar": crate::pane::SIDEBAR_CHROME_TITLE,
    })
    .to_string()
    .replace('%', "%25")
    .replace(',', "%2C")
}

/// Prefix mode is one-shot; pane, tab, resize, and move stay entered only after
/// a motion. A mode action names its own next mode.
fn returns_to_normal(mode: KeyMode, action: &KeyAction) -> bool {
    let (changes_mode, motion) = match action {
        KeyAction::Named(name) => (name.changes_mode(), name.is_motion()),
        KeyAction::Run { .. } => (false, false),
    };
    match mode {
        KeyMode::Normal => false,
        KeyMode::Prefix => !changes_mode,
        KeyMode::Pane | KeyMode::Tab | KeyMode::Resize | KeyMode::Move => !changes_mode && !motion,
    }
}

fn mode_word(mode: KeyMode) -> &'static str {
    match mode {
        KeyMode::Normal => "locked",
        KeyMode::Prefix => "tmux",
        KeyMode::Pane => "pane",
        KeyMode::Tab => "tab",
        KeyMode::Resize => "resize",
        KeyMode::Move => "move",
    }
}

fn key_label(key: KeyChord) -> String {
    match key {
        KeyChord::Alt(key) => format!("Alt {key}"),
        KeyChord::Ctrl(key) => format!("Ctrl {key}"),
        KeyChord::Character(key) => key.to_string(),
        KeyChord::Esc => "Esc".into(),
        KeyChord::Enter => "Enter".into(),
        KeyChord::Arrow(direction) => direction_word(direction).into(),
        KeyChord::AltArrow(direction) => format!("Alt {}", direction_word(direction)),
        KeyChord::CtrlArrow(direction) => format!("Ctrl {}", direction_word(direction)),
    }
}

/// The bytes a terminal sends for the prefix chord, which `parse_prefix` limits
/// to `Alt` or `Ctrl` with one ASCII graphic character.
fn chord_bytes(chord: KeyChord) -> Vec<u8> {
    match chord {
        KeyChord::Ctrl(key) => vec![(key.to_ascii_lowercase() as u8) & 0x1f],
        KeyChord::Alt(key) => vec![0x1b, key as u8],
        _ => Vec::new(),
    }
}

fn step(
    action: &KeyAction,
    prefix: Option<KeyChord>,
    session: &str,
    root: &Path,
    commands: &mut Vec<Vec<String>>,
) -> Value {
    let name = match action {
        KeyAction::Run { args } => {
            return command_step(commands, args.clone(), "run", root);
        }
        KeyAction::Named(name) => *name,
    };
    let native: &[&str] = match name {
        KeyActionName::Command { args, .. } => {
            let mut args: Vec<_> = args.iter().map(|arg| (*arg).to_owned()).collect();
            args.extend(["--session-name", session].map(str::to_owned));
            return command_step(commands, args, "run", root);
        }
        KeyActionName::Palette(mode) => return json!({"palette":mode.as_str()}),
        KeyActionName::Mode(mode) => return json!({"mode":mode_word(mode)}),
        KeyActionName::Scrollback => return json!({"mode":"scroll"}),
        KeyActionName::KeysHelp => {
            let args = ["keys", "--wait"].map(str::to_owned).to_vec();
            return command_step(commands, args, "float", root);
        }
        KeyActionName::PaneSendPrefix => {
            let write = std::iter::once("Write".to_owned())
                .chain(
                    prefix
                        .into_iter()
                        .flat_map(chord_bytes)
                        .map(|byte| byte.to_string()),
                )
                .collect::<Vec<_>>();
            return json!({"action":write});
        }
        KeyActionName::PaneFocus(direction) => &["MoveFocus", direction_word(direction)],
        KeyActionName::PaneFocusNext => &["FocusNextPane"],
        KeyActionName::PaneSwap(direction) => {
            return json!({"guard":{"action":["MovePane", direction_word(direction)]}});
        }
        KeyActionName::PaneSplit(direction) => {
            let (side, word) = match direction {
                KeySplitDirection::Right => ("right", "Right"),
                KeySplitDirection::Down => ("down", "Down"),
            };
            let args = ["sidebar", "split", side, "--session-name", session].map(str::to_owned);
            let run = command_step(commands, args.to_vec(), "run", root)["run"].clone();
            return json!({"guard":{"action":["NewPane", word], "run": run}});
        }
        KeyActionName::PaneResize(direction) => &[
            "Resize",
            match direction {
                KeyDirection::Left => "Increase Left",
                KeyDirection::Right => "Increase Right",
                KeyDirection::Up => "Increase Up",
                KeyDirection::Down => "Increase Down",
            },
        ],
        KeyActionName::PaneClose => return json!({"guard":{"action":["CloseFocus"]}}),
        KeyActionName::TabNew => &["NewTab"],
        KeyActionName::TabNext => &["GoToNextTab"],
        KeyActionName::TabPrev => &["GoToPreviousTab"],
        KeyActionName::SessionDetach => &["Detach"],
    };
    json!({"action":native})
}

fn direction_word(direction: KeyDirection) -> &'static str {
    match direction {
        KeyDirection::Left => "Left",
        KeyDirection::Right => "Right",
        KeyDirection::Up => "Up",
        KeyDirection::Down => "Down",
    }
}

fn command_step(
    commands: &mut Vec<Vec<String>>,
    args: Vec<String>,
    kind: &str,
    root: &Path,
) -> Value {
    let index = commands.len();
    commands.push(
        [
            "--mux".into(),
            "zellij".into(),
            "--root".into(),
            root.display().to_string(),
        ]
        .into_iter()
        .chain(args)
        .collect(),
    );
    json!({(kind):index})
}

fn scroll_modes() -> Vec<Value> {
    let exit = || json!([{"action":["ScrollToBottom"]},{"mode":"locked"}]);
    let bind = |key: &str, steps: Value| json!({"key":key,"steps":steps});
    vec![
        json!({"mode":"scroll","clear":true,"binds":[
            bind("j", json!([{"action":["ScrollDown"]}])),
            bind("k", json!([{"action":["ScrollUp"]}])),
            bind("Ctrl d", json!([{"action":["HalfPageScrollDown"]}])),
            bind("Ctrl u", json!([{"action":["HalfPageScrollUp"]}])),
            bind("g", json!([{"action":["ScrollToTop"]}])),
            bind("G", json!([{"action":["ScrollToBottom"]}])),
            bind("e", json!([{"action":["EditScrollback"]},{"mode":"locked"}])),
            bind("/", json!([{"action":["SearchInput","0"]},{"mode":"entersearch"}])),
            bind("Esc", exit()), bind("q", exit())
        ]}),
        json!({"mode":"entersearch","clear":true,"binds":[
            bind("Enter", json!([{"mode":"search"}])),
            bind("Esc", json!([{"mode":"scroll"}]))
        ]}),
        json!({"mode":"search","clear":true,"binds":[
            bind("n", json!([{"action":["Search","Down"]}])),
            bind("N", json!([{"action":["Search","Up"]}])),
            bind("Esc", exit()), bind("q", exit())
        ]}),
    ]
}

/// Zellij's stock scroll, enter-search, and search keys, with every exit landing
/// in locked, RimZ's home mode.
fn native_scroll_modes() -> Vec<Value> {
    let exit = || json!([{"action":["ScrollToBottom"]},{"mode":"locked"}]);
    let bind = |key: &str, steps: Value| json!({"key":key,"steps":steps});
    let motions = || {
        let mut binds = Vec::new();
        for (keys, action) in [
            (&["j", "Down"][..], "ScrollDown"),
            (&["k", "Up"][..], "ScrollUp"),
            (&["Ctrl f", "PageDown", "Right", "l"][..], "PageScrollDown"),
            (&["Ctrl b", "PageUp", "Left", "h"][..], "PageScrollUp"),
            (&["d"][..], "HalfPageScrollDown"),
            (&["u"][..], "HalfPageScrollUp"),
        ] {
            binds.extend(
                keys.iter()
                    .map(|key| bind(key, json!([{"action":[action]}]))),
            );
        }
        binds
    };
    let exits = || ["Ctrl c", "Esc", "Enter"].map(|key| bind(key, exit()));
    let mut scroll = motions();
    scroll.push(bind(
        "e",
        json!([{"action":["EditScrollback"]},{"mode":"locked"}]),
    ));
    scroll.push(bind(
        "s",
        json!([{"action":["SearchInput","0"]},{"mode":"entersearch"}]),
    ));
    scroll.extend(exits());
    let mut search = motions();
    search.push(bind("n", json!([{"action":["Search","Down"]}])));
    search.push(bind("p", json!([{"action":["Search","Up"]}])));
    for (key, option) in [("c", "CaseSensitivity"), ("w", "Wrap"), ("o", "WholeWord")] {
        search.push(bind(key, json!([{"action":["SearchToggleOption",option]}])));
    }
    search.extend(exits());
    vec![
        json!({"mode":"scroll","clear":true,"binds":scroll}),
        json!({"mode":"entersearch","clear":true,"binds":[
            bind("Enter", json!([{"mode":"search"}])),
            bind("Esc", json!([{"mode":"scroll"}])),
            bind("Ctrl c", json!([{"mode":"scroll"}]))
        ]}),
        json!({"mode":"search","clear":true,"binds":search}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineConfig;
    use serde_json::{Value, json};

    #[test]
    fn every_named_action_compiles() {
        for name in KeyActionName::vocabulary() {
            let action = KeyAction::Named(KeyActionName::parse(name).expect(name));
            assert_eq!(action.to_string(), name);
            let prefix = Some(KeyChord::Ctrl('b'));
            let compiled = step(
                &action,
                prefix,
                "room",
                Path::new("/project"),
                &mut Vec::new(),
            );
            assert!(compiled.is_object(), "{name}: {compiled}");
        }
    }

    fn wire(config: &str) -> Value {
        let config: MachineConfig = toml::from_str(config).unwrap();
        let encoded = encode(&config.room_keymap(), "room", Path::new("/project,=%2C"));
        assert!(!encoded.contains(','));
        serde_json::from_str(&encoded.replace("%2C", ",").replace("%25", "%")).unwrap()
    }

    #[test]
    fn native_wire_keeps_stock_modes_and_pins_complete_commands() {
        let wire = wire("");
        assert_eq!(wire["report_modes"], false);
        assert_eq!(
            wire["commands"],
            json!([
                [
                    "--mux",
                    "zellij",
                    "--root",
                    "/project,=%2C",
                    "pane",
                    "zoom",
                    "--session-name",
                    "room"
                ],
                [
                    "--mux",
                    "zellij",
                    "--root",
                    "/project,=%2C",
                    "sidebar",
                    "focus",
                    "--toggle",
                    "--session-name",
                    "room"
                ]
            ])
        );
        for (index, mode) in ["locked", "normal"].into_iter().enumerate() {
            assert_eq!(wire["modes"][index]["mode"], mode);
            assert_eq!(wire["modes"][index]["clear"], false);
            assert_eq!(
                wire["modes"][index]["binds"],
                json!([
                    {"key":"Alt g", "steps":[{"run":0}]},
                    {"key":"Alt p", "steps":[{"run":1}]}
                ])
            );
        }
    }

    fn steps(wire: &Value, mode: &str, key: &str) -> Value {
        let bind = wire["modes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["mode"] == mode)
            .flat_map(|entry| entry["binds"].as_array().unwrap())
            .find(|bind| bind["key"] == key);
        assert!(bind.is_some(), "{mode} {key} must be bound");
        bind.unwrap()["steps"].clone()
    }

    #[test]
    fn sticky_modes_stay_only_after_a_motion_and_prefix_stays_one_shot() {
        let wire = wire(
            "[room]\nstyle='rimz'\n[keys.bind.pane]\nh='pane.focus.left'\nz='pane.zoom'\n\
             [keys.bind.tab]\nn='tab.next'\nc='tab.new'\n[keys.bind.resize]\nz='pane.zoom'\n\
             [keys.bind.move]\nz='pane.zoom'\n[keys.bind.prefix]\np='mode.pane'\n",
        );
        assert_eq!(
            steps(&wire, "pane", "h"),
            json!([{"action":["MoveFocus","Left"]}])
        );
        assert_eq!(
            steps(&wire, "tab", "n"),
            json!([{"action":["GoToNextTab"]}])
        );
        assert_eq!(
            steps(&wire, "move", "h"),
            json!([{"guard":{"action":["MovePane","Left"]}}])
        );
        assert_eq!(
            steps(&wire, "tab", "c"),
            json!([{"action":["NewTab"]},{"mode":"locked"}])
        );
        for mode in ["pane", "resize", "move"] {
            let steps = steps(&wire, mode, "z");
            assert_eq!(
                steps[1],
                json!({"mode":"locked"}),
                "{mode} z must return to normal"
            );
        }
        assert_eq!(steps(&wire, "tmux", "p"), json!([{"mode":"pane"}]));
        assert_eq!(
            steps(&wire, "tmux", "h"),
            json!([{"action":["MoveFocus","Left"]},{"mode":"locked"}])
        );
    }

    #[test]
    fn split_close_and_swap_compile_to_guarded_steps_naming_the_sidebar() {
        let wire = wire(
            "[room]\nstyle='rimz'\n[keys.bind.pane]\nx='pane.close'\nd='pane.split.down'\n\
             r='pane.split.right'\n[keys.bind.move]\nl='pane.swap.right'\n",
        );
        assert_eq!(wire["sidebar"], crate::pane::SIDEBAR_CHROME_TITLE);
        assert_eq!(
            steps(&wire, "pane", "x"),
            json!([{"guard":{"action":["CloseFocus"]}},{"mode":"locked"}])
        );
        assert_eq!(
            steps(&wire, "move", "l"),
            json!([{"guard":{"action":["MovePane","Right"]}}])
        );
        for (key, side, word) in [("d", "down", "Down"), ("r", "right", "Right")] {
            let steps = steps(&wire, "pane", key);
            let index = steps[0]["guard"]["run"].as_u64().unwrap() as usize;
            assert_eq!(
                steps[0]["guard"],
                json!({"action":["NewPane", word],"run":index})
            );
            assert_eq!(
                wire["commands"][index],
                json!([
                    "--mux",
                    "zellij",
                    "--root",
                    "/project,=%2C",
                    "sidebar",
                    "split",
                    side,
                    "--session-name",
                    "room"
                ])
            );
        }
    }

    #[test]
    fn send_prefix_focus_next_and_arrow_keys_encode() {
        for (prefix, bytes) in [
            ("Ctrl+b", json!(["Write", "2"])),
            ("Alt+w", json!(["Write", "27", "119"])),
        ] {
            let wire = wire(&format!(
                "[room]\nstyle='rimz'\n[keys]\nprefix='{prefix}'\n[keys.bind.prefix]\n\
                 b='pane.send_prefix'\no='pane.focus.next'\nLeft='pane.focus.left'\n\
                 'Alt+Up'='pane.focus.up'\n'Ctrl+Down'='pane.focus.down'\n"
            ));
            assert_eq!(
                steps(&wire, "tmux", "b")[0],
                json!({"action":bytes}),
                "{prefix}"
            );
            assert_eq!(
                steps(&wire, "tmux", "o")[0],
                json!({"action":["FocusNextPane"]})
            );
            assert_eq!(
                steps(&wire, "tmux", "Left")[0],
                json!({"action":["MoveFocus","Left"]})
            );
            assert_eq!(
                steps(&wire, "tmux", "Alt Up")[0],
                json!({"action":["MoveFocus","Up"]})
            );
            assert_eq!(
                steps(&wire, "tmux", "Ctrl Down")[0],
                json!({"action":["MoveFocus","Down"]})
            );
        }
    }

    fn scroll_trio(wire: &Value) -> Vec<Value> {
        wire["modes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|mode| {
                matches!(
                    mode["mode"].as_str(),
                    Some("scroll" | "entersearch" | "search")
                )
            })
            .cloned()
            .collect()
    }

    #[test]
    fn vi_scrollback_installs_the_rimz_scroll_tables() {
        assert_eq!(scroll_trio(&wire("[room]\nstyle='rimz'")), scroll_modes());
    }

    #[test]
    fn native_scrollback_installs_zellij_stock_keys_exiting_to_locked() {
        let config: MachineConfig = toml::from_str("[room]\nstyle='rimz'").unwrap();
        let mut keymap = config.room_keymap();
        keymap.style.as_mut().unwrap().scrollback = KeyScrollback::Native;
        let encoded = encode(&keymap, "room", Path::new("/project"));
        let wire: Value =
            serde_json::from_str(&encoded.replace("%2C", ",").replace("%25", "%")).unwrap();
        let rows = |table: &str| -> Vec<Value> {
            table
                .lines()
                .map(|row| {
                    let (key, steps) = row.split_once(" => ").unwrap();
                    let steps: Vec<_> = steps
                        .split(" ; ")
                        .map(|step| match step.strip_prefix("mode ") {
                            Some(mode) => json!({"mode":mode}),
                            None => json!({"action":step.split(' ').collect::<Vec<_>>()}),
                        })
                        .collect();
                    json!({"key":key,"steps":steps})
                })
                .collect()
        };
        let motions = concat!(
            "j => ScrollDown\nDown => ScrollDown\nk => ScrollUp\nUp => ScrollUp\n",
            "Ctrl f => PageScrollDown\nPageDown => PageScrollDown\nRight => PageScrollDown\n",
            "l => PageScrollDown\nCtrl b => PageScrollUp\nPageUp => PageScrollUp\n",
            "Left => PageScrollUp\nh => PageScrollUp\nd => HalfPageScrollDown\n",
            "u => HalfPageScrollUp\n"
        );
        let exits = concat!(
            "Ctrl c => ScrollToBottom ; mode locked\nEsc => ScrollToBottom ; mode locked\n",
            "Enter => ScrollToBottom ; mode locked"
        );
        let scroll = format!(
            "{motions}e => EditScrollback ; mode locked\n\
             s => SearchInput 0 ; mode entersearch\n{exits}"
        );
        let search = format!(
            "{motions}n => Search Down\np => Search Up\nc => SearchToggleOption CaseSensitivity\n\
             w => SearchToggleOption Wrap\no => SearchToggleOption WholeWord\n{exits}"
        );
        let entersearch = "Enter => mode search\nEsc => mode scroll\nCtrl c => mode scroll";
        assert_eq!(
            scroll_trio(&wire),
            [
                json!({"mode":"scroll","clear":true,"binds":rows(&scroll)}),
                json!({"mode":"entersearch","clear":true,"binds":rows(entersearch)}),
                json!({"mode":"search","clear":true,"binds":rows(&search)}),
            ]
        );
    }

    /// Each bundled keymap's wire is checked in, so the presence plugin's own
    /// tests can prove its renderer keeps every bind. After a deliberate change,
    /// rewrite them with `RIMZ_KEYMAP_FIXTURES=update cargo nextest run -p rimz
    /// --lib --all-features -E 'test(=<this test>)'`; `cargo xtask test` strips
    /// the variable.
    #[test]
    fn bundled_keymap_wires_match_their_plugin_fixtures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zellij-keymaps");
        for keymap in ["rimz", "tmux", "zellij", "vim"] {
            let wire = wire(&format!(
                "[room]\nstyle='rimz'\n[keys]\nkeymap='{keymap}'\n"
            ));
            let path = dir.join(format!("{keymap}.json"));
            if std::env::var("RIMZ_KEYMAP_FIXTURES").as_deref() == Ok("update") {
                std::fs::write(&path, serde_json::to_string_pretty(&wire).unwrap() + "\n").unwrap();
            }
            let fixture = std::fs::read_to_string(&path).unwrap_or_default();
            let fixture: Value = serde_json::from_str(&fixture).unwrap_or_default();
            assert_eq!(
                wire,
                fixture,
                "{} is stale; rerun with RIMZ_KEYMAP_FIXTURES=update",
                path.display()
            );
        }
    }

    #[test]
    fn rimz_wire_clears_every_enterable_mode_exits_prefix_and_keeps_literal_run_arguments() {
        let wire = wire(
            "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+,\"={run=\"rimz message @coder -- 'a,%2C=word'\"}",
        );
        assert_eq!(wire["report_modes"], true);
        let modes = wire["modes"].as_array().expect("mode array");
        let words: Vec<_> = modes
            .iter()
            .map(|mode| mode["mode"].as_str().unwrap())
            .collect();
        assert_eq!(
            words,
            [
                "locked",
                "tmux",
                "pane",
                "tab",
                "resize",
                "move",
                "scroll",
                "entersearch",
                "search"
            ]
        );
        assert!(modes.iter().all(|mode| mode["clear"] == true));
        let normal = &modes[0];
        assert_eq!(normal["mode"], "locked");
        let binds = normal["binds"].as_array().unwrap();
        let custom = binds.iter().find(|bind| bind["key"] == "Alt ,").unwrap();
        let index = custom["steps"][0]["run"].as_u64().unwrap() as usize;
        assert_eq!(
            wire["commands"][index],
            json!([
                "--mux",
                "zellij",
                "--root",
                "/project,=%2C",
                "message",
                "@coder",
                "--",
                "a,%2C=word"
            ])
        );
        let prefix = modes.iter().find(|mode| mode["mode"] == "tmux").unwrap();
        let binds = prefix["binds"].as_array().unwrap();
        assert!(binds.iter().any(|bind| bind["key"] == "h"
            && bind["steps"] == json!([{"action":["MoveFocus","Left"]},{"mode":"locked"}])));
        assert!(
            binds
                .iter()
                .any(|bind| bind["key"] == "r" && bind["steps"] == json!([{"mode":"resize"}]))
        );
        assert!(binds.iter().any(|bind| bind["key"] == "p"
            && bind["steps"] == json!([{"palette":"commands"},{"mode":"locked"}])));
        assert!(
            binds
                .iter()
                .any(|bind| bind["key"] == "?" && bind["steps"][0].get("float").is_some())
        );
        let help = binds.iter().find(|bind| bind["key"] == "?").unwrap();
        let index = help["steps"][0]["float"].as_u64().unwrap() as usize;
        assert_eq!(
            wire["commands"][index],
            json!([
                "--mux",
                "zellij",
                "--root",
                "/project,=%2C",
                "keys",
                "--wait"
            ])
        );
        let resize = modes.iter().find(|mode| mode["mode"] == "resize").unwrap();
        assert!(
            resize["binds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|bind| bind["key"] == "h"
                    && bind["steps"] == json!([{"action":["Resize","Increase Left"]}]))
        );
        for mode in ["scroll", "entersearch", "search"] {
            assert!(modes.iter().any(
                |entry| entry["mode"] == mode && !entry["binds"].as_array().unwrap().is_empty()
            ));
        }
    }
}
