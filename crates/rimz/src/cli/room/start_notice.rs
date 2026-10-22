//! Start-time workspace and version notices.

use std::io::Write;

use anyhow::Result;
use rimz::RuntimePaths;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::sidebar::SessionBuildDrift;

use super::StartPosture;

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

/// Report the root class of a non-repository room on stderr.
pub(super) fn report_start_notices(workspace: &rimz::ResolvedWorkspace) -> Result<()> {
    if let Some(notice) = root_class_notice(workspace) {
        writeln!(std::io::stderr().lock(), "rimz: {notice}")?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum SkewAction {
    Warn(String),
    Refuse { message: String, code: i32 },
}

fn remote_skew_action(
    client: &str,
    host: &str,
    forced: bool,
    posture: StartPosture,
    supervised: bool,
) -> Option<SkewAction> {
    use rimz::remote::version::Skew;

    let warning = || {
        format!(
            "you connected with rimz {client} but this host runs rimz {host}; upgrade the older side to keep them matched"
        )
    };
    // A supervised attach's local fatal line names the real target's fix.
    let refusal = |tier: &str, advice: &str| {
        let refusal = format!(
            "rimz {client} cannot connect to this host running rimz {host} because they differ by a {tier} version"
        );
        if supervised {
            refusal
        } else {
            format!("{refusal}; {advice}")
        }
    };
    let action = match rimz::remote::version::classify(client, host) {
        Skew::Match => None,
        Skew::Patch | Skew::Unparseable => Some(SkewAction::Warn(warning())),
        Skew::Minor if forced => Some(SkewAction::Warn(format!(
            "you connected with rimz {client} but this host runs rimz {host}; continuing because --force-version was given; upgrade the older side to keep them matched"
        ))),
        Skew::Minor => Some(SkewAction::Refuse {
            message: refusal(
                "minor",
                "upgrade the older side (`rimz remote setup <host>` upgrades the remote), or retry with --force-version to attach anyway",
            ),
            code: rimz::remote::REMOTE_VERSION_SKEW_EXIT,
        }),
        Skew::Major => Some(SkewAction::Refuse {
            message: refusal(
                "major",
                "upgrade required (`rimz remote setup <host>` upgrades the remote); --force-version does not apply to major mismatches",
            ),
            code: rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT,
        }),
    };
    action.filter(|action| {
        posture != StartPosture::Reconnect || matches!(action, SkewAction::Refuse { .. })
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
    posture: StartPosture,
) -> Result<()> {
    let mut notices = Vec::new();
    if let Ok(client_version) = std::env::var(rimz::remote::REMOTE_CLIENT_VERSION_ENV) {
        let forced = std::env::var_os(rimz::remote::REMOTE_FORCE_VERSION_ENV)
            .is_some_and(|value| value == "1");
        let supervised = std::env::var_os(rimz::remote::REMOTE_SUPERVISED_ENV)
            .is_some_and(|value| !value.is_empty());
        match remote_skew_action(
            &client_version,
            rimz::build_id::VERSION,
            forced,
            posture,
            supervised,
        ) {
            Some(SkewAction::Warn(notice)) => notices.push(notice),
            Some(SkewAction::Refuse { message, code }) => {
                let _ = writeln!(std::io::stderr().lock(), "rimz: {message}");
                std::process::exit(code);
            }
            None => {}
        }
    }
    if posture != StartPosture::Reconnect
        && was_live
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
    fn remote_skew_action_maps_compatibility_tiers() {
        assert_eq!(
            remote_skew_action("0.5.0", "0.5.0", false, StartPosture::Attended, false),
            None
        );

        let Some(SkewAction::Warn(patch)) =
            remote_skew_action("0.5.0", "0.5.1", false, StartPosture::Attended, false)
        else {
            panic!("patch skew warns");
        };
        assert!(patch.contains("rimz 0.5.0"), "{patch}");
        assert!(patch.contains("rimz 0.5.1"), "{patch}");

        let Some(SkewAction::Refuse { message, code }) =
            remote_skew_action("0.5.0", "0.4.9", false, StartPosture::Attended, false)
        else {
            panic!("minor skew refuses");
        };
        assert_eq!(code, rimz::remote::REMOTE_VERSION_SKEW_EXIT);
        assert!(message.contains("rimz 0.5.0"), "{message}");
        assert!(message.contains("rimz 0.4.9"), "{message}");
        assert!(message.contains("rimz remote setup <host>"), "{message}");
        assert!(message.contains("--force-version"), "{message}");

        let Some(SkewAction::Warn(forced)) =
            remote_skew_action("1.5.0", "1.4.9", true, StartPosture::Attended, false)
        else {
            panic!("forced minor skew warns");
        };
        assert!(
            forced.contains("continuing because --force-version was given"),
            "{forced}"
        );

        for (client, host, tier, expected_code) in [
            (
                "0.5.0",
                "0.4.9",
                "minor",
                rimz::remote::REMOTE_VERSION_SKEW_EXIT,
            ),
            (
                "1.0.0",
                "0.5.0",
                "major",
                rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT,
            ),
        ] {
            let Some(SkewAction::Refuse { message, code }) =
                remote_skew_action(client, host, false, StartPosture::Attended, true)
            else {
                panic!("supervised {tier} skew refuses");
            };
            assert_eq!(code, expected_code);
            assert!(message.contains(&format!("rimz {client}")), "{message}");
            assert!(message.contains(&format!("rimz {host}")), "{message}");
            assert!(
                message.contains(&format!("differ by a {tier} version")),
                "{message}"
            );
            assert!(!message.contains("rimz remote setup"), "{message}");
            assert!(!message.contains("<host>"), "{message}");
        }
    }

    #[test]
    fn reconnect_skips_skew_warnings_but_keeps_refusals() {
        for (client, host, forced) in [
            ("0.5.0", "0.5.1", false),
            ("dev", "0.5.0", false),
            ("0.5.0", "0.4.9", true),
        ] {
            assert!(matches!(
                remote_skew_action(client, host, forced, StartPosture::NoTerminal, false),
                Some(SkewAction::Warn(_))
            ));
            assert_eq!(
                remote_skew_action(client, host, forced, StartPosture::Reconnect, true),
                None
            );
        }
        for (client, host, expected_code) in [
            ("0.5.0", "0.4.9", rimz::remote::REMOTE_VERSION_SKEW_EXIT),
            (
                "1.0.0",
                "0.5.0",
                rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT,
            ),
        ] {
            let Some(SkewAction::Refuse { code, .. }) =
                remote_skew_action(client, host, false, StartPosture::Reconnect, true)
            else {
                panic!("reconnect must not bypass version refusals");
            };
            assert_eq!(code, expected_code);
        }
    }

    #[test]
    fn remote_skew_action_keeps_major_mismatches_hard() {
        for forced in [false, true] {
            let Some(SkewAction::Refuse { message, code }) =
                remote_skew_action("1.0.0", "0.5.0", forced, StartPosture::Attended, false)
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
        let Some(SkewAction::Warn(message)) =
            remote_skew_action("dev", "0.5.0", false, StartPosture::Attended, false)
        else {
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
