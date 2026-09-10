//! Linux agent mount views: room-owned scratch and profile-selected directory overlays.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{Isolation, SkillSpec};

#[cfg(target_os = "linux")]
mod linux;
mod skills;

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
        "sandbox scratch would hide required path /tmp; move the working directory or configured root below /tmp, or set agents.isolation = \"host\""
    )]
    ScratchCollision,
    #[error(
        "unknown skill {name:?}; searched {roots:?}; install the skill or remove it from the profile"
    )]
    UnknownSkill { name: String, roots: Vec<PathBuf> },
    #[error("profile skills need agents.isolation = \"sandbox\"")]
    SkillsNeedSandbox,
    #[error("invalid profile skills: {0}")]
    Skills(#[from] crate::config::SkillSpecErr),
}

pub struct SandboxInputs<'a> {
    pub env: &'a BTreeMap<String, String>,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub worktree: Option<&'a Path>,
    pub scratch_dir: &'a Path,
    pub provider_home: Option<ProviderHome>,
    pub skills: &'a [SkillSpec],
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

pub fn preflight(isolation: Isolation) -> Result<(), SandboxErr> {
    if isolation == Isolation::Host {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    return linux::probe().map(|_| ());
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

pub fn prepare(inputs: &SandboxInputs<'_>) -> Result<MountPlan, SandboxErr> {
    let views = skills::prepare(inputs.env, inputs.skills)?;
    let mut mounts = Vec::new();
    let mut required = crate::mux::domain::ProcessDomain::required_paths(inputs.env);
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
        "RIMZ_AGENTS_HOME",
    ] {
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
    validate_path(inputs.scratch_dir)?;
    mounts.push(Mount::Bind {
        source: inputs.scratch_dir.to_path_buf(),
        target: PathBuf::from("/tmp"),
    });
    let mut reach = BTreeSet::new();
    for path in required {
        validate_path(&path)?;
        let path = crate::utils::path::normalize_path_lexical(&path);
        if path == Path::new("/tmp") {
            return Err(SandboxErr::ScratchCollision);
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
    for view in views {
        validate_path(&view.root)?;
        mounts.push(Mount::Tmpfs {
            target: view.root.clone(),
        });
        for entry in view.entries {
            validate_path(&entry.source)?;
            mounts.push(Mount::RoBind {
                source: entry.source,
                target: view.root.join(entry.name),
            });
        }
    }
    crate::disk::paths::ensure_private_runtime_dir(inputs.scratch_dir)?;
    Ok(MountPlan { mounts })
}

fn validate_path(path: &Path) -> Result<(), SandboxErr> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(SandboxErr::InvalidPath(path.to_path_buf()));
    }
    Ok(())
}

pub fn bwrap_argv(plan: &MountPlan, cwd: &Path, inner: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = [
        "bwrap",
        "--bind",
        "/",
        "/",
        "--dev-bind",
        "/dev",
        "/dev",
        "--die-with-parent",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
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
        insta::assert_debug_snapshot!(bwrap_argv(&plan, Path::new("/project"), &["agent".into(), "two words".into()]), @r###"
        [
            "bwrap",
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
