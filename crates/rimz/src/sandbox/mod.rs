//! Agent isolation admission and Linux mount views: temp units and profile-selected overlays.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::ManualSkill;
use crate::config::{Isolation, SkillName};
use crate::disk::paths::StatePaths;
use crate::ids::AgentKind;

#[cfg(target_os = "linux")]
mod linux;
mod rewrite;
mod skills;

pub use rewrite::PlannedCopy;

const SANDBOX_TMP: &str = "/tmp";
/// Where a scoped agent sees its temp unit: both host temp roots.
const TMP_MOUNTS: [&str; 2] = [SANDBOX_TMP, "/var/tmp"];

/// Where an agent sees its temp unit: at both temp roots in a scoped view, at its
/// host path otherwise.
pub struct TmpView {
    unit_dir: PathBuf,
    sandboxed: bool,
}

impl TmpView {
    pub fn current(isolation: Isolation, owner: Option<&str>, paths: &StatePaths) -> Self {
        Self {
            unit_dir: paths.temp_unit_dir(owner),
            sandboxed: isolation.is_scoped(),
        }
    }

    /// The host path behind a path the agent names.
    pub fn host_path(&self, agent: &Path) -> PathBuf {
        let agent = crate::utils::path::normalize_path_lexical(agent);
        if !self.sandboxed {
            return agent;
        }
        TMP_MOUNTS
            .iter()
            .find_map(|mount| agent.strip_prefix(mount).ok())
            .map_or_else(
                || agent.clone(),
                |rest| self.unit_dir.join(rest).components().collect(),
            )
    }

    /// The path the agent sees for a host path.
    pub(crate) fn agent_path(&self, host: &Path) -> PathBuf {
        match host.strip_prefix(&self.unit_dir) {
            Ok(rest) if self.sandboxed => Path::new(SANDBOX_TMP).join(rest).components().collect(),
            _ => host.to_path_buf(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxErr {
    #[error(transparent)]
    NativeSandbox(#[from] crate::blocker::Blocker),
    #[error("scoped isolation requires Linux; set agents.isolation = \"host\"")]
    UnsupportedOs,
    #[error(
        "scoped isolation needs bwrap on PATH; install bubblewrap or set agents.isolation = \"host\""
    )]
    MissingBwrap,
    #[error(
        "bubblewrap probe failed ({status}): {stderr}; check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy, or set agents.isolation = \"host\""
    )]
    ProbeFailed { status: String, stderr: String },
    #[error("scoped view cannot access {path}: {source}; fix the path before launching")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Path(#[from] crate::disk::paths::PathErr),
    #[error("scoped view requires an absolute UTF-8 path, got {0:?}; use an absolute UTF-8 path")]
    InvalidPath(PathBuf),
    #[error(
        "scoped tmp would hide required path {}; move the working directory or configured root below {}, or set agents.isolation = \"host\"",
        .path.display(),
        .path.display()
    )]
    TmpCollision { path: PathBuf },
    #[error(
        "unknown skill {name:?}; searched {roots:?}; install the skill or remove it from the profile"
    )]
    UnknownSkill { name: String, roots: Vec<PathBuf> },
    #[error(
        "skills {listed:?} and {unlisted:?} resolve to {path} but have conflicting invocation policies; list both names or neither in the profile skills list"
    )]
    ConflictingSkillAliases {
        listed: String,
        unlisted: String,
        path: PathBuf,
    },
    #[error("provider {kind} declares no skill root; remove the profile skills list")]
    SkillsNeedRoot { kind: String },
    #[error("provider {kind} cannot mark skills user-only; remove the profile skills list")]
    ManualSkillsUnsupported { kind: String },
    #[error("invalid profile skills: {0}")]
    Skills(#[from] crate::config::SkillListErr),
}

pub struct SkillInputs<'a> {
    pub kind: &'a str,
    pub home: Option<PathBuf>,
    pub manual: ManualSkill,
    pub callable: Option<&'a [SkillName]>,
}

pub struct SandboxInputs<'a> {
    pub env: &'a BTreeMap<String, String>,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub worktree: Option<&'a Path>,
    pub read_only: Vec<PathBuf>,
    pub tmp_dir: &'a Path,
    pub skills_dir: &'a Path,
    pub provider_home: Option<ProviderHome>,
    pub provider_home_env_keys: &'a [&'a str],
    /// The provider's own home that a named account's settings, and a shared
    /// account's history, link into.
    pub default_home: Option<PathBuf>,
    pub skills: SkillInputs<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum EnvPin {
    Set(String),
    Unset,
}

pub struct SandboxPlan {
    pub plan: MountPlan,
    pub pins: BTreeMap<String, EnvPin>,
    pub skipped: Vec<SkippedSkill>,
    pub copies: Vec<PlannedCopy>,
    tmp_dir: PathBuf,
}

#[derive(Debug, serde::Serialize)]
pub struct SkippedSkill {
    pub name: String,
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, serde::Serialize)]
pub enum SkipReason {
    Unreadable(#[serde(serialize_with = "serialize_io_error")] std::io::Error),
    Metadata(&'static str),
}

fn serialize_io_error<S: serde::Serializer>(
    error: &std::io::Error,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(error)
}

impl std::fmt::Display for SkippedSkill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "starting without skill {:?}: RimZ cannot ", self.name)?;
        match &self.reason {
            SkipReason::Unreadable(error) => {
                write!(f, "read it ({}: {error})", self.path.display())?;
            }
            SkipReason::Metadata(reason) => {
                write!(
                    f,
                    "yet rewrite its metadata ({}: {reason})",
                    self.path.display()
                )?;
            }
        }
        write!(
            f,
            "; the installed skill is untouched and unaffected skills stay available"
        )
    }
}

pub struct ProviderHome {
    pub source: PathBuf,
    pub target: PathBuf,
}

struct DirView {
    root: PathBuf,
    entries: Vec<DirEntry>,
    shadows: BTreeMap<PathBuf, PathBuf>,
}

struct DirEntry {
    name: String,
    source: PathBuf,
    kind: DirEntryKind,
}

enum DirEntryKind {
    Bind,
    Symlink { target: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Mount {
    Bind { source: PathBuf, target: PathBuf },
    RoBind { source: PathBuf, target: PathBuf },
    Tmpfs { target: PathBuf },
    Symlink { target: PathBuf, path: PathBuf },
    RemountRo { target: PathBuf },
}

pub struct MountPlan {
    pub mounts: Vec<Mount>,
}

pub const HOOK_HOST_PATHS_ENV: &str = "RIMZ_SANDBOX_HOST_PATHS";

impl MountPlan {
    pub fn host_bound_paths(&self) -> Vec<PathBuf> {
        self.mounts
            .iter()
            .filter_map(|mount| match mount {
                Mount::Bind { source, target } | Mount::RoBind { source, target }
                    if source == target =>
                {
                    Some(target.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SandboxDiagnostic {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub fn preflight_launch(
    isolation: Isolation,
    kind: &AgentKind,
    skills_configured: bool,
    manual: ManualSkill,
) -> Result<Option<PathBuf>, SandboxErr> {
    if isolation == Isolation::Sandbox {
        use crate::blocker::{Blocker, Surface};
        let refusal = |problem: String| {
            Blocker::new(problem)
                .with_fix(
                    "Launch with --isolation scoped",
                    Some("rimz config set agents.isolation scoped".into()),
                    Surface::Security,
                )
                .with_fix(
                    "Launch with --isolation host",
                    Some("rimz config set agents.isolation host".into()),
                    Surface::Security,
                )
        };
        let prerequisites = crate::agents::find_definition(kind.as_str())
            .and_then(|adapter| adapter.native_sandbox_prerequisites())
            .ok_or_else(|| refusal(format!("sandbox isolation is unsupported for {kind}; supported providers are Claude Code and Codex")))?;
        #[cfg(target_os = "linux")]
        {
            for binary in prerequisites {
                if which::which(binary).is_err() {
                    return Err(refusal(format!(
                        "{kind} sandbox isolation needs {binary} on PATH"
                    ))
                    .with_fix(
                        format!(
                            "Install {}",
                            if *binary == "bwrap" {
                                "bubblewrap (bwrap)"
                            } else {
                                binary
                            }
                        ),
                        None,
                        Surface::Plain,
                    )
                    .into());
                }
            }
            if prerequisites.contains(&"bwrap") {
                linux::probe().map_err(|error| refusal(error.to_string()).with_fix(
                    "Check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy",
                    None, Surface::Plain,
                ))?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = prerequisites;
        return Ok(None);
    }
    if isolation.is_scoped() && skills_configured && manual == ManualSkill::Unsupported {
        return Err(SandboxErr::ManualSkillsUnsupported {
            kind: kind.to_string(),
        });
    }
    preflight(isolation)
}

pub fn preflight(isolation: Isolation) -> Result<Option<PathBuf>, SandboxErr> {
    if !isolation.is_scoped() {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    return linux::probe().map(Some);
    #[cfg(not(target_os = "linux"))]
    Err(SandboxErr::UnsupportedOs)
}

impl From<SandboxErr> for crate::blocker::Blocker {
    fn from(error: SandboxErr) -> Self {
        use crate::blocker::Surface;
        let blocker = match error {
            SandboxErr::NativeSandbox(blocker) => return blocker,
            SandboxErr::UnsupportedOs => Self::new("scoped isolation requires Linux"),
            SandboxErr::MissingBwrap => Self::new("scoped isolation needs bwrap on PATH")
                .with_fix("Install bubblewrap", None, Surface::Plain),
            SandboxErr::ProbeFailed { status, stderr } => {
                Self::new(format!("bubblewrap probe failed ({status}): {stderr}")).with_fix(
                    "Check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy",
                    None,
                    Surface::Plain,
                )
            }
            other => return Self::new(other.to_string()),
        };
        blocker.with_fix(
            "Use host isolation",
            Some("rimz config set agents.isolation host".into()),
            Surface::Security,
        )
    }
}

pub fn diagnose() -> SandboxDiagnostic {
    #[cfg(target_os = "linux")]
    return linux::diagnose();
    #[cfg(not(target_os = "linux"))]
    SandboxDiagnostic {
        path: None,
        version: None,
        error: Some(SandboxErr::UnsupportedOs.to_string()),
    }
}

pub fn apply(plan: &SandboxPlan) -> Result<(), SandboxErr> {
    for copy in &plan.copies {
        rewrite::apply(copy)?;
    }
    crate::disk::paths::ensure_private_runtime_dir(&plan.tmp_dir)?;
    Ok(())
}

pub fn plan(inputs: &SandboxInputs<'_>) -> Result<SandboxPlan, SandboxErr> {
    let views = skills::plan(inputs.env, inputs.skills_dir, &inputs.skills)?;
    let mut mounts = Vec::new();
    let mut required = crate::mux::domain::ProcessDomain::required_paths(inputs.env);
    let mut pins = BTreeMap::new();
    let root_keys = [
        "HOME",
        "RIMZ_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "RIMZ_AGENTS_HOME",
    ];
    let mut keys: BTreeSet<&str> = root_keys.into_iter().collect();
    keys.extend(["TMUX", "ZELLIJ_SOCKET_DIR"]);
    keys.extend(inputs.provider_home_env_keys.iter().copied());
    for key in keys {
        pins.insert(
            key.to_owned(),
            inputs
                .env
                .get(key)
                .cloned()
                .map_or(EnvPin::Unset, EnvPin::Set),
        );
    }
    pins.insert("TMPDIR".to_owned(), EnvPin::Set(SANDBOX_TMP.to_owned()));
    // A host TMPDIR may not exist in the view; mux children here keep /tmp.
    for key in [
        crate::child_process::USER_TMPDIR_ENV,
        crate::child_process::TEMP_ROOT_KEYS_ENV,
    ] {
        pins.insert(key.to_owned(), EnvPin::Unset);
    }
    // A shared sccache server runs rustc in the view that spawned it; client-side
    // mode (sccache >= 0.17) compiles in this view and uses the server for storage.
    pins.insert(
        "SCCACHE_CLIENT_SIDE".to_owned(),
        EnvPin::Set("1".to_owned()),
    );
    for key in root_keys {
        if let Some(value) = inputs.env.get(key).filter(|value| !value.is_empty()) {
            required.push(PathBuf::from(value));
        }
    }
    required.extend([inputs.cwd.to_path_buf(), inputs.project_root.to_path_buf()]);
    required.extend(inputs.worktree.map(Path::to_path_buf));
    required.extend(inputs.default_home.clone());
    if let Some(home) = &inputs.provider_home
        && home.source.exists()
    {
        validate_path(&home.source)?;
        validate_path(&home.target)?;
        mounts.push(Mount::Bind {
            source: home.source.clone(),
            target: home.target.clone(),
        });
        required.push(home.source.clone());
    }
    validate_path(inputs.tmp_dir)?;
    // bwrap resolves each bind source against the host root, so the second
    // mount sees the unit and not the first mount.
    for mount in TMP_MOUNTS {
        mounts.push(Mount::Bind {
            source: inputs.tmp_dir.to_path_buf(),
            target: PathBuf::from(mount),
        });
    }
    let mut reach = BTreeSet::new();
    for path in required {
        validate_path(&path)?;
        let path = crate::utils::path::normalize_path_lexical(&path);
        if TMP_MOUNTS.iter().any(|mount| path == Path::new(mount)) {
            return Err(SandboxErr::TmpCollision { path });
        }
        if TMP_MOUNTS.iter().any(|mount| path.starts_with(mount)) && path.exists() {
            reach.insert(path);
        }
    }
    let mut bound: Vec<PathBuf> = Vec::new();
    for path in reach {
        if bound.iter().any(|parent| path.starts_with(parent)) {
            continue;
        }
        mounts.push(Mount::Bind {
            source: path.clone(),
            target: path.clone(),
        });
        bound.push(path);
    }
    for path in &inputs.read_only {
        validate_path(path)?;
        mounts.push(Mount::RoBind {
            source: path.clone(),
            target: path.clone(),
        });
    }
    let mut read_only_overlays = BTreeSet::new();
    for path in protected_paths(inputs)
        .into_iter()
        .filter(|path| path.exists())
    {
        validate_path(&path)?;
        if let Some(view) = views.dir.as_ref().filter(|view| {
            view.root
                .starts_with(path.canonicalize().unwrap_or_else(|_| path.clone()))
        }) {
            // Build the skill overlay before making its parent read-only; a late
            // parent bind would hide the overlay and restore native discovery.
            mounts.push(Mount::Bind {
                source: path.clone(),
                target: path.clone(),
            });
            read_only_overlays.extend([path, view.root.clone()]);
        } else {
            mounts.push(Mount::RoBind {
                source: path.clone(),
                target: path,
            });
        }
    }
    if let Some(view) = views.dir {
        mounts.push(Mount::Tmpfs {
            target: view.root.clone(),
        });
        for entry in view.entries {
            let path = view.root.join(entry.name);
            mounts.push(match entry.kind {
                DirEntryKind::Bind => Mount::RoBind {
                    source: entry.source,
                    target: path,
                },
                DirEntryKind::Symlink { target } => Mount::Symlink { target, path },
            });
        }
        for (target, source) in view.shadows {
            mounts.push(Mount::RoBind { source, target });
        }
    }
    mounts.extend(
        read_only_overlays
            .into_iter()
            .map(|target| Mount::RemountRo { target }),
    );
    let plan = MountPlan { mounts };
    pins.insert(
        HOOK_HOST_PATHS_ENV.into(),
        EnvPin::Set(
            // validate_path checks identity-bind sources; rewrite::plan uses a validated skills_dir for copy paths.
            serde_json::to_string(&plan.host_bound_paths())
                .expect("identity bind targets were validated as absolute UTF-8 paths"),
        ),
    );
    Ok(SandboxPlan {
        plan,
        pins,
        skipped: views.skipped,
        copies: views.copies,
        tmp_dir: inputs.tmp_dir.to_path_buf(),
    })
}

fn protected_paths(inputs: &SandboxInputs<'_>) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    if let Some(home) = inputs.env.get("HOME").filter(|home| !home.is_empty()) {
        paths.extend(
            [
                ".profile",
                ".bashrc",
                ".bash_profile",
                ".bash_login",
                ".bash_logout",
                ".zshenv",
                ".zprofile",
                ".zshrc",
                ".zlogin",
                ".zlogout",
                ".config/fish",
                ".gitconfig",
                ".ssh",
                ".config/systemd/user",
            ]
            .map(|entry| Path::new(home).join(entry)),
        );
    }
    let checkout = inputs
        .cwd
        .ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(inputs.cwd);
    let git = crate::worktree::git_admin_dir_from_checkout_metadata(checkout)
        .ok()
        .flatten()
        .unwrap_or_else(|| checkout.join(".git"));
    let git = std::fs::read_to_string(git.join("commondir")).map_or_else(
        |_| git.clone(),
        |common| crate::utils::path::normalize_path_lexical(&git.join(common.trim())),
    );
    paths.extend([git.join("hooks"), git.join("config")]);
    for root in [checkout, inputs.project_root] {
        paths.extend([".claude", ".codex", ".agents", ".mcp.json"].map(|entry| root.join(entry)));
    }
    paths
}

fn validate_path(path: &Path) -> Result<(), SandboxErr> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(SandboxErr::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

pub fn bwrap_argv(bwrap: &Path, plan: &MountPlan, cwd: &Path, inner: &[String]) -> Vec<String> {
    let mut argv = vec![bwrap.display().to_string()];
    argv.extend(
        [
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--die-with-parent",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    for mount in &plan.mounts {
        match mount {
            Mount::Bind { source, target } | Mount::RoBind { source, target } => {
                argv.push(
                    if matches!(mount, Mount::Bind { .. }) {
                        "--bind"
                    } else {
                        "--ro-bind"
                    }
                    .to_owned(),
                );
                argv.push(source.display().to_string());
                argv.push(target.display().to_string());
            }
            Mount::Tmpfs { target } => {
                argv.push("--tmpfs".to_owned());
                argv.push(target.display().to_string());
            }
            Mount::Symlink { target, path } => {
                argv.push("--symlink".to_owned());
                argv.push(target.display().to_string());
                argv.push(path.display().to_string());
            }
            Mount::RemountRo { target } => {
                argv.push("--remount-ro".to_owned());
                argv.push(target.display().to_string());
            }
        }
    }
    argv.extend([
        "--chdir".to_owned(),
        cwd.display().to_string(),
        "--".to_owned(),
    ]);
    argv.extend_from_slice(inner);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_preflight_host_skips_unsupported_skills() {
        assert_eq!(
            preflight_launch(
                Isolation::Host,
                &AgentKind::new_unchecked("codex"),
                true,
                ManualSkill::Unsupported,
            )
            .map_err(|err| err.to_string()),
            Ok(None)
        );
    }

    #[test]
    fn launch_preflight_rejects_unsupported_skills_before_probing() {
        assert!(matches!(
            preflight_launch(
                Isolation::Scoped,
                &AgentKind::new_unchecked("codex"),
                true,
                ManualSkill::Unsupported,
            ),
            Err(SandboxErr::ManualSkillsUnsupported { kind }) if kind == "codex"
        ));
    }

    #[test]
    fn launch_preflight_without_skills_preserves_the_probe_result() {
        for isolation in [Isolation::Host, Isolation::Scoped, Isolation::Sandbox] {
            assert_eq!(
                preflight_launch(
                    isolation,
                    &AgentKind::new_unchecked("codex"),
                    false,
                    ManualSkill::Unsupported,
                )
                .map_err(|err| err.to_string()),
                preflight(isolation).map_err(|err| err.to_string())
            );
        }
    }

    #[test]
    fn skipped_skill_explains_the_omission() {
        for (reason, explanation) in [
            (
                SkipReason::Metadata("YAML anchors, aliases, and tags are not supported"),
                "yet rewrite its metadata (/skills/demo/SKILL.md: YAML anchors, aliases, and tags are not supported)",
            ),
            (
                SkipReason::Unreadable(std::io::Error::other("permission denied")),
                "read it (/skills/demo/SKILL.md: permission denied)",
            ),
        ] {
            let skipped = SkippedSkill {
                name: "demo".to_owned(),
                path: "/skills/demo/SKILL.md".into(),
                reason,
            };
            assert_eq!(
                skipped.to_string(),
                format!(
                    "starting without skill \"demo\": RimZ cannot {explanation}; the installed skill is untouched and unaffected skills stay available"
                )
            );
        }
    }

    fn state() -> StatePaths {
        StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(Path::new("/project")),
            Path::new("/state"),
        )
        .unwrap()
    }

    fn unit_plan(unit: &Path, cwd: &Path) -> Result<SandboxPlan, SandboxErr> {
        plan(&SandboxInputs {
            env: &BTreeMap::new(),
            cwd,
            project_root: Path::new("/project"),
            worktree: None,
            read_only: Vec::new(),
            tmp_dir: unit,
            skills_dir: &state().agent_skills_dir(Some("otter")),
            provider_home: None,
            provider_home_env_keys: &[],
            default_home: None,
            skills: SkillInputs {
                kind: "claude",
                home: None,
                manual: ManualSkill::Frontmatter,
                callable: None,
            },
        })
    }

    #[test]
    fn read_only_checkout_follows_temp_reach_rebinds() {
        let checkout = tempfile::tempdir().unwrap();
        let plan = plan(&SandboxInputs {
            env: &BTreeMap::new(),
            cwd: checkout.path(),
            project_root: checkout.path(),
            worktree: None,
            read_only: vec![checkout.path().to_path_buf()],
            tmp_dir: Path::new("/state/tmp/unit"),
            skills_dir: Path::new("/state/skills"),
            provider_home: None,
            provider_home_env_keys: &[],
            default_home: None,
            skills: SkillInputs {
                kind: "claude",
                home: None,
                manual: ManualSkill::Frontmatter,
                callable: None,
            },
        })
        .unwrap();
        let mounts = &plan.plan.mounts;
        let reach = mounts
            .iter()
            .position(
                |mount| matches!(mount, Mount::Bind {target, ..} if target == checkout.path()),
            )
            .unwrap();
        let readonly = mounts.iter().position(
            |mount| matches!(mount, Mount::RoBind {target, ..} if target == checkout.path()),
        );
        assert!(
            readonly.is_some(),
            "the checkout must have a read-only overlay"
        );
        assert!(reach < readonly.unwrap());
        let argv = bwrap_argv(
            Path::new("/bin/bwrap"),
            &plan.plan,
            checkout.path(),
            &["true".into()],
        );
        assert!(argv.windows(3).any(|args| args == ["--bind", "/", "/"]));
    }

    #[test]
    fn the_temp_unit_binds_at_tmp_and_var_tmp() {
        let unit = state().temp_unit_dir(Some("otter"));
        let plan = unit_plan(&unit, Path::new("/project")).unwrap();
        let tmp = plan
            .plan
            .mounts
            .iter()
            .position(|mount| {
                mount
                    == &Mount::Bind {
                        source: unit.clone(),
                        target: "/tmp".into(),
                    }
            })
            .expect("unit at /tmp");
        assert_eq!(
            plan.plan.mounts.get(tmp + 1),
            Some(&Mount::Bind {
                source: unit.clone(),
                target: "/var/tmp".into(),
            })
        );
        assert_eq!(
            plan.pins.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "HOME",
                "RIMZ_AGENTS_HOME",
                "RIMZ_HOME",
                "RIMZ_SANDBOX_HOST_PATHS",
                "RIMZ_TEMP_ROOT_KEYS",
                "RIMZ_USER_TMPDIR",
                "SCCACHE_CLIENT_SIDE",
                "TMPDIR",
                "TMUX",
                "XDG_CACHE_HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_RUNTIME_DIR",
                "XDG_STATE_HOME",
                "ZELLIJ_SOCKET_DIR",
            ]
        );
        assert_eq!(plan.pins["TMPDIR"], EnvPin::Set("/tmp".to_owned()));
        assert_eq!(plan.pins["RIMZ_USER_TMPDIR"], EnvPin::Unset);
        assert_eq!(plan.pins["RIMZ_TEMP_ROOT_KEYS"], EnvPin::Unset);
    }

    #[test]
    fn a_required_path_at_either_mount_point_collides() {
        let unit = state().temp_unit_dir(Some("otter"));
        for mount in ["/tmp", "/var/tmp", "/var/tmp/"] {
            let error = unit_plan(&unit, Path::new(mount)).err().expect(mount);
            let expected = Path::new(mount).components().collect::<PathBuf>();
            assert!(
                matches!(&error, SandboxErr::TmpCollision { path } if *path == expected),
                "{error}"
            );
            assert!(
                error
                    .to_string()
                    .contains(&format!("required path {}", expected.display()))
            );
        }
    }

    #[test]
    fn tmp_view_maps_the_owner_unit_only() {
        let paths = state();
        let unit = paths.temp_unit_dir(Some("otter"));
        let other = paths.temp_unit_dir(Some("fox")).join("f");
        let sandbox = TmpView::current(Isolation::Scoped, Some("otter"), &paths);
        for (host, agent) in [
            (unit.join("f"), "/tmp/f"),
            (unit.clone(), "/tmp"),
            (other.clone(), other.to_str().unwrap()),
            (PathBuf::from("/elsewhere/file"), "/elsewhere/file"),
        ] {
            assert_eq!(sandbox.agent_path(&host), Path::new(agent));
            assert_eq!(sandbox.host_path(Path::new(agent)), host);
        }
        assert_eq!(sandbox.host_path(Path::new("/var/tmp/x")), unit.join("x"));
        assert_eq!(sandbox.host_path(Path::new("/var/tmp")), unit);
        assert_eq!(
            sandbox.host_path(Path::new("/tmp/../home/x")),
            Path::new("/home/x")
        );
        assert_eq!(
            TmpView::current(Isolation::Scoped, None, &paths)
                .agent_path(&paths.temp_unit_dir(None).join("f")),
            Path::new("/tmp/f")
        );
        let host = TmpView::current(Isolation::Host, Some("otter"), &paths);
        assert_eq!(host.agent_path(&unit.join("f")), unit.join("f"));
        for path in ["/tmp/f", "/var/tmp/f"] {
            assert_eq!(host.host_path(Path::new(path)), Path::new(path));
        }
    }

    #[test]
    fn sandbox_preflight_errors_end_with_fix() {
        insta::assert_debug_snapshot!([
            SandboxErr::UnsupportedOs.to_string(),
            SandboxErr::MissingBwrap.to_string(),
            SandboxErr::ProbeFailed { status: "exit status: 1".into(), stderr: "namespace denied".into() }.to_string(),
        ], @r###"
        [
            "scoped isolation requires Linux; set agents.isolation = \"host\"",
            "scoped isolation needs bwrap on PATH; install bubblewrap or set agents.isolation = \"host\"",
            "bubblewrap probe failed (exit status: 1): namespace denied; check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy, or set agents.isolation = \"host\"",
        ]
        "###);
    }

    #[test]
    fn start_blockers_keep_host_isolation_a_user_run_security_fix() {
        for error in [
            SandboxErr::UnsupportedOs,
            SandboxErr::MissingBwrap,
            SandboxErr::ProbeFailed {
                status: "exit status: 1".into(),
                stderr: "namespace denied".into(),
            },
        ] {
            let blocker = crate::blocker::Blocker::from(error);
            assert!(
                blocker.fixes.iter().any(|fix| {
                    fix.command.as_deref() == Some("rimz config set agents.isolation host")
                        && fix.surface == crate::blocker::Surface::Security
                }),
                "{blocker:?}"
            );
        }
    }

    #[test]
    fn bwrap_argv_renders_mount_view_in_order() {
        let plan = MountPlan {
            mounts: vec![
                Mount::Bind {
                    source: "/home/user/.claude".into(),
                    target: "/home/user/.claude".into(),
                },
                Mount::Bind {
                    source: "/state/tmp/otter".into(),
                    target: "/tmp".into(),
                },
                Mount::Bind {
                    source: "/tmp/runtime".into(),
                    target: "/tmp/runtime".into(),
                },
                Mount::Tmpfs {
                    target: "/home/user/.agents/skills".into(),
                },
                Mount::RoBind {
                    source: "/skills/one".into(),
                    target: "/home/user/.agents/skills/one".into(),
                },
                Mount::Symlink {
                    target: "../../.agents/skills/one".into(),
                    path: "/home/user/.claude/skills/one".into(),
                },
            ],
        };
        insta::assert_debug_snapshot!(bwrap_argv(Path::new("/usr/bin/bwrap"), &plan, Path::new("/project"), &["agent".into(), "two words".into()]), @r###"
        [
            "/usr/bin/bwrap",
            "--bind",
            "/",
            "/",
            "--dev-bind",
            "/dev",
            "/dev",
            "--die-with-parent",
            "--bind",
            "/home/user/.claude",
            "/home/user/.claude",
            "--bind",
            "/state/tmp/otter",
            "/tmp",
            "--bind",
            "/tmp/runtime",
            "/tmp/runtime",
            "--tmpfs",
            "/home/user/.agents/skills",
            "--ro-bind",
            "/skills/one",
            "/home/user/.agents/skills/one",
            "--symlink",
            "../../.agents/skills/one",
            "/home/user/.claude/skills/one",
            "--chdir",
            "/project",
            "--",
            "agent",
            "two words",
        ]
        "###);
    }
}
