use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::super::{Ctx, GlobalFlags, render, report_unknown_config_keys};
use rimz::agents::attribution::LaneLifetimes;
use rimz::agents::{AgentState, AgentStatus, TurnPhase};
use rimz::config::{CommandsConfig, ProfilesConfig, Team, TeamsConfig};
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
        &rimz::disk::paths::config_home(),
    )?;
    let snapshot = ctx.published_snapshot()?;
    let audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let lifetimes = rimz::worktree::lane_lifetimes(audit.agents.iter());
    render::warn_unreadable_lanes(&lifetimes);
    let prices = rimz::agents::pricing::cached_book(&ctx.runtime().shared_pricing_cache_path());
    Ok(build_catalog(
        &effective.teams,
        &effective.profiles,
        &machine.agents.commands,
        LiveCatalog {
            snapshot: &snapshot,
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
    Ok(rimz::config::effective::load(
        &machine.agents,
        &machine.subagents.profiles,
        &workspace.project_root,
        &rimz::disk::paths::config_home(),
    )?
    .teams)
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
    teams: &TeamsConfig,
    catalog: LiveCatalog<'_>,
) -> BTreeMap<String, Vec<LiveInstance>> {
    let snapshot = catalog.snapshot;
    let cohorts = rimz::harness::target::team_cohorts(&snapshot.agents)
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
                    handle: rimz::harness::target::agent_handle(agent, &members, false),
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
    let mut table = render::Table::new(["TEAM", "LANE", "STAGE", "PR", "STATUS"])
        .max_width(render::terminal_columns(120));
    let machine = crate::cli::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    for report in reports {
        for instance in report
            .instances
            .iter()
            .map(Some)
            .chain(report.instances.is_empty().then_some(None))
        {
            let status = report.error.as_deref().map_or_else(
                || {
                    instance
                        .map_or(
                            if report.defined {
                                "ready"
                            } else {
                                "not defined"
                            },
                            |instance| instance.state.as_str(),
                        )
                        .to_owned()
                },
                |error| format!("broken: {error}"),
            );
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
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;
    use rimz::config::RoleBinding;

    fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
            agents,
            jiff::Timestamp::UNIX_EPOCH,
        )
    }

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
                append_system_prompt_files: vec!["consensus.md".into()],
                args: None,
            }],
            leader: Some("planner".to_owned()),
            layout: None,
            scratch_files: Vec::new(),
            stages: Vec::new(),
        }
    }

    #[test]
    fn catalog_projects_cohort_observability_by_worktree() {
        use rimz::store::snapshot::{AgentCard, RowCard, SidebarRow, SidebarWorktreeGroup};

        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(
            first.join("blackboard.md"),
            "# Board\nStage: Implement (delta) (@coder)\n",
        )
        .unwrap();
        std::fs::write(first.join("plan-notes.md"), "one\ntwo\nthree\n").unwrap();
        let mut definition = team();
        definition.stages = vec!["Plan".into(), "Implement".into()];
        definition.scratch_files = vec!["/blackboard.md".into(), "/*-notes.md".into()];
        let teams = TeamsConfig(BTreeMap::from([("forge".into(), definition)]));
        let mut agents = Vec::new();
        let mut groups = Vec::new();
        for (lane, path, number) in [("first", &first, 41), ("second", &second, 42)] {
            let mut agent = AgentState::stub("codex", lane, AgentStatus::Running);
            agent.team = Some("forge".into());
            agent.role = Some("coder".into());
            agent.channel = Some(lane.into());
            agent.worktree_path = Some(path.join(".").to_string_lossy().into_owned());
            agent.worktree_branch = Some(format!("branch-{lane}"));
            agent.last_activity = jiff::Timestamp::UNIX_EPOCH;
            agent.last_seen = jiff::Timestamp::from_second(600).unwrap();
            let mut group: SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
                "key": lane, "label": lane, "kind": "worktree", "status_counts": [], "rows": [],
                "pr_number": number, "pr_ci": "passing"
            }))
            .unwrap();
            group.rows.push(SidebarRow {
                id: agent.agent_id.to_string(),
                name: lane.into(),
                pane: None,
                worktree_path: Some(path.to_string_lossy().into_owned()),
                worktree_branch: agent.worktree_branch.clone(),
                channel: Some(lane.into()),
                unread: false,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: agent.last_activity,
                card: RowCard::Agent(Box::new(AgentCard {
                    description: Some(format!("reading {lane}.rs")),
                    ..AgentCard::default()
                })),
            });
            groups.push(group);
            agents.push(agent);
        }
        let mut snapshot = snapshot(agents);
        snapshot.worktree_groups = groups;
        let build = |snapshot: &SidebarSnapshot| {
            build_catalog(
                &teams,
                &ProfilesConfig::default(),
                &CommandsConfig::default(),
                LiveCatalog {
                    snapshot,
                    audit_agents: &[],
                    lifetimes: &rimz::worktree::lane_lifetimes([]),
                    prices: &rimz::agents::PriceBook::default(),
                    worktree: None,
                },
                |_| None,
            )
        };
        let reports = build(&snapshot);
        let first_report = &reports[0].instances[0];
        assert_eq!(first_report.worktree.as_deref(), Some(first.as_path()));
        assert_eq!(first_report.branch.as_deref(), Some("branch-first"));
        assert_eq!(
            first_report.stage.as_ref().unwrap().name,
            "Implement (delta)"
        );
        assert_eq!(
            first_report.stage.as_ref().unwrap().owner.as_deref(),
            Some("coder")
        );
        assert_eq!(first_report.pr.as_ref().unwrap().number, Some(41));
        assert_eq!(first_report.pr.as_ref().unwrap().state, None);
        assert_eq!(first_report.memory.len(), 2);
        assert_eq!(first_report.memory[1].lines, 3);
        assert!(
            first_report
                .memory
                .iter()
                .all(|file| file.path.is_absolute() && file.modified_at.is_some())
        );
        assert_eq!(
            first_report.members[0].activity.as_deref(),
            Some("reading first.rs")
        );
        assert_eq!(
            first_report.members[0].last_activity_at,
            jiff::Timestamp::UNIX_EPOCH
        );
        assert_eq!(
            reports[0].instances[1].pr.as_ref().unwrap().number,
            Some(42)
        );
        assert!(reports[0].instances[1].stage.is_none());
        assert!(reports[0].instances[1].memory.is_empty());
        let json = serde_json::to_value(&reports).unwrap();
        assert_eq!(
            json[0]["instances"][0]["members"][0]["last_activity_at"],
            "1970-01-01T00:00:00Z"
        );
        assert!(json[0]["instances"][0]["members"][0]["phase"].is_string());
        assert!(json[0]["instances"][1]["stage"].is_null());
        let mut rendered = anstream::StripStream::new(Vec::new());
        write_catalog(&mut rendered, &reports).unwrap();
        insta::assert_snapshot!(
            "human_catalog_has_one_row_per_cohort",
            String::from_utf8(rendered.into_inner()).unwrap()
        );

        let mut conflicting = snapshot.agents[0].clone();
        conflicting.agent_id = "conflicting".into();
        conflicting.worktree_path = Some(second.to_string_lossy().into_owned());
        conflicting.worktree_branch = Some("other".into());
        snapshot.agents.push(conflicting);
        let conflicting = build(&snapshot);
        let instance = &conflicting[0].instances[0];
        assert!(instance.worktree.is_none());
        assert!(instance.branch.is_none());
        assert!(instance.stage.is_none());
        assert!(instance.pr.is_none());
        assert!(instance.memory.is_empty());
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
            LiveCatalog {
                snapshot: &snapshot(vec![agent]),
                audit_agents: &[],
                lifetimes: &rimz::worktree::lane_lifetimes([]),
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
            |_| Some("/tmp/team.toml".to_owned()),
        );

        assert_eq!(reports.len(), 1);
        assert!(reports[0].valid);
        assert_eq!(reports[0].roles[0].model.as_deref(), Some("fable"));
        assert_eq!(reports[0].instances[0].channel, "feat-x");
        assert_eq!(reports[0].instances[0].members.len(), 1);
        assert_eq!(reports[0].instances[0].state, "working");
        let json = serde_json::to_value(&reports).unwrap();
        assert_eq!(json[0]["roles"][0]["system_prompt_file"], "planner.md");
        assert_eq!(
            json[0]["roles"][0]["append_system_prompt_files"],
            serde_json::json!(["consensus.md"])
        );
        assert_eq!(json[0]["instances"][0]["members"][0]["handle"], "@planner");
        assert_eq!(json[0]["instances"][0]["members"][0]["status"], "running");
        assert!(json[0]["instances"][0]["members"][0].get("phase").is_some());
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
        let lifetimes = rimz::worktree::lane_lifetimes(std::iter::once(&agent));
        let reports = build_catalog(
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            LiveCatalog {
                snapshot: &snapshot(vec![agent.clone()]),
                audit_agents: &[agent],
                lifetimes: &lifetimes,
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
            |_| None,
        );

        assert_eq!(reports[0].instances[0].members[0].cost_usd, Some(0.25));
    }

    #[test]
    fn live_member_cost_counts_only_the_current_lane_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("feat-x");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let marker = rimz::worktree::WorktreeMarker {
            version: 4,
            name: "feat-x".to_owned(),
            branch: "feat-x".to_owned(),
            base_branch: Some("main".to_owned()),
            from_pr: None,
            base_ref: "main".to_owned(),
            repo_root: dir.path().to_path_buf(),
            worktree_path: worktree.clone(),
            created_at: "2026-06-02T10:00:00Z".parse().unwrap(),
        };
        rimz::disk::atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker)
            .unwrap();
        let transcript = dir.path().join("opencode.db");
        let connection = rusqlite::Connection::open(&transcript).unwrap();
        connection
            .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
            .unwrap();
        for (session, cost) in [("old-planner", 10.0), ("current-planner", 0.25)] {
            let data = serde_json::json!({
                "cost": cost,
                "modelID": "gpt",
                "providerID": "openai",
                "time": { "created": 1780394400000_i64 },
                "tokens": { "input": 10, "output": 2, "cache": { "read": 3, "write": 4 } }
            });
            connection
                .execute(
                    "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                    (session, session, data.to_string()),
                )
                .unwrap();
        }
        drop(connection);
        let mut current = AgentState::stub("opencode", "current-planner", AgentStatus::Running);
        current.team = Some("forge".to_owned());
        current.role = Some("planner".to_owned());
        current.channel = Some("feat-x".to_owned());
        current.worktree_path = Some(worktree.to_string_lossy().into_owned());
        current.registered_at = Some("2026-06-02T10:00:01Z".parse().unwrap());
        current.transcript_path = Some(transcript.to_string_lossy().into_owned());
        let mut old = AgentState::stub("opencode", "old-planner", AgentStatus::Success);
        old.team = current.team.clone();
        old.role = current.role.clone();
        old.channel = current.channel.clone();
        old.worktree_path = current.worktree_path.clone();
        old.registered_at = Some("2026-06-02T09:59:59Z".parse().unwrap());
        old.transcript_path = current.transcript_path.clone();
        let audit_agents = [old, current.clone()];
        let build = || {
            let lifetimes = rimz::worktree::lane_lifetimes(audit_agents.iter());
            build_catalog(
                &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
                &ProfilesConfig::default(),
                &CommandsConfig::default(),
                LiveCatalog {
                    snapshot: &snapshot(vec![current.clone()]),
                    audit_agents: &audit_agents,
                    lifetimes: &lifetimes,
                    prices: &rimz::agents::PriceBook::default(),
                    worktree: None,
                },
                |_| None,
            )
        };
        let reports = build();
        let instance = &reports[0].instances[0];
        assert_eq!(instance.members.len(), 1);
        assert_eq!(instance.members[0].cost_usd, Some(0.25));

        std::fs::write(git_dir.join("rimz-worktree.json"), "invalid marker").unwrap();
        let unreadable = build();
        let unreadable_instance = &unreadable[0].instances[0];
        assert_eq!(unreadable_instance.members.len(), 1);
        assert_eq!(unreadable_instance.members[0].cost_usd, None);
        assert_eq!(
            unreadable_instance.members[0].handle,
            instance.members[0].handle
        );
        assert_eq!(
            unreadable_instance.members[0].status,
            instance.members[0].status
        );
        assert_eq!(unreadable_instance.channel, instance.channel);
        assert_eq!(unreadable_instance.state, instance.state);
        assert_eq!(unreadable_instance.status_counts, instance.status_counts);

        std::fs::remove_dir_all(&worktree).unwrap();
        let removed = build();
        let removed_instance = &removed[0].instances[0];
        assert_eq!(removed_instance.members.len(), 1);
        assert_eq!(removed_instance.members[0].cost_usd, None);
        assert_eq!(
            removed_instance.members[0].handle,
            instance.members[0].handle
        );
        assert_eq!(
            removed_instance.members[0].status,
            instance.members[0].status
        );
        assert_eq!(removed_instance.channel, instance.channel);
        assert_eq!(removed_instance.state, instance.state);
        assert_eq!(removed_instance.status_counts, instance.status_counts);
        assert!(transcript.exists());
    }

    #[test]
    fn invalid_team_stays_visible_with_its_error() {
        let mut broken = team();
        broken.roles[0].profile = "missing".to_owned();
        let reports = build_catalog(
            &TeamsConfig(BTreeMap::from([("broken".to_owned(), broken)])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            LiveCatalog {
                snapshot: &snapshot(Vec::new()),
                audit_agents: &[],
                lifetimes: &rimz::worktree::lane_lifetimes([]),
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
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
            LiveCatalog {
                snapshot: &snapshot(Vec::new()),
                audit_agents: &[],
                lifetimes: &rimz::worktree::lane_lifetimes([]),
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
            |_| None,
        );
        let mut rendered = Vec::new();
        write_catalog(&mut rendered, &reports).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();
        assert!(rendered.contains("forge"));
        assert!(rendered.contains("STAGE"));
        assert!(rendered.contains("ready"));

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
                LiveCatalog {
                    snapshot: &snapshot(vec![agent.clone()]),
                    audit_agents: &[],
                    lifetimes: &rimz::worktree::lane_lifetimes([]),
                    prices: &rimz::agents::PriceBook::default(),
                    worktree: filter,
                },
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
