//! Locked durable backlog mutations. Run-lock decisions share this lock.

use super::backlog::{self, Backlog};

#[derive(Debug, thiserror::Error)]
pub(super) enum BacklogWriteErr {
    #[error(transparent)]
    Read(#[from] backlog::BacklogErr),
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Write(#[from] crate::disk::atomic::AtomicErr),
}

pub(super) struct LockedBacklog<'a> {
    paths: &'a crate::StatePaths,
    _guard: crate::disk::lock::WorkspaceLock,
    pub(super) record: Backlog,
}

impl<'a> LockedBacklog<'a> {
    pub(super) fn acquire(paths: &'a crate::StatePaths) -> Result<Self, BacklogWriteErr> {
        let guard = hold(paths)?;
        Ok(Self {
            paths,
            _guard: guard,
            record: backlog::load(paths)?,
        })
    }

    pub(super) fn save(&mut self) -> Result<(), BacklogWriteErr> {
        self.record
            .tasks
            .retain(|_, task| !task.triggers.is_empty() || task.stopping.is_some());
        crate::disk::atomic::write_temp_then_rename(&backlog::path(self.paths), &self.record)?;
        Ok(())
    }
}

/// The backlog lock alone, for a release that must serialize even when the record is unreadable.
pub(super) fn hold(
    paths: &crate::StatePaths,
) -> Result<crate::disk::lock::WorkspaceLock, BacklogWriteErr> {
    Ok(crate::disk::lock::WorkspaceLock::acquire(
        &paths.lock_path("loop-backlog.lock"),
    )?)
}

pub(super) fn remove(paths: &crate::StatePaths, task: &str) -> Result<(), BacklogWriteErr> {
    if !backlog::load(paths)?.tasks.contains_key(task) {
        return Ok(());
    }
    let mut backlog = LockedBacklog::acquire(paths)?;
    if backlog.record.tasks.remove(task).is_some() {
        backlog.save()?;
    }
    Ok(())
}

pub(super) fn rename(
    paths: &crate::StatePaths,
    task: &str,
    name: &str,
) -> Result<(), BacklogWriteErr> {
    if !backlog::load(paths)?.tasks.contains_key(task) {
        return Ok(());
    }
    let mut backlog = LockedBacklog::acquire(paths)?;
    if let Some(waiting) = backlog.record.tasks.remove(task) {
        backlog.record.tasks.insert(name.to_owned(), waiting);
        backlog.save()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverlapPolicy;
    use backlog::{TaskBacklog, WaitingTrigger};
    use jiff::Timestamp;

    fn trigger(n: i64, payload: serde_json::Value) -> WaitingTrigger {
        WaitingTrigger {
            arrived_at: Timestamp::from_second(n).unwrap(),
            signal: payload
                .as_object()
                .map(|payload| super::super::run_log::SignalRecord {
                    name: "trunk.moved".parse().unwrap(),
                    payload: payload.clone(),
                }),
            absorbed: 0,
            from_arrived_at: None,
        }
    }

    #[test]
    fn coalesce_and_replace_keep_the_newest_slot_and_count_absorbed() {
        for policy in [OverlapPolicy::Coalesce, OverlapPolicy::Replace] {
            for order in [[1, 2, 3], [3, 2, 1], [3, 1, 2], [2, 3, 1]] {
                let mut task = TaskBacklog::default();
                for n in order {
                    assert!(task.enqueue(
                        policy,
                        10,
                        trigger(n, serde_json::json!({"from": n - 1, "to": n}))
                    ));
                    task = serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
                }
                assert_eq!(task.triggers.len(), 1);
                assert_eq!(task.triggers[0].arrived_at.as_second(), 3);
                assert_eq!(task.triggers[0].absorbed, 2);
                assert_eq!(
                    task.triggers[0].signal.as_ref().unwrap().payload,
                    *serde_json::json!({"from": 0, "to": 3}).as_object().unwrap(),
                    "{policy:?}: {order:?}"
                );
                assert!(task.stopping.is_none());
            }
        }
    }

    #[test]
    fn queue_keeps_arrival_order_and_refuses_past_limit() {
        let mut task = TaskBacklog::default();
        assert!(task.enqueue(OverlapPolicy::Queue, 2, trigger(2, serde_json::Value::Null)));
        assert!(task.enqueue(OverlapPolicy::Queue, 2, trigger(1, serde_json::Value::Null)));
        assert!(!task.enqueue(OverlapPolicy::Queue, 2, trigger(3, serde_json::Value::Null)));
        assert_eq!(
            task.triggers
                .iter()
                .map(|trigger| trigger.arrived_at.as_second())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(task.triggers.iter().all(|trigger| trigger.absorbed == 0));
    }

    #[test]
    fn merge_preserves_oldest_from_only_for_matching_signal_and_other_keys() {
        use serde_json::json;
        let old = json!({"repo":"r", "trunk":"main", "from":"a", "to":"b"});
        let latest = json!({"repo":"r", "trunk":"main", "from":"b", "to":"c"});
        let mut new = trigger(2, latest.clone());
        new.absorb(trigger(1, old.clone()));
        assert_eq!(
            new.signal.unwrap().payload,
            json!({"repo":"r", "trunk":"main", "from":"a", "to":"c"})
                .as_object()
                .unwrap()
                .clone()
        );
        for payload in [
            json!({"repo":"other", "from":"b", "to":"c"}),
            json!({"from":"b"}),
        ] {
            let mut new = trigger(2, payload.clone());
            new.absorb(trigger(1, old.clone()));
            assert_eq!(new.signal.unwrap().payload, *payload.as_object().unwrap());
            assert_eq!(new.absorbed, 1);
        }
        let mut new = trigger(2, latest.clone());
        let mut other = trigger(1, old.clone());
        other.signal.as_mut().unwrap().name = "team.stage".parse().unwrap();
        new.absorb(other);
        assert_eq!(new.signal.unwrap().payload, *latest.as_object().unwrap());
        let mut clock = trigger(2, serde_json::Value::Null);
        clock.absorb(trigger(1, old));
        assert!(clock.signal.is_none());
        assert_eq!(clock.absorbed, 1);
    }

    #[test]
    fn equal_stamps_keep_insertion_order_and_latest_merge_payload() {
        let mut task = TaskBacklog::default();
        for to in [1, 2] {
            assert!(task.enqueue(
                OverlapPolicy::Queue,
                10,
                trigger(1, serde_json::json!({"from": to - 1, "to": to}))
            ));
        }
        assert_eq!(task.triggers[0].signal.as_ref().unwrap().payload["to"], 1);
        assert_eq!(task.triggers[1].signal.as_ref().unwrap().payload["to"], 2);
        let mut newest = task.triggers.pop_back().unwrap();
        newest.absorb(task.triggers.pop_front().unwrap());
        assert_eq!(
            newest.signal.unwrap().payload,
            *serde_json::json!({"from": 0, "to": 2}).as_object().unwrap()
        );
    }

    #[test]
    fn backlog_is_durable_strict_and_prunes_empty_tasks_but_keeps_stop_marks() {
        let home = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
        {
            let mut locked = LockedBacklog::acquire(&paths).unwrap();
            locked.record.tasks.insert(
                "busy".into(),
                TaskBacklog {
                    triggers: [trigger(1, serde_json::Value::Null)].into(),
                    stopping: None,
                },
            );
            locked
                .record
                .tasks
                .insert("empty".into(), TaskBacklog::default());
            locked.record.tasks.insert(
                "stopped".into(),
                TaskBacklog {
                    stopping: Some(backlog::StopMark {
                        pid: 42,
                        started_at: Timestamp::UNIX_EPOCH,
                        cause: backlog::StopCause::Replace,
                    }),
                    ..Default::default()
                },
            );
            locked.save().unwrap();
        }
        let loaded = backlog::load(&paths).unwrap();
        assert_eq!(loaded.tasks.len(), 2);
        assert_eq!(loaded.tasks["busy"].triggers.len(), 1);
        assert_eq!(loaded.tasks["stopped"].stopping.as_ref().unwrap().pid, 42);
        std::fs::write(backlog::path(&paths), "{broken").unwrap();
        assert!(backlog::load(&paths).is_err());
        std::fs::write(backlog::path(&paths), r#"{"version":2,"tasks":{}}"#).unwrap();
        assert!(matches!(
            backlog::load(&paths),
            Err(backlog::BacklogErr::Version(2))
        ));
    }
}
