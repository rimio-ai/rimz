//! Read-only projection of armed one-shot deliveries from the loop catalog.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::{PendingWake, PendingWakeTrigger};
use crate::config::MachineConfig;
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
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
    let meta = task.entry().wake_meta.as_ref();
    let armed_at = meta.map(|meta| meta.armed_at);
    let trigger = match &parsed.trigger {
        Trigger::Schedule(schedule) => {
            // A deadline shortens row lifetime, but does not make a recurring
            // clock's next occurrence derivable from its original arm time.
            if !schedule.once {
                return None;
            }
            let anchor = armed_at.map(|at| at.to_zoned(now.time_zone().clone()));
            let due = parsed.next_after(anchor.as_ref().unwrap_or(now))?;
            PendingWakeTrigger::Timer {
                due,
                delay: meta.and_then(|meta| meta.delay.clone()),
            }
        }
        Trigger::Watch { command } => match meta.and_then(|meta| meta.pid) {
            Some(pid) => PendingWakeTrigger::Pid { pid },
            None => PendingWakeTrigger::Command {
                command: command.clone(),
            },
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
    now: &jiff::Zoned,
) -> BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWake>> {
    let mut wakes = BTreeMap::<_, Vec<_>>::new();
    for (name, task) in session_deliveries(catalog, root, None) {
        let Some(wake) = pending_wake(name, task, now) else {
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
                PendingWakeTrigger::Timer { due, .. } => (0, Some(due)),
                PendingWakeTrigger::Pid { .. } | PendingWakeTrigger::Command { .. } => (1, None),
                PendingWakeTrigger::Signal { .. } => (2, None),
            };
            (kind, due, wake.name.clone())
        });
    }
    wakes
}

pub(crate) fn project_pending_wakes(
    snapshot: &mut SidebarSnapshot,
    project_root: Option<&Path>,
    config: &MachineConfig,
) {
    let mut wakes = project_root.map_or_else(BTreeMap::new, |root| {
        let instance_root = crate::disk::paths::workspaces_dir()
            .join(WorkspaceId::from_project_root(root).as_str());
        if super::instances::load_from(&instance_root).0.is_empty() {
            return BTreeMap::new();
        }
        pending_wakes_by_session(
            &TaskCatalog::load_lenient(Some(root)),
            root,
            &snapshot.now.to_zoned(config.time_zone()),
        )
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
    fn pending_wakes_preserve_pid_and_delay() {
        let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
        let armed_at = "2026-06-01T09:42:00Z".parse().unwrap();
        let meta = crate::config::WakeMeta {
            armed_at,
            delay: Some("30m".into()),
            pid: None,
        };
        let timer = LoadedTask::new(
            "timer",
            TaskEntry {
                at: Some("10:12".into()),
                wake_meta: Some(meta.clone()),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        let wake = pending_wake("timer", &timer, &now).unwrap();
        assert_eq!(wake.armed_at, Some(armed_at));
        assert_eq!(
            wake.trigger,
            PendingWakeTrigger::Timer {
                due: "2026-06-01T10:12:00Z".parse().unwrap(),
                delay: Some("30m".into()),
            }
        );
        let command = "while kill -0 16776 2>/dev/null; do sleep 1; done";
        for pid in [None, Some(16776)] {
            let task = LoadedTask::new(
                "watch",
                TaskEntry {
                    watch: Some(command.into()),
                    wake_meta: Some(crate::config::WakeMeta {
                        delay: None,
                        pid,
                        ..meta.clone()
                    }),
                    ..TaskEntry::default()
                },
                TaskSource::Instance,
            );
            assert_eq!(
                pending_wake("watch", &task, &now).unwrap().trigger,
                match pid {
                    Some(pid) => PendingWakeTrigger::Pid { pid },
                    None => PendingWakeTrigger::Command {
                        command: command.into()
                    },
                }
            );
        }
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
                        pid: None,
                    }),
                    ..TaskEntry::default()
                },
                TaskSource::Instance,
            );
            assert_eq!(
                pending_wake("timer", &task, &now).unwrap().trigger,
                PendingWakeTrigger::Timer {
                    due: expected.parse().unwrap(),
                    delay: None,
                },
            );
        }
    }
}
