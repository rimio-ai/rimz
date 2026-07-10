//! Machine-tier plugin discovery and one-shot registry loading.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing::warn;

use super::manifest::{PluginManifest, resolve_path};
use super::probes::resolve_executable;
use super::{PluginAdapter, build_adapter};
use crate::agents::AgentAdapter;

static PLUGINS: OnceLock<LoadedPlugins> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PluginLoadError {
    pub path: PathBuf,
    pub kind_hint: Option<String>,
    pub error: String,
}

impl std::fmt::Display for PluginLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.error)
    }
}

#[derive(Clone, Debug)]
pub struct PluginDiagnostic {
    pub kind: String,
    pub path: PathBuf,
    pub valid: bool,
    pub error: Option<String>,
    pub setup_doc: Option<PathBuf>,
    pub probes: Vec<ProbeDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct ProbeDiagnostic {
    pub name: &'static str,
    pub command: String,
    pub present: bool,
    pub executable: bool,
}

pub struct LoadedPlugins {
    pub adapters: Vec<&'static dyn AgentAdapter>,
    pub errors: Vec<PluginLoadError>,
    pub diagnostics: Vec<PluginDiagnostic>,
    pub(super) plugin_adapters: Vec<&'static PluginAdapter>,
}

pub fn loaded() -> &'static LoadedPlugins {
    PLUGINS.get_or_init(|| {
        let loaded = load_from_root(&plugins_root());
        for error in &loaded.errors {
            warn!(path = %error.path.display(), error = %error.error, "agent plugin skipped");
        }
        loaded
    })
}

pub fn plugins_root() -> PathBuf {
    crate::store::paths::config_home()
        .join("rimz")
        .join("agents.d")
}

pub fn load_from_root(root: &Path) -> LoadedPlugins {
    let mut manifest_paths = fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("agent.toml"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    let mut adapters = Vec::new();
    let mut plugin_adapters = Vec::new();
    let mut errors = Vec::new();
    let mut diagnostics = Vec::new();
    let mut kinds = HashSet::new();
    for path in manifest_paths {
        let kind_hint = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                push_error(
                    &mut errors,
                    &mut diagnostics,
                    path,
                    kind_hint,
                    format!("cannot read manifest: {err}"),
                );
                continue;
            }
        };
        let manifest = match PluginManifest::parse(&path, &text) {
            Ok(manifest) => manifest,
            Err(error) => {
                push_error(&mut errors, &mut diagnostics, path, kind_hint, error);
                continue;
            }
        };
        if !kinds.insert(manifest.kind.clone()) {
            push_error(
                &mut errors,
                &mut diagnostics,
                path,
                Some(manifest.kind.clone()),
                format!(
                    "kind `{}` is declared by more than one plugin",
                    manifest.kind
                ),
            );
            continue;
        }
        let plugin_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        diagnostics.push(valid_diagnostic(&path, &plugin_dir, &manifest));
        let adapter: &'static PluginAdapter = build_adapter(manifest, plugin_dir);
        plugin_adapters.push(adapter);
        adapters.push(adapter as &'static dyn AgentAdapter);
    }
    LoadedPlugins {
        adapters,
        errors,
        diagnostics,
        plugin_adapters,
    }
}

fn push_error(
    errors: &mut Vec<PluginLoadError>,
    diagnostics: &mut Vec<PluginDiagnostic>,
    path: PathBuf,
    kind_hint: Option<String>,
    error: String,
) {
    diagnostics.push(PluginDiagnostic {
        kind: kind_hint.clone().unwrap_or_else(|| "unknown".into()),
        path: path.clone(),
        valid: false,
        error: Some(error.clone()),
        setup_doc: None,
        probes: Vec::new(),
    });
    errors.push(PluginLoadError {
        path,
        kind_hint,
        error,
    });
}

fn valid_diagnostic(path: &Path, plugin_dir: &Path, manifest: &PluginManifest) -> PluginDiagnostic {
    let probes = [
        ("spend", manifest.probes.spend.as_ref()),
        ("account", manifest.probes.account.as_ref()),
        ("version", manifest.probes.version.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, argv)| {
        let argv = argv?;
        let executable = resolve_executable(plugin_dir, &argv[0]);
        let resolved = if executable.components().count() > 1 || executable.is_absolute() {
            Some(executable)
        } else {
            which::which(&executable).ok()
        };
        let present = resolved.as_ref().is_some_and(|path| path.is_file());
        #[cfg(unix)]
        let executable_bit = resolved
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .is_some_and(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            });
        #[cfg(not(unix))]
        let executable_bit = present;
        Some(ProbeDiagnostic {
            name,
            command: argv.join(" "),
            present,
            executable: executable_bit,
        })
    })
    .collect();

    PluginDiagnostic {
        kind: manifest.kind.clone(),
        path: path.to_path_buf(),
        valid: true,
        error: None,
        setup_doc: Some(resolve_path(plugin_dir, &manifest.setup_doc)),
        probes,
    }
}
