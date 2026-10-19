use std::fs;

use super::super::target::{ModuleRule, StranglerRule, Verdict, VerdictKind};
use super::*;

#[test]
fn conform_flag_errors_are_stable() {
    let cases: &[(&[&str], String)] = &[
        (
            &["--only"],
            "atlas conform --only requires a value".to_owned(),
        ),
        (
            &["--ratchet", "--ratchet"],
            "atlas conform --ratchet and --tighten are mutually exclusive".to_owned(),
        ),
        (
            &["--ratchet", "--tighten"],
            "atlas conform --ratchet and --tighten are mutually exclusive".to_owned(),
        ),
        (
            &["--only", "x"],
            "atlas conform --only requires --tighten".to_owned(),
        ),
        (
            &["--wat"],
            "unknown atlas conform argument `--wat`".to_owned(),
        ),
    ];
    for (raw, expected) in cases {
        let args = raw.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_args(&args).unwrap_err().to_string(),
            *expected,
            "{raw:?}"
        );
    }
    for raw in [
        vec!["--wat", "--help"],
        vec!["--help", "--wat"],
        vec!["--path", "--help"],
        vec!["--wat", "-h"],
    ] {
        let args = raw.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert!(parse_args(&args).unwrap().is_none(), "{raw:?}");
    }
}

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
    let mut rule = module_rule("src/lower.rs");
    rule.surface_budget = 0;
    let target = Target {
        version: 5,
        layers: vec![vec!["lower".to_owned()], vec!["upper".to_owned()]],
        modules: vec![rule],
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
    assert!(
        error.contains("with\n[[module]]\npath = \"src/lower.rs\""),
        "{error}"
    );
    assert!(
        error.contains("upward-dependencies = [\"upper\"]"),
        "{error}"
    );
    assert!(error.contains("surface-budget = 0"), "{error}");
    assert_eq!(report.rules[0].unallowed_dependencies, ["upper"]);
}

#[test]
fn conform_labels_and_fixes_surface_and_strangler_excesses() {
    let root = fixture_root();
    fs::write(root.path().join("src/lib.rs"), "mod upper;\n").unwrap();
    fs::write(root.path().join("src/upper.rs"), "pub fn open() {}\n").unwrap();
    let mut rule = module_rule("src/upper.rs");
    rule.surface_budget = 0;
    let target = Target {
        version: 5,
        layers: Vec::new(),
        modules: vec![rule],
        strangler: vec![StranglerRule {
            symbol: "open".to_owned(),
            path: PathBuf::from("src/upper.rs"),
            baseline: 0,
            config_line: 2,
        }],
        verdicts: Vec::new(),
    };
    let target_path = root.path().join(TARGET_FILE);

    let report = evaluate(root.path(), &target, &target_path).unwrap();
    let error = enforce(&report).unwrap_err().to_string();

    assert!(
        error.contains("surface of `src/upper.rs` is 1 above 0"),
        "{error}"
    );
    assert!(error.contains("surface-budget = 1"), "{error}");
    assert!(
        error.contains("strangler `open` in `src/upper.rs` is 1 above 0"),
        "{error}"
    );
    assert!(
        error.contains("[[strangler]]\nsymbol = \"open\"\npath = \"src/upper.rs\"\nbaseline = 1"),
        "{error}"
    );
    assert!(!error.contains("upward-dependency"), "{error}");
    assert!(!error.contains("upward-dependencies"), "{error}");
}

#[test]
fn conform_fixes_an_implicit_rule_at_its_measured_surface() {
    let root = fixture_root();
    fs::write(root.path().join("src/lib.rs"), "mod lower;\nmod upper;\n").unwrap();
    fs::write(
        root.path().join("src/lower.rs"),
        "pub fn call() { crate::upper::f(); }\n",
    )
    .unwrap();
    fs::write(root.path().join("src/upper.rs"), "pub fn f() {}\n").unwrap();
    let target = Target {
        version: 5,
        layers: vec![vec!["lower".to_owned()], vec!["upper".to_owned()]],
        modules: Vec::new(),
        strangler: Vec::new(),
        verdicts: Vec::new(),
    };
    let target_path = root.path().join(TARGET_FILE);

    let report = evaluate(root.path(), &target, &target_path).unwrap();
    let error = enforce(&report).unwrap_err().to_string();

    assert!(
        error.contains(&format!("fix: add to {}", target_path.display())),
        "{error}"
    );
    assert!(error.contains("path = \"src/lower.rs\""), "{error}");
    assert!(error.contains("surface-budget = 1"), "{error}");
    assert_eq!((report.rules[0].current, report.rules[0].budget), (0, 0));
    assert!(
        error.contains("upward-dependencies = [\"upper\"]"),
        "{error}"
    );
}

#[test]
fn admission_modes_judge_downward_and_upward_sites() {
    let root = fixture_root();
    for (name, text) in [
        ("lib", "mod low; mod middle; mod high;"),
        ("low", "pub fn f() {}"),
        ("middle", "fn f() { crate::low::f(); crate::high::f(); }"),
        ("high", "pub fn f() {}"),
    ] {
        fs::write(root.path().join(format!("src/{name}.rs")), text).unwrap();
    }
    let mut rule = module_rule("src/middle.rs");
    rule.allowed_dependencies = Some(vec![]);
    let mut target = Target {
        version: 5,
        layers: vec![
            vec!["low".into()],
            vec!["middle".into()],
            vec!["high".into()],
        ],
        modules: vec![rule],
        strangler: vec![],
        verdicts: vec![],
    };
    let path = root.path().join(TARGET_FILE);
    let report = evaluate(root.path(), &target, &path).unwrap();
    assert_eq!(report.rules[0].unallowed_dependencies, ["high", "low"]);
    assert!(enforce(&report).is_err());
    target.modules[0].allowed_dependencies = None;
    target.modules[0].upward_dependencies = Some(vec![]);
    let report = evaluate(root.path(), &target, &path).unwrap();
    assert_eq!(report.rules[0].unallowed_dependencies, ["high"]);
}

#[test]
fn tighten_refuses_parse_failures_and_missing_paths_name_config_line() {
    let root = fixture_root();
    fs::write(root.path().join("src/lib.rs"), "").unwrap();
    let path = root.path().join(TARGET_FILE);
    let raw = "version = 5\nlayers = []\n[[module]]\npath = 'src/broken.rs'\nsurface-budget = 10\n";
    fs::write(&path, raw).unwrap();
    let error = run(root.path(), &[]).unwrap_err().to_string();
    assert!(
        error.contains(&format!("{}:3: configured module path", path.display())),
        "{error}"
    );
    fs::write(root.path().join("src/broken.rs"), "fn broken(").unwrap();
    let error = run(root.path(), &["--tighten".into()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot measure configured rules"), "{error}");
    assert!(error.contains("src/broken.rs"), "{error}");
    assert_eq!(fs::read_to_string(path).unwrap(), raw);
}

#[test]
fn stranglers_exclude_directory_entry_and_keep_file_scope_exact() {
    let root = fixture_root();
    fs::create_dir(root.path().join("src/feature")).unwrap();
    for (path, text) in [
        ("src/lib.rs", "mod feature;"),
        ("src/feature.rs", "fn legacy() { legacy(); legacy(); }"),
        ("src/feature/detail.rs", "fn legacy() { legacy(); }"),
        ("src/featured.rs", "fn legacy() {}"),
    ] {
        fs::write(root.path().join(path), text).unwrap();
    }
    let target = Target {
        version: 5,
        layers: vec![],
        modules: vec![],
        strangler: ["src/feature", "src/feature.rs"]
            .map(|path| StranglerRule {
                path: path.into(),
                symbol: "legacy".into(),
                baseline: 0,
                config_line: 1,
            })
            .to_vec(),
        verdicts: vec![],
    };
    let report = evaluate(root.path(), &target, &root.path().join(TARGET_FILE)).unwrap();
    assert_eq!(
        report
            .rules
            .iter()
            .map(|rule| rule.current)
            .collect::<Vec<_>>(),
        [2, 3]
    );
}

#[test]
fn conform_directory_rule_covers_sibling_file() {
    let boundary = Boundary::new(Path::new("src/feature"));
    let sibling = Path::new("src/feature.rs");

    assert!(boundary.covers(sibling));
    assert!(boundary.covers(Path::new("src/feature/detail.rs")));
    assert!(!boundary.covers(Path::new("src/featured.rs")));
}

#[test]
fn conform_args_only_accept_report_ratchet_and_tighten() {
    assert_eq!(
        parse_args(&[]).unwrap(),
        Some(Args {
            mode: Mode::Report,
            only: BTreeSet::new()
        })
    );
    assert_eq!(
        parse_args(&["--ratchet".into()]).unwrap(),
        Some(Args {
            mode: Mode::Ratchet,
            only: BTreeSet::new(),
        })
    );
    assert_eq!(
        parse_args(&["--tighten".into()]).unwrap(),
        Some(Args {
            mode: Mode::Tighten,
            only: BTreeSet::new(),
        })
    );
    for removed in ["--status", "--init", "--json", "--file"] {
        assert!(parse_args(&[removed.to_owned()]).is_err());
    }
    assert_eq!(
        parse_args(&["--only", "a/", "--tighten", "--only", "b", "--only", "a"].map(String::from))
            .unwrap(),
        Some(Args {
            mode: Mode::Tighten,
            only: BTreeSet::from([PathBuf::from("a"), PathBuf::from("b")]),
        })
    );
    for args in [
        vec!["--only", "x"],
        vec!["--ratchet", "--only", "x"],
        vec!["--tighten", "--only"],
        vec!["--tighten", "--ratchet"],
        vec!["--tighten", "--only", "../x"],
        vec!["--tighten", "--only", "/x"],
    ] {
        assert!(parse_args(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
    }
}

#[test]
fn conform_tighten_only_preserves_unselected_rules_and_selects_stranglers() {
    let root = fixture_root();
    fs::write(root.path().join("src/lib.rs"), "mod lower;\nmod upper;\n").unwrap();
    fs::write(root.path().join("src/lower.rs"), "pub fn old() {}\n").unwrap();
    fs::write(root.path().join("src/upper.rs"), "pub fn legacy() {}\n").unwrap();
    let path = root.path().join(TARGET_FILE);
    let raw = r#"version = 5
layers = []

[[module]]
path = "src/lower.rs"
upward-dependencies = ["unused"]
# keep this budget comment
surface-budget = 10

[[module]]
path = 'src/upper.rs'
allowed-dependencies = [ 'unused' ]
surface-budget = 20 # another pass owns this

[[strangler]]
path = 'src/upper.rs'
symbol = 'legacy'
baseline = 30
"#;
    fs::write(&path, raw).unwrap();

    run(
        root.path(),
        &["--tighten", "--only", "src/lower.rs"].map(String::from),
    )
    .unwrap();
    let expected = raw
        .replace("upward-dependencies = [\"unused\"]\n", "")
        .replace("surface-budget = 10", "surface-budget = 1");
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    ratchet(root.path()).unwrap();

    run(
        root.path(),
        &["--tighten", "--only", "src/upper.rs"].map(String::from),
    )
    .unwrap();
    let target = target::load(&path).unwrap().unwrap();
    assert_eq!(target.modules[1].surface_budget, 1);
    assert_eq!(target.modules[1].allowed_dependencies, Some(Vec::new()));
    assert_eq!(target.strangler[0].baseline, 1);
    ratchet(root.path()).unwrap();
}

#[test]
fn conform_only_rejects_unknown_paths_before_measuring_or_writing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(TARGET_FILE);
    let args = ["--tighten", "--only", "src/typo", "--only", "src/other"].map(String::from);
    assert!(
        run(root.path(), &args)
            .unwrap_err()
            .to_string()
            .contains("requires refactor-target.toml")
    );
    let raw = "version = 5\nlayers = []\n[[module]]\npath = 'missing'\nsurface-budget = 10\n";
    fs::write(&path, raw).unwrap();

    let error = run(root.path(), &args).unwrap_err().to_string();

    assert_eq!(
        error,
        "atlas conform --only names no rule in refactor-target.toml:\n  src/other\n  src/typo"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), raw);
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
                path: PathBuf::from("src/store"),
                symbol: None,
                current: 3,
                budget: 10,
                unallowed_dependencies: Vec::new(),
                unallowed_dependency_sites: Vec::new(),
                used_dependencies: BTreeSet::from(["cli".to_owned()]),
                config_line: Some(2),
                fix: None,
            },
            RuleResult {
                path: PathBuf::from("src/store"),
                symbol: Some("legacy".to_owned()),
                current: 1,
                budget: 5,
                unallowed_dependencies: Vec::new(),
                unallowed_dependency_sites: Vec::new(),
                used_dependencies: BTreeSet::new(),
                config_line: Some(8),
                fix: None,
            },
        ],
        parse_failure_paths: Vec::new(),
    };

    tighten(&mut target, &report, &BTreeSet::new());

    assert_eq!(target.modules[0].surface_budget, 3);
    assert_eq!(
        target.modules[0].upward_dependencies.as_deref(),
        Some(&["cli".to_owned()][..])
    );
    assert_eq!(target.strangler[0].baseline, 1);
    assert_eq!(target.verdicts, [verdict]);
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
