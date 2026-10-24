use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use rimz::agents::PermissionMode;
use rimz::ids::AgentKind;
use rimz::mux::{MuxBackend, SplitPaneOptions, SplitPlacement, SplitTarget};
use rimz::store::run::RunRecord;

use super::support::*;
use crate::common::{Env, ZellijNamespace};

#[test]
fn workflow_layout_closes_all_cells_and_preserves_unrelated_panes() {
    require_zellij!();
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let room = LiveZellijSession::from_namespace(ZellijNamespace::new(), workspace.session_name);
    let mut sidebar = sidebar_opts(room.name(), &env.project_root, env.rimz_bin(), 160);
    sidebar.workspace_id = workspace.workspace_id;
    sidebar.runtime =
        rimz::RuntimePaths::for_state_under(&env.state_path_for(&env.project_root), room.path());
    sidebar
        .extra_env
        .insert("RIMZ_HOME".into(), env.rimz_home().display().to_string());
    publish_room_bin(room.path(), &sidebar);
    room.backend().open_sidebar(&sidebar, None).unwrap();
    let unrelated = wait_for_pane_count(room.path(), room.name(), 2)
        .into_iter()
        .find(|pane| pane.title.as_deref() != Some("rimz-sidebar"))
        .unwrap()
        .pane_id;
    let _client = AttachedClient::attach(&room, 160, 40);
    crate::backend::workflow::check_layout(&env, room.backend(), room.name(), &unrelated, || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("XDG_RUNTIME_DIR", room.path())
            .env("XDG_CACHE_HOME", room.path());
        command
    });
}

#[test]
fn terminal_run_closes_only_its_command_pane() {
    require_zellij!();
    let env = Env::new();
    env.record(&env.project_root);
    let room = LiveZellijSession::new("command-cell");
    room.create_plain_background(&env.project_root, "60");
    let unrelated = wait_for_pane_count(room.path(), room.name(), 1)[0]
        .pane_id
        .clone();
    let store = env.store();
    let mut workspace = rimz::workspace::record::read(&store.paths().workspace_record).unwrap();
    workspace.session_name = room.name().to_owned();
    rimz::workspace::record::write(store.paths(), &workspace).unwrap();
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".into(),
        env.project_root.clone(),
    );
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let ready = env.home_root.join("command-ready");
    let environment = BTreeMap::from([
        ("RIMZ_HOME".into(), env.rimz_home().display().to_string()),
        ("HOME".into(), env.home_root.display().to_string()),
        ("XDG_RUNTIME_DIR".into(), room.path().display().to_string()),
    ]);
    room.backend()
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Session(room.name().to_owned()),
            command: Some(vec![
                env.rimz_bin().display().to_string(),
                "--zellij".into(),
                "agents".into(),
                "run-command".into(),
                "--workspace".into(),
                env.workspace_id.to_string(),
                "--run".into(),
                record.run_id.to_string(),
                "--".into(),
                "sh".into(),
                "-c".into(),
                "printf ready > \"$1\"; exec sleep 60".into(),
                "command".into(),
                ready.display().to_string(),
            ]),
            cwd: Some(env.project_root.display().to_string()),
            env: environment,
            title: Some("command-cell".into()),
            close_on_exit: false,
            placement: SplitPlacement::default(),
            focus: false,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(ready.exists(), "a live run must start its command");
    let own = wait_for_pane_count(room.path(), room.name(), 2)
        .into_iter()
        .find(|pane| pane.pane_id != unrelated)
        .unwrap()
        .pane_id;
    rimz::harness::run::cancel(store.paths(), &record.run_id).unwrap();
    let panes = poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |panes| panes.pane_refs().iter().all(|pane| pane.pane_id != own),
        "terminal run closes its command pane",
    );
    assert!(
        panes
            .pane_refs()
            .iter()
            .any(|pane| pane.pane_id == unrelated),
        "unrelated pane must survive"
    );
}
