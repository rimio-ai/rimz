//! `rimz teams` — discover, inspect, install, launch, and resume named teams.

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
    /// Emit the team catalogue as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TeamsSubcmd {
    /// Show one team's definition and live instances.
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
    Launch(LaunchArgs),
    /// Resume a configured team's prior cohort.
    Resume(ResumeArgs),
    /// List or install team bundles from the matching RimZ release.
    Install(install::InstallArgs),
}

#[derive(Debug, Args)]
struct LaunchArgs {
    #[arg(
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Seed the member cards' description until agents name their sessions.
    #[arg(long, value_name = "TEXT")]
    description: Option<String>,
    /// Use a RimZ-owned worktree. Bare flag creates one fresh worktree; NAME reuses or creates it.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "channel",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
    /// Launch into a durable named channel.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "worktree",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::channels)
    )]
    channel: Option<String>,
    /// Create or reuse a RimZ-owned worktree from a pull request number or URL.
    #[arg(
        long = "from-pr",
        value_name = "PR",
        value_parser = rimz::forge::parse,
        conflicts_with = "channel"
    )]
    from_pr: Option<rimz::forge::PrTarget>,
    /// Launch without focusing the new team tab.
    #[arg(long)]
    bg: bool,
    /// Open the launch in a new tab/window.
    #[arg(long)]
    new_tab: bool,
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
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
}

pub fn run(args: TeamsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        None => list::run(args.json, globals),
        Some(TeamsSubcmd::Show { name, json }) => show::run(&name, json, globals),
        Some(TeamsSubcmd::Launch(args)) => {
            ensure_defined(&args.name, globals)?;
            agents_cmd::run(
                agents_cmd::AgentsArgs::team_launch(
                    args.name,
                    args.prompt,
                    agents_cmd::TeamLaunchOptions {
                        description: args.description,
                        worktree: args.worktree,
                        channel: args.channel,
                        from_pr: args.from_pr,
                        bg: args.bg,
                        new_tab: args.new_tab,
                    },
                ),
                globals,
            )
        }
        Some(TeamsSubcmd::Resume(args)) => {
            ensure_defined(&args.name, globals)?;
            agents_cmd::run(
                agents_cmd::AgentsArgs::team_resume(args.name, args.worktree),
                globals,
            )
        }
        Some(TeamsSubcmd::Install(args)) => install::run(args),
    }
}

fn ensure_defined(name: &str, globals: &GlobalFlags) -> Result<()> {
    let teams = list::effective_teams(globals)?;
    validate_team_name(name, &teams)
}

fn validate_team_name(name: &str, teams: &rimz::config::TeamsConfig) -> Result<()> {
    if teams.0.contains_key(name) {
        return Ok(());
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

    #[test]
    fn launch_guard_accepts_only_defined_teams_and_lists_choices() {
        let teams = rimz::config::TeamsConfig(BTreeMap::from([(
            "forge".to_owned(),
            rimz::config::Team::default(),
        )]));

        validate_team_name("forge", &teams).unwrap();
        let error = validate_team_name("missing", &teams).unwrap_err();
        assert!(error.to_string().contains("configured teams: forge"));
    }
}
