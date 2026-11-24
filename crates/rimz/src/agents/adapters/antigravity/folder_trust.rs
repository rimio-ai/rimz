//! agy's workspace trust under the room's selected login, and the launch bridge that extends
//! it. agy skips its trust prompt only for a folder its `settings.json` `trustedWorkspaces`
//! names exactly: an entry for a parent, or for the primary checkout of a linked worktree, still
//! prompts (agy 1.5.16). RimZ also judges a folder below a trusted entry, and a linked worktree
//! whose primary checkout is trusted, as trusted, and passes such a launch directory to agy as
//! session-only trust in the launch's one `--settings` document, never in the saved file. A
//! pull-request worktree takes trust only from an entry naming it exactly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agents::launch_settings::merge_settings;
use crate::agents::skills::{LaunchSettingsArtifact, LaunchSettingsErr};
use crate::agents::{FolderTrust, FolderTrustGap, FolderTrustPreview};
use crate::worktree::LinkedWorktree;

const TRUSTED: &str = "trustedWorkspaces";

/// The login's saved settings, read as agy reads them.
struct Saved {
    original: Option<String>,
    root: Map<String, Value>,
    entries: Vec<String>,
}

#[derive(PartialEq)]
enum Cover {
    Exact,
    Inherited,
    Untrusted,
}

/// The grant key is the primary checkout for a linked worktree, the folder itself for a
/// pull-request worktree, and otherwise the repository root the caller names, else `cwd`.
pub(super) fn folder_trust(
    cwd: &Path,
    repo_root: Option<&Path>,
    login_env: &BTreeMap<String, String>,
) -> FolderTrust {
    let linked = crate::worktree::linked_worktree(cwd);
    let key = canonical(match &linked {
        Some(linked) if !pull_request(linked) => &linked.primary,
        Some(_) => cwd,
        None => repo_root.unwrap_or(cwd),
    });
    let (path, saved) = match read(login_env) {
        Ok(read) => read,
        Err((path, reason)) => {
            return FolderTrust::Undecided(FolderTrustGap {
                path,
                key,
                grant: Err(reason),
            });
        }
    };
    if cover(cwd, linked.as_ref(), &saved.entries) != Cover::Untrusted {
        return FolderTrust::Decided;
    }
    let grant = preview(saved, &key).map_err(|err| format!("repair `{}`: {err}", path.display()));
    FolderTrust::Undecided(FolderTrustGap { path, key, grant })
}

/// Union the saved list and `cwd` into the launch's settings document when `cwd` is trusted
/// only by inheritance. A settings file agy cannot read leaves argv alone, so agy asks itself.
pub(super) fn bridge(
    login_env: &BTreeMap<String, String>,
    (cwd, artifact_dir): (&Path, &Path),
    extra_args: &mut Vec<String>,
    artifact: &mut Option<LaunchSettingsArtifact>,
) -> Result<(), LaunchSettingsErr> {
    let Ok((_, saved)) = read(login_env) else {
        return Ok(());
    };
    let linked = crate::worktree::linked_worktree(cwd);
    if cover(cwd, linked.as_ref(), &saved.entries) != Cover::Inherited {
        return Ok(());
    }
    let launch_dirs = [cwd.to_path_buf(), canonical(cwd)].map(|dir| dir.display().to_string());
    *artifact = merge_settings(
        cwd,
        artifact_dir,
        extra_args,
        artifact.as_ref(),
        false,
        |object| {
            let list = object
                .entry(TRUSTED)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| format!("{TRUSTED} must be an array"))?;
            for entry in saved.entries.into_iter().chain(launch_dirs) {
                let entry = Value::String(entry);
                if !list.contains(&entry) {
                    list.push(entry);
                }
            }
            Ok(())
        },
    )?;
    Ok(())
}

fn read(login_env: &BTreeMap<String, String>) -> Result<(PathBuf, Saved), (PathBuf, String)> {
    let path = super::install::settings_path(login_env)
        .map_err(|err| (PathBuf::new(), err.to_string()))?;
    let saved = crate::agents::locate::read_optional_file("antigravity", &path)
        .map_err(|err| err.to_string())
        .and_then(|original| {
            let text = original.as_deref().unwrap_or("{}");
            let value = crate::agents::jsonc::from_slice(text.as_bytes())
                .map_err(|err| format!("invalid JSON: {err}"))?;
            let Value::Object(root) = value else {
                return Err("expected a JSON object".to_owned());
            };
            let entries = match root.get(TRUSTED) {
                None => Vec::new(),
                Some(Value::Array(entries)) => entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                Some(_) => return Err(format!("expected {TRUSTED} to be an array")),
            };
            Ok(Saved {
                original,
                root,
                entries,
            })
        });
    match saved {
        Ok(saved) => Ok((path, saved)),
        Err(err) => {
            let reason = format!("repair `{}`: {err}", path.display());
            Err((path, reason))
        }
    }
}

/// A pull-request worktree, or one whose marker cannot say it is not, takes trust only from an
/// entry naming it.
fn pull_request(linked: &LinkedWorktree) -> bool {
    linked.from_pr.is_some() || linked.marker_unreadable
}

fn cover(cwd: &Path, linked: Option<&LinkedWorktree>, entries: &[String]) -> Cover {
    let resolved = canonical(cwd);
    let named = |dir: &Path| entries.iter().any(|entry| Path::new(entry) == dir);
    // Which spelling of the directory agy compares is unobserved, so agy trusts the launch
    // itself only when both spellings are saved, and otherwise takes both from the bridge.
    if named(cwd) && named(&resolved) {
        return Cover::Exact;
    }
    let trusted: Vec<PathBuf> = entries
        .iter()
        .map(|entry| canonical(Path::new(entry)))
        .collect();
    if named(cwd) || trusted.contains(&resolved) {
        return Cover::Inherited;
    }
    if linked.is_some_and(pull_request) {
        return Cover::Untrusted;
    }
    let below_trusted = |dir: &Path| trusted.iter().any(|entry| dir.starts_with(entry));
    if below_trusted(&resolved)
        || linked.is_some_and(|linked| below_trusted(&canonical(&linked.primary)))
    {
        Cover::Inherited
    } else {
        Cover::Untrusted
    }
}

fn preview(saved: Saved, key: &Path) -> Result<FolderTrustPreview, String> {
    let mut root = saved.root;
    root.entry(TRUSTED)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("expected {TRUSTED} to be an array"))?
        .push(Value::String(key.display().to_string()));
    let candidate = crate::agents::settings_json::render_json("antigravity", &root)
        .map_err(|err| err.to_string())?;
    Ok(FolderTrustPreview {
        original: saved.original,
        candidate,
    })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.into())
}
