use serde::Deserialize;
use serde_json::Value;

use super::kdl_string;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Keymap {
    pub report_modes: bool,
    commands: Vec<Vec<String>>,
    modes: Vec<Value>,
}

impl Keymap {
    pub fn decode(encoded: &str) -> Option<Self> {
        serde_json::from_str(&encoded.replace("%2C", ",").replace("%25", "%")).ok()
    }

    pub(super) fn kdl(&self, plugin_id: u32, rimz_bin: &str) -> String {
        let mut modes = String::new();
        for raw in &self.modes {
            let Ok(mode) = serde_json::from_value::<Mode>(raw.clone()) else {
                continue;
            };
            if !valid_mode(&mode.mode) {
                continue;
            }
            let clear = if mode.clear {
                " clear-defaults=true"
            } else {
                ""
            };
            modes.push_str(&format!("    {}{clear} {{\n", mode.mode));
            for raw in mode.binds {
                let Ok(bind) = serde_json::from_value::<Bind>(raw) else {
                    continue;
                };
                if !valid_key(&bind.key) || bind.steps.is_empty() {
                    continue;
                }
                let Some(steps) = bind
                    .steps
                    .iter()
                    .map(|step| self.step_kdl(step, plugin_id, rimz_bin))
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                modes.push_str(&format!(
                    "        bind {} {{\n            {}\n        }}\n",
                    kdl_string(&bind.key),
                    steps.join("\n            ")
                ));
            }
            modes.push_str("    }\n");
        }
        if modes.is_empty() {
            return String::new();
        }
        format!("keybinds {{\n{modes}}}\n")
    }

    pub(crate) fn command_argv(
        &self,
        payload: Option<&str>,
        rimz_bin: Option<&str>,
    ) -> Option<Vec<String>> {
        let index = payload?.parse::<usize>().ok()?;
        let args = self.commands.get(index)?;
        Some(
            std::iter::once(rimz_bin.unwrap_or("rimz").to_owned())
                .chain(args.iter().cloned())
                .collect(),
        )
    }

    fn step_kdl(&self, step: &Step, plugin_id: u32, rimz_bin: &str) -> Option<String> {
        match step {
            Step::Run(index) => {
                self.commands.get(*index)?;
                Some(format!(
                    "MessagePluginId {plugin_id} {{ name \"rimz:key\"; payload \"{index}\"; }}"
                ))
            }
            Step::Palette(mode) if matches!(mode.as_str(), "cards" | "commands" | "new") => {
                Some(format!(
                    "MessagePluginId {plugin_id} {{ name \"rimz:palette\"; payload {}; }}",
                    kdl_string(mode)
                ))
            }
            Step::Mode(mode) if valid_mode(mode) => {
                Some(format!("SwitchToMode {};", kdl_string(mode)))
            }
            Step::Action(args) => action_kdl(args),
            Step::Float(index) => {
                let args = self.commands.get(*index)?;
                let command = std::iter::once(rimz_bin)
                    .chain(args.iter().map(String::as_str))
                    .map(kdl_string)
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(format!(
                    "Run {command} {{ floating true; close_on_exit true; \
                     x \"10%\"; y \"10%\"; width \"80%\"; height \"80%\"; }}"
                ))
            }
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mode {
    mode: String,
    clear: bool,
    binds: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    key: String,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    Run(usize),
    Action(Vec<String>),
    Mode(String),
    Palette(String),
    Float(usize),
}

fn valid_mode(mode: &str) -> bool {
    matches!(
        mode,
        "normal" | "locked" | "tmux" | "resize" | "move" | "scroll" | "entersearch" | "search"
    )
}

fn valid_key(key: &str) -> bool {
    if matches!(key, "Esc" | "Enter") {
        return true;
    }
    let key = key
        .strip_prefix("Alt ")
        .or_else(|| key.strip_prefix("Ctrl "))
        .unwrap_or(key);
    key.len() == 1 && key.as_bytes()[0].is_ascii_graphic()
}

fn action_kdl(args: &[String]) -> Option<String> {
    let valid = match args {
        [name] => matches!(
            name.as_str(),
            "CloseFocus"
                | "NewTab"
                | "GoToNextTab"
                | "GoToPreviousTab"
                | "Detach"
                | "ScrollDown"
                | "ScrollUp"
                | "HalfPageScrollDown"
                | "HalfPageScrollUp"
                | "ScrollToTop"
                | "ScrollToBottom"
                | "EditScrollback"
        ),
        [name, arg] => match name.as_str() {
            "MoveFocus" | "MovePane" => matches!(arg.as_str(), "Left" | "Right" | "Up" | "Down"),
            "Resize" => matches!(
                arg.as_str(),
                "Increase Left" | "Increase Right" | "Increase Up" | "Increase Down"
            ),
            "NewPane" => matches!(arg.as_str(), "Right" | "Down"),
            "Search" => matches!(arg.as_str(), "Up" | "Down"),
            "SearchInput" => arg == "0",
            _ => false,
        },
        _ => false,
    };
    if !valid {
        return None;
    }
    if args[0] == "SearchInput" {
        return Some("SearchInput 0;".into());
    }
    let suffix = args[1..]
        .iter()
        .map(|arg| format!(" {}", kdl_string(arg)))
        .collect::<String>();
    Some(format!("{}{suffix};", args[0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(modes: Value) -> Keymap {
        serde_json::from_value(json!({
            "report_modes": false,
            "commands": [["message", "@coder", "a,%2C=word"], ["keys", "--wait"]],
            "modes": modes
        }))
        .unwrap()
    }

    #[test]
    fn encoded_wire_decodes_commas_without_decoding_literal_percent_sequences_twice() {
        let json = json!({
            "report_modes":false,
            "commands":[["message","a,%2C=word"]],
            "modes":[{"mode":"locked","clear":false,"binds":[
                {"key":"Alt ,","steps":[{"run":0}]}
            ]}]
        })
        .to_string();
        let encoded = json.replace('%', "%25").replace(',', "%2C");
        let decoded = Keymap::decode(&encoded);
        assert!(decoded.is_some(), "the host's encoded keymap must decode");
        let decoded = decoded.unwrap();
        assert_eq!(decoded.commands[0][1], "a,%2C=word");
        assert!(decoded.kdl(42, "/bin/rimz").contains("bind \"Alt ,\""));
    }

    #[test]
    fn renders_allowlisted_steps_and_quotes_commands_without_raw_kdl() {
        let map = map(json!([{"mode":"locked","clear":true,"binds":[
            {"key":"Alt j","steps":[{"run":0}]},
            {"key":"Alt h","steps":[{"action":["MoveFocus","Left"]}]},
            {"key":"Alt n","steps":[{"palette":"new"}]},
            {"key":"Alt p","steps":[{"mode":"tmux"}]},
            {"key":"Alt ?","steps":[{"float":1}]}
        ]}]));
        let kdl = map.kdl(42, "/bin/a\"\\rimz");
        assert!(kdl.contains("locked clear-defaults=true {"), "{kdl}");
        assert!(
            kdl.contains("MessagePluginId 42 { name \"rimz:key\"; payload \"0\"; }"),
            "{kdl}"
        );
        assert!(kdl.contains("MoveFocus \"Left\";"));
        assert!(kdl.contains("name \"rimz:palette\"; payload \"new\";"));
        assert!(kdl.contains("SwitchToMode \"tmux\";"));
        assert!(kdl.contains("Run \"/bin/a\\\"\\\\rimz\" \"keys\" \"--wait\""));
        assert!(kdl.contains(concat!(
            "floating true; close_on_exit true; x \"10%\"; y \"10%\"; ",
            "width \"80%\"; height \"80%\";"
        )));
    }

    #[test]
    fn bad_modes_keys_actions_and_command_indices_drop_only_the_bad_bind() {
        let map = map(json!([
            {"mode":"locked","clear":false,"binds":[
                {"key":"Alt j","steps":[{"run":0}]},
                {"key":"Alt bad","steps":[{"run":0}]},
                {"key":"Alt x","steps":[{"action":["Run","sh"]}]},
                {"key":"Alt y","steps":[{"mode":"bogus"}]},
                {"key":"Alt z","steps":[{"run":999}]},
                {"key":"Alt a","steps":[{"action":["MoveFocus","Left; Run sh"]}]},
                {"key":"Alt b","steps":[{"palette":"raw kdl"}]},
                {"key":"Alt c","steps":[{"run":0},{"action":["raw kdl"]}]},
                {"key":"Alt d","steps":"Run sh"},
                {"key":"Alt e","steps":[{"run":0}],"raw":"Run sh"}
            ]},
            {"mode":"bad { Run sh; }","clear":true,"binds":[]},
            {"mode":"resize","clear":true,"binds":[
                {"key":"h","steps":[{"action":["Resize","Increase Left"]}]}
            ]}
        ]));
        let kdl = map.kdl(42, "/bin/rimz");
        assert_eq!(kdl.matches("bind ").count(), 2, "{kdl}");
        assert!(kdl.contains("locked {"));
        assert!(kdl.contains("resize clear-defaults=true {"));
        assert!(!kdl.contains("raw kdl"));
        assert!(!kdl.contains("Run sh"));
    }

    #[test]
    fn command_payload_selects_only_an_index_into_complete_argv() {
        let map = map(json!([]));
        assert_eq!(
            map.command_argv(Some("0"), Some("/bin/rimz")),
            Some(
                vec!["/bin/rimz", "message", "@coder", "a,%2C=word"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            )
        );
        assert_eq!(
            map.command_argv(Some("1"), None),
            Some(
                vec!["rimz", "keys", "--wait"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            )
        );
        for payload in [None, Some("-1"), Some("99"), Some("0; sh"), Some("Run sh")] {
            assert!(map.command_argv(payload, None).is_none());
        }
    }

    #[test]
    fn key_labels_admit_graphic_and_named_keys_but_not_raw_chord_grammar() {
        for key in [
            "Alt p", "Ctrl S", "Alt j", "Alt `", "Alt ,", "Alt =", "Esc", "Enter", "p",
        ] {
            let map = map(json!([{"mode":"tmux","clear":true,"binds":[
                {"key":key,"steps":[{"mode":"locked"}]}
            ]}]));
            let kdl = map.kdl(42, "/bin/rimz");
            assert!(kdl.contains("bind "), "{key}: {kdl}");
        }
        for key in [
            "Super p",
            "Alt pp",
            "Alt ",
            "",
            "Alt+p",
            "ctrl-S",
            "  m+j  ",
            "Ctrl Alt p",
        ] {
            let map = map(json!([{"mode":"tmux","clear":true,"binds":[
                {"key":key,"steps":[{"mode":"locked"}]}
            ]}]));
            let kdl = map.kdl(42, "/bin/rimz");
            assert!(!kdl.contains("bind "), "{key}: {kdl}");
        }
    }
}
