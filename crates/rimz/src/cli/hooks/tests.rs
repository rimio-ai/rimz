use super::lifecycle::fill_root_launch_identity;
use super::lifecycle::handle_lifecycle_hook;
use super::proctree::matches_agent_kind;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentAdapter as _, AgentLifecycleObservation};
use rimz::ids::AgentSessionId;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::store::runtime::process_owner;
use std::sync::atomic::{AtomicUsize, Ordering};

fn id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn pane(raw: &str, command: &str, cwd: &str, _focused: bool) -> PaneRef {
    PaneRef {
        pane_id: id(raw),
        session_name: "rimz-test".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn root_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    )
}

fn hooks_test_store() -> (tempfile::TempDir, rimz::Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace_id =
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
    let paths = rimz::store::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = rimz::store::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    (dir, store)
}

fn hooks_test_workspace(worktree_branch: Option<&str>) -> rimz::ResolvedWorkspace {
    rimz::ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new(
            "/tmp/hooks-test",
        )),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
        worktree_branch: worktree_branch.map(ToOwned::to_owned),
        session_name: "hooks-test".to_owned(),
        mux_hint: None,
    }
}

fn hooks_test_globals() -> crate::cli::GlobalFlags {
    crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    }
}

#[derive(Default)]
struct CorrelationTestAdapter {
    correlation_calls: AtomicUsize,
}

impl rimz::agents::AgentAdapter for CorrelationTestAdapter {
    fn descriptor(&self) -> &'static rimz::agents::AgentDescriptor {
        rimz::agents::AntigravityAdapter.descriptor()
    }

    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::DecodedHook> {
        let mut decoded = rimz::agents::AntigravityAdapter.decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(id("terminal_77"));
        });
        Ok(decoded)
    }

    fn correlate_subagent(
        &self,
        input: rimz::agents::SubagentCorrelationInput<'_>,
    ) -> Option<rimz::agents::SubagentCorrelation> {
        self.correlation_calls.fetch_add(1, Ordering::Relaxed);
        let child = input.child_agent_id.as_str();
        let parent = input.parent_agent_id.as_str();
        let matches = match child {
            "child-clean" | "child-failed" | "child-parked" | "child-recovered" => parent == "root",
            "nested-child" => parent == "child-clean",
            "ambiguous-child" => {
                matches!(parent, "ambiguous-parent-a" | "ambiguous-parent-b")
            }
            "cycle-child" => parent == "cycle-parent",
            _ => false,
        };
        matches.then(|| rimz::agents::SubagentCorrelation {
            agent_name: Some(format!("name-{child}")),
            role: Some(format!("role-{child}")),
            task: Some(format!("task-{child}")),
            prompt: Some(format!("prompt-{child}")),
            model: None,
        })
    }

    fn spawned_subagents(
        &self,
        input: rimz::agents::SubagentSpawnInput<'_>,
    ) -> Vec<rimz::agents::SpawnedSubagent> {
        let child =
            |id: &'static str, name: &'static str, role: &'static str, prompt: &'static str| {
                rimz::agents::SpawnedSubagent {
                    child_agent_id: AgentSessionId::from(id),
                    agent_name: Some(name.to_owned()),
                    role: Some(role.to_owned()),
                    prompt: Some(prompt.to_owned()),
                    model: None,
                    total_tokens: None,
                }
            };
        match input.parent_agent_id.as_str() {
            "late-root" => vec![
                child(
                    "late-child-clean",
                    "clean-name",
                    "clean-role",
                    "clean prompt",
                ),
                child(
                    "late-child-failed",
                    "failed-name",
                    "failed-role",
                    "failed prompt",
                ),
            ],
            "reaped-root" => vec![child(
                "late-child-reaped",
                "reaped-name",
                "reaped-role",
                "reaped prompt",
            )],
            _ => Vec::new(),
        }
    }
}

fn antigravity_payload(agent_id: &str) -> serde_json::Value {
    serde_json::json!({
        "conversationId": agent_id,
        "workspacePaths": ["/tmp/hooks-test"],
        "transcriptPath": format!("/tmp/{agent_id}.jsonl"),
    })
}

fn feed_antigravity(
    store: &rimz::Store,
    adapter: &dyn rimz::agents::AgentAdapter,
    event_name: &str,
    payload: serde_json::Value,
) {
    let decoded = adapter.decode_hook(event_name, &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        adapter,
        &decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

struct CopilotCorrelationAdapter;

impl rimz::agents::AgentAdapter for CopilotCorrelationAdapter {
    fn descriptor(&self) -> &'static rimz::agents::AgentDescriptor {
        rimz::agents::CopilotAdapter.descriptor()
    }

    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::DecodedHook> {
        let mut decoded = rimz::agents::CopilotAdapter.decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(id("terminal_88"));
        });
        Ok(decoded)
    }

    fn correlate_subagent(
        &self,
        input: rimz::agents::SubagentCorrelationInput<'_>,
    ) -> Option<rimz::agents::SubagentCorrelation> {
        rimz::agents::CopilotAdapter.correlate_subagent(input)
    }

    fn spawned_subagents(
        &self,
        input: rimz::agents::SubagentSpawnInput<'_>,
    ) -> Vec<rimz::agents::SpawnedSubagent> {
        rimz::agents::CopilotAdapter.spawned_subagents(input)
    }
}

fn feed_copilot(store: &rimz::Store, event_name: &str, payload: serde_json::Value) {
    let decoded = CopilotCorrelationAdapter
        .decode_hook(event_name, &payload)
        .unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        &CopilotCorrelationAdapter,
        &decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

struct PiAdoptionTestAdapter {
    pane_id: PaneId,
}

impl rimz::agents::AgentAdapter for PiAdoptionTestAdapter {
    fn descriptor(&self) -> &'static rimz::agents::AgentDescriptor {
        rimz::agents::PiAdapter.descriptor()
    }

    fn decode_hook(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> rimz::agents::Result<rimz::agents::DecodedHook> {
        let mut decoded = rimz::agents::PiAdapter.decode_hook(event_name, payload)?;
        decoded.update_lifecycle(|observation| {
            observation.pane_id = Some(self.pane_id.clone());
        });
        Ok(decoded)
    }
}

fn feed_pi(
    store: &rimz::Store,
    adapter: &dyn rimz::agents::AgentAdapter,
    event_name: &str,
    payload: serde_json::Value,
) {
    let decoded = adapter.decode_hook(event_name, &payload).unwrap();
    handle_lifecycle_hook(
        &hooks_test_workspace(Some("main")),
        store,
        adapter,
        &decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &hooks_test_globals(),
    )
    .unwrap();
}

fn pi_session_payload(session_id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": session_id,
        "cwd": "/tmp/hooks-test",
        "model": "gpt-5.6-sol",
        "effort": "xhigh",
        "context_pct": 12,
        "context_window": 200_000,
        "total_tokens": 24_000,
    })
}

fn pi_bridge_payload(parent_id: &str, child_id: &str) -> serde_json::Value {
    serde_json::json!({
        "session_id": parent_id,
        "cwd": "/tmp/hooks-test",
        "subagent_id": child_id,
        "subagent_label": "general-purpose: inspect the bridge",
        "subagent_source": "pi-session",
    })
}

fn adoption_signals(store: &rimz::Store) -> Vec<LifecycleSignal> {
    store
        .read_events()
        .unwrap()
        .iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.event_name.as_deref() == Some("SubagentAdopted") =>
            {
                Some(payload.observation.signal.clone())
            }
            _ => None,
        })
        .collect()
}

fn seed_subagent_candidate(store: &rimz::Store, agent_id: &str, parent_id: &str) {
    let mut candidate = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::SubagentStarted,
    );
    candidate.parent_agent_id = Some(AgentSessionId::from(parent_id));
    candidate.agent_name = Some(agent_id.to_owned());
    candidate.task = Some("seeded candidate".to_owned());
    candidate.pane_id = Some(id("terminal_77"));
    candidate.worktree_path = Some("/tmp/hooks-test".to_owned());
    candidate.transcript_path = Some(format!("/tmp/{agent_id}.jsonl"));
    store
        .append_agent_lifecycle(rimz::store::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "seed-candidate",
            observation: &candidate,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn launch_identity_env(
    _observation: &AgentLifecycleObservation,
    var: &'static str,
) -> Option<String> {
    match var {
        rimz::harness::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
        rimz::harness::run::ENV_TEAM => Some("forge".to_owned()),
        rimz::harness::run::ENV_LAUNCH_GROUP => Some("launch_group_1".to_owned()),
        rimz::harness::run::ENV_LAUNCH_ORDINAL => Some("2".to_owned()),
        rimz::harness::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
        rimz::harness::run::ENV_AGENT_MODEL => Some("env-model".to_owned()),
        rimz::harness::run::ENV_AGENT_EFFORT => Some("env-effort".to_owned()),
        _ => None,
    }
}

#[test]
fn agent_kind_matches_known_launch_shapes() {
    for (comm, source, expected) in [
        ("claude", "claude", true),
        ("codex", "codex", true),
        ("codex-aarch64-a", "codex", true),
        ("node", "codex", true),
        ("node", "claude", false),
        ("kiro-cli", "kiro", true),
        ("kiro-cli-chat", "kiro", true),
        ("kiro-cli-term", "kiro", false),
        ("zsh", "claude", false),
        ("bash", "codex", false),
    ] {
        assert_eq!(
            matches_agent_kind(comm, source),
            expected,
            "{comm}/{source}"
        );
    }
}

#[test]
fn stop_failure_records_turn_error_transcript_entry() {
    let (_dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();

    let payload = serde_json::json!({
        "session_id": "sess-1",
        "error": "overloaded",
        "last_assistant_message": "API Error: Response stalled mid-stream. The response above may be incomplete."
    });
    let decoded = rimz::agents::ClaudeAdapter
        .decode_hook("StopFailure", &payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        &decoded,
        &payload,
        rimz::agents::HookIngressOwner::agent(Some(std::process::id())),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one transcript entry, got {entries:?}");
    };
    assert_eq!(entry.entry, rimz::transcript::TranscriptKind::Error);
    assert_eq!(entry.kind.as_str(), "claude");
    assert_eq!(entry.agent_id.as_str(), "sess-1");
    assert_eq!(entry.channel.as_deref(), Some("hooks-test"));
    assert_eq!(
        entry.text,
        "API Error: Response stalled mid-stream. The response above may be incomplete."
    );
}

#[test]
fn canonical_droid_prompt_and_worker_stop_record_one_conversation() {
    let (dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();
    let transcript_path = dir.path().join("droid-session.jsonl");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"user\",\"timestamp\":\"2026-07-13T20:19:51.315Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ping\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"assistant\",\"parentId\":\"user\",\"timestamp\":\"2026-07-13T20:19:54.616Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}}\n",
        ),
    )
    .unwrap();
    let path = transcript_path.to_string_lossy();
    let owner_pid = std::process::id();

    let prompt_payload = serde_json::json!({
        "session_id": "droid-session",
        "transcript_path": path,
        "prompt": "ping"
    });
    let prompt_decoded = rimz::agents::DroidAdapter
        .decode_hook("UserPromptSubmit", &prompt_payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::DroidAdapter,
        &prompt_decoded,
        &prompt_payload,
        rimz::agents::HookIngressOwner::agent(Some(owner_pid)),
        &globals,
    )
    .unwrap();
    let stop_payload = serde_json::json!({
        "session_id": "droid-session",
        "transcript_path": path
    });
    let stop_decoded = rimz::agents::DroidAdapter
        .decode_hook("Stop", &stop_payload)
        .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::DroidAdapter,
        &stop_decoded,
        &stop_payload,
        rimz::agents::HookIngressOwner::agent(Some(owner_pid)),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[0].text, "ping");
    assert_eq!(
        entries[1].entry,
        rimz::transcript::TranscriptKind::Assistant
    );
    assert_eq!(entries[1].text, "pong");
    let user_inputs = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(user_inputs.len(), 1);
    assert_eq!(user_inputs[0].kind.as_str(), "droid");
    assert_eq!(
        user_inputs[0].origin.as_deref(),
        Some(std::path::Path::new("/tmp/hooks-test"))
    );
}

#[test]
fn root_launch_identity_fills_from_env_then_config_without_clobbering_payload() {
    let mut observed = root_observation();
    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(observed.launch.role.as_deref(), Some("coder"));
    assert_eq!(observed.launch.team.as_deref(), Some("forge"));
    assert_eq!(
        observed.launch.launch_group.as_deref(),
        Some("launch_group_1")
    );
    assert_eq!(observed.launch.launch_ordinal, Some(2));
    assert_eq!(observed.launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(observed.launch.model.as_deref(), Some("env-model"));
    assert_eq!(observed.launch.effort.as_deref(), Some("env-effort"));

    let mut payload = root_observation();
    payload.launch.role = Some("payload-role".to_owned());
    payload.launch.team = Some("payload-team".to_owned());
    payload.launch.launch_group = Some("payload-group".to_owned());
    payload.launch.launch_ordinal = Some(7);
    payload.launch.profile = Some("payload-profile".to_owned());
    payload.launch.model = Some("payload-model".to_owned());
    payload.launch.effort = Some("payload-effort".to_owned());
    fill_root_launch_identity(
        &mut payload,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(payload.launch.role.as_deref(), Some("payload-role"));
    assert_eq!(payload.launch.team.as_deref(), Some("payload-team"));
    assert_eq!(
        payload.launch.launch_group.as_deref(),
        Some("payload-group")
    );
    assert_eq!(payload.launch.launch_ordinal, Some(7));
    assert_eq!(payload.launch.profile.as_deref(), Some("payload-profile"));
    assert_eq!(payload.launch.model.as_deref(), Some("payload-model"));
    assert_eq!(payload.launch.effort.as_deref(), Some("payload-effort"));

    let mut configured = root_observation();
    fill_root_launch_identity(
        &mut configured,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        |_observation, var| match var {
            rimz::harness::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
            rimz::harness::run::ENV_TEAM => Some("forge".to_owned()),
            rimz::harness::run::ENV_LAUNCH_ORDINAL => Some("not-a-number".to_owned()),
            rimz::harness::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
            _ => None,
        },
    );
    assert_eq!(configured.launch.model.as_deref(), Some("cfg-model"));
    assert_eq!(configured.launch.effort.as_deref(), Some("cfg-effort"));
    assert_eq!(configured.launch.launch_ordinal, None);
}

#[test]
fn subagent_launch_identity_is_not_inherited_from_parent_env() {
    let mut observed = root_observation();
    observed.parent_agent_id = Some(AgentSessionId::from("parent-1"));

    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );

    assert_eq!(observed.launch.role, None);
    assert_eq!(observed.launch.team, None);
    assert_eq!(observed.launch.launch_group, None);
    assert_eq!(observed.launch.launch_ordinal, None);
    assert_eq!(observed.launch.profile, None);
    assert_eq!(observed.launch.model, None);
    assert_eq!(observed.launch.effort, None);
}

#[test]
fn correlated_antigravity_children_keep_independent_lifecycle_and_root_parent() {
    let (_dir, store) = hooks_test_store();
    let adapter = CorrelationTestAdapter::default();
    let mut root = antigravity_payload("root");
    root["invocationNum"] = serde_json::json!(0);
    root["modelName"] = serde_json::json!("parent-model");
    feed_antigravity(&store, &adapter, "PreInvocation", root);

    let mut clean = antigravity_payload("child-clean");
    clean["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", clean.clone());
    let calls_after_start = adapter.correlation_calls.load(Ordering::Relaxed);
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "child-clean")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("root"));
    assert_eq!(child.name.as_deref(), Some("name-child-clean"));
    assert_eq!(child.role.as_deref(), Some("role-child-clean"));
    assert_eq!(child.task.as_deref(), Some("task-child-clean"));
    assert_eq!(child.prompt.as_deref(), Some("prompt-child-clean"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Running);
    assert!(child.profile.is_none() && child.team.is_none());
    assert!(child.model.is_none() && child.effort.is_none());

    feed_antigravity(&store, &adapter, "PostToolUse:observed", clean.clone());
    let mut clean_stop = clean;
    clean_stop["fullyIdle"] = serde_json::json!(true);
    clean_stop["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", clean_stop);
    assert_eq!(
        adapter.correlation_calls.load(Ordering::Relaxed),
        calls_after_start,
        "a persisted child relation must skip later transcript correlation"
    );
    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "child-clean")
        .unwrap();
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);
    assert_eq!(child.parent_agent_id.as_deref(), Some("root"));

    let mut failed = antigravity_payload("child-failed");
    failed["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", failed.clone());
    failed["fullyIdle"] = serde_json::json!(true);
    failed["terminationReason"] = serde_json::json!("max_steps_exceeded");
    feed_antigravity(&store, &adapter, "Stop", failed);

    let mut parked = antigravity_payload("child-parked");
    parked["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", parked.clone());
    parked["fullyIdle"] = serde_json::json!(false);
    parked["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", parked.clone());
    let parked_state = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "child-parked")
        .unwrap();
    assert_eq!(parked_state.status, rimz::agents::AgentStatus::Running);
    assert_eq!(parked_state.phase, rimz::agents::TurnPhase::Parked);
    parked["fullyIdle"] = serde_json::json!(true);
    feed_antigravity(&store, &adapter, "Stop", parked);

    let mut recovered = antigravity_payload("child-recovered");
    recovered["fullyIdle"] = serde_json::json!(true);
    recovered["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", recovered);

    let mut nested = antigravity_payload("nested-child");
    nested["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", nested);

    let mut uncorrelated = antigravity_payload("unrelated-root");
    uncorrelated["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", uncorrelated);

    let agents = store.snapshot_cached().unwrap().agents;
    let state = |id: &str| agents.iter().find(|state| state.agent_id == id).unwrap();
    assert_eq!(
        state("child-failed").status,
        rimz::agents::AgentStatus::Failed
    );
    assert_eq!(
        state("child-parked").status,
        rimz::agents::AgentStatus::Success
    );
    assert_eq!(
        state("child-recovered").status,
        rimz::agents::AgentStatus::Success
    );
    assert_eq!(
        state("child-recovered").parent_agent_id.as_deref(),
        Some("root")
    );
    assert_eq!(
        state("nested-child").parent_agent_id.as_deref(),
        Some("root")
    );
    assert_eq!(state("unrelated-root").parent_agent_id, None);
}

#[test]
fn copilot_child_metadata_reconciles_at_the_parent_checkpoint() {
    let (_store_dir, store) = hooks_test_store();
    let transcript_dir = tempfile::tempdir().unwrap();
    let parent_dir = transcript_dir.path().join("parent-session");
    std::fs::create_dir(&parent_dir).unwrap();
    let transcript = parent_dir.join("events.jsonl");
    let records = include_str!("../../agents/copilot/tests/fixtures/subagents.jsonl")
        .lines()
        .collect::<Vec<_>>();
    std::fs::write(&transcript, format!("{}\n{}\n", records[0], records[1])).unwrap();
    feed_copilot(
        &store,
        "sessionStart",
        serde_json::json!({
            "sessionId":"parent-session",
            "source":"startup",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "userPromptSubmitted",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "prompt":"delegate this",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "userPromptSubmitted",
        serde_json::json!({
            "sessionId":"toolu_alpha",
            "cwd":"/tmp/hooks-test",
            "prompt":"Trace the retry flow",
            "transcriptPath":"",
        }),
    );
    feed_copilot(
        &store,
        "agentStop",
        serde_json::json!({
            "sessionId":"toolu_alpha",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":"",
        }),
    );

    let child_before_completion = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "toolu_alpha")
        .unwrap();
    assert_eq!(
        child_before_completion.parent_agent_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(child_before_completion.name.as_deref(), Some("researcher"));
    assert_eq!(
        child_before_completion.task.as_deref(),
        Some("Inspect auth retry")
    );
    assert_eq!(
        child_before_completion.model.as_deref(),
        Some("claude-haiku-4.5")
    );
    assert_eq!(child_before_completion.usage.total_tokens, None);
    assert_eq!(
        child_before_completion.status,
        rimz::agents::AgentStatus::Success
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    use std::io::Write as _;
    writeln!(file, "{}", records[2]).unwrap();
    feed_copilot(
        &store,
        "postToolUse",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
            "toolName":"task",
        }),
    );

    let child = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|state| state.agent_id == "toolu_alpha")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent-session"));
    assert_eq!(child.name.as_deref(), Some("researcher"));
    assert_eq!(child.task.as_deref(), Some("Inspect auth retry"));
    assert_eq!(child.model.as_deref(), Some("claude-haiku-4.5"));
    assert_eq!(child.usage.total_tokens, Some(22_116));
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);

    let reconciliation_count = || {
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("SubagentReconciled")
                )
            })
            .count()
    };
    assert_eq!(reconciliation_count(), 1);
    feed_copilot(
        &store,
        "postToolUse",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    feed_copilot(
        &store,
        "agentStop",
        serde_json::json!({
            "sessionId":"parent-session",
            "cwd":"/tmp/hooks-test",
            "transcriptPath":transcript,
        }),
    );
    assert_eq!(
        reconciliation_count(),
        1,
        "repeated root checkpoints are idempotent"
    );

    let signals = store
        .read_events()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.observation.agent_id.as_deref() == Some("toolu_alpha") =>
            {
                Some(payload.observation.signal)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        signals,
        vec![
            LifecycleSignal::SubagentStarted,
            LifecycleSignal::SubagentStopped { errored: false },
            LifecycleSignal::SubagentStopped { errored: false },
        ]
    );
}

#[test]
fn pi_bridge_adopts_an_existing_rich_root_with_the_live_signal() {
    let (_dir, store) = hooks_test_store();
    let adapter = PiAdoptionTestAdapter {
        pane_id: id("terminal_pi"),
    };
    feed_pi(
        &store,
        &adapter,
        "session_start",
        pi_session_payload("parent-session"),
    );
    feed_pi(
        &store,
        &adapter,
        "session_start",
        pi_session_payload("child-session"),
    );
    feed_pi(
        &store,
        &adapter,
        "subagent_started",
        pi_bridge_payload("parent-session", "child-session"),
    );

    assert_eq!(
        adoption_signals(&store),
        vec![LifecycleSignal::SubagentStarted]
    );
    let snapshot = store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "child-session")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("parent-session"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Running);
    assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(child.effort.as_deref(), Some("xhigh"));
    assert_eq!(child.usage.context_pct, Some(12));
    assert_eq!(child.usage.total_tokens, Some(24_000));
}

#[test]
fn pi_bridge_adoption_preserves_parented_and_foreign_pane_roots() {
    let parent_adapter = PiAdoptionTestAdapter {
        pane_id: id("terminal_parent"),
    };

    let (_dir, parented_store) = hooks_test_store();
    let mut parented = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("parented-child")),
        LifecycleSignal::SubagentStarted,
    );
    parented.parent_agent_id = Some(AgentSessionId::from("original-parent"));
    parented.task = Some("existing child".to_owned());
    parented.pane_id = Some(parent_adapter.pane_id.clone());
    parented_store
        .append_agent_lifecycle(rimz::store::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("pi"),
            event_name: "seed-parented",
            observation: &parented,
            spawned_subagents: &[],
        })
        .unwrap();
    feed_pi(
        &parented_store,
        &parent_adapter,
        "subagent_started",
        pi_bridge_payload("new-parent", "parented-child"),
    );
    let snapshot = parented_store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "parented-child")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("original-parent"));
    assert!(adoption_signals(&parented_store).is_empty());

    let (_dir, foreign_store) = hooks_test_store();
    let foreign_adapter = PiAdoptionTestAdapter {
        pane_id: id("terminal_foreign"),
    };
    feed_pi(
        &foreign_store,
        &parent_adapter,
        "session_start",
        pi_session_payload("parent-session"),
    );
    feed_pi(
        &foreign_store,
        &foreign_adapter,
        "session_start",
        pi_session_payload("foreign-child"),
    );
    feed_pi(
        &foreign_store,
        &parent_adapter,
        "subagent_started",
        pi_bridge_payload("parent-session", "foreign-child"),
    );
    let snapshot = foreign_store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "foreign-child")
        .unwrap();
    assert_eq!(child.parent_agent_id, None);
    assert_eq!(child.model.as_deref(), Some("gpt-5.6-sol"));
    assert!(adoption_signals(&foreign_store).is_empty());
}

#[test]
fn antigravity_parent_stop_adopts_children_after_late_transcript_flush() {
    let (_dir, store) = hooks_test_store();
    let adapter = CorrelationTestAdapter::default();
    let mut root = antigravity_payload("late-root");
    root["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", root.clone());

    let mut clean = antigravity_payload("late-child-clean");
    clean["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", clean.clone());
    clean["fullyIdle"] = serde_json::json!(true);
    clean["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", clean);

    let mut failed = antigravity_payload("late-child-failed");
    failed["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", failed.clone());
    failed["fullyIdle"] = serde_json::json!(true);
    failed["terminationReason"] = serde_json::json!("max_steps_exceeded");
    feed_antigravity(&store, &adapter, "Stop", failed);

    let before = store.snapshot_cached().unwrap().agents;
    let failed = before
        .iter()
        .find(|state| state.agent_id == "late-child-failed")
        .unwrap();
    assert_eq!(failed.parent_agent_id, None);
    assert_ne!(failed.name.as_deref(), Some("failed-name"));

    root["fullyIdle"] = serde_json::json!(true);
    root["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", root.clone());

    let agents = store.snapshot_cached().unwrap().agents;
    let clean = agents
        .iter()
        .find(|state| state.agent_id == "late-child-clean")
        .unwrap();
    assert_eq!(clean.parent_agent_id.as_deref(), Some("late-root"));
    assert_eq!(clean.status, rimz::agents::AgentStatus::Success);
    assert_eq!(clean.name.as_deref(), Some("clean-name"));
    assert_eq!(clean.role.as_deref(), Some("clean-role"));
    assert_eq!(clean.task.as_deref(), Some("clean-role"));
    assert_eq!(clean.prompt.as_deref(), Some("clean prompt"));
    let failed = agents
        .iter()
        .find(|state| state.agent_id == "late-child-failed")
        .unwrap();
    assert_eq!(failed.parent_agent_id.as_deref(), Some("late-root"));
    assert_eq!(failed.status, rimz::agents::AgentStatus::Failed);
    assert_eq!(failed.name.as_deref(), Some("failed-name"));
    assert_eq!(failed.role.as_deref(), Some("failed-role"));

    let adoption_count = || {
        store
            .read_events()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    rimz::store::event::EventKind::AgentLifecycle(payload)
                        if payload.event_name.as_deref() == Some("SubagentAdopted")
                )
            })
            .count()
    };
    assert_eq!(adoption_count(), 2);
    feed_antigravity(&store, &adapter, "Stop", root);
    assert_eq!(adoption_count(), 2, "a repeated parent Stop is idempotent");
}

#[test]
fn antigravity_parent_stop_rematerializes_a_reaped_child() {
    let (_dir, store) = hooks_test_store();
    let adapter = CorrelationTestAdapter::default();
    let mut root = antigravity_payload("reaped-root");
    root["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", root.clone());

    let mut child = antigravity_payload("late-child-reaped");
    child["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", child.clone());
    child["fullyIdle"] = serde_json::json!(true);
    child["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", child);
    let ended = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("late-child-reaped")),
        LifecycleSignal::Ended,
    );
    store
        .append_agent_lifecycle(rimz::store::AgentLifecycleIntent {
            session_name: "hooks-test",
            agent_kind: rimz::ids::AgentKind::new_unchecked("antigravity"),
            event_name: "ReapedSuperseded",
            observation: &ended,
            spawned_subagents: &[],
        })
        .unwrap();
    assert!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .all(|state| state.agent_id != "late-child-reaped")
    );

    root["fullyIdle"] = serde_json::json!(true);
    root["terminationReason"] = serde_json::json!("model_stop");
    feed_antigravity(&store, &adapter, "Stop", root);

    let snapshot = store.snapshot_cached().unwrap();
    let child = snapshot
        .agents
        .iter()
        .find(|state| state.agent_id == "late-child-reaped")
        .unwrap();
    assert_eq!(child.parent_agent_id.as_deref(), Some("reaped-root"));
    assert_eq!(child.status, rimz::agents::AgentStatus::Success);
    assert_eq!(child.name.as_deref(), Some("reaped-name"));
    assert_eq!(child.role.as_deref(), Some("reaped-role"));
    assert_eq!(child.task.as_deref(), Some("reaped-role"));
}

#[test]
fn ambiguous_and_cyclic_antigravity_parent_candidates_stay_roots() {
    let (_dir, store) = hooks_test_store();
    let adapter = CorrelationTestAdapter::default();
    for root_id in ["root", "other-root"] {
        let mut payload = antigravity_payload(root_id);
        payload["invocationNum"] = serde_json::json!(0);
        feed_antigravity(&store, &adapter, "PreInvocation", payload);
    }
    seed_subagent_candidate(&store, "ambiguous-parent-a", "root");
    seed_subagent_candidate(&store, "ambiguous-parent-b", "root");
    let mut ambiguous = antigravity_payload("ambiguous-child");
    ambiguous["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", ambiguous);
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "ambiguous-child")
            .unwrap()
            .parent_agent_id,
        None
    );

    seed_subagent_candidate(&store, "cycle-parent", "cycle-child");
    let mut cyclic = antigravity_payload("cycle-child");
    cyclic["invocationNum"] = serde_json::json!(0);
    feed_antigravity(&store, &adapter, "PreInvocation", cyclic);
    assert_eq!(
        store
            .snapshot_cached()
            .unwrap()
            .agents
            .iter()
            .find(|state| state.agent_id == "cycle-child")
            .unwrap()
            .parent_agent_id,
        None
    );
}

#[test]
fn cursor_participant_start_path_selects_only_a_nonempty_absolute_project_dir() {
    use std::ffi::OsStr;

    assert_eq!(
        super::participant_start_path("cursor", Some(OsStr::new("/repo/worktree"))),
        std::path::PathBuf::from("/repo/worktree"),
    );
    for value in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("relative/path")),
    ] {
        assert_eq!(
            super::participant_start_path("cursor", value),
            std::path::PathBuf::from("."),
        );
    }
    assert_eq!(
        super::participant_start_path("claude", Some(OsStr::new("/repo/worktree"))),
        std::path::PathBuf::from("."),
    );
}

#[test]
fn recovered_binding_stamps_full_pane_and_reowns_to_in_pane_agent_process() {
    let mut observation = root_observation();
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);
    let child_pid = 5678;

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            Some(child_pid)
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    let owner = observation.runtime_owner.as_ref().expect("runtime owner");
    assert_eq!(owner.kind, RuntimeOwnerKind::Agent);
    assert_eq!(owner.pid, child_pid);
}

#[test]
fn recovered_binding_preserves_prior_owner_when_agent_process_is_unknown() {
    let mut observation = root_observation();
    let existing_owner = process_owner(RuntimeOwnerKind::Daemon, "sess-1", std::process::id());
    observation.runtime_owner = Some(existing_owner.clone());
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            None
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    assert_eq!(observation.runtime_owner.as_ref(), Some(&existing_owner));
}

#[test]
fn ingress_accepts_camelcase_field_and_dispatches_the_canonical_event() {
    let payload = serde_json::json!({
        "hookEventName": "session_start",
        "sessionId": "session-1"
    });
    let classified = rimz::agents::GrokAdapter
        .decode_hook("session_start", &payload)
        .unwrap();
    assert_eq!(classified.event_name(), "SessionStart");
    assert_eq!(classified.class(), rimz::agents::AgentHookClass::Lifecycle);

    let explicit = rimz::agents::GrokAdapter
        .decode_hook("post_tool_use", &payload)
        .unwrap();
    assert_eq!(explicit.event_name(), "PostToolUse");
}
