//! Machine-local arming state for loop tasks.
//!
//! Arming overlays every task source without editing its durable definition.
//! Project tasks default disabled until this machine records an explicit
//! enable. An ended pause and a fresh enable remain effective last-fire edges
//! so schedules do not replay occurrences missed while held.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::catalog::TaskSource;
use super::task_record::{self, Binding, TaskRecordErr};
use super::task_record_store::{self, TaskStoreErr};

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ArmingError(#[from] TaskStoreErr);

type Result<T> = std::result::Result<T, ArmingError>;

/// Machine-local arming record for one task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Arming {
    pub enabled: bool,
    /// When this enable or disable took effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikes: Option<u32>,
}

impl Arming {
    fn default_for(source: TaskSource) -> Self {
        Self {
            enabled: !matches!(source, TaskSource::Project { .. }),
            at: None,
            pause_until: None,
            strikes: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmState {
    Live,
    Disabled(DisabledReason),
    Paused(Timestamp),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledReason {
    /// A project task with no local record: the default-off case.
    NotEnabledHere,
    Manual,
    Strikes(u32),
}

impl ArmState {
    pub fn resolve(record: Option<&Arming>, source: TaskSource, now: Timestamp) -> Self {
        let Some(record) = record else {
            return if Arming::default_for(source).enabled {
                Self::Live
            } else {
                Self::Disabled(DisabledReason::NotEnabledHere)
            };
        };
        if !record.enabled {
            return Self::Disabled(
                record
                    .strikes
                    .map_or(DisabledReason::Manual, DisabledReason::Strikes),
            );
        }
        if let Some(until) = record.pause_until.filter(|until| *until > now) {
            return Self::Paused(until);
        }
        Self::Live
    }
}

/// Every bound task's arming, by binding key; an unreadable task record is an error, never an
/// empty map.
pub fn load() -> std::result::Result<BTreeMap<String, Arming>, TaskRecordErr> {
    Ok(task_record::load()?
        .by_key()
        .into_iter()
        .filter_map(|(key, (_, record))| record.arming.map(|arming| (key, arming)))
        .collect())
}

pub(super) fn enable(binding: &Binding) -> Result<Arming> {
    let entry = Arming {
        enabled: true,
        at: Some(Timestamp::now()),
        pause_until: None,
        strikes: None,
    };
    edit(binding, |arming| *arming = Some(entry))?;
    Ok(entry)
}

pub fn disable(binding: &Binding, strikes: Option<u32>) -> Result<()> {
    let entry = Arming {
        enabled: false,
        at: Some(Timestamp::now()),
        pause_until: None,
        strikes,
    };
    edit(binding, |arming| *arming = Some(entry))
}

/// Pause a task without changing its effective enablement.
///
/// A missing record inherits its source default before the pause is written.
pub fn pause(binding: &Binding, source: TaskSource, until: Timestamp) -> Result<()> {
    edit(binding, |arming| paused(arming, source, until))
}

/// Disable a live task, as a strike-out does; a disabled or paused task is left as it is.
pub(super) fn disable_if_live(
    binding: &Binding,
    source: TaskSource,
    strikes: Option<u32>,
    now: Timestamp,
) -> Result<bool> {
    edit(binding, |arming| {
        disabled_if_live(arming, source, strikes, now)
    })
}

fn edit<T>(binding: &Binding, change: impl FnOnce(&mut Option<Arming>) -> T) -> Result<T> {
    Ok(task_record_store::mutate(|records| {
        change(&mut records.bind(binding).1.arming)
    })?)
}

fn paused(arming: &mut Option<Arming>, source: TaskSource, until: Timestamp) {
    arming
        .get_or_insert_with(|| Arming::default_for(source))
        .pause_until = Some(until);
}

pub(super) fn disabled_if_live(
    arming: &mut Option<Arming>,
    source: TaskSource,
    strikes: Option<u32>,
    now: Timestamp,
) -> bool {
    if ArmState::resolve(arming.as_ref(), source, now) != ArmState::Live {
        return false;
    }
    *arming = Some(Arming {
        enabled: false,
        at: Some(now),
        pause_until: None,
        strikes,
    });
    true
}

pub(super) fn effective_last_fire(
    stamp: Timestamp,
    record: Option<&Arming>,
    now: Timestamp,
) -> Timestamp {
    let Some(record) = record else {
        return stamp;
    };
    let enabled_at = record.enabled.then_some(record.at).flatten();
    let ended_pause = record.pause_until.filter(|until| *until <= now);
    enabled_at
        .into_iter()
        .chain(ended_pause)
        .fold(stamp, Timestamp::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TrustState;

    fn ts(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("timestamp")
    }

    fn project() -> TaskSource {
        TaskSource::Project {
            state: TrustState::Trusted,
        }
    }

    fn record(enabled: bool) -> Arming {
        Arming {
            enabled,
            at: Some(ts(10)),
            pause_until: None,
            strikes: None,
        }
    }

    #[test]
    fn source_defaults_and_explicit_states_resolve() {
        assert_eq!(
            ArmState::resolve(None, TaskSource::Config, ts(20)),
            ArmState::Live
        );
        assert_eq!(
            ArmState::resolve(None, TaskSource::Instance, ts(20)),
            ArmState::Live
        );
        assert_eq!(
            ArmState::resolve(None, project(), ts(20)),
            ArmState::Disabled(DisabledReason::NotEnabledHere)
        );
        assert_eq!(
            ArmState::resolve(Some(&record(false)), TaskSource::Config, ts(20)),
            ArmState::Disabled(DisabledReason::Manual)
        );
        assert_eq!(
            ArmState::resolve(
                Some(&Arming {
                    strikes: Some(3),
                    ..record(false)
                }),
                TaskSource::Config,
                ts(20)
            ),
            ArmState::Disabled(DisabledReason::Strikes(3))
        );
    }

    #[test]
    fn disabled_precedes_pause_and_pause_expires() {
        let disabled = Arming {
            pause_until: Some(ts(30)),
            ..record(false)
        };
        let paused = Arming {
            pause_until: Some(ts(30)),
            ..record(true)
        };
        assert_eq!(
            ArmState::resolve(Some(&disabled), TaskSource::Config, ts(20)),
            ArmState::Disabled(DisabledReason::Manual)
        );
        assert_eq!(
            ArmState::resolve(Some(&paused), TaskSource::Config, ts(20)),
            ArmState::Paused(ts(30))
        );
        assert_eq!(
            ArmState::resolve(Some(&paused), TaskSource::Config, ts(30)),
            ArmState::Live
        );
    }

    #[test]
    fn pause_preserves_each_source_default() {
        let mut machine = None;
        paused(&mut machine, TaskSource::Config, ts(30));
        let mut project_task = None;
        paused(&mut project_task, project(), ts(30));
        let machine = machine.expect("machine pause");
        assert!(machine.enabled);
        assert_eq!(machine.at, None);
        assert!(!project_task.expect("project pause").enabled);
    }

    #[test]
    fn effective_stamp_uses_only_live_enable_and_ended_pause_edges() {
        let enabled = Arming {
            at: Some(ts(20)),
            pause_until: Some(ts(30)),
            ..record(true)
        };
        let disabled = Arming {
            at: Some(ts(40)),
            pause_until: None,
            ..record(false)
        };
        assert_eq!(effective_last_fire(ts(10), Some(&enabled), ts(25)), ts(20));
        assert_eq!(effective_last_fire(ts(10), Some(&enabled), ts(35)), ts(30));
        assert_eq!(effective_last_fire(ts(32), Some(&enabled), ts(35)), ts(32));
        assert_eq!(effective_last_fire(ts(10), Some(&disabled), ts(50)), ts(10));
        assert_eq!(effective_last_fire(ts(10), None, ts(50)), ts(10));
    }

    #[test]
    fn automatic_disable_preserves_an_active_pause() {
        let mut arming = None;
        paused(&mut arming, TaskSource::Config, ts(30));
        assert!(
            !disabled_if_live(&mut arming, TaskSource::Config, Some(3), ts(20)),
            "an active pause"
        );
        assert!(
            disabled_if_live(&mut arming, TaskSource::Config, Some(3), ts(30)),
            "an ended pause"
        );
        assert_eq!(
            ArmState::resolve(arming.as_ref(), TaskSource::Config, ts(30)),
            ArmState::Disabled(DisabledReason::Strikes(3))
        );
    }

    #[test]
    fn automatic_disable_respects_missing_record_source_defaults() {
        assert!(!disabled_if_live(&mut None, project(), Some(3), ts(20)));
        assert!(disabled_if_live(
            &mut None,
            TaskSource::Config,
            Some(3),
            ts(20)
        ));
    }
}
