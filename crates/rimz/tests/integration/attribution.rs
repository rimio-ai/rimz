//! Durable attribution through the public CLI, including an exited teammate.

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use serde_json::Value;

use crate::common::Env;

#[test]
fn attribution_credits_exited_team_members_and_transcript_spend() {
    let env = Env::new();
    env.record(&env.project_root);
    let pricing = env.runtime_paths().shared_pricing_cache_path();
    std::fs::create_dir_all(pricing.parent().expect("pricing cache parent"))
        .expect("mkdir pricing cache parent");
    std::fs::write(
        pricing,
        r#"{"schema":4,"models":{"gpt-5.5":{"input":0.000005,"output":0.00003,"cache_read":0.0000005,"cache_create":0.000005,"cache_read_explicit":true,"fast_multiplier":1.0}}}"#,
    )
    .expect("write pricing cache");
    let sessions = env.home_root.join("codex-sessions");
    let day = sessions.join("2026").join("07").join("23");
    std::fs::create_dir_all(&day).expect("mkdir rollout tree");
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");

    for (ordinal, role, session) in [
        (0, "planner", "sess-attribution-planner"),
        (1, "coder", "sess-attribution-coder"),
    ] {
        let rollout = day.join(format!(
            "rollout-2026-07-23T00-00-0{ordinal}-{session}.jsonl"
        ));
        std::fs::write(
            &rollout,
            concat!(
                "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n",
                "{\"type\":\"event_msg\",\"timestamp\":\"2026-07-23T00:00:00.000Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":1000,\"cached_input_tokens\":500,\"output_tokens\":200,\"total_tokens\":1200}}}}\n",
            ),
        )
        .expect("write rollout");
        let mut observation =
            AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
        observation.launch.role = Some(role.to_owned());
        observation.launch.team = Some("forge".to_owned());
        observation.launch.launch_ordinal = Some(ordinal);
        observation.launch.channel = Some("feature".to_owned());
        observation.launch.model = Some("gpt-5.5".to_owned());
        observation.launch.effort = Some("high".to_owned());
        observation.worktree_path = Some(env.project_root.display().to_string());
        observation.transcript_path = Some(rollout.display().to_string());
        env.store()
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                &workspace.session_name,
                "codex",
                "UserPromptSubmit",
                &observation,
            ))
            .expect("register attribution agent");
    }

    let ended = AgentLifecycleObservation::new(
        Some("sess-attribution-planner".into()),
        LifecycleSignal::Ended,
    );
    let store = env.store();
    let active_start = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
    rimz::store::active_time::record_progress(
        &env.runtime_paths(),
        "codex",
        "sess-attribution-coder",
        active_start,
        180,
    )
    .expect("open active span");
    rimz::store::active_time::record_stop(
        &env.runtime_paths(),
        "codex",
        "sess-attribution-coder",
        active_start + jiff::SignedDuration::from_secs(60),
        180,
    )
    .expect("close active span");
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "codex",
            "rimz.agent-ended",
            &ended,
        ))
        .expect("stamp planner ended");
    let mut audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    audit
        .agents
        .sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    assert_eq!(
        audit
            .agents
            .iter()
            .map(|agent| (
                agent.agent_id.as_str(),
                agent.channel.as_deref(),
                agent.team.as_deref(),
                agent.role.as_deref(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                "sess-attribution-coder",
                Some("feature"),
                Some("forge"),
                Some("coder")
            ),
            (
                "sess-attribution-planner",
                Some("feature"),
                Some("forge"),
                Some("planner")
            ),
        ]
    );

    let output = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature", "--json"])
        .output()
        .expect("run attribution");
    assert!(
        output.status.success(),
        "attribution failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("attribution json");
    assert_eq!(report["schema"], 1);
    assert_eq!(
        report["groups"].as_array().map(Vec::len),
        Some(1),
        "{report:#}"
    );
    assert_eq!(report["groups"][0]["team"]["name"], "forge");
    let members = report["groups"][0]["members"].as_array().expect("members");
    assert_eq!(members.len(), 2);
    let planner = members
        .iter()
        .find(|member| member["role"] == "planner")
        .expect("planner");
    assert_eq!(planner["presence"], "exited");
    assert!(
        report["totals"]["tokens"]["input"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        report["totals"]["cost_usd"]
            .as_f64()
            .is_some_and(|cost| cost > 0.0)
    );
    assert_eq!(report["totals"]["active_secs"], 60);

    let markdown = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature", "--md"])
        .output()
        .expect("run markdown attribution");
    assert!(markdown.status.success());
    let markdown = String::from_utf8(markdown.stdout).expect("markdown utf8");
    assert!(markdown.starts_with("<details>\n"));
    assert!(markdown.contains("<code>forge</code> team"));
    assert!(markdown.contains("- **planner** — Codex `gpt-5.5@high`"));
    assert!(markdown.contains("  - tokens: "));
}

#[test]
fn attribution_output_modes_conflict_at_the_cli_boundary() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["agents", "attribution", "--json", "--md"])
        .output()
        .expect("run conflicting modes");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn attribution_default_stays_in_the_callers_checkout() {
    let env = Env::new();
    env.record(&env.project_root);
    let sibling = env.home_root.join("sibling-worktree");
    std::fs::create_dir_all(&sibling).expect("mkdir sibling worktree");
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    for (session, channel, worktree) in [
        (
            "sess-main-agent",
            "project/forge",
            env.project_root.as_path(),
        ),
        ("sess-sibling-agent", "sibling-worktree", sibling.as_path()),
    ] {
        let mut observation =
            AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
        observation.launch.team = Some("forge".to_owned());
        observation.launch.role = Some("coder".to_owned());
        observation.launch.channel = Some(channel.to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
        env.store()
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                &workspace.session_name,
                "codex",
                "UserPromptSubmit",
                &observation,
            ))
            .expect("register attribution agent");
    }

    let default = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "--json"])
        .output()
        .expect("run default attribution");
    assert!(
        default.status.success(),
        "default attribution failed: {}",
        String::from_utf8_lossy(&default.stderr)
    );
    let default: Value = serde_json::from_slice(&default.stdout).expect("default json");
    assert_eq!(default["totals"]["agents"], 1);
    assert_eq!(
        default["scope"]["worktree"],
        env.project_root.display().to_string()
    );

    let all = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "--all", "--json"])
        .output()
        .expect("run room-wide attribution");
    assert!(
        all.status.success(),
        "room-wide attribution failed: {}",
        String::from_utf8_lossy(&all.stderr)
    );
    let all: Value = serde_json::from_slice(&all.stdout).expect("all json");
    assert_eq!(all["totals"]["agents"], 2);
}
