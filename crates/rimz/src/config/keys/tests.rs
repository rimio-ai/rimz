use super::*;
use std::path::Path;

fn config(text: &str) -> MachineConfig {
    MachineConfig::parse_text(
        Path::new("config.toml"),
        text,
        Path::new("missing-definitions"),
    )
    .unwrap()
}

fn action(map: &RoomKeymap, mode: KeyMode, key: &str) -> Option<String> {
    let chord = KeyChord::parse(key);
    assert!(chord.is_some(), "key grammar must accept {key}");
    map.modes
        .get(&mode)
        .and_then(|bindings| bindings.get(&chord.unwrap()))
        .map(ToString::to_string)
}

#[test]
fn key_grammar_preserves_case_and_terminal_aliases() {
    for (raw, canonical) in [
        ("Alt+p", "Alt+p"),
        ("ctrl-s", "Ctrl+s"),
        ("M-0", "Alt+0"),
        ("Alt+`", "Alt+`"),
        ("meta+J", "Alt+J"),
        (" control - ? ", "Ctrl+?"),
        ("j", "j"),
        ("J", "J"),
        ("Esc", "Esc"),
        ("Enter", "Enter"),
        ("Alt+,", "Alt+,"),
        ("Alt+=", "Alt+="),
        ("Alt++", "Alt++"),
        ("-", "-"),
    ] {
        let chord = KeyChord::parse(raw);
        assert!(chord.is_some(), "{raw} must parse");
        assert_eq!(chord.unwrap().to_string(), canonical, "{raw}");
    }
    assert_ne!(KeyChord::parse("Alt+J"), KeyChord::parse("Alt+j"));
    for raw in [
        "",
        "Super+p",
        "Alt+Space",
        "Alt+",
        "Space",
        "Ctrl+Alt+p",
        "é",
        "Alt+Enter",
    ] {
        assert_eq!(KeyChord::parse(raw), None, "{raw}");
    }
    for code in b'!'..=b'~' {
        for raw in [
            char::from(code).to_string(),
            format!("Alt+{}", char::from(code)),
            format!("Ctrl+{}", char::from(code)),
        ] {
            assert!(KeyChord::parse(&raw).is_some(), "{raw}");
        }
    }
}

#[test]
fn embedded_keymap_matches_every_spec_binding() {
    let machine = config("[room]\nstyle = 'rimz'\n");
    assert!(
        machine.notices.key_bindings.is_empty(),
        "{:?}",
        machine.notices
    );
    let map = machine.room_keymap();
    let expected = [
        (
            KeyMode::Normal,
            concat!(
                "Alt+j card.next\nAlt+k card.prev\nAlt+J worktree.next\nAlt+K worktree.prev\n",
                "Alt+h pane.focus.left\nAlt+l pane.focus.right\nAlt+? filter.needs_you\n",
                "Alt+n palette.new_agent\nAlt+t tab.new\nAlt+1 card.slot.1\nAlt+2 card.slot.2\n",
                "Alt+3 card.slot.3\nAlt+4 card.slot.4\nAlt+5 card.slot.5\nAlt+6 card.slot.6\n",
                "Alt+7 card.slot.7\nAlt+8 card.slot.8\nAlt+9 card.slot.9\nAlt+; card.last\nAlt+p mode.prefix"
            ),
        ),
        (
            KeyMode::Prefix,
            concat!(
                "z pane.zoom\np palette.commands\nAlt+p palette.commands\n/ palette.cards\n",
                "s pane.split.down\nv pane.split.right\nh pane.focus.left\nj pane.focus.down\n",
                "k pane.focus.up\nl pane.focus.right\nH pane.swap.left\nJ pane.swap.down\n",
                "K pane.swap.up\nL pane.swap.right\nr mode.resize\n[ mode.scrollback\n",
                ", tab.prev\n. tab.next\nx card.close\nX pane.close\nu card.reopen\n",
                "d session.detach\n? keys.help\nEsc mode.exit"
            ),
        ),
        (
            KeyMode::Resize,
            "h pane.resize.left\nj pane.resize.down\nk pane.resize.up\nl pane.resize.right\nEsc mode.exit\nEnter mode.exit",
        ),
        (
            KeyMode::Move,
            "h pane.swap.left\nj pane.swap.down\nk pane.swap.up\nl pane.swap.right\nEsc mode.exit\nEnter mode.exit",
        ),
    ];
    assert_eq!(
        map.modes.len(),
        expected.len(),
        "rimz must resolve all four data modes"
    );
    for (mode, rows) in expected {
        assert_eq!(map.modes[&mode].len(), rows.lines().count(), "{mode:?}");
        for row in rows.lines() {
            let (key, expected) = row.split_once(' ').unwrap();
            assert_eq!(
                action(&map, mode, key).as_deref(),
                Some(expected),
                "{mode:?} {key}"
            );
        }
    }
    assert!(
        !map.modes
            .values()
            .flat_map(BTreeMap::values)
            .any(|action| action.to_string() == "sidebar.focus")
    );
}

#[test]
fn overlays_remove_bindings_and_prefix_moves_both_symbolic_entries_last() {
    let machine = config(
        "[room]\nstyle='rimz'\n[keys]\nprefix='Ctrl+a'\n\
         [keys.bind.normal]\n'Alt+t'='none'\n'Ctrl+a'='pane.close'\n'Alt+x'='sidebar.focus'\n\
         [keys.bind.prefix]\n'z'={run='rimz message @coder \"two words\"'}\n'Ctrl+a'='pane.close'\n",
    );
    assert!(machine.notices.key_bindings.is_empty());
    let map = machine.room_keymap();
    assert!(
        map.modes.contains_key(&KeyMode::Prefix),
        "resolved overrides must retain prefix mode"
    );
    assert_eq!(action(&map, KeyMode::Normal, "Alt+p"), None);
    assert_eq!(action(&map, KeyMode::Prefix, "Alt+p"), None);
    assert_eq!(
        action(&map, KeyMode::Normal, "Ctrl+a").as_deref(),
        Some("mode.prefix")
    );
    assert_eq!(
        action(&map, KeyMode::Prefix, "Ctrl+a").as_deref(),
        Some("palette.commands")
    );
    assert_eq!(action(&map, KeyMode::Normal, "Alt+t"), None);
    assert_eq!(
        action(&map, KeyMode::Normal, "Alt+x").as_deref(),
        Some("sidebar.focus")
    );
    assert_eq!(
        map.modes[&KeyMode::Prefix][&KeyChord::parse("z").unwrap()],
        KeyAction::Run {
            args: vec!["message".into(), "@coder".into(), "two words".into()]
        }
    );
    assert_eq!(
        action(&map, KeyMode::Prefix, "p").as_deref(),
        Some("palette.commands")
    );
}

#[test]
fn invalid_bindings_drop_only_themselves_under_strict_and_lenient_loads() {
    for (binding, reason) in [
        ("'Alt+x'=42", "action name or { run"),
        ("'Alt+x'={run='rimz ping', typo=true}", "only `run`"),
        ("'Super+x'='card.close'", "Alt+x"),
        ("'Alt+x'='not.an.action'", "unknown action"),
        ("'x'='card.close'", "modifier"),
        ("'Esc'='mode.exit'", "modifier"),
        ("'Enter'='mode.exit'", "modifier"),
        ("'Alt+x'={run='echo no'}", "start with `rimz`"),
        ("'Alt+x'={run='rimz \"unclosed'}", "quot"),
        ("'Alt+x'={run=42}", "string"),
        ("'Alt+x'=[true]", "action name or { run"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let definitions = dir.path().join("definitions");
        std::fs::write(&path, format!(
            "timezone='UTC'\n[room]\nstyle='rimz'\n[keys.bind.normal]\n{binding}\n'Alt+y'='card.reopen'\n",
        )).unwrap();
        let strict = MachineConfig::load_from(&path, &definitions).unwrap();
        let lenient = MachineConfig::load_lenient_from(&path, &definitions, &BTreeMap::new());
        for machine in [strict, lenient] {
            assert_eq!(machine.timezone.as_deref(), Some("UTC"));
            assert!(machine.notices.unreadable_files.is_empty());
            assert_eq!(
                machine.notices.key_bindings.len(),
                1,
                "{binding}: {:?}",
                machine.notices
            );
            let notice = machine.notices.key_bindings.values().next().unwrap();
            assert!(notice.contains(reason), "{notice}");
            assert!(notice.contains("keys.bind.normal."), "{notice}");
            assert!(notice.contains("binding ignored"), "{notice}");
            let map = machine.room_keymap();
            assert_eq!(action(&map, KeyMode::Normal, "Alt+x"), None);
            assert_eq!(
                action(&map, KeyMode::Normal, "Alt+y").as_deref(),
                Some("card.reopen")
            );
        }
    }
}

#[test]
fn bad_prefix_and_unknown_keymap_warn_and_use_defaults() {
    for prefix in ["'x'", "'Esc'", "'Alt+Space'", "'Ctrl+m'", "42"] {
        let machine = config(&format!("[room]\nstyle='rimz'\n[keys]\nprefix={prefix}\n"));
        assert_eq!(
            machine.notices.key_bindings.len(),
            1,
            "bad prefix must warn"
        );
        assert!(machine.notices.key_bindings["keys.prefix"].contains("Alt+p"));
        assert_eq!(
            action(&machine.room_keymap(), KeyMode::Normal, "Alt+p").as_deref(),
            Some("mode.prefix")
        );
    }
    let machine = config("[room]\nstyle='rimz'\n[keys]\nkeymap='unknown'\n");
    assert!(machine.notices.key_bindings["keys.keymap"].contains("rimz"));
    assert_eq!(
        action(&machine.room_keymap(), KeyMode::Normal, "Alt+j").as_deref(),
        Some("card.next")
    );
}

#[test]
fn terminal_control_aliases_warn_without_losing_other_bindings() {
    for key in ["h", "H", "i", "I", "j", "J", "m", "M", "[", "?", "3", "8"] {
        for mode in ["normal", "prefix"] {
            let machine = config(&format!(
                "[room]\nstyle='rimz'\n[keys.bind.{mode}]\n'Ctrl+{key}'='card.close'\n'Alt+y'='card.reopen'"
            ));
            assert_eq!(machine.notices.key_bindings.len(), 1, "{mode}: Ctrl+{key}");
            let notice = machine.notices.key_bindings.values().next().unwrap();
            assert!(notice.contains("terminal"), "{notice}");
            let mode = KeyMode::parse(mode).unwrap();
            let map = machine.room_keymap();
            assert_eq!(action(&map, mode, &format!("Ctrl+{key}")), None);
            assert_eq!(action(&map, mode, "Alt+y").as_deref(), Some("card.reopen"));
        }
    }
}

#[test]
fn native_ignores_keys_and_retains_only_valid_modified_sidebar_chords() {
    let machine = config("[keys]\nkeymap=42\nprefix='bad'\n[keys.bind.normal]\nx=42\n");
    assert!(machine.notices.key_bindings.is_empty());
    assert!(machine.notices.unknown_keys.is_empty());
    let map = machine.room_keymap();
    assert_eq!(
        map.modes.len(),
        1,
        "native must resolve its normal-mode chords"
    );
    assert_eq!(map.modes[&KeyMode::Normal].len(), 2);
    assert_eq!(
        action(&map, KeyMode::Normal, "Alt+p").as_deref(),
        Some("sidebar.focus")
    );
    assert_eq!(
        action(&map, KeyMode::Normal, "Alt+g").as_deref(),
        Some("pane.zoom")
    );
    for raw in [
        "p",
        "Super+p",
        "Alt+Space",
        "Alt+",
        "Esc",
        "Enter",
        "off",
        "",
    ] {
        let machine = config(&format!("[sidebar]\nfocus_key={raw:?}\nzoom_key='off'\n"));
        assert!(
            machine.room_keymap().modes[&KeyMode::Normal].is_empty(),
            "{raw}"
        );
    }
}

#[test]
fn rimz_actions_carry_commands_and_typed_palette_modes() {
    for (name, args) in [
        ("card.next", vec!["sidebar", "walk", "next"]),
        ("card.prev", vec!["sidebar", "walk", "prev"]),
        (
            "worktree.next",
            vec!["sidebar", "walk", "next", "--worktree"],
        ),
        (
            "worktree.prev",
            vec!["sidebar", "walk", "prev", "--worktree"],
        ),
        ("card.last", vec!["sidebar", "last"]),
        (
            "filter.needs_you",
            vec!["sidebar", "filter", "needs-you", "--toggle"],
        ),
        ("card.close", vec!["sidebar", "close"]),
        ("card.reopen", vec!["sidebar", "reopen"]),
        ("sidebar.focus", vec!["sidebar", "focus", "--toggle"]),
        ("pane.zoom", vec!["pane", "zoom"]),
    ] {
        let Some(KeyActionName::Command {
            name: actual,
            args: actual_args,
        }) = KeyActionName::parse(name)
        else {
            panic!("{name} must carry command arguments");
        };
        assert_eq!(actual, name);
        assert_eq!(actual_args, args);
    }
    for slot in 1..=9 {
        let slot = slot.to_string();
        let action = KeyActionName::parse(&format!("card.slot.{slot}")).unwrap();
        assert!(matches!(action, KeyActionName::Command { args, .. }
            if args == ["sidebar", "slot", slot.as_str()]));
    }
    for (name, mode, word) in [
        ("palette.cards", KeyPaletteMode::Cards, "cards"),
        ("palette.commands", KeyPaletteMode::Commands, "commands"),
        ("palette.new_agent", KeyPaletteMode::NewAgent, "new"),
    ] {
        assert_eq!(
            KeyActionName::parse(name),
            Some(KeyActionName::Palette(mode))
        );
        assert_eq!(mode.command(), ["palette", "--popup", "--mode", word]);
    }
    assert_eq!(
        KeyActionName::parse("pane.focus.left"),
        Some(KeyActionName::PaneFocus(KeyDirection::Left))
    );
    assert_eq!(KeyActionName::parse("card.slot.0"), None);
    assert_eq!(KeyActionName::parse("card.slot.10"), None);
}
