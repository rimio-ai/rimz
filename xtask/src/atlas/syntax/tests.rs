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
    assert_eq!(
        names,
        [
            "View",
            "render",
            "Draw",
            "draw",
            "nested",
            "run",
            "not_boundary"
        ]
    );
}

#[test]
fn visibility_reach_follows_restrictions_and_private_module_links() {
    let report = analyze_sources(&[source(
        r#"
pub fn everywhere() {}
pub(crate) fn in_crate() {}
pub(super) fn in_parent() {}
pub(in crate::cli) fn in_cli() {}
mod private {
    pub fn confined() {}
}
pub(crate) mod visible {
    pub fn still_in_crate() {}
}
"#,
    )]);
    let file = &report.files[0];
    let reaches = file
        .pub_items
        .iter()
        .map(|item| (item.name.as_str(), item.reach.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(reaches["everywhere"], EXTERNAL_REACH);
    assert_eq!(reaches["in_crate"], "");
    assert_eq!(reaches["in_parent"], "cli");
    assert_eq!(reaches["in_cli"], "cli");
    assert_eq!(reaches["confined"], "cli::demo");
    assert_eq!(reaches["still_in_crate"], "");

    let index = ModIndex::new(std::slice::from_ref(file));
    let confined = file
        .pub_items
        .iter()
        .find(|item| item.name == "confined")
        .unwrap();
    assert_eq!(index.effective_reach(file, confined), "cli::demo");
}

#[test]
fn external_module_declarations_confine_items_in_sibling_files() {
    let parent = Source::new("crates/rimz/src/cli/mod.rs", "mod demo;\n");
    let child = source("pub fn confined() {}\n");
    let report = analyze_sources(&[parent, child]);
    let index = ModIndex::new(&report.files);
    let file = report
        .files
        .iter()
        .find(|file| file.module_path == "cli::demo")
        .unwrap();
    let item = &file.pub_items[0];
    assert_eq!(index.effective_reach(file, item), "cli");
}

#[test]
fn module_declarations_do_not_collide_across_crates() {
    let report = analyze_sources(&[
        Source::new("crates/a/src/lib.rs", "pub mod shared;\n"),
        Source::new("crates/a/src/shared.rs", "pub fn visible() {}\n"),
        Source::new("crates/b/src/lib.rs", "mod shared;\n"),
        Source::new("crates/b/src/shared.rs", "pub fn confined() {}\n"),
    ]);
    let index = ModIndex::new(&report.files);
    let effective_reach = |crate_path: &str, name: &str| {
        let file = report
            .files
            .iter()
            .find(|file| file.crate_path == Path::new(crate_path) && file.module_path == "shared")
            .unwrap();
        let item = file
            .pub_items
            .iter()
            .find(|item| item.name == name)
            .unwrap();
        index.effective_reach(file, item)
    };

    assert_eq!(effective_reach("crates/a", "visible"), EXTERNAL_REACH);
    assert_eq!(effective_reach("crates/b", "confined"), "");
}

#[test]
fn inline_test_regions_cover_mid_file_trailing_and_multiple_modules() {
    let report = analyze_sources(&[source(
        r#"
fn before() {}
#[cfg(test)]
mod first {
    fn check() {}
}
fn middle() {}
#[cfg(test)]
mod second {
    fn check() {}
}
"#,
    )]);
    assert_eq!(report.files[0].test_regions, [3..7, 8..12]);
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
