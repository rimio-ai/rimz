//! Git and linked pack sources, durable installation records, and namespace claims.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::disk::{atomic, lock::WorkspaceLock, paths};
use crate::utils::path::normalize_path_lexical;

mod git;
mod layout;
mod source;
pub mod transaction;

#[cfg(test)]
mod tests;

const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PackName(String);

impl PackName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PackName {
    type Err = PackErr;

    fn from_str(value: &str) -> Result<Self> {
        if !safe_name(value) {
            return Err(PackErr::InvalidName {
                name: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for PackName {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Agents,
    Subagents,
    Teams,
    Traits,
    Skills,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ItemFate {
    Loads,
    Overridden {
        path: PathBuf,
    },
    Excluded,
    ExcludedType,
    Conflict {
        claims: Vec<PackClaim>,
        message: String,
    },
    Failed {
        message: String,
    },
}

impl ItemFate {
    pub(crate) fn problem(&self) -> Option<&str> {
        match self {
            Self::Conflict { message, .. } | Self::Failed { message } => Some(message),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PackItem {
    #[serde(skip)]
    pub pack: PackName,
    #[serde(rename = "type")]
    pub kind: ItemType,
    pub name: String,
    pub path: PathBuf,
    pub fate: ItemFate,
}

impl ItemType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Subagents => "subagents",
            Self::Teams => "teams",
            Self::Traits => "traits",
            Self::Skills => "skills",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ItemKey {
    #[serde(rename = "type")]
    pub kind: ItemType,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusions {
    #[serde(default)]
    pub types: BTreeSet<ItemType>,
    #[serde(default)]
    pub items: BTreeSet<ItemKey>,
}

impl Exclusions {
    pub fn contains(&self, kind: ItemType, name: &str) -> bool {
        self.types.contains(&kind)
            || self.items.contains(&ItemKey {
                kind,
                name: name.to_owned(),
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExclusionTarget {
    Type(ItemType),
    Item(ItemKey),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPack {
    pub name: PackName,
    #[serde(default)]
    pub excluded: Exclusions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSource {
    pub entry: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub linked: bool,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    pub commit: Option<String>,
    pub packs: Vec<InstalledPack>,
}

impl InstalledSource {
    pub fn spelling(&self) -> String {
        GitSource {
            source: self.source.clone(),
            url: self.url.clone().unwrap_or_else(|| self.source.clone()),
            pack: self.packs.first().map(|pack| pack.name.clone()),
            reference: None,
        }
        .spelling()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub sources: Vec<InstalledSource>,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            version: 1,
            sources: Vec::new(),
        }
    }
}

impl Record {
    /// Read installed identity without creating the root or following unrecorded entries.
    pub fn read(agents_home: &Path) -> std::result::Result<Self, Problem> {
        read_record(&paths::packs_dir(agents_home))
    }
}

#[derive(Debug, Default, Serialize)]
pub struct CheckoutReport {
    pub checkouts: Vec<Checkout>,
}

#[derive(Debug, Serialize)]
pub struct Checkout {
    pub packs: Vec<PackName>,
    pub source: String,
    pub commit: String,
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: CheckoutState,
    #[serde(skip)]
    pub reinstall: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CheckoutState {
    Clean,
    Modified {
        #[serde(skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        changed: Vec<String>,
    },
    Unreadable {
        error: String,
    },
}

/// Inspect recorded Git checkouts without refreshing indexes or reading linked sources.
pub fn inspect_checkouts(agents_home: &Path) -> Result<CheckoutReport> {
    let record = Record::read(agents_home)?;
    let root = paths::packs_dir(agents_home);
    let client = git::Client {
        timeout: std::time::Duration::from_secs(10),
        ..git::Client::default()
    };
    let mut checkouts = Vec::new();
    for source in record.sources.iter().filter(|source| !source.linked) {
        let path = layout::checkout(&root, source)?;
        // layout::checkout rejects missing or malformed recorded commits.
        let commit = source.commit.clone().expect("validated checkout commit");
        let mut reinstall = source.spelling();
        if source.pinned
            && let Some(reference) = &source.reference
        {
            reinstall.push('@');
            reinstall.push_str(reference);
        }
        let state = client
            .inspect(&path, &commit, &source.source)
            .unwrap_or_else(|error| CheckoutState::Unreadable {
                error: error.to_string(),
            });
        checkouts.push(Checkout {
            packs: source.packs.iter().map(|pack| pack.name.clone()).collect(),
            source: source.source.clone(),
            commit,
            path,
            state,
            reinstall,
        });
    }
    Ok(CheckoutReport { checkouts })
}

fn read_record(root: &Path) -> std::result::Result<Record, Problem> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }

    let path = root.join("installed.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Record::default()),
        Err(error) => return Err(problem(&path, error.to_string())),
    };
    let version: Version =
        serde_json::from_slice(&bytes).map_err(|error| problem(&path, error.to_string()))?;
    if version.version > 1 {
        return Err(problem(
            &path,
            "written by a newer RimZ; upgrade RimZ to load these packs",
        ));
    }
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|error| problem(&path, error.to_string()))?;
    let mut entries = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut names = BTreeSet::new();
    for source in &record.sources {
        if !safe_name(&source.entry) || !entries.insert(&source.entry) {
            return Err(problem(
                &path,
                format!("invalid or duplicate entry {:?}", source.entry),
            ));
        }
        if source.source.is_empty() || !sources.insert(&source.source) {
            return Err(problem(
                &path,
                format!("empty or duplicate source {:?}", source.source),
            ));
        }
        for pack in &source.packs {
            if !names.insert(&pack.name) {
                return Err(problem(&path, format!("duplicate pack name {}", pack.name)));
            }
        }
    }
    Ok(record)
}

#[derive(Clone, Debug, Deserialize)]
struct PackManifest {
    name: PackName,
    description: String,
    path: PathBuf,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    requires: Vec<String>,
    rimz: Option<String>,
    #[serde(skip)]
    rimz_requirement: Option<semver::VersionReq>,
}

#[derive(Clone, Debug)]
struct Manifest {
    packs: Vec<PackManifest>,
    warnings: Vec<Problem>,
}

impl Manifest {
    /// Validate the complete source before installation, including directory containment.
    fn read(source: &Path) -> std::result::Result<Self, Problem> {
        let manifest = read_manifest(source)?;
        let roots = manifest_roots(source, &manifest);
        for pack in &manifest.packs {
            if let Some(Err(error)) = roots.get(&pack.name) {
                return Err(problem(
                    &source.join("rimz-packs.toml"),
                    format!("pack {} path: {error}", pack.name),
                ));
            }
        }
        Ok(manifest)
    }
}

fn read_manifest(source: &Path) -> std::result::Result<Manifest, Problem> {
    #[derive(Deserialize)]
    struct ManifestFile {
        #[serde(default)]
        pack: Vec<PackManifest>,
    }

    let path = source.join("rimz-packs.toml");
    let invalid = |message: String| {
        problem(
            &path,
            format!("{message}; expected [[pack]] with name, description and a relative path"),
        )
    };
    let text = fs::read_to_string(&path).map_err(|error| invalid(error.to_string()))?;
    let deserializer =
        toml::Deserializer::parse(&text).map_err(|error| invalid(error.to_string()))?;
    let mut warnings = Vec::new();
    let mut file: ManifestFile = serde_ignored::deserialize(deserializer, |key| {
        warnings.push(problem(&path, format!("unknown manifest key `{key}`")));
    })
    .map_err(|error| invalid(error.to_string()))?;
    if file.pack.is_empty() {
        return Err(invalid("manifest needs at least one [[pack]]".into()));
    }
    let absolute = std::path::absolute(source).map_err(|error| invalid(error.to_string()))?;
    let absolute = normalize_path_lexical(&absolute);
    let mut names = BTreeSet::new();
    let mut pack_paths = BTreeSet::new();
    for pack in &mut file.pack {
        if !names.insert(pack.name.clone()) {
            return Err(invalid(format!("duplicate pack name {}", pack.name)));
        }
        if pack.description.trim().is_empty() {
            return Err(invalid(format!(
                "pack {} description must not be empty",
                pack.name
            )));
        }
        let normalized = normalize_path_lexical(&absolute.join(&pack.path));
        if pack.path.is_absolute() || !normalized.starts_with(&absolute) {
            return Err(invalid(format!(
                "pack {} path must be relative and stay inside the source",
                pack.name
            )));
        }
        if !pack_paths.insert(normalized) {
            return Err(invalid(format!(
                "duplicate pack path {}",
                pack.path.display()
            )));
        }
        pack.requires.retain(|required| {
            if source::requirement(required, "").is_some() {
                return true;
            }
            warnings.push(problem(
                &path,
                format!(
                    "pack {} ignores malformed requires entry {required:?}; use a sibling name, \
                     crew/pack, skills/pack or github:owner/repo/pack, without refs or URLs",
                    pack.name
                ),
            ));
            false
        });
        if let Some(rimz) = &pack.rimz {
            match semver::VersionReq::parse(rimz) {
                Ok(requirement) => pack.rimz_requirement = Some(requirement),
                Err(error) => {
                    warnings.push(problem(
                        &path,
                        format!(
                            "pack {} ignores malformed rimz requirement {rimz:?}: {error}",
                            pack.name
                        ),
                    ));
                    pack.rimz = None;
                }
            }
        }
    }
    Ok(Manifest {
        packs: file.pack,
        warnings,
    })
}

fn manifest_roots(
    source: &Path,
    manifest: &Manifest,
) -> BTreeMap<PackName, std::result::Result<PathBuf, Problem>> {
    let mut roots = BTreeMap::new();
    let mut canonical_paths = BTreeMap::new();
    for pack in &manifest.packs {
        let checked = checked_pack_root(source, pack);
        if let Ok((_, canonical)) = &checked
            && let Some(previous) = canonical_paths.insert(canonical.clone(), pack.name.clone())
        {
            let error = problem(
                &source.join("rimz-packs.toml"),
                format!("duplicate path for packs {previous} and {}", pack.name),
            );
            roots.insert(previous, Err(error.clone()));
            roots.insert(pack.name.clone(), Err(error));
            continue;
        }
        roots.insert(pack.name.clone(), checked.map(|(lexical, _)| lexical));
    }
    roots
}

fn checked_pack_root(
    source: &Path,
    pack: &PackManifest,
) -> std::result::Result<(PathBuf, PathBuf), Problem> {
    let path = source.join(&pack.path);
    let absolute = std::path::absolute(&path).map_err(|error| problem(&path, error.to_string()))?;
    let lexical = normalize_path_lexical(&absolute);
    let invalid =
        |message: String| problem(&lexical, format!("pack {} path: {message}", pack.name));
    let canonical_source = fs::canonicalize(source).map_err(|error| invalid(error.to_string()))?;
    let canonical_spelling =
        fs::canonicalize(&path).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    let canonical = fs::canonicalize(&lexical)
        .map_err(|error| invalid(format!("{}: {error}", lexical.display())))?;
    if !canonical_spelling.starts_with(&canonical_source)
        || !canonical.starts_with(&canonical_source)
    {
        return Err(invalid("leaves the source after resolving symlinks".into()));
    }
    if !fs::metadata(&canonical)
        .map_err(|error| invalid(error.to_string()))?
        .is_dir()
    {
        return Err(invalid("must be an existing directory".into()));
    }
    Ok((lexical, canonical))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{path}: {message}")]
pub struct Problem {
    pub path: PathBuf,
    pub message: String,
}

fn problem(path: &Path, message: impl Into<String>) -> Problem {
    Problem {
        path: path.to_owned(),
        message: message.into(),
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedPack {
    pub name: PackName,
    pub description: String,
    pub source: String,
    pub linked: bool,
    pub entry: String,
    pub root: PathBuf,
    pub excluded: Exclusions,
}

#[derive(Clone, Debug, Default)]
pub struct ResolvedPacks {
    pub record: Record,
    pub packs: Vec<ResolvedPack>,
    pub problems: Vec<Problem>,
    pub warnings: Vec<Problem>,
    pub(crate) pack_problems: BTreeMap<PackName, Vec<Problem>>,
}

/// Read recorded packs in order, retaining problems while omitting unusable packs.
pub fn resolve(agents_home: &Path) -> ResolvedPacks {
    resolve_dir(&paths::packs_dir(agents_home))
}

fn resolve_dir(root: &Path) -> ResolvedPacks {
    let record = match read_record(root) {
        Ok(record) => record,
        Err(error) => {
            return ResolvedPacks {
                problems: vec![error],
                ..ResolvedPacks::default()
            };
        }
    };
    resolve_record(root, record, &BTreeMap::new())
}

fn resolve_record(
    root: &Path,
    record: Record,
    overrides: &BTreeMap<String, PathBuf>,
) -> ResolvedPacks {
    // Cargo validates the package version as SemVer.
    let version = semver::Version::parse(RUNNING_VERSION).expect("valid Cargo package version");
    let mut packs = Vec::new();
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let mut pack_problems = BTreeMap::<PackName, Vec<Problem>>::new();
    for source in &record.sources {
        let entry = overrides
            .get(&source.entry)
            .cloned()
            .unwrap_or_else(|| root.join(&source.entry));
        let manifest = match read_manifest(&entry) {
            Ok(manifest) => manifest,
            Err(error) => {
                for pack in &source.packs {
                    pack_problems
                        .entry(pack.name.clone())
                        .or_default()
                        .push(error.clone());
                }
                problems.push(error);
                continue;
            }
        };
        let mut roots = manifest_roots(&entry, &manifest);
        warnings.extend(manifest.warnings);
        for installed in &source.packs {
            let Some(pack) = manifest
                .packs
                .iter()
                .find(|pack| pack.name == installed.name)
            else {
                let name = &installed.name;
                let message = format!(
                    "recorded pack {name} is no longer listed; restore its manifest entry \
                     or run `rimz packs remove {name}`"
                );
                let error = problem(&entry.join("rimz-packs.toml"), message);
                pack_problems
                    .entry(name.clone())
                    .or_default()
                    .push(error.clone());
                problems.push(error);
                continue;
            };
            let pack_root = match roots.remove(&pack.name) {
                Some(Ok(root)) => root,
                Some(Err(error)) => {
                    pack_problems
                        .entry(pack.name.clone())
                        .or_default()
                        .push(error.clone());
                    problems.push(error);
                    continue;
                }
                // Each manifest entry was checked above; names are unique.
                None => unreachable!("manifest pack has a checked root"),
            };
            if let (Some(rimz), Some(requirement)) = (&pack.rimz, &pack.rimz_requirement)
                && !requirement.matches(&version)
            {
                let remedy = if source.linked {
                    format!("update the linked checkout at {}", source.source)
                } else if source.pinned {
                    let spelling = source.spelling();
                    // shlex refuses only a NUL byte, which a hand-edited record can carry.
                    let command = shlex::try_join(["rimz", "packs", "install", &spelling])
                        .unwrap_or_else(|_| format!("rimz packs install {spelling}"));
                    format!(
                        "pinned at {}; follow updates again with `{command}`",
                        source.reference.as_deref().unwrap_or("<unknown>")
                    )
                } else {
                    format!("run `rimz packs update {}`", pack.name)
                };
                warnings.push(problem(
                    &entry.join("rimz-packs.toml"),
                    format!(
                        "pack {} requires RimZ {rimz}, but the running version is {version}; \
                         the pack still loads; {remedy}",
                        pack.name
                    ),
                ));
            }
            packs.push(ResolvedPack {
                name: pack.name.clone(),
                description: pack.description.clone(),
                source: source.source.clone(),
                linked: source.linked,
                entry: source.entry.clone(),
                root: pack_root,
                excluded: installed.excluded.clone(),
            });
        }
    }
    ResolvedPacks {
        record,
        packs,
        problems,
        warnings,
        pack_problems,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PackClaim {
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub kind: ItemType,
    pub pack: PackName,
    pub path: PathBuf,
    #[serde(skip)]
    pub excluded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimResolution {
    Home(PathBuf),
    Pack(PackClaim),
    Conflict {
        claims: Vec<PackClaim>,
        message: String,
    },
}

/// Resolve one namespace without I/O. Home claims include files that failed to load.
pub fn resolve_claims(
    home: impl IntoIterator<Item = (String, PathBuf)>,
    packs: impl IntoIterator<Item = PackClaim>,
) -> BTreeMap<String, ClaimResolution> {
    let mut resolved: BTreeMap<_, _> = home
        .into_iter()
        .map(|(name, path)| (name, ClaimResolution::Home(path)))
        .collect();
    let mut claims = BTreeMap::<String, Vec<PackClaim>>::new();
    for claim in packs {
        if claim.excluded || resolved.contains_key(&claim.name) {
            continue;
        }
        claims.entry(claim.name.clone()).or_default().push(claim);
    }
    for (name, claims) in claims {
        let resolution = match claims.as_slice() {
            [claim] => ClaimResolution::Pack(claim.clone()),
            _ => {
                let origins = claims
                    .iter()
                    .map(|claim| format!("{} ({})", claim.pack, claim.path.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let fixes = claims
                    .iter()
                    .map(|claim| {
                        format!(
                            "`rimz packs exclude {}:{name} --type {}`",
                            claim.pack,
                            claim.kind.as_str()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" or ");
                let message = format!(
                    "pack conflict for `{name}`: {origins}; exclude conflicting copies ({fixes}), \
                     or add a file of your own named `{name}`"
                );
                ClaimResolution::Conflict { claims, message }
            }
        };
        resolved.insert(name, resolution);
    }
    resolved
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallSource {
    Linked(PathBuf),
    Git(GitSource),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSource {
    pub source: String,
    pub url: String,
    pub pack: Option<PackName>,
    pub reference: Option<String>,
}

impl FromStr for InstallSource {
    type Err = PackErr;

    fn from_str(value: &str) -> Result<Self> {
        if matches!(value, "." | ".." | "~")
            || ["/", "./", "../", "~/"]
                .iter()
                .any(|prefix| value.starts_with(prefix))
        {
            return Ok(Self::Linked(PathBuf::from(value)));
        }
        source::parse(value).map(Self::Git)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackErr {
    #[error(transparent)]
    Invalid(#[from] Problem),
    #[error("invalid pack name {name:?}; use only letters, digits, '_' or '-'")]
    InvalidName { name: String },
    #[error(
        "invalid pack source {argument:?}; use a local path ./path \
         (or '.', '..', '~', '/', '../', '~/'), crew[/pack][@ref], skills[/pack][@ref], \
         github:owner/repo[/pack][@ref], \
         or a Git URL[@ref]"
    )]
    NotPath { argument: String },
    #[error("Git source {source_url}: {message}")]
    Git { source_url: String, message: String },
    #[error("pack installation changed while staging; nothing changed; run the command again")]
    RecordChanged,
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Lock(#[from] crate::disk::lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("source {source_path} overlaps agents home {agents_home}; move the source outside it")]
    OverlappingHome {
        source_path: PathBuf,
        agents_home: PathBuf,
    },
    #[error(
        "pack {name} is already provided by {source_path}; run `rimz packs remove {name}` first"
    )]
    NameTaken { name: PackName, source_path: String },
    #[error("unknown pack {name}; installed packs: {installed}")]
    UnknownPack { name: PackName, installed: String },
    #[error(
        "{pack}'s {kind} type is excluded; run `rimz packs include {pack} --type {kind}` first"
    )]
    TypeExcluded { pack: PackName, kind: &'static str },
}

pub type Result<T> = std::result::Result<T, PackErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallStatus {
    Installed,
    Refreshed,
    UpToDate,
}

#[derive(Clone, Debug)]
pub struct InstallOutcome {
    pub status: InstallStatus,
    pub entry: String,
    pub packs: Vec<PackName>,
    pub added: Vec<PackName>,
    pub dropped: Vec<PackName>,
    pub warnings: Vec<Problem>,
}

/// Link all manifest packs, then commit their identity under the packs lock.
pub fn install(agents_home: &Path, source: &Path) -> Result<InstallOutcome> {
    let expanded = if let Ok(rest) = source.strip_prefix("~") {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| problem(source, "HOME is unset; use an absolute source path"))?;
        PathBuf::from(home).join(rest)
    } else {
        source.to_owned()
    };
    let source_path = fs::canonicalize(&expanded).map_err(|error| io_err(source, error))?;
    if !source_path.is_dir() {
        return Err(problem(source, "source must be an existing directory").into());
    }
    let canonical_home = canonical_home(agents_home)?;
    if source_path.starts_with(&canonical_home) || canonical_home.starts_with(&source_path) {
        return Err(PackErr::OverlappingHome {
            source_path,
            agents_home: canonical_home,
        });
    }
    let source_text = source_path
        .to_str()
        .ok_or_else(|| problem(source, "source path must be valid UTF-8"))?;
    let root = paths::packs_dir(agents_home);
    let _guard = WorkspaceLock::acquire(&root.join("installed.lock"))?;
    let mut record = read_record(&root)?;
    layout::maintain(&root, &record)?;
    let manifest = Manifest::read(&source_path)?;
    let existing = record
        .sources
        .iter()
        .position(|source| source.source == source_text);
    for pack in &manifest.packs {
        if let Some(owner) = record.sources.iter().find(|owner| {
            owner.source != source_text
                && owner
                    .packs
                    .iter()
                    .any(|installed| installed.name == pack.name)
        }) {
            return Err(PackErr::NameTaken {
                name: pack.name.clone(),
                source_path: owner.source.clone(),
            });
        }
    }
    let entry = existing
        .map(|index| record.sources[index].entry.clone())
        .unwrap_or_else(|| entry_name(&source_path, &record));
    let previous = existing
        .map(|index| record.sources[index].packs.as_slice())
        .unwrap_or_default();
    let packs: Vec<_> = manifest
        .packs
        .iter()
        .map(|pack| InstalledPack {
            name: pack.name.clone(),
            excluded: previous
                .iter()
                .find(|installed| installed.name == pack.name)
                .map(|installed| installed.excluded.clone())
                .unwrap_or_default(),
        })
        .collect();
    let added = packs
        .iter()
        .filter(|pack| !previous.iter().any(|old| old.name == pack.name))
        .map(|pack| pack.name.clone())
        .collect();
    let dropped = previous
        .iter()
        .filter(|old| !packs.iter().any(|pack| pack.name == old.name))
        .map(|pack| pack.name.clone())
        .collect();
    let next = InstalledSource {
        entry: entry.clone(),
        source: source_text.to_owned(),
        url: None,
        linked: true,
        reference: None,
        pinned: false,
        commit: None,
        packs,
    };
    let entry_path = root.join(&entry);
    let unchanged = existing.is_some_and(|index| record.sources[index] == next)
        && fs::read_link(&entry_path).is_ok_and(|target| target == source_path);
    let status = if unchanged {
        InstallStatus::UpToDate
    } else {
        if fs::symlink_metadata(&entry_path).is_ok_and(|metadata| metadata.is_dir()) {
            fs::remove_dir_all(&entry_path).map_err(|error| io_err(&entry_path, error))?;
        }
        atomic::symlink_atomically(&source_path, &entry_path)?;
        if let Some(index) = existing {
            record.sources[index] = next.clone();
        } else {
            record.sources.push(next.clone());
        }
        write_record(&root, &record)?;
        if existing.is_some() {
            InstallStatus::Refreshed
        } else {
            InstallStatus::Installed
        }
    };
    let mut warnings = manifest.warnings;
    for pack in &manifest.packs {
        for required in pack
            .requires
            .iter()
            .filter_map(|value| source::requirement(value, source_text))
        {
            if record
                .sources
                .iter()
                .flat_map(|source| &source.packs)
                .any(|pack| Some(&pack.name) == required.pack.as_ref())
            {
                continue;
            }
            let argument = if required.url == source_text {
                source_text.to_owned()
            } else {
                required.spelling()
            };
            // Parsed requirements always name a pack; neither they nor OS paths contain NUL.
            let command = shlex::try_join(["rimz", "packs", "install", &argument])
                .expect("paths and requirements carry no NUL byte");
            warnings.push(problem(
                &source_path.join("rimz-packs.toml"),
                format!(
                    "pack {} has an unmet requirement {}; run `{command}`",
                    pack.name,
                    required.pack.as_ref().expect("requirements name a pack")
                ),
            ));
        }
    }
    Ok(InstallOutcome {
        status,
        entry,
        packs: next.packs.into_iter().map(|pack| pack.name).collect(),
        added,
        dropped,
        warnings,
    })
}

fn io_err(path: &Path, source: std::io::Error) -> PackErr {
    PackErr::Io {
        path: path.to_owned(),
        source,
    }
}

fn canonical_home(home: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(home).map_err(|error| io_err(home, error))?;
    for ancestor in absolute.ancestors() {
        match fs::canonicalize(ancestor) {
            Ok(existing) => {
                // `ancestor` comes from this exact path's ancestors iterator.
                let suffix = absolute
                    .strip_prefix(ancestor)
                    .expect("ancestor is a prefix");
                return Ok(normalize_path_lexical(&existing.join(suffix)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_err(ancestor, error)),
        }
    }
    Err(problem(home, "agents home has no accessible ancestor").into())
}

fn entry_name(source: &Path, record: &Record) -> String {
    let stem: String = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source")
        .chars()
        .take(128)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let stem = if stem.is_empty() { "source" } else { &stem };
    let mut entry = stem.to_owned();
    let mut suffix = 2;
    while record.sources.iter().any(|source| source.entry == entry) {
        entry = format!("{stem}-{suffix}");
        suffix += 1;
    }
    entry
}

fn write_record(root: &Path, record: &Record) -> Result<()> {
    let mut record = record.clone();
    record.version = 1;
    atomic::write_bytes_atomically(
        &root.join("installed.json"),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RemoveOutcome {
    pub pack: PackName,
    pub source: String,
    pub removed_entry: Option<PathBuf>,
    pub cleanup_warning: Option<Problem>,
}

/// Commit removal before entry cleanup, returning any cleanup failure alongside the receipt.
pub fn remove(agents_home: &Path, name: &PackName) -> Result<RemoveOutcome> {
    let root = paths::packs_dir(agents_home);
    let _guard = WorkspaceLock::acquire(&root.join("installed.lock"))?;
    let mut record = read_record(&root)?;
    let mut cleanup_warning = layout::maintain(&root, &record)
        .err()
        .map(|error| problem(&root, format!("pack maintenance failed: {error}")));
    let index = source_index(&record, name)?;
    let source = &mut record.sources[index];
    source.packs.retain(|pack| pack.name != *name);
    let source_text = source.source.clone();
    let removed = source
        .packs
        .is_empty()
        .then(|| record.sources.remove(index));
    write_record(&root, &record)?;
    let removed_entry = if let Some(source) = removed {
        let path = root.join(source.entry);
        let result = fs::remove_file(&path);
        match result {
            Ok(()) => Some(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(path),
            Err(error) => {
                let cleanup = problem(
                    &path,
                    format!(
                        "entry cleanup failed: {error}; this unrecorded entry is ignored by loaders and replaced by the next install of this source"
                    ),
                );
                if let Some(warning) = &mut cleanup_warning {
                    warning.message.push_str(&format!("; {cleanup}"));
                } else {
                    cleanup_warning = Some(cleanup);
                }
                None
            }
        }
    } else {
        None
    };
    Ok(RemoveOutcome {
        pack: name.clone(),
        source: source_text,
        removed_entry,
        cleanup_warning,
    })
}

fn source_index(record: &Record, name: &PackName) -> Result<usize> {
    record
        .sources
        .iter()
        .position(|source| source.packs.iter().any(|pack| pack.name == *name))
        .ok_or_else(|| PackErr::UnknownPack {
            name: name.clone(),
            installed: record
                .sources
                .iter()
                .flat_map(|source| &source.packs)
                .map(|pack| pack.name.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExclusionOutcome {
    Changed,
    AlreadyExcluded,
    AlreadyIncluded,
}

/// Exclude an exact, caller-matched target without scanning the pack's item trees.
pub fn exclude(
    agents_home: &Path,
    name: &PackName,
    target: &ExclusionTarget,
) -> Result<ExclusionOutcome> {
    change_exclusion(agents_home, name, target, true)
}

/// Include an exact target, refusing an item while its whole type remains excluded.
pub fn include(
    agents_home: &Path,
    name: &PackName,
    target: &ExclusionTarget,
) -> Result<ExclusionOutcome> {
    change_exclusion(agents_home, name, target, false)
}

fn change_exclusion(
    agents_home: &Path,
    name: &PackName,
    target: &ExclusionTarget,
    exclude: bool,
) -> Result<ExclusionOutcome> {
    let root = paths::packs_dir(agents_home);
    let _guard = WorkspaceLock::acquire(&root.join("installed.lock"))?;
    let mut record = read_record(&root)?;
    layout::maintain(&root, &record)?;
    let index = source_index(&record, name)?;
    // source_index found this pack in this source, and the record's names are unique.
    let pack = record.sources[index]
        .packs
        .iter_mut()
        .find(|pack| pack.name == *name)
        .expect("source holds the named pack");
    let changed = match target {
        ExclusionTarget::Type(kind) => {
            if exclude {
                pack.excluded.types.insert(*kind)
            } else {
                pack.excluded.types.remove(kind)
            }
        }
        ExclusionTarget::Item(item) => {
            if pack.excluded.types.contains(&item.kind) {
                if exclude {
                    return Ok(ExclusionOutcome::AlreadyExcluded);
                }
                return Err(PackErr::TypeExcluded {
                    pack: name.clone(),
                    kind: item.kind.as_str(),
                });
            }
            if exclude {
                pack.excluded.items.insert(item.clone())
            } else {
                pack.excluded.items.remove(item)
            }
        }
    };
    if !changed {
        return Ok(if exclude {
            ExclusionOutcome::AlreadyExcluded
        } else {
            ExclusionOutcome::AlreadyIncluded
        });
    }
    write_record(&root, &record)?;
    Ok(ExclusionOutcome::Changed)
}
