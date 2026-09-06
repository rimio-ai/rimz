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

    for (file_ordinal, launch_ordinal, role, session) in [
        (0, 0, "planner", "sess-attribution-planner"),
        (1, 0, "planner", "sess-attribution-planner-resumed"),
        (2, 1, "coder", "sess-attribution-coder"),
    ] {
        let rollout = day.join(format!(
            "rollout-2026-07-23T00-00-0{file_ordinal}-{session}.jsonl"
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
        observation.launch.launch_ordinal = Some(launch_ordinal);
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
    for session in [
        "sess-attribution-planner",
        "sess-attribution-planner-resumed",
    ] {
        let ended = AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Ended);
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                &workspace.session_name,
                "codex",
                "rimz.agent-ended",
                &ended,
            ))
            .expect("stamp planner session ended");
    }
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
            (
                "sess-attribution-planner-resumed",
                Some("feature"),
                Some("forge"),
                Some("planner")
            ),
        ]
    );
    let paths = env.state_path_for(&env.project_root);
    let ask_id = rimz::ids::AskId::parse("ask_0123456789abcdef").expect("ask id");
    for (session, entry_kind, from) in [
        (
            "sess-attribution-planner",
            rimz::transcript::TranscriptKind::Prompt,
            None,
        ),
        (
            "sess-attribution-planner",
            rimz::transcript::TranscriptKind::Ask,
            None,
        ),
        (
            "sess-attribution-planner",
            rimz::transcript::TranscriptKind::Answer,
            Some("you"),
        ),
        (
            "sess-attribution-coder",
            rimz::transcript::TranscriptKind::Message,
            Some("@planner"),
        ),
    ] {
        let mut entry = rimz::transcript::TranscriptEntry::new(
            jiff::Timestamp::now(),
            rimz::ids::AgentKind::new_unchecked("codex"),
            session.into(),
            entry_kind,
            String::new(),
        );
        if matches!(
            entry_kind,
            rimz::transcript::TranscriptKind::Ask | rimz::transcript::TranscriptKind::Answer
        ) {
            entry.id = Some(ask_id.clone());
        }
        entry.channel = Some("feature".to_owned());
        entry.from = from.map(ToOwned::to_owned);
        rimz::transcript::append(&paths, &entry).expect("append attribution transcript");
    }

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
    assert_eq!(report["schema"], 7);
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
    assert_eq!(planner["sessions"], 2);
    assert_eq!(planner["asks"], 1);
    assert_eq!(planner["asks_answered"], 1);
    assert_eq!(planner["messages"]["from_user"], 1);
    assert_eq!(planner["messages"]["to_teammates"], 1);
    assert_eq!(report["totals"]["messages"]["from_teammates"], 1);
    assert_eq!(report["totals"]["tokens"]["input"], 1_000);
    assert_eq!(report["totals"]["tokens"]["output"], 400);
    assert_eq!(report["totals"]["tokens"]["cache_read"], 1_000);
    assert_eq!(report["totals"]["cost_usd"], 0.0175);
    assert_eq!(report["totals"]["active_secs"], 60);
    assert_eq!(report["totals"]["asks_answered"], 1);
    assert_eq!(report["models"].as_array().map(Vec::len), Some(1));
    assert_eq!(report["models"][0]["model"], "gpt-5.5");
    assert_eq!(
        report["models"][0]["cost_usd"],
        report["totals"]["cost_usd"]
    );
    assert_eq!(report["models"][0]["tokens"], report["totals"]["tokens"]);
    for member in members {
        assert_eq!(member["models"].as_array().map(Vec::len), Some(1));
        assert_eq!(member["models"][0]["model"], "gpt-5.5");
        assert_eq!(member["models"][0]["tokens"], member["tokens"]);
        assert_eq!(member["models"][0]["cost_usd"], member["cost_usd"]);
    }
    for component in ["input", "output", "cache_write", "cache_read"] {
        assert_eq!(
            members
                .iter()
                .map(|member| member["models"][0]["tokens"][component]
                    .as_u64()
                    .expect("member model tokens"))
                .sum::<u64>(),
            report["models"][0]["tokens"][component]
                .as_u64()
                .expect("report model tokens")
        );
    }
    assert_eq!(
        members
            .iter()
            .map(|member| member["models"][0]["cost_usd"]
                .as_f64()
                .expect("model cost"))
            .sum::<f64>(),
        report["models"][0]["cost_usd"]
            .as_f64()
            .expect("total model cost")
    );

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
    assert!(markdown.contains(
        r#"<a href="https://github.com/rimio-ai/rimz">RimZ</a> <code>forge</code> team ·"#
    ));
    assert!(markdown.contains(" active ·"));
    assert!(markdown.contains(" messages (1 from you)</summary>"));
    assert!(markdown.contains("- **planner** — Codex `gpt-5.5@high`"));
    assert!(markdown.contains("  - activity: 1 ask"));
    assert!(markdown.contains("  - messages: 1 from you · 1 to teammates"));
    assert!(markdown.contains("  - tokens: "));
    assert!(!markdown.contains("agent-time"));
    assert!(!markdown.contains("human:"));
}

#[test]
fn attribution_cli_credits_claude_subagent_companions() {
    let env = Env::new();
    env.record(&env.project_root);
    let session = "sess-claude-subagents";
    let projects = env.config_root().join("claude/projects/project");
    let transcript = projects.join(format!("{session}.jsonl"));
    let subagents = projects.join(session).join("subagents");
    std::fs::create_dir_all(&subagents).expect("mkdir subagents");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-07-23T00:00:00.000Z","costUSD":1.0,"requestId":"main","message":{"id":"main","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            "\n"
        ),
    )
    .expect("write parent transcript");
    std::fs::write(
        subagents.join("agent-child.jsonl"),
        concat!(
            r#"{"timestamp":"2026-07-23T00:00:01.000Z","costUSD":2.0,"requestId":"child","isSidechain":true,"message":{"id":"child","model":"child-model","usage":{"input_tokens":20,"output_tokens":2}}}"#,
            "\n"
        ),
    )
    .expect("write subagent transcript");
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let mut observation =
        AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
    observation.launch.team = Some("forge".to_owned());
    observation.launch.role = Some("planner".to_owned());
    observation.launch.channel = Some("feature".to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "UserPromptSubmit",
            &observation,
        ))
        .expect("register Claude agent");
    let mut child =
        AgentLifecycleObservation::new(Some("child".into()), LifecycleSignal::Registered);
    child.parent_agent_id = Some(session.into());
    child.task = Some("Explore".to_owned());
    child.worktree_path = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SubagentStart",
            &child,
        ))
        .expect("register Claude subagent");

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

    assert_eq!(report["schema"], 7);
    assert_eq!(
        report["models"],
        serde_json::json!([
            {
                "model": "child-model",
                "tokens": {"input": 20, "output": 2, "cache_write": 0, "cache_read": 0},
                "cost_usd": 2.0,
            },
            {
                "model": null,
                "tokens": {"input": 10, "output": 1, "cache_write": 0, "cache_read": 0},
                "cost_usd": 1.0,
            },
        ])
    );
    assert_eq!(
        report["groups"][0]["members"][0]["models"],
        report["models"]
    );
    assert_eq!(report["totals"]["tokens"]["input"], 30);
    assert_eq!(report["totals"]["tokens"]["output"], 3);
    assert_eq!(report["totals"]["cost_usd"], 3.0);
    assert_eq!(
        report["groups"][0]["members"][0]["subagents"][0]["task"],
        "explore"
    );
    assert_eq!(
        report["groups"][0]["members"][0]["subagents"][0]["count"],
        1
    );
    assert_eq!(
        report["groups"][0]["members"][0]["subagents"][0]["cost_usd"],
        2.0
    );
    assert!(
        report["groups"][0]["members"][0]["subagents"][0]
            .get("origin")
            .is_none()
    );

    let markdown = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature", "--md"])
        .output()
        .expect("run markdown attribution");
    assert!(markdown.status.success());
    let markdown = String::from_utf8(markdown.stdout).expect("markdown utf8");
    assert!(markdown.contains("effort: $3.00"));
    assert!(markdown.contains("  - effort: $3.00\n  - subagents: 1 × explore · $2.00"));
    assert!(markdown.contains("**Models**"));
    assert!(markdown.contains("`child-model`"));
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
fn attribution_markdown_drops_opened_turns_without_recorded_contributions() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let session = "sess-opened-turn-only";
    let mut registered =
        AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
    registered.launch.team = Some("forge".to_owned());
    registered.launch.role = Some("reviewer".to_owned());
    registered.launch.channel = Some("feature".to_owned());
    registered.worktree_path = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &registered,
        ))
        .expect("register attribution agent");
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "UserPromptSubmit",
            &AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::TurnStarted),
        ))
        .expect("open attribution turn");

    let panel = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature"])
        .output()
        .expect("run panel attribution");
    assert!(panel.status.success());
    assert!(
        String::from_utf8(panel.stdout)
            .expect("panel utf8")
            .contains("@reviewer")
    );

    let json = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature", "--json"])
        .output()
        .expect("run JSON attribution");
    assert!(json.status.success());
    let json: Value = serde_json::from_slice(&json.stdout).expect("attribution json");
    assert_eq!(json["totals"]["agents"], 1);

    let markdown = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "#feature", "--md"])
        .output()
        .expect("run Markdown attribution");
    assert!(markdown.status.success());
    assert!(markdown.stdout.is_empty());
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
        let active_start = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1);
        rimz::store::active_time::record_progress(
            &env.runtime_paths(),
            "codex",
            session,
            active_start,
            180,
        )
        .expect("open active span");
        rimz::store::active_time::record_stop(
            &env.runtime_paths(),
            "codex",
            session,
            active_start + jiff::SignedDuration::from_secs(1),
            180,
        )
        .expect("close active span");
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

#[test]
fn attribution_counts_only_the_current_worktree_lifetime() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let env = Env::new();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "rimz@example.com"],
        vec!["config", "user.name", "RimZ Test"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(args)
            .output()
            .expect("run fixture git");
        assert!(
            output.status.success(),
            "fixture git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let run = |args: &[&str]| {
        let output = env.rimz().args(args).output().expect("run rimz");
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&["worktree", "new", "demo"]);
    let checkout = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&checkout)
        .expect("read worktree marker")
        .expect("managed worktree marker");
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let transcripts = env.home_root.join("attribution-transcripts");
    std::fs::create_dir_all(&transcripts).expect("mkdir transcripts");
    let records = [
        ("sess-old-root", None, -2, 10.0),
        ("sess-old-child", Some("sess-old-root"), -1, 20.0),
        ("sess-current-root", None, 1, 1.0),
        ("sess-current-child", Some("sess-current-root"), 2, 3.0),
    ];
    let store = env.store();
    let mut transcript_contents = Vec::new();
    for (session, parent, offset, cost) in records {
        let registered_at = marker.created_at + jiff::SignedDuration::from_secs(offset);
        let transcript = transcripts.join(format!("{session}.jsonl"));
        let contents = format!(
            "{}\n",
            serde_json::json!({
                "timestamp": registered_at,
                "costUSD": cost,
                "requestId": session,
                "message": {"id": session, "usage": {"input_tokens": 10, "output_tokens": 1}},
            })
        );
        std::fs::write(&transcript, &contents).expect("write transcript");
        transcript_contents.push((transcript.clone(), contents));
        let mut observation =
            AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Registered);
        observation.launch.team = Some("forge".to_owned());
        observation.launch.role = Some("planner".to_owned());
        observation.launch.channel = Some("demo".to_owned());
        observation.worktree_path = Some(marker.worktree_path.display().to_string());
        observation.worktree_branch = Some(marker.branch.clone());
        observation.transcript_path = Some(transcript.display().to_string());
        if let Some(parent) = parent {
            observation.launch.parent_agent_id = Some(parent.into());
            observation.launch.parent_agent_kind =
                Some(rimz::ids::AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
            observation.launch.profile = Some("explorer".to_owned());
        }
        let mut event = rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "UserPromptSubmit",
            &observation,
        );
        event.timestamp = registered_at;
        store
            .append_event(&event)
            .expect("register attribution agent");
        let ended = AgentLifecycleObservation::new(Some(session.into()), LifecycleSignal::Ended);
        let mut event = rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "rimz.agent-ended",
            &ended,
        );
        event.timestamp = registered_at + jiff::SignedDuration::from_millis(100);
        store.append_event(&event).expect("end attribution agent");
    }

    for selector in ["#demo", "--all"] {
        let report: Value =
            serde_json::from_slice(&run(&["agents", "attribution", selector, "--json"]))
                .expect("attribution json");
        assert_eq!(report["groups"].as_array().expect("groups").len(), 1);
        let members = report["groups"][0]["members"].as_array().expect("members");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0]["sessions"], 1);
        assert_eq!(members[0]["cost_usd"], 4.0);
        assert_eq!(
            members[0]["subagents"],
            serde_json::json!([{"task": "explorer", "count": 1, "cost_usd": 3.0}])
        );
        assert_eq!(report["totals"]["agents"], 1);
        assert_eq!(report["totals"]["cost_usd"], 4.0);
        assert_eq!(report["scope"]["since"], marker.created_at.to_string());
        assert_eq!(
            report["scope"]["worktree"],
            marker.worktree_path.display().to_string()
        );
    }
    let boundary = format!("since {}", marker.created_at);
    for args in [
        vec!["agents", "attribution", "#demo"],
        vec!["agents", "attribution", "#demo", "--md"],
    ] {
        let report = String::from_utf8(run(&args)).expect("attribution utf8");
        assert!(report.contains(&boundary), "missing {boundary}: {report}");
    }

    run(&["worktree", "remove", "demo"]);
    assert!(!checkout.exists(), "worktree removed");
    for selector in ["#demo", "--all"] {
        let report: Value =
            serde_json::from_slice(&run(&["agents", "attribution", selector, "--json"]))
                .expect("attribution json after removal");
        assert_eq!(report["groups"], serde_json::json!([]));
        assert_eq!(
            report["totals"],
            serde_json::to_value(rimz::agents::attribution::EffortTotals::default())
                .expect("empty totals")
        );
    }
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection after removal");
    assert_eq!(audit.agents.len(), records.len());
    for (session, _, offset, _) in records {
        let agent = audit
            .agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == session)
            .expect("retained audit agent");
        assert_eq!(
            agent.registered_at,
            Some(marker.created_at + jiff::SignedDuration::from_secs(offset))
        );
    }
    for (path, contents) in transcript_contents {
        assert_eq!(
            std::fs::read_to_string(path).expect("retained transcript"),
            contents
        );
    }
}

#[test]
fn attribution_scope_keeps_a_launched_child_with_its_parent() {
    let env = Env::new();
    env.record(&env.project_root);
    let sibling = env.home_root.join("sibling-worktree");
    std::fs::create_dir_all(&sibling).expect("mkdir sibling worktree");
    let transcripts = env.home_root.join("attribution-transcripts");
    std::fs::create_dir_all(&transcripts).expect("mkdir transcript dir");
    let parent_transcript = transcripts.join("parent.jsonl");
    let child_transcript = transcripts.join("child.jsonl");
    std::fs::write(
        &parent_transcript,
        concat!(
            r#"{"timestamp":"2026-07-23T00:00:00.000Z","costUSD":1.0,"requestId":"parent","message":{"id":"parent","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            "\n"
        ),
    )
    .expect("write parent transcript");
    std::fs::write(
        &child_transcript,
        concat!(
            r#"{"timestamp":"2026-07-23T00:00:01.000Z","costUSD":3.0,"requestId":"child","message":{"id":"child","usage":{"input_tokens":30,"output_tokens":3}}}"#,
            "\n"
        ),
    )
    .expect("write child transcript");
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");

    let mut parent = AgentLifecycleObservation::new(
        Some("sess-attribution-parent".into()),
        LifecycleSignal::Registered,
    );
    parent.launch.team = Some("forge".to_owned());
    parent.launch.role = Some("planner".to_owned());
    parent.launch.channel = Some("project/forge".to_owned());
    parent.worktree_path = Some(env.project_root.display().to_string());
    parent.transcript_path = Some(parent_transcript.display().to_string());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "UserPromptSubmit",
            &parent,
        ))
        .expect("register parent");

    let mut child = AgentLifecycleObservation::new(
        Some("sess-attribution-child".into()),
        LifecycleSignal::Registered,
    );
    child.launch.parent_agent_id = Some("sess-attribution-parent".into());
    child.launch.parent_agent_kind = Some(rimz::ids::AgentKind::new_unchecked("claude"));
    child.launch.launch_depth = Some(1);
    child.launch.profile = Some("explorer".to_owned());
    child.launch.channel = Some("sibling-worktree".to_owned());
    child.worktree_path = Some(sibling.display().to_string());
    child.transcript_path = Some(child_transcript.display().to_string());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "UserPromptSubmit",
            &child,
        ))
        .expect("register child");

    let run_json = |args: &[&str]| {
        let output = env
            .rimz()
            .arg("--root")
            .arg(&env.project_root)
            .args(args)
            .output()
            .expect("run attribution");
        assert!(
            output.status.success(),
            "attribution failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).expect("attribution json")
    };

    let default = run_json(&["agents", "attribution", "--json"]);
    assert_eq!(default["totals"]["agents"], 1);
    assert_eq!(default["totals"]["cost_usd"], 4.0);
    assert!(
        default["groups"][0]["members"][0]["subagents"][0]
            .get("origin")
            .is_none()
    );

    let sibling_scope = run_json(&["agents", "attribution", "sibling-worktree", "--json"]);
    assert_eq!(sibling_scope["totals"]["agents"], 0);

    let all = run_json(&["agents", "attribution", "--all", "--json"]);
    assert_eq!(all["totals"]["agents"], 1);
    assert_eq!(all["totals"]["cost_usd"], 4.0);

    let markdown = env
        .rimz()
        .arg("--root")
        .arg(&env.project_root)
        .args(["agents", "attribution", "--md"])
        .output()
        .expect("run Markdown attribution");
    assert!(markdown.status.success());
    let markdown = String::from_utf8(markdown.stdout).expect("Markdown utf8");
    assert!(markdown.contains("  - effort: $4.00\n  - subagents: 1 × explorer · $3.00"));
    assert!(!markdown.contains("launched"));
}
