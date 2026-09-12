//! Provider accounts at the command line: `rimz accounts add|list|remove`,
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

pub fn run(args: AccountsArgs, _globals: &GlobalFlags) -> Result<()> {
    match args.command {
        AccountsSubcmd::Add { kind, name, home } => add(&account_kind(&kind)?, name, home),
        AccountsSubcmd::List { json } => list(json),
        AccountsSubcmd::Remove { kind, name } => remove(&account_kind(&kind)?, &name),
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
    let machine = MachineConfig::load()?;
    let existing = LoginCatalog::from_config(&machine.accounts)?
        .select(kind, &name)
        .ok();
    let login = match (existing, home) {
        (Some(existing), None) => existing,
        (existing, home) => {
            let mut accounts = machine.accounts.clone();
            if let Some(declared) = accounts.named_mut(kind) {
                declared.insert(name.clone(), NamedAccount { home: home.clone() });
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
                None => {
                    ConfigEditor::machine().upsert_named_account(kind, &name, home.as_deref())?;
                    login
                }
            }
        }
    };
    // Sound: `select` answers a non-default name only with a declared home.
    let home = login.home().expect("a named account has a home");
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating {kind} account home {}", home.display()))?;
    let definition = rimz::agents::definition_by_kind(kind.as_str())?;
    let mut out = render::out();
    crate::cli::hooks::install_hooks_into(
        definition,
        &login.env(&rimz::agents::ambient_env()),
        &mut out,
    )?;
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
        "{kind} account `{name}` lives at {}\n  log in once   {home_override} {kind}\n  use it        rimz start --account {kind}={name}",
        render::home_relative(&home.display().to_string())
    ))
}

fn lexical_home(login: &ProviderLogin) -> Option<PathBuf> {
    login.home().map(normalize_path_lexical)
}

#[derive(Serialize)]
struct AccountRow {
    kind: AgentKind,
    name: LoginName,
    home: Option<PathBuf>,
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
    let rows: Vec<AccountRow> = catalog
        .all()
        .filter(|login| machine.accounts.named(login.kind()).is_some())
        .map(|login| {
            let problem = login.preflight(&ambient).err();
            AccountRow {
                kind: login.kind().clone(),
                name: login.name().clone(),
                home: login.home_dir(&ambient),
                status: match &problem {
                    None if login.is_default() => "native",
                    None => "ready",
                    Some(BirthLoginErr::MissingHome { .. }) => "home missing",
                    Some(BirthLoginErr::HooksMissing { .. }) => "hooks missing",
                    Some(BirthLoginErr::HooksUntrusted { .. }) => "hooks untrusted",
                    Some(BirthLoginErr::Frozen { .. } | BirthLoginErr::Login(_)) => "unavailable",
                },
                problem: problem.map(|err| err.to_string()),
            }
        })
        .collect();
    if json {
        return render::json_pretty(&rows);
    }
    let mut table = render::Table::new(["KIND", "NAME", "HOME", "STATUS"]);
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
    let home = LoginCatalog::from_config(&MachineConfig::load()?.accounts)?
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
    render::finish(writeln!(
        out,
        "removed {kind} account `{name}`; its home {} and the provider files in it stay on disk, and a room still using it refuses to start until `rimz reset`",
        render::home_relative(home.as_deref().unwrap_or_default())
    ))
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
