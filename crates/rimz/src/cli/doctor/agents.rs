use rimz::agents::AgentStatus;
use rimz::trust::{self};

use super::super::open_existing_store;
use super::model::{
    AccountRow, Accounts, AgentCounts, AgentRollup, AgentRow, CompactionLowered, HookRow,
    HookStatus, ModSignal, OutdatedIntegration, PluginProbeRow, PluginRow, Probe, Trust,
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
    use super::model::{EmptyDir, EmptyDirCause, EmptyDirRow};
    use rimz::agents::{FolderTrust, LoginCatalog, RoomLoginSet, empty_dir};

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
                empty_dir::verdict(&kind, &logins.env(&login))
            else {
                continue;
            };
            let cause = match &trust {
                FolderTrust::Restricted { .. } => EmptyDirCause::Restricted,
                FolderTrust::Undecided(gap) if gap.grant.is_ok() => EmptyDirCause::WiderKey,
                FolderTrust::Undecided(_) => EmptyDirCause::NotGrantable,
                FolderTrust::Decided => continue,
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
                trust: crate::cli::folder_trust::Row::for_empty_dir(&row),
                profiles: profiles.clone(),
                cause,
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
    let live = LiveSessions {
        keys: projection
            .agents
            .iter()
            .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
            .collect(),
        mod_checked_in: rimz::store::mod_checkin::read_all(store.runtime_paths())
            .into_iter()
            .map(|record| ((record.kind.clone(), record.agent_id.clone()), record))
            .collect(),
        mod_blockers: mod_blockers(&projection.agents),
    };
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
        &live,
    )
}

type SessionKey = (rimz::ids::AgentKind, rimz::ids::AgentSessionId);

/// The managed policy blocking the mod for each login a live session runs on,
/// read once per login.
fn mod_blockers(
    agents: &[rimz::agents::AgentState],
) -> std::collections::BTreeMap<rimz::ids::LoginKey, rimz::agents::ModBlocker> {
    let mut blockers = std::collections::BTreeMap::new();
    let logins: std::collections::BTreeSet<_> = agents
        .iter()
        .filter(|agent| agent.ended_at.is_none())
        .map(|agent| (agent.login_key(), agent.login.clone()))
        .collect();
    for (key, login) in logins {
        let blocker = rimz::agents::find_definition(key.kind.as_str())
            .and_then(|definition| definition.managed_integration())
            .zip(rimz::agents::session_login_env(&key.kind, login.as_ref()).ok())
            .and_then(|(integration, env)| integration.mod_blocker(&env));
        if let Some(blocker) = blocker {
            blockers.insert(key, blocker);
        }
    }
    blockers
}

/// The sessions live in the room, and the RimZ mod's check-in for each it
/// reached.
#[derive(Default)]
struct LiveSessions {
    keys: std::collections::BTreeSet<SessionKey>,
    mod_checked_in:
        std::collections::BTreeMap<SessionKey, rimz::store::mod_checkin::ModCheckinRecord>,
    /// Logins whose provider policy keeps the mod from loading: their
    /// sessions owe no mod reports, and the row shows the policy instead.
    mod_blockers: std::collections::BTreeMap<rimz::ids::LoginKey, rimz::agents::ModBlocker>,
}

impl LiveSessions {
    /// A live, registered Claude session the mod owes reports for, a child
    /// launched through `rimz subagents` included. A provider-native child
    /// runs inside its parent's session, and a provisional row has no session
    /// yet. Only Claude ships a RimZ mod.
    fn owes_mod_reports(&self, agent: &rimz::agents::AgentState) -> bool {
        self.mod_blocked(agent).is_none() && self.runs_the_mod(agent)
    }

    /// The policy that keeps the mod from a session that would otherwise owe
    /// its reports.
    fn mod_blocked(&self, agent: &rimz::agents::AgentState) -> Option<&rimz::agents::ModBlocker> {
        self.mod_blockers
            .get(&agent.login_key())
            .filter(|_| self.runs_the_mod(agent))
    }

    fn runs_the_mod(&self, agent: &rimz::agents::AgentState) -> bool {
        agent.kind.as_str() == "claude"
            && !agent.is_provider_subagent()
            && !agent.agent_id.is_provisional()
            && self
                .keys
                .contains(&(agent.kind.clone(), agent.agent_id.clone()))
    }

    fn check_in(
        &self,
        agent: &rimz::agents::AgentState,
    ) -> Option<&rimz::store::mod_checkin::ModCheckinRecord> {
        self.mod_checked_in
            .get(&(agent.kind.clone(), agent.agent_id.clone()))
    }

    /// A session the mod owes reports for that it never reported from.
    fn mod_missing(&self, agent: &rimz::agents::AgentState) -> bool {
        self.owes_mod_reports(agent) && self.check_in(agent).is_none()
    }

    /// Each signal a checked-in session's mod never sent although the row has
    /// finished a turn since it checked in: every report of a working mod
    /// carries readings, and a finished turn brings a turn report.
    fn mod_signals_missing(&self, agent: &rimz::agents::AgentState) -> Vec<ModSignal> {
        let Some(check_in) = self
            .check_in(agent)
            .filter(|_| self.owes_mod_reports(agent))
        else {
            return Vec::new();
        };
        if agent
            .turn_ended_at
            .is_none_or(|ended| ended <= check_in.checked_in_at())
        {
            return Vec::new();
        }
        [
            (ModSignal::Readings, check_in.readings_at),
            (ModSignal::TurnReport, check_in.turn_report_at),
        ]
        .into_iter()
        .filter(|(_, stamp)| stamp.is_none())
        .map(|(signal, _)| signal)
        .collect()
    }
}

fn project_agent_rollup(
    agents: &[rimz::agents::AgentState],
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    demotion: &rimz::agents::ParkDemotion,
    reader: &rimz::address::AddressContext,
    accounts: &mut Probe<Accounts>,
    audit: bool,
    live: &LiveSessions,
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
                || !live
                    .keys
                    .contains(&(agent.kind.clone(), agent.agent_id.clone()));
            let compaction_lowered = (!historical)
                .then(|| agent_compaction_lowered(agent))
                .flatten();
            let mod_missing = !historical && live.mod_missing(agent);
            let mod_blocked = (!historical)
                .then(|| live.mod_blocked(agent).cloned())
                .flatten();
            let mod_signals_missing = if historical {
                Vec::new()
            } else {
                live.mod_signals_missing(agent)
            };
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
            let (status, phase) =
                rimz::store::snapshot::card_status(agent, row, child, demotion, snapshot.now);
            counts.add(status);
            if !audit
                && !matches!(status, AgentStatus::Failed | AgentStatus::Paused)
                && compaction_lowered.is_none()
                && !mod_missing
                && mod_signals_missing.is_empty()
                && mod_blocked.is_none()
            {
                return None;
            }
            let handle = rimz::address::paste_address(agent, snapshot, reader);
            let account = agent.login_key();
            let account_key = account.to_string();
            let usage = match accounts {
                Probe::Ready(accounts) => accounts
                    .rows
                    .iter_mut()
                    .find(|row| format!("{}@{}", row.kind, row.name) == account_key)
                    .and_then(|row| row.usage.as_mut()),
                Probe::Unavailable { .. } => None,
            };
            let attributed_to_account = !historical
                && matches!(status, AgentStatus::Failed | AgentStatus::Paused)
                && agent
                    .displayed_turn_error(snapshot.now)
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
                turn_error: agent.displayed_turn_error_label(snapshot.now),
                attributed_to_account,
                compaction_lowered,
                mod_missing,
                mod_signals_missing,
                mod_blocked,
                restartable: agent.pane.is_some() && agent.ended_at.is_none(),
                historical,
            })
        })
        .collect();
    AgentRollup::Observed { counts, rows }
}

fn agent_compaction_lowered(agent: &rimz::agents::AgentState) -> Option<CompactionLowered> {
    let context = agent.context.as_ref()?;
    let model = context.model_id.as_ref()?;
    let limits = context
        .model_context_limits
        .as_ref()
        .filter(|limits| limits.model == *model);
    let compaction = agent.native_compaction()?;
    let configured = compaction.lowered?;
    let max_window = limits.and_then(|limits| limits.max_window);
    let fix = if max_window.is_some_and(|max| compaction.raw_window >= max) {
        let profile = agent.profile.as_deref().map_or_else(
            || "its profile".to_owned(),
            |name| format!("the `{name}` profile"),
        );
        format!(
            "lower `auto-compact` to {} or less where this launch sets it: its team role, \
             {profile}, its kind base, else `[harness] auto_compact`",
            compaction.point
        )
    } else {
        let config = rimz::agents::session_login_env(&agent.kind, agent.login.as_ref())
            .ok()
            .and_then(|env| {
                rimz::agents::find_definition(agent.kind.as_str())?.model_config_path(&env)
            });
        let location = config.map_or_else(
            || format!("the {} account's config.toml", agent.login_key()),
            |path| path.display().to_string(),
        );
        format!("set `model_context_window` in {location}")
    };
    Some(CompactionLowered {
        configured,
        effective: compaction.point,
        model: model.clone(),
        raw_window: compaction.raw_window,
        max_window,
        fix,
    })
}

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
                    outdated: None,
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
    let options = crate::cli::hook_install_options();
    let mut seen: std::collections::BTreeSet<_> = catalog
        .all()
        .filter(|login| login.is_default())
        .filter_map(|login| rimz::agents::find_definition(login.kind()))
        .filter_map(|agent| agent.managed_integration())
        .flat_map(|integration| integration.outdated_files(ambient, options))
        .map(|path| integration_file_identity(&path))
        .collect();
    let in_room = |kind: &rimz::ids::AgentKind, name: &rimz::ids::LoginName| {
        room.and_then(|room| room.name(kind).ok()).as_ref() == Some(name)
    };
    let mut rows: Vec<AccountRow> = catalog
        .all()
        .filter(|login| !login.is_default())
        .map(|login| {
            let outdated = rimz::agents::find_definition(login.kind())
                .and_then(|agent| agent.managed_integration())
                .and_then(|integration| {
                    let files: Vec<_> = integration
                        .outdated_files(&login.env(ambient), options)
                        .into_iter()
                        .filter_map(|path| std::path::absolute(path).ok())
                        .filter(|path| seen.insert(integration_file_identity(path)))
                        .collect();
                    (!files.is_empty()).then(|| OutdatedIntegration {
                        fix: integration_update_fix(
                            &format!("rimz accounts add {} {}", login.kind(), login.name()),
                            &files,
                        ),
                        files,
                    })
                });
            AccountRow {
                kind: login.kind().to_string(),
                name: login.name().to_string(),
                home: login.home().map(|home| home.display().to_string()),
                room: in_room(login.kind(), login.name()),
                machine_default: machine.get(login.kind()) == Some(login.name()),
                default_for: rimz::room::Scopes::default(),
                usage: None,
                outdated,
                // A pane born on this account exports its home by design.
                problem: (!in_room(login.kind(), login.name()))
                    .then(|| login.check_exported_home(ambient).err())
                    .flatten()
                    .map(Into::into)
                    .or_else(|| login.preflight(ambient).err().map(Into::into)),
            }
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
            outdated: None,
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
                outdated: None,
                problem,
            });
        }
    }
    rows.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
    rows
}

fn integration_file_identity(path: &std::path::Path) -> std::path::PathBuf {
    let mut target = path.to_path_buf();
    'links: for _ in 0..40 {
        for ancestor in target.ancestors() {
            // Every ancestor is a component-wise prefix of this target.
            let suffix = target.strip_prefix(ancestor).expect("ancestor is a prefix");
            if let Ok(resolved) = ancestor.canonicalize() {
                return resolved.join(suffix);
            }
            if let Ok(link) = std::fs::read_link(ancestor) {
                let Some(parent) = ancestor.parent() else {
                    break;
                };
                target = parent.join(link).join(suffix);
                continue 'links;
            }
        }
        break;
    }
    path.to_path_buf()
}

fn integration_update_fix(command: &str, files: &[std::path::PathBuf]) -> String {
    let files = files
        .iter()
        .map(|path| crate::cli::render::home_relative_path(path))
        .collect::<Vec<_>>()
        .join(", ");
    format!("run `{command}` to update {files}")
}

/// Each adapter's RimZ-hook wiring state. A run in a RimZ room registers nothing
/// until the agent's own hook system invokes `rimz hooks feed`, so this
/// distinguishes installed, present-but-unwired, absent, and
/// known-but-not-installable adapters.
pub(super) fn collect_hooks() -> Vec<HookRow> {
    let login_env = rimz::agents::ambient_env();
    let options = crate::cli::hook_install_options();
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
                } else if !untrusted.is_empty() {
                    HookStatus::InstalledUntrusted {
                        events: untrusted,
                        fix: rimz::agents::hook_trust_fix(name),
                    }
                } else {
                    let files: Vec<_> = agent
                        .managed_integration()
                        .map(|integration| integration.outdated_files(&login_env, options))
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|path| std::path::absolute(path).ok())
                        .collect();
                    if files.is_empty() {
                        HookStatus::Installed
                    } else {
                        HookStatus::Outdated {
                            fix: integration_update_fix(
                                &format!("rimz hooks install {name}"),
                                &files,
                            ),
                            files,
                        }
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

pub(super) fn collect_soft_compact_skips(
    hooks: &[HookRow],
) -> Vec<super::model::SoftCompactSkipRow> {
    let mut rows: Vec<_> = hooks
        .iter()
        .filter(|row| row.detected)
        .filter_map(|row| {
            let definition = rimz::agents::find_definition(&row.kind)?;
            let reason = rimz::agents::soft_compact_skip(definition)?;
            Some(super::model::SoftCompactSkipRow {
                kind: row.kind.clone(),
                reason,
            })
        })
        .collect();
    rows.sort_unstable_by(|left, right| left.kind.cmp(&right.kind));
    rows
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
mod tests;
