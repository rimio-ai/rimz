use std::path::Path;

use crate::config::keys::{
    KeyAction, KeyActionName, KeyChord, KeyDirection, KeyMode, KeySplitDirection, RoomKeymap,
};
use serde_json::{Value, json};

pub(super) fn encode(keymap: &RoomKeymap, session: &str, root: &Path) -> String {
    let rimz = keymap.modes.contains_key(&KeyMode::Prefix);
    let mut commands = Vec::new();
    let mut modes = Vec::new();
    for (&mode, bindings) in &keymap.modes {
        let binds: Vec<_> = bindings
            .iter()
            .map(|(&key, action)| {
                let mut steps = vec![step(action, session, root, &mut commands)];
                let changes_mode = matches!(action, KeyAction::Named(name) if name.changes_mode());
                if mode == KeyMode::Prefix && !changes_mode {
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
    if rimz {
        modes.extend(scroll_modes());
    }
    json!({"report_modes":rimz, "commands":commands, "modes":modes})
        .to_string()
        .replace('%', "%25")
        .replace(',', "%2C")
}

fn mode_word(mode: KeyMode) -> &'static str {
    match mode {
        KeyMode::Normal => "locked",
        KeyMode::Prefix => "tmux",
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
    }
}

fn step(action: &KeyAction, session: &str, root: &Path, commands: &mut Vec<Vec<String>>) -> Value {
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
        KeyActionName::PaneFocus(direction) => &["MoveFocus", direction_word(direction)],
        KeyActionName::PaneSwap(direction) => &["MovePane", direction_word(direction)],
        KeyActionName::PaneSplit(direction) => &[
            "NewPane",
            match direction {
                KeySplitDirection::Right => "Right",
                KeySplitDirection::Down => "Down",
            },
        ],
        KeyActionName::PaneResize(direction) => &[
            "Resize",
            match direction {
                KeyDirection::Left => "Increase Left",
                KeyDirection::Right => "Increase Right",
                KeyDirection::Up => "Increase Up",
                KeyDirection::Down => "Increase Down",
            },
        ],
        KeyActionName::PaneClose => &["CloseFocus"],
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
            let compiled = step(&action, "room", Path::new("/project"), &mut Vec::new());
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

    #[test]
    fn rimz_wire_clears_seven_modes_exits_prefix_and_keeps_literal_run_arguments() {
        let wire = wire(
            "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+,\"={run=\"rimz message @coder -- 'a,%2C=word'\"}",
        );
        assert_eq!(wire["report_modes"], true);
        let modes = wire["modes"].as_array().expect("mode array");
        assert_eq!(modes.len(), 7);
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
