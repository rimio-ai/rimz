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
            cyclomatic: 26.0,
            cognitive: 29.0,
            sloc: 118.0,
            cx: 12.7,
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
        ambiguous: Vec::new(),
        ledger: LedgerNote {
            present: true,
            intents: 54,
            holds: 1,
            problems: (0..30)
                .map(|index| format!("row {index} is unreadable"))
                .collect(),
            restamps: Vec::new(),
            restamped: false,
        },
    };

    let output = render_markdown(&report, 20, &OutputArgs::default());

    assert!(output.lines().count() <= 180, "{}", output.lines().count());
    assert!(output.contains("module-19"));
    assert!(!output.contains("module-20"));
    assert!(output.contains("| function | file:line | cyc | cog | sloc | cx | churn% | hot |"));
    assert!(output.contains("| hot-19 | src/hot-19.rs:1 | 26 | 29 | 118 | 12.7 | 1.0 | 1.0 |"));
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

    let json: serde_json::Value =
        serde_json::from_str(&render_json(&report, &OutputArgs::default()).unwrap()).unwrap();
    let hot = &json["hot"][0];
    assert_eq!(
        (&hot["cyclomatic"], &hot["cognitive"], &hot["sloc"]),
        (
            &serde_json::json!(26.0),
            &serde_json::json!(29.0),
            &serde_json::json!(118.0)
        )
    );
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
            cyclomatic: 0.0,
            cognitive: 0.0,
            sloc: 0.0,
            cx: 10.0,
            churn: 7.13,
            hot: 71.3,
        },
        Hotspot {
            function: "apply_delta".to_owned(),
            path: PathBuf::from("crates/rimz/src/store/snapshot/apply.rs"),
            line: 20,
            cyclomatic: 0.0,
            cognitive: 0.0,
            sloc: 0.0,
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
    // A rebase-merged branch commit: it resolves, but HEAD never reaches it.
    let rewritten = git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit-tree",
        "HEAD^{tree}",
        "-p",
        &base,
        "-m",
        "branch",
    ]);
    let ledger_path = root.path().join(LEDGER_FILE);
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let committed = format!(
        "## Module verdicts\n\n| module | status | sha | reopen at | note |\n| --- | --- | --- | --- | --- |\n| `store/snapshot` | holds | {base} | 3 | fresh |\n| `config` | holds | {base} | 2 | stale |\n"
    );
    std::fs::write(&ledger_path, &committed).unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "record",
    ]);
    let uncommitted = format!(
        "{committed}| `mux` | holds | 0000000 | 2 | unknown sha |\n| `lsp` | holds | {rewritten} | 30 | off trunk |\n"
    );
    std::fs::write(&ledger_path, &uncommitted).unwrap();
    let ledger = ledger::load(root.path()).unwrap().unwrap();
    assert!(
        ledger.restamps.is_empty(),
        "working-tree-only SHAs cannot resolve"
    );
    let mut rows = ["store/snapshot", "config", "mux", "agents", "lsp"]
        .map(|module| Row {
            module: module.to_owned(),
            ..Row::default()
        })
        .to_vec();
    let mut problems = ledger.problems.clone();

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
    assert!(
        rows[4].flags.is_empty(),
        "an off-trunk sha leaves lsp unheld"
    );
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].starts_with("`mux` holds at 0000000"));
    assert_eq!(
        problems[1],
        format!("`lsp` holds at {rewritten}: {rewritten} is not an ancestor of HEAD")
    );

    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"atlas-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "land verdicts",
    ]);
    let landed = git(&["rev-parse", "HEAD"]);
    let short = git(&["rev-parse", "--short=7", "HEAD"]);
    let args = parse_args(&[
        "--path".to_owned(),
        "src/store".to_owned(),
        "--restamp".to_owned(),
        "--json".to_owned(),
        "--section".to_owned(),
        "footer".to_owned(),
    ])
    .unwrap()
    .unwrap();
    assert!(args.restamp);
    let mut facts = Facts::load(
        root.path(),
        &args.path,
        Facets {
            history: true,
            ..Facets::default()
        },
    )
    .unwrap();
    facts.metrics = Some(super::super::metrics::MetricsReport {
        module_scores: BTreeMap::new(),
        functions: Vec::new(),
    });
    let report = build_report(root.path(), &facts, &args.path, args.by, args.all).unwrap();
    ledger::write_restamps(root.path(), &report.ledger.restamps).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&render_json(&report, &args.output).unwrap()).unwrap();
    let restamps = json["footer"]["ledger"]["restamps"].as_array().unwrap();
    assert_eq!(restamps.len(), 2);
    assert_eq!(restamps[0]["module"], "mux");
    assert_eq!(restamps[1]["module"], "lsp");
    assert_eq!(
        std::fs::read_to_string(&ledger_path).unwrap(),
        uncommitted
            .replace("0000000", &short)
            .replace(&rewritten, &landed)
    );
    let ledger = ledger::load(root.path()).unwrap().unwrap();
    assert!(ledger.problems.is_empty());
    assert!(ledger.restamps.is_empty());
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
fn survey_accepts_restamp_once() {
    assert!(parse_args(&["--restamp".to_owned()]).is_ok());
    let error = parse_args(&["--restamp".to_owned(), "--restamp".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("may only be passed once"));
}

#[test]
fn ledger_restamp_footer_is_bounded_and_names_the_write_action() {
    let mut note = LedgerNote {
        present: true,
        restamps: ["demo", "other"]
            .map(|module| ledger::Restamp {
                module: module.to_owned(),
                from: "deadbeef0".to_owned(),
                to: "123456789".to_owned(),
                line: 1,
            })
            .to_vec(),
        ..LedgerNote::default()
    };
    let mut output = String::new();
    render_ledger_note(&mut output, &note, 1);
    assert!(output.contains("ledger restamp: `demo` deadbeef0 -> 123456789"));
    assert!(!output.contains("ledger restamp: `other`"));
    assert!(output.contains("ledger restamps: and 1 more"));
    assert!(output.contains("`cargo xtask atlas survey --restamp` writes them"));
    note.restamped = true;
    output.clear();
    render_ledger_note(&mut output, &note, 1);
    assert!(output.contains(&format!(
        "ledger restamps: wrote 2 sha cells to {LEDGER_FILE}"
    )));
}

#[test]
fn survey_rejects_unknown_sections() {
    let args = ["--section", "rank,unknown"].map(str::to_owned).to_vec();

    let error = parse_args(&args).unwrap_err().to_string();

    assert!(error.contains("unknown section(s) unknown"));
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn ledger_rows_are_spelled_crate_relative_whatever_the_scope() {
    for (scope, row, key) in [
        ("crates/rimz/src", "cli/remote", "cli/remote"),
        ("crates/rimz/src", "(root)", "(root)"),
        ("crates/rimz/src/cli", "remote", "cli/remote"),
        ("crates/rimz/src/cli", "(root)", "cli/(root)"),
        (
            "crates/rimz/src/cli",
            "agents_cmd/(root)",
            "cli/agents_cmd/(root)",
        ),
        ("crates/rimz/src/lsp", "(root)", "lsp/(root)"),
        ("crates/rimz/src/cli/remote.rs", "remote", "cli/remote"),
        ("crates/rimz/src/lib.rs", "(root)", "(root)"),
        ("src", "store/snapshot", "store/snapshot"),
    ] {
        assert_eq!(ledger_row(Path::new(scope), row), key, "{scope} {row}");
    }
}

#[test]
fn scoped_rows_read_the_ledger_row_of_their_crate_relative_spelling() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    std::fs::create_dir_all(root.path().join("crates/demo/src/cli")).unwrap();
    std::fs::write(root.path().join("crates/demo/src/cli/remote.rs"), "\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "base"]);
    let base = git(root.path(), &["rev-parse", "HEAD"]);
    let ledger_path = root.path().join(LEDGER_FILE);
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let flags = |ledger_row: &str| {
        std::fs::write(
            &ledger_path,
            format!(
                "## Module verdicts\n\n| module | status | sha | reopen at | note |\n| --- | --- | --- | --- | --- |\n| `{ledger_row}` | holds | {base} | 5 | reviewed |\n"
            ),
        )
        .unwrap();
        let ledger = ledger::load(root.path()).unwrap().unwrap();
        let mut rows = vec![Row {
            module: "remote".to_owned(),
            ..Row::default()
        }];
        let mut problems = Vec::new();
        flag_held_rows(
            root.path(),
            Path::new("crates/demo/src/cli"),
            &ledger,
            &mut rows,
            &mut problems,
        );
        assert!(problems.is_empty(), "{problems:?}");
        rows.remove(0).flags
    };

    assert!(
        flags("remote").is_empty(),
        "the library `remote` row does not hold `cli/remote`"
    );
    assert_eq!(flags("cli/remote"), [HELD_FLAG]);
}

#[test]
fn stale_verdict_keys_are_judged_crate_wide_whatever_the_scope() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    let caller = |name: &str| {
        format!(
            "fn {name}(s: &crate::store::S) {{\n    if s.phase == crate::store::Phase::Ready {{}}\n}}\n"
        )
    };
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n".to_owned(),
        ),
        (
            "src/lib.rs",
            "mod store;\nmod outside;\nmod inside;\n".to_owned(),
        ),
        (
            "src/store.rs",
            "#[derive(PartialEq)]\npub enum Phase { Ready }\npub struct S { pub phase: Phase }\n"
                .to_owned(),
        ),
        ("src/outside/a.rs", caller("a")),
        ("src/outside/b.rs", caller("b")),
        ("src/outside/c.rs", caller("c")),
        (
            "src/outside/cmd.rs",
            "fn run_private() -> usize { 1 }\nfn forward(value: usize) -> usize { target(value) }\nfn target(value: usize) -> usize { value }\n"
                .to_owned(),
        ),
        ("src/inside/mod.rs", "pub fn quiet() {}\n".to_owned()),
    ];
    for (path, text) in &files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "fixture"]);
    let scope = Path::new("src/inside");
    let mut facts = Facts::load(
        root.path(),
        scope,
        Facets {
            history: true,
            ..Facets::default()
        },
    )
    .unwrap();
    facts.metrics = Some(super::super::metrics::MetricsReport {
        module_scores: BTreeMap::new(),
        functions: Vec::new(),
    });
    let outside = detect::guard_families(&facts, Path::new("."));
    assert_eq!(outside.len(), 1, "{outside:?}");
    assert!(
        detect::guard_families(&facts, scope).is_empty(),
        "the family lives only outside the scope"
    );
    let verdict = |kind: &str, key: &str| {
        format!("[[verdict]]\nkind = \"{kind}\"\nkey = '{key}'\nreason = \"probe\"\n\n")
    };
    std::fs::write(
        root.path().join(TARGET_FILE),
        format!(
            "version = 5\nlayers = []\n\n{}{}{}{}{}{}",
            verdict("guard", &outside[0].key),
            verdict("guard", "gone.phase==Phase::Ready"),
            verdict("item", "outside::cmd::run_private"),
            verdict("item", "outside::cmd::gone"),
            verdict("pass-through", "outside::cmd::forward"),
            verdict("pass-through", "outside::cmd::run_private"),
        ),
    )
    .unwrap();

    let report = build_report(root.path(), &facts, scope, RankBy::default(), false).unwrap();

    assert_eq!(
        report.stale,
        [
            "guard:gone.phase==Phase::Ready",
            "item:outside::cmd::gone",
            "pass-through:outside::cmd::run_private",
        ]
    );
    let output = render_markdown(&report, 20, &OutputArgs::default());
    assert!(output.contains(
        "stale verdict keys: guard:gone.phase==Phase::Ready, item:outside::cmd::gone, pass-through:outside::cmd::run_private"
    ));
    let json: serde_json::Value =
        serde_json::from_str(&render_json(&report, &OutputArgs::default()).unwrap()).unwrap();
    assert_eq!(
        json["footer"]["stale_verdict_keys"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn scoped_assemblers_count_modules_outside_the_scope_by_top_module() {
    let report = super::super::syntax::analyze_sources(
        &[Source::new(
            "crates/demo/src/cli/exec.rs",
            "use crate::harness::launch;\nuse crate::store::open;\nuse crate::agents::catalog;\nfn run_exec() {\n    launch::start();\n    open();\n    catalog::load();\n    super::render::table();\n    self::helper();\n}\nfn inside_only() { super::render::table(); super::room::open(); self::helper(); }\nfn helper() {}\n",
        )],
        &BTreeSet::new(),
    );
    let known = [
        "cli",
        "cli::exec",
        "cli::render",
        "cli::room",
        "harness",
        "harness::launch",
        "store",
        "agents",
        "agents::catalog",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();

    let rows = assemblers(
        &report.files,
        &known,
        &BTreeSet::new(),
        Path::new("crates/demo/src/cli"),
    );

    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].function, "run_exec");
    assert_eq!(
        rows[0]
            .providers
            .iter()
            .map(|provider| (provider.provider.as_str(), provider.sites))
            .collect::<Vec<_>>(),
        [
            ("agents", 1),
            ("cli/render", 1),
            ("harness", 1),
            ("store", 1)
        ]
    );
    let mut output = String::new();
    let report = Report {
        path: PathBuf::from("crates/demo/src/cli"),
        probes: Vec::new(),
        rows: Vec::new(),
        totals: Totals::default(),
        hot: Vec::new(),
        assemblers: rows,
        debt: Debt::default(),
        cycles: Vec::new(),
        shapes: Vec::new(),
        guards: Vec::new(),
        history_commits: 0,
        pace_window: 0,
        parse_failures: 0,
        shape_families_dropped: shapes::FamilyDrops::default(),
        guard_families_dropped: detect::GuardDrops::default(),
        suppressed: 0,
        stale: Vec::new(),
        ambiguous: Vec::new(),
        ledger: LedgerNote::default(),
    };
    output.push_str(&render_markdown(
        &report,
        20,
        &parse_args(&["--section".to_owned(), "assemblers".to_owned()])
            .unwrap()
            .unwrap()
            .output,
    ));
    assert!(output.contains("1 functions call into 3+ modules (syntax-resolved"));
}

#[test]
fn scoped_assemblers_keep_a_scope_row_apart_from_the_library_module_of_its_name() {
    let report = super::super::syntax::analyze_sources(
        &[Source::new(
            "crates/demo/src/cli/reset.rs",
            "use crate::room::Room;\nfn run() {\n    Room::open();\n    super::room::prompt();\n    crate::store::open();\n}\n",
        )],
        &BTreeSet::new(),
    );
    let known = ["cli", "cli::reset", "cli::room", "room", "store"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    let rows = assemblers(
        &report.files,
        &known,
        &BTreeSet::new(),
        Path::new("crates/demo/src/cli"),
    );

    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0]
            .providers
            .iter()
            .map(|provider| (provider.provider.as_str(), provider.sites))
            .collect::<Vec<_>>(),
        [("cli/room", 1), ("room", 1), ("store", 1)]
    );
}

#[test]
fn assemblers_resolve_callees_qualified_by_a_workspace_crate_name() {
    let report = super::super::syntax::analyze_sources(
        &[Source::new(
            "crates/demo/src/cli/exec.rs",
            "fn run_exec() {\n    demo::harness::launch::compile();\n    demo::store::run::RunRecord::load();\n    demo::agents::list();\n    demo::Store::open();\n    other::harness::start();\n}\n",
        )],
        &BTreeSet::from(["demo".to_owned()]),
    );
    let known = [
        "cli",
        "cli::exec",
        "harness",
        "harness::launch",
        "store",
        "store::run",
        "agents",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let wiring = |crate_names: &BTreeSet<String>, scope: &str| {
        assemblers(&report.files, &known, crate_names, Path::new(scope))
            .into_iter()
            .map(|row| {
                row.providers
                    .into_iter()
                    .map(|provider| (provider.provider, provider.sites))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    // The library root reads `(root)` whatever the scope: a scoped run
    // spells its own root `cli/(root)`, so the two never merge.
    let expected = [
        ("(root)".to_owned(), 1),
        ("agents".to_owned(), 1),
        ("harness".to_owned(), 1),
        ("store".to_owned(), 1),
    ];

    let crate_names = BTreeSet::from(["demo".to_owned()]);
    assert_eq!(
        wiring(&crate_names, "crates/demo/src/cli"),
        [expected.to_vec()]
    );
    assert_eq!(wiring(&crate_names, "crates/demo/src"), [expected.to_vec()]);
    assert!(wiring(&BTreeSet::new(), "crates/demo/src/cli").is_empty());
}

#[test]
fn stale_item_keys_resolve_private_items_owner_keys_and_child_module_methods() {
    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        (
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "src/lib.rs",
            "mod disk;\nmod lsp;\nmod mux;\nmod sidebar;\n",
        ),
        ("src/disk.rs", "mod usage;\n"),
        (
            "src/disk/usage.rs",
            "struct FileIdentity {\n    dev: u64,\n}\nenum RequiredDecision {\n    Run,\n}\n",
        ),
        (
            "src/sidebar/mod.rs",
            "trait SidebarMux {}\nstruct Shared {}\n",
        ),
        ("src/sidebar/view.rs", "pub struct Shared;\n"),
        ("src/lsp.rs", "mod check;\n"),
        (
            "src/lsp/check.rs",
            "pub struct Report;\nimpl Report {\n    pub fn render(&self) {}\n}\n",
        ),
        ("src/mux.rs", "mod zellij;\n"),
        ("src/mux/zellij.rs", "mod presence;\n"),
        (
            "src/mux/zellij/presence.rs",
            "pub struct Zellij;\nimpl Zellij {\n    pub fn converge_presence_plugin_for(&self) {}\n}\n",
        ),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let facts = Facts::load(root.path(), Path::new("src"), Facets::default()).unwrap();
    let verdict = |key: &str| super::super::target::Verdict {
        kind: VerdictKind::Item,
        key: key.to_owned(),
        reason: "probe".to_owned(),
    };
    let target = Target {
        version: 5,
        layers: Vec::new(),
        modules: Vec::new(),
        strangler: Vec::new(),
        verdicts: [
            "disk::usage::FileIdentity",
            "disk::usage::RequiredDecision",
            "sidebar::SidebarMux",
            "sidebar::Shared",
            "lsp::check::Report::render",
            "lsp::check::render",
            "lsp::check::Other::render",
            "mux::zellij::converge_presence_plugin_for",
            "mux::zellij::converge_gone",
            "disk::usage::Gone",
        ]
        .map(verdict)
        .to_vec(),
    };

    let check = verdict_key_check(&target, &facts, false);
    assert_eq!(
        check.stale,
        [
            "item:disk::usage::Gone",
            "item:lsp::check::Other::render",
            "item:mux::zellij::converge_gone",
        ]
    );
    assert!(check.ambiguous.is_empty(), "{:?}", check.ambiguous);
}

#[test]
fn root_rows_count_the_commits_of_the_root_file_they_head() {
    let root = tempfile::tempdir().unwrap();
    for path in [
        "crates/demo/src/lib.rs",
        "crates/demo/src/main.rs",
        "crates/demo/src/cli/mod.rs",
        "crates/demo/src/mux.rs",
        "crates/demo/src/mux/zellij.rs",
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "\n").unwrap();
    }
    let paths = |scope: &str, row: &str| {
        row_history_paths(root.path(), Path::new(scope), row)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        paths("crates/demo/src/cli", "(root)"),
        ["crates/demo/src/cli/mod.rs"]
    );
    assert_eq!(
        paths("crates/demo/src/mux", "(root)"),
        ["crates/demo/src/mux.rs"]
    );
    assert_eq!(
        paths("crates/demo/src", "(root)"),
        ["crates/demo/src/lib.rs", "crates/demo/src/main.rs"]
    );
    assert_eq!(
        paths("crates/demo/src", "cli/(root)"),
        ["crates/demo/src/cli/mod.rs"]
    );
    assert_eq!(
        paths("crates/demo/src", "cli/remote"),
        [
            "crates/demo/src/cli/remote",
            "crates/demo/src/cli/remote.rs"
        ]
    );

    // The decision the path feeds: a scoped `(root)` hold reopens once its
    // root file's commits reach the count.
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "base"]);
    let base = git(root.path(), &["rev-parse", "HEAD"]);
    for step in 0..2 {
        std::fs::write(
            root.path().join("crates/demo/src/cli/mod.rs"),
            format!("// {step}\n"),
        )
        .unwrap();
        git(root.path(), &["commit", "-qam", "touch cli root"]);
    }
    let ledger_path = root.path().join(LEDGER_FILE);
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    std::fs::write(
        &ledger_path,
        format!(
            "## Module verdicts\n\n| module | status | sha | reopen at | note |\n| --- | --- | --- | --- | --- |\n| `cli/(root)` | holds | {base} | 2 | reviewed |\n"
        ),
    )
    .unwrap();
    let ledger = ledger::load(root.path()).unwrap().unwrap();
    let mut rows = vec![Row {
        module: "(root)".to_owned(),
        ..Row::default()
    }];
    let mut problems = Vec::new();

    flag_held_rows(
        root.path(),
        Path::new("crates/demo/src/cli"),
        &ledger,
        &mut rows,
        &mut problems,
    );

    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(rows[0].flags, [REOPEN_FLAG]);
}

#[test]
fn ambiguous_verdict_keys_are_reported_beside_stale_ones() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    for (path, text) in [
        (
            "Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        ),
        ("src/lib.rs", "mod store;\n"),
        (
            "src/store.rs",
            "pub struct Left;\npub struct Right;\nimpl Left {\n    /// Opens.\n    pub fn open() {}\n    /// Forwards.\n    fn forward(value: usize) -> usize {\n        target(value)\n    }\n}\nimpl Right {\n    pub fn open() {}\n    fn forward(value: usize) -> usize {\n        target(value)\n    }\n}\nfn relay(value: usize) -> usize {\n    target(value)\n}\nfn target(value: usize) -> usize {\n    value\n}\n",
        ),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "fixture"]);
    let verdict = |kind: &str, key: &str| {
        format!("[[verdict]]\nkind = \"{kind}\"\nkey = '{key}'\nreason = \"probe\"\n\n")
    };
    std::fs::write(
        root.path().join(TARGET_FILE),
        format!(
            "version = 5\nlayers = []\n\n{}{}{}{}{}",
            verdict("item", "store::open"),
            verdict("item", "store::Left::open"),
            verdict("pass-through", "store::forward"),
            verdict("pass-through", "store::Left::forward"),
            verdict("pass-through", "store::relay"),
        ),
    )
    .unwrap();
    let mut facts = Facts::load(
        root.path(),
        Path::new("src"),
        Facets {
            history: true,
            ..Facets::default()
        },
    )
    .unwrap();
    facts.metrics = Some(super::super::metrics::MetricsReport {
        module_scores: BTreeMap::new(),
        functions: Vec::new(),
    });

    let report = build_report(
        root.path(),
        &facts,
        Path::new("src"),
        RankBy::default(),
        false,
    )
    .unwrap();

    assert!(report.stale.is_empty(), "{:?}", report.stale);
    assert_eq!(report.ambiguous.len(), 2, "{:?}", report.ambiguous);
    assert!(
        report.ambiguous[0].starts_with(
            "item:store::open — ambiguous: 2 visible items named open: src/store.rs:5 (owner Left), src/store.rs:12 (owner Right)"
        ),
        "{:?}",
        report.ambiguous
    );
    assert!(
        report.ambiguous[1].starts_with(
            "pass-through:store::forward — ambiguous: 2 production functions named forward"
        ),
        "{:?}",
        report.ambiguous
    );
    let output = render_markdown(&report, 20, &OutputArgs::default());
    assert!(output.contains("stale verdict keys: none\nambiguous verdict keys: item:store::open"));
    let json: serde_json::Value =
        serde_json::from_str(&render_json(&report, &OutputArgs::default()).unwrap()).unwrap();
    assert_eq!(
        json["footer"]["ambiguous_verdict_keys"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let mut clean = report;
    clean.ambiguous.clear();
    let output = render_markdown(&clean, 20, &OutputArgs::default());
    assert!(output.contains("stale verdict keys: none\nambiguous verdict keys: none"));
    clean.stale.push("item:store::gone".to_owned());
    let output = render_markdown(&clean, 20, &OutputArgs::default());
    assert!(
        !output.contains("ambiguous verdict keys"),
        "a clean ambiguous line prints only beside a `none` stale line"
    );
}

#[test]
fn shape_keys_one_view_forms_several_families_under_are_collisions() {
    let family = |name: &str, path: &str| ShapeFamily {
        name: name.to_owned(),
        members: vec![Member {
            path: PathBuf::from(path),
            line: 3,
            name: "load".to_owned(),
            sloc: 20,
        }],
        files: 1,
        mean_sloc: 20.0,
        sloc_in_play: 20.0,
        score: 1.0,
        siblings: 0,
        role: None,
        provider: None,
    };

    let collisions = shape_collisions_in(&[
        family("load+parse", "src/a.rs"),
        family("load+parse", "src/b.rs"),
        family("walk", "src/c.rs"),
    ]);

    assert_eq!(
        collisions.into_iter().collect::<Vec<_>>(),
        [(
            "load+parse".to_owned(),
            vec![
                "src/a.rs:3 (load)".to_owned(),
                "src/b.rs:3 (load)".to_owned()
            ]
        )]
    );
}
