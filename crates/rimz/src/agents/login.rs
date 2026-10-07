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
use std::sync::Arc;

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
        "`{env_key}` is exported as `{home}`, the home of {kind} account `{name}`, so the `default` account launches into it too; unset `{env_key}` and run `rimz accounts use --global {kind} {name}` for new rooms and rooms following the machine default"
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
    /// The account keeps only its credentials and reads everything else from
    /// the provider's own home.
    shared: bool,
    /// Every declared home of the kind, this one included: an exported home
    /// override naming one is a pane's account, never the provider's own home.
    declared: Arc<[PathBuf]>,
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
                declared: [home.clone()].into(),
                path: home,
                env_key,
                shared: false,
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

    /// The history pool this login reads and writes, as [`LoginCatalog::pool`]
    /// answers for its key.
    pub fn pool(&self) -> LoginKey {
        if self.shares_history() {
            LoginKey::default_for(self.kind.clone())
        } else {
            self.key()
        }
    }

    pub fn is_default(&self) -> bool {
        self.home.is_none()
    }

    /// The declared home, or `None` when the provider resolves its own.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_ref().map(|home| home.path.as_path())
    }

    /// Whether this account shares the `default` account's history.
    pub fn shares_history(&self) -> bool {
        self.home.as_ref().is_some_and(|home| home.shared)
    }

    /// The sessions `adapter` finds for `workspaces` in this login's home,
    /// each carrying this account.
    pub fn local_sessions(
        &self,
        adapter: &super::AgentDefinition,
        workspaces: &[&Path],
        ambient: &BTreeMap<String, String>,
    ) -> Vec<super::LocalSessionObservation> {
        let login = (!self.is_default()).then(|| self.name.clone());
        let mut observations = adapter.discover_local_sessions(workspaces, &self.env(ambient));
        for observation in &mut observations {
            observation.login.clone_from(&login);
        }
        observations
    }

    /// `ambient`, with this login's overrides applied. The default login
    /// leaves the ambient environment exactly as it is, so today's resolution
    /// policies — comma lists, XDG order, test overrides — keep running.
    pub fn env(&self, ambient: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = ambient.clone();
        env.extend(self.overrides(ambient));
        env
    }

    /// What this login sets on top of `ambient`: its home, and for a shared
    /// account whose provider keeps databases beside it, the home those live
    /// in. A database home the user exported stands.
    pub fn overrides(&self, ambient: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let Some(home) = &self.home else {
            return BTreeMap::new();
        };
        let mut overrides = BTreeMap::from([(
            home.env_key.to_owned(),
            home.path.to_string_lossy().into_owned(),
        )]);
        let databases = crate::agents::find_definition(self.kind.as_str())
            .filter(|_| home.shared)
            .and_then(|adapter| adapter.shared_database_home_env_key())
            .filter(|key| ambient.get(*key).is_none_or(String::is_empty));
        if let Some((key, default)) = databases.zip(self.default_home(ambient)) {
            overrides.insert(key.to_owned(), default.to_string_lossy().into_owned());
        }
        overrides
    }

    /// `ambient` as the provider's own home reads it: an exported home
    /// override that names a declared account's home is dropped, since a pane
    /// born on that account exports it by design and `default` is not that
    /// account.
    fn native_ambient(&self, ambient: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut native = ambient.clone();
        if let Some(home) = &self.home
            && home
                .declared
                .iter()
                .any(|declared| exports_home(&self.kind, home.env_key, declared, ambient))
        {
            native.remove(home.env_key);
        }
        native
    }

    /// The provider's own home, the base a named account links into.
    pub fn default_home(&self, ambient: &BTreeMap<String, String>) -> Option<PathBuf> {
        Self::default_for(self.kind.clone()).home_dir(&self.native_ambient(ambient))
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
        if !exports_home(&self.kind, env_key, home, ambient) {
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

/// Whether `ambient` exports `env_key` so that `kind`'s own home resolves to `home`.
fn exports_home(
    kind: &AgentKind,
    env_key: &str,
    home: &Path,
    ambient: &BTreeMap<String, String>,
) -> bool {
    if !ambient.get(env_key).is_some_and(|value| !value.is_empty()) {
        return false;
    }
    ProviderLogin::default_for(kind.clone())
        .home_dir(ambient)
        .is_some_and(|path| match (path.canonicalize(), home.canonicalize()) {
            (Ok(default), Ok(named)) => default == named,
            _ => normalize_path_lexical(&path) == normalize_path_lexical(home),
        })
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
    fn for_kind(accounts: &AccountsConfig, kind: &AgentKind) -> Result<Self, LoginConfigErr> {
        let mut accounts = accounts.clone();
        for other in super::known_kinds()
            .map(AgentKind::new_unchecked)
            .filter(|other| other != kind)
        {
            if let Some(named) = accounts.named_mut(&other) {
                named.clear();
            }
        }
        Self::from_config(&accounts)
    }

    /// A partial read-side catalogue and each excluded kind's declaration error.
    pub fn room_view(accounts: &AccountsConfig) -> (Self, BTreeMap<AgentKind, LoginConfigErr>) {
        let mut result = Self::default();
        let mut errors = BTreeMap::new();
        for kind in super::known_kinds().map(AgentKind::new_unchecked) {
            let catalog = match Self::for_kind(accounts, &kind) {
                Ok(catalog) => catalog,
                Err(error) => {
                    errors.insert(kind, error);
                    continue;
                }
            };
            result.logins.extend(
                catalog
                    .logins
                    .into_iter()
                    .filter(|(key, _)| key.kind == kind),
            );
            result.account_kinds.extend(catalog.account_kinds);
            result.standalone.extend(catalog.standalone);
        }
        (result, errors)
    }

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
                let mut login = ProviderLogin::named(kind.clone(), name.clone(), declared_home)
                    .expect("named accounts are configurable only for kinds with a home override");
                if account.history == AccountHistory::Standalone {
                    standalone.insert(login.key());
                } else if let Some(home) = &mut login.home {
                    home.shared = true;
                }
                logins.insert(login.key(), login);
            }
            let declared: Arc<[PathBuf]> = claimed.into_keys().collect();
            for home in logins
                .values_mut()
                .filter(|login| login.kind == kind)
                .filter_map(|login| login.home.as_mut())
            {
                home.declared = Arc::clone(&declared);
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

    /// Fold requested accounts over project, machine, then provider defaults.
    pub fn birth_selection(
        &self,
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
        self.logins
            .values()
            .find(|login| login.kind() == kind && !login.is_default())
            .map_or_else(|| ambient.clone(), |login| login.native_ambient(ambient))
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
    /// The home is set up, and the provider reports no login in it.
    LoggedOut,
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
                BirthLoginErr::Login(_)
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
            Self::LoggedOut => "logged out",
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
    /// Fail fast on a named account a room cannot be born on or switched to:
    /// its home must exist and carry trusted RimZ hooks. The default account
    /// is skipped here, for room birth and account switching only, where it
    /// keeps the provider's own hook flow that `rimz start` walks; a launch
    /// runs [`Self::health`] instead.
    pub fn preflight(&self, ambient: &BTreeMap<String, String>) -> Result<(), BirthLoginErr> {
        if self.is_default() {
            return Ok(());
        }
        self.health(ambient)
    }

    /// Whether this account's home is a directory carrying trusted RimZ
    /// hooks. It is the gate every RimZ launch and relaunch runs on the login
    /// it launches under, `default` included, and what the account rows
    /// display; pass it the [`LoginCatalog::native_ambient`] of its kind.
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
    Ok(LoginCatalog::for_kind(accounts, kind)?.select(kind, name)?)
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

/// The machine's read-side catalog for a caller with no config in scope.
/// Invalid declarations exclude only their kind; other kinds keep their history pools.
pub fn machine_login_catalog() -> LoginCatalog {
    LoginCatalog::room_view(&crate::config::MachineConfig::load_lenient().accounts).0
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
    #[error(transparent)]
    Birth(#[from] Box<BirthLoginErr>),
    #[error(transparent)]
    Core(#[from] Box<crate::config::ConfigErr>),
    #[error(transparent)]
    Trust(#[from] Box<crate::trust::TrustErr>),
    #[error("{}", crate::trust::blocked_project_logins(*state))]
    Blocked { state: crate::trust::TrustState },
    #[error(transparent)]
    Resolution(Arc<RoomLoginErr>),
}

impl RoomLoginErr {
    fn resolution(mut error: Arc<Self>) -> Self {
        while let Self::Resolution(inner) = error.as_ref() {
            error = inner.clone();
        }
        Self::Resolution(error)
    }
}

/// What decides the account a room launches a kind under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginSource {
    Pinned,
    Project,
    Machine,
    Provider,
}

impl std::fmt::Display for LoginSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pinned => "pinned",
            Self::Project => "project default",
            Self::Machine => "machine default",
            Self::Provider => "provider default",
        })
    }
}

#[derive(Clone, Debug)]
pub struct RoomAccount {
    pub name: LoginName,
    pub source: LoginSource,
}

/// Each kind's launch account or refusal. Failed kinds never become `default`.
#[derive(Clone, Debug, Default)]
pub struct RoomAccounts {
    outcomes: BTreeMap<AgentKind, Result<RoomAccount, Arc<RoomLoginErr>>>,
    pins: RoomLogins,
    inherited_error: Option<Arc<RoomLoginErr>>,
    project: Option<crate::trust::ProjectLogins>,
}

impl RoomAccounts {
    /// Carry resolved launch overrides without discarding other kinds' refusals.
    pub(crate) fn with_names(mut self, names: RoomLogins) -> Self {
        for (kind, name) in names {
            self.outcomes.insert(
                kind,
                Ok(RoomAccount {
                    name,
                    source: LoginSource::Pinned,
                }),
            );
        }
        self
    }

    pub(crate) fn machine(machine: &crate::config::MachineConfig) -> Self {
        resolve_accounts(&RoomLogins::new(), None, machine)
    }

    pub(crate) fn unavailable(error: RoomLoginErr) -> Self {
        let error = Arc::new(error);
        Self {
            outcomes: super::known_kinds()
                .map(|kind| (AgentKind::new_unchecked(kind), Err(error.clone())))
                .collect(),
            pins: RoomLogins::new(),
            inherited_error: Some(error),
            project: None,
        }
    }

    /// Kinds carrying an explicit outcome, including refusals.
    pub fn kinds(&self) -> impl Iterator<Item = &AgentKind> {
        self.outcomes.keys()
    }

    pub(crate) fn project(&self) -> Option<&crate::trust::ProjectLogins> {
        self.project.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_layers(
        pins: &RoomLogins,
        project: crate::trust::ProjectLogins,
        machine: &crate::config::MachineConfig,
    ) -> Self {
        resolve_account_layers(pins, Ok(project), machine)
    }

    pub fn account(&self, kind: &AgentKind) -> Result<RoomAccount, RoomLoginErr> {
        match self.outcomes.get(kind) {
            Some(Ok(account)) => Ok(account.clone()),
            Some(Err(error)) => Err(RoomLoginErr::resolution(error.clone())),
            None => {
                if let Some(error) = &self.inherited_error {
                    return Err(RoomLoginErr::resolution(error.clone()));
                }
                Ok(RoomAccount {
                    name: LoginName::default_login(),
                    source: LoginSource::Provider,
                })
            }
        }
    }

    pub fn name(&self, kind: &AgentKind) -> Result<LoginName, RoomLoginErr> {
        self.account(kind).map(|account| account.name)
    }

    pub fn source(&self, kind: &AgentKind) -> Result<LoginSource, RoomLoginErr> {
        self.account(kind).map(|account| account.source)
    }

    pub fn pinned(&self, kind: &AgentKind) -> bool {
        self.pins.contains_key(kind)
    }

    pub fn pin(&self, kind: &AgentKind) -> Option<&LoginName> {
        self.pins.get(kind)
    }

    pub fn pinned_names(&self) -> RoomLogins {
        self.pins.clone()
    }

    /// Successful names only; absence means no account, not `default`.
    pub fn names(&self) -> RoomLogins {
        self.outcomes
            .iter()
            .filter_map(|(kind, account)| {
                account
                    .as_ref()
                    .ok()
                    .map(|account| (kind.clone(), account.name.clone()))
            })
            .collect()
    }

    pub fn login(
        &self,
        kind: &AgentKind,
        accounts: &AccountsConfig,
    ) -> Result<ProviderLogin, RoomLoginErr> {
        session_login(kind, Some(&self.name(kind)?), accounts)
    }
}

impl From<RoomLogins> for RoomAccounts {
    fn from(pins: RoomLogins) -> Self {
        Self {
            outcomes: pins
                .iter()
                .map(|(kind, name)| {
                    (
                        kind.clone(),
                        Ok(RoomAccount {
                            name: name.clone(),
                            source: LoginSource::Pinned,
                        }),
                    )
                })
                .collect(),
            pins,
            inherited_error: None,
            project: None,
        }
    }
}

/// Resolve the pin, trusted project, machine, then provider default per kind.
pub fn resolve_room_accounts(
    pins: &RoomLogins,
    project_root: &Path,
    machine: &crate::config::MachineConfig,
) -> RoomAccounts {
    resolve_accounts(pins, Some(project_root), machine)
}

fn resolve_accounts(
    pins: &RoomLogins,
    project_root: Option<&Path>,
    machine: &crate::config::MachineConfig,
) -> RoomAccounts {
    let project = if super::known_kinds()
        .map(AgentKind::new_unchecked)
        .chain(machine.accounts.use_accounts.keys().cloned())
        .any(|kind| !pins.contains_key(&kind))
    {
        project_root
            .map(crate::trust::project_logins)
            .transpose()
            .map(|project| project.unwrap_or(crate::trust::ProjectLogins::Unconfigured))
            .map_err(|error| Arc::new(RoomLoginErr::Trust(Box::new(error))))
    } else {
        Ok(crate::trust::ProjectLogins::Unconfigured)
    };
    resolve_account_layers(pins, project, machine)
}

fn resolve_account_layers(
    pins: &RoomLogins,
    project: Result<crate::trust::ProjectLogins, Arc<RoomLoginErr>>,
    machine: &crate::config::MachineConfig,
) -> RoomAccounts {
    let mut kinds: BTreeSet<_> = super::known_kinds()
        .map(AgentKind::new_unchecked)
        .filter(|kind| machine.accounts.named(kind).is_some())
        .chain(pins.keys().cloned())
        .chain(machine.accounts.use_accounts.keys().cloned())
        .collect();
    if let Ok(crate::trust::ProjectLogins::Apply(project)) = &project {
        kinds.extend(project.keys().cloned());
    }
    let core_error = machine
        .require_readable_core("resolve this room's account")
        .err()
        .map(|error| Arc::new(RoomLoginErr::Core(Box::new(error))));
    let inherited_error = core_error.clone().or_else(|| match &project {
        Ok(crate::trust::ProjectLogins::Blocked(state)) => {
            Some(Arc::new(RoomLoginErr::Blocked { state: *state }))
        }
        Err(error) => Some(error.clone()),
        _ => None,
    });
    let outcomes = kinds
        .into_iter()
        .map(|kind| {
            let resolved = (|| -> Result<RoomAccount, RoomLoginErr> {
                if pins.get(&kind).is_some_and(LoginName::is_default) {
                    return Ok(RoomAccount {
                        name: LoginName::default_login(),
                        source: LoginSource::Pinned,
                    });
                }
                let error = if pins.contains_key(&kind) {
                    &core_error
                } else {
                    &inherited_error
                };
                if let Some(error) = error {
                    return Err(RoomLoginErr::resolution(error.clone()));
                }
                let empty = RoomLogins::new();
                let project = if pins.contains_key(&kind) {
                    &empty
                } else if let Ok(crate::trust::ProjectLogins::Apply(project)) = &project {
                    project
                } else {
                    &empty
                };
                let source = if pins.contains_key(&kind) {
                    LoginSource::Pinned
                } else if project.contains_key(&kind) {
                    LoginSource::Project
                } else if machine.accounts.use_accounts.contains_key(&kind) {
                    LoginSource::Machine
                } else {
                    LoginSource::Provider
                };
                let name = birth_name(&kind, pins, project, &machine.accounts.use_accounts);
                if !name.is_default() {
                    let catalog = LoginCatalog::for_kind(&machine.accounts, &kind)?;
                    if source == LoginSource::Machine {
                        catalog.select_machine(&kind, &name).map_err(Box::new)?;
                    } else {
                        catalog.select(&kind, &name)?;
                    }
                }
                Ok(RoomAccount { name, source })
            })()
            .map_err(Arc::new);
            (kind, resolved)
        })
        .collect();
    RoomAccounts {
        outcomes,
        pins: pins.clone(),
        inherited_error,
        project: project.ok(),
    }
}

/// The account of every live root agent, with a pane or without: ended rows
/// and provider subagents run on no account of their own.
fn live_login_keys(agents: &[super::AgentState]) -> impl Iterator<Item = LoginKey> + '_ {
    agents
        .iter()
        .filter(|agent| agent.ended_at.is_none() && !agent.is_provider_subagent())
        .map(super::AgentState::login_key)
}

/// The room's live accounts. Only an unreadable record fails the whole view. With no record, `project_root` supplies the prospective room's project layer.
pub fn room_accounts(
    record: &Path,
    project_root: Option<&Path>,
    machine: &crate::config::MachineConfig,
) -> Result<RoomAccounts, RoomLoginErr> {
    match crate::workspace::record::read_optional(record).map_err(Box::new)? {
        Some(record) => Ok(resolve_room_accounts(
            &record.pins,
            &record.project_root,
            machine,
        )),
        None => Ok(resolve_accounts(&RoomLogins::new(), project_root, machine)),
    }
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
    selection: Option<RoomAccounts>,
    /// `None` outside a room, where every kind is its provider's own home.
    catalog: Option<LoginCatalog>,
    ambient: BTreeMap<String, String>,
    live_logins: BTreeSet<LoginKey>,
}

impl RoomLoginSet {
    /// Include live root agents' stamps alongside the room defaults.
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

    /// Every login of one kind the machine catalog holds, `default` included,
    /// whether or not this room uses it; none when the account config did not
    /// load.
    pub fn declared(&self, kind: &str) -> Vec<ProviderLogin> {
        self.catalog
            .iter()
            .flat_map(LoginCatalog::all)
            .filter(|login| login.kind().as_str() == kind)
            .cloned()
            .collect()
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
                    .flat_map(|selection| selection.outcomes.keys().cloned()),
            )
    }

    pub fn new(
        selection: Option<RoomAccounts>,
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

    /// The recorded room, or a prospective one at `project_root`, under `machine`.
    pub fn resolve(
        record: &Path,
        project_root: Option<&Path>,
        machine: &crate::config::MachineConfig,
    ) -> Self {
        Self::new(
            room_accounts(record, project_root, machine).ok(),
            Some(LoginCatalog::room_view(&machine.accounts).0),
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
            None,
            &crate::config::MachineConfig::load_lenient(),
        )
    }

    /// Every kind under its provider's own home, for callers outside a room.
    pub fn native() -> Self {
        Self::new(Some(RoomAccounts::default()), None, ambient_env())
    }

    pub fn default_login(&self, kind: &str) -> Option<ProviderLogin> {
        let kind = AgentKind::new_unchecked(kind);
        let name = self.selection.as_ref()?.name(&kind).ok()?;
        if name.is_default() {
            return Some(ProviderLogin::default_for(kind));
        }
        self.catalog.as_ref()?.select(&kind, &name).ok()
    }

    pub fn default_key(&self, kind: &str) -> Option<LoginKey> {
        self.default_login(kind).map(|login| login.key())
    }

    /// The history pool of `login`; its own key when the account config does
    /// not load.
    pub fn pool(&self, login: &LoginKey) -> LoginKey {
        self.catalog
            .as_ref()
            .map_or_else(|| login.clone(), |catalog| catalog.pool(login))
    }

    /// The environment `login`'s provider state is read under.
    pub fn env(&self, login: &ProviderLogin) -> BTreeMap<String, String> {
        login.env(&self.ambient)
    }
}

/// The login the room whose `workspace.json` is `record` launches `kind`
/// under now, resolving only that kind's account or refusal.
pub fn room_account(
    record: &Path,
    machine: &crate::config::MachineConfig,
    kind: &AgentKind,
) -> Result<ProviderLogin, RoomLoginErr> {
    room_accounts(record, None, machine)?.login(kind, &machine.accounts)
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
