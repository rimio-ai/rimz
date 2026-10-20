//! Provider accounts at the command line: `rimz accounts add|use|list|show|redeem|remove`,
//! and the `--account <kind>@<name>` selection `rimz start` and `rimz reset`
//! pass to a room's birth.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use rimz::RuntimePaths;
use rimz::agents::account::RedemptionCode;
use rimz::agents::{LoginCatalog, ProviderLogin, RateLimitWindow, RedeemEffect, ResetCredits};
use rimz::config::{AccountHistory, AccountsConfig, ConfigEditor, MachineConfig, NamedAccount};
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::auto_redeem::{RedeemReport, prepare_manual_redeem, supports_kind};
use rimz::ids::{AgentKind, LoginKey, LoginName, RoomLogins};
use rimz::store::snapshot::RedeemForecast;
use rimz::utils::path::normalize_path_lexical;
use rimz::utils::time::format_local_timestamp;

use super::{GlobalFlags, render};

mod report;

const PACE_HELP: &str = "A window cell reads what's left, then the time to reset, then the pace. Pace is the used share ÷ the elapsed share; 1.0x lasts exactly to the reset.";

#[derive(Debug, Default, Args)]
struct ListArgs {
    /// Emit the accounts as JSON.
    #[arg(long)]
    json: bool,
    /// Bypass account and usage refresh TTLs.
    #[arg(long)]
    refresh: bool,
    /// Expand logged-out and empty provider kinds into tables.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
pub(super) struct ProvidersArgs {
    /// Show only one provider kind.
    #[arg(value_name = "KIND")]
    kind: Option<String>,
    /// Emit the report as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,
    /// Bypass account and usage refresh TTLs.
    #[arg(long)]
    refresh: bool,
    /// Expand logged-out and empty provider kinds into tables.
    #[arg(long)]
    all: bool,
}

pub(super) fn run_alias(args: ProvidersArgs, globals: &GlobalFlags) -> Result<()> {
    writeln!(
        std::io::stderr().lock(),
        "rimz: warning: `rimz providers` is deprecated and goes in the next release; use `rimz accounts` (`rimz accounts show <kind>` for one provider)"
    )?;
    report::list(
        ListArgs {
            json: args.json,
            refresh: args.refresh,
            all: args.all,
        },
        args.kind.as_deref(),
        true,
        globals,
    )
}

fn redeem_exit_code(outcome: RedemptionCode) -> i32 {
    match outcome {
        RedemptionCode::Reset => 0,
        RedemptionCode::NoCredit => 3,
        RedemptionCode::NothingToReset => 4,
        RedemptionCode::AlreadyRedeemed => 5,
        RedemptionCode::Unknown => 6,
        RedemptionCode::Cooldown => 7,
    }
}

#[derive(Debug, Args)]
#[command(after_long_help = PACE_HELP, args_conflicts_with_subcommands = true)]
pub struct AccountsArgs {
    #[command(flatten)]
    listing: ListArgs,
    #[command(subcommand)]
    command: Option<AccountsSubcmd>,
}

#[derive(Debug, Subcommand)]
enum AccountsSubcmd {
    /// Compare provider accounts, usage left, credits, and launch health.
    #[command(after_long_help = PACE_HELP)]
    List(ListArgs),
    /// Show one provider's accounts or one named account in detail.
    Show {
        /// Provider kind, optionally followed by @name.
        #[arg(value_name = "KIND[@NAME]")]
        account: String,
        /// Emit the accounts as JSON.
        #[arg(long)]
        json: bool,
        /// Bypass account and usage refresh TTLs.
        #[arg(long)]
        refresh: bool,
    },
    /// Pin this room's launch account, or change the live machine default with --global.
    Use {
        /// Set the default for new rooms and rooms following the machine default.
        #[arg(long)]
        global: bool,
        /// Unpin this kind and follow the project or machine default live.
        #[arg(long, conflicts_with_all = ["name", "global"])]
        reset: bool,
        /// Provider kind: claude or codex.
        kind: String,
        /// Declared account name, or `default` for the provider's own home.
        #[arg(required_unless_present = "reset")]
        name: Option<LoginName>,
    },
    /// Declare a named account, create its home, and install RimZ hooks there.
    ///
    /// Rerun to finish an account whose setup stopped part way.
    Add {
        /// Provider kind: claude or codex.
        kind: String,
        /// Account name, such as `work`.
        name: LoginName,
        /// Provider home for this account. Defaults to a directory under the
        /// RimZ data root.
        #[arg(long)]
        home: Option<PathBuf>,
        /// `shared` (the default) keeps only credentials in the account home;
        /// `standalone` keeps the account's sessions and transcripts apart.
        #[arg(long, value_name = "MODE")]
        history: Option<AccountHistory>,
    },
    /// Preview and spend one reset credit by hand (claude or codex).
    Redeem {
        /// Provider kind: claude or codex.
        kind: String,
        /// Account name; defaults to the account a launch here uses.
        name: Option<LoginName>,
        /// Print the fresh preview without spending or prompting.
        #[arg(long)]
        dry_run: bool,
        /// Skip confirmation (required without a terminal, unless --dry-run).
        #[arg(long)]
        yes: bool,
    },
    /// Forget a named account; its home directory stays on disk.
    Remove {
        /// Provider kind: claude or codex.
        kind: String,
        /// Account name.
        name: LoginName,
    },
}

pub fn run(args: AccountsArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command.unwrap_or(AccountsSubcmd::List(args.listing)) {
        AccountsSubcmd::Add {
            kind,
            name,
            home,
            history,
        } => add(&account_kind(&kind)?, name, home, history),
        AccountsSubcmd::List(args) => report::list(args, None, false, globals),
        AccountsSubcmd::Show {
            account,
            json,
            refresh,
        } => report::list(
            ListArgs {
                json,
                refresh,
                all: true,
            },
            Some(&account),
            false,
            globals,
        ),
        AccountsSubcmd::Redeem {
            kind,
            name,
            dry_run,
            yes,
        } => redeem(globals, &account_kind(&kind)?, name, dry_run, yes),
        AccountsSubcmd::Remove { kind, name } => remove(&account_kind(&kind)?, &name),
        AccountsSubcmd::Use {
            kind, name, global, ..
        } => {
            if global {
                // Clap requires a name unless --reset, which conflicts with --global.
                use_account(&kind, &name.expect("a global selection has a name"))
            } else {
                use_room_account(globals, &account_kind(&kind)?, name.as_ref())
            }
        }
    }
}

/// A kind that can carry named accounts, in its canonical spelling.
fn account_kind(raw: &str) -> Result<AgentKind> {
    let supported: Vec<_> = rimz::agents::known_kinds()
        .map(AgentKind::new_unchecked)
        .filter(|kind| AccountsConfig::default().named(kind).is_some())
        .collect();
    if let Some(definition) = rimz::agents::find_definition(raw) {
        let kind = AgentKind::new_unchecked(definition.spec().kind);
        if supported.contains(&kind) {
            return Ok(kind);
        }
    }
    bail!(
        "{raw} has no named accounts; accounts are supported for {}",
        supported
            .iter()
            .map(AgentKind::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn redeem(
    globals: &GlobalFlags,
    kind: &AgentKind,
    name: Option<LoginName>,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    if !dry_run && !yes && !std::io::stdin().is_terminal() {
        bail!("pass --yes to confirm without a terminal, or --dry-run to preview without spending");
    }
    let machine = MachineConfig::load()?;
    let (standing, in_room) = super::position_standing(globals, &machine)?;
    let name = name.unwrap_or_else(|| standing.active(kind).unwrap_or_default());
    let catalog = LoginCatalog::from_config(&machine.accounts)?;
    let login = catalog.select(kind, &name)?;
    let ambient = rimz::agents::ambient_env();
    let ambient = if login.is_default() {
        catalog.native_ambient(kind, &ambient)
    } else {
        ambient
    };
    login.preflight(&ambient)?;
    if rimz::utils::env::flag_enabled("RIMZ_OAUTH_USAGE_OFFLINE") {
        bail!(
            "RIMZ_OAUTH_USAGE_OFFLINE disables provider usage reads; unset it to redeem a reset credit"
        );
    }
    let runtime = if in_room {
        let workspace = rimz::WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
        super::runtime_paths_for(workspace.workspace_id)?
    } else {
        RuntimePaths::shared()
    };
    runtime.ensure_shared_dirs()?;
    let key = login.key();
    let preview = prepare_manual_redeem(&runtime, &key, &login.env(&ambient), &machine.resume)?;
    let mut out = render::out();
    write_redeem_preview(
        &mut out,
        &key,
        &preview.credits,
        &preview.windows,
        preview.forecast,
        preview.min_gain,
        Timestamp::now(),
    )?;
    if let Some(hold) = preview.hold() {
        let mut rows = render::KeyVals::new().indent(2);
        rows.push("hold", render::cell(&hold.reason));
        rows.render(&mut out)?;
    }
    out.flush()?;
    if dry_run {
        return Ok(());
    }
    if let Some(hold) = preview.hold() {
        writeln!(out, "{}: {}", hold.code.as_str(), hold.reason)?;
        out.flush()?;
        std::process::exit(redeem_exit_code(hold.code));
    }
    if preview.credits.count == 0 {
        writeln!(out, "no_credit: nothing spent")?;
        out.flush()?;
        std::process::exit(redeem_exit_code(RedemptionCode::NoCredit));
    }
    if !yes && !super::confirm("redeem one credit now?")? {
        writeln!(std::io::stderr().lock(), "nothing spent")?;
        return Ok(());
    }
    let request_id = uuid::Uuid::now_v7();
    let redeemed = match preview.consume(&runtime, &key, request_id) {
        Ok(redeemed) => redeemed,
        Err(error) => {
            if let Some(report) = error.attempted_report() {
                append_manual_report(&key, request_id, report, Some(error.to_string()));
            }
            return Err(error).with_context(|| format!("redeeming {} reset credit", key.kind));
        }
    };
    if let Some((identity, snapshot)) = redeemed.usage {
        rimz::sidebar::refresh::publish_account_usage_snapshot(&runtime, &key, identity, snapshot);
    }
    append_manual_report(&key, request_id, &redeemed.report, None);
    if in_room && redeemed.report.outcome == Some(RedemptionCode::Reset) {
        let _ = rimz::wakeup::wake_store_delta(&runtime, None, None);
    }
    if let Some(error) = redeemed.refresh_error {
        writeln!(
            std::io::stderr().lock(),
            "rimz: warning: the credit was spent and the window refilled, but usage refresh failed: {error}; the sidebar will catch up on its next refresh"
        )?;
    }
    // Every completed consume returns a provider outcome, including unknown codes.
    let outcome = redeemed
        .report
        .outcome
        .expect("a completed consume has an outcome");
    writeln!(out, "{key}: {}", outcome.as_str())?;
    out.flush()?;
    let code = redeem_exit_code(outcome);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn write_redeem_preview(
    w: &mut impl std::io::Write,
    key: &LoginKey,
    credits: &ResetCredits,
    windows: &[RateLimitWindow],
    forecast: Option<RedeemForecast>,
    min_gain: Duration,
    now: Timestamp,
) -> std::io::Result<()> {
    writeln!(
        w,
        "{}",
        render::paint(
            render::palette::identity(key.kind.as_str()),
            &key.to_string()
        )
    )?;
    let mut rows = render::KeyVals::new().indent(2);
    let expiry = credits
        .soonest_expiry
        .map(|expiry| {
            format!(
                "; soonest expiry {} ({})",
                render::rel_until(expiry, now),
                format_local_timestamp(expiry)
            )
        })
        .unwrap_or_default();
    rows.push(
        "credits",
        render::cell(format!("{}{expiry}", credits.count)),
    );
    for window in windows {
        if window.lifted {
            rows.push(
                rimz::theme::fmt::window_label(window),
                render::cell("∞ not enforced now"),
            );
            continue;
        }
        let used = window.used_percentage.map_or_else(
            || "unknown usage".to_owned(),
            |used| format!("{used}% used"),
        );
        let reset = window.resets_at.map_or_else(
            || "reset unknown".to_owned(),
            |reset| format!("reset {}", render::rel_until(reset, now)),
        );
        rows.push(
            rimz::theme::fmt::window_label(window),
            render::cell(format!("{used}; {reset}")),
        );
    }
    for window in windows.iter().filter(|window| !window.lifted) {
        let effect = match credits.effect {
            RedeemEffect::RestartsWindow => {
                // The harness projects only the duration-bearing 5h and 7d windows.
                let mins = window
                    .duration_mins
                    .expect("preview windows have a duration");
                format!(
                    "refills now, next reset in {}",
                    render::format_compact_duration(u64::from(mins) * 60)
                )
            }
            RedeemEffect::KeepsSchedule => window.resets_at.map_or_else(
                || "refills now, reset stays unknown".to_owned(),
                |reset| {
                    format!(
                        "refills now, reset stays at {}",
                        format_local_timestamp(reset)
                    )
                },
            ),
        };
        rows.push(
            format!("redeem {}", rimz::theme::fmt::window_label(window)),
            render::cell(effect),
        );
        if credits.effect == RedeemEffect::RestartsWindow
            && let Some(reset) = window.resets_at.filter(|reset| {
                *reset > now && reset.duration_since(now).as_secs_f64() < min_gain.as_secs_f64()
            })
        {
            rows.push(
                "warning",
                render::cell(format!(
                    "gives up the free {} reset {}",
                    rimz::theme::fmt::window_label(window),
                    render::rel_until(reset, now)
                ))
                .fg(render::palette::warn()),
            );
        }
    }
    if !supports_kind(&key.kind) {
        return rows.render(w);
    }
    let forecast = match forecast {
        Some(RedeemForecast::Manual) => "off",
        Some(RedeemForecast::Armed) => "armed",
        Some(RedeemForecast::Holding) => "holding",
        None => "no credits",
    };
    rows.push(
        "auto-redeem forecast",
        render::cell(format!("{forecast}, if the longest window ran dry now")),
    );
    rows.render(w)
}

fn append_manual_report(
    login: &LoginKey,
    request_id: uuid::Uuid,
    report: &RedeemReport,
    error: Option<String>,
) {
    let outcome = if error.is_none() {
        report.outcome.map(|outcome| outcome.as_str().to_owned())
    } else {
        None
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::AutoRedeem {
            kind: login.kind.to_string(),
            login: Some(login.name.clone()),
            reason: report.reason,
            request_id: request_id.to_string(),
            credits: report.credits,
            soonest_expiry: report.soonest_expiry,
            natural_reset: report.natural_reset,
            outcome,
            windows_reset: report.windows_reset,
            window_resets: report.window_resets.clone(),
            error,
        },
    });
}

fn add(
    kind: &AgentKind,
    name: LoginName,
    home: Option<PathBuf>,
    history: Option<AccountHistory>,
) -> Result<()> {
    if name.is_default() {
        bail!("`default` is {kind}'s own home and needs no declaring; choose another name");
    }
    let home_flag = home
        .map(std::path::absolute)
        .transpose()
        .context("resolving --home")?;
    let ambient = rimz::agents::ambient_env();
    let machine = MachineConfig::load()?;
    let catalog = LoginCatalog::from_config(&machine.accounts)?;
    let existing = catalog.select(kind, &name).ok();
    let declaring = existing.is_none();
    // The login under the requested declaration, which reaches the config
    // file only once the home is reconciled to it.
    let mut accounts = machine.accounts.clone();
    if let Some(declared) = accounts.named_mut(kind) {
        let account = declared
            .entry(name.clone())
            .or_insert_with(|| NamedAccount {
                home: home_flag.clone(),
                history: AccountHistory::default(),
            });
        account.history = history.unwrap_or(account.history);
    }
    let login = LoginCatalog::from_config(&accounts)?.select(kind, &name)?;
    if let Some((existing, home)) = existing.as_ref().zip(home_flag.as_deref())
        && lexical_home(existing) != Some(normalize_path_lexical(home))
    {
        bail!(
            "{kind} account `{name}` already lives at `{}`; rerun without --home, or remove the account first",
            existing
                .home()
                .unwrap_or(std::path::Path::new(""))
                .display()
        );
    }
    for account in std::iter::once(&login).chain(
        catalog
            .all()
            .filter(|account| account.kind() == kind && account.name() != &name),
    ) {
        if let Err(error) = account.check_exported_home(&ambient) {
            if let rimz::agents::LoginConfigErr::ExportedHome {
                env_key,
                home,
                name: exported_name,
                ..
            } = error
            {
                bail!(
                    "`{env_key}` is exported as `{}`, the home of {kind} account `{exported_name}`; unset `{env_key}` so `default` resolves to {kind}'s own home, e.g. `env -u {env_key} rimz accounts add {kind} {name}`",
                    home.display()
                );
            }
            return Err(error.into());
        }
    }
    let default_home = login
        .default_home(&ambient)
        .with_context(|| format!("cannot resolve {kind}'s own home; set HOME"))?;
    // Sound: `select` answers a non-default name only with a declared home.
    let named_home = login.home().expect("a named account has a home");
    rimz::agents::account_links::check_distinct_homes(named_home, &default_home)?;
    // A first declaration is written before the home changes, so a rerun
    // finds it; a mode switch is written only once the home matches it.
    let write_declaration = || {
        ConfigEditor::machine().upsert_named_account(
            kind,
            &name,
            home_flag.as_deref().filter(|_| declaring),
            history,
        )
    };
    if declaring {
        write_declaration()?;
    }
    let home = login.home().expect("a named account has a home");
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating {kind} account home {}", home.display()))?;
    let definition = rimz::agents::definition_by_kind(kind.as_str())?;
    let account = login.key();
    let shared = rimz::agents::account_links::reconcile(&login, &ambient, &|| {
        rimz::room::other_live_agents_on(&account, &[]).ok()
    })?;
    if !declaring && history.is_some() {
        write_declaration()?;
    }
    let mut out = render::out();
    if let Some(shared) = shared {
        writeln!(out, "{shared}")?;
    }
    crate::cli::hooks::install_hooks_into(definition, &login.env(&ambient), &mut out)?;
    render::finish(writeln!(
        out,
        "{kind} account `{name}` lives at {}\n  log in once   {}\n  use at birth  rimz start --account {kind}@{name}\n  this room     rimz accounts use {kind} {name}\n  new rooms     rimz accounts use --global {kind} {name}",
        render::home_relative(&home.display().to_string()),
        login_command(&login, &ambient)
    ))
}

/// The command that logs `login` in: its home overrides, then the kind. The
/// caller names a login whose home exists, so its path holds no NUL byte.
fn login_command(login: &ProviderLogin, ambient: &BTreeMap<String, String>) -> String {
    login
        .overrides(ambient)
        .into_iter()
        .map(|(key, value)| {
            let value = shlex::try_quote(&value).expect("an existing path is shell-quotable");
            format!("{key}={value} ")
        })
        .chain([login.kind().to_string()])
        .collect()
}

fn lexical_home(login: &ProviderLogin) -> Option<PathBuf> {
    login.home().map(normalize_path_lexical)
}

fn use_room_account(
    globals: &GlobalFlags,
    kind: &AgentKind,
    name: Option<&LoginName>,
) -> Result<()> {
    let root = super::pinned_room_root().context(
        "`rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms and rooms following it",
    )?;
    if let Some(override_root) = &globals.root
        && override_root.canonicalize()? != root
    {
        bail!(
            "`rimz accounts use` switches the current room at `{}`; --root `{}` names another room; drop --root or run the command inside that room",
            root.display(),
            override_root.display()
        );
    }
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone())
            .context("resolving current workspace")?;
    let machine = MachineConfig::load_lenient();
    let store = super::require_existing_store(&workspace)?;
    let current = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &machine,
    )?;
    let mut prior_pin = current.pin(kind).cloned();
    let prior_account = current
        .account(kind)
        .ok()
        .filter(|account| account.source != rimz::agents::LoginSource::Pinned);
    let mut pins = current.pinned_names();
    if let Some(name) = name {
        pins.insert(kind.clone(), name.clone());
    } else {
        pins.remove(kind);
    }
    let selected = rimz::agents::resolve_room_accounts(&pins, &workspace.project_root, &machine);
    let account = selected.account(kind).with_context(|| {
        if name.is_none() {
            "cannot reset this room's account"
        } else {
            "cannot select this room's account"
        }
    })?;
    selected
        .login(kind, &machine.accounts)?
        .preflight(&rimz::agents::ambient_env())?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let heading = if name.is_none() {
        store.unpin_room_login(&workspace, kind)?;
        let change = if prior_pin.is_some() {
            "now"
        } else {
            "already"
        };
        format!(
            "this room {change} follows the {} for {kind} (`{kind}@{}`)",
            account.source, account.name
        )
    } else {
        prior_pin = store.switch_room_login(&workspace, kind, &account.name)?;
        if prior_pin.as_ref() == Some(&account.name) {
            return render::finish(writeln!(
                render::out(),
                "this room already launches {kind} on `{kind}@{}` (pinned)",
                account.name
            ));
        }
        let suffix = if prior_pin.is_some() {
            String::new()
        } else if let Some(prior) = &prior_account {
            format!(" (pinned, was {})", prior.source)
        } else {
            " (pinned)".to_owned()
        };
        format!(
            "this room now launches {kind} on account `{kind}@{}`{suffix}",
            account.name
        )
    };
    if name.is_none() && prior_pin.is_none() {
        return render::finish(writeln!(render::out(), "{heading}"));
    }
    let prior = prior_pin.or_else(|| prior_account.map(|account| account.name));
    let count = snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.kind == *kind
                && agent.ended_at.is_none()
                && !agent.is_provider_subagent()
                && prior
                    .as_ref()
                    .is_none_or(|prior| agent.login_key().name == *prior)
        })
        .count();
    let remaining = match prior {
        Some(prior) => format!(
            "{count} running {kind} agent(s) stay on {}",
            LoginKey {
                kind: kind.clone(),
                name: prior
            }
        ),
        None => format!("{count} running {kind} agent(s) stay on their accounts"),
    };
    render::finish(writeln!(
        render::out(),
        "{heading}; new launches only; {remaining}"
    ))
}

fn use_account(raw_kind: &str, name: &LoginName) -> Result<()> {
    // Clearing also repairs a hand-set selection for a kind without named accounts.
    let kind = &match account_kind(raw_kind) {
        Err(_) if name.is_default() => AgentKind::new_unchecked(raw_kind),
        kind => kind?,
    };
    let mut machine = MachineConfig::load()?;
    let login = match LoginCatalog::from_config(&machine.accounts)?.select(kind, name) {
        Ok(login) => Some(login),
        Err(_) if name.is_default() => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(login) = login {
        login.preflight(&rimz::agents::ambient_env())?;
    }
    let prior = LoginKey {
        kind: kind.clone(),
        name: machine
            .accounts
            .use_accounts
            .get(kind)
            .cloned()
            .unwrap_or_default(),
    };
    let remaining = match rimz::room::live_agents_on_machine_default(&prior, &machine) {
        Ok(count) => format!("{count} running {kind} agent(s) stay on {prior}"),
        Err(error) => {
            writeln!(
                render::err(),
                "warning: could not count running agents: {error}"
            )?;
            format!("running {kind} agents stay on {prior} (count unknown)")
        }
    };
    ConfigEditor::machine().use_account(kind, name)?;
    if name.is_default() {
        machine.accounts.use_accounts.remove(kind);
    } else {
        machine
            .accounts
            .use_accounts
            .insert(kind.clone(), name.clone());
    }
    let live = live_rooms_where(&machine, |accounts| {
        matches!(
            accounts.source(kind),
            Ok(rimz::agents::LoginSource::Machine | rimz::agents::LoginSource::Provider)
        )
    });
    let reach = if live.is_empty() {
        format!("no live room follows it for {kind}")
    } else {
        format!(
            "{} live room(s) follow it for {kind}: {}",
            live.len(),
            live.join(", ")
        )
    };
    let selected = if name.is_default() {
        format!("{kind}'s own home (`{kind}@default`)")
    } else {
        format!("{kind} account `{kind}@{name}`")
    };
    render::finish(writeln!(
        render::out(),
        "new rooms and rooms following the machine default now use {selected}; new launches only; {remaining}; {reach}; a pinned room keeps its account until `rimz accounts use --reset {kind}` runs inside it"
    ))
}

fn remove(kind: &AgentKind, name: &LoginName) -> Result<()> {
    if name.is_default() {
        bail!("`default` is {kind}'s own home and cannot be removed");
    }
    let machine = MachineConfig::load()?;
    let clears_selection = machine.accounts.use_accounts.get(kind) == Some(name);
    let home = LoginCatalog::from_config(&machine.accounts)?
        .select(kind, name)
        .ok()
        .and_then(|login| login.home().map(|home| home.display().to_string()));
    let removed = ConfigEditor::machine().remove_named_account(kind, name)?;
    let mut out = render::out();
    if !removed {
        return render::finish(writeln!(
            out,
            "no {kind} account `{name}` is configured; nothing to remove"
        ));
    }
    // After the removal: the probe costs a session listing on both backends,
    // and a room's selection lives in its own record rather than this config.
    let live = live_rooms_selecting(kind, name);
    if clears_selection {
        render::finish(writeln!(
            out,
            "cleared the machine selection; new rooms and rooms following the machine default now use {kind} account `default`"
        ))?;
    }
    render::finish(writeln!(
        out,
        "{}",
        removed_notice(
            kind,
            name,
            &render::home_relative(home.as_deref().unwrap_or_default()),
            &live,
        )
    ))
}

/// Live rooms pinned to this account, not an inventory of session stamps.
fn live_rooms_selecting(kind: &AgentKind, name: &LoginName) -> Vec<String> {
    live_rooms_where(&MachineConfig::load_lenient(), |accounts| {
        accounts.pin(kind) == Some(name)
    })
}

fn live_rooms_where(
    machine: &MachineConfig,
    matches: impl Fn(&rimz::agents::RoomAccounts) -> bool,
) -> Vec<String> {
    let inventory = match rimz::room::session::room_inventory() {
        Ok(inventory) => inventory,
        Err(err) => {
            tracing::debug!(%err, "could not inventory room accounts");
            return Vec::new();
        }
    };
    let mut rooms = Vec::new();
    for room in inventory.live {
        let selection = (|| -> Result<rimz::agents::RoomAccounts> {
            let paths = rimz::StatePaths::for_workspace(room.workspace_id)?;
            Ok(rimz::agents::room_accounts(
                &paths.workspace_record,
                None,
                machine,
            )?)
        })();
        match selection {
            Ok(accounts) if matches(&accounts) => rooms.push(room.session_name),
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(%err, session = %room.session_name, "could not read room accounts")
            }
        }
    }
    rooms.sort();
    rooms
}

/// What `remove` prints, given the account, its displayed home, and the live rooms still selecting it.
fn removed_notice(kind: &AgentKind, name: &LoginName, home: &str, live: &[String]) -> String {
    let mut notice = format!(
        "removed {kind} account `{name}`; its home {home} and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use {kind} default` for future launches"
    );
    if !live.is_empty() {
        let (room, verb) = if live.len() == 1 {
            ("room", "pins")
        } else {
            ("rooms", "pin")
        };
        notice.push_str(&format!(
            "\nwarning: {room} {} {verb} it for new {kind} launches; add the account back, run `rimz accounts use --reset {kind}` to follow the defaults, or run `rimz accounts use {kind} default` inside each room",
            live.join(", ")
        ));
    }
    notice
}

/// One `--account <kind>@<name>` flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountFlag {
    kind: AgentKind,
    name: LoginName,
}

impl std::fmt::Display for AccountFlag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={}", self.kind, self.name)
    }
}

pub(crate) fn parse_account_flag(raw: &str) -> std::result::Result<AccountFlag, String> {
    let Some((kind, name)) = raw.split_once('@').or_else(|| raw.split_once('=')) else {
        return Err(format!("expected `<kind>@<name>`, got `{raw}`"));
    };
    let Some(definition) = rimz::agents::find_definition(kind) else {
        return Err(format!("unknown agent kind `{kind}`"));
    };
    Ok(AccountFlag {
        kind: AgentKind::new_unchecked(definition.spec().kind),
        name: name.parse().map_err(|err| format!("{err}"))?,
    })
}

/// The room selection the flags request; naming one kind twice is refused
/// rather than letting the later flag silently win.
pub(crate) fn requested_logins(flags: &[AccountFlag]) -> Result<RoomLogins> {
    let mut logins = RoomLogins::new();
    for flag in flags {
        if let Some(first) = logins.insert(flag.kind.clone(), flag.name.clone()) {
            bail!(
                "--account names {} twice (`{first}` and `{}`); pass one account per kind",
                flag.kind,
                flag.name
            );
        }
    }
    Ok(logins)
}
