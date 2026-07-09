//! Previous-incarnation coroner for room birth.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::ids::{AgentKind, AgentSessionId, WorkspaceId};
use rimz::store::event::{LastDeathMarker, SessionDeathAgent, SessionDeathCause};
use rimz::{RuntimePaths, StatePaths, Store};

use super::resume::reboot_since_last_birth;

const CRASH_ARCHIVE_RETENTION: usize = 5;

#[derive(Clone, Debug, Default)]
pub(super) struct BirthRecovery {
    pub(super) recover_agents: bool,
    pub(super) death: Option<LastDeathMarker>,
    pub(super) roster: BTreeSet<(AgentKind, AgentSessionId)>,
}

struct AuditState {
    paths: StatePaths,
    store: Store,
    projection: rimz::RuntimeProjection,
}

pub(super) fn inspect_previous_incarnation(
    backend: &dyn rimz::mux::MuxBackend,
    workspace_id: &WorkspaceId,
    session_name: &str,
) -> BirthRecovery {
    let reboot = reboot_since_last_birth(workspace_id);
    let audit = read_audit_state(workspace_id);
    let recovery_roster = audit.as_ref().map(recovery_roster).unwrap_or_default();
    let recover_agents = reboot || !recovery_roster.is_empty();
    if !recover_agents {
        return BirthRecovery::default();
    }

    let Some(audit) = audit else {
        return BirthRecovery {
            recover_agents,
            death: None,
            roster: recovery_roster,
        };
    };
    let cause = if reboot {
        SessionDeathCause::Reboot
    } else {
        SessionDeathCause::Crash
    };
    let lost_agents = lost_agent_summaries(&audit.projection.agents, &recovery_roster);
    let marker = LastDeathMarker {
        cause,
        lost_agents: lost_agents.clone(),
        at: Timestamp::now(),
        recovered: None,
    };
    append_session_death(&audit.store, workspace_id, session_name, &marker);
    write_last_death_marker(&audit.paths, &marker);
    if cause == SessionDeathCause::Crash {
        let roster = lost_agent_roster(&audit.projection.agents, &recovery_roster);
        if let Err(err) = archive_crash(backend, &audit.paths, session_name, &roster, marker.at) {
            tracing::debug!(
                workspace = %workspace_id,
                session = %session_name,
                error = %err,
                "crash archive skipped",
            );
        }
    }
    BirthRecovery {
        recover_agents,
        death: Some(marker),
        roster: recovery_roster,
    }
}

pub(super) fn report_previous_session_death(death: &LastDeathMarker) {
    let _ = writeln!(std::io::stderr().lock(), "{}", death_notice(death));
}

pub(super) fn record_recovery_outcome(
    workspace_id: &WorkspaceId,
    death: &LastDeathMarker,
    recovered: usize,
) {
    let Ok(paths) = StatePaths::for_workspace(workspace_id.clone()) else {
        return;
    };
    let mut marker = death.clone();
    marker.recovered = Some(recovered);
    write_last_death_marker(&paths, &marker);
}

fn death_notice(death: &LastDeathMarker) -> String {
    let at = death.at.strftime("%Y-%m-%d %H:%M");
    match death.cause {
        SessionDeathCause::Reboot => {
            format!("rimz: machine rebooted since this room was last open ({at})")
        }
        SessionDeathCause::Crash => {
            format!("rimz: this room's previous session ended with agents still running ({at})")
        }
    }
}

fn read_audit_state(workspace_id: &WorkspaceId) -> Option<AuditState> {
    let paths = StatePaths::for_workspace(workspace_id.clone()).ok()?;
    let runtime = RuntimePaths::for_workspace(workspace_id.clone()).ok()?;
    let store = Store::open(paths.clone(), runtime).ok()?;
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit).ok()?;
    Some(AuditState {
        paths,
        store,
        projection,
    })
}

fn recovery_roster(audit: &AuditState) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let Some(roster) = rimz::store::live_roster::read(&audit.paths.live_roster) else {
        return BTreeSet::new();
    };
    let audited = audit
        .projection
        .agents
        .iter()
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    roster.agents.intersection(&audited).cloned().collect()
}

fn lost_agent_summaries(
    agents: &[rimz::agents::AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<SessionDeathAgent> {
    lost.iter()
        .map(|(kind, agent_id)| {
            let name = agents
                .iter()
                .find(|agent| agent.kind == *kind && agent.agent_id == *agent_id)
                .and_then(|agent| agent.name.clone());
            SessionDeathAgent {
                kind: kind.clone(),
                agent_id: agent_id.clone(),
                name,
            }
        })
        .collect()
}

fn lost_agent_roster(
    agents: &[rimz::agents::AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<rimz::agents::AgentState> {
    agents
        .iter()
        .filter(|agent| lost.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .cloned()
        .collect()
}

fn append_session_death(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    marker: &LastDeathMarker,
) {
    let event = rimz::EventEnvelope::session_death(
        workspace_id.clone(),
        session_name,
        marker.cause,
        marker.lost_agents.clone(),
    );
    if let Err(err) = store.append_event(&event) {
        tracing::warn!(
            workspace = %workspace_id,
            session = %session_name,
            error = %err,
            "session death event skipped",
        );
    }
}

fn write_last_death_marker(paths: &StatePaths, marker: &LastDeathMarker) {
    if let Err(err) = rimz::store::atomic::write_temp_then_rename(&paths.last_death_marker, marker)
    {
        tracing::debug!(
            path = %paths.last_death_marker.display(),
            error = %err,
            "last death marker write skipped",
        );
    }
}

fn archive_crash(
    backend: &dyn rimz::mux::MuxBackend,
    paths: &StatePaths,
    session_name: &str,
    roster: &[rimz::agents::AgentState],
    at: Timestamp,
) -> Result<()> {
    let archive = paths.crashes_dir.join(archive_name(at));
    let mux_cache = archive.join("mux-cache");
    std::fs::create_dir_all(&mux_cache)
        .with_context(|| format!("creating crash archive {}", mux_cache.display()))?;
    let cache_root = rimz::store::paths::cache_home();
    for source in backend.resurrection_cache_paths(session_name) {
        let dest = cache_archive_destination(&mux_cache, &cache_root, &source);
        copy_path(&source, &dest)
            .with_context(|| format!("archiving mux cache {}", source.display()))?;
    }
    rimz::store::atomic::write_temp_then_rename(&archive.join("roster.json"), &roster)
        .with_context(|| format!("writing crash roster {}", archive.display()))?;
    prune_crash_archives(&paths.crashes_dir)?;
    Ok(())
}

fn archive_name(at: Timestamp) -> String {
    at.strftime("%Y%m%dT%H%M%SZ").to_string()
}

fn cache_archive_destination(mux_cache: &Path, cache_root: &Path, source: &Path) -> PathBuf {
    if let Ok(relative) = source.strip_prefix(cache_root)
        && !relative.as_os_str().is_empty()
    {
        return mux_cache.join(relative);
    }
    mux_cache.join(source.file_name().unwrap_or_else(|| OsStr::new("cache")))
}

fn copy_path(source: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_path(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, dest)?;
    }
    Ok(())
}

fn prune_crash_archives(crashes_dir: &Path) -> Result<()> {
    let mut archives = std::fs::read_dir(crashes_dir)
        .with_context(|| format!("reading crash archives {}", crashes_dir.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect::<Vec<_>>();
    archives.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    for archive in archives.into_iter().skip(CRASH_ARCHIVE_RETENTION) {
        std::fs::remove_dir_all(archive.path())
            .with_context(|| format!("removing old crash archive {}", archive.path().display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_roster_intersects_persisted_live_agents_with_rollup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).expect("paths");
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
        let store = Store::open(paths.clone(), runtime).expect("store");
        let claude = AgentKind::new_unchecked("claude");
        let mut ghost = rimz::testkit::agent_state("claude", "ghost", Timestamp::UNIX_EPOCH);
        ghost.name = Some("ghost".to_owned());
        let mut live = rimz::testkit::agent_state("claude", "live", Timestamp::UNIX_EPOCH);
        live.name = Some("live".to_owned());
        let projection = rimz::RuntimeProjection {
            ended: BTreeSet::new(),
            agents: vec![ghost, live],
        };
        rimz::store::live_roster::publish(
            &paths.live_roster,
            [
                (claude.clone(), "live".into()),
                (claude.clone(), "missing".into()),
            ]
            .into_iter()
            .collect(),
        )
        .expect("publish roster");
        let audit = AuditState {
            paths,
            store,
            projection,
        };

        let roster = recovery_roster(&audit);

        assert_eq!(roster, [(claude, "live".into())].into_iter().collect());
    }

    #[test]
    fn last_death_marker_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace, dir.path()).expect("paths");
        let marker = LastDeathMarker {
            cause: SessionDeathCause::Crash,
            lost_agents: vec![SessionDeathAgent {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: "sess-a".into(),
                name: Some("lucid-atlas".to_owned()),
            }],
            at: Timestamp::UNIX_EPOCH,
            recovered: Some(2),
        };

        write_last_death_marker(&paths, &marker);

        let loaded: LastDeathMarker =
            serde_json::from_slice(&std::fs::read(&paths.last_death_marker).expect("read marker"))
                .expect("json marker");
        assert_eq!(loaded, marker);
    }

    #[test]
    fn death_notice_uses_neutral_crash_wording() {
        let notice = death_notice(&death_marker(SessionDeathCause::Crash, 2));

        assert_eq!(
            notice,
            "rimz: this room's previous session ended with agents still running (1970-01-01 00:00)"
        );
        assert!(!notice.contains("crash"), "{notice}");
        assert!(!notice.contains("died"), "{notice}");
        assert!(!notice.contains("offering"), "{notice}");
    }

    #[test]
    fn death_notice_uses_reboot_wording() {
        let notice = death_notice(&death_marker(SessionDeathCause::Reboot, 1));

        assert_eq!(
            notice,
            "rimz: machine rebooted since this room was last open (1970-01-01 00:00)"
        );
        assert!(!notice.contains("crash"), "{notice}");
        assert!(!notice.contains("died"), "{notice}");
    }

    #[test]
    fn crash_archive_retention_keeps_newest_five() {
        let dir = tempfile::tempdir().expect("tempdir");
        let crashes = dir.path().join("crashes");
        for index in 0..7 {
            std::fs::create_dir_all(crashes.join(format!("2026010{index}T000000Z")))
                .expect("archive dir");
        }

        prune_crash_archives(&crashes).expect("prune");

        let mut kept = std::fs::read_dir(&crashes)
            .expect("read")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        kept.sort();
        assert_eq!(
            kept,
            vec![
                "20260102T000000Z",
                "20260103T000000Z",
                "20260104T000000Z",
                "20260105T000000Z",
                "20260106T000000Z",
            ]
        );
    }

    fn death_marker(cause: SessionDeathCause, agents: usize) -> LastDeathMarker {
        LastDeathMarker {
            cause,
            lost_agents: (0..agents)
                .map(|index| SessionDeathAgent {
                    kind: AgentKind::new_unchecked("claude"),
                    agent_id: format!("sess-{index}").into(),
                    name: None,
                })
                .collect(),
            at: Timestamp::UNIX_EPOCH,
            recovered: None,
        }
    }
}
