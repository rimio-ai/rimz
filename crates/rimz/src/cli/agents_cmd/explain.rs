//! Read-only launch inspection through the real launch compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use rimz::agents::capabilities::SystemTextChannel;
use rimz::agents::{PermissionMode, PresetArgMatcher, PresetField};
use rimz::config::effective::ProfileScope;
use rimz::config::{Isolation, ProfilesConfig, SkillName};
use rimz::harness::budget::BudgetSpec;
use rimz::harness::launch::{self, ExecAction, ExecIdentity, ExecRequest};
use rimz::harness::launch_plan::{self, LaunchPlan, LaunchPlanInputs};
use rimz::harness::team_prompt::{self, BUILT_IN_CONSENSUS, Consensus};
use rimz::mcp::catalog::{Capability, LoadMode, SERVER_NAME, render_listing, tools_for};
use rimz::mcp::launch_env;
use rimz::mcp::wording::Wording;
use rimz::sandbox::{EnvPin, Mount, SkippedSkill};

use super::{LaunchOverrideArgs, launch_resolve};
use crate::cli::render::Table;
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
    /// Print only the MCP servers, tools, and load modes the launch gets, then the full text of
    /// RimZ's tools.
    #[arg(long, conflicts_with_all = ["json", "prompt"])]
    mcps: bool,
    /// Explain PROFILE from [subagents.profiles], as `rimz agents --subagent-profile` launches it.
    #[arg(long)]
    subagent_profile: bool,
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
    cli::report_unknown_config_keys(&machine)?;
    let mut effective = rimz::config::effective::load(&machine, &workspace.project_root)?;
    let state = rimz::StatePaths::for_project_root(&workspace.project_root)?;
    let runtime = rimz::RuntimePaths::for_state(&state)?;
    let channel = cli::current_channel(&workspace, store.as_ref());
    let mut warnings = Vec::new();
    let (request, cwd, action_note, system_prompt_digest, profiles, tuning) = if args
        .target
        .starts_with('@')
    {
        let store = store.as_ref().with_context(|| {
            cli::no_room(format_args!("at {}", workspace.project_root.display()))
        })?;
        let snapshot = snapshot
            .as_ref()
            .expect("an existing store supplied the snapshot");
        let (agent, _) = super::resolve_live_or_audit(
            store,
            &workspace,
            &runtime,
            snapshot,
            &args.target,
            &channel.address_context(),
        )?;
        let scope = ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child());
        let profiles = effective.profiles_for_launch(scope, agent.profile.as_deref());
        let posture = rimz::launcher::swap::member_posture(&agent, profiles, &effective.teams);
        if let Some(reason) = &posture.degraded {
            warnings.push(format!("{reason}; showing bare {} posture", agent.kind));
        }
        let cwd = agent
            .worktree_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace.worktree_root.clone());
        let logins = rimz::agents::room_accounts(
            &state.workspace_record,
            Some(&workspace.project_root),
            &rimz::config::MachineConfig::load_lenient(),
        )?;
        let (action, login, note) = rimz::launcher::relaunch::relaunch_action(
            &agent,
            &logins,
            &rimz::agents::machine_login_catalog(),
            &cwd,
        )?;
        (
            rimz::launcher::swap::relaunch_request(&agent, &posture, action, login, None),
            cwd,
            note.map(str::to_owned),
            agent.system_prompt_digest.clone(),
            profiles,
            posture.launch.tuning.clone(),
        )
    } else {
        let scope = if args.subagent_profile {
            rimz::harness::plan::check_subagent_profile_spec(
                &effective,
                &machine.agents.commands,
                Some(&args.target),
            )?;
            ProfileScope::Subagents
        } else {
            ProfileScope::Agents
        };
        let finalized = launch_resolve::resolve_finalized_layout(
            snapshot.as_ref(),
            &machine,
            &effective,
            scope,
            Some(&args.target),
            None,
            &args.overrides,
            args.budget,
            None,
            channel.as_deref().filter(|_| !args.subagent_profile),
            false,
            Some((&runtime, &state)),
        )?;
        warnings.extend(finalized.warnings.iter().map(ToString::to_string));
        effective.profiles = finalized.profiles;
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
        let projection = store
            .as_ref()
            .map(|store| store.runtime_projection(rimz::RuntimeScope::Audit))
            .transpose()?;
        // Explain launches nothing, so a refusal the real launch would raise
        // is reported and the plan renders without ancestry.
        let ancestry = match projection.as_ref().map(|projection| {
            rimz::harness::ancestry::resolve_launch_ancestry_here(
                &projection.agents,
                false,
                machine.agents.max_chain_length,
            )
        }) {
            Some(Ok(ancestry)) => ancestry,
            Some(Err(refusal)) => {
                warnings.push(format!(
                    "a real launch from here would be refused ({refusal}); the plan below assumes no launch ancestry"
                ));
                None
            }
            None => None,
        };
        let agents = projection
            .as_ref()
            .map_or(&[][..], |projection| &projection.agents[..]);
        let caller = rimz::harness::ancestry::resolve_calling_agent(agents).ok();
        let lane = rimz::harness::lanes::decide(&rimz::harness::lanes::LaneRequest {
            project_root: &workspace.project_root,
            target: &workspace.worktree_root,
            typed: None,
            team: resolved.team_name.as_deref(),
            joins_cohort: finalized.inferred_lane.is_some(),
            caller: rimz::launcher::context::caller_lane(&workspace.project_root, caller, false),
            agents,
        })?;
        let identities = rimz::harness::plan::launch_identity_requests(
            &resolved.layout,
            None,
            None,
            resolved.team_name.as_deref(),
            Some(&lane),
            None,
            None,
            ancestry.as_ref(),
        )?;
        let mut params = identities
            .into_iter()
            .next()
            .expect("one cell yields one identity request")
            .launch;
        if args.subagent_profile {
            params.profile_scope = Some(scope);
        }
        // A preview has no allocating batch; resolve its prospective account without writing a launch.
        let accounts = rimz::agents::room_accounts(
            &state.workspace_record,
            Some(&workspace.project_root),
            &machine,
        )?;
        let login = rimz::store::writer::LaunchLogin::from(cell.launch.login.clone()).resolve(
            &cell.kind,
            &accounts,
            &machine.accounts,
        )?;
        params.login = (!login.is_default()).then(|| login.name().clone());
        let tuning = cell.tuning.clone();
        let request = ExecRequest::fresh(
            cell,
            ExecIdentity {
                row_key: None,
                resume_model_override: false,
                name: None,
                name_explicit: false,
                launch_id: None,
                params,
            },
            None,
            false,
        );
        (
            request,
            workspace.worktree_root.clone(),
            None,
            None,
            &effective.profiles,
            tuning,
        )
    };
    let adapter = rimz::agents::find_definition(request.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", request.kind))?;
    let isolation = Isolation::resolve(
        request.identity.params.isolation,
        request.isolation_default,
        machine.agents.isolation,
    );
    let bwrap = rimz::sandbox::preflight_launch(
        isolation,
        &request.kind,
        request.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let ambient_env = rimz::agents::ambient_env();
    if Isolation::ambient(&ambient_env).is_some_and(Isolation::is_scoped) {
        warnings.push("the rendered skill view is planned from this pane's scoped view and can differ from the host's".to_owned());
    }
    let plan = launch_plan::compile(LaunchPlanInputs {
        recorded_system_prompt_digest: system_prompt_digest.as_deref(),
        request: &request,
        cwd: &cwd,
        project_root: &workspace.project_root,
        rimz_bin: &rimz::proc::rimz_exe(),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        machine: &machine,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation,
        bwrap: bwrap.as_deref(),
        agents: Ok(snapshot.as_ref().map_or(&[], |snapshot| &snapshot.agents)),
        ambient_env: &ambient_env,
        agent_shell: machine.agents.shell.as_deref(),
        hibernate: machine.agents.hibernate.enabled()
            && rimz::harness::hibernate::Admission::read(&runtime).available,
    })?;
    warnings.extend(plan.warnings.iter().map(ToString::to_string));
    let report = ExplainReport::new(
        &args,
        &plan,
        profiles,
        &tuning,
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
    if args.mcps {
        return render::finish(write_mcps(
            &plan,
            machine.agents.rimz_mcp_server,
            &mut render::out(),
        ));
    }
    if args.prompt {
        write_prompt(&report.prompt, &mut render::out())?;
        if let Some(notice) = undelivered_reminder_notice(&report.prompt, report.kind.as_str()) {
            writeln!(render::err(), "rimz: {notice}")?;
        }
        return Ok(());
    }
    render_explain(&report)
}

/// What a launch hands its provider for MCP: RimZ's server and the load mode
/// of each of its tools, the user's servers it keeps, each `mcps` mark the
/// adapter did not apply, and the provider arguments that carry them, all read
/// from the compiled plan; then the instructions and each tool's description
/// and schema, as the server sends them.
fn write_mcps(plan: &LaunchPlan, server_on: bool, w: &mut impl Write) -> std::io::Result<()> {
    let kind = plan.request.kind.as_str();
    let adapter = rimz::agents::find_definition(kind).expect("compiled agent has an adapter");
    let mut servers = render::KeyVals::new().indent(2);
    if !adapter.takes_mcp_servers() {
        servers.push(
            "rimz",
            render::cell(format!(
                "{kind} takes no MCP servers from a launch; it loads its own as configured"
            )),
        );
        writeln!(w, "MCP servers")?;
        return servers.render(w);
    }
    let process = plan.process();
    let rides_agent_file = adapter.rides_agent_file();
    let arguments = if rides_agent_file {
        agent_file_mcp_lines(plan)
    } else {
        mcp_arguments(&process.provider_argv)
    };
    let served = &plan.mcp_capabilities[..];
    let launch_env: Vec<(&str, &str)> = if adapter.serves_mcp_from_launch_env() {
        [launch_env::ENV_SERVE, launch_env::ENV_CWD]
            .into_iter()
            .filter_map(|key| Some((key, process.env.get(key)?.as_str())))
            .collect()
    } else {
        Vec::new()
    };
    // A resume writes no agent file: the conversation keeps the servers it started with, except
    // a server the provider starts from its own config, which follows each launch's env.
    let resumes = rides_agent_file && !matches!(plan.request.action, ExecAction::Launch { .. });
    let kept = "kept from the conversation's start: a resume writes no agent file";
    let rimz_line = if let Some((flag, value)) = &plan.mcp_switched_off {
        format!("off: {flag} {value}")
    } else if !served.is_empty() {
        let names: Vec<_> = served.iter().map(|c| c.name()).collect();
        match adapter.mcp_server_name() {
            SERVER_NAME => format!("serves {}", names.join(", ")),
            name => format!("serves {} as `{name}`", names.join(", ")),
        }
    } else if resumes && !adapter.serves_mcp_from_launch_env() {
        kept.to_owned()
    } else if !server_on {
        "off: [agents] rimz-mcp-server = false".to_owned()
    } else if rides_agent_file && plan.agent_file.is_none() && !adapter.serves_mcp_from_launch_env()
    {
        "not handed over: this launch writes no agent file".to_owned()
    } else {
        format!(
            "not handed over: {}",
            if plan.request.rimz_capabilities.is_some() {
                "its capabilities name no RimZ capability with MCP tools"
            } else {
                "the mcps list names no RimZ capability"
            }
        )
    };
    servers.push("rimz", render::cell(rimz_line));
    let user_line = if resumes {
        kept.to_owned()
    } else {
        user_servers(plan, &arguments, rides_agent_file)
    };
    servers.push("your servers", render::cell(user_line));
    writeln!(w, "MCP servers")?;
    servers.render(w)?;
    if !plan.hidden_skills.is_empty() {
        writeln!(w, "\nHidden skills")?;
        for skill in &plan.hidden_skills {
            writeln!(
                w,
                "  {skill}: the launch serves its MCP tools, so the model is not offered the skill"
            )?;
        }
    }
    let wording = Wording::read_home();
    let tools: Vec<_> = tools_for(served).collect();
    if !served.is_empty() {
        let mut table = Table::new(["CAPABILITY", "TOOL", "TOKENS", "LOAD"]).right(&[2]);
        let mut total = 0;
        for tool in &tools {
            let size = tool.tokens(&wording);
            total += size;
            let load = load_mode(&arguments, &launch_env, tool.capability);
            table.row([
                render::cell(tool.group_name()),
                render::cell(tool.name),
                render::cell(format!("~{size}")),
                render::cell(load),
            ]);
        }
        writeln!(w, "\nTools")?;
        table.render(w)?;
        writeln!(w, "total: ~{total} tokens in {} tools", tools.len())?;
    }
    if !process.mcp_warnings.is_empty() {
        writeln!(w, "\nNot applied")?;
        for warning in &process.mcp_warnings {
            writeln!(w, "  {warning}")?;
        }
    }
    if !arguments.is_empty() {
        let heading = if rides_agent_file {
            "Agent file"
        } else {
            "Provider arguments"
        };
        writeln!(w, "\n{heading}")?;
        for (flag, value) in &arguments {
            writeln!(w, "  {flag} {value}")?;
        }
    }
    if !launch_env.is_empty() {
        writeln!(w, "\nLaunch environment")?;
        for (key, value) in &launch_env {
            writeln!(w, "  {key}={value}")?;
        }
    }
    let pointers: &[&str] = match kind {
        "claude" => &[
            "claude mcp list: the servers Claude connects for the account and project",
            "/context in the session: what fills Claude's context, its MCP tools included",
        ],
        "codex" => &[
            "CODEX_ROLLOUT_TRACE_ROOT=<dir> on a launch: Codex writes each request's tool \
             declarations there",
        ],
        _ => &[],
    };
    writeln!(w, "\nCheck it")?;
    writeln!(w, "  rimz mcp tools: every RimZ tool and its size")?;
    for pointer in pointers {
        writeln!(w, "  {pointer}")?;
    }
    if served.is_empty() {
        return Ok(());
    }
    let modes: Vec<_> = served
        .iter()
        .filter_map(|&capability| {
            LoadMode::from_name(load_mode(&arguments, &launch_env, Some(capability)))
                .map(|mode| (capability, mode))
        })
        .collect();
    let server_wide = LoadMode::from_name(load_mode(&arguments, &launch_env, None));
    writeln!(w, "\nServed text")?;
    writeln!(
        w,
        "  what RimZ's server puts in context, word for word; rimz cannot see the tools of your \
         own MCP servers\n"
    )?;
    write!(
        w,
        "{}",
        render_listing(&tools, served, &modes, server_wide, &wording)
    )
}

/// The user's servers a launch keeps, read from what the adapter wrote: the
/// name-only items an agent file lists beside `inheritMcp: false`, and on a kind
/// whose prompt rides an agent file nothing else; a Claude allowlist in its
/// settings (a file or the inline `--settings`), or the servers a Codex `-c`
/// override switches off.
fn user_servers(plan: &LaunchPlan, arguments: &[(&str, &str)], rides_agent_file: bool) -> String {
    if arguments.contains(&("inheritMcp:", "false")) {
        let items = arguments
            .iter()
            .find(|(key, _)| *key == "mcpServers:")
            .and_then(|(_, value)| serde_json::from_str::<Vec<serde_json::Value>>(value).ok())
            .unwrap_or_default();
        let names: Vec<_> = items
            .iter()
            .filter_map(|item| item["name"].as_str())
            .filter(|name| *name != SERVER_NAME)
            .collect();
        let names = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        };
        return format!("only {names} (inheritMcp: false)");
    }
    if rides_agent_file {
        return "every server the account and project configure".to_owned();
    }
    let process = plan.process();
    let inline = process
        .provider_argv
        .windows(2)
        .rev()
        .find(|pair| pair[0] == "--settings")
        .and_then(|pair| serde_json::from_str::<serde_json::Value>(&pair[1]).ok());
    if let Some(allowed) = process
        .settings()
        .or(inline.as_ref())
        .and_then(|settings| settings.get("allowedMcpServers"))
        .and_then(serde_json::Value::as_array)
    {
        let names: Vec<_> = allowed
            .iter()
            .filter_map(|entry| entry["serverName"].as_str())
            .filter(|name| *name != SERVER_NAME)
            .collect();
        let names = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        };
        return format!("only {names} (allowedMcpServers)");
    }
    // Codex applies the last `mcp_servers=` table it is given.
    let table = mcp_arguments(&process.provider_argv)
        .into_iter()
        .filter(|(_, value)| value.starts_with("mcp_servers="))
        .filter_map(|(_, value)| toml::from_str::<toml::Table>(value).ok())
        .filter_map(|table| table.get("mcp_servers")?.as_table().cloned())
        .next_back()
        .unwrap_or_default();
    let switched_off: Vec<_> = table
        .iter()
        .filter(|(_, entry)| entry.get("enabled").and_then(toml::Value::as_bool) == Some(false))
        .map(|(name, _)| name.as_str())
        .collect();
    if switched_off.is_empty() {
        return "every server the account and project configure".to_owned();
    }
    format!("switched off: {}", switched_off.join(", "))
}

/// A capability's load mode as the launch passes it. On Claude, a
/// `--always` or `--deferred` flag naming it in the `rimz` server's
/// `--mcp-config` args, else that entry's `alwaysLoad`; on Antigravity, its
/// mark in the launch's `RIMZ_MCP_SERVE` (`launch_env`); on Codex, the
/// server's `omit_tools_from` override, where `["deferred"]` loads its tools
/// from the start and `[]` defers them; the last one passed wins, as in
/// Codex. `default` when neither is passed. With no `capability`, as for
/// `guide`, only the server-wide mode counts, which the launch env never sets.
fn load_mode(
    arguments: &[(&str, &str)],
    launch_env: &[(&str, &str)],
    capability: Option<Capability>,
) -> &'static str {
    if let Some((_, value)) = launch_env
        .iter()
        .find(|(key, _)| *key == launch_env::ENV_SERVE)
    {
        return launch_env::parse(value)
            .ok()
            .and_then(|served| {
                served
                    .into_iter()
                    .find(|(served, _)| Some(*served) == capability)?
                    .1
            })
            .map_or("default", LoadMode::name);
    }
    let omit_prefix = format!("mcp_servers.{SERVER_NAME}.omit_tools_from=");
    let mut load = None;
    for (flag, value) in arguments {
        if *flag == "--mcp-config" {
            let Ok(config) = serde_json::from_str::<serde_json::Value>(value) else {
                continue;
            };
            let Some(server) = config["mcpServers"].get(SERVER_NAME) else {
                continue;
            };
            let args: Vec<&str> = server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            let flagged = args
                .windows(2)
                .filter(|pair| capability.is_some_and(|capability| pair[1] == capability.name()))
                .find_map(|pair| pair[0].strip_prefix("--").and_then(LoadMode::from_name));
            let server_wide = server["alwaysLoad"].as_bool().map(|always| {
                if always {
                    LoadMode::Always
                } else {
                    LoadMode::Deferred
                }
            });
            load = flagged.or(server_wide).or(load);
        } else if let Some(omit) = value.strip_prefix(&omit_prefix) {
            let Ok(table) = toml::from_str::<toml::Table>(&format!("omit = {omit}")) else {
                continue;
            };
            let Some(surfaces) = table["omit"].as_array() else {
                continue;
            };
            let mode = if surfaces
                .iter()
                .any(|surface| surface.as_str() == Some("deferred"))
            {
                LoadMode::Always
            } else {
                LoadMode::Deferred
            };
            load = Some(mode);
        }
    }
    load.map_or("default", LoadMode::name)
}

/// The MCP lines of a planned agent file's frontmatter, each split into its key and value.
fn agent_file_mcp_lines(plan: &LaunchPlan) -> Vec<(&str, &str)> {
    let Some(text) = plan
        .agent_file
        .as_ref()
        .and_then(|file| std::str::from_utf8(&file.bytes).ok())
    else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .filter_map(|line| line.split_once(' '))
        .filter(|(key, _)| matches!(*key, "inheritMcp:" | "mcpServers:"))
        .collect()
}

/// The provider arguments that carry MCP servers, in argv order: Claude's
/// `--mcp-config` and Codex's `mcp_servers…` overrides, given as `-c <kv>`,
/// `--config <kv>`, or `--config=<kv>`.
fn mcp_arguments(argv: &[String]) -> Vec<(&str, &str)> {
    let mut arguments = Vec::new();
    for (index, arg) in argv.iter().enumerate() {
        let next = argv.get(index + 1).map(String::as_str);
        let (flag, value) = match (arg.as_str(), next) {
            ("--mcp-config", Some(value)) => (arg.as_str(), value),
            ("-c" | "--config", Some(value)) => (arg.as_str(), value),
            _ => match arg.strip_prefix("--config=") {
                Some(value) => ("--config", value),
                None => continue,
            },
        };
        if flag == "--mcp-config" || value.starts_with("mcp_servers") {
            arguments.push((flag, value));
        }
    }
    arguments
}

fn validate_target_overrides(args: &ExplainArgs) -> Result<()> {
    if args.target.starts_with('@') && args.subagent_profile {
        bail!(
            "--subagent-profile applies to a profile name; `{}` explains the profile table its launch recorded; drop the flag",
            args.target
        );
    }
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
    isolation: Isolation,
    isolation_source: String,
    target: &'a str,
    kind: &'a rimz::ids::AgentKind,
    action: &'static str,
    action_note: Option<String>,
    account: String,
    name: Option<&'a str>,
    launch_id: Option<&'a str>,
    cwd: &'a Path,
    profile: Option<ProfileReport<'a>>,
    overrides: Vec<String>,
    mode: Option<PermissionMode>,
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<rimz::config::tiers::ModelTier>,
    #[serde(skip_serializing_if = "<[rimz::agents::TierSkip]>::is_empty")]
    tier_skipped: &'a [rimz::agents::TierSkip],
    #[serde(skip_serializing_if = "<[rimz::agents::TierUnbuildable]>::is_empty")]
    tier_unbuildable: &'a [rimz::agents::TierUnbuildable],
    #[serde(skip)]
    tier_stamp: Option<&'a rimz::agents::TierStamp>,
    effort: Option<&'a str>,
    effort_source: Option<String>,
    fast: Option<bool>,
    fast_source: Option<String>,
    code_mode: Option<bool>,
    code_mode_source: Option<String>,
    auto_compact: Option<u64>,
    auto_compact_source: Option<String>,
    budget: Option<&'a str>,
    grant: GrantReport<'a>,
    skills: SkillsReport<'a>,
    program: &'a str,
    provider_argv: Vec<String>,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    unset: &'a BTreeSet<String>,
    redacted_keys: &'a BTreeSet<String>,
    prompt: PromptReport<'a>,
    sandbox: Option<SandboxReport<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_links: Option<&'a rimz::agents::skill_links::SkillLinkPlan>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct ProfileReport<'a> {
    name: &'a str,
    chain: Vec<String>,
    role: Option<&'a str>,
    team: Option<&'a str>,
}
/// What the launch may do: its tools, each RimZ capability it has and how it reaches it, its
/// subagents allowlist, and its pre-approved rules.
#[derive(Serialize)]
struct GrantReport<'a> {
    /// The applied tools list, less what the kind's argv drops; `None` leaves the provider's
    /// own default.
    tools: Option<Vec<String>>,
    /// The reminder's capability lines, as `(name, how it reaches it)`.
    rimz_capabilities: Vec<RimzReach<'a>>,
    /// The profile's subagents allowlist, empty on a launch that cannot delegate; `None` leaves
    /// delegation unrestricted.
    subagents: Option<Vec<String>>,
    #[serde(skip)]
    cannot_delegate: bool,
    allowed_tools: Option<&'a [rimz::config::ToolRule]>,
}
#[derive(Serialize)]
struct RimzReach<'a> {
    name: &'a str,
    via: &'a str,
}
#[derive(Serialize)]
struct SkillsReport<'a> {
    callable: Option<Vec<SkillName>>,
    /// The rimz skills hidden because the launch registers their MCP tools.
    hidden: &'a [&'static str],
    #[serde(skip)]
    no_skill_tool: bool,
    #[serde(skip)]
    grant: bool,
    applied: bool,
    host: Option<&'a rimz::agents::skills::HostSkillPlan>,
}
#[derive(Serialize)]
struct PromptSource<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a Path>,
    /// Names text compiled into RimZ, which has no path.
    #[serde(skip_serializing_if = "Option::is_none")]
    builtin: Option<&'static str>,
    bytes: u64,
    /// The on-disk inspection copy of built-in text, named in the human
    /// report only; nothing reads it back, so the JSON shape leaves it out.
    #[serde(skip)]
    copy: Option<std::path::PathBuf>,
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
        profiles: &ProfilesConfig,
        tuning: &rimz::harness::spec::TuningSources,
        bwrap: Option<&'a Path>,
        mut warnings: Vec<String>,
        action_note: Option<String>,
    ) -> Result<Self> {
        let process = plan.process();
        let request = &plan.request;
        let params = &request.identity.params;
        let routed_profile = params
            .profile
            .as_ref()
            .and_then(|name| profiles.0.get(name))
            .filter(|profile| {
                profile.definition_renders.as_ref().is_some_and(|renders| {
                    args.overrides
                        .agent
                        .as_deref()
                        .map(str::trim)
                        .filter(|family| !family.is_empty())
                        .is_none_or(|family| renders.renders.contains_key(family))
                })
            });
        let adapter = rimz::agents::find_definition(request.kind.as_str())
            .expect("compiled agent has an adapter");
        let resolved = params.profile.as_deref().and_then(|name| {
            rimz::harness::spec::resolve_profile_rebased(
                name,
                rimz::harness::plan::normalized_preset_value(args.overrides.agent.as_deref())
                    .as_deref(),
                profiles,
            )
            .ok()
        });
        // A source names the layer that supplied the planned value; a value the plan
        // dropped or leaves unset has none.
        let source = |source: &Option<rimz::harness::spec::TuningSource>, planned: bool| {
            source.as_ref().filter(|_| planned).map(ToString::to_string)
        };
        let auto_compact = params
            .record
            .as_ref()
            .and_then(|record| record.auto_compact);
        let profile = params.profile.as_deref().map(|name| ProfileReport {
            name,
            chain: resolved
                .clone()
                .map(rimz::harness::spec::ResolvedProfile::into_chain)
                .unwrap_or_default(),
            role: params.role.as_deref(),
            team: params.team.as_deref(),
        });
        let mut env = process.env.clone();
        let account_home = plan.login.home().map(|home| home.to_string_lossy());
        for (key, value) in &mut env {
            // The account home is RimZ's own override, not the project secret
            // it shadows, so it stays readable even when the key was declared
            // in the trusted project env.
            if process.secret_keys.contains(key) && account_home.as_deref() != Some(value.as_str())
            {
                *value = launch::REDACTED_ENV_VALUE.to_owned();
            }
        }
        let prompt_sources = &plan.prompt.sources;
        let file_source = |path: &'a Path| -> Result<PromptSource<'a>> {
            Ok(PromptSource {
                path: Some(path),
                builtin: None,
                copy: None,
                bytes: std::fs::metadata(path)
                    .with_context(|| format!("reading prompt source {}", path.display()))?
                    .len(),
            })
        };
        let agents_home = rimz::disk::paths::agents_home();
        let reflect_copy = team_prompt::reflect_copy_path(&agents_home);
        let prompt_source = |source: &'a rimz::config::PromptSource| -> Result<PromptSource<'a>> {
            match source {
                rimz::config::PromptSource::File(path) => file_source(path),
                rimz::config::PromptSource::Text { origin, text } if *origin == reflect_copy => {
                    Ok(PromptSource {
                        path: None,
                        builtin: Some("consensus piece reflect"),
                        bytes: text.len() as u64,
                        copy: Some(origin.clone()).filter(|path| path.is_file()),
                    })
                }
                rimz::config::PromptSource::Text { origin, text } => Ok(PromptSource {
                    path: Some(origin),
                    builtin: None,
                    copy: None,
                    bytes: text.len() as u64,
                }),
            }
        };
        let mut sources = prompt_sources
            .system_prompt_file
            .iter()
            .chain(&prompt_sources.append_system_prompt_files)
            .map(prompt_source)
            .collect::<Result<Vec<_>>>()?;
        if let Some(team_prompt) = &prompt_sources.team_prompt {
            sources.push(match &team_prompt.consensus {
                Consensus::BuiltIn => PromptSource {
                    path: None,
                    builtin: Some("team consensus"),
                    bytes: BUILT_IN_CONSENSUS.len() as u64,
                    copy: Some(team_prompt::consensus_copy_path(&agents_home))
                        .filter(|path| path.is_file()),
                },
                Consensus::File(path) => file_source(path)?,
            });
            for path in &team_prompt.files {
                sources.push(prompt_source(path)?);
            }
        }
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
        if let Some(links) = &plan.skill_links {
            warnings.extend(links.shadowed_report(links.shadowed()));
        }
        Ok(Self {
            // Compilation stamps every launch, including login-shell reentry.
            isolation: Isolation::ambient(&process.env).expect("compiled launch stamps isolation"),
            isolation_source: if params.isolation.is_some() && args.target.starts_with('@') {
                "recorded --isolation".to_owned()
            } else if params.isolation.is_some() {
                "--isolation".to_owned()
            } else if request.isolation_default.is_some() {
                format!(
                    "profile default {}",
                    params.profile.as_deref().unwrap_or(request.kind.as_str())
                )
            } else {
                "machine policy".to_owned()
            },
            target: &args.target,
            kind: &request.kind,
            action: match request.action {
                ExecAction::Launch { .. } => "launch",
                ExecAction::Pick { .. } => "pick",
                ExecAction::Resume { .. } => "resume",
                ExecAction::Fork { .. } => "fork",
            },
            action_note: action_note.or_else(|| match &request.action {
                ExecAction::Resume { session_id, .. } => {
                    Some(format!("resumes session {session_id}"))
                }
                _ => None,
            }),
            account: plan.login.key().to_string(),
            name: request.identity.name.as_deref(),
            launch_id: request.identity.launch_id.as_deref(),
            cwd: &plan.cwd,
            profile,
            overrides: applied_overrides(
                args,
                &process.provider_argv,
                request.kind.as_str(),
                params.model.as_deref(),
                params.effort.as_deref(),
            ),
            mode: params.mode,
            model: params.model.as_deref(),
            tier: if let Some(stamp) = &params.tier {
                Some(stamp.tier)
            } else if args.target.starts_with('@')
                || routed_profile.is_none()
                || args
                    .overrides
                    .model
                    .as_deref()
                    .is_some_and(|model| !model.trim().is_empty())
            {
                None
            } else {
                args.overrides.tier.or_else(|| {
                    routed_profile
                        .and_then(|profile| profile.model_tier.as_ref())
                        .map(|provenance| provenance.tier)
                })
            },
            effort: params.effort.as_deref(),
            effort_source: source(&tuning.effort, params.effort.is_some()),
            fast: params.fast,
            fast_source: source(&tuning.fast, params.fast.is_some()),
            code_mode: params.code_mode,
            code_mode_source: source(&tuning.code_mode, params.code_mode.is_some()),
            auto_compact,
            auto_compact_source: source(&tuning.auto_compact, auto_compact.is_some()),
            tier_skipped: params
                .tier
                .as_ref()
                .map_or(&[], |stamp| stamp.skipped.as_slice()),
            tier_unbuildable: params
                .tier
                .as_ref()
                .map_or(&[], |stamp| stamp.unbuildable.as_slice()),
            tier_stamp: params.tier.as_deref(),
            budget: params.budget.as_deref(),
            grant: GrantReport {
                tools: request
                    .tools
                    .as_deref()
                    .and_then(|tools| rimz::agents::applied_tools(request.kind.as_str(), tools)),
                rimz_capabilities: plan
                    .rimz_reach
                    .iter()
                    .map(|(name, via)| RimzReach { name, via })
                    .collect(),
                subagents: if request.cannot_delegate() {
                    Some(Vec::new())
                } else {
                    params
                        .profile
                        .as_ref()
                        .and_then(|name| profiles.0.get(name))
                        .and_then(|profile| profile.subagents.clone())
                },
                cannot_delegate: request.cannot_delegate(),
                allowed_tools: request.allowed_tools.as_deref(),
            },
            skills: SkillsReport {
                // A granted name the reminder gives no reach has no skill on disk to call.
                callable: request
                    .callable_skills(&plan.withheld_skills)
                    .map(|skills| {
                        skills
                            .into_iter()
                            .filter(|skill| {
                                let skill = skill.to_string();
                                request.rimz_capabilities.is_none()
                                    || rimz::mcp::catalog::Capability::from_name(&skill).is_none()
                                    || plan.rimz_reach.iter().any(|(name, _)| *name == skill)
                            })
                            .collect()
                    }),
                hidden: &plan.hidden_skills,
                no_skill_tool: request.no_skill_tool,
                grant: request.rimz_capabilities.is_some(),
                applied: bwrap.is_some(),
                host: process.host_skills.as_ref(),
            },
            program: &process.provider_program,
            provider_argv: launch::redact_env_tokens(&process.provider_argv, |key| {
                process.secret_keys.contains(key)
            }),
            argv: launch::redact_env_tokens(&process.argv, |key| process.secret_keys.contains(key)),
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
                artifact: plan
                    .prompt
                    .artifact
                    .as_deref()
                    .or(plan.agent_file.as_ref().map(|file| file.path.as_path())),
                reminder: Some(process.reminder.as_str()),
                reminder_channel: plan.reminder_channel.as_ref().map(reminder_channel_label),
                reminder_delivered: plan.reminder_channel.is_some(),
            },
            sandbox,
            skill_links: plan.skill_links.as_ref(),
            warnings,
        })
    }
}

/// The Grant section: the tools, each RimZ capability as the reminder words its reach, the
/// subagents allowlist, and the allowed-tools rules.
fn write_grant(grant: &GrantReport<'_>, output: &mut impl Write) -> std::io::Result<()> {
    let joined = |names: &mut dyn Iterator<Item = String>, empty: &str| {
        let names: Vec<_> = names.collect();
        if names.is_empty() {
            empty.to_owned()
        } else {
            names.join(", ")
        }
    };
    writeln!(output, "\nGrant")?;
    let mut rows = render::KeyVals::new().indent(2);
    rows.push(
        "tools",
        render::cell(grant.tools.as_deref().map_or_else(
            || "provider default (no tools list)".to_owned(),
            |tools| joined(&mut tools.iter().cloned(), "none"),
        )),
    );
    if grant.rimz_capabilities.is_empty() {
        rows.push("rimz", render::cell("no RimZ capability"));
    }
    for reach in &grant.rimz_capabilities {
        rows.push(reach.name, render::cell(reach.via));
    }
    rows.push(
        "subagents",
        render::cell(match grant.subagents.as_deref() {
            _ if grant.cannot_delegate => "none (cannot delegate)".to_owned(),
            None => "unrestricted".to_owned(),
            Some(names) => joined(&mut names.iter().cloned(), "none"),
        }),
    );
    rows.push(
        "allowed-tools",
        render::cell(joined(
            &mut grant
                .allowed_tools
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string),
            "none",
        )),
    );
    rows.render(output)
}

/// The plan's text row for a tuning value, marked when the kind base supplied it.
fn with_base_mark(value: &str, source: Option<&str>) -> String {
    match source.filter(|source| source.starts_with("kind base")) {
        Some(source) => format!("{value} ({source})"),
        None => value.to_owned(),
    }
}

fn channel_label(matcher: PresetArgMatcher) -> String {
    match matcher {
        PresetArgMatcher::Flag(flags) | PresetArgMatcher::TextFlag(flags) => flags.join(" "),
        PresetArgMatcher::ConfigKey { flags, key } => format!("{} {key}", flags.join(" ")),
        PresetArgMatcher::EnvPathVar(key) => key,
        PresetArgMatcher::EnvConfigMember { var, member } => format!("{var} {member}"),
        PresetArgMatcher::AgentFile(flag) => format!("agent file selected by {flag}"),
    }
}

fn reminder_channel_label(channel: &SystemTextChannel) -> String {
    match channel {
        SystemTextChannel::TextFlag { flags } => flags.join(" "),
        SystemTextChannel::ConfigKey { flags, key } => format!("{} {key}", flags.join(" ")),
        SystemTextChannel::ExtensionEnv => "rimz extension".to_owned(),
        SystemTextChannel::AgentFile => "the launch's agent file".to_owned(),
    }
}

/// The `--prompt` notice for a reminder the launch renders but no channel carries.
fn undelivered_reminder_notice(prompt: &PromptReport<'_>, kind: &str) -> Option<String> {
    (prompt.reminder.is_some() && !prompt.reminder_delivered).then(|| {
        format!("this {kind} launch has no reminder channel; the reminder is not delivered")
    })
}

fn applied_overrides(
    args: &ExplainArgs,
    provider_argv: &[String],
    kind: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Vec<String> {
    let mut flags = Vec::new();
    let overrides = &args.overrides;
    if let Some(account) = &overrides.account {
        flags.push(format!("--account {account}"));
    }
    if let Some(tier) = overrides.tier {
        flags.push(format!(
            "--tier {tier} → {kind} {} ({})",
            model.unwrap_or("-"),
            effort.unwrap_or("-")
        ));
    }
    for (flag, value) in [
        ("--model", &overrides.model),
        ("--agent", &overrides.agent),
        ("--effort", &overrides.effort),
    ] {
        if let Some(value) = value {
            flags.push(format!("{flag} {value}"));
        }
    }
    match overrides.fast_override() {
        Some(true) => flags.push("--fast".to_owned()),
        Some(false) => flags.push("--no-fast".to_owned()),
        None => {}
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
        let unchanged = provider_argv
            .windows(overrides.passthrough.len())
            .any(|window| window == overrides.passthrough.as_slice());
        let value = if unchanged {
            overrides.passthrough.join(" ")
        } else {
            "(see provider argv)".to_owned()
        };
        flags.push(format!("-- {value}"));
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
        "isolation",
        render::cell(format!(
            "{} ({})",
            report.isolation, report.isolation_source
        )),
    );
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
    plan.push("account", render::cell(&report.account));
    if let Some(profile) = &report.profile {
        let mut chain = profile.chain.clone();
        if let (Some(team), Some(role)) = (profile.team, profile.role) {
            let qualified_role = format!("{team}.{role}");
            if chain.first() != Some(&qualified_role) {
                chain.insert(0, qualified_role);
            }
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
    plan.push(
        "model",
        render::cell(report.model.unwrap_or("provider default")),
    );
    plan.push(
        "effort",
        render::cell(report.effort.map_or_else(
            || "provider default".to_owned(),
            |effort| with_base_mark(effort, report.effort_source.as_deref()),
        )),
    );
    for (label, value, source) in [
        ("fast", report.fast, &report.fast_source),
        ("code-mode", report.code_mode, &report.code_mode_source),
    ] {
        let value = match value {
            Some(true) => "on",
            Some(false) => "off",
            None => "unset",
        };
        plan.push(
            label,
            render::cell(with_base_mark(value, source.as_deref())),
        );
    }
    if let Some(tokens) = report.auto_compact {
        plan.push(
            "auto-compact",
            render::cell(with_base_mark(
                &tokens.to_string(),
                report.auto_compact_source.as_deref(),
            )),
        );
    }
    if let Some(tier) = report.tier {
        plan.push(
            "tier",
            render::cell(format!(
                "{tier} → {}",
                report.model.unwrap_or("provider default")
            )),
        );
        if let Some(stamp) = report.tier_stamp {
            for pass in stamp.passes() {
                plan.push("skipped", render::cell(format!("{pass:#}")));
            }
        }
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
    write_grant(&report.grant, &mut output)?;
    writeln!(output, "\nSkills")?;
    writeln!(
        output,
        "  callable: {}",
        if report.skills.no_skill_tool {
            "none (tools list has no Skill)".to_owned()
        } else {
            report.skills.callable.as_deref().map_or_else(
                || {
                    if report.skills.grant {
                        "native discovery, less the RimZ skills outside the grant".to_owned()
                    } else {
                        "native discovery".to_owned()
                    }
                },
                |skills| {
                    if skills.is_empty() {
                        "none (all manual)".to_owned()
                    } else {
                        skills
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                },
            )
        }
    )?;
    use rimz::agents::skills::HostSkillPlan;
    if report.skills.applied {
        writeln!(output, "  profile view: scoped view applied")?;
    } else {
        match report.skills.host {
            Some(HostSkillPlan::Applied {
                flag,
                effect,
                unlisted,
                ..
            }) => {
                writeln!(
                    output,
                    "  profile view: host switch applied ({flag}, {} skills {effect})",
                    unlisted.len()
                )?;
                writeln!(
                    output,
                    "  unlisted keys: {}",
                    serde_json::to_string(unlisted)?
                )?;
            }
            Some(HostSkillPlan::Unenforced) => writeln!(
                output,
                "  profile view: not enforced ({} has no per-launch skill switch)",
                report.kind
            )?,
            None => writeln!(output, "  profile view: native discovery (no list)")?,
        }
    }
    if let Some(links) = report.skill_links {
        writeln!(output, "\nSkill links")?;
        writeln!(output, "  root: {}", links.root().display())?;
        writeln!(output, "  library: {}", links.library().display())?;
        for action in links.actions() {
            match action {
                rimz::agents::skill_links::SkillLinkAction::Link { name, target } => {
                    writeln!(output, "  link: {name} → {}", target.display())?;
                }
                rimz::agents::skill_links::SkillLinkAction::Unlink { name } => {
                    writeln!(output, "  unlink: {name}")?;
                }
            }
        }
        if !links.shadowed().is_empty() {
            writeln!(output, "  shadowed: {}", links.shadowed().join(", "))?;
        }
    }
    writeln!(output, "\nSandbox")?;
    if let Some(sandbox) = &report.sandbox {
        writeln!(output, "  bwrap: {}", sandbox.bwrap.display())?;
        let mut mounts = render::Table::new(["type", "source", "target"]).indent(2);
        for mount in sandbox.mounts {
            let (kind, source, target) = match mount {
                Mount::Bind { source, target } => ("bind", Some(source), target),
                Mount::RoBind { source, target } | Mount::RoBindTry { source, target } => {
                    ("ro-bind", Some(source), target)
                }
                Mount::Tmpfs { target } => ("tmpfs", None, target),
                Mount::Symlink { target, path } => ("symlink", Some(target), path),
                Mount::RemountRo { target } => ("remount-ro", None, target),
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
        for (key, pin) in &sandbox.pins {
            match pin {
                EnvPin::Set(value) => writeln!(output, "  pin: {key}={value}")?,
                EnvPin::Unset => writeln!(output, "  pin: unset {key}")?,
            }
        }
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
        let label = source.path.map_or_else(
            || format!("built-in {}", source.builtin.unwrap_or_default()),
            |path| path.display().to_string(),
        );
        match &source.copy {
            Some(copy) => writeln!(
                output,
                "  source: {label} ({} bytes; read-only copy at {})",
                source.bytes,
                copy.display()
            )?,
            None => writeln!(output, "  source: {label} ({} bytes)", source.bytes)?,
        }
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
            "── <{}> via {} ──\n{reminder}",
            rimz::agents::RimzBlock::SystemReminder.tag(),
            report
                .prompt
                .reminder_channel
                .as_deref()
                .unwrap_or("no reminder channel on this launch (not delivered)")
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
    fn opencode_system_prompt_channel_names_the_env_and_member() {
        let channel = rimz::agents::find_definition("opencode")
            .unwrap()
            .spec()
            .launch
            .preset_arg_matcher(PresetField::SystemPromptFile)
            .map(channel_label);
        assert_eq!(
            channel.as_deref(),
            Some("OPENCODE_CONFIG_CONTENT agents.build.system")
        );
    }

    /// An Antigravity prompt rides the fresh launch's agent file, with the reminder in it;
    /// a launch with no prompt sources writes no file, so its reminder has no channel.
    #[test]
    fn antigravity_prompt_and_reminder_channels_name_the_agent_file() {
        let channel = rimz::agents::find_definition("antigravity")
            .unwrap()
            .spec()
            .launch
            .preset_arg_matcher(PresetField::SystemPromptFile)
            .map(channel_label);
        assert_eq!(channel.as_deref(), Some("agent file selected by --agent"));
        assert_eq!(
            reminder_channel_label(&SystemTextChannel::AgentFile),
            "the launch's agent file"
        );
        let mut prompt = PromptReport {
            sources: Vec::new(),
            composed: None,
            channel,
            artifact: None,
            reminder: Some("<system_reminder>\nmodel\n</system_reminder>"),
            reminder_channel: None,
            reminder_delivered: false,
        };
        assert_eq!(
            undelivered_reminder_notice(&prompt, "antigravity").as_deref(),
            Some("this antigravity launch has no reminder channel; the reminder is not delivered")
        );
        prompt.reminder_channel = Some(reminder_channel_label(&SystemTextChannel::AgentFile));
        prompt.reminder_delivered = true;
        assert_eq!(undelivered_reminder_notice(&prompt, "antigravity"), None);
    }

    #[test]
    fn explain_tier_override_names_its_resolution() {
        let args = ParserArgs::try_parse_from(["explain", "coder", "--tier", "principal"])
            .unwrap()
            .explain;
        assert_eq!(
            applied_overrides(&args, &[], "claude", Some("fable"), Some("high")),
            ["--tier principal → claude fable (high)"]
        );
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
            "--account",
            "rimio",
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
            applied_overrides(&args, &["--foo".to_owned()], "claude", Some("opus"), None),
            ["--account rimio", "--model opus", "--yolo", "-- --foo"]
        );
        for conflict in [
            ["--json", "--prompt"],
            ["--mcps", "--json"],
            ["--mcps", "--prompt"],
        ] {
            let error = ParserArgs::try_parse_from(["explain", "coder", conflict[0], conflict[1]])
                .err()
                .unwrap();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
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
