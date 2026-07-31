use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};

pub(super) fn scope_for_matching(scope: &Path) -> &Path {
    match scope.strip_prefix(Path::new(".")) {
        Ok(scope) if scope.as_os_str().is_empty() => Path::new(""),
        Ok(scope) => scope,
        Err(_) => scope,
    }
}

pub(super) fn module_for_path(path: &Path, scope: &Path) -> String {
    let relative = path.strip_prefix(scope_for_matching(scope)).unwrap_or(path);
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return file_module(path);
    };
    if components.next().is_some() {
        first.as_os_str().to_string_lossy().into_owned()
    } else if crate::source_files::is_test_file(relative) {
        "(root)".to_owned()
    } else {
        file_module(relative)
    }
}

pub(super) fn rust_module_for_path(path: &Path, scope: &Path) -> Option<String> {
    (path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs"))
        .then(|| module_for_path(path, scope))
}

pub(super) fn file_module(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return "(root)".to_owned();
    };
    let name = name.to_string_lossy();
    match name.as_ref() {
        "lib.rs" | "main.rs" | "mod.rs" => "(root)".to_owned(),
        _ => name.strip_suffix(".rs").unwrap_or(&name).to_owned(),
    }
}

pub(super) fn crate_module_for_path(path: &Path) -> String {
    let mut after_src = false;
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        let component = component.to_string_lossy();
        if after_src {
            parts.push(component.into_owned());
        } else if component == "src" {
            after_src = true;
        }
    }
    if let Some(last) = parts.last_mut() {
        if matches!(last.as_str(), "lib.rs" | "main.rs" | "mod.rs") {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_owned();
        }
    }
    parts.join("::")
}

pub(super) fn path_in_scope(path: &Path, scope: &Path) -> bool {
    path.starts_with(scope_for_matching(scope))
}

pub(super) fn workspace_crate_names(root: &Path) -> Result<BTreeSet<String>> {
    let manifest_path = root.join("Cargo.toml");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", manifest_path.display()))?;
    let mut names = BTreeSet::new();
    if let Some(name) = crate_name_from_manifest(&raw)? {
        names.insert(name);
    }
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str);
    for member in members {
        for directory in workspace_member_directories(root, member)? {
            let path = directory.join("Cargo.toml");
            let raw =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            if let Some(name) = crate_name_from_manifest(&raw)
                .with_context(|| format!("parsing {}", path.display()))?
            {
                names.insert(name);
            }
        }
    }
    Ok(names)
}

fn workspace_member_directories(root: &Path, member: &str) -> Result<Vec<PathBuf>> {
    let Some(parent) = member.strip_suffix("/*") else {
        return Ok(vec![root.join(member)]);
    };
    let directory = root.join(parent);
    let mut members = fs::read_dir(&directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect::<Vec<_>>();
    members.sort();
    Ok(members)
}

fn crate_name_from_manifest(raw: &str) -> Result<Option<String>> {
    let manifest: toml::Value = toml::from_str(raw)?;
    let name = manifest
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            manifest
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        });
    Ok(name.map(|name| name.replace('-', "_")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_rollup_follows_the_requested_scope() {
        assert_eq!(
            module_for_path(
                Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
                Path::new("crates/rimz/src/cli")
            ),
            "agents_cmd"
        );
        assert_eq!(
            crate_module_for_path(Path::new("crates/rimz/src/cli/agents_cmd/mod.rs")),
            "cli::agents_cmd"
        );
        assert_eq!(
            rust_module_for_path(
                Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
                Path::new("crates/rimz/src/cli")
            )
            .as_deref(),
            Some("agents_cmd")
        );
        assert_eq!(
            rust_module_for_path(
                Path::new("crates/rimz/src/cli/snapshots/surface.snap"),
                Path::new("crates/rimz/src/cli")
            ),
            None
        );
        assert_eq!(
            module_for_path(
                Path::new("crates/rimz/src/cli/surface_tests.rs"),
                Path::new("crates/rimz/src/cli")
            ),
            "(root)"
        );
    }

    #[test]
    fn manifest_crate_names_follow_lib_override_and_rust_normalization() {
        assert_eq!(
            crate_name_from_manifest("[package]\nname = \"rimz-presence-zellij\"\n")
                .unwrap()
                .as_deref(),
            Some("rimz_presence_zellij")
        );
        assert_eq!(
            crate_name_from_manifest(
                "[package]\nname = \"package-name\"\n[lib]\nname = \"import_name\"\n"
            )
            .unwrap()
            .as_deref(),
            Some("import_name")
        );
    }
}
