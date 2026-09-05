//! Signal vocabulary shared by event ingress, lifecycle hooks, and loop firing.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::runner::CheckOutcome;
use super::{Trigger, arming};
use crate::RuntimePaths;
use crate::harness::schedule::runner::RunLockInfo;
use std::path::Path;

pub const MAX_SIGNAL_NAME_BYTES: usize = 64;

const RESERVED_FAMILIES: &[&str] = &["agent", "wake", "team", "ci", "pr"];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalName(String);

impl SignalName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn family(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }

    pub fn is_reserved(&self) -> bool {
        RESERVED_FAMILIES.contains(&self.family())
    }
}

impl fmt::Display for SignalName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for SignalName {
    type Err = SignalNameErr;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let valid = !raw.is_empty()
            && raw.len() <= MAX_SIGNAL_NAME_BYTES
            && raw.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
                    && segment.chars().all(|ch| {
                        ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_'
                    })
            });
        valid
            .then(|| Self(raw.to_owned()))
            .ok_or_else(|| SignalNameErr(raw.to_owned()))
    }
}

impl Serialize for SignalName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SignalName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid signal name `{0}`; use lowercase dot-separated words, at most 64 bytes")]
pub struct SignalNameErr(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalSelector {
    Exact(SignalName),
    Family(String),
}

impl SignalSelector {
    pub fn family(&self) -> &str {
        match self {
            Self::Exact(name) => name.family(),
            Self::Family(family) => family,
        }
    }
}

impl fmt::Display for SignalSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(name) => name.fmt(f),
            Self::Family(family) => write!(f, "{family}.*"),
        }
    }
}

impl FromStr for SignalSelector {
    type Err = SignalNameErr;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if let Some(family) = raw.strip_suffix(".*") {
            if raw.len() > MAX_SIGNAL_NAME_BYTES || family.contains('.') {
                return Err(SignalNameErr(raw.to_owned()));
            }
            return family
                .parse::<SignalName>()
                .map(|name| Self::Family(name.0));
        }
        raw.parse().map(Self::Exact)
    }
}

impl Serialize for SignalSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SignalSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SignalResolution {
    Ignore,
    Skip,
    Deliver,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignalSource {
    Cli,
    Watch,
    Lifecycle,
    Forge,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Signal {
    pub name: SignalName,
    #[serde(default)]
    pub payload: Map<String, Value>,
    pub source: SignalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchOutcome>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WatchOutcome {
    Exited { code: Option<i32>, output: String },
    TimedOut { code: Option<i32>, output: String },
    Lost { detail: String },
}

impl WatchOutcome {
    pub fn to_check_outcome(&self) -> CheckOutcome {
        match self {
            Self::Exited { code, output } => {
                CheckOutcome::new(*code == Some(0), false, output.clone(), *code)
            }
            Self::TimedOut { code, output } => {
                CheckOutcome::new(false, true, output.clone(), *code)
            }
            Self::Lost { detail } => {
                CheckOutcome::new(false, false, format!("watcher lost: {detail}"), None)
            }
        }
    }
}

pub fn match_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

pub fn lifecycle_signal(event: &crate::agents::LifecycleEvent) -> Option<Signal> {
    if matches!(
        event.transition,
        crate::agents::LifecycleTransition::Ignored { .. }
    ) {
        return None;
    }
    let (name, errored) = match &event.signal {
        crate::agents::LifecycleSignal::Registered
        | crate::agents::LifecycleSignal::SubagentStarted => ("agent.started", false),
        crate::agents::LifecycleSignal::TurnEnded { errored, .. } if *errored => {
            ("agent.failed", true)
        }
        crate::agents::LifecycleSignal::TurnEnded { .. } => ("agent.idle", false),
        crate::agents::LifecycleSignal::AwaitingInput { .. } => ("agent.waiting", false),
        crate::agents::LifecycleSignal::Ended
        | crate::agents::LifecycleSignal::Lost
        | crate::agents::LifecycleSignal::SubagentStopped { .. } => ("agent.ended", false),
        _ => return None,
    };
    let mut payload = Map::from_iter([
        (
            "kind".to_owned(),
            Value::String(event.kind.as_str().to_owned()),
        ),
        (
            "session".to_owned(),
            Value::String(event.agent_id.as_str().to_owned()),
        ),
        (
            "status".to_owned(),
            Value::String(event.status.as_str().to_owned()),
        ),
        ("errored".to_owned(), Value::Bool(errored)),
    ]);
    if let Some(agent_name) = &event.agent_name {
        payload.insert("handle".to_owned(), Value::String(format!("@{agent_name}")));
    }
    if let Some(parent) = &event.parent_agent_id {
        payload.insert(
            "parent".to_owned(),
            Value::String(parent.as_str().to_owned()),
        );
    }
    // These literals are covered by the signal-name grammar test.
    let name = name.parse().expect("static lifecycle signal name is valid");
    Some(Signal {
        name,
        payload,
        source: SignalSource::Lifecycle,
        watch: None,
    })
}

/// Fire matching tasks in the emitter process. Signal events are never replayed.
pub fn fire_signal(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    signal: &Signal,
) -> Result<Vec<String>, serde_json::Error> {
    let project_root = project_root
        .map(Path::to_path_buf)
        .or_else(|| super::fire::workspace_project_root(runtime));
    let tasks = super::fire::runnable_tasks_for(runtime, project_root.as_deref());
    let arming_entries = arming::load();
    let encoded = serde_json::to_string(signal)?;
    let mut fired = Vec::new();
    for (name, task) in tasks {
        let Ok(parsed) = task.trigger() else { continue };
        let resolution = parsed.trigger.resolve(&name, signal);
        if resolution == SignalResolution::Ignore {
            continue;
        }
        let key = arming::TaskKey::for_task(&name, task.source(), &task.entry().resolved_root());
        if arming::ArmState::resolve(
            arming_entries.get(&key),
            task.source(),
            jiff::Timestamp::now(),
        ) != arming::ArmState::Live
        {
            continue;
        }
        if matches!(parsed.trigger, Trigger::Schedule(_)) {
            continue;
        }
        if task.source() == super::catalog::TaskSource::Instance
            && task.entry().wake_meta.is_some()
            && matches!(parsed.trigger, Trigger::Signal { .. })
        {
            match super::instances::observe_signal_wake(&name, task.entry(), jiff::Timestamp::now())
            {
                Ok(true) => {}
                Ok(false) => continue,
                Err(err) => {
                    tracing::warn!(task = name, error = %err, "signal observation failed");
                    continue;
                }
            }
        }
        if resolution == SignalResolution::Skip {
            use super::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult, SignalRecord};
            let mut record = LoopRunRecord::new(
                &name,
                LoopRunResult::SignalSkipped,
                LoopRunMode::Scheduled,
                0,
            );
            record.signal = Some(SignalRecord {
                name: signal.name.clone(),
                payload: signal.payload.clone(),
            });
            super::run_log::record_transition(&task, &record);
            continue;
        }
        super::fire::spawn_loop_run(
            runtime,
            project_root.as_deref(),
            &name,
            Some(&encoded),
            false,
        );
        fired.push(name);
    }
    Ok(fired)
}

pub struct WatchLockGuard {
    file: File,
}

impl Drop for WatchLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn watch_lock_path(runtime: &RuntimePaths, name: &str) -> std::path::PathBuf {
    runtime.root.join(format!("loop-watch-{name}.lock"))
}

pub fn acquire_watch_lock(
    runtime: &RuntimePaths,
    name: &str,
) -> std::io::Result<Option<WatchLockGuard>> {
    let path = watch_lock_path(runtime, name);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            serde_json::to_writer(
                &mut file,
                &RunLockInfo {
                    pid: std::process::id(),
                    started_at: jiff::Timestamp::now(),
                },
            )
            .map_err(std::io::Error::other)?;
            file.flush()?;
            Ok(Some(WatchLockGuard { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(err) => Err(std::io::Error::from(err)),
    }
}

pub fn watcher_info(runtime: &RuntimePaths, name: &str) -> std::io::Result<Option<RunLockInfo>> {
    let path = watch_lock_path(runtime, name);
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match file.try_lock() {
        Ok(()) => {
            file.unlock()?;
            Ok(None)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(serde_json::from_slice(&bytes).ok())
        }
        Err(err) => Err(std::io::Error::from(err)),
    }
}

pub fn stop_watcher(runtime: &RuntimePaths, name: &str) -> std::io::Result<bool> {
    let Some(info) = watcher_info(runtime, name)? else {
        return Ok(false);
    };
    let Ok(pid) = i32::try_from(info.pid) else {
        return Ok(false);
    };
    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(()) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(false),
        Err(err) => Err(std::io::Error::other(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{
        AgentStatus, LifecycleEvent, LifecycleSignal, LifecycleTransition, TurnPhase,
    };
    use crate::ids::{AgentKind, AgentSessionId, EventId, WorkspaceId};

    fn lifecycle_event(signal: LifecycleSignal) -> LifecycleEvent {
        LifecycleEvent {
            v: 1,
            event_id: EventId::parse("evt_018f47a2c00070008000000000000000").unwrap(),
            at: "2026-06-01T12:00:00Z".parse().unwrap(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("session-1"),
            agent_name: Some("coder".to_owned()),
            parent_agent_id: Some(AgentSessionId::from("session-parent")),
            signal,
            prior_status: Some(AgentStatus::Running),
            status: AgentStatus::Success,
            phase: TurnPhase::Idle,
            transition: LifecycleTransition::Normal,
            compaction_closed: false,
            waiting_cleared: false,
        }
    }

    #[test]
    fn signal_names_pin_the_public_grammar() {
        for valid in ["ci.finished", "deploy_done", "a-b.c2"] {
            assert_eq!(valid.parse::<SignalName>().unwrap().as_str(), valid);
        }
        for invalid in ["", ".ci", "ci.", "CI.finished", "ci finished", "_ci"] {
            assert!(invalid.parse::<SignalName>().is_err(), "{invalid}");
        }
        assert!("agent.idle".parse::<SignalName>().unwrap().is_reserved());
        assert!("wake.task".parse::<SignalName>().unwrap().is_reserved());
        for family in ["agent", "wake", "team", "ci", "pr"] {
            let name = format!("{family}.done").parse::<SignalName>().unwrap();
            assert_eq!(name.family(), family);
            assert!(name.is_reserved());
        }
        assert!(!"deploy.done".parse::<SignalName>().unwrap().is_reserved());
        for invalid in ["*", "a.*", "a.b.*", "a*"] {
            assert!(invalid.parse::<SignalName>().is_err());
        }
        for valid in [
            "ci.failed",
            "ci.*",
            "deploy.finished",
            "a.b.c",
            "deploy_done",
        ] {
            let selector = valid.parse::<SignalSelector>().unwrap();
            assert_eq!(selector.to_string(), valid);
            let encoded = serde_json::to_string(&selector).unwrap();
            assert_eq!(encoded, format!("\"{valid}\""));
            assert_eq!(
                serde_json::from_str::<SignalSelector>(&encoded).unwrap(),
                selector
            );
        }
        for invalid in ["*", "a.b.*", "a*", "A.*", ".*", "a..*"] {
            assert!(invalid.parse::<SignalSelector>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn lifecycle_events_derive_the_public_signal_table() {
        for (input, expected, errored) in [
            (LifecycleSignal::Registered, "agent.started", false),
            (LifecycleSignal::SubagentStarted, "agent.started", false),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                },
                "agent.idle",
                false,
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: true,
                    parked_on_background: false,
                },
                "agent.failed",
                true,
            ),
            (
                LifecycleSignal::AwaitingInput {
                    kind: crate::agents::AskKind::Question,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                "agent.waiting",
                false,
            ),
            (LifecycleSignal::Ended, "agent.ended", false),
            (LifecycleSignal::Lost, "agent.ended", false),
            (
                LifecycleSignal::SubagentStopped { errored: true },
                "agent.ended",
                false,
            ),
        ] {
            let signal = lifecycle_signal(&lifecycle_event(input)).expect("derived signal");
            assert_eq!(signal.name.as_str(), expected);
            assert_eq!(signal.source, SignalSource::Lifecycle);
            assert_eq!(signal.payload["handle"], "@coder");
            assert_eq!(signal.payload["session"], "session-1");
            assert_eq!(signal.payload["parent"], "session-parent");
            assert_eq!(signal.payload["errored"], errored);
        }

        let mut ignored = lifecycle_event(LifecycleSignal::Registered);
        ignored.transition = LifecycleTransition::Ignored {
            reason: "duplicate".to_owned(),
        };
        assert_eq!(lifecycle_signal(&ignored), None);
        assert_eq!(
            lifecycle_signal(&lifecycle_event(LifecycleSignal::TurnStarted)),
            None
        );
    }
}
