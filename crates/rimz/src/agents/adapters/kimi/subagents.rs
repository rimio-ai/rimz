//! Read-only child lifecycle observations from Kimi's durable main wire.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::Duration;

use super::wire;
use crate::agents::{
    AgentLifecycleObservation, LifecycleSignal, SanitizedPrompt, sanitize_user_prompt,
};
use crate::ids::AgentSessionId;

pub(super) fn derive_at(session: &Path, parent: &AgentSessionId) -> Vec<AgentLifecycleObservation> {
    let Some(records) = wire::read_subagent_records(&session.join("agents/main/wire.jsonl")) else {
        return Vec::new();
    };
    let mut children = BTreeMap::new();
    for record in records {
        let errored = matches!(record.event, wire::WireEvent::SubagentFailed { .. });
        match record.event {
            wire::WireEvent::SubagentSpawned(spawn) => {
                let mut components = Path::new(&spawn.subagent_id).components();
                if !matches!(components.next(), Some(Component::Normal(id)) if id == std::ffi::OsStr::new(&spawn.subagent_id))
                    || components.next().is_some()
                {
                    continue;
                }
                children.insert(spawn.subagent_id.clone(), (spawn, None));
            }
            wire::WireEvent::SubagentCompleted(completed) => {
                if let Some((_, stopped)) = children.get_mut(&completed.subagent_id) {
                    *stopped = Some((false, completed.usage.map(|usage| usage.total())));
                }
            }
            wire::WireEvent::SubagentFailed { id } | wire::WireEvent::SubagentCancelled { id } => {
                if let Some((_, stopped)) = children.get_mut(&id) {
                    *stopped = Some((errored, None));
                }
            }
            _ => {}
        }
    }
    let mut observations = Vec::new();
    for (id, (spawn, stopped)) in children {
        let path = session.join("agents").join(&id).join("wire.jsonl");
        let attribution = if spawn.model.is_none() || spawn.thinking_effort.is_none() {
            wire::read_attribution(&path).unwrap_or_default()
        } else {
            wire::EffectiveAttribution::default()
        };
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(format!("{parent}:{id}"))),
            LifecycleSignal::SubagentStarted,
        );
        observation.parent_agent_id = Some(parent.clone());
        observation.agent_name = Some(spawn.subagent_name);
        observation.task = sanitize_user_prompt(spawn.description.as_deref());
        observation.prompt = SanitizedPrompt::new(spawn.description.as_deref());
        observation.launch.model = spawn.model.or_else(|| attribution.display_model());
        observation.launch.effort = spawn.thinking_effort.or(attribution.thinking_effort);
        observation.transcript_path = Some(path.to_string_lossy().into_owned());
        observations.push(observation.clone());
        if let Some((errored, tokens)) = stopped {
            observation.signal = LifecycleSignal::SubagentStopped { errored };
            observation.usage.run_total_tokens = tokens;
            observations.push(observation);
        }
    }
    observations
}

pub(super) fn main_turn_mid_step(session: &Path) -> bool {
    let check = || {
        let Some((records, _)) = wire::read_records(&session.join("agents/main/wire.jsonl"), 0)
        else {
            return false;
        };
        records
            .iter()
            .any(|record| matches!(record.event, wire::WireEvent::SubagentSpawned(_)))
            && records
                .iter()
                .fold(false, |open, record| match record.event {
                    wire::WireEvent::LlmRequest(_) => true,
                    wire::WireEvent::Prompt {
                        kind: wire::PromptKind::Prompt,
                        ..
                    } => false,
                    wire::WireEvent::AppendLoopEvent(wire::LoopEvent::StepEnd { .. }) => false,
                    _ => open,
                })
    };
    if !check() {
        return false;
    }
    for millis in [10, 20, 40, 80, 100] {
        std::thread::sleep(Duration::from_millis(millis));
        if !check() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LifecycleSignal;

    #[test]
    fn durable_children_settle_latest_runs_and_reject_unsafe_ids() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path();
        std::fs::create_dir_all(session.join("agents/main")).unwrap();
        std::fs::create_dir_all(session.join("agents/live")).unwrap();
        std::fs::create_dir_all(session.join("agents/resumed/wire.jsonl")).unwrap();
        std::fs::write(session.join("agents/live/wire.jsonl"), "{\"type\":\"config.update\",\"modelAlias\":\"kimi-code/fallback\",\"thinkingEffort\":\"high\"}\n").unwrap();
        let mut records = Vec::new();
        for id in [
            "done",
            "failed",
            "cancelled",
            "live",
            "../escape",
            "resumed",
        ] {
            records.push(serde_json::json!({"type":"subagent.spawned", "subagentId":id, "subagentName":"coder", "description":"inspect parser"}));
        }
        records.extend([
            serde_json::json!({"type":"subagent.completed","subagentId":"done","usage":{"inputOther":10,"output":3,"inputCacheRead":2,"inputCacheCreation":1}}),
            serde_json::json!({"type":"subagent.failed","subagentId":"failed","error":"boom"}),
            serde_json::json!({"type":"subagent.cancelled","subagentId":"cancelled"}),
            serde_json::json!({"type":"subagent.completed","subagentId":"resumed"}),
            serde_json::json!({"type":"subagent.spawned","subagentId":"resumed","subagentName":"reviewer","description":"new task","model":"explicit","thinkingEffort":"low"}),
        ]);
        let path = session.join("agents/main/wire.jsonl");
        std::fs::write(
            &path,
            records.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let observations = derive_at(session, &"sid".into());
        assert_eq!(observations.len(), 8);
        for id in ["done", "failed", "cancelled", "live", "resumed"] {
            let child = observations
                .iter()
                .find(|o| o.agent_id.as_deref() == Some(&format!("sid:{id}")))
                .unwrap();
            assert_eq!(child.parent_agent_id.as_deref(), Some("sid"));
            assert_eq!(child.signal, LifecycleSignal::SubagentStarted);
            assert_eq!(
                child.transcript_path.as_deref(),
                session.join(format!("agents/{id}/wire.jsonl")).to_str()
            );
        }
        let stops = observations
            .iter()
            .filter(|o| matches!(o.signal, LifecycleSignal::SubagentStopped { .. }))
            .collect::<Vec<_>>();
        assert_eq!(stops.len(), 3);
        for stop in stops {
            assert_eq!(
                stop.signal,
                LifecycleSignal::SubagentStopped {
                    errored: stop.agent_id.as_deref() == Some("sid:failed")
                }
            );
            if stop.agent_id.as_deref() == Some("sid:done") {
                assert_eq!(stop.usage.run_total_tokens, Some(16));
            }
        }
        let live = observations
            .iter()
            .find(|o| o.agent_id.as_deref() == Some("sid:live"))
            .unwrap();
        assert_eq!(live.launch.model.as_deref(), Some("fallback"));
        assert_eq!(live.launch.effort.as_deref(), Some("high"));
        let resumed = observations
            .iter()
            .find(|o| o.agent_id.as_deref() == Some("sid:resumed"))
            .unwrap();
        assert_eq!(resumed.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(resumed.task.as_deref(), Some("new task"));
        assert_eq!(resumed.launch.model.as_deref(), Some("explicit"));
        assert_eq!(resumed.launch.effort.as_deref(), Some("low"));
        std::fs::write(
            path,
            "{\"type\":\"subagent.failed\",\"subagentId\":\"unknown\"}\n",
        )
        .unwrap();
        assert!(derive_at(session, &"sid".into()).is_empty());
    }
}
