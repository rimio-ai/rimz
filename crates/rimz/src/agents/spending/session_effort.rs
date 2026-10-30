//! Durable, account-shared active-time releases and conversation counts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, AtomicErr};
use crate::disk::parse_cache::{ParseCache, StampedPath};
use crate::disk::paths::{RuntimePaths, SpendingHistoryPaths};
use crate::ids::{AgentKind, AgentSessionId};

use super::{EffortWindow, RoleTallies, aggregate, session_roles};

pub(super) const RETAIN_SECS: u64 = 366 * 86_400;

// Only existing daily segments from one root within the 366-day retention window survive a load.
static SEGMENTS: Mutex<BTreeMap<PathBuf, ParseCache<Vec<SessionEffortRecord>>>> =
    Mutex::new(BTreeMap::new());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEffortRecord {
    pub kind: AgentKind,
    pub session: AgentSessionId,
    pub at: Timestamp,
    #[serde(flatten)]
    pub event: SessionEffortEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEffortEvent {
    Active {
        active_ms: u64,
    },
    FromUser,
    FromAgent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_kind: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_session: Option<AgentSessionId>,
    },
    Totals {
        active_ms: u64,
        from_user: u64,
        from_teammates: u64,
        to_teammates: u64,
    },
}

impl SessionEffortRecord {
    fn credits(&self) -> impl Iterator<Item = (&AgentKind, &AgentSessionId, EffortWindow)> {
        let mut received = EffortWindow::default();
        let mut sender = None;
        match &self.event {
            SessionEffortEvent::Active { active_ms } => received.active_ms = *active_ms,
            SessionEffortEvent::FromUser => received.from_user = 1,
            SessionEffortEvent::FromAgent {
                sender_kind,
                sender_session,
            } => {
                received.from_teammates = 1;
                sender =
                    sender_kind
                        .as_ref()
                        .zip(sender_session.as_ref())
                        .map(|(kind, session)| {
                            (
                                kind,
                                session,
                                EffortWindow {
                                    to_teammates: 1,
                                    ..Default::default()
                                },
                            )
                        });
            }
            SessionEffortEvent::Totals {
                active_ms,
                from_user,
                from_teammates,
                to_teammates,
            } => {
                received = EffortWindow {
                    active_ms: *active_ms,
                    from_user: *from_user,
                    from_teammates: *from_teammates,
                    to_teammates: *to_teammates,
                };
            }
        }
        std::iter::once((&self.kind, &self.session, received)).chain(sender)
    }
}

#[must_use = "durability barrier; check the result"]
pub fn record(paths: &RuntimePaths, record: &SessionEffortRecord) -> Result<(), AtomicErr> {
    let date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let path = paths.shared_session_effort(date);
    // Separate a torn prior tail without reading the segment on the hook path.
    let mut bytes = vec![b'\n'];
    serde_json::to_writer(&mut bytes, record)?;
    bytes.push(b'\n');
    super::history::append(&paths.spending_history, &path, &bytes)
}

/// Decode intact records, ignoring unknown fields and malformed lines.
/// Missing or unreadable segments contribute no records.
pub fn read_segment(path: &Path) -> Vec<SessionEffortRecord> {
    try_read_segment(path).unwrap_or_default()
}

fn try_read_segment(path: &Path) -> std::io::Result<Vec<SessionEffortRecord>> {
    let bytes = std::fs::read(path)?;
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

pub(super) fn load(
    paths: &SpendingHistoryPaths,
    now_secs: u64,
) -> Vec<Arc<Vec<SessionEffortRecord>>> {
    segments(paths, now_secs)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, end)| *end > now_secs.saturating_sub(RETAIN_SECS))
        .map(|(path, _)| read_memo(&path))
        .collect()
}

fn segments(
    paths: &SpendingHistoryPaths,
    now_secs: u64,
) -> Result<BTreeMap<PathBuf, u64>, AtomicErr> {
    let mut segments = BTreeMap::new();
    let Some(now) = i64::try_from(now_secs)
        .ok()
        .and_then(|second| Timestamp::from_second(second).ok())
    else {
        return Ok(segments);
    };
    let today = now.to_zoned(TimeZone::UTC).date();
    for path in super::history::segments(&paths.effort_dir())? {
        let Some(date) = super::history::segment_date(&path) else {
            continue;
        };
        if date > today {
            continue;
        }
        let Ok(start) = date.at(0, 0, 0, 0).to_zoned(TimeZone::UTC) else {
            continue;
        };
        let Ok(end) = u64::try_from(start.timestamp().as_second() + 86_400) else {
            continue;
        };
        segments.insert(path, end);
    }
    SEGMENTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|path, _| {
            segments
                .get(path)
                .is_some_and(|end| *end > now_secs.saturating_sub(RETAIN_SECS))
        });
    Ok(segments)
}

fn read_memo(path: &Path) -> Arc<Vec<SessionEffortRecord>> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Arc::default();
    };
    let stamped = StampedPath {
        path: path.to_path_buf(),
        stamp: crate::disk::parse_cache::FileStamp::from_metadata(&metadata),
    };
    let mut memo = SEGMENTS.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = memo
        .entry(path.to_path_buf())
        .or_insert_with(ParseCache::new);
    if let Some(records) = slot.get_stamped(&stamped) {
        return records;
    }
    let Ok(records) = try_read_segment(path) else {
        return Arc::default();
    };
    let records = Arc::new(records);
    slot.store_stamped(&stamped, Arc::clone(&records));
    records
}

pub(super) fn fold(
    index: &session_roles::RoleIndex,
    tallies: &mut RoleTallies,
    now_secs: u64,
    window_starts: super::WindowStarts,
) {
    for record in index.effort.iter().flat_map(|segment| segment.iter()) {
        let Ok(second) = u64::try_from(record.at.as_second()) else {
            continue;
        };
        tallies.effort_since_secs = tallies
            .effort_since_secs
            .into_iter()
            .chain(Some(second))
            .min();
        if !aggregate::within_widest_window(second, now_secs) {
            continue;
        }
        for (kind, session, credit) in record.credits() {
            let Some(role) = index
                .roles
                .get(kind)
                .and_then(|sessions| sessions.get(session))
            else {
                continue;
            };
            let owner = role
                .owner_placement()
                .filter(|_| credit.active_ms > 0)
                .map(|owner| {
                    (
                        owner,
                        EffortWindow {
                            active_ms: credit.active_ms,
                            ..Default::default()
                        },
                    )
                });
            for (placement, credit) in
                std::iter::once((role.placement(kind.as_str()), credit)).chain(owner)
            {
                let effort = &mut tallies.entry(placement).effort;
                if second >= window_starts.year {
                    effort.year.merge(credit);
                }
                if second >= window_starts.month {
                    effort.month.merge(credit);
                }
                if second >= window_starts.week {
                    effort.week.merge(credit);
                }
                if second >= window_starts.day {
                    effort.day.merge(credit);
                }
            }
        }
    }
}

pub(super) fn maintain(hold: &super::history::Maintenance, now_secs: u64) -> Result<(), AtomicErr> {
    let segments = segments(&hold.paths, now_secs)?;
    let mut removed = false;
    for (path, end) in &segments {
        if *end <= now_secs.saturating_sub(RETAIN_SECS) {
            match std::fs::remove_file(path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(AtomicErr::Io {
                        path: path.clone(),
                        source,
                    });
                }
            }
            continue;
        }
        if *end > now_secs.saturating_sub(aggregate::RAW_RETAIN_SECS) {
            continue;
        }
        let records = read_memo(path);
        if records
            .iter()
            .all(|record| matches!(record.event, SessionEffortEvent::Totals { .. }))
        {
            continue;
        }
        let mut totals = BTreeMap::<_, (Timestamp, EffortWindow)>::new();
        for record in records.iter() {
            let date = record.at.to_zoned(TimeZone::UTC).date();
            for (kind, session, credit) in record.credits() {
                let (at, total) = totals
                    .entry((kind.clone(), session.clone(), date))
                    .or_insert((record.at, EffortWindow::default()));
                *at = (*at).max(record.at);
                total.merge(credit);
            }
        }
        let mut bytes = Vec::new();
        for ((kind, session, _), (at, total)) in totals {
            serde_json::to_writer(
                &mut bytes,
                &SessionEffortRecord {
                    kind,
                    session,
                    at,
                    event: SessionEffortEvent::Totals {
                        active_ms: total.active_ms,
                        from_user: total.from_user,
                        from_teammates: total.from_teammates,
                        to_teammates: total.to_teammates,
                    },
                },
            )?;
            bytes.push(b'\n');
        }
        super::super::provider_file::write_bytes_mode(path, &bytes, super::history::FILE_MODE)?;
    }
    if removed && let Some(directory) = segments.keys().next().and_then(|path| path.parent()) {
        atomic::sync_dir(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn append_after_a_torn_tail_preserves_complete_records_and_decodes_totals() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        let path = paths.shared_session_effort(date);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"kind\":\"claude\",\"session\":\"old\",\"at\":\"2026-01-01T00:00:00Z\",\"event\":\"totals\",\"active_ms\":20,\"from_user\":1,\"from_teammates\":2,\"to_teammates\":3,\"future\":true}\nnot json\n{\"kind\":").unwrap();
        let appended = SessionEffortRecord {
            kind: AgentKind::new_unchecked("codex"),
            session: "new".into(),
            at: "2025-12-31T23:59:59Z".parse().unwrap(),
            event: SessionEffortEvent::Active { active_ms: 30 },
        };
        record(&paths, &appended).unwrap();
        let records = read_segment(&path);
        assert_eq!(
            records.len(),
            2,
            "only the malformed/torn records may be lost"
        );
        assert!(matches!(
            records[0].event,
            SessionEffortEvent::Totals {
                active_ms: 20,
                from_user: 1,
                from_teammates: 2,
                to_teammates: 3
            }
        ));
        assert_eq!(
            records[1], appended,
            "a replayed event lands in today's segment"
        );
    }

    fn segment_fixture(
        paths: &RuntimePaths,
        at: Timestamp,
        records: &[SessionEffortRecord],
    ) -> std::path::PathBuf {
        let path = paths.shared_session_effort(at.to_zoned(jiff::tz::TimeZone::UTC).date());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn segment_memo_reuses_closed_parses_and_reloads_only_changed_segments() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let now = Timestamp::from_second(1_750_000_000).unwrap();
        let old = now
            .checked_sub(jiff::SignedDuration::from_hours(240))
            .unwrap();
        let record = SessionEffortRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            at: now,
            event: SessionEffortEvent::Active { active_ms: 10 },
        };
        let closed = segment_fixture(
            &paths,
            old,
            &[SessionEffortRecord {
                at: old,
                ..record.clone()
            }],
        );
        let current = segment_fixture(&paths, now, std::slice::from_ref(&record));
        let history = paths.spending_history.clone();
        let now_secs = now.as_second() as u64;
        let first = load(&history, now_secs);
        assert_eq!(first.len(), 2, "both retained segments must load");
        let second = load(&history, now_secs);
        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert!(Arc::ptr_eq(&first[1], &second[1]));
        let other_history = history.clone();
        let other_thread = std::thread::spawn(move || load(&other_history, now_secs))
            .join()
            .unwrap();
        assert!(
            Arc::ptr_eq(&first[0], &other_thread[0]),
            "a closed segment must share its parse across walking threads"
        );

        let changed = SessionEffortRecord {
            event: SessionEffortEvent::Active { active_ms: 20 },
            ..record.clone()
        };
        segment_fixture(&paths, now, std::slice::from_ref(&changed));
        std::fs::File::open(&current)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        let third = load(&history, now_secs);
        assert!(
            Arc::ptr_eq(&first[0], &third[0]),
            "closed segment must not reparse"
        );
        assert!(
            !Arc::ptr_eq(&first[1], &third[1]),
            "same-length mtime change must reparse"
        );
        assert_eq!(third[1][0], changed);

        segment_fixture(&paths, now, &[record, changed]);
        std::fs::File::open(&current)
            .unwrap()
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        let fourth = load(&history, now_secs);
        assert!(Arc::ptr_eq(&third[0], &fourth[0]));
        assert!(
            !Arc::ptr_eq(&third[1], &fourth[1]),
            "a length change must reparse"
        );
        assert_eq!(fourth[1].len(), 2);
        std::fs::remove_file(closed).unwrap();
        assert_eq!(
            load(&history, now_secs).len(),
            1,
            "deleted segments leave the memo"
        );
    }

    #[test]
    fn compaction_preserves_effort_windows_sender_credit_and_young_segments() {
        use super::super::session_roles::{self, SessionRole, SessionRoleRecord};
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let now = Timestamp::from_second(1_750_000_000).unwrap();
        let now_secs = now.as_second() as u64;
        let old = now
            .checked_sub(jiff::SignedDuration::from_hours(240))
            .unwrap();
        let previous = old
            .checked_sub(jiff::SignedDuration::from_hours(24))
            .unwrap();
        for (kind, session) in [("claude", "receiver"), ("codex", "sender")] {
            session_roles::record(
                &paths,
                &SessionRoleRecord {
                    kind: AgentKind::new_unchecked(kind),
                    session: session.into(),
                    at: now,
                    role: SessionRole::Subagent {
                        profile: session.into(),
                        owner: None,
                    },
                },
            )
            .unwrap();
        }
        let record = SessionEffortRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "receiver".into(),
            at: old,
            event: SessionEffortEvent::Active { active_ms: 100 },
        };
        let old_path = segment_fixture(
            &paths,
            old,
            &[
                record.clone(),
                SessionEffortRecord {
                    at: old.checked_sub(jiff::SignedDuration::from_secs(1)).unwrap(),
                    event: SessionEffortEvent::FromUser,
                    ..record.clone()
                },
                SessionEffortRecord {
                    event: SessionEffortEvent::FromAgent {
                        sender_kind: Some(AgentKind::new_unchecked("codex")),
                        sender_session: Some("sender".into()),
                    },
                    ..record.clone()
                },
                SessionEffortRecord {
                    at: previous,
                    event: SessionEffortEvent::Totals {
                        active_ms: 50,
                        from_user: 2,
                        from_teammates: 3,
                        to_teammates: 4,
                    },
                    ..record.clone()
                },
            ],
        );
        let young = now
            .checked_sub(jiff::SignedDuration::from_hours(7 * 24))
            .unwrap();
        let young_path = segment_fixture(
            &paths,
            young,
            &[SessionEffortRecord {
                at: young,
                ..record.clone()
            }],
        );
        let expired = now
            .checked_sub(jiff::SignedDuration::from_hours(368 * 24))
            .unwrap();
        let expired_path = segment_fixture(
            &paths,
            expired,
            &[SessionEffortRecord {
                at: expired,
                ..record
            }],
        );
        let young_bytes = std::fs::read(&young_path).unwrap();
        let fold = || {
            let index = session_roles::load_history(&paths.spending_history, now_secs);
            super::super::aggregate_walk_publish(
                &[],
                &Default::default(),
                &[],
                now_secs,
                None,
                &super::super::HeadlineSpec {
                    timezone: Some("UTC".into()),
                    ..Default::default()
                },
                super::super::HistoryContext {
                    roles: Some(&index),
                    ..Default::default()
                },
            )
            .roles
        };
        let before = serde_json::to_value(fold()).unwrap();
        let hold = super::super::history::try_maintain(&paths.spending_history)
            .unwrap()
            .unwrap();
        maintain(&hold, now_secs).unwrap();
        let compacted = read_segment(&old_path);
        assert!(
            compacted
                .iter()
                .all(|row| matches!(row.event, SessionEffortEvent::Totals { .. })),
            "old segments must contain only totals"
        );
        assert_eq!(
            compacted.len(),
            3,
            "provider/session/UTC-day grouping includes sender credit"
        );
        assert_eq!(
            compacted
                .iter()
                .find(|row| row.session.as_str() == "sender")
                .unwrap()
                .event,
            SessionEffortEvent::Totals {
                active_ms: 0,
                from_user: 0,
                from_teammates: 0,
                to_teammates: 1
            }
        );
        let after = serde_json::to_value(fold()).unwrap();
        assert_eq!(
            after["subagents"], before["subagents"],
            "compaction preserves every effort window"
        );
        assert_eq!(
            after["subagents"]["receiver"]["effort"]["year"]["active_ms"],
            250
        );
        assert_eq!(
            std::fs::read(&young_path).unwrap(),
            young_bytes,
            "young segments stay byte-exact"
        );
        assert!(
            !expired_path.exists(),
            "segments past retention must be pruned"
        );
        let bytes = std::fs::read(&old_path).unwrap();
        let modified = std::fs::metadata(&old_path).unwrap().modified().unwrap();
        maintain(&hold, now_secs).unwrap();
        assert_eq!(std::fs::read(&old_path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&old_path).unwrap().modified().unwrap(),
            modified,
            "a second compaction is a no-op"
        );
    }
}
