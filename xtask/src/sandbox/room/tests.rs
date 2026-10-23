use super::*;

#[test]
fn team_projection_waits_for_every_rostered_member_in_its_tab() {
    let team: Team = serde_json::from_str(
        r##"{"instances":[{"channel":"probe","members":[
            {"handle":"@coder"},{"handle":"@reviewer"},{"handle":"@auditor"}
        ]}]}"##,
    )
    .unwrap();
    let team = &team.instances[0];
    let stale: Panes = serde_json::from_str(
        r##"{"tabs":[{"name":"#probe","panes":[
            {"kind":"agent","pane_id":"zellij:terminal_7","agent":{"handle":"@coder#probe"}}
        ]}]}"##,
    )
    .unwrap();
    assert_eq!(
        missing_team_members(&stale, team),
        ["@reviewer#probe", "@auditor#probe"]
    );
    let panes: Panes = serde_json::from_str(
        r##"{"tabs":[{"name":"shell","panes":[
            {"kind":"agent","pane_id":"tmux:%1","agent":{"handle":"@reviewer#probe"}}
        ]},{"name":"#probe","panes":[
            {"kind":"agent","pane_id":"tmux:%2","agent":{"handle":"@coder#probe"}},
            {"kind":"process","pane_id":"tmux:%3","agent":{"handle":"@reviewer#probe"}},
            {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@auditor#other"}}
        ]}]}"##,
    )
    .unwrap();
    assert_eq!(
        missing_team_members(&panes, team),
        ["@reviewer#probe", "@auditor#probe"]
    );
    let ready: Panes = serde_json::from_str(
        r##"{"tabs":[{"name":"#probe","panes":[
            {"kind":"agent","pane_id":"tmux:%2","agent":{"handle":"@coder#probe"}},
            {"kind":"agent","pane_id":"tmux:%3","agent":{"handle":"@reviewer#probe"}},
            {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@auditor#probe"}}
        ]}]}"##,
    )
    .unwrap();
    assert!(missing_team_members(&ready, team).is_empty());
}

#[test]
fn client_argv_accepts_both_managed_attach_shapes_not_bootstrap_or_other_sessions() {
    for cmdline in [
        &b"/usr/local/bin/zellij\0attach\0--create\0room-abcd\0options\0--mouse-mode\0true\0"[..],
        &b"zellij\0attach\0room-abcd\0options\0--mouse-mode\0true\0"[..],
    ] {
        assert!(is_client_argv(cmdline, "room-abcd"), "{cmdline:?}");
    }
    for cmdline in [
        &b"zellij\0attach\0--create-background\0room-abcd\0"[..],
        &b"zellij\0attach\0room-abcd-other\0options\0"[..],
        &b"zellij\0--session\0room-abcd\0action\0list-clients\0"[..],
        &b"zellij\0--server\0room-abcd\0"[..],
        &b"rimz\0attach\0room-abcd\0"[..],
        &b""[..],
    ] {
        assert!(!is_client_argv(cmdline, "room-abcd"), "{cmdline:?}");
    }
}

#[test]
fn client_confirmation_needs_one_listed_id_in_every_reply() {
    assert!(client_ids(b"").is_empty());
    assert!(client_ids(b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\n  \n").is_empty());
    let reply = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        client_ids(
            b"\nCLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\n1 terminal_1 /bin/sh\n2 terminal_2 vim\n\n"
        ),
        reply(&[1, 2])
    );

    assert!(!one_client_in_every(&[]));
    assert!(!one_client_in_every(&[reply(&[])]));
    assert!(one_client_in_every(&[reply(&[1])]));
    // A bootstrap row that drains, replaced by a client under another id, is not one client.
    assert!(!one_client_in_every(&[
        reply(&[1]),
        reply(&[1]),
        reply(&[2])
    ]));
    assert!(!one_client_in_every(&[
        reply(&[1]),
        reply(&[]),
        reply(&[1])
    ]));
    assert!(one_client_in_every(&[
        reply(&[1, 2]),
        reply(&[2]),
        reply(&[2, 3])
    ]));
}

#[test]
fn room_refuses_provider_path_that_does_not_resolve_to_stub() {
    let stub = Path::new("/room/bin/claude");
    assert!(verify_stub(OsStr::new("/room/bin:/host/bin"), stub, |_| true).is_ok());
    let error = verify_stub(OsStr::new("/host/bin:/room/bin"), stub, |_| true).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("put the executable room stub first on PATH")
    );
    assert!(verify_stub(OsStr::new("/room/bin:/host/bin"), stub, |path| path != stub).is_err());
}

#[test]
fn room_card_uses_live_roles_and_snapshot_owner() {
    let panes = serde_json::from_str(r##"{"session":"private","tabs":[{"name":"shell","panes":[{"kind":"sidebar","pane_id":"tmux:%1"}]},{"name":"#probe","panes":[{"kind":"sidebar","pane_id":"tmux:%3"},{"kind":"agent","pane_id":"tmux:%4","pid":4242,"agent":{"handle":"@coder#probe"}}]}]}"##).unwrap();
    let renderers: Vec<Renderer> =
        serde_json::from_str(r#"[{"pane_id":"tmux:%3"},{"pane_id":"tmux:%1"}]"#).unwrap();
    let snapshot = serde_json::from_str(
        r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"coder"}}]}"#,
    )
    .unwrap();
    let record = RoomRecord {
        mux: "tmux".into(),
        session: "private".into(),
        repo: "/room".into(),
        worktree: "/room-worktrees/probe".into(),
        stub_dir: "/bin".into(),
        binary: "/dev/rimz".into(),
    };
    let card = render_card(
        Path::new("/sandbox"),
        Path::new("/dev/rimz"),
        &record,
        &panes,
        &renderers,
        &snapshot,
    )
    .unwrap();
    assert!(card.contains("Sidebar: tmux:%1  Tab: shell\n"));
    assert!(card.contains("Sidebar: tmux:%3  Tab: #probe\n"));
    assert!(card.contains("Stage: Build  Owner: coder"));
    assert!(card.contains("Role: @coder#probe  tmux:%4  pid 4242"));
    assert!(card.contains(
        "  As: target/debug/xtask sandbox in '/sandbox' --as '@coder#probe' -- '/dev/rimz' --tmux"
    ));
    assert!(card.contains(
        "target/debug/xtask sandbox in '/sandbox' -- '/dev/rimz' --tmux pane capture 'tmux:%3'"
    ));
    assert!(card.contains("teams flip Review 'live check' --team forge"));
    assert!(card.contains("sidebar click 'tmux:%3' 2"));
    assert!(card.contains("tmux -S '/sandbox/runtime/rimz/tmux/server' display -p '#{pane_id}'"));
    assert!(card.contains(
        "Look: target/debug/xtask sandbox in '/sandbox' -- '/dev/rimz' '--tmux' 'pane' 'focus' 'tmux:%3'"
    ));
    assert!(card.contains("unwatched sidebar can hold a stale frame"));
}

fn with_ready_room(test: impl FnOnce(&Room<'_>, &RoomRecord)) {
    let sandbox = HostSandbox::for_manual_command().unwrap();
    let cwd = sandbox.env["HOME"].clone();
    let binary = cwd.join("rimz");
    std::fs::write(
        &binary,
        r#"#!/bin/sh
printf '%s\n' "$*" >> queries
case "$*" in
'--tmux pane list --json')
    if test -f roster-error; then
        printf '%s\n' 'multiplexer unavailable' >&2
        exit 1
    fi
    exec cat panes.json ;;
'--tmux pane focus '*) exec sh focus.sh "$4" ;;
'--tmux sidebar renderers --json') exec cat renderers.json ;;
'--tmux sidebar snapshot --json') exec cat snapshot.json ;;
*) exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        cwd.join("panes.json"),
        r##"{"session":"private","tabs":[
            {"view_id":"@1","panes":[{"kind":"sidebar","pane_id":"tmux:%1"}]},
            {"view_id":"@2","panes":[
            {"kind":"sidebar","pane_id":"tmux:%3"},
            {"kind":"agent","pane_id":"tmux:%4","agent":{"handle":"@coder#probe"}},
            {"kind":"agent","pane_id":"tmux:%5","agent":{"handle":"@reviewer#probe"}}
        ]}]}"##,
    )
    .unwrap();
    std::fs::write(
        cwd.join("renderers.json"),
        r#"[{"pane_id":"tmux:%1"},{"pane_id":"tmux:%3"}]"#,
    )
    .unwrap();
    std::fs::write(
        cwd.join("snapshot.json"),
        r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"coder"}}]}"#,
    )
    .unwrap();
    let record = RoomRecord {
        mux: "tmux".into(),
        session: "private".into(),
        repo: cwd.clone(),
        worktree: cwd.clone(),
        stub_dir: cwd.clone(),
        binary: binary.clone(),
    };
    let room = Room {
        sandbox: &sandbox,
        binary: &binary,
        mux: "tmux",
        path: std::env::var_os("PATH").unwrap(),
        cwd,
    };
    room.ready().unwrap();
    std::fs::write(room.cwd.join("queries"), "").unwrap();
    test(&room, &record);
}

#[test]
fn room_card_refreshes_after_readiness_despite_stale_renderer() {
    with_ready_room(|room, record| {
        std::fs::write(
            room.cwd.join("panes.json"),
            r##"{"session":"private","tabs":[{"panes":[
                {"kind":"sidebar","pane_id":"tmux:%3"},
                {"kind":"agent","pane_id":"tmux:%5","agent":{"handle":"@reviewer#probe"}}
            ]}]}"##,
        )
        .unwrap();
        std::fs::write(
            room.cwd.join("snapshot.json"),
            r#"{"worktree_groups":[{"pipeline":{"stage":"Build","owner":"reviewer"}}]}"#,
        )
        .unwrap();
        let card = room.card(record).unwrap();
        assert!(card.contains("Sidebar: tmux:%3"));
        assert!(card.contains("Role: @reviewer#probe"));
        assert!(card.contains("Stage: Build  Owner: reviewer"));
        assert!(!card.contains("tmux:%1"));
        assert!(!card.contains("tmux:%4"));
        assert!(!card.contains("@coder#probe"));
        let queries = std::fs::read_to_string(room.cwd.join("queries")).unwrap();
        assert_eq!(queries.lines().count(), 3);
        assert_eq!(queries.lines().last(), Some("--tmux pane list --json"));
    });
}

#[test]
fn room_card_refuses_renderer_missing_after_readiness() {
    with_ready_room(|room, record| {
        std::fs::write(
            room.cwd.join("renderers.json"),
            r#"[{"pane_id":"tmux:%1"}]"#,
        )
        .unwrap();
        let result = room.card(record);
        assert!(
            result.is_err_and(|error| error.to_string().contains("sidebar lacks live renderer"))
        );
    });
}

#[test]
fn tab_visit_retries_only_known_refusals_with_bounded_delays() {
    let refusal = b"There is no active session!\n";
    for attempt in 1..=5 {
        assert_eq!(
            look_retry_delay("zellij", refusal, attempt),
            Some(Duration::from_millis(100))
        );
    }
    assert_eq!(look_retry_delay("zellij", refusal, 6), None);
    assert_eq!(look_retry_delay("tmux", refusal, 1), None);
    for stderr in [
        &b""[..],
        &b"session permission denied\n"[..],
        &b"There is no active session!\nanother error\n"[..],
    ] {
        assert_eq!(look_retry_delay("zellij", stderr, 1), None);
    }
    for attempt in 1..LOOK_ATTEMPTS {
        assert_eq!(
            look_retry_delay("tmux", FOCUS_SUPERSEDED.as_bytes(), attempt),
            Some(Duration::from_millis(200))
        );
    }
    assert_eq!(
        look_retry_delay("tmux", FOCUS_SUPERSEDED.as_bytes(), LOOK_ATTEMPTS),
        None
    );
}

#[test]
fn zellij_looks_at_a_tab_by_number_because_pane_focus_does_not_move_its_client() {
    let binary = Path::new("/dev/rimz");
    assert_eq!(
        look_argv(binary, "zellij", "room", 3, "zellij:terminal_6"),
        ["zellij", "--session", "room", "action", "go-to-tab", "3"]
    );
    assert_eq!(
        look_argv(binary, "tmux", "room", 3, "tmux:%3"),
        ["/dev/rimz", "--tmux", "pane", "focus", "tmux:%3"]
    );
}

#[test]
fn tab_number_follows_the_view_id_not_the_listing_order() {
    let panes: Panes = serde_json::from_str(
        r##"{"session":"private","tabs":[
            {"view_id":"tab_1","name":"rimzd","panes":[]},
            {"view_id":"tab_0","name":"rimz","panes":[]},
            {"name":"#probe","panes":[]}
        ]}"##,
    )
    .unwrap();
    let numbers: Vec<_> = panes
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| tab_number(tab, index))
        .collect();
    // The first two follow their own position; the unidentified tab falls back to its index.
    assert_eq!(numbers, [2, 1, 3]);
}

fn prepare_visit(room: &Room<'_>, script: &str) -> Tab {
    std::fs::write(room.cwd.join("focus.sh"), script).unwrap();
    let mut panes: Panes =
        serde_json::from_slice(&std::fs::read(room.cwd.join("panes.json")).unwrap()).unwrap();
    panes.tabs.remove(1)
}

#[test]
fn look_follows_a_replaced_sidebar_in_the_same_view() {
    with_ready_room(|room, record| {
        let tab = prepare_visit(
            room,
            r#"case "$1" in
    tmux:%3)
        cp replacement.json panes.json
        printf '%s\n' "can't find pane: %3" >&2
        exit 1 ;;
    tmux:%9) exit 0 ;;
esac
exit 1
"#,
        );
        std::fs::write(
            room.cwd.join("replacement.json"),
            r#"{"session":"private","tabs":[
                {"view_id":"@2","panes":[{"kind":"sidebar","pane_id":"tmux:%9"}]},
                {"view_id":"@1","panes":[{"kind":"sidebar","pane_id":"tmux:%1"}]}
            ]}"#,
        )
        .unwrap();

        let result = room.look(&record.session, 1, &tab);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(room.cwd.join("queries")).unwrap(),
            "--tmux pane focus tmux:%3\n--tmux pane list --json\n--tmux pane focus tmux:%9\n"
        );
    });
}

#[test]
fn look_refuses_a_missing_sidebar_or_unidentified_tab() {
    for (view_id, tabs, reason) in [
        (
            Some("@2"),
            serde_json::json!([{"view_id":"@2","panes":[{"kind":"shell","pane_id":"tmux:%9"}]}]),
            "lists no sidebar",
        ),
        (
            Some("@2"),
            serde_json::json!([{"view_id":"@1","panes":[{"kind":"sidebar","pane_id":"tmux:%9"}]}]),
            "tab @2 is gone",
        ),
        (
            None,
            serde_json::json!([{"panes":[{"kind":"sidebar","pane_id":"tmux:%9"}]}]),
            "tab has no view_id",
        ),
    ] {
        with_ready_room(|room, record| {
            let mut tab = prepare_visit(
                room,
                "cp replacement.json panes.json\nprintf '%s\\n' \"can't find pane: %3\" >&2\nexit 1\n",
            );
            tab.view_id = view_id.map(str::to_owned);
            std::fs::write(
                room.cwd.join("replacement.json"),
                serde_json::to_vec(&serde_json::json!({"session":"private","tabs":tabs})).unwrap(),
            )
            .unwrap();

            let error = room.look(&record.session, 1, &tab).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(reason), "{message}");
            assert!(message.contains("can't find pane: %3"), "{message}");
            assert_eq!(
                std::fs::read_to_string(room.cwd.join("queries")).unwrap(),
                "--tmux pane focus tmux:%3\n--tmux pane list --json\n"
            );
        });
    }
}

#[test]
fn look_preserves_a_failure_on_the_still_listed_first_sidebar() {
    with_ready_room(|room, record| {
        let tab = prepare_visit(room, "printf '%s\\n' 'permission denied' >&2\nexit 1\n");
        std::fs::write(
            room.cwd.join("panes.json"),
            r#"{"session":"private","tabs":[{"view_id":"@2","panes":[
                {"kind":"sidebar","pane_id":"tmux:%3"},
                {"kind":"sidebar","pane_id":"tmux:%9"}
            ]}]}"#,
        )
        .unwrap();
        let error = room.look(&record.session, 1, &tab).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("still lists visited pane tmux:%3"),
            "{message}"
        );
        assert!(message.contains("permission denied"), "{message}");
        assert_eq!(
            std::fs::read_to_string(room.cwd.join("queries")).unwrap(),
            "--tmux pane focus tmux:%3\n--tmux pane list --json\n"
        );
    });
}

#[test]
fn look_preserves_both_errors_when_the_roster_read_fails() {
    with_ready_room(|room, record| {
        let tab = prepare_visit(
            room,
            "touch roster-error\nprintf '%s\\n' \"can't find pane: %3\" >&2\nexit 1\n",
        );
        let error = room.look(&record.session, 1, &tab).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("can't find pane: %3"), "{message}");
        assert!(message.contains("roster re-read failed"), "{message}");
        assert!(message.contains("multiplexer unavailable"), "{message}");
        assert_eq!(
            std::fs::read_to_string(room.cwd.join("queries")).unwrap(),
            "--tmux pane focus tmux:%3\n--tmux pane list --json\n"
        );
    });
}

#[test]
fn look_retries_a_superseded_focus_without_reading_the_roster() {
    with_ready_room(|room, record| {
        let tab = prepare_visit(
            room,
            &format!(
                "if test ! -f superseded; then\n    touch superseded\n    printf '%s\\n' '{FOCUS_SUPERSEDED}' >&2\n    exit 1\nfi\n"
            ),
        );
        let result = room.look(&record.session, 1, &tab);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(room.cwd.join("queries")).unwrap(),
            "--tmux pane focus tmux:%3\n--tmux pane focus tmux:%3\n"
        );
    });
}

#[test]
fn look_bounds_replacements_and_superseded_focuses_together() {
    for superseded in [false, true] {
        with_ready_room(|room, record| {
            let tab = prepare_visit(
                room,
                &format!(
                    r#"if test -f superseded; then
    printf '%s\n' '{FOCUS_SUPERSEDED}' >&2
    exit 1
fi
cp "after-${{1##*%}}.json" panes.json
printf '%s\n' "can't find pane: $1" >&2
if test -f supersede-next; then touch superseded; fi
exit 1
"#,
                ),
            );
            for (before, after) in [(3, 9), (9, 10), (10, 11)] {
                std::fs::write(
                    room.cwd.join(format!("after-{before}.json")),
                    serde_json::to_vec(&serde_json::json!({
                        "session":"private",
                        "tabs":[{"view_id":"@2","panes":[{
                            "kind":"sidebar","pane_id":format!("tmux:%{after}")
                        }]}]
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            if superseded {
                std::fs::write(room.cwd.join("supersede-next"), "").unwrap();
            }

            let error = room.look(&record.session, 1, &tab).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("attempts exhausted"), "{message}");
            let last_error = if superseded {
                FOCUS_SUPERSEDED
            } else {
                "can't find pane: tmux:%10"
            };
            assert!(message.contains(last_error), "{message}");
            let queries = std::fs::read_to_string(room.cwd.join("queries")).unwrap();
            let expected = if superseded {
                "--tmux pane focus tmux:%3\n--tmux pane list --json\n--tmux pane focus tmux:%9\n--tmux pane focus tmux:%9\n"
            } else {
                "--tmux pane focus tmux:%3\n--tmux pane list --json\n--tmux pane focus tmux:%9\n--tmux pane list --json\n--tmux pane focus tmux:%10\n--tmux pane list --json\n"
            };
            assert_eq!(queries, expected);
        });
    }
}

#[cfg(target_os = "linux")]
fn failed_start(action: &str) -> (anyhow::Error, PathBuf) {
    let probe = tempfile::tempdir().unwrap();
    let binary = probe.path().join("rimz");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$HOME\" > \"${{0%/*}}/sandbox-home\"\n{action}\nexit 1\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = open(probe.path(), &binary, "tmux")
        .err()
        .expect("the preflight binary fails");
    let home = std::fs::read_to_string(probe.path().join("sandbox-home")).unwrap();
    let root = Path::new(home.trim()).parent().unwrap().to_path_buf();
    (error, root)
}

#[cfg(target_os = "linux")]
#[test]
fn failed_room_start_keeps_its_root_and_reports_the_step_error() {
    let (error, root) = failed_start("");
    let root_exists = root.exists();
    let marker = std::fs::read_to_string(root.join("kept"));
    if root_exists {
        std::fs::remove_dir_all(&root).unwrap();
    }

    assert!(root_exists, "a failed room start must leave its root");
    assert!(root.is_absolute());
    let note = error.to_string();
    let first_line = note.lines().next().unwrap();
    assert!(first_line.contains(&root.display().to_string()), "{note}");
    assert!(
        first_line.contains(&format!("rm -rf {}", quote(root.as_os_str()))),
        "{note}"
    );
    assert!(
        error
            .chain()
            .skip(1)
            .any(|cause| cause.to_string().contains("testkit preflight")),
        "{error:?}"
    );
    let marker = marker.unwrap();
    assert!(marker.contains("testkit preflight"), "{marker}");
    assert!(!marker.contains("rm -rf"), "{marker}");
}

#[cfg(target_os = "linux")]
#[test]
fn marker_write_failure_reports_the_reason_and_removes_the_root() {
    let (error, root) = failed_start("mkdir \"${HOME%/*}/kept\"");
    let root_exists = root.exists();
    if root_exists {
        std::fs::remove_dir_all(&root).unwrap();
    }

    assert!(
        !root_exists,
        "a failed marker write must not preserve the root"
    );
    let note = error.to_string();
    assert!(note.contains("could not keep root"), "{note}");
    assert!(note.contains(&root.display().to_string()), "{note}");
    assert!(note.contains("Is a directory"), "{note}");
    assert!(
        error
            .chain()
            .skip(1)
            .any(|cause| cause.to_string().contains("testkit preflight")),
        "{error:?}"
    );
}
