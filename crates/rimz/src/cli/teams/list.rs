use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::super::{Ctx, GlobalFlags, render, report_unknown_config_keys};
use rimz::agents::attribution::LaneLifetimes;
use rimz::agents::{AgentState, AgentStatus, TurnPhase};
use rimz::config::{
    CommandsConfig, MachineConfig, ProfilesConfig, TaskEntry, Team, TeamsConfig, ThemeConfig,
};
use rimz::harness::schedule::catalog::{LoadedTask, TaskCatalog, TaskSource};
use rimz::harness::spec::{AgentCell, LayoutSpec};
use rimz::store::snapshot::{SidebarSnapshot, WorktreePrCi, WorktreePrState};
use rimz::utils::path::normalize_path_lexical;
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
    pub signals: Vec<DeclaredSignal>,
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
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub stages: Vec<String>,
    pub stage: Option<StageReport>,
    pub pr: Option<PrReport>,
    pub memory: Vec<MemoryReport>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct StageReport {
    pub name: String,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PrReport {
    pub number: Option<u64>,
    pub state: Option<WorktreePrState>,
    pub ci: Option<WorktreePrCi>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct MemoryReport {
    pub path: PathBuf,
    pub lines: usize,
    pub modified_at: Option<jiff::Timestamp>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LiveMember {
    pub role: Option<String>,
    pub signals: Vec<LiveSignal>,
    pub handle: String,
    pub kind: String,
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub activity: Option<String>,
    pub last_activity_at: jiff::Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_fill_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct DeclaredSignal {
    pub signal: String,
    #[serde(rename = "match")]
    pub matches: BTreeMap<String, String>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LiveSignal {
    pub name: String,
    pub selector: String,
    pub matches: BTreeMap<String, String>,
}

pub(super) fn run(json: bool, globals: &GlobalFlags) -> Result<()> {
    let machine = MachineConfig::load().context("loading machine config")?;
    let reports = load_catalog(globals, None, &machine)?;
    if json {
        return render::json_pretty(&reports);
    }
    write_catalog(&mut render::out(), &reports, &machine.theme)
}

pub(super) fn load_catalog(
    globals: &GlobalFlags,
    worktree: Option<&str>,
    machine: &MachineConfig,
) -> Result<Vec<TeamReport>> {
    let ctx = Ctx::open(globals)?;
    report_unknown_config_keys(machine)?;
    let effective = rimz::config::effective::load(machine, &ctx.workspace.project_root)?;
    let snapshot = ctx.published_snapshot()?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let lifetimes = rimz::worktree::lane_lifetimes(audit.agents.iter());
    render::warn_unreadable_lanes(&lifetimes);
    let prices = rimz::agents::pricing::cached_book(&ctx.runtime().shared_pricing_cache_path());
    let tasks = TaskCatalog::load(Some(&ctx.workspace.project_root))?;
    Ok(build_catalog(
        &effective.teams,
        &effective.profiles,
        &machine.agents.commands,
        LiveCatalog {
            snapshot: &snapshot,
            tasks: tasks.visible(),
            audit_agents: &audit.agents,
            lifetimes: &lifetimes,
            prices: &prices,
            worktree,
        },
        |name| team_source(&ctx.workspace.project_root, name),
    ))
}

pub(super) fn effective_teams(globals: &GlobalFlags) -> Result<TeamsConfig> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine = rimz::config::MachineConfig::load().context("loading machine config")?;
    report_unknown_config_keys(&machine)?;
    Ok(rimz::config::effective::load(&machine, &workspace.project_root)?.teams)
}

fn build_catalog(
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    live_catalog: LiveCatalog<'_>,
    source: impl Fn(&str) -> Option<String>,
) -> Vec<TeamReport> {
    let live = live_instances(teams, live_catalog);
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

struct LiveCatalog<'a> {
    snapshot: &'a SidebarSnapshot,
    tasks: &'a BTreeMap<String, LoadedTask>,
    audit_agents: &'a [AgentState],
    lifetimes: &'a LaneLifetimes,
    prices: &'a rimz::agents::PriceBook,
    worktree: Option<&'a str>,
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
    let mut roles = layout
        .as_ref()
        .map(|layout| resolved_roles(team, layout))
        .unwrap_or_else(|| unresolved_roles(team, profiles));
    for role in &mut roles {
        role.signals = team
            .signals
            .iter()
            .filter(|binding| binding.role == role.role)
            .map(|binding| DeclaredSignal {
                signal: binding.signal.clone(),
                matches: binding.matches.clone(),
                prompt: binding.prompt.clone(),
            })
            .collect();
    }
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
        signals: Vec::new(),
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
                signals: Vec::new(),
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
    teams: &TeamsConfig,
    catalog: LiveCatalog<'_>,
) -> BTreeMap<String, Vec<LiveInstance>> {
    let snapshot = catalog.snapshot;
    let cohorts = rimz::address::team_cohorts(&snapshot.agents)
        .into_iter()
        .filter(|cohort| {
            catalog
                .worktree
                .is_none_or(|worktree| super::cohort::matches_worktree(cohort, worktree))
        })
        .collect::<Vec<_>>();
    let live_ids = cohorts
        .iter()
        .flat_map(|cohort| cohort.members.iter().map(|agent| agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    let audit_refs = catalog.audit_agents.iter().collect::<Vec<_>>();
    let mut effort_by_session = BTreeMap::new();
    let mut memo = rimz::agents::spending::EffortParseMemo::default();
    for records in rimz::agents::attribution::slot_groups(&audit_refs, catalog.lifetimes) {
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
            catalog.prices,
            &mut memo,
        );
        for record in records {
            effort_by_session.insert(record.agent_id.clone(), effort);
        }
    }
    let mut by_team: BTreeMap<String, Vec<LiveInstance>> = BTreeMap::new();
    for cohort in cohorts {
        let instance = format!("{}#{}", cohort.team, cohort.channel);
        let members = cohort.members;
        let team = teams.0.get(cohort.team);
        let worktree = unique_value(members.iter().map(|agent| {
            agent
                .worktree_path
                .as_deref()
                .map(|path| normalize_path_lexical(Path::new(path)))
        }));
        let branch = unique_value(members.iter().map(|agent| agent.worktree_branch.clone()));
        let stage = worktree
            .as_deref()
            .and_then(rimz::harness::scratch::board_stage)
            .map(|stage| StageReport {
                name: stage.name,
                owner: stage.owner,
            });
        let memory = worktree
            .as_deref()
            .zip(team)
            .map(|(root, team)| {
                rimz::harness::scratch::scan(root, &team.scratch_files)
                    .files
                    .into_iter()
                    .map(|file| MemoryReport {
                        path: file.path,
                        lines: file.lines,
                        modified_at: file
                            .modified
                            .and_then(|time| jiff::Timestamp::try_from(time).ok()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let pr = worktree.as_deref().and_then(|root| {
            snapshot
                .worktree_groups
                .iter()
                .filter(|group| {
                    group.rows.iter().any(|row| {
                        row.is_agent()
                            && row
                                .worktree_path
                                .as_deref()
                                .is_some_and(|path| normalize_path_lexical(Path::new(path)) == root)
                    })
                })
                .max_by_key(|group| {
                    group.rows.iter().any(|row| {
                        row.is_agent()
                            && members
                                .iter()
                                .any(|agent| row.id == agent.agent_id.as_str())
                    })
                })
                .filter(|group| {
                    group.pr_number.is_some()
                        || group.pr_state.is_some()
                        || group.pr_ci.is_some()
                        || group.pr_url.is_some()
                })
                .map(|group| PrReport {
                    number: group.pr_number,
                    state: group.pr_state,
                    ci: group.pr_ci,
                    url: group.pr_url.clone(),
                })
        });
        let mut status_counts = BTreeMap::new();
        for agent in &members {
            *status_counts
                .entry(agent.effective_status().as_str().to_owned())
                .or_default() += 1;
        }
        let state = instance_state(&status_counts).to_owned();
        let members = members
            .iter()
            .map(|agent| {
                let status = agent.effective_status();
                let card = snapshot
                    .rows()
                    .find(|row| row.is_agent() && row.id == agent.agent_id.as_str())
                    .and_then(|row| row.as_agent());
                LiveMember {
                    role: agent.role.clone(),
                    signals: catalog
                        .tasks
                        .iter()
                        .filter_map(|(name, task)| {
                            live_signal(name, task.entry(), task.source(), &instance, agent)
                        })
                        .collect(),
                    handle: rimz::address::agent_handle(agent, &members, false),
                    kind: agent.kind.to_string(),
                    status,
                    phase: if status == AgentStatus::Running {
                        agent.phase
                    } else {
                        TurnPhase::Idle
                    },
                    context_fill_pct: agent.context_fill_pct(),
                    activity: render::agent_activity_line(agent, card),
                    last_activity_at: agent.last_activity,
                    cost_usd: effort_by_session
                        .get(&agent.agent_id)
                        .and_then(|effort| effort.cost_usd),
                }
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
                worktree,
                branch,
                stages: team.map(|team| team.stages.clone()).unwrap_or_default(),
                stage,
                pr,
                memory,
            });
    }
    by_team
}

fn unique_value<T: Eq>(mut values: impl Iterator<Item = Option<T>>) -> Option<T> {
    let first = values.next()??;
    values
        .all(|value| value.as_ref() == Some(&first))
        .then_some(first)
}

fn live_signal(
    name: &str,
    entry: &TaskEntry,
    source: TaskSource,
    instance: &str,
    agent: &AgentState,
) -> Option<LiveSignal> {
    let target = entry.wake.as_ref()?;
    if source != TaskSource::Instance
        || entry
            .team
            .as_ref()
            .is_none_or(|team| team.to_string() != instance)
        || target.kind != agent.kind
        || target.session != agent.agent_id
    {
        return None;
    }
    Some(LiveSignal {
        name: name.to_owned(),
        selector: entry.signal.clone()?,
        matches: entry.matches.clone().unwrap_or_default(),
    })
}

fn instance_state(counts: &BTreeMap<String, usize>) -> &'static str {
    if counts.contains_key("waiting") || counts.contains_key("failed") {
        "blocked"
    } else if counts.contains_key("paused") {
        "paused"
    } else if counts.contains_key("running") {
        "working"
    } else if counts.contains_key("sleeping") {
        "sleeping"
    } else if counts.contains_key("success") {
        "done"
    } else {
        "idle"
    }
}

fn write_catalog(w: &mut impl Write, reports: &[TeamReport], theme: &ThemeConfig) -> Result<()> {
    if reports.is_empty() {
        writeln!(w, "No teams defined.")?;
        writeln!(w, "Install forge with: rimz teams install forge")?;
        writeln!(w, "Guide: docs/guide/teams.md")?;
        return Ok(());
    }
    let mut table = render::Table::new(["TEAM", "LANE", "STAGE", "PR", "STATUS"])
        .max_width(render::terminal_columns(120));
    let glyph = rimz::theme::theme_glyphs(theme);
    for report in reports {
        for instance in report
            .instances
            .iter()
            .map(Some)
            .chain(report.instances.is_empty().then_some(None))
        {
            let state = instance.map_or("ready", |instance| instance.state.as_str());
            let status = if let Some(error) = &report.error {
                format!("broken: {error}")
            } else if !report.defined {
                format!("{state} · not defined")
            } else {
                state.to_owned()
            };
            let pr = instance.and_then(|instance| instance.pr.as_ref());
            let mut pr_text = pr
                .and_then(|pr| pr.number)
                .map(|number| format!("#{number}"))
                .unwrap_or_default();
            let mut pr_style = render::palette::accent();
            if let Some(ci) = pr
                .filter(|pr| pr.state != Some(WorktreePrState::Closed))
                .and_then(|pr| pr.ci)
            {
                let role = match ci {
                    WorktreePrCi::Passing => rimz::config::GlyphRole::WorktreeCiPassing,
                    WorktreePrCi::Pending => rimz::config::GlyphRole::WorktreeCiPending,
                    WorktreePrCi::Failing => rimz::config::GlyphRole::WorktreeCiFailing,
                };
                if !pr_text.is_empty() {
                    pr_text.push(' ');
                }
                pr_text.push_str(&glyph(role));
                pr_style = ci_style(ci);
            }
            table.row([
                render::cell(&report.name).fg(render::palette::accent()),
                render::cell(
                    instance
                        .map(|instance| format!("#{}", instance.channel))
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
                render::cell(
                    instance
                        .and_then(|instance| instance.stage.as_ref())
                        .map(stage_label)
                        .unwrap_or_else(|| "-".to_owned()),
                )
                .dash(),
                render::cell(if pr_text.is_empty() {
                    "-".to_owned()
                } else {
                    pr_text
                })
                .fg(pr_style)
                .dash(),
                render::cell(status).fg(if report.error.is_some() {
                    render::palette::alarm()
                } else {
                    render::palette::muted()
                }),
            ]);
        }
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

pub(super) fn stage_label(stage: &StageReport) -> String {
    stage.owner.as_ref().map_or_else(
        || stage.name.clone(),
        |owner| format!("{} (@{owner})", stage.name),
    )
}

pub(super) fn ci_style(ci: WorktreePrCi) -> anstyle::Style {
    match ci {
        WorktreePrCi::Passing => render::palette::good(),
        WorktreePrCi::Pending => render::palette::warn(),
        WorktreePrCi::Failing => render::palette::alarm(),
    }
}

fn team_source(project_root: &Path, name: &str) -> Option<String> {
    let config_root = rimz::disk::paths::config_home();
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
    let fragment = rimz::disk::paths::agents_home()
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
mod tests;
