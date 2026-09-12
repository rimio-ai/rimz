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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::AccountsConfig;
use crate::ids::{AgentKind, AgentSessionId, LoginKey, LoginName, RoomLogins};
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
}

fn config_home_env_key(kind: &AgentKind) -> Option<&'static str> {
    match crate::agents::find_definition(kind.as_str())?.config_home_env_keys() {
        [key] => Some(key),
        _ => None,
    }
}

/// Where RimZ puts an account home the user did not place itself.
pub fn default_named_home(kind: &AgentKind, name: &LoginName) -> PathBuf {
    crate::disk::paths::data_home()
        .join("accounts")
        .join(kind.as_str())
        .join(name.as_str())
}

/// Every login this machine knows: one `default` per registered kind, plus
/// every declared account.
#[derive(Clone, Debug, Default)]
pub struct LoginCatalog {
    logins: BTreeMap<LoginKey, ProviderLogin>,
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
        for kind in crate::agents::known_kinds().map(AgentKind::new_unchecked) {
            let key = LoginKey::default_for(kind.clone());
            logins.insert(key, ProviderLogin::default_for(kind.clone()));
            let Some(declared) = accounts.named(&kind) else {
                continue;
            };
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
                logins.insert(login.key(), login);
            }
        }
        Ok(Self { logins })
    }

    /// The login a kind launches under when the room names `name`.
    pub fn select(&self, kind: &AgentKind, name: &LoginName) -> Result<ProviderLogin, LoginErr> {
        self.logins
            .get(&LoginKey::new(kind.clone(), name.clone()))
            .cloned()
            .ok_or_else(|| {
                if name.is_default() || config_home_env_key(kind).is_none() {
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

    /// Every login, defaults included, in `<kind>@<name>` order.
    pub fn all(&self) -> impl Iterator<Item = &ProviderLogin> {
        self.logins.values()
    }

    /// The declared names of a kind, `default` first.
    pub fn names(&self, kind: &AgentKind) -> Vec<LoginName> {
        self.logins
            .values()
            .filter(|login| login.kind() == kind)
            .map(|login| login.name().clone())
            .collect()
    }
}

/// The process environment every login overrides. Pairs that are not UTF-8
/// cannot name a home the adapters resolve, so they are dropped.
pub fn ambient_env() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
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

/// A session that cannot be resumed because it was born under another account.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "cannot resume {kind} session `{session_id}`: session account is `{session_login}`, room account is `{room_login}`; use a room started with `rimz start --account {kind}={session_login}`, or run `rimz reset --account {kind}={session_login}` here"
)]
pub struct LoginMismatch {
    pub kind: AgentKind,
    pub session_id: AgentSessionId,
    pub session_login: LoginName,
    pub room_login: LoginName,
}

impl LoginMismatch {
    /// The mismatch between a session's birth stamp and the room's selection,
    /// or `None` when the two agree. `None` on either side is `default`.
    pub fn between(
        kind: &AgentKind,
        session_id: &AgentSessionId,
        session_login: Option<&LoginName>,
        room_login: Option<&LoginName>,
    ) -> Option<Self> {
        let session = session_login.cloned().unwrap_or_default();
        let room = room_login.cloned().unwrap_or_default();
        (session != room).then(|| Self {
            kind: kind.clone(),
            session_id: session_id.clone(),
            session_login: session,
            room_login: room,
        })
    }
}

#[cfg(test)]
mod tests;
