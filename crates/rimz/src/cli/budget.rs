//! Inspect and change room-fleet or provider-account daily dollar caps.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;

use super::{Ctx, GlobalFlags};
use rimz::config::{DayCap, MachineConfig};
use rimz::harness::budget::{
    BudgetSpec, BudgetWindow, DailyBudgetScope, read_scope_state, scope_interrupted,
};
use rimz::ids::AgentKind;
use rimz::message::DeliveryGate;

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// New `/day` cap, `+AMOUNT`, or `off`/`clear`; omit to inspect.
    value: Option<String>,
    /// Target one provider login instead of this room's fleet.
    #[arg(long, value_name = "KIND")]
    account: Option<String>,
    /// Lift a park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
}

pub fn run(args: BudgetArgs, globals: &GlobalFlags) -> Result<()> {
    let config = MachineConfig::load_lenient();
    let account = args
        .account
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(validate_kind)
        .transpose()?;
    if account.is_some() || args.value.is_none() {
        config.accounts.validate_budgets()?;
    }
    let ctx = Ctx::open(globals)?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let now = Timestamp::now();
    let scope = account.map_or(DailyBudgetScope::Fleet, DailyBudgetScope::Account);
    let fleet_spend =
        matches!(scope, DailyBudgetScope::Fleet).then(|| live_fleet_spend(&ctx, &config, now));
    if args.value.is_none() {
        return inspect(store.runtime_paths(), &config, &scope, now, fleet_spend);
    }

    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let scope_state = read_scope_state(store.runtime_paths());
    let was_parked = mutate_scope(
        store.runtime_paths(),
        &scope,
        &config,
        args.value.as_deref().expect("value checked"),
    )?;
    let affected = snapshot.root_agents().filter(|agent| {
        !agent.agent_id.is_empty()
            && !agent.agent_id.is_provisional()
            && match &scope {
                DailyBudgetScope::Fleet => true,
                DailyBudgetScope::Account(kind) => agent.kind == *kind,
            }
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
            rimz::message::deliver::queue_nudge(
                workspace,
                store,
                agent,
                continue_text.to_owned(),
                DeliveryGate::Done,
                None,
            )
            .context("queueing budget continue prompt")?;
        }
    }

    inspect(store.runtime_paths(), &config, &scope, now, fleet_spend)
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
    let delta = raw
        .strip_prefix('+')
        .expect("caller checked prefix")
        .parse::<BudgetSpec>()
        .map_err(|err| anyhow::anyhow!("invalid budget raise `{raw}`: {err}"))?;
    if delta.window != BudgetWindow::Session {
        bail!("a relative budget raise uses `+AMOUNT`, without `/day`");
    }
    Ok(delta.cap_usd)
}

fn mutate_scope(
    runtime: &rimz::RuntimePaths,
    scope: &DailyBudgetScope,
    config: &MachineConfig,
    raw: &str,
) -> Result<bool> {
    let mut ledger = scope.read_ledger(runtime);
    let was_parked = ledger.parked.is_some();
    match raw.trim() {
        "clear" | "off" => {
            ledger.disabled = true;
            ledger.raised_cap_usd = None;
        }
        raw if raw.starts_with('+') => {
            let current = scope
                .effective_cap_usd(&ledger, config)
                .with_context(|| scope.raise_unavailable_message())?;
            ledger.raised_cap_usd = Some(checked_raise(current, parse_raise(raw)?)?);
            ledger.disabled = false;
        }
        raw => {
            scope
                .require_configured(config)
                .map_err(anyhow::Error::msg)?;
            scope.apply_absolute(&mut ledger, parse_day_cap(raw)?.as_spec());
        }
    }
    ledger.parked = None;
    scope
        .write_ledger(runtime, &ledger)
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

fn inspect(
    runtime: &rimz::RuntimePaths,
    config: &MachineConfig,
    scope: &DailyBudgetScope,
    now: Timestamp,
    fleet_spend: Option<f64>,
) -> Result<()> {
    let provider_needed = matches!(scope, DailyBudgetScope::Account(_))
        || (matches!(scope, DailyBudgetScope::Fleet) && !config.accounts.budget.is_empty());
    let provider = provider_needed.then(|| {
        rimz::agents::spending::read_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
        )
    });
    let ledger = scope.read_ledger(runtime);
    let mut kv = crate::cli::render::KeyVals::new();
    kv.push("scope", crate::cli::render::cell(scope.label()));
    kv.push(
        "cap",
        crate::cli::render::cell(cap_label(scope.effective_cap_usd(&ledger, config))),
    );
    kv.push(
        "source",
        crate::cli::render::cell(scope.cap_source(&ledger, config).to_string()),
    );
    kv.push(
        "spend",
        crate::cli::render::cell(format!(
            "${:.2} today",
            fleet_spend.unwrap_or_else(|| {
                scope.day_spend_usd(runtime, config, now, provider.as_ref())
            })
        )),
    );
    kv.push(
        "parked",
        crate::cli::render::cell(if ledger.parked.is_some() { "yes" } else { "no" }),
    );
    if let Some(cap) = config.harness.turn_budget {
        kv.push(
            "turn cap",
            crate::cli::render::cell(format!(
                "${:.2}/turn (source: config; per turn)",
                cap.as_usd()
            )),
        );
    }
    let mut out = crate::cli::render::out();
    kv.render(&mut out)?;

    if matches!(scope, DailyBudgetScope::Fleet) && !config.accounts.budget.is_empty() {
        writeln!(out)?;
        let mut table =
            crate::cli::render::Table::new(["ACCOUNT", "CAP", "SOURCE", "SPEND", "PARKED"]);
        for kind in config.accounts.budget.keys() {
            let kind = AgentKind::new_unchecked(kind);
            let scope = DailyBudgetScope::Account(kind);
            let account = scope.read_ledger(runtime);
            table.row([
                crate::cli::render::cell(
                    scope
                        .account_kind()
                        .expect("account table contains account scopes")
                        .to_string(),
                ),
                crate::cli::render::cell(cap_label(scope.effective_cap_usd(&account, config))),
                crate::cli::render::cell(scope.cap_source(&account, config).to_string()),
                crate::cli::render::cell(format!(
                    "${:.2}",
                    scope.day_spend_usd(runtime, config, now, provider.as_ref())
                )),
                crate::cli::render::cell(if account.parked.is_some() {
                    "yes"
                } else {
                    "no"
                }),
            ]);
        }
        table.render(&mut out)?;
    }
    Ok(())
}

fn cap_label(cap: Option<f64>) -> String {
    cap.map_or_else(|| "none".to_owned(), |cap| format!("${cap:.2}/day"))
}

fn checked_raise(current: f64, delta: f64) -> Result<f64> {
    let raised = current + delta;
    if !raised.is_finite() {
        bail!("budget raise is too large");
    }
    Ok(raised)
}
