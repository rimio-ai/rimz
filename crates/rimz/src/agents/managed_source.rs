//! Managed integration seam for whole-file RimZ sources and strict JSON hook
//! merges. Adapters declare one source; backend-specific ownership and merge
//! policy stays behind it.

use std::path::{Path, PathBuf};

use crate::store::atomic;

use super::hook_types::HookEventSpec;
use super::managed_json_hooks::ManagedJsonHookSpec;
use super::{
    AgentErr, AgentSpec, HookInstallFilePreview, HookInstallFileReport, HookInstallPreview,
    HookInstallReport, HookUninstallReport, Result, read_optional_file,
};

pub(crate) const RIMZ_MANAGED_MARKER: &str = "_rimz_managed";

/// Provider-owned file transaction behind the adapter's managed integration seam.
pub trait ManagedIntegration: Sync {
    fn install(&self) -> Result<HookInstallReport>;
    fn preview(&self) -> Result<HookInstallPreview>;
    fn uninstall(&self) -> Result<HookUninstallReport>;
    fn installed(&self) -> bool;

    fn managed_artifacts_present(&self) -> bool {
        self.installed()
    }

    fn upgrade_available(&self) -> bool {
        false
    }

    fn wiring_input_paths(&self, _descriptor: &AgentSpec) -> Vec<PathBuf> {
        Vec::new()
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        None
    }

    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        None
    }

    fn untrusted_installed_hooks(&self) -> Vec<String> {
        Vec::new()
    }

    fn untrusted_preflight_hooks(&self) -> Vec<String> {
        self.untrusted_installed_hooks()
    }
}

enum ManagedSourceBackend {
    WholeFile {
        source: &'static str,
        catalog: &'static [HookEventSpec],
        artifact_noun: &'static str,
        upgradeable: bool,
    },
    JsonHooks(&'static ManagedJsonHookSpec),
}

/// One adapter's managed integration source.
pub struct ManagedSource {
    agent: &'static str,
    path: fn() -> Result<PathBuf>,
    backend: ManagedSourceBackend,
}

impl ManagedSource {
    pub(crate) const fn new(
        agent: &'static str,
        source: &'static str,
        catalog: &'static [HookEventSpec],
        artifact_noun: &'static str,
        path: fn() -> Result<PathBuf>,
        upgradeable: bool,
    ) -> Self {
        Self {
            agent,
            path,
            backend: ManagedSourceBackend::WholeFile {
                source,
                catalog,
                artifact_noun,
                upgradeable,
            },
        }
    }

    pub(crate) const fn json(
        spec: &'static ManagedJsonHookSpec,
        path: fn() -> Result<PathBuf>,
    ) -> Self {
        Self {
            agent: spec.agent,
            path,
            backend: ManagedSourceBackend::JsonHooks(spec),
        }
    }

    pub fn install(&self) -> Result<HookInstallReport> {
        let path = (self.path)()?;
        self.install_into(&path)
    }

    /// Resolve the provider-owned path without reading or mutating it.
    pub fn resolved_path(&self) -> Result<PathBuf> {
        (self.path)()
    }

    pub fn preview(&self) -> Result<HookInstallPreview> {
        let path = (self.path)()?;
        self.preview_at(&path)
    }

    pub fn uninstall(&self) -> Result<HookUninstallReport> {
        let path = (self.path)()?;
        self.uninstall_from(&path)
    }

    pub fn installed(&self) -> bool {
        (self.path)().is_ok_and(|path| self.installed_at(&path))
    }

    pub fn upgrade_available(&self) -> bool {
        (self.path)().is_ok_and(|path| self.upgrade_available_at(&path))
    }

    pub fn managed_artifacts_present(&self) -> bool {
        (self.path)().is_ok_and(|path| self.managed_artifacts_at(&path))
    }

    pub fn wrapped_status_line_command(&self) -> Option<String> {
        self.wrapped_status_line_command_at(0)
    }

    pub fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        self.wrapped_status_line_command_at(1)
    }

    /// Install is whole-file ownership: the embedded source overwrites the path
    /// verbatim - idempotent by construction. A marked file (RimZ wrote it,
    /// however edited since) is reclaimed byte-for-byte; an unmarked file is
    /// the user's own source and refuses.
    pub fn install_into(&self, path: &Path) -> Result<HookInstallReport> {
        let source = match &self.backend {
            ManagedSourceBackend::JsonHooks(spec) => return spec.install_into(path),
            ManagedSourceBackend::WholeFile { source, .. } => source,
        };
        let original = read_optional_file(self.agent, path)?;
        self.refuse_unmarked(path, original.as_deref())?;
        atomic::write_bytes_atomically(path, source.as_bytes())?;
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
        let source = match &self.backend {
            ManagedSourceBackend::JsonHooks(spec) => return spec.preview_at(path),
            ManagedSourceBackend::WholeFile { source, .. } => source,
        };
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
                candidate: (*source).to_owned(),
            }],
            planned_events: self.installed_event_names(),
            status_line_change: None,
            subagent_status_line_change: None,
        })
    }

    pub fn uninstall_from(&self, path: &Path) -> Result<HookUninstallReport> {
        if let ManagedSourceBackend::JsonHooks(spec) = &self.backend {
            return spec.uninstall_from(path);
        }
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
    /// as "not installed". The first-line marker distinguishes the RimZ-owned
    /// source from a user's own file at the same path.
    pub fn installed_at(&self, path: &Path) -> bool {
        match &self.backend {
            ManagedSourceBackend::WholeFile { .. } => {
                std::fs::read_to_string(path).is_ok_and(|content| file_is_rimz_managed(&content))
            }
            ManagedSourceBackend::JsonHooks(spec) => spec.installed_at(path),
        }
    }

    pub fn managed_artifacts_at(&self, path: &Path) -> bool {
        match &self.backend {
            ManagedSourceBackend::WholeFile { .. } => self.installed_at(path),
            ManagedSourceBackend::JsonHooks(spec) => spec.managed_artifacts_at(path),
        }
    }

    fn wrapped_status_line_command_at(&self, index: usize) -> Option<String> {
        let path = (self.path)().ok()?;
        match &self.backend {
            ManagedSourceBackend::WholeFile { .. } => None,
            ManagedSourceBackend::JsonHooks(spec) => {
                spec.wrapped_status_line_command_at(&path, index)
            }
        }
    }

    /// Whether a RimZ-owned source differs from the source embedded in this
    /// build. Best-effort: missing, unreadable, and user-owned files are not
    /// upgrade candidates.
    pub fn upgrade_available_at(&self, path: &Path) -> bool {
        let ManagedSourceBackend::WholeFile {
            source,
            upgradeable,
            ..
        } = &self.backend
        else {
            return false;
        };
        *upgradeable
            && std::fs::read_to_string(path)
                .is_ok_and(|content| file_is_rimz_managed(&content) && content != *source)
    }

    fn refuse_unmarked(&self, path: &Path, original: Option<&str>) -> Result<()> {
        let ManagedSourceBackend::WholeFile { artifact_noun, .. } = &self.backend else {
            return Ok(());
        };
        match original {
            Some(existing) if !file_is_rimz_managed(existing) => Err(AgentErr::Install {
                agent: self.agent,
                reason: format!(
                    "refusing to overwrite an unmarked user {} at {}; move it aside or remove it to let RimZ manage this file",
                    artifact_noun,
                    path.display()
                ),
            }),
            _ => Ok(()),
        }
    }

    fn installed_event_names(&self) -> Vec<String> {
        let ManagedSourceBackend::WholeFile { catalog, .. } = &self.backend else {
            return Vec::new();
        };
        catalog.iter().map(|hook| hook.event.to_owned()).collect()
    }
}

impl ManagedIntegration for ManagedSource {
    fn install(&self) -> Result<HookInstallReport> {
        ManagedSource::install(self)
    }

    fn preview(&self) -> Result<HookInstallPreview> {
        ManagedSource::preview(self)
    }

    fn uninstall(&self) -> Result<HookUninstallReport> {
        ManagedSource::uninstall(self)
    }

    fn installed(&self) -> bool {
        ManagedSource::installed(self)
    }

    fn managed_artifacts_present(&self) -> bool {
        ManagedSource::managed_artifacts_present(self)
    }

    fn upgrade_available(&self) -> bool {
        ManagedSource::upgrade_available(self)
    }

    fn wiring_input_paths(&self, definition: &AgentSpec) -> Vec<PathBuf> {
        if definition.capabilities.local_session_discovery || !definition.has_wired_hook_install() {
            return Vec::new();
        }
        self.resolved_path().into_iter().collect()
    }

    fn wrapped_status_line_command(&self) -> Option<String> {
        ManagedSource::wrapped_status_line_command(self)
    }

    fn wrapped_subagent_status_line_command(&self) -> Option<String> {
        ManagedSource::wrapped_subagent_status_line_command(self)
    }
}

/// Whether the on-disk source is RimZ-owned: the ownership marker rides the
/// first line of every managed build.
fn file_is_rimz_managed(content: &str) -> bool {
    content
        .lines()
        .next()
        .is_some_and(|line| line.contains(RIMZ_MANAGED_MARKER))
}
