use std::collections::BTreeSet;
use std::fs;

use super::super::sources::Source;
use super::*;

fn syntax() -> (tempfile::TempDir, Vec<FileSyntax>) {
    let root = tempfile::tempdir().unwrap();
    let sources = [
        Source::new("src/caller.rs", "fn call() {}"),
        Source::new("src/provider.rs", "pub fn serve() {}"),
    ];
    let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
    (root, syntax.files)
}

fn contract() -> PassContract {
    PassContract {
        version: 1,
        base: "main".to_owned(),
        kind: PassKind::Module,
        paths: vec![PathBuf::from("src")],
        max_production_sloc_delta: -1,
        assembly: vec![AssemblyExpectation {
            from: "caller".to_owned(),
            to: "provider".to_owned(),
            max_items: 3,
        }],
        esc: Vec::new(),
        delete: Vec::new(),
        rehome: Vec::new(),
        dependency: Vec::new(),
        cx: Vec::new(),
    }
}

#[test]
fn contract_rejects_unknown_versions() {
    let (root, syntax) = syntax();
    let mut contract = contract();
    contract.version = 3;
    assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
}

#[test]
fn cx_contract_resolves_private_functions_and_methods_on_either_side() {
    let root = tempfile::tempdir().unwrap();
    let syntax = super::super::syntax::analyze_sources(
        &[Source::new(
            "src/cli.rs",
            "fn run() {} struct Owner; impl Owner { fn new() {} }",
        )],
        &BTreeSet::new(),
    );
    for item in ["cli::run", "cli::Owner::new"] {
        for (base, current) in [
            (&syntax.files[..], &[][..]),
            (&[][..], &syntax.files[..]),
            (&syntax.files[..], &syntax.files[..]),
        ] {
            let parsed: PassContract = toml::from_str(&format!("version = 2\nbase = 'main'\npaths = ['src']\nmax-production-sloc-delta = 0\n[[cx]]\nitem = '{item}'\nmax = 5")).expect("cx schema accepts integer maxima");
            assert!(validate(root.path(), current, base, parsed).is_ok());
        }
    }
}

#[test]
fn cx_contract_resolves_inline_modules_on_either_side() {
    let root = tempfile::tempdir().unwrap();
    let syntax = super::super::syntax::analyze_sources(
        &[Source::new(
            "src/cli.rs",
            "mod a { fn deserialize() {} struct Owner; impl Owner { fn new() {} } }
             mod b { fn deserialize() {} }",
        )],
        &BTreeSet::new(),
    );
    for item in [
        "cli::a::deserialize",
        "cli::b::deserialize",
        "cli::a::Owner::new",
        "cli::deserialize",
    ] {
        for (base, current) in [
            (&syntax.files[..], &[][..]),
            (&[][..], &syntax.files[..]),
            (&syntax.files[..], &syntax.files[..]),
        ] {
            let mut contract = contract();
            contract.version = 2;
            contract.assembly.clear();
            contract.cx = vec![CxExpectation {
                item: Some(item.into()),
                path: None,
                max: 5.0,
            }];
            let result = validate(root.path(), current, base, contract);
            if item == "cli::deserialize" {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("defined at neither base nor current")
                );
            } else {
                assert!(result.is_ok(), "{item}: {result:?}");
                for files in [base, current]
                    .into_iter()
                    .filter(|files| !files.is_empty())
                {
                    assert_eq!(cx_functions(files, item).len(), 1);
                }
            }
        }
    }
}

#[test]
fn cx_contract_rejects_invalid_keys_and_scopes() {
    let root = tempfile::tempdir().unwrap();
    let syntax = super::super::syntax::analyze_sources(
        &[Source::new(
            "src/cli.rs",
            "struct Owner; impl A for Owner { fn fmt() {} }\nimpl B for Owner { fn fmt() {} }",
        )],
        &BTreeSet::new(),
    );
    for (row, message) in [
        ("item = 'cli::Owner::fmt'", "src/cli.rs:1, src/cli.rs:2"),
        ("item = 'cli::missing'", "neither base nor current"),
        ("item = 'cli::missing'\npath = 'src/cli.rs'", "exactly one"),
        ("", "exactly one"),
        ("path = 'elsewhere/file.rs'", "inside pass contract paths"),
    ] {
        let parsed: PassContract = toml::from_str(&format!("version = 2\nbase = 'main'\npaths = ['src']\nmax-production-sloc-delta = 0\n[[cx]]\n{row}\nmax = 5.0")).expect("cx schema parses before validation");
        let error = validate(root.path(), &syntax.files, &syntax.files, parsed)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn cx_contract_rejects_out_of_scope_item_and_either_side_ambiguity() {
    let root = tempfile::tempdir().unwrap();
    let syntax = super::super::syntax::analyze_sources(
        &[Source::new("src/cli.rs", "fn run() {}")],
        &BTreeSet::new(),
    );
    let mut contract = contract();
    contract.assembly.clear();
    contract.paths = vec!["src/other".into()];
    contract.cx = vec![CxExpectation {
        item: Some("cli::run".into()),
        path: None,
        max: 5.0,
    }];
    for (base, current) in [(&syntax.files[..], &[][..]), (&[][..], &syntax.files[..])] {
        let error = validate(root.path(), current, base, contract.clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains("inside pass contract paths"), "{error}");
    }
    contract.paths = vec!["src".into()];
    let ambiguous = [syntax.files[0].clone(), syntax.files[0].clone()];
    for (base, current) in [
        (&ambiguous[..], &syntax.files[..]),
        (&syntax.files[..], &ambiguous[..]),
    ] {
        assert!(
            validate(root.path(), current, base, contract.clone())
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }
}

#[test]
fn contract_rejects_empty_paths() {
    let (root, syntax) = syntax();
    let mut contract = contract();
    contract.paths.clear();
    assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
}

#[test]
fn contract_rejects_invalid_paths() {
    let (root, syntax) = syntax();
    let mut contract = contract();
    contract.paths = vec![PathBuf::from("../src")];
    assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
}

#[test]
fn module_contract_takes_a_nonnegative_sloc_ceiling() {
    let (root, syntax) = syntax();
    let mut contract = contract();
    contract.max_production_sloc_delta = 3;
    let loaded = validate(root.path(), &syntax, &syntax, contract).unwrap();
    assert_eq!(loaded.kind, PassKind::Module);
    assert_eq!(loaded.max_production_sloc_delta, 3);
}

#[test]
fn seam_contract_takes_a_flat_ceiling() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    for ceiling in [0, 12] {
        fs::write(
            &path,
            format!(
                r#"version = 2
base = "main"
kind = "seam"
paths = ["src"]
max-production-sloc-delta = {ceiling}

[[dependency]]
from = "caller"
to = "provider"
max-sites = 0
"#
            ),
        )
        .unwrap();

        let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

        assert_eq!(loaded.kind, PassKind::Seam);
        assert_eq!(loaded.max_production_sloc_delta, ceiling);
    }
}

#[test]
fn tooling_contract_loads_a_positive_ceiling() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "main"
kind = "tooling"
paths = ["src"]
max-production-sloc-delta = 160
"#,
    )
    .unwrap();

    let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

    assert_eq!(loaded.kind, PassKind::Tooling);
    assert_eq!(loaded.max_production_sloc_delta, 160);
}

#[test]
fn seam_contract_needs_a_seam_row() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "main"
kind = "seam"
paths = ["src"]
max-production-sloc-delta = 0
"#,
    )
    .unwrap();

    let error = format!(
        "{:#}",
        load(root.path(), &path, &syntax, &syntax).unwrap_err()
    );

    assert!(
        error.contains("needs a [[dependency]] or [[rehome]] row"),
        "{error}"
    );
}

#[test]
fn contract_rejects_unresolved_assembly_modules() {
    let (root, syntax) = syntax();
    let mut contract = contract();
    contract.assembly[0].to = "missing".to_owned();
    assert!(validate(root.path(), &syntax, &syntax, contract).is_err());
}

#[test]
fn v2_contract_loads_esc_and_delete_rows() {
    let (root, current) = syntax();
    let base_sources = [
        Source::new("src/caller.rs", "fn call() {}"),
        Source::new(
            "src/provider.rs",
            "pub const OLD: usize = 1;\npub fn serve() {}",
        ),
    ];
    let base = super::super::syntax::analyze_sources(&base_sources, &BTreeSet::new());
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[esc]]
path = "src"
max = 2

[[delete]]
item = "provider::OLD"
"#,
    )
    .unwrap();

    let loaded = load(root.path(), &path, &current, &base.files).unwrap();

    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.esc.len(), 1);
    assert_eq!(loaded.delete.len(), 1);
}

#[test]
fn v1_contract_is_still_accepted() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 1
base = "main"
paths = ["src"]
max-production-sloc-delta = -1
"#,
    )
    .unwrap();

    let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.kind, PassKind::Module);
    assert!(loaded.esc.is_empty());
    assert!(loaded.delete.is_empty());
    assert!(loaded.rehome.is_empty());
    assert!(loaded.dependency.is_empty());
}

#[test]
fn delete_row_for_item_absent_at_base_is_rejected_at_load() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "HEAD~3"
paths = ["src"]
max-production-sloc-delta = -1

[[delete]]
item = "provider::NEVER_DEFINED"
"#,
    )
    .unwrap();

    let error = load(root.path(), &path, &syntax, &syntax).unwrap_err();

    assert!(
        format!("{error:#}").contains("provider::NEVER_DEFINED` is not defined at base HEAD~3")
    );
}

#[test]
fn v2_contract_loads_rehome_rows() {
    let root = tempfile::tempdir().unwrap();
    let base_sources = [Source::new(
        "src/message.rs",
        "pub struct Thing;\npub fn send() {}",
    )];
    let current_sources = [
        Source::new("src/message.rs", "pub fn send() {}"),
        Source::new("src/store.rs", "pub struct Thing;"),
    ];
    let base = super::super::syntax::analyze_sources(&base_sources, &BTreeSet::new());
    let current = super::super::syntax::analyze_sources(&current_sources, &BTreeSet::new());
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[rehome]]
item = "message::Thing"
to = "store"
"#,
    )
    .unwrap();

    let loaded = load(root.path(), &path, &current.files, &base.files).unwrap();

    assert_eq!(loaded.rehome.len(), 1);
    assert_eq!(loaded.rehome[0].item.as_deref(), Some("message::Thing"));
    assert_eq!(loaded.rehome[0].to, "store");
}

#[test]
fn rehome_row_for_item_absent_at_base_is_rejected_at_load() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "HEAD~3"
paths = ["src"]
max-production-sloc-delta = -1

[[rehome]]
item = "provider::NEVER_DEFINED"
to = "caller"
"#,
    )
    .unwrap();

    let error = load(root.path(), &path, &syntax, &syntax).unwrap_err();

    assert!(
        format!("{error:#}").contains("provider::NEVER_DEFINED` is not defined at base HEAD~3")
    );
}

fn contract_text(rows: &str) -> String {
    format!(
        "version = 2\nbase = 'main'\nkind = 'seam'\npaths = ['src']\nmax-production-sloc-delta = 0\n{rows}"
    )
}

fn load_text(text: &str, current: &[FileSyntax], base: &[FileSyntax]) -> Result<PassContract> {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pass.toml");
    fs::write(&path, text).unwrap();
    load(root.path(), &path, current, base).map_err(|error| anyhow::anyhow!("{error:#}"))
}

fn exec_and_harness() -> Vec<FileSyntax> {
    super::super::syntax::analyze_sources(
        &[
            Source::new(
                "src/cli/exec.rs",
                "pub fn run() {}\nfn settle() {}\nstruct Owner;\nimpl Owner { fn step(&self) {} fn run(&self) {} }",
            ),
            Source::new("src/harness.rs", "pub fn serve() {}"),
        ],
        &BTreeSet::new(),
    )
    .files
}

#[test]
fn delete_and_item_rehome_resolve_private_functions_at_base() {
    let base = exec_and_harness();
    for item in ["cli::exec::settle", "cli::exec::Owner::step", "cli::settle"] {
        for row in [
            format!(
                "[[delete]]\nitem = '{item}'\n[[dependency]]\nfrom = 'cli'\nto = 'harness'\nmax-sites = 0"
            ),
            format!("[[rehome]]\nitem = '{item}'\nto = 'harness'"),
        ] {
            let loaded = load_text(&contract_text(&row), &base, &base);
            assert!(loaded.is_ok(), "{item}: {loaded:?}");
        }
    }
}

#[test]
fn a_pub_item_wins_over_a_same_named_private_method() {
    let base = exec_and_harness();
    let key = "cli::exec::run";
    let definitions = super::super::modules::definitions_for_key(&base, key);
    assert_eq!(definitions.len(), 1);
    assert_eq!((definitions[0].line, definitions[0].owner), (1, None));
    let row = format!("[[rehome]]\nitem = '{key}'\nto = 'harness'");
    assert!(load_text(&contract_text(&row), &base, &base).is_ok());
}

#[test]
fn private_function_keys_report_absence_and_ambiguity_at_base() {
    let base = super::super::syntax::analyze_sources(
        &[Source::new(
            "src/cli/exec.rs",
            "fn settle() {}\nmod a { fn step() {} }\nmod b { fn step() {} }\nstruct O;\nimpl O { fn step(&self) {} }",
        )],
        &BTreeSet::new(),
    )
    .files;
    for (item, message) in [
        ("cli::exec::missing", "is not defined at base main"),
        (
            "cli::exec::step",
            // The method `O::step` stays out: a name-only key names free
            // functions first.
            "ambiguous at base main (2 definitions: src/cli/exec.rs:2, src/cli/exec.rs:3)",
        ),
    ] {
        let row = format!("[[rehome]]\nitem = '{item}'\nto = 'cli'");
        let error = load_text(&contract_text(&row), &base, &base)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn rehome_rows_take_exactly_one_form() {
    let files = exec_and_harness();
    for row in [
        "item = 'cli::exec::settle'\nto = 'harness'",
        "from = 'cli::exec'\nto = 'harness'\nmin-decisions = 40",
    ] {
        let text = contract_text(&format!("[[rehome]]\n{row}"));
        let loaded = load_text(&text, &files, &files);
        assert!(loaded.is_ok(), "{row}: {loaded:?}");
    }
    let loaded = load_text(
        &contract_text("[[rehome]]\nfrom = 'cli::exec'\nto = 'harness'\nmin-decisions = 40"),
        &files,
        &files,
    )
    .unwrap();
    assert!(loaded.needs_metrics());
    assert_eq!(
        loaded.rehome[0].form().unwrap(),
        RehomeForm::Logic {
            from: "cli::exec",
            min_decisions: 40
        }
    );
    for (row, message) in [
        (
            "item = 'cli::exec::settle'\nfrom = 'cli::exec'\nto = 'harness'\nmin-decisions = 1",
            "`item` + `to` (item form), or `from` + `to` + `min-decisions` (logic form)",
        ),
        (
            "item = 'cli::exec::settle'\nto = 'harness'\nmin-decisions = 1",
            "must set exactly one form",
        ),
        ("to = 'harness'", "must set exactly one form"),
        (
            "from = 'cli::exec'\nto = 'harness'",
            "must set exactly one form",
        ),
        (
            "from = 'cli::exec'\nto = 'harness'\nmin-decisions = 0",
            "needs min-decisions >= 1",
        ),
        (
            "from = 'cli'\nto = 'cli::exec'\nmin-decisions = 1",
            "within one another",
        ),
        (
            "from = 'cli::exec'\nto = 'cli'\nmin-decisions = 1",
            "within one another",
        ),
        (
            "from = 'cli exec'\nto = 'harness'\nmin-decisions = 1",
            "rehome.from `cli exec` must be",
        ),
    ] {
        let text = contract_text(&format!("[[rehome]]\n{row}"));
        let error = load_text(&text, &files, &files).unwrap_err().to_string();
        assert!(error.contains(message), "{row}: {error}");
    }
}

#[test]
fn logic_rehome_needs_from_at_base_and_to_on_either_side_inside_paths() {
    let base = exec_and_harness();
    let row = |from: &str, to: &str| {
        contract_text(&format!(
            "[[rehome]]\nfrom = '{from}'\nto = '{to}'\nmin-decisions = 1"
        ))
    };
    let current = super::super::syntax::analyze_sources(
        &[Source::new("src/store/new_owner.rs", "fn run() {}")],
        &BTreeSet::new(),
    )
    .files;
    // A new owner that exists only at current is allowed.
    assert!(load_text(&row("cli::exec", "store"), &current, &base).is_ok());
    for (from, to, current, message) in [
        (
            "store",
            "harness",
            &base,
            "rehome.from `store` has no production files inside pass contract paths at base main",
        ),
        (
            "cli::exec",
            "missing",
            &base,
            "rehome.to `missing` has no production files",
        ),
        (
            "cli::exec",
            "store",
            &base,
            "rehome.to `store` has no production files",
        ),
    ] {
        let error = load_text(&row(from, to), current, &base)
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
    }
    let outside = super::super::syntax::analyze_sources(
        &[
            Source::new("elsewhere/src/cli/exec.rs", "fn run() {}"),
            Source::new("src/harness.rs", "fn run() {}"),
        ],
        &BTreeSet::new(),
    )
    .files;
    let error = load_text(&row("cli::exec", "harness"), &outside, &outside)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("rehome.from `cli::exec` has no production files"),
        "{error}"
    );
}

#[test]
fn thin_cli_kind_is_no_longer_a_pass_kind() {
    let error = toml::from_str::<PassContract>(
        "version = 2\nbase = 'main'\nkind = 'thin-cli'\npaths = ['src']\nmax-production-sloc-delta = 0",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unknown variant `thin-cli`"), "{error}");
}

#[test]
fn v2_contract_loads_dependency_rows() {
    let (root, syntax) = syntax();
    let path = root.path().join("pass.toml");
    fs::write(
        &path,
        r#"version = 2
base = "main"
paths = ["src"]
max-production-sloc-delta = -1

[[dependency]]
from = "caller"
to = "provider"
max-sites = 0
"#,
    )
    .unwrap();

    let loaded = load(root.path(), &path, &syntax, &syntax).unwrap();

    assert_eq!(loaded.dependency.len(), 1);
    assert_eq!(loaded.dependency[0].from, "caller");
    assert_eq!(loaded.dependency[0].to, "provider");
    assert_eq!(loaded.dependency[0].max_sites, 0);
}
