use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rimz::harness::worktree_entry::WorktreeEntry;
use rimz::worktree::{LandedVerdict, WorktreeMarker};

use super::usage::shell_command;
use super::{GlobalFlags, render};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Choice {
    Fresh,
    Remove,
    Resume,
}

#[derive(Clone, Copy)]
pub(super) enum PrintInput {
    Text,
    Stdin,
    StreamJson,
}

pub(super) struct Offer<'a> {
    pub(super) name: &'a str,
    pub(super) subject: &'a str,
    pub(super) facts: &'a WorktreeEntry,
    pub(super) resume_command: Option<&'a str>,
    pub(super) fresh_command: &'a str,
}

pub(super) fn inspect(
    store: &rimz::Store,
    path: &Path,
    marker: &WorktreeMarker,
) -> Result<WorktreeEntry> {
    let audit = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    Ok(rimz::harness::worktree_entry::read(
        path,
        marker,
        &audit.agents,
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::MachineConfig,
    globals: &GlobalFlags,
    store: &rimz::Store,
    name: &str,
    spec: &str,
    team: Option<&str>,
    prompt: Option<&str>,
    fresh: bool,
    fresh_command: Option<&str>,
) -> Result<()> {
    let path =
        rimz::worktree::worktree_path(workspace.launch_repo_root(), &config.agents.worktree, name)?;
    let Some(marker) = rimz::worktree::read_marker_for_worktree(&path)? else {
        return Ok(());
    };
    let alive = super::alive_snapshot(store, &workspace.session_name)?;
    let repo_root = workspace.launch_repo_root();
    if rimz::harness::worktree_entry::occupied(&path, repo_root, &alive.agents)? {
        return Ok(());
    }
    let subject = cohort_subject(spec, team);
    if fresh {
        return launch_fresh(name, &subject);
    }
    let facts = inspect(store, &path, &marker)?;
    let (_, command) = relaunch_commands(name, spec, team, None, prompt, None);
    let choice = offer(
        &Offer {
            name,
            subject: &subject,
            facts: &facts,
            resume_command: None,
            fresh_command: fresh_command.unwrap_or(&command),
        },
        std::io::stdin().is_terminal(),
    )?;
    match choice {
        Choice::Remove => remove(workspace, config, globals, store, name, &path, &subject),
        Choice::Fresh | Choice::Resume => launch_fresh(name, &subject),
    }
}

pub(super) fn offer(offer: &Offer<'_>, terminal: bool) -> Result<Choice> {
    if terminal {
        writeln!(
            std::io::stderr().lock(),
            "{}",
            facts_block(offer.name, offer.facts)
        )?;
    }
    offer_with(offer, terminal, super::choose)
}

fn offer_with(
    offer: &Offer<'_>,
    terminal: bool,
    choose: impl FnOnce(&str, &[&str], usize) -> Result<Option<usize>>,
) -> Result<Choice> {
    let Offer {
        name,
        subject,
        facts,
        resume_command,
        fresh_command,
    } = offer;
    let state = if facts.removable {
        "is clean and landed"
    } else {
        "has work in progress"
    };
    if !terminal {
        let resume = resume_command
            .map(|command| format!("resume with `{command}`, or "))
            .unwrap_or_default();
        let remove = if facts.removable {
            let command = shell_command(["rimz", "worktree", "remove", name]);
            format!("recreate {subject} with `{command}` then relaunch, ")
        } else {
            String::new()
        };
        bail!(
            "worktree `{name}` {state}; {remove}{resume}launch fresh with `{fresh_command}`\n{}",
            facts_block(name, facts)
        );
    }
    let (choices, default, action): (&[&str], _, _) = if facts.removable {
        (
            &["remove", "fresh", "cancel"],
            2,
            format!("remove it and recreate {subject}, launch it fresh, or cancel?"),
        )
    } else if resume_command.is_some() {
        (
            &["resume", "fresh", "cancel"],
            0,
            format!(
                "resume {subject}'s closed sessions, launch new agents into the same checkout, or cancel?"
            ),
        )
    } else {
        (
            &["fresh", "cancel"],
            0,
            format!("launch {subject} fresh in the same checkout, or cancel?"),
        )
    };
    match choose(
        &format!("worktree `{name}` {state}; {action}"),
        choices,
        default,
    )?
    .and_then(|index| choices.get(index))
    {
        Some(&"fresh") => Ok(Choice::Fresh),
        Some(&"remove") => Ok(Choice::Remove),
        Some(&"resume") => Ok(Choice::Resume),
        _ => canceled(*resume_command, fresh_command),
    }
}

fn facts_block(name: &str, facts: &WorktreeEntry) -> String {
    let now = jiff::Timestamp::now();
    let activity = facts
        .last_activity
        .map(|at| render::rel_age(at, now))
        .unwrap_or_else(|| "none recorded".to_owned());
    let landed = match (
        facts.status.landed,
        facts.status.target.as_deref(),
        facts.status.ahead,
    ) {
        (LandedVerdict::Landed, Some(target), _) => format!("landed on {target}"),
        (LandedVerdict::Landed, None, _) => "landed".to_owned(),
        (LandedVerdict::Unlanded, Some(target), Some(ahead)) => {
            format!("not landed ({ahead} commits ahead of {target})")
        }
        (LandedVerdict::Unlanded, _, _) => "not landed".to_owned(),
        (LandedVerdict::Unknown, _, _) => "landed: unknown".to_owned(),
    };
    format!(
        "worktree `{name}`: created {}\n  last agent activity: {activity}\n  {} uncommitted file(s); {landed}",
        render::rel_age(facts.created_at, now),
        facts.status.changed_files
    )
}

pub(super) fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
    }
}

pub(super) fn launch_fresh(name: &str, subject: &str) -> Result<()> {
    writeln!(
        std::io::stderr().lock(),
        "launching {subject} fresh in worktree `{name}`"
    )?;
    Ok(())
}

fn canceled<T>(resume_command: Option<&str>, fresh_command: &str) -> Result<T> {
    let resume = resume_command
        .map(|command| format!("resume with `{command}` or "))
        .unwrap_or_default();
    bail!("canceled; nothing launched; {resume}launch fresh with `{fresh_command}`")
}

pub(super) fn remove(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::MachineConfig,
    globals: &GlobalFlags,
    store: &rimz::Store,
    name: &str,
    path: &Path,
    subject: &str,
) -> Result<()> {
    // Read holders from both this room and the room of the repository that owns the tree.
    let protections = super::worktree_protection::for_explicit_removal(
        &[&workspace.project_root, workspace.launch_repo_root()],
        globals,
    );
    let memo = rimz::worktree::LandedMemo::load(path);
    let result = rimz::worktree::remove(
        workspace.launch_repo_root(),
        &config.agents.worktree,
        name,
        path,
        false,
        &protections.protections,
        &memo,
    );
    memo.save();
    let removed = match result {
        Err(rimz::worktree::WorktreeErr::Missing { .. }) => {
            writeln!(
                std::io::stderr().lock(),
                "worktree `{name}` was already removed; recreating {subject}"
            )?;
            return Ok(());
        }
        Err(rimz::worktree::WorktreeErr::InUse { .. }) => bail!(
            "worktree `{name}` is in use by {}; nothing removed; nothing launched",
            protections.protections.holders(path)
        ),
        result => result?,
    };
    let retirement = rimz::worktree::retire_removal(
        store,
        &removed,
        "worktree recreated",
        &workspace.session_name,
    );
    let session_retirement = retirement
        .session_retirement
        .context("retiring sessions for recreated worktree");
    let message_archival = retirement
        .message_archival
        .context("archiving messages for recreated worktree channel");
    session_retirement?;
    message_archival?;
    writeln!(
        std::io::stderr().lock(),
        "removed worktree `{name}`; recreating {subject}"
    )?;
    Ok(())
}

pub(super) fn relaunch_commands(
    name: &str,
    spec_display: &str,
    team: Option<&str>,
    channel: Option<&str>,
    prompt: Option<&str>,
    print_input: Option<PrintInput>,
) -> (String, String) {
    let target = team.map(|team| format!("{team}#{name}"));
    let resume_target = team
        .zip(channel)
        .map(|(team, channel)| format!("{team}#{channel}"));
    let (mut resume, mut fresh) = match target.as_deref() {
        Some(target) => (
            vec![
                "rimz",
                "teams",
                "resume",
                resume_target.as_deref().unwrap_or(target),
            ],
            vec!["rimz", "teams", target, "--fresh"],
        ),
        None => {
            let mut fresh = vec!["rimz", "agents", spec_display];
            if print_input.is_some() {
                fresh.push("-p");
            }
            fresh.extend(["-w", name, "--fresh"]);
            match print_input {
                Some(PrintInput::Stdin) => fresh.push("--stdin"),
                Some(PrintInput::StreamJson) => fresh.extend(["--input-format", "stream-json"]),
                _ => {}
            }
            (
                vec!["rimz", "agents", spec_display, "-w", name, "--continue"],
                fresh,
            )
        }
    };
    if let Some(prompt) = prompt {
        if team.is_some() && prompt.starts_with('-') {
            resume.push("--");
            fresh.push("--");
        }
        resume.push(prompt);
        fresh.push(prompt);
    }
    (shell_command(resume), shell_command(fresh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::worktree::{LandedVerdict, WorktreeStatus};

    fn facts(landed: LandedVerdict, changed_files: u32) -> WorktreeEntry {
        WorktreeEntry {
            created_at: jiff::Timestamp::from_second(100).unwrap(),
            status: WorktreeStatus {
                changed_files,
                landed,
                ahead: Some(2),
                target: Some("main".into()),
            },
            removable: landed == LandedVerdict::Landed && changed_files == 0,
            last_activity: None,
        }
    }

    #[test]
    fn nonterminal_refusals_carry_facts_and_only_safe_commands() {
        let observed = [
            (LandedVerdict::Landed, 1, false, "landed on main"),
            (LandedVerdict::Unlanded, 0, false, "2 commits ahead of main"),
            (LandedVerdict::Unknown, 0, false, "landed: unknown"),
            (LandedVerdict::Landed, 0, true, "landed on main"),
        ]
        .map(|(landed, dirty, removable, verdict)| {
            let facts = facts(landed, dirty);
            let error = offer_with(
                &Offer {
                    name: "demo",
                    subject: "`claude`",
                    facts: &facts,
                    resume_command: None,
                    fresh_command: "rimz agents claude -w demo --fresh",
                },
                false,
                |_, _, _| panic!("nonterminal must not prompt"),
            )
            .unwrap_err()
            .to_string();
            let correct = [
                "worktree `demo`",
                "created ",
                "last agent activity: none recorded",
                &format!("{dirty} uncommitted file(s)"),
                verdict,
                "rimz agents claude -w demo --fresh",
            ]
            .iter()
            .all(|text| error.contains(text))
                && error.contains("rimz worktree remove demo") == removable;
            (correct, error)
        });
        assert!(
            observed.iter().all(|(correct, _)| *correct),
            "{observed:#?}"
        );
    }

    #[test]
    fn enter_defaults_to_fresh_for_work_and_cancel_for_clean_landed() {
        let observed = [
            (LandedVerdict::Landed, 1),
            (LandedVerdict::Unlanded, 0),
            (LandedVerdict::Unknown, 0),
            (LandedVerdict::Landed, 0),
        ]
        .map(|(landed, dirty)| {
            offer_with(
                &Offer {
                    name: "demo",
                    subject: "`claude`",
                    facts: &facts(landed, dirty),
                    resume_command: None,
                    fresh_command: "fresh",
                },
                true,
                |_, choices, default| Ok(crate::cli::parse_choice("", choices, default)),
            )
            .map_err(|error| error.to_string())
        });
        assert_eq!(
            observed,
            [
                Ok(Choice::Fresh),
                Ok(Choice::Fresh),
                Ok(Choice::Fresh),
                Err("canceled; nothing launched; launch fresh with `fresh`".into())
            ]
        );
    }

    #[test]
    fn eof_and_unknown_answers_cancel_without_launching() {
        let observed =
            [None, Some("unknown"), Some("cancel")].map(|answer| {
                offer_with(
                    &Offer {
                        name: "demo",
                        subject: "`claude`",
                        facts: &facts(LandedVerdict::Unlanded, 0),
                        resume_command: None,
                        fresh_command: "fresh",
                    },
                    true,
                    |_, choices, default| {
                        Ok(answer
                            .and_then(|answer| crate::cli::parse_choice(answer, choices, default)))
                    },
                )
                .unwrap_err()
                .to_string()
            });
        assert_eq!(
            observed,
            ["canceled; nothing launched; launch fresh with `fresh`"; 3].map(str::to_owned)
        );
    }

    #[test]
    fn cancel_refuses_without_launching() {
        assert!(canceled::<()>(Some("resume"), "fresh").is_err());
    }

    #[test]
    fn nonterminal_work_in_progress_refuses() {
        assert!(
            offer_with(
                &Offer {
                    name: "topic",
                    subject: "forge",
                    facts: &facts(LandedVerdict::Unlanded, 1),
                    resume_command: Some("resume"),
                    fresh_command: "fresh"
                },
                false,
                |_, _, _| panic!("nonterminal must not prompt")
            )
            .is_err()
        );
    }

    #[test]
    fn nonterminal_clean_landed_refuses() {
        assert!(
            offer_with(
                &Offer {
                    name: "topic",
                    subject: "forge",
                    facts: &facts(LandedVerdict::Landed, 0),
                    resume_command: Some("resume"),
                    fresh_command: "fresh"
                },
                false,
                |_, _, _| panic!("nonterminal must not prompt")
            )
            .is_err()
        );
    }

    #[test]
    fn relaunch_commands_cover_teams_and_inline_specs() {
        assert_eq!(
            relaunch_commands("topic", "forge", Some("forge"), Some("topic"), None, None),
            (
                "rimz teams resume 'forge#topic'".to_owned(),
                "rimz teams 'forge#topic' --fresh".to_owned(),
            )
        );
        assert_eq!(
            relaunch_commands("topic", "claude,codex", None, None, None, None),
            (
                "rimz agents 'claude,codex' -w topic --continue".to_owned(),
                "rimz agents 'claude,codex' -w topic --fresh".to_owned(),
            )
        );
    }

    #[test]
    fn team_relaunch_commands_resume_the_recorded_lane_and_fresh_worktree() {
        assert_eq!(
            relaunch_commands("review", "forge", Some("forge"), Some("triage"), None, None),
            (
                "rimz teams resume 'forge#triage'".to_owned(),
                "rimz teams 'forge#review' --fresh".to_owned(),
            )
        );
    }

    #[test]
    fn relaunch_commands_preserve_prompt_as_one_argument() {
        for (team, prompt) in [
            (Some("forge"), "review 'this' and $(that)"),
            (None, "review 'this' and $(that)"),
            (Some("forge"), "--fresh"),
        ] {
            let (resume, fresh) = relaunch_commands(
                "topic",
                "claude,codex",
                team,
                Some("triage"),
                Some(prompt),
                None,
            );
            for command in [resume, fresh] {
                let words = shlex::split(&command).unwrap();
                assert_eq!(words.last().map(String::as_str), Some(prompt));
                if prompt.starts_with('-') {
                    assert_eq!(words[words.len() - 2], "--");
                }
            }
        }
    }

    #[test]
    fn print_fresh_commands_keep_prompt_as_one_argument_and_preserve_stdin_mode() {
        let prompt = "review 'this' and $(that)";
        let observed = [
            (PrintInput::Text, Some(prompt)),
            (PrintInput::Stdin, Some(prompt)),
            (PrintInput::StreamJson, None),
        ]
        .map(|(input, prompt)| {
            shlex::split(&relaunch_commands("demo", "claude", None, None, prompt, Some(input)).1)
                .unwrap()
        });
        let base = ["rimz", "agents", "claude", "-p", "-w", "demo", "--fresh"];
        let expected = [
            vec![prompt],
            vec!["--stdin", prompt],
            vec!["--input-format", "stream-json"],
        ]
        .map(|flags| {
            base.into_iter()
                .chain(flags)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        assert_eq!(observed, expected);
    }
}
