//! Session-record lookup, one-shot cross-mux live sets, mux choice, and
//! renamed-session retirement.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::ids::MuxName;
use crate::mux::MuxBackend;
use crate::store::workspace_record;
use crate::{RuntimePaths, StatePaths, WorkspaceRecord};
use anyhow::{Context, Result, bail};

const LIST_SESSIONS_ATTEMPTS: u8 = 3;
const LIST_SESSIONS_RETRY_DELAY: Duration = Duration::from_millis(250);
const SESSION_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const SESSION_PROBE_RETRY_TIMEOUT: Duration = Duration::from_secs(3);
const TEST_SESSION_PROBE_MS: &str = "RIMZ_TEST_SESSION_PROBE_MS";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingSessionReport {
    Silent,
    Warn,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MuxPick {
    pub mux: MuxName,
    pub notices: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct MuxPickErr {
    pub notices: Vec<String>,
    #[source]
    pub source: crate::mux::MuxErr,
}

/// Both backends' live session names, probed once for batch operations.
/// Missing backends and transient list failures contribute an empty set.
pub struct LiveSessions {
    zellij: HashSet<String>,
    tmux: HashSet<String>,
}

impl LiveSessions {
    pub fn probe() -> Self {
        let names = |mux| -> HashSet<String> {
            crate::mux::backend_for(mux)
                .list_sessions()
                .unwrap_or_default()
                .into_iter()
                .collect()
        };
        Self {
            zellij: names(MuxName::Zellij),
            tmux: names(MuxName::Tmux),
        }
    }

    /// Resolve a live session with Zellij-first precedence. An absent session
    /// maps to `None`, preserving best-effort empty-set fallback.
    pub fn mux_of(&self, session: &str) -> Option<MuxName> {
        if self.zellij.contains(session) {
            Some(MuxName::Zellij)
        } else if self.tmux.contains(session) {
            Some(MuxName::Tmux)
        } else {
            None
        }
    }
}

pub fn session_probe_timeout() -> Duration {
    test_session_probe_timeout().unwrap_or(SESSION_PROBE_TIMEOUT)
}

pub fn session_probe_retry_timeout() -> Duration {
    test_session_probe_timeout()
        .map(|duration| duration.saturating_mul(3))
        .unwrap_or(SESSION_PROBE_RETRY_TIMEOUT)
}

fn test_session_probe_timeout() -> Option<Duration> {
    let value = std::env::var_os(TEST_SESSION_PROBE_MS)?;
    let value = value.to_str()?.parse::<u64>().ok()?;
    Some(Duration::from_millis(value))
}

pub fn pick_mux_for_session(
    session: &str,
    explicit: Option<MuxName>,
    missing_report: MissingSessionReport,
) -> std::result::Result<MuxPick, MuxPickErr> {
    if let Some(mux) = explicit {
        return Ok(MuxPick {
            mux,
            notices: Vec::new(),
        });
    }
    let mut notices = Vec::new();
    for candidate in [MuxName::Zellij, MuxName::Tmux] {
        let backend = crate::mux::backend_for(candidate);
        match list_sessions_with_retry(backend.as_ref()) {
            Ok(sessions) if sessions.iter().any(|s| s == session) => {
                return Ok(MuxPick {
                    mux: candidate,
                    notices,
                });
            }
            Ok(_) => {}
            Err(crate::mux::MuxErr::NotInstalled { .. }) => {}
            Err(err @ crate::mux::MuxErr::Timeout { .. }) => {
                notices.push(format!("{err}; skipping {candidate} session lookup."))
            }
            Err(err) => tracing::warn!(mux = %candidate, error = %err, "list_sessions failed"),
        }
    }
    let detected = match crate::mux::auto_detect_backend(None) {
        Ok(detected) => detected,
        Err(source) => return Err(MuxPickErr { notices, source }),
    };
    if missing_report == MissingSessionReport::Warn {
        tracing::warn!(
            session = %session,
            mux = %detected,
            "no live session matches; emitting attach command for auto-detected mux",
        );
    }
    Ok(MuxPick {
        mux: detected,
        notices,
    })
}

/// Fail-fast guard for a new-room birth: refuse when the other backend already
/// runs this path's room. A rival that isn't installed or can't be listed never
/// blocks — best-effort probe, hard refusal only on a positive. Session identity
/// is shared across backends, so a matching rival session would share the room's
/// store while its panes stay unreachable.
pub fn ensure_single_backend_room(mux: MuxName, session_name: &str) -> Result<Vec<String>> {
    let rival = mux.other();
    let backend = crate::mux::backend_for(rival);
    let sessions = match list_sessions_with_retry(backend.as_ref()) {
        Ok(sessions) => sessions,
        Err(crate::mux::MuxErr::NotInstalled { .. }) => return Ok(Vec::new()),
        Err(err @ crate::mux::MuxErr::Timeout { .. }) => {
            return Ok(vec![format!(
                "{err}; skipping the cross-backend room check."
            )]);
        }
        Err(err) => {
            tracing::warn!(mux = %rival, error = %err, "rival list_sessions failed; allowing start");
            return Ok(Vec::new());
        }
    };
    if sessions.iter().any(|name| name == session_name) {
        bail!(
            "This project's room is already running under {rival} (session `{session_name}`).\n\
             Rimz keeps one room per project, so opening it under {mux} too would split your \
             fleet across two multiplexers that can't reach each other's panes.\n\n\
             Attach to the running room:\n    rimz attach {session_name}\n\n\
             Or close it, then start under {mux}:\n    rimz --mux {rival} reset --no-start"
        );
    }
    Ok(Vec::new())
}

fn list_sessions_with_retry(backend: &dyn MuxBackend) -> crate::mux::Result<Vec<String>> {
    list_sessions_retrying(
        || backend.list_sessions_within(session_probe_timeout()),
        LIST_SESSIONS_ATTEMPTS,
        LIST_SESSIONS_RETRY_DELAY,
    )
}

fn list_sessions_retrying(
    mut list_sessions: impl FnMut() -> crate::mux::Result<Vec<String>>,
    attempts: u8,
    retry_delay: Duration,
) -> crate::mux::Result<Vec<String>> {
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        match list_sessions() {
            Ok(sessions) => return Ok(sessions),
            Err(err @ crate::mux::MuxErr::NotInstalled { .. }) => return Err(err),
            Err(err @ crate::mux::MuxErr::Timeout { .. }) => return Err(err),
            Err(err) if attempt + 1 == attempts => return Err(err),
            Err(_) => std::thread::sleep(retry_delay),
        }
    }
    Ok(Vec::new())
}

/// Decide whether a workspace's live mux session is stranded by a session-name
/// change. The session name is derived from the project root, so changing the
/// derivation (or the path) leaves the previously-born session answering to the
/// recorded name while every new lookup, wakeup, and sidebar launch keys on the
/// derived one. Returns the recorded name to retire when it diverges from the
/// derived name and a session under it is still live.
fn renamed_session_to_retire<'a>(
    recorded: Option<&'a str>,
    derived: &str,
    live: &[String],
) -> Option<&'a str> {
    let recorded = recorded?;
    if recorded == derived {
        return None;
    }
    live.iter().any(|name| name == recorded).then_some(recorded)
}

/// Retire a live session left behind by a session-name change so the upcoming
/// `ensure_session` rebirths the workspace under the derived name (with a fresh
/// sidebar) instead of orphaning the old one. Must run before `record_workspace`
/// overwrites the stored name — that record is the only breadcrumb to the old
/// session. Best-effort: any lookup failure leaves the launch to proceed.
pub fn retire_renamed_session(backend: &dyn MuxBackend, workspace: &crate::ResolvedWorkspace) {
    let Ok(paths) = StatePaths::for_workspace(workspace.workspace_id.clone()) else {
        return;
    };
    let recorded = match workspace_record::read(&paths.workspace_record) {
        Ok(record) => record.session_name,
        Err(_) => return, // No prior record: first birth, nothing to retire.
    };
    let live = backend.list_sessions().unwrap_or_default();
    if let Some(stale) = renamed_session_to_retire(Some(&recorded), &workspace.session_name, &live)
    {
        match backend.kill_session(stale) {
            Ok(()) => tracing::info!(
                old = %stale,
                new = %workspace.session_name,
                "retired session left by a session-name change; rebirthing under the new name",
            ),
            Err(err) => tracing::warn!(
                old = %stale,
                error = %err,
                "could not retire renamed session; launch will create the new session alongside it",
            ),
        }
    }
}

pub fn workspace_record_for_session(session: &str) -> Result<Option<WorkspaceRecord>> {
    workspace_record_for_session_under(
        session,
        &crate::store::paths::state_home(),
        &crate::store::paths::runtime_home(),
    )
}

fn workspace_record_for_session_under(
    session: &str,
    state_root: &Path,
    runtime_root: &Path,
) -> Result<Option<WorkspaceRecord>> {
    let root = crate::store::paths::workspaces_dir_under(state_root);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", root.display())),
    };
    let mut record_paths = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            record_paths.push(path.join("workspace.json"));
        }
    }
    record_paths.sort();
    let mut matches = Vec::new();
    for path in record_paths {
        let record = match workspace_record::read(&path) {
            Ok(record) => record,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping unreadable workspace record");
                continue;
            }
        };
        if record.session_name == session {
            matches.push(record);
        }
    }
    if matches.len() <= 1 {
        return Ok(matches.into_iter().next());
    }
    Ok(Some(prefer_live_session_record(
        session,
        matches,
        runtime_root,
    )))
}

fn prefer_live_session_record(
    session: &str,
    records: Vec<WorkspaceRecord>,
    runtime_root: &Path,
) -> WorkspaceRecord {
    let mut fallback = None;
    let mut live = None;
    for record in records {
        if fallback.is_none() {
            fallback = Some(record.clone());
        }
        if let Some(last_seen) = freshest_matching_sidebar_heartbeat(session, &record, runtime_root)
        {
            let replace = live
                .as_ref()
                .is_none_or(|(prior, _): &(SystemTime, WorkspaceRecord)| last_seen > *prior);
            if replace {
                live = Some((last_seen, record));
            }
        }
    }
    live.map(|(_, record)| record)
        .or(fallback)
        .expect("caller only passes non-empty records")
}

fn freshest_matching_sidebar_heartbeat(
    session: &str,
    record: &WorkspaceRecord,
    runtime_root: &Path,
) -> Option<SystemTime> {
    let runtime = RuntimePaths::under(record.workspace_id.clone(), runtime_root).ok()?;
    let heartbeats =
        crate::sidebar::heartbeat::read_current_heartbeats(&runtime.heartbeat_dir).ok()?;
    heartbeats
        .into_iter()
        .filter_map(|(path, heartbeat)| {
            matching_sidebar_heartbeat_mtime(session, record, &path, &heartbeat)
        })
        .max()
}

fn matching_sidebar_heartbeat_mtime(
    session: &str,
    record: &WorkspaceRecord,
    path: &Path,
    heartbeat: &crate::sidebar::heartbeat::SidebarHeartbeat,
) -> Option<SystemTime> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let fresh = match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= crate::sidebar::timing::SIDEBAR_HEARTBEAT_TTL,
        Err(_) => true,
    };
    if !fresh {
        return None;
    }
    (heartbeat.session_name == session && heartbeat.workspace_id == record.workspace_id)
        .then_some(modified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn live_sessions_resolve_with_zellij_first_precedence() {
        let live = LiveSessions {
            zellij: HashSet::from(["shared".to_owned(), "zellij".to_owned()]),
            tmux: HashSet::from(["shared".to_owned(), "tmux".to_owned()]),
        };

        assert_eq!(live.mux_of("shared"), Some(MuxName::Zellij));
        assert_eq!(live.mux_of("zellij"), Some(MuxName::Zellij));
        assert_eq!(live.mux_of("tmux"), Some(MuxName::Tmux));
        assert_eq!(live.mux_of("missing"), None);
    }

    #[test]
    fn list_sessions_retrying_follows_retry_policy() {
        let mut calls = 0;
        let sessions = list_sessions_retrying(
            || {
                calls += 1;
                if calls < 3 {
                    Err(transient_list_sessions_error())
                } else {
                    Ok(vec!["rimz-room".to_owned()])
                }
            },
            3,
            Duration::ZERO,
        )
        .expect("transient list-sessions recovers");

        assert_eq!(sessions, vec!["rimz-room"]);
        assert_eq!(calls, 3);

        let mut calls = 0;
        let sessions = list_sessions_retrying(
            || {
                calls += 1;
                Ok(Vec::new())
            },
            3,
            Duration::ZERO,
        )
        .expect("empty list-sessions succeeds");

        assert!(sessions.is_empty());
        assert_eq!(calls, 1);

        let mut calls = 0;
        let err = list_sessions_retrying(
            || {
                calls += 1;
                Err(crate::mux::MuxErr::NotInstalled {
                    program: "zellij".to_owned(),
                })
            },
            3,
            Duration::ZERO,
        )
        .expect_err("not-installed is definitive");

        assert!(matches!(err, crate::mux::MuxErr::NotInstalled { .. }));
        assert_eq!(calls, 1);

        let mut calls = 0;
        let err = list_sessions_retrying(
            || {
                calls += 1;
                Err(crate::mux::MuxErr::Timeout {
                    program: "zellij".to_owned(),
                    args: "list-sessions".to_owned(),
                    seconds: 1,
                })
            },
            3,
            Duration::ZERO,
        )
        .expect_err("timeout is definitive");

        assert!(matches!(err, crate::mux::MuxErr::Timeout { .. }));
        assert_eq!(calls, 1);
    }

    #[test]
    fn renamed_session_retires_only_a_live_diverged_name() {
        let live = vec!["rimz-old".to_owned(), "unrelated".to_owned()];

        assert_eq!(
            renamed_session_to_retire(Some("rimz-old"), "rimz-new", &live),
            Some("rimz-old"),
        );
        assert_eq!(
            renamed_session_to_retire(Some("rimz-old"), "rimz-old", &live),
            None,
        );
        assert_eq!(
            renamed_session_to_retire(Some("rimz-gone"), "rimz-new", &live),
            None,
        );
        assert_eq!(renamed_session_to_retire(None, "rimz-new", &live), None);
    }

    #[test]
    fn workspace_record_for_session_prefers_live_heartbeat_then_sorted_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let state_root = dir.path().join("state");
        let runtime_root = dir.path().join("run");
        let stale_id = WorkspaceId::parse("ws_000000000000000000000001").unwrap();
        let live_id = WorkspaceId::parse("ws_ffffffffffffffffffffffff").unwrap();
        let session = "rimz-room";

        write_workspace_record(&state_root, stale_id.clone(), session, "/repo/stale");
        write_workspace_record(&state_root, live_id.clone(), session, "/repo/live");

        let runtime = RuntimePaths::under(live_id.clone(), &runtime_root).unwrap();
        runtime.ensure_dirs().unwrap();
        let instance = crate::SidebarInstanceId::new();
        let heartbeat = crate::sidebar::heartbeat::SidebarHeartbeat::new(
            live_id.clone(),
            instance.clone(),
            MuxName::Zellij,
            session,
            runtime.sock_dir.join("sidebar.sock"),
            None,
        );
        let path = runtime.sidebar_heartbeat_path(&instance);
        std::fs::write(&path, serde_json::to_vec(&heartbeat).unwrap()).unwrap();

        let record = workspace_record_for_session_under(session, &state_root, &runtime_root)
            .unwrap()
            .expect("session record");

        assert_eq!(record.workspace_id, live_id);
        std::fs::remove_file(path).unwrap();
        let record = workspace_record_for_session_under(session, &state_root, &runtime_root)
            .unwrap()
            .expect("session record");

        assert_eq!(record.workspace_id, stale_id);
    }

    fn write_workspace_record(
        state_root: &Path,
        workspace_id: WorkspaceId,
        session_name: &str,
        project_root: &str,
    ) {
        let paths = StatePaths::under(workspace_id.clone(), state_root).unwrap();
        paths.ensure_dirs().unwrap();
        let record = WorkspaceRecord {
            workspace_id,
            project_root: project_root.into(),
            worktree_root: None,
            session_name: session_name.to_owned(),
            root_class: crate::workspace::RootClass::Repo,
            rimz_bin: None,
            updated_at: jiff::Timestamp::now(),
        };
        workspace_record::write(&paths, &record).unwrap();
    }

    fn transient_list_sessions_error() -> crate::mux::MuxErr {
        crate::mux::MuxErr::Command {
            program: "zellij".to_owned(),
            args: "list-sessions".to_owned(),
            stderr: "transient".to_owned(),
        }
    }
}
