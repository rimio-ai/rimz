use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;
use crate::policy::{self, KEEPALIVE_MS, POKE_FLOOR_MS, SETTLE_POKE_MS, SIDEBAR_PANE_TITLE};

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
        is_focused: false,
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

fn sidebar_pane(id: u32) -> PaneFields {
    PaneFields {
        title: SIDEBAR_PANE_TITLE.to_owned(),
        ..pane(id)
    }
}

fn plugin_pane(id: u32) -> PaneFields {
    PaneFields {
        is_plugin: true,
        ..pane(id)
    }
}

fn focused(mut pane: PaneFields) -> PaneFields {
    pane.is_focused = true;
    pane
}

fn tabs(panes: Vec<PaneFields>) -> BTreeMap<usize, Vec<PaneFields>> {
    BTreeMap::from([(0, panes)])
}

fn tabs_by_index(entries: Vec<(usize, Vec<PaneFields>)>) -> BTreeMap<usize, Vec<PaneFields>> {
    entries.into_iter().collect()
}

fn raw_hash(tabs: &BTreeMap<usize, Vec<PaneFields>>) -> u64 {
    policy::manifest_hash(tabs)
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

fn args_after<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    argv.windows(2)
        .filter(|window| window[0] == flag)
        .map(|window| window[1].as_str())
        .collect()
}

fn has_timeout(effects: &[Effect], delay_ms: u64) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, Effect::SetTimeout(actual) if *actual == delay_ms))
}

#[test]
fn pregrant_change_holds_until_grant_and_grant_without_pending_emits_alive() {
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
    assert!(
        !reasons(&effects).contains(&"pane-opened"),
        "first manifest is learned baseline, not pane opens",
    );
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
fn manifest_adding_two_card_panes_emits_two_opens_without_changed() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1)]), 20, &host);
    let manifest = tabs(vec![pane(1), pane(2), pane(3)]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 30, &host);

    assert_eq!(reasons(&effects), vec!["pane-opened", "pane-opened"]);
}

#[test]
fn focus_only_manifest_emits_patch_and_settle_changed() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), pane(2)]),
        20,
        &host,
    );
    let manifest = tabs(vec![pane(1), focused(pane(2))]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 30, &host);

    assert_eq!(reasons(&effects), vec!["focus-changed"]);
    assert!(has_timeout(&effects, SETTLE_POKE_MS));

    let effects = engine.on_timer(30 + SETTLE_POKE_MS, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

#[test]
fn manifest_focus_reconciliation_repairs_stale_register_and_dual_focus() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names, 20, &host);
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), focused(pane(2))]),
        30,
        &host,
    );
    engine.session_focused_pane = Some(1);
    let manifest = tabs(vec![pane(1), focused(pane(2))]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 40, &host);

    let focus_wake = run_commands(&effects)
        .into_iter()
        .find(|argv| arg_after(argv, "--reason") == Some("focus-changed"))
        .expect("focus changed wake");
    assert_eq!(
        args_after(focus_wake, "--focused-pane-id"),
        vec!["terminal_2"]
    );
    assert_eq!(
        args_after(focus_wake, "--unfocused-pane-id"),
        vec!["terminal_1"]
    );
    let topology = topology_json(focus_wake);
    assert_eq!(topology["focused_pane"], 2);
    let panes = topology["panes"].as_array().expect("topology panes");
    assert_eq!(
        panes
            .iter()
            .filter(|pane| pane["is_focused"].as_bool() == Some(true))
            .map(|pane| pane["id"].as_u64().expect("pane id"))
            .collect::<Vec<_>>(),
        vec![2],
    );
    assert_eq!(engine.session_focused_pane, Some(2));
}

#[test]
fn focus_correction_with_no_unfocused_pane_clears_tab_siblings() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned()), (1, "tab-1".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, &host);
    seed_manifest(
        &mut engine,
        tabs_by_index(vec![
            (0, vec![pane_in_tab(1, 0)]),
            (
                1,
                vec![focused(pane_in_tab(11, 1)), focused(pane_in_tab(12, 1))],
            ),
        ]),
        30,
        &host,
    );

    let _ = engine.on_tab_update(Some(1), names, 100, &host);
    let effects = engine.on_timer(100 + policy::FOCUS_SETTLE_MS, &host);

    let focus_wake = run_commands(&effects)
        .into_iter()
        .find(|argv| arg_after(argv, "--reason") == Some("focus-changed"))
        .expect("focus changed wake");
    assert_eq!(
        args_after(focus_wake, "--focused-pane-id"),
        vec!["terminal_11"]
    );
    assert!(args_after(focus_wake, "--unfocused-pane-id").is_empty());
    assert_eq!(
        engine.tabs[&1]
            .iter()
            .filter(|pane| pane.is_focused)
            .map(|pane| pane.id)
            .collect::<Vec<_>>(),
        vec![11],
    );
}

#[test]
fn floating_focus_manifest_does_not_move_session_register() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names, 20, &host);
    let mut floating = pane(2);
    floating.is_floating = true;
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), floating.clone()]),
        30,
        &host,
    );
    engine.session_focused_pane = Some(1);
    floating.is_focused = true;
    let manifest = tabs(vec![pane(1), floating]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 40, &host);

    assert!(!reasons(&effects).contains(&"focus-changed"));
    assert_eq!(engine.session_focused_pane, Some(1));
}

#[test]
fn command_changed_floor_is_per_pane_and_pane_close_clears_it() {
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
    assert_eq!(reasons(&effects), vec!["command-changed"]);

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
    assert_eq!(reasons(&effects), vec!["command-changed"]);

    let _ = engine.on_pane_closed(ProjectedPaneId::Terminal(1), 220, &host);
    let effects = engine.on_command_changed(
        ProjectedPaneId::Terminal(1),
        strings(&["python"]),
        true,
        230,
        &host,
    );
    assert_eq!(reasons(&effects), vec!["command-changed"]);
}

#[test]
fn pane_closed_terminal_has_identity_and_plugin_falls_back_to_changed() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), plugin_pane(9)]), 20, &host);

    let effects = engine.on_pane_closed(ProjectedPaneId::Terminal(1), 30, &host);
    assert_eq!(reasons(&effects), vec!["pane-closed"]);

    let effects = engine.on_pane_closed(ProjectedPaneId::Plugin(9), 40, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

#[test]
fn tab_switch_focus_correction_reports_stranded_sidebar_and_work_focus() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned()), (1, "tab-1".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, &host);
    seed_manifest(
        &mut engine,
        tabs_by_index(vec![
            (0, vec![focused(pane_in_tab(1, 0))]),
            (1, vec![focused(sidebar_pane(10)), pane_in_tab(11, 1)]),
        ]),
        30,
        &host,
    );

    let _ = engine.on_tab_update(Some(1), names.clone(), 100, &host);
    let effects = engine.on_timer(100 + policy::FOCUS_SETTLE_MS, &host);
    assert_eq!(reasons(&effects), vec!["focus-stranded"]);

    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, &host);
    seed_manifest(
        &mut engine,
        tabs_by_index(vec![
            (0, vec![focused(pane_in_tab(1, 0))]),
            (1, vec![focused(sidebar_pane(10)), pane_in_tab(11, 1)]),
        ]),
        30,
        &host,
    );
    let _ = engine.on_tab_update(Some(1), names, 100, &host);
    let manifest = tabs_by_index(vec![
        (0, vec![pane_in_tab(1, 0)]),
        (1, vec![sidebar_pane(10), focused(pane_in_tab(11, 1))]),
    ]);
    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest.clone(), 120, &host);
    let focus_wakes = run_commands(&effects)
        .into_iter()
        .filter(|argv| arg_after(argv, "--reason") == Some("focus-changed"))
        .collect::<Vec<_>>();
    assert_eq!(focus_wakes.len(), 1, "tab-switch correction owns the wake");
    assert!(focus_wakes.iter().any(|argv| {
        arg_after(argv, "--reason") == Some("focus-changed")
            && argv
                .windows(2)
                .any(|window| window[0] == "--focused-pane-id" && window[1] == "terminal_11")
            && argv
                .windows(2)
                .any(|window| window[0] == "--unfocused-pane-id" && window[1] == "terminal_1")
    }));
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
    assert_eq!(reasons(&effects), vec!["pane-opened"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
    assert!(topology["panes"][1].get("pane_pid").is_none());

    let changed = tabs(vec![focused(pane(1)), pane(2)]);
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
    engine.tabs = tabs(vec![pane(1)]);

    let effects = engine.on_dump_topology_pipe(20, &host);

    assert_eq!(reasons(&effects), vec!["alive"]);
    assert_eq!(*host.pid_calls.borrow(), vec![1]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["panes"][0]["pane_pid"], 101);
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
fn list_clients_change_emits_changed_wake_and_unchanged_reply_is_quiet() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    seed_manifest(&mut engine, tabs(vec![pane(1), pane(2)]), 20, &host);

    let sample = vec![
        ProjectedClientFocus {
            client_id: 2,
            pane_id: 2,
        },
        ProjectedClientFocus {
            client_id: 1,
            pane_id: 1,
        },
        ProjectedClientFocus {
            client_id: 1,
            pane_id: 1,
        },
    ];
    let effects = engine.on_list_clients(sample.clone(), 30, &host);
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["clients"]["human_clients"], 2);
    assert_eq!(
        topology["clients"]["viewed_panes"],
        serde_json::json!([1, 2])
    );

    let effects = engine.on_list_clients(sample, 40, &host);
    assert!(
        run_commands(&effects).is_empty(),
        "unchanged client sample should not wake"
    );

    let effects = engine.on_list_clients(
        vec![ProjectedClientFocus {
            client_id: 1,
            pane_id: 1,
        }],
        50,
        &host,
    );
    assert_eq!(reasons(&effects), vec!["panes-changed"]);
}

#[test]
fn manifest_focus_repair_prefers_client_view_and_updates_published_focus() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let _ = engine.on_tab_update(
        Some(0),
        BTreeMap::from([(0, "tab-0".to_owned())]),
        20,
        &host,
    );
    let _ = engine.on_list_clients(
        vec![ProjectedClientFocus {
            client_id: 1,
            pane_id: 2,
        }],
        30,
        &host,
    );
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), focused(pane(2))]),
        40,
        &host,
    );

    let effects = engine.on_dump_topology_pipe(50, &host);

    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["focused_pane"], 2);
    assert_eq!(
        topology["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .filter(|pane| pane["is_focused"].as_bool() == Some(true))
            .map(|pane| pane["id"].as_u64().expect("pane id"))
            .collect::<Vec<_>>(),
        vec![2],
    );
}

#[test]
fn late_client_sample_reselects_a_recorded_contested_focus() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let _ = engine.on_tab_update(
        Some(0),
        BTreeMap::from([(0, "tab-0".to_owned())]),
        20,
        &host,
    );
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), focused(pane(2))]),
        30,
        &host,
    );
    assert_eq!(engine.session_focused_pane, Some(1));

    let effects = engine.on_list_clients(
        vec![ProjectedClientFocus {
            client_id: 1,
            pane_id: 2,
        }],
        40,
        &host,
    );

    let topology = topology_json(run_commands(&effects)[0]);
    assert_eq!(topology["focused_pane"], 2);
    assert_eq!(
        topology["panes"]
            .as_array()
            .expect("panes")
            .iter()
            .filter(|pane| pane["is_focused"].as_bool() == Some(true))
            .map(|pane| pane["id"].as_u64().expect("pane id"))
            .collect::<Vec<_>>(),
        vec![2],
    );
}

#[test]
fn clean_focus_with_another_manifest_change_keeps_the_fast_overlay() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let _ = engine.on_tab_update(
        Some(0),
        BTreeMap::from([(0, "tab-0".to_owned())]),
        20,
        &host,
    );
    seed_manifest(
        &mut engine,
        tabs(vec![focused(pane(1)), pane(2)]),
        30,
        &host,
    );
    engine.session_focused_pane = Some(1);
    let manifest = tabs(vec![pane(1), focused(pane(2)), pane(3)]);

    let effects = engine.on_pane_manifest(raw_hash(&manifest), |_| manifest, 40, &host);

    assert!(reasons(&effects).contains(&"focus-changed"));
}

#[test]
fn session_update_and_keepalive_request_client_sample() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);

    let effects = engine.on_session_update(Some(1), 20, &host);
    assert!(effects.contains(&Effect::ListClients));
    let effects = engine.on_session_update(Some(1), 30, &host);
    assert!(!effects.contains(&Effect::ListClients));

    let effects = engine.on_timer(KEEPALIVE_MS, &host);
    assert!(effects.contains(&Effect::ListClients));
    assert_eq!(reasons(&effects), vec!["alive"]);
}

#[test]
fn active_tab_switch_requests_client_sample() {
    let host = FakeHost::default();
    let mut engine = Engine::new(0, config());
    grant(&mut engine, 10, &host);
    let names = BTreeMap::from([(0, "tab-0".to_owned()), (1, "tab-1".to_owned())]);
    let _ = engine.on_tab_update(Some(0), names.clone(), 20, &host);
    seed_manifest(
        &mut engine,
        tabs_by_index(vec![
            (0, vec![pane_in_tab(1, 0)]),
            (1, vec![sidebar_pane(10), pane_in_tab(11, 1)]),
        ]),
        30,
        &host,
    );

    let effects = engine.on_tab_update(Some(1), names.clone(), 100, &host);
    assert!(effects.contains(&Effect::ListClients));

    let effects = engine.on_tab_update(Some(1), names, 110, &host);
    assert!(!effects.contains(&Effect::ListClients));
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
        .find(|argv| arg_after(argv, "--reason") == Some("command-changed"))
        .expect("command-changed wake");
    let topology = topology_json(argv);

    assert_eq!(topology["panes"][0]["pane_command"], "vim README.md");
}
