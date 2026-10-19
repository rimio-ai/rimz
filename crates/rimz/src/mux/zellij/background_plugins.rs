//! Read-only resolution of the background plugins a birth layout must preserve.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode};

use crate::mux::CommandSpec;

const DEFAULT_BACKGROUND_PLUGINS: [&str; 1] = ["zellij:link"];

pub(super) fn resolve(spec: &CommandSpec, cwd: &Path) -> Option<KdlDocument> {
    resolve_with(cwd, |key| {
        effective_env(spec, key, |key| std::env::var_os(key))
    })
}

fn effective_env(
    spec: &CommandSpec,
    key: &str,
    inherited: impl Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
    if let Some(value) = spec.env.get(key) {
        return Some(value.into());
    }
    if spec.env_remove.contains(key) {
        return None;
    }
    inherited(key)
}

fn resolve_with(cwd: &Path, env: impl Fn(&str) -> Option<OsString>) -> Option<KdlDocument> {
    let explicit = env("ZELLIJ_CONFIG_FILE");
    let path = if let Some(path) = &explicit {
        cwd.join(path)
    } else {
        let directory = env("ZELLIJ_CONFIG_DIR").map(PathBuf::from).or_else(|| {
            let home = env("HOME").map(PathBuf::from);
            #[cfg(target_os = "macos")]
            let platform = home.as_ref().map(|home| {
                home.join("Library/Application Support/org.Zellij-Contributors.Zellij")
            });
            #[cfg(not(target_os = "macos"))]
            let platform = env("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join("zellij"))
                .or_else(|| home.as_ref().map(|home| home.join(".config/zellij")));
            [
                home.as_ref().map(|home| home.join(".config/zellij")),
                platform,
                Some(PathBuf::from("/etc/zellij")),
            ]
            .into_iter()
            .flatten()
            .map(|path| cwd.join(path))
            .find(|path| path.exists())
        });
        let Some(directory) = directory else {
            return Some(from_config(&KdlDocument::new()));
        };
        cwd.join(directory).join("config.kdl")
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            return Some(from_config(&KdlDocument::new()));
        }
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "reading Zellij background plugins failed; keeping the pipe load route");
            return None;
        }
    };
    match raw.parse::<KdlDocument>() {
        Ok(config) => Some(from_config(&config)),
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "parsing Zellij background plugins failed; keeping the pipe load route");
            None
        }
    }
}

pub(super) fn from_config(config: &KdlDocument) -> KdlDocument {
    if let Some(block) = config.get("load_plugins") {
        return block.children().cloned().unwrap_or_default();
    }
    let mut plugins = KdlDocument::new();
    for location in DEFAULT_BACKGROUND_PLUGINS {
        plugins.nodes_mut().push(KdlNode::new(location));
    }
    plugins
}

#[cfg(test)]
#[path = "background_plugins_tests.rs"]
mod tests;
