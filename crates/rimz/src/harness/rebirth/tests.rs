use super::*;
use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::config::{Profile, RoleBinding, Team};
use crate::ids::{MuxName, PaneId};

struct Fixture {
    _dir: tempfile::TempDir,
    paths: StatePaths,
    runtime: RuntimePaths,
    project: PathBuf,
}

impl Fixture {
    fn new(agents: &[(&str, &Path, bool)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        let workspace = WorkspaceId::from_project_root(&project);
        let paths = StatePaths::under(workspace.clone(), &dir.path().join("state")).expect("paths");
        let runtime =
            RuntimePaths::under(workspace.clone(), &dir.path().join("runtime")).expect("runtime");
        let store = Store::open(paths.clone(), runtime.clone()).expect("store");
        write_boot_marker(&paths.boot_marker, "boot-a");
        let mut roster = BTreeSet::new();
        for (id, worktree, create_worktree) in agents {
            if *create_worktree {
                std::fs::create_dir_all(worktree).expect("worktree");
            }
            let mut observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from(*id)),
                LifecycleSignal::Registered,
            );
            observation.agent_name = Some((*id).to_owned());
            observation.worktree_path = Some(worktree.display().to_string());
            observation.worktree_branch = Some("feature".to_owned());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, format!("%{id}")));
            store
                .append_event(&crate::EventEnvelope::agent_lifecycle(
                    workspace.clone(),
                    "rimz-test",
                    "claude",
                    "SessionStart",
                    &observation,
                ))
                .expect("agent event");
            roster.insert((
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from(*id),
            ));
        }
        crate::store::live_roster::publish(&paths.live_roster, roster).expect("roster");
        Self {
            _dir: dir,
            paths,
            runtime,
            project,
        }
    }

    fn inspect(&self, disabled: bool) -> RebirthPlan {
        self.inspect_with(&MachineConfig::default(), disabled)
    }

    fn inspect_with(&self, machine: &MachineConfig, disabled: bool) -> RebirthPlan {
        inspect_at(
            self.paths.clone(),
            self.runtime.clone(),
            Some("boot-a".to_owned()),
            Vec::new(),
            &self.project,
            machine,
            disabled,
        )
        .expect("inspect")
    }

    fn stamp_team(&self, id: &str, worktree: &Path, team: &str, role: &str, profile: &str) {
        let store = Store::open(self.paths.clone(), self.runtime.clone()).expect("store");
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(id)),
            LifecycleSignal::Registered,
        );
        observation.agent_name = Some(id.to_owned());
        observation.launch.team = Some(team.to_owned());
        observation.launch.role = Some(role.to_owned());
        observation.launch.profile = Some(profile.to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
        observation.worktree_branch = Some("feature".to_owned());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, format!("%{id}")));
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                self.paths.workspace_id.clone(),
                "rimz-test",
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("team agent event");
    }

    fn seed_named_channel(&self, name: &str) {
        let workspace = crate::workspace::WorkspaceResolver::resolve(&self.project, None)
            .expect("resolve workspace");
        crate::store::workspace_record::write(
            &self.paths,
            &crate::store::workspace_record::WorkspaceRecord::from_resolved(&workspace),
        )
        .expect("workspace record");
        crate::channel::register(&self.paths, name).expect("channel record");
    }
}

#[test]
fn inspection_is_read_only_and_scopes_to_live_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let mut roster = crate::store::live_roster::read(&fixture.paths.live_roster)
        .expect("read roster")
        .agents;
    roster.insert((
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("not-in-audit"),
    ));
    crate::store::live_roster::publish(&fixture.paths.live_roster, roster)
        .expect("publish expanded roster");
    let boot_before = std::fs::read(&fixture.paths.boot_marker).expect("boot marker");
    let roster_before = std::fs::read(&fixture.paths.live_roster).expect("roster");
    let events_before = std::fs::read(&fixture.paths.events_log).expect("events");

    let plan = fixture.inspect(false);

    assert_eq!(plan.preview().death().unwrap().lost_agents.len(), 1);
    assert_eq!(
        plan.preview().death().unwrap().lost_agents[0]
            .agent_id
            .as_str(),
        "live"
    );
    assert_eq!(
        std::fs::read(&fixture.paths.boot_marker).unwrap(),
        boot_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.live_roster).unwrap(),
        roster_before
    );
    assert_eq!(
        std::fs::read(&fixture.paths.events_log).unwrap(),
        events_before
    );
    assert!(!fixture.paths.last_death_marker.exists());
    assert!(!fixture.paths.crashes_dir.exists());
}

#[test]
fn recover_orders_death_ended_stamp_and_rebirth_then_consumes_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let missing = dir.path().join("missing");
    let fixture = Fixture::new(&[("live", &live, true), ("missing", &missing, false)]);
    let plan = fixture.inspect(false);

    let outcome = plan.materialize(RebirthChoice::Recover, "rimz-test");

    assert_eq!(
        outcome
            .resume
            .tabs
            .iter()
            .map(ResumeTab::pane_count)
            .sum::<usize>(),
        1
    );
    assert!(!fixture.paths.live_roster.exists());
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    let death = events.find("session.death").expect("death");
    let ended = events.find("rimz.worktree-gone").expect("ended stamp");
    let rebirth = events.find("session.rebirth").expect("rebirth");
    assert!(death < ended && ended < rebirth, "{events}");
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(marker.recovered, Some(1));
}

#[test]
fn fresh_archives_crash_and_records_zero_recovered_without_tabs() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(false);

    let outcome = plan.materialize(RebirthChoice::Fresh, "rimz-test");

    assert!(outcome.resume.tabs.is_empty());
    assert!(!fixture.paths.live_roster.exists());
    let marker: LastDeathMarker =
        serde_json::from_slice(&std::fs::read(&fixture.paths.last_death_marker).unwrap())
            .expect("marker");
    assert_eq!(marker.recovered, Some(0));
    assert_eq!(
        std::fs::read_dir(&fixture.paths.crashes_dir)
            .expect("crashes")
            .count(),
        1
    );
    let archive = std::fs::read_dir(&fixture.paths.crashes_dir)
        .expect("crashes")
        .next()
        .expect("archive")
        .expect("archive entry")
        .path();
    let roster: Vec<AgentState> = serde_json::from_slice(
        &std::fs::read(archive.join("roster.json")).expect("roster archive"),
    )
    .expect("archived roster json");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].agent_id.as_str(), "live");
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    assert!(events.find("session.death").unwrap() < events.find("session.rebirth").unwrap());
    assert!(!events.contains("rimz.worktree-gone"));
}

#[test]
fn failed_rebirth_append_still_consumes_roster() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let plan = fixture.inspect(false);
    std::fs::remove_file(&fixture.paths.events_log).expect("remove log");
    std::fs::create_dir(&fixture.paths.events_log).expect("block log append");

    plan.materialize(RebirthChoice::Fresh, "rimz-test");

    assert!(!fixture.paths.live_roster.exists());
}

#[test]
fn disabled_recovery_restores_empty_channels_without_seeding_agents() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    fixture.seed_named_channel("auth");
    let plan = fixture.inspect(true);

    assert_eq!(plan.preview().pane_count(), 0);
    let outcome = plan.materialize(RebirthChoice::Recover, "rimz-test");
    assert_eq!(outcome.resume.tabs.len(), 1);
    assert_eq!(outcome.resume.tabs[0].label, "#auth");
    assert_eq!(outcome.resume.tabs[0].pane_count(), 0);
}

#[test]
fn team_recovery_allocates_fresh_role_and_keeps_other_tabs_after_team_failure() {
    let dir = tempfile::tempdir().expect("worktrees");
    let worktree = dir.path().join("forge");
    let fixture = Fixture::new(&[("planner", &worktree, true)]);
    fixture.stamp_team("planner", &worktree, "forge", "planner", "claude-plan");
    let machine = team_machine();
    let mut plan = fixture.inspect_with(&machine, false);
    assert_eq!(plan.planned.team.len(), 1);
    assert_eq!(plan.preview().pane_count(), 2);

    let mut broken = plan.planned.team[0].clone();
    broken.label = "#broken".to_owned();
    broken.team = "broken".to_owned();
    broken.cohort.seeds.clear();
    plan.planned.team.insert(0, broken);
    plan.planned.flat.tabs.push(ResumeTab::flat(
        "flat".to_owned(),
        fixture.project.clone(),
        vec![vec!["true".to_owned()]],
    ));

    let outcome = plan.materialize(RebirthChoice::Recover, "rimz-test");

    assert!(outcome.resume.tabs.iter().any(|tab| tab.label == "#forge"));
    assert!(outcome.resume.tabs.iter().any(|tab| tab.label == "flat"));
    assert!(!outcome.resume.tabs.iter().any(|tab| tab.label == "#broken"));
    let team = outcome
        .resume
        .tabs
        .iter()
        .find(|tab| tab.label == "#forge")
        .expect("team tab");
    assert_eq!(team.pane_count(), 2);
    let argvs = team
        .layout
        .columns
        .iter()
        .flat_map(|column| column.panes.iter().map(|pane| &pane.argv))
        .collect::<Vec<_>>();
    let decode_request = |argv: &[String]| {
        let payload = argv
            .windows(2)
            .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
            .expect("exec request payload");
        crate::harness::launch::decode_exec_request(&argv[3], None, payload)
            .expect("decode exec request")
    };
    let planner = decode_request(argvs[0]);
    assert_eq!(planner.identity.name.as_deref(), Some("planner"));
    let coder = decode_request(argvs[1]);
    assert_eq!(coder.identity.params.role.as_deref(), Some("coder"));
    let store = Store::open(fixture.paths.clone(), fixture.runtime.clone()).expect("store");
    let projection = store
        .runtime_projection(crate::RuntimeScope::Audit)
        .expect("projection");
    let coder = projection
        .agents
        .iter()
        .find(|agent| agent.role.as_deref() == Some("coder"))
        .expect("fresh coder identity");
    assert_eq!(coder.kind.as_str(), "codex");
    assert_eq!(coder.team.as_deref(), Some("forge"));
    assert!(coder.agent_id.as_str().starts_with("launch_"));
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    assert!(
        events.find("session.death").unwrap() < events.find("agent.launched").unwrap()
            && events.find("agent.launched").unwrap() < events.find("session.rebirth").unwrap(),
        "{events}"
    );
}

#[test]
fn supervised_boundary_consumes_roster_without_inspecting_or_seeding_agents() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);

    record_boundary_at(
        fixture.paths.clone(),
        fixture.runtime.clone(),
        &fixture.paths.workspace_id,
        "rimz-test",
    );

    assert!(!fixture.paths.live_roster.exists());
    assert!(!fixture.paths.last_death_marker.exists());
    assert!(!fixture.paths.crashes_dir.exists());
    let events =
        String::from_utf8_lossy(&std::fs::read(&fixture.paths.events_log).unwrap()).into_owned();
    assert!(events.contains("session.rebirth"), "{events}");
    assert!(!events.contains("session.death"), "{events}");
    assert!(!events.contains("agent.launched"), "{events}");
}

#[test]
fn crash_archive_retention_keeps_newest_five() {
    let dir = tempfile::tempdir().expect("tempdir");
    let crashes = dir.path().join("crashes");
    for index in 0..7 {
        std::fs::create_dir_all(crashes.join(format!("2026010{index}T000000Z")))
            .expect("archive dir");
    }

    prune_crash_archives(&crashes).expect("prune");

    let mut kept = std::fs::read_dir(&crashes)
        .expect("read")
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    kept.sort();
    assert_eq!(
        kept,
        vec![
            "20260102T000000Z",
            "20260103T000000Z",
            "20260104T000000Z",
            "20260105T000000Z",
            "20260106T000000Z",
        ]
    );
}

#[cfg(unix)]
#[test]
fn crash_copy_preserves_relative_cache_paths_and_skips_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let source = cache.join("zellij/session");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::write(source.join("state.kdl"), "state").expect("state");
    symlink(source.join("state.kdl"), source.join("link.kdl")).expect("symlink");
    let mux_cache = dir.path().join("archive/mux-cache");
    let snapshot = capture_cache_sources(&cache, std::slice::from_ref(&source));

    write_cache_snapshot(&snapshot, &mux_cache).expect("write snapshot");

    let destination = mux_cache.join("zellij/session");
    assert_eq!(destination, mux_cache.join("zellij/session"));
    assert!(destination.join("state.kdl").is_file());
    assert!(!destination.join("link.kdl").exists());
}

#[test]
fn crash_archive_uses_cache_bytes_captured_before_room_birth() {
    let dir = tempfile::tempdir().expect("worktrees");
    let live = dir.path().join("live");
    let fixture = Fixture::new(&[("live", &live, true)]);
    let source = dir.path().join("session-info");
    std::fs::create_dir_all(&source).expect("cache source");
    std::fs::write(source.join("state.kdl"), "crashed").expect("crashed cache");
    let plan = inspect_at(
        fixture.paths.clone(),
        fixture.runtime.clone(),
        Some("boot-a".to_owned()),
        vec![source.clone()],
        &fixture.project,
        &MachineConfig::default(),
        false,
    )
    .expect("inspect");

    std::fs::write(source.join("state.kdl"), "reborn").expect("reborn cache");
    plan.materialize(RebirthChoice::Fresh, "rimz-test");

    let archive = std::fs::read_dir(&fixture.paths.crashes_dir)
        .expect("crashes")
        .next()
        .expect("archive")
        .expect("archive entry")
        .path();
    assert_eq!(
        std::fs::read_to_string(archive.join("mux-cache/session-info/state.kdl"))
            .expect("archived cache"),
        "crashed"
    );
}

#[test]
fn boot_helpers_parse_stable_tokens() {
    assert!(boot_changed(None, Some("boot-a")));
    assert!(!boot_changed(Some("boot-a"), Some("boot-a")));
    assert!(boot_changed(Some("boot-a"), Some("boot-b")));
    assert_eq!(
        parse_proc_btime("cpu 1 2\nbtime 1780040667\n"),
        Some("1780040667".to_owned())
    );
    assert_eq!(
        parse_kern_boottime("{ sec = 1780040667, usec = 0 }"),
        Some("1780040667".to_owned())
    );
}

fn team_machine() -> MachineConfig {
    let mut machine = MachineConfig::default();
    machine.agents.profiles.0.insert(
        "claude-plan".to_owned(),
        Profile {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        },
    );
    machine.agents.profiles.0.insert(
        "codex-code".to_owned(),
        Profile {
            agent: "codex".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        },
    );
    machine.agents.teams.0.insert(
        "forge".to_owned(),
        Team {
            roles: vec![
                RoleBinding {
                    role: "planner".to_owned(),
                    profile: "claude-plan".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_file: None,
                    args: None,
                },
                RoleBinding {
                    role: "coder".to_owned(),
                    profile: "codex-code".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_file: None,
                    args: None,
                },
            ],
            leader: None,
            layout: Some("planner,coder".to_owned()),
        },
    );
    machine
}
