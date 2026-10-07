use super::*;

#[cfg(unix)]
use super::backend::{off_spec_sidebars, reconcile_pane};
pub(crate) mod support;

#[cfg(unix)]
use self::support::{command_count, shim_log, zellij_shim};

#[cfg(unix)]
use crate::config::MultiplexerConfig;
#[cfg(unix)]
use crate::disk::paths::RuntimePaths;
#[cfg(unix)]
use crate::ids::{PaneId, WorkspaceId};
#[cfg(unix)]
use crate::mux::zellij::pane_topology::{
    PaneTopologyCache, PaneTopologyPane, TopologyClients, write_pane_topology_cache,
};
#[cfg(unix)]
use crate::mux::{
    LayoutColumn, LayoutPanes, MuxBackend, PaneCmd, PaneListOptions, PaneReadConsistency,
    ReconcilePaneRole, SessionHealth, SidebarLiveness, SidebarPaneOptions, SidebarWidth,
    SplitDirection, SplitPaneOptions, SplitPlacement, SplitTarget, TabOptions, WidthSyncOptions,
};
#[cfg(unix)]
use crate::utils::time::unix_now_ms;

#[test]
fn tab_move_count_places_new_last_tab_after_anchor() {
    assert_eq!(backend::moves_to_place_after(2, 5), 1);
    assert_eq!(backend::moves_to_place_after(3, 5), 0);
    assert_eq!(backend::moves_to_place_after(4, 5), 0);
    assert_eq!(backend::moves_to_place_after(8, 5), 0);
}

#[test]
fn pane_short_name_uses_program_basename() {
    assert_eq!(
        pane_short_name(&[
            "/opt/rimz".to_owned(),
            "agents".to_owned(),
            "exec".to_owned(),
            "claude".to_owned(),
        ]),
        Some("rimz".to_owned()),
    );
    assert_eq!(
        pane_short_name(&["/usr/bin/zsh".to_owned(), "-l".to_owned()]),
        Some("zsh".to_owned()),
    );
    assert_eq!(pane_short_name(&[]), None);
}

#[cfg(unix)]
#[test]
fn send_keys_separates_dash_leading_text_from_zellij_options() {
    let (temp, shim) = support::logging_shim();
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");

    backend
        .send_keys(&pane, "room-a", "- keep the open questions")
        .expect("send literal text");

    assert_eq!(
        shim_log(&temp).trim(),
        "--session room-a action write-chars --pane-id terminal_7 -- - keep the open questions"
    );
}

#[cfg(unix)]
#[test]
fn every_pane_io_action_names_the_session() {
    let (temp, shim) = support::logging_shim();
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    let session = "room-a";

    backend.send_keys(&pane, session, "hi").expect("send keys");
    backend
        .send_key(&pane, session, crate::pane::keys::NamedKey::Enter)
        .expect("send key");
    backend.paste_text(&pane, session, "hi").expect("paste");
    backend
        .capture_pane(&pane, session, None, false)
        .expect("capture");

    let log = shim_log(&temp);
    let verbs = log
        .lines()
        .map(|line| {
            line.strip_prefix("--session room-a action ")
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or_else(|| panic!("session-less pane action: {line}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(verbs, ["write-chars", "write", "write", "dump-screen"]);
}

#[cfg(unix)]
#[test]
fn paste_text_chunks_large_byte_streams_without_changing_bytes() {
    use crate::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN};

    for text in [
        String::new(),
        "x".repeat(100),
        "é\r\n\0\x1b[200~".repeat(2048),
    ] {
        let (temp, shim) = support::logging_shim();
        let backend = ZellijBackend::with_program_for_test(&shim);
        let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
        backend
            .paste_text(&pane, "room-a", &text)
            .expect("paste text");
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        let expected =
            format!("{BRACKET_PASTE_OPEN}{normalized}{BRACKET_PASTE_CLOSE}").into_bytes();
        let log = shim_log(&temp);
        assert_eq!(
            log.lines().count(),
            expected.len().div_ceil(ZELLIJ_WRITE_CHUNK)
        );
        let mut actual = Vec::new();
        for line in log.lines() {
            let bytes = line
                .strip_prefix("--session room-a action write --pane-id terminal_7 ")
                .expect("targeted byte write")
                .split_whitespace()
                .map(|byte| byte.parse::<u8>().expect("decimal byte"))
                .collect::<Vec<_>>();
            assert!(bytes.len() <= ZELLIJ_WRITE_CHUNK);
            actual.extend(bytes);
        }
        assert_eq!(actual, expected);
    }
}

#[cfg(unix)]
#[test]
fn paste_text_closes_after_second_failed_zellij_chunk() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ -e "$dir/first-chunk" ]; then
    if [ -e "$dir/failed-chunk" ]; then
        printf 'close failed\n' >&2
        exit 2
    fi
    touch "$dir/failed-chunk"
    printf 'chunk failed\n' >&2
    exit 1
fi
touch "$dir/first-chunk"
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    let err = backend
        .paste_text(&pane, "room-a", &"x".repeat(20 * 1024))
        .expect_err("second chunk fails");
    assert!(err.to_string().contains("chunk failed"));
    let log = shim_log(&temp);
    let writes = log
        .lines()
        .map(|line| {
            line.strip_prefix("--session room-a action write --pane-id terminal_7 ")
                .expect("targeted byte write")
                .split_whitespace()
                .map(|byte| byte.parse::<u8>().expect("decimal byte"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        writes.len(),
        3,
        "no body chunks after the failed second chunk"
    );
    let expected = format!(
        "{}{}",
        crate::pane::keys::BRACKET_PASTE_OPEN,
        "x".repeat(20 * 1024)
    );
    assert_eq!(writes[0], expected.as_bytes()[..ZELLIJ_WRITE_CHUNK]);
    assert_eq!(
        writes[1],
        expected.as_bytes()[ZELLIJ_WRITE_CHUNK..2 * ZELLIJ_WRITE_CHUNK]
    );
    assert_eq!(writes[2], crate::pane::keys::BRACKET_PASTE_CLOSE.as_bytes());
}

#[cfg(unix)]
struct TestRoom {
    runtime_root: tempfile::TempDir,
    project_root: tempfile::TempDir,
    workspace_id: WorkspaceId,
    runtime: RuntimePaths,
}

#[cfg(unix)]
impl TestRoom {
    fn new() -> Self {
        let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
        let project_root = tempfile::TempDir::new().expect("project tempdir");
        let workspace_id = WorkspaceId::from_project_root(project_root.path());
        let runtime =
            RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        Self {
            runtime_root,
            project_root,
            workspace_id,
            runtime,
        }
    }

    fn backend(&self, shim: &std::path::Path) -> ZellijBackend {
        ZellijBackend::with_program_and_runtime_for_test(shim, self.runtime_root.path())
            .with_presence_plugin_for_test(shim)
    }

    fn publish_workspace(&self) {
        let state = crate::disk::paths::StatePaths::under(
            self.workspace_id.clone(),
            self.runtime_root.path(),
        )
        .unwrap();
        state.ensure_dirs().unwrap();
        crate::workspace::record::write(
            &state,
            &crate::workspace::record::WorkspaceRecord {
                layout: 2,
                workspace_id: self.workspace_id.clone(),
                project_root: self.project_root.path().to_path_buf(),
                worktree_root: None,
                session_name: "rimz-test".to_owned(),
                root_class: crate::workspace::RootClass::Directory,
                rimz_bin: None,
                rimz_build: None,
                pins: crate::ids::RoomLogins::new(),
                updated_at: jiff::Timestamp::now(),
            },
        )
        .unwrap();
    }

    fn write_cache(
        &self,
        produced_at_ms: u64,
        focused_pane: Option<u64>,
        clients: Option<TopologyClients>,
        panes: Vec<PaneTopologyPane>,
    ) {
        write_pane_topology_cache(
            &self.runtime,
            &PaneTopologyCache {
                session_name: "rimz-test".to_owned(),
                produced_at_ms,
                writer: None,
                focused_pane,
                clients,
                panes,
            },
        )
        .expect("write topology cache");
    }

    fn sidebar_options(&self, view_cols: u16) -> SidebarPaneOptions {
        let width = SidebarWidth::default();
        let view_cols = std::num::NonZeroU16::new(view_cols).expect("nonzero test view");
        let requested_cols = std::num::NonZeroU16::new(
            u16::try_from(width.target_cols(u64::from(view_cols.get()))).expect("test target"),
        )
        .expect("nonzero test width");
        let share = crate::mux::WidthPermille::from_cols(requested_cols, view_cols);
        SidebarPaneOptions {
            runtime: self.runtime.clone(),
            session_name: "rimz-test".to_owned(),
            workspace_id: self.workspace_id.clone(),
            project_root: self.project_root.path().to_path_buf(),
            extra_env: Default::default(),
            cwd: self.project_root.path().to_path_buf(),
            target: crate::mux::SidebarTarget {
                share,
                max_cols: width.max_cols,
                pinned: false,
            },
            detected_view_size: None,
            rimz_bin: "rimz".into(),
            pristine_birth: false,
            config: MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        }
    }
}

#[test]
fn existing_session_attach_has_no_creation_or_options_tail() {
    let spec = ZellijBackend::default().attach_existing_command("rimz-test");

    assert_eq!(spec.args, ["attach", "rimz-test"]);
    assert!(!spec.args.iter().any(|arg| arg == "--create"));
    assert!(!spec.args.iter().any(|arg| arg == "options"));
}

#[test]
fn readonly_attach_relies_on_the_broadcast_ttyd_input_boundary() {
    let spec = ZellijBackend::default().attach_readonly_command("rimz-test");

    assert_eq!(spec.args, ["attach", "rimz-test"]);
}

#[cfg(unix)]
#[test]
fn live_session_that_fails_native_probe_is_unresponsive() {
    let room = TestRoom::new();
    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" action list-panes --all --json "*) exit 1 ;;
esac
exit 0
"#,
    );

    let health = room
        .backend(&shim)
        .with_health_probe_timeout_for_test(Duration::from_millis(50))
        .ensure_clean_session(&room.sidebar_options(120), None)
        .expect("classify live session");

    assert_eq!(health, SessionHealth::Unresponsive);
}

#[cfg(unix)]
#[test]
fn live_session_native_probe_accepts_success_before_deadline() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; marker="$dir/probe-failed"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" action list-panes --all --json "*)
    if [ ! -e "$marker" ]; then : > "$marker"; exit 1; fi
    printf '[]\n'; exit 0 ;;
esac
exit 0
"#,
    );

    let health = room
        .backend(&shim)
        .with_health_probe_timeout_for_test(Duration::from_millis(500))
        .ensure_clean_session(&room.sidebar_options(120), None)
        .expect("classify live session");

    assert_eq!(health, SessionHealth::Healthy);
    assert_eq!(
        command_count(&shim_log(&temp), "action list-panes --all --json"),
        2,
    );
}

const LIST_PANES: &str = "action list-panes --all --json";

#[cfg(unix)]
#[test]
fn pane_content_size_reads_terminal_content_not_outer_geometry() {
    let (temp, shim) = support::pane_roster_shim(
        r#"[{"id":7,"is_plugin":true,"pane_content_rows":1,"pane_content_columns":2},{"id":7,"pane_rows":40,"pane_columns":120,"pane_content_rows":38,"pane_content_columns":118}]"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    assert_eq!(
        backend
            .pane_content_size(&pane, Some("room-a"), Duration::from_secs(2))
            .ok(),
        Some(Some(crate::mux::PaneContentSize {
            rows: 38,
            cols: 118
        })),
    );
    assert_eq!(
        shim_log(&temp).trim(),
        "--session room-a action list-panes --all --json"
    );
}

#[cfg(unix)]
#[test]
fn pane_content_size_preserves_zero_and_missing_panes() {
    let (_temp, shim) =
        support::pane_roster_shim(r#"[{"id":7,"pane_content_rows":0,"pane_content_columns":118}]"#);
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    assert_eq!(
        backend
            .pane_content_size(&pane, Some("room-a"), Duration::from_secs(2))
            .ok(),
        Some(Some(crate::mux::PaneContentSize { rows: 0, cols: 118 }))
    );
    let missing = PaneId::from_parts(crate::MuxName::Zellij, "terminal_8");
    assert_eq!(
        backend
            .pane_content_size(&missing, Some("room-a"), Duration::from_secs(2))
            .ok(),
        Some(None)
    );
}

#[cfg(unix)]
#[test]
fn pane_room_check_reruns_a_transient_empty_listing() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
case $(wc -l < "$dir/zellij.log") in
  1) exit 0 ;;
  2) printf ' \n'; exit 0 ;;
esac
printf '[{"id":0,"is_plugin":false}]\n'
"#,
    );

    ZellijBackend::with_program_for_test(&shim)
        .require_pane_in_session(
            &PaneId::from_parts(crate::MuxName::Zellij, "terminal_0"),
            "rimz-test",
        )
        .expect("the pane is in the room once the listing answers");

    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 3);
}

#[cfg(unix)]
#[test]
fn pane_listing_reruns_only_an_empty_success() {
    use crate::mux::MuxErr;

    let (temp, shim) = support::pane_roster_shim("[]");
    let listed = ZellijBackend::with_program_for_test(&shim)
        .raw_listed_panes("rimz-test", Duration::from_secs(5))
        .expect("an empty roster is an answer");
    assert!(listed.is_empty());
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 1);

    let (temp, shim) = support::pane_roster_shim("[{");
    let err = ZellijBackend::with_program_for_test(&shim)
        .raw_listed_panes("rimz-test", Duration::from_secs(5))
        .expect_err("malformed JSON is not an answer");
    assert!(matches!(err, MuxErr::Output { .. }), "{err}");
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 1);

    let (temp, shim) = support::failing_roster_shim();
    let err = ZellijBackend::with_program_for_test(&shim)
        .raw_listed_panes("rimz-test", Duration::from_secs(5))
        .expect_err("a failed listing is not an answer");
    assert!(matches!(err, MuxErr::Command { .. }), "{err}");
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 1);

    let (temp, shim) = support::logging_shim();
    let err = ZellijBackend::with_program_for_test(&shim)
        .raw_listed_panes("rimz-test", Duration::from_secs(5))
        .expect_err("a listing that stays empty is not an answer");
    assert!(
        err.to_string()
            .contains("`list-panes --all --json` returned no output on every attempt"),
        "{err}"
    );
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 5);
}

#[cfg(unix)]
#[test]
fn a_failed_session_listing_does_not_accept_an_agent_close() {
    let (_temp, shim) = zellij_shim("#!/bin/sh\nprintf 'rimz-test [Created 1s ago]\\n'\n");
    assert!(ZellijBackend::with_program_for_test(&shim).session_accepts_agent_close("rimz-test"));

    let (_temp, shim) = zellij_shim("#!/bin/sh\necho 'socket refused' >&2\nexit 1\n");
    assert!(!ZellijBackend::with_program_for_test(&shim).session_accepts_agent_close("rimz-test"));
}

#[cfg(unix)]
#[test]
fn pane_listing_rerun_stays_inside_the_caller_budget() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$(wc -l < "$dir/zellij.log")" -eq 1 ]; then sleep 1; exit 0; fi
exec sleep 30
"#,
    );
    let started = std::time::Instant::now();

    let err = ZellijBackend::with_program_for_test(&shim)
        .raw_listed_panes("rimz-test", Duration::from_secs(2))
        .expect_err("the rerun hangs");

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the rerun restarted the budget: {:?}",
        started.elapsed()
    );
    assert!(
        matches!(err, crate::mux::MuxErr::Timeout { seconds: 2, .. }),
        "{err}"
    );
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 2);
}

#[cfg(unix)]
#[test]
fn rename_tab_resolves_the_anchor_to_a_stable_id() {
    use crate::mux::tab_name::TabNameIntent;

    for intent in [
        TabNameIntent::Claim {
            pane_name: "-opus".to_owned(),
        },
        TabNameIntent::Status {
            observed: "#feat".to_owned(),
        },
        TabNameIntent::Rest {
            observed: "#feat".to_owned(),
        },
        TabNameIntent::Rebuild {
            observed: "#feat".to_owned(),
            base: "#feat".to_owned(),
        },
        TabNameIntent::Release {
            observed: "#feat".to_owned(),
        },
    ] {
        let room = TestRoom::new();
        room.publish_workspace();
        let claim = matches!(intent, TabNameIntent::Claim { .. });
        let release = matches!(intent, TabNameIntent::Release { .. });
        let writes_ownership = !matches!(
            intent,
            TabNameIntent::Status { .. } | TabNameIntent::Rest { .. }
        );
        let (temp, shim) = support::pane_roster_shim(
            r##"[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3,"tab_name":"#feat","is_focused":false},{"id":8,"is_plugin":false,"tab_id":43,"tab_position":1,"is_focused":true}]"##,
        );
        let backend = room.backend(&shim);
        let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
        let original = crate::mux::tab_name::TabOwnerRecord {
            base: "#feat".to_owned(),
            founders: vec![PaneId::from_parts(crate::MuxName::Zellij, "terminal_99")],
        };
        let path = room.runtime.lane_path("tab-owners.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "session_name": "rimz-test",
                "tabs": {"42": original, "43": {"base": "peer", "founders": []}, "99": original},
            }))
            .unwrap(),
        )
        .unwrap();

        backend
            .rename_tab("rimz-test", &pane, "#feat ✓", intent)
            .expect("rename by stable tab id");

        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if release {
            assert!(
                file["tabs"]["42"].is_null(),
                "Release clears the anchor's ownership"
            );
        } else {
            let owner: crate::mux::tab_name::TabOwnerRecord =
                serde_json::from_value(file["tabs"]["42"].clone()).unwrap();
            assert_eq!(owner.base, "#feat", "recorded base excludes the glyph");
            assert_eq!(
                owner.founders,
                if claim {
                    vec![pane.clone()]
                } else {
                    original.founders
                }
            );
        }
        assert_eq!(
            file["tabs"]["43"]["base"], "peer",
            "live peer ownership survives"
        );
        assert_eq!(
            file["tabs"]["99"].is_null(),
            writes_ownership,
            "only owner writes prune dead tabs"
        );

        let log = shim_log(&temp);
        assert!(
            log.contains("--session rimz-test action rename-tab-by-id 42 #feat ✓"),
            "{log}"
        );
        assert_eq!(
            command_count(&log, "action rename-pane"),
            usize::from(claim)
        );
        if claim {
            assert!(
                log.contains(
                    "--session rimz-test action rename-pane --pane-id terminal_7 -- -opus"
                ),
                "{log}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn projected_rename_skips_a_tab_renamed_since_its_observation() {
    use crate::mux::tab_name::TabNameIntent;

    for intent in [
        TabNameIntent::Status {
            observed: "shell ?".to_owned(),
        },
        TabNameIntent::Rest {
            observed: "shell ?".to_owned(),
        },
        TabNameIntent::Rebuild {
            observed: "shell ?".to_owned(),
            base: "shell".to_owned(),
        },
        TabNameIntent::Release {
            observed: "shell ?".to_owned(),
        },
    ] {
        let room = TestRoom::new();
        room.publish_workspace();
        let (temp, shim) = support::pane_roster_shim(
            r#"[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3,"tab_name":"opus","is_focused":true}]"#,
        );
        let backend = room.backend(&shim);
        let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
        let path = room.runtime.lane_path("tab-owners.json");
        let record = serde_json::to_vec(&serde_json::json!({
            "version": 1, "session_name": "rimz-test",
            "tabs": {"42": {"base": "opus", "founders": [pane]}},
        }))
        .unwrap();
        std::fs::write(&path, &record).unwrap();

        backend
            .rename_tab("rimz-test", &pane, "shell", intent)
            .expect("a stale projection is a silent miss");

        let log = shim_log(&temp);
        assert_eq!(command_count(&log, "action rename-tab-by-id"), 0, "{log}");
        assert_eq!(
            std::fs::read(path).unwrap(),
            record,
            "stale projections leave ownership untouched"
        );
    }
}

#[cfg(unix)]
#[test]
fn rebuild_changes_only_the_owned_base() {
    let room = TestRoom::new();
    room.publish_workspace();
    let (_temp, shim) = support::pane_roster_shim(
        r#"[{"id":7,"tab_id":42,"tab_position":3,"tab_name":"debugger"}]"#,
    );
    let backend = room.backend(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    let path = room.runtime.lane_path("tab-owners.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "session_name": "rimz-test",
            "tabs": {"42": {"base": "debugger", "founders": ["zellij:terminal_1"]}},
        }))
        .unwrap(),
    )
    .unwrap();
    backend
        .rename_tab(
            "rimz-test",
            &pane,
            "brainstormer ✓",
            crate::mux::tab_name::TabNameIntent::Rebuild {
                observed: "debugger".to_owned(),
                base: "brainstormer".to_owned(),
            },
        )
        .unwrap();
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["tabs"]["42"]["base"], "brainstormer");
    assert_eq!(
        file["tabs"]["42"]["founders"],
        serde_json::json!(["zellij:terminal_1"])
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "session_name": "rimz-test", "tabs": {},
        }))
        .unwrap(),
    )
    .unwrap();
    backend
        .rename_tab(
            "rimz-test",
            &pane,
            "brainstormer ✓",
            crate::mux::tab_name::TabNameIntent::Rebuild {
                observed: "debugger".to_owned(),
                base: "brainstormer".to_owned(),
            },
        )
        .unwrap();
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        file["tabs"].as_object().unwrap().is_empty(),
        "Rebuild does not claim an unowned tab"
    );
}

#[cfg(unix)]
#[test]
fn ownership_rename_holds_the_lock_while_listing_and_renaming() {
    let room = TestRoom::new();
    room.publish_workspace();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
case " $* " in
  *" action list-panes "*)
    touch "$dir/listing"
    while [ ! -f "$dir/listing-continue" ]; do sleep 0.01; done
    printf '%s\n' '[{"id":7,"tab_id":42,"tab_name":"debugger"}]' ;;
  *" action rename-tab-by-id "*)
    touch "$dir/renaming"
    while [ ! -f "$dir/renaming-continue" ]; do sleep 0.01; done ;;
esac
"#,
    );
    let backend = room.backend(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    let lock_path = room.runtime.lock_path("tab-owners.lock");
    let mut held = Vec::new();
    std::thread::scope(|scope| {
        let rename = scope.spawn(|| {
            backend.rename_tab(
                "rimz-test",
                &pane,
                "brainstormer",
                crate::mux::tab_name::TabNameIntent::Rebuild {
                    observed: "debugger".to_owned(),
                    base: "brainstormer".to_owned(),
                },
            )
        });
        for stage in ["listing", "renaming"] {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !temp.path().join(stage).exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "rename did not reach {stage}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            held.push(
                crate::disk::lock::WorkspaceLock::try_acquire(&lock_path)
                    .unwrap()
                    .is_none(),
            );
            std::fs::write(temp.path().join(format!("{stage}-continue")), b"").unwrap();
        }
        rename.join().unwrap().unwrap();
    });
    assert_eq!(
        held,
        [true, true],
        "both decisions must exclude another owner writer"
    );
}

#[cfg(unix)]
#[test]
fn list_panes_joins_ownership_by_stable_id_on_cached_and_native_paths() {
    let room = TestRoom::new();
    room.publish_workspace();
    let panes = r#"[{"id":7,"tab_id":42,"tab_position":3,"stable_tab_id":42,"tab_name":"debugger"},{"id":8,"tab_id":43,"tab_position":4,"tab_name":"legacy"}]"#;
    let (_temp, shim) = support::pane_roster_shim(panes);
    let backend = room.backend(&shim);
    // The legacy pane lacks the new identity in the cache, but the native row has it.
    let cached = panes
        .replace("\"tab_id\":42,", "")
        .replace("\"tab_id\":43,", "");
    room.write_cache(
        unix_now_ms(),
        None,
        None,
        serde_json::from_str(&cached).unwrap(),
    );
    let owner = crate::mux::tab_name::TabOwnerRecord {
        base: "debugger".to_owned(),
        founders: vec![PaneId::from_parts(crate::MuxName::Zellij, "terminal_7")],
    };
    std::fs::write(
        room.runtime.lane_path("tab-owners.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "session_name": "rimz-test", "tabs": {"42": owner, "43": owner},
        }))
        .unwrap(),
    )
    .unwrap();
    for consistency in [
        PaneReadConsistency::Cached,
        PaneReadConsistency::RequireAuthoritative,
    ] {
        let listing = backend
            .list_panes(PaneListOptions {
                session_name: Some("rimz-test".to_owned()),
                runtime_paths: Some(room.runtime.clone()),
                workspace_id: Some(room.workspace_id.clone()),
                consistency,
                ..Default::default()
            })
            .unwrap();
        let views = listing
            .views
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            views.get("tab_3").and_then(|view| view.owner.as_ref()),
            Some(&owner)
        );
        assert_eq!(
            views.get("tab_4").and_then(|view| view.owner.as_ref()),
            (consistency == PaneReadConsistency::RequireAuthoritative).then_some(&owner)
        );
    }
}

#[cfg(unix)]
fn terminal_pane(
    id: u64,
    tab_position: u64,
    pane_columns: u64,
    pane_x: u64,
    title: &str,
) -> PaneTopologyPane {
    PaneTopologyPane {
        id,
        is_plugin: false,
        is_fullscreen: false,
        is_held: false,
        exited: false,
        is_suppressed: false,
        is_floating: false,
        tab_position,
        stable_tab_id: None,
        tab_name: Some("work".to_owned()),
        pane_columns: Some(pane_columns),
        pane_x: Some(pane_x),
        title: Some(title.to_owned()),
        pane_command: None,
        pane_cwd: None,
        pane_pid: None,
        terminal_command: None,
    }
}

#[cfg(unix)]
#[test]
fn fullscreen_tabs_hold_sidebar_geometry_reconcile() {
    let sidebar = terminal_pane(1, 0, 200, 0, crate::pane::SIDEBAR_CHROME_TITLE);
    let mut work = terminal_pane(2, 0, 120, 80, "zsh");
    let panes = [sidebar.clone(), work.clone()];
    assert_eq!(off_spec_sidebars(&panes, &[], None), vec![(0, 1)]);

    work.is_fullscreen = true;
    assert!(off_spec_sidebars(&[sidebar, work], &[], None).is_empty());
}

#[cfg(unix)]
#[test]
fn reconcile_mapping_classifies_native_sidebar_daemon_and_work_panes() {
    let mut sidebar = terminal_pane(1, 4, 20, 0, crate::pane::SIDEBAR_CHROME_TITLE);
    sidebar.is_held = true;
    let mut named_daemon = terminal_pane(2, 5, 80, 20, "host");
    named_daemon.tab_name = Some(crate::pane::VIEW_NAME.to_owned());
    let mut command_daemon = terminal_pane(3, 6, 80, 20, "host");
    command_daemon.terminal_command = Some("rimz app-server serve".to_owned());
    let mut work = terminal_pane(4, 7, 80, 20, "work");
    work.exited = true;
    let mut plugin = terminal_pane(5, 8, 80, 20, "plugin");
    plugin.is_plugin = true;

    let mapped = [
        reconcile_pane(&sidebar).expect("held sidebar remains structural"),
        reconcile_pane(&named_daemon).expect("named daemon remains structural"),
        reconcile_pane(&command_daemon).expect("command daemon remains structural"),
        reconcile_pane(&work).expect("exited work pane remains structural"),
    ];
    assert_eq!(mapped[0].role, ReconcilePaneRole::Sidebar);
    assert_eq!(mapped[1].role, ReconcilePaneRole::DaemonHost);
    assert_eq!(mapped[2].role, ReconcilePaneRole::DaemonHost);
    assert_eq!(mapped[3].role, ReconcilePaneRole::Working);
    assert!(reconcile_pane(&plugin).is_none());
}

fn option_map(args: &[String]) -> std::collections::BTreeMap<&str, &str> {
    assert!(
        args.len().is_multiple_of(2),
        "option argv must be pairs: {args:?}"
    );
    args.chunks_exact(2)
        .map(|pair| (pair[0].as_str(), pair[1].as_str()))
        .collect()
}

fn expected_option_map(spec: &str) -> std::collections::BTreeMap<&str, &str> {
    spec.split_whitespace()
        .map(|entry| entry.split_once('=').expect("flag=value"))
        .collect()
}

#[cfg(unix)]
#[test]
fn split_pane_routes_directional_and_anchored_requests() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3}]\n' ;;
esac
exit 0
"#,
    );
    let backend =
        ZellijBackend::with_program_for_test(&shim).with_ambient_session_for_test("caller");
    for direction in [SplitDirection::Right, SplitDirection::Down] {
        let created = backend
            .split_pane(SplitPaneOptions {
                placement: SplitPlacement::Directional(direction),
                focus: true,
                title: Some("rimz managed pane".to_owned()),
                ..Default::default()
            })
            .expect("directional split");
        assert_eq!(created, None, "no printed id is still a successful split");
    }
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Directional(SplitDirection::Right),
            focus: true,
            ..Default::default()
        })
        .expect("explicit session-pane split");
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Directional(SplitDirection::Right),
            focus: false,
            ..Default::default()
        })
        .expect("background directional split");
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Stacked,
            focus: false,
            ..Default::default()
        })
        .expect("anchored stack");

    let log = shim_log(&temp);
    let shell = crate::proc::shell_pane_name();
    for command in [
        "--session caller action new-pane --direction right --name rimz managed pane",
        "--session caller action new-pane --direction down --name rimz managed pane",
    ] {
        assert!(log.contains(command), "{log}");
    }
    assert_eq!(
        command_count(
            &log,
            &format!(
                "--session rimz-test action new-pane --direction right --no-focus --name {shell} | pane=7"
            )
        ),
        2,
        "focus-taking and background splits both name the exact anchor:\n{log}"
    );
    let anchored = log.lines().last().expect("anchored command");
    assert!(
        anchored.contains(&format!(
            "action new-pane --stacked --no-focus --name {shell} | pane=7"
        )),
        "{log}"
    );
    assert!(
        !log.contains("--tab-id") && !log.contains("focus-pane-id"),
        "{log}"
    );
    assert_eq!(log.lines().count(), 5, "expected split calls:\n{log}");
}

#[cfg(unix)]
#[test]
fn split_pane_focuses_the_printed_pane_after_an_exact_anchor_spawn() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
case " $* " in
  *" action new-pane "*) printf 'terminal_9\n' ;;
  *" action focus-pane-id "*) exit 1 ;;
esac
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    for focus in [true, false] {
        let created = backend
            .split_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: "rimz-test".to_owned(),
                    pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
                },
                placement: SplitPlacement::Directional(SplitDirection::Down),
                focus,
                ..Default::default()
            })
            .expect("a spawned pane outlives its failed focus jump");
        assert_eq!(
            created,
            Some(PaneId::from_parts(crate::MuxName::Zellij, "terminal_9"))
        );
    }

    let log = shim_log(&temp);
    let lines = log.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "{log}");
    assert!(
        lines[0].contains("action new-pane --direction down --no-focus")
            && lines[0].ends_with("pane=7"),
        "{log}"
    );
    assert!(
        lines[1].starts_with("--session rimz-test action focus-pane-id terminal_9 "),
        "{log}"
    );
    assert_eq!(lines[2], lines[0], "{log}");
}

#[cfg(unix)]
#[test]
fn split_pane_focus_taking_directional_split_keeps_the_tab_anchor_below_zellij_045() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes "*) printf '[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3}]\n' ;;
  *" action new-pane "*) printf 'terminal_9\n' ;;
esac
exit 0
"#,
    );
    let created = ZellijBackend::with_program_for_test(&shim)
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Directional(SplitDirection::Down),
            focus: true,
            ..Default::default()
        })
        .expect("legacy focus-taking split");
    assert_eq!(
        created,
        Some(PaneId::from_parts(crate::MuxName::Zellij, "terminal_9"))
    );

    let log = shim_log(&temp);
    assert!(
        log.contains("action new-pane --direction down --tab-id 42 "),
        "{log}"
    );
    assert!(
        !log.contains("--no-focus") && !log.contains("focus-pane-id"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn split_pane_defaults_to_command_title_and_prefixes_environment() {
    let (temp, shim) = support::logging_shim();
    ZellijBackend::with_program_for_test(&shim)
        .with_ambient_session_for_test("caller")
        .split_pane(SplitPaneOptions {
            focus: true,
            command: Some(vec!["/usr/bin/sleep".to_owned(), "600".to_owned()]),
            env: [("RIMZ_TEST_VALUE".to_owned(), "present".to_owned())].into(),
            ..Default::default()
        })
        .expect("command split");
    assert_eq!(
        shim_log(&temp).trim(),
        "--session caller action new-pane --direction right --name sleep -- env RIMZ_TEST_VALUE=present /usr/bin/sleep 600"
    );
}

#[cfg(unix)]
fn assert_companion_boundary_resizes(stacked: bool, whole_boundary: bool) {
    let pane = |id, x, y, cols, rows| {
        serde_json::json!({"id": id, "tab_id": 42, "pane_x": x, "pane_y": y,
            "pane_columns": cols, "pane_rows": rows})
    };
    let (before, opened, resized) = if stacked {
        (
            vec![pane(7, 30, 0, 120, 40), pane(9, 30, 40, 120, 40)],
            vec![
                pane(7, 30, 0, 80, 40),
                pane(9, 30, 40, 80, 40),
                pane(8, 110, 0, 40, 80),
            ],
            vec![
                pane(7, 30, 0, 60, 40),
                pane(9, 30, 40, if whole_boundary { 60 } else { 80 }, 40),
                pane(8, 90, 0, 60, 80),
            ],
        )
    } else {
        (
            vec![pane(7, 30, 0, 120, 80)],
            vec![pane(7, 30, 0, 65, 80), pane(8, 95, 0, 55, 80)],
            vec![pane(7, 30, 0, 50, 80), pane(8, 80, 0, 70, 80)],
        )
    };
    let balanced = vec![
        pane(7, 30, 0, 60, 40),
        pane(9, 30, 40, 60, 40),
        pane(8, 90, 0, 60, 80),
    ];
    let (temp, shim) = zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
case " $* " in
  *" new-pane "*) touch "$dir/opened" ;;
  *" resize "*)
    if [ -f "$dir/resized" ]; then touch "$dir/balanced"; fi
    touch "$dir/resized" ;;
  *" list-panes "*)
    if [ -f "$dir/balanced" ]; then printf '%s\n' '{balanced}';
    elif [ -f "$dir/resized" ]; then printf '%s\n' '{resized}';
    elif [ -f "$dir/opened" ]; then printf '%s\n' '{opened}';
    else printf '%s\n' '{before}'; fi ;;
esac
"#,
        before = serde_json::to_string(&before).unwrap(),
        opened = serde_json::to_string(&opened).unwrap(),
        resized = serde_json::to_string(&resized).unwrap(),
        balanced = serde_json::to_string(&balanced).unwrap(),
    ));
    let result = ZellijBackend::with_program_for_test(&shim)
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            ..Default::default()
        })
        .expect("companion append");
    assert_eq!(result, crate::mux::CompanionPaneAppend::Opened);
    let log = shim_log(&temp);
    let actions = log
        .lines()
        .filter(|line| line.contains("action resize") || line.contains("action list-panes"))
        .collect::<Vec<_>>();
    let list = "--session rimz-test action list-panes --all --json";
    let decrease = "--session rimz-test action resize decrease right --pane-id terminal_7";
    let expected = if !stacked {
        vec![
            list,
            list,
            decrease,
            list,
            "--session rimz-test action resize increase right --pane-id terminal_7",
        ]
    } else if whole_boundary {
        vec![list, list, decrease, list, list]
    } else {
        vec![
            list,
            list,
            decrease,
            list,
            "--session rimz-test action resize decrease right --pane-id terminal_9",
            list,
        ]
    };
    assert_eq!(actions, expected, "{log}");
    assert_eq!(command_count(&log, "action new-pane"), 1, "{log}");
}

#[cfg(unix)]
#[test]
fn companion_balance_undoes_overshoot_on_the_same_pane_and_stops() {
    // fec66d600: preserve the closer grid when a native resize overshoots.
    assert_companion_boundary_resizes(false, false);
}

#[cfg(unix)]
#[test]
fn companion_balance_walks_both_boundary_segments_after_rereading() {
    // fec66d600: each independently resizable segment needs its own native step.
    assert_companion_boundary_resizes(true, false);
}

#[cfg(unix)]
#[test]
fn companion_balance_skips_a_boundary_segment_already_moved() {
    // fec66d600: do not resize twice when the native step moved the whole boundary.
    assert_companion_boundary_resizes(true, true);
}

#[cfg(unix)]
#[test]
fn companion_append_counts_held_terminals_before_spawning() {
    let panes = (0..8)
        .map(|id| {
            serde_json::json!({
                "id": id, "tab_id": 42, "is_held": true, "exited": true,
                "pane_x": (id / 4) * 60, "pane_y": (id % 4) * 20,
                "pane_columns": 60, "pane_rows": 20,
            })
        })
        .collect::<Vec<_>>();
    let (temp, shim) = zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
printf '%s\n' "$*" >> "$dir/zellij.log"
printf '%s\n' '{}'
"#,
        serde_json::to_string(&panes).unwrap()
    ));
    let result = ZellijBackend::with_program_for_test(&shim)
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_0"),
            },
            ..Default::default()
        })
        .expect("full held grid");
    assert_eq!(result, crate::mux::CompanionPaneAppend::Full);
    assert!(!shim_log(&temp).contains("new-pane"));
}

#[cfg(unix)]
#[test]
fn companion_append_returns_full_below_zellij_045() {
    let (temp, shim) = support::logging_shim();
    let result = ZellijBackend::with_program_for_test(&shim)
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            ..Default::default()
        })
        .expect("unsupported companion append");
    assert_eq!(result, crate::mux::CompanionPaneAppend::Full);
    let log = shim_log(&temp);
    assert_eq!(log.trim(), "--version");
    assert!(!log.contains("new-pane"), "{log}");
}

#[cfg(unix)]
#[test]
fn companion_balance_resizes_toward_equal_columns() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
case " $* " in
  *" new-pane "*) touch "$dir/opened" ;;
  *" action list-panes --all --json "*)
    if [ -f "$dir/opened" ]; then
      printf '[{"id":7,"tab_id":42,"pane_x":30,"pane_y":0,"pane_columns":80,"pane_rows":80},{"id":8,"tab_id":42,"pane_x":110,"pane_y":0,"pane_columns":40,"pane_rows":80}]\n'
    else
      printf '[{"id":7,"tab_id":42,"pane_x":30,"pane_y":0,"pane_columns":120,"pane_rows":80}]\n'
    fi ;;
esac
exit 0
"#,
    );
    let result = ZellijBackend::with_program_for_test(&shim)
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
            ..Default::default()
        })
        .expect("opened and balanced companion");
    assert_eq!(result, crate::mux::CompanionPaneAppend::Opened);
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action new-pane"), 1, "{log}");
    assert_eq!(
        command_count(&log, "action list-panes --all --json"),
        3,
        "{log}"
    );
    let resizes = log
        .lines()
        .filter(|line| line.contains("action resize"))
        .collect::<Vec<_>>();
    assert_eq!(
        resizes,
        ["--session rimz-test action resize decrease right --pane-id terminal_7"],
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn companion_append_does_not_retry_after_geometry_failure() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.45.0\n'; exit 0; fi
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
case " $* " in
  *" new-pane "*) touch "$dir/opened" ;;
  *" list-panes "*)
    if [ -f "$dir/opened" ]; then printf 'invalid json'; else
      printf '[{"id":7,"tab_id":42,"pane_x":30,"pane_y":0,"pane_columns":120,"pane_rows":80}]\n'
    fi ;;
esac
"#,
    );
    let result = ZellijBackend::with_program_for_test(&shim)
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
            ..Default::default()
        })
        .expect("opened despite balancing failure");
    assert_eq!(result, crate::mux::CompanionPaneAppend::Opened);
    let log = shim_log(&temp);
    let spawns = log
        .lines()
        .filter(|line| line.contains("new-pane"))
        .collect::<Vec<_>>();
    assert_eq!(spawns.len(), 1);
    assert!(spawns[0].contains("--direction right --no-focus"), "{log}");
    assert!(spawns[0].ends_with("pane=7"), "{log}");
    assert!(!spawns[0].contains("--tab-id"), "{log}");
}

#[cfg(unix)]
#[test]
fn split_pane_keeps_the_legacy_anchor_below_zellij_045() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
exit 0
"#,
    );
    ZellijBackend::with_program_for_test(&shim)
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Stacked,
            focus: false,
            ..Default::default()
        })
        .expect("anchored stack");

    let log = shim_log(&temp);
    let shell = crate::proc::shell_pane_name();
    assert!(
        log.contains(&format!(
            "action new-pane --stacked --near-current-pane --name {shell} | pane=7"
        )),
        "{log}"
    );
    assert!(!log.contains("--no-focus"), "{log}");
}

#[cfg(unix)]
#[test]
fn split_pane_background_restores_client_instead_of_anchor_on_zellij_044() {
    assert_background_split_restores_client(true);
}

#[cfg(unix)]
#[test]
fn split_pane_background_restores_the_callers_session_client_on_zellij_044() {
    assert_background_split_restores_client(false);
}

#[cfg(unix)]
fn assert_background_split_restores_client(named_session: bool) {
    let room = TestRoom::new();
    room.write_cache(
        9_999_999_999_999,
        Some(9),
        None,
        vec![terminal_pane(9, 1, 120, 0, "zsh")],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
if [ "$1" = "--session" ] && [ -z "$2" ]; then exit 1; fi
case " $* " in
  *" action list-clients "*)
    if [ -f "$dir/spawned" ] && [ ! -f "$dir/restored" ]; then printf '1 terminal_8 work\n'; else printf '1 terminal_9 work\n'; fi ;;
  *" action list-panes "*) printf '[{"id":7,"tab_id":42,"tab_position":0},{"id":9,"tab_id":43,"tab_position":1}]\n' ;;
  *" action new-pane "*) touch "$dir/spawned" ;;
  *" action focus-pane-id terminal_9 "*) touch "$dir/restored" ;;
esac
exit 0
"#,
    );
    room.backend(&shim)
        .with_ambient_session_for_test("caller")
        .split_pane(SplitPaneOptions {
            target: if named_session {
                SplitTarget::SessionPane {
                    session_name: "rimz-test".to_owned(),
                    pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
                }
            } else {
                SplitTarget::Pane(PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"))
            },
            env: [(
                crate::workspace::ENV_WORKSPACE_ID.to_owned(),
                room.workspace_id.to_string(),
            )]
            .into(),
            focus: false,
            ..Default::default()
        })
        .expect("background split");
    let log = shim_log(&temp);
    if !named_session {
        assert_actions_name(&log, "caller");
    }
    assert!(log.contains("action focus-pane-id terminal_9"), "{log}");
    assert!(!log.contains("action focus-pane-id terminal_7"), "{log}");
    assert!(
        log.find("action list-clients").unwrap() < log.find("action new-pane").unwrap(),
        "{log}"
    );
}

/// Every logged line but a version probe is `--session <session> action …`.
#[cfg(unix)]
fn assert_actions_name(log: &str, session: &str) {
    let prefix = format!("--session {session} action ");
    assert!(!log.is_empty(), "no action ran");
    for line in log.lines().filter(|line| *line != "--version") {
        assert!(
            line.starts_with(&prefix),
            "not addressed to {session}: {line}"
        );
    }
}

/// A shim that answers the version probe with `version` and logs every other
/// argv, with one client on `terminal_9` and panes 7 and 9 to list.
#[cfg(unix)]
fn versioned_action_shim(version: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij {version}\n'; exit 0; fi
printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-clients "*) printf '1 terminal_9 work\n' ;;
  *" action list-panes "*) printf '[{{"id":7,"tab_id":42,"tab_position":0}},{{"id":9,"tab_id":43,"tab_position":1}}]\n' ;;
esac
exit 0
"#
    ))
}

#[cfg(unix)]
#[test]
fn every_split_focus_client_and_detach_action_names_the_session() {
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    for version in ["0.44.3", "0.45.0"] {
        for (target, session) in [
            (SplitTarget::Ambient, "caller"),
            (SplitTarget::Pane(pane.clone()), "caller"),
            (SplitTarget::Session("room-a".to_owned()), "room-a"),
            (
                SplitTarget::SessionPane {
                    session_name: "room-a".to_owned(),
                    pane_id: pane.clone(),
                },
                "room-a",
            ),
        ] {
            for focus in [true, false] {
                let (temp, shim) = versioned_action_shim(version);
                ZellijBackend::with_program_for_test(&shim)
                    .with_ambient_session_for_test("caller")
                    .split_pane(SplitPaneOptions {
                        target: target.clone(),
                        focus,
                        ..Default::default()
                    })
                    .expect("split");
                let log = shim_log(&temp);
                assert_actions_name(&log, session);
                assert_eq!(command_count(&log, " action new-pane "), 1, "{log}");
            }
        }
    }

    let (temp, shim) = versioned_action_shim("0.45.0");
    let backend =
        ZellijBackend::with_program_for_test(&shim).with_ambient_session_for_test("caller");
    backend.focus_pane(&pane, None).expect("focus");
    backend
        .client_view(crate::mux::ClientFocusOptions::default())
        .expect("client view");
    backend.detach("room-a").expect("detach");
    assert_eq!(
        shim_log(&temp),
        "--session caller action focus-pane-id terminal_7\n\
         --session caller action list-clients\n\
         --session caller action detach\n"
    );
}

#[cfg(unix)]
#[test]
fn an_action_with_no_session_to_address_refuses_before_zellij_is_spawned() {
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    for version in ["0.44.3", "0.45.0"] {
        let (temp, shim) = versioned_action_shim(version);
        let backend = ZellijBackend::with_program_for_test(&shim);
        let mut refusals = Vec::new();
        for target in [SplitTarget::Ambient, SplitTarget::Pane(pane.clone())] {
            for focus in [true, false] {
                refusals.push(
                    backend
                        .split_pane(SplitPaneOptions {
                            target: target.clone(),
                            focus,
                            ..Default::default()
                        })
                        .map(|_| ()),
                );
            }
        }
        refusals.push(backend.focus_pane(&pane, None));
        refusals.push(
            backend
                .client_view(crate::mux::ClientFocusOptions::default())
                .map(|_| ()),
        );
        refusals.push(backend.detach("room-a"));
        for refusal in refusals {
            assert!(
                matches!(refusal, Err(MuxErr::NoSessionToAddress)),
                "{refusal:?}"
            );
        }
        assert_eq!(shim_log(&temp), "", "zellij {version}");
    }
}

#[cfg(unix)]
#[test]
fn split_pane_emits_close_on_exit_only_when_requested() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 0
"#,
    );
    let backend =
        ZellijBackend::with_program_for_test(&shim).with_ambient_session_for_test("caller");

    backend
        .split_pane(SplitPaneOptions {
            close_on_exit: true,
            focus: true,
            ..Default::default()
        })
        .expect("self-closing split");
    backend
        .split_pane(SplitPaneOptions {
            focus: true,
            ..Default::default()
        })
        .expect("default split");

    let log = shim_log(&temp);
    let mut commands = log.lines();
    assert!(
        commands
            .next()
            .is_some_and(|command| command.contains("--close-on-exit")),
        "{log}"
    );
    assert!(
        commands
            .next()
            .is_some_and(|command| !command.contains("--close-on-exit")),
        "{log}"
    );
    assert!(commands.next().is_none(), "{log}");
}

#[cfg(unix)]
#[test]
fn list_panes_uses_fresh_topology_and_honors_explicit_floor() {
    let room = TestRoom::new();
    let floor = unix_now_ms();
    room.write_cache(
        floor.saturating_sub(1),
        Some(7),
        Some(TopologyClients {
            human_clients: Some(2),
            viewed_panes: Some(vec![7]),
            views: Vec::new(),
        }),
        vec![PaneTopologyPane {
            pane_command: Some("zsh".to_owned()),
            pane_cwd: Some(room.project_root.path().to_string_lossy().into_owned()),
            terminal_command: None,
            ..terminal_pane(7, 0, 100, 0, "zsh")
        }],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let backend = room.backend(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            ..Default::default()
        })
        .expect("fresh topology");
    assert_eq!(listing.panes.len(), 1);
    let pane = &listing.panes[0];
    assert_eq!(
        (pane.pane_id.raw(), pane.view_id.as_deref()),
        ("terminal_7", Some("tab_0"))
    );
    assert_eq!(
        (pane.command.as_deref(), pane.spawn_command.as_deref()),
        (Some("zsh"), None)
    );
    assert_eq!(
        pane.cwd.as_deref(),
        Some(room.project_root.path().to_string_lossy().as_ref())
    );
    let client = listing.client_view.expect("client view");
    assert_eq!(
        (client.presence.human_clients, client.presence.last_input_ms),
        (2, None)
    );
    assert_eq!(client.viewed_panes, vec![pane.pane_id.clone()]);
    assert!(
        shim_log(&temp).is_empty(),
        "fresh cache must avoid Zellij actions"
    );

    backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            min_topology_produced_at_ms: Some(floor),
            command_timeout: Some(Duration::from_millis(1)),
            ..Default::default()
        })
        .expect_err("explicit floor rejects pre-floor cache");
}

#[cfg(unix)]
#[test]
fn reconcile_sidebars_rejects_topology_before_the_liveness_floor() {
    let room = TestRoom::new();
    let produced_at_ms = unix_now_ms();
    room.write_cache(
        produced_at_ms,
        Some(1),
        None,
        vec![
            terminal_pane(1, 0, 50, 0, "rimz-sidebar"),
            terminal_pane(2, 0, 150, 50, "work"),
        ],
    );
    let (_temp, shim) = zellij_shim("#!/bin/sh\nexit 1\n");
    let backend = room.backend(&shim);
    let live = SidebarLiveness {
        claimed_panes: [PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_1",
        )]
        .into(),
        topology_floor_ms: Some(produced_at_ms.saturating_add(1)),
        ..SidebarLiveness::default()
    };

    backend
        .reconcile_sidebars(&room.sidebar_options(200), &live)
        .expect_err("a repair pass cannot judge geometry from pre-floor topology");
}

#[cfg(unix)]
#[test]
fn reconcile_sidebars_defers_detached_adds_and_geometry_repairs() {
    // fa15860b2: detached geometry repairs defer alongside missing-sidebar adds.
    let room = TestRoom::new();
    let produced_at_ms = unix_now_ms();
    room.write_cache(
        produced_at_ms,
        None,
        None,
        vec![
            terminal_pane(1, 0, 150, 0, "rimz-sidebar"),
            terminal_pane(2, 0, 50, 150, "work"),
            terminal_pane(3, 1, 200, 0, "work"),
        ],
    );
    let (temp, shim) = support::logging_shim();
    let live = SidebarLiveness {
        claimed_panes: [PaneId::from_parts(crate::MuxName::Zellij, "terminal_1")].into(),
        topology_floor_ms: Some(produced_at_ms),
        ..Default::default()
    };
    let mut opts = room.sidebar_options(200);
    opts.rimz_bin = shim.clone();
    let report = room
        .backend(&shim)
        .reconcile_sidebars(&opts, &live)
        .expect("detached reconcile");
    assert_eq!(
        report,
        crate::mux::SidebarRecovery {
            deferred: 2,
            ..Default::default()
        }
    );
    let log = shim_log(&temp);
    assert!(log.contains("action list-clients"), "{log}");
    assert!(
        !log.contains("new-pane") && !log.contains("resize"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn floorless_reconcile_skips_width_only_repairs() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(1),
        None,
        vec![
            terminal_pane(1, 0, 150, 0, "rimz-sidebar"),
            terminal_pane(2, 0, 50, 150, "work"),
        ],
    );
    let (temp, shim) = zellij_shim("#!/bin/sh\nexit 1\n");
    let backend = room.backend(&shim);
    let live = SidebarLiveness {
        claimed_panes: [PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_1",
        )]
        .into(),
        ..SidebarLiveness::default()
    };

    let report = backend
        .reconcile_sidebars(&room.sidebar_options(200), &live)
        .expect("floorless structural reconcile");

    assert_eq!(report, crate::mux::SidebarRecovery::default());
    assert!(
        shim_log(&temp).is_empty(),
        "an unproven viewport must not trigger a width action",
    );
}

#[cfg(unix)]
#[test]
fn cached_pane_roster_reads_only_fresh_normalized_terminal_ids() {
    let room = TestRoom::new();
    let produced_at_ms = unix_now_ms();
    room.write_cache(
        produced_at_ms,
        None,
        None,
        vec![
            terminal_pane(7, 0, 80, 0, "zsh"),
            PaneTopologyPane {
                is_plugin: true,
                ..terminal_pane(9, 0, 0, 0, "plugin")
            },
        ],
    );
    let backend = ZellijBackend::with_runtime_dir(room.runtime_root.path());

    assert_eq!(
        backend.cached_pane_roster("rimz-test", &room.workspace_id),
        Some(crate::mux::CachedPaneRoster {
            pane_ids: vec![PaneId::from_parts(
                crate::ids::MuxName::Zellij,
                "terminal_7",
            )],
            observed_at_ms: produced_at_ms,
        }),
    );

    room.write_cache(
        produced_at_ms
            .saturating_sub(crate::mux::PRESENCE_STAMP_FRESH.as_millis() as u64)
            .saturating_sub(1),
        None,
        None,
        vec![terminal_pane(7, 0, 80, 0, "zsh")],
    );
    assert_eq!(
        backend.cached_pane_roster("rimz-test", &room.workspace_id),
        None,
    );
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_preserves_server_identity_and_cache_enrichment() {
    let room = TestRoom::new();
    room.write_cache(
        1,
        None,
        None,
        vec![
            PaneTopologyPane {
                tab_name: Some("old".to_owned()),
                pane_command: Some("vim".to_owned()),
                pane_cwd: Some(room.project_root.path().to_string_lossy().into_owned()),
                pane_pid: Some(707),
                ..terminal_pane(7, 0, 999, 999, "zsh")
            },
            PaneTopologyPane {
                is_plugin: true,
                pane_command: Some("plugin-command".to_owned()),
                pane_cwd: Some("/plugin".to_owned()),
                pane_pid: Some(909),
                ..terminal_pane(7, 0, 888, 888, "plugin")
            },
            PaneTopologyPane {
                pane_command: Some("rimz-sidebar".to_owned()),
                terminal_command: Some("rimz".to_owned()),
                ..terminal_pane(8, 1, 40, 0, "rimz-sidebar")
            },
            PaneTopologyPane {
                pane_command: Some("rimz-sidebar".to_owned()),
                terminal_command: Some("rimz".to_owned()),
                ..terminal_pane(9, 2, 50, 0, "rimz-sidebar")
            },
        ],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":7,"is_plugin":true,"tab_position":0,"tab_name":"work","title":"plugin"},{"id":7,"is_plugin":false,"tab_position":0,"tab_name":"work","pane_columns":100,"pane_x":0,"title":"zsh","terminal_command":"/bin/zsh","pane_command":"cargo test","pane_cwd":"/native"},{"id":8,"is_plugin":false,"tab_position":1,"tab_name":"background","pane_columns":40,"pane_x":0,"title":"rimz-sidebar","terminal_command":"rimz"},{"id":10,"is_plugin":false,"tab_position":0,"tab_name":"work","title":"zsh","pane_command":"rimz agents claude --worktree=x","pane_cwd":""}]\n'
    exit 0 ;;
esac
exit 1
"#,
    );
    let listing = room
        .backend(&shim)
        .authoritative_pane_listing(
            "rimz-test",
            None,
            Some(&room.workspace_id),
            Duration::from_secs(1),
        )
        .expect("authoritative listing");
    assert_eq!(listing.panes.len(), 4, "cache-only pane stays absent");
    let plugin = listing
        .panes
        .iter()
        .find(|pane| pane.is_plugin && pane.id == 7)
        .expect("plugin");
    assert_eq!(plugin.pane_command.as_deref(), Some("plugin-command"));
    assert_eq!(plugin.pane_cwd.as_deref(), Some("/plugin"));
    assert_eq!(plugin.pane_pid, Some(909));
    let active = listing
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 7)
        .expect("active");
    assert_eq!((active.pane_columns, active.pane_x), (Some(100), Some(0)));
    assert_eq!(
        (active.pane_command.as_deref(), active.pane_cwd.as_deref()),
        (Some("cargo test"), Some("/native")),
        "Zellij's live command and cwd outrank the cached copy",
    );
    assert_eq!(active.pane_pid, Some(707));
    let launcher = listing
        .panes
        .iter()
        .find(|pane| pane.id == 10)
        .expect("launcher");
    assert_eq!(
        (
            launcher.pane_command.as_deref(),
            launcher.pane_cwd.as_deref()
        ),
        (None, None),
        "launch chrome and empty fields read as absent",
    );
    let background = listing
        .panes
        .iter()
        .find(|pane| pane.id == 8)
        .expect("background");
    assert_eq!(
        (background.pane_columns, background.pane_x),
        (Some(40), Some(0))
    );
    assert_eq!(background.pane_command.as_deref(), Some("rimz-sidebar"));
    assert_eq!(listing.focused_pane, None, "pane roster is not focus truth");
    assert!(listing.produced_at_ms >= unix_now_ms().saturating_sub(1_000));
    assert!(shim_log(&temp).contains("action list-panes --all --json"));
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_falls_back_unless_required() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(8),
        None,
        vec![PaneTopologyPane {
            tab_name: Some("fallback".to_owned()),
            pane_command: Some("zsh".to_owned()),
            ..terminal_pane(8, 1, 80, 0, "zsh")
        }],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let backend = room.backend(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            consistency: PaneReadConsistency::PreferAuthoritative,
            ..Default::default()
        })
        .expect("optional authoritative read falls back");
    assert_eq!(listing.panes[0].pane_id.raw(), "terminal_8");
    let err = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .expect_err("required authoritative read propagates failure");
    assert!(matches!(err, crate::mux::MuxErr::Command { .. }));
    assert_eq!(
        command_count(&shim_log(&temp), "action list-panes --all --json"),
        2
    );
}

#[cfg(unix)]
#[test]
fn list_panes_fails_fast_when_session_absent() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "list-sessions" ]; then exit 0; fi
exit 1
"#,
    );
    let err = room
        .backend(&shim)
        .list_panes(PaneListOptions {
            session_name: Some("rimz-dead".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            command_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        })
        .expect_err("absent session");
    assert!(
        matches!(err, crate::mux::MuxErr::SessionNotFound { session } if session == "rimz-dead")
    );
    let log = shim_log(&temp);
    assert!(
        log.contains("list-sessions --no-formatting") && !log.contains("rimz:dump_topology"),
        "{log}"
    );
}

#[cfg(unix)]
fn assert_stepwise_width(
    name: &str,
    initial: u64,
    step: i64,
    view: u64,
    target: u16,
    direction: &str,
    calls: usize,
) {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, initial, 0, "rimz-sidebar"),
            terminal_pane(9, 1, view - initial, initial, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"; attempts="$dir/resize-attempts"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0); cols=$(({initial} + count * {step})); work=$(({view} - cols))
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]}}\n' "$now" "$cols" "$cols" "$work" > "{cache}"
    exit 0 ;;
  *" action resize {direction} right --pane-id terminal_8 "*)
    attempt=$(cat "$attempts" 2>/dev/null || printf 0); attempt=$((attempt + 1)); printf '%s\n' "$attempt" > "$attempts"
    if [ "$attempt" -eq 1 ]; then exit 1; fi
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.lane_path("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(target).expect("target");
    let view_cols = std::num::NonZeroU16::new(u16::try_from(view).expect("test view"))
        .expect("nonzero test view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };
    let (floor, resized) = backend.converge_sidebar_widths_stepwise(&width, 1, 8, None);
    assert!(resized, "{name}: expected resize");
    let log = shim_log(&temp);
    assert_eq!(
        command_count(
            &log,
            &format!("action resize {direction} right --pane-id terminal_8")
        ),
        calls,
        "{name}:\n{log}"
    );
    let final_cols = backend
        .topology_panes_for_workspace(
            "rimz-test",
            &room.workspace_id,
            floor,
            crate::mux::zellij::RECONCILE_LIST_TIMEOUT,
        )
        .expect("final topology")
        .into_iter()
        .find(|pane| pane.is_terminal() && pane.id == 8)
        .and_then(|pane| pane.pane_columns)
        .expect("sidebar columns");
    let target = u64::from(width.target.cols(Some(view_cols.get())).get());
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            target,
            crate::mux::width::zellij_resize_stop_step_cols(view)
        ),
        "{name}: final width {final_cols}"
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_converges_across_supported_steps() {
    for (name, initial, step, view, target, direction, calls) in [
        ("shrink", 90, -1, 360, 72, "decrease", 10),
        ("grow", 40, 19, 380, 72, "increase", 3),
        ("full-step-below", 53, 10, 213, 64, "increase", 2),
    ] {
        assert_stepwise_width(name, initial, step, view, target, direction, calls);
    }
}

#[cfg(unix)]
fn assert_stepwise_park(
    name: &str,
    widths: &[u64],
    target: u16,
    expected_increase: usize,
    expected_decrease: usize,
) {
    let initial = widths[0];
    let last = widths[widths.len() - 1];
    let geometry_cases = widths
        .iter()
        .enumerate()
        .map(|(count, width)| format!("      {count}) cols={width} ;;\n"))
        .collect::<String>();
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, initial, 0, "rimz-sidebar"),
            terminal_pane(9, 1, 213 - initial, initial, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    case "$count" in
{geometry_cases}      *) cols={last} ;;
    esac
    work=$((213 - cols))
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]}}\n' "$now" "$cols" "$cols" "$work" > "{cache}"
    exit 0 ;;
  *" action resize increase right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.lane_path("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(target).expect("target");
    let view_cols = std::num::NonZeroU16::new(213).expect("view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };

    let resized = backend
        .converge_sidebar_widths_stepwise(&width, 1, 8, None)
        .1;
    assert_eq!(
        resized,
        expected_increase + expected_decrease > 0,
        "{name}: resize outcome",
    );
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize increase right --pane-id terminal_8"),
        expected_increase,
        "{name}:\n{log}",
    );
    assert_eq!(
        command_count(&log, "action resize decrease right --pane-id terminal_8"),
        expected_decrease,
        "{name}:\n{log}",
    );
    let action_count = std::fs::read_to_string(temp.path().join("resize-count"))
        .unwrap_or_else(|_| "0".to_owned())
        .trim()
        .parse::<usize>()
        .expect("numeric resize count");
    let final_cols = widths.get(action_count).copied().unwrap_or(last);
    let target = u64::from(width.target.cols(Some(view_cols.get())).get());
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            target,
            crate::mux::width::zellij_resize_stop_step_cols(213),
        ),
        "{name}: final width {final_cols}",
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_parks_at_the_nearest_reachable_width() {
    assert_stepwise_park("default-between-lattice-points", &[53, 64], 54, 0, 0);
    assert_stepwise_park("grow", &[48, 59, 70], 64, 1, 0);
    assert_stepwise_park("shrink", &[82, 71, 60], 64, 0, 2);
    assert_stepwise_park(
        "coarse-step undershoot reverses once",
        &[53, 76, 63],
        64,
        1,
        1,
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_uses_authoritative_geometry_over_fresh_stale_cache() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, 171, 0, "rimz-sidebar"),
            terminal_pane(9, 1, 209, 171, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" action list-panes --all --json "*)
    count=$(cat "$state" 2>/dev/null || printf 0); cols=$((171 - count * 19)); if [ "$cols" -lt 72 ]; then cols=72; fi
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":72}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":72,"pane_columns":308}}]}}\n' "$now" > "{cache}"
    printf '[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]\n' "$cols" "$cols" "$((380 - cols))"; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.lane_path("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(72).expect("target");
    let view_cols = std::num::NonZeroU16::new(380).expect("view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };
    assert!(
        backend
            .converge_sidebar_widths_stepwise(&width, 1, 8, None)
            .1
    );
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize decrease right --pane-id terminal_8"),
        5,
        "{log}"
    );
    let final_cols = backend
        .authoritative_pane_listing(
            "rimz-test",
            None,
            Some(&room.workspace_id),
            crate::mux::zellij::RECONCILE_LIST_TIMEOUT,
        )
        .expect("final listing")
        .panes
        .into_iter()
        .find(|pane| pane.is_terminal() && pane.id == 8)
        .and_then(|pane| pane.pane_columns)
        .expect("sidebar columns");
    assert!(!crate::mux::width::sidebar_width_off_spec(
        final_cols,
        72,
        crate::mux::width::zellij_resize_stop_step_cols(380)
    ));
}

#[cfg(unix)]
#[test]
fn redock_moves_across_every_adjacent_pane_before_resizing() {
    let room = TestRoom::new();
    let mut stale: Vec<_> = (1..=7)
        .map(|id| PaneTopologyPane {
            ..terminal_pane(id, 1, 140, (id - 1) * 140, "zsh")
        })
        .collect();
    stale.push(terminal_pane(8, 1, 140, 980, "rimz-sidebar"));
    room.write_cache(9_999_999_999_999, Some(1), None, stale);
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; moves="$dir/move-count"; published="$dir/published-count"; resized="$dir/resized"
printf '%s\n' "$*" >> "$log"
case " $* " in
  *" action list-panes --all --json "*)
    count=$(cat "$moves" 2>/dev/null || printf 0); if [ "$count" -gt 7 ]; then count=7; fi
    visible=$(cat "$published" 2>/dev/null || printf 0); if [ "$visible" -lt "$count" ]; then printf '%s\n' "$count" > "$published"; fi
    slot=$((7 - visible)); sidebar_x=$((slot * 140)); cols=140; if [ -f "$resized" ]; then cols=72; fi
    printf '['; i=0
    while [ "$i" -lt 7 ]; do
      if [ "$i" -ge "$slot" ]; then x=$(((i + 1) * 140)); else x=$((i * 140)); fi
      if [ "$i" -gt 0 ]; then printf ','; fi
      printf '{"id":%s,"is_plugin":false,"tab_position":1,"pane_columns":140,"pane_x":%s,"title":"zsh"}' "$((i + 1))" "$x"; i=$((i + 1))
    done
    printf ',{"id":8,"is_plugin":false,"tab_position":1,"pane_columns":%s,"pane_x":%s,"title":"rimz-sidebar"}]\n' "$cols" "$sidebar_x"; exit 0 ;;
  *" action move-pane left --pane-id terminal_8 "*) count=$(cat "$moves" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$moves"; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*) : > "$resized"; exit 0 ;;
esac
exit 1
"#,
    );
    let backend = room.backend(&shim);
    backend.converge_sidebar_geometry(&room.sidebar_options(1120), 1, 8, Some(0));
    let log = shim_log(&temp);
    let lines: Vec<_> = log.lines().collect();
    let moves: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| line.contains("action move-pane left").then_some(i))
        .collect();
    let resize = lines
        .iter()
        .position(|line| line.contains("action resize decrease right"))
        .expect("resize");
    assert_eq!(moves.len(), 7, "{log}");
    assert!(resize > *moves.last().expect("moves"), "{log}");
    let listing = backend
        .structural_geometry_listing("rimz-test", &room.workspace_id, None)
        .expect("final geometry");
    assert_eq!(
        listing
            .panes
            .iter()
            .find(|pane| pane.id == 8)
            .and_then(|pane| pane.pane_x),
        Some(0)
    );
}

#[cfg(unix)]
#[test]
fn redock_stops_on_authoritative_no_progress() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*) printf '[{"id":1,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":0,"title":"zsh"},{"id":2,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":90,"title":"zsh"},{"id":8,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":180,"title":"rimz-sidebar"}]\n'; exit 0 ;;
  *" action move-pane left --pane-id terminal_8 "*) exit 0 ;;
esac
exit 1
"#,
    );
    room.backend(&shim)
        .converge_sidebar_geometry(&room.sidebar_options(270), 1, 8, Some(0));
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action move-pane left"), 1, "{log}");
    assert!(!log.contains("action resize"), "{log}");
}

#[cfg(unix)]
#[test]
fn width_nudge_targets_only_named_pane() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"; exit 0
"#,
    );
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_8");
    ZellijBackend::with_program_for_test(&shim)
        .nudge_sidebar_width("rimz-test", &pane, 40, 72)
        .expect("nudge");
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize increase right --pane-id terminal_8"),
        1
    );
    assert!(
        !log.contains("list-panes") && !log.contains("list-clients"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn sidebar_add_never_cleans_cross_talk_hint_and_uses_supported_split() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(7),
        None,
        vec![PaneTopologyPane {
            pane_command: Some("zsh".to_owned()),
            terminal_command: Some("zsh".to_owned()),
            ..terminal_pane(7, 1, 120, 0, "zsh")
        }],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/new-pane-count"
printf 'pane=%s args=%s\n' "$ZELLIJ_PANE_ID" "$*" >> "$log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    if [ "$count" -ge 2 ]; then
      printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":7,"panes":[{{"id":9,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}},{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90}}]}}\n' "$now" > "{cache}"
    else
      printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":7,"panes":[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":90}},{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":90,"pane_columns":30}}]}}\n' "$now" > "{cache}"
    fi
    exit 0 ;;
  *" action list-panes --all --json "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    if [ "$count" -ge 2 ]; then printf '[{{"id":9,"is_plugin":false,"tab_id":1,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}},{{"id":7,"is_plugin":false,"tab_id":1,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90}}]\n';
    elif [ "$count" -ge 1 ]; then printf '[{{"id":7,"is_plugin":false,"tab_id":1,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":90}},{{"id":8,"is_plugin":false,"tab_id":1,"tab_position":1,"title":"rimz-sidebar","pane_x":90,"pane_columns":30}}]\n';
    else printf '[{{"id":7,"is_plugin":false,"tab_id":1,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":120}}]\n'; fi
    exit 0 ;;
  *" action new-pane "*) count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; printf 'terminal_7\n'; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.lane_path("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    if let Err(err) = room
        .backend(&shim)
        .add_sidebar_to_tab(&room.sidebar_options(120), 1, Some(0))
    {
        panic!("retry add: {err}\n{}", shim_log(&temp));
    }
    let log = shim_log(&temp);
    let adds: Vec<_> = log
        .lines()
        .filter(|line| line.contains(" action new-pane "))
        .collect();
    assert_eq!(adds.len(), 2, "first misdock must retry:\n{log}");
    assert!(
        adds.iter().all(|line| line.contains("new-pane --tab-id 1")
            && line.contains("--borderless true")
            && !line.contains("--near-current-pane")
            && !line.contains("--direction")),
        "{log}"
    );
    assert!(
        !log.contains("action go-to-tab") && !log.contains("action focus-pane-id"),
        "stable tab targeting must not mutate global focus:\n{log}"
    );
    assert!(
        log.contains("close-pane --pane-id terminal_8"),
        "topology-proven failed add is cleaned:\n{log}"
    );
    assert!(
        !log.contains("close-pane --pane-id terminal_7"),
        "cross-talk hint must stay open:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn commands_classify_session_not_found_for_zero_and_nonzero_exit() {
    for (name, script) in [
        (
            "zero",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n'
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
exit 0
"#,
        ),
        (
            "nonzero",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n' >&2
exit 1
"#,
        ),
    ] {
        let (_temp, shim) = zellij_shim(script);
        let err = ZellijBackend::with_program_for_test(&shim)
            .list_tabs("missing-room")
            .expect_err(name);
        assert!(
            matches!(err, crate::mux::MuxErr::SessionNotFound { ref session } if session == "missing-room"),
            "{name}: {err}"
        );
        assert!(!err.to_string().contains("rimz-other"), "{name}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn list_tabs_retries_a_parsed_empty_listing() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; count_file="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"
count=$(cat "$count_file" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s\n' "$count" > "$count_file"
if [ "$count" -eq 1 ]; then printf '[]\n'; else printf '[{"name":"main"}]\n'; fi
"#,
    );

    let tabs = ZellijBackend::with_program_for_test(&shim)
        .list_tabs("room")
        .expect("retry empty listing");

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].name, "main");
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action list-tabs --json --panes"), 2);
}

#[cfg(unix)]
#[test]
fn new_tab_keeps_layout_until_panes_materialize() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; tab="$dir/tab-created"; layout_ref="$dir/layout-path"; count_file="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
case " $* " in
  *" action new-tab "*) while [ "$#" -gt 0 ]; do if [ "$1" = "--layout" ]; then shift; printf '%s' "$1" > "$layout_ref"; fi; shift; done; : > "$tab"; printf '7\n'; exit 0 ;;
  *" action list-tabs "*)
    count=$(cat "$count_file" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s\n' "$count" > "$count_file"
    printf '[{"name":"main","selectable_tiled_panes_count":1}'
    if [ -f "$tab" ]; then panes=0; layout=$(cat "$layout_ref" 2>/dev/null || true); if [ "$count" -ge 3 ]; then if [ -n "$layout" ] && [ -f "$layout" ]; then panes=2; else printf 'layout-missing-before-materialized\n' >> "$log"; fi; fi; printf ',{"name":"work","selectable_tiled_panes_count":%s}' "$panes"; fi
    printf ']\n'; exit 0 ;;
esac
exit 0
"#,
    );
    room.backend(&shim)
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "work".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                        name: None,
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: room.sidebar_options(120),
        })
        .expect("open tab");
    let log = shim_log(&temp);
    assert!(
        command_count(&log, "action list-tabs --json --panes") >= 3,
        "{log}"
    );
    assert!(!log.contains("layout-missing-before-materialized"), "{log}");
    assert!(!log.contains("query-tab-names"), "{log}");
    assert_eq!(command_count(&log, "action new-tab "), 1, "{log}");
}

#[cfg(unix)]
#[test]
fn unconfirmed_tab_is_closed_by_id_without_closing_an_existing_namesake() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); tab="$dir/tab-created"
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then printf 'zellij 0.45.1\n'; exit 0; fi
case " $* " in
  *" action new-tab "*) touch "$tab"; printf '7\n' ;;
  *" action list-tabs "*)
    printf '[{"name":"work","tab_id":2,"selectable_tiled_panes_count":1}'
    if [ -f "$tab" ]; then printf ',{"name":"work","tab_id":7,"selectable_tiled_panes_count":0}'; fi
    printf ']\n' ;;
esac
"#,
    );
    let error = room
        .backend(&shim)
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "work".into(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".into(), "600".into()],
                        name: None,
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: room.sidebar_options(120),
        })
        .expect_err("unmaterialized tab");
    assert!(error.to_string().contains("did not materialize"), "{error}");
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action new-tab "), 1, "{log}");
    assert_eq!(
        command_count(&log, "action close-tab --tab-id 7"),
        1,
        "{log}"
    );
    assert!(!log.contains("action close-tab --tab-id 2"), "{log}");
}

#[cfg(unix)]
#[test]
fn open_tab_moves_by_tab_id_on_zellij_044() {
    assert_open_tab_move_confirmation("0.44.3", true);
}

#[cfg(unix)]
#[test]
fn open_tab_stops_after_first_unconfirmed_move() {
    assert_open_tab_move_confirmation("0.45.0", false);
}

#[cfg(unix)]
#[test]
fn open_tab_moves_by_tab_id_on_zellij_045() {
    assert_open_tab_move_confirmation("0.45.0", true);
}

#[cfg(unix)]
fn assert_open_tab_move_confirmation(version: &str, move_confirms: bool) {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1" = "--version" ]; then printf 'zellij {version}\n'; exit 0; fi
case " $* " in
  *" action move-tab "*) printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log" ;;
  *) printf '%s\n' "$*" >> "$dir/zellij.log" ;;
esac
case " $* " in
  *" action new-tab "*) touch "$dir/opened"; printf '45\n' ;;
  *" action move-tab "*)
    position=$(cat "$dir/position" 2>/dev/null || printf 3)
    printf '%s' "$((position - 1))" > "$dir/position" ;;
  *" action list-clients "*) printf '1 terminal_7 work\n' ;;
  *" action list-tabs "*)
    printf '[{{"name":"main","selectable_tiled_panes_count":1}},{{"name":"two","selectable_tiled_panes_count":1}},{{"name":"three","selectable_tiled_panes_count":1}}'
    if [ -f "$dir/opened" ]; then printf ',{{"name":"new","selectable_tiled_panes_count":1}}'; fi
    printf ']\n' ;;
  *" action list-panes "*)
    position=3
    if {move_confirms}; then position=$(cat "$dir/position" 2>/dev/null || printf 3); fi
    printf '[{{"id":7,"tab_id":42,"tab_position":0,"pane_columns":120,"pane_x":0}},{{"id":8,"tab_id":45,"tab_position":%s,"pane_columns":120,"pane_x":0}}]\n' "$position" ;;
esac
exit 0
"#,
    ));
    room.backend(&shim)
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "new".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                        name: None,
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            focus: version == "0.44.3",
            dock_sidebar: false,
            after: Some(PaneId::from_parts(crate::MuxName::Zellij, "terminal_7")),
            sidebar: room.sidebar_options(120),
        })
        .expect("tab opens even when placement fails");
    let log = shim_log(&temp);
    let actions = log
        .lines()
        .skip_while(|line| !line.contains("action move-tab"))
        .collect::<Vec<_>>();
    let attempts = usize::try_from(FOCUS_RESTORE_ATTEMPTS).unwrap();
    let move_tab = "--session rimz-test action move-tab left --tab-id 45 | pane=";
    let list = "--session rimz-test action list-panes --all --json";
    let mut expected = vec![];
    if move_confirms {
        expected.extend([move_tab, list].repeat(2));
    } else {
        expected.push(move_tab);
        expected.extend(std::iter::repeat_n(list, attempts));
    }
    assert_eq!(actions, expected, "{log}");
    assert!(
        !log.contains("focus-pane-id") && !log.contains("list-clients"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn open_tab_restore_switches_tab_between_request_and_dispatch() {
    assert_open_tab_background("0.44.3");
}

#[cfg(unix)]
#[test]
fn open_tab_background_never_moves_client_on_zellij_045() {
    assert_open_tab_background("0.45.0");
}

#[cfg(unix)]
fn assert_open_tab_background(version: &str) {
    let room = TestRoom::new();
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");
    room.write_cache(
        9_999_999_999_999,
        Some(7),
        None,
        vec![terminal_pane(7, 0, 120, 0, "zsh")],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; tab="$dir/tab-created"
if [ "$1" = "--version" ]; then printf 'zellij VERSION\n'; exit 0; fi
printf '%s\n' "$*" >> "$log"
case " $* " in
  *" action list-clients "*) printf '1 terminal_7 zsh\n'; exit 0 ;;
  *" action list-panes --all --json "*) printf '[{"id":7,"is_plugin":false,"tab_position":0,"pane_columns":120,"pane_x":0,"title":"zsh"}]\n'; exit 0 ;;
  *" action list-tabs "*)
    if [ -f "$tab" ]; then printf '[{"name":"main","selectable_tiled_panes_count":1},{"name":"new","selectable_tiled_panes_count":1}]\n';
    else printf '[{"name":"main","selectable_tiled_panes_count":1}]\n'; fi
    exit 0 ;;
  *" action new-tab "*) : > "$tab"; printf '7\n'; exit 0 ;;
esac
exit 0
"#.replace("VERSION", version).as_str(),
    );

    room.backend(&shim)
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "new".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                        name: None,
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar: room.sidebar_options(120),
        })
        .expect("open unfocused tab");

    let log = shim_log(&temp);
    if version == "0.45.0" {
        assert!(
            log.lines()
                .any(|line| line.contains("action new-tab") && line.contains("--no-focus")),
            "{log}"
        );
        for forbidden in ["list-clients", "go-to-tab", "focus-pane-id"] {
            assert!(!log.contains(forbidden), "{log}");
        }
        return;
    }
    let request = log.rfind("action list-clients").expect("request sample");
    let switch = log
        .find("--session rimz-test action go-to-tab 1")
        .expect("tab switch");
    let dispatch = log
        .find("--session rimz-test action focus-pane-id terminal_7")
        .expect("focus dispatch");
    assert!(request < switch && switch < dispatch, "{log}");

    let anchor = crate::mux::focus_anchor::load(&room.runtime).expect("focus anchor");
    assert_eq!(
        anchor.state,
        crate::mux::focus_anchor::FocusIntentState::Applied
    );
    assert_eq!(anchor.pane_id, pane);
}

#[test]
fn runtime_dir_pins_full_zellij_env_surface() {
    let runtime = tempfile::TempDir::new().expect("runtime");
    let runtime = runtime.path().to_string_lossy().into_owned();
    let pinned = ZellijBackend::with_runtime_dir(&runtime).cmd();
    let keys = [
        "XDG_RUNTIME_DIR",
        "RIMZ_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ];
    for key in keys {
        assert_eq!(pinned.env.get(key), Some(&runtime), "{key}");
    }
    let default = ZellijBackend::default().cmd();
    for key in keys {
        assert!(
            !default.env.contains_key(key),
            "production must inherit {key}"
        );
    }
}

#[test]
fn version_parser_accepts_zellij_output_shapes() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = ZellijBackend::default();
    backend
        .version
        .set("zellij 9.9.9".to_owned())
        .expect("fresh cache");
    assert_eq!(backend.version().expect("cached version"), "zellij 9.9.9");
}

#[test]
fn zellij_session_options_respect_defaults_overrides_and_order() {
    use crate::config::{ZellijClipboard, ZellijForceClose};
    use ZellijOptionValue::{Bool, Int, Word};

    let pairs = |config: &ZellijConfig| {
        zellij_session_options(config)
            .into_iter()
            .map(|option| (option.key, option.value))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pairs(&ZellijConfig::default()),
        vec![
            ("auto_layout", Bool(false)),
            ("stacked_resize", Bool(true)),
            ("stacked_pane_list", Bool(false)),
            ("mouse_click_through", Bool(true)),
            ("focus_follows_mouse", Bool(false)),
            ("session_serialization", Bool(false)),
            ("disable_session_metadata", Bool(true)),
        ]
    );
    let configured = ZellijConfig {
        mouse_mode: Some(true),
        mouse_click_through: false,
        focus_follows_mouse: true,
        session_serialization: true,
        disable_session_metadata: false,
        advanced_mouse_actions: Some(true),
        mouse_hover_effects: Some(false),
        pane_frames: Some(true),
        on_force_close: Some(ZellijForceClose::Quit),
        scroll_buffer_size: Some(200_000),
        show_startup_tips: Some(true),
        show_release_notes: Some(false),
        copy_clipboard: Some(ZellijClipboard::Primary),
        copy_on_select: Some(false),
        support_kitty_keyboard_protocol: Some(true),
        osc8_hyperlinks: Some(false),
    };
    assert_eq!(
        pairs(&configured),
        vec![
            ("auto_layout", Bool(false)),
            ("stacked_resize", Bool(true)),
            ("stacked_pane_list", Bool(false)),
            ("mouse_click_through", Bool(false)),
            ("focus_follows_mouse", Bool(true)),
            ("session_serialization", Bool(true)),
            ("disable_session_metadata", Bool(false)),
            ("advanced_mouse_actions", Bool(true)),
            ("mouse_hover_effects", Bool(false)),
            ("pane_frames", Bool(true)),
            ("on_force_close", Word("quit")),
            ("scroll_buffer_size", Int(200_000)),
            ("show_startup_tips", Bool(true)),
            ("show_release_notes", Bool(false)),
            ("copy_clipboard", Word("primary")),
            ("copy_on_select", Bool(false)),
            ("support_kitty_keyboard_protocol", Bool(true)),
            ("osc8_hyperlinks", Bool(false)),
        ]
    );
}

#[test]
fn zellij_client_options_never_enable_xor_booleans() {
    for mouse_mode in [None, Some(true), Some(false)] {
        for kitty in [None, Some(true), Some(false)] {
            let config = ZellijConfig {
                mouse_mode,
                support_kitty_keyboard_protocol: kitty,
                pane_frames: Some(true),
                focus_follows_mouse: true,
                session_serialization: true,
                ..ZellijConfig::default()
            };
            let args = zellij_client_options_args(&config);
            let mut expected = expected_option_map("--default-mode=locked");
            if mouse_mode == Some(false) {
                expected.insert("--mouse-mode", "false");
            }
            if let Some(value) = kitty {
                expected.insert(
                    "--support-kitty-keyboard-protocol",
                    if value { "true" } else { "false" },
                );
            }
            assert_eq!(option_map(&args), expected);
            for pair in args.chunks_exact(2) {
                if pair[1] == "true" {
                    assert_eq!(pair[0], "--support-kitty-keyboard-protocol");
                }
            }
        }
    }
}

/// A shim that counts its `action` runs in `runs` and answers each with
/// `answer`, a shell fragment that may read the count as `$n`.
#[cfg(unix)]
fn counting_action_shim(answer: &str) -> (tempfile::TempDir, ZellijBackend) {
    let (temp, shim) = zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
n=$(($(cat "$dir/runs" 2>/dev/null || echo 0) + 1))
echo "$n" > "$dir/runs"
{answer}
"#
    ));
    // The shim is `zellij` in the temp dir, where the socket base would land.
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, temp.path().join("run"));
    (temp, backend)
}

#[cfg(unix)]
fn action_runs(temp: &tempfile::TempDir) -> u32 {
    std::fs::read_to_string(temp.path().join("runs"))
        .expect("shim run count")
        .trim()
        .parse()
        .expect("numeric run count")
}

/// Pin `spec`'s socket directory inside the fixture, so no socket entry lands
/// in a `ZELLIJ_SOCKET_DIR` the test process inherited.
#[cfg(unix)]
fn pinned(temp: &tempfile::TempDir, spec: CommandSpec) -> CommandSpec {
    spec.env(
        "ZELLIJ_SOCKET_DIR",
        temp.path().join("sock").to_string_lossy(),
    )
}

/// Put a socket-dir entry for `session` where a pinned command looks.
#[cfg(unix)]
fn leave_session_socket(temp: &tempfile::TempDir, session: &str) {
    let socket = socket::spec_socket_path(&pinned(temp, CommandSpec::new("zellij")), session);
    std::fs::create_dir_all(socket.parent().expect("socket dir")).expect("socket dir");
    std::fs::write(socket, "").expect("socket entry");
}

#[cfg(unix)]
#[test]
fn a_spec_socket_path_follows_the_commands_own_environment() {
    let spec = CommandSpec::new("zellij")
        .env("XDG_RUNTIME_DIR", "/x")
        .env("TMPDIR", "/t");
    let set = spec.clone().env("ZELLIJ_SOCKET_DIR", "/s");
    assert_eq!(
        socket::spec_socket_path(&set, "room"),
        Path::new("/s/contract_version_1/room")
    );
    // A removed key is unset in the child whatever this process exports.
    let removed = spec.env_remove("ZELLIJ_SOCKET_DIR");
    let xdg_home = if cfg!(target_os = "linux") {
        "/x/zellij/contract_version_1/room"
    } else {
        "/t"
    };
    assert!(socket::spec_socket_path(&removed, "room").starts_with(xdg_home));
    let no_xdg = CommandSpec::new("zellij")
        .env("TMPDIR", "/t")
        .env_remove("ZELLIJ_SOCKET_DIR")
        .env_remove("XDG_RUNTIME_DIR");
    assert!(
        socket::spec_socket_path(&no_xdg, "room").starts_with("/t"),
        "falls to the command's TMPDIR"
    );
}

#[cfg(unix)]
#[test]
fn a_rerun_that_outlasts_the_deadline_reports_the_callers_bound() {
    let (temp, backend) = counting_action_shim(&format!(
        "if [ \"$n\" -lt 2 ]; then {REFUSE}; fi; exec sleep 30"
    ));
    leave_session_socket(&temp, "rimz-test");

    let err = pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("new-pane")
        .run_with_timeout(Duration::from_secs(1))
        .expect_err("the rerun hangs");

    assert!(matches!(err, MuxErr::Timeout { seconds: 1, .. }), "{err:?}");
    assert_eq!(action_runs(&temp), 2);
}

#[cfg(unix)]
const REFUSE: &str = "printf 'There is no active session!\\n' >&2; exit 1";

#[cfg(unix)]
#[test]
fn a_refusal_of_a_live_session_is_rerun_until_the_action_lands() {
    let (temp, backend) = counting_action_shim(&format!(
        "if [ \"$n\" -lt 3 ]; then {REFUSE}; fi; printf 'done\\n'"
    ));
    leave_session_socket(&temp, "rimz-test");

    let output = pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("new-pane")
        .run()
        .expect("the refusal clears");

    assert_eq!(output.stdout, b"done\n");
    assert_eq!(action_runs(&temp), 3);
}

#[cfg(unix)]
#[test]
fn a_refusal_that_never_clears_stops_at_its_bound() {
    let (temp, backend) = counting_action_shim(REFUSE);
    leave_session_socket(&temp, "rimz-test");

    let err = pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("new-pane")
        .run()
        .expect_err("the refusal stands");

    assert!(matches!(err, MuxErr::Command { .. }), "{err:?}");
    assert_eq!(action_runs(&temp), 1 + PREDISPATCH_REFUSAL_RERUNS);
}

#[cfg(unix)]
#[test]
fn an_absent_session_and_an_unrelated_failure_are_answered_once() {
    // No socket: the session is gone, and its refusal is the answer.
    let (temp, backend) = counting_action_shim(REFUSE);
    let err = pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("new-pane")
        .run()
        .expect_err("absent session");
    assert!(
        matches!(&err, MuxErr::Command { stderr, .. } if stderr.contains("no active session")),
        "{err:?}"
    );
    assert_eq!(action_runs(&temp), 1);

    // Another session's socket does not vouch for the named one.
    leave_session_socket(&temp, "rimz-other");
    pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("new-pane")
        .run()
        .expect_err("absent session beside a live one");
    assert_eq!(action_runs(&temp), 2);

    // A live session's own failure may follow a dispatched action.
    let (temp, backend) = counting_action_shim("printf 'pane not found\\n' >&2; exit 1");
    leave_session_socket(&temp, "rimz-test");
    pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("close-pane")
        .run()
        .expect_err("unrelated failure");
    assert_eq!(action_runs(&temp), 1);

    // The banner on a successful exit is the caller's to classify.
    let (temp, backend) = counting_action_shim("printf 'There is no active session!\\n' >&2");
    leave_session_socket(&temp, "rimz-test");
    pinned(&temp, backend.zellij_action("rimz-test"))
        .arg("list-panes")
        .run()
        .expect("exit 0");
    assert_eq!(action_runs(&temp), 1);
}

#[cfg(unix)]
fn planned_resume_tab(label: &str, panes: usize) -> crate::mux::ResumeTab {
    crate::mux::ResumeTab {
        label: label.to_owned(),
        cwd: std::path::PathBuf::from("/tmp"),
        env: Default::default(),
        layout: LayoutPanes {
            columns: vec![LayoutColumn {
                panes: vec![
                    PaneCmd {
                        argv: vec!["sh".to_owned()],
                        name: None,
                    };
                    panes
                ],
                stacked: false,
            }],
            focused_pane: 0,
        },
    }
}

#[cfg(unix)]
#[test]
fn resume_confirmation_counts_work_panes_per_live_tab() {
    use crate::mux::{ResumeTabShape, ResumeTabUnconfirmed, confirm_resume_tab_shapes};

    // `#seeded` holds two agents beside its sidebar, a plugin, a float, and an
    // exited pane; `#thin` holds one agent and wears a status glyph.
    let theme = crate::config::ThemeConfig::default();
    let glyph = format!(
        " {}",
        crate::theme::unicode_glyph(crate::config::GlyphRole::StatusWorking)
    );
    let panes: Vec<backend::RawListedPane> = serde_json::from_str(&format!(
        r##"[
  {{"id":1,"tab_id":10,"tab_name":"#seeded","title":"rimz-sidebar"}},
  {{"id":2,"tab_id":10,"tab_name":"#seeded","title":"claude"}},
  {{"id":3,"tab_id":10,"tab_name":"#seeded","title":"codex"}},
  {{"id":4,"tab_id":10,"tab_name":"#seeded","title":"gone","exited":true}},
  {{"id":5,"tab_id":10,"tab_name":"#seeded","title":"float","is_floating":true}},
  {{"id":0,"tab_id":10,"tab_name":"#seeded","is_plugin":true}},
  {{"id":6,"tab_id":11,"tab_name":"#thin{glyph}","title":"rimz-sidebar"}},
  {{"id":7,"tab_id":11,"tab_name":"#thin{glyph}","title":"claude"}}
]"##
    ))
    .expect("listing");
    let live = backend::live_tab_shapes(&panes, &theme);
    assert_eq!(
        live,
        [
            ResumeTabShape {
                name: "#seeded".to_owned(),
                panes: 2
            },
            ResumeTabShape {
                name: "#thin".to_owned(),
                panes: 1
            },
        ]
    );
    let planned =
        [("#seeded", 2), ("#absent", 1), ("#seeded", 2), ("#thin", 2)].map(|(name, panes)| {
            ResumeTabShape {
                name: name.to_owned(),
                panes,
            }
        });
    assert_eq!(
        confirm_resume_tab_shapes(&planned, &live),
        [
            Ok(()),
            Err(ResumeTabUnconfirmed::Absent),
            // One live tab answers for one planned tab.
            Err(ResumeTabUnconfirmed::Absent),
            Err(ResumeTabUnconfirmed::ShortOfPanes {
                found: 1,
                planned: 2
            }),
        ]
    );
}

#[cfg(unix)]
#[test]
fn resume_confirmation_waits_for_panes_and_fails_every_tab_on_a_dead_listing() {
    use crate::mux::ResumeTabUnconfirmed;

    // The tab lists with one pane first, then with both.
    let (temp, shim) = zellij_shim(
        r##"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    if [ -e "$dir/listed" ]; then
      printf '[{"id":1,"tab_id":10,"tab_name":"#seeded","title":"claude"},{"id":2,"tab_id":10,"tab_name":"#seeded","title":"codex"}]\n'
    else
      : > "$dir/listed"
      printf '[{"id":1,"tab_id":10,"tab_name":"#seeded","title":"claude"}]\n'
    fi
    exit 0 ;;
esac
exit 1
"##,
    );
    assert_eq!(
        ZellijBackend::with_program_for_test(&shim)
            .confirm_resume_tabs("rimz-test", &[planned_resume_tab("#seeded", 2)]),
        [Ok(())]
    );
    assert_eq!(command_count(&shim_log(&temp), LIST_PANES), 2);

    let (_temp, shim) = zellij_shim("#!/bin/sh\necho 'no server' >&2; exit 1\n");
    let outcomes = ZellijBackend::with_program_for_test(&shim).confirm_resume_tabs(
        "rimz-test",
        &[
            planned_resume_tab("#seeded", 2),
            planned_resume_tab("#thin", 1),
        ],
    );
    assert!(
        matches!(
            outcomes[..],
            [
                Err(ResumeTabUnconfirmed::Unlisted(_)),
                Err(ResumeTabUnconfirmed::Unlisted(_))
            ]
        ),
        "{outcomes:?}"
    );

    let (_temp, shim) = zellij_shim("#!/bin/sh\nexec sleep 60\n");
    let started = std::time::Instant::now();
    let outcomes = ZellijBackend::with_program_for_test(&shim)
        .confirm_resume_tabs("rimz-test", &[planned_resume_tab("#seeded", 2)]);
    assert!(matches!(
        outcomes[..],
        [Err(ResumeTabUnconfirmed::Unlisted(_))]
    ));
    assert!(
        started.elapsed() < NEW_TAB_MATERIALIZE_WINDOW + std::time::Duration::from_secs(5),
        "a stalled listing must share the confirmation deadline: {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn resume_confirmation_keeps_last_observation_when_its_deadline_expires() {
    use crate::mux::ResumeTabUnconfirmed;

    let (_temp, shim) = zellij_shim(
        r##"#!/bin/sh
dir=$(dirname "$0")
if [ -e "$dir/listed" ]; then exec sleep 60; fi
: > "$dir/listed"
printf '[{"id":1,"tab_id":10,"tab_name":"#seeded","title":"claude"}]\n'
"##,
    );
    assert_eq!(
        ZellijBackend::with_program_for_test(&shim).confirm_resume_tabs(
            "rimz-test",
            &[
                planned_resume_tab("#seeded", 1),
                planned_resume_tab("#absent", 1),
            ],
        ),
        [Ok(()), Err(ResumeTabUnconfirmed::Absent)]
    );
}
