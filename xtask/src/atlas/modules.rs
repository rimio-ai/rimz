use std::path::{Component, Path};

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
    } else {
        file_module(relative)
    }
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
    }
}
