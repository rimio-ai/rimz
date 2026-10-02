//! Provider logins: the resolution of a room's *account* selection into the
//! environment a provider reads its home from.
//!
//! A login is a standalone provider home. `default` is the provider's own
//! native resolution and carries no path; every other name is a directory the
//! user declared under `[accounts.<kind>.<name>]`. RimZ never swaps a home in
//! place and never sets a process-wide variable: a login is expressed only as
//! an override on top of an ambient environment map, and every home-resolving
//! read — launch, host-side discovery, install — resolves that map through the
//! adapter's own [`LaunchCapability::config_home`].
//!
//! [`LaunchCapability::config_home`]: crate::agents::capabilities::LaunchCapability::config_home

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{AccountHistory, AccountsConfig};
use crate::ids::{AgentKind, LoginKey, LoginName, RoomLogins};
use crate::utils::path::normalize_path_lexical;

/// Selecting a login that the machine config does not describe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginErr {
    #[error(
        "unknown {kind} account `{name}`; configured: {}; run `rimz accounts add {kind} {name}`",
        render_names(configured)
    )]
    Unknown {
        kind: AgentKind,
        name: LoginName,
        configured: Vec<LoginName>,
    },
    #[error("{kind} has no named accounts; only `default` is available")]
    Unsupported { kind: AgentKind },
}

/// A machine-config `[accounts.<kind>.<name>]` entry RimZ cannot honour.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginConfigErr {
    #[error(
        "`accounts.{kind}.default` is reserved for the provider's own home; remove it and declare another name"
    )]
    ReservedName { kind: AgentKind },
    #[error("`accounts.{kind}.{name}.home` must be an absolute path, not `{home}`")]
    RelativeHome {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
    },
    #[error(
        "`accounts.{kind}.{name}.home` is `{home}`; provider home lists split on `,` and Claude reads a trailing `projects` as its transcript folder, so choose a directory without either"
    )]
    AmbiguousHome {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
    },
    #[error(
        "`accounts.{kind}.{name}.home` is `{home}`, already used by `accounts.{kind}.{first}`; give each account its own directory"
    )]
    DuplicateHome {
        kind: AgentKind,
        name: LoginName,
        first: LoginName,
        home: PathBuf,
    },
    #[error(
        "`accounts.{kind}.{name}.home` is `{home}`, the provider's own home; that one is already the `default` account"
    )]
    NativeHome {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
    },
    #[error(
        "`{env_key}` is exported as `{home}`, the home of {kind} account `{name}`, so the `default` account launches into it too; unset `{env_key}` and run `rimz accounts use --global {kind} {name}` to start new rooms on it"
    )]
    ExportedHome {
        kind: AgentKind,
        name: LoginName,
        env_key: &'static str,
        home: PathBuf,
    },
}

fn render_names(names: &[LoginName]) -> String {
    names
        .iter()
        .map(LoginName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A resolved account of one provider kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderLogin {
    kind: AgentKind,
    name: LoginName,
    home: Option<NamedHome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NamedHome {
    path: PathBuf,
    env_key: &'static str,
}

impl ProviderLogin {
    /// The provider's native home.
    pub fn default_for(kind: AgentKind) -> Self {
        Self {
            kind,
            name: LoginName::default_login(),
            home: None,
        }
    }

    /// A declared account launching into `home`.
    pub fn named(kind: AgentKind, name: LoginName, home: PathBuf) -> Result<Self, LoginErr> {
        let env_key = config_home_env_key(&kind)
            .ok_or_else(|| LoginErr::Unsupported { kind: kind.clone() })?;
        Ok(Self {
            kind,
            name,
            home: Some(NamedHome {
                path: home,
                env_key,
            }),
        })
    }

    pub fn kind(&self) -> &AgentKind {
        &self.kind
    }

    pub fn name(&self) -> &LoginName {
        &self.name
    }

    pub fn key(&self) -> LoginKey {
        LoginKey::new(self.kind.clone(), self.name.clone())
    }

    pub fn is_default(&self) -> bool {
        self.home.is_none()
    }

    /// The declared home, or `None` when the provider resolves its own.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_ref().map(|home| home.path.as_path())
    }

    /// `ambient`, with this login's home override applied. The default login
    /// leaves the ambient environment exactly as it is, so today's resolution
    /// policies — comma lists, XDG order, test overrides — keep running.
    pub fn env(&self, ambient: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = ambient.clone();
        if let Some(home) = &self.home {
            env.insert(
                home.env_key.to_owned(),
                home.path.to_string_lossy().into_owned(),
            );
        }
        env
    }

    /// The directory this login's provider reads its config from, as the
    /// adapter itself resolves it.
    pub fn home_dir(&self, ambient: &BTreeMap<String, String>) -> Option<PathBuf> {
        crate::agents::find_definition(self.kind.as_str())?.config_home(&self.env(ambient))
    }

    /// Refuses a named account whose home the exported provider variable
    /// (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`) already names: `default` resolves to
    /// that home too, so two logins would share one provider home. The catalog
    /// cannot check this at load, since a room pane on this account exports its
    /// home by design; `rimz accounts add` and `rimz doctor` ask instead.
    pub fn check_exported_home(
        &self,
        ambient: &BTreeMap<String, String>,
    ) -> Result<(), LoginConfigErr> {
        let (Some(home), Some(env_key)) = (self.home(), config_home_env_key(&self.kind)) else {
            return Ok(());
        };
        if !ambient.get(env_key).is_some_and(|value| !value.is_empty()) {
            return Ok(());
        }
        let default = Self::default_for(self.kind.clone()).home_dir(ambient);
        let same_home =
            default.is_some_and(|path| match (path.canonicalize(), home.canonicalize()) {
                (Ok(default), Ok(named)) => default == named,
                _ => normalize_path_lexical(&path) == normalize_path_lexical(home),
            });
        if !same_home {
            return Ok(());
        }
        Err(LoginConfigErr::ExportedHome {
            kind: self.kind.clone(),
            name: self.name.clone(),
            env_key,
            home: home.to_path_buf(),
        })
    }
}

fn config_home_env_key(kind: &AgentKind) -> Option<&'static str> {
    match crate::agents::find_definition(kind.as_str())?.config_home_env_keys() {
        [key] => Some(key),
        _ => None,
    }
}

/// Where RimZ puts an account home the user did not place itself:
/// `<home>/accounts/<kind>/<name>`. It carries provider credentials and
/// history, not RimZ data, so nothing in RimZ removes it.
pub fn default_named_home(kind: &AgentKind, name: &LoginName) -> PathBuf {
    crate::disk::paths::accounts_dir()
        .join(kind.as_str())
        .join(name.as_str())
}

/// Every login this machine knows: one `default` per registered kind, plus
/// every declared account.
#[derive(Clone, Debug, Default)]
pub struct LoginCatalog {
    logins: BTreeMap<LoginKey, ProviderLogin>,
    account_kinds: BTreeSet<AgentKind>,
    standalone: BTreeSet<LoginKey>,
}

impl LoginCatalog {
    pub fn from_config(accounts: &AccountsConfig) -> Result<Self, LoginConfigErr> {
        Self::from_config_under(
            accounts,
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        )
    }

    fn from_config_under(
        accounts: &AccountsConfig,
        home: Option<&Path>,
    ) -> Result<Self, LoginConfigErr> {
        let mut logins = BTreeMap::new();
        let mut account_kinds = BTreeSet::new();
        let mut standalone = BTreeSet::new();
        for kind in crate::agents::known_kinds().map(AgentKind::new_unchecked) {
            let key = LoginKey::default_for(kind.clone());
            logins.insert(key, ProviderLogin::default_for(kind.clone()));
            let Some(declared) = accounts.named(&kind) else {
                continue;
            };
            account_kinds.insert(kind.clone());
            let native = native_home(&kind, home).map(|path| normalize_path_lexical(&path));
            let mut claimed: BTreeMap<PathBuf, LoginName> = BTreeMap::new();
            for (name, account) in declared {
                if name.is_default() {
                    return Err(LoginConfigErr::ReservedName { kind });
                }
                let declared_home = account
                    .home
                    .clone()
                    .map(|home| crate::agents::transcript_fs::expand_tilde(&home.to_string_lossy()))
                    .unwrap_or_else(|| default_named_home(&kind, name));
                if !declared_home.is_absolute() {
                    return Err(LoginConfigErr::RelativeHome {
                        kind,
                        name: name.clone(),
                        home: declared_home,
                    });
                }
                let normalized = normalize_path_lexical(&declared_home);
                if normalized.to_string_lossy().contains(',')
                    || normalized
                        .file_name()
                        .is_some_and(|last| last == "projects")
                {
                    return Err(LoginConfigErr::AmbiguousHome {
                        kind,
                        name: name.clone(),
                        home: declared_home,
                    });
                }
                if native.as_ref() == Some(&normalized) {
                    return Err(LoginConfigErr::NativeHome {
                        kind,
                        name: name.clone(),
                        home: declared_home,
                    });
                }
                if let Some(first) = claimed.get(&normalized) {
                    return Err(LoginConfigErr::DuplicateHome {
                        kind,
                        name: name.clone(),
                        first: first.clone(),
                        home: declared_home,
                    });
                }
                claimed.insert(normalized, name.clone());
                // Sound: `accounts.named` answers for exactly the kinds whose
                // adapter declares one home override key.
                let login = ProviderLogin::named(kind.clone(), name.clone(), declared_home)
                    .expect("named accounts are configurable only for kinds with a home override");
                if account.history == AccountHistory::Standalone {
                    standalone.insert(login.key());
                }
                logins.insert(login.key(), login);
            }
        }
        Ok(Self {
            logins,
            account_kinds,
            standalone,
        })
    }

    /// Resolve a machine `[accounts.use]` entry; the error names that source and both fixes.
    pub fn select_machine(
        &self,
        kind: &AgentKind,
        name: &LoginName,
    ) -> Result<ProviderLogin, BirthLoginErr> {
        let path = crate::config::MachineConfig::config_path();
        self.select(kind, name).map_err(|source| match source {
            LoginErr::Unknown {
                kind,
                name,
                configured,
            } => BirthLoginErr::MachineUnknown {
                kind,
                name,
                configured,
                path,
            },
            LoginErr::Unsupported { kind } => BirthLoginErr::MachineUnsupported {
                kind,
                name: name.clone(),
                path,
            },
        })
    }

    /// The login a kind launches under when the room names `name`.
    pub fn select(&self, kind: &AgentKind, name: &LoginName) -> Result<ProviderLogin, LoginErr> {
        self.logins
            .get(&LoginKey::new(kind.clone(), name.clone()))
            .cloned()
            .ok_or_else(|| {
                if name.is_default() || !self.account_kinds.contains(kind) {
                    LoginErr::Unsupported { kind: kind.clone() }
                } else {
                    LoginErr::Unknown {
                        kind: kind.clone(),
                        name: name.clone(),
                        configured: self.names(kind),
                    }
                }
            })
    }

    /// The login of every registered kind under a room's selection: the named
    /// one where the room froze one, `default` everywhere else.
    pub fn room(&self, selection: &RoomLogins) -> Result<Vec<ProviderLogin>, LoginErr> {
        self.logins
            .values()
            .filter(|login| login.is_default())
            .map(|login| match selection.get(login.kind()) {
                Some(name) if !name.is_default() => self.select(login.kind(), name),
                _ => Ok(login.clone()),
            })
            .collect()
    }

    /// The login one kind launches under, under a room's selection.
    pub fn room_login(
        &self,
        selection: &RoomLogins,
        kind: &AgentKind,
    ) -> Result<ProviderLogin, LoginErr> {
        match selection.get(kind) {
            Some(name) if !name.is_default() => self.select(kind, name),
            _ => Ok(ProviderLogin::default_for(kind.clone())),
        }
    }

    /// The selection a room is born with. A recorded selection stands, and a
    /// requested account that disagrees with it is refused; otherwise the
    /// requested accounts win over the project's, then the machine's, then `default`.
    /// Every kind that can carry an account gets an explicit entry, so the
    /// recorded selection reads the same whichever layer chose it.
    pub fn birth_selection(
        &self,
        frozen: Option<&RoomLogins>,
        requested: &RoomLogins,
        project: &RoomLogins,
        machine: &RoomLogins,
    ) -> Result<RoomLogins, BirthLoginErr> {
        let chosen_project = project
            .iter()
            .filter(|(kind, _)| !requested.contains_key(*kind));
        for (kind, name) in requested.iter().chain(chosen_project) {
            self.select(kind, name)?;
        }
        if let Some(frozen) = frozen {
            for (kind, name) in requested {
                let current = frozen.get(kind).cloned().unwrap_or_default();
                if &current != name {
                    return Err(BirthLoginErr::Frozen {
                        kind: kind.clone(),
                        current,
                        requested: name.clone(),
                    });
                }
            }
            return Ok(frozen.clone());
        }
        for (kind, name) in machine
            .iter()
            .filter(|(kind, _)| !requested.contains_key(*kind) && !project.contains_key(*kind))
        {
            self.select_machine(kind, name)?;
        }
        Ok(self
            .account_kinds
            .iter()
            .map(|kind| (kind.clone(), birth_name(kind, requested, project, machine)))
            .collect())
    }

    /// `ambient` as `kind`'s own home reads it: an exported home override
    /// that names a declared account's home is dropped, since a pane born on
    /// that account exports it by design and `default` is not that account.
    pub fn native_ambient(
        &self,
        kind: &AgentKind,
        ambient: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let exported = self
            .logins
            .values()
            .filter(|login| login.kind() == kind)
            .find_map(|login| match login.check_exported_home(ambient) {
                Err(LoginConfigErr::ExportedHome { env_key, .. }) => Some(env_key),
                _ => None,
            });
        let mut native = ambient.clone();
        if let Some(env_key) = exported {
            native.remove(env_key);
        }
        native
    }

    /// The history pool a login belongs to: `<kind>@default` for the default
    /// and every shared account of the kind, the login's own key for a
    /// standalone account and for a name the config does not declare.
    pub fn pool(&self, login: &LoginKey) -> LoginKey {
        if self.logins.contains_key(login) && !self.standalone.contains(login) {
            LoginKey::default_for(login.kind.clone())
        } else {
            login.clone()
        }
    }

    /// Every login, defaults included, in `<kind>@<name>` order.
    pub fn all(&self) -> impl Iterator<Item = &ProviderLogin> {
        self.logins.values()
    }

    /// The declared names of a kind, `default` first.
    fn names(&self, kind: &AgentKind) -> Vec<LoginName> {
        self.logins
            .values()
            .filter(|login| login.kind() == kind)
            .map(|login| login.name().clone())
            .collect()
    }
}

/// The account a fresh room launches `kind` under: the requested one, then
/// the project's, then the machine's, then `default`.
pub(crate) fn birth_name(
    kind: &AgentKind,
    requested: &RoomLogins,
    project: &RoomLogins,
    machine: &RoomLogins,
) -> LoginName {
    requested
        .get(kind)
        .or_else(|| project.get(kind))
        .or_else(|| machine.get(kind))
        .cloned()
        .unwrap_or_default()
}

/// The command that repairs an account's home: `default` is the provider's
/// own home, which `rimz hooks install` wires; a named one is re-added.
fn home_fix(kind: &AgentKind, name: &LoginName) -> String {
    if name.is_default() {
        format!("rimz hooks install {kind}")
    } else {
        format!("rimz accounts add {kind} {name}")
    }
}

fn started_as(env_key: Option<&str>, home: &Path, kind: &AgentKind) -> String {
    env_key.map_or_else(String::new, |env_key| {
        format!(", started as `{env_key}={} {kind}`", home.display())
    })
}

/// Whether a room can launch into an account, in the words `rimz accounts
/// list` and `rimz doctor` show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Ready,
    HomeMissing,
    HooksMissing,
    HooksUntrusted,
    Unavailable,
}

impl AccountStatus {
    pub fn of(problem: Option<&BirthLoginErr>) -> Self {
        match problem {
            None => Self::Ready,
            Some(BirthLoginErr::MissingHome { .. }) => Self::HomeMissing,
            Some(BirthLoginErr::HooksMissing { .. }) => Self::HooksMissing,
            Some(BirthLoginErr::HooksUntrusted { .. }) => Self::HooksUntrusted,
            Some(
                BirthLoginErr::Frozen { .. }
                | BirthLoginErr::Login(_)
                | BirthLoginErr::MachineUnknown { .. }
                | BirthLoginErr::MachineUnsupported { .. },
            ) => Self::Unavailable,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::HomeMissing => "home missing",
            Self::HooksMissing => "hooks missing",
            Self::HooksUntrusted => "hooks untrusted",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A room's account selection that cannot be born.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BirthLoginErr {
    #[error(
        "unknown {kind} account `{name}` selected by [accounts.use] in {path}; configured: {}; run `rimz accounts add {kind} {name}` or `rimz accounts use --global {kind} default`",
        render_names(configured)
    )]
    MachineUnknown {
        kind: AgentKind,
        name: LoginName,
        configured: Vec<LoginName>,
        path: PathBuf,
    },
    #[error(
        "[accounts.use] in {path} selects {kind} account `{name}`, but {kind} has no named accounts; run `rimz accounts use --global {kind} default`"
    )]
    MachineUnsupported {
        kind: AgentKind,
        name: LoginName,
        path: PathBuf,
    },
    #[error(
        "this room uses {kind} account `{current}`, not `{requested}`; switch it with `rimz accounts use {kind} {requested}`"
    )]
    Frozen {
        kind: AgentKind,
        current: LoginName,
        requested: LoginName,
    },
    #[error(transparent)]
    Login(#[from] LoginErr),
    #[error(
        "{kind} account `{name}` home `{}` is not a directory; run `{}`",
        home.display(),
        home_fix(kind, name)
    )]
    MissingHome {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
    },
    #[error(
        "RimZ hooks are missing for {kind} account `{name}` at `{}`; run `{}`",
        home.display(),
        home_fix(kind, name)
    )]
    HooksMissing {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
    },
    #[error(
        "RimZ hooks for {kind} account `{name}` at `{}` are untrusted ({hooks}); {fix}{}",
        home.display(),
        started_as(*env_key, home, kind)
    )]
    HooksUntrusted {
        kind: AgentKind,
        name: LoginName,
        home: PathBuf,
        /// The home override a named account's provider is started with.
        env_key: Option<&'static str>,
        hooks: Box<str>,
        fix: Box<str>,
    },
}

impl ProviderLogin {
    /// Fail fast on a named account a room cannot launch into: its home must
    /// exist and carry trusted RimZ hooks. The default account keeps the
    /// provider's own hook flow, which `rimz start` already walks.
    pub fn preflight(&self, ambient: &BTreeMap<String, String>) -> Result<(), BirthLoginErr> {
        if self.is_default() {
            return Ok(());
        }
        self.health(ambient)
    }

    /// Whether this account's home is a directory carrying trusted RimZ
    /// hooks. Unlike [`Self::preflight`] it checks `default` too, for display;
    /// pass `default` the [`LoginCatalog::native_ambient`] of its kind.
    pub fn health(&self, ambient: &BTreeMap<String, String>) -> Result<(), BirthLoginErr> {
        let Some(adapter) = crate::agents::find_definition(&self.kind) else {
            return Ok(());
        };
        let env = self.env(ambient);
        let Some(path) = self
            .home()
            .map(Path::to_path_buf)
            .or_else(|| adapter.config_home(&env))
        else {
            return Ok(());
        };
        let kind = self.kind.clone();
        let name = self.name.clone();
        if !path.is_dir() {
            return Err(BirthLoginErr::MissingHome {
                kind,
                name,
                home: path,
            });
        }
        match crate::agents::preflight_hooks(adapter, &env, crate::agents::TurnLifecycleNeed::None)
        {
            Ok(()) | Err(crate::agents::HookPreflightErr::TurnLifecycleUnsupported { .. }) => {
                Ok(())
            }
            Err(crate::agents::HookPreflightErr::HooksMissing) => {
                Err(BirthLoginErr::HooksMissing {
                    kind,
                    name,
                    home: path,
                })
            }
            Err(crate::agents::HookPreflightErr::HooksUntrusted { hooks, fix }) => {
                Err(BirthLoginErr::HooksUntrusted {
                    kind,
                    name,
                    home: path,
                    env_key: self.home.as_ref().map(|home| home.env_key),
                    hooks: hooks.into(),
                    fix: fix.into(),
                })
            }
        }
    }
}

/// The process environment every login overrides. Pairs that are not UTF-8
/// cannot name a home the adapters resolve, so they are dropped.
pub fn ambient_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Resolve a session's stamped account; an unstamped session uses the provider's own home.
pub fn session_login(
    kind: &AgentKind,
    login: Option<&LoginName>,
    accounts: &AccountsConfig,
) -> Result<ProviderLogin, RoomLoginErr> {
    let Some(name) = login.filter(|name| !name.is_default()) else {
        return Ok(ProviderLogin::default_for(kind.clone()));
    };
    Ok(LoginCatalog::from_config(accounts)?.select(kind, name)?)
}

/// Resolve the environment of a session's stamped account.
pub fn session_login_env(
    kind: &AgentKind,
    login: Option<&LoginName>,
) -> Result<BTreeMap<String, String>, RoomLoginErr> {
    let ambient = ambient_env();
    let accounts = &crate::config::MachineConfig::load_lenient().accounts;
    Ok(session_login(kind, login, accounts)?.env(&ambient))
}

/// Resolving the login a room launches a kind under.
#[derive(Debug, thiserror::Error)]
pub enum RoomLoginErr {
    #[error(transparent)]
    Record(#[from] Box<crate::workspace::record::WorkspaceRecordErr>),
    #[error(transparent)]
    Config(#[from] LoginConfigErr),
    #[error(transparent)]
    Login(#[from] LoginErr),
}

/// The account of every live pane-backed agent: ended rows and provider
/// subagents run on no account of their own.
pub(crate) fn live_login_keys(agents: &[super::AgentState]) -> impl Iterator<Item = LoginKey> + '_ {
    agents
        .iter()
        .filter(|agent| agent.ended_at.is_none() && !agent.is_provider_subagent())
        .map(super::AgentState::login_key)
}

/// The account selection of the room whose `workspace.json` is `record`; a
/// record without one, or none at all, selects `default` for every kind.
pub fn room_logins(record: &Path) -> Result<RoomLogins, RoomLoginErr> {
    Ok(crate::workspace::record::read_optional(record)
        .map_err(Box::new)?
        .and_then(|record| record.logins)
        .unwrap_or_default())
}

/// The logins a room reads provider state under, resolved once per refresh
/// pass or fold over one ambient snapshot. A kind whose account cannot be
/// resolved answers `None`, so callers skip it rather than read another
/// account's state. Two views answer "which logins": the current one per kind
/// (`current_keys`, what the dashboard shows) and every one in use
/// (`keys_in_use`, what producer state is kept for).
#[derive(Clone, Debug)]
pub struct RoomLoginSet {
    /// `None` when the room's record could not be read.
    selection: Option<RoomLogins>,
    /// `None` when the machine's account config does not load.
    catalog: Option<LoginCatalog>,
    ambient: BTreeMap<String, String>,
    live_logins: BTreeSet<LoginKey>,
}

impl RoomLoginSet {
    /// Include live pane-backed agents' stamps alongside the room defaults.
    pub fn with_agents(mut self, agents: &[super::AgentState]) -> Self {
        self.live_logins = live_login_keys(agents).collect();
        self
    }

    /// Resolvable defaults and live stamps for one kind, without duplicates.
    pub fn in_use(&self, kind: &str) -> Vec<ProviderLogin> {
        let mut logins = BTreeMap::new();
        if let Some(login) = self.default_login(kind) {
            logins.insert(login.key(), login);
        }
        for key in self
            .live_logins
            .iter()
            .filter(|key| key.kind.as_str() == kind)
        {
            let login = if key.name.is_default() {
                Some(ProviderLogin::default_for(key.kind.clone()))
            } else {
                self.catalog
                    .as_ref()
                    .and_then(|catalog| catalog.select(&key.kind, &key.name).ok())
            };
            if let Some(login) = login {
                logins.insert(key.clone(), login);
            }
        }
        logins.into_values().collect()
    }

    /// The resolvable logins this room currently uses across all kinds.
    pub fn keys_in_use(&self) -> BTreeSet<LoginKey> {
        self.kinds()
            .flat_map(|kind| self.in_use(kind.as_str()))
            .map(|login| login.key())
            .collect()
    }

    /// The login each kind launches under in this room; a kind whose account
    /// does not resolve contributes none.
    pub fn current_keys(&self) -> BTreeSet<LoginKey> {
        self.kinds()
            .filter_map(|kind| self.default_key(kind.as_str()))
            .collect()
    }

    /// Registered kinds, live agents' kinds, and the kinds the room selects.
    fn kinds(&self) -> impl Iterator<Item = AgentKind> + '_ {
        super::known_kinds()
            .map(AgentKind::new_unchecked)
            .chain(self.live_logins.iter().map(|key| key.kind.clone()))
            .chain(
                self.selection
                    .iter()
                    .flat_map(|selection| selection.keys().cloned()),
            )
    }

    pub fn new(
        selection: Option<RoomLogins>,
        catalog: Option<LoginCatalog>,
        ambient: BTreeMap<String, String>,
    ) -> Self {
        Self {
            selection,
            catalog,
            ambient,
            live_logins: BTreeSet::new(),
        }
    }

    /// The room whose `workspace.json` is `record`, under machine `accounts`.
    pub fn resolve(record: &Path, accounts: &AccountsConfig) -> Self {
        Self::new(
            room_logins(record).ok(),
            LoginCatalog::from_config(accounts).ok(),
            ambient_env(),
        )
    }

    /// The room `runtime` belongs to, under the machine's account config; a
    /// room whose state paths do not resolve answers no login.
    pub fn for_runtime(runtime: &crate::RuntimePaths) -> Self {
        let Ok(paths) = crate::StatePaths::for_workspace(runtime.workspace_id.clone()) else {
            return Self::new(None, None, ambient_env());
        };
        Self::resolve(
            &paths.workspace_record,
            &crate::config::MachineConfig::load_lenient().accounts,
        )
    }

    /// Every kind under its provider's own home, for callers outside a room.
    pub fn native() -> Self {
        Self::new(Some(RoomLogins::new()), None, ambient_env())
    }

    pub fn default_login(&self, kind: &str) -> Option<ProviderLogin> {
        let kind = AgentKind::new_unchecked(kind);
        match self.selection.as_ref()?.get(&kind) {
            Some(name) if !name.is_default() => self.catalog.as_ref()?.select(&kind, name).ok(),
            _ => Some(ProviderLogin::default_for(kind)),
        }
    }

    pub fn default_key(&self, kind: &str) -> Option<LoginKey> {
        self.default_login(kind).map(|login| login.key())
    }

    /// The environment `login`'s provider state is read under.
    pub fn env(&self, login: &ProviderLogin) -> BTreeMap<String, String> {
        login.env(&self.ambient)
    }
}

/// The login the room whose `workspace.json` is `record` launches `kind`
/// under. A record without a selection, or none at all, is the provider's own
/// home.
pub fn room_login(
    record: &Path,
    accounts: &AccountsConfig,
    kind: &AgentKind,
) -> Result<ProviderLogin, RoomLoginErr> {
    let record = crate::workspace::record::read_optional(record).map_err(Box::new)?;
    let Some(selection) = record.and_then(|record| record.logins) else {
        return Ok(ProviderLogin::default_for(kind.clone()));
    };
    Ok(LoginCatalog::from_config(accounts)?.room_login(&selection, kind)?)
}

fn native_home(kind: &AgentKind, home: Option<&Path>) -> Option<PathBuf> {
    let adapter = crate::agents::find_definition(kind.as_str())?;
    let env = home
        .map(|home| BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())]))
        .unwrap_or_default();
    adapter.config_home(&env)
}

#[cfg(test)]
mod tests;
