//! `rimz teams` — discover, inspect, launch, and drive named teams.

mod board_context;
mod cohort;
mod flip;
mod install;
mod list;
mod record;
mod show;
mod wait;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::usage::UsageError;
use super::{GlobalFlags, agents_cmd};

const TEAM_ADDRESS_HELP: &str = "Team, or team#lane for one cohort. Without a lane: the cohort in your current lane, else the team's only live cohort";
const TEAM_RESUME_HELP: &str = "Team, or team#lane for one cohort. Without a lane: the team's newest closed cohort, in your linked worktree when you stand in one, else anywhere in the room";

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct TeamsArgs {
    #[command(subcommand)]
    command: Option<TeamsSubcmd>,
    /// Configured team to launch.
    #[arg(
        value_name = "TEAM[#LANE]",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: Option<String>,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT", allow_hyphen_values = true, value_parser = agents_cmd::parse_cohort_prompt)]
    prompt: Option<String>,
    #[command(flatten)]
    launch: agents_cmd::CohortLaunchArgs,
    /// Run the cohort under this isolation instead of machine `agents.isolation`.
    #[arg(long, value_name = "host|scoped|sandbox", requires = "name")]
    isolation: Option<rimz::config::Isolation>,
    /// Retired listing flag, kept so the no-verb refusal can echo it into `list`.
    #[arg(long, hide = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TeamsSubcmd {
    /// Show team definitions and live cohorts.
    #[command(alias = "inspect")]
    Show {
        /// Team for its definition and cohorts, team#lane for one cohort, or #lane for every team live there.
        #[arg(
            value_name = "TEAM[#LANE]",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
        )]
        name: Option<String>,
        /// Same as #LANE.
        #[arg(
            long,
            short = 'w',
            value_name = "NAME",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
        )]
        worktree: Option<String>,
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
    /// List the team role profiles (`<team>.<role>`).
    Profiles {
        /// Emit JSON.
        #[arg(long)]
        json: bool,
        /// Include each profile's defining file path.
        #[arg(long)]
        path: bool,
    },
    /// Resume a configured team's prior cohort.
    Resume(ResumeArgs),
    /// Stop every live member of a team cohort.
    Stop(CohortArgs),
    /// Focus the member of a team cohort that needs attention.
    Focus(CohortArgs),
    /// Restart every live member of a team cohort.
    Restart(CohortArgs),
    /// Hand the board to the next stage's owner.
    Flip(FlipArgs),
    /// Append a stamped entry to the board's Goal, Decisions, or Result.
    Record(RecordArgs),
    /// Block until team cohorts' boards reach Done, then print their Result.
    Wait(WaitArgs),
    /// List or install team bundles from the matching RimZ release.
    Install(install::InstallArgs),
}

#[derive(Debug, Args)]
#[command(
    after_help = "`rimz teams` sets where a cohort runs, whether it resumes, and what each member may spend. `rimz agents` sets what an agent is — model, effort, prompts, permission posture, name, pane placement, supervised runs."
)]
struct TeamLaunchArgs {
    #[arg(
        help = TEAM_ADDRESS_HELP,
        value_name = "TEAM[#LANE]",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT", allow_hyphen_values = true, value_parser = agents_cmd::parse_cohort_prompt)]
    prompt: Option<String>,
    #[command(flatten)]
    launch: agents_cmd::CohortLaunchArgs,
    /// Run the cohort under this isolation instead of machine `agents.isolation`.
    #[arg(long, value_name = "host|scoped|sandbox")]
    isolation: Option<rimz::config::Isolation>,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    #[arg(
        help = TEAM_RESUME_HELP,
        value_name = "TEAM[#LANE]",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Queue a user message to the team's leader after reopening.
    #[arg(value_name = "PROMPT", allow_hyphen_values = true, value_parser = agents_cmd::parse_cohort_prompt)]
    prompt: Option<String>,
    /// Save this model for subsequent resumes.
    #[arg(long, value_name = "MODEL", conflicts_with = "tier")]
    model: Option<String>,
    /// Save this reasoning effort for subsequent resumes.
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Save fast mode on for subsequent resumes (Claude, Codex, Amp, Pi on OpenAI).
    #[arg(long, conflicts_with = "no_fast")]
    fast: bool,
    /// Save fast mode off for subsequent resumes.
    #[arg(long)]
    no_fast: bool,
    /// Replace the saved launch record with a same-provider profile or kind base.
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
    /// Pick this tier's same-provider entry and save the launch record.
    #[arg(long, value_name = "TIER")]
    tier: Option<rimz::config::tiers::ModelTier>,
    /// Save permission mode that asks before tool use where supported.
    #[arg(long, conflicts_with = "yolo")]
    ask: bool,
    /// Save permission mode that skips provider permission prompts where supported.
    #[arg(long)]
    yolo: bool,
    /// Same as #LANE.
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
    /// Replace the resumed members' recorded isolation.
    #[arg(long, value_name = "host|scoped|sandbox")]
    isolation: Option<rimz::config::Isolation>,
}

impl ResumeArgs {
    fn into_agents_args(self) -> agents_cmd::AgentsArgs {
        agents_cmd::AgentsArgs::from_launch(agents_cmd::AgentLaunchArgs {
            spec: Some(self.name),
            prompt: self.prompt,
            teams_scene: true,
            cohort: agents_cmd::CohortLaunchArgs {
                worktree: self.worktree,
                continue_cohort: true,
                bg: self.bg,
                ..Default::default()
            },
            overrides: agents_cmd::LaunchOverrideArgs {
                isolation: self.isolation,
                model: self.model,
                effort: self.effort,
                fast: self.fast,
                no_fast: self.no_fast,
                agent: self.agent,
                tier: self.tier,
                ask: self.ask,
                yolo: self.yolo,
                ..Default::default()
            },
            ..Default::default()
        })
    }
}

#[derive(Debug, Args)]
struct CohortArgs {
    #[arg(
        help = TEAM_ADDRESS_HELP,
        value_name = "TEAM[#LANE]",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Same as #LANE.
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "rimz teams flip [TEAM#LANE] <STAGE> <NOTE> [--team NAME]",
    after_help = "[TEAM#LANE] selects a live or held cohort. Without it, use the current worktree."
)]
struct FlipArgs {
    /// Destination stage owned by the team, or Done to finish the work.
    #[arg(value_name = "STAGE")]
    stage: String,
    /// Nonblank progress note recorded on the board; what is done or where the work stands.
    #[arg(
        value_name = "NOTE",
        value_parser = parse_progress_note,
        allow_hyphen_values = true
    )]
    note: String,
    /// Progress note when a TEAM#LANE precedes the stage.
    #[arg(hide = true, value_name = "NOTE", value_parser = parse_progress_note, allow_hyphen_values = true)]
    addressed_note: Option<String>,
    /// Select a team when several teams share the worktree.
    #[arg(
        long,
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    team: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "rimz teams record [TEAM#LANE] <SECTION> <TEXT|--file <PATH>|--stdin>",
    after_help = "[TEAM#LANE] selects a live or held cohort. Without it, use the current worktree."
)]
struct RecordArgs {
    /// Board section: Goal, Decisions, or Result (case-insensitive).
    #[arg(value_name = "SECTION")]
    section: String,
    /// Text to append as one stamped entry.
    #[arg(value_name = "TEXT", allow_hyphen_values = true)]
    text: Option<String>,
    /// Entry text when a TEAM#LANE precedes the section.
    #[arg(hide = true, value_name = "TEXT", allow_hyphen_values = true)]
    addressed_text: Option<String>,
    /// Read the entry from a UTF-8 file.
    #[arg(long, value_name = "PATH", conflicts_with = "stdin")]
    file: Option<std::path::PathBuf>,
    /// Read the entry from stdin to EOF.
    #[arg(long)]
    stdin: bool,
}

impl FlipArgs {
    fn normalize(mut self) -> Result<(Option<String>, Self)> {
        let mut note = Some(std::mem::take(&mut self.note));
        let address = board_address(&mut self.stage, &mut note, self.addressed_note.take())?;
        self.note = note.ok_or_else(|| {
            UsageError::new("provide a stage and a nonblank progress note".into())
        })?;
        if address.is_some() && self.team.is_some() {
            return Err(UsageError::new(
                "use TEAM#LANE or --team in the current worktree, not both".into(),
            )
            .into());
        }
        Ok((address, self))
    }
}

impl RecordArgs {
    fn normalize(mut self) -> Result<(Option<String>, Self)> {
        let address = board_address(
            &mut self.section,
            &mut self.text,
            self.addressed_text.take(),
        )?;
        if usize::from(self.text.is_some())
            + usize::from(self.file.is_some())
            + usize::from(self.stdin)
            != 1
        {
            return Err(UsageError::new(
                "provide exactly one of TEXT, --file PATH, or --stdin".into(),
            )
            .into());
        }
        Ok((address, self))
    }
}

fn board_address(
    first: &mut String,
    second: &mut Option<String>,
    extra: Option<String>,
) -> Result<Option<String>> {
    let Some((team, lane)) = first.split_once('#') else {
        if let Some(extra) = extra {
            return Err(UsageError::new(format!(
                "unexpected argument '{extra}'; an optional board address must be TEAM#LANE"
            ))
            .into());
        }
        return Ok(None);
    };
    if team.is_empty() || lane.is_empty() {
        return Err(UsageError::new("expected TEAM#LANE before the board arguments".into()).into());
    }
    let value = second
        .take()
        .ok_or_else(|| UsageError::new("provide the board arguments after TEAM#LANE".into()))?;
    let address = std::mem::replace(first, value);
    *second = extra;
    Ok(Some(address))
}

#[derive(Debug, Args)]
struct WaitArgs {
    #[arg(
        help = TEAM_ADDRESS_HELP,
        value_name = "TEAM[#LANE]",
        required = true,
        num_args = 1..,
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    references: Vec<String>,
    /// Same as #LANE (one team only).
    #[arg(
        long,
        short = 'w',
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::worktrees)
    )]
    worktree: Option<String>,
    /// Return when the first cohort settles instead of all of them.
    #[arg(long)]
    any: bool,
    /// Give up after this long, exiting 124 and leaving the cohorts running.
    #[arg(long, value_name = "DURATION", value_parser = crate::cli::supervised::parse_timeout)]
    timeout: Option<std::time::Duration>,
    /// Emit the outcome as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_progress_note(note: &str) -> Result<String, String> {
    if note.trim().is_empty() {
        return Err(
            "provide a nonblank progress note: what is done or where the work stands".to_owned(),
        );
    }
    Ok(note.to_owned())
}

pub fn run(args: TeamsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        None => match args.name.filter(|name| !name.trim().is_empty()) {
            Some(name) => {
                let (teams, profiles) = list::effective_catalog(globals)?;
                let team = name.split_once('#').map_or(name.as_str(), |(team, _)| team);
                if !teams.0.contains_key(team)
                    && let Some(detail) = crate::cli::machine_config().definition_failure_for(team)
                {
                    bail!("{detail}");
                }
                guard_launch(&name, args.prompt.as_deref(), &teams, &profiles)?;
                if args.json {
                    bail!(
                        "--json is only supported with `rimz teams list`; use `rimz teams show {name} --json` for one team"
                    );
                }
                launch_team(name, args.prompt, args.launch, args.isolation, globals)
            }
            None => {
                let flags: &[&str] = if args.json { &["--json"] } else { &[] };
                Err(UsageError::missing_scene_verb(
                    crate::cli::help::Scene::Teams,
                    "a team name",
                    flags,
                )
                .into())
            }
        },
        Some(TeamsSubcmd::Show {
            name,
            worktree,
            json,
        }) => {
            let (name, worktree) = show_target(name, worktree)?;
            show::run(name.as_deref(), worktree.as_deref(), json, globals)
        }
        Some(TeamsSubcmd::List { json }) => list::run(json, globals),
        Some(TeamsSubcmd::Profiles { json, path }) => list_profiles(json, path),
        Some(TeamsSubcmd::Launch(args)) => {
            launch_team(args.name, args.prompt, args.launch, args.isolation, globals)
        }
        Some(TeamsSubcmd::Resume(mut args)) => {
            (args.name, args.worktree) = team_lane(args.name, args.worktree)?;
            ensure_defined(&args.name, globals)?;
            agents_cmd::run(args.into_agents_args(), globals)
        }
        Some(TeamsSubcmd::Stop(args)) => {
            let (name, worktree) = team_lane(args.name, args.worktree)?;
            ensure_defined(&name, globals)?;
            cohort::stop(&name, worktree.as_deref(), globals)
        }
        Some(TeamsSubcmd::Focus(args)) => {
            let (name, worktree) = team_lane(args.name, args.worktree)?;
            let teams = ensure_defined(&name, globals)?;
            let leader = teams.0.get(&name).and_then(|team| team.leader.as_deref());
            cohort::focus(&name, worktree.as_deref(), leader, globals)
        }
        Some(TeamsSubcmd::Restart(args)) => {
            let (name, worktree) = team_lane(args.name, args.worktree)?;
            ensure_defined(&name, globals)?;
            cohort::restart(&name, worktree.as_deref(), globals)
        }
        Some(TeamsSubcmd::Install(args)) => install::run(args),
        Some(TeamsSubcmd::Flip(args)) => flip::run(args, globals),
        Some(TeamsSubcmd::Record(args)) => record::run(args, globals),
        Some(TeamsSubcmd::Wait(args)) => wait::run(args, globals),
    }
}

fn launch_team(
    name: String,
    prompt: Option<String>,
    mut launch: agents_cmd::CohortLaunchArgs,
    isolation: Option<rimz::config::Isolation>,
    globals: &GlobalFlags,
) -> Result<()> {
    let (name, worktree) = team_lane(name, launch.worktree)?;
    if worktree.is_some() && launch.channel.is_some() {
        bail!("--channel cannot be used with team#worktree addressing");
    }
    launch.worktree = worktree;
    ensure_defined(&name, globals)?;
    agents_cmd::run(
        agents_cmd::AgentsArgs::launch_verb(agents_cmd::AgentLaunchArgs {
            spec: Some(name),
            prompt,
            teams_scene: true,
            cohort: launch,
            overrides: agents_cmd::LaunchOverrideArgs {
                isolation,
                ..Default::default()
            },
            ..Default::default()
        }),
        globals,
    )
}

fn team_lane(name: String, worktree: Option<String>) -> Result<(String, Option<String>)> {
    let Some((team, lane)) = name.split_once('#') else {
        return Ok((name, worktree));
    };
    if lane.is_empty() {
        bail!("expected a worktree or lane after `#` in `{name}`");
    }
    if worktree.is_some() {
        bail!("worktree given twice; use either `team#worktree` or `-w/--worktree`");
    }
    Ok((team.to_owned(), Some(lane.to_owned())))
}

fn show_target(
    name: Option<String>,
    worktree: Option<String>,
) -> Result<(Option<String>, Option<String>)> {
    let Some(name) = name else {
        if worktree.is_none() {
            // An unquoted `#lane` never reaches us: shells with interactive
            // comments (zsh under oh-my-zsh, bash) drop it as a comment.
            bail!(
                "expected a team name or #lane (quote a bare lane as '#lane'; \
                 an unquoted # starts a shell comment)"
            );
        }
        return Ok((None, worktree));
    };
    let lane_only = name.starts_with('#');
    let (team, lane) = team_lane(name, worktree)?;
    Ok(((!lane_only).then_some(team), lane))
}

fn list_profiles(json: bool, path: bool) -> Result<()> {
    let (config, sources) = rimz::config::MachineConfig::load_with_agent_spec_sources()
        .context("loading machine config")?;
    crate::cli::report_definition_errors(&config)?;
    let reports = crate::cli::profile_report::available_profiles(
        &config.agents.profiles,
        &config.agents.commands,
        &sources,
        rimz::config::effective::ProfileScope::Agents,
    );
    crate::cli::profile_report::list_profiles(
        crate::cli::profile_report::partition_team_profiles(reports, &config.agents.teams).0,
        crate::cli::profile_report::ProfileListing::Teams,
        json,
        path,
    )
}

fn ensure_defined(name: &str, globals: &GlobalFlags) -> Result<rimz::config::TeamsConfig> {
    let (teams, profiles) = list::effective_catalog(globals)?;
    if !teams.0.contains_key(name)
        && let Some(detail) = crate::cli::machine_config().definition_failure_for(name)
    {
        bail!("{detail}");
    }
    validate_team_name(name, &teams, &profiles)?;
    Ok(teams)
}

fn validate_team_name(
    name: &str,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
) -> Result<()> {
    if teams.0.contains_key(name) {
        return Ok(());
    }
    if teams.role_spec(name).is_some() {
        bail!("`{name}` names one role; launch it with `rimz agents {name}`");
    }
    if let Some(suggestion) =
        super::render::launch_suggestions::teams_suggestions(name, teams, profiles, false)
    {
        bail!("unknown team `{name}`\n{suggestion}");
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

fn guard_launch(
    name: &str,
    prompt: Option<&str>,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
) -> Result<()> {
    let verbs = super::usage::scene_verbs("teams");
    let team = name.split_once('#').map_or(name, |(team, _)| team);
    if teams.role_spec(team).is_some() {
        return validate_team_name(team, teams, profiles);
    }
    if !teams.0.contains_key(team) {
        if let Some(suggestion) =
            super::render::launch_suggestions::teams_suggestions(team, teams, profiles, true)
        {
            return Err(UsageError::new(format!(
                "`{team}` is neither a teams command nor a configured team\n{suggestion}"
            ))
            .into());
        }
        let configured = teams.0.keys().cloned().collect::<Vec<_>>().join(", ");
        let message = format!(
            "`{team}` is neither a teams command nor a configured team\ncommands: {}\nconfigured teams: {configured}",
            verbs.join(", ")
        );
        return Err(UsageError::new(message).into());
    }
    if let Some(verb) = prompt.filter(|prompt| verbs.iter().any(|verb| verb == *prompt)) {
        let drive = super::usage::shell_command(["rimz", "teams", verb, name]);
        let launch = super::usage::shell_command(["rimz", "teams", "launch", name, verb]);
        let message = format!(
            "`{verb}` is a teams command; did you mean `{drive}`?\nTo send it as a prompt: `{launch}`"
        );
        return Err(UsageError::new(message).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
