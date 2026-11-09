use super::*;
use crate::config::MuxStatusBar;
use serde_json::json;

#[test]
fn old_full_status_is_pending_without_migrating_the_birth_record() {
    let record: Record = serde_json::from_value(json!({
        "version": 1,
        "session": {"mux.status_bar": "full"},
        "client": null
    }))
    .unwrap();
    let PendingSettings::Tracked(pending) =
        diff(Some(&record), &MachineConfig::default(), MuxName::Zellij)
    else {
        panic!("tracked room")
    };
    assert!(
        pending.contains(&PendingSetting {
            key: "mux.status_bar".into(),
            configured: Some(json!("on")),
            recorded: Some(json!("full")),
            when: AppliesWhen::RoomStart,
        }),
        "{pending:?}"
    );
    assert_eq!(record.session.unwrap()["mux.status_bar"], json!("full"));
}

#[test]
fn every_room_setting_has_its_apply_class() {
    use AppliesWhen::{Attach, Reload, RoomStart};
    let config = MachineConfig::default();
    let table = classes(&config, MuxName::Zellij);
    for key in [
        "mouse_click_through",
        "focus_follows_mouse",
        "advanced_mouse_actions",
        "mouse_hover_effects",
        "pane_frames",
        "copy_clipboard",
        "copy_on_select",
    ] {
        assert_eq!(table.get(format!("zellij.{key}").as_str()), Some(&Reload));
    }
    for key in [
        "session_serialization",
        "disable_session_metadata",
        "scroll_buffer_size",
        "on_force_close",
        "osc8_hyperlinks",
        "show_startup_tips",
        "show_release_notes",
    ] {
        assert_eq!(
            table.get(format!("zellij.{key}").as_str()),
            Some(&RoomStart)
        );
    }
    for key in ["mouse_mode", "support_kitty_keyboard_protocol"] {
        assert_eq!(table.get(format!("zellij.{key}").as_str()), Some(&Attach));
    }
    assert_eq!(table.get("mux.status_bar"), Some(&RoomStart));
    assert!(!table.contains_key("mux.default"));
    assert_eq!(table.get("sidebar.focus_key"), Some(&Reload));
    assert_eq!(table.get("sidebar.zoom_key"), Some(&Reload));
    for key in [
        "room.style",
        "keys.keymap",
        "keys.prefix",
        "keys.bind.normal",
        "keys.bind.prefix",
        "keys.bind.resize",
        "keys.bind.move",
        "keys.bind.pane",
        "keys.bind.tab",
    ] {
        assert_eq!(table.get(key), Some(&Reload), "{key}");
    }
    let tmux = classes(&config, MuxName::Tmux);
    assert_eq!(tmux.get("mux.status_bar"), Some(&Reload));
    for key in [
        "mouse",
        "focus_events",
        "history_limit",
        "allow_passthrough",
        "set_clipboard",
        "extended_keys",
        "extended_keys_format",
        "escape_time_ms",
        "renumber_windows",
        "aggressive_resize",
        "pane_border_status",
        "pane_border_lines",
    ] {
        assert_eq!(tmux.get(format!("tmux.{key}").as_str()), Some(&Reload));
    }
}

#[test]
fn changing_room_style_defers_the_zellij_bar_but_not_the_keymap() {
    let mut record = Record::default();
    record.replace_session(&MultiplexerConfig::from(&MachineConfig::default()));
    let config = MachineConfig::parse_text(
        std::path::Path::new("config.toml"),
        "[room]\nstyle='rimz'\n",
        std::path::Path::new("missing-definitions"),
    )
    .unwrap();
    assert_eq!(
        classes(&config, MuxName::Zellij).get("room.style"),
        Some(&AppliesWhen::Reload)
    );
    let PendingSettings::Tracked(pending) = diff(Some(&record), &config, MuxName::Zellij) else {
        panic!("tracked room")
    };
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].key, "mux.status_bar");
    assert_eq!(pending[0].configured, Some(json!("off")));
    assert_eq!(pending[0].when, AppliesWhen::RoomStart);
}

#[test]
fn birth_replaces_session_and_clears_prior_client() {
    let mut record = Record::default();
    let mut config = MultiplexerConfig::default();
    config.zellij.scroll_buffer_size = Some(10000);
    config.zellij.pane_frames = Some(true);
    record.replace_session(&config);
    assert!(record.session.is_some(), "birth records its settings");
    let session = record.session.as_ref().unwrap();
    assert_eq!(session["mux.status_bar"], json!("on"));
    assert_eq!(session["zellij.scroll_buffer_size"], json!(10000));
    assert_eq!(session["zellij.pane_frames"], json!(true));
    for fixed in ["auto_layout", "stacked_resize", "stacked_pane_list"] {
        assert!(!session.contains_key(&format!("zellij.{fixed}")));
    }
    config.status_bar = MuxStatusBar::Off;
    config.zellij.pane_frames = None;
    record.client = Some(BTreeMap::new());
    record.replace_session(&config);
    assert_eq!(
        record.session.as_ref().unwrap()["mux.status_bar"],
        json!("off")
    );
    assert!(
        !record
            .session
            .as_ref()
            .unwrap()
            .contains_key("zellij.pane_frames")
    );
    assert!(record.client.is_none());
    // Teardown removes the dead session's file; its successor starts without it.
    let mut successor = Record::default();
    successor.replace_session(&config);
    assert_eq!(successor.session, record.session);
}

#[test]
fn handoff_updates_only_live_keys_without_fabricating_a_birth() {
    let mut record = Record {
        session: Some(BTreeMap::from([
            ("zellij.pane_frames".into(), json!(false)),
            ("zellij.copy_on_select".into(), json!(true)),
            ("zellij.session_serialization".into(), json!(false)),
            (
                "zellij.support_kitty_keyboard_protocol".into(),
                json!(false),
            ),
        ])),
        ..Record::default()
    };
    let config = ZellijConfig {
        pane_frames: Some(true),
        session_serialization: true,
        support_kitty_keyboard_protocol: Some(true),
        ..Default::default()
    };
    record.merge_live(&super::super::zellij::zellij_session_options(&config));
    let session = record.session.as_ref().unwrap();
    assert_eq!(session["zellij.pane_frames"], json!(true));
    assert_eq!(session["zellij.copy_on_select"], json!(true));
    assert_eq!(session["zellij.session_serialization"], json!(false));
    assert_eq!(
        session["zellij.support_kitty_keyboard_protocol"],
        json!(false)
    );
    let mut older = Record::default();
    older.merge_live(&super::super::zellij::zellij_session_options(&config));
    assert!(older.session.is_none());
}

#[test]
fn attach_records_exactly_the_client_flags_and_replaces_previous_ones() {
    let mut record = Record::default();
    record.replace_client(&ZellijConfig {
        mouse_mode: Some(false),
        support_kitty_keyboard_protocol: Some(true),
        ..Default::default()
    });
    assert_eq!(
        record.client,
        Some(BTreeMap::from([
            ("zellij.mouse_mode".into(), json!(false)),
            ("zellij.support_kitty_keyboard_protocol".into(), json!(true)),
        ]))
    );
    record.replace_client(&ZellijConfig {
        mouse_mode: Some(true),
        ..Default::default()
    });
    assert_eq!(record.client, Some(BTreeMap::new()));
    assert!(record.session.is_none());
}

#[test]
fn pending_compares_birth_removals_and_attach_assertions_but_not_live_changes() {
    let record = Record {
        session: Some(BTreeMap::from([
            ("mux.status_bar".into(), json!("on")),
            ("zellij.pane_frames".into(), json!(false)),
            ("zellij.copy_on_select".into(), json!(true)),
        ])),
        client: Some(BTreeMap::new()),
        ..Record::default()
    };
    let mut config = MachineConfig::default();
    config.mux.status_bar = Some(MuxStatusBar::Off);
    config.zellij.pane_frames = Some(true);
    config.zellij.mouse_mode = Some(false);
    config.mux.default = Some(MuxName::Tmux);
    let PendingSettings::Tracked(pending) = diff(Some(&record), &config, MuxName::Zellij) else {
        panic!("tracked room")
    };
    for (key, configured, recorded, when) in [
        (
            "mux.status_bar",
            Some(json!("off")),
            Some(json!("on")),
            AppliesWhen::RoomStart,
        ),
        (
            "zellij.copy_on_select",
            None,
            Some(json!(true)),
            AppliesWhen::RoomStart,
        ),
        (
            "zellij.mouse_mode",
            Some(json!(false)),
            None,
            AppliesWhen::Attach,
        ),
    ] {
        assert!(
            pending.contains(&PendingSetting {
                key: key.into(),
                configured,
                recorded,
                when
            }),
            "{pending:?}"
        );
    }
    assert!(
        !pending
            .iter()
            .any(|p| p.key == "zellij.pane_frames" || p.key == "mux.default")
    );
    config.zellij.mouse_mode = Some(true);
    let PendingSettings::Tracked(pending) = diff(Some(&record), &config, MuxName::Zellij) else {
        unreachable!()
    };
    assert!(!pending.iter().any(|p| p.when == AppliesWhen::Attach));
    config.zellij.mouse_mode = Some(false);
    let no_client = Record {
        client: None,
        ..record
    };
    let PendingSettings::Tracked(pending) = diff(Some(&no_client), &config, MuxName::Zellij) else {
        unreachable!()
    };
    assert!(!pending.iter().any(|p| p.when == AppliesWhen::Attach));
}

#[test]
fn absent_or_future_records_predate_tracking_only_on_zellij() {
    let config = MachineConfig::default();
    let temp = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    for record in [
        None,
        Some(Record::default()),
        Some(Record {
            version: 2,
            session: Some(BTreeMap::new()),
            ..Record::default()
        }),
    ] {
        if let Some(record) = record {
            std::fs::write(
                runtime.lane_path("room-settings.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            pending(&runtime, &config, MuxName::Zellij),
            PendingSettings::BeforeTracking
        );
        assert_eq!(
            pending(&runtime, &config, MuxName::Tmux),
            PendingSettings::Tracked(vec![])
        );
    }
    let record: Record = serde_json::from_value(json!({"version": 1,
        "session": {"unknown.future": 9}}))
    .unwrap();
    let PendingSettings::Tracked(pending) = diff(Some(&record), &config, MuxName::Zellij) else {
        panic!("known version")
    };
    assert!(!pending.iter().any(|p| p.key == "unknown.future"));
}

#[test]
fn releasing_a_false_mouse_override_names_the_configured_true_value() {
    let mut config = MachineConfig::default();
    let mut record = Record::default();
    record.replace_session(&MultiplexerConfig::from(&config));
    record.replace_client(&ZellijConfig {
        mouse_mode: Some(false),
        ..Default::default()
    });
    config.zellij.mouse_mode = Some(true);
    assert_eq!(
        diff(Some(&record), &config, MuxName::Zellij),
        PendingSettings::Tracked(vec![PendingSetting {
            key: "zellij.mouse_mode".into(),
            configured: Some(json!(true)),
            recorded: Some(json!(false)),
            when: AppliesWhen::Attach,
        }]),
        "an unassertable true is not an unset config key"
    );
}
