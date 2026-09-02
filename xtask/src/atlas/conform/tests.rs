use std::fs;

use super::super::target::{ModuleRule, StranglerRule, Verdict, VerdictKind};
use super::*;

#[test]
fn conform_rejects_upward_dependency_spelled_as_a_qualified_path() {
    let root = fixture_root();
    fs::write(root.path().join("src/lib.rs"), "mod lower;\nmod upper;\n").unwrap();
    fs::write(
        root.path().join("src/lower.rs"),
        "fn call() { crate::upper::f(); }\n",
    )
    .unwrap();
    fs::write(root.path().join("src/upper.rs"), "pub fn f() {}\n").unwrap();
    let target = Target {
        version: 5,
        layers: vec![vec!["lower".to_owned()], vec!["upper".to_owned()]],
        modules: vec![module_rule("src/lower.rs")],
        strangler: Vec::new(),
        verdicts: Vec::new(),
    };
    let target_path = root.path().join(TARGET_FILE);

    let report = evaluate(root.path(), &target, &target_path).unwrap();
    let error = enforce(&report).unwrap_err().to_string();

    assert!(
        error.contains("src/lower.rs:1 (qualified) (upper)"),
        "{error}"
    );
    assert_eq!(report.rules[0].unallowed_dependencies, ["upper"]);
}

#[test]
fn conform_directory_rule_covers_sibling_file() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("src/feature")).unwrap();
    let sibling = Path::new("src/feature.rs");

    assert!(rule_covers_path(
        root.path(),
        Path::new("src/feature"),
        sibling
    ));
    assert!(rule_covers_path(
        root.path(),
        Path::new("src/feature"),
        Path::new("src/feature/detail.rs")
    ));
    assert!(!rule_covers_path(
        root.path(),
        Path::new("src/feature"),
        Path::new("src/featured.rs")
    ));
}

#[test]
fn conform_args_only_accept_report_ratchet_and_tighten() {
    assert_eq!(parse_args(&[]).unwrap(), Some(Args { mode: Mode::Report }));
    assert_eq!(
        parse_args(&["--ratchet".into()]).unwrap(),
        Some(Args {
            mode: Mode::Ratchet
        })
    );
    assert_eq!(
        parse_args(&["--tighten".into()]).unwrap(),
        Some(Args {
            mode: Mode::Tighten
        })
    );
    for removed in ["--status", "--init", "--json", "--file"] {
        assert!(parse_args(&[removed.to_owned()]).is_err());
    }
}

#[test]
fn layer_direction_classifies_upward_same_downward_and_unknown() {
    let ranks = LayerRanks::new(&[
        vec!["store".to_owned()],
        vec!["agents".to_owned(), "harness".to_owned()],
        vec!["cli".to_owned()],
    ]);

    assert_eq!(
        layer_direction(&ranks, "store::writer", "cli::render"),
        Some(Direction::Upward)
    );
    assert_eq!(
        layer_direction(&ranks, "agents::state", "harness::target"),
        Some(Direction::Same)
    );
    assert_eq!(
        layer_direction(&ranks, "cli", "store"),
        Some(Direction::Downward)
    );
    assert_eq!(layer_direction(&ranks, "remote", "store"), None);
}

#[test]
fn tighten_lowers_counts_drops_unused_admissions_and_preserves_verdicts() {
    let verdict = Verdict {
        kind: VerdictKind::Item,
        key: "store::open".to_owned(),
        reason: "intentional boundary".to_owned(),
    };
    let mut target = Target {
        version: 5,
        layers: Vec::new(),
        modules: vec![ModuleRule {
            path: PathBuf::from("src/store"),
            allowed_dependencies: None,
            upward_dependencies: Some(vec!["cli".to_owned(), "agents".to_owned()]),
            surface_budget: 10,
            config_line: 2,
        }],
        strangler: vec![StranglerRule {
            symbol: "legacy".to_owned(),
            path: PathBuf::from("src/store"),
            baseline: 5,
            config_line: 8,
        }],
        verdicts: vec![verdict.clone()],
    };
    let report = Report {
        target: PathBuf::from("target.toml"),
        layers: Vec::new(),
        rules: vec![
            RuleResult {
                kind: "upward-dependency",
                path: PathBuf::from("src/store"),
                symbol: None,
                current: 3,
                budget: 10,
                unallowed_dependencies: Vec::new(),
                unallowed_dependency_sites: Vec::new(),
                used_dependencies: BTreeSet::from(["cli".to_owned()]),
                config_line: 2,
            },
            RuleResult {
                kind: "strangler",
                path: PathBuf::from("src/store"),
                symbol: Some("legacy".to_owned()),
                current: 1,
                budget: 5,
                unallowed_dependencies: Vec::new(),
                unallowed_dependency_sites: Vec::new(),
                used_dependencies: BTreeSet::new(),
                config_line: 8,
            },
        ],
        parse_failure_paths: Vec::new(),
    };

    tighten(&mut target, &report);

    assert_eq!(target.modules[0].surface_budget, 3);
    assert_eq!(
        target.modules[0].upward_dependencies.as_deref(),
        Some(&["cli".to_owned()][..])
    );
    assert_eq!(target.strangler[0].baseline, 1);
    assert_eq!(target.verdicts, [verdict]);
}

#[test]
fn count_in_sources_excludes_inline_test_regions() {
    let source = Source::new(
        "src/lib.rs",
        "fn legacy() {}\n#[cfg(test)]\nmod tests { fn check() { legacy(); } }\n",
    );
    let syntax = syntax::analyze_sources(std::slice::from_ref(&source), &BTreeSet::new());

    assert_eq!(count_in_sources(&[source], &syntax.files, "legacy"), 1);
}

fn fixture_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    root
}

fn module_rule(path: &str) -> ModuleRule {
    ModuleRule {
        path: PathBuf::from(path),
        allowed_dependencies: None,
        upward_dependencies: None,
        surface_budget: 10,
        config_line: 1,
    }
}
