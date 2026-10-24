use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::common::{Env, path_with_front, write_fake_login_shell, write_path_shim};
use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::mux::{MuxBackend, PaneListOptions, PaneReadConsistency};
use rimz::store::workflow::{self, RunState};

pub(super) fn check_layout(
    env: &Env,
    backend: &dyn MuxBackend,
    session: &str,
    unrelated: &rimz::ids::PaneId,
    command: impl Fn() -> Command,
) {
    if crate::common::git::git_missing() {
        return;
    }
    crate::common::git::init_repo(&env.project_root);
    env.install_agent_hooks("claude");
    let bin = env.home_root.join("workflow-bin");
    let provider_ready = env.home_root.join("provider-ready");
    write_path_shim(
        &bin,
        "claude",
        &format!(
            "[ \"$1\" = --version ] && {{ printf '2.1.296 (Claude Code)\\n'; exit; }}; printf '%s' \"$$\" > {}; exec sleep 300",
            shlex::try_quote(provider_ready.to_str().unwrap()).unwrap()
        ),
    );
    let ready = env.home_root.join("editor-ready");
    write_path_shim(
        &bin,
        "editor",
        &format!(
            "printf ready > {}; exec sleep 300",
            shlex::try_quote(ready.to_str().unwrap()).unwrap()
        ),
    );
    let shell = write_fake_login_shell(env, "workflow-shell", &[]);
    fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&serde_json::json!({
            "mux": {"default": unrelated.mux()},
            "agents": {"shell":shell, "commands": {"editor": shlex::try_quote(bin.join("editor").to_str().unwrap()).unwrap()}}
        }))
        .unwrap(),
    )
    .unwrap();
    env.write_config(
        &env.project_root,
        &toml::to_string(&serde_json::json!({
            "agents": [{"name":"claude", "isolation":"host", "env":{"PATH":path_with_front(&bin).to_string_lossy()}}]
        }))
        .unwrap(),
    );
    let grant = command()
        .args(["trust", "grant", "--agents", "claude"])
        .env("PATH", path_with_front(&bin))
        .output()
        .unwrap();
    assert!(
        grant.status.success(),
        "{}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let dir = env.rimz_home().join("workflows/layout");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("prompt.md"), "Work with me").unwrap();
    let state = env.state_path_for(&env.project_root);
    let context = command();
    let runtime_root = context
        .get_envs()
        .find(|(key, _)| *key == "XDG_RUNTIME_DIR")
        .and_then(|(_, value)| value)
        .unwrap();
    let store = rimz::Store::open(
        state.clone(),
        rimz::RuntimePaths::for_state_under(&state, runtime_root.as_ref()),
    )
    .unwrap();
    let panes = || {
        backend
            .list_panes(PaneListOptions {
                session_name: Some(session.into()),
                consistency: PaneReadConsistency::RequireAuthoritative,
                ..Default::default()
            })
            .unwrap()
    };
    for lifecycle in ["turn", "stay"] {
        let _ = fs::remove_file(&ready);
        let _ = fs::remove_file(&provider_ready);
        fs::write(dir.join("workflow.toml"), format!("[workflow]\nscope='worktree'\nstart='before'\n[checks.before]\nshell='true'\non={{pass='work',fail='@failed'}}\n[steps.work]\nagent='claude,editor'\nlifecycle='{lifecycle}'\nprompt-file='prompt.md'\nthen='after'\n[checks.after]\nshell='true'\non={{pass='@done',fail='@failed'}}\n")).unwrap();
        let output = command()
            .env("PATH", path_with_front(&bin))
            .env("SHELL", &shell)
            .args(["workflow", "run", "layout"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "a mixed workflow layout must launch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let id: rimz::ids::WorkflowRunId = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let run = loop {
            let record = workflow::load(&state.workflow(&id)).unwrap();
            assert_eq!(
                record.state,
                RunState::Active,
                "{record:#?}\n{:?}",
                workflow::journal(&state.workflow(&id)).unwrap()
            );
            if let Some(attempt) = record.attempt.and_then(|attempt| attempt.run) {
                let run = rimz::harness::run::load(&state, &attempt.run_id).unwrap();
                let started_pid = fs::read_to_string(&provider_ready)
                    .ok()
                    .and_then(|pid| pid.parse::<u32>().ok());
                if ready.exists()
                    && started_pid.is_some()
                    && started_pid == run.provider_pid
                    && run.pane_id.is_some()
                    && run.provider_pid.is_some()
                {
                    break run;
                }
            }
            assert!(
                Instant::now() < deadline,
                "both cells must start: ready={}, panes={:?}, sessions={:?}, runs={:?}",
                ready.exists(),
                panes(),
                backend.list_sessions(),
                rimz::harness::run::list(&state).unwrap()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        // No tick may stand in for the wrapper's provider-exit wake.
        let tick_after = jiff::Timestamp::now()
            .checked_add(Duration::from_secs(3600))
            .unwrap();
        rimz::disk::atomic::write_temp_then_rename_cache(
            &store.runtime_paths().lane_path("workflow-tick.json"),
            &BTreeMap::from([(id.clone(), tick_after)]),
        )
        .unwrap();
        let host = workflow::load(&state.workflow(&id))
            .unwrap()
            .attempt
            .unwrap()
            .host;
        let deadline = Instant::now() + Duration::from_secs(15);
        while rimz::proc::process_is_live(host.pid, host.start.as_deref()) {
            assert!(
                Instant::now() < deadline,
                "initial workflow advance must exit before provider-exit wake"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let cells: Vec<_> = panes()
            .panes
            .iter()
            .filter(|pane| {
                Some(&pane.pane_id) == run.pane_id.as_ref()
                    || pane.title.as_deref() == Some("editor")
            })
            .map(|pane| pane.pane_id.clone())
            .collect();
        assert_eq!(cells.len(), 2, "leader and command must have panes");
        let leader = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| run.matches_agent(agent))
            .unwrap();
        let adapter = rimz::agents::find_definition("claude").unwrap();
        for signal in [
            LifecycleSignal::Registered,
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        ] {
            let observation = AgentLifecycleObservation::new(Some(leader.agent_id.clone()), signal);
            rimz::harness::run::settle_lifecycle(&store, &run.run_id, adapter, &observation, None)
                .unwrap();
        }
        if lifecycle == "stay" {
            assert_eq!(
                rimz::harness::run::load(&state, &run.run_id)
                    .unwrap()
                    .status,
                rimz::store::run::RunStatus::Running
            );
            let output = command().args(["workflow", "list"]).output().unwrap();
            assert!(String::from_utf8_lossy(&output.stdout).contains("with you"));
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(run.provider_pid.unwrap().try_into().unwrap()),
                nix::sys::signal::Signal::SIGTERM,
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let record = workflow::load(&state.workflow(&id)).unwrap();
                if record.state == RunState::Done {
                    assert_eq!(
                        record.prev_attempt.unwrap().outcome,
                        workflow::AttemptOutcome::Ended
                    );
                    assert_eq!(record.node, "after", "the wake must route then");
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "provider-exit wake must finish the stay attempt without a tick: {record:#?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        } else {
            assert!(
                command()
                    .args(["workflow", "advance", id.as_str()])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let current = panes();
            let current = &current.panes;
            assert!(
                current.iter().any(|pane| &pane.pane_id == unrelated),
                "unrelated pane must survive"
            );
            if current.iter().all(|pane| !cells.contains(&pane.pane_id)) {
                break;
            }
            assert!(Instant::now() < deadline, "every workflow cell must close");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            workflow::load(&state.workflow(&id)).unwrap().state,
            RunState::Done
        );
    }
}
