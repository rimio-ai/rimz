//! Read-only Kimi child identity correlation from session metadata and wires.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::{transcript, wire};

const START_RETRY_BACKOFF: [Duration; 5] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(100),
];
const STOP_CONFIRM_BACKOFF: [Duration; 5] = START_RETRY_BACKOFF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChildMatch {
    pub(super) id: String,
    pub(super) task: Option<String>,
    pub(super) profile: Option<String>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    pub(super) transcript_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SessionState {
    title: Option<String>,
    agents: BTreeMap<String, SessionAgent>,
}

pub(super) fn session_title(session_dir: &Path) -> Option<String> {
    let state: SessionState =
        serde_json::from_slice(&std::fs::read(session_dir.join("state.json")).ok()?).ok()?;
    non_empty(state.title)
        // Kimi Code creates a session with this title before the first prompt.
        .filter(|title| title != "New Session")
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SessionAgent {
    homedir: PathBuf,
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "parentAgentId")]
    _parent_agent_id: Option<String>,
    #[serde(rename = "swarmItem")]
    swarm_item: Option<String>,
}

#[derive(Debug)]
struct ChildAgent {
    id: String,
    swarm_item: Option<String>,
    transcript_path: PathBuf,
}

#[derive(Debug)]
struct SessionAgentMap {
    children: Vec<ChildAgent>,
}

impl SessionAgentMap {
    fn read(session_dir: &Path) -> Option<Self> {
        let session_dir = std::fs::canonicalize(session_dir).ok()?;
        let state: SessionState =
            serde_json::from_slice(&std::fs::read(session_dir.join("state.json")).ok()?).ok()?;
        let children = state
            .agents
            .into_iter()
            .filter(|(_, agent)| agent.kind == "sub")
            .filter_map(|(id, agent)| {
                let homedir = validated_child_home(&session_dir, &id, &agent.homedir)?;
                Some(ChildAgent {
                    id,
                    swarm_item: non_empty(agent.swarm_item),
                    transcript_path: homedir.join("wire.jsonl"),
                })
            })
            .collect();
        Some(Self { children })
    }
}

fn validated_child_home(session_dir: &Path, id: &str, homedir: &Path) -> Option<PathBuf> {
    let agents_dir = session_dir.join("agents");
    if id.trim().is_empty()
        || !homedir.is_absolute()
        || homedir
            .components()
            .any(|part| part == Component::ParentDir)
        || !homedir.starts_with(&agents_dir)
        || homedir.file_name().and_then(|name| name.to_str()) != Some(id)
    {
        return None;
    }
    if homedir.exists() {
        let canonical = std::fs::canonicalize(homedir).ok()?;
        let canonical_agents = std::fs::canonicalize(&agents_dir).ok()?;
        return canonical.starts_with(canonical_agents).then_some(canonical);
    }
    Some(homedir.to_path_buf())
}

enum StartResolution {
    Match(ChildMatch),
    Missing,
    Ambiguous,
}

pub(super) fn resolve_start(
    session_dir: &Path,
    prompt_preview: Option<&str>,
) -> Option<ChildMatch> {
    let mut delays = START_RETRY_BACKOFF.into_iter();
    loop {
        match resolve_start_once(session_dir, prompt_preview) {
            StartResolution::Match(child) => return Some(child),
            StartResolution::Ambiguous => return None,
            StartResolution::Missing => {
                let delay = delays.next()?;
                std::thread::sleep(delay);
            }
        }
    }
}

fn resolve_start_once(session_dir: &Path, prompt_preview: Option<&str>) -> StartResolution {
    let Some(map) = SessionAgentMap::read(session_dir) else {
        return StartResolution::Missing;
    };
    let candidates = map
        .children
        .into_iter()
        .filter_map(|child| {
            let records = child_records(&child.transcript_path)?;
            records
                .iter()
                .all(|record| {
                    !matches!(
                        &record.event,
                        wire::WireEvent::Prompt {
                            kind: wire::PromptKind::Prompt,
                            ..
                        }
                    )
                })
                .then_some((child, records))
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [] => StartResolution::Missing,
        [(child, records)] => StartResolution::Match(child_match(
            child,
            records,
            non_empty(prompt_preview.map(str::to_owned)),
        )),
        _ => {
            let Some(prompt) = prompt_preview else {
                return StartResolution::Ambiguous;
            };
            let matching = candidates
                .iter()
                .filter(|(child, _)| {
                    child
                        .swarm_item
                        .as_deref()
                        .is_some_and(|item| prompt.contains(item))
                })
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [(child, records)] => StartResolution::Match(child_match(
                    child,
                    records,
                    non_empty(Some(prompt.to_owned())),
                )),
                _ => StartResolution::Ambiguous,
            }
        }
    }
}

pub(super) fn resolve_stop(
    session_dir: &Path,
    response_preview: Option<&str>,
) -> Option<ChildMatch> {
    let map = SessionAgentMap::read(session_dir)?;
    let response = response_preview
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if response.is_none() && map.children.len() == 1 {
        let child = &map.children[0];
        let records = child_records(&child.transcript_path)?;
        return Some(child_match(child, &records, first_prompt(&records)));
    }
    let response = response?;
    let matching = map
        .children
        .iter()
        .filter_map(|child| {
            let records = child_records(&child.transcript_path)?;
            transcript::latest_assistant_from_records(&records)
                .is_some_and(|assistant| assistant.starts_with(response))
                .then_some((child, records))
        })
        .collect::<Vec<_>>();
    let [(child, records)] = matching.as_slice() else {
        return None;
    };
    Some(child_match(child, records, first_prompt(records)))
}

pub(super) fn has_subagents(session_dir: &Path) -> bool {
    SessionAgentMap::read(session_dir).is_some_and(|map| !map.children.is_empty())
}

pub(super) fn main_turn_mid_step(session_dir: &Path) -> bool {
    if !main_turn_mid_step_once(session_dir) {
        return false;
    }
    for delay in STOP_CONFIRM_BACKOFF {
        std::thread::sleep(delay);
        if !main_turn_mid_step_once(session_dir) {
            return false;
        }
    }
    true
}

fn main_turn_mid_step_once(session_dir: &Path) -> bool {
    let Some((records, _)) = wire::read_records(&session_dir.join("agents/main/wire.jsonl"), 0)
    else {
        return false;
    };
    records
        .iter()
        .fold(false, |mid_step, record| match &record.event {
            wire::WireEvent::LlmRequest(_) => true,
            wire::WireEvent::AppendLoopEvent(wire::LoopEvent::StepEnd { .. }) => false,
            _ => mid_step,
        })
}

fn child_records(path: &Path) -> Option<Vec<wire::WireRecord>> {
    if !path.exists() {
        return Some(Vec::new());
    }
    wire::read_records(path, 0).map(|(records, _)| records)
}

fn child_match(
    child: &ChildAgent,
    records: &[wire::WireRecord],
    task: Option<String>,
) -> ChildMatch {
    let attribution = wire::effective_attribution(records);
    ChildMatch {
        id: child.id.clone(),
        task,
        profile: latest_profile(records),
        model: attribution.display_model(),
        effort: attribution.thinking_effort,
        transcript_path: child.transcript_path.clone(),
    }
}

fn first_prompt(records: &[wire::WireRecord]) -> Option<String> {
    records.iter().find_map(|record| {
        let wire::WireEvent::Prompt { prompt, .. } = &record.event else {
            return None;
        };
        non_empty(Some(
            prompt
                .input
                .iter()
                .filter_map(wire::ContentPart::text)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    })
}

fn latest_profile(records: &[wire::WireRecord]) -> Option<String> {
    records.iter().rev().find_map(|record| match &record.event {
        wire::WireEvent::ConfigUpdate(update) => non_empty(update.profile_name.clone()),
        _ => None,
    })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
