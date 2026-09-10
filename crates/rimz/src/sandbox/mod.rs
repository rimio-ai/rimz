//! Linux agent mount views: room-owned tmp and profile-selected directory overlays.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::ManualSkill;
use crate::config::{Isolation, MachineConfig, SkillName};
use crate::disk::paths::StatePaths;
use crate::ids::AgentKind;

#[cfg(target_os = "linux")]
mod linux;
mod rewrite;
mod skills;

const SANDBOX_TMP: &str = "/tmp";

/// Where an agent sees room tmp: `/tmp` in a sandbox, the host path otherwise.
pub struct TmpView {
    tmp_dir: PathBuf,
    sandboxed: bool,
}

impl TmpView {
    fn new(isolation: Isolation, paths: &StatePaths) -> Self {
        Self {
            tmp_dir: paths.tmp_dir.clone(),
            sandboxed: isolation == Isolation::Sandbox,
        }
    }

    pub fn current(paths: &StatePaths) -> Self {
        Self::new(MachineConfig::load_lenient().agents.isolation, paths)
    }

    pub fn agent_path(&self, host: &Path) -> PathBuf {
        if self.sandboxed
            && let Ok(relative) = host.strip_prefix(&self.tmp_dir)
        {
            return Path::new(SANDBOX_TMP).join(relative);
        }
        host.to_path_buf()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxErr {
    #[error("sandbox isolation requires Linux; set agents.isolation = \"host\"")]
    UnsupportedOs,
    #[error(
        "sandbox isolation needs bwrap on PATH; install bubblewrap or set agents.isolation = \"host\""
    )]
    MissingBwrap,
    #[error(
        "bubblewrap probe failed ({status}): {stderr}; check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy, or set agents.isolation = \"host\""
    )]
    ProbeFailed { status: String, stderr: String },
    #[error("sandbox cannot access {path}: {source}; fix the path before launching")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Path(#[from] crate::disk::paths::PathErr),
    #[error("sandbox requires an absolute UTF-8 path, got {0:?}; use an absolute UTF-8 path")]
    InvalidPath(PathBuf),
    #[error(
        "sandbox tmp would hide required path /tmp; move the working directory or configured root below /tmp, or set agents.isolation = \"host\""
    )]
    TmpCollision,
    #[error(
        "unknown skill {name:?}; searched {roots:?}; install the skill or remove it from the profile"
    )]
    UnknownSkill { name: String, roots: Vec<PathBuf> },
    #[error("provider {kind} declares no skill root; remove the profile skills list")]
    SkillsNeedRoot { kind: String },
    #[error("provider {kind} cannot mark skills user-only; remove the profile skills list")]
    ManualSkillsUnsupported { kind: String },
    #[error(
        "cannot rewrite skill metadata {path}: {reason}; use block mappings for skill metadata or remove the profile skills list"
    )]
    SkillMetadata { path: PathBuf, reason: &'static str },
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
    pub tmp_dir: &'a Path,
    pub skills_dir: &'a Path,
    pub provider_home: Option<ProviderHome>,
    pub provider_home_env_keys: &'a [&'a str],
    pub skills: SkillInputs<'a>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvPin {
    Set(String),
    Unset,
}

pub struct Prepared {
    pub plan: MountPlan,
    pub pins: BTreeMap<String, EnvPin>,
}

pub struct ProviderHome {
    pub source: PathBuf,
    pub target: PathBuf,
}

struct DirView {
    root: PathBuf,
    entries: Vec<DirEntry>,
}

struct DirEntry {
    name: String,
    source: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
enum Mount {
    Bind { source: PathBuf, target: PathBuf },
    RoBind { source: PathBuf, target: PathBuf },
    Tmpfs { target: PathBuf },
}

pub struct MountPlan {
    mounts: Vec<Mount>,
}

#[derive(Debug, serde::Serialize)]
pub struct SandboxDiagnostic {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub fn preflight_skills(
    isolation: Isolation,
    kind: &AgentKind,
    configured: bool,
    manual: ManualSkill,
) -> Result<(), SandboxErr> {
    if isolation == Isolation::Host || !configured {
        return Ok(());
    }
    if manual == ManualSkill::Unsupported {
        return Err(SandboxErr::ManualSkillsUnsupported {
            kind: kind.to_string(),
        });
    }
    Ok(())
}

pub fn preflight(isolation: Isolation) -> Result<Option<PathBuf>, SandboxErr> {
    if isolation == Isolation::Host {
        return Ok(None);
    }
    #[cfg(target_os = "linux")]
    return linux::probe().map(Some);
    #[cfg(not(target_os = "linux"))]
    Err(SandboxErr::UnsupportedOs)
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

pub fn prepare(inputs: &SandboxInputs<'_>) -> Result<Prepared, SandboxErr> {
    let views = skills::prepare(inputs.env, inputs.skills_dir, &inputs.skills)?;
    let mut mounts = Vec::new();
    let mut required = crate::mux::domain::ProcessDomain::required_paths(inputs.env);
    let mut pins = BTreeMap::new();
    let root_keys = [
        "HOME",
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
    pins.insert("TMPDIR".to_owned(), EnvPin::Set("/tmp".to_owned()));
    for key in root_keys {
        if let Some(value) = inputs.env.get(key).filter(|value| !value.is_empty()) {
            required.push(PathBuf::from(value));
        }
    }
    required.extend([inputs.cwd.to_path_buf(), inputs.project_root.to_path_buf()]);
    required.extend(inputs.worktree.map(Path::to_path_buf));
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
    mounts.push(Mount::Bind {
        source: inputs.tmp_dir.to_path_buf(),
        target: PathBuf::from(SANDBOX_TMP),
    });
    let mut reach = BTreeSet::new();
    for path in required {
        validate_path(&path)?;
        let path = crate::utils::path::normalize_path_lexical(&path);
        if path == Path::new("/tmp") {
            return Err(SandboxErr::TmpCollision);
        }
        if path.starts_with("/tmp") && path.exists() {
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
    if let Some(view) = views {
        mounts.push(Mount::Tmpfs {
            target: view.root.clone(),
        });
        for entry in view.entries {
            mounts.push(Mount::RoBind {
                source: entry.source,
                target: view.root.join(entry.name),
            });
        }
    }
    crate::disk::paths::ensure_private_runtime_dir(inputs.tmp_dir)?;
    Ok(Prepared {
        plan: MountPlan { mounts },
        pins,
    })
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
    fn tmp_view_maps_only_sandbox_room_paths() {
        let paths = StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(Path::new("/project")),
            Path::new("/state"),
        )
        .unwrap();
        let output = paths.wakes_dir.join("wake-test.output");
        let sandbox = TmpView::new(Isolation::Sandbox, &paths);
        assert_eq!(
            sandbox.agent_path(&output),
            Path::new("/tmp/rimz-wakes/wake-test.output")
        );
        assert_eq!(
            sandbox.agent_path(Path::new("/elsewhere/file")),
            Path::new("/elsewhere/file")
        );
        assert_eq!(
            TmpView::new(Isolation::Host, &paths).agent_path(&output),
            output
        );
    }

    #[test]
    fn sandbox_preflight_errors_end_with_fix() {
        insta::assert_debug_snapshot!([
            SandboxErr::UnsupportedOs.to_string(),
            SandboxErr::MissingBwrap.to_string(),
            SandboxErr::ProbeFailed { status: "exit status: 1".into(), stderr: "namespace denied".into() }.to_string(),
        ], @r###"
        [
            "sandbox isolation requires Linux; set agents.isolation = \"host\"",
            "sandbox isolation needs bwrap on PATH; install bubblewrap or set agents.isolation = \"host\"",
            "bubblewrap probe failed (exit status: 1): namespace denied; check kernel.unprivileged_userns_clone, user.max_user_namespaces and AppArmor/LSM policy, or set agents.isolation = \"host\"",
        ]
        "###);
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
                    source: "/state/tmp".into(),
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
            "/state/tmp",
            "/tmp",
            "--bind",
            "/tmp/runtime",
            "/tmp/runtime",
            "--tmpfs",
            "/home/user/.agents/skills",
            "--ro-bind",
            "/skills/one",
            "/home/user/.agents/skills/one",
            "--chdir",
            "/project",
            "--",
            "agent",
            "two words",
        ]
        "###);
    }
}
