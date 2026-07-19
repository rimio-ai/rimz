use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::policy::{self, KEEPALIVE_MS, POKE_FLOOR_MS, SETTLE_POKE_MS};

#[derive(Clone)]
struct FakeHost {
    pids: BTreeMap<u32, u32>,
    pid_calls: RefCell<Vec<u32>>,
    telemetry: PluginTelemetry,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            pids: BTreeMap::new(),
            pid_calls: RefCell::new(Vec::new()),
            telemetry: PluginTelemetry {
                plugin_id: None,
                plugin_build: None,
                loaded_at_ms: 0,
                mem_pages: 12,
                uptime_ms: 0,
                commands_completed: 34,
                commands_succeeded: 34,
                commands_failed: 0,
                stale_writer_rejections: 0,
                topology_failures: 0,
                other_failures: 0,
                zellij_version: "0.44.3".to_owned(),
            },
        }
    }
}

impl Host for FakeHost {
    fn pane_pid(&self, pane_id: u32) -> Option<u32> {
        self.pid_calls.borrow_mut().push(pane_id);
        self.pids.get(&pane_id).copied()
    }

    fn telemetry(&self) -> PluginTelemetry {
        self.telemetry.clone()
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        workspace_id: Some("workspace-1".to_owned()),
        session_name: Some("session-1".to_owned()),
        rimz_bin: Some("/bin/rimz".to_owned()),
        plugin_id: Some(9),
        plugin_build: Some("wasm-build".to_owned()),
        plugin_config: Some("config-hash".to_owned()),
        focus_key: None,
        focus_follows_mouse: None,
        mouse_click_through: None,
    }
}

fn reconfigure_config() -> EngineConfig {
    EngineConfig {
        focus_key: Some("Alt+p".to_owned()),
        focus_follows_mouse: Some(false),
        mouse_click_through: Some(true),
        ..config()
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_suppressed: false,
        is_floating: false,
        exited: false,
        is_held: false,
        tab_position: 0,
        tab_name: Some("main".to_owned()),
        pane_x: Some(0),
        pane_columns: Some(80),
        title: format!("pane-{id}"),
        pane_command: None,
        pane_cwd: None,
        pane_pid: None,
        terminal_command: Some("zsh".to_owned()),
    }
}

fn pane_in_tab(id: u32, tab: usize) -> PaneFields {
    PaneFields {
        tab_position: tab as u64,
        tab_name: Some(format!("tab-{tab}")),
        ..pane(id)
    }
}

fn plugin_pane(id: u32) -> PaneFields {
    PaneFields {
        is_plugin: true,
        ..pane(id)
    }
}

fn tabs(panes: Vec<PaneFields>) -> BTreeMap<usize, Vec<PaneFields>> {
    BTreeMap::from([(0, panes)])
}

fn tabs_by_index(entries: Vec<(usize, Vec<PaneFields>)>) -> BTreeMap<usize, Vec<PaneFields>> {
    entries.into_iter().collect()
}

fn raw_hash(tabs: &BTreeMap<usize, Vec<PaneFields>>) -> u64 {
    policy::raw_stable_hash(tabs.iter().flat_map(|(tab, panes)| {
        panes.iter().map(move |pane| {
            (
                *tab,
                policy::RawStablePaneFields {
                    id: pane.id,
                    is_plugin: pane.is_plugin,
                    is_suppressed: pane.is_suppressed,
                    is_floating: pane.is_floating,
                    exited: pane.exited,
                    is_held: pane.is_held,
                    tab_position: pane.tab_position,
                    tab_name: pane.tab_name.as_deref(),
                    pane_x: pane.pane_x,
                    pane_columns: pane.pane_columns,
                    terminal_command: pane.terminal_command.as_deref(),
                },
            )
        })
    }))
}

fn grant(engine: &mut Engine, now: u64, host: &FakeHost) {
    let _ = engine.on_permission_granted(now, host);
}

fn seed_manifest(
    engine: &mut Engine,
    manifest: BTreeMap<usize, Vec<PaneFields>>,
    now: u64,
    host: &FakeHost,
) {
    let hash = raw_hash(&manifest);
    let _ = engine.on_pane_manifest(hash, |_| manifest, now, host);
}

fn run_commands(effects: &[Effect]) -> Vec<&Vec<String>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::RunCommand(argv) => Some(argv),
            _ => None,
        })
        .collect()
}

fn reasons(effects: &[Effect]) -> Vec<&str> {
    run_commands(effects)
        .into_iter()
        .filter_map(|argv| arg_after(argv, "--reason"))
        .collect()
}

fn arg_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

fn topology_json(argv: &[String]) -> serde_json::Value {
    serde_json::from_str(arg_after(argv, "--topology").expect("topology argv"))
        .expect("topology JSON")
}

fn has_timeout(effects: &[Effect], delay_ms: u64) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::SetTimeout(actual) if *actual == delay_ms))
}

#[test]
fn pregrant_unknown_patch_is_retained_until_the_manifest_arrives() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, reconfigure_config());

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["codex"]),
        true,
        10,
        &host,
    );
    assert!(run_commands(&effects).is_empty());

    let effects = engine.on_permission_granted(20, &host);
    assert!(effects.contains(&Effect::HideSelf));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Reconfigure(_)))
    );
    assert!(effects.contains(&Effect::ListClients));
    assert_eq!(reasons(&effects), vec!["panes-changed"]);

    let manifest = tabs(vec![pane(1)]);
    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest, 30, &host);
    assert!(run_commands(&effects).is_empty());
    let effects = engine.on_dump_topology_pipe(40, &host);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_command"], "codex");

    let mut fresh = Engine::new(0, config());
    let effects = fresh.on_permission_granted(20, &host);
    assert!(effects.contains(&Effect::ListClients));
    assert_eq!(reasons(&effects), vec!["alive"]);
}

#[test]
fn cached_grant_on_first_manifest_hides_and_first_manifest_is_baseline() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    let manifest = tabs(vec![pane(1), pane(2)]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 10, &host);

    assert!(effects.contains(&Effect::HideSelf));
    assert!(effects.contains(&Effect::ListClients));
    assert_eq!(reasons(&effects), vec!["alive"]);
}

#[test]
fn first_manifest_after_explicit_grant_is_baseline() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let manifest = tabs(vec![pane(1), pane(2)]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 20, &host);

    assert!(run_commands(&effects).is_empty());
}

#[test]
fn stable_manifest_skips_lazy_projection() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);
    let renamed = tabs(vec![PaneFields {
        title: "title-only churn".to_owned(),
        ..pane(1)
    }]);

    let effects = engine.on_pane_manifest(
        raw_hash(&renamed),
        |_| panic!("stable manifest must not project"),
        30,
        &host,
    );

    assert!(run_commands(&effects).is_empty());
}

#[test]
fn manifest_adding_two_card_panes_emits_one_changed_snapshot() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);
    let manifest = tabs(vec![pane(1), pane(2), pane(3)]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 30, &host);

    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

#[test]
fn command_and_close_changes_share_the_room_poke_floor() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), pane(2)]), 20, &host);

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim"]),
        true,
        100,
        &host,
    );
    assert_eq!(reasons(&effects), vec!["panes-changed"]);

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim", "README.md"]),
        true,
        150,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
    assert!(has_timeout(&effects, POKE_FLOOR_MS - 50));

    let effects = engine.on_timer(100 + POKE_FLOOR_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(2),
        strings(&["top"]),
        true,
        210,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
    assert!(has_timeout(&effects, POKE_FLOOR_MS - 10));

    let _ = engine.on_pane_closed(ProjectedPaneId::Terminal(1), 220, &host);
    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["python"]),
        true,
        230,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
}

#[test]
fn pane_closed_signals_canonical_room_changes() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), plugin_pane(9)]), 20, &host);

    let effects = engine.on_pane_closed(ProjectedPaneId::Terminal(1), 30, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);

    let effects = engine.on_pane_closed(ProjectedPaneId::Plugin(9), 40, &host);
    assert!(run_commands(&effects).is_empty());
}

#[test]
fn dump_topology_bypasses_floor_and_pregrant_dump_holds_signal() {
    let host = FakeHost::default();
    let mut pregrant = Engine::new(0, config());
    let effects = pregrant.on_dump_topology_pipe(10, &host);
    assert!(effects.is_empty());
    let effects = pregrant.on_permission_granted(20, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);

    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);
    let _ = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim"]),
        true,
        100,
        &host,
    );

    let effects = engine.on_dump_topology_pipe(150, &host);
    assert_eq!(reasons(&effects), vec!["alive"]);
    assert!(
        run_commands(&effects)[0].contains(&"--topology".to_owned()),
        "dump publishes immediate topology even inside the duplicate floor",
    );
}

#[test]
fn manifest_probes_each_pane_pid_once_including_failures() {
    let mut host = FakeHost::default();
    host.pids.insert(1, 101);
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let initial = tabs(vec![pane(1)]);
    let _ = engine.on_pane_manifest(raw_hash(&initial), |_| initial, 20, &host);
    let manifest = tabs(vec![pane(1), pane(2)]);
    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 200, &host);

    assert_eq!(*host.pid_calls.borrow(), vec![1, 2]);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
    assert!(topology["panes"][1].get("pane_pid").is_none());

    let changed = tabs(vec![pane(1), pane(2)]);
    let _ = engine.on_pane_manifest(raw_hash(&changed), |_| changed, 300, &host);
    assert_eq!(
        *host.pid_calls.borrow(),
        vec![1, 2],
        "a failed pid lookup is not retried on later manifests",
    );
}

#[test]
fn dump_topology_on_fresh_state_probes_missing_pids() {
    let mut host = FakeHost::default();
    host.pids.insert(1, 101);
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    assert!(engine.room.apply_manifest(tabs(vec![pane(1)]), &host));

    let effects = engine.on_dump_topology_pipe(20, &host);

    assert_eq!(reasons(&effects), vec!["alive"]);
    assert_eq!(*host.pid_calls.borrow(), vec![1]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
}

#[test]
fn canonical_room_retains_partial_tabs_and_event_enrichment_while_moving_panes() {
    let host = FakeHost::default();
    let mut room = RoomState::default();
    assert!(room.apply_manifest(
        tabs_by_index(vec![
            (0, vec![pane_in_tab(10, 0)]),
            (1, vec![pane_in_tab(20, 1)]),
        ]),
        &host,
    ));
    assert!(room.update_foreground(
        ProjectedPaneId::Terminal(10),
        Some("codex --search".to_owned()),
    ));

    assert!(!room.apply_manifest(tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]), &host,));
    assert!(!room.apply_manifest(tabs_by_index(vec![(1, Vec::new())]), &host));
    let retained = room.published_panes();
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0].pane_command.as_deref(), Some("codex --search"));
    assert_eq!(retained[1].tab_position, 1);

    assert!(room.apply_manifest(
        tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), pane_in_tab(20, 0)],)]),
        &host,
    ));
    let moved = room.published_panes();
    assert_eq!(
        moved.iter().map(|pane| pane.id).collect::<Vec<_>>(),
        vec![10, 20]
    );
    assert!(moved.iter().all(|pane| pane.tab_position == 0));
}

#[test]
fn canonical_room_ignores_command_enrichment_for_plugin_panes() {
    let host = FakeHost::default();
    let mut room = RoomState::default();
    assert!(room.apply_manifest(tabs(vec![plugin_pane(9)]), &host));

    assert!(!room.update_foreground(ProjectedPaneId::Plugin(9), Some("ignored".to_owned()),));
    assert!(room.published_panes()[0].pane_command.is_none());
}

#[test]
fn cwd_changed_updates_published_topology_and_signals() {
    let mut host = FakeHost::default();
    host.pids.insert(1, 101);
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);

    let effects = engine.on_cwd_changed(
        ProjectedPaneId::Terminal(1),
        Some("/repo/main".to_owned()),
        200,
        &host,
    );

    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_cwd"], "/repo/main");
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
}

#[test]
fn shell_command_replaces_finished_foreground_in_topology() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);
    let _ = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["sleep", "5"]),
        true,
        200,
        &host,
    );

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["zsh"]),
        false,
        400,
        &host,
    );

    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_command"], "zsh");
}

#[test]
fn closing_a_pane_prunes_enrichment_and_allows_a_new_lifetime_probe() {
    let mut host = FakeHost::default();
    host.pids.insert(1, 101);
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let manifest = tabs(vec![pane(1)]);
    seed_manifest(&mut engine, manifest.clone(), 20, &host);
    let _ = engine.on_cwd_changed(
        ProjectedPaneId::Terminal(1),
        Some("/old".to_owned()),
        30,
        &host,
    );
    let _ = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["zsh"]),
        false,
        40,
        &host,
    );

    let _ = engine.on_pane_closed(ProjectedPaneId::Terminal(1), 50, &host);
    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest, 200, &host);

    assert_eq!(*host.pid_calls.borrow(), vec![1, 1]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert!(topology["panes"][0].get("pane_command").is_none());
    assert!(topology["panes"][0].get("pane_cwd").is_none());
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
}

#[test]
fn list_clients_publish_presence_and_unique_session_focus() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), pane(2)]), 20, &host);

    let sample = vec![
        ProjectedClientFocus {
            client_id: 2,
            pane_id: ProjectedPaneId::Terminal(2),
        },
        ProjectedClientFocus {
            client_id: 1,
            pane_id: ProjectedPaneId::Terminal(1),
        },
        ProjectedClientFocus {
            client_id: 1,
            pane_id: ProjectedPaneId::Terminal(1),
        },
    ];
    let effects = engine.on_list_clients(sample.clone(), 30, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert!(topology["clients"].get("human_clients").is_none());
    assert!(topology["clients"].get("viewed_panes").is_none());
    assert_eq!(topology["clients"]["views"].as_array().unwrap().len(), 2);

    let effects = engine.on_list_clients(sample, 40, &host);
    assert!(
        run_commands(&effects).is_empty(),
        "unchanged client sample should not wake"
    );

    let effects = engine.on_list_clients(
        vec![ProjectedClientFocus {
            client_id: 1,
            pane_id: ProjectedPaneId::Terminal(1),
        }],
        50,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
    let effects = engine.on_timer(30 + POKE_FLOOR_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

fn client(client_id: u16, pane_id: ProjectedPaneId) -> ProjectedClientFocus {
    ProjectedClientFocus { client_id, pane_id }
}

fn seed_switch_room(engine: &mut Engine, host: &FakeHost) -> BTreeMap<usize, String> {
    let names = BTreeMap::from([(0, "tab-0".to_owned()), (1, "tab-1".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, host);
    seed_manifest(
        engine,
        tabs_by_index(vec![
            (0, vec![pane_in_tab(1, 0)]),
            (1, vec![pane_in_tab(10, 1), pane_in_tab(11, 1)]),
        ]),
        30,
        host,
    );
    let _ = engine.on_list_clients(vec![client(1, ProjectedPaneId::Terminal(1))], 40, host);
    let _ = engine.on_list_clients(vec![client(1, ProjectedPaneId::Terminal(1))], 50, host);
    names
}

#[test]
fn stable_pane_update_refreshes_clients_without_republishing_topology() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let manifest = tabs(vec![pane(1)]);
    seed_manifest(&mut engine, manifest.clone(), 20, &host);
    let _ = engine.on_list_clients(Vec::new(), 30, &host);
    let _ = engine.on_list_clients(Vec::new(), 40, &host);

    let effects = engine.on_pane_manifest(
        raw_hash(&manifest),
        |_| panic!("stable PaneUpdate must not project topology"),
        50,
        &host,
    );

    assert_eq!(
        effects
            .iter()
            .filter(|effect| **effect == Effect::ListClients)
            .count(),
        1
    );
    assert!(run_commands(&effects).is_empty());
}

#[test]
fn tab_switch_emits_one_settled_observation_at_the_deadline() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = seed_switch_room(&mut engine, &host);

    let effects = engine.on_tab_update(Some(1), names, 100, &host);
    assert!(!effects.contains(&Effect::ListClients));
    assert!(
        !engine
            .on_timer(100 + policy::FOCUS_SETTLE_MS - 1, &host)
            .contains(&Effect::ListClients)
    );

    let effects = engine.on_timer(100 + policy::FOCUS_SETTLE_MS, &host);
    assert!(effects.contains(&Effect::ListClients));
    let settled = engine.on_list_clients(
        vec![client(1, ProjectedPaneId::Terminal(10))],
        100 + policy::FOCUS_SETTLE_MS + 1,
        &host,
    );
    assert!(reasons(&settled).contains(&"switch-settled"));
    assert_eq!(
        arg_after(run_commands(&settled)[0], "--active-tab"),
        Some("1")
    );
    assert_eq!(
        arg_after(run_commands(&settled)[0], "--focus-generation"),
        Some("1")
    );
    assert_eq!(
        arg_after(run_commands(&settled)[0], "--focus-clients"),
        Some(r#"[{"client_id":1,"pane_id":{"kind":"terminal","id":10}}]"#)
    );
    let topology = topology_json(run_commands(&settled)[0]);
    assert!(topology.get("focused_pane").is_none());
    assert_eq!(topology["clients"]["views"][0]["pane_id"]["id"], 10);
    assert_eq!(
        run_commands(&settled)[0][1..4],
        ["sidebar", "wake", "--reason"],
        "plugin reports evidence and leaves classification to the host",
    );
}

#[test]
fn rapid_switch_supersedes_the_old_query() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = seed_switch_room(&mut engine, &host);

    let first = engine.on_tab_update(Some(1), names.clone(), 100, &host);
    assert!(!first.contains(&Effect::ListClients));
    assert!(
        engine
            .on_timer(100 + policy::FOCUS_SETTLE_MS, &host)
            .contains(&Effect::ListClients)
    );
    let second = engine.on_tab_update(Some(0), names, 360, &host);
    assert!(!second.contains(&Effect::ListClients));

    let stale = engine.on_list_clients(vec![client(1, ProjectedPaneId::Terminal(11))], 370, &host);
    assert!(!reasons(&stale).contains(&"switch-settled"));

    assert!(
        engine
            .on_timer(360 + policy::FOCUS_SETTLE_MS, &host)
            .contains(&Effect::ListClients)
    );
    let latest = engine.on_list_clients(
        vec![client(1, ProjectedPaneId::Terminal(1))],
        360 + policy::FOCUS_SETTLE_MS + 1,
        &host,
    );
    assert!(reasons(&latest).contains(&"switch-settled"));
    assert_eq!(
        arg_after(run_commands(&latest)[0], "--focus-generation"),
        Some("2")
    );
}

#[test]
fn same_and_distinct_client_views_publish_without_a_focus_verdict() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), pane(2)]), 20, &host);
    let _ = engine.on_list_clients(Vec::new(), 30, &host);
    let _ = engine.on_list_clients(Vec::new(), 40, &host);

    let effects = engine.on_list_clients(
        vec![
            client(1, ProjectedPaneId::Terminal(1)),
            client(2, ProjectedPaneId::Terminal(1)),
        ],
        50,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
    let effects = engine.on_timer(30 + POKE_FLOOR_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert!(topology.get("focused_pane").is_none());
    assert_eq!(topology["clients"]["views"].as_array().unwrap().len(), 2);

    let effects = engine.on_list_clients(
        vec![
            client(1, ProjectedPaneId::Terminal(1)),
            client(2, ProjectedPaneId::Terminal(2)),
        ],
        140,
        &host,
    );
    assert!(run_commands(&effects).is_empty());
    let effects = engine.on_timer(30 + 2 * POKE_FLOOR_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    assert!(
        topology_json(run_commands(&effects)[0])
            .get("focused_pane")
            .is_none()
    );
}

#[test]
fn detached_switch_still_emits_the_settled_observation() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = seed_switch_room(&mut engine, &host);
    let _ = engine.on_tab_update(Some(1), names, 100, &host);
    assert!(
        engine
            .on_timer(100 + policy::FOCUS_SETTLE_MS, &host)
            .contains(&Effect::ListClients)
    );
    let settled = engine.on_list_clients(Vec::new(), 100 + policy::FOCUS_SETTLE_MS + 1, &host);
    assert!(reasons(&settled).contains(&"switch-settled"));
    assert_eq!(
        arg_after(run_commands(&settled)[0], "--focus-clients"),
        Some("[]")
    );
}

#[test]
fn expired_untagged_reply_is_general_and_rearms_settled_query() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = seed_switch_room(&mut engine, &host);
    let _ = engine.on_tab_update(Some(1), names, 100, &host);

    let expired = engine.on_timer(100 + policy::FOCUS_SETTLE_MS, &host);
    assert!(expired.contains(&Effect::ListClients));

    let stale = engine.on_list_clients(
        vec![client(1, ProjectedPaneId::Terminal(10))],
        100 + policy::FOCUS_SETTLE_MS + KEEPALIVE_MS + 1,
        &host,
    );
    assert!(!reasons(&stale).contains(&"switch-settled"));
    assert!(stale.contains(&Effect::ListClients));

    let settled = engine.on_list_clients(
        vec![client(1, ProjectedPaneId::Terminal(10))],
        100 + policy::FOCUS_SETTLE_MS + KEEPALIVE_MS + 2,
        &host,
    );
    assert!(reasons(&settled).contains(&"switch-settled"));
}

#[test]
fn plugin_client_views_publish_without_derived_presence_fields() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);

    let effects = engine.on_list_clients(vec![client(7, ProjectedPaneId::Plugin(99))], 30, &host);
    let topology = topology_json(run_commands(&effects)[0]);
    assert!(topology["clients"].get("human_clients").is_none());
    assert!(topology["clients"].get("viewed_panes").is_none());
    assert_eq!(topology["clients"]["views"][0]["client_id"], 7);
    assert!(topology.get("focused_pane").is_none());
}

#[test]
fn session_update_and_keepalive_request_client_sample() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let _ = engine.on_list_clients(Vec::new(), 15, &host);

    let effects = engine.on_session_update(Some(1), 20, &host);
    assert!(effects.contains(&Effect::ListClients));
    let effects = engine.on_session_update(Some(1), 30, &host);
    assert!(!effects.contains(&Effect::ListClients));
    let _ = engine.on_list_clients(Vec::new(), 35, &host);

    let effects = engine.on_timer(KEEPALIVE_MS, &host);
    assert!(effects.contains(&Effect::ListClients));
    assert_eq!(reasons(&effects), vec!["panes-changed", "alive"]);
}

#[test]
fn repeated_active_tab_update_keeps_the_original_settle_deadline() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned()), (1, "tab-1".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, &host);
    seed_manifest(
        &mut engine,
        tabs_by_index(vec![
            (0, vec![pane_in_tab(1, 0)]),
            (1, vec![pane_in_tab(10, 1), pane_in_tab(11, 1)]),
        ]),
        30,
        &host,
    );
    let _ = engine.on_list_clients(Vec::new(), 40, &host);
    let _ = engine.on_list_clients(Vec::new(), 50, &host);

    let effects = engine.on_tab_update(Some(1), names.clone(), 100, &host);
    assert!(!effects.contains(&Effect::ListClients));

    let effects = engine.on_tab_update(Some(1), names, 110, &host);
    assert!(!effects.contains(&Effect::ListClients));
    assert!(
        engine
            .on_timer(100 + policy::FOCUS_SETTLE_MS, &host)
            .contains(&Effect::ListClients)
    );
}

#[test]
fn client_query_merge_prefers_switches_and_newer_generations() {
    let older = ClientQueryPurpose::SwitchSettled {
        generation: 1,
        tab: 1,
    };
    let newer = ClientQueryPurpose::SwitchSettled {
        generation: 2,
        tab: 0,
    };

    assert_eq!(
        latest_client_query(ClientQueryPurpose::General, older),
        older
    );
    assert_eq!(
        latest_client_query(older, ClientQueryPurpose::General),
        older
    );
    assert_eq!(latest_client_query(older, newer), newer);
    assert_eq!(latest_client_query(newer, older), newer);
}

#[test]
fn retire_pipe_mutes_same_plugin_id_and_closes_different_older_instance() {
    let host = FakeHost::default();
    let mut same_id = Engine::new(0, config());
    grant(&mut same_id, 1, &host);
    seed_manifest(&mut same_id, tabs(vec![pane(1)]), 2, &host);

    assert_eq!(
        same_id.on_retire_pipe(Some(r#"{"plugin_id":9,"loaded_at_ms":10}"#)),
        vec![Effect::Unsubscribe]
    );
    assert!(same_id.on_focus_sidebar_pipe().is_empty());
    assert!(same_id.on_timer(KEEPALIVE_MS, &host).is_empty());
    assert!(
        same_id
            .on_pane_manifest(
                raw_hash(&tabs(vec![pane(1)])),
                |_| tabs(vec![pane(1)]),
                20,
                &host
            )
            .is_empty()
    );
    assert!(
        same_id
            .on_retire_pipe(Some(r#"{"plugin_id":9,"loaded_at_ms":20}"#))
            .is_empty()
    );

    let effects = same_id.on_dump_topology_pipe(30, &host);
    assert_eq!(effects.first(), Some(&Effect::Resubscribe));
    assert_eq!(reasons(&effects), vec!["alive"]);
    assert!(run_commands(&effects)[0].contains(&"--topology".to_owned()));
    assert!(!same_id.on_focus_sidebar_pipe().is_empty());
    assert_eq!(
        same_id.on_retire_pipe(Some(r#"{"plugin_id":9,"loaded_at_ms":40}"#)),
        vec![Effect::Unsubscribe],
        "a revived stale clone remains retireable",
    );

    let mut different_id = Engine::new(0, config());
    assert_eq!(
        different_id.on_retire_pipe(Some(r#"{"plugin_id":10,"loaded_at_ms":10}"#)),
        vec![Effect::CloseSelf]
    );
}

#[test]
fn retire_pipe_closes_a_newer_instance_with_the_wrong_identity() {
    let mut engine = Engine::new(100, config());

    assert_eq!(
        engine.on_retire_pipe(Some(
            r#"{"plugin_id":1,"loaded_at_ms":10,"build":"new-build","config":"new-config"}"#,
        )),
        vec![Effect::CloseSelf]
    );
}

#[test]
fn repeated_command_failures_fall_back_to_path_once_and_success_resets_the_streak() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 1, &host);

    assert!(
        engine
            .on_run_command_result(None, false, 10, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(Some(0), false, 20, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(Some(1), false, 30, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(None, false, 40, &host)
            .is_empty()
    );

    let effects = engine.on_run_command_result(Some(1), false, 50, &host);
    assert_eq!(reasons(&effects), vec!["alive"]);
    assert_eq!(run_commands(&effects).len(), 1);
    assert_eq!(
        run_commands(&effects)[0].first().map(String::as_str),
        Some("rimz")
    );

    assert!(
        engine
            .on_run_command_result(None, false, 60, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(None, false, 70, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(None, false, 80, &host)
            .is_empty(),
        "PATH mode does not keep firing fallback pokes",
    );
}

#[test]
fn repeated_stale_writer_rejections_retire_the_losing_plugin() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 1, &host);

    assert!(
        engine
            .on_run_command_result(Some(wire::STALE_WRITER_EXIT_CODE), true, 10, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(Some(0), false, 15, &host)
            .is_empty(),
        "an unrelated successful command does not reset publish rejections",
    );
    assert!(
        engine
            .on_run_command_result(Some(wire::STALE_WRITER_EXIT_CODE), true, 20, &host)
            .is_empty()
    );
    assert!(
        engine
            .on_run_command_result(Some(2), true, 25, &host)
            .is_empty(),
        "a failed topology fork preserves the stale-writer rejection streak",
    );
    assert_eq!(
        engine.on_run_command_result(Some(wire::STALE_WRITER_EXIT_CODE), true, 30, &host),
        vec![Effect::Unsubscribe]
    );
    assert!(engine.on_timer(40, &host).is_empty());
    let effects = engine.on_dump_topology_pipe(50, &host);
    assert_eq!(effects.first(), Some(&Effect::Resubscribe));
    assert_eq!(reasons(&effects), vec!["alive"]);

    let mut reset = Engine::new(0, config());
    grant(&mut reset, 1, &host);
    assert!(
        reset
            .on_run_command_result(Some(wire::STALE_WRITER_EXIT_CODE), true, 10, &host)
            .is_empty()
    );
    assert!(
        reset
            .on_run_command_result(Some(0), true, 20, &host)
            .is_empty()
    );
    assert!(
        reset
            .on_run_command_result(Some(wire::STALE_WRITER_EXIT_CODE), true, 30, &host)
            .is_empty()
    );
}

#[test]
fn retire_pipe_ignores_equal_newer_and_invalid_generations() {
    let mut engine = Engine::new(10, config());

    assert!(
        engine
            .on_retire_pipe(Some(r#"{"plugin_id":9,"loaded_at_ms":10}"#))
            .is_empty()
    );
    assert!(
        engine
            .on_retire_pipe(Some(r#"{"plugin_id":8,"loaded_at_ms":10}"#))
            .is_empty()
    );
    assert!(engine.on_retire_pipe(Some("garbage")).is_empty());
    assert!(engine.on_retire_pipe(None).is_empty());
}

#[test]
fn share_pipe_runs_immediately_and_replays_on_explicit_grant() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());

    assert_eq!(engine.on_share_session_pipe(), vec![Effect::ShareSession]);
    let effects = engine.on_permission_granted(10, &host);

    assert!(effects.contains(&Effect::ShareSession));
}

#[test]
fn timers_arm_once_supersede_earlier_deadlines_and_dispatch_due_pokes() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());

    assert_eq!(
        engine.on_load(0, &host),
        vec![Effect::SetTimeout(KEEPALIVE_MS)]
    );
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim"]),
        true,
        100,
        &host,
    );
    assert!(has_timeout(&effects, SETTLE_POKE_MS));

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim", "README.md"]),
        true,
        150,
        &host,
    );
    assert!(has_timeout(&effects, POKE_FLOOR_MS - 50));

    let effects = engine.on_timer(100 + POKE_FLOOR_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

#[test]
fn topology_after_foreground_change_carries_overlaid_command() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let mut implicit = pane(1);
    implicit.terminal_command = None;
    seed_manifest(&mut engine, tabs(vec![implicit]), 20, &host);

    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["vim", "README.md"]),
        true,
        30,
        &host,
    );
    let argv = run_commands(&effects)
        .into_iter()
        .find(|argv| arg_after(argv, "--reason") == Some("panes-changed"))
        .expect("changed snapshot wake");
    let topology = topology_json(argv);

    assert_eq!(topology["panes"][0]["pane_command"], "vim README.md");
}
