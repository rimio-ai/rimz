//! Link a named account's home into the provider's own home: everything but
//! private and transient names for an account that shares the default's
//! history, settings alone for a standalone one. A link into the provider's
//! own home at a private or transient name is removed in either mode.
//! Links under HOME are relative from the account home's real location to
//! the provider's configured home. Currency is by text; another spelling of
//! the same existing entry is replaced in place without moving data aside.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::capabilities::SharedHomeKind;
use super::runtime_control::DaemonSessions;
use super::{AgentDefinition, ProviderLogin, skill_links};
use crate::disk::atomic;
use crate::disk::lock::{LockErr, WorkspaceLock};
use crate::ids::{AgentKind, LoginKey};
use crate::utils::path::normalize_path_lexical;

/// Where a conflicting entry of the account home is kept, inside that home.
pub(crate) const ASIDE_DIR: &str = ".rimz-aside";

#[derive(Debug, thiserror::Error)]
pub enum ShareErr {
    #[error("`--home` names the provider's own home, which is the `default` account ({}); choose another home", home.display())]
    SameHome { home: PathBuf },
    #[error("cannot resolve {kind}'s own home; set HOME")]
    DefaultHome { kind: AgentKind },
    #[error(
        "cannot share `{entry}` of {account} with {}: {}; end those agents, or set `history = \"standalone\"` under `[accounts.{}.{}]`",
        default_home.display(),
        live_writers(*agents, "to the copy in its home, which would move aside under them"),
        account.kind,
        account.name
    )]
    LiveAgents {
        account: LoginKey,
        entry: String,
        default_home: PathBuf,
        /// `None` when the live rooms could not be read.
        agents: Option<usize>,
    },
    #[error(
        "cannot unlink `{entry}` of {account} from {}: {}; end those agents, or remove `history = \"standalone\"` from `[accounts.{}.{}]`",
        default_home.display(),
        live_writers(*agents, "through that link, which would be removed under them"),
        account.kind,
        account.name
    )]
    LiveAgentsUnlink {
        account: LoginKey,
        entry: String,
        default_home: PathBuf,
        /// `None` when the live rooms could not be read.
        agents: Option<usize>,
    },
    #[error(
        "cannot share `{entry}` of {account} with {}: {}, or set `history = \"standalone\"` under `[accounts.{}.{}]`",
        default_home.display(),
        daemon_writers(*sessions, &account.kind, "to the copy in its home, which would move aside under"),
        account.kind,
        account.name
    )]
    LiveDaemon {
        account: LoginKey,
        entry: String,
        default_home: PathBuf,
        /// `None` when the daemon did not report its sessions.
        sessions: Option<NonZeroUsize>,
    },
    #[error(
        "cannot unlink `{entry}` of {account} from {}: {}, or remove `history = \"standalone\"` from `[accounts.{}.{}]`",
        default_home.display(),
        daemon_writers(*sessions, &account.kind, "through that link, which would be removed under"),
        account.kind,
        account.name
    )]
    LiveDaemonUnlink {
        account: LoginKey,
        entry: String,
        default_home: PathBuf,
        /// `None` when the daemon did not report its sessions.
        sessions: Option<NonZeroUsize>,
    },
    /// A failure after entries were already set aside, which a rerun would no
    /// longer report.
    #[error("{source}\n{}", warnings.join("\n"))]
    AfterSetAside {
        warnings: Vec<String>,
        source: Box<ShareErr>,
    },
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error("cannot link {}: {source}; fix access to that path, then rerun `rimz accounts add`", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot relink {}: {source}; fix access to that path, then rerun `rimz accounts add`", path.display())]
    Relink {
        path: PathBuf,
        source: atomic::AtomicErr,
    },
    #[error("cannot adopt {}: {source}; fix the settings file, then rerun `rimz accounts add`", path.display())]
    Adopt {
        path: PathBuf,
        source: super::AgentErr,
    },
}

fn live_writers(agents: Option<usize>, writes: &str) -> String {
    match agents {
        Some(count) => format!("{count} live agent(s) on the account write {writes}"),
        None => {
            format!("the live rooms could not be read, so agents on the account may write {writes}")
        }
    }
}

/// What the account's daemon writes and the fixes that end it, up to the
/// `history` alternative the caller appends.
fn daemon_writers(sessions: Option<NonZeroUsize>, kind: &AgentKind, writes: &str) -> String {
    let toggle =
        format!("`rimz config set remote_control.{kind} false`, rerun, then set it back to `true`");
    match sessions {
        Some(count) => format!(
            "the remote-control daemon on the account holds {count} live session(s) that write {writes} them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with {toggle}"
        ),
        None => format!(
            "the remote-control daemon on the account did not report its sessions, so it may write {writes} it; stop it with {toggle}"
        ),
    }
}

#[derive(Debug)]
pub struct ShareReport {
    account: LoginKey,
    default_home: PathBuf,
    linked: Vec<String>,
    current: Vec<String>,
    /// Entries only the account had, now in the provider's own home.
    moved: Vec<String>,
    /// Links a standalone account no longer shares.
    unlinked: Vec<String>,
    set_aside: Vec<(PathBuf, PathBuf)>,
    /// Links removed from names no account shares.
    repaired: Vec<PathBuf>,
    /// Entries that could not move to the provider's own home.
    left_local: Vec<PathBuf>,
    notes: Vec<String>,
}

impl ShareReport {
    /// What the account's owner must hear about: every link removed from a
    /// name no account shares, every entry set aside, and every entry left in
    /// the account home.
    pub fn warnings(&self) -> Vec<String> {
        let repaired = self.repaired.iter().map(|slot| {
            format!(
                "{} account: removed the link {} into {}; that name is per-account and no longer shared",
                self.account,
                slot.display(),
                self.default_home.display()
            )
        });
        let set_aside = self.set_aside.iter().map(|(slot, aside)| {
            format!(
                "{} account: moved {} aside to {}; the account now reads the copy in {}",
                self.account,
                slot.display(),
                aside.display(),
                self.default_home.display()
            )
        });
        let left_local = self.left_local.iter().map(|slot| {
            format!(
                "{} account: {} stays in the account home; it cannot move to {} on another filesystem",
                self.account,
                slot.display(),
                self.default_home.display()
            )
        });
        repaired.chain(set_aside).chain(left_local).collect()
    }
}

impl std::fmt::Display for ShareReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for warning in self.warnings() {
            writeln!(f, "{warning}")?;
        }
        for note in &self.notes {
            writeln!(f, "{note}")?;
        }
        let home = self.default_home.display();
        if !self.moved.is_empty() {
            writeln!(f, "moved to {home}: {}", self.moved.join(", "))?;
        }
        if !self.unlinked.is_empty() {
            writeln!(f, "unlinked from {home}: {}", self.unlinked.join(", "))?;
        }
        if self.linked.is_empty() {
            write!(f, "already linked to {home}")
        } else {
            write!(f, "linked to {home}: {}", self.linked.join(", "))
        }
    }
}

enum Action {
    Link,
    Current,
    Replace,
    /// Real in the account home and absent from the provider's own.
    Move(fs::FileType),
    SetAside(fs::FileType),
}

struct PlannedLink {
    name: String,
    slot: PathBuf,
    target: PathBuf,
    link_text: PathBuf,
    action: Action,
}

fn io_err(path: &Path, source: std::io::Error) -> ShareErr {
    ShareErr::Io {
        path: path.into(),
        source,
    }
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, ShareErr> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(path, error)),
    }
}

/// Resolve existing text from the real account home before the stale scan's
/// lexical comparison, without requiring the target to exist.
fn link_target(named_home: &Path, link: &Path) -> Result<PathBuf, ShareErr> {
    if link.is_absolute() {
        return Ok(normalize_path_lexical(link));
    }
    let real_home = named_home
        .canonicalize()
        .map_err(|error| io_err(named_home, error))?;
    Ok(normalize_path_lexical(&real_home.join(link)))
}

/// Only matching text is current. A spelling change that reaches the same
/// existing entry needs no migration or live-writer gate.
fn classify(slot: &Path, target: &Path, link_text: &Path) -> Result<Action, ShareErr> {
    let Some(existing) = metadata(slot)? else {
        return Ok(Action::Link);
    };
    if existing.is_symlink() {
        let existing_text = fs::read_link(slot).map_err(|error| io_err(slot, error))?;
        return Ok(if existing_text.as_os_str() == link_text.as_os_str() {
            Action::Current
        } else if fs::metadata(slot)
            .ok()
            .zip(fs::metadata(target).ok())
            .is_some_and(|(slot, target)| slot.dev() == target.dev() && slot.ino() == target.ino())
        {
            Action::Replace
        } else {
            Action::SetAside(existing.file_type())
        });
    }
    Ok(if metadata(target)?.is_some() {
        Action::SetAside(existing.file_type())
    } else {
        Action::Move(existing.file_type())
    })
}

/// The top-level names of a home; a home that does not exist has none. A name
/// that is not UTF-8 is no provider's and stays where it is.
fn top_level(home: &Path) -> Result<BTreeSet<String>, ShareErr> {
    let entries = match fs::read_dir(home) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(io_err(home, error)),
    };
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_err(home, error))?;
        names.extend(entry.file_name().into_string().ok());
    }
    Ok(names)
}

fn is_private(adapter: &AgentDefinition, name: &str) -> bool {
    adapter
        .private_home_entries()
        .iter()
        .chain(&[ASIDE_DIR])
        .any(|private| {
            name.strip_prefix(private)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
}

/// A provider's lock or temp file: `.lock` or `.tmp` as a dotted component
/// past the start of the name, so `.tmp` and `thread-writer-locks` are not.
fn is_transient(name: &str) -> bool {
    [".lock", ".tmp"].iter().any(|suffix| {
        name.match_indices(suffix).any(|(at, _)| {
            let rest = &name[at + suffix.len()..];
            at > 0 && (rest.is_empty() || rest.starts_with('.'))
        })
    })
}

/// Whether a top-level name stays out of the link set of every account.
fn is_unshared(adapter: &AgentDefinition, name: &str) -> bool {
    is_private(adapter, name) || is_transient(name)
}

pub fn check_distinct_homes(named_home: &Path, default_home: &Path) -> Result<(), ShareErr> {
    let named_canonical = match named_home.canonicalize() {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_err(named_home, error)),
    };
    match default_home.canonicalize() {
        Ok(home) if home == named_canonical => Err(ShareErr::SameHome { home }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_err(default_home, error)),
    }
}

/// Bring a named account's home to the link set its history mode asks for.
/// `default` has no links and answers `None`. `other_live_agents` counts the
/// live agents on the account besides the one launching, or `None` when the
/// live rooms cannot be read; it is asked only before a directory would move
/// aside, and so is the provider, once no agent is counted, for the live
/// sessions a daemon holds under the account home: a daemon that holds none
/// does not block, and one that does not report them does.
pub fn reconcile(
    login: &ProviderLogin,
    ambient: &BTreeMap<String, String>,
    other_live_agents: &dyn Fn() -> Option<usize>,
) -> Result<Option<ShareReport>, ShareErr> {
    let Some((named_home, adapter)) = login
        .home()
        .zip(super::find_definition(login.kind().as_str()))
    else {
        return Ok(None);
    };
    let default_home = login
        .default_home(ambient)
        .ok_or_else(|| ShareErr::DefaultHome {
            kind: login.kind().clone(),
        })?;
    let account = login.key();
    reconcile_homes(
        &Homes {
            adapter,
            named: named_home,
            default: &default_home,
            user: ambient.get("HOME").map(Path::new),
            shared: login.shares_history(),
            lock: &crate::disk::paths::account_lock(login.kind()),
            account: &account,
            daemon_writes: &|| {
                super::runtime_control::writes_history(login.kind().as_str(), &login.env(ambient))
            },
        },
        other_live_agents,
    )
    .map(Some)
}

struct Homes<'a> {
    adapter: &'a AgentDefinition,
    account: &'a LoginKey,
    named: &'a Path,
    default: &'a Path,
    user: Option<&'a Path>,
    shared: bool,
    lock: &'a Path,
    /// The live sessions a provider daemon holds under `named`.
    daemon_writes: &'a dyn Fn() -> DaemonSessions,
}

fn relative_prefix(named_home: &Path, default_home: &Path, user: Option<&Path>) -> Option<PathBuf> {
    let user = user.filter(|home| !home.as_os_str().is_empty())?;
    let real_user = user.canonicalize().ok()?;
    let real_named = named_home.canonicalize().ok()?;
    let below_user = real_named.strip_prefix(real_user).ok()?;
    let configured_user = normalize_path_lexical(&std::path::absolute(user).ok()?);
    let provider = default_home.strip_prefix(configured_user).ok()?;
    let mut prefix = PathBuf::new();
    for _ in below_user.components() {
        prefix.push("..");
    }
    prefix.push(provider);
    Some(prefix)
}

fn reconcile_homes(
    homes: &Homes<'_>,
    other_live_agents: &dyn Fn() -> Option<usize>,
) -> Result<ShareReport, ShareErr> {
    let adapter = homes.adapter;
    check_distinct_homes(homes.named, homes.default)?;
    let named_home = std::path::absolute(homes.named)
        .map(|home| normalize_path_lexical(&home))
        .map_err(|error| io_err(homes.named, error))?;
    let default_home = std::path::absolute(homes.default)
        .map(|home| normalize_path_lexical(&home))
        .map_err(|error| io_err(homes.default, error))?;
    let _lock = WorkspaceLock::acquire(homes.lock)?;
    let prefix = relative_prefix(&named_home, &default_home, homes.user)
        .unwrap_or_else(|| default_home.clone());

    let included = adapter.shared_home_entries();
    let mut names: Vec<String> = included.iter().map(|entry| entry.name.to_owned()).collect();
    let mut dirs: Vec<&str> = included
        .iter()
        .filter(|entry| matches!(entry.kind, SharedHomeKind::Dir))
        .map(|entry| entry.name)
        .collect();
    let in_account = top_level(&named_home)?;
    if homes.shared {
        dirs.extend(adapter.history_home_entries());
        let rest: BTreeSet<String> = in_account
            .iter()
            .cloned()
            .chain(top_level(&default_home)?)
            .chain(
                adapter
                    .history_home_entries()
                    .iter()
                    .map(|&name| name.to_owned()),
            )
            .filter(|name| !is_unshared(adapter, name) && !names.contains(name))
            .collect();
        names.extend(rest);
    }
    let mut plan = Vec::new();
    for name in &names {
        let slot = named_home.join(name);
        let target = default_home.join(name);
        let link_text = prefix.join(name);
        let action = classify(&slot, &target, &link_text)?;
        plan.push(PlannedLink {
            name: name.clone(),
            slot,
            target,
            link_text,
            action,
        });
    }
    let mut stale = Vec::new();
    for name in in_account {
        let slot = named_home.join(&name);
        if names.contains(&name) || !metadata(&slot)?.is_some_and(|entry| entry.is_symlink()) {
            continue;
        }
        let link = fs::read_link(&slot).map_err(|error| io_err(&slot, error))?;
        if link.as_os_str() == prefix.join(&name).as_os_str()
            || link_target(&named_home, &link)?.starts_with(&default_home)
        {
            stale.push((name, slot));
        }
    }
    // A provider appends by path, so a directory, or a link to one, leaves
    // its slot only when no other agent on the account can be writing and no
    // provider daemon holds a session under the account home. A link at an
    // unshared name is exempt: the provider must own that name in each home,
    // so the link is the defect.
    let set_aside = plan
        .iter()
        .find(|planned| matches!(planned.action, Action::SetAside(_)) && planned.slot.is_dir())
        .map(|planned| (planned.name.clone(), false));
    let unlinked = stale
        .iter()
        .find(|(name, slot)| !is_unshared(adapter, name) && slot.is_dir())
        .map(|(name, _)| (name.clone(), true));
    if let Some((entry, unlink)) = set_aside.or(unlinked) {
        let agents = other_live_agents();
        let account = homes.account.clone();
        match (agents, unlink) {
            (Some(0), _) => {
                let sessions = match (homes.daemon_writes)() {
                    DaemonSessions::Clear => None,
                    DaemonSessions::Live(count) => Some(Some(count)),
                    DaemonSessions::Unknown => Some(None),
                };
                if let Some(sessions) = sessions {
                    return Err(if unlink {
                        ShareErr::LiveDaemonUnlink {
                            account,
                            entry,
                            default_home,
                            sessions,
                        }
                    } else {
                        ShareErr::LiveDaemon {
                            account,
                            entry,
                            default_home,
                            sessions,
                        }
                    });
                }
            }
            (_, false) => {
                return Err(ShareErr::LiveAgents {
                    account,
                    entry,
                    default_home,
                    agents,
                });
            }
            (_, true) => {
                return Err(ShareErr::LiveAgentsUnlink {
                    account,
                    entry,
                    default_home,
                    agents,
                });
            }
        }
    }

    let mut report = ShareReport {
        account: homes.account.clone(),
        default_home: default_home.clone(),
        linked: Vec::new(),
        current: Vec::new(),
        moved: Vec::new(),
        unlinked: Vec::new(),
        set_aside: Vec::new(),
        repaired: Vec::new(),
        left_local: Vec::new(),
        notes: Vec::new(),
    };
    let applied = apply(
        adapter,
        &named_home,
        &default_home,
        &dirs,
        plan,
        stale,
        &mut report,
    );
    match applied {
        Ok(()) => Ok(report),
        Err(source) if report.warnings().is_empty() => Err(source),
        Err(source) => Err(ShareErr::AfterSetAside {
            warnings: report.warnings(),
            source: Box::new(source),
        }),
    }
}

fn apply(
    adapter: &AgentDefinition,
    named_home: &Path,
    default_home: &Path,
    dirs: &[&str],
    plan: Vec<PlannedLink>,
    stale: Vec<(String, PathBuf)>,
    report: &mut ShareReport,
) -> Result<(), ShareErr> {
    let mut aside_dir = None;
    for PlannedLink {
        name,
        slot,
        target,
        link_text,
        action,
    } in plan
    {
        if dirs.contains(&name.as_str()) && !matches!(action, Action::Move(_)) {
            fs::create_dir_all(&target).map_err(|error| io_err(&target, error))?;
        }
        if let Action::Move(kind) | Action::SetAside(kind) = &action
            && name == "skills"
            && kind.is_dir()
        {
            skill_links::plan(
                &slot,
                &crate::disk::paths::skills_library(),
                skill_links::Desired::None,
            )
            .and_then(|plan| skill_links::apply(&plan))
            .map_err(|skill_links::SkillLinkErr::Io { path, source }| io_err(&path, source))?;
        }
        match action {
            Action::Current => {
                report.current.push(name);
                continue;
            }
            Action::Link => {}
            Action::Replace => {
                atomic::symlink_atomically(&link_text, &slot)
                    .map_err(|source| ShareErr::Relink { path: slot, source })?;
                report.linked.push(name);
                continue;
            }
            Action::Move(_) => {
                fs::create_dir_all(default_home).map_err(|error| io_err(default_home, error))?;
                match fs::rename(&slot, &target) {
                    Ok(()) => report.moved.push(name.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                        report.left_local.push(slot);
                        continue;
                    }
                    Err(error) => return Err(io_err(&slot, error)),
                }
            }
            Action::SetAside(kind) => {
                if kind.is_file()
                    && let Some(note) =
                        adapter
                            .adopt_shared_file(&name, &slot, &target)
                            .map_err(|source| ShareErr::Adopt {
                                path: slot.clone(),
                                source,
                            })?
                {
                    report.notes.push(note);
                }
                let dir = match &aside_dir {
                    Some(dir) => dir,
                    None => aside_dir.insert(new_aside_dir(named_home)?),
                };
                let aside = dir.join(&name);
                set_aside(named_home, &slot, &aside, &kind)?;
                report.set_aside.push((slot.clone(), aside));
            }
        }
        std::os::unix::fs::symlink(&link_text, &slot).map_err(|error| io_err(&slot, error))?;
        report.linked.push(name);
    }
    for (name, slot) in stale {
        fs::remove_file(&slot).map_err(|error| io_err(&slot, error))?;
        if is_unshared(adapter, &name) {
            report.repaired.push(slot);
        } else {
            report.unlinked.push(name);
        }
    }
    Ok(())
}

/// Move the entry at `slot` to `aside`. A relative link resolves against the
/// real directory holding it, so it is rewritten against the canonical
/// account home without folding `..`, preserving the kernel's resolution.
fn set_aside(
    named_home: &Path,
    slot: &Path,
    aside: &Path,
    kind: &fs::FileType,
) -> Result<(), ShareErr> {
    if kind.is_symlink() {
        let link = fs::read_link(slot).map_err(|error| io_err(slot, error))?;
        if link.is_relative() {
            let real_home = named_home
                .canonicalize()
                .map_err(|error| io_err(named_home, error))?;
            std::os::unix::fs::symlink(real_home.join(link), aside)
                .map_err(|error| io_err(aside, error))?;
            return fs::remove_file(slot).map_err(|error| io_err(slot, error));
        }
    }
    fs::rename(slot, aside).map_err(|error| io_err(slot, error))
}

/// A fresh `<account home>/.rimz-aside/<UTC timestamp>` directory, so a second
/// conflict on one name never meets the first.
fn new_aside_dir(named_home: &Path) -> Result<PathBuf, ShareErr> {
    let root = named_home.join(ASIDE_DIR);
    fs::create_dir_all(&root).map_err(|error| io_err(&root, error))?;
    let stamp = jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string();
    let mut dir = root.join(&stamp);
    let mut attempt = 1;
    loop {
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                dir = root.join(format!("{stamp}-{attempt}"));
            }
            Err(error) => return Err(io_err(&dir, error)),
        }
    }
}

#[cfg(test)]
mod tests;
