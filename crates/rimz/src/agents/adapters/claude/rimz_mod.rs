//! RimZ's Claude Code mod: the embedded plugin folder hook install writes under
//! the RimZ home, and the settings entry that loads it in every session.
//!
//! The folder's content is stable across RimZ releases and carries its own
//! [`MOD_VERSION`]: every rewrite of an installed file prints a reload line in
//! each open session of the account, so install writes only a file whose bytes
//! differ. Claude also lays its own `.claude-plugin/types/` beside the files at
//! load; only the files listed here are RimZ's, so only they count as drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::managed_env_list::ManagedEnvPath;
use crate::agents::{AgentErr, HookInstallFilePreview, HookInstallFileReport, Result};

/// The mod's own version, bumped by hand whenever a file below changes.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only the unit test reads it, pinning the manifest and module to it"
    )
)]
pub(super) const MOD_VERSION: &str = "1.0.0";

/// The settings `env` variable Claude reads its plugin folders from.
pub(super) const PLUGIN_DIRS_KEY: &str = "CLAUDE_CODE_PLUGIN_DIRS";

const FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("rimz_mod/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("rimz_mod/hooks.json")),
    ("hooks/rimz.ts", include_str!("rimz_mod/rimz.ts")),
];

/// The mod folder for this login, shared by every Claude account.
pub(super) fn dir(login_env: &BTreeMap<String, String>) -> PathBuf {
    crate::disk::paths::claude_mod_dir_in(login_env)
}

/// The settings entry that lists `dir` in `env.CLAUDE_CODE_PLUGIN_DIRS`.
pub(super) fn env_path(dir: &Path) -> Result<ManagedEnvPath<'_>> {
    let path = dir.to_str().ok_or_else(|| AgentErr::Install {
        agent: "claude",
        reason: format!(
            "the RimZ mod folder {} is not a UTF-8 path; Claude cannot list it",
            dir.display()
        ),
    })?;
    Ok(ManagedEnvPath {
        key: PLUGIN_DIRS_KEY,
        path,
    })
}

/// Each mod file under `dir` whose content differs from the embedded one, with
/// what is there now (`None` when missing).
pub(super) fn previews(dir: &Path) -> Result<Vec<HookInstallFilePreview>> {
    let mut previews = Vec::new();
    for (relative, content) in FILES {
        let path = dir.join(relative);
        let original = crate::agents::read_optional_file("claude", &path)?;
        if original.as_deref() == Some(*content) {
            continue;
        }
        previews.push(HookInstallFilePreview {
            existed: original.is_some(),
            original,
            candidate: (*content).to_owned(),
            path,
        });
    }
    Ok(previews)
}

/// Write each mod file whose content differs, reporting the files written.
pub(super) fn write(dir: &Path) -> Result<Vec<HookInstallFileReport>> {
    let mut written = Vec::new();
    for preview in previews(dir)? {
        crate::agents::provider_file::write_bytes(&preview.path, preview.candidate.as_bytes())?;
        written.push(HookInstallFileReport {
            path: preview.path,
            existed: preview.existed,
        });
    }
    Ok(written)
}

/// Mod files missing, unreadable, or different from the embedded ones. Reads
/// only.
pub(super) fn outdated(dir: &Path) -> Vec<PathBuf> {
    FILES
        .iter()
        .map(|(relative, expected)| (dir.join(relative), *expected))
        .filter(|(path, expected)| {
            std::fs::read(path).map_or(true, |content| content != expected.as_bytes())
        })
        .map(|(path, _)| path)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_module_carry_the_mod_version() {
        let manifest: serde_json::Value = serde_json::from_str(FILES[0].1).unwrap();
        assert_eq!(manifest["version"], MOD_VERSION);
        assert_eq!(manifest["name"], "rimz");
        let hooks: serde_json::Value = serde_json::from_str(FILES[1].1).unwrap();
        assert_eq!(hooks["modules"], serde_json::json!(["./rimz.ts"]));
        assert!(
            FILES[2]
                .1
                .contains(&format!("const MOD_VERSION = '{MOD_VERSION}'")),
            "the hooks module reports the version the manifest names"
        );
    }

    #[test]
    fn write_touches_only_files_that_differ() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("mods/claude");
        assert_eq!(
            outdated(&dir).len(),
            FILES.len(),
            "a missing folder is drift"
        );
        let first = write(&dir).unwrap();
        assert_eq!(first.len(), FILES.len());
        assert!(first.iter().all(|file| !file.existed));
        assert!(outdated(&dir).is_empty());
        assert!(
            write(&dir).unwrap().is_empty(),
            "a reinstall writes nothing"
        );

        let module = dir.join("hooks/rimz.ts");
        std::fs::write(&module, "stale").unwrap();
        std::fs::create_dir_all(dir.join(".claude-plugin/types")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/types/index.d.ts"),
            "laid by Claude",
        )
        .unwrap();
        assert_eq!(outdated(&dir), vec![module.clone()]);
        let refreshed = write(&dir).unwrap();
        assert_eq!(
            refreshed,
            vec![HookInstallFileReport {
                path: module.clone(),
                existed: true
            }]
        );
        assert!(dir.join(".claude-plugin/types/index.d.ts").is_file());

        std::fs::remove_file(&module).unwrap();
        std::fs::create_dir(&module).unwrap();
        assert_eq!(
            outdated(&dir),
            vec![module],
            "a mod file that cannot be read is stale"
        );
    }
}
