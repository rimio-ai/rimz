use super::*;
use crate::atlas::{sources::Source, syntax};

#[test]
fn module_rollup_follows_the_requested_scope() {
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
            Path::new("crates/rimz/src/cli")
        ),
        "agents_cmd"
    );
    assert_eq!(
        crate_module_for_row(Path::new("crates/rimz/src/cli"), "agents_cmd"),
        "cli::agents_cmd"
    );
    assert_eq!(
        crate_module_for_path(Path::new("crates/rimz/src/cli/agents_cmd/mod.rs")),
        "cli::agents_cmd"
    );
    assert_eq!(
        rust_module_for_path(
            Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
            Path::new("crates/rimz/src/cli")
        )
        .as_deref(),
        Some("agents_cmd")
    );
    assert_eq!(
        rust_module_for_path(
            Path::new("crates/rimz/src/cli/snapshots/surface.snap"),
            Path::new("crates/rimz/src/cli")
        ),
        None
    );
    assert_eq!(
        module_for_path(
            Path::new("crates/rimz/src/cli/surface_tests.rs"),
            Path::new("crates/rimz/src/cli")
        ),
        "(root)"
    );
}

#[test]
fn crate_modules_fall_back_to_parent_and_stem_outside_src() {
    assert_eq!(
        crate_module_for_path(Path::new("crates/rimz/examples/seed_perf_workspace.rs")),
        "examples::seed_perf_workspace"
    );
    assert_eq!(
        crate_module_for_path(Path::new("crates/rimz/benches/hotpath.rs")),
        "benches::hotpath"
    );
}

#[test]
fn declaration_only_kinds_are_mods_and_uses() {
    assert!(is_declaration_only("mod"));
    assert!(is_declaration_only("use"));
    assert!(!is_declaration_only("fn"));
}

#[test]
fn escaping_items_preserve_duplicate_cross_revision_identities() {
    let sources = vec![Source::new(
        "src/agents.rs",
        "pub struct A;\npub struct B;\nimpl A { pub fn name() {} }\nimpl B { pub fn name() {} }\n",
    )];
    let syntax = syntax::analyze_sources(&sources, &BTreeSet::new());
    let index = syntax::ModIndex::new(&syntax.files);

    let files = syntax.files.iter().collect::<Vec<_>>();
    let items = escaping_items(&files, Path::new("src"), &index);

    assert_eq!(items["agents"].len(), 4);
    assert_eq!(
        items["agents"]
            .iter()
            .filter(|item| item.id.name == "name")
            .count(),
        2
    );
}

#[test]
fn manifest_crate_names_follow_lib_override_and_rust_normalization() {
    assert_eq!(
        crate_name_from_manifest("[package]\nname = \"rimz-presence-zellij\"\n")
            .unwrap()
            .as_deref(),
        Some("rimz_presence_zellij")
    );
    assert_eq!(
        crate_name_from_manifest(
            "[package]\nname = \"package-name\"\n[lib]\nname = \"import_name\"\n"
        )
        .unwrap()
        .as_deref(),
        Some("import_name")
    );
}

#[test]
fn module_containment_models_crate_external_reach() {
    assert!(module_is_within("store", EXTERNAL_REACH));
    assert!(module_is_within("", EXTERNAL_REACH));
    assert!(module_is_within(EXTERNAL_REACH, EXTERNAL_REACH));
    assert!(!module_is_within(EXTERNAL_REACH, ""));
    assert!(module_is_within("store::event_log", "store"));
    assert!(!module_is_within("storehouse", "store"));
}

#[test]
fn module_endpoints_distinguish_scope_root_from_crate_root() {
    assert_eq!(module_endpoint("", ""), "(root)");
    assert_eq!(module_endpoint("", "agents"), "(crate)");
    assert_eq!(module_endpoint("(crate)", ""), "(root)");
    assert_eq!(module_endpoint("agents", "agents"), "(root)");
    assert_eq!(module_endpoint("agents::adapters", "agents"), "adapters");
    assert_eq!(module_endpoint("(crate)", "agents"), "(crate)");
}

#[test]
fn reference_labels_preserve_exact_non_root_modules() {
    assert_eq!(reference_module_label("", ""), "(root)");
    assert_eq!(reference_module_label("", "agents"), "(crate)");
    assert_eq!(
        reference_module_label("cli::agents_cmd", "agents"),
        "cli::agents_cmd"
    );
}

mod key_tests {
    use std::collections::BTreeSet;

    use super::super::*;
    use crate::atlas::{sources::Source, syntax};

    fn files(sources: &[(&str, &str)]) -> Vec<FileSyntax> {
        let sources = sources
            .iter()
            .map(|(path, text)| Source::new(*path, *text))
            .collect::<Vec<_>>();
        syntax::analyze_sources(&sources, &BTreeSet::new()).files
    }

    #[test]
    fn items_for_key_prefers_the_named_module_then_searches_beneath() {
        let files = files(&[
            (
                "crates/demo/src/message.rs",
                "pub mod deliver;\npub fn send() {}\n",
            ),
            (
                "crates/demo/src/message/deliver.rs",
                "pub fn queue_synthetic() {}\npub fn send() {}\n",
            ),
        ]);
        let names = |key: &str| {
            items_for_key(&files, key)
                .into_iter()
                .map(|(_, item)| format!("{}::{}", item.module, item.name))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("message::queue_synthetic"),
            ["message::deliver::queue_synthetic"]
        );
        assert_eq!(names("message::send"), ["message::send"]);
        assert_eq!(names("message::deliver::send"), ["message::deliver::send"]);
        assert_eq!(names("send"), ["message::send", "message::deliver::send"]);
        assert!(names("message::missing").is_empty());
        assert!(names("cli::send").is_empty());
    }

    fn resolved(files: &[FileSyntax], key: &str) -> Vec<String> {
        definitions_for_key(files, key)
            .into_iter()
            .map(|definition| {
                let tier = match definition.resolved {
                    Resolved::Visible(item) => format!("visible {}", item.module),
                    Resolved::Function(function) => format!("function {}", function.module),
                    Resolved::Private(item) => format!("private {}", item.module),
                };
                definition
                    .owner
                    .map_or(tier.clone(), |owner| format!("{tier} (owner {owner})"))
            })
            .collect()
    }

    #[test]
    fn definitions_for_key_places_items_in_their_inline_module() {
        let files = files(&[(
            "crates/demo/src/config.rs",
            "mod ttl_serde { fn deserialize() {} struct Wire; }\nmod other { fn deserialize() {} }\n",
        )]);
        assert_eq!(
            resolved(&files, "config::ttl_serde::deserialize"),
            ["function config::ttl_serde"]
        );
        assert_eq!(
            resolved(&files, "config::ttl_serde::Wire"),
            ["private config::ttl_serde"]
        );
        assert_eq!(
            resolved(&files, "config::deserialize"),
            ["function config::ttl_serde", "function config::other"]
        );
    }

    #[test]
    fn definitions_for_key_resolves_private_items_last() {
        let files = files(&[
            (
                "crates/demo/src/disk.rs",
                "mod usage;\nstruct Shadowed {}\n",
            ),
            (
                "crates/demo/src/disk/usage.rs",
                "struct FileIdentity { dev: u64 }\nenum RequiredDecision { Run }\ntrait SidebarMux {}\nconst LIMIT: usize = 1;\nstatic NAME: &str = \"\";\ntype Alias = usize;\npub struct Shadowed;\nstruct probe {}\nfn helper() {}\n#[cfg(test)]\nstruct OnlyInTests;\n#[cfg(test)]\nmod tests {\n    struct Fixture;\n}\n",
            ),
            ("crates/demo/src/probe.rs", "fn probe() {}\n"),
        ]);
        let usage = files
            .iter()
            .find(|file| file.module_path == "disk::usage")
            .expect("usage file");
        assert_eq!(
            usage
                .pub_items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Shadowed"],
            "private items leave the visible surface alone"
        );
        assert_eq!(
            usage
                .private_items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            [
                "FileIdentity",
                "RequiredDecision",
                "SidebarMux",
                "LIMIT",
                "NAME",
                "Alias",
                "probe",
            ],
            "test-only items and functions are not private items"
        );

        for key in [
            "disk::usage::FileIdentity",
            "disk::usage::RequiredDecision",
            "disk::usage::SidebarMux",
            "disk::usage::LIMIT",
            "disk::usage::NAME",
            "disk::usage::Alias",
            "disk::FileIdentity",
            "FileIdentity",
        ] {
            assert_eq!(resolved(&files, key), ["private disk::usage"], "{key}");
        }
        assert_eq!(
            resolved(&files, "disk::Shadowed"),
            ["visible disk::usage"],
            "a visible item beneath the module wins over a private one in it"
        );
        assert_eq!(
            resolved(&files, "probe"),
            ["function probe"],
            "a production function wins over a same-named private item"
        );
        assert!(resolved(&files, "disk::usage::OnlyInTests").is_empty());
        assert!(resolved(&files, "disk::usage::Fixture").is_empty());
        assert!(resolved(&files, "disk::usage::Missing").is_empty());
        assert!(resolved(&files, "cli::FileIdentity").is_empty());
    }

    #[test]
    fn definitions_for_key_accepts_owner_keys_and_child_module_methods() {
        let files = files(&[
            (
                "crates/demo/src/lsp/check.rs",
                "pub struct Report;\nimpl Report {\n    pub fn render(&self) {}\n}\n",
            ),
            ("crates/demo/src/mux/zellij.rs", "mod presence;\n"),
            (
                "crates/demo/src/mux/zellij/presence.rs",
                "pub struct Zellij;\nimpl Zellij {\n    pub fn converge_presence_plugin_for(&self) {}\n    fn converge_presence_plugin_for_with(&self) {}\n}\n",
            ),
        ]);
        assert_eq!(
            resolved(&files, "lsp::check::Report::render"),
            ["function lsp::check (owner Report)"]
        );
        assert_eq!(
            resolved(&files, "lsp::check::render"),
            ["visible lsp::check (owner Report)"],
            "the name-only method key stays valid"
        );
        assert!(resolved(&files, "lsp::check::Other::render").is_empty());
        assert_eq!(
            resolved(&files, "mux::zellij::converge_presence_plugin_for"),
            ["visible mux::zellij::presence (owner Zellij)"]
        );
        assert_eq!(
            resolved(&files, "mux::zellij::converge_presence_plugin_for_with"),
            ["function mux::zellij::presence (owner Zellij)"]
        );
        assert_eq!(
            resolved(
                &files,
                "mux::zellij::Zellij::converge_presence_plugin_for_with"
            ),
            ["function mux::zellij::presence (owner Zellij)"]
        );
        assert!(resolved(&files, "mux::zellij::Other::converge_presence_plugin_for").is_empty());
        assert!(resolved(&files, "mux::zellij::converge_gone").is_empty());
    }

    #[test]
    fn cfg_alternatives_resolve_to_their_first_definition() {
        let files = files(&[
            (
                "crates/demo/src/observability.rs",
                "#[cfg(feature = \"sentry\")]\nmod reporting;\n#[cfg(feature = \"sentry\")]\npub use reporting::{Reporting, init};\n\n#[cfg(not(feature = \"sentry\"))]\nmod disabled;\n#[cfg(not(feature = \"sentry\"))]\npub use disabled::{Reporting, init};\n",
            ),
            (
                "crates/demo/src/observability/reporting.rs",
                "pub struct Reporting;\npub fn init() {}\n",
            ),
            (
                "crates/demo/src/observability/disabled.rs",
                "pub struct Reporting;\npub fn init() {}\n",
            ),
            (
                "crates/demo/src/proc/mod.rs",
                "#[cfg(target_os = \"macos\")]\npub use macos::{argv};\n\n#[cfg(target_os = \"linux\")]\npub fn argv() {}\n\n#[cfg(not(any(target_os = \"linux\", target_os = \"macos\")))]\npub fn argv() {}\n\npub struct Left;\npub struct Right;\n#[cfg(unix)]\nimpl Left {\n    pub fn host() {}\n}\n#[cfg(windows)]\nimpl Right {\n    pub fn host() {}\n}\nimpl Left {\n    #[cfg(unix)]\n    pub fn twin() {}\n    pub fn plain() {}\n    #[cfg(unix)]\n    pub fn mixed() {}\n}\nimpl Right {\n    #[cfg(unix)]\n    pub fn twin() {}\n    pub fn plain() {}\n    pub fn mixed() {}\n}\n",
            ),
        ]);
        let sites = |key: &str| {
            definitions_for_key(&files, key)
                .into_iter()
                .map(|definition| format!("{}:{}", definition.file.path.display(), definition.line))
                .collect::<Vec<_>>()
        };
        let observability = files
            .iter()
            .find(|file| file.module_path == "observability")
            .expect("observability file");
        assert_eq!(
            observability
                .pub_items
                .iter()
                .filter_map(|item| item.cfg.as_deref())
                .collect::<Vec<_>>(),
            [
                "feature=\"sentry\"",
                "feature=\"sentry\"",
                "not(feature=\"sentry\")",
                "not(feature=\"sentry\")",
            ],
            "the gate is the normalized token text; the private mods add no visible item"
        );

        // A cfg-gated `pub use` pair: the sentry/not-sentry `observability` shape.
        assert_eq!(
            sites("observability::init"),
            ["crates/demo/src/observability.rs:3"]
        );
        assert_eq!(
            sites("observability::Reporting"),
            ["crates/demo/src/observability.rs:3"]
        );
        // A cfg-gated `pub use` against cfg-gated `pub fn`s: the `proc`
        // target_os shape, first in file order.
        assert_eq!(sites("proc::argv"), ["crates/demo/src/proc/mod.rs:1"]);
        // An `impl` gate reaches its methods.
        assert_eq!(sites("proc::host"), ["crates/demo/src/proc/mod.rs:14"]);
        // Same-cfg, ungated, or partly gated definitions stay several.
        assert_eq!(sites("proc::twin").len(), 2);
        assert_eq!(sites("proc::plain").len(), 2);
        assert_eq!(sites("proc::mixed").len(), 2);
    }

    #[test]
    fn name_only_keys_name_free_items_before_methods() {
        let files = files(&[
            (
                "crates/demo/src/store/mod.rs",
                "pub mod snapshot;\npub struct Store;\nimpl Store {\n    pub fn snapshot(&self) {}\n    pub fn open() {}\n    fn load(&self) {}\n    fn reload(&self) {}\n}\npub trait Source {\n    fn fetch(&self);\n}\nfn reload() {}\n",
            ),
            (
                "crates/demo/src/store/snapshot.rs",
                "pub struct Snapshot;\n",
            ),
        ]);
        let sites = |key: &str| {
            definitions_for_key(&files, key)
                .into_iter()
                .map(|definition| {
                    format!(
                        "{}{}",
                        definition.line,
                        definition
                            .owner
                            .map_or(String::new(), |owner| format!(" ({owner})"))
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            sites("store::snapshot"),
            ["1"],
            "the module, not the method"
        );
        assert_eq!(sites("store::Store::snapshot"), ["4 (Store)"]);
        assert_eq!(sites("store::reload"), ["12"], "the free function");
        assert_eq!(sites("store::Store::reload"), ["7 (Store)"]);
        // With no free item of the name, a name-only key still names the
        // method.
        assert_eq!(sites("store::open"), ["5 (Store)"]);
        assert_eq!(sites("store::load"), ["6 (Store)"]);
        assert_eq!(sites("store::fetch"), ["10"]);
    }

    #[test]
    fn owner_keys_name_inherent_methods_before_trait_impl_methods() {
        let files = files(&[(
            "crates/demo/src/agents/source.rs",
            "pub struct Source;\nimpl Source {\n    fn present(&self) -> bool { true }\n}\ntrait Integration {\n    fn present(&self) -> bool;\n    fn install(&self);\n}\nimpl Integration for Source {\n    fn present(&self) -> bool { false }\n    fn install(&self) {}\n}\n",
        )]);
        let sites = |key: &str| {
            definitions_for_key(&files, key)
                .into_iter()
                .map(|definition| {
                    let owner = definition.owner.unwrap_or("-");
                    format!("{} {owner}", definition.line)
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            sites("agents::source::Source::present"),
            ["3 Source"],
            "the inherent method, not the trait impl's"
        );
        // With no inherent method of the name, the owner key still names the
        // trait impl's method.
        assert_eq!(sites("agents::source::Source::install"), ["11 Source"]);
    }

    #[test]
    fn reexports_resolve_to_definitions_through_chains_and_globs() {
        let files = files(&[
            (
                "crates/demo/src/store.rs",
                "mod snapshot;\npub use snapshot::Snapshot;\npub use snapshot::*;\npub use crate::ids::Id;\npub use anyhow::Result;\npub use snapshot::Missing;\n",
            ),
            (
                "crates/demo/src/store/snapshot.rs",
                "mod row;\npub use row::{Row as Snapshot, Extra};\n",
            ),
            (
                "crates/demo/src/store/snapshot/row.rs",
                "pub struct Row;\npub struct Extra;\n",
            ),
            ("crates/demo/src/ids.rs", "pub struct Id;\n"),
        ]);
        let store = files
            .iter()
            .find(|file| file.module_path == "store")
            .expect("store file");
        let item = |name: &str| {
            store
                .pub_items
                .iter()
                .find(|item| item.name == name)
                .expect("store re-exports the name")
        };
        let definition = |name: &str| match resolve_reexport(&files, item(name)) {
            ReExport::Definition(file, definition) => {
                format!("{}::{}", file.module_path, definition.name)
            }
            other => panic!("{name} resolved to {other:?}"),
        };
        assert_eq!(definition("Snapshot"), "store::snapshot::row::Row");
        assert_eq!(definition("Id"), "ids::Id");
        let ReExport::Glob(items) = resolve_reexport(&files, item("*")) else {
            panic!("a glob resolves to a glob");
        };
        assert_eq!(
            items
                .iter()
                .map(|(_, item)| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Row", "Extra"]
        );
        assert!(matches!(
            resolve_reexport(&files, item("Result")),
            ReExport::Foreign
        ));
        assert!(matches!(
            resolve_reexport(&files, item("Missing")),
            ReExport::Unresolved
        ));
    }
}
