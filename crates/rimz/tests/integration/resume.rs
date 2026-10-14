//! Resume-on-rebirth, driven through the REAL fold.
//!
//! The bug these tests guard against: `plan_resume` reads the durable audit
//! rollup, but the rollup builds each agent's pane via `PaneRef::from_id`, which
//! leaves `session_name` empty. A prior filter compared that empty stamp against
//! the live session name and so dropped every candidate, on every rebirth, for
//! every workspace — the reborn room always came up bare.
//!
//! The load-bearing property here is that the `PaneRef` comes from the production
//! reducer (append real lifecycle events → `runtime_projection(Audit)`), not a
//! hand-built test value. The in-module unit tests in `src/harness/resume.rs` cannot
//! catch a rollup-shape mismatch because they fabricate the `AgentState`; these
//! do, because the fold produces it.

use rimz::EventEnvelope;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
use rimz::ids::AgentKind;
use rimz::ids::MuxName;
use rimz::ids::PaneId;
use std::path::Path;

use crate::common::Harness;

/// A `Registered` observation for a root agent that stamped a pane in a worktree
/// — the shape a `SessionStart` hook records. `name` is the durable card name a
/// launcher passes through; pinning it keeps the resume argv deterministic.
fn registered(
    agent_id: &str,
    name: &str,
    pane_raw: &str,
    worktree: &str,
    branch: &str,
) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        ask_queue: None,
        agent_id: Some(agent_id.into()),
        agent_name: Some(name.to_owned()),
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        account_key: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: Some(branch.to_owned()),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        compacted_from: None,
        usage: rimz::agents::AgentUsageSummary::default(),
        pane_id: Some(PaneId::from_parts(MuxName::Zellij, pane_raw)),
        pane_stamp: None,
        parent_agent_id: None,
        background_shells: None,
    }
}

/// Plan a resume from the workspace's real audit rollup, the way `plan_room_resume`
/// does, so the `AgentState` under test is whatever the fold actually produces.
fn plan_from_rollup(h: &Harness) -> rimz::harness::resume::ResumePlan {
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    rimz::harness::resume::plan_resume(
        &projection.agents,
        &projection.ended,
        rimz::harness::resume::ResumeContext {
            in_place: false,
            project_root: None,
            workspace_id: &h.workspace_id,
            rimz_bin: Path::new("/bin/rimz"),
            runtime: h.store.runtime_paths(),
            profiles: &rimz::config::ProfilesConfig::default(),
            max: rimz::config::ResumeConfig::default().max,
            logins: &rimz::agents::RoomAccounts::default(),
            catalog: &rimz::agents::LoginCatalog::default(),
        },
        |_| true,
        |_| true,
    )
}

fn resume_argv(h: &Harness, kind: &str, id: &str, name: &str) -> Vec<String> {
    rimz::harness::launch::exec_argv(
        Path::new("/bin/rimz"),
        h.store.runtime_paths(),
        &ExecRequest {
            isolation_default: None,
            kind: AgentKind::new_unchecked(kind),
            action: ExecAction::Resume {
                session_id: id.to_owned(),
                extra_args: Vec::new(),
            },
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            skills: None,
            allowed_tools: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            subagent: false,
            loop_reminder: None,
            headless: None,
            identity: ExecIdentity {
                name: Some(name.to_owned()),
                launch_id: Some(id.to_owned()),
                ..ExecIdentity::default()
            },
        },
    )
    .expect("resume argv")
}

fn decode_exec_request(argv: &[String]) -> ExecRequest {
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("exec request payload");
    rimz::harness::launch::decode_exec_request(&argv[3], None, payload)
        .expect("decode exec request")
}

fn single_column(tab: &rimz::mux::ResumeTab) -> Vec<Vec<String>> {
    tab.layout
        .columns
        .first()
        .expect("resume tab has one column")
        .panes
        .iter()
        .map(|pane| pane.argv.clone())
        .collect()
}

fn lifecycle(
    h: &Harness,
    kind: &str,
    event: &str,
    obs: &AgentLifecycleObservation,
) -> EventEnvelope {
    EventEnvelope::agent_lifecycle(h.workspace_id.clone(), "rimz-test", kind, event, obs)
}

#[cfg(unix)]
mod agent_ref {
    use super::*;
    use crate::common::room::seed_live_zellij_room;
    use crate::common::{
        CommandTimeoutExt, Env, path_with_front, write_definition, write_env_dump_shim,
        write_fake_login_shell, zellij_trace_shim,
    };
    use rimz::agents::PermissionMode;
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState};
    use std::path::PathBuf;
    use std::process::{Command, Output};

    const SESSION: &str = "saved-session";
    const NAME: &str = "bright-otter";
    const PANES: &str = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;

    struct Fixture {
        env: Env,
        cwd: PathBuf,
        transcript: PathBuf,
        trace: PathBuf,
        shim: PathBuf,
        shell: PathBuf,
    }

    impl Fixture {
        fn new(launch: LaunchParams, live: bool, parent: Option<&str>) -> Self {
            let env = Env::new();
            env.install_agent_hooks("claude");
            std::fs::write(
                env.rimz_home().join("config.toml"),
                "[agents]\nisolation = 'host'\n",
            )
            .unwrap();
            write_definition(
                &env,
                "agents",
                "worker",
                "description: Worker\nagent: claude\nmode: ask\nmodel: sonnet\ntools: []",
                "Work.",
            );
            let shim = write_env_dump_shim(&env, "claude");
            let shell = write_fake_login_shell(&env, "resume-shell", &[]);
            let cwd = env.project_root.join("checkout");
            std::fs::create_dir_all(&cwd).unwrap();
            let transcript = cwd.join("saved.jsonl");
            std::fs::write(&transcript, "{}\n").unwrap();
            let trace = env.home_root.join("resume-mux.log");
            let workspace = env.resolve_workspace(&env.project_root);
            let store = env.store();
            store.record_workspace(&workspace).unwrap();
            let owner = live.then(|| {
                rimz::store::runtime::current_process_owner(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    SESSION,
                )
            });
            store
                .append_event(&EventEnvelope::agent_launched(
                    workspace.workspace_id.clone(),
                    &workspace.session_name,
                    &AgentKind::new_unchecked("claude"),
                    AgentLaunchPayload {
                        system_prompt_digest: None,
                        agent_id: SESSION.into(),
                        launch_id: Some("saved-launch".into()),
                        agent_name: NAME.into(),
                        agent_name_explicit: true,
                        launch: LaunchParams {
                            profile: Some("worker".into()),
                            channel: Some("saved".into()),
                            mode: Some(PermissionMode::Auto),
                            isolation: Some(rimz::config::Isolation::Host),
                            record: Some(rimz::agents::LaunchRecord::replay_or_new(
                                None,
                                Some("sonnet"),
                                None,
                                false,
                            )),
                            parent_agent_id: parent.map(Into::into),
                            ..launch
                        },
                        state: AgentLaunchState::Bound,
                        run_id: None,
                        pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_2")),
                        runtime_owner: owner.clone(),
                        worktree_path: Some(cwd.display().to_string()),
                        worktree_branch: None,
                        prompt: None,
                        description: None,
                    },
                ))
                .unwrap();
            let mut observation = AgentLifecycleObservation::new(
                Some(SESSION.into()),
                if live {
                    LifecycleSignal::Registered
                } else {
                    LifecycleSignal::Ended
                },
            );
            observation.runtime_owner = owner;
            observation.transcript_path = Some(transcript.display().to_string());
            store
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id.clone(),
                    &workspace.session_name,
                    "claude",
                    "SessionEnd",
                    &observation,
                ))
                .unwrap();
            seed_live_zellij_room(
                &env.runtime_paths(),
                &workspace.session_name,
                serde_json::from_str(PANES).unwrap(),
            );
            std::fs::write(env.home_root.join("presence.wasm"), b"test-presence").unwrap();
            Self {
                env,
                cwd,
                transcript,
                trace,
                shim,
                shell,
            }
        }

        fn command(&self, reference: &str) -> Command {
            self.command_args(&["resume", reference])
        }

        fn command_args(&self, args: &[&str]) -> Command {
            let workspace = self.env.resolve_workspace(&self.env.project_root);
            let mut command = self.env.rimz();
            command
                .args(["--mux", "zellij", "agents"])
                .args(args)
                .envs(rimz::workspace::pin_env(
                    &workspace.workspace_id,
                    &self.env.project_root,
                ))
                .env("SHELL", &self.shell)
                .env("PATH", path_with_front(&self.shim))
                .env(
                    "RIMZ_TEST_AGENT_ENV_DUMP",
                    self.env.home_root.join("agent.env"),
                )
                .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
                .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace)
                .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
                .env(
                    "RIMZ_PRESENCE_PLUGIN",
                    self.env.home_root.join("presence.wasm"),
                )
                .env(
                    "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                    format!("{} [Created 1s ago]\n", workspace.session_name),
                )
                .env("RIMZ_TEST_ZELLIJ_LIST_PANES", PANES);
            command.env("ZELLIJ_SESSION_NAME", workspace.session_name);
            command
        }

        fn output(&self, reference: &str) -> Output {
            self.command(reference).bounded_output().unwrap()
        }

        fn trace(&self) -> String {
            std::fs::read_to_string(&self.trace).unwrap_or_default()
        }
    }

    fn resumed_pane(document: &kdl::KdlDocument) -> Option<(&kdl::KdlNode, Vec<String>)> {
        for node in document.nodes() {
            if node.name().value() == "pane"
                && let Some(args) = node.children().and_then(|children| children.get("args"))
            {
                let argv: Vec<String> = args
                    .entries()
                    .iter()
                    .map(|entry| entry.value().as_string().unwrap().to_owned())
                    .collect();
                if argv.iter().any(|arg| arg == "--request") {
                    return Some((node, argv));
                }
            }
            if let Some(pane) = node.children().and_then(resumed_pane) {
                return Some(pane);
            }
        }
        None
    }

    #[test]
    fn ended_solo_name_and_session_prefix_restore_the_recorded_session() {
        for reference in ["@bright-otter", "@saved-sess"] {
            let fixture = Fixture::new(LaunchParams::default(), false, None);
            let store = fixture.env.store();
            let mut newer = store
                .read_events()
                .unwrap()
                .into_iter()
                .find_map(|event| match event.kind() {
                    rimz::store::event::EventKind::AgentLaunch(payload) => Some(payload),
                    _ => None,
                })
                .unwrap();
            newer.agent_id = "newer-session".into();
            newer.launch_id = Some("newer-launch".into());
            newer.agent_name = "newer-fox".into();
            newer.runtime_owner = Some(rimz::store::runtime::current_process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                "newer-session",
            ));
            let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
            store
                .append_event(&EventEnvelope::agent_launched(
                    workspace.workspace_id,
                    &workspace.session_name,
                    &AgentKind::new_unchecked("claude"),
                    newer,
                ))
                .unwrap();
            let output = fixture.output(reference);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "resumed claude:bright-otter (saved-session)\n"
            );
            let trace = fixture.trace();
            assert_eq!(
                trace
                    .lines()
                    .filter(|line| line.contains("action\tnew-tab"))
                    .count(),
                1,
                "{trace}"
            );
            let layout: String = serde_json::from_str(
                trace
                    .lines()
                    .find_map(|line| line.strip_prefix("layout-json\t"))
                    .unwrap(),
            )
            .unwrap();
            let layout: kdl::KdlDocument = layout.parse().unwrap();
            let (pane, argv) = resumed_pane(&layout).unwrap();
            assert_eq!(
                pane.get("cwd").unwrap().value().as_string(),
                Some(fixture.cwd.to_str().unwrap())
            );
            let payload = argv.windows(2).find(|pair| pair[0] == "--request").unwrap();
            let request =
                rimz::harness::launch::decode_exec_request("claude", None, &payload[1]).unwrap();
            assert!(
                matches!(&request.action, ExecAction::Resume { session_id, .. } if session_id == SESSION)
            );
            assert_eq!(request.identity.name.as_deref(), Some(NAME));
            assert_eq!(request.identity.params.channel.as_deref(), Some("saved"));
            assert_eq!(request.identity.params.profile.as_deref(), Some("worker"));
            assert_eq!(request.identity.params.mode, Some(PermissionMode::Auto));
            assert_eq!(request.identity.params.model.as_deref(), Some("sonnet"));
            assert!(request.close_pane_on_exit);
        }
    }

    #[test]
    fn exact_provider_id_keeps_the_recorded_handle() {
        let fixture = Fixture::new(LaunchParams::default(), false, None);
        let output = fixture
            .command_args(&["claude", "--resume", SESSION])
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "resumed claude:bright-otter (saved-session)\n"
        );
        assert!(fixture.trace().contains("action\tnew-tab"));
    }

    #[test]
    fn exact_provider_id_refusals_name_the_existing_session_action() {
        for (kind, live, launch, hint) in [
            (
                "codex",
                false,
                LaunchParams::default(),
                "rimz agents resume @bright-otter",
            ),
            (
                "claude",
                true,
                LaunchParams::default(),
                "rimz agents focus @bright-otter",
            ),
            (
                "claude",
                false,
                LaunchParams {
                    team: Some("forge".into()),
                    role: Some("coder".into()),
                    ..Default::default()
                },
                "rimz agents forge.coder --continue",
            ),
        ] {
            let fixture = Fixture::new(launch, live, None);
            let output = fixture
                .command_args(&[kind, "--resume", SESSION])
                .bounded_output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(hint), "{error}");
            assert!(!output.status.success());
            assert!(!fixture.trace().contains("action\tnew-tab"));
        }
    }

    #[test]
    fn provider_resume_refusals_explain_scope_picker_and_search_home() {
        let fixture = Fixture::new(LaunchParams::default(), false, None);
        for (args, hint) in [
            (vec!["claude,codex", "--resume"], "--continue"),
            (vec!["claude", "--resume"], "--resume=<id>"),
            (vec!["claude", "--resume=absent-id"], "absent-id"),
        ] {
            let output = fixture.command_args(&args).bounded_output().unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(hint), "{error}");
            assert!(!output.status.success());
            if args.contains(&"--resume=absent-id") {
                assert!(
                    error.contains("claude") && error.contains(".claude"),
                    "{error}"
                );
            }
            assert!(!fixture.trace().contains("action\tnew-tab"));
        }
    }

    #[test]
    fn live_session_in_another_channel_is_refused_with_focus() {
        let fixture = Fixture::new(LaunchParams::default(), true, None);
        let output = fixture.output("@bright-otter");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("claude:bright-otter")
                && stderr.contains("still live")
                && stderr.contains("rimz agents focus @bright-otter"),
            "{stderr}"
        );
        assert!(!fixture.trace().contains("action\tnew-tab"));
    }

    #[test]
    fn closed_team_member_is_refused_with_role_resume() {
        let fixture = Fixture::new(
            LaunchParams {
                team: Some("forge".into()),
                role: Some("coder".into()),
                ..Default::default()
            },
            false,
            None,
        );
        let output = fixture.output("@bright-otter");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("member of team `forge`")
                && stderr.contains("rimz agents forge.coder --continue"),
            "{stderr}"
        );
    }

    #[test]
    fn children_are_refused_with_subagents() {
        for (launch, parent) in [
            (LaunchParams::default(), Some("parent-session")),
            (
                LaunchParams {
                    launch_depth: Some(1),
                    ..Default::default()
                },
                None,
            ),
        ] {
            let fixture = Fixture::new(launch, false, parent);
            let output = fixture.output("@bright-otter");
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("rimz subagents"), "{stderr}");
        }
    }

    #[test]
    fn fresh_with_an_agent_reference_is_a_usage_error() {
        let fixture = Fixture::new(LaunchParams::default(), false, None);
        let output = fixture
            .command("@bright-otter")
            .arg("--fresh")
            .bounded_output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn background_resume_in_the_saved_directory_splits_without_focus() {
        let fixture = Fixture::new(LaunchParams::default(), false, None);
        let output = fixture
            .command("@bright-otter")
            .current_dir(&fixture.cwd)
            .env("ZELLIJ_PANE_ID", "2")
            .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.1")
            .arg("--bg")
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let trace = fixture.trace();
        assert!(!trace.contains("action\tnew-tab"), "{trace}");
        assert!(
            trace
                .lines()
                .any(|line| line.contains("action\tnew-pane") && line.contains("\t--no-focus")),
            "{trace}"
        );
    }

    #[test]
    fn in_place_resume_returns_without_an_idle_shell() {
        let fixture = Fixture::new(LaunchParams::default(), false, None);
        let marker = fixture.env.home_root.join("idle-shell");
        let output = fixture
            .command("@bright-otter")
            .current_dir(&fixture.cwd)
            .env("ZELLIJ_PANE_ID", "2")
            .env("RIMZ_TEST_IDLE_SHELL_MARKER", &marker)
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !marker.exists(),
            "in-place resume must return to the existing shell"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("resumed claude:bright-otter (saved-session)")
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("rimz agents resume @bright-otter")
        );
        let trace = fixture.trace();
        assert!(
            !trace.contains("action\tnew-tab") && !trace.contains("action\tnew-pane"),
            "{trace}"
        );
    }

    #[test]
    fn flag_resume_announces_before_new_tab_and_same_pane_takeover() {
        for same_pane in [false, true] {
            let fixture = Fixture::new(LaunchParams::default(), false, None);
            let output = fixture
                .command_args(&["worker", "--continue"])
                .current_dir(if same_pane {
                    &fixture.cwd
                } else {
                    &fixture.env.project_root
                })
                .env("ZELLIJ_PANE_ID", "2")
                .bounded_output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.starts_with(
                    "resuming worker in #saved\nresumed claude:bright-otter (saved-session)\n"
                ),
                "{stdout}"
            );
            assert_eq!(fixture.trace().contains("action\tnew-tab"), !same_pane);
            if same_pane {
                assert!(!fixture.trace().contains("action\tnew-pane"));
            }
        }
    }

    #[test]
    fn missing_conversation_and_worktree_use_planner_skip_labels() {
        for gone in [false, true] {
            let fixture = Fixture::new(LaunchParams::default(), false, None);
            if gone {
                std::fs::remove_dir_all(&fixture.cwd).unwrap();
            } else {
                std::fs::remove_file(&fixture.transcript).unwrap();
            }
            let output = fixture.output("@bright-otter");
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            let label = if gone {
                "worktree gone"
            } else {
                "no saved conversation"
            };
            assert!(
                stderr.contains(&format!("cannot resume @bright-otter: {label}")),
                "{stderr}"
            );
            if gone {
                assert!(fixture.env.store().read_events().unwrap().iter().any(|event| matches!(event.kind(), rimz::store::event::EventKind::AgentLifecycle(payload) if payload.event_name.as_deref() == Some("rimz.worktree-gone"))));
            }
        }
    }
}

#[test]
fn resumes_an_agent_stamped_in_the_real_rollup() {
    // The regression case: under the old session-name filter this rollup yields
    // an empty plan because every fold-built pane carries an empty session_name.
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/feature",
        "feature",
    );
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "the stamped agent is resumed from the real fold"
    );
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![resume_argv(&h, "claude", "sess-claude", "warm-drift")]
    );
    assert_eq!(plan.tabs[0].label, "#feature");
}

#[test]
fn rebirth_skips_a_session_stamped_with_another_account() {
    let h = Harness::new();
    let mut obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/feature",
        "feature",
    );
    obs.launch.login = Some("personal".parse().expect("login name"));
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append");

    let plan = plan_from_rollup(&h);

    assert!(
        plan.tabs.is_empty(),
        "a personal session never resumes in a default room"
    );
    assert_eq!(
        plan.skipped,
        [rimz::harness::resume::ResumeSkip {
            label: "claude:feature".to_owned(),
            reason: rimz::harness::resume::ResumeSkipReason::LoginMismatch,
        }]
    );
    assert_eq!(
        plan.warnings,
        [
            "@warm-drift's session belongs to claude account `personal`; this room launches claude on `default` (provider default). Run `rimz accounts use claude personal` to pin this room to it and resume, then `rimz accounts use --reset claude` to follow the provider default again."
        ]
    );
}

#[test]
fn resume_replays_role_and_team() {
    let h = Harness::new();
    let mut obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/feature",
        "feature",
    );
    obs.launch.role = Some("planner".to_owned());
    obs.launch.team = Some("forge".to_owned());
    obs.launch.profile = Some("claude-planner".to_owned());
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append");

    let plan = plan_from_rollup(&h);
    assert_eq!(plan.tabs.len(), 1);
    let commands = single_column(&plan.tabs[0]);
    assert_eq!(commands.len(), 1);
    let request = decode_exec_request(&commands[0]);
    assert_eq!(
        request.action,
        ExecAction::Resume {
            session_id: "sess-claude".to_owned(),
            extra_args: Vec::new(),
        }
    );
    assert_eq!(request.identity.name.as_deref(), Some("warm-drift"));
    assert_eq!(
        request.identity.params.profile.as_deref(),
        Some("claude-planner")
    );
    assert_eq!(request.identity.params.role.as_deref(), Some("planner"));
    assert_eq!(request.identity.params.team.as_deref(), Some("forge"));
    assert!(request.close_pane_on_exit);
}

#[test]
fn two_same_kind_agents_in_one_worktree_each_resume_their_own_pane() {
    // Two Claude sessions running side by side in one worktree, on distinct
    // panes. The fold keeps both stamped agents; resume keys on the pane, so
    // both come back — the `(kind, worktree, branch)` dedup used to collapse
    // them to one.
    let h = Harness::new();
    let first = registered("sess-a", "lane-a", "terminal_3", "/repo/shared", "main");
    let second = registered("sess-b", "lane-b", "terminal_4", "/repo/shared", "main");
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &first))
        .expect("append first");
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &second))
        .expect("append second");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "two concurrent same-kind agents in one worktree share one resume tab"
    );
    assert_eq!(plan.tabs[0].label, "#shared");
    assert_eq!(single_column(&plan.tabs[0]).len(), 2);
}

#[test]
fn a_relaunch_reusing_one_pane_resumes_only_the_newest() {
    // Sequential relaunch in place: a second session re-used the first's pane.
    // The audit fold keeps both stamped agents (it never collapses across
    // agent ids), so resume must dedup by pane and seed exactly one.
    let h = Harness::new();
    let older = registered("sess-old", "ember", "terminal_3", "/repo/work", "main");
    let newer = registered("sess-new", "ember", "terminal_3", "/repo/work", "main");
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &older))
        .expect("append older");
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &newer))
        .expect("append newer");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        plan.tabs.len(),
        1,
        "a relaunch sharing a pane resumes a single seed, not a ghost double"
    );
}

#[test]
fn a_rebirth_boundary_clears_a_prior_stamp_but_keeps_the_session_resumable() {
    // The pane stamp recorded before the boundary names a dead pane, so the
    // fold clears it. Durable provider identity still seeds explicit lane
    // resume after the pane numbering has been retired.
    let h = Harness::new();
    let obs = registered("sess-old", "old-ember", "terminal_3", "/repo/old", "old");
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append agent");
    h.store
        .append_event(&EventEnvelope::session_rebirth(
            h.workspace_id.clone(),
            "rimz-test",
        ))
        .expect("append rebirth");

    let plan = plan_from_rollup(&h);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![resume_argv(&h, "claude", "sess-old", "old-ember")]
    );
}

#[test]
fn soft_reset_preserves_dead_paneless_resume_identity() {
    let h = Harness::new();
    std::fs::write(h.store.paths().cache_dir.join("dead-reap.stamp"), b"")
        .expect("defer dead-owner reap");
    let mut planner = registered(
        "sess-planner",
        "warm-drift",
        "terminal_3",
        "/repo/feature",
        "feature",
    );
    planner.agent_pid = Some(u32::MAX);
    planner.launch.role = Some("planner".to_owned());
    planner.launch.team = Some("forge".to_owned());
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &planner))
        .expect("append dead-owner agent");
    h.store
        .append_event(&EventEnvelope::session_rebirth(
            h.workspace_id.clone(),
            "rimz-test",
        ))
        .expect("retire pane stamps");

    h.store.reset_records(false).expect("soft reset");
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    let preserved = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == "sess-planner")
        .expect("soft reset kept durable identity");
    assert!(preserved.pane.is_none());
    assert_eq!(preserved.role.as_deref(), Some("planner"));
    assert_eq!(preserved.team.as_deref(), Some("forge"));

    let plan = rimz::harness::resume::plan_resume(
        &projection.agents,
        &projection.ended,
        rimz::harness::resume::ResumeContext {
            in_place: false,
            project_root: None,
            workspace_id: &h.workspace_id,
            rimz_bin: Path::new("/bin/rimz"),
            runtime: h.store.runtime_paths(),
            profiles: &rimz::config::ProfilesConfig::default(),
            max: rimz::config::ResumeConfig::default().max,
            logins: &rimz::agents::RoomAccounts::default(),
            catalog: &rimz::agents::LoginCatalog::default(),
        },
        |_| true,
        |_| true,
    );
    assert_eq!(plan.tabs.len(), 1);
    let commands = single_column(&plan.tabs[0]);
    let command = &commands[0];
    let request = decode_exec_request(command);
    assert!(matches!(
        request.action,
        ExecAction::Resume { ref session_id, .. } if session_id == "sess-planner"
    ));
    assert_eq!(request.identity.params.role.as_deref(), Some("planner"));
    assert_eq!(request.identity.params.team.as_deref(), Some("forge"));
}

#[test]
fn a_stamp_after_the_rebirth_boundary_survives_and_is_resumed() {
    // A resumed agent re-stamps its new pane after the boundary; that fresh stamp
    // survives the fold's clear, so the next rebirth brings it back. This is what
    // keeps recovery working across repeated reboots.
    let h = Harness::new();
    let before = registered(
        "sess-codex",
        "calm-harbor",
        "terminal_3",
        "/repo/work",
        "work",
    );
    h.store
        .append_event(&lifecycle(&h, "codex", "SessionStart", &before))
        .expect("append pre-boundary");
    h.store
        .append_event(&EventEnvelope::session_rebirth(
            h.workspace_id.clone(),
            "rimz-test",
        ))
        .expect("append rebirth");
    // Same agent id, a fresh pane id (panes renumber on rebirth), after the boundary.
    let after = registered(
        "sess-codex",
        "calm-harbor",
        "terminal_1",
        "/repo/work",
        "work",
    );
    h.store
        .append_event(&lifecycle(&h, "codex", "SessionStart", &after))
        .expect("append post-boundary");

    let plan = plan_from_rollup(&h);
    assert_eq!(plan.tabs.len(), 1, "the post-boundary re-stamp is resumed");
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![resume_argv(&h, "codex", "sess-codex", "calm-harbor")]
    );
}

#[test]
fn an_agent_ended_trace_is_not_resumed() {
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/work",
        "work",
    );
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append start");
    let ended = AgentLifecycleObservation::new(Some("sess-claude".into()), LifecycleSignal::Ended);
    h.store
        .append_event(&lifecycle(&h, "claude", "rimz.agent-ended", &ended))
        .expect("append ended");

    let plan = plan_from_rollup(&h);
    assert!(
        plan.tabs.is_empty(),
        "rebirth auto-resume excludes ended agents"
    );
}

#[test]
fn missing_worktree_candidate_is_stamped_ended_not_reported() {
    let h = Harness::new();
    let obs = registered(
        "sess-claude",
        "warm-drift",
        "terminal_3",
        "/repo/gone",
        "gone",
    );
    h.store
        .append_event(&lifecycle(&h, "claude", "SessionStart", &obs))
        .expect("append start");
    let projection = h
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    let plan = rimz::harness::resume::plan_resume(
        &projection.agents,
        &projection.ended,
        rimz::harness::resume::ResumeContext {
            in_place: false,
            project_root: None,
            workspace_id: &h.workspace_id,
            rimz_bin: Path::new("/bin/rimz"),
            runtime: h.store.runtime_paths(),
            profiles: &rimz::config::ProfilesConfig::default(),
            max: rimz::config::ResumeConfig::default().max,
            logins: &rimz::agents::RoomAccounts::default(),
            catalog: &rimz::agents::LoginCatalog::default(),
        },
        |_| false,
        |_| true,
    );
    assert!(plan.tabs.is_empty());
    assert!(plan.skipped.is_empty());
    assert_eq!(
        plan.agents_to_end,
        vec![(
            rimz::ids::AgentKind::new_unchecked("claude"),
            "sess-claude".into()
        )]
    );

    let ended = AgentLifecycleObservation::new(Some("sess-claude".into()), LifecycleSignal::Ended);
    h.store
        .append_event(&lifecycle(&h, "claude", "rimz.worktree-gone", &ended))
        .expect("append end observation");
    assert!(
        plan_from_rollup(&h).tabs.is_empty(),
        "a follow-up rebirth plan sees the durable end stamp"
    );
}
