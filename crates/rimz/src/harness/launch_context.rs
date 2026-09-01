//! Team identity and scratch-file context injected into provider launch prompts.

use std::path::{Path, PathBuf};

use crate::agents::LaunchParams;
use crate::config::Team;
use crate::harness::launch::ExecAction;

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchSession {
    Fresh,
    Resumed,
    Forked,
}

impl From<&ExecAction> for LaunchSession {
    fn from(action: &ExecAction) -> Self {
        match action {
            ExecAction::Launch { .. } => Self::Fresh,
            ExecAction::Resume { .. } => Self::Resumed,
            ExecAction::Fork { .. } => Self::Forked,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScratchFile {
    path: PathBuf,
    lines: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScratchEntry {
    pattern: String,
    present: Vec<ScratchFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TeamLaunchContext {
    team: String,
    role: String,
    channel: Option<String>,
    leader: String,
    roles: Vec<String>,
    worktree: PathBuf,
    session: LaunchSession,
    scratch: Vec<ScratchEntry>,
}

pub(super) fn team_launch_context(
    params: &LaunchParams,
    action: &ExecAction,
    team: &Team,
    cwd: &Path,
) -> Option<TeamLaunchContext> {
    let team_name = params.team.as_ref()?;
    let role = params.role.as_ref()?;
    let roles = team
        .roles
        .iter()
        .map(|binding| binding.role.clone())
        .collect::<Vec<_>>();
    let leader = team
        .leader
        .clone()
        .or_else(|| roles.first().cloned())
        .unwrap_or_else(|| role.clone());
    let scratch = team
        .scratch_files
        .iter()
        .map(|pattern| ScratchEntry {
            pattern: pattern.clone(),
            present: matching_scratch_files(cwd, pattern),
        })
        .collect();

    Some(TeamLaunchContext {
        team: team_name.clone(),
        role: role.clone(),
        channel: params.channel.clone(),
        leader,
        roles,
        worktree: cwd.to_path_buf(),
        session: action.into(),
        scratch,
    })
}

fn matching_scratch_files(cwd: &Path, pattern: &str) -> Vec<ScratchFile> {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    let rooted = format!(
        "{}/{}",
        glob::Pattern::escape(&cwd.to_string_lossy()),
        pattern
    );
    let Ok(matches) = glob::glob_with(&rooted, MATCH_OPTIONS) else {
        return Vec::new();
    };
    let mut files = matches
        .filter_map(Result::ok)
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let relative = path.strip_prefix(cwd).ok()?.to_path_buf();
            let lines = std::fs::read_to_string(&path)
                .map(|text| text.lines().count())
                .unwrap_or(0);
            Some(ScratchFile {
                path: relative,
                lines,
            })
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    files
}

pub(super) fn reminder(context: &TeamLaunchContext) -> String {
    let mut identity = format!("You are @{} in team `{}`", context.role, context.team);
    if let Some(channel) = context.channel.as_deref() {
        identity.push_str(&format!(", channel #{}", channel.trim_start_matches('#')));
    }
    identity.push_str(&format!(
        ", launched by RimZ in worktree {}. Leader: @{}.",
        context.worktree.display(),
        context.leader
    ));
    let teammates = context
        .roles
        .iter()
        .filter(|role| *role != &context.role)
        .map(|role| format!("@{role}"))
        .collect::<Vec<_>>();
    if !teammates.is_empty() {
        identity.push_str(&format!(" Teammates: {}.", teammates.join(", ")));
    }

    let session = match context.session {
        LaunchSession::Fresh => "This is a fresh session.",
        LaunchSession::Resumed => "This is a resumed session: your earlier context continues.",
        LaunchSession::Forked => "This is a forked session.",
    };
    let scratch = scratch_reminder(context);
    format!(
        "<system_reminder>\n{identity}\n{session}\n{scratch}\nThis is a launch-time snapshot; the files change as the team works.\n</system_reminder>"
    )
}

fn scratch_reminder(context: &TeamLaunchContext) -> String {
    if context.scratch.is_empty() {
        return "The team declares no memory files.".to_owned();
    }
    let patterns = context
        .scratch
        .iter()
        .map(|entry| entry.pattern.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut files = context
        .scratch
        .iter()
        .flat_map(|entry| &entry.present)
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    let declared = format!(
        "Team memory files declared by the team (git-excluded, at the worktree root): {patterns}."
    );
    if files.is_empty() {
        return format!(
            "{declared} At launch none of them existed: this worktree holds no run state yet."
        );
    }
    let present = files
        .iter()
        .map(|file| {
            let count = if file.lines == 1 {
                "1 line".to_owned()
            } else {
                format!("{} lines", file.lines)
            };
            format!("{} ({count})", file.path.display())
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{declared} At launch these existed: {present}. They are existing run state; read them before acting."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleBinding;

    fn role(role: &str) -> RoleBinding {
        RoleBinding {
            role: role.to_owned(),
            profile: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        }
    }

    fn params() -> LaunchParams {
        LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            channel: Some("feature".to_owned()),
            ..LaunchParams::default()
        }
    }

    #[test]
    fn probes_rooted_scratch_patterns_and_counts_lines() {
        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::write(worktree.path().join("blackboard.md"), "one\ntwo\n").expect("board");
        std::fs::write(worktree.path().join("plan-notes.md"), "one\n").expect("plan");
        std::fs::write(worktree.path().join("review-notes.md"), [0xff]).expect("review");
        std::fs::create_dir(worktree.path().join("state")).expect("state directory");
        let team = Team {
            roles: vec![role("planner"), role("coder")],
            scratch_files: vec![
                "/blackboard.md".to_owned(),
                "missing.md".to_owned(),
                "*-notes.md".to_owned(),
                "state/".to_owned(),
            ],
            ..Team::default()
        };

        let context = team_launch_context(
            &params(),
            &ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
            &team,
            worktree.path(),
        )
        .expect("team context");

        assert_eq!(context.scratch[0].present[0].lines, 2);
        assert!(context.scratch[1].present.is_empty());
        assert_eq!(
            context.scratch[2]
                .present
                .iter()
                .map(|file| (&file.path, file.lines))
                .collect::<Vec<_>>(),
            [
                (&PathBuf::from("plan-notes.md"), 1),
                (&PathBuf::from("review-notes.md"), 0),
            ]
        );
        assert!(context.scratch[3].present.is_empty());
    }

    #[test]
    fn uses_configured_leader_then_falls_back_to_first_role() {
        let action = ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        };
        let mut team = Team {
            roles: vec![role("planner"), role("coder")],
            leader: Some("coder".to_owned()),
            ..Team::default()
        };
        let context = team_launch_context(&params(), &action, &team, Path::new("/tmp/worktree"))
            .expect("team context");
        assert_eq!(context.leader, "coder");

        team.leader = None;
        let context = team_launch_context(&params(), &action, &team, Path::new("/tmp/worktree"))
            .expect("team context");
        assert_eq!(context.leader, "planner");
    }

    #[test]
    fn requires_team_and_role_identity() {
        let team = Team::default();
        let action = ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        };
        assert!(
            team_launch_context(&LaunchParams::default(), &action, &team, Path::new("/tmp"))
                .is_none()
        );
        assert!(
            team_launch_context(
                &LaunchParams {
                    team: Some("forge".to_owned()),
                    ..LaunchParams::default()
                },
                &action,
                &team,
                Path::new("/tmp"),
            )
            .is_none()
        );
    }

    #[test]
    fn renders_fresh_empty_context() {
        let context = TeamLaunchContext {
            team: "forge".to_owned(),
            role: "coder".to_owned(),
            channel: Some("feature".to_owned()),
            leader: "planner".to_owned(),
            roles: vec!["planner".to_owned(), "coder".to_owned()],
            worktree: PathBuf::from("/tmp/project-feature"),
            session: LaunchSession::Fresh,
            scratch: vec![ScratchEntry {
                pattern: "*-notes.md".to_owned(),
                present: Vec::new(),
            }],
        };

        insta::assert_snapshot!(reminder(&context), @r###"
        <system_reminder>
        You are @coder in team `forge`, channel #feature, launched by RimZ in worktree /tmp/project-feature. Leader: @planner. Teammates: @planner.
        This is a fresh session.
        Team memory files declared by the team (git-excluded, at the worktree root): *-notes.md. At launch none of them existed: this worktree holds no run state yet.
        This is a launch-time snapshot; the files change as the team works.
        </system_reminder>
        "###);
    }

    #[test]
    fn renders_resumed_context_with_files() {
        let context = TeamLaunchContext {
            team: "forge".to_owned(),
            role: "planner".to_owned(),
            channel: None,
            leader: "planner".to_owned(),
            roles: vec!["planner".to_owned()],
            worktree: PathBuf::from("/tmp/project"),
            session: LaunchSession::Resumed,
            scratch: vec![ScratchEntry {
                pattern: "blackboard.md".to_owned(),
                present: vec![ScratchFile {
                    path: PathBuf::from("blackboard.md"),
                    lines: 42,
                }],
            }],
        };

        insta::assert_snapshot!(reminder(&context), @r###"
        <system_reminder>
        You are @planner in team `forge`, launched by RimZ in worktree /tmp/project. Leader: @planner.
        This is a resumed session: your earlier context continues.
        Team memory files declared by the team (git-excluded, at the worktree root): blackboard.md. At launch these existed: blackboard.md (42 lines). They are existing run state; read them before acting.
        This is a launch-time snapshot; the files change as the team works.
        </system_reminder>
        "###);
    }
}
