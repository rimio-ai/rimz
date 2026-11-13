use super::*;
use rimz::ids::MuxName;
use rimz::mux::room_settings::{AppliesWhen, PendingSetting, PendingSettings};
use rimz::reload::{MachineReload, ReloadOutcome, RoomReload};
use serde_json::json;

fn room(name: &str, path: &str) -> RoomReload {
    RoomReload {
        project_root: path.into(),
        mux: MuxName::Tmux,
        session_name: name.into(),
        outcome: ReloadOutcome::default(),
        sidebars: 2,
        current_sidebars: 2,
        record_error: None,
        options_error: None,
        plugin_error: None,
        claude_restarts: false,
        pending: PendingSettings::Tracked(vec![]),
        repair: None,
    }
}

fn rendered(result: &ReloadReport, width: usize) -> (String, usize) {
    let mut bytes = Vec::new();
    let needs_you = report(&mut anstream::AutoStream::never(&mut bytes), result, width).unwrap();
    (String::from_utf8(bytes).unwrap(), needs_you)
}

fn result(rooms: Vec<RoomReload>) -> ReloadReport {
    ReloadReport {
        rooms,
        machine: MachineReload::default(),
    }
}

#[test]
fn reload_error_agrees_with_one_or_several_items() {
    for (n, phrase) in [(1, "1 item that needs you"), (2, "2 items that need you")] {
        assert_eq!(
            reload_error(n).to_string(),
            format!("reload left {phrase}; see the fixes above")
        );
    }
}

#[test]
fn healthy_rooms_name_the_build_and_keep_internal_upgrades_quiet() {
    let mut healthy = room("unique-session", "/work/project");
    healthy.outcome.drainers_restarted = 1;
    healthy.outcome.plugin_upgraded = 1;
    for n in [1, 2] {
        let (text, exit) = rendered(&result(vec![healthy.clone(); n]), 160);
        assert!(
            text.starts_with(&format!("✓ {} on rimz ", count(n, "room"))),
            "{text}"
        );
        assert_eq!(text.matches(rimz::build_id::VERSION).count(), 1);
        let line = text
            .lines()
            .find(|line| line.contains("/work/project"))
            .unwrap();
        assert!(line.ends_with("2 sidebars"), "{text}");
        assert_eq!(line.matches("unique-session").count(), 1);
        assert!(
            !text.contains("drainer") && !text.contains("plugin"),
            "{text}"
        );
        assert_eq!(exit, 0);
    }
}

#[test]
fn settling_rooms_are_unconfirmed_and_retry_without_needing_you() {
    for unlocated in [false, true] {
        let mut settling = room("settling-room", "/work/settling");
        settling.current_sidebars = 1;
        if unlocated {
            settling.outcome.unverified = 1;
        } else {
            settling.outcome.unconverged = 1;
        }
        let (text, exit) = rendered(
            &result(vec![room("healthy", "/work/healthy"), settling]),
            160,
        );
        assert!(text.starts_with("… 1 of 2 rooms on rimz "), "{text}");
        assert!(text.contains("· 1 settling"), "{text}");
        assert!(
            text.contains("1 of 2 sidebars · 1 settling, retries on its own"),
            "{text}"
        );
        assert!(!text.contains('▸'), "{text}");
        assert_eq!(exit, 0);
    }
}

#[test]
fn a_silent_room_settles_without_a_count_and_never_needs_you() {
    let mut silent = room("silent-room", "/work/silent");
    silent.sidebars = 0;
    silent.current_sidebars = 0;
    silent.outcome.sidebar_silent = 1;
    let (text, exit) = rendered(&result(vec![room("healthy", "/work/healthy"), silent]), 160);
    assert!(text.starts_with("… 1 of 2 rooms on rimz "), "{text}");
    assert!(text.contains("· 1 settling"), "{text}");
    let line = text
        .lines()
        .find(|line| line.contains("/work/silent"))
        .unwrap();
    assert!(line.starts_with("  … "), "{text}");
    assert!(
        line.ends_with("0 sidebars · settling, retries on its own"),
        "{text}"
    );
    assert!(
        !text.contains("fix:") && !text.contains("needs you"),
        "{text}"
    );
    assert_eq!(exit, 0);
}

#[test]
fn mounted_sidebars_are_counted_on_a_healthy_row() {
    for (n, phrase) in [(1, "1 sidebar mounted"), (2, "2 sidebars mounted")] {
        let mut mounted = room("mounted-room", "/work/mounted");
        mounted.outcome.recovered = n;
        let (text, exit) = rendered(&result(vec![mounted.clone()]), 160);
        assert!(text.starts_with("✓ 1 room on rimz "), "{text}");
        let line = text
            .lines()
            .find(|line| line.contains("/work/mounted"))
            .unwrap();
        assert!(line.ends_with(&format!("2 sidebars · {phrase}")), "{text}");
        assert_eq!(text.matches(phrase).count(), 1, "{text}");
        assert_eq!(exit, 0);

        mounted.outcome.misdocked = 1;
        let (text, exit) = rendered(&result(vec![mounted]), 160);
        assert!(
            text.contains(&format!("sidebar working but not docked · {phrase}")),
            "a misdocked mount keeps its count: {text}"
        );
        assert_eq!(exit, 1);
    }
}

fn assert_room_problem(phrase: &str, fix: &str, change: fn(&mut RoomReload)) {
    for n in [1, 2] {
        let mut problem = room("problem-room", "/work/project");
        change(&mut problem);
        let (text, exit) = rendered(&result(vec![problem; n]), 180);
        let need = if n == 1 { "1 needs you" } else { "2 need you" };
        assert!(
            text.starts_with(&format!("! 0 of {} on rimz ", count(n, "room"))),
            "{text}"
        );
        assert!(text.contains(need), "{text}");
        assert_eq!(text.matches(phrase).count(), 2 * n, "{text}");
        assert_eq!(text.matches(&format!("fix: {fix}\n")).count(), n, "{text}");
        assert_eq!(text.matches("problem-room").count(), 2 * n, "{text}");
        assert!(!text.contains('✗'), "{text}");
        assert_eq!(exit, n);
    }
}

macro_rules! room_case {
    ($name:ident, $phrase:literal, $fix:literal, $change:expr) => {
        #[test]
        fn $name() {
            assert_room_problem($phrase, $fix, $change);
        }
    };
}

room_case!(
    unsignalable_sidebar_names_repair,
    "sidebar on a protocol this build cannot signal",
    "rimz reload --repair",
    |r| r.outcome.sidebar_foreign_protocol = 1
);
room_case!(
    failed_mount_names_repair,
    "sidebar could not be mounted",
    "rimz reload --repair",
    |r| r.outcome.failed = 1
);
room_case!(
    deferred_mount_names_attach,
    "sidebar mount waits for an attached client",
    "rimz attach --root /work/project",
    |r| r.outcome.deferred = 1
);
room_case!(
    busy_mount_waits_for_the_other_pass,
    "another sidebar pass holds the room",
    "rimz reload once it finishes",
    |r| r.outcome.busy = 1
);
room_case!(
    misdocked_mount_names_repair,
    "sidebar working but not docked",
    "rimz reload --repair",
    |r| {
        r.outcome.recovered = 1;
        r.outcome.misdocked = 1;
    }
);
room_case!(
    old_drainer_waits_for_idle,
    "hook drainer still on the old build",
    "rimz reload once its agents are idle",
    |r| r.outcome.drainers_stop_failed = 1
);
room_case!(
    record_failure_names_doctor,
    "new build not recorded: disk full",
    "rimz doctor --root /work/project",
    |r| r.record_error = Some("disk full".into())
);
room_case!(
    tmux_failure_names_reload,
    "tmux room options not applied: refused",
    "rimz reload",
    |r| r.options_error = Some("refused".into())
);
room_case!(
    plugin_failure_names_doctor,
    "presence plugin not upgraded: refused",
    "rimz doctor --root /work/project",
    |r| r.plugin_error = Some("refused".into())
);
room_case!(
    missing_plugin_artifact_names_doctor,
    "presence plugin not upgraded: presence plugin artifact unavailable",
    "rimz doctor --root /work/project",
    |r| r.plugin_error = Some("presence plugin artifact unavailable".into())
);
room_case!(
    failed_repair_names_attach,
    "sidebar could not be repaired",
    "rimz attach --root /work/project, then rimz reload --repair",
    |r| r.repair = Some(ReloadOutcome {
        failed: 1,
        ..Default::default()
    })
);
room_case!(
    deferred_repair_names_attach,
    "repair waits for an attached client",
    "rimz attach --root /work/project, then rimz reload --repair",
    |r| r.repair = Some(ReloadOutcome {
        deferred: 1,
        ..Default::default()
    })
);
room_case!(
    deferred_daemon_names_attach,
    "repair waits for an attached client",
    "rimz attach --root /work/project, then rimz reload --repair",
    |r| r.repair = Some(ReloadOutcome {
        daemon_reopen_deferred: 1,
        ..Default::default()
    })
);
room_case!(
    misdocked_sidebar_names_repair,
    "sidebar working but not docked",
    "rimz reload --repair",
    |r| r.repair = Some(ReloadOutcome {
        misdocked: 1,
        ..Default::default()
    })
);
room_case!(
    busy_repair_waits_for_its_owner,
    "another repair holds the room",
    "rimz reload --repair once it finishes",
    |r| r.repair = Some(ReloadOutcome {
        busy: 1,
        ..Default::default()
    })
);
room_case!(
    failed_daemon_names_repair,
    "rimzd tab could not be reopened",
    "rimz reload --repair",
    |r| r.repair = Some(ReloadOutcome {
        daemon_reopen_failed: 1,
        ..Default::default()
    })
);
room_case!(
    silent_presence_names_attach,
    "presence plugin silent",
    "rimz attach --root /work/project",
    |r| r.repair = Some(ReloadOutcome {
        presence_dead: 1,
        ..Default::default()
    })
);

macro_rules! machine_case {
    ($name:ident, $phrase:literal, $fix:literal, $change:expr) => {
        #[test]
        fn $name() {
            for n in [1, 2] {
                let mut report = result(vec![room("healthy", "/work/healthy"); n]);
                $change(&mut report.machine);
                if n == 2 {
                    report.rooms[0].outcome.failed = 1;
                }
                let (text, exit) = rendered(&report, 160);
                assert!(
                    text.starts_with(&format!("! 1 of {} on rimz ", count(n, "room"))),
                    "{text}"
                );
                let tally = if n == 1 { "1 needs you" } else { "2 need you" };
                assert!(text.contains(tally), "{text}");
                assert!(text.contains($phrase), "{text}");
                assert!(text.contains(concat!("fix: ", $fix)), "{text}");
                assert_eq!(exit, n);
            }
        }
    };
}

machine_case!(
    old_brokers_name_the_idle_retry,
    "machine: 1 language-server broker too old to retire",
    "rimz reload once idle",
    |m: &mut MachineReload| m.brokers_kept = 1
);
machine_case!(
    unchecked_brokers_name_path,
    "machine: no readable rimz on PATH; brokers not checked",
    "put rimz on PATH, then rimz reload",
    |m: &mut MachineReload| m.brokers_unchecked = 1
);
machine_case!(
    web_failure_names_restart,
    "machine: web daemon restart failed: refused",
    "rimz web restart",
    |m: &mut MachineReload| m.web_error = Some("refused".into())
);

#[test]
fn machine_problems_join_the_same_closing_block_even_without_rooms() {
    let mut report = result(vec![]);
    report.machine.brokers_kept = 2;
    report.machine.web_error = Some("refused".into());
    let (text, exit) = rendered(&report, 160);
    assert!(text.contains("! 2 need you"), "{text}");
    assert!(
        text.contains("2 language-server brokers too old to retire"),
        "{text}"
    );
    assert!(text.starts_with("No running sidebars to reload."), "{text}");
    assert_eq!(exit, 2);
}

#[test]
fn rooms_sort_by_state_then_byte_ordered_paths() {
    let mut problem = room("problem", "/work/problem");
    problem.outcome.failed = 1;
    let mut settling = room("settling", "/work/settling");
    settling.outcome.unconverged = 1;
    let (text, _) = rendered(
        &result(vec![
            room("lower", "/work/alpha"),
            room("upper", "/work/Alpha"),
            settling,
            problem,
        ]),
        160,
    );
    let paths: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("  ") && line.contains("/work/"))
        .collect();
    for (line, path) in paths.iter().zip([
        "/work/problem",
        "/work/settling",
        "/work/Alpha",
        "/work/alpha",
    ]) {
        assert!(line.contains(path), "{text}");
    }
    assert_eq!(paths.len(), 4);
}

#[test]
fn narrow_reports_drop_session_before_counts_and_keep_the_last_two_path_segments() {
    let report = result(vec![room(
        "unique-session",
        "/a/very/long/workspace/checkout/service",
    )]);
    let (wide, _) = rendered(&report, 100);
    assert!(wide.contains("unique-session"), "{wide}");
    for width in [60, 40] {
        let (text, _) = rendered(&report, width);
        for line in text.lines() {
            assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line}");
        }
        assert!(!text.contains("unique-session"), "{text}");
        assert!(text.contains("2 sidebars"), "{text}");
        assert!(text.contains("…/checkout/service"), "{text}");
    }
    let (tiny, _) = rendered(&report, 24);
    assert!(!tiny.contains("2 sidebars"), "{tiny}");
    let mut detailed = report;
    detailed.rooms[0].record_error = Some("the disk is full".into());
    detailed.rooms[0].pending = PendingSettings::BeforeTracking;
    detailed.machine.web_error = Some("daemon refused the restart".into());
    for width in [60, 40] {
        let (text, _) = rendered(&detailed, width);
        let prose = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("rimz "));
        for line in prose {
            assert!(
                UnicodeWidthStr::width(line) <= width,
                "prose at {width}: {line}"
            );
        }
    }
}

#[test]
fn narrow_settling_zellij_rows_drop_counts_before_cutting_two_segments() {
    let mut settling = room("unique-session", "/work/checkout/service");
    settling.mux = MuxName::Zellij;
    settling.current_sidebars = 1;
    settling.outcome.unconverged = 1;
    let (text, _) = rendered(&result(vec![settling]), 40);
    assert!(text.contains("/work/checkout/service"), "{text}");
    assert!(!text.contains("1 of 2 sidebars"), "{text}");
    assert!(!text.contains("unique-session"), "{text}");
    for line in text.lines() {
        assert!(UnicodeWidthStr::width(line) <= 40, "{text}");
    }
}

#[test]
fn narrow_paths_drop_whole_components_before_cutting_characters() {
    let (text, _) = rendered(
        &result(vec![room(
            "unique-session",
            "~/workspace/rimio-ai/rimz-worktrees/reload-report",
        )]),
        60,
    );
    assert!(text.contains("…/rimz-worktrees/reload-report"), "{text}");
    assert!(text.contains("2 sidebars"), "{text}");
}

fn pending_room(root: &str) -> RoomReload {
    let mut tracked = room("tracked", root);
    tracked.pending = PendingSettings::Tracked(vec![
        PendingSetting {
            key: "zellij.scroll_buffer_size".into(),
            configured: Some(json!(5000)),
            recorded: None,
            when: AppliesWhen::RoomStart,
        },
        PendingSetting {
            key: "zellij.mouse_mode".into(),
            configured: Some(json!(false)),
            recorded: None,
            when: AppliesWhen::Attach,
        },
    ]);
    tracked
}

#[test]
fn copyable_commands_stay_whole_even_when_they_overflow_eighty_columns() {
    let root = "/tmp/rimz-sandbox-render/home/a-project-directory-too-long-for-eighty-columns";
    let mut tracked = pending_room(root);
    tracked.record_error = Some("the disk is full".into());
    let (text, _) = rendered(&result(vec![tracked]), 80);
    for verb in ["reset", "attach", "doctor"] {
        let command = format!("rimz {verb} --root {root}");
        let lines: Vec<_> = text
            .lines()
            .filter(|line| line.contains(&command))
            .collect();
        assert_eq!(lines.len(), 1, "whole {command}: {text}");
        assert!(lines[0].starts_with("        rimz "), "{text}");
        assert!(UnicodeWidthStr::width(lines[0]) > 80, "{text}");
    }
    assert!(
        text.contains(&format!(
            "      to apply now:\n        rimz reset --root {root}\n        (closes this room's agents)"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "      to apply now: detach, then\n        rimz attach --root {root}"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("      fix:\n        rimz doctor --root {root}")),
        "{text}"
    );

    let root = "/tmp/project-name";
    let (text, _) = rendered(&result(vec![pending_room(root)]), 80);
    assert!(
        text.contains(&format!(
            "      to apply now:\n        rimz reset --root {root}   (closes this room's agents)"
        )),
        "an aside that fits follows the whole command: {text}"
    );
}

#[test]
fn detach_then_is_muted_and_only_the_attach_command_is_accented() {
    let reload = result(vec![pending_room("/tmp/project-name")]);
    for width in [80, 400] {
        let mut bytes = Vec::new();
        report(&mut bytes, &reload, width).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.contains(&format!("{}detach,", palette::muted().render())),
            "{text}"
        );
        assert!(
            text.contains(&render::paint(
                palette::accent(),
                "rimz attach --root /tmp/project-name"
            )),
            "{text}"
        );
    }
}

#[test]
fn wrapped_pending_settings_hang_deeper_than_their_first_line() {
    let (text, _) = rendered(&result(vec![pending_room("/work/project")]), 80);
    let lines: Vec<_> = text.lines().collect();
    let setting = lines
        .iter()
        .position(|line| line.contains("[zellij] scroll_buffer_size"))
        .unwrap();
    assert!(lines[setting].starts_with("      [zellij]"), "{text}");
    assert_eq!(
        lines[setting + 1],
        "        your config.kdl value",
        "{text}"
    );
}

#[test]
fn pending_and_remote_restart_lines_are_informational_and_shell_quote_targets() {
    let mut tracked = room("tracked", "/work/project with 'quotes'");
    tracked.claude_restarts = true;
    tracked.outcome.reaped = 1;
    tracked.pending = PendingSettings::Tracked(vec![
        PendingSetting {
            key: "mux.status_bar".into(),
            configured: Some(json!("off")),
            recorded: Some(json!("on")),
            when: AppliesWhen::RoomStart,
        },
        PendingSetting {
            key: "zellij.mouse_mode".into(),
            configured: Some(json!(false)),
            recorded: Some(json!(true)),
            when: AppliesWhen::Attach,
        },
        PendingSetting {
            key: "zellij.copy_on_select".into(),
            configured: None,
            recorded: Some(json!(true)),
            when: AppliesWhen::RoomStart,
        },
        PendingSetting {
            key: "zellij.scroll_buffer_size".into(),
            configured: Some(json!(10000)),
            recorded: None,
            when: AppliesWhen::RoomStart,
        },
        PendingSetting {
            key: "zellij.support_kitty_keyboard_protocol".into(),
            configured: Some(json!(false)),
            recorded: None,
            when: AppliesWhen::Attach,
        },
    ]);
    let mut older = room("older", "/work/older");
    older.pending = PendingSettings::BeforeTracking;
    let mut report = result(vec![tracked, older.clone()]);
    report.machine = MachineReload {
        stats_reloaded: 4,
        brokers_retired: 1,
        brokers_marked: 2,
        dead_swept: 3,
        web_restarted: true,
        ..Default::default()
    };
    let (text, exit) = rendered(&report, 400);
    for phrase in [
        "Also: 4 stats dashboards reloaded",
        "1 language-server broker retired",
        "2 brokers retire when their server stops",
        "1 orphaned sidebar process reaped",
        "3 leftover processes swept from stopped rooms",
        "web daemon restarted",
        "Claude remote control restarts in tracked; an open phone session drops.",
        "Applies when the room next starts:",
        "Applies when you next attach:",
        "[mux] status_bar is \"off\" in your config; the room started with \"on\"",
        "[zellij] mouse_mode is false in your config; your last attach used true",
        "[zellij] copy_on_select is unset in your config; the room keeps true",
        "the room started with your config.kdl value",
        concat!(
            "[zellij] support_kitty_keyboard_protocol is false in your config; ",
            "your last attach used your config.kdl value"
        ),
        concat!(
            "1 room started before RimZ tracked room settings; ",
            "changes there show after its next start."
        ),
    ] {
        assert!(text.contains(phrase), "missing {phrase}: {text}");
    }
    let line = text
        .lines()
        .find(|line| line.contains("to apply now: rimz reset"))
        .unwrap();
    let command = line
        .trim()
        .strip_prefix("to apply now: ")
        .unwrap()
        .split("   (closes")
        .next()
        .unwrap();
    assert_eq!(
        shlex::split(command).unwrap(),
        ["rimz", "reset", "--root", "/work/project with 'quotes'"]
    );
    assert_eq!(exit, 0);
    let (text, _) = rendered(&result(vec![older.clone(), older]), 160);
    assert!(
        text.contains(concat!(
            "2 rooms started before RimZ tracked room settings; ",
            "changes there show after their next start."
        )),
        "{text}"
    );
}

#[test]
fn repair_completed_before_the_report_replaces_bare_reload_findings() {
    let mut repaired = room("repaired", "/work/repaired");
    repaired.sidebars = 1;
    repaired.current_sidebars = 1;
    repaired.outcome.sidebar_silent = 1;
    repaired.outcome.sidebar_foreign_protocol = 1;
    repaired.repair = Some(ReloadOutcome {
        recovered: 1,
        ..Default::default()
    });
    let mut report = result(vec![repaired]);
    let (text, exit) = rendered(&report, 160);
    assert!(text.starts_with("✓ 1 room on rimz "), "{text}");
    assert!(text.contains("1 sidebar"), "{text}");
    assert!(
        !text.contains("settling") && !text.contains("protocol"),
        "{text}"
    );
    assert_eq!(exit, 0);
    report.rooms[0].repair = Some(ReloadOutcome {
        failed: 1,
        ..Default::default()
    });
    let (text, exit) = rendered(&report, 160);
    assert!(text.contains("sidebar could not be repaired"), "{text}");
    assert_eq!(exit, 1);
}

#[test]
fn repair_cleanup_is_included_in_the_machine_footer() {
    let mut repaired = room("repaired", "/work/repaired");
    repaired.outcome.reaped = 1;
    repaired.repair = Some(ReloadOutcome {
        reaped: 2,
        ..Default::default()
    });
    let (text, exit) = rendered(&result(vec![repaired]), 160);
    assert!(
        text.contains("3 orphaned sidebar processes reaped"),
        "{text}"
    );
    assert_eq!(exit, 0);
}
