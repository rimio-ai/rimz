//! Start-time workspace, configuration, and version notices.

use std::io::Write;

use anyhow::Result;
use rimz::RuntimePaths;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::sidebar::SessionBuildDrift;

use crate::cli::render;

fn broken_config_notice(err: &rimz::config::ConfigErr) -> String {
    broken_config_notice_for(
        err,
        err.path().starts_with(rimz::store::paths::agents_home()),
    )
}

fn broken_config_notice_for(err: &rimz::config::ConfigErr, fragment: bool) -> String {
    let path = render::home_relative(&err.path().display().to_string());
    let detail = err
        .diagnosis()
        .map(rimz::config::ConfigFileDiagnosis::summary)
        .unwrap_or_else(|| render::one_line_error(err));
    if fragment {
        format!(
            "{path} cannot be used: {detail}; `rimz agents` and `rimz teams` refuse launches until this fragment is fixed"
        )
    } else if err.diagnosis().is_some() {
        format!(
            "{path} is unparseable — every setting in it is ignored and built-in defaults apply: {detail}; fix the file, then restart"
        )
    } else {
        format!(
            "{path} is invalid — every setting in it is ignored and built-in defaults apply: {detail}; fix the file, then restart"
        )
    }
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

/// The `rimz start` notices: configuration notices plus the root-class line
/// for a non-repo room. Notices go to stderr; stdout stays the protocol surface.
pub(super) fn report_start_notices(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    let mut notices: Vec<_> = rimz::config::broken_machine_files()
        .iter()
        .map(broken_config_notice)
        .collect();
    notices.extend(
        rimz::config::MachineConfig::load_lenient()
            .notices
            .unknown_keys
            .iter()
            .map(|notice| {
                format!(
                    "unknown config key `{}` in {} — ignored; run `rimz setup` to remove it",
                    notice.key,
                    notice.path.display(),
                )
            }),
    );
    notices.extend(root_class_notice(workspace));
    if notices.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    for notice in notices {
        writeln!(stderr, "rimz: {notice}")?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum SkewAction {
    Warn(String),
    Refuse { message: String, code: i32 },
}

fn remote_skew_action(client: &str, host: &str, forced: bool) -> Option<SkewAction> {
    use rimz::remote::version::Skew;

    let warning = || {
        format!(
            "you connected with rimz {client} but this host runs rimz {host}; upgrade the older side to keep them matched"
        )
    };
    match rimz::remote::version::classify(client, host) {
        Skew::Match => None,
        Skew::Patch | Skew::Unparseable => Some(SkewAction::Warn(warning())),
        Skew::Minor if forced => Some(SkewAction::Warn(format!(
            "you connected with rimz {client} but this host runs rimz {host}; continuing because --force-version was given; upgrade the older side to keep them matched"
        ))),
        Skew::Minor => Some(SkewAction::Refuse {
            message: format!(
                "rimz {client} cannot connect to this host running rimz {host} because they differ by a minor version; upgrade the older side (`rimz remote setup <host>` upgrades the remote), or retry with --force-version to attach anyway"
            ),
            code: rimz::remote::REMOTE_VERSION_SKEW_EXIT,
        }),
        Skew::Major => Some(SkewAction::Refuse {
            message: format!(
                "rimz {client} cannot connect to this host running rimz {host} because they differ by a major version; upgrade required (`rimz remote setup <host>` upgrades the remote); --force-version does not apply to major mismatches"
            ),
            code: rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT,
        }),
    }
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
    if let Ok(client_version) = std::env::var(rimz::remote::REMOTE_CLIENT_VERSION_ENV) {
        let forced = std::env::var_os(rimz::remote::REMOTE_FORCE_VERSION_ENV)
            .is_some_and(|value| value == "1");
        match remote_skew_action(&client_version, rimz::build_id::VERSION, forced) {
            Some(SkewAction::Warn(notice)) => notices.push(notice),
            Some(SkewAction::Refuse { message, code }) => {
                let _ = writeln!(std::io::stderr().lock(), "rimz: {message}");
                std::process::exit(code);
            }
            None => {}
        }
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
            cwd_project_root: None,
            root_class: class,
            worktree_root: root.clone(),
            worktree_branch: None,
            session_name: format!("rimz-{}", root.display()),
            mux_hint: None,
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
        assert!(notice.contains("line 3"), "{notice}");
        assert!(
            notice.contains("`max_cols` is defined more than once"),
            "{notice}"
        );
        assert!(notice.contains("built-in defaults apply"), "{notice}");
    }

    #[test]
    fn broken_fragment_notice_names_launch_precondition_not_fallback() {
        let err = rimz::config::MachineConfig::parse_text(
            std::path::Path::new("/tmp/agents.toml"),
            "[agents.teams.bad]\nlayout = \"claude,,codex\"\n",
            std::path::Path::new("/tmp/missing-agents-home"),
        )
        .expect_err("invalid layout fails");

        let notice = broken_config_notice_for(&err, true);

        assert!(
            notice.contains("/tmp/agents.toml cannot be used"),
            "{notice}"
        );
        assert!(notice.contains("refuse launches"), "{notice}");
        assert!(!notice.contains("unparseable"), "{notice}");
        assert!(!notice.contains("built-in defaults"), "{notice}");
    }

    #[test]
    fn remote_skew_action_maps_compatibility_tiers() {
        assert_eq!(remote_skew_action("0.5.0", "0.5.0", false), None);

        let Some(SkewAction::Warn(patch)) = remote_skew_action("0.5.0", "0.5.1", false) else {
            panic!("patch skew warns");
        };
        assert!(patch.contains("rimz 0.5.0"), "{patch}");
        assert!(patch.contains("rimz 0.5.1"), "{patch}");

        let Some(SkewAction::Refuse { message, code }) =
            remote_skew_action("0.5.0", "0.4.9", false)
        else {
            panic!("minor skew refuses");
        };
        assert_eq!(code, rimz::remote::REMOTE_VERSION_SKEW_EXIT);
        assert!(message.contains("rimz 0.5.0"), "{message}");
        assert!(message.contains("rimz 0.4.9"), "{message}");
        assert!(message.contains("rimz remote setup <host>"), "{message}");
        assert!(message.contains("--force-version"), "{message}");

        let Some(SkewAction::Warn(forced)) = remote_skew_action("1.5.0", "1.4.9", true) else {
            panic!("forced minor skew warns");
        };
        assert!(
            forced.contains("continuing because --force-version was given"),
            "{forced}"
        );
    }

    #[test]
    fn remote_skew_action_keeps_major_mismatches_hard() {
        for forced in [false, true] {
            let Some(SkewAction::Refuse { message, code }) =
                remote_skew_action("1.0.0", "0.5.0", forced)
            else {
                panic!("major skew refuses");
            };
            assert_eq!(code, rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT);
            assert!(message.contains("rimz 1.0.0"), "{message}");
            assert!(message.contains("rimz 0.5.0"), "{message}");
            assert!(
                message.contains("--force-version does not apply"),
                "{message}"
            );
        }
    }

    #[test]
    fn remote_skew_action_warns_when_versions_cannot_be_parsed() {
        let Some(SkewAction::Warn(message)) = remote_skew_action("dev", "0.5.0", false) else {
            panic!("unparseable skew warns");
        };
        assert!(message.contains("rimz dev"), "{message}");
        assert!(message.contains("rimz 0.5.0"), "{message}");
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
