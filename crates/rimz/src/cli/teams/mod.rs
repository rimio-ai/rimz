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
    /// Run the cohort under this isolation instead of machine `agents.isolation`.
    #[arg(long, value_name = "host|sandbox", requires = "name")]
    isolation: Option<rimz::config::Isolation>,
    /// Emit the team catalogue as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TeamsSubcmd {
    /// Show team definitions and live instances.
    #[command(alias = "inspect")]
    Show {
        /// Team, team#lane, or #lane for every team live in a lane.
        #[arg(
            value_name = "NAME",
            add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
        )]
        name: Option<String>,
        /// Scope live instances to one worktree or lane.
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
        value_name = "NAME",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    name: String,
    /// Prompt delivered to the team's configured leader.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    #[command(flatten)]
    launch: agents_cmd::CohortLaunchArgs,
    /// Run the cohort under this isolation instead of machine `agents.isolation`.
    #[arg(long, value_name = "host|sandbox")]
    isolation: Option<rimz::config::Isolation>,
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
    /// Replace the resumed members' recorded isolation.
    #[arg(long, value_name = "host|sandbox")]
    isolation: Option<rimz::config::Isolation>,
}

impl ResumeArgs {
    fn into_agents_args(self) -> agents_cmd::AgentsArgs {
        agents_cmd::AgentsArgs::from_launch(agents_cmd::AgentLaunchArgs {
            spec: Some(self.name),
            cohort: agents_cmd::CohortLaunchArgs {
                worktree: self.worktree,
                resume: true,
                bg: self.bg,
                ..Default::default()
            },
            overrides: agents_cmd::LaunchOverrideArgs {
                isolation: self.isolation,
                ..Default::default()
            },
            ..Default::default()
        })
    }
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

#[derive(Debug, Args)]
struct FlipArgs {
    /// Destination stage owned by the team, or Done to finish the work.
    #[arg(value_name = "STAGE")]
    stage: String,
    /// Nonblank progress note recorded on the board; what is done or where the work stands.
    #[arg(value_name = "NOTE", value_parser = parse_progress_note)]
    note: String,
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
    group(clap::ArgGroup::new("input").required(true).args(["text", "file", "stdin"])),
    override_usage = "rimz teams record <SECTION> <TEXT|--file <PATH>|--stdin>"
)]
struct RecordArgs {
    /// Board section: Goal, Decisions, or Result (case-insensitive).
    #[arg(value_name = "SECTION")]
    section: String,
    /// Text to append as one stamped entry.
    #[arg(value_name = "TEXT")]
    text: Option<String>,
    /// Read the entry from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    file: Option<std::path::PathBuf>,
    /// Read the entry from stdin to EOF.
    #[arg(long)]
    stdin: bool,
}

#[derive(Debug, Args)]
struct WaitArgs {
    /// Team or team#lane whose live cohort to wait on; several wait on all.
    #[arg(
        value_name = "NAME",
        required = true,
        num_args = 1..,
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::team_names)
    )]
    references: Vec<String>,
    /// Select the cohort by worktree name or lane (one NAME only).
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
        None => match args.name {
            Some(name) => {
                if args.json {
                    bail!(
                        "--json is only supported with `rimz teams` and `rimz teams list`; use `rimz teams show {name} --json` for one team"
                    );
                }
                launch_team(name, args.prompt, args.launch, args.isolation, globals)
            }
            None => {
                reject_launch_flags_without_name(&args.prompt, &args.launch)?;
                list::run(args.json, globals)
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
        agents_cmd::AgentsArgs::from_launch(agents_cmd::AgentLaunchArgs {
            spec: Some(name),
            prompt,
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
        || launch.fresh
        || launch.budget.is_some()
        || launch.bg
        || launch.new_tab
    {
        bail!("team launch options require a team name");
    }
    Ok(())
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
    let teams = list::effective_teams(globals)?;
    if !teams.0.contains_key(name)
        && let Some(detail) = crate::cli::machine_config().definition_failure_for(name)
    {
        bail!("{detail}");
    }
    validate_team_name(name, &teams)?;
    Ok(teams)
}

fn validate_team_name(name: &str, teams: &rimz::config::TeamsConfig) -> Result<()> {
    if teams.0.contains_key(name) {
        return Ok(());
    }
    if teams.role_spec(name).is_some() {
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
    fn record_accepts_exactly_one_text_source_and_lists_sections() {
        for args in [
            vec!["rimz", "record", "Decisions", "text"],
            vec!["rimz", "record", "Goal", "--stdin"],
            vec!["rimz", "record", "Result", "--file", "entry.md"],
        ] {
            assert!(TeamsHarness::try_parse_from(args).is_ok());
        }
        for args in [
            vec!["rimz", "record"],
            vec!["rimz", "record", "Decisions"],
            vec!["rimz", "record", "Goal", "text", "--file", "entry.md"],
            vec!["rimz", "record", "Goal", "text", "--stdin"],
            vec!["rimz", "record", "Goal", "--stdin", "--file", "entry.md"],
            vec!["rimz", "record", "Goal", "text", "--team", "forge"],
        ] {
            assert!(TeamsHarness::try_parse_from(args).is_err());
        }
        let help = TeamsHarness::try_parse_from(["rimz", "record", "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("rimz teams record <SECTION> <TEXT|--file <PATH>|--stdin>"));
        for section in ["Goal", "Decisions", "Result"] {
            assert!(help.contains(section), "{help}");
        }
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
    fn profiles_parse_as_team_profile_listing() {
        let args = parse_teams(&["rimz", "profiles", "--json", "--path"]);
        assert!(matches!(
            args.command,
            Some(TeamsSubcmd::Profiles {
                json: true,
                path: true
            })
        ));
    }

    #[test]
    fn flip_requires_progress_note_and_accepts_only_team_selection() {
        let args = parse_teams(&["rimz", "flip", "Implement", "note", "--team", "forge"]);
        let Some(TeamsSubcmd::Flip(args)) = args.command else {
            panic!("flip verb");
        };
        assert_eq!(args.stage, "Implement");
        assert_eq!(args.note, "note");
        assert_eq!(args.team.as_deref(), Some("forge"));
        assert!(TeamsHarness::try_parse_from(["rimz", "flip"]).is_err());
        for stage in ["Implement", "Done"] {
            assert!(TeamsHarness::try_parse_from(["rimz", "flip", stage]).is_err());
            assert!(TeamsHarness::try_parse_from(["rimz", "flip", stage, "note"]).is_ok());
        }
        for note in ["", " ", "\t\r\n", "\u{2003}"] {
            let error = TeamsHarness::try_parse_from(["rimz", "flip", "Plan", note]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
            assert!(error.to_string().contains("nonblank progress note"));
        }
        let note = "  Ready.\r\nKeep the evidence.  ";
        let args = parse_teams(&["rimz", "flip", "Plan", note]);
        let Some(TeamsSubcmd::Flip(args)) = args.command else {
            panic!("flip verb");
        };
        assert_eq!(args.note, note);
        let help = TeamsHarness::try_parse_from(["rimz", "flip", "--help"]).unwrap_err();
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(
            help.to_string()
                .contains("Destination stage owned by the team, or Done")
        );
        assert!(TeamsHarness::try_parse_from(["rimz", "flip", "Plan", "note", "--steer"]).is_err());
        for flag in ["-m", "--note", "-w", "--worktree"] {
            assert!(
                TeamsHarness::try_parse_from(["rimz", "flip", "Plan", "note", flag, "value"])
                    .is_err()
            );
        }
    }

    #[test]
    fn wait_parses_references_and_join_flags() {
        let args = parse_teams(&[
            "rimz",
            "wait",
            "forge#a",
            "forge#b",
            "--any",
            "--timeout",
            "5m",
            "--json",
        ]);
        let Some(TeamsSubcmd::Wait(args)) = args.command else {
            panic!("wait verb");
        };
        assert_eq!(args.references, ["forge#a", "forge#b"]);
        assert!(args.any && args.json);
        assert_eq!(args.timeout, Some(std::time::Duration::from_secs(300)));
        let args = parse_teams(&["rimz", "wait", "forge", "-w", "feat-x"]);
        let Some(TeamsSubcmd::Wait(args)) = args.command else {
            panic!("wait verb");
        };
        assert_eq!(args.worktree.as_deref(), Some("feat-x"));
        assert!(TeamsHarness::try_parse_from(["rimz", "wait"]).is_err());
    }

    #[test]
    fn all_team_launch_doorways_parse_the_same_cohort_payload() {
        let agents = AgentsHarness::try_parse_from([
            "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
        ])
        .expect("parse agents launch")
        .args;
        let bare = parse_teams(&[
            "rimz", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
        ]);
        let verb = parse_teams(&[
            "rimz", "launch", "forge", "ship", "-w", "feat-x", "--budget", "20", "--bg", "--fresh",
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
        for argv in [
            &["rimz", "forge", "--resume", "--isolation", "host"][..],
            &["rimz", "launch", "forge", "--resume", "--isolation", "host"],
        ] {
            let args = parse_teams(argv);
            let (launch, isolation) = match args.command {
                Some(TeamsSubcmd::Launch(args)) => (args.launch, args.isolation),
                _ => (args.launch, args.isolation),
            };
            assert!(launch.resume);
            assert_eq!(isolation, Some(rimz::config::Isolation::Host));
        }
        let resume = parse_teams(&["rimz", "resume", "forge", "--isolation", "host"]);
        let Some(TeamsSubcmd::Resume(args)) = resume.command else {
            panic!("resume verb");
        };
        let args = args.into_agents_args();
        assert_eq!(args.launch.spec.as_deref(), Some("forge"));
        assert!(args.launch.cohort.resume);
        assert_eq!(
            args.launch.overrides.isolation,
            Some(rimz::config::Isolation::Host)
        );
    }

    #[test]
    fn launch_flags_without_a_team_name_are_rejected() {
        for argv in [vec!["rimz", "-w", "feat-x"], vec!["rimz", "--fresh"]] {
            let args = parse_teams(&argv);
            let error = reject_launch_flags_without_name(&args.prompt, &args.launch)
                .expect_err("missing team");
            assert!(error.to_string().contains("require a team name"));
        }
    }

    #[test]
    fn fused_team_lane_feeds_the_canonical_worktree_argument() {
        let show = parse_teams(&["rimz", "show", "forge#feat-x"]);
        let stop = parse_teams(&["rimz", "stop", "forge#feat-x"]);
        let resume = parse_teams(&["rimz", "resume", "forge#feat-x"]);
        let bare = parse_teams(&["rimz", "forge#feat-x", "ship", "--fresh"]);
        assert!(bare.launch.fresh);

        let Some(TeamsSubcmd::Show { name, worktree, .. }) = show.command else {
            panic!("show verb");
        };
        assert_eq!(
            show_target(name, worktree).unwrap(),
            (Some("forge".to_owned()), Some("feat-x".to_owned()))
        );
        let Some(TeamsSubcmd::Stop(args)) = stop.command else {
            panic!("stop verb");
        };
        assert_eq!(
            team_lane(args.name, args.worktree).unwrap().1.as_deref(),
            Some("feat-x")
        );
        let Some(TeamsSubcmd::Resume(args)) = resume.command else {
            panic!("resume verb");
        };
        assert_eq!(
            team_lane(args.name, args.worktree).unwrap().1.as_deref(),
            Some("feat-x")
        );
        assert_eq!(
            team_lane(bare.name.unwrap(), bare.launch.worktree)
                .unwrap()
                .1
                .as_deref(),
            Some("feat-x")
        );
    }

    #[test]
    fn fused_team_lane_rejects_missing_and_duplicate_lanes() {
        let missing = team_lane("forge#".to_owned(), None).unwrap_err();
        assert!(missing.to_string().contains("after `#`"));

        let duplicate = team_lane("forge#feat-x".to_owned(), Some("other".to_owned())).unwrap_err();
        assert!(duplicate.to_string().contains("given twice"));
    }

    #[test]
    fn show_accepts_lane_only_targets() {
        for argv in [
            vec!["rimz", "show", "#feat-x"],
            vec!["rimz", "show", "-w", "feat-x"],
        ] {
            let Some(TeamsSubcmd::Show { name, worktree, .. }) = parse_teams(&argv).command else {
                panic!("show verb");
            };
            assert_eq!(
                show_target(name, worktree).unwrap(),
                (None, Some("feat-x".to_owned()))
            );
        }
        let missing = show_target(None, None).unwrap_err();
        assert!(missing.to_string().contains("'#lane'"));
        assert!(show_target(Some("#".into()), None).is_err());
        assert!(show_target(Some("#feat-x".into()), Some("other".into())).is_err());
    }
}
