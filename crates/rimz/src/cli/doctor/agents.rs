use rimz::agents::AgentStatus;
use rimz::trust::{self};

use super::super::open_existing_store;
use super::model::{
    AccountRow, Accounts, AgentCounts, AgentRollup, AgentRow, HookRow, HookStatus, PluginProbeRow,
    PluginRow, Probe, Trust,
};

/// Walk the snapshot's agent rollup into health counts and problem rows. The
/// default scope is live runtime state; audit widens to durable history and
/// emits every observed row.
pub(super) fn collect_agent_rollup(ws: &rimz::ResolvedWorkspace, audit: bool) -> AgentRollup {
    let store = match open_existing_store(ws) {
        Ok(Some(store)) => store,
        Ok(None) => return AgentRollup::None,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let scope = if audit {
        rimz::RuntimeScope::Audit
    } else {
        rimz::RuntimeScope::Runtime
    };
    let projection = match store.runtime_projection(scope) {
        Ok(projection) => projection,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    if projection.agents.is_empty() {
        return AgentRollup::None;
    }
    let mut counts = AgentCounts::default();
    for agent in &projection.agents {
        counts.add(agent.status);
    }
    let mut agents: Vec<_> = projection.agents.iter().collect();
    agents.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.agent_id.as_str().cmp(right.agent_id.as_str()))
    });
    let rows = agents
        .into_iter()
        .filter(|agent| audit || matches!(agent.status, AgentStatus::Failed | AgentStatus::Paused))
        .map(|agent| AgentRow {
            kind: agent.kind.as_str().to_owned(),
            agent_id: agent.agent_id.as_str().to_owned(),
            branch: agent.worktree_branch.clone(),
            status: agent.status,
            phase: agent.phase,
            last_seen: agent.last_seen,
        })
        .collect();
    AgentRollup::Observed { counts, rows }
}

/// Each adapter's RimZ-hook wiring state. A run in a RimZ room registers nothing
/// until the agent's own hook system invokes `rimz hooks feed`, so this
/// distinguishes installed, present-but-unwired, absent, and
/// known-but-not-installable adapters.
pub(super) fn collect_accounts(ws: Option<&rimz::ResolvedWorkspace>) -> Probe<Accounts> {
    let config = rimz::config::MachineConfig::load_lenient();
    let (catalog, errors) = rimz::agents::LoginCatalog::room_view(&config.accounts);
    match ws.map(|ws| room_logins(ws, &config)).transpose() {
        Ok(room) => {
            let standing = match ws {
                Some(ws) => rimz::room::AccountStanding::at(&ws.project_root, &config),
                None => Ok(rimz::room::AccountStanding::machine_only(&config)),
            };
            let standing = match standing {
                Ok(standing) => standing,
                Err(error) => {
                    return Probe::Unavailable {
                        error: format!("{error:#}"),
                    };
                }
            };
            let mut rows = account_rows(
                &catalog,
                room.as_ref(),
                &rimz::agents::ambient_env(),
                &config.accounts.use_accounts,
            );
            for (kind, error) in errors {
                rows.retain(|row| row.kind != kind.as_str() || row.name != "default");
                rows.push(AccountRow {
                    kind: kind.to_string(),
                    name: "default".to_owned(),
                    home: None,
                    room: false,
                    machine_default: false,
                    default_for: rimz::room::Scopes::default(),
                    problem: Some(error.to_string()),
                });
            }
            rows.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
            for row in &mut rows {
                if let Ok(name) = row.name.parse() {
                    row.default_for =
                        standing.scopes(&rimz::ids::AgentKind::new_unchecked(&row.kind), &name);
                }
            }
            Probe::Ready(Accounts { rows })
        }
        Err(error) => Probe::Unavailable { error },
    }
}

fn room_logins(
    ws: &rimz::ResolvedWorkspace,
    machine: &rimz::config::MachineConfig,
) -> Result<rimz::agents::RoomAccounts, String> {
    let paths =
        rimz::StatePaths::for_project_root(&ws.project_root).map_err(|err| err.to_string())?;
    rimz::agents::room_accounts(&paths.workspace_record, Some(&ws.project_root), machine)
        .map_err(|err| err.to_string())
}

/// Every named account with its launch verdict, plus the room's `default`
/// selections and any selection naming an account no longer declared.
fn account_rows(
    catalog: &rimz::agents::LoginCatalog,
    room: Option<&rimz::agents::RoomAccounts>,
    ambient: &std::collections::BTreeMap<String, String>,
    machine: &rimz::ids::RoomLogins,
) -> Vec<AccountRow> {
    let in_room = |kind: &rimz::ids::AgentKind, name: &rimz::ids::LoginName| {
        room.and_then(|room| room.name(kind).ok()).as_ref() == Some(name)
    };
    let mut rows: Vec<AccountRow> = catalog
        .all()
        .filter(|login| !login.is_default())
        .map(|login| AccountRow {
            kind: login.kind().to_string(),
            name: login.name().to_string(),
            home: login.home().map(|home| home.display().to_string()),
            room: in_room(login.kind(), login.name()),
            machine_default: machine.get(login.kind()) == Some(login.name()),
            default_for: rimz::room::Scopes::default(),
            // A pane born on this account exports its home by design.
            problem: (!in_room(login.kind(), login.name()))
                .then(|| login.check_exported_home(ambient).err())
                .flatten()
                .map(|err| err.to_string())
                .or_else(|| login.preflight(ambient).err().map(|err| err.to_string())),
        })
        .collect();
    for (room, kind) in room
        .into_iter()
        .flat_map(|room| room.kinds().map(move |kind| (room, kind)))
    {
        let (name, problem, resolved) = match room.name(kind) {
            Ok(name) => {
                let problem = match catalog.select(kind, &name) {
                    Ok(_) if !name.is_default() => continue,
                    Ok(_) => None,
                    Err(error) => Some(error.to_string()),
                };
                (name, problem, true)
            }
            Err(error) => (
                room.pin(kind)
                    .or_else(|| machine.get(kind))
                    .cloned()
                    .unwrap_or_default(),
                Some(error.to_string()),
                false,
            ),
        };
        if let Some(row) = rows
            .iter_mut()
            .find(|row| row.kind == kind.as_str() && row.name == name.as_str())
        {
            row.problem = problem;
            row.room = resolved;
            continue;
        }
        rows.push(AccountRow {
            kind: kind.to_string(),
            name: name.to_string(),
            home: None,
            room: resolved,
            machine_default: !name.is_default() && machine.get(kind) == Some(&name),
            default_for: rimz::room::Scopes::default(),
            problem,
        });
    }
    for (kind, name) in machine {
        if let Err(error) = catalog.select_machine(kind, name) {
            let problem = Some(error.to_string());
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.kind == kind.as_str() && row.name == name.as_str())
            {
                row.problem = problem;
                continue;
            }
            rows.push(AccountRow {
                kind: kind.to_string(),
                name: name.to_string(),
                home: None,
                room: false,
                machine_default: true,
                default_for: rimz::room::Scopes::default(),
                problem,
            });
        }
    }
    rows.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    rows
}

pub(super) fn collect_hooks() -> Vec<HookRow> {
    let login_env = rimz::agents::ambient_env();
    rimz::agents::all_definitions()
        .map(|agent| {
            let definition = agent.spec();
            let name = definition.kind;
            let detected = rimz::agents::locate_binary(definition).is_some();
            let status = if !definition.has_wired_hook_install() {
                HookStatus::Unsupported {
                    reason: definition
                        .hook_install_failure_detail()
                        .unwrap_or("hook install is not supported for this adapter")
                        .to_owned(),
                }
            } else if agent.hooks_installed(&login_env) {
                let untrusted = agent.untrusted_installed_hooks(&login_env);
                if untrusted.is_empty() {
                    HookStatus::Installed
                } else {
                    HookStatus::InstalledUntrusted {
                        events: untrusted,
                        fix: rimz::agents::hook_trust_fix(name),
                    }
                }
            } else if detected {
                HookStatus::NotInstalled {
                    fix: format!("run `rimz hooks install {name}` to wire {name} agents"),
                }
            } else {
                HookStatus::NotDetected
            };
            HookRow {
                kind: name.to_owned(),
                detected,
                status,
            }
        })
        .collect()
}

pub(super) fn collect_plugins() -> Vec<PluginRow> {
    rimz::agents::plugins::loaded()
        .diagnostics
        .iter()
        .map(|plugin| PluginRow {
            kind: plugin.kind.clone(),
            manifest: plugin.path.display().to_string(),
            valid: plugin.valid,
            error: plugin.error.clone(),
            setup_doc: plugin
                .setup_doc
                .as_ref()
                .map(|path| path.display().to_string()),
            probes: plugin
                .probes
                .iter()
                .map(|probe| PluginProbeRow {
                    name: probe.name,
                    command: probe.command.clone(),
                    present: probe.present,
                    executable: probe.executable,
                })
                .collect(),
        })
        .collect()
}

/// Project-trust state. `Stale` is the case worth seeing: the executable surface
/// drifted since the last grant, so command-running fields are inert until
/// `rimz trust grant` runs again.
pub(super) fn collect_trust(ws: &rimz::ResolvedWorkspace) -> Probe<Trust> {
    match trust::status(&ws.project_root) {
        Ok(report) => Probe::Ready(Trust {
            state: report.state,
            granted_at: report.granted_at.map(|at| at.to_string()),
        }),
        Err(err) => Probe::Unavailable {
            error: super::config_file_error_detail(&err, err.diagnosis()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_rows_mark_machine_defaults_and_report_dangling_selections() {
        let config: rimz::config::AccountsConfig =
            toml::from_str("[codex.work]\n[use]\ncodex = \"work\"\nclaude = \"missing\"\n")
                .unwrap();
        let catalog = rimz::agents::LoginCatalog::from_config(&config).unwrap();
        let rows = account_rows(
            &catalog,
            None,
            &std::collections::BTreeMap::new(),
            &config.use_accounts,
        );
        let rows = serde_json::to_value(rows).unwrap();
        let rows = rows.as_array().unwrap();
        let work = rows.iter().find(|row| row["name"] == "work").unwrap();
        assert_eq!(work["machine_default"], true);
        let missing = rows
            .iter()
            .find(|row| row["name"] == "missing")
            .expect("dangling machine selection");
        assert_eq!(missing["machine_default"], true);
        assert!(
            missing["problem"]
                .as_str()
                .unwrap()
                .contains("rimz accounts use --global claude default")
        );
    }

    #[test]
    fn account_rows_flag_an_exported_home_outside_the_room_born_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("rimio");
        std::fs::create_dir(&home).unwrap();
        let config: rimz::config::AccountsConfig = toml::from_str(&format!(
            "[codex.rimio]\nhome = {:?}\n",
            home.display().to_string()
        ))
        .unwrap();
        let catalog = rimz::agents::LoginCatalog::from_config(&config).unwrap();
        let ambient = std::collections::BTreeMap::from([
            ("HOME".to_owned(), "/home/u".to_owned()),
            ("CODEX_HOME".to_owned(), home.display().to_string()),
        ]);
        let problem = |room: Option<&rimz::agents::RoomAccounts>| {
            let rows = account_rows(&catalog, room, &ambient, &config.use_accounts);
            let rows = serde_json::to_value(rows).unwrap();
            rows[0]["problem"].as_str().map(str::to_owned)
        };

        let outside = problem(None).expect("an exported account home is a problem");
        assert!(outside.contains("unset `CODEX_HOME`"), "{outside}");
        let room: rimz::agents::RoomAccounts = rimz::ids::RoomLogins::from([(
            rimz::ids::AgentKind::new_unchecked("codex"),
            "rimio".parse().unwrap(),
        )])
        .into();
        let inside = problem(Some(&room)).unwrap_or_default();
        assert!(!inside.contains("CODEX_HOME"), "{inside}");
    }
}
