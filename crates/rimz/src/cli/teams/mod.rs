//! `rimz teams` — discover, inspect, launch, and drive named teams.

mod cohort;
mod install;
mod list;
mod show;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, agents_cmd};

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct TeamsArgs {
    #[command(subcommand)]
    command: Option<TeamsSubcmd>,
    /// Configured team to launch.
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: Option<String>,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    #[command(flatten)]
    launch: agents_cmd::CohortLaunchArgs,
    /// Emit the team catalogue as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TeamsSubcmd {
    /// Show one team's definition and live instances.
    #[command(alias = "inspect")]
    Show {
        #[arg(
            value_name = "NAME",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
        )]
        name: String,
        /// Emit the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Launch a configured team.
    Launch(TeamLaunchArgs),
    /// List configured teams and their live cohorts.
    #[command(alias = "ls")]
    List {
        /// Emit the team catalogue as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume a configured team's prior cohort.
    Resume(ResumeArgs),
    /// Stop every live member of a team cohort.
    Stop(CohortArgs),
    /// Focus the member of a team cohort that needs attention.
    Focus(CohortArgs),
    /// Restart every live member of a team cohort.
    Restart(CohortArgs),
    /// List or install team bundles from the matching RimZ release.
    Install(install::InstallArgs),
}

#[derive(Debug, Args)]
#[command(
    after_help = "`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend. `rimz agents` sets what an agent is — model, effort, prompts, permission posture, name, pane placement, supervised runs."
)]
struct TeamLaunchArgs {
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    #[command(flatten)]
    launch: agents_cmd::CohortLaunchArgs,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Scope resume to one worktree.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = "",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
    /// Open without focusing the resumed team tab.
    #[arg(long)]
    bg: bool,
}

#[derive(Debug, Args)]
struct CohortArgs {
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Select one live cohort by worktree name or lane.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
}

pub fn run(args: TeamsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        None => match args.name {
            Some(name) => {
                if args.json {
                    bail!(
                        "--json is only supported with `rimz teams` and `rimz teams list`; use `rimz teams show {name} --json` for one team"
                    );
                }
                launch_team(name, args.prompt, args.launch, globals)
            }
            None => {
                reject_launch_flags_without_name(&args.prompt, &args.launch)?;
                list::run(args.json, globals)
            }
        },
        Some(TeamsSubcmd::Show { name, json }) => show::run(&name, json, globals),
        Some(TeamsSubcmd::List { json }) => list::run(json, globals),
        Some(TeamsSubcmd::Launch(args)) => {
            launch_team(args.name, args.prompt, args.launch, globals)
        }
        Some(TeamsSubcmd::Resume(args)) => {
            ensure_defined(&args.name, globals)?;
            agents_cmd::run(
                agents_cmd::AgentsArgs::from_launch(agents_cmd::AgentLaunchArgs {
                    spec: Some(args.name),
                    cohort: agents_cmd::CohortLaunchArgs {
                        worktree: args.worktree,
                        resume: true,
                        bg: args.bg,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                globals,
            )
        }
        Some(TeamsSubcmd::Stop(args)) => {
            ensure_defined(&args.name, globals)?;
            cohort::stop(&args.name, args.worktree.as_deref(), globals)
        }
        Some(TeamsSubcmd::Focus(args)) => {
            ensure_defined(&args.name, globals)?;
            cohort::focus(&args.name, args.worktree.as_deref(), globals)
        }
        Some(TeamsSubcmd::Restart(args)) => {
            ensure_defined(&args.name, globals)?;
            cohort::restart(&args.name, args.worktree.as_deref(), globals)
        }
        Some(TeamsSubcmd::Install(args)) => install::run(args),
    }
}

fn launch_team(
    name: String,
    prompt: Option<String>,
    launch: agents_cmd::CohortLaunchArgs,
    globals: &GlobalFlags,
) -> Result<()> {
    ensure_defined(&name, globals)?;
    agents_cmd::run(
        agents_cmd::AgentsArgs::from_launch(agents_cmd::AgentLaunchArgs {
            spec: Some(name),
            prompt,
            cohort: launch,
            ..Default::default()
        }),
        globals,
    )
}

fn reject_launch_flags_without_name(
    prompt: &Option<String>,
    launch: &agents_cmd::CohortLaunchArgs,
) -> Result<()> {
    if prompt.is_some()
        || launch.description.is_some()
        || launch.worktree.is_some()
        || launch.channel.is_some()
        || launch.from_pr.is_some()
        || launch.resume
        || launch.budget.is_some()
        || launch.bg
        || launch.new_tab
    {
        bail!("team launch options require a team name");
    }
    Ok(())
}

fn ensure_defined(name: &str, globals: &GlobalFlags) -> Result<()> {
    let teams = list::effective_teams(globals)?;
    validate_team_name(name, &teams)
}

fn validate_team_name(name: &str, teams: &rimz::config::TeamsConfig) -> Result<()> {
    if teams.0.contains_key(name) {
        return Ok(());
    }
    if let Some((team, _role)) = name.split_once('.')
        && teams.0.contains_key(team)
    {
        bail!("`{name}` names one role; launch it with `rimz agents {name}`");
    }
    let valid = teams.0.keys().cloned().collect::<Vec<_>>();
    if valid.is_empty() {
        bail!(
            "unknown team `{name}`; no teams are configured (install one with `rimz teams install forge`)"
        );
    }
    bail!(
        "unknown team `{name}`; configured teams: {}",
        valid.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TeamsHarness {
        #[command(flatten)]
        args: TeamsArgs,
    }

    #[derive(Debug, Parser)]
    struct AgentsHarness {
        #[command(flatten)]
        args: agents_cmd::AgentsArgs,
    }

    fn parse_teams(argv: &[&str]) -> TeamsArgs {
        TeamsHarness::try_parse_from(argv)
            .expect("parse teams command")
            .args
    }

    #[test]
    fn launch_guard_accepts_only_defined_teams_and_lists_choices() {
        let teams = rimz::config::TeamsConfig(BTreeMap::from([(
            "forge".to_owned(),
            rimz::config::Team::default(),
        )]));

        validate_team_name("forge", &teams).unwrap();
        let error = validate_team_name("missing", &teams).unwrap_err();
        assert!(error.to_string().contains("configured teams: forge"));

        let error = validate_team_name("forge.reviewer", &teams).unwrap_err();
        assert!(error.to_string().contains("rimz agents forge.reviewer"));
    }

    #[test]
    fn all_team_launch_doorways_parse_the_same_cohort_payload() {
        let agents = AgentsHarness::try_parse_from([
            "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg",
        ])
        .expect("parse agents launch")
        .args;
        let bare = parse_teams(&[
            "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg",
        ]);
        let verb = parse_teams(&[
            "rimz", "launch", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg",
        ]);

        assert_eq!(agents.launch.spec.as_deref(), Some("forge"));
        assert_eq!(agents.launch.prompt.as_deref(), Some("ship"));
        assert_eq!(bare.name.as_deref(), Some("forge"));
        assert_eq!(bare.prompt.as_deref(), Some("ship"));
        assert_eq!(bare.launch, agents.launch.cohort);
        let Some(TeamsSubcmd::Launch(verb)) = verb.command else {
            panic!("launch verb");
        };
        assert_eq!(verb.name, "forge");
        assert_eq!(verb.prompt.as_deref(), Some("ship"));
        assert_eq!(verb.launch, bare.launch);
    }

    #[test]
    fn launch_flags_without_a_team_name_are_rejected() {
        let args = parse_teams(&["rimz", "-w", "feat-x"]);

        let error =
            reject_launch_flags_without_name(&args.prompt, &args.launch).expect_err("missing team");

        assert!(error.to_string().contains("require a team name"));
    }
}
