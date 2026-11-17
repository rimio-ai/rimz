//! A resume of a live lane and a launch onto a live cohort focus the pane the hosting lookup
//! answers for the agent the planner chose, never the pane stored on that agent's row.

use std::io::Write as _;
use std::process::Stdio;
use std::time::Duration;

use super::*;
use crate::common::DeliveryRendezvous;

/// How the live agent is registered: a lane's own agent, or a member of a `duo` cohort.
#[derive(Clone, Copy)]
enum Live {
    Lane,
    Cohort,
}

impl Live {
    fn session(self) -> &'static str {
        match self {
            Self::Lane => "provider-session",
            Self::Cohort => "held-session",
        }
    }
}

/// A user's `rimz --mux zellij <args>` over a room whose listing shows the zellij panes
/// `ids`, with one live claude agent registered in `terminal_3` as `live` says.
fn live_room(live: Live, ids: &[u64], args: &[&str]) -> (Env, std::process::Command) {
    let env = Env::new();
    env.install_agent_hooks("claude");
    crate::common::trust_claude_project(&env, &env.project_root);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    env.store().record_workspace(&workspace).unwrap();
    let worktree = match live {
        Live::Lane => env.project_root.clone(),
        Live::Cohort => {
            crate::common::write_definition(
                &env,
                "agents",
                "worker",
                "description: Worker\nagent: claude\ntools: []",
                "",
            );
            crate::common::write_definition(
                &env,
                "teams",
                "duo",
                "layout: lead,worker\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}\n  - {role: worker, agent: worker}",
                "Complete the work.",
            );
            assert!(init_launch_repo(&env.project_root));
            let path =
                rimz::worktree::worktree_path(&env.project_root, &Default::default(), "review")
                    .unwrap();
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    };
    let session = live.session();
    let observation = AgentLifecycleObservation {
        pane_id: Some(rimz::ids::PaneId::parse("zellij:terminal_3").unwrap()),
        runtime_owner: Some(rimz::pane::RuntimeOwner::new(
            rimz::pane::RuntimeOwnerKind::Agent,
            session,
            // An owner apart from the test process, which is the command's ancestor: an
            // owner among its ancestors would make the agent the caller, which takes no focus.
            env.agent_owner_pid(),
            None,
        )),
        worktree_path: Some(worktree.display().to_string()),
        launch: LaunchParams {
            lane: Some(crate::common::wait::stamped_lane(&env, &worktree, "review")),
            team: matches!(live, Live::Cohort).then(|| "duo".to_owned()),
            ..Default::default()
        },
        ..AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered)
    };
    append_registered(&env, observation);
    let panes = serde_json::Value::Array(
        ids.iter()
            .map(|id| serde_json::json!({"id": id, "is_plugin": false, "tab_id": 1, "title": "claude"}))
            .collect(),
    )
    .to_string();
    let mut command = env.rimz();
    command
        .args(["--mux", "zellij"])
        .args(args)
        .env("ZELLIJ_PANE_ID", "1")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_TRACE_CONTEXT", "1")
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.1")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_CLIENTS",
            "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1 terminal_1 sh\n",
        )
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        );
    (env, command)
}

fn append_registered(env: &Env, observation: AgentLifecycleObservation) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
}

/// The panes the command focused, in order.
fn focused(env: &Env) -> Vec<String> {
    std::fs::read_to_string(env.home_root.join("mux.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            line.split('\t')
                .skip_while(|arg| *arg != "focus-pane-id")
                .nth(1)
        })
        .map(str::to_owned)
        .collect()
}

/// The live agent moves from its stored `terminal_3` to `terminal_4` after the planner chose
/// it and before the command reads the lookup: the focus lands on `terminal_4`.
#[test]
fn live_lane_and_cohort_focus_the_pane_the_lookup_answers() {
    for (live, args) in [
        (Live::Lane, &["agents", "resume", "#review"][..]),
        (Live::Cohort, &["teams", "duo", "-w", "review"]),
    ] {
        let (env, mut command) = live_room(live, &[1, 3, 4], args);
        let hold = DeliveryRendezvous::new(&env, "before-read");
        let child = command
            .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
            let mut observation = AgentLifecycleObservation::new(
                Some(live.session().into()),
                LifecycleSignal::Registered,
            );
            observation.pane_id = Some(rimz::ids::PaneId::parse("zellij:terminal_4").unwrap());
            append_registered(&env, observation);
            held.write_all(&[0]).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(focused(&env), ["terminal_4"], "{args:?}: {output:?}");
    }
}

/// The live agent's stored pane is no longer listed, so the lookup hosts it nowhere: the lane
/// resume refuses and the cohort launch reports it running, and neither focuses a pane.
#[test]
fn live_lane_and_cohort_hosted_nowhere_focus_nothing() {
    let (env, mut command) = live_room(Live::Lane, &[1], &["agents", "resume", "#review"]);
    let output = command.bounded_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("error: live lane agent has no bound pane"),
        "{stderr}"
    );
    assert!(focused(&env).is_empty(), "{:?}", focused(&env));

    let (env, mut command) = live_room(Live::Cohort, &[1], &["teams", "duo", "-w", "review"]);
    let output = command.bounded_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(stderr.contains("duo#review is already running"), "{stderr}");
    assert!(!stderr.contains("focused it"), "{stderr}");
    assert!(focused(&env).is_empty(), "{:?}", focused(&env));
}

/// The live cohort member leaves `terminal_3` for `terminal_4` while the launch wakes it: the
/// launch reports the cohort running, as for a member hosted nowhere, and focuses nothing.
#[test]
fn live_cohort_member_that_moves_during_its_wake_is_reported_running_unfocused() {
    let (env, mut command) = live_room(Live::Cohort, &[1, 3, 4], &["teams", "duo", "-w", "review"]);
    let hold = DeliveryRendezvous::new(&env, "after-wake");
    let child = command
        .env("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &hold.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
        let mut observation = AgentLifecycleObservation::new(
            Some(Live::Cohort.session().into()),
            LifecycleSignal::Registered,
        );
        observation.pane_id = Some(rimz::ids::PaneId::parse("zellij:terminal_4").unwrap());
        append_registered(&env, observation);
        held.write_all(&[0]).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(stderr.contains("duo#review is already running"), "{stderr}");
    assert!(!stderr.contains("focused it"), "{stderr}");
    assert!(!stderr.contains("while it was woken"), "{stderr}");
    assert!(focused(&env).is_empty(), "{:?}", focused(&env));
}
