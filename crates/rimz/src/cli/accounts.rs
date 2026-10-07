//! Provider accounts at the command line: `rimz accounts add|use|list|redeem|remove`,
//! and the `--account <kind>=<name>` selection `rimz start` and `rimz reset`
//! pass to a room's birth.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use jiff::Timestamp;
use rimz::RuntimePaths;
use rimz::agents::account::{ProviderStatus, RedemptionCode, WindowSpan};
use rimz::agents::spending::read_provider_spending_cache;
use rimz::agents::{
    AccountStatus, BirthLoginErr, LoginCatalog, ProviderLogin, RateLimitWindow, RedeemEffect,
    ResetCredits,
};
use rimz::config::{AccountHistory, AccountsConfig, ConfigEditor, MachineConfig, NamedAccount};
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::auto_redeem::{ManualRedeem, RedeemReport, prepare_manual_redeem};
use rimz::ids::{AgentKind, LoginKey, LoginName, RoomLogins};
use rimz::room::{AccountStanding, Deciding, Scopes};
use rimz::sidebar::enrich::provider_panel_for_login;
use rimz::sidebar::refresh::{query_provider_accounts, refresh_provider_usage};
use rimz::store::snapshot::RedeemForecast;
use rimz::utils::path::normalize_path_lexical;
use serde::Serialize;

use super::spinner::Spinner;
use super::{GlobalFlags, render};

const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);

fn redeem_exit_code(outcome: RedemptionCode) -> i32 {
    match outcome {
        RedemptionCode::Reset => 0,
        RedemptionCode::NoCredit => 3,
        RedemptionCode::NothingToReset => 4,
        RedemptionCode::AlreadyRedeemed => 5,
        RedemptionCode::Unknown => 6,
    }
}

fn check_redeem_kind(kind: &AgentKind) -> Result<()> {
    if kind.as_str() == "claude" {
        bail!(
            "Claude limit resets cannot be spent from RimZ; redeem them in Claude's Settings > Usage"
        );
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct AccountsArgs {
    #[command(subcommand)]
    command: AccountsSubcmd,
}

#[derive(Debug, Subcommand)]
enum AccountsSubcmd {
    /// Select this room's account for future launches, or new rooms' with --global.
    Use {
        /// Set the machine default for new rooms instead of this room's.
        #[arg(long)]
        global: bool,
        /// Return this room's account for the kind to the one a new room here would get.
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
    /// List every account with its 5h and 7d usage left, its home, and whether a room can launch into it.
    List {
        /// Emit the accounts as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Preview and spend one Codex reset credit by hand.
    Redeem {
        /// Provider kind: codex. Claude limit resets must be spent in Settings > Usage.
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
    match args.command {
        AccountsSubcmd::Add {
            kind,
            name,
            home,
            history,
        } => add(&account_kind(&kind)?, name, home, history),
        AccountsSubcmd::List { json } => list(globals, json),
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
    check_redeem_kind(kind)?;
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
    write_redeem_preview(&mut out, &key, &preview, Timestamp::now())?;
    out.flush()?;
    if dry_run {
        return Ok(());
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
            return Err(error).context("redeeming Codex reset credit");
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
    preview: &ManualRedeem,
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
    let expiry = preview
        .credits
        .soonest_expiry
        .map(|expiry| {
            let local = expiry.to_zoned(jiff::tz::TimeZone::system());
            format!(
                "; soonest expiry {} ({})",
                render::rel_until(expiry, now),
                local.strftime("%Y-%m-%d %H:%M:%S %:z")
            )
        })
        .unwrap_or_default();
    rows.push(
        "credits",
        render::cell(format!("{}{expiry}", preview.credits.count)),
    );
    for window in &preview.windows {
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
    for window in preview.windows.iter().filter(|window| !window.lifted) {
        let effect = match preview.credits.effect {
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
                        reset
                            .to_zoned(jiff::tz::TimeZone::system())
                            .strftime("%Y-%m-%d %H:%M:%S %:z")
                    )
                },
            ),
        };
        rows.push(
            format!("redeem {}", rimz::theme::fmt::window_label(window)),
            render::cell(effect),
        );
        if preview.credits.effect == RedeemEffect::RestartsWindow
            && let Some(reset) = window.resets_at.filter(|reset| {
                *reset > now
                    && reset.duration_since(now).as_secs_f64() < preview.min_gain.as_secs_f64()
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
    let forecast = match preview.forecast {
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
        "{kind} account `{name}` lives at {}\n  log in once   {}\n  use at birth  rimz start --account {kind}={name}\n  this room     rimz accounts use {kind} {name}\n  new rooms     rimz accounts use --global {kind} {name}",
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
        "`rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms",
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
    let machine = if name.is_some() {
        std::sync::Arc::new(MachineConfig::load()?)
    } else {
        MachineConfig::load_lenient()
    };
    let (name, layer) = match name {
        Some(name) => (name.clone(), None),
        None => {
            let (name, layer) =
                rimz::room::reset_login_selection(&workspace.project_root, &machine, kind)?;
            (name, Some(layer))
        }
    };
    let login = LoginCatalog::from_config(&machine.accounts)?.select(kind, &name)?;
    login.preflight(&rimz::agents::ambient_env())?;
    let store = super::open_existing_store(&workspace)?
        .context("this room has no store; run `rimz start` first")?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let prior = store.switch_room_login(&workspace, kind, &name)?;
    let layer = layer.map(|layer| format!(" ({layer})")).unwrap_or_default();
    let mut out = render::out();
    if prior == name {
        return render::finish(writeln!(
            out,
            "this room already launches {kind} on `{name}`{layer}"
        ));
    }
    let count = snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.kind == *kind
                && agent.ended_at.is_none()
                && !agent.is_provider_subagent()
                && agent.login_key().name == prior
        })
        .count();
    let remaining = if count == 0 {
        format!("no running {kind} agent is on `{prior}`")
    } else {
        format!("{count} running {kind} agent(s) keep `{prior}` until they end")
    };
    render::finish(writeln!(
        out,
        "this room now launches {kind} on account `{name}`{layer}; {remaining}"
    ))
}

fn use_account(raw_kind: &str, name: &LoginName) -> Result<()> {
    // Clearing takes any kind, so it removes a hand-set entry for a kind
    // without named accounts, the fix its birth refusal names.
    let kind = &match account_kind(raw_kind) {
        Err(_) if name.is_default() => AgentKind::new_unchecked(raw_kind),
        kind => kind?,
    };
    let machine = MachineConfig::load()?;
    let login = match LoginCatalog::from_config(&machine.accounts)?.select(kind, name) {
        Ok(login) => Some(login),
        Err(_) if name.is_default() => None,
        Err(error) => return Err(error.into()),
    };
    ConfigEditor::machine().use_account(kind, name)?;
    let mut out = render::out();
    if name.is_default() {
        render::finish(writeln!(
            out,
            "new rooms now use {kind}'s own home (`default`); existing rooms, running or stopped, keep their recorded account until `rimz accounts use {kind} {name}` runs inside each"
        ))?;
    } else {
        render::finish(writeln!(
            out,
            "new rooms now use {kind} account `{name}`; existing rooms, running or stopped, keep their recorded account until `rimz accounts use {kind} {name}` runs inside each"
        ))?;
    }
    if let Some(Err(error)) = login.map(|login| login.preflight(&rimz::agents::ambient_env())) {
        writeln!(std::io::stderr().lock(), "rimz: warning: {error}")?;
    }
    Ok(())
}

#[derive(Serialize)]
struct AccountRow {
    kind: AgentKind,
    name: LoginName,
    home: Option<PathBuf>,
    /// `None` for `default` and for a selected account nothing declares.
    history: Option<AccountHistory>,
    machine_default: bool,
    /// Why a room cannot launch into this account, with the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    status: AccountStatus,
    /// A launch from here uses this account for its kind.
    active: bool,
    default_for: Scopes,
    /// Live agents on this account across every live room; `None` when the
    /// rooms could not be inventoried.
    agents: Option<usize>,
    /// The unscoped 5h then 7d window the provider projection holds, in used
    /// terms as `rimz providers --json` reports them.
    windows: Vec<RateLimitWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_credits: Option<ResetCredits>,
    /// `false` for an account without subscription windows.
    metered: Option<bool>,
    /// This run's login record; `None` for a row no declared login backs.
    #[serde(skip)]
    login: Option<ProviderStatus>,
}

impl AccountRow {
    /// Fold this run's reading of the row's login into it. A logged-out
    /// account whose setup is healthy takes the `logged out` status; a setup
    /// problem stands, since it must be fixed first.
    fn read(&mut self, reading: Option<&LoginReading>, login_command: impl FnOnce() -> String) {
        let status = reading.map_or(ProviderStatus::Unavailable, |reading| reading.status);
        self.login = Some(status);
        let Some(reading) = reading else {
            return;
        };
        self.metered = reading.metered;
        self.windows.clone_from(&reading.windows);
        self.reset_credits.clone_from(&reading.reset_credits);
        if self.status == AccountStatus::Ready && status == ProviderStatus::LoggedOut {
            self.status = AccountStatus::LoggedOut;
            self.problem = Some(format!(
                "{} account `{}` is logged out; log in once: {}",
                self.kind,
                self.name,
                login_command()
            ));
        }
    }

    /// Whether this run's record says the account has no login, or the row
    /// has no login at all: its windows are not its quota then.
    fn without_login(&self) -> bool {
        matches!(self.login, None | Some(ProviderStatus::LoggedOut))
    }
}

/// What one run read about a login: its probe record and its panel.
struct LoginReading {
    status: ProviderStatus,
    metered: Option<bool>,
    windows: Vec<RateLimitWindow>,
    reset_credits: Option<ResetCredits>,
}

/// The windows the list shows: the unscoped 5h and 7d ones, in that order.
fn list_windows(windows: &[RateLimitWindow]) -> Vec<RateLimitWindow> {
    WINDOW_SPANS
        .into_iter()
        .filter_map(|span| {
            windows
                .iter()
                .find(|window| {
                    window.scope.is_none() && window.duration_mins == Some(span.minutes())
                })
                .cloned()
        })
        .collect()
}

const WINDOW_SPANS: [WindowSpan; 2] = [WindowSpan::FiveHour, WindowSpan::SevenDay];

fn list(globals: &GlobalFlags, json: bool) -> Result<()> {
    let machine = MachineConfig::load()?;
    let catalog = LoginCatalog::from_config(&machine.accounts)?;
    let standing = match super::position_standing(globals, &machine) {
        Ok((standing, _in_room)) => standing,
        Err(error) => {
            writeln!(
                std::io::stderr().lock(),
                "rimz: warning: cannot read the account selection here, so no account is marked: {error:#}"
            )?;
            AccountStanding::unread(&machine)
        }
    };
    if let Some(blocked) = standing.blocked() {
        writeln!(std::io::stderr().lock(), "rimz: warning: {blocked}")?;
    }
    let agents = match rimz::room::live_agents_by_login() {
        Ok(agents) => Some(agents),
        Err(error) => {
            writeln!(
                std::io::stderr().lock(),
                "rimz: warning: cannot count live agents per account: {error}"
            )?;
            None
        }
    };
    let terminal = std::io::stdout().is_terminal();
    let readings = login_readings(&machine, &catalog, !json && terminal)?;
    let rows = account_rows(
        &machine.accounts,
        &catalog,
        &standing,
        &rimz::agents::ambient_env(),
        agents.as_ref(),
        &readings,
    );
    if json {
        return render::json_pretty(&rows);
    }
    let deciding = rows
        .iter()
        .filter_map(|row| Some((row.kind.clone(), standing.deciding(&row.kind)?)))
        .collect();
    let mut out = render::out();
    render::finish(write_accounts(
        &mut out,
        &rows,
        &deciding,
        Timestamp::now(),
        terminal.then(|| render::terminal_columns(120)),
    ))
}

/// Probe every listed login and refresh its usage on the cadence `rimz
/// providers` follows, then read each one's panel from the published caches.
fn login_readings(
    machine: &MachineConfig,
    catalog: &LoginCatalog,
    spin: bool,
) -> Result<BTreeMap<LoginKey, LoginReading>> {
    let runtime = RuntimePaths::shared();
    runtime
        .ensure_shared_dirs()
        .context("preparing shared provider cache paths")?;
    let logins: Vec<ProviderLogin> = catalog
        .all()
        .filter(|login| machine.accounts.named(login.kind()).is_some())
        .cloned()
        .collect();
    let spinner = spin.then(|| Spinner::delayed("Querying provider accounts", SPINNER_MIN_AGE));
    let started_ms = rimz::utils::time::unix_now_ms();
    let mut accounts = query_provider_accounts(&runtime, &logins, false);
    // A cached logout rides the ten-minute TTL, so it is probed again on
    // every run: a login made since the last list shows at once. So is an
    // older build's record that does not say which outcome it holds.
    let logged_out: Vec<ProviderLogin> = logins
        .iter()
        .filter(|login| {
            accounts.logins.get(&login.key()).is_some_and(|record| {
                record.probed_at_ms < started_ms
                    && (record.login_is_ambiguous()
                        || ProviderStatus::from_record(Some(record)) == ProviderStatus::LoggedOut)
            })
        })
        .cloned()
        .collect();
    if !logged_out.is_empty() {
        accounts = query_provider_accounts(&runtime, &logged_out, true);
    }
    if let Some(spinner) = &spinner {
        spinner.set("Refreshing provider usage");
    }
    for login in &logins {
        if ProviderStatus::from_record(accounts.logins.get(&login.key()))
            == ProviderStatus::LoggedIn
        {
            refresh_provider_usage(&runtime, login, false);
        }
    }
    drop(spinner);
    let spending = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    Ok(logins
        .iter()
        .map(|login| {
            let record = accounts.logins.get(&login.key());
            let account = record.and_then(|record| record.account.clone());
            let probed_metered = account.as_ref().and_then(|account| account.metered);
            let panel = provider_panel_for_login(
                &runtime,
                login,
                Some(catalog),
                machine,
                account,
                &spending,
            );
            let reading = LoginReading {
                reset_credits: panel.as_ref().and_then(|panel| panel.reset_credits.clone()),
                status: ProviderStatus::from_record(record),
                metered: panel.as_ref().map(|panel| panel.metered).or(probed_metered),
                windows: panel
                    .as_ref()
                    .map_or_else(Vec::new, |panel| list_windows(&panel.windows)),
            };
            (login.key(), reading)
        })
        .collect())
}

/// Every account of each kind that carries named accounts, `default` first,
/// plus a row for each account some layer selects but nothing declares.
fn account_rows(
    accounts: &AccountsConfig,
    catalog: &LoginCatalog,
    standing: &AccountStanding,
    ambient: &BTreeMap<String, String>,
    agents: Option<&BTreeMap<LoginKey, usize>>,
    readings: &BTreeMap<LoginKey, LoginReading>,
) -> Vec<AccountRow> {
    let row =
        |kind: &AgentKind, name: &LoginName, home, problem: Option<BirthLoginErr>| AccountRow {
            kind: kind.clone(),
            name: name.clone(),
            home,
            history: accounts
                .named(kind)
                .and_then(|declared| declared.get(name))
                .map(|account| account.history),
            machine_default: accounts.use_accounts.get(kind).cloned().unwrap_or_default() == *name,
            status: AccountStatus::of(problem.as_ref()),
            problem: problem.map(|err| err.to_string()),
            active: standing.active(kind).as_ref() == Some(name),
            default_for: standing.scopes(kind, name),
            agents: agents.map(|agents| {
                agents
                    .get(&LoginKey {
                        kind: kind.clone(),
                        name: name.clone(),
                    })
                    .copied()
                    .unwrap_or_default()
            }),
            windows: Vec::new(),
            reset_credits: None,
            metered: None,
            login: None,
        };
    let mut rows: Vec<AccountRow> = catalog
        .all()
        .filter(|login| accounts.named(login.kind()).is_some())
        .map(|login| {
            let ambient = if login.is_default() {
                catalog.native_ambient(login.kind(), ambient)
            } else {
                ambient.clone()
            };
            let mut row = row(
                login.kind(),
                login.name(),
                login.home_dir(&ambient),
                login.health(&ambient).err(),
            );
            row.read(readings.get(&login.key()), || {
                login_command(login, &ambient)
            });
            row
        })
        .collect();
    for key in standing.selected() {
        let problem = if standing.machine_selects(&key.kind, &key.name) {
            catalog.select_machine(&key.kind, &key.name).err()
        } else {
            catalog
                .select(&key.kind, &key.name)
                .err()
                .map(BirthLoginErr::from)
        };
        if let Some(problem) = problem {
            rows.push(row(&key.kind, &key.name, None, Some(problem)));
        }
    }
    rows.sort_by(|a, b| {
        (&a.kind, !a.name.is_default(), &a.name).cmp(&(&b.kind, !b.name.is_default(), &b.name))
    });
    rows
}

fn write_accounts(
    w: &mut impl std::io::Write,
    rows: &[AccountRow],
    deciding: &BTreeMap<AgentKind, Deciding>,
    now: Timestamp,
    width: Option<usize>,
) -> std::io::Result<()> {
    let mut table = render::Table::new([
        "", "KIND", "NAME", "STATUS", "5h LEFT", "7d LEFT", "CREDITS", "AGENTS", "HISTORY", "HOME",
    ]);
    if let Some(width) = width {
        table = table.max_width(width);
    }
    let mut previous: Option<&AgentKind> = None;
    for row in rows {
        if previous.is_some_and(|kind| *kind != row.kind) {
            table.blank();
        }
        previous = Some(&row.kind);
        let muted = |cell: render::Cell| {
            if row.name.is_default() {
                cell.fg(render::palette::muted())
            } else {
                cell
            }
        };
        let [five_hour, seven_day] = WINDOW_SPANS.map(|span| window_cell(row, span, now));
        table.row([
            if row.active {
                render::cell("●").fg(render::palette::accent())
            } else if row.machine_default {
                render::cell("○").fg(render::palette::muted())
            } else {
                render::cell("")
            },
            render::cell(row.kind.as_str()).fg(render::palette::identity(row.kind.as_str())),
            muted(render::cell(row.name.as_str())),
            render::cell(row.status.as_str()).fg(render::status::account(row.status)),
            five_hour,
            seven_day,
            if row.without_login() {
                unknown_cell()
            } else {
                match row.reset_credits.as_ref().map(|credits| credits.count) {
                    None => unknown_cell(),
                    Some(0) => render::cell("-").dash(),
                    Some(count) => render::cell(count.to_string()),
                }
            },
            match row.agents {
                None => unknown_cell(),
                Some(0) => render::cell("-").dash(),
                Some(count) => render::cell(count.to_string()),
            },
            row.history.map_or_else(
                || render::cell("-").dash(),
                |history| render::cell(history.as_str()),
            ),
            row.home.as_ref().map_or_else(
                || render::cell("-").dash(),
                |home| {
                    muted(render::cell(render::home_relative(
                        &home.display().to_string(),
                    )))
                },
            ),
        ]);
    }
    table.render(w)?;
    if let Some(legend) = legend(rows, deciding) {
        writeln!(w, "{legend}")?;
    }
    for problem in rows.iter().filter_map(|row| row.problem.as_deref()) {
        writeln!(w, "{}", render::paint(render::palette::warn(), problem))?;
    }
    Ok(())
}

fn unknown_cell() -> render::Cell {
    render::cell("–").fg(render::palette::faint())
}

/// One window's cell in left terms: `–` without a login or a reading, `∞`
/// for an account without subscription windows or a lifted limit.
fn window_cell(row: &AccountRow, span: WindowSpan, now: Timestamp) -> render::Cell {
    if row.without_login() {
        return unknown_cell();
    }
    if row.metered == Some(false) {
        return render::cell("∞");
    }
    row.windows
        .iter()
        .find(|window| window.duration_mins == Some(span.minutes()))
        .and_then(|window| render::window_cell(window, now))
        .unwrap_or_else(unknown_cell)
}

/// What the two markers mean here: `●` by the highest layer deciding any
/// kind shown, `○` wherever a row carries it or a room or project decides.
fn legend(rows: &[AccountRow], deciding: &BTreeMap<AgentKind, Deciding>) -> Option<String> {
    let layer = [Deciding::Room, Deciding::Project, Deciding::Machine]
        .into_iter()
        .find(|layer| {
            rows.iter()
                .any(|row| deciding.get(&row.kind) == Some(layer))
        });
    let active = layer.map(|layer| {
        let words = match layer {
            Deciding::Room => "this room",
            Deciding::Project => "this project",
            Deciding::Machine => "new rooms",
        };
        format!("{}  {words}", render::paint(render::palette::accent(), "●"))
    });
    let new_rooms = (matches!(layer, Some(Deciding::Room | Deciding::Project))
        || rows.iter().any(|row| row.machine_default && !row.active))
    .then(|| {
        format!(
            "{}  new rooms",
            render::paint(render::palette::muted(), "○")
        )
    });
    let halves: Vec<String> = active.into_iter().chain(new_rooms).collect();
    (!halves.is_empty()).then(|| halves.join("   "))
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
            "cleared the machine selection; new rooms now use {kind} account `default`"
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

/// Live rooms whose default for new launches still names this account; not an inventory of session stamps.
fn live_rooms_selecting(kind: &AgentKind, name: &LoginName) -> Vec<String> {
    let inventory = match rimz::room::session::room_inventory() {
        Ok(inventory) => inventory,
        Err(err) => {
            tracing::debug!(%err, "could not inventory rooms before removing account");
            return Vec::new();
        }
    };
    let mut rooms = Vec::new();
    for room in inventory.live {
        let selection = (|| -> Result<RoomLogins> {
            let paths = rimz::StatePaths::for_workspace(room.workspace_id)?;
            Ok(rimz::agents::room_logins(&paths.workspace_record)?)
        })();
        match selection {
            Ok(logins) if logins.get(kind) == Some(name) => rooms.push(room.session_name),
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(%err, session = %room.session_name, "could not read room accounts before removing account");
            }
        }
    }
    rooms
}

/// What `remove` prints, given the account, its displayed home, and the live rooms still selecting it.
fn removed_notice(kind: &AgentKind, name: &LoginName, home: &str, live: &[String]) -> String {
    let mut notice = format!(
        "removed {kind} account `{name}`; its home {home} and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use {kind} default` for future launches"
    );
    if !live.is_empty() {
        let (room, verb) = if live.len() == 1 {
            ("room", "selects")
        } else {
            ("rooms", "select")
        };
        notice.push_str(&format!(
            "\nwarning: {room} {} {verb} it as the default for new {kind} launches; add the account back or run `rimz accounts use {kind} default` inside each room",
            live.join(", ")
        ));
    }
    notice
}

/// One `--account <kind>=<name>` flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountFlag {
    kind: AgentKind,
    name: LoginName,
}

pub(crate) fn parse_account_flag(raw: &str) -> std::result::Result<AccountFlag, String> {
    let Some((kind, name)) = raw.split_once('=') else {
        return Err(format!("expected `<kind>=<name>`, got `{raw}`"));
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

#[cfg(test)]
mod tests;
