//! Account-shared ledger publication, compatibility, migration, and maintenance.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use sha2::{Digest, Sha256};

use crate::agents::provider_file;
use crate::disk::atomic::{self, AtomicErr};
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::disk::paths::{RuntimePaths, SpendingHistoryPaths};

const FORMAT: u32 = 1;
const DIRECTORY_MODE: u32 = 0o2775;
pub(super) const FILE_MODE: u32 = 0o664;

#[derive(Debug, thiserror::Error)]
pub(super) enum HistoryErr {
    #[error(transparent)]
    Atomic(#[from] AtomicErr),
    #[error(transparent)]
    Lock(#[from] LockErr),
}

pub(super) struct Maintenance {
    _lock: WorkspaceLock,
    pub(super) paths: SpendingHistoryPaths,
}

pub(super) fn try_maintain(
    paths: &SpendingHistoryPaths,
) -> Result<Option<Maintenance>, HistoryErr> {
    prepare(&paths.history_root)?;
    let Some(lock) = WorkspaceLock::try_acquire_mode(&paths.maintain_lock(), FILE_MODE)? else {
        return Ok(None);
    };
    match std::fs::read_to_string(paths.format()) {
        Ok(mark) if mark.trim().parse::<u32>().is_ok_and(|mark| mark <= FORMAT) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            provider_file::write_bytes_mode(
                &paths.format(),
                format!("{FORMAT}\n").as_bytes(),
                FILE_MODE,
            )?;
        }
        _ => return Ok(None),
    }
    Ok(Some(Maintenance {
        _lock: lock,
        paths: paths.clone(),
    }))
}

impl Maintenance {
    pub(super) fn maintain(&self, now_secs: u64) -> Result<(), AtomicErr> {
        super::session_roles::prune_segments(self, now_secs)?;
        super::session_effort::maintain(self, now_secs)?;
        super::closed_days::expire(self, now_secs)
    }
}

pub(super) fn maintain(paths: &SpendingHistoryPaths, now_secs: u64) -> Option<Maintenance> {
    let hold = match try_maintain(paths) {
        Ok(hold) => hold,
        Err(error) => {
            tracing::warn!(%error, "failed to open shared spending history maintenance");
            return None;
        }
    };
    if let Some(hold) = &hold
        && let Err(error) = hold.maintain(now_secs)
    {
        tracing::warn!(%error, "failed to maintain shared spending history");
    }
    hold
}

fn prepare(path: &Path) -> Result<(), AtomicErr> {
    atomic::create_dir_all_mode(path, DIRECTORY_MODE)?;
    if let Some(parent) = path.parent()
        && let Ok(parent) = std::fs::metadata(parent)
        && let Ok(directory) = std::fs::metadata(path)
        && directory.gid() != parent.gid()
        && std::os::unix::fs::chown(path, None, Some(parent.gid())).is_ok()
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIRECTORY_MODE))
            .map_err(|source| io(path, source))?;
    }
    Ok(())
}

pub(super) fn append(
    paths: &SpendingHistoryPaths,
    path: &Path,
    bytes: &[u8],
) -> Result<(), AtomicErr> {
    prepare(&paths.history_root)?;
    if let Some(parent) = path.parent() {
        prepare(parent)?;
    }
    atomic::append_record_bytes_mode(path, bytes, FILE_MODE)?;
    atomic::sync_file_data(path)
}

fn io(path: &Path, source: std::io::Error) -> AtomicErr {
    AtomicErr::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub(super) fn segments(directory: &Path) -> Result<Vec<PathBuf>, AtomicErr> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io(directory, source)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io(directory, source))?;
        let path = entry.path();
        if segment_stem(&path).is_some()
            && entry
                .file_type()
                .map_err(|source| io(&path, source))?
                .is_file()
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn segment_stem(path: &Path) -> Option<&str> {
    if path.extension()? != "jsonl" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let Some((stem, tag)) = stem.split_once('.') else {
        return Some(stem);
    };
    (tag.len() == 16
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(stem)
}

pub(super) fn segment_year(path: &Path) -> Option<i16> {
    let stem = segment_stem(path)?;
    if stem.len() != 4 || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

pub(super) fn segment_date(path: &Path) -> Option<Date> {
    let stem = segment_stem(path)?;
    let date = stem.parse::<Date>().ok()?;
    (date.to_string() == stem).then_some(date)
}

pub(super) fn migrate(runtime: &RuntimePaths, now_secs: u64) -> Result<(), AtomicErr> {
    let Some(now) = i64::try_from(now_secs)
        .ok()
        .and_then(|second| Timestamp::from_second(second).ok())
    else {
        return Ok(());
    };
    let year = now.to_zoned(TimeZone::UTC).year();
    for directory in [
        runtime.legacy_session_roles(),
        runtime.legacy_session_effort(),
    ] {
        for source in segments(&directory)? {
            let (destination, retained) = if directory == runtime.legacy_session_roles() {
                let Some(segment_year) = segment_year(&source) else {
                    continue;
                };
                (
                    runtime.spending_history.roles(segment_year),
                    segment_year >= year - 1,
                )
            } else {
                let Some(date) = segment_date(&source) else {
                    continue;
                };
                let end = date
                    .at(0, 0, 0, 0)
                    .to_zoned(TimeZone::UTC)
                    .map(|start| start.timestamp().as_second() + 86_400)
                    .ok()
                    .and_then(|end| u64::try_from(end).ok());
                (
                    runtime.spending_history.effort(date),
                    end.is_some_and(|end| {
                        end > now_secs.saturating_sub(super::session_effort::RETAIN_SECS)
                    }),
                )
            };
            let bytes = std::fs::read(&source).map_err(|error| io(&source, error))?;
            if retained && !bytes.is_empty() {
                let home = std::fs::canonicalize(&runtime.spending_legacy_home)
                    .map_err(|error| io(&runtime.spending_legacy_home, error))?;
                let mut hash = Sha256::new();
                hash.update(home.as_os_str().as_encoded_bytes());
                hash.update([0]);
                hash.update(&bytes);
                let tag = hex::encode(&hash.finalize()[..8]);
                let target = SpendingHistoryPaths::tagged_segment(&destination, &tag);
                prepare(&runtime.spending_history.history_root)?;
                if let Some(parent) = target.parent() {
                    prepare(parent)?;
                }
                if target.try_exists().map_err(|error| io(&target, error))? {
                    if let Some(parent) = target.parent() {
                        atomic::sync_dir(parent)?;
                    }
                } else {
                    atomic::write_bytes_if_absent_mode(&target, &bytes, FILE_MODE)?;
                }
            }
            std::fs::remove_file(&source).map_err(|error| io(&source, error))?;
            atomic::sync_dir(&directory)?;
        }
        match std::fs::remove_dir(&directory) {
            Ok(()) => {
                if let Some(parent) = directory.parent() {
                    atomic::sync_dir(parent)?;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(io(&directory, error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::spending::session_effort::{SessionEffortEvent, SessionEffortRecord};
    use crate::agents::spending::session_roles::{SessionRole, SessionRoleRecord};
    use crate::agents::spending::{session_effort, session_roles};
    use crate::ids::{AgentKind, WorkspaceId};
    use jiff::Timestamp;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, RuntimePaths, u64) {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let now: Timestamp = "2026-06-01T12:00:00Z".parse().unwrap();
        (dir, paths, now.as_second() as u64)
    }

    fn effort(at: &str) -> SessionEffortRecord {
        SessionEffortRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            at: at.parse().unwrap(),
            event: SessionEffortEvent::Active { active_ms: 123 },
        }
    }

    fn write_record(path: &Path, record: &impl serde::Serialize) {
        let mut bytes = serde_json::to_vec(record).unwrap();
        bytes.push(b'\n');
        provider_file::write_bytes(path, &bytes).unwrap();
    }

    mod permissions;

    #[test]
    fn maintenance_obeys_format_lock_and_retention() {
        let (_dir, runtime, now) = fixture();
        let paths = &runtime.spending_history;
        let old = paths.effort("2026-05-01".parse().unwrap());
        let expired = paths.effort("2024-01-01".parse().unwrap());
        let old_role = paths.roles(2024);
        write_record(&old, &effort("2026-05-01T12:00:00Z"));
        write_record(&expired, &effort("2024-01-01T12:00:00Z"));
        write_record(
            &old_role,
            &SessionRoleRecord {
                kind: AgentKind::new_unchecked("claude"),
                session: "session".into(),
                at: "2024-01-01T12:00:00Z".parse().unwrap(),
                role: SessionRole::Subagent {
                    profile: "old".into(),
                    owner: None,
                },
            },
        );
        let raw = std::fs::read(&old).unwrap();
        let unknown = paths.roles_dir().join("2024.not-a-migration.jsonl");
        provider_file::write_bytes(&unknown, b"keep unknown files").unwrap();
        for mark in [b"2\n".as_slice(), b"unreadable\n"] {
            provider_file::write_bytes(&paths.format(), mark).unwrap();
            assert!(
                try_maintain(paths).unwrap().is_none(),
                "unknown formats must not admit maintenance"
            );
            assert_eq!(std::fs::read(&old).unwrap(), raw);
            assert!(expired.exists());
            assert!(old_role.exists());
            let at = Timestamp::now();
            let allowed = SessionRoleRecord {
                kind: AgentKind::new_unchecked("claude"),
                session: "append-under-new-format".into(),
                at,
                role: SessionRole::Subagent {
                    profile: "allowed".into(),
                    owner: None,
                },
            };
            session_roles::record(&runtime, &allowed).unwrap();
            let added = SessionEffortRecord {
                session: allowed.session.clone(),
                at,
                ..effort("2026-05-01T12:00:00Z")
            };
            session_effort::record(&runtime, &added).unwrap();
            let index = session_roles::load_history(paths, at.as_second() as u64);
            assert_eq!(
                index
                    .roles
                    .get(&allowed.kind)
                    .and_then(|rows| rows.get(&allowed.session)),
                Some(&allowed.role)
            );
            assert!(
                index.effort.iter().any(|segment| segment.contains(&added)),
                "newer marks must not block effort appends or reads"
            );
            assert_eq!(std::fs::read(paths.format()).unwrap(), mark);
        }
        std::fs::remove_file(paths.format()).unwrap();
        std::fs::create_dir(paths.format()).unwrap();
        assert!(
            try_maintain(paths).unwrap().is_none(),
            "an unreadable mark must forbid maintenance"
        );
        std::fs::remove_dir(paths.format()).unwrap();
        let hold = try_maintain(paths)
            .unwrap()
            .expect("missing format must admit its creator");
        assert!(
            paths.format().is_file(),
            "maintenance must create the format mark"
        );
        assert_eq!(std::fs::read(paths.format()).unwrap(), b"1\n");
        assert!(
            try_maintain(paths).unwrap().is_none(),
            "another maintainer must not wait"
        );
        hold.maintain(now).unwrap();
        assert!(!expired.exists(), "expired effort must be removed");
        assert!(!old_role.exists(), "expired role years must be removed");
        assert!(
            unknown.exists(),
            "unrecognized segment names must never be removed"
        );
        assert!(matches!(
            session_effort::read_segment(&old)[0].event,
            SessionEffortEvent::Totals { active_ms: 123, .. }
        ));
        drop(hold);
        assert!(try_maintain(paths).unwrap().is_some());
    }

    #[test]
    fn migration_is_idempotent_and_recovers_copy_before_remove() {
        let (dir, runtime, now) = fixture();
        let roles = runtime
            .shared_spending_cursor_path()
            .with_file_name("session-roles");
        let efforts = runtime
            .shared_spending_cursor_path()
            .with_file_name("session-effort");
        let role = SessionRoleRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            at: "2026-05-01T12:00:00Z".parse().unwrap(),
            role: SessionRole::Subagent {
                profile: "fast".into(),
                owner: None,
            },
        };
        let record = effort("2026-05-01T12:00:00Z");
        write_record(&roles.join("2026.jsonl"), &role);
        write_record(&efforts.join("2026-05-01.jsonl"), &record);
        write_record(&roles.join("2024.jsonl"), &role);
        write_record(&efforts.join("2024-01-01.jsonl"), &record);
        provider_file::write_bytes(&efforts.join("2026-05-02.jsonl"), b"").unwrap();
        migrate(&runtime, now).unwrap();
        assert!(
            !roles.exists(),
            "migrated legacy directories must be removed"
        );
        assert!(!efforts.exists());
        let copies = || {
            std::fs::read_dir(runtime.spending_history.effort_dir())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        };
        let first = copies();
        assert_eq!(first.len(), 1, "expired and empty files are not copied");
        assert_eq!(
            session_effort::read_segment(&first[0]),
            vec![record.clone()]
        );
        let credit = || {
            let index = session_roles::load_history(&runtime.spending_history, now);
            super::super::aggregate_walk_publish(
                &[],
                &Default::default(),
                &[],
                now,
                None,
                &Default::default(),
                super::super::HistoryContext {
                    roles: Some(&index),
                    ..Default::default()
                },
            )
            .roles
            .subagents
            .get("fast")
            .map(|tally| tally.effort.year.active_ms)
        };
        assert_eq!(credit(), Some(123), "the fold must read migrated effort");
        let name = first[0].file_stem().unwrap().to_str().unwrap();
        let (date, tag) = name.split_once('.').unwrap();
        assert_eq!(date, "2026-05-01");
        assert_eq!(tag.len(), 16);
        assert!(
            tag.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        migrate(&runtime, now).unwrap();
        assert_eq!(copies(), first, "a second migration is a no-op");
        write_record(&efforts.join("2026-05-01.jsonl"), &record);
        assert_eq!(
            credit(),
            Some(123),
            "a remaining legacy original must not duplicate effort"
        );
        migrate(&runtime, now).unwrap();
        assert_eq!(
            credit(),
            Some(123),
            "migration retry must preserve consumer credit"
        );
        assert_eq!(
            copies(),
            first,
            "copy-before-remove recovery must not duplicate effort"
        );
        write_record(
            &efforts.join("2026-05-01.jsonl"),
            &SessionEffortRecord {
                event: SessionEffortEvent::FromUser,
                ..record
            },
        );
        migrate(&runtime, now).unwrap();
        assert_eq!(
            copies().len(),
            2,
            "a recreated legacy segment has its own content tag"
        );
        let mut other = RuntimePaths::under(
            WorkspaceId::from_project_root(Path::new("/other")),
            &dir.path().join("another-home"),
        )
        .unwrap();
        other.spending_history = runtime.spending_history.clone();
        let other_legacy = other
            .shared_spending_cursor_path()
            .with_file_name("session-effort")
            .join("2026-05-01.jsonl");
        write_record(&other_legacy, &effort("2026-05-01T12:00:00Z"));
        migrate(&other, now).unwrap();
        assert_eq!(
            copies().len(),
            3,
            "distinct canonical homes must not share migration tags"
        );
    }

    #[test]
    fn shared_store_modes_ignore_umask() {
        if std::env::var_os("RIMZ_TEST_HISTORY_MODES").is_none() {
            let output = std::process::Command::new("sh")
                .args([
                    "-c",
                    "umask 077; exec \"$1\" --exact \"$2\" --nocapture",
                    "sh",
                ])
                .arg(std::env::current_exe().unwrap())
                .arg("agents::spending::history::tests::shared_store_modes_ignore_umask")
                .env("RIMZ_TEST_HISTORY_MODES", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (_dir, runtime, now) = fixture();
        session_roles::record(
            &runtime,
            &SessionRoleRecord {
                kind: AgentKind::new_unchecked("claude"),
                session: "session".into(),
                at: "2026-05-01T12:00:00Z".parse().unwrap(),
                role: SessionRole::Subagent {
                    profile: "fast".into(),
                    owner: None,
                },
            },
        )
        .unwrap();
        session_effort::record(&runtime, &effort("2026-05-01T12:00:00Z")).unwrap();
        assert!(
            runtime.spending_history.roles_dir().is_dir(),
            "hook appends must use shared history"
        );
        let legacy = runtime
            .shared_spending_cursor_path()
            .with_file_name("session-effort")
            .join("2026-05-01.jsonl");
        write_record(&legacy, &effort("2026-05-01T12:00:00Z"));
        migrate(&runtime, now).unwrap();
        let paths = &runtime.spending_history;
        let hold = try_maintain(paths).unwrap().unwrap();
        hold.maintain(now).unwrap();
        for directory in [&paths.history_root, &paths.roles_dir(), &paths.effort_dir()] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o7777,
                0o2775
            );
            for entry in std::fs::read_dir(directory).unwrap() {
                let metadata = entry.unwrap().metadata().unwrap();
                if metadata.is_file() {
                    assert_eq!(metadata.permissions().mode() & 0o777, 0o664);
                }
            }
        }
    }

    #[test]
    fn earliest_role_wins_across_tagged_files() {
        let (_dir, runtime, now) = fixture();
        let paths = &runtime.spending_history;
        let later = SessionRoleRecord {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            at: "2026-05-02T12:00:00Z".parse().unwrap(),
            role: SessionRole::Subagent {
                profile: "later".into(),
                owner: None,
            },
        };
        let earlier = SessionRoleRecord {
            at: "2026-05-01T12:00:00Z".parse().unwrap(),
            role: SessionRole::Subagent {
                profile: "earlier".into(),
                owner: None,
            },
            ..later.clone()
        };
        for (first_tag, second_tag) in [
            ("0000000000000000", "ffffffffffffffff"),
            ("ffffffffffffffff", "0000000000000000"),
        ] {
            write_record(
                &SpendingHistoryPaths::tagged_segment(&paths.roles(2026), first_tag),
                &later,
            );
            write_record(
                &SpendingHistoryPaths::tagged_segment(&paths.roles(2026), second_tag),
                &earlier,
            );
            let index = session_roles::load_history(paths, now);
            assert_eq!(
                index
                    .roles
                    .get(&earlier.kind)
                    .and_then(|rows| rows.get(&earlier.session)),
                Some(&earlier.role),
                "file order must not decide provenance"
            );
        }
        let original = std::fs::read(paths.roles(2026)).ok();
        session_roles::record(&runtime, &later).unwrap();
        assert_eq!(
            std::fs::read(paths.roles(2026)).ok(),
            original,
            "a tagged current-year role prevents another append"
        );
    }

    #[test]
    fn tagged_effort_compacts_in_place_without_rewriting_its_sibling() {
        let (_dir, runtime, now) = fixture();
        let paths = &runtime.spending_history;
        let sibling = paths.effort("2026-05-01".parse().unwrap());
        let tagged = SpendingHistoryPaths::tagged_segment(&sibling, "0123456789abcdef");
        write_record(&tagged, &effort("2026-05-01T12:00:00Z"));
        let compacted = SessionEffortRecord {
            event: SessionEffortEvent::Totals {
                active_ms: 7,
                from_user: 0,
                from_teammates: 0,
                to_teammates: 0,
            },
            ..effort("2026-05-01T12:00:00Z")
        };
        write_record(&sibling, &compacted);
        let sibling_bytes = std::fs::read(&sibling).unwrap();
        try_maintain(paths).unwrap().unwrap().maintain(now).unwrap();
        assert!(
            matches!(
                session_effort::read_segment(&tagged)[0].event,
                SessionEffortEvent::Totals { active_ms: 123, .. }
            ),
            "tagged migration copies must compact under their own names"
        );
        assert_eq!(std::fs::read(&sibling).unwrap(), sibling_bytes);
        let loaded = session_effort::load(paths, now);
        assert_eq!(loaded.iter().map(|records| records.len()).sum::<usize>(), 2);
    }

    #[test]
    fn another_homes_walk_joins_shared_roles_and_effort() {
        use crate::agents::spending::{
            HeadlineSpec, SilentWalk, SpendingFile, SpendingWalker, WalkRequest,
        };
        let (dir, runtime, _) = fixture();
        let at = Timestamp::now();
        let now = at.as_second() as u64;
        let mut other = RuntimePaths::under(
            WorkspaceId::from_project_root(Path::new("/other")),
            &dir.path().join("other"),
        )
        .unwrap();
        other.spending_history = runtime.spending_history.clone();
        assert_ne!(
            runtime.shared_spending_cursor_path(),
            other.shared_spending_cursor_path()
        );
        let session = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        session_roles::record(
            &runtime,
            &SessionRoleRecord {
                session: session.into(),
                kind: AgentKind::new_unchecked("claude"),
                at,
                role: SessionRole::Subagent {
                    profile: "shared".into(),
                    owner: None,
                },
            },
        )
        .unwrap();
        let record = SessionEffortRecord {
            session: session.into(),
            at,
            ..effort("2026-06-01T11:00:00Z")
        };
        session_effort::record(&runtime, &record).unwrap();
        let transcript = dir.path().join(format!("{session}.jsonl"));
        write_record(
            &transcript,
            &serde_json::json!({
                "timestamp": at, "costUSD": 2.0,
                "message": {"usage": {"input_tokens": 10, "output_tokens": 5}}
            }),
        );
        let adapter = crate::agents::definition_by_kind("claude").unwrap();
        let files = [SpendingFile {
            adapter,
            login: crate::ids::LoginKey::default_for(adapter.spec().kind_id()),
            path: transcript,
        }];
        let result = SpendingWalker::new().walk_local(
            &other.shared_spending_cursor_path(),
            &WalkRequest {
                history: &other.spending_history,
                files: &files,
                prices: &Default::default(),
                now_secs: now,
                origin_overrides: &Default::default(),
                user_inputs: &[],
                scope: None,
                spec: &HeadlineSpec::default(),
            },
            &mut SilentWalk,
        );
        let tally = result.roles.subagents.get("shared");
        assert_eq!(
            tally.map(|tally| tally.tally.year.usd),
            Some(2.0),
            "the consuming home's fold must see the other home's provenance"
        );
        assert_eq!(tally.map(|tally| tally.effort.year.active_ms), Some(123));
    }
}
