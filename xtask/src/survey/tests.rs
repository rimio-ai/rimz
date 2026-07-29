use super::*;

#[test]
fn parse_args_accepts_flags_in_any_order() {
    assert_eq!(
        parse_survey_args(&[]).unwrap(),
        Some(SurveyArgs {
            path: PathBuf::from(DEFAULT_PATH),
            deps: false,
            json: false,
        })
    );
    assert_eq!(
        parse_survey_args(&[
            "--json".to_owned(),
            "--path".to_owned(),
            "xtask/src".to_owned(),
            "--deps".to_owned(),
        ])
        .unwrap(),
        Some(SurveyArgs {
            path: PathBuf::from("xtask/src"),
            deps: true,
            json: true,
        })
    );
}

#[test]
fn parse_args_detects_help_and_rejects_invalid_flags() {
    assert_eq!(parse_survey_args(&["--help".to_owned()]).unwrap(), None);
    assert_eq!(
        parse_survey_args(&["--deps".to_owned(), "-h".to_owned()]).unwrap(),
        None
    );
    assert!(parse_survey_args(&["--path".to_owned()]).is_err());
    assert!(
        parse_survey_args(&[
            "--path".to_owned(),
            "src".to_owned(),
            "--path".to_owned(),
            "tests".to_owned(),
        ])
        .is_err()
    );
    assert!(parse_survey_args(&["--deps".to_owned(), "--deps".to_owned()]).is_err());
    assert!(parse_survey_args(&["--json".to_owned(), "--json".to_owned()]).is_err());
    assert!(parse_survey_args(&["--path".to_owned(), "../outside".to_owned()]).is_err());
    assert!(parse_survey_args(&["--unknown".to_owned()]).is_err());
}

#[test]
fn size_uses_the_shared_inline_test_split() {
    let source = "fn live() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn works() {}\n}\n";
    assert_eq!(rust_sloc(source), 6);
    let marker = source_files::inline_test_marker_line(source);
    assert_eq!(marker, Some(2));
    assert_eq!(
        source_files::split_file_loc(rust_sloc(source) as f64, marker),
        (1.0, 5.0)
    );
    assert_eq!(split_rust_sloc(source), (1, 5));
}

#[test]
fn rust_sloc_ignores_comments_and_understands_nested_and_raw_literals() {
    let source = r####"
// comment
fn live() { /* comment
    /* nested */
*/ }
const URL: &str = "https://example.com/a//b";
const RAW: &str = r###"not /* a comment */
still code"###;
const QUOTE: char = '"';
// not code after the quote character

fn after_quote() {}
"####;
    assert_eq!(rust_sloc(source), 7);
}

#[test]
fn rename_folding_attributes_old_commits_to_the_head_module() {
    let temp = tempfile::tempdir().unwrap();
    let current = temp.path().join("src/new/current.rs");
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    fs::write(&current, "fn current() {}\n").unwrap();
    let history = parse_history(
        "@a\nA\tsrc/old/current.rs\n\
         @b\nM\tsrc/old/current.rs\n\
         @c\nR100\tsrc/old/current.rs\tsrc/new/current.rs\n\
         @d\nM\tsrc/new/current.rs\n\
         @e\nA\tsrc/dead.rs\n\
         @f\nD\tsrc/dead.rs\n",
    )
    .unwrap();

    let report = fold_history(temp.path(), Path::new("src"), &history);

    assert_eq!(report.commits, 6);
    assert_eq!(report.retired_commits, 2);
    assert_eq!(report.modules.len(), 1);
    assert_eq!(report.modules[0].module, "new");
    assert_eq!(report.modules[0].commits, 4);
}

#[test]
fn window_sizes_round_up_and_noise_floor_checks_both_populations() {
    assert_eq!(window_size(1, 10), 1);
    assert_eq!(window_size(10, 10), 1);
    assert_eq!(window_size(11, 10), 2);
    assert_eq!(window_size(11, 25), 3);

    assert!(pace_is_noisy(19, 10));
    assert!(pace_is_noisy(20, 4));
    assert!(!pace_is_noisy(20, 5));
}

#[test]
fn dot_edges_aggregate_by_top_level_module() {
    let dot = r#"
        "rimz::sidebar::one" -> "rimz::store::A" [label="uses"];
        "rimz::sidebar::two" -> "rimz::store::B" [label="uses"];
        "rimz::store::back" -> "rimz::sidebar::Thing" [label="uses"];
        "rimz::store::inside" -> "rimz::store::Other" [label="uses"];
        "rimz" -> "rimz::agents::Agent" [label="uses"];
    "#;

    let report = aggregate_coupling(dot);

    assert_eq!(
        report.edges,
        vec![
            ("sidebar".to_owned(), "store".to_owned(), 2),
            ("(root)".to_owned(), "agents".to_owned(), 1),
            ("store".to_owned(), "sidebar".to_owned(), 1),
        ]
    );
    assert_eq!(
        report.mutual,
        vec![("sidebar".to_owned(), "store".to_owned(), 2, 1)]
    );
    let store = report
        .degree
        .iter()
        .find(|degree| degree.module == "store")
        .unwrap();
    assert_eq!((store.fan_in, store.fan_out), (1, 1));
}

#[test]
fn path_modules_use_the_first_segment_below_the_scope() {
    assert!(path_in_scope(
        Path::new("/repo"),
        Path::new("/repo/xtask/src/survey.rs"),
        Path::new(".")
    ));
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/agents/context.rs"),
            Path::new("crates/rimz/src")
        ),
        "agents"
    );
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/lib.rs"),
            Path::new("crates/rimz/src")
        ),
        "(root)"
    );
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/workspace.rs"),
            Path::new("crates/rimz/src")
        ),
        "workspace"
    );
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/workspace/paths.rs"),
            Path::new("crates/rimz/src")
        ),
        "workspace"
    );
    assert_eq!(
        module_for_path(Path::new("xtask/src/survey.rs"), Path::new("xtask/src")),
        "survey"
    );
    assert_eq!(
        module_for_path(Path::new("xtask/src/survey.rs"), Path::new(".")),
        "xtask"
    );
    assert_eq!(
        module_for_path(Path::new("Cargo.toml"), Path::new(".")),
        "Cargo.toml"
    );
    assert_eq!(
        module_for_path(Path::new("Cargo.lock"), Path::new(".")),
        "Cargo.lock"
    );
}
