//! Whole-file Rimz-managed integration sources (pi's extension, OpenCode's
//! plugin): the ownership-marker protocol shared by every adapter whose wire
//! Rimz authors. Install is whole-file ownership: a marked file is reclaimed
//! byte-for-byte, an unmarked file is the user's and refuses.

use std::path::Path;

use crate::store::atomic;

use super::{
    AgentErr, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview, HookInstallReport,
    HookUninstallReport, Result, read_optional_file,
};

pub(crate) const RIMZ_MANAGED_MARKER: &str = "_rimz_managed";

pub(crate) struct ManagedSource {
    pub agent: &'static str,
    /// The embedded integration source; carries RIMZ_MANAGED_MARKER on line 1.
    pub source: &'static str,
    pub wired_events: &'static [&'static str],
    /// The noun in the refuse-unmarked message ("extension" / "plugin"),
    /// keeping each adapter's existing user-facing error text byte-identical.
    pub artifact_noun: &'static str,
}

impl ManagedSource {
    /// Install is whole-file ownership: the embedded source overwrites the path
    /// verbatim - idempotent by construction. A marked file (Rimz wrote it,
    /// however edited since) is reclaimed byte-for-byte; an unmarked file is
    /// the user's own source and refuses.
    pub fn install_into(&self, path: &Path) -> Result<HookInstallReport> {
        let original = read_optional_file(self.agent, path)?;
        self.refuse_unmarked(path, original.as_deref())?;
        atomic::write_bytes_atomically(path, self.source.as_bytes())?;
        Ok(HookInstallReport {
            agent: self.agent,
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed: original.is_some(),
            }],
            installed_events: self.installed_event_names(),
        })
    }

    pub fn preview_at(&self, path: &Path) -> Result<HookInstallPreview> {
        let original = read_optional_file(self.agent, path)?;
        // Mirror install's refusal so the consent gate surfaces the conflict
        // before a doomed install, not after.
        self.refuse_unmarked(path, original.as_deref())?;
        Ok(HookInstallPreview {
            agent: self.agent,
            files: vec![HookInstallFilePreview {
                path: path.to_path_buf(),
                existed: original.is_some(),
                original,
                candidate: self.source.to_owned(),
            }],
            planned_events: self.installed_event_names(),
            status_line_change: None,
            subagent_status_line_change: None,
        })
    }

    pub fn uninstall_from(&self, path: &Path) -> Result<HookUninstallReport> {
        let original = read_optional_file(self.agent, path)?;
        let existed = original.is_some();
        let mut removed_events = Vec::new();
        if original.as_deref().is_some_and(file_is_rimz_managed) {
            std::fs::remove_file(path).map_err(|source| AgentErr::InstallIo {
                agent: self.agent,
                path: path.to_path_buf(),
                source,
            })?;
            removed_events = self.installed_event_names();
        }
        // An unmarked file is user-owned: left in place, nothing removed.
        Ok(HookUninstallReport {
            agent: self.agent,
            files: vec![HookInstallFileReport {
                path: path.to_path_buf(),
                existed,
            }],
            removed_events,
        })
    }

    /// Best-effort like the other adapters: a missing or unreadable file reads
    /// as "not installed". The first-line marker distinguishes the Rimz-owned
    /// source from a user's own file at the same path.
    pub fn installed_at(&self, path: &Path) -> bool {
        std::fs::read_to_string(path).is_ok_and(|content| file_is_rimz_managed(&content))
    }

    fn refuse_unmarked(&self, path: &Path, original: Option<&str>) -> Result<()> {
        match original {
            Some(existing) if !file_is_rimz_managed(existing) => Err(AgentErr::Install {
                agent: self.agent,
                reason: format!(
                    "refusing to overwrite an unmarked user {} at {}; move it aside or remove it to let Rimz manage this file",
                    self.artifact_noun,
                    path.display()
                ),
            }),
            _ => Ok(()),
        }
    }

    fn installed_event_names(&self) -> Vec<String> {
        self.wired_events
            .iter()
            .map(|event| (*event).to_owned())
            .collect()
    }
}

/// Whether the on-disk source is Rimz-owned: the ownership marker rides the
/// first line of every managed build.
fn file_is_rimz_managed(content: &str) -> bool {
    content
        .lines()
        .next()
        .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
}
