use serde_json::json;
use std::io::Write as _;

use super::*;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("test timestamp")
}

fn agent(path: &Path, id: &str, kind: &str, registered: i64) -> AgentState {
    serde_json::from_value(json!({
        "agent_id": id,
        "kind": kind,
        "status": "idle",
        "phase": "idle",
        "pane": null,
        "worktree_path": path,
        "worktree_branch": "feature",
        "task": null,
        "model": "model",
        "effort": "high",
        "last_seen": at(registered + 10),
        "last_activity": at(registered + 10),
        "registered_at": at(registered),
    }))
    .expect("test agent")
}

fn unbounded_lifetimes<'a>(records: impl IntoIterator<Item = &'a AgentState>) -> LaneLifetimes {
    LaneLifetimes::new(
        records
            .into_iter()
            .filter_map(|record| record.worktree_path.as_deref())
            .map(|path| (PathBuf::from(path), LaneLifetime::Unbounded))
            .collect(),
    )
}

#[test]
fn lane_lifetimes_admit_registration_at_or_after_marker_birth() {
    let lane = Path::new("/repo/lane");
    let mut agents = [
        agent(lane, "old", "claude", 19),
        agent(lane, "boundary", "claude", 20),
        agent(lane, "new", "claude", 21),
        agent(lane, "unknown", "claude", 22),
    ];
    for record in &mut agents {
        record.team = Some("forge".to_owned());
        record.role = Some("coder".to_owned());
        record.tool_calls.insert("exec".to_owned(), 1);
    }
    agents[3].registered_at = None;
    let refs = agents.iter().collect::<Vec<_>>();
    let lifetimes = LaneLifetimes::new(HashMap::from([(
        lane.to_owned(),
        LaneLifetime::Since(at(20)),
    )]));

    let groups = slot_groups(&refs, &lifetimes);

    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]
            .iter()
            .map(|record| &record.agent_id)
            .collect::<Vec<_>>(),
        [&agents[1].agent_id, &agents[2].agent_id]
    );
    let report = build_scoped(&agents, &[], &[], &lifetimes);
    assert_eq!(report.groups[0].members[0].sessions, 2);
    assert_eq!(report.totals.tool_calls, 2);
}

#[test]
fn lane_lifetimes_exclude_removed_and_unresolved_paths() {
    let record = agent(Path::new("/repo/lane"), "current", "claude", 21);
    for by_path in [
        HashMap::from([(PathBuf::from("/repo/lane"), LaneLifetime::Removed)]),
        HashMap::new(),
    ] {
        let lifetimes = LaneLifetimes::new(by_path);
        assert!(slot_groups(&[&record], &lifetimes).is_empty());
        assert_eq!(lifetimes.common_since(&[&record]), None);
    }
}

#[test]
fn lane_lifetimes_admit_unbounded_and_unstamped_records_without_registration() {
    let mut unbounded = agent(Path::new("/repo/lane"), "unbounded", "claude", 1);
    unbounded.registered_at = None;
    let mut unstamped = unbounded.clone();
    unstamped.agent_id = AgentSessionId::from("unstamped");
    unstamped.worktree_path = None;
    let lifetimes = LaneLifetimes::new(HashMap::from([(
        PathBuf::from("/repo/lane"),
        LaneLifetime::Unbounded,
    )]));

    assert_eq!(slot_groups(&[&unbounded, &unstamped], &lifetimes).len(), 2);
}

#[test]
fn slot_groups_admit_roots_and_children_by_their_own_lifetime() {
    let lane = PathBuf::from("/repo/lane");
    let sibling = PathBuf::from("/repo/sibling");
    let mut old = agent(&lane, "old", "claude", 19);
    old.team = Some("forge".to_owned());
    old.role = Some("coder".to_owned());
    old.tool_calls.insert("exec".to_owned(), 100);
    let mut current = agent(&lane, "current", "claude", 20);
    current.team = old.team.clone();
    current.role = old.role.clone();
    current.tool_calls.insert("exec".to_owned(), 1);
    let child = |path: &Path, id, registered, parent: &AgentState| {
        let mut child = agent(path, id, "claude", registered);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child
    };
    let old_child = child(&lane, "old-child", 19, &old);
    let stale_child = child(&lane, "stale-child", 19, &current);
    let current_child = child(&lane, "current-child", 21, &current);
    let sibling_child = child(&sibling, "sibling-child", 15, &current);
    let orphan = child(&sibling, "orphan", 15, &old);
    let agents = [
        old,
        current,
        old_child,
        stale_child,
        current_child,
        sibling_child,
        orphan,
    ];
    let refs = agents.iter().collect::<Vec<_>>();
    let lifetimes = LaneLifetimes::new(HashMap::from([
        (lane, LaneLifetime::Since(at(20))),
        (sibling, LaneLifetime::Since(at(10))),
    ]));

    let groups = slot_groups(&refs, &lifetimes);
    let current_group = groups
        .iter()
        .find(|group| {
            group
                .iter()
                .any(|record| record.agent_id == agents[1].agent_id)
        })
        .expect("current seat");
    assert_eq!(current_group.len(), 3);
    for index in [1, 4, 5] {
        assert!(
            current_group
                .iter()
                .any(|record| record.agent_id == agents[index].agent_id)
        );
    }
    assert_eq!(groups.len(), 2);
    let orphan_group = groups
        .iter()
        .find(|group| {
            group
                .iter()
                .any(|record| record.agent_id == agents[6].agent_id)
        })
        .expect("eligible orphan slot");
    assert_eq!(orphan_group.len(), 1);
    let report = build_scoped(&agents, &[], &[], &lifetimes);
    assert_eq!(report.totals.agents, 1);
    assert_eq!(report.totals.tool_calls, 1);
    assert_eq!(report.groups[0].members[0].sessions, 1);
}

#[test]
fn common_since_requires_one_managed_lane() {
    let lane = PathBuf::from("/repo/lane");
    let sibling = PathBuf::from("/repo/sibling");
    let first = agent(&lane, "first", "claude", 19);
    let second = agent(&lane, "second", "claude", 21);
    let other = agent(&sibling, "other", "claude", 21);
    let unmarked = agent(Path::new("/repo/unmarked"), "unmarked", "claude", 1);
    let mut unstamped = first.clone();
    unstamped.worktree_path = None;
    let removed = agent(Path::new("/repo/removed"), "removed", "claude", 21);
    let lifetimes = LaneLifetimes::new(HashMap::from([
        (lane, LaneLifetime::Since(at(20))),
        (sibling, LaneLifetime::Since(at(20))),
        (PathBuf::from("/repo/unmarked"), LaneLifetime::Unbounded),
        (PathBuf::from("/repo/removed"), LaneLifetime::Removed),
    ]));

    assert_eq!(lifetimes.common_since(&[&first]), Some(at(20)));
    assert_eq!(lifetimes.common_since(&[&first, &second]), Some(at(20)));
    assert_eq!(lifetimes.common_since(&[&first, &other]), None);
    assert_eq!(lifetimes.common_since(&[&first, &unmarked]), None);
    assert_eq!(lifetimes.common_since(&[&first, &unstamped]), None);
    assert_eq!(lifetimes.common_since(&[&unmarked]), None);
    assert_eq!(lifetimes.common_since(&[&unstamped]), None);
    assert_eq!(lifetimes.common_since(&[&removed]), None);
    assert_eq!(lifetimes.common_since(&[]), None);
}

fn build_for(agents: &[AgentState]) -> Attribution {
    build_with(agents, &[], &[])
}

fn build_with(
    agents: &[AgentState],
    subagents: &[AgentState],
    transcript: &[TranscriptEntry],
) -> Attribution {
    let lifetimes = unbounded_lifetimes(agents.iter().chain(subagents));
    build_scoped(agents, subagents, transcript, &lifetimes)
}

fn build_scoped(
    agents: &[AgentState],
    subagents: &[AgentState],
    transcript: &[TranscriptEntry],
    lifetimes: &LaneLifetimes,
) -> Attribution {
    let dir = tempfile::tempdir().expect("tempdir");
    let refs = agents.iter().collect::<Vec<_>>();
    let subagent_refs = subagents.iter().collect::<Vec<_>>();
    let active_secs = BTreeMap::new();
    let pricing_cache_path = dir.path().join("prices.json");
    build(AttributionRequest {
        agents: &refs,
        lifetimes,
        peers: &refs,
        subagents: &subagent_refs,
        transcript,
        me: None,
        active_secs: &active_secs,
        pricing_cache_path: &pricing_cache_path,
        require_contribution: false,
        scope: AttributionScope::default(),
        now: at(100),
    })
}

fn mixed_origin_report() -> Attribution {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("parent.jsonl");
    let subagent_dir = dir.path().join("parent/subagents");
    let launched_transcript = dir.path().join("launched.jsonl");
    std::fs::create_dir_all(&subagent_dir).expect("mkdir subagent dir");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":1.0,"requestId":"parent","message":{"id":"parent","model":"parent-model","usage":{"input_tokens":10,"output_tokens":1,"cache_creation_input_tokens":4,"cache_read_input_tokens":5}}}"#,
            "\n"
        ),
    )
    .expect("write parent transcript");
    std::fs::write(
        subagent_dir.join("agent-native.jsonl"),
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":2.0,"requestId":"native","isSidechain":true,"message":{"id":"native","model":"child-model","usage":{"input_tokens":20,"output_tokens":2,"cache_creation_input_tokens":6,"cache_read_input_tokens":7}}}"#,
            "\n"
        ),
    )
    .expect("write native transcript");
    std::fs::write(
        &launched_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:02.000Z","costUSD":3.0,"requestId":"launched","message":{"id":"launched","model":"child-model","usage":{"input_tokens":30,"output_tokens":3,"cache_creation_input_tokens":8,"cache_read_input_tokens":9}}}"#,
            "\n"
        ),
    )
    .expect("write launched transcript");

    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    parent.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let mut native = agent(Path::new("/repo/lane"), "native", "claude", 20);
    native.parent_agent_id = Some(parent.agent_id.clone());
    native.parent_agent_kind = Some(parent.kind.clone());
    native.task = Some("Explorer".to_owned());
    let mut launched = agent(Path::new("/repo/lane"), "launched", "claude", 30);
    launched.parent_agent_id = Some(parent.agent_id.clone());
    launched.parent_agent_kind = Some(parent.kind.clone());
    launched.launch_depth = Some(1);
    launched.profile = Some("explorer".to_owned());
    launched.transcript_path = Some(launched_transcript.to_string_lossy().into_owned());

    build_with(&[parent, launched], &[native], &[])
}

#[test]
fn folds_compaction_continuations_and_sums_rollup_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first_transcript = dir.path().join("one.jsonl");
    let second_transcript = dir.path().join("two.jsonl");
    std::fs::write(
        &first_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":1.0,"requestId":"one","message":{"id":"one","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &second_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":2.0,"requestId":"two","message":{"id":"two","usage":{"input_tokens":20,"output_tokens":2}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let pane = crate::pane::PaneRef::from_id(crate::ids::PaneId::from_parts(
        crate::ids::MuxName::Tmux,
        "%3",
    ));
    let mut first = agent(Path::new("/repo/lane"), "one", "claude", 10);
    first.team = Some("forge".to_owned());
    first.role = Some("coder".to_owned());
    first.launch_ordinal = Some(1);
    first.ended_at = Some(at(20));
    first.tool_calls.insert("exec".to_owned(), 2);
    first.compaction_count = 1;
    first.launch_id = Some(AgentSessionId::from("launch_coder"));
    first.pane = Some(pane.clone());
    first.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "one",
        42,
        Some("agent-start".to_owned()),
    ));
    first.transcript_path = Some(first_transcript.to_string_lossy().into_owned());
    let mut second = agent(Path::new("/repo/lane"), "two", "claude", 30);
    second.team = Some("forge".to_owned());
    second.role = Some("coder".to_owned());
    second.launch_ordinal = Some(1);
    second.tool_calls.insert("exec".to_owned(), 3);
    second.compaction_count = 2;
    second.launch_id = Some(AgentSessionId::from("launch_coder"));
    second.pane = Some(pane);
    second.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "two",
        42,
        Some("agent-start".to_owned()),
    ));
    second.transcript_path = Some(second_transcript.to_string_lossy().into_owned());

    let report = build_for(&[first, second]);
    let member = &report.groups[0].members[0];
    assert_eq!(member.handle, "@coder");
    assert_eq!(member.sessions, 2);
    assert_eq!(member.tool_calls, 5);
    assert_eq!(member.compactions, 3);
    assert_eq!(member.tokens.input, 30);
    assert_eq!(member.tokens.output, 3);
    assert_eq!(member.cost_usd, Some(3.0));
    assert_eq!(member.presence, Presence::Live);
}

#[test]
fn resumed_slot_deduplicates_replayed_transcript_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut agents = Vec::new();
    for (id, filename) in [("one", "one.jsonl"), ("two", "two.jsonl")] {
        let path = dir.path().join(filename);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","payload":{{"id":"{id}"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"model":"gpt-5.6-sol","timestamp":"2026-01-01T10:00:00.000Z","usage":{{"input_tokens":100,"output_tokens":20}}}}"#
        )
        .unwrap();
        let mut state = agent(
            Path::new("/repo/lane"),
            id,
            "codex",
            if id == "one" { 10 } else { 30 },
        );
        state.team = Some("forge".to_owned());
        state.role = Some("coder".to_owned());
        state.transcript_path = Some(path.to_string_lossy().into_owned());
        agents.push(state);
    }

    let report = build_for(&agents);
    let member = &report.groups[0].members[0];

    assert_eq!(member.sessions, 2);
    assert_eq!(member.tokens.input, 100);
    assert_eq!(member.tokens.output, 20);
}

#[test]
fn launched_child_attribution_corroborates_missing_parent_kind() {
    let wrong = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    let mut parent = agent(Path::new("/repo/lane"), "current", "codex", 20);
    parent.launch_id = Some(AgentSessionId::from("parent"));
    let mut child = agent(Path::new("/repo/lane"), "child", "codex", 30);
    child.parent_agent_id = Some(AgentSessionId::from("parent"));
    child.launch_depth = Some(1);

    let refs = [&wrong, &parent, &child];
    let lifetimes = unbounded_lifetimes(refs);
    let groups = slot_groups(&refs, &lifetimes);
    let group = groups
        .iter()
        .find(|records| {
            records
                .iter()
                .any(|record| record.agent_id == child.agent_id)
        })
        .unwrap();
    assert_eq!(group.len(), 2);
    assert!(
        group
            .iter()
            .any(|record| record.agent_id == parent.agent_id)
    );
    assert!(!group.iter().any(|record| record.kind == wrong.kind));
}

#[test]
fn launched_child_continuations_deduplicate_as_one_subagent_seat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 5);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    parent.tool_calls.insert("exec".to_owned(), 1);
    let pane = crate::pane::PaneRef::from_id(crate::ids::PaneId::from_parts(
        crate::ids::MuxName::Tmux,
        "%3",
    ));
    let mut children = Vec::new();
    for (id, filename, registered) in [
        ("child-one", "child-one.jsonl", 10),
        ("child-two", "child-two.jsonl", 30),
    ] {
        let path = dir.path().join(filename);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":2.0,"requestId":"request","message":{{"id":"message","usage":{{"input_tokens":20,"output_tokens":2}}}}}}"#
        )
        .unwrap();
        let mut child = agent(Path::new("/repo/lane"), id, "claude", registered);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.profile = Some("explorer".to_owned());
        child.pane = Some(pane.clone());
        child.transcript_path = Some(path.to_string_lossy().into_owned());
        children.push(child);
    }
    let agents = [parent, children.remove(0), children.remove(0)];

    let report = build_for(&agents);
    let member = &report.groups[0].members[0];

    assert_eq!(member.tokens.input, 20);
    assert_eq!(member.tokens.output, 2);
    assert_eq!(member.cost_usd, Some(2.0));
    assert_eq!(member.sessions, 1);
    assert_eq!(
        member.subagents,
        vec![SubagentStat {
            task: Some("explorer".to_owned()),
            count: 1,
            cost_usd: Some(2.0),
        }]
    );
}

#[test]
fn claude_slot_credits_subagent_transcript_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("claude-session.jsonl");
    let subagents = dir.path().join("claude-session/subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":1.0,"requestId":"main","message":{"id":"main","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        subagents.join("agent-child.jsonl"),
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":2.0,"requestId":"child","isSidechain":true,"message":{"id":"child","model":"child-model","usage":{"input_tokens":20,"output_tokens":2}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut state = agent(Path::new("/repo/lane"), "claude-session", "claude", 10);
    state.team = Some("forge".to_owned());
    state.role = Some("planner".to_owned());
    state.transcript_path = Some(transcript.to_string_lossy().into_owned());

    let report = build_for(&[state]);
    let member = &report.groups[0].members[0];

    assert_eq!(member.tokens.input, 30);
    assert_eq!(member.tokens.output, 3);
    assert_eq!(member.cost_usd, Some(3.0));
    assert_eq!(
        member.models,
        [
            ModelStat {
                model: Some("child-model".to_owned()),
                tokens: TokenSplit {
                    input: 20,
                    output: 2,
                    ..TokenSplit::default()
                },
                cost_usd: Some(2.0),
            },
            ModelStat {
                model: None,
                tokens: TokenSplit {
                    input: 10,
                    output: 1,
                    ..TokenSplit::default()
                },
                cost_usd: Some(1.0),
            },
        ]
    );
    assert_eq!(report.models, member.models);
}

#[test]
fn launched_child_effort_folds_into_the_parent_seat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent_transcript = dir.path().join("parent.jsonl");
    let child_transcript = dir.path().join("child.jsonl");
    std::fs::write(
        &parent_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":1.0,"requestId":"parent","message":{"id":"parent","model":"parent-model","usage":{"input_tokens":10,"output_tokens":1}}}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &child_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":2.0,"requestId":"child","message":{"id":"child","model":"child-model","usage":{"input_tokens":20,"output_tokens":2}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    parent.transcript_path = Some(parent_transcript.to_string_lossy().into_owned());
    parent.tool_calls.insert("exec".to_owned(), 1);
    let mut child = agent(Path::new("/repo/lane"), "child", "claude", 30);
    child.name = Some("helper".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.profile = Some("explorer".to_owned());
    child.transcript_path = Some(child_transcript.to_string_lossy().into_owned());
    child.tool_calls.insert("exec".to_owned(), 5);
    child.compaction_count = 1;

    let parent_last_activity = parent.last_activity;
    let transcript_entries = [
        TranscriptEntry::new(
            at(50),
            child.kind.clone(),
            child.agent_id.clone(),
            TranscriptKind::Prompt,
            String::new(),
        ),
        TranscriptEntry::new(
            at(51),
            child.kind.clone(),
            child.agent_id.clone(),
            TranscriptKind::Message,
            String::new(),
        ),
        TranscriptEntry::new(
            at(52),
            child.kind.clone(),
            child.agent_id.clone(),
            TranscriptKind::Ask,
            String::new(),
        ),
    ];
    let agents = [parent, child];
    let refs = agents.iter().collect::<Vec<_>>();
    let lifetimes = unbounded_lifetimes(agents.iter());
    let active_secs = BTreeMap::from([((agents[0].kind.clone(), agents[0].agent_id.clone()), 10)]);
    let pricing_cache_path = dir.path().join("prices.json");
    let report = build(AttributionRequest {
        agents: &refs,
        lifetimes: &lifetimes,
        peers: &refs,
        subagents: &[],
        transcript: &transcript_entries,
        me: None,
        active_secs: &active_secs,
        pricing_cache_path: &pricing_cache_path,
        require_contribution: false,
        scope: AttributionScope::default(),
        now: at(100),
    });
    let member = &report.groups[0].members[0];

    assert_eq!(report.totals.agents, 1);
    assert_eq!(member.handle, "@planner");
    assert_eq!(member.sessions, 1);
    assert_eq!(member.cost_usd, Some(3.0));
    assert_eq!(member.tokens.input, 30);
    assert_eq!(member.tokens.output, 3);
    assert_eq!(member.tool_calls, 1);
    assert_eq!(member.compactions, 0);
    assert_eq!(member.last_activity, parent_last_activity);
    assert_eq!(member.active_secs, Some(10));
    assert_eq!(member.asks, 0);
    assert_eq!(member.messages, MessageCounts::default());
    assert_eq!(
        member.subagents,
        vec![SubagentStat {
            task: Some("explorer".to_owned()),
            count: 1,
            cost_usd: Some(2.0),
        }]
    );
    assert_eq!(report.totals.cost_usd, Some(3.0));
    assert_eq!(report.totals.tokens.input, 30);
    assert_eq!(report.totals.tokens.output, 3);
    assert_eq!(report.groups[0].totals.tool_calls, 1);
    assert_eq!(report.totals.active_secs, Some(10));
    assert_eq!(report.totals.asks, 0);
    assert_eq!(report.totals.messages, MessageCounts::default());
    assert_eq!(
        member.models,
        [
            ModelStat {
                model: Some("child-model".to_owned()),
                tokens: TokenSplit {
                    input: 20,
                    output: 2,
                    ..TokenSplit::default()
                },
                cost_usd: Some(2.0),
            },
            ModelStat {
                model: Some("parent-model".to_owned()),
                tokens: TokenSplit {
                    input: 10,
                    output: 1,
                    ..TokenSplit::default()
                },
                cost_usd: Some(1.0),
            },
        ]
    );
    assert_eq!(report.models, member.models);
}

#[test]
fn subagents_with_one_label_merge_across_origins() {
    let report = mixed_origin_report();
    let group = &report.groups[0];
    let member = &group.members[0];

    assert_eq!(member.cost_usd, Some(6.0));
    assert_eq!(member.tokens.input, 60);
    assert_eq!(member.tokens.output, 6);
    assert_eq!(
        member.subagents,
        [SubagentStat {
            task: Some("explorer".to_owned()),
            count: 2,
            cost_usd: Some(5.0),
        }]
    );
    assert_eq!(group.totals.cost_usd, Some(6.0));
    assert_eq!(report.totals.cost_usd, Some(6.0));
}

#[test]
fn schema_seven_serializes_one_cost_and_task_grouped_subagents() {
    let report = serde_json::to_value(mixed_origin_report()).expect("serialize attribution");
    let member = &report["groups"][0]["members"][0];
    let subagent = &member["subagents"][0];

    assert_eq!(report["schema"], 7);
    assert_eq!(report["scope"].get("since"), Some(&serde_json::Value::Null));
    assert_eq!(member["cost_usd"], 6.0);
    assert_eq!(member.as_object().map(serde_json::Map::len), Some(23));
    assert_eq!(
        report["models"],
        json!([
            {
                "model": "child-model",
                "tokens": {"input": 50, "output": 5, "cache_write": 14, "cache_read": 16},
                "cost_usd": 5.0,
            },
            {
                "model": "parent-model",
                "tokens": {"input": 10, "output": 1, "cache_write": 4, "cache_read": 5},
                "cost_usd": 1.0,
            },
        ])
    );
    assert_eq!(member["models"], report["models"]);
    for (models, totals) in [
        (&member["models"], member),
        (&report["models"], &report["totals"]),
    ] {
        let models = models.as_array().expect("model rows");
        assert_eq!(
            models
                .iter()
                .map(|row| row["cost_usd"].as_f64().expect("model cost"))
                .sum::<f64>(),
            totals["cost_usd"].as_f64().expect("total cost")
        );
        for component in ["input", "output", "cache_write", "cache_read"] {
            assert_eq!(
                models
                    .iter()
                    .map(|row| row["tokens"][component].as_u64().expect("model tokens"))
                    .sum::<u64>(),
                totals["tokens"][component].as_u64().expect("total tokens")
            );
        }
    }
    assert_eq!(
        subagent,
        &json!({
            "task": "explorer",
            "count": 2,
            "cost_usd": 5.0,
        })
    );
    assert_eq!(
        report["totals"].as_object().map(serde_json::Map::len),
        Some(10)
    );
}

#[test]
fn model_stats_orders_by_cost_tokens_and_model_id_with_unknowns_last() {
    let rows = model_stats(
        [
            (Some("expensive"), Some(2.0), 0, 0),
            (Some("cached"), Some(1.0), 0, 11),
            (Some("alpha"), Some(1.0), 10, 0),
            (Some("beta"), Some(1.0), 10, 0),
            (None, Some(1.0), 10, 0),
            (Some("unpriced"), None, 100, 0),
        ]
        .into_iter()
        .map(|(model, cost_usd, input, cache_read)| {
            (
                model.map(ToOwned::to_owned),
                spending::SlotEffort {
                    tokens: TokenSplit {
                        input,
                        cache_read,
                        ..TokenSplit::default()
                    },
                    cost_usd,
                },
            )
        })
        .collect(),
    );

    assert_eq!(
        rows.iter()
            .map(|row| row.model.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("expensive"),
            Some("cached"),
            Some("alpha"),
            Some("beta"),
            None,
            Some("unpriced"),
        ]
    );
    assert_eq!(rows.last().expect("unpriced row").cost_usd, None);
    assert_eq!(rows.last().expect("unpriced row").tokens.input, 100);
}

#[test]
fn model_stats_merges_empty_ids_and_omits_rows_without_effort() {
    let rows = model_stats(
        [
            (Some(""), None, 10),
            (Some(" \t"), None, 5),
            (None, Some(0.25), 20),
            (Some("unused"), None, 0),
            (Some("unpriced"), None, 5),
            (Some("cost-only"), Some(0.5), 0),
        ]
        .into_iter()
        .map(|(model, cost_usd, input)| {
            (
                model.map(ToOwned::to_owned),
                spending::SlotEffort {
                    tokens: TokenSplit {
                        input,
                        ..TokenSplit::default()
                    },
                    cost_usd,
                },
            )
        })
        .collect(),
    );

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].model.as_deref(), Some("cost-only"));
    assert_eq!(rows[0].cost_usd, Some(0.5));
    assert_eq!(rows[0].tokens, TokenSplit::default());
    assert_eq!(rows[1].model, None);
    assert_eq!(rows[1].cost_usd, Some(0.25));
    assert_eq!(rows[1].tokens.input, 35);
    assert_eq!(rows[2].model.as_deref(), Some("unpriced"));
    assert_eq!(rows[2].cost_usd, None);
    assert_eq!(rows[2].tokens.input, 5);
    assert!(
        model_stats(BTreeMap::from([
            (None, spending::SlotEffort::default()),
            (Some(String::new()), spending::SlotEffort::default()),
        ]))
        .is_empty()
    );
}

#[test]
fn launched_only_delegation_keeps_the_member_with_all_in_cost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let child_transcript = dir.path().join("child.jsonl");
    std::fs::write(
        &child_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":3.0,"requestId":"child","message":{"id":"child","usage":{"input_tokens":30,"output_tokens":3}}}"#,
            "\n"
        ),
    )
    .expect("write child transcript");
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    let mut child = agent(Path::new("/repo/lane"), "child", "claude", 30);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.profile = Some("explorer".to_owned());
    child.transcript_path = Some(child_transcript.to_string_lossy().into_owned());
    let agents = [parent, child];
    let refs = agents.iter().collect::<Vec<_>>();
    let lifetimes = unbounded_lifetimes(agents.iter());
    let active_secs = BTreeMap::new();
    let pricing_cache_path = dir.path().join("prices.json");

    let report = build(AttributionRequest {
        agents: &refs,
        lifetimes: &lifetimes,
        peers: &refs,
        subagents: &[],
        transcript: &[],
        me: None,
        active_secs: &active_secs,
        pricing_cache_path: &pricing_cache_path,
        require_contribution: true,
        scope: AttributionScope::default(),
        now: at(100),
    });
    let member = &report.groups[0].members[0];

    assert_eq!(member.cost_usd, Some(3.0));
    assert_eq!(member.tokens.input, 30);
    assert_eq!(member.tokens.output, 3);
    assert_eq!(report.groups[0].totals.cost_usd, Some(3.0));
    assert_eq!(report.totals.cost_usd, Some(3.0));
}

#[test]
fn launched_child_in_another_lane_follows_its_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let child_transcript = dir.path().join("child.jsonl");
    std::fs::write(
        &child_transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":3.0,"requestId":"child","message":{"id":"child","usage":{"input_tokens":30,"output_tokens":3}}}"#,
            "\n"
        ),
    )
    .expect("write child transcript");
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.channel = Some("feature".to_owned());
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    let mut child = agent(Path::new("/repo/lane"), "child", "claude", 30);
    child.channel = Some("sibling".to_owned());
    child.worktree_path = Some("/repo/sibling".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.profile = Some("explorer".to_owned());
    child.transcript_path = Some(child_transcript.to_string_lossy().into_owned());

    let report = build_for(&[parent, child]);

    assert_eq!(report.totals.agents, 1);
    assert_eq!(report.totals.cost_usd, Some(3.0));
    assert_eq!(report.groups[0].members[0].subagents[0].count, 1);
}

#[test]
fn same_named_children_of_different_parents_stay_with_their_launcher() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut parents = Vec::new();
    let mut children = Vec::new();
    for (ordinal, role, child_id, cost) in [
        (0, "planner", "planner-audit", 1.0),
        (1, "coder", "coder-audit", 2.0),
    ] {
        let mut parent = agent(
            Path::new("/repo/lane"),
            role,
            "claude",
            10 + i64::from(ordinal),
        );
        parent.team = Some("forge".to_owned());
        parent.role = Some(role.to_owned());
        parent.launch_ordinal = Some(ordinal);
        let transcript = dir.path().join(format!("{child_id}.jsonl"));
        std::fs::write(
            &transcript,
            format!(
                "{{\"timestamp\":\"2026-01-01T10:00:01.000Z\",\"costUSD\":{cost},\"requestId\":\"{child_id}\",\"message\":{{\"id\":\"{child_id}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":1}}}}}}\n"
            ),
        )
        .expect("write child transcript");
        let mut child = agent(
            Path::new("/repo/lane"),
            child_id,
            "claude",
            30 + i64::from(ordinal),
        );
        child.name = Some("audit".to_owned());
        child.name_explicit = true;
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.profile = Some("reviewer".to_owned());
        child.transcript_path = Some(transcript.to_string_lossy().into_owned());
        parents.push(parent);
        children.push(child);
    }
    let agents = parents.into_iter().chain(children).collect::<Vec<_>>();

    let report = build_for(&agents);
    let members = &report.groups[0].members;
    let planner = members
        .iter()
        .find(|member| member.role.as_deref() == Some("planner"))
        .expect("planner member");
    let coder = members
        .iter()
        .find(|member| member.role.as_deref() == Some("coder"))
        .expect("coder member");

    assert_eq!(planner.cost_usd, Some(1.0));
    assert_eq!(coder.cost_usd, Some(2.0));
    assert_eq!(planner.subagents[0].count, 1);
    assert_eq!(planner.subagents[0].cost_usd, Some(1.0));
    assert_eq!(coder.subagents[0].count, 1);
    assert_eq!(coder.subagents[0].cost_usd, Some(2.0));
    assert_eq!(report.totals.cost_usd, Some(3.0));
}

#[test]
fn all_in_cost_sums_child_slots_in_stable_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    let mut agents = vec![parent.clone()];
    for (ordinal, child_id, cost) in [
        (0, "child-a", 0.1_f64),
        (1, "child-b", 0.2_f64),
        (2, "child-c", 0.3_f64),
    ] {
        let transcript = dir.path().join(format!("{child_id}.jsonl"));
        std::fs::write(
            &transcript,
            format!(
                "{{\"timestamp\":\"2026-01-01T10:00:01.000Z\",\"costUSD\":{cost},\"requestId\":\"{child_id}\",\"message\":{{\"id\":\"{child_id}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":1}}}}}}\n"
            ),
        )
        .expect("write child transcript");
        let mut child = agent(Path::new("/repo/lane"), child_id, "claude", 30 + ordinal);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.profile = Some("reviewer".to_owned());
        child.transcript_path = Some(transcript.to_string_lossy().into_owned());
        agents.push(child);
    }

    let report = build_for(&agents);
    let member = &report.groups[0].members[0];
    let expected = 0.1_f64 + 0.2_f64 + 0.3_f64;

    assert_eq!(member.cost_usd, Some(expected));
    assert_eq!(member.subagents[0].cost_usd, Some(expected));
}

#[test]
fn launched_child_without_a_retained_parent_is_not_a_member() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("child.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"timestamp":"2026-01-01T10:00:01.000Z","costUSD":2.0,"requestId":"child","message":{"id":"child","usage":{"input_tokens":20,"output_tokens":2}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut child = agent(Path::new("/repo/lane"), "child", "claude", 30);
    child.parent_agent_id = Some(AgentSessionId::from("missing"));
    child.launch_depth = Some(1);
    child.transcript_path = Some(transcript.to_string_lossy().into_owned());
    child.tool_calls.insert("exec".to_owned(), 1);

    let report = build_for(&[child]);

    assert_eq!(report.totals.agents, 0);
    assert!(report.groups.is_empty());
    assert_eq!(report.totals.cost_usd, None);
}

#[test]
fn team_roles_and_provider_kinds_keep_distinct_slots() {
    let mut planner = agent(Path::new("/repo/lane"), "planner", "claude", 10);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.launch_ordinal = Some(0);
    planner.tool_calls.insert("read".to_owned(), 1);
    let mut coder = agent(Path::new("/repo/lane"), "coder", "codex", 11);
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    coder.launch_ordinal = Some(1);
    coder.tool_calls.insert("exec".to_owned(), 1);
    let mut other_kind = coder.clone();
    other_kind.agent_id = "claude-coder".into();
    other_kind.kind = AgentKind::new_unchecked("claude");

    let report = build_for(&[planner, coder, other_kind]);
    assert_eq!(report.groups[0].members.len(), 3);
}

#[test]
fn identical_team_roles_in_different_lanes_keep_distinct_slots() {
    let mut auth = agent(Path::new("/repo/lane"), "auth-coder", "codex", 10);
    auth.team = Some("forge".to_owned());
    auth.role = Some("coder".to_owned());
    auth.channel = Some("auth".to_owned());
    auth.tool_calls.insert("exec".to_owned(), 1);
    let mut docs = agent(Path::new("/repo/lane"), "docs-coder", "codex", 20);
    docs.team = Some("forge".to_owned());
    docs.role = Some("coder".to_owned());
    docs.channel = Some("docs".to_owned());
    docs.tool_calls.insert("exec".to_owned(), 1);

    let report = build_for(&[auth, docs]);
    assert_eq!(report.groups[0].members.len(), 2);
    assert_eq!(report.groups[0].totals.agents, 2);
    assert!(
        report.groups[0]
            .members
            .iter()
            .all(|member| member.sessions == 1)
    );
}

#[test]
fn roleless_cohorts_and_reused_panes_fold() {
    let mut first = agent(Path::new("/repo/lane"), "one", "codex", 10);
    first.launch_group = Some("group".to_owned());
    first.launch_ordinal = Some(0);
    first.tool_calls.insert("exec".to_owned(), 1);
    let mut continuation = agent(Path::new("/repo/lane"), "two", "codex", 20);
    continuation.launch_group = Some("group".to_owned());
    continuation.launch_ordinal = Some(0);
    let mut pane_first = agent(Path::new("/repo/lane"), "pane-one", "claude", 30);
    pane_first.pane = Some(crate::pane::PaneRef::from_id(
        crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
    ));
    pane_first.tool_calls.insert("read".to_owned(), 1);
    let mut pane_second = agent(Path::new("/repo/lane"), "pane-two", "claude", 40);
    pane_second.pane = pane_first.pane.clone();

    let report = build_for(&[first, continuation, pane_first, pane_second]);
    let sessions = report.groups[0]
        .members
        .iter()
        .map(|member| member.sessions)
        .collect::<Vec<_>>();
    assert_eq!(sessions, [2, 2]);
}

#[test]
fn exited_presence_wall_clock_and_teamless_order_are_honest() {
    let mut team = agent(Path::new("/repo/lane"), "team", "codex", 10);
    team.team = Some("forge".to_owned());
    team.role = Some("coder".to_owned());
    team.ended_at = Some(at(20));
    team.tool_calls.insert("exec".to_owned(), 1);
    let mut stray = agent(Path::new("/repo/lane"), "stray", "claude", 30);
    stray.tool_calls.insert("read".to_owned(), 1);

    let report = build_for(&[team, stray]);
    assert_eq!(report.groups.len(), 2);
    assert_eq!(report.groups[0].team.as_ref().unwrap().name, "forge");
    assert!(report.groups[1].team.is_none());
    assert_eq!(report.groups[0].members[0].presence, Presence::Exited);
    assert_eq!(report.totals.wall_clock_secs, 30);
    assert_eq!(report.totals.cost_usd, None);
}

#[test]
fn agents_without_a_recorded_contribution_are_omitted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut idle = agent(Path::new("/repo/lane"), "idle", "claude", 10);
    idle.team = Some("idle-team".to_owned());
    idle.role = Some("planner".to_owned());
    let mut contributor = agent(Path::new("/repo/lane"), "contributor", "codex", 20);
    contributor.team = Some("forge".to_owned());
    contributor.role = Some("coder".to_owned());
    contributor.tool_calls.insert("exec".to_owned(), 1);
    let mut turn_contributor = agent(
        Path::new("/repo/lane"),
        "turn-contributor",
        "antigravity",
        30,
    );
    turn_contributor.team = Some("durable".to_owned());
    turn_contributor.role = Some("reviewer".to_owned());
    turn_contributor.turn_started_at = Some(at(35));

    let agents = [idle, contributor, turn_contributor];
    let report = build_for(&agents);

    assert_eq!(report.totals.agents, 2);
    assert_eq!(report.groups.len(), 2);
    assert!(
        report
            .groups
            .iter()
            .filter_map(|group| group.team.as_ref())
            .all(|team| team.name != "idle-team")
    );
    let members = report
        .groups
        .iter()
        .flat_map(|group| group.members.iter())
        .collect::<Vec<_>>();
    assert!(
        members
            .iter()
            .any(|member| member.handle == "@coder" && member.active_secs.is_none())
    );
    let durable = members
        .iter()
        .find(|member| member.handle == "@reviewer")
        .expect("turn contributor");
    assert_eq!(durable.active_secs, None);
    assert_eq!(durable.tool_calls, 0);
    assert_eq!(durable.compactions, 0);
    assert_eq!(durable.tokens, TokenSplit::default());
    assert_eq!(durable.cost_usd, None);

    let refs = agents.iter().collect::<Vec<_>>();
    let lifetimes = unbounded_lifetimes(agents.iter());
    let active_secs = BTreeMap::new();
    let pricing_cache_path = dir.path().join("prices.json");
    let report = build(AttributionRequest {
        agents: &refs,
        lifetimes: &lifetimes,
        peers: &refs,
        subagents: &[],
        transcript: &[],
        me: None,
        active_secs: &active_secs,
        pricing_cache_path: &pricing_cache_path,
        require_contribution: true,
        scope: AttributionScope::default(),
        now: at(100),
    });

    assert_eq!(report.totals.agents, 1);
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].team.as_ref().unwrap().name, "forge");
}

#[test]
fn transcript_counts_messages_asks_and_sent_credit_per_slot() {
    let mut planner = agent(Path::new("/repo/lane"), "planner-session", "claude", 10);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.channel = Some("feature".to_owned());
    let mut coder = agent(Path::new("/repo/lane"), "coder-session", "codex", 20);
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    coder.channel = Some("feature".to_owned());
    let mut docs_coder = agent(Path::new("/repo/lane"), "docs-coder", "codex", 30);
    docs_coder.team = Some("forge".to_owned());
    docs_coder.role = Some("coder".to_owned());
    docs_coder.channel = Some("docs".to_owned());

    let entry = |kind: &str, id: &str, entry| {
        TranscriptEntry::new(
            at(50),
            AgentKind::new_unchecked(kind),
            AgentSessionId::from(id),
            entry,
            String::new(),
        )
    };
    let mut received = entry("codex", "coder-session", TranscriptKind::Message);
    received.channel = Some("feature".to_owned());
    received.from = Some("@planner".to_owned());
    let mut cross_lane = entry("codex", "docs-coder", TranscriptKind::Message);
    cross_lane.channel = Some("docs".to_owned());
    cross_lane.from = Some("@planner#feature".to_owned());
    let mut system_nudge = entry("claude", "planner-session", TranscriptKind::Prompt);
    system_nudge.from = Some("rimz".to_owned());
    let ask_id = crate::ids::AskId::parse("ask_0123456789abcdef").expect("ask id");
    let mut ask = entry("claude", "planner-session", TranscriptKind::Ask);
    ask.id = Some(ask_id.clone());
    let mut answer = entry("claude", "planner-session", TranscriptKind::Answer);
    answer.id = Some(ask_id);
    let mut unrelated_answer = entry("claude", "planner-session", TranscriptKind::Answer);
    unrelated_answer.id =
        Some(crate::ids::AskId::parse("ask_0123456789abcdee").expect("unrelated ask id"));
    let transcript = vec![
        entry("claude", "planner-session", TranscriptKind::Prompt),
        ask,
        answer,
        unrelated_answer,
        received,
        cross_lane,
        system_nudge,
    ];

    let report = build_with(&[planner, coder, docs_coder], &[], &transcript);
    let members = report.groups[0].members.iter().collect::<Vec<_>>();
    let planner = members
        .iter()
        .find(|member| member.handle == "@planner")
        .expect("planner");
    assert_eq!(planner.asks, 1);
    assert_eq!(planner.asks_answered, 1);
    assert_eq!(
        planner.messages,
        MessageCounts {
            from_user: 1,
            from_teammates: 0,
            to_teammates: 2,
        }
    );
    assert_eq!(report.totals.asks, 1);
    assert_eq!(report.totals.asks_answered, 1);
    assert_eq!(report.totals.messages.from_user, 1);
    assert_eq!(report.totals.messages.from_teammates, 2);
}

#[test]
fn subagents_group_by_task_and_join_durable_child_cost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let transcript = dir.path().join("parent.jsonl");
    let subagent_dir = dir.path().join("parent/subagents");
    std::fs::create_dir_all(&subagent_dir).unwrap();
    std::fs::write(&transcript, "").unwrap();
    for (child, cost) in [
        ("explore-one", 1.25),
        ("explore-two", 2.0),
        ("untyped", 0.5),
    ] {
        std::fs::write(
            subagent_dir.join(format!("agent-{child}.jsonl")),
            format!(
                "{{\"timestamp\":\"2026-01-01T10:00:01.000Z\",\"costUSD\":{cost},\"requestId\":\"{child}\",\"isSidechain\":true,\"message\":{{\"id\":\"{child}\",\"model\":\"child-model\",\"usage\":{{\"input_tokens\":20,\"output_tokens\":2}}}}}}\n"
            ),
        )
        .unwrap();
    }
    let mut parent = agent(Path::new("/repo/lane"), "parent", "claude", 10);
    parent.team = Some("forge".to_owned());
    parent.role = Some("planner".to_owned());
    parent.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let mut explore_one = agent(Path::new("/repo/lane"), "explore-one", "claude", 20);
    explore_one.parent_agent_id = Some(AgentSessionId::from("parent"));
    explore_one.task = Some("Explore".to_owned());
    let mut explore_two = agent(Path::new("/repo/lane"), "explore-two", "claude", 30);
    explore_two.parent_agent_id = Some(AgentSessionId::from("parent"));
    explore_two.task = Some("Explore".to_owned());
    let mut described = agent(Path::new("/repo/lane"), "described", "claude", 40);
    described.parent_agent_id = Some(AgentSessionId::from("parent"));
    described.task = Some("Inspect every parser call site".to_owned());

    let report = build_with(&[parent], &[explore_one, explore_two, described], &[]);
    let stats = &report.groups[0].members[0].subagents;

    assert_eq!(
        stats,
        &[
            SubagentStat {
                task: Some("explore".to_owned()),
                count: 2,
                cost_usd: Some(3.25),
            },
            SubagentStat {
                task: None,
                count: 2,
                cost_usd: Some(0.5),
            },
        ]
    );
}

#[test]
fn subagent_type_rejects_descriptions_and_unbounded_labels() {
    assert_eq!(subagent_type(Some("Explore")), Some("explore".to_owned()));
    assert_eq!(
        subagent_type(Some("general-purpose")),
        Some("general-purpose".to_owned())
    );
    assert_eq!(subagent_type(Some("Inspect auth retries")), None);
    assert_eq!(subagent_type(Some(&"x".repeat(25))), None);
}

#[test]
fn sent_messages_alone_are_a_contribution() {
    let idle = agent(Path::new("/repo/lane"), "idle", "claude", 10);
    let mut sent = TranscriptEntry::new(
        at(50),
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("receiver"),
        TranscriptKind::Message,
        "hello".to_owned(),
    );
    sent.channel = Some("lane".to_owned());
    sent.from = Some("@claude".to_owned());

    let report = build_with(&[idle], &[], &[sent]);

    assert_eq!(report.totals.agents, 1);
    assert_eq!(report.groups[0].members[0].messages.to_teammates, 1);
}
