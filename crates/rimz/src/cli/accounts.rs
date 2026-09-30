//! Provider accounts at the command line: `rimz accounts add|use|list|remove`,
//! and the `--account <kind>=<name>` selection `rimz start` and `rimz reset`
//! pass to a room's birth.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use rimz::agents::{BirthLoginErr, LoginCatalog, ProviderLogin};
use rimz::config::{AccountsConfig, ConfigEditor, MachineConfig, NamedAccount};
use rimz::ids::{AgentKind, LoginName, RoomLogins};
use rimz::utils::path::normalize_path_lexical;
use serde::Serialize;

use super::{GlobalFlags, render};

#[derive(Debug, Args)]
pub struct AccountsArgs {
    #[command(subcommand)]
    command: AccountsSubcmd,
}

#[derive(Debug, Subcommand)]
enum AccountsSubcmd {
    /// Select the account for new rooms, or future launches here with --room.
    Use {
        /// Change this room's default for future launches.
        #[arg(long)]
        room: bool,
        /// Provider kind: claude or codex.
        kind: String,
        /// Declared account name, or `default` for the provider's own home.
        name: LoginName,
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
    },
    /// List every account with its home and whether a room can launch into it.
    List {
        /// Emit the accounts as JSON.
        #[arg(long)]
        json: bool,
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
        AccountsSubcmd::Add { kind, name, home } => add(&account_kind(&kind)?, name, home),
        AccountsSubcmd::List { json } => list(json),
        AccountsSubcmd::Remove { kind, name } => remove(&account_kind(&kind)?, &name),
        AccountsSubcmd::Use { kind, name, room } => {
            if room {
                use_room_account(globals, &account_kind(&kind)?, &name)
            } else {
                use_account(&kind, &name)
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

fn add(kind: &AgentKind, name: LoginName, home: Option<PathBuf>) -> Result<()> {
    if name.is_default() {
        bail!("`default` is {kind}'s own home and needs no declaring; choose another name");
    }
    let home = home
        .map(std::path::absolute)
        .transpose()
        .context("resolving --home")?;
    let ambient = rimz::agents::ambient_env();
    let machine = MachineConfig::load()?;
    let catalog = LoginCatalog::from_config(&machine.accounts)?;
    let existing = catalog.select(kind, &name).ok();
    let declaring = existing.is_none();
    let login = match (existing, home.as_ref()) {
        (Some(existing), None) => existing,
        (existing, home) => {
            let mut accounts = machine.accounts.clone();
            if let Some(declared) = accounts.named_mut(kind) {
                declared.insert(
                    name.clone(),
                    NamedAccount {
                        home: home.cloned(),
                    },
                );
            }
            let login = LoginCatalog::from_config(&accounts)?.select(kind, &name)?;
            match existing {
                Some(existing) if lexical_home(&existing) != lexical_home(&login) => bail!(
                    "{kind} account `{name}` already lives at `{}`; rerun without --home, or remove the account first",
                    existing
                        .home()
                        .unwrap_or(std::path::Path::new(""))
                        .display()
                ),
                Some(existing) => existing,
                None => login,
            }
        }
    };
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
    let default_home = ProviderLogin::default_for(kind.clone())
        .home_dir(&ambient)
        .with_context(|| format!("cannot resolve {kind}'s own home; set HOME"))?;
    // Sound: `select` answers a non-default name only with a declared home.
    let named_home = login.home().expect("a named account has a home");
    rimz::agents::account_links::check_distinct_homes(named_home, &default_home)?;
    if declaring {
        ConfigEditor::machine().upsert_named_account(kind, &name, home.as_deref())?;
    }
    let home = named_home;
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating {kind} account home {}", home.display()))?;
    let definition = rimz::agents::definition_by_kind(kind.as_str())?;
    let shared = rimz::agents::account_links::share_settings(definition, home, &default_home)?;
    let mut out = render::out();
    writeln!(out, "{shared}")?;
    crate::cli::hooks::install_hooks_into(definition, &login.env(&ambient), &mut out)?;
    let home_override = login
        .env(&BTreeMap::new())
        .into_iter()
        .map(|(key, value)| {
            // The home was just created, so it holds no NUL byte.
            let value = shlex::try_quote(&value).expect("an existing path is shell-quotable");
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    render::finish(writeln!(
        out,
        "{kind} account `{name}` lives at {}\n  log in once   {home_override} {kind}\n  use at birth  rimz start --account {kind}={name}\n  this room     rimz accounts use --room {kind} {name}\n  new rooms     rimz accounts use {kind} {name}",
        render::home_relative(&home.display().to_string())
    ))
}

fn lexical_home(login: &ProviderLogin) -> Option<PathBuf> {
    login.home().map(normalize_path_lexical)
}

fn use_room_account(globals: &GlobalFlags, kind: &AgentKind, name: &LoginName) -> Result<()> {
    let pin = std::env::var(rimz::workspace::ENV_WORKSPACE_ID)
        .ok()
        .zip(std::env::var_os(rimz::workspace::ENV_PROJECT_ROOT))
        .and_then(|(id, root)| rimz::workspace::verify_pin(&id, &PathBuf::from(root)));
    let root = pin.context(
        "--room needs a running room; run it inside one, or drop --room to set the machine default",
    )?;
    if let Some(override_root) = &globals.root
        && override_root.canonicalize()? != root
    {
        bail!(
            "--room switches the current room at `{}`; --root `{}` names another room; drop --root or run the command inside that room",
            root.display(),
            override_root.display()
        );
    }
    let ctx = super::ctx::Ctx::open(globals)?;
    let machine = MachineConfig::load()?;
    let login = LoginCatalog::from_config(&machine.accounts)?.select(kind, name)?;
    login.preflight(&rimz::agents::ambient_env())?;
    let snapshot = ctx.cached_snapshot()?;
    let prior = ctx.store.switch_room_login(&ctx.workspace, kind, name)?;
    let mut out = render::out();
    if prior == *name {
        return render::finish(writeln!(
            out,
            "this room already launches {kind} on `{name}`"
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
        "this room now launches {kind} on account `{name}`; {remaining}"
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
            "new rooms now use {kind}'s own home (`default`); a running room keeps its account until `rimz accounts use --room {kind} {name}` runs inside it"
        ))?;
    } else {
        render::finish(writeln!(
            out,
            "new rooms now use {kind} account `{name}`; a running room keeps its account until `rimz accounts use --room {kind} {name}` runs inside it"
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
    machine_default: bool,
    /// Why a room cannot launch into this account, with the fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    #[serde(skip)]
    status: &'static str,
}

fn list(json: bool) -> Result<()> {
    let machine = MachineConfig::load()?;
    let catalog = LoginCatalog::from_config(&machine.accounts)?;
    let ambient = rimz::agents::ambient_env();
    let mut rows: Vec<AccountRow> = catalog
        .all()
        .filter(|login| machine.accounts.named(login.kind()).is_some())
        .map(|login| {
            let problem = login.preflight(&ambient).err();
            AccountRow {
                kind: login.kind().clone(),
                name: login.name().clone(),
                home: login.home_dir(&ambient),
                machine_default: machine
                    .accounts
                    .use_accounts
                    .get(login.kind())
                    .cloned()
                    .unwrap_or_default()
                    == *login.name(),
                status: match &problem {
                    None if login.is_default() => "native",
                    None => "ready",
                    Some(BirthLoginErr::MissingHome { .. }) => "home missing",
                    Some(BirthLoginErr::HooksMissing { .. }) => "hooks missing",
                    Some(BirthLoginErr::HooksUntrusted { .. }) => "hooks untrusted",
                    Some(
                        BirthLoginErr::Frozen { .. }
                        | BirthLoginErr::Login(_)
                        | BirthLoginErr::MachineUnknown { .. }
                        | BirthLoginErr::MachineUnsupported { .. },
                    ) => "unavailable",
                },
                problem: problem.map(|err| err.to_string()),
            }
        })
        .collect();
    for (kind, name) in &machine.accounts.use_accounts {
        if let Err(problem) = catalog.select_machine(kind, name) {
            rows.push(AccountRow {
                kind: kind.clone(),
                name: name.clone(),
                home: None,
                machine_default: true,
                status: "unavailable",
                problem: Some(problem.to_string()),
            });
        }
    }
    if json {
        return render::json_pretty(&rows);
    }
    let mut table = render::Table::new(["KIND", "NAME", "HOME", "STATUS", "NEW ROOMS"]);
    for row in &rows {
        let status = render::cell(row.status);
        table.row([
            render::cell(row.kind.as_str()),
            render::cell(row.name.as_str()),
            row.home.as_ref().map_or_else(
                || render::cell("-").dash(),
                |home| render::cell(render::home_relative(&home.display().to_string())),
            ),
            if row.problem.is_some() {
                status.fg(render::palette::warn())
            } else {
                status
            },
            render::cell(if row.machine_default { "yes" } else { "-" }),
        ]);
    }
    let mut out = render::out();
    render::finish(table.render(&mut out))?;
    for problem in rows.iter().filter_map(|row| row.problem.as_deref()) {
        render::finish(writeln!(
            out,
            "{}",
            render::paint(render::palette::warn(), problem)
        ))?;
    }
    Ok(())
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
        "removed {kind} account `{name}`; its home {home} and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use --room {kind} default` for future launches"
    );
    if !live.is_empty() {
        let (room, verb) = if live.len() == 1 {
            ("room", "selects")
        } else {
            ("rooms", "select")
        };
        notice.push_str(&format!(
            "\nwarning: {room} {} {verb} it as the default for new {kind} launches; add the account back or run `rimz accounts use --room {kind} default` inside each room",
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
mod tests {
    use super::*;

    #[test]
    fn removed_notice_without_live_rooms_matches_reference() {
        assert_eq!(
            removed_notice(
                &AgentKind::new_unchecked("claude"),
                &"work".parse().unwrap(),
                "~/.rimz/accounts/claude/work",
                &[]
            ),
            "removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use --room claude default` for future launches"
        );
    }

    #[test]
    fn removed_notice_warns_about_live_room_defaults() {
        for (live, warning) in [
            (
                vec!["rimz-one".to_owned()],
                "warning: room rimz-one selects it as the default for new claude launches; add the account back or run `rimz accounts use --room claude default` inside each room",
            ),
            (
                vec!["rimz-one".to_owned(), "rimz-two".to_owned()],
                "warning: rooms rimz-one, rimz-two select it as the default for new claude launches; add the account back or run `rimz accounts use --room claude default` inside each room",
            ),
        ] {
            let notice = removed_notice(
                &AgentKind::new_unchecked("claude"),
                &"work".parse().unwrap(),
                "~/.rimz/accounts/claude/work",
                &live,
            );
            assert_eq!(notice.lines().nth(1), Some(warning));
            assert_eq!(notice.lines().count(), 2);
        }
    }

    #[test]
    fn account_flags_parse_kind_and_name_and_refuse_a_repeated_kind() {
        let work = parse_account_flag("claude=work").expect("claude=work");
        let default = parse_account_flag("codex=default").expect("codex=default");
        assert_eq!(
            requested_logins(&[work.clone(), default]).expect("one per kind"),
            RoomLogins::from([
                (AgentKind::new_unchecked("claude"), "work".parse().unwrap()),
                (
                    AgentKind::new_unchecked("codex"),
                    LoginName::default_login()
                ),
            ])
        );
        assert!(parse_account_flag("claude").is_err());
        assert!(parse_account_flag("nope=work").is_err());
        assert!(parse_account_flag("claude=Work").is_err());

        let personal = parse_account_flag("claude=personal").expect("claude=personal");
        let err = requested_logins(&[work, personal]).unwrap_err();
        assert!(err.to_string().contains("names claude twice"), "{err}");
    }
}
