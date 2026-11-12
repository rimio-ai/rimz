use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::StatePaths;
use crate::disk::parse_cache::KeyedParseCache;
use crate::harness::workflow::{Definition, hops, reached};
use crate::store::snapshot::{
    SidebarSnapshot, SidebarWorkflow, SidebarWorkflowNode, WorkflowNodeKind,
};
use crate::store::workflow::{JournalEntry, RunOrigin, RunRecord, RunState};

thread_local! {
    // Sealed specs are immutable; the shared cache bounds retained paths to 1,024.
    static SPECS: KeyedParseCache<Definition> = KeyedParseCache::default();
    // Apply the current held verdict after reading the cached journal.
    static JOURNALS: KeyedParseCache<Vec<JournalEntry>> = KeyedParseCache::default();
}

/// Strips for live deployed runs; a loop task's own run shows in `rimz loop list` instead.
pub(super) fn seeds(records: &[RunRecord], state: &StatePaths) -> Vec<SidebarWorkflow> {
    records
        .iter()
        .filter(|record| !record.state.is_terminal() && record.origin == RunOrigin::Deployed)
        .filter_map(|record| {
            let paths = state.workflow(&record.id);
            let spec = SPECS.with(|cache| {
                cache
                    .read_with(&paths.spec, |bytes| {
                        crate::harness::workflow::parse(std::str::from_utf8(bytes).ok()?).ok()
                    })
                    .ok()
                    .flatten()
            });
            let reached_nodes = spec.as_deref().and_then(|spec| {
                JOURNALS
                    .with(|cache| {
                        cache
                            .read_with(&paths.journal, |bytes| {
                                Some(crate::store::workflow::parse_journal(bytes, &paths.journal))
                            })
                            .ok()
                            .flatten()
                    })
                    .map(|journal| {
                        reached(&hops(spec, &journal, record.held.as_ref()), &journal)
                            .into_keys()
                            .map(str::to_owned)
                            .collect()
                    })
            });
            project(record, spec.as_deref(), reached_nodes.as_ref())
        })
        .collect()
}

pub(super) fn bind_agents(snapshot: &mut SidebarSnapshot, records: &[RunRecord]) {
    let peers = crate::address::addressable_agents(snapshot);
    let identities = records
        .iter()
        .filter_map(|record| {
            let agent = record.step_agent(&peers)?;
            Some((
                record.id.clone(),
                (agent.kind.clone(), agent.agent_id.clone()),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    if snapshot
        .worktree_groups
        .iter()
        .filter_map(|group| group.workflow.as_ref())
        .all(|run| run.agent.as_ref() == identities.get(&run.run))
    {
        return;
    }
    for run in Arc::make_mut(&mut snapshot.worktree_groups)
        .iter_mut()
        .filter_map(|group| group.workflow.as_mut())
    {
        run.agent = identities.get(&run.run).cloned();
    }
}

fn project(
    record: &RunRecord,
    spec: Option<&Definition>,
    reached: Option<&BTreeSet<String>>,
) -> Option<SidebarWorkflow> {
    if record.state.is_terminal() {
        return None;
    }
    let held = record.held.as_ref();
    let name = held.map_or(record.node.as_str(), |held| held.attempt.node.as_str());
    let entered_at = held.map_or(record.entered_at, |held| held.entered_at);
    let kind = if spec.is_some_and(|spec| spec.step(name).is_some())
        || (spec.is_none()
            && (record.session_attempt().is_some() || record.visits.contains_key(name)))
    {
        WorkflowNodeKind::Step
    } else {
        WorkflowNodeKind::Check
    };
    let walk = spec.map(Definition::walk).unwrap_or_default();
    let current = walk.nodes.iter().find(|node| node.node == name);
    let detour_parent = current.and_then(|node| node.hangs_under.clone());
    let track = if current.is_some() {
        walk.nodes
            .iter()
            .filter(|node| node.hangs_under.is_none())
            .map(|node| SidebarWorkflowNode {
                name: node.node.clone(),
                kind: if spec.is_some_and(|spec| spec.step(&node.node).is_some()) {
                    WorkflowNodeKind::Step
                } else {
                    WorkflowNodeKind::Check
                },
                reached: reached.is_some_and(|nodes| nodes.contains(&node.node)),
            })
            .collect()
    } else {
        Vec::new()
    };
    let capped = spec.and_then(|spec| spec.step(name)).is_some_and(|step| {
        step.visit_bounds(
            record.visits.get(name),
            record.visit_bucket(),
            record.visit_grant,
        )
        .any(|(_, used, max)| used >= max)
    });
    let hold_limit = record
        .wait
        .as_ref()
        .filter(|wait| wait.outcome == "hold" && wait.unknown.is_none())
        .map(|wait| {
            spec.and_then(|spec| spec.check(&wait.node))
                .and_then(|check| {
                    let hold = check.cases().get(wait.case? as usize)?.hold()?;
                    Some(hold.text().to_owned())
                })
                .unwrap_or_else(|| "?".into())
        });
    let word = if record.state == RunState::Parked {
        "parked".into()
    } else if let Some(wait) = &record.wait {
        match wait.outcome.as_str() {
            "capacity" => "held".into(),
            "hold" if wait.unknown.is_some() => "unknown".into(),
            "hold" => wait
                .detail
                .as_ref()
                .map_or_else(|| "holding".into(), |detail| format!("holding {detail}")),
            // A retried launch's reason is an error's text, which can span lines; the strip
            // is one line, so it reads as the CLI's waiting line does.
            "retry" => wait.detail.as_deref().map_or_else(
                || "retry".into(),
                |detail| {
                    format!(
                        "retry: {}",
                        detail.split_whitespace().collect::<Vec<_>>().join(" ")
                    )
                },
            ),
            _ => wait.outcome.clone(),
        }
    } else if kind == WorkflowNodeKind::Step {
        "running".into()
    } else {
        "checking".into()
    };
    let since = if record.state == RunState::Parked {
        record.park.as_ref().map_or(entered_at, |park| park.at)
    } else if kind == WorkflowNodeKind::Check {
        record.wait.as_ref().map_or(entered_at, |wait| wait.first)
    } else {
        entered_at
    };
    Some(SidebarWorkflow {
        run: record.id.clone(),
        workflow: record.workflow.clone(),
        scope_path: record.scope.path.to_string_lossy().into_owned(),
        state: record.state,
        track,
        current: SidebarWorkflowNode {
            name: name.to_owned(),
            kind,
            reached: reached.is_some_and(|nodes| nodes.contains(name)),
        },
        detour_parent,
        visits: record.visits.get(name).map_or(0, |visits| visits.total),
        capped,
        word,
        since,
        hold_limit,
        entered_at,
        created_at: record.created_at,
        agent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::workflow::RunState;
    use jiff::Timestamp;
    use serde_json::json;

    fn record() -> RunRecord {
        serde_json::from_value(json!({
            "version": crate::store::workflow::VERSION, "id": crate::ids::WorkflowRunId::new(), "workflow": "land",
            "spec_dir": "/spec", "spec_digest": "digest", "project_root": "/repo",
            "scope": {"kind": "worktree", "path": "/trees/lane"},
            "created_at": Timestamp::UNIX_EPOCH, "entered_at": "2026-01-01T00:00:00Z",
            "state": "active", "node": "watch", "visits": {}, "visit_grant": 0,
            "deadline_fired": false, "next_seq": 2
        }))
        .unwrap()
    }

    fn spec() -> Definition {
        toml::from_str(
            r#"
            [workflow]
            scope = "worktree"
            start = "sweep"
            [steps.sweep]
            agent = "coder"
            then = "watch"
            [checks.watch]
            else = "queued"
            [checks.watch.on]
            queued = "@wait"
            fix = "fix"
            merged = "clean"
            unknown = "@wait"
            [[checks.watch.cases]]
            when = ["pr.open"]
            for = "30m"
            outcome = "queued"
            [steps.fix]
            agent = "coder"
            max-visits = 2
            then = "watch"
            [steps.clean]
            run = "clean"
            then = "triage"
            [steps.triage]
            agent = "coder"
            lifecycle = "until"
            then = "@done"
        "#,
        )
        .unwrap()
    }

    #[test]
    fn projection_tracks_main_rail_current_detour_caps_and_held_agent() {
        let mut record = record();
        let spec = spec();
        record.node = "fix".into();
        record.visits.insert(
            "fix".into(),
            serde_json::from_value(json!({"total":2, "outcomes":[]})).unwrap(),
        );
        let reached = ["sweep", "watch", "fix"].map(str::to_owned).into();
        let projected = project(&record, Some(&spec), Some(&reached));
        assert!(projected.is_some(), "active run projects");
        let projected = projected.unwrap();
        assert_eq!(
            projected
                .track
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["sweep", "watch", "clean", "triage"]
        );
        assert_eq!(projected.current.name, "fix");
        assert_eq!(
            projected
                .track
                .iter()
                .map(|node| (node.name.as_str(), node.reached))
                .collect::<Vec<_>>(),
            [
                ("sweep", true),
                ("watch", true),
                ("clean", false),
                ("triage", false)
            ]
        );
        assert!(projected.current.reached);
        assert_eq!(projected.detour_parent.as_deref(), Some("watch"));
        assert_eq!(projected.visits, 2);
        assert!(projected.capped);
        assert_eq!(projected.word, "running");

        record.node = "ruled".into();
        record.held = Some(
            serde_json::from_value(json!({
                "attempt": {"kind":"agent", "lifecycle":"until", "seq":1, "node":"triage",
                    "started_at":record.created_at, "host":{"pid":1,"start":null}},
                "entered_at":record.created_at
            }))
            .unwrap(),
        );
        let mut agent = crate::testkit::agent_state("codex", "triage-session", record.created_at);
        agent.workflow = Some(crate::agents::WorkflowAttemptRef {
            run: record.id.clone(),
            seq: 1,
            role: crate::agents::WorkflowAttemptRole::Step,
        });
        let projected = project(&record, Some(&spec), None).unwrap();
        assert_eq!(projected.current.name, "triage");
        assert!(
            projected.agent.is_none(),
            "identity binds after the pane fold"
        );
        assert_eq!(
            record.step_agent(&[&agent]).map(|bound| &bound.agent_id),
            Some(&agent.agent_id)
        );
        assert_eq!(projected.entered_at, record.created_at);
        assert_eq!(projected.since, record.created_at);
    }

    #[test]
    fn projection_words_and_clocks_come_from_wait_node_or_park() {
        let mut record = record();
        let spec = spec();
        for (outcome, expected) in [
            ("queued", "queued"),
            ("capacity", "held"),
            ("takeover", "takeover"),
            ("hold", "holding checks"),
            ("retry", "retry: checks"),
        ] {
            record.wait = Some(
                serde_json::from_value(json!({
                    "node":"watch", "outcome":outcome, "detail":"checks", "case":0,
                    "first":record.created_at, "last":record.entered_at, "count":1
                }))
                .unwrap(),
            );
            let projected = project(&record, Some(&spec), None);
            assert!(projected.is_some(), "live outcome {outcome} projects");
            let projected = projected.unwrap();
            assert_eq!(projected.word, expected);
            assert_eq!(projected.since, record.created_at);
            assert_eq!(
                projected.hold_limit.as_deref(),
                (outcome == "hold").then_some("30m")
            );
        }
        record.wait = Some(
            serde_json::from_value(json!({
                "node":"watch", "outcome":"retry", "detail":"stop failed:\n  pane gone",
                "first":record.created_at, "last":record.entered_at, "count":1
            }))
            .unwrap(),
        );
        assert_eq!(
            project(&record, Some(&spec), None).unwrap().word,
            "retry: stop failed: pane gone"
        );
        record.wait = Some(
            serde_json::from_value(json!({
                "node":"watch", "outcome":"hold", "detail":"checks", "case":0,
                "first":record.created_at, "last":record.entered_at, "count":1
            }))
            .unwrap(),
        );
        let projected = project(&record, None, None);
        assert_eq!(
            projected.and_then(|run| run.hold_limit).as_deref(),
            Some("?")
        );
        record.wait = None;
        assert_eq!(
            project(&record, Some(&spec), None).unwrap().word,
            "checking"
        );
        record.node = "clean".into();
        let projected = project(&record, Some(&spec), None).unwrap();
        assert_eq!(projected.word, "running");
        assert_eq!(projected.since, record.entered_at);
        record.state = RunState::Parked;
        record.park = Some(
            serde_json::from_value(json!({"reason":"user", "at":record.created_at, "text":null}))
                .unwrap(),
        );
        let projected = project(&record, Some(&spec), None).unwrap();
        assert_eq!(projected.word, "parked");
        assert_eq!(projected.since, record.created_at);
    }

    #[test]
    fn unreadable_or_unreachable_spec_is_trackless_and_terminal_run_is_absent() {
        let mut record = record();
        let spec = spec();
        record.node = "gone".into();
        for spec in [None, Some(&spec)] {
            let projected = project(&record, spec, None);
            assert!(
                projected.is_some(),
                "a missing track still projects its current node"
            );
            let projected = projected.unwrap();
            assert!(projected.track.is_empty());
            assert_eq!(projected.current.name, "gone");
        }
        for state in [RunState::Done, RunState::Failed, RunState::Canceled] {
            record.state = state;
            assert!(project(&record, Some(&spec), None).is_none());
        }
    }
}
