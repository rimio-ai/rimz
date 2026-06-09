use super::*;

pub(super) fn pick_mux_for_session(
    session: &str,
    explicit: Option<MuxName>,
    missing_report: MissingSessionReport,
) -> Result<MuxName> {
    if let Some(mux) = explicit {
        return Ok(mux);
    }
    for candidate in [MuxName::Zellij, MuxName::Tmux] {
        match rimz::mux::backend_for(candidate).list_sessions() {
            Ok(sessions) if sessions.iter().any(|s| s == session) => return Ok(candidate),
            Ok(_) => {}
            Err(rimz::mux::MuxErr::NotInstalled { .. }) => {}
            Err(err) => tracing::warn!(mux = %candidate, error = %err, "list_sessions failed"),
        }
    }
    let detected = rimz::mux::auto_detect_backend(None)?;
    if missing_report == MissingSessionReport::Warn {
        tracing::warn!(
            session = %session,
            mux = %detected,
            "no live session matches; emitting attach command for auto-detected mux",
        );
    }
    Ok(detected)
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
pub(super) fn retire_renamed_session(
    backend: &dyn MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
) {
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

pub(super) fn workspace_record_for_session(session: &str) -> Result<Option<WorkspaceRecord>> {
    let root = workspaces_dir();
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
    for path in record_paths {
        let record = match workspace_record::read(&path) {
            Ok(record) => record,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "skipping unreadable workspace record");
                continue;
            }
        };
        if record.session_name == session {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
