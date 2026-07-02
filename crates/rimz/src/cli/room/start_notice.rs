//! Start-time workspace notices for root class and overlapping live rooms.

use std::io::Write;

use anyhow::Result;
use rimz::ids::MuxName;

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
        if let Ok(sessions) = rimz::mux::backend_for(mux).list_sessions() {
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
    let mut notices = Vec::new();
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
}
