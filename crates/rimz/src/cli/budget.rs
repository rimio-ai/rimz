//! Inspect and change room-fleet or provider-account dollar caps.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;
use serde::Serialize;

use super::{Ctx, GlobalFlags};
use rimz::agents::{BudgetWindow, LoginCatalog, RoomLoginSet};
use rimz::config::{AccountCap, DayCap, MachineConfig};
use rimz::harness::budget::{BudgetSpec, DailyBudgetScope, read_scope_state, scope_interrupted};
use rimz::ids::{AgentKind, LoginKey};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, MessageSender};

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// New `/day` cap (`/month` with `--account`), `+AMOUNT`, or `off`/`clear`;
    /// omit to inspect.
    value: Option<String>,
    /// Target a provider's room default or an explicit kind@name instead of the fleet.
    #[arg(long, value_name = "KIND[@NAME]")]
    account: Option<String>,
    /// Lift a park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
    /// Print structured budget facts.
    #[arg(long)]
    json: bool,
}

pub fn run(args: BudgetArgs, globals: &GlobalFlags) -> Result<()> {
    let config = MachineConfig::load_lenient();
    let account = args
        .account
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(|raw| {
            let key = if raw.contains('@') {
                let key = LoginKey::from_str(raw)?;
                validate_kind(key.kind.as_str())?;
                LoginCatalog::from_config(&config.accounts)?.select(&key.kind, &key.name)?;
                Some(key)
            } else {
                validate_kind(raw)?;
                None
            };
            Ok::<_, anyhow::Error>((raw, key))
        })
        .transpose()?;
    let ctx = Ctx::open(globals)?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let now = Timestamp::now();
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap_or_default();
    let requested = match account {
        None => None,
        Some((_, Some(key))) => Some(key),
        Some((kind, None)) => Some(
            RoomLoginSet::for_runtime(store.runtime_paths())
                .default_key(kind)
                .with_context(|| {
                    format!("cannot resolve this room's {kind} account; check `rimz accounts list`")
                })?,
        ),
    };
    let scope = requested.as_ref().map_or(DailyBudgetScope::Fleet, |key| {
        DailyBudgetScope::account(&catalog, key)
    });
    if let Some(key) = requested.as_ref() {
        config.accounts.validate_budget_for_login(key)?;
    }
    let fleet_spend =
        matches!(scope, DailyBudgetScope::Fleet).then(|| live_fleet_spend(&ctx, &config, now));
    if args.value.is_none() {
        return inspect(
            store.runtime_paths(),
            store.paths(),
            &config,
            &args,
            requested.as_ref(),
            now,
            fleet_spend,
        );
    }

    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let scope_state = read_scope_state(store.runtime_paths());
    let was_parked = mutate_scope(
        store.runtime_paths(),
        store.paths(),
        &scope,
        &config,
        args.value.as_deref().expect("value checked"),
    )?;
    let affected = snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| {
            !agent.agent_id.is_empty()
                && !agent.agent_id.is_provisional()
                && (matches!(scope, DailyBudgetScope::Fleet)
                    || DailyBudgetScope::account(&catalog, &agent.login_key()) == scope)
        });
    let continue_text = config.resume.auto_continue_text.trim();
    for agent in affected {
        rimz::harness::budget::clear_budget_park(
            store.runtime_paths(),
            &agent.kind,
            &agent.agent_id,
        );
        if was_parked
            && scope_interrupted(&scope_state, agent)
            && !args.no_continue
            && !continue_text.is_empty()
        {
            store
                .queue_message(
                    &SyntheticMessage {
                        agent,
                        text: continue_text.to_owned(),
                        sender: MessageSender::System,
                        gate: DeliveryGate::Done,
                        pane_id: None,
                    }
                    .record(workspace),
                    &workspace.session_name,
                )
                .context("queueing budget continue prompt")?;
        }
    }

    inspect(
        store.runtime_paths(),
        store.paths(),
        &config,
        &args,
        requested.as_ref(),
        now,
        fleet_spend,
    )
}

fn live_fleet_spend(ctx: &Ctx, config: &MachineConfig, now: Timestamp) -> f64 {
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let cache = rimz::harness::budget::workspace_day_cache(store.runtime_paths(), config, now);
    let Ok(mut snapshot) = ctx.resolution_snapshot() else {
        return cache.day.usd;
    };
    snapshot = snapshot.with_project_root(Some(workspace.project_root.clone()));
    if let Ok(home) =
        rimz::worktree::worktree_parent(&workspace.project_root, &config.agents.worktree)
    {
        snapshot = snapshot.with_worktree_home(Some(home));
    }
    snapshot = ctx.fold_agent_context(snapshot);
    rimz::sidebar::refresh::apply_live_day_spend(&mut snapshot, &cache);
    snapshot.fleet_day_spend_usd.unwrap_or(cache.day.usd)
}

fn validate_kind(raw: &str) -> Result<AgentKind> {
    rimz::config::AccountsConfig::validate_budget_kind(raw)?;
    Ok(AgentKind::new_unchecked(raw))
}

fn parse_raise(raw: &str) -> Result<f64> {
    if raw.contains('/') {
        bail!("a relative budget raise uses `+AMOUNT`, without `/day` or `/month`");
    }
    let delta = raw
        .strip_prefix('+')
        .expect("caller checked prefix")
        .parse::<BudgetSpec>()
        .map_err(|err| anyhow::anyhow!("invalid budget raise `{raw}`: {err}"))?;
    Ok(delta.cap_usd)
}

fn mutate_scope(
    runtime: &rimz::RuntimePaths,
    state: &rimz::StatePaths,
    scope: &DailyBudgetScope,
    config: &MachineConfig,
    raw: &str,
) -> Result<bool> {
    let mut ledger = scope.read_ledger(runtime, Some(state));
    let was_parked = ledger.parked.is_some();
    match raw.trim() {
        "clear" | "off" => {
            ledger.disabled = true;
            ledger.raised_cap_usd = None;
        }
        raw if raw.starts_with('+') => {
            let current = scope
                .standing_cap(&ledger, config)
                .map(|cap| cap.cap_usd)
                .with_context(|| scope.raise_unavailable_message())?;
            ledger.raised_cap_usd = Some(checked_raise(current, parse_raise(raw)?)?);
            ledger.disabled = false;
        }
        raw => {
            let cap = match scope {
                DailyBudgetScope::Fleet if raw.ends_with("/month") => {
                    bail!("a fleet cap is `/day` only; `/month` applies with `--account`");
                }
                DailyBudgetScope::Fleet => parse_day_cap(raw)?.as_spec(),
                DailyBudgetScope::Account(_) => raw
                    .parse::<AccountCap>()
                    .with_context(|| format!("invalid account budget value `{raw}`"))?
                    .as_spec(),
            };
            scope.apply_absolute(&mut ledger, cap);
        }
    }
    ledger.parked = None;
    scope
        .write_ledger(runtime, state, &ledger)
        .with_context(|| format!("writing {}", scope.ledger_label()))?;
    Ok(was_parked)
}

fn parse_day_cap(raw: &str) -> Result<DayCap> {
    DayCap::from_str(raw).map_err(|err| {
        anyhow::anyhow!(
            "invalid budget value `{raw}`; use a `/day` cap such as `50/day`, `+10` to raise, or `off` to disable: {err}"
        )
    })
}

#[derive(Serialize)]
struct ScopeReport {
    scope: String,
    cap_usd: Option<f64>,
    window: Option<BudgetWindow>,
    source: String,
    spend_usd: f64,
    parked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    members: Option<Vec<LoginKey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested: Option<LoginKey>,
    #[serde(skip)]
    subscription_warning: Option<String>,
}

impl ScopeReport {
    fn read(
        scope: &DailyBudgetScope,
        runtime: &rimz::RuntimePaths,
        state: &rimz::StatePaths,
        config: &MachineConfig,
        catalog: &LoginCatalog,
        spend: impl FnOnce(BudgetWindow) -> f64,
    ) -> Self {
        let ledger = scope.read_ledger(runtime, Some(state));
        let cap = scope.effective_cap(&ledger, config, runtime);
        let pool = scope.account_login();
        Self {
            scope: pool.map_or_else(|| "fleet".to_owned(), ToString::to_string),
            cap_usd: cap.map(|cap| cap.cap_usd),
            window: cap.map(|cap| cap.window),
            source: scope.cap_source(&ledger, config, runtime).to_string(),
            spend_usd: spend(cap.map_or(BudgetWindow::Day, |cap| cap.window)),
            parked: ledger.parked.is_some(),
            members: pool.map(|pool| {
                catalog
                    .all()
                    .filter(|login| &login.pool() == pool)
                    .map(|login| login.key())
                    .collect()
            }),
            requested: None,
            subscription_warning: scope
                .subscription_warning(&ledger, config, runtime)
                .map(|(_, warning)| warning),
        }
    }

    fn write(&self, out: &mut impl Write, command: &str) -> std::io::Result<()> {
        let Some(cap) = self.cap_usd else {
            if self.members.is_some() {
                return writeln!(
                    out,
                    "no cap · set one: {command} 50/day or {command} 300/month"
                );
            }
            return writeln!(out, "no daily cap · set one: {command} 50/day");
        };
        let period = if self.window == Some(BudgetWindow::Month) {
            "this month"
        } else {
            "today"
        };
        let park = if self.parked { "parked" } else { "not parked" };
        writeln!(
            out,
            "${:.2} of ${cap:.2} {period} · {park} · source {} · raise: {command} +10 · lift: {command} off",
            self.spend_usd, self.source
        )
    }
}

#[derive(Serialize)]
struct BudgetReport {
    fleet: Option<ScopeReport>,
    accounts: Vec<ScopeReport>,
    turn_cap_usd: Option<f64>,
}

fn inspect(
    runtime: &rimz::RuntimePaths,
    state: &rimz::StatePaths,
    config: &MachineConfig,
    args: &BudgetArgs,
    requested: Option<&LoginKey>,
    now: Timestamp,
    fleet_spend: Option<f64>,
) -> Result<()> {
    let catalog = LoginCatalog::from_config(&config.accounts).unwrap_or_default();
    let provider = rimz::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    let read = |scope: &DailyBudgetScope| {
        ScopeReport::read(scope, runtime, state, config, &catalog, |window| {
            if matches!(scope, DailyBudgetScope::Fleet) {
                fleet_spend
            } else {
                None
            }
            .unwrap_or_else(|| {
                scope
                    .spend_usd(runtime, config, now, Some(&provider), window)
                    .unwrap_or_default()
            })
        })
    };
    let mut report = if let Some(key) = requested {
        let mut account = read(&DailyBudgetScope::account(&catalog, key));
        account.requested = Some(key.clone());
        BudgetReport {
            fleet: None,
            accounts: vec![account],
            turn_cap_usd: config.harness.turn_budget.map(|cap| cap.as_usd()),
        }
    } else {
        let fleet = read(&DailyBudgetScope::Fleet);
        let scopes: std::collections::BTreeSet<_> = catalog
            .all()
            .map(|login| login.key())
            .chain(
                rimz::agents::known_kinds()
                    .map(|kind| LoginKey::default_for(AgentKind::new_unchecked(kind))),
            )
            .filter(|key| {
                rimz::config::AccountsConfig::validate_budget_kind(key.kind.as_str()).is_ok()
            })
            .map(|key| DailyBudgetScope::account(&catalog, &key))
            .collect();
        BudgetReport {
            fleet: Some(fleet),
            accounts: scopes.iter().map(read).collect(),
            turn_cap_usd: config.harness.turn_budget.map(|cap| cap.as_usd()),
        }
    };
    let mut stderr = crate::cli::render::err();
    for account in &report.accounts {
        if let Some(warning) = &account.subscription_warning {
            writeln!(stderr, "rimz: warning: {warning}")?;
        }
    }
    if requested.is_none() {
        report
            .accounts
            .retain(|scope| scope.cap_usd.is_some() || scope.source != "none");
    }
    if args.json {
        crate::cli::render::json(&report)?;
    } else {
        let mut out = crate::cli::render::out();
        if let Some(fleet) = &report.fleet {
            fleet.write(&mut out, "rimz budget")?;
        } else if let Some((key, account)) = requested.zip(report.accounts.first()) {
            if args.account.as_ref().is_some_and(|raw| !raw.contains('@')) {
                write!(out, "{} resolves through this room's account: ", key.kind)?;
            }
            write!(out, "{key}")?;
            let others = account
                .members
                .iter()
                .flatten()
                .filter(|member| *member != key)
                .map(|member| member.name.to_string())
                .collect::<Vec<_>>();
            if !others.is_empty() {
                write!(
                    out,
                    " (shares the {} pool with {})",
                    account.scope,
                    others.join(", ")
                )?;
            } else {
                write!(out, " (pool {})", account.scope)?;
            }
            writeln!(out)?;
            account.write(&mut out, &format!("rimz budget --account {key}"))?;
        }
        if let Some(cap) = report.turn_cap_usd {
            writeln!(out, "turn cap: ${cap:.2}/turn (source: config; per turn)")?;
        }
        if report.fleet.is_some() && !report.accounts.is_empty() {
            writeln!(out)?;
            let mut table =
                crate::cli::render::Table::new(["ACCOUNT", "CAP", "SOURCE", "SPEND", "PARKED"]);
            for account in &report.accounts {
                table.row([
                    crate::cli::render::cell(&account.scope),
                    crate::cli::render::cell(cap_label(account.cap_usd, account.window)),
                    crate::cli::render::cell(&account.source),
                    crate::cli::render::cell(format!("${:.2}", account.spend_usd)),
                    crate::cli::render::cell(if account.parked { "yes" } else { "no" }),
                ]);
            }
            table.render(&mut out)?;
        }
    }
    if requested.is_none() {
        let mut stderr = crate::cli::render::err();
        for (_, detail) in config.accounts.budget_errors() {
            writeln!(stderr, "rimz: warning: {detail}")?;
        }
    }
    Ok(())
}

fn cap_label(cap: Option<f64>, window: Option<BudgetWindow>) -> String {
    cap.zip(window).map_or_else(
        || "none".to_owned(),
        |(cap_usd, window)| BudgetSpec { cap_usd, window }.to_string(),
    )
}

fn checked_raise(current: f64, delta: f64) -> Result<f64> {
    let raised = current + delta;
    if !raised.is_finite() {
        bail!("budget raise is too large");
    }
    Ok(raised)
}
