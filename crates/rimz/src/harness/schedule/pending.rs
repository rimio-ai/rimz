//! Read-only projection of armed one-shot deliveries from the loop catalog.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::{PendingWake, PendingWakeTrigger};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::SidebarSnapshot;

use super::Trigger;
use super::catalog::{LoadedTask, TaskCatalog, TaskSource};

pub fn session_deliveries<'a>(
    catalog: &'a TaskCatalog,
    root: &Path,
    session: Option<&(AgentKind, AgentSessionId)>,
) -> impl Iterator<Item = (&'a str, &'a LoadedTask)> {
    catalog.visible().iter().filter_map(move |(name, task)| {
        if task.source() != TaskSource::Instance || task.entry().resolved_root() != root {
            return None;
        }
        let target = task.entry().wake.as_ref()?;
        if session.is_some_and(|(kind, session)| target.kind != *kind || target.session != *session)
        {
            return None;
        }
        Some((name.as_str(), task))
    })
}

fn pending_wake(name: &str, task: &LoadedTask, now: &jiff::Zoned) -> Option<PendingWake> {
    if !task.is_ephemeral() {
        return None;
    }
    let parsed = task.trigger().as_ref().ok()?;
    let armed_at = task.entry().wake_meta.as_ref().map(|meta| meta.armed_at);
    let trigger = match &parsed.trigger {
        Trigger::Schedule(schedule) => {
            // A deadline shortens row lifetime, but does not make a recurring
            // clock's next occurrence derivable from its original arm time.
            if !schedule.once {
                return None;
            }
            let anchor = armed_at.map(|at| at.to_zoned(now.time_zone().clone()));
            let due = parsed.next_after(anchor.as_ref().unwrap_or(now))?;
            PendingWakeTrigger::Timer { due }
        }
        Trigger::Watch { command } => PendingWakeTrigger::Command {
            command: command.clone(),
        },
        Trigger::Signal { selector, .. } => PendingWakeTrigger::Signal {
            selector: selector.to_string(),
            deadline: task.entry().deadline,
        },
    };
    Some(PendingWake {
        name: name.to_owned(),
        trigger,
        armed_at,
    })
}

pub fn pending_wakes_by_session(
    catalog: &TaskCatalog,
    root: &Path,
    now: jiff::Timestamp,
) -> BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWake>> {
    let mut wakes = BTreeMap::<_, Vec<_>>::new();
    let now = now.to_zoned(crate::config::MachineConfig::load_lenient().time_zone());
    for (name, task) in session_deliveries(catalog, root, None) {
        let Some(wake) = pending_wake(name, task, &now) else {
            continue;
        };
        let Some(target) = task.entry().wake.as_ref() else {
            continue;
        };
        wakes
            .entry((target.kind.clone(), target.session.clone()))
            .or_default()
            .push(wake);
    }
    for wakes in wakes.values_mut() {
        wakes.sort_by_key(|wake| {
            let (kind, due) = match wake.trigger {
                PendingWakeTrigger::Timer { due } => (0, Some(due)),
                PendingWakeTrigger::Command { .. } => (1, None),
                PendingWakeTrigger::Signal { .. } => (2, None),
            };
            (kind, due, wake.name.clone())
        });
    }
    wakes
}

pub(crate) fn project_pending_wakes(snapshot: &mut SidebarSnapshot, project_root: Option<&Path>) {
    let mut wakes = project_root.map_or_else(BTreeMap::new, |root| {
        pending_wakes_by_session(&TaskCatalog::load_lenient(Some(root)), root, snapshot.now)
    });
    for agent in &mut snapshot.agents {
        agent.pending_wakes = wakes
            .remove(&(agent.kind.clone(), agent.agent_id.clone()))
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TaskEntry;

    #[test]
    fn pending_wake_requires_an_ephemeral_valid_trigger_and_one_shot_clock() {
        let deadline = "2026-06-01T12:00:00Z".parse().unwrap();
        let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
        for entry in [
            TaskEntry {
                signal: Some("pr.merged".into()),
                ..TaskEntry::default()
            },
            TaskEntry {
                watch: Some(String::new()),
                ..TaskEntry::default()
            },
            TaskEntry {
                every: Some("1h".into()),
                deadline: Some(deadline),
                ..TaskEntry::default()
            },
        ] {
            let task = LoadedTask::new("wake", entry, TaskSource::Instance);
            assert!(pending_wake("wake", &task, &now).is_none());
        }
        let task = LoadedTask::new(
            "signal",
            TaskEntry {
                signal: Some("pr.merged".into()),
                deadline: Some(deadline),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        assert_eq!(
            pending_wake("signal", &task, &now).unwrap().trigger,
            PendingWakeTrigger::Signal {
                selector: "pr.merged".into(),
                deadline: Some(deadline),
            }
        );
    }

    #[test]
    fn clock_due_uses_arm_time_or_the_snapshot_clock() {
        let now = "2026-06-02T10:00:00Z[UTC]".parse().unwrap();
        for (armed_at, expected) in [
            (Some("2026-06-01T10:00:00Z"), "2026-06-01T12:00:00Z"),
            (None, "2026-06-02T12:00:00Z"),
        ] {
            let task = LoadedTask::new(
                "timer",
                TaskEntry {
                    at: Some("12:00".into()),
                    wake_meta: armed_at.map(|at| crate::config::WakeMeta {
                        armed_at: at.parse().unwrap(),
                        delay: None,
                    }),
                    ..TaskEntry::default()
                },
                TaskSource::Instance,
            );
            assert_eq!(
                pending_wake("timer", &task, &now).unwrap().trigger,
                PendingWakeTrigger::Timer {
                    due: expected.parse().unwrap()
                },
            );
        }
    }
}
