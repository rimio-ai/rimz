//! Start-time workspace notices for root class and overlapping live rooms.

use std::io::Write;

use anyhow::Result;
use rimz::RuntimePaths;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::sidebar::SessionBuildDrift;

use crate::cli::render;

use rimz::room::session::session_probe_timeout;

fn broken_config_notice(err: &rimz::config::ConfigErr) -> String {
    let path = render::home_relative(&err.path().display().to_string());
    format!(
        "{path} is unparseable — every setting in it is ignored and built-in defaults apply: {}; fix the file, then restart",
        render::one_line_error(err),
    )
}

fn root_class_notice(workspace: &rimz::ResolvedWorkspace) -> Option<String> {
    use rimz::workspace::RootClass;
    match workspace.root_class {
        RootClass::Repo => None,
        RootClass::Marker => Some(format!(
            "marker-rooted workspace at {} (project marker, no git repository)",
            workspace.project_root.display(),
        )),
        RootClass::Directory => Some(format!(
            "directory workspace rooted at {} (no repository or project marker)",
            workspace.project_root.display(),
        )),
    }
}

/// The known workspaces whose recorded root nests inside or contains this
/// room's root — the candidates the (more expensive) liveness probe filters.
fn overlapping_known<'a>(
    workspace: &rimz::ResolvedWorkspace,
    known: &'a [rimz::workspace::KnownWorkspace],
) -> Vec<&'a rimz::workspace::KnownWorkspace> {
    use rimz::workspace::root_contains;
    known
        .iter()
        .filter(|ws| ws.workspace_id != workspace.workspace_id)
        .filter(|ws| {
            root_contains(&ws.project_root, &workspace.project_root)
                || root_contains(&workspace.project_root, &ws.project_root)
        })
        .collect()
}

/// Every session name live on either backend, best-effort: a missing or
/// wedged mux contributes nothing rather than failing the caller.
pub(crate) fn live_session_names() -> std::collections::BTreeSet<String> {
    let mut live = std::collections::BTreeSet::new();
    for mux in [MuxName::Zellij, MuxName::Tmux] {
        if let Ok(sessions) =
            rimz::mux::backend_for(mux).list_sessions_within(session_probe_timeout())
        {
            live.extend(sessions);
        }
    }
    live
}

/// The `rimz start` notices: the root-class line for a non-repo room, and one
/// line per *live* room whose root nests inside or contains this one. Overlap
/// is legal — an agent belongs to the room its pane lives in — so the notice
/// names the standing situation rather than blocking it. Notices go to stderr;
/// stdout stays the protocol surface.
pub(super) fn report_start_notices(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    let mut notices: Vec<_> = rimz::config::broken_machine_files()
        .iter()
        .map(broken_config_notice)
        .collect();
    notices.extend(root_class_notice(workspace));
    if let Ok(known) = rimz::workspace::known_workspaces() {
        let candidates = overlapping_known(workspace, &known);
        if !candidates.is_empty() {
            // The two `list-sessions` forks run only when a recorded root
            // actually overlaps — the common start pays a readdir, no mux call.
            let live = live_session_names();
            notices.extend(
                candidates
                    .into_iter()
                    .filter(|ws| live.contains(&ws.session_name))
                    .map(|ws| {
                        format!(
                            "this room overlaps live room `{}` rooted at {}; an agent belongs to the room its pane lives in",
                            ws.session_name,
                            ws.project_root.display(),
                        )
                    }),
            );
        }
    }
    if notices.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    for notice in notices {
        writeln!(stderr, "rimz: {notice}")?;
    }
    Ok(())
}

fn remote_client_skew_notice(client: &str, host: &str) -> Option<String> {
    (client != host).then(|| {
        format!(
            "you connected with rimz {client} but this host runs rimz {host}; upgrade the older side to keep them matched"
        )
    })
}

fn build_drift_notice(drift: &SessionBuildDrift, own_version: &str) -> String {
    match drift.versions.as_slice() {
        [room_version] if room_version != own_version => format!(
            "this room is running rimz {room_version} but this binary is rimz {own_version}; run `rimz reload` to move the room onto this build"
        ),
        _ => "this room is running a different rimz build than this binary; run `rimz reload` to move the room onto this build".to_owned(),
    }
}

/// Report version skew carried by a remote client and build drift in a live,
/// managed room. Missing runtime evidence stays silent.
pub(super) fn report_version_mismatch_notices(
    workspace_id: Option<&WorkspaceId>,
    mux: MuxName,
    session_name: &str,
    was_live: bool,
) -> Result<()> {
    let mut notices = Vec::new();
    if let Ok(client_version) = std::env::var(rimz::remote::REMOTE_CLIENT_VERSION_ENV)
        && let Some(notice) = remote_client_skew_notice(&client_version, rimz::build_id::VERSION)
    {
        notices.push(notice);
    }
    if was_live
        && let Some(drift) = workspace_id
            .and_then(|id| RuntimePaths::for_workspace(id.clone()).ok())
            .and_then(|runtime| rimz::sidebar::session_build_drift(&runtime, mux, session_name))
    {
        notices.push(build_drift_notice(&drift, rimz::build_id::VERSION));
    }

    if notices.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    for notice in notices {
        writeln!(stderr, "rimz: {notice}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn resolved(root: &str, class: rimz::workspace::RootClass) -> rimz::ResolvedWorkspace {
        use rimz::ids::WorkspaceId;
        let root = PathBuf::from(root);
        rimz::ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(&root),
            project_root: root.clone(),
            root_class: class,
            worktree_root: root.clone(),
            worktree_branch: None,
            session_name: format!("rimz-{}", root.display()),
            mux_hint: None,
        }
    }

    fn known(root: &str) -> rimz::workspace::KnownWorkspace {
        use rimz::ids::WorkspaceId;
        let root = PathBuf::from(root);
        rimz::workspace::KnownWorkspace {
            workspace_id: WorkspaceId::from_project_root(&root),
            session_name: format!("rimz-{}", root.display()),
            project_root: root,
            root_class: rimz::workspace::RootClass::Repo,
            rimz_bin: None,
        }
    }

    #[test]
    fn root_class_notice_names_non_repo_rooms_only() {
        use rimz::workspace::RootClass;
        assert_eq!(
            root_class_notice(&resolved("/code/repo", RootClass::Repo)),
            None
        );
        let marker = root_class_notice(&resolved("/code/proj", RootClass::Marker))
            .expect("marker rooms are noticed");
        assert!(marker.contains("/code/proj"), "names the root: {marker}");
        let dir = root_class_notice(&resolved("/tmp/scratch", RootClass::Directory))
            .expect("directory rooms are noticed");
        assert!(
            dir.contains("directory workspace rooted at /tmp/scratch"),
            "names the class and root: {dir}",
        );
    }

    #[test]
    fn overlapping_known_keeps_nesting_rooms_and_drops_the_rest() {
        use rimz::workspace::RootClass;
        let workspace = resolved("/home/m/code", RootClass::Directory);
        let rooms = vec![
            known("/home/m/code"),
            known("/home/m/code/query"),
            known("/home/m"),
            known("/home/m/codex"),
            known("/srv/elsewhere"),
        ];
        let hits: Vec<_> = overlapping_known(&workspace, &rooms)
            .into_iter()
            .map(|ws| ws.project_root.display().to_string())
            .collect();
        assert_eq!(hits, vec!["/home/m/code/query", "/home/m"]);
    }

    #[test]
    fn broken_config_notice_is_one_line_and_names_the_fallback() {
        let err = rimz::config::MachineConfig::parse_text(
            std::path::Path::new("/tmp/theme.toml"),
            "[theme.display]\nmax_cols = 64\nmax_cols = 72\n",
            std::path::Path::new("/tmp/missing-agents-home"),
        )
        .expect_err("duplicate key fails");

        let notice = broken_config_notice(&err);

        assert_eq!(
            notice.lines().count(),
            1,
            "notice stays on one line: {notice}"
        );
        assert!(
            notice.contains("/tmp/theme.toml is unparseable"),
            "{notice}"
        );
        assert!(
            notice.contains("duplicate key") && notice.contains("max_cols"),
            "{notice}",
        );
        assert!(notice.contains("built-in defaults apply"), "{notice}");
    }

    #[test]
    fn remote_client_skew_notice_names_both_versions() {
        assert_eq!(remote_client_skew_notice("0.5.0", "0.5.0"), None);
        assert_eq!(
            remote_client_skew_notice("0.5.0", "0.4.1").as_deref(),
            Some(
                "you connected with rimz 0.5.0 but this host runs rimz 0.4.1; upgrade the older side to keep them matched"
            ),
        );
    }

    #[test]
    fn build_drift_notice_uses_semantic_version_only_when_unambiguous() {
        let known = SessionBuildDrift {
            versions: vec!["0.4.1".to_owned()],
        };
        assert_eq!(
            build_drift_notice(&known, "0.5.0"),
            "this room is running rimz 0.4.1 but this binary is rimz 0.5.0; run `rimz reload` to move the room onto this build",
        );

        let same_version = SessionBuildDrift {
            versions: vec!["0.5.0".to_owned()],
        };
        assert_eq!(
            build_drift_notice(&same_version, "0.5.0"),
            "this room is running a different rimz build than this binary; run `rimz reload` to move the room onto this build",
        );
    }
}
