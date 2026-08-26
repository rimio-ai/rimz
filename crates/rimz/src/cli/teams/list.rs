use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::super::{Ctx, GlobalFlags, render, report_unknown_config_keys};
use rimz::agents::AgentState;
use rimz::config::{CommandsConfig, ProfilesConfig, Team, TeamsConfig};
use rimz::harness::spec::{AgentCell, LayoutSpec};
use rimz::workspace::WorkspaceResolver;

#[derive(Clone, Debug, Serialize)]
pub(super) struct TeamReport {
    pub name: String,
    pub defined: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    pub roles: Vec<RoleReport>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub instances: Vec<LiveInstance>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RoleReport {
    pub role: String,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub append_system_prompt_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LiveInstance {
    pub channel: String,
    pub state: String,
    pub status_counts: BTreeMap<String, usize>,
    pub members: Vec<LiveMember>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LiveMember {
    pub handle: String,
    pub kind: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_fill_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let reports = load_catalog(globals, None)?;
    if json {
        return render::json_pretty(&reports);
    }
    write_catalog(&mut render::out(), &reports)
}

pub(super) fn load_catalog(
    globals: &GlobalFlags,
    worktree: Option<&str>,
) -> Result<Vec<TeamReport>> {
    let ctx = Ctx::open(globals)?;
    let machine = rimz::config::MachineConfig::load().context("loading machine config")?;
    report_unknown_config_keys(&machine)?;
    let effective = rimz::config::effective::load(
        &machine.agents,
        &machine.subagents.profiles,
        &ctx.workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let snapshot = ctx.alive_snapshot()?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let prices = rimz::agents::pricing::cached_book(&ctx.runtime().shared_pricing_cache_path());
    Ok(build_catalog(
        &effective.teams,
        &effective.profiles,
        &machine.agents.commands,
        &snapshot.agents,
        &audit.agents,
        &prices,
        worktree,
        |name| team_source(&ctx.workspace.project_root, name),
    ))
}

pub(super) fn effective_teams(globals: &GlobalFlags) -> Result<TeamsConfig> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine = rimz::config::MachineConfig::load().context("loading machine config")?;
    report_unknown_config_keys(&machine)?;
    Ok(rimz::config::effective::load(
        &machine.agents,
        &machine.subagents.profiles,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?
    .teams)
}

fn build_catalog(
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    agents: &[AgentState],
    audit_agents: &[AgentState],
    prices: &rimz::agents::PriceBook,
    worktree: Option<&str>,
    source: impl Fn(&str) -> Option<String>,
) -> Vec<TeamReport> {
    let live = live_instances(agents, audit_agents, prices, worktree);
    let mut reports = teams
        .0
        .iter()
        .map(|(name, team)| {
            definition_report(
                name,
                team,
                profiles,
                commands,
                source(name),
                live.get(name).cloned().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    for (name, instances) in live {
        if teams.0.contains_key(&name) {
            continue;
        }
        reports.push(TeamReport {
            name,
            defined: false,
            source: None,
            layout: None,
            leader: None,
            roles: Vec::new(),
            valid: false,
            error: None,
            instances,
        });
    }
    reports.sort_by(|left, right| left.name.cmp(&right.name));
    reports
}

fn definition_report(
    name: &str,
    team: &Team,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    source: Option<String>,
    instances: Vec<LiveInstance>,
) -> TeamReport {
    let resolved = rimz::harness::spec::resolve_team(
        name,
        &TeamsConfig(BTreeMap::from([(name.to_owned(), team.clone())])),
        profiles,
        commands,
    );
    let (layout, validation) = match resolved {
        Ok(layout) => {
            let validation = rimz::harness::spec::prompt_leader(&layout, Some(team)).map(|_| ());
            (Some(layout), validation)
        }
        Err(error) => (None, Err(error)),
    };
    let roles = layout
        .as_ref()
        .map(|layout| resolved_roles(team, layout))
        .unwrap_or_else(|| unresolved_roles(team, profiles));
    let leader = team
        .leader
        .clone()
        .or_else(|| team.roles.first().map(|binding| binding.role.clone()))
        .or_else(|| {
            layout
                .as_ref()
                .and_then(|layout| layout.agent_cells().next())
                .map(cell_label)
        });
    TeamReport {
        name: name.to_owned(),
        defined: true,
        source,
        layout: Some(team.layout.clone().unwrap_or_else(|| {
            team.roles
                .iter()
                .map(|role| role.role.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })),
        leader,
        roles,
        valid: validation.is_ok(),
        error: validation
            .err()
            .map(|error| render::one_line(&error.to_string())),
        instances,
    }
}

fn resolved_roles(team: &Team, layout: &LayoutSpec) -> Vec<RoleReport> {
    let cells = layout.agent_cells().collect::<Vec<_>>();
    if team.roles.is_empty() {
        return cells
            .into_iter()
            .map(|cell| role_report(cell_label(cell), cell))
            .collect();
    }
    team.roles
        .iter()
        .filter_map(|binding| {
            cells
                .iter()
                .copied()
                .find(|cell| cell.launch.role.as_deref() == Some(binding.role.as_str()))
                .map(|cell| role_report(binding.role.clone(), cell))
        })
        .collect()
}

fn role_report(role: String, cell: &AgentCell) -> RoleReport {
    RoleReport {
        role,
        profile: cell
            .launch
            .profile
            .clone()
            .unwrap_or_else(|| cell.kind.to_string()),
        kind: Some(cell.kind.to_string()),
        model: cell.launch.model.clone(),
        effort: cell.launch.effort.clone(),
        mode: cell.launch.mode.map(|mode| mode.to_string()),
        system_prompt_file: cell.system_prompt_file.clone(),
        append_system_prompt_files: cell.append_system_prompt_files.clone(),
    }
}

fn unresolved_roles(team: &Team, profiles: &ProfilesConfig) -> Vec<RoleReport> {
    team.roles
        .iter()
        .map(|binding| {
            let resolved = rimz::harness::spec::resolve_profile(&binding.profile, profiles).ok();
            RoleReport {
                role: binding.role.clone(),
                profile: binding.profile.clone(),
                kind: resolved.as_ref().map(|profile| profile.kind.to_string()),
                model: binding.model.clone().or_else(|| {
                    resolved
                        .as_ref()
                        .and_then(|profile| profile.launch.model.clone())
                }),
                effort: binding.effort.clone().or_else(|| {
                    resolved
                        .as_ref()
                        .and_then(|profile| profile.launch.effort.clone())
                }),
                mode: binding
                    .mode
                    .or_else(|| resolved.as_ref().and_then(|profile| profile.launch.mode))
                    .map(|mode| mode.to_string()),
                system_prompt_file: binding.system_prompt_file.clone().or_else(|| {
                    resolved
                        .as_ref()
                        .and_then(|profile| profile.system_prompt_file.clone())
                }),
                append_system_prompt_files: resolved
                    .as_ref()
                    .map(|profile| profile.append_system_prompt_files.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .chain(binding.append_system_prompt_files.iter().cloned())
                    .collect(),
            }
        })
        .collect()
}

fn cell_label(cell: &AgentCell) -> String {
    cell.launch
        .role
        .as_deref()
        .or(cell.launch.profile.as_deref())
        .unwrap_or(cell.kind.as_str())
        .to_owned()
}

fn live_instances(
    agents: &[AgentState],
    audit_agents: &[AgentState],
    prices: &rimz::agents::PriceBook,
    worktree: Option<&str>,
) -> BTreeMap<String, Vec<LiveInstance>> {
    let cohorts = rimz::harness::target::team_cohorts(agents)
        .into_iter()
        .filter(|cohort| {
            worktree.is_none_or(|worktree| super::cohort::matches_worktree(cohort, worktree))
        })
        .collect::<Vec<_>>();
    let live_ids = cohorts
        .iter()
        .flat_map(|cohort| cohort.members.iter().map(|agent| agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    let audit_refs = audit_agents.iter().collect::<Vec<_>>();
    let mut effort_by_session = BTreeMap::new();
    let mut memo = rimz::agents::spending::EffortParseMemo::default();
    for records in rimz::agents::attribution::slot_groups(&audit_refs) {
        if !records
            .iter()
            .any(|agent| live_ids.contains(&agent.agent_id))
        {
            continue;
        }
        let effort = rimz::agents::spending::slot_effort_with_memo(
            &records
                .iter()
                .map(|agent| rimz::agents::spending::EffortSessionRef::from_state(agent))
                .collect::<Vec<_>>(),
            prices,
            &mut memo,
        );
        for record in records {
            effort_by_session.insert(record.agent_id.clone(), effort);
        }
    }
    let mut by_team: BTreeMap<String, Vec<LiveInstance>> = BTreeMap::new();
    for cohort in cohorts {
        let members = cohort.members;
        let mut status_counts = BTreeMap::new();
        for agent in &members {
            *status_counts
                .entry(agent.effective_status().as_str().to_owned())
                .or_default() += 1;
        }
        let state = instance_state(&status_counts).to_owned();
        let members = members
            .iter()
            .map(|agent| LiveMember {
                handle: rimz::harness::target::agent_handle(agent, &members, false),
                kind: agent.kind.to_string(),
                status: agent.effective_status().as_str().to_owned(),
                context_fill_pct: agent.context_fill_pct(),
                cost_usd: effort_by_session
                    .get(&agent.agent_id)
                    .and_then(|effort| effort.cost_usd),
            })
            .collect();
        by_team
            .entry(cohort.team.to_owned())
            .or_default()
            .push(LiveInstance {
                channel: cohort.channel,
                state,
                status_counts,
                members,
            });
    }
    by_team
}

fn instance_state(counts: &BTreeMap<String, usize>) -> &'static str {
    if counts.contains_key("waiting") || counts.contains_key("failed") {
        "blocked"
    } else if counts.contains_key("paused") {
        "paused"
    } else if counts.contains_key("running") {
        "working"
    } else if counts.contains_key("success") {
        "done"
    } else {
        "idle"
    }
}

fn write_catalog(w: &mut impl Write, reports: &[TeamReport]) -> Result<()> {
    if reports.is_empty() {
        writeln!(w, "No teams defined.")?;
        writeln!(w, "Install forge with: rimz teams install forge")?;
        writeln!(w, "Guide: docs/guide/teams.md")?;
        return Ok(());
    }
    let mut table = render::Table::new(["TEAM", "ROLES", "LIVE", "STATUS"])
        .max_width(render::terminal_columns(120));
    for report in reports {
        let roles = if report.defined {
            roles_summary(&report.roles)
        } else {
            "<not defined>".to_owned()
        };
        let live = if report.instances.is_empty() {
            "-".to_owned()
        } else {
            report
                .instances
                .iter()
                .map(|instance| format!("#{} ×{}", instance.channel, instance.members.len()))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let status = report.error.as_deref().map_or_else(
            || {
                if report.instances.is_empty() {
                    "ready".to_owned()
                } else {
                    report
                        .instances
                        .iter()
                        .map(|instance| format!("#{} {}", instance.channel, instance.state))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            },
            |error| format!("broken: {error}"),
        );
        table.row([
            render::cell(&report.name).fg(render::palette::accent()),
            render::cell(roles).dash(),
            render::cell(live).dash(),
            render::cell(status).fg(if report.error.is_some() {
                render::palette::alarm()
            } else {
                render::palette::muted()
            }),
        ]);
    }
    table.render(w)?;
    if reports
        .iter()
        .all(|report| report.source.as_deref() == Some("built-in"))
    {
        writeln!(w)?;
        writeln!(
            w,
            "No installed teams. Install forge with: rimz teams install forge"
        )?;
        writeln!(w, "Guide: docs/guide/teams.md")?;
    }
    Ok(())
}

pub(super) fn roles_summary(roles: &[RoleReport]) -> String {
    roles
        .iter()
        .map(|role| {
            let model = role
                .model
                .as_deref()
                .or(role.kind.as_deref())
                .unwrap_or(role.profile.as_str());
            match role.effort.as_deref() {
                Some(effort) => format!("{}:{model}@{effort}", role.role),
                None => format!("{}:{model}", role.role),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn team_source(project_root: &Path, name: &str) -> Option<String> {
    let config_root = rimz::store::paths::config_home();
    let repo = project_root.join(".rimz/config.toml");
    if rimz::trust::status_with_roots(project_root, &config_root)
        .is_ok_and(|report| report.state == rimz::trust::TrustState::Trusted)
        && file_defines_team(&repo, name)
    {
        return Some(repo.display().to_string());
    }
    let machine = rimz::config::MachineConfig::agents_path();
    if file_defines_team(&machine, name) {
        return Some(machine.display().to_string());
    }
    let fragment = rimz::store::paths::agents_home()
        .join("teams")
        .join(name)
        .join("team.toml");
    if file_defines_team(&fragment, name) {
        return Some(fragment.display().to_string());
    }
    (name == "peer").then(|| "built-in".to_owned())
}

fn file_defines_team(path: &Path, name: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|value| value.get("agents")?.get("teams")?.get(name).cloned())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;
    use rimz::config::RoleBinding;

    fn team() -> Team {
        Team {
            roles: vec![RoleBinding {
                role: "planner".to_owned(),
                profile: "claude".to_owned(),
                mode: None,
                model: Some("fable".to_owned()),
                effort: Some("high".to_owned()),
                budget: None,
                system_prompt_file: Some("planner.md".into()),
                append_system_prompt_files: Vec::new(),
                args: None,
            }],
            leader: Some("planner".to_owned()),
            layout: None,
            scratch_files: Vec::new(),
        }
    }

    #[test]
    fn catalog_merges_definition_and_live_instance() {
        let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), team())]));
        let mut agent = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
        agent.team = Some("forge".to_owned());
        agent.role = Some("planner".to_owned());
        agent.channel = Some("feat-x".to_owned());
        let reports = build_catalog(
            &teams,
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &[agent],
            &[],
            &rimz::agents::PriceBook::default(),
            None,
            |_| Some("/tmp/team.toml".to_owned()),
        );

        assert_eq!(reports.len(), 1);
        assert!(reports[0].valid);
        assert_eq!(roles_summary(&reports[0].roles), "planner:fable@high");
        assert_eq!(reports[0].instances[0].channel, "feat-x");
        assert_eq!(reports[0].instances[0].members.len(), 1);
        assert_eq!(reports[0].instances[0].state, "working");
        assert_eq!(
            serde_json::to_value(&reports).unwrap()[0]["instances"][0]["members"][0]["handle"],
            "@planner"
        );
    }

    #[test]
    fn live_member_cost_comes_from_its_audit_slot() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&transcript).unwrap();
        connection
            .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
            .unwrap();
        connection
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                (
                    "message",
                    "sess-planner",
                    r#"{"cost":0.25,"modelID":"gpt","providerID":"openai","time":{"created":1780394400000},"tokens":{"input":10,"output":2,"cache":{"read":3,"write":4}}}"#,
                ),
            )
            .unwrap();
        drop(connection);
        let mut agent = AgentState::stub("opencode", "sess-planner", AgentStatus::Running);
        agent.team = Some("forge".to_owned());
        agent.role = Some("planner".to_owned());
        agent.channel = Some("feat-x".to_owned());
        agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
        let reports = build_catalog(
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &[agent.clone()],
            &[agent],
            &rimz::agents::PriceBook::default(),
            None,
            |_| None,
        );

        assert_eq!(reports[0].instances[0].members[0].cost_usd, Some(0.25));
    }

    #[test]
    fn invalid_team_stays_visible_with_its_error() {
        let mut broken = team();
        broken.roles[0].profile = "missing".to_owned();
        let reports = build_catalog(
            &TeamsConfig(BTreeMap::from([("broken".to_owned(), broken)])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &[],
            &[],
            &rimz::agents::PriceBook::default(),
            None,
            |_| None,
        );

        assert!(!reports[0].valid);
        assert!(
            reports[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("unknown profile"))
        );
    }

    #[test]
    fn live_instance_state_follows_team_attention_priority() {
        assert_eq!(
            instance_state(&BTreeMap::from([
                ("running".to_owned(), 2),
                ("failed".to_owned(), 1),
            ])),
            "blocked"
        );
        assert_eq!(
            instance_state(&BTreeMap::from([
                ("success".to_owned(), 1),
                ("paused".to_owned(), 1),
            ])),
            "paused"
        );
        assert_eq!(
            instance_state(&BTreeMap::from([("success".to_owned(), 2)])),
            "done"
        );
    }

    #[test]
    fn human_catalog_and_empty_state_teach_the_command() {
        let reports = build_catalog(
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            &[],
            &[],
            &rimz::agents::PriceBook::default(),
            None,
            |_| None,
        );
        let mut rendered = Vec::new();
        write_catalog(&mut rendered, &reports).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();
        assert!(rendered.contains("forge"));
        assert!(rendered.contains("planner:fable@high"));

        let mut empty = Vec::new();
        write_catalog(&mut empty, &[]).unwrap();
        let empty = String::from_utf8(empty).unwrap();
        assert!(empty.contains("rimz teams install forge"));
        assert!(empty.contains("docs/guide/teams.md"));

        let mut built_in_only = Vec::new();
        write_catalog(
            &mut built_in_only,
            &[TeamReport {
                name: "peer".to_owned(),
                defined: true,
                source: Some("built-in".to_owned()),
                layout: Some("claude,codex".to_owned()),
                leader: Some("claude".to_owned()),
                roles: Vec::new(),
                valid: true,
                error: None,
                instances: Vec::new(),
            }],
        )
        .unwrap();
        assert!(
            String::from_utf8(built_in_only)
                .unwrap()
                .contains("No installed teams")
        );
    }

    #[test]
    fn catalog_filter_matches_an_exact_lane_or_member_worktree() {
        let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), team())]));
        let mut agent = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
        agent.team = Some("forge".to_owned());
        agent.role = Some("planner".to_owned());
        agent.channel = None;
        agent.worktree_path = Some("/repo-worktrees/feat-x".to_owned());
        let build = |filter| {
            build_catalog(
                &teams,
                &ProfilesConfig::default(),
                &CommandsConfig::default(),
                std::slice::from_ref(&agent),
                &[],
                &rimz::agents::PriceBook::default(),
                filter,
                |_| None,
            )
        };

        assert_eq!(build(Some("feat-x"))[0].instances[0].channel, "feat-x");
        assert_eq!(
            build(Some("/repo-worktrees/feat-x"))[0].instances[0].channel,
            "feat-x"
        );
        assert!(build(Some("other"))[0].instances.is_empty());
    }
}
