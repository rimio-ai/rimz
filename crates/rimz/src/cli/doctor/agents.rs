use rimz::agents::AgentStatus;
use rimz::trust::{self};

use super::super::open_existing_store;
use super::model::{
    AccountRow, Accounts, AgentCounts, AgentRollup, AgentRow, HookRow, HookStatus, PluginProbeRow,
    PluginRow, Probe, Trust,
};

impl From<rimz::agents::BirthLoginErr> for super::model::AccountProblem {
    fn from(error: rimz::agents::BirthLoginErr) -> Self {
        (&error).into()
    }
}

impl From<&rimz::agents::BirthLoginErr> for super::model::AccountProblem {
    fn from(error: &rimz::agents::BirthLoginErr) -> Self {
        use rimz::agents::BirthLoginErr;
        let text = error.to_string();
        let (summary, fix) = match error {
            BirthLoginErr::MachineUnknown {
                kind,
                name,
                configured,
                path,
            } => (
                format!(
                    "unknown {kind} account `{name}` selected by [accounts.use] in {}; configured: {}",
                    path.display(),
                    configured
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!(
                    "run `rimz accounts add {kind} {name}` or `rimz accounts use --global {kind} default`"
                ),
            ),
            BirthLoginErr::MachineUnsupported { kind, name, path } => (
                format!(
                    "[accounts.use] in {} selects {kind} account `{name}`, but {kind} has no named accounts",
                    path.display()
                ),
                format!("run `rimz accounts use --global {kind} default`"),
            ),
            BirthLoginErr::Login(error) => return error.into(),
            BirthLoginErr::MissingHome { kind, name, home }
            | BirthLoginErr::HooksMissing { kind, name, home } => (
                if matches!(error, BirthLoginErr::MissingHome { .. }) {
                    format!(
                        "{kind} account `{name}` home `{}` is not a directory",
                        home.display()
                    )
                } else {
                    format!(
                        "RimZ hooks are missing for {kind} account `{name}` at `{}`",
                        home.display()
                    )
                },
                if name.is_default() {
                    format!("run `rimz hooks install {kind}`")
                } else {
                    format!("run `rimz accounts add {kind} {name}`")
                },
            ),
            BirthLoginErr::HooksUntrusted {
                kind,
                name,
                home,
                hooks,
                fix,
                ..
            } => (
                format!(
                    "RimZ hooks for {kind} account `{name}` at `{}` are untrusted ({hooks})",
                    home.display()
                ),
                fix.to_string(),
            ),
            BirthLoginErr::HooksDisabled {
                kind,
                name,
                reason,
                fix,
            } => (
                format!("{kind} account `{name}` cannot run RimZ hooks: {reason}"),
                fix.to_string(),
            ),
        };
        Self { text, summary, fix }
    }
}

impl From<rimz::agents::login::LoginErr> for super::model::AccountProblem {
    fn from(error: rimz::agents::login::LoginErr) -> Self {
        (&error).into()
    }
}

impl From<&rimz::agents::login::LoginErr> for super::model::AccountProblem {
    fn from(error: &rimz::agents::login::LoginErr) -> Self {
        use rimz::agents::login::LoginErr;
        let text = error.to_string();
        let (summary, fix) = match error {
            LoginErr::Unknown {
                kind,
                name,
                configured,
            } => (
                format!(
                    "unknown {kind} account `{name}`; configured: {}",
                    configured
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!("run `rimz accounts add {kind} {name}`"),
            ),
            LoginErr::Unsupported { kind } => (
                text.clone(),
                format!("run `rimz accounts use --global {kind} default`"),
            ),
        };
        Self { text, summary, fix }
    }
}

impl From<rimz::agents::LoginConfigErr> for super::model::AccountProblem {
    fn from(error: rimz::agents::LoginConfigErr) -> Self {
        (&error).into()
    }
}

impl From<&rimz::agents::LoginConfigErr> for super::model::AccountProblem {
    fn from(error: &rimz::agents::LoginConfigErr) -> Self {
        use rimz::agents::LoginConfigErr;
        let text = error.to_string();
        let (summary, fix) = match error {
            LoginConfigErr::ReservedName { kind } => (
                format!("`accounts.{kind}.default` is reserved for the provider's own home"),
                format!("remove `accounts.{kind}.default` and declare another name"),
            ),
            LoginConfigErr::RelativeHome { kind, name, .. } => (
                text.clone(),
                format!("set `accounts.{kind}.{name}.home` to an absolute path"),
            ),
            LoginConfigErr::AmbiguousHome { kind, name, home } => (
                format!(
                    "`accounts.{kind}.{name}.home` is `{}`, ambiguous in provider home lists",
                    home.display()
                ),
                format!(
                    "choose a directory without a comma or trailing `projects` for `accounts.{kind}.{name}.home`"
                ),
            ),
            LoginConfigErr::DuplicateHome {
                kind,
                name,
                first,
                home,
            } => (
                format!(
                    "`accounts.{kind}.{name}.home` is `{}`, already used by `accounts.{kind}.{first}`",
                    home.display()
                ),
                "give each account its own directory".into(),
            ),
            LoginConfigErr::NativeHome { kind, name, .. } => (
                text.clone(),
                format!("remove `accounts.{kind}.{name}` and use the provider's `default` account"),
            ),
            LoginConfigErr::ExportedHome {
                kind,
                name,
                env_key,
                home,
            } => (
                format!(
                    "`{env_key}` is exported as `{}`, the home of {kind} account `{name}`; select named accounts through RimZ instead of exporting their homes",
                    home.display()
                ),
                format!(
                    "unset `{env_key}` and run `rimz accounts use --global {kind} {name}` for new rooms and rooms following the machine default"
                ),
            ),
        };
        Self { text, summary, fix }
    }
}

impl From<rimz::agents::RoomLoginErr> for super::model::AccountProblem {
    fn from(error: rimz::agents::RoomLoginErr) -> Self {
        let mut error = &error;
        while let rimz::agents::RoomLoginErr::Resolution(inner) = error {
            error = inner;
        }
        match error {
            rimz::agents::RoomLoginErr::Config(error) => error.into(),
            rimz::agents::RoomLoginErr::Login(error) => error.into(),
            rimz::agents::RoomLoginErr::Birth(error) => error.as_ref().into(),
            _ => error.to_string().into(),
        }
    }
}

fn empty_dir_fix(error: &rimz::agents::empty_dir::EmptyDirErr) -> &'static str {
    if matches!(error, rimz::agents::empty_dir::EmptyDirErr::Dirty(_)) {
        "remove unexpected files from the empty root, then re-run `rimz doctor`"
    } else {
        "repair access to the empty root, then re-run `rimz doctor`"
    }
}

pub(super) fn collect_empty_dir(ws: Option<&rimz::ResolvedWorkspace>) -> super::model::EmptyDir {
    use super::model::{EmptyDir, EmptyDirRow};
    use rimz::agents::{LoginCatalog, RoomLoginSet, empty_dir};

    let machine = rimz::config::MachineConfig::load_lenient();
    let effective = ws
        .map(|ws| rimz::config::effective::load(&machine, &ws.project_root))
        .transpose();
    let (profiles, subagent_profiles, profiles_error) = match effective {
        Ok(Some(effective)) => (effective.profiles, effective.subagent_profiles, None),
        result => (
            machine.agents.profiles.clone(),
            machine.subagents.profiles.clone(),
            result.err().map(|error| error.to_string()),
        ),
    };
    let logins = RoomLoginSet::new(
        None,
        Some(LoginCatalog::room_view(&machine.accounts).0),
        rimz::agents::ambient_env(),
    );
    let mut by_kind = std::collections::BTreeMap::<_, Vec<String>>::new();
    for profiles in [&profiles, &subagent_profiles] {
        for name in profiles.0.keys() {
            let Ok(profile) = rimz::harness::spec::resolve_profile(name, profiles) else {
                continue;
            };
            if profile.empty_dir == Some(true) {
                by_kind.entry(profile.kind).or_default().push(name.clone());
            }
        }
    }
    let profiles_error = profiles_error.filter(|_| !by_kind.is_empty());
    let mut rows = Vec::new();
    for (kind, mut profiles) in by_kind {
        profiles.sort();
        profiles.dedup();
        for login in logins.declared(kind.as_str()) {
            let empty_dir::Verdict::Fallback(trust) =
                empty_dir::verdict(true, &kind, &logins.env(&login))
            else {
                continue;
            };
            // resolve_profile rejects unknown terminal providers before grouping by kind.
            let row = rimz::agents::FolderTrustRow {
                kind: rimz::agents::find_definition(kind.as_str())
                    .expect("resolved profile has a known provider")
                    .spec()
                    .kind,
                login: login.name().clone(),
                trust,
            };
            rows.push(EmptyDirRow {
                trust: crate::cli::folder_trust::Row::for_subject(
                    &row,
                    "rimz trust grant --empty-dir",
                ),
                profiles: profiles.clone(),
            });
        }
    }
    let mut check_fix = "repair the workspace state paths, then re-run `rimz doctor`";
    EmptyDir {
        rows,
        check: match ws.map_or_else(
            || empty_dir::check_root().map_err(|error| (error.to_string(), empty_dir_fix(&error))),
            |ws| {
                rimz::StatePaths::for_project_root(&ws.project_root)
                    .map_err(|error| (error.to_string(), check_fix))
                    .and_then(|paths| {
                        empty_dir::check(&paths)
                            .map_err(|error| (error.to_string(), empty_dir_fix(&error)))
                    })
            },
        ) {
            Ok(()) => Probe::Ready(()),
            Err((error, fix)) => {
                check_fix = fix;
                Probe::Unavailable {
                    error,
                    diagnosis: None,
                }
            }
        },
        check_fix,
        profiles_error,
    }
}

/// Walk the snapshot's agent rollup into health counts and problem rows. The
/// default scope is live runtime state; audit widens to durable history and
/// emits every observed row.
pub(super) fn collect_agent_rollup(
    ws: &rimz::ResolvedWorkspace,
    audit: bool,
    accounts: &mut Probe<Accounts>,
) -> AgentRollup {
    let store = match open_existing_store(ws) {
        Ok(Some(store)) => store,
        Ok(None) => return AgentRollup::None,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let mut projection = match store.runtime_projection(rimz::RuntimeScope::Runtime) {
        Ok(projection) => projection,
        Err(err) => {
            return AgentRollup::Unavailable {
                error: err.to_string(),
            };
        }
    };
    let live_keys = projection
        .agents
        .iter()
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    if audit {
        projection = match store.runtime_projection(rimz::RuntimeScope::Audit) {
            Ok(projection) => projection,
            Err(error) => {
                return AgentRollup::Unavailable {
                    error: error.to_string(),
                };
            }
        };
    }
    if projection.agents.is_empty() {
        return AgentRollup::None;
    }
    let snapshot = match rimz::sidebar::consumer::PublishedSnapshotReader::new(
        store.runtime_paths().clone(),
        &ws.session_name,
        None,
    )
    .read(store.paths())
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return AgentRollup::Unavailable {
                error: error.to_string(),
            };
        }
    };
    let mut contexts = rimz::store::agent_context::read_all(store.runtime_paths())
        .into_iter()
        .map(|record| ((record.kind, record.agent_id), record.context))
        .collect::<std::collections::BTreeMap<_, _>>();
    for agent in &mut projection.agents {
        agent.context = contexts.remove(&(agent.kind.clone(), agent.agent_id.clone()));
    }
    let config = rimz::config::MachineConfig::load_lenient();
    let demotion = rimz::harness::park_demotion(
        Some(&store),
        store.paths(),
        store.runtime_paths(),
        &config,
        &projection.agents,
        &projection.resume_outcomes,
        jiff::Timestamp::now(),
    );
    let reader = super::super::current_channel(ws, Some(&store)).address_context();
    project_agent_rollup(
        &projection.agents,
        &snapshot,
        &demotion,
        &reader,
        accounts,
        audit,
        &live_keys,
    )
}

fn project_agent_rollup(
    agents: &[rimz::agents::AgentState],
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    demotion: &rimz::agents::ParkDemotion,
    reader: &rimz::address::AddressContext,
    accounts: &mut Probe<Accounts>,
    audit: bool,
    live_keys: &std::collections::BTreeSet<(rimz::ids::AgentKind, rimz::ids::AgentSessionId)>,
) -> AgentRollup {
    let mut counts = AgentCounts::default();
    let mut agents: Vec<_> = agents.iter().collect();
    agents.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.agent_id.as_str().cmp(right.agent_id.as_str()))
    });
    let rows = agents
        .into_iter()
        .filter_map(|agent| {
            let historical = agent.ended_at.is_some()
                || !live_keys.contains(&(agent.kind.clone(), agent.agent_id.clone()));
            let agent = if historical {
                agent
            } else {
                snapshot
                    .agents
                    .iter()
                    .find(|candidate| {
                        candidate.kind == agent.kind && candidate.agent_id == agent.agent_id
                    })
                    .unwrap_or(agent)
            };
            let row = snapshot
                .rows()
                .find(|row| row.is_agent() && row.id == agent.agent_id.as_str());
            let child = agent.parent_agent_id.as_ref().and_then(|parent| {
                snapshot
                    .rows()
                    .find(|row| row.is_agent() && row.id == parent.as_str())?
                    .sub_agents()
                    .iter()
                    .find(|child| child.id == agent.agent_id.as_str())
            });
            let (status, phase) = rimz::store::snapshot::card_status(agent, row, child, demotion);
            counts.add(status);
            if !audit && !matches!(status, AgentStatus::Failed | AgentStatus::Paused) {
                return None;
            }
            let handle = rimz::address::paste_address(agent, snapshot, reader);
            let account = agent.login_key().to_string();
            let usage = match accounts {
                Probe::Ready(accounts) => accounts
                    .rows
                    .iter_mut()
                    .find(|row| format!("{}@{}", row.kind, row.name) == account)
                    .and_then(|row| row.usage.as_mut()),
                Probe::Unavailable { .. } => None,
            };
            let attributed_to_account = !historical
                && matches!(status, AgentStatus::Failed | AgentStatus::Paused)
                && agent
                    .displayed_turn_error()
                    .is_some_and(|(class, _)| class.is_limit())
                && usage
                    .as_ref()
                    .is_some_and(|usage| usage.standing != super::model::AccountStanding::Ready);
            if attributed_to_account && let Some(usage) = usage {
                usage.agents.push(handle.clone());
            }
            Some(AgentRow {
                kind: agent.kind.as_str().to_owned(),
                agent_id: agent.agent_id.as_str().to_owned(),
                branch: agent.worktree_branch.clone(),
                status,
                phase,
                last_seen: agent.last_seen,
                handle,
                account,
                turn_error: agent.displayed_turn_error_label(),
                attributed_to_account,
                restartable: agent.pane.is_some() && agent.ended_at.is_none(),
                historical,
            })
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
                        diagnosis: None,
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
                    problem: Some(error.into()),
                    usage: None,
                });
            }
            rows.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
            for row in &mut rows {
                if let Ok(name) = row.name.parse() {
                    row.default_for =
                        standing.scopes(&rimz::ids::AgentKind::new_unchecked(&row.kind), &name);
                }
            }
            if let Some(runtime) =
                ws.and_then(|ws| rimz::RuntimePaths::for_project_root(&ws.project_root).ok())
            {
                collect_account_usage(&mut rows, &runtime, &config);
            }
            Probe::Ready(Accounts { rows })
        }
        Err(error) => Probe::Unavailable {
            error,
            diagnosis: None,
        },
    }
}

fn collect_account_usage(
    rows: &mut [AccountRow],
    runtime: &rimz::RuntimePaths,
    config: &rimz::config::MachineConfig,
) {
    use super::model::{AccountStanding, AccountUsage};
    use rimz::agents::account::{ProviderCapacity, cached_usage_read_at};
    use rimz::harness::spend_ladder::{SpendFacts, SpendStep};
    let now = jiff::Timestamp::now();
    for row in rows {
        let Ok(name) = row.name.parse() else { continue };
        let key = rimz::ids::LoginKey {
            kind: rimz::ids::AgentKind::new_unchecked(&row.kind),
            name,
        };
        let read_at = cached_usage_read_at(runtime, &key);
        let Some(read_at) = read_at else { continue };
        let resets_at = ProviderCapacity::read(runtime, &key)
            .and_then(|capacity| capacity.latest_spent_window_reset(now));
        let standing = if resets_at.is_some() {
            AccountStanding::Spent
        } else if SpendFacts::read(runtime, &key, now).step(&config.resume, None, now)
            == SpendStep::Spent
        {
            AccountStanding::Exhausted
        } else {
            AccountStanding::Ready
        };
        row.usage = Some(AccountUsage {
            read_at,
            standing,
            resets_at,
            agents: Vec::new(),
        });
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
            usage: None,
            // A pane born on this account exports its home by design.
            problem: (!in_room(login.kind(), login.name()))
                .then(|| login.check_exported_home(ambient).err())
                .flatten()
                .map(Into::into)
                .or_else(|| login.preflight(ambient).err().map(Into::into)),
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
                    Err(error) => Some(error.into()),
                };
                (name, problem, true)
            }
            Err(error) => (
                room.pin(kind)
                    .or_else(|| machine.get(kind))
                    .cloned()
                    .unwrap_or_default(),
                Some(error.into()),
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
            usage: None,
            problem,
        });
    }
    for (kind, name) in machine {
        if let Err(error) = catalog.select_machine(kind, name) {
            let problem = Some(error.into());
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
                usage: None,
                problem,
            });
        }
    }
    rows.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    rows
}

pub(super) fn collect_hooks() -> Vec<HookRow> {
    let login_env = rimz::agents::ambient_env();
    let cwd = std::env::current_dir().ok();
    let definitions: Vec<_> = rimz::agents::all_definitions().collect();
    rimz::utils::parallel::bounded_map(
        rimz::utils::parallel::WorkLane::Other,
        8,
        &definitions,
        |agent| {
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
            } else if let Some(fix) = agent
                .managed_integration()
                .and_then(|integration| integration.install_blocker(&login_env))
            {
                HookStatus::Blocked { fix }
            } else if agent.hooks_installed(&login_env) {
                let untrusted = agent.untrusted_installed_hooks(&login_env);
                if let Some(conflict) = cwd
                    .as_deref()
                    .and_then(|cwd| agent.hooks_disabled(cwd, &login_env))
                {
                    HookStatus::InstalledDisabled {
                        fix: conflict.to_string(),
                    }
                } else if untrusted.is_empty() {
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
        },
    )
}

pub(super) fn collect_prompt_cache_fallback(
    hooks: &[HookRow],
    harness: &rimz::config::HarnessConfig,
) -> super::model::PromptCacheFallback {
    use rimz::agents::context::SignIn;
    use rimz::config::LifetimeSource;
    use rimz::ids::AgentKind;

    let ttl = harness.fallback_lifetime().map_or_else(
        || "off".to_owned(),
        rimz::utils::time::format_duration_compact,
    );
    let mut kinds: Vec<_> = hooks
        .iter()
        .filter(|row| row.detected)
        .filter(|row| {
            let kind = AgentKind::new_unchecked(&row.kind);
            [None, Some(SignIn::Subscription), Some(SignIn::ApiKey)]
                .into_iter()
                .any(|sign_in| {
                    harness.configured_lifetime(&kind, sign_in).source == LifetimeSource::Fallback
                })
        })
        .map(|row| row.kind.clone())
        .collect();
    kinds.sort_unstable();
    super::model::PromptCacheFallback { ttl, kinds }
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
            diagnosis: err.diagnosis().cloned(),
            error: super::config_file_error_detail(&err, err.diagnosis()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cache_fallback_lists_detected_kinds_using_any_sign_in_mode() {
        let hooks: Vec<_> = ["custom", "codex", "amp", "claude", "gemini"]
            .map(|kind| HookRow {
                kind: kind.into(),
                detected: kind != "gemini",
                status: HookStatus::Installed,
            })
            .into();
        let actual: Vec<_> = [
            ("", "1h", vec!["amp", "custom"]),
            ("[prompt_cache_ttl]\namp = 'off'", "1h", vec!["custom"]),
            (
                "[prompt_cache_ttl]\namp = {subscription = '2h', api-key = '3h'}",
                "1h",
                vec!["amp", "custom"],
            ),
            (
                "[prompt_cache_ttl]\nfallback = 'off'",
                "off",
                vec!["amp", "custom"],
            ),
            (
                "[prompt_cache_ttl]\nfallback = '2h'",
                "2h",
                vec!["amp", "custom"],
            ),
        ]
        .map(|(config, ttl, kinds)| {
            let harness = toml::from_str(config).unwrap();
            let summary = collect_prompt_cache_fallback(&hooks, &harness);
            let empty = collect_prompt_cache_fallback(&[], &harness);
            (
                serde_json::to_value(summary).unwrap()
                    == serde_json::json!({"ttl": ttl, "kinds": kinds}),
                serde_json::to_value(empty).unwrap()
                    == serde_json::json!({"ttl": ttl, "kinds": []}),
            )
        })
        .into();
        assert_eq!(actual, [(true, true); 5]);
    }

    #[test]
    fn account_config_remedies_are_separate_from_the_wire_problem() {
        use rimz::agents::LoginConfigErr;
        let kind = rimz::ids::AgentKind::new_unchecked("codex");
        for (error, remedy) in [
            (
                LoginConfigErr::ReservedName { kind: kind.clone() },
                "remove",
            ),
            (
                LoginConfigErr::AmbiguousHome {
                    kind: kind.clone(),
                    name: "work".parse().unwrap(),
                    home: "/home/one,two".into(),
                },
                "choose",
            ),
            (
                LoginConfigErr::DuplicateHome {
                    kind,
                    name: "work".parse().unwrap(),
                    first: "other".parse().unwrap(),
                    home: "/home/work".into(),
                },
                "give",
            ),
        ] {
            let text = error.to_string();
            let problem = super::super::model::AccountProblem::from(error);
            assert!(!problem.summary.contains(remedy), "{problem:?}");
            assert!(problem.fix.contains(remedy), "{problem:?}");
            assert_eq!(serde_json::to_value(problem).unwrap(), text);
        }
    }

    #[test]
    fn empty_dir_io_errors_do_not_prescribe_removing_files() {
        let error = rimz::agents::empty_dir::EmptyDirErr::Io {
            path: "/empty".into(),
            source: std::io::ErrorKind::PermissionDenied.into(),
        };
        assert!(empty_dir_fix(&error).contains("repair access"));
        assert!(
            empty_dir_fix(&rimz::agents::empty_dir::EmptyDirErr::Dirty(
                "/empty/file".into()
            ))
            .contains("remove unexpected files")
        );
    }

    #[test]
    fn rowless_budget_park_uses_the_published_agent() {
        let now = jiff::Timestamp::now();
        let mut raw = rimz::testkit::agent_state("codex", "session", now);
        raw.status = AgentStatus::Running;
        let mut published = raw.clone();
        published.budget_park = Some(rimz::agents::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 2.0,
            window: rimz::agents::BudgetWindow::Session,
            at: now,
            scope: Default::default(),
            account_kind: None,
            resets_at: None,
            reset_window: None,
        });
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
            vec![published],
            now,
        );
        let rollup = project_agent_rollup(
            &[raw.clone()],
            &snapshot,
            &Default::default(),
            &rimz::address::AddressContext {
                channel: None,
                origin: rimz::address::ChannelOrigin::Root,
            },
            &mut Probe::Ready(Accounts { rows: vec![] }),
            false,
            &[(raw.kind, raw.agent_id)].into(),
        );
        let AgentRollup::Observed { counts, rows } = rollup else {
            panic!("missing rollup")
        };
        assert_eq!(counts.paused, 1);
        assert_eq!(rows[0].status, AgentStatus::Paused);
    }

    #[test]
    fn newer_published_activity_keeps_the_named_handle() {
        let now = jiff::Timestamp::now();
        let mut raw = rimz::testkit::agent_state("codex", "session", now);
        raw.name = Some("otter".into());
        raw.name_explicit = true;
        let mut published = raw.clone();
        published.last_activity = now + jiff::SignedDuration::from_secs(1);
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
            vec![published],
            now,
        );
        let rollup = project_agent_rollup(
            &[raw.clone()],
            &snapshot,
            &Default::default(),
            &rimz::address::AddressContext {
                channel: None,
                origin: rimz::address::ChannelOrigin::Root,
            },
            &mut Probe::Ready(Accounts { rows: vec![] }),
            true,
            &[(raw.kind, raw.agent_id)].into(),
        );
        let AgentRollup::Observed { rows, .. } = rollup else {
            panic!("missing rollup")
        };
        assert_eq!(rows[0].handle, "@otter");
    }

    #[test]
    fn invalid_account_names_do_not_read_default_login_usage() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = rimz::RuntimePaths::under(
            rimz::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        runtime.ensure_dirs().unwrap();
        let cache = rimz::agents::account::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("codex"))
                    .into(),
                rimz::agents::account::RateLimitCacheEntry {
                    limits: rimz::agents::AgentRateLimits {
                        windows: vec![rimz::agents::RateLimitWindow {
                            observed_at: Some(jiff::Timestamp::now()),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        std::fs::write(
            rimz::agents::RateLimitsCache::shared_path(&runtime),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
        let mut rows = vec![AccountRow {
            kind: "codex".into(),
            name: "not@a-login".into(),
            home: None,
            room: false,
            machine_default: false,
            default_for: Default::default(),
            problem: None,
            usage: None,
        }];
        collect_account_usage(&mut rows, &runtime, &rimz::config::MachineConfig::default());
        assert!(
            rows[0].usage.is_none(),
            "invalid login inherited default usage"
        );
    }

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
