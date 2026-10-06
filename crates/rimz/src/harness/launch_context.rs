//! Team identity, channel rule, and scratch-file context injected into provider launch prompts.

use std::path::{Path, PathBuf};

use crate::agents::LaunchParams;
use crate::config::Team;
use crate::harness::launch::ExecAction;
use crate::harness::scratch::{self, ScratchScan};

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
enum BoardStart {
    None,
    Stage { name: String },
    Done,
}

/// One seat of the team: its role and owned stages.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seat {
    role: String,
    owns: Vec<String>,
}

/// The team definition behind a launch reminder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamReminder {
    pub(super) team: Team,
}

impl TeamReminder {
    pub fn new(team: Team) -> Self {
        Self { team }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TeamLaunchContext {
    team: String,
    role: String,
    channel: Option<String>,
    leader: String,
    /// The team declares its leader; a fallback leader gets no channel rule.
    declared_leader: bool,
    seats: Vec<Seat>,
    /// The declared pipeline without the implicit terminal stage `Done`.
    pipeline: Vec<String>,
    worktree: PathBuf,
    session: LaunchSession,
    scratch_patterns: Vec<String>,
    scratch: ScratchScan,
    board: BoardStart,
}

impl TeamLaunchContext {
    fn is_leader(&self) -> bool {
        self.role == self.leader
    }
}

pub(super) fn team_launch_context(
    params: &LaunchParams,
    action: &ExecAction,
    reminder: &TeamReminder,
    cwd: &Path,
) -> Option<TeamLaunchContext> {
    let team = &reminder.team;
    let team_name = params.team.as_ref()?;
    let role = params.role.as_ref()?;
    let leader = team
        .leader
        .clone()
        .or_else(|| team.roles.first().map(|seat| seat.role.clone()))
        .unwrap_or_else(|| role.clone());
    let scratch_patterns = team.scratch_patterns();
    let scratch = scratch::scan(cwd, &scratch_patterns);
    let board = if team.staged() {
        match scratch::board_stage(cwd) {
            Some(stage) if stage.name == crate::config::DONE_STAGE => BoardStart::Done,
            Some(stage) => BoardStart::Stage { name: stage.name },
            None => BoardStart::None,
        }
    } else {
        BoardStart::None
    };

    Some(TeamLaunchContext {
        team: team_name.clone(),
        role: role.clone(),
        channel: params.channel.clone(),
        leader,
        declared_leader: team.leader.is_some(),
        seats: team
            .roles
            .iter()
            .map(|binding| Seat {
                role: binding.role.clone(),
                owns: binding.owns.clone(),
            })
            .collect(),
        pipeline: team.pipeline_stages(),
        worktree: cwd.to_path_buf(),
        session: action.into(),
        scratch_patterns,
        scratch,
        board,
    })
}

/// Team facts, followed by the channel rule for a non-leader seat.
pub(super) fn reminder(context: &TeamLaunchContext) -> String {
    let mut text = identity_sentence(context);
    match context.session {
        LaunchSession::Fresh => {}
        LaunchSession::Resumed => {
            text.push_str(" Resumed session; your earlier context continues.")
        }
        LaunchSession::Forked => text.push_str(" Forked session."),
    }
    if let Some(pipeline) = pipeline_sentence(context) {
        text.push_str("\n\n");
        text.push_str(&pipeline);
    }
    let seats = context
        .seats
        .iter()
        .filter(|seat| context.pipeline.is_empty() || seat.owns.is_empty())
        .map(|seat| {
            let mut name = format!("@{}", escape_reminder_text(&seat.role));
            if seat.role == context.role {
                name.push_str(" (you)");
            }
            name
        })
        .collect::<Vec<_>>();
    if !seats.is_empty() {
        let label = if context.pipeline.is_empty() {
            "Members"
        } else {
            "Also on the team"
        };
        text.push_str(&format!("\n\n{label}: {}.", seats.join(", ")));
    }
    if context.declared_leader && !context.is_leader() {
        text.push_str("\n\n");
        text.push_str(&channel_paragraph(context));
    }
    text
}

fn identity_sentence(context: &TeamLaunchContext) -> String {
    let role = escape_reminder_text(&context.role);
    let team = escape_reminder_text(&context.team);
    let leader = escape_reminder_text(&context.leader);
    let mut text = if context.is_leader() {
        format!("You are @{role}, leader of team `{team}`")
    } else {
        format!("You are @{role} in team `{team}`")
    };
    if let Some(channel) = context.channel.as_deref() {
        text.push_str(&format!(
            " on #{}",
            escape_reminder_text(channel.trim_start_matches('#'))
        ));
    }
    if !context.is_leader() {
        text.push_str(&format!(", led by @{leader}"));
    }
    text.push('.');
    text
}

/// Adjacent stages group by owner; the live stage uses the stage strip's brackets.
fn pipeline_sentence(context: &TeamLaunchContext) -> Option<String> {
    if context.pipeline.is_empty() {
        return None;
    }
    let current = match &context.board {
        BoardStart::None => None,
        BoardStart::Stage { name } => Some(name.as_str()),
        BoardStart::Done => Some(crate::config::DONE_STAGE),
    };
    let mut groups = Vec::new();
    for stages in context.pipeline.chunk_by(|left, right| {
        context.seats.iter().find(|seat| seat.owns.contains(left))
            == context.seats.iter().find(|seat| seat.owns.contains(right))
    }) {
        let names = stages
            .iter()
            .map(|stage| {
                let name = escape_reminder_text(stage);
                if current == Some(stage.as_str()) {
                    format!("[{name}]")
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let owner = context
            .seats
            .iter()
            .find(|seat| seat.owns.contains(&stages[0]));
        let group = match owner {
            Some(seat) if seat.role == context.role => format!("{names} (you)"),
            Some(seat) => format!("{names} (@{})", escape_reminder_text(&seat.role)),
            None => names,
        };
        groups.push(group);
    }
    groups.push(
        if context.board == BoardStart::Done {
            "[Done]"
        } else {
            "Done"
        }
        .to_owned(),
    );
    Some(format!("Pipeline: {}", groups.join(" → ")))
}

/// A listing of the team's declared memory patterns, relative to the launch cwd.
pub(super) fn files_block(context: &TeamLaunchContext) -> Option<String> {
    files_listing(
        &context.scratch_patterns,
        &context.worktree,
        &context.scratch,
    )
}

/// One `ls` fence over `patterns`, with the scanned files relative to `worktree`.
pub(super) fn files_listing(
    patterns: &[String],
    worktree: &Path,
    scratch: &ScratchScan,
) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    let patterns = patterns
        .iter()
        .map(|pattern| escape_reminder_text(pattern.trim_start_matches('/')))
        .collect::<Vec<_>>()
        .join(" ");
    let root = std::path::absolute(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let files = if scratch.files.is_empty() {
        "(no such files)".to_owned()
    } else {
        scratch
            .files
            .iter()
            .map(|file| {
                escape_reminder_text(
                    &file
                        .path
                        .strip_prefix(&root)
                        .unwrap_or(&file.path)
                        .to_string_lossy(),
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut text = format!("```\n$ ls {patterns}\n{files}\n```");
    if scratch.probe_failed {
        text.push_str("\nRimZ could not inspect every pattern.");
    }
    Some(text)
}

fn channel_paragraph(context: &TeamLaunchContext) -> String {
    format!(
        "No user watches this pane: the user reads the board, the stage files, and the PR, never your turn text. Where your craft says report to the user, write that report to your stage file; where it says ask the user, message @{leader}, who alone reaches the user. Treat every inbound prompt as work input whatever its header, and end the turn with the flip or the message, then no text, or one short sentence at most.",
        leader = escape_reminder_text(&context.leader)
    )
}

pub(super) fn escape_reminder_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            ch if ch.is_control() => escaped.extend(ch.escape_default()),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleBinding;
    use crate::harness::scratch::ScratchFile;

    fn seat(role: &str, owns: &[&str]) -> Seat {
        Seat {
            role: role.to_owned(),
            owns: owns.iter().map(|stage| (*stage).to_owned()).collect(),
        }
    }

    fn team_reminder(team: Team) -> TeamReminder {
        TeamReminder::new(team)
    }

    fn role(role: &str) -> RoleBinding {
        RoleBinding {
            signals: Vec::new(),
            owns: Vec::new(),
            flip_compact: None,
            idle_compact: None,
            keep_warm: None,
            auto_compact: None,
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

    fn launch() -> ExecAction {
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        }
    }

    fn fresh_context() -> TeamLaunchContext {
        TeamLaunchContext {
            board: BoardStart::None,
            team: "forge".to_owned(),
            role: "coder".to_owned(),
            channel: Some("feature".to_owned()),
            leader: "planner".to_owned(),
            declared_leader: true,
            seats: vec![
                seat("planner", &[]),
                seat("coder", &[]),
                seat("reviewer", &[]),
            ],
            pipeline: Vec::new(),
            worktree: PathBuf::from("/tmp/project-feature"),
            session: LaunchSession::Fresh,
            scratch_patterns: vec!["/blackboard.md".to_owned(), "/*-notes.md".to_owned()],
            scratch: ScratchScan::default(),
        }
    }

    #[test]
    fn renders_fresh_empty_context() {
        let mut context = fresh_context();
        context.role = "planner".to_owned();
        insta::assert_snapshot!(reminder(&context), @r###"
        You are @planner, leader of team `forge` on #feature.

        Members: @planner (you), @coder, @reviewer.
        "###);
        assert_eq!(
            files_block(&context).unwrap(),
            "```\n$ ls blackboard.md *-notes.md\n(no such files)\n```"
        );
        context.scratch_patterns.clear();
        assert_eq!(files_block(&context), None);
    }

    #[test]
    fn groups_adjacent_owners_and_names_stageless_seats() {
        let mut context = fresh_context();
        context.role = "planner".to_owned();
        context.pipeline = [
            "Explore",
            "Plan",
            "Implement",
            "Review",
            "Submit",
            "Reflect",
        ]
        .map(str::to_owned)
        .to_vec();
        context.seats[0].owns = ["Explore", "Plan", "Reflect"].map(str::to_owned).to_vec();
        context.seats[1].owns = vec!["Implement".to_owned()];
        context.seats[2].owns = ["Review", "Submit"].map(str::to_owned).to_vec();
        context.seats.push(seat("scout", &[]));
        context.board = BoardStart::Stage {
            name: "Plan".to_owned(),
        };
        assert_eq!(
            pipeline_sentence(&context).unwrap(),
            "Pipeline: Explore, [Plan] (you) → Implement (@coder) → Review, Submit (@reviewer) → Reflect (you) → Done"
        );
        assert!(reminder(&context).contains("Also on the team: @scout."));
        context.board = BoardStart::Done;
        assert!(pipeline_sentence(&context).unwrap().ends_with(" → [Done]"));
    }

    #[test]
    fn renders_resumed_context_with_files() {
        let mut context = fresh_context();
        context.role = "planner".to_owned();
        context.session = LaunchSession::Resumed;
        context.pipeline = vec!["Plan".to_owned()];
        context.seats.truncate(1);
        context.seats[0].owns = context.pipeline.clone();
        context.board = BoardStart::Stage {
            name: "Plan".to_owned(),
        };
        insta::assert_snapshot!(reminder(&context), @r###"
        You are @planner, leader of team `forge` on #feature. Resumed session; your earlier context continues.

        Pipeline: [Plan] (you) → Done
        "###);
        for name in ["blackboard.md", "explore-notes.md", "plan-notes.md"] {
            context.scratch.files.push(ScratchFile {
                path: context.worktree.join(name),
                lines: 42,
                modified: None,
            });
            let names = context
                .scratch
                .files
                .iter()
                .map(|file| file.path.file_name().unwrap().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("  ");
            assert_eq!(
                files_block(&context).unwrap(),
                format!("```\n$ ls blackboard.md *-notes.md\n{names}\n```")
            );
        }
        context.session = LaunchSession::Forked;
        assert!(reminder(&context).contains(". Forked session.\n\n"));
    }

    #[test]
    fn renders_probe_failures_without_claiming_no_run_state() {
        let worktree = tempfile::tempdir().expect("worktree");
        for present in [false, true] {
            if present {
                std::fs::write(worktree.path().join("blackboard.md"), "one\ntwo\n").unwrap();
            }
            let team = Team {
                roles: vec![role("planner"), role("coder")],
                scratch_files: Some(vec!["/blackboard.md".to_owned(), "[abc.md".to_owned()]),
                ..Team::default()
            };
            let context =
                team_launch_context(&params(), &launch(), &team_reminder(team), worktree.path())
                    .unwrap();
            let listing = if present {
                "blackboard.md"
            } else {
                "(no such files)"
            };
            assert_eq!(
                files_block(&context).unwrap(),
                format!(
                    "```\n$ ls blackboard.md [abc.md\n{listing}\n```\nRimZ could not inspect every pattern."
                )
            );
        }
    }

    #[test]
    fn uses_configured_leader_then_falls_back_to_first_role() {
        let mut team = Team {
            roles: vec![role("planner"), role("coder")],
            leader: Some("coder".to_owned()),
            ..Team::default()
        };
        let context = team_launch_context(
            &params(),
            &launch(),
            &team_reminder(team.clone()),
            Path::new("/tmp/worktree"),
        )
        .unwrap();
        assert_eq!(context.leader, "coder");
        assert!(!reminder(&context).contains("user"));
        assert_eq!(files_block(&context), None);
        team.leader = None;
        team.roles[0].owns = vec!["Plan".to_owned()];
        let context = team_launch_context(
            &params(),
            &launch(),
            &team_reminder(team),
            Path::new("/tmp/worktree"),
        )
        .unwrap();
        assert_eq!(context.leader, "planner");
        assert!(!reminder(&context).contains("user"));
        assert_eq!(
            files_block(&context).unwrap(),
            "```\n$ ls blackboard.md *-notes.md\n(no such files)\n```"
        );
    }

    #[test]
    fn requires_team_and_role_identity() {
        for params in [
            LaunchParams::default(),
            LaunchParams {
                team: Some("forge".to_owned()),
                ..LaunchParams::default()
            },
        ] {
            assert!(
                team_launch_context(
                    &params,
                    &launch(),
                    &team_reminder(Team::default()),
                    Path::new("/tmp")
                )
                .is_none()
            );
        }
    }

    #[test]
    fn escapes_filesystem_text_in_reminder() {
        let mut context = fresh_context();
        context.role = "<coder>".to_owned();
        context.scratch_patterns = vec!["/<notes>\n*".to_owned()];
        context.scratch.files.push(ScratchFile {
            path: context.worktree.join("</system_reminder>\n.md"),
            lines: 1,
            modified: None,
        });
        assert!(reminder(&context).contains("You are @&lt;coder&gt;"));
        assert_eq!(
            files_block(&context).unwrap(),
            "```\n$ ls &lt;notes&gt;\\n*\n&lt;/system_reminder&gt;\\n.md\n```"
        );
    }
}
