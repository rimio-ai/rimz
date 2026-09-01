use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

mod goals_and_layers {
    use super::*;

    #[test]
    fn peers_in_one_layer_group_are_not_upward() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/lib.rs"),
            "mod cli;\nmod config;\nmod store;\n",
        )
        .unwrap();
        fs::write(root.path().join("src/cli.rs"), "pub struct High;\n").unwrap();
        fs::write(root.path().join("src/config.rs"), "pub struct Peer;\n").unwrap();
        fs::write(
        root.path().join("src/store.rs"),
        "use crate::cli::High;\nuse crate::config::Peer;\nfn load() { let _ = (High, Peer); }\n",
    )
    .unwrap();
        let target = Target {
            version: 4,
            layers: vec![
                Layer::Group(vec!["store".to_owned(), "config".to_owned()]),
                Layer::Module("cli".to_owned()),
            ],
            modules: vec![ModuleRule {
                path: PathBuf::from("src/store.rs"),
                allowed_imports: None,
                upward_imports: None,
                surface_budget: 0,
                surface_goal: None,
                upward_debt: None,
                config_line: 1,
            }],
            strangler: Vec::new(),
        };

        let report = evaluate(
            root.path(),
            &target,
            &root.path().join("target.toml"),
            false,
            Mode::Report,
        )
        .unwrap();

        assert_eq!(report.rules[0].unallowed_imports, ["cli"]);
        assert_eq!(
            status_rules(&report, false)
                .iter()
                .map(|rule| rule.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("src/store.rs")]
        );
    }

    #[test]
    fn status_reports_remaining_distance_and_open_debt() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/lib.rs"),
            "pub mod agents;\npub mod cli;\npub mod store;\n",
        )
        .unwrap();
        fs::write(root.path().join("src/agents.rs"), "pub struct Future;\n").unwrap();
        fs::write(
            root.path().join("src/cli.rs"),
            "pub struct Other;\npub struct Thing;\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/store.rs"),
            "use crate::cli::Other;\nuse crate::cli::Thing;\npub fn visible() -> Thing { Thing }\n",
        )
        .unwrap();
        let target_path = root.path().join("goal.toml");
        fs::write(
            &target_path,
            r#"version = 4
layers = ["store", "agents", "cli"]

[[module]]
path = "src/store.rs"
upward-imports = ["cli", "agents"]
surface-budget = 5
surface-goal = 0
upward-debt = ["cli", "agents"]

[[strangler]]
symbol = "visible"
path = "src/store.rs"
baseline = 1
"#,
        )
        .unwrap();
        let target = target::load(&target_path).unwrap().unwrap();

        let before = evaluate(root.path(), &target, &target_path, false, Mode::Status).unwrap();
        assert_eq!(before.rules[0].debt[0].sites, 2);
        fs::write(
            root.path().join("src/store.rs"),
            "use crate::cli::Thing;\npub fn visible() -> Thing { Thing }\n",
        )
        .unwrap();
        let report = evaluate(root.path(), &target, &target_path, false, Mode::Status).unwrap();
        let module = &report.rules[0];
        assert_eq!(report.mode, "status");
        assert_eq!(module.goal, Some(0));
        assert_eq!(module.remaining, Some(1));
        assert_eq!(
            module
                .debt
                .iter()
                .map(|debt| (debt.prefix.as_str(), debt.sites, debt.open))
                .collect::<Vec<_>>(),
            [("cli", 1, true), ("agents", 0, false)]
        );
        assert_eq!(report.rules[1].goal, Some(0));
        assert_eq!(report.rules[1].remaining, Some(1));
        assert_eq!(
            status_summary(&report),
            "summary: 1 module rules with goals, 1 with debt, 1 stranglers; remaining surface 1; strangler occurrences 1; open debt 1/2 (1 sites); 0 regressions; 0 parse failures"
        );
    }

    #[test]
    fn status_summary_reports_parse_failures() {
        let mut report = tighten_report(2, None, BTreeSet::new());
        report.parse_failures = 2;

        assert_eq!(
            status_summary(&report),
            "summary: 0 module rules with goals, 0 with debt, 0 stranglers; remaining surface 0; strangler occurrences 0; open debt 0/0 (0 sites); 0 regressions; 2 parse failures"
        );
    }

    #[test]
    fn tighten_retires_closed_debt_with_its_admission() {
        let mut target = target_with_goal_and_debt();
        let report = tighten_report(3, Some(2), BTreeSet::from(["cli".to_owned()]));

        tighten(&mut target, &report);

        assert_eq!(target.modules[0].surface_budget, 3);
        assert_eq!(
            target.modules[0].upward_imports.as_deref(),
            Some(&["cli".to_owned()][..])
        );
        assert_eq!(
            target.modules[0].upward_debt.as_deref(),
            Some(&["cli".to_owned()][..])
        );
        assert_eq!(target.modules[0].surface_goal, Some(2));
    }

    #[test]
    fn tighten_retires_a_met_goal_with_the_budget_it_lowers() {
        let mut target = target_with_goal_and_debt();
        target.modules[0].surface_goal = Some(3);
        let report = tighten_report(2, Some(3), BTreeSet::new());

        tighten(&mut target, &report);

        assert_eq!(target.modules[0].surface_budget, 2);
        assert_eq!(target.modules[0].surface_goal, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.toml");
        target::write(&path, &target).unwrap();
        assert!(target::load(&path).unwrap().is_some());
    }

    fn target_with_goal_and_debt() -> Target {
        Target {
            version: 4,
            layers: vec![
                Layer::Module("store".to_owned()),
                Layer::Module("cli".to_owned()),
            ],
            modules: vec![ModuleRule {
                path: PathBuf::from("src/store"),
                allowed_imports: None,
                upward_imports: Some(vec!["cli".to_owned(), "agents".to_owned()]),
                surface_budget: 4,
                surface_goal: Some(2),
                upward_debt: Some(vec!["cli".to_owned(), "agents".to_owned()]),
                config_line: 2,
            }],
            strangler: Vec::new(),
        }
    }

    fn tighten_report(current: usize, goal: Option<usize>, used: BTreeSet<String>) -> Report {
        let used_upward_imports = used
            .into_iter()
            .map(|prefix| (prefix, BTreeSet::from([(PathBuf::new(), 0)])))
            .collect();
        Report {
            version: REPORT_VERSION,
            verb: "conform",
            mode: "tighten",
            target: PathBuf::from("target.toml"),
            default_target: false,
            layers: Vec::new(),
            rules: vec![RuleResult {
                kind: "upward-import",
                path: PathBuf::from("src/store"),
                symbol: None,
                status: if goal.is_some_and(|goal| current <= goal) {
                    "met"
                } else {
                    "ok"
                },
                current,
                budget: 4,
                delta: current as isize - 4,
                goal,
                remaining: goal.map(|goal| current.saturating_sub(goal)),
                debt: Vec::new(),
                unallowed_imports: Vec::new(),
                unallowed_import_sites: Vec::new(),
                used_upward_imports,
                config_line: 2,
            }],
            regressions: 0,
            parse_failures: 0,
            parse_failure_paths: Vec::new(),
        }
    }
}

mod baseline_and_ratchet {
    use super::*;

    #[test]
    fn conform_args_parse_status_and_grouped_layers() {
        assert_eq!(
            parse_args(&[]).unwrap(),
            Some(Args {
                mode: Mode::Report,
                file: None,
                path: None,
                layers: None,
                verbose: false,
                json: false,
            })
        );
        assert_eq!(
            parse_args(&["--ratchet".into()]).unwrap(),
            Some(Args {
                mode: Mode::Ratchet,
                file: None,
                path: None,
                layers: None,
                verbose: false,
                json: false,
            })
        );
        assert!(parse_args(&["--ratchet".into(), "--tighten".into()]).is_err());
        assert_eq!(
            parse_args(&["--status".into(), "--verbose".into()])
                .unwrap()
                .unwrap()
                .mode,
            Mode::Status
        );
        assert!(parse_args(&["--status".into(), "--json".into()]).is_ok());
        assert_eq!(
            parse_args(&[
                "--init".into(),
                "--layers".into(),
                "ids+utils, store + config, cli".into(),
            ])
            .unwrap()
            .unwrap()
            .layers,
            Some(vec![
                Layer::Group(vec!["ids".to_owned(), "utils".to_owned()]),
                Layer::Group(vec!["store".to_owned(), "config".to_owned()]),
                Layer::Module("cli".to_owned()),
            ])
        );
        assert!(parse_args(&["--init".into(), "--layers".into(), "ids+,cli".into()]).is_err());
        assert_eq!(
            parse_args(&["--file".into(), "targets/cli.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("targets/cli.toml"))
        );
        assert_eq!(
            parse_args(&["--file".into(), "/tmp/target.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("/tmp/target.toml"))
        );
        assert_eq!(
            parse_args(&["--file".into(), "../target.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("../target.toml"))
        );
        assert!(parse_args(&["--file".into(), String::new()]).is_err());
        assert!(parse_args(&["--path".into(), "src".into()]).is_err());
        assert!(parse_args(&["--ratchet".into(), "--verbose".into()]).is_err());
        assert!(parse_args(&["--json".into(), "--verbose".into()]).is_err());
        assert!(parse_args(&["--verbose".into()]).unwrap().unwrap().verbose);
        assert_eq!(
            parse_args(&["--init".into(), "--path".into(), "src".into()])
                .unwrap()
                .unwrap()
                .path,
            Some(PathBuf::from("src"))
        );
    }

    #[test]
    fn allow_list_matches_descendants_not_prefix_collisions() {
        assert!(module_is_within("cli::render::table", "cli::render"));
        assert!(!module_is_within("cli::renderer", "cli::render"));
    }

    #[test]
    fn layer_direction_classifies_upward_same_downward_and_unknown() {
        let layers = vec![
            Layer::Module("store".to_owned()),
            Layer::Group(vec!["agents".to_owned(), "harness".to_owned()]),
            Layer::Module("cli".to_owned()),
        ];
        let ranks = LayerRanks::new(&layers);

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
    fn surface_budget_counts_only_reach_outside_the_rule_module() {
        let sources = vec![
            Source::new("src/lib.rs", "mod feature;\n"),
            Source::new(
                "src/feature/mod.rs",
                "mod detail;\npub(in crate) fn crate_wide() {}\n",
            ),
            Source::new(
                "src/feature/detail.rs",
                "pub(super) fn sibling_only() {}\npub fn behind_private_link() {}\n",
            ),
        ];
        let syntax = syntax::analyze_sources(&sources);
        let index = syntax::ModIndex::new(&syntax.files);
        let feature_files = syntax
            .files
            .iter()
            .filter(|file| file.module_path.starts_with("feature"));
        assert_eq!(escaping_surface(feature_files, "feature", &index), 1);
    }

    #[test]
    fn crate_root_surface_counts_only_crate_external_reach() {
        let sources = vec![Source::new(
            "src/lib.rs",
            "pub fn external() {}\npub(crate) fn crate_only() {}\n",
        )];
        let syntax = syntax::analyze_sources(&sources);
        let index = syntax::ModIndex::new(&syntax.files);

        assert_eq!(escaping_surface(&syntax.files, "", &index), 1);
    }

    #[test]
    fn conform_default_report_prioritizes_regressions_and_headroom() {
        let rule = |path: &str, status, current, budget| RuleResult {
            kind: "module",
            path: PathBuf::from(path),
            symbol: None,
            status,
            current,
            budget,
            delta: current as isize - budget as isize,
            goal: None,
            remaining: None,
            debt: Vec::new(),
            unallowed_imports: Vec::new(),
            unallowed_import_sites: Vec::new(),
            used_upward_imports: BTreeMap::new(),
            config_line: 1,
        };
        let report = Report {
            version: REPORT_VERSION,
            verb: "conform",
            mode: "report",
            target: PathBuf::from("target.toml"),
            default_target: false,
            layers: Vec::new(),
            rules: vec![
                rule("at-budget", "ok", 2, 2),
                rule("headroom", "ok", 1, 3),
                rule("regression", "regression", 4, 2),
            ],
            regressions: 1,
            parse_failures: 0,
            parse_failure_paths: Vec::new(),
        };

        let (displayed, folded) = displayed_rules(&report, false);
        assert_eq!(
            displayed
                .iter()
                .map(|rule| rule.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("regression"), Path::new("headroom")]
        );
        assert_eq!(folded, 1);
        assert_eq!(displayed_rules(&report, true).0.len(), 3);
    }

    #[test]
    fn configured_target_ratchets_and_tightens_without_subprocesses() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("src/nested")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n[package]\nname = \"probe\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/lib.rs"),
            "use probe::other::Thing;\npub fn run() -> Thing { Thing }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/other.rs"),
            "pub struct Thing;\nfn caller() { let _ = crate::run(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/nested/mod.rs"),
            "use probe::other::Thing;\npub fn nested() -> Thing { Thing }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/tests.rs"),
            "fn characterization() { crate::run(); crate::run(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join(TARGET_FILE),
            r#"
version = 4
layers = []
[[module]]
path = "src/nested"
allowed-imports = ["other"]
surface-budget = 5
[[strangler]]
symbol = "run"
path = "src/lib.rs"
baseline = 5
[[strangler]]
symbol = "run"
path = "src"
baseline = 5
"#,
        )
        .unwrap();

        ratchet(root.path()).unwrap();
        let target_path = root.path().join(TARGET_FILE);
        let mut configured = target::load(&target_path).unwrap().unwrap();
        let mut forbidden = configured.clone();
        forbidden.modules[0]
            .allowed_imports
            .as_mut()
            .unwrap()
            .clear();
        let forbidden_report =
            evaluate(root.path(), &forbidden, &target_path, true, Mode::Report).unwrap();
        assert_eq!(forbidden_report.regressions, 1);
        assert_eq!(forbidden_report.rules[0].unallowed_imports, ["other"]);
        assert_eq!(
            forbidden_report.rules[0].unallowed_import_sites,
            [ImportSite {
                module: "other".to_owned(),
                path: PathBuf::from("src/nested/mod.rs"),
                line: 1,
            }]
        );
        assert!(enforce(&forbidden_report).is_err());

        let report = evaluate(root.path(), &configured, &target_path, true, Mode::Report).unwrap();
        tighten(&mut configured, &report);
        target::write(&target_path, &configured).unwrap();
        let tightened = target::load(&target_path).unwrap().unwrap();
        assert_eq!(tightened.modules[0].surface_budget, 1);
        assert_eq!(tightened.strangler[0].baseline, 1);
        assert_eq!(tightened.strangler[1].baseline, 2);

        let target_directory = tempfile::tempdir().unwrap();
        let initialized_path = target_directory.path().join("initialized.toml");
        let initialized_arg = initialized_path.display().to_string();
        run(
            root.path(),
            &[
                "--init".into(),
                "--path".into(),
                "src".into(),
                "--file".into(),
                initialized_arg.clone(),
            ],
        )
        .unwrap();
        let initialized = target::load(&initialized_path).unwrap().unwrap();
        let initialized_report = evaluate(
            root.path(),
            &initialized,
            &initialized_path,
            false,
            Mode::Report,
        )
        .unwrap();
        assert_eq!(initialized_report.regressions, 0);
        assert_eq!(initialized.modules.len(), 3);
        assert!(initialized.modules[0].allowed_imports.is_none());
        assert_eq!(initialized.modules[1].path, Path::new("src/nested"));
        assert!(initialized.modules[1].upward_imports.is_none());
        let before_tighten = fs::read_to_string(&initialized_path).unwrap();
        run(
            root.path(),
            &["--tighten".into(), "--file".into(), initialized_arg.clone()],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&initialized_path).unwrap(),
            before_tighten
        );
        assert!(
            run(
                root.path(),
                &["--init".into(), "--file".into(), initialized_arg],
            )
            .is_err()
        );
    }

    #[test]
    fn tighten_refuses_when_a_scoped_file_does_not_parse() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.path().join("src/lib.rs"), "mod broken;\n").unwrap();
        fs::write(root.path().join("src/broken.rs"), "pub fn broken(\n").unwrap();
        let target_path = root.path().join("target.toml");
        fs::write(
            &target_path,
            r#"version = 4
layers = []

[[module]]
path = "src/broken.rs"
allowed-imports = []
surface-budget = 5
"#,
        )
        .unwrap();
        let before = fs::read_to_string(&target_path).unwrap();

        let error = run(
            root.path(),
            &["--tighten".into(), "--file".into(), "target.toml".into()],
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("src/broken.rs"));
        assert!(message.contains("Repair the file, then tighten"));
        assert_eq!(fs::read_to_string(target_path).unwrap(), before);
    }

    #[test]
    fn uncovered_layered_file_rejects_upward_imports() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.path().join("src/lib.rs"), "mod cli;\nmod store;\n").unwrap();
        fs::write(root.path().join("src/cli.rs"), "pub struct Report;\n").unwrap();
        fs::write(
            root.path().join("src/store.rs"),
            "use crate::cli::Report;\nfn load() -> Report { Report }\n",
        )
        .unwrap();
        let target = Target {
            version: 4,
            layers: vec![
                Layer::Module("store".to_owned()),
                Layer::Module("cli".to_owned()),
            ],
            modules: vec![ModuleRule {
                path: PathBuf::from("src/cli.rs"),
                allowed_imports: None,
                upward_imports: None,
                surface_budget: 1,
                surface_goal: None,
                upward_debt: None,
                config_line: 1,
            }],
            strangler: Vec::new(),
        };

        let report = evaluate(
            root.path(),
            &target,
            &root.path().join("target.toml"),
            false,
            Mode::Report,
        )
        .unwrap();
        let uncovered = report
            .rules
            .iter()
            .find(|rule| rule.path == Path::new("src/store.rs"))
            .unwrap();

        assert_eq!(uncovered.status, "regression");
        assert_eq!(uncovered.unallowed_imports, ["cli"]);
        assert_eq!(
            uncovered.unallowed_import_sites,
            [ImportSite {
                module: "cli".to_owned(),
                path: PathBuf::from("src/store.rs"),
                line: 1,
            }]
        );
    }

    #[test]
    fn strangler_paths_do_not_count_matching_modules_from_other_crates() {
        let root = tempfile::tempdir().unwrap();
        for path in ["app/src/legacy", "tool/src/legacy"] {
            fs::create_dir_all(root.path().join(path)).unwrap();
        }
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"tool\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        for member in ["app", "tool"] {
            fs::write(
                root.path().join(member).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"
                ),
            )
            .unwrap();
        }
        fs::write(root.path().join("app/src/lib.rs"), "pub mod legacy;\n").unwrap();
        fs::write(
            root.path().join("app/src/legacy/mod.rs"),
            "fn doomed() {}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/legacy/caller.rs"),
            "fn call() { doomed(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tool/src/lib.rs"),
            "fn doomed() { doomed(); doomed(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tool/src/legacy/mod.rs"),
            "fn doomed() { doomed(); }\n",
        )
        .unwrap();

        let target = Target {
            version: 4,
            layers: Vec::new(),
            modules: Vec::new(),
            strangler: vec![
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src"),
                    baseline: 10,
                    config_line: 1,
                },
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src/legacy"),
                    baseline: 10,
                    config_line: 2,
                },
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src/lib.rs"),
                    baseline: 10,
                    config_line: 3,
                },
            ],
        };

        let report = evaluate(
            root.path(),
            &target,
            &root.path().join("custom-target.toml"),
            false,
            Mode::Report,
        )
        .unwrap();
        assert_eq!(
            report
                .rules
                .iter()
                .map(|rule| rule.current)
                .collect::<Vec<_>>(),
            [2, 2, 0]
        );
    }

    #[test]
    fn missing_explicit_target_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let error = run(
            root.path(),
            &["--file".into(), "missing-target.toml".into()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing-target.toml"));
    }
}
