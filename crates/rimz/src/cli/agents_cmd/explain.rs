//! Read-only launch inspection through the real launch compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use rimz::agents::{PermissionMode, PresetArgMatcher, PresetField};
use rimz::config::{SkillName, effective::LaunchAgents};
use rimz::harness::budget::BudgetSpec;
use rimz::harness::launch::{self, AgentProcessStage, ExecAction, ExecIdentity, ExecRequest};
use rimz::harness::launch_plan::{self, LaunchPlan, LaunchPlanInputs};
use rimz::sandbox::{EnvPin, Mount, SkippedSkill};

use super::{LaunchOverrideArgs, launch_resolve, restart};
use crate::cli::{self, GlobalFlags, render};

#[derive(Debug, Args)]
pub(super) struct ExplainArgs {
    /// Profile, team role, or agent address.
    #[arg(value_name = "PROFILE|@HANDLE")]
    target: String,
    #[command(flatten)]
    overrides: LaunchOverrideArgs,
    /// Cap the member's spend for the session or local day.
    #[arg(long, value_name = "AMOUNT[/day]")]
    budget: Option<BudgetSpec>,
    /// Emit the launch plan as JSON.
    #[arg(long, conflicts_with = "prompt")]
    json: bool,
    /// Print only the composed system prompt and RimZ reminder.
    #[arg(long)]
    prompt: bool,
}

pub(super) fn run(args: ExplainArgs, globals: &GlobalFlags) -> Result<()> {
    validate_target_overrides(&args)?;
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = cli::open_existing_store(&workspace)?;
    let snapshot = store
        .as_ref()
        .map(rimz::Store::snapshot_cached)
        .transpose()?;
    let machine = cli::machine_config();
    cli::require_agents_fragments(&machine)?;
    cli::report_unknown_config_keys(&machine)?;
    let effective = rimz::config::effective::load(&machine, &workspace.project_root)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let state = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())?;
    let channel = cli::current_channel(&workspace);
    let mut warnings = Vec::new();
    let (request, cwd, action_note) = if args.target.starts_with('@') {
        let store = store.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no room state under {}; @handle needs a room that has run",
                workspace.project_root.display()
            )
        })?;
        let snapshot = snapshot
            .as_ref()
            .expect("an existing store supplied the snapshot");
        let agent =
            match cli::resolve_agent_one(store, snapshot, &args.target, None, channel.as_deref()) {
                Ok(agent) => agent.clone(),
                Err(error) => {
                    super::show::resolve_audit_agent(store, &workspace, &runtime, &args.target)?
                        .ok_or(error)?
                }
            };
        let posture = rimz::harness::resume::resolve_posture(
            rimz::harness::resume::PostureRequest {
                profile: agent.profile.as_deref(),
                kind: &agent.kind,
                stamped_mode: agent.mode,
            },
            &effective.profiles,
        );
        if let Some(reason) = &posture.degraded {
            warnings.push(format!("{reason}; showing bare {} posture", agent.kind));
        }
        let cwd = agent
            .worktree_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.worktree_root.clone());
        let (action, note) = restart::relaunch_action(&agent, &cwd)?;
        (
            restart::relaunch_request(&agent, &posture, action, None),
            cwd,
            note.map(str::to_owned),
        )
    } else {
        let finalized = launch_resolve::resolve_finalized_layout(
            snapshot.as_ref(),
            &machine,
            &effective,
            Some(&args.target),
            None,
            &args.overrides,
            args.budget,
            None,
            channel.as_deref(),
            false,
        )?;
        warnings.extend(finalized.warnings.iter().map(ToString::to_string));
        let resolved = finalized.resolved;
        let count = resolved.layout.agent_cells().count();
        let cell_count: usize = resolved
            .layout
            .columns
            .iter()
            .map(|column| column.rows.len())
            .sum();
        if count != 1 || cell_count != 1 {
            bail!(
                "explain describes one agent; `{}` is a {}-cell layout — name a profile or <team>.<role>",
                args.target,
                cell_count
            );
        }
        let cell = resolved
            .layout
            .agent_cells()
            .next()
            .expect("one agent cell was required");
        let ancestry = store
            .as_ref()
            .map(|store| {
                let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
                rimz::harness::ancestry::resolve_launch_ancestry_here(
                    &projection.agents,
                    false,
                    machine.agents.max_chain_length,
                )
                .map_err(anyhow::Error::from)
            })
            .transpose()?
            .flatten();
        let identities = rimz::harness::plan::launch_identity_requests(
            &resolved.layout,
            None,
            None,
            resolved.team_name.as_deref(),
            resolved
                .team_name
                .as_ref()
                .and_then(|name| resolved.teams.0.get(name))
                .map(|team| team.roles.as_slice()),
            channel.as_deref().or(finalized.inferred_lane.as_deref()),
            None,
            None,
            ancestry.as_ref(),
        )?;
        let params = identities
            .into_iter()
            .next()
            .expect("one cell yields one identity request")
            .launch;
        let request = ExecRequest::fresh(
            cell,
            ExecIdentity {
                name: None,
                name_explicit: false,
                launch_id: None,
                params,
            },
            None,
            false,
        );
        (request, workspace.worktree_root.clone(), None)
    };
    let adapter = rimz::agents::find_definition(request.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", request.kind))?;
    rimz::sandbox::preflight_skills(
        machine.agents.isolation,
        &request.kind,
        request.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let bwrap = rimz::sandbox::preflight(machine.agents.isolation)?;
    let ambient_env = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    let plan = launch_plan::compile(LaunchPlanInputs {
        request: &request,
        cwd: &cwd,
        project_root: &workspace.project_root,
        rtk: machine.harness.rtk,
        rimz_bin: &rimz::proc::rimz_exe(),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        bwrap: bwrap.as_deref(),
        ambient_env: &ambient_env,
    })?;
    warnings.extend(plan.warnings.iter().map(ToString::to_string));
    let report = ExplainReport::new(
        &args,
        &plan,
        &effective,
        bwrap.as_deref(),
        warnings,
        action_note,
    )?;
    for warning in &report.warnings {
        writeln!(render::err(), "rimz: {warning}")?;
    }
    if args.json {
        return render::json_pretty(&report);
    }
    if args.prompt {
        write_prompt(&report.prompt, &mut render::out())?;
        if report.prompt.reminder.is_some() && !report.prompt.reminder_delivered {
            writeln!(
                render::err(),
                "rimz: {} has no append-system-text channel; the reminder is not delivered",
                report.kind
            )?;
        }
        return Ok(());
    }
    render_explain(&report)
}

fn validate_target_overrides(args: &ExplainArgs) -> Result<()> {
    if args.target.starts_with('@')
        && (args.overrides != LaunchOverrideArgs::default() || args.budget.is_some())
    {
        bail!(
            "overrides apply to profile plans; a seat replays its profile posture (edit the profile, or launch fresh with rimz agents <profile> …)"
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct ExplainReport<'a> {
    target: &'a str,
    kind: &'a rimz::ids::AgentKind,
    action: &'static str,
    action_note: Option<String>,
    name: Option<&'a str>,
    launch_id: Option<&'a str>,
    cwd: &'a Path,
    profile: Option<ProfileReport<'a>>,
    overrides: Vec<String>,
    mode: Option<PermissionMode>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    budget: Option<&'a str>,
    skills: SkillsReport<'a>,
    program: &'a str,
    provider_argv: Vec<String>,
    argv: Vec<String>,
    reentry: Option<Vec<String>>,
    env: BTreeMap<String, String>,
    unset: &'a BTreeSet<String>,
    redacted_keys: &'a BTreeSet<String>,
    prompt: PromptReport<'a>,
    sandbox: Option<SandboxReport<'a>>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct ProfileReport<'a> {
    name: &'a str,
    chain: Vec<String>,
    role: Option<&'a str>,
    team: Option<&'a str>,
}
#[derive(Serialize)]
struct SkillsReport<'a> {
    callable: Option<&'a [SkillName]>,
    applied: bool,
}
#[derive(Serialize)]
struct PromptSource<'a> {
    path: &'a Path,
    bytes: u64,
}
#[derive(Serialize)]
struct PromptReport<'a> {
    sources: Vec<PromptSource<'a>>,
    composed: Option<&'a str>,
    channel: Option<String>,
    artifact: Option<&'a Path>,
    reminder: Option<&'a str>,
    reminder_channel: Option<String>,
    reminder_delivered: bool,
}
#[derive(Serialize)]
struct CopyReport<'a> {
    source: &'a Path,
    target: &'a Path,
}
#[derive(Serialize)]
struct SandboxReport<'a> {
    bwrap: &'a Path,
    mounts: &'a [Mount],
    pins: BTreeMap<String, EnvPin>,
    skipped: &'a [SkippedSkill],
    copies: Vec<CopyReport<'a>>,
}

impl<'a> ExplainReport<'a> {
    fn new(
        args: &'a ExplainArgs,
        plan: &'a LaunchPlan,
        effective: &LaunchAgents,
        bwrap: Option<&'a Path>,
        mut warnings: Vec<String>,
        action_note: Option<String>,
    ) -> Result<Self> {
        let process = plan.process();
        let request = &plan.request;
        let params = &request.identity.params;
        let adapter = rimz::agents::find_definition(request.kind.as_str())
            .expect("compiled agent has an adapter");
        let profile = params.profile.as_deref().map(|name| ProfileReport {
            name,
            chain: rimz::harness::spec::resolve_profile(name, &effective.profiles)
                .map(|profile| profile.chain)
                .unwrap_or_default(),
            role: params.role.as_deref(),
            team: params.team.as_deref(),
        });
        let mut env = process.env.clone();
        for (key, value) in &mut env {
            if process.secret_keys.contains(key) {
                *value = launch::REDACTED_ENV_VALUE.to_owned();
            }
        }
        let sources = plan
            .prompt
            .sources
            .system_prompt_file
            .iter()
            .chain(&plan.prompt.sources.append_system_prompt_files)
            .map(|path| {
                Ok(PromptSource {
                    path,
                    bytes: std::fs::metadata(path)
                        .with_context(|| format!("reading prompt source {}", path.display()))?
                        .len(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let sandbox = plan.sandbox.as_ref().zip(bwrap).map(|(sandbox, bwrap)| {
            warnings.extend(sandbox.skipped.iter().map(ToString::to_string));
            let mut pins = sandbox.pins.clone();
            for (key, pin) in &mut pins {
                if process.secret_keys.contains(key) && matches!(pin, EnvPin::Set(_)) {
                    *pin = EnvPin::Set(launch::REDACTED_ENV_VALUE.to_owned());
                }
            }
            SandboxReport {
                bwrap,
                mounts: &sandbox.plan.mounts,
                pins,
                skipped: &sandbox.skipped,
                copies: sandbox
                    .copies
                    .iter()
                    .map(|copy| CopyReport {
                        source: &copy.source,
                        target: &copy.target,
                    })
                    .collect(),
            }
        });
        Ok(Self {
            target: &args.target,
            kind: &request.kind,
            action: match request.action {
                ExecAction::Launch { .. } => "launch",
                ExecAction::Resume { .. } => "resume",
                ExecAction::Fork { .. } => "fork",
            },
            action_note: action_note.or_else(|| match &request.action {
                ExecAction::Resume { session_id, .. } => {
                    Some(format!("resumes session {session_id}"))
                }
                _ => None,
            }),
            name: request.identity.name.as_deref(),
            launch_id: request.identity.launch_id.as_deref(),
            cwd: &plan.cwd,
            profile,
            overrides: applied_overrides(args),
            mode: params.mode,
            model: params.model.as_deref(),
            effort: params.effort.as_deref(),
            budget: params.budget.as_deref(),
            skills: SkillsReport {
                callable: request.skills.as_deref(),
                applied: bwrap.is_some(),
            },
            program: &process.provider_program,
            provider_argv: launch::redact_env_tokens(&process.provider_argv, |key| {
                process.secret_keys.contains(key)
            }),
            argv: launch::redact_env_tokens(&process.argv, |key| process.secret_keys.contains(key)),
            reentry: match &plan.stage {
                AgentProcessStage::LoginShellReentry { argv, .. } => {
                    Some(launch::redact_env_tokens(argv, |key| {
                        process.secret_keys.contains(key)
                    }))
                }
                _ => None,
            },
            env,
            unset: &process.unset,
            redacted_keys: &process.secret_keys,
            prompt: PromptReport {
                sources,
                composed: plan.prompt.composed.as_deref(),
                channel: adapter
                    .spec()
                    .launch
                    .preset_arg_matcher(PresetField::SystemPromptFile)
                    .map(channel_label),
                artifact: plan.prompt.artifact.as_deref(),
                reminder: process.reminder.as_deref(),
                reminder_channel: plan
                    .reminder_channel
                    .as_ref()
                    .map(|channel| channel_label(PresetArgMatcher::from(channel))),
                reminder_delivered: plan.reminder_channel.is_some() && process.reminder.is_some(),
            },
            sandbox,
            warnings,
        })
    }
}

fn channel_label(matcher: PresetArgMatcher) -> String {
    match matcher {
        PresetArgMatcher::Flag(flags) | PresetArgMatcher::TextFlag(flags) => flags.join(" "),
        PresetArgMatcher::ConfigKey { flags, key } => format!("{} {key}", flags.join(" ")),
        PresetArgMatcher::EnvPathVar(key) => key,
    }
}

fn applied_overrides(args: &ExplainArgs) -> Vec<String> {
    let mut flags = Vec::new();
    let overrides = &args.overrides;
    for (flag, value) in [
        ("--model", &overrides.model),
        ("--agent", &overrides.agent),
        ("--effort", &overrides.effort),
    ] {
        if let Some(value) = value {
            flags.push(format!("{flag} {value}"));
        }
    }
    if overrides.ask {
        flags.push("--ask".to_owned());
    }
    if overrides.yolo {
        flags.push("--yolo".to_owned());
    }
    if let Some(path) = &overrides.system_prompt_file {
        flags.push(format!("--system-prompt-file {}", path.display()));
    }
    for path in &overrides.append_system_prompt_files {
        flags.push(format!("--append-system-prompt-file {}", path.display()));
    }
    if let Some(budget) = args.budget {
        flags.push(format!("--budget {budget}"));
    }
    if !overrides.passthrough.is_empty() {
        flags.push(format!("-- {}", overrides.passthrough.join(" ")));
    }
    flags
}

fn write_prompt(prompt: &PromptReport<'_>, output: &mut impl Write) -> std::io::Result<()> {
    if let Some(composed) = prompt.composed {
        write!(output, "{composed}")?;
    }
    if let Some(reminder) = prompt.reminder {
        if prompt.composed.is_some() {
            write!(output, "\n\n")?;
        }
        write!(output, "{reminder}")?;
    }
    Ok(())
}

fn render_explain(report: &ExplainReport<'_>) -> Result<()> {
    let mut output = render::out();
    writeln!(output, "Plan")?;
    let mut plan = render::KeyVals::new().indent(2);
    plan.push(
        "target",
        render::cell(report.target).fg(render::palette::identity(report.kind.as_str())),
    );
    plan.push(
        "provider",
        render::cell(report.kind.to_string()).fg(render::palette::identity(report.kind.as_str())),
    );
    plan.push(
        "action",
        render::cell(report.action_note.as_deref().unwrap_or(report.action)),
    );
    plan.push(
        "name",
        render::cell(report.name.unwrap_or("(minted at launch)")),
    );
    plan.push(
        "launch id",
        render::cell(report.launch_id.unwrap_or("(minted at launch)")),
    );
    plan.push("cwd", render::cell(report.cwd.display().to_string()));
    if let Some(profile) = &report.profile {
        let mut chain = profile.chain.clone();
        if let (Some(team), Some(role)) = (profile.team, profile.role) {
            chain.insert(0, format!("{team}.{role}"));
        }
        plan.push("profile", render::cell(chain.join(" ← ")));
    }
    plan.push(
        "mode",
        render::cell(
            report
                .mode
                .map_or_else(|| "provider default".to_owned(), |mode| mode.to_string()),
        ),
    );
    for (label, value) in [("model", report.model), ("effort", report.effort)] {
        plan.push(label, render::cell(value.unwrap_or("provider default")));
    }
    plan.push("budget", render::cell(report.budget.unwrap_or("no cap")));
    plan.push(
        "overrides",
        render::cell(if report.overrides.is_empty() {
            "none".to_owned()
        } else {
            report.overrides.join(", ")
        }),
    );
    plan.render(&mut output)?;
    writeln!(output, "\nCommand")?;
    let mut command = render::KeyVals::new().indent(2);
    command.push("program", render::cell(report.program));
    command.push_lines(
        "provider argv",
        report
            .provider_argv
            .iter()
            .map(|arg| vec![render::cell(arg)]),
    );
    command.push_lines(
        "via login shell",
        report.argv.iter().map(|arg| vec![render::cell(arg)]),
    );
    if let Some(reentry) = &report.reentry {
        command.push_lines("reentry", reentry.iter().map(|arg| vec![render::cell(arg)]));
    }
    command.render(&mut output)?;
    writeln!(output, "\nEnvironment")?;
    for (key, value) in &report.env {
        writeln!(output, "  {key}={value}")?;
    }
    if !report.unset.is_empty() {
        writeln!(
            output,
            "  unset: {}",
            report.unset.iter().cloned().collect::<Vec<_>>().join(", ")
        )?;
    }
    writeln!(output, "\nSkills")?;
    writeln!(
        output,
        "  callable: {}",
        report.skills.callable.map_or_else(
            || "native discovery".to_owned(),
            |skills| if skills.is_empty() {
                "none (all manual)".to_owned()
            } else {
                skills
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )
    )?;
    writeln!(
        output,
        "  profile view: {}",
        if report.skills.applied {
            "applied"
        } else {
            "not applied (host isolation)"
        }
    )?;
    writeln!(output, "\nSandbox")?;
    if let Some(sandbox) = &report.sandbox {
        writeln!(output, "  bwrap: {}", sandbox.bwrap.display())?;
        let mut mounts = render::Table::new(["type", "source", "target"]).indent(2);
        for mount in sandbox.mounts {
            let (kind, source, target) = match mount {
                Mount::Bind { source, target } => ("bind", Some(source), target),
                Mount::RoBind { source, target } => ("ro-bind", Some(source), target),
                Mount::Tmpfs { target } => ("tmpfs", None, target),
                Mount::Symlink { target, path } => ("symlink", Some(target), path),
            };
            mounts.row([
                render::cell(kind),
                render::cell(
                    source.map_or_else(|| "-".to_owned(), |path| path.display().to_string()),
                ),
                render::cell(target.display().to_string()),
            ]);
        }
        mounts.render(&mut output)?;
        for copy in &sandbox.copies {
            writeln!(
                output,
                "  copy: {} → {}",
                copy.source.display(),
                copy.target.display()
            )?;
        }
    } else {
        writeln!(output, "  host isolation")?;
    }
    writeln!(output, "\nSystem prompt")?;
    for source in &report.prompt.sources {
        writeln!(
            output,
            "  source: {} ({} bytes)",
            source.path.display(),
            source.bytes
        )?;
    }
    if let Some(composed) = report.prompt.composed {
        writeln!(
            output,
            "── composed via {} ──\n{composed}",
            report.prompt.channel.as_deref().unwrap_or("provider")
        )?;
    }
    if let Some(reminder) = report.prompt.reminder {
        writeln!(
            output,
            "── <system_reminder> via {} ──\n{reminder}",
            report
                .prompt
                .reminder_channel
                .as_deref()
                .unwrap_or("no append-system-text channel (not delivered)")
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct ParserArgs {
        #[command(flatten)]
        explain: ExplainArgs,
    }

    #[test]
    fn explain_accepts_shared_overrides_without_resume_argument() {
        for target in ["coder", "forge.coder", "@coder"] {
            let args = ParserArgs::try_parse_from(["explain", target])
                .unwrap()
                .explain;
            assert_eq!(args.target, target);
            validate_target_overrides(&args).unwrap();
        }
        let args = ParserArgs::try_parse_from([
            "explain",
            "forge.coder",
            "--model",
            "opus",
            "--yolo",
            "--json",
            "--",
            "--foo",
        ])
        .unwrap()
        .explain;
        assert!(args.json);
        assert_eq!(
            applied_overrides(&args),
            ["--model opus", "--yolo", "-- --foo"]
        );
        let error = ParserArgs::try_parse_from(["explain", "coder", "--json", "--prompt"])
            .err()
            .unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn seat_overrides_are_rejected_before_opening_the_room() {
        let args = ParserArgs::try_parse_from(["explain", "@coder", "--model", "opus"])
            .unwrap()
            .explain;
        insta::assert_snapshot!(validate_target_overrides(&args).unwrap_err().to_string(), @"overrides apply to profile plans; a seat replays its profile posture (edit the profile, or launch fresh with rimz agents <profile> …)");
    }

    #[test]
    fn prompt_output_preserves_composed_bytes_and_reminder() {
        let prompt = PromptReport {
            sources: Vec::new(),
            composed: Some("base\n\nfragment\n"),
            channel: None,
            artifact: None,
            reminder: Some("<system_reminder>\nmodel\n</system_reminder>"),
            reminder_channel: None,
            reminder_delivered: false,
        };
        let mut bytes = Vec::new();
        write_prompt(&prompt, &mut bytes).unwrap();
        assert_eq!(
            bytes,
            b"base\n\nfragment\n\n\n<system_reminder>\nmodel\n</system_reminder>"
        );
    }
}
