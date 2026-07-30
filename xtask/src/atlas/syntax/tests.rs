use super::*;

fn source(text: &str) -> Source {
    Source {
        path: PathBuf::from("crates/rimz/src/cli/demo.rs"),
        text: text.to_owned(),
    }
}

#[test]
fn extracts_boundary_items_imports_params_and_shapes() {
    let report = analyze_sources(&[source(
        r#"
use crate::agents::{AgentDefinition, state::Rollup};
use super::render::table;
pub(crate) struct View;
pub(self) fn hidden() {}
pub fn run(a: usize, b: bool) {
    if a > 0 { table(); }
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
    assert!(file.fns[1].skeleton.iter().any(|token| token == "IF"));
    assert!(file.fns[1].skeleton.iter().any(|token| token == "MATCH2"));
}

#[test]
fn parse_failures_are_reported_without_aborting_other_files() {
    let report = analyze_sources(&[source("fn {"), source("pub struct Fine;")]);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.parse_failures.len(), 1);
}
