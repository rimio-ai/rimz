use super::*;
use crate::config::MachineConfig;
use std::path::Path;

fn config(text: &str) -> MachineConfig {
    MachineConfig::parse_text(
        Path::new("config.toml"),
        text,
        Path::new("missing-definitions"),
    )
    .unwrap()
}

fn sequence(raw: &str) -> Vec<SidebarChord> {
    parse_sequence(raw).unwrap_or_else(|reason| panic!("{raw}: {reason}"))
}

fn action(map: &SidebarKeymap, raw: &str) -> SequenceMatch {
    map.lookup(&sequence(raw))
}

#[test]
fn every_action_has_one_name_and_a_label() {
    for (name, action, label) in ACTIONS {
        assert_eq!(SidebarAction::parse(name), Some(*action));
        assert_eq!(action.name(), *name);
        assert!(!label.is_empty(), "{name}");
    }
    let mut names = ACTIONS.iter().map(|(name, ..)| *name).collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), ACTIONS.len());
}

#[test]
fn defaults_bind_the_vocabulary_without_prefix_conflicts() {
    let map = SidebarKeymap::default();
    assert_eq!(map.bindings.len(), DEFAULTS.len());
    for (left, _) in &map.bindings {
        for (right, _) in &map.bindings {
            assert!(
                left == right || !right.starts_with(left),
                "{} is a prefix of {}",
                Sequence(left),
                Sequence(right)
            );
        }
    }
    for (raw, name) in [
        ("g g", "move.top"),
        ("M", "move.screen_middle"),
        ("Ctrl+d", "move.half_page_down"),
        ("c-u", "move.half_page_up"),
        ("pagedown", "move.page_down"),
        ("Tab", "card.last"),
        ("d d", "card.close"),
        ("U", "read.all"),
        ("f ?", "filter.asks"),
        ("f n", "filter.needs_you"),
        ("z R", "fold.open_all"),
        ("z i", "fold.inactive"),
        ("<", "width.narrower"),
        ("=", "width.reset"),
        ("Ctrl+l", "view.redraw"),
        ("Esc", "view.cancel"),
        ("Right", "dashboard.next"),
    ] {
        assert_eq!(
            action(&map, raw),
            SequenceMatch::Action(SidebarAction::parse(name).unwrap()),
            "{raw}"
        );
    }
    for prefix in ["g", "d", "f", "z"] {
        assert_eq!(action(&map, prefix), SequenceMatch::Pending, "{prefix}");
    }
    for unbound in ["Space", "x", "a", "g j", "Ctrl+o"] {
        assert_eq!(action(&map, unbound), SequenceMatch::Unbound, "{unbound}");
    }
}

#[test]
fn chords_share_the_room_aliases_and_keep_case() {
    for (raw, key, ctrl, alt) in [
        ("H", SidebarKey::Char('H'), false, false),
        ("h", SidebarKey::Char('h'), false, false),
        ("ctrl+f", SidebarKey::Char('f'), true, false),
        ("control-f", SidebarKey::Char('f'), true, false),
        ("M-v", SidebarKey::Char('v'), false, true),
        ("meta+>", SidebarKey::Char('>'), false, true),
        ("Ctrl+-", SidebarKey::Char('-'), true, false),
        ("Ctrl+Alt+x", SidebarKey::Char('x'), true, true),
        ("Ctrl+L", SidebarKey::Char('l'), true, false),
        ("Alt+L", SidebarKey::Char('L'), false, true),
        ("PageDown", SidebarKey::PageDown, false, false),
        ("space", SidebarKey::Char(' '), false, false),
        ("Alt+Up", SidebarKey::Up, false, true),
        ("Backspace", SidebarKey::Backspace, false, false),
    ] {
        assert_eq!(
            SidebarChord::parse(raw),
            Ok(SidebarChord { key, ctrl, alt }),
            "{raw}"
        );
    }
    for raw in ["ctrl", "ctrl+", "super+x", "ctrl+shift+x", "notakey", "é"] {
        assert!(SidebarChord::parse(raw).is_err(), "{raw}");
    }
    for raw in ["Ctrl+i", "Ctrl+m", "Ctrl+[", "Ctrl+h", "c-I"] {
        let reason = SidebarChord::parse(raw).unwrap_err();
        assert!(reason.contains("terminal"), "{raw}: {reason}");
    }
}

#[test]
fn user_sequences_override_defaults_and_none_unbinds() {
    let machine = config(
        "[keys.sidebar]\n'g x' = 'move.bottom'\n'Ctrl+d' = 'card.close'\n'x' = 'card.close'\n'd d' = 'none'\n'j' = 'none'\n",
    );
    assert!(
        machine.notices.key_bindings.is_empty(),
        "{:?}",
        machine.notices
    );
    let map = machine.sidebar_keymap();
    let close = SequenceMatch::Action(SidebarAction::CardClose);
    assert_eq!(action(&map, "g x"), SequenceMatch::Action(A::MoveBottom));
    assert_eq!(action(&map, "g g"), SequenceMatch::Action(A::MoveTop));
    assert_eq!(action(&map, "Ctrl+d"), close);
    assert_eq!(action(&map, "x"), close);
    assert_eq!(action(&map, "d d"), SequenceMatch::Unbound);
    assert_eq!(action(&map, "d"), SequenceMatch::Unbound);
    assert_eq!(action(&map, "j"), SequenceMatch::Unbound);
    assert_eq!(action(&map, "Down"), SequenceMatch::Action(A::MoveDown));
    assert_eq!(
        map.sequences(A::CardClose)
            .map(|sequence| Sequence(sequence).to_string())
            .collect::<Vec<_>>(),
        ["Ctrl+d", "x"]
    );
    assert_eq!(
        map.continuations(&sequence("g"))
            .map(|(rest, action)| (Sequence(rest).to_string(), action.label()))
            .collect::<Vec<_>>(),
        [("g".to_owned(), "top"), ("x".to_owned(), "bottom")]
    );
}

#[test]
fn invalid_entries_warn_with_dotted_paths_and_keep_the_rest() {
    for (entry, path, reason) in [
        (
            "'Ctrl+i' = 'move.down'",
            "keys.sidebar.\"Ctrl+i\"",
            "terminal",
        ),
        (
            "'Hyper+x' = 'move.down'",
            "keys.sidebar.\"Hyper+x\"",
            "not a key",
        ),
        (
            "'v' = 'move.sideways'",
            "keys.sidebar.\"v\"",
            "unknown action",
        ),
        ("'v' = 42", "keys.sidebar.\"v\"", "action name"),
        ("'' = 'move.down'", "keys.sidebar.\"\"", "key sequence"),
        (
            "'g' = 'move.top'",
            "keys.sidebar.\"g\"",
            "\"g\" is a prefix of the bound sequence \"g g\"",
        ),
        (
            "'G G' = 'move.top'",
            "keys.sidebar.\"G G\"",
            "\"G G\" extends the bound sequence \"G\"",
        ),
    ] {
        let machine = config(&format!("[keys.sidebar]\n{entry}\n'w' = 'help'\n"));
        let notices = &machine.notices.key_bindings;
        assert_eq!(notices.len(), 1, "{entry}: {notices:?}");
        let notice = &notices[path];
        assert!(notice.starts_with(path), "{notice}");
        assert!(notice.contains(reason), "{notice}");
        assert!(notice.ends_with("binding ignored"), "{notice}");
        let map = machine.sidebar_keymap();
        assert_eq!(action(&map, "w"), SequenceMatch::Action(A::Help), "{entry}");
        assert_eq!(action(&map, "g g"), SequenceMatch::Action(A::MoveTop));
        assert_eq!(action(&map, "G"), SequenceMatch::Action(A::MoveBottom));
    }
}

#[test]
fn in_pane_notices_show_under_native_and_rimz_styles() {
    for style in ["native", "rimz"] {
        let machine = config(&format!(
            "[room]\nstyle='{style}'\n[keys.sidebar]\n'Ctrl+i' = 'move.down'\n"
        ));
        assert!(
            machine
                .notices
                .key_bindings
                .contains_key("keys.sidebar.\"Ctrl+i\""),
            "{style}: {:?}",
            machine.notices
        );
    }
    let machine = config("[keys]\nsidebar = 'j'\n");
    assert!(machine.notices.key_bindings["keys.sidebar"].contains("table"));
    assert_eq!(machine.sidebar_keymap(), SidebarKeymap::default());
}

#[test]
fn the_removed_sidebar_keys_table_is_an_unknown_key() {
    for style in ["native", "rimz"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!("[room]\nstyle='{style}'\n[sidebar]\nkeys = {{ down = 'v' }}\n"),
        )
        .unwrap();
        let machine = MachineConfig::load_lenient_from(
            &path,
            &dir.path().join("definitions"),
            &BTreeMap::new(),
        );
        assert!(
            machine.notices.unknown_keys.iter().any(|notice| notice
                .key
                .split('.')
                .take(2)
                .eq(["sidebar", "keys"])),
            "{style}: {:?}",
            machine.notices
        );
    }
}
