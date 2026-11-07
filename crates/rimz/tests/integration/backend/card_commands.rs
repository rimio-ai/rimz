use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, AgentStatus, LifecycleSignal};
use rimz::ids::PaneId;
use rimz::mux::{ClientFocusOptions, MuxBackend, PaneListOptions, PaneReadConsistency};
use rimz::store::event::EventEnvelope;
use rimz::store::snapshot::SidebarSnapshot;

use crate::backend::shared_view::wait_frame;
use crate::common::{
    CommandTimeoutExt, Env, path_with_front, write_fake_login_shell, write_hook_firing_agent,
    write_path_shim,
};

pub(super) fn check_cards(
    env: &Env,
    backend: &dyn MuxBackend,
    session: &str,
    runtime_root: &Path,
    command: impl Fn() -> Command,
) {
    env.install_agent_hooks("claude");
    let bin = write_hook_firing_agent(env, "claude");
    let provider = bin.join("claude-hooks");
    std::fs::rename(bin.join("claude"), &provider).unwrap();
    write_path_shim(
        &bin,
        "claude",
        &format!(
            "case \"$RIMZ_AGENT_NAME\" in\n\
             card-a) export RIMZ_TEST_AGENT_SESSION=a1111111-1111-4111-8111-111111111111;;\n\
             *) export RIMZ_TEST_AGENT_SESSION=b1111111-1111-4111-8111-111111111111;;\n\
             esac\nexport RIMZ_TEST_AGENT_WAIT_STDIN=1\nexec {} \"$@\"",
            shlex::try_quote(provider.to_str().unwrap()).unwrap()
        ),
    );
    let shell = write_fake_login_shell(env, "card-shell", &[]);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&serde_json::json!({"agents": {"shell": shell}})).unwrap(),
    )
    .unwrap();
    env.write_config(
        &env.project_root,
        &toml::to_string(&serde_json::json!({
            "agents": [{"name": "claude", "env": {"PATH": path_with_front(&bin).to_string_lossy()}}]
        }))
        .unwrap(),
    );
    let command = || {
        let mut cmd = command();
        cmd.env("PATH", path_with_front(&bin)).env("SHELL", &shell);
        cmd
    };
    command()
        .args(["trust", "grant", "--agents", "claude"])
        .assert_success_within_timeout("trust the stub provider");
    let runtime =
        rimz::RuntimePaths::for_state_under(&env.state_path_for(&env.project_root), runtime_root);
    wait_frame(
        &runtime,
        |frame| !frame.client_views.is_empty(),
        "attached client",
    );
    let first_born = backend
        .client_view(ClientFocusOptions {
            session_name: Some(session.into()),
            ..Default::default()
        })
        .unwrap()
        .clients[0]
        .pane_id
        .clone();
    let store = rimz::Store::open(env.state_path_for(&env.project_root), runtime.clone()).unwrap();
    let mut agents = Vec::new();
    for (name, id) in [
        ("card-a", "a1111111-1111-4111-8111-111111111111"),
        ("card-b", "b1111111-1111-4111-8111-111111111111"),
    ] {
        command()
            .args([
                "agents",
                "claude",
                "work",
                "--name",
                name,
                "--new-tab",
                "--isolation",
                "host",
            ])
            .assert_success_within_timeout("launch a card in its own tab");
        let deadline = Instant::now() + Duration::from_secs(30);
        let agent = loop {
            let snapshot = store.snapshot().unwrap();
            if let Some(agent) = snapshot.agents.iter().find(|agent| {
                agent.agent_id.as_str() == id
                    && agent.pane.is_some()
                    && agent.status == AgentStatus::Success
            }) {
                break agent.clone();
            }
            if Instant::now() >= deadline {
                let capture = command()
                    .args(["pane", "capture", &format!("@{name}")])
                    .bounded_output()
                    .unwrap();
                let agents: Vec<_> = snapshot
                    .agents
                    .iter()
                    .map(|agent| (&agent.agent_id, &agent.name, agent.status))
                    .collect();
                panic!("provider hooks never settled: {agents:?}; capture: {capture:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        agents.push(agent);
    }
    for agent in &agents {
        let observation = AgentLifecycleObservation::new(
            Some(agent.agent_id.clone()),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            },
        );
        store
            .append_event(&EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                session,
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let output = command()
            .args(["sidebar", "snapshot", "--json", "--no-produce"])
            .bounded_output()
            .unwrap();
        assert!(output.status.success(), "consumer snapshot: {output:?}");
        let snapshot: SidebarSnapshot = serde_json::from_slice(&output.stdout).unwrap();
        if agents.iter().all(|agent| {
            snapshot.rows().any(|row| {
                row.id == agent.agent_id.as_str()
                    && row
                        .attention_status()
                        .is_some_and(AgentStatus::is_actionable)
            })
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both cards must reach the consumer before narrowing the roster"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let card = |args: &[&str]| {
        let output = command()
            .current_dir(&env.home_root)
            .args(["sidebar"])
            .args(args)
            .args(["--session-name", session])
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success() && output.stdout.is_empty(),
            "{args:?}: {output:?}"
        );
        output
    };
    let a = &agents[0].pane.as_ref().unwrap().pane_id;
    let b = &agents[1].pane.as_ref().unwrap().pane_id;
    let focus = |pane: &PaneId| {
        command()
            .args(["pane", "focus", &pane.to_string()])
            .assert_success_within_timeout("external pane focus");
        wait_focus(backend, session, &runtime, pane);
    };
    focus(a);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let output = card(&["filter", "needs-you"]);
        if output.stderr.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "needs-you never reached host: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    quiet(card(&["walk", "next"]));
    wait_focus(backend, session, &runtime, b);
    quiet(card(&["walk", "next"]));
    wait_focus(backend, session, &runtime, a);
    quiet(card(&["walk", "prev"]));
    wait_focus(backend, session, &runtime, b);
    quiet(card(&["slot", "1"]));
    wait_focus(backend, session, &runtime, &first_born);
    quiet(card(&["last"]));
    wait_focus(backend, session, &runtime, b);
    quiet(card(&["last"]));
    wait_focus(backend, session, &runtime, &first_born);

    focus(a);
    quiet(card(&["walk", "next"]));
    wait_focus(backend, session, &runtime, b);
    assert!(
        pane_views(backend, session, &runtime).contains_key(b),
        "original agent tab must exist"
    );
    quiet(card(&["close"]));
    let surviving_views = pane_views(backend, session, &runtime);
    assert!(
        !surviving_views.contains_key(b),
        "closed pane still exists: {b}"
    );
    quiet(card(&["reopen"]));
    let deadline = Instant::now() + Duration::from_secs(30);
    let reopened = loop {
        let snapshot = store.snapshot().unwrap();
        if let Some(agent) = snapshot.agents.iter().find(|agent| {
            agent.agent_id == agents[1].agent_id
                && agent.pane.as_ref().is_some_and(|pane| pane.pane_id != *b)
        }) {
            break agent.clone();
        }
        assert!(
            Instant::now() < deadline,
            "reopened card never bound: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let new_pane = reopened.pane.unwrap().pane_id;
    let reopened_views = pane_views(backend, session, &runtime);
    let new_view = reopened_views
        .get(&new_pane)
        .expect("reopened agent tab must exist");
    assert!(
        surviving_views
            .keys()
            .all(|pane| reopened_views.get(pane) != Some(new_view)),
        "reopen needs a new tab: {new_view}, surviving tabs: {surviving_views:?}"
    );
    wait_focus(backend, session, &runtime, &new_pane);
}

fn quiet(output: Output) {
    assert!(output.stderr.is_empty(), "unexpected refusal: {output:?}");
}

#[track_caller]
fn wait_focus(
    backend: &dyn MuxBackend,
    session: &str,
    runtime: &rimz::RuntimePaths,
    pane: &PaneId,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let view = backend
            .client_view(ClientFocusOptions {
                session_name: Some(session.into()),
                ..Default::default()
            })
            .unwrap();
        if view.clients.len() == 1 && view.clients[0].pane_id == *pane {
            break;
        }
        assert!(Instant::now() < deadline, "focus {pane}: {view:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
    wait_frame(
        runtime,
        |frame| frame.focused_pane.as_ref() == Some(pane),
        "host sees focus",
    );
}

fn pane_views(
    backend: &dyn MuxBackend,
    session: &str,
    runtime: &rimz::RuntimePaths,
) -> HashMap<PaneId, String> {
    backend
        .list_panes(PaneListOptions {
            session_name: Some(session.into()),
            runtime_paths: Some(runtime.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .unwrap()
        .panes
        .into_iter()
        .filter_map(|pane| pane.view_id.map(|view| (pane.pane_id, view)))
        .collect()
}
