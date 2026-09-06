use super::super::detect::GuardSite;
use super::super::shapes::Member;
use super::super::sources::Source;
use super::*;

#[test]
fn assemblers_count_distinct_scope_modules_a_function_calls_into() {
    let report = super::super::syntax::analyze_sources(
        &[Source::new(
            "crates/demo/src/cli.rs",
            "use crate::agents;\nuse crate::config::Config;\nuse crate::store::open;\nfn run() {\n    open();\n    agents::list();\n    agents::catalog::load();\n    Config::load();\n    crate::mux::attach();\n    crate::Paths::load();\n    self::helper();\n    value.finish();\n}\nfn light() { open(); Config::load(); }\nfn helper() {}\n",
        )],
        &BTreeSet::new(),
    );
    let known = ["cli", "agents", "agents::catalog", "config", "store", "mux"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    let rows = assemblers(
        &report.files,
        &known,
        &BTreeSet::new(),
        Path::new("crates/demo/src"),
    );

    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].function, "run");
    assert_eq!(rows[0].callees, 6);
    assert_eq!(
        rows[0]
            .providers
            .iter()
            .map(|provider| (provider.provider.as_str(), provider.sites))
            .collect::<Vec<_>>(),
        [
            ("agents", 2),
            ("(root)", 1),
            ("config", 1),
            ("mux", 1),
            ("store", 1)
        ]
    );
}

#[test]
fn survey_output_is_bounded_by_top() {
    let rows = (0..30)
        .map(|index| Row {
            module: format!("module-{index}"),
            code: index,
            ..Row::default()
        })
        .collect::<Vec<_>>();
    let shapes = (0..30)
        .map(|index| ShapeFamily {
            name: format!("shape-{index}"),
            members: vec![Member {
                path: PathBuf::from(format!("src/shape-{index}.rs")),
                line: 1,
                name: "work".to_owned(),
                sloc: 40,
            }],
            files: 1,
            mean_sloc: 40.0,
            sloc_in_play: 40.0,
            score: 40.0,
            siblings: 0,
            role: None,
            provider: None,
        })
        .collect();
    let guards = (0..30)
        .map(|index| GuardFamily {
            key: format!("guard-{index}"),
            files: 3,
            sites: 3,
            locations: vec![GuardSite {
                path: PathBuf::from(format!("src/guard-{index}.rs")),
                line: 1,
                kind: "if".to_owned(),
            }],
        })
        .collect();
    let hot = (0..30)
        .map(|index| Hotspot {
            function: format!("hot-{index}"),
            path: PathBuf::from(format!("src/hot-{index}.rs")),
            line: 1,
            cx: 1.0,
            churn: 1.0,
            hot: 1.0,
        })
        .collect();
    let report = Report {
        path: PathBuf::from("src"),
        probes: Vec::new(),
        totals: rank::totals(&rows),
        rows,
        hot,
        assemblers: Vec::new(),
        debt: Debt {
            configured: true,
            rules: (0..30)
                .map(|index| DebtRow {
                    path: PathBuf::from(format!("src/rule-{index}")),
                    upward_sites: 30 - index,
                    reviewed: vec![ReviewedSites {
                        provider: "cli".to_owned(),
                        sites: 30 - index - 1,
                        intent: "keep".to_owned(),
                    }],
                    unreviewed: vec![ProviderSites {
                        provider: "cli::render".to_owned(),
                        sites: 1,
                    }],
                    unadmitted: Vec::new(),
                })
                .collect(),
            stranglers: vec![StranglerRow {
                path: PathBuf::from("src/store"),
                symbol: "legacy_open".to_owned(),
                current: 2,
                baseline: 3,
            }],
        },
        cycles: vec![Cycle {
            a: "(crate)".to_owned(),
            b: "harness".to_owned(),
            a_to_b: 2,
            b_to_a: 20,
            same_layer: None,
        }],
        shapes,
        guards,
        history_commits: 100,
        pace_window: 25,
        parse_failures: 0,
        shape_families_dropped: shapes::FamilyDrops {
            vocabulary: 6,
            below_gate: 3,
            single_provider: 2,
        },
        guard_families_dropped: detect::GuardDrops {
            vocabulary: 4,
            predicate_use: 2,
        },
        suppressed: 0,
        stale: (0..30)
            .map(|index| format!("shape:stale-{index}"))
            .collect(),
        ledger: LedgerNote {
            present: true,
            intents: 54,
            holds: 1,
            problems: (0..30)
                .map(|index| format!("row {index} is unreadable"))
                .collect(),
        },
    };

    let output = render_markdown(&report, 20, &OutputArgs::default());

    assert!(output.lines().count() <= 180, "{}", output.lines().count());
    assert!(output.contains("module-19"));
    assert!(!output.contains("module-20"));
    assert!(output.contains("hot-19"));
    assert!(!output.contains("hot-20"));
    assert!(output.contains("and 10 more"));
    assert!(output.contains("sites counted per `[[module]]` rule"));
    assert!(output.contains(
        "| rule | upward sites | reviewed (sites, intent) | unreviewed (sites) | unadmitted (sites) |"
    ));
    assert!(output.contains("| src/rule-19 | 11 | cli 10 (keep) | cli::render 1 | — |"));
    assert!(output.contains("module cycles: (crate root re-exports) ↔ harness (2/20 sites)"));
    assert!(output.contains("ledger: 54 admission intents, 1 holds"));
    assert!(output.contains("ledger problem: row 19 is unreadable"));
    assert!(!output.contains("row 20 is unreadable"));
    assert!(output.contains("ledger problems: and 10 more"));
    assert!(!output.contains("src/rule-20"));
    assert!(output.contains("_10 more rules omitted._"));
    assert!(output.contains("stranglers (current/baseline): `legacy_open` src/store 2/3"));
    assert!(output.contains("shape families dropped as std vocabulary: 6"));
    assert!(output.contains("3 below the finding gate"));
    assert!(output.contains("2 as one module's API"));
    assert!(output.contains("guard families dropped as std idiom: 4"));
    assert!(output.contains("2 as predicate use"));
    assert!(output.contains("cx: severity-weighted over-threshold excess"));
}

#[test]
fn probes_join_rank_hot_shapes_guards_and_admitted_debt() {
    let rows = vec![Row {
        module: "store/snapshot".to_owned(),
        code: 3_200,
        esc: 28,
        churn: 4.1,
        flags: vec!["pin", "cx"],
        ..Row::default()
    }];
    let hot = vec![
        Hotspot {
            function: "fold_snapshot".to_owned(),
            path: PathBuf::from("crates/rimz/src/store/snapshot.rs"),
            line: 10,
            cx: 10.0,
            churn: 7.13,
            hot: 71.3,
        },
        Hotspot {
            function: "apply_delta".to_owned(),
            path: PathBuf::from("crates/rimz/src/store/snapshot/apply.rs"),
            line: 20,
            cx: 8.0,
            churn: 5.025,
            hot: 40.2,
        },
    ];
    let shapes = vec![
        ShapeFamily {
            name: "fold".to_owned(),
            members: vec![Member {
                path: PathBuf::from("crates/rimz/src/store/snapshot.rs"),
                line: 10,
                name: "fold_snapshot".to_owned(),
                sloc: 40,
            }],
            files: 1,
            mean_sloc: 40.0,
            sloc_in_play: 40.0,
            score: 40.0,
            siblings: 0,
            role: None,
            provider: None,
        },
        ShapeFamily {
            name: "apply".to_owned(),
            members: vec![Member {
                path: PathBuf::from("crates/rimz/src/store/snapshot/apply.rs"),
                line: 20,
                name: "apply_delta".to_owned(),
                sloc: 40,
            }],
            files: 2,
            mean_sloc: 40.0,
            sloc_in_play: 80.0,
            score: 80.0,
            siblings: 2,
            role: Some("apply.rs".to_owned()),
            provider: None,
        },
    ];
    let guards = vec![GuardFamily {
        key: "ready".to_owned(),
        files: 1,
        sites: 3,
        locations: vec![GuardSite {
            path: PathBuf::from("crates/rimz/src/store/snapshot.rs"),
            line: 30,
            kind: "if".to_owned(),
        }],
    }];
    let debt = Debt {
        configured: true,
        rules: vec![DebtRow {
            path: PathBuf::from("crates/rimz/src/store"),
            upward_sites: 200,
            reviewed: vec![ReviewedSites {
                provider: "cli".to_owned(),
                sites: 173,
                intent: "keep".to_owned(),
            }],
            unreviewed: vec![ProviderSites {
                provider: "cli::render".to_owned(),
                sites: 12,
            }],
            unadmitted: vec![ProviderSites {
                provider: "agents".to_owned(),
                sites: 15,
            }],
        }],
        stranglers: Vec::new(),
    };

    let probes = build_probes(
        Path::new("crates/rimz/src"),
        &rows,
        &hot,
        &shapes,
        &guards,
        &debt,
    );

    assert_eq!(probes[0].module, "store/snapshot");
    assert_eq!(probes[0].rank, 1);
    assert_eq!(probes[0].hot.len(), 2);
    assert_eq!(probes[0].shape_families, 2);
    assert_eq!(probes[0].sibling_families, 1);
    assert_eq!(probes[0].guard_families, 1);
    assert_eq!(probes[0].admitted_upward_sites, 185);
    assert_eq!(probes[0].unreviewed_upward_sites, 12);
    assert_eq!(
        probes[0].next,
        "cargo xtask atlas inspect --module store::snapshot --section verdict,callers,heaviest,calls --out /tmp/atlas-store-snapshot.md"
    );
    let mut output = String::new();
    render_probes(&mut output, &probes);
    assert!(output.contains(
        "`store/snapshot` — rank #1 (code 3.2k, esc 28, churn 4.1, flags pin,cx) · hot: fold_snapshot 71.3, apply_delta 40.2 · shapes: 2 families (1 sibling → collapse?) · guards: 1 family · admitted upward: 185 sites (12 unreviewed)"
    ));
}

#[test]
fn held_rows_keep_their_rank_but_leave_the_probes() {
    let rows = vec![
        Row {
            module: "store/snapshot".to_owned(),
            code: 3_200,
            flags: vec![HELD_FLAG],
            ..Row::default()
        },
        Row {
            module: "config".to_owned(),
            code: 2_000,
            flags: vec![REOPEN_FLAG],
            ..Row::default()
        },
    ];

    let probes = build_probes(
        Path::new("crates/rimz/src"),
        &rows,
        &[],
        &[],
        &[],
        &Debt::default(),
    );

    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].module, "config");
    assert_eq!(
        probes[0].rank, 2,
        "rank is the row's position, not the probe's"
    );
}

#[test]
fn held_rows_are_flagged_from_the_ledger_and_reopen_at_the_commit_count() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "base",
    ]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::create_dir_all(root.path().join("src/store")).unwrap();
    for step in 0..2 {
        std::fs::write(
            root.path().join("src/store/snapshot.rs"),
            format!("// {step}\n"),
        )
        .unwrap();
        std::fs::write(root.path().join("src/config.rs"), format!("// {step}\n")).unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "touch",
        ]);
    }
    let ledger = ledger::parse(&format!(
        "## Module verdicts\n\n| module | status | sha | reopen at | note |\n| --- | --- | --- | --- | --- |\n| `store/snapshot` | holds | {base} | 3 | fresh |\n| `config` | holds | {base} | 2 | stale |\n| `mux` | holds | 0000000 | 2 | unknown sha |\n"
    ));
    let mut rows = ["store/snapshot", "config", "mux", "agents"]
        .map(|module| Row {
            module: module.to_owned(),
            ..Row::default()
        })
        .to_vec();
    let mut problems = Vec::new();

    flag_held_rows(
        root.path(),
        Path::new("src"),
        &ledger,
        &mut rows,
        &mut problems,
    );

    assert_eq!(rows[0].flags, [HELD_FLAG], "2 commits under reopen at 3");
    assert_eq!(rows[1].flags, [REOPEN_FLAG], "2 commits reach reopen at 2");
    assert!(rows[2].flags.is_empty());
    assert!(rows[3].flags.is_empty());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("`mux` holds at 0000000"));
}

#[test]
fn cycles_count_dependency_sites_in_both_directions() {
    let sources = vec![
        super::super::sources::Source::new(
            "crates/demo/src/a.rs",
            "use crate::b::{one, two};\npub fn back() {}\n",
        ),
        super::super::sources::Source::new(
            "crates/demo/src/b.rs",
            "use crate::a::back;\npub fn one() {}\npub fn two() {}\n",
        ),
    ];
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    let known_modules = syntax
        .files
        .iter()
        .map(|file| file.module_path.clone())
        .collect::<BTreeSet<_>>();

    let cycles = cycles_from_syntax(
        &syntax.files,
        &known_modules,
        &BTreeSet::new(),
        Path::new("crates/demo/src"),
        None,
    );

    assert_eq!(
        cycles,
        [Cycle {
            a: "a".to_owned(),
            b: "b".to_owned(),
            a_to_b: 2,
            b_to_a: 1,
            same_layer: None,
        }]
    );

    let ranks = LayerRanks::new(&[vec!["a".to_owned()], vec!["b".to_owned()]]);
    let cycles = cycles_from_syntax(
        &syntax.files,
        &known_modules,
        &BTreeSet::new(),
        Path::new("crates/demo/src"),
        Some(&ranks),
    );
    assert_eq!(cycles[0].same_layer, Some(false));
}

#[test]
fn survey_parses_json_out_and_sections() {
    let args = [
        "--json",
        "--out",
        "/tmp/atlas-survey.json",
        "--section",
        "rank,guards",
        "--by",
        "tc",
        "--all",
    ]
    .map(str::to_owned)
    .to_vec();

    let parsed = parse_args(&args).unwrap().unwrap();

    assert!(parsed.output.json);
    assert_eq!(parsed.by, RankBy::TestCode);
    assert!(parsed.all);
    assert_eq!(
        parsed.output.out.as_deref(),
        Some(Path::new("/tmp/atlas-survey.json"))
    );
    assert!(parsed.output.wants("rank"));
    assert!(parsed.output.wants("guards"));
    assert!(!parsed.output.wants("shapes"));
}

#[test]
fn survey_rejects_unknown_sections() {
    let args = ["--section", "rank,unknown"].map(str::to_owned).to_vec();

    let error = parse_args(&args).unwrap_err().to_string();

    assert!(error.contains("unknown section(s) unknown"));
}
