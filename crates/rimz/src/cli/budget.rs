//! Inspect and change room-fleet or provider-account daily dollar caps.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;

use super::GlobalFlags;
use rimz::config::{DayCap, MachineConfig};
use rimz::harness::budget::{
    BudgetSpec, BudgetWindow, account_day_spend_usd, fleet_day_spend_usd, read_account_ledger,
    read_fleet_ledger, write_account_ledger, write_fleet_ledger,
};
use rimz::ids::AgentKind;
use rimz::message::{DeliveryGate, MessageRecord, MessageSender};
use rimz::workspace::WorkspaceResolver;

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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let config = MachineConfig::load_lenient();
    let now = Timestamp::now();

    let account = args
        .account
        .as_deref()
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(validate_kind)
        .transpose()?;
    let fleet_spend = account
        .is_none()
        .then(|| live_fleet_spend(&workspace, &store, globals, &config, now));
    if args.value.is_none() {
        return inspect(
            store.runtime_paths(),
            &config,
            account.as_ref(),
            now,
            fleet_spend,
        );
    }

    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let was_parked = if let Some(kind) = account.as_ref() {
        mutate_account(
            store.runtime_paths(),
            kind,
            &config,
            args.value.as_deref().expect("value checked"),
        )?
    } else {
        mutate_fleet(
            store.runtime_paths(),
            &config,
            args.value.as_deref().expect("value checked"),
        )?
    };
    let affected = snapshot.root_agents().filter(|agent| {
        !agent.agent_id.is_empty()
            && !agent.agent_id.is_provisional()
            && account.as_ref().is_none_or(|kind| agent.kind == *kind)
    });
    let continue_text = config.resume.auto_continue_text.trim();
    for agent in affected {
        rimz::harness::budget::clear_resume_park(
            store.runtime_paths(),
            &agent.kind,
            &agent.agent_id,
        );
        if was_parked && !args.no_continue && !continue_text.is_empty() {
            let message = MessageRecord::new(
                workspace.workspace_id.clone(),
                agent,
                continue_text.to_owned(),
                true,
                DeliveryGate::Done,
            )
            .with_channel(rimz::harness::target::agent_channel(agent))
            .with_sender(MessageSender::Human);
            store
                .queue_message(&message, &workspace.session_name)
                .context("queueing budget continue prompt")?;
        }
    }

    inspect(
        store.runtime_paths(),
        &config,
        account.as_ref(),
        now,
        fleet_spend,
    )
}

fn live_fleet_spend(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    config: &MachineConfig,
    now: Timestamp,
) -> f64 {
    let cache = rimz::harness::budget::workspace_day_cache(store.runtime_paths(), config, now);
    let Ok(mut snapshot) = crate::cli::resolution_snapshot(workspace, store, globals) else {
        return cache.day.usd;
    };
    snapshot = snapshot.with_project_root(Some(workspace.project_root.clone()));
    if let Ok(home) =
        rimz::worktree::worktree_parent(&workspace.project_root, &config.agents.worktree)
    {
        snapshot = snapshot.with_worktree_home(Some(home));
    }
    snapshot =
        snapshot.with_agent_context(rimz::store::agent_context::read_all(store.runtime_paths()));
    rimz::sidebar::refresh::apply_live_day_spend(&mut snapshot, &cache);
    snapshot.fleet_day_spend_usd.unwrap_or(cache.day.usd)
}

fn validate_kind(raw: &str) -> Result<AgentKind> {
    rimz::agents::find_adapter(raw)
        .map(|_| AgentKind::new_unchecked(raw))
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{raw}`"))
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

fn mutate_fleet(runtime: &rimz::RuntimePaths, config: &MachineConfig, raw: &str) -> Result<bool> {
    let mut ledger = read_fleet_ledger(runtime);
    let was_parked = ledger.parked.is_some();
    match raw.trim() {
        "clear" | "off" => {
            ledger.disabled = true;
            ledger.raised_cap_usd = None;
        }
        raw if raw.starts_with('+') => {
            let current = ledger.effective_cap_usd(config).context(
                "cannot raise a cleared or unset fleet budget; set an absolute `/day` cap first",
            )?;
            ledger.raised_cap_usd = Some(checked_raise(current, parse_raise(raw)?)?);
            ledger.disabled = false;
        }
        raw => {
            if config.harness.budget.is_none() {
                bail!(
                    "no fleet budget is configured; turn it on with `rimz config set harness.budget 50/day`"
                );
            }
            let cap = parse_day_cap(raw)?;
            ledger.override_spec = Some(cap.as_spec());
            ledger.raised_cap_usd = None;
            ledger.disabled = false;
        }
    }
    ledger.parked = None;
    write_fleet_ledger(runtime, &ledger).context("writing fleet budget ledger")?;
    Ok(was_parked)
}

fn mutate_account(
    runtime: &rimz::RuntimePaths,
    kind: &AgentKind,
    config: &MachineConfig,
    raw: &str,
) -> Result<bool> {
    let mut ledger = read_account_ledger(runtime, kind);
    let was_parked = ledger.parked.is_some();
    match raw.trim() {
        "clear" | "off" => {
            ledger.disabled = true;
            ledger.raised_cap_usd = None;
        }
        raw if raw.starts_with('+') => {
            let current = ledger.effective_cap_usd(kind, config).with_context(|| {
                format!(
                    "cannot raise a cleared or unset {kind} account budget; set an absolute `/day` cap first"
                )
            })?;
            ledger.raised_cap_usd = Some(checked_raise(current, parse_raise(raw)?)?);
            ledger.disabled = false;
        }
        raw => {
            if config.accounts.budget(kind.as_str()).is_none() {
                bail!(
                    "no {kind} account budget is configured; turn it on with `rimz config set accounts.budget.{kind} 100/day`"
                );
            }
            ledger.raised_cap_usd = Some(parse_day_cap(raw)?.as_usd());
            ledger.disabled = false;
        }
    }
    ledger.parked = None;
    write_account_ledger(runtime, kind, &ledger).context("writing account budget ledger")?;
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
    account: Option<&AgentKind>,
    now: Timestamp,
    fleet_spend: Option<f64>,
) -> Result<()> {
    if let Some(kind) = account {
        return render_account(runtime, kind, config, now);
    }
    let ledger = read_fleet_ledger(runtime);
    let mut kv = crate::cli::render::KeyVals::new();
    kv.push("scope", crate::cli::render::cell("fleet"));
    kv.push(
        "cap",
        crate::cli::render::cell(cap_label(ledger.effective_cap_usd(config))),
    );
    kv.push(
        "source",
        crate::cli::render::cell(ledger.cap_source(config).to_string()),
    );
    kv.push(
        "spend",
        crate::cli::render::cell(format!(
            "${:.2} today",
            fleet_spend.unwrap_or_else(|| fleet_day_spend_usd(runtime, config, now))
        )),
    );
    kv.push(
        "parked",
        crate::cli::render::cell(if ledger.parked.is_some() { "yes" } else { "no" }),
    );
    let mut out = crate::cli::render::out();
    kv.render(&mut out)?;

    if !config.accounts.budget.is_empty() {
        writeln!(out)?;
        let mut table =
            crate::cli::render::Table::new(["ACCOUNT", "CAP", "SOURCE", "SPEND", "PARKED"]);
        for kind in config.accounts.budget.keys() {
            let kind = AgentKind::new_unchecked(kind);
            let account = read_account_ledger(runtime, &kind);
            table.row([
                crate::cli::render::cell(kind.to_string()),
                crate::cli::render::cell(cap_label(account.effective_cap_usd(&kind, config))),
                crate::cli::render::cell(account.cap_source(&kind, config).to_string()),
                crate::cli::render::cell(format!(
                    "${:.2}",
                    account_day_spend_usd(runtime, &kind, config, now)
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

fn render_account(
    runtime: &rimz::RuntimePaths,
    kind: &AgentKind,
    config: &MachineConfig,
    now: Timestamp,
) -> Result<()> {
    let ledger = read_account_ledger(runtime, kind);
    let mut kv = crate::cli::render::KeyVals::new();
    kv.push("scope", crate::cli::render::cell(format!("{kind} account")));
    kv.push(
        "cap",
        crate::cli::render::cell(cap_label(ledger.effective_cap_usd(kind, config))),
    );
    kv.push(
        "source",
        crate::cli::render::cell(ledger.cap_source(kind, config).to_string()),
    );
    kv.push(
        "spend",
        crate::cli::render::cell(format!(
            "${:.2} today",
            account_day_spend_usd(runtime, kind, config, now)
        )),
    );
    kv.push(
        "parked",
        crate::cli::render::cell(if ledger.parked.is_some() { "yes" } else { "no" }),
    );
    crate::cli::render::finish(kv.render(&mut crate::cli::render::out()))
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
