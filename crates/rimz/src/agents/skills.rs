//! Host skill discovery and provider-owned per-launch skill and permission settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::SkillName;
use crate::packs::{
    self, ClaimResolution, ItemFate, ItemType, PackClaim, PackItem, PackName, ResolvedPack,
};
use crate::utils::path::normalize_path_lexical;

/// The project skills a launch reaches: the skill directories under `<checkout>/.agents/skills`,
/// and the folder under the checkout the launched kind reads project skills from by itself.
#[derive(Clone, Debug)]
pub(crate) struct ProjectTier {
    checkout: PathBuf,
    kind: String,
    reads: crate::agents::ProjectSkills,
    manual: crate::agents::ManualSkill,
}

impl ProjectTier {
    pub(crate) fn new(checkout: PathBuf, adapter: &crate::agents::AgentDefinition) -> Self {
        Self {
            checkout,
            kind: adapter.spec().kind.to_owned(),
            reads: adapter.project_skills(),
            manual: adapter.manual_skill(),
        }
    }

    /// The tier of a launch in `cwd`: that of the Git checkout enclosing `cwd`, else of `cwd`
    /// itself. Under `empty_root` only `cwd` counts, since the checkout around RimZ's state is
    /// not the agent's.
    pub(crate) fn discover(
        cwd: &Path,
        empty_root: &Path,
        adapter: &crate::agents::AgentDefinition,
    ) -> Result<Self, SkillErr> {
        let checkout = if crate::disk::paths::is_under_root(cwd, empty_root) {
            None
        } else {
            crate::worktree::discover_checkout(cwd).map_err(|error| SkillErr::Io {
                path: error.path,
                source: error.source,
            })?
        };
        Ok(Self::new(
            checkout.unwrap_or_else(|| cwd.to_owned()),
            adapter,
        ))
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.checkout.join(".agents/skills")
    }

    /// The folder under the checkout the kind reads project skills from by itself.
    pub(crate) fn native_folder(&self) -> Option<PathBuf> {
        match self.reads {
            crate::agents::ProjectSkills::Native => Some(self.root()),
            crate::agents::ProjectSkills::Own {
                folder,
                main_checkout,
                ..
            } => {
                let own = self.checkout.join(folder);
                let main = (main_checkout && !own.exists())
                    .then(|| crate::worktree::linked_worktree(&self.checkout))
                    .flatten()
                    .map(|worktree| worktree.primary.join(folder));
                Some(main.unwrap_or(own))
            }
            crate::agents::ProjectSkills::Unread => None,
        }
    }

    /// The folder whose project skills a launch's inventory holds: the tier in a scoped view,
    /// which merges it for a kind that does not read it, else the kind's native folder.
    pub(crate) fn folder(&self, scoped: bool) -> Option<PathBuf> {
        if scoped {
            Some(self.root())
        } else {
            self.native_folder()
        }
    }

    /// Whether the kind loads its user root's copy of a name over its native folder's.
    pub(crate) fn user_first(&self) -> bool {
        matches!(
            self.reads,
            crate::agents::ProjectSkills::Own {
                user_first: true,
                ..
            }
        )
    }

    /// The refusal of listed `name`, resolved to `skill`, when its author marks it user-only
    /// for the kind.
    pub(crate) fn user_only(&self, name: &SkillName, skill: &Path) -> Option<SkillErr> {
        user_only_refusal(name, skill, &self.kind, self.manual)
    }

    /// Whether the tier holds `name` and the kind does not read the tier.
    fn holds_unread(&self, name: &SkillName) -> bool {
        self.reads != crate::agents::ProjectSkills::Native
            && self.root().join(name.as_str()).join("SKILL.md").exists()
    }

    /// The refusal of a host launch whose inventory lacks listed `name`, when only this tier,
    /// which the kind does not read, holds it; `None` when the tier cannot supply it.
    pub(crate) fn only_unread_copy(&self, name: &SkillName) -> Option<SkillErr> {
        if !self.holds_unread(name) {
            return None;
        }
        let root = self.root();
        let mut fixes = Vec::new();
        if let crate::agents::ProjectSkills::Own { folder, .. } = self.reads {
            fixes.push(format!(
                "add {} (a symlink to {} works)",
                self.checkout.join(folder).join(name.as_str()).display(),
                root.join(name.as_str()).display()
            ));
        }
        if self.manual != crate::agents::ManualSkill::Unsupported {
            fixes.push("launch with scoped isolation".to_owned());
        }
        let last = "remove it from skills";
        Some(SkillErr::ProjectOnly {
            name: name.clone(),
            kind: self.kind.clone(),
            root,
            fix: if fixes.is_empty() {
                last.to_owned()
            } else {
                format!("{}, or {last}", fixes.join(", "))
            },
        })
    }
}

#[derive(Clone, Debug)]
pub struct SkillChain {
    pub(crate) library: PathBuf,
    packs_dir: PathBuf,
    packs: Vec<ResolvedPack>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SkillInventory {
    pub(crate) skills: BTreeMap<String, SkillDir>,
    pub(crate) conflicts: BTreeMap<String, packs::Problem>,
    pub(crate) pack_skills: Vec<PackItem>,
    pub(crate) pack_problems: BTreeMap<PackName, Vec<packs::Problem>>,
}

impl SkillChain {
    pub fn from_home(home: &Path) -> Self {
        Self::from_resolved(home, &packs::resolve(home))
    }

    pub(crate) fn from_resolved(home: &Path, resolved: &packs::ResolvedPacks) -> Self {
        let home = std::path::absolute(home)
            .map(|home| normalize_path_lexical(&home))
            .unwrap_or_else(|_| home.to_owned());
        Self {
            library: crate::disk::paths::skills_library_under(&home),
            packs_dir: crate::disk::paths::packs_dir(&home),
            packs: resolved.packs.clone(),
        }
    }

    pub(crate) fn from_env(env: &BTreeMap<String, String>) -> Option<Self> {
        let library = crate::disk::paths::skills_library_in(env)?;
        Some(Self::from_home(library.parent()?))
    }

    pub(crate) fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.library.clone())
            .chain(
                self.packs
                    .iter()
                    .map(|pack| crate::disk::paths::skills_library_under(&pack.root)),
            )
            .collect()
    }

    pub(super) fn owned_roots(&self) -> [&Path; 2] {
        [&self.library, &self.packs_dir]
    }

    pub(crate) fn owned_link(&self, path: &Path) -> Result<Option<PathBuf>, SkillErr> {
        match std::fs::read_link(path) {
            Ok(target) => Ok(super::skill_links::owned_target(
                path.parent().unwrap_or(path),
                self.owned_roots(),
                &target,
            )
            .map(|_| target)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(None)
            }
            Err(source) => Err(SkillErr::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub(crate) fn inventory(
        &self,
        keep: impl Fn(&str) -> bool,
    ) -> Result<SkillInventory, SkillErr> {
        self.read_inventory(keep, LibraryRead::Strict)
    }

    pub(super) fn inventory_for_links(&self) -> Result<SkillInventory, SkillErr> {
        self.read_inventory(|_| true, LibraryRead::SkipUnreadable)
    }

    fn read_inventory(
        &self,
        keep: impl Fn(&str) -> bool,
        library_read: LibraryRead,
    ) -> Result<SkillInventory, SkillErr> {
        let mut home = BTreeMap::new();
        let mut inventory = SkillInventory::default();
        for (name, path) in skill_paths(&self.library, &keep, true)? {
            match read_skill(name.clone(), path.clone(), false) {
                Ok(Some(skill)) => {
                    inventory.skills.insert(name.clone(), skill);
                }
                Ok(None) if !path.is_symlink() => continue,
                Ok(None) => {}
                Err(_) if matches!(library_read, LibraryRead::SkipUnreadable) => {}
                Err(error) => return Err(error),
            }
            home.insert(name, path);
        }
        let mut shipped = Vec::new();
        for pack in &self.packs {
            let scanned = scan_pack(pack, &home, &keep);
            inventory.pack_skills.extend(scanned.pack_skills);
            inventory.pack_problems.extend(scanned.pack_problems);
            for (name, skill) in scanned.skills {
                shipped.push((pack, name, skill));
            }
        }
        let claims = packs::resolve_claims(
            home.iter()
                .map(|(name, path)| (name.clone(), path.join("SKILL.md"))),
            shipped.iter().map(|(pack, name, skill)| PackClaim {
                name: name.clone(),
                kind: ItemType::Skills,
                pack: pack.name.clone(),
                path: skill.path.join("SKILL.md"),
                excluded: pack.excluded.contains(ItemType::Skills, name),
            }),
        );
        for (pack, name, skill) in shipped {
            let fate = match &claims[&name] {
                ClaimResolution::Home(path) => ItemFate::Overridden { path: path.clone() },
                ClaimResolution::Pack(_) => {
                    inventory.skills.insert(name.clone(), skill.clone());
                    ItemFate::Loads
                }
                ClaimResolution::Conflict { claims, message } => {
                    let message = format!(
                        "{message}; a provider-root skill of this name still wins for that provider"
                    );
                    inventory
                        .conflicts
                        .entry(name.clone())
                        .or_insert_with(|| packs::Problem {
                            path: skill.path.join("SKILL.md"),
                            message: message.clone(),
                        });
                    ItemFate::Conflict {
                        claims: claims.clone(),
                        message,
                    }
                }
            };
            inventory.pack_skills.push(PackItem {
                pack: pack.name.clone(),
                kind: ItemType::Skills,
                name,
                path: skill.path,
                fate,
            });
        }
        Ok(inventory)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderSkillKey(String);

impl ProviderSkillKey {
    pub(super) fn new(name: String) -> Self {
        Self(name)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct SkillDir {
    pub(crate) name: String,
    pub(crate) source: PathBuf,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub enum HostSkills {
    Unsupported,
    Switch {
        flag: &'static str,
        effect: &'static str,
        key: fn(&SkillDir) -> Result<ProviderSkillKey, LaunchSettingsErr>,
        render: RenderHostSkills,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum ToolRules {
    Unsupported,
    /// `translate` is one rule in the provider's grammar, `None` where it has none; `render`
    /// merges the translated rules into the launch's settings document.
    Settings {
        render: RenderToolRules,
        translate: fn(&crate::config::ToolRule) -> Option<String>,
    },
}

impl ToolRules {
    /// The rules a launch leaves out because the provider's grammar has no translation for
    /// them. An unsupported provider applies none, which its own warning covers.
    pub fn untranslated(self, rules: &[crate::config::ToolRule]) -> Vec<&crate::config::ToolRule> {
        match self {
            Self::Unsupported => Vec::new(),
            Self::Settings { translate, .. } => rules
                .iter()
                .filter(|rule| translate(rule).is_none())
                .collect(),
        }
    }
}

type RenderToolRules = fn(
    &[crate::config::ToolRule],
    (&Path, &Path),
    &mut Vec<String>,
    &mut Option<LaunchSettingsArtifact>,
) -> Result<(), LaunchSettingsErr>;

/// Publication path, settings, and original source path for chained-merge diagnostics.
pub(crate) type LaunchSettingsArtifact = (PathBuf, serde_json::Value, PathBuf);

type RenderHostSkills = fn(
    &[ProviderSkillKey],
    &Path,
    &Path,
    &mut Vec<String>,
) -> Result<Option<LaunchSettingsArtifact>, LaunchSettingsErr>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum HostSkillPlan {
    Applied {
        flag: &'static str,
        effect: &'static str,
        listed: Vec<ProviderSkillKey>,
        unlisted: Vec<ProviderSkillKey>,
    },
    Unenforced,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillErr {
    #[error("reading skills at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "unknown skill '{name}'; searched {roots:?}; install it in one of these roots or remove it from skills"
    )]
    UnknownSkill {
        name: SkillName,
        roots: Vec<PathBuf>,
    },
    #[error("{message}")]
    Conflict { message: String },
    #[error(
        "listed skill '{name}' is only in the project skills at {}, which {kind} does not read at host isolation; {fix}",
        .root.display()
    )]
    ProjectOnly {
        name: SkillName,
        kind: String,
        root: PathBuf,
        fix: String,
    },
    #[error(
        "listed skill '{name}' is marked user-only for {kind} at {}; listing cannot lift that marker: drop it from skills or remove the marker",
        .path.display()
    )]
    UserOnly {
        name: SkillName,
        kind: String,
        path: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchSettingsErr {
    #[error(transparent)]
    Skills(#[from] SkillErr),
    #[error("{0}; correct skill root access, or set allow-routine-rimz = false")]
    RoutineSkills(#[source] SkillErr),
    #[error("invalid launch settings at {path}: {reason}")]
    Settings { path: PathBuf, reason: String },
    #[error("skills sharing provider name {name:?} have conflicting invocation policies; list all of their directories or none", name = .key.as_str())]
    ConflictingKey { key: ProviderSkillKey },
}

/// The refusal of listed `name`, resolved to directory `skill`, when its author marks it
/// user-only for `kind`, whose marker is `manual`; an unreadable marker is left to the enforcer
/// that reads it.
fn user_only_refusal(
    name: &SkillName,
    skill: &Path,
    kind: &str,
    manual: crate::agents::ManualSkill,
) -> Option<SkillErr> {
    let marked = is_user_only(skill, manual, |path| std::fs::read_to_string(path));
    matches!(marked, Ok(true)).then(|| SkillErr::UserOnly {
        name: name.clone(),
        kind: kind.to_owned(),
        path: skill.join(match manual {
            crate::agents::ManualSkill::OpenAiPolicy => "agents/openai.yaml",
            _ => "SKILL.md",
        }),
    })
}

/// The skills named by `keep` in RimZ's inventory order: the provider root, then the
/// `project` folder, then the RimZ skill chain. The first copy of a name wins.
pub(crate) fn enumerate(
    provider_root: Option<&Path>,
    project: Option<&Path>,
    chain: Option<&SkillChain>,
    keep: impl Fn(&str) -> bool + Copy,
) -> Result<SkillInventory, SkillErr> {
    let provider =
        provider_root.map_or_else(|| Ok(BTreeMap::new()), |root| scan(root, chain, keep))?;
    let project = project.map_or_else(
        || Ok(BTreeMap::new()),
        |root| {
            project_folder(root, provider_root, |name| {
                keep(name) && !provider.contains_key(name)
            })
        },
    )?;
    let mut inventory = chain.map_or_else(
        || Ok(SkillInventory::default()),
        |chain| {
            chain.inventory(|name| {
                keep(name) && !provider.contains_key(name) && !project.contains_key(name)
            })
        },
    )?;
    inventory.skills.extend(project);
    inventory.skills.extend(provider);
    Ok(inventory)
}

/// The skills a launch of `project`'s kind resolves, in the order its enforcer reads them: the
/// provider root, the project folder of its inventory (the tier in a `scoped` view), the RimZ
/// skill chain, then in a scoped view the kind's native folder, which the view leaves native.
pub(crate) fn launch_inventory(
    provider_root: Option<&Path>,
    project: Option<&ProjectTier>,
    chain: Option<&SkillChain>,
    scoped: bool,
    keep: impl Fn(&str) -> bool + Copy,
) -> Result<SkillInventory, SkillErr> {
    let folder = project.and_then(|project| project.folder(scoped));
    let mut inventory = enumerate(provider_root, folder.as_deref(), chain, keep)?;
    let native = project
        .filter(|_| scoped)
        .and_then(ProjectTier::native_folder)
        .filter(|native| folder.as_ref() != Some(native));
    if let Some(native) = native {
        let own = project_folder(&native, provider_root, |name| {
            keep(name) && !inventory.skills.contains_key(name)
        })?;
        inventory.skills.extend(own);
    }
    Ok(inventory)
}

/// The skills project folder `root` supplies to RimZ: none when it is the provider root, which
/// RimZ reads as that, and never a `rimz-*` name, which only the provider root and the chain
/// supply. A link into RimZ's state is the project's own skill here.
fn project_folder(
    root: &Path,
    provider_root: Option<&Path>,
    keep: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, SkillDir>, SkillErr> {
    if provider_root.is_some_and(|provider| same_dir(provider, root)) {
        return Ok(BTreeMap::new());
    }
    scan(root, None, |name| !name.starts_with("rimz-") && keep(name))
}

/// Whether `a` and `b` name one directory, by their canonical paths where both resolve.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => normalize_path_lexical(a) == normalize_path_lexical(b),
    }
}

/// The skills in a project folder whose names satisfy `keep`. An entry without a `SKILL.md`
/// or a broken link is skipped, as the library read skips one.
pub(crate) fn project_skills(
    root: &Path,
    keep: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, SkillDir>, SkillErr> {
    scan(root, None, keep)
}

/// The skills on disk under a provider root and the RimZ skill chain whose names
/// satisfy `keep`. A root that is missing or cannot be read adds nothing, so the
/// set holds what the readable roots establish.
fn skills_on_disk(
    provider_root: Option<&Path>,
    chain: Option<&SkillChain>,
    keep: impl Fn(&str) -> bool,
) -> std::collections::BTreeSet<String> {
    let provider = provider_root
        .and_then(|root| skill_paths(root, &keep, false).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, path)| path.join("SKILL.md").exists())
        .map(|(name, _)| name);
    let library = chain
        .and_then(|chain| {
            chain
                .read_inventory(&keep, LibraryRead::SkipUnreadable)
                .ok()
        })
        .unwrap_or_default();
    provider.chain(library.skills.into_keys()).collect()
}

/// The `rimz-*` skills on disk under a provider root and the RimZ skill chain. A root
/// that is missing or cannot be read adds nothing, so the set holds what the
/// readable roots establish.
pub fn rimz_skills(
    provider_root: Option<&Path>,
    chain: Option<&SkillChain>,
) -> std::collections::BTreeSet<String> {
    skills_on_disk(provider_root, chain, |name| name.starts_with("rimz-"))
}

/// Every skill on disk a launch of `kind` in `env` resolves: those under its provider root and
/// the RimZ skill chain, and with a `cwd` those of the project folders a launch from there
/// reads, in a scoped view when `scoped`. A root that is missing or cannot be read adds
/// nothing, so the set holds what the readable roots establish.
pub fn available_skills(
    kind: &str,
    env: &BTreeMap<String, String>,
    chain: Option<&SkillChain>,
    cwd: Option<&Path>,
    scoped: bool,
) -> std::collections::BTreeSet<String> {
    let adapter = crate::agents::find_definition(kind);
    let provider_root = adapter.and_then(|adapter| adapter.skills_home(env));
    let mut skills = skills_on_disk(provider_root.as_deref(), chain, |_| true);
    let project = adapter.zip(cwd).and_then(|(adapter, cwd)| {
        ProjectTier::discover(cwd, &crate::disk::paths::empty_dir_root(), adapter).ok()
    });
    let folders = project.iter().flat_map(|project| {
        project
            .folder(scoped)
            .into_iter()
            .chain(project.native_folder().filter(|_| scoped))
    });
    for folder in folders {
        if let Ok(found) = project_folder(&folder, provider_root.as_deref(), |_| true) {
            skills.extend(found.into_keys());
        }
    }
    skills
}

fn scan_pack(
    pack: &ResolvedPack,
    home: &BTreeMap<String, PathBuf>,
    keep: impl Fn(&str) -> bool,
) -> SkillInventory {
    let root = crate::disk::paths::skills_library_under(&pack.root);
    let mut inventory = SkillInventory::default();
    let paths = match skill_paths(&root, keep, false) {
        Ok(paths) => paths,
        Err(error) => {
            if !pack.excluded.types.contains(&ItemType::Skills) {
                inventory.pack_problems.insert(
                    pack.name.clone(),
                    vec![packs::Problem {
                        path: root,
                        message: error.to_string(),
                    }],
                );
            }
            return inventory;
        }
    };
    let mut problems = Vec::new();
    for (name, path) in paths {
        let fate = if pack.excluded.types.contains(&ItemType::Skills) {
            ItemFate::ExcludedType
        } else if pack.excluded.contains(ItemType::Skills, &name) {
            ItemFate::Excluded
        } else if let Some(path) = home.get(&name) {
            ItemFate::Overridden {
                path: path.join("SKILL.md"),
            }
        } else {
            match read_skill(name.clone(), path.clone(), true) {
                Ok(Some(skill)) => {
                    inventory.skills.insert(name, skill);
                    continue;
                }
                Ok(None) => continue,
                Err(error) => {
                    let message = format!("{error}{}", exclusion_fix(pack.name.as_str(), &name));
                    problems.push(packs::Problem {
                        path: match error {
                            SkillErr::Io { path, .. } => path,
                            _ => path.clone(),
                        },
                        message: message.clone(),
                    });
                    ItemFate::Failed { message }
                }
            }
        };
        inventory.pack_skills.push(PackItem {
            pack: pack.name.clone(),
            kind: ItemType::Skills,
            name,
            path,
            fate,
        });
    }
    if let Some(broken) = inventory
        .pack_skills
        .iter()
        .find(|item| matches!(item.fate, ItemFate::Failed { .. }))
    {
        let message = format!(
            "withheld: skill `{}` in this pack could not be read",
            broken.name
        );
        for (name, skill) in std::mem::take(&mut inventory.skills) {
            inventory.pack_skills.push(PackItem {
                pack: pack.name.clone(),
                kind: ItemType::Skills,
                name,
                path: skill.path,
                fate: ItemFate::Failed {
                    message: message.clone(),
                },
            });
        }
        inventory.pack_problems.insert(pack.name.clone(), problems);
    }
    inventory
}

/// The `; run ...` tail naming the command that excludes one pack skill, the
/// same tail a failed pack definition carries.
fn exclusion_fix(pack: &str, skill: &str) -> String {
    format!(
        "; run `{}`",
        crate::packs::exclude_command(pack, skill, ItemType::Skills)
    )
}

enum LibraryRead {
    Strict,
    SkipUnreadable,
}

fn skill_paths(
    root: &Path,
    keep: impl Fn(&str) -> bool,
    include_symlinks: bool,
) -> Result<BTreeMap<String, PathBuf>, SkillErr> {
    let mut paths = BTreeMap::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(source) => {
            return Err(SkillErr::Io {
                path: root.to_owned(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| SkillErr::Io {
            path: root.to_owned(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !keep(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir()
            || (include_symlinks
                && entry
                    .file_type()
                    .map_err(|source| SkillErr::Io {
                        path: path.clone(),
                        source,
                    })?
                    .is_symlink())
        {
            paths.insert(name, path);
        }
    }
    Ok(paths)
}

fn scan(
    root: &Path,
    chain: Option<&SkillChain>,
    keep: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, SkillDir>, SkillErr> {
    let mut skills = BTreeMap::new();
    for (name, path) in skill_paths(root, keep, false)? {
        if let Some(chain) = chain
            && chain.owned_link(&path)?.is_some()
        {
            continue;
        }
        if let Some(skill) = read_skill(name.clone(), path, false)? {
            skills.insert(name, skill);
        }
    }
    Ok(skills)
}

fn read_skill(
    name: String,
    path: PathBuf,
    require_file: bool,
) -> Result<Option<SkillDir>, SkillErr> {
    let marker = path.join("SKILL.md");
    match marker.metadata() {
        Ok(metadata) if !require_file || metadata.is_file() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SkillErr::Io {
                path: marker,
                source,
            });
        }
    }
    let source = path.canonicalize().map_err(|source| SkillErr::Io {
        path: path.clone(),
        source,
    })?;
    Ok(Some(SkillDir { name, source, path }))
}

/// Whether a launch's skill policy makes skill `name` callable: never when `withheld` names it;
/// under a `capabilities:` `grant`, the skill of a capability of RimZ's MCP server only when the
/// grant names it; any other, a `rimz-*` skill alone included, as `listed` says. Each enforcer
/// applies it to the inventory it enumerates itself.
pub(crate) fn launch_callable(
    listed: Option<&[SkillName]>,
    withheld: &[&str],
    grant: Option<&[crate::config::RimzEntry]>,
    name: &str,
) -> bool {
    if withheld.contains(&name) {
        return false;
    }
    match grant {
        Some(grant) if crate::mcp::catalog::Capability::from_name(name).is_some() => {
            grant.iter().any(|entry| entry.name() == name)
        }
        _ => crate::config::skill_is_callable(listed, name),
    }
}

/// Failure while reading or parsing a skill's `SKILL.md` or `agents/openai.yaml` user-only
/// marker.
#[derive(Debug)]
pub(crate) enum SkillMarkerErr {
    Unreadable { path: PathBuf, message: String },
    Invalid { path: PathBuf, message: String },
}

/// Whether `skill_dir` marks itself user-only for `marker`, reading files through `read_text`
/// so callers with a read cache reuse it. Always reads `SKILL.md` so an unreadable skill file
/// fails even when `marker` is [`crate::agents::ManualSkill::Unsupported`].
pub(crate) fn is_user_only<S, E>(
    skill_dir: &Path,
    marker: crate::agents::ManualSkill,
    mut read_text: impl FnMut(&Path) -> Result<S, E>,
) -> Result<bool, SkillMarkerErr>
where
    S: AsRef<str>,
    E: std::borrow::Borrow<std::io::Error>,
{
    let skill_path = skill_dir.join("SKILL.md");
    let text = read_text(&skill_path).map_err(|error| SkillMarkerErr::Unreadable {
        path: skill_path.clone(),
        message: error.borrow().to_string(),
    })?;
    match marker {
        crate::agents::ManualSkill::Unsupported => Ok(false),
        crate::agents::ManualSkill::Frontmatter => {
            let (block, _) =
                crate::config::definitions::frontmatter::split(&skill_path, text.as_ref())
                    .map_err(|error| SkillMarkerErr::Invalid {
                        path: error.path,
                        message: error.message,
                    })?;
            Ok(block.lines().any(|line| {
                line.strip_prefix("disable-model-invocation:")
                    .is_some_and(|value| value.trim() == "true")
            }))
        }
        crate::agents::ManualSkill::OpenAiPolicy => {
            let policy_path = skill_dir.join("agents/openai.yaml");
            match read_text(&policy_path) {
                Ok(text) => {
                    #[derive(serde::Deserialize)]
                    struct Document {
                        policy: Option<Policy>,
                    }
                    #[derive(serde::Deserialize)]
                    struct Policy {
                        allow_implicit_invocation: Option<bool>,
                    }
                    let document: Option<Document> = serde_saphyr::from_str(text.as_ref())
                        .map_err(|error| SkillMarkerErr::Invalid {
                            path: policy_path,
                            message: error.to_string(),
                        })?;
                    Ok(document
                        .and_then(|document| document.policy)
                        .and_then(|policy| policy.allow_implicit_invocation)
                        == Some(false))
                }
                Err(error) if error.borrow().kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(SkillMarkerErr::Unreadable {
                    path: policy_path,
                    message: error.borrow().to_string(),
                }),
            }
        }
    }
}

/// Whether skill `name` is installed, readable, non-conflicting, and model-callable for
/// `adapter` in `env`, without colliding with an unlisted skill in `listed`.
///
/// The collision check mirrors the two enforcers so an optional seat-added entry is dropped
/// before either enforcer would refuse the launch: `HostSkills::apply`
/// (`LaunchSettingsErr::ConflictingKey` when two directories share one `ProviderSkillKey`
/// with opposing policies) and `crate::sandbox::skills::plan_view`
/// (`SandboxErr::ConflictingSkillAliases` when two directory names resolve to the same
/// canonical `source` with opposing policies).
pub(crate) fn is_model_callable_on_disk(
    name: &str,
    adapter: &crate::agents::AgentDefinition,
    env: &BTreeMap<String, String>,
    listed: &[SkillName],
) -> bool {
    let provider_root = adapter.skills_home(env);
    let chain = SkillChain::from_env(env);
    let Ok(inventory) = enumerate(provider_root.as_deref(), None, chain.as_ref(), |_| true) else {
        return false;
    };
    if inventory.conflicts.contains_key(name) {
        return false;
    }
    let Some(found) = inventory.skills.get(name) else {
        return false;
    };
    if !matches!(
        is_user_only(&found.source, adapter.manual_skill(), |path| {
            std::fs::read_to_string(path)
        }),
        Ok(false)
    ) {
        return false;
    }
    let own_key = match adapter.spec().host_skills {
        HostSkills::Switch { key, .. } => match key(found) {
            Ok(key) => Some(key),
            Err(_) => return false,
        },
        HostSkills::Unsupported => None,
    };
    // Mirrors `HostSkills::apply` (`LaunchSettingsErr::ConflictingKey`) and
    // `crate::sandbox::skills::plan_view` (`SandboxErr::ConflictingSkillAliases`).
    for (other_name, other) in &inventory.skills {
        if other_name == name || listed.iter().any(|skill| skill.as_str() == other_name) {
            continue;
        }
        if other.source == found.source {
            return false;
        }
        if let (Some(own_key), HostSkills::Switch { key, .. }) =
            (&own_key, adapter.spec().host_skills)
            && key(other).ok().as_ref() == Some(own_key)
        {
            return false;
        }
    }
    true
}

impl HostSkills {
    pub(crate) fn apply(
        self,
        (listed, withheld, grant): (
            Option<&[SkillName]>,
            &[&str],
            Option<&[crate::config::RimzEntry]>,
        ),
        root: Option<&Path>,
        project: Option<&ProjectTier>,
        chain: Option<&SkillChain>,
        paths: (&Path, &Path),
        args: &mut Vec<String>,
    ) -> Result<(HostSkillPlan, Option<LaunchSettingsArtifact>), LaunchSettingsErr> {
        // A kind without a switch is unenforced, so it reads the inventory only to refuse a
        // listed name whose copy sits in a tier the kind never reads, and only for those names.
        let switch = matches!(self, Self::Switch { .. });
        let tier_listed = |name: &str| {
            listed.into_iter().flatten().any(|listed| {
                listed.as_str() == name
                    && project.is_some_and(|project| project.holds_unread(listed))
            })
        };
        if !switch
            && !listed
                .into_iter()
                .flatten()
                .any(|name| project.is_some_and(|project| project.holds_unread(name)))
        {
            return Ok((HostSkillPlan::Unenforced, None));
        }
        let native = project.and_then(ProjectTier::native_folder);
        let inventory = if switch {
            enumerate(root, native.as_deref(), chain, |_| true)?
        } else {
            enumerate(root, native.as_deref(), chain, tier_listed)?
        };
        for name in listed.into_iter().flatten() {
            if let Some(found) = inventory.skills.get(name.as_str()) {
                if let Some(refusal) =
                    project.and_then(|project| project.user_only(name, &found.path))
                {
                    return Err(refusal.into());
                }
                continue;
            }
            if let Some(conflict) = inventory.conflicts.get(name.as_str()).filter(|_| switch) {
                return Err(SkillErr::Conflict {
                    message: conflict.message.clone(),
                }
                .into());
            }
            if let Some(refusal) = project.and_then(|project| project.only_unread_copy(name)) {
                return Err(refusal.into());
            }
            // Without a checkout the launch's own compile decides a name no other root holds.
            if !switch || project.is_none() {
                continue;
            }
            let mut roots: Vec<PathBuf> = Vec::new();
            for root in root
                .map(Path::to_owned)
                .into_iter()
                .chain(native.clone())
                .chain(chain.into_iter().flat_map(SkillChain::roots))
            {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            return Err(SkillErr::UnknownSkill {
                name: name.clone(),
                roots,
            }
            .into());
        }
        let Self::Switch {
            flag,
            effect,
            key,
            render,
        } = self
        else {
            return Ok((HostSkillPlan::Unenforced, None));
        };
        let mut callable = Vec::new();
        let mut unlisted = Vec::new();
        for (name, skill) in inventory.skills {
            let provider_key = key(&skill)?;
            let (keys, other_policy) = if launch_callable(listed, withheld, grant, &name) {
                (&mut callable, &unlisted)
            } else {
                (&mut unlisted, &callable)
            };
            if other_policy.contains(&provider_key) {
                return Err(LaunchSettingsErr::ConflictingKey { key: provider_key });
            }
            keys.push(provider_key);
        }
        let artifact = if listed.is_none() && unlisted.is_empty() {
            None
        } else {
            render(&unlisted, paths.0, paths.1, args)?
        };
        Ok((
            HostSkillPlan::Applied {
                flag,
                effect,
                listed: callable,
                unlisted,
            },
            artifact,
        ))
    }
}

#[cfg(test)]
mod tests;
