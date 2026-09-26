//! Start-time workspace, configuration, and version notices.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use rimz::RuntimePaths;
use rimz::config::definitions::{DefinitionCause, DefinitionErr};
use rimz::ids::{MuxName, WorkspaceId};
use rimz::sidebar::SessionBuildDrift;

use crate::cli::render;

fn definition_summary(errors: &[&DefinitionErr], agents_home: &Path) -> Vec<String> {
    let mut missing = BTreeMap::<_, (BTreeSet<_>, BTreeSet<_>)>::new();
    let mut dependent = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    for error in errors {
        match &error.cause {
            DefinitionCause::MissingSkill { skill, roots } => {
                let (paths, searched) = missing.entry(skill).or_default();
                paths.insert(error.path.as_path());
                searched.extend(roots.iter().map(|root| root.as_path()));
            }
            DefinitionCause::DependsOnFailed { .. } => {
                dependent.insert(error.path.as_path());
            }
            DefinitionCause::Invalid => {
                invalid.insert(error.path.as_path());
            }
        }
    }
    let mut lines = Vec::new();
    for (skill, (paths, roots)) in missing {
        let roots = roots
            .into_iter()
            .map(render::home_relative_path)
            .collect::<Vec<_>>()
            .join(" or ");
        lines.push(definition_group(
            &paths,
            agents_home,
            &format!("missing skill '{skill}' (not installed in {roots})"),
        ));
    }
    for (paths, reason) in [
        (dependent, "depending on one that failed"),
        (invalid, "with another error"),
    ] {
        if !paths.is_empty() {
            lines.push(definition_group(&paths, agents_home, reason));
        }
    }
    if !lines.is_empty() {
        lines.push("launches that select them are refused until the files are fixed; `rimz agents validate` lists every error".to_owned());
    }
    lines
}

fn definition_group(paths: &BTreeSet<&Path>, agents_home: &Path, reason: &str) -> String {
    let mut names: Vec<_> = paths
        .iter()
        .map(|path| {
            path.strip_prefix(agents_home)
                .ok()
                .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
                .map_or_else(
                    || render::home_relative_path(path),
                    |path| path.with_extension("").display().to_string(),
                )
        })
        .collect();
    names.sort();
    let count = names.len();
    names.truncate(6);
    if count > 6 {
        names.push(format!("+{} more", count - 6));
    }
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} definition{suffix} {reason}: {}", names.join(", "))
}

fn broken_config_notice(err: &rimz::config::ConfigErr) -> String {
    let path = render::home_relative(&err.path().display().to_string());
    let detail = err
        .diagnosis()
        .map(rimz::config::ConfigFileDiagnosis::summary)
        .unwrap_or_else(|| render::one_line_error(err));
    if err.diagnosis().is_some() {
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
    let errors = rimz::config::broken_machine_files();
    let definitions: Vec<_> = errors
        .iter()
        .filter_map(|error| match error {
            rimz::config::ConfigErr::Definition(error) => Some(error),
            _ => None,
        })
        .collect();
    let mut notices = definition_summary(&definitions, &rimz::disk::paths::agents_home());
    notices.extend(
        errors
            .iter()
            .filter(|error| !matches!(error, rimz::config::ConfigErr::Definition(_)))
            .map(broken_config_notice),
    );
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
    fn definition_summary_groups_causes_and_deduplicates_team_roles() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let agents_home = home.join(".rimz");
        let missing = DefinitionCause::MissingSkill {
            skill: "rimz-lsp".parse().unwrap(),
            roots: vec![home.join(".claude/skills"), agents_home.join("skills")],
        };
        let failed = DefinitionCause::DependsOnFailed {
            name: "finder".to_owned(),
        };
        let names = [
            "agents/coder",
            "agents/planner",
            "subagents/finder",
            "subagents/newcomer",
            "subagents/surveyor",
        ];
        let mut errors: Vec<_> = names
            .iter()
            .map(|name| DefinitionErr {
                path: agents_home.join(format!("{name}.md")),
                message: "detail".to_owned(),
                cause: missing.clone(),
            })
            .collect();
        for name in ["agents/astra", "agents/reviewer", "teams/x", "teams/x"] {
            errors.push(DefinitionErr {
                path: agents_home.join(format!("{name}.md")),
                message: "detail".to_owned(),
                cause: failed.clone(),
            });
        }
        let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &agents_home);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("5 definitions"));
        assert!(lines[0].contains("'rimz-lsp'"));
        assert!(lines[0].contains("~/.claude/skills or ~/.rimz/skills"));
        for name in names {
            assert!(lines[0].contains(name));
        }
        assert!(lines[1].starts_with("3 definitions"));
        assert_eq!(lines[1].matches("teams/x").count(), 1);
        assert!(lines[2].contains("rimz agents validate"));
        assert!(
            lines
                .iter()
                .all(|line| line.len() <= 210 && !line.contains(&home.display().to_string())),
            "{lines:?}"
        );
    }

    #[test]
    fn definition_summary_caps_names_and_handles_other_errors() {
        let home = PathBuf::from("/definitions");
        let mut errors: Vec<_> = (0..9)
            .rev()
            .map(|n| DefinitionErr {
                path: home.join(format!("agents/a{n}.md")),
                message: "detail".to_owned(),
                cause: DefinitionCause::MissingSkill {
                    skill: "one".parse().unwrap(),
                    roots: vec![home.join("skills")],
                },
            })
            .collect();
        errors.push(DefinitionErr {
            path: PathBuf::from("/outside/bad.md"),
            message: "detail".to_owned(),
            cause: DefinitionCause::Invalid,
        });
        let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("9 definitions"));
        assert!(lines[0].ends_with(
            "agents/a0, agents/a1, agents/a2, agents/a3, agents/a4, agents/a5, +3 more"
        ));
        assert!(lines[1].starts_with("1 definition"));
        assert!(lines[1].ends_with("/outside/bad.md"));
        assert!(definition_summary(&[], &home).is_empty());
    }

    #[test]
    fn definition_summary_unions_roots_per_skill() {
        let home = PathBuf::from("/definitions");
        let errors: Vec<_> = [("one", "/a"), ("one", "/b"), ("two", "/a")]
            .into_iter()
            .map(|(skill, root)| DefinitionErr {
                path: home.join("agents/same.md"),
                message: "detail".to_owned(),
                cause: DefinitionCause::MissingSkill {
                    skill: skill.parse().unwrap(),
                    roots: vec![PathBuf::from(root)],
                },
            })
            .collect();
        let lines = definition_summary(&errors.iter().collect::<Vec<_>>(), &home);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("1 definition"));
        assert!(lines[0].contains("'one'") && lines[0].contains("/a or /b"));
        assert!(lines[1].contains("'two'"));
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
