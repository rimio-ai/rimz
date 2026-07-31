use super::*;

fn source(text: &str) -> Source {
    Source::new("crates/rimz/src/cli/demo.rs", text)
}

#[test]
fn extracts_boundary_items_imports_params_and_callees() {
    let report = analyze_sources(&[source(
        r#"
use crate::agents::{AgentDefinition, state::Rollup};
use super::render::table;
pub(crate) struct View;
pub(self) fn hidden() {}
pub fn run(a: usize, b: bool) {
    if a > 0 { table().render(); }
    View::build();
    match b { true => return, false => {} }
}
"#,
    )]);
    let file = &report.files[0];
    assert_eq!(file.pub_items.len(), 2);
    assert_eq!(file.pub_items[1].params, Some(2));
    assert_eq!(
        file.imports
            .iter()
            .map(|item| item.module_path.as_str())
            .collect::<Vec<_>>(),
        ["agents", "agents::state", "cli::render"]
    );
    assert_eq!(
        file.fns[1]
            .callees
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["table", ".render", "View::build"])
    );
}

#[test]
fn parse_failures_are_reported_without_aborting_other_files() {
    let report = analyze_sources(&[source("fn {"), source("pub struct Fine;")]);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.parse_failures.len(), 1);
}

#[test]
fn public_surface_includes_visible_methods_traits_and_inline_modules() {
    let report = analyze_sources(&[source(
        r#"
pub struct View;
impl View {
    pub fn render(&self, width: usize) {}
    fn hidden(&self) {}
}
pub trait Draw {
    fn draw(&self);
}
pub(crate) mod nested {
    pub fn run() {}
}
mod private {
    pub fn not_boundary() {}
}
"#,
    )]);
    let names = report.files[0]
        .pub_items
        .iter()
        .map(|item| item.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["View", "render", "Draw", "draw", "nested", "run"]);
}

#[test]
fn workspace_crate_imports_normalize_to_local_module_paths() {
    let report = analyze_sources(&[source(
        "use rimz::store::Event;\nuse rimz::agents;\nuse anyhow::Result;\n",
    )]);
    let known_modules = ["store", "agents"].map(str::to_owned).into_iter().collect();
    let workspace_crates = ["rimz".to_owned()].into_iter().collect();
    let resolved = report.files[0]
        .imports
        .iter()
        .filter_map(|import| resolved_internal_import(import, &known_modules, &workspace_crates))
        .collect::<Vec<_>>();
    assert_eq!(resolved, ["store", "agents"]);
}
