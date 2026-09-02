use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::super::modules::{crate_module_for_path, module_is_within, path_in_scope};
use super::super::syntax::FileSyntax;

#[cfg(test)]
#[path = "selector/tests.rs"]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::atlas) struct ModuleSelector {
    pub(in crate::atlas::inspect) module: String,
    pub(in crate::atlas::inspect) path: Option<PathBuf>,
    pub(in crate::atlas::inspect) directory: bool,
}

impl ModuleSelector {
    pub(in crate::atlas) fn matches(&self, module: &str, path: &Path) -> bool {
        if !module_is_within(module, &self.module) {
            return false;
        }
        match &self.path {
            Some(scope) if self.directory => {
                path_in_scope(path, scope) || path == scope.with_extension("rs")
            }
            Some(file) => path == file,
            None => true,
        }
    }
}

pub(in crate::atlas) fn resolve_module(
    root: &Path,
    syntax_files: &[FileSyntax],
    raw: &str,
    command: &str,
    flag: &str,
) -> Result<ModuleSelector> {
    let path_like = raw.contains('/') || raw.ends_with(".rs") || root.join(raw).exists();
    let (module, path, directory) = if path_like {
        let path = super::super::validate_scope(raw, &format!("{command} {flag}"))?;
        let absolute = root.join(&path);
        if !absolute.exists() {
            bail!("atlas {command} {flag} path `{raw}` does not exist");
        }
        let directory = absolute.is_dir();
        if !directory && path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            bail!("atlas {command} {flag} `{raw}` does not match a Rust module");
        }
        let entry = if directory {
            path.join("mod.rs")
        } else {
            path.clone()
        };
        (crate_module_for_path(&entry), Some(path), directory)
    } else {
        (
            raw.strip_prefix("crate::").unwrap_or(raw).to_owned(),
            None,
            false,
        )
    };
    if syntax_files.iter().any(|file| {
        module_is_within(&file.module_path, &module)
            && match &path {
                Some(scope) if directory => {
                    path_in_scope(&file.path, scope) || file.path == scope.with_extension("rs")
                }
                Some(source) => &file.path == source,
                None => true,
            }
    }) {
        return Ok(ModuleSelector {
            module,
            path,
            directory,
        });
    }
    bail!("atlas {command} {flag} `{raw}` does not match a Rust module")
}
