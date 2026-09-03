use std::collections::BTreeSet;

use super::super::super::sources::Source;
use super::super::testkit::{edge, selector};
use super::*;

#[test]
fn functions_rank_by_distinct_items_and_quote_the_heaviest() {
    let statements = (0..81)
        .map(|index| format!("    let value_{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = Source::new(
        "crates/demo/src/caller.rs",
        format!("fn heavy() {{\n{statements}\n}}\n\nfn light() {{\n    let value = 1;\n}}\n"),
    );
    let syntax = super::super::super::syntax::analyze_sources(
        std::slice::from_ref(&source),
        &BTreeSet::new(),
    );
    let heavy = syntax.files[0]
        .fns
        .iter()
        .find(|function| function.name == "heavy")
        .unwrap();
    let light = syntax.files[0]
        .fns
        .iter()
        .find(|function| function.name == "light")
        .unwrap();
    let mut edges = vec![
        edge("a", "caller", Some(("heavy", heavy.line)), false),
        edge("b", "caller", Some(("heavy", heavy.line)), false),
        edge("a", "caller", Some(("heavy", heavy.line)), false),
        edge("c", "caller", Some(("light", light.line)), false),
    ];
    edges[0].from_line = heavy.line + 10;
    edges[1].from_line = heavy.line + 40;
    edges[2].from_line = heavy.line + 80;

    let functions = assembly_functions(
        &edges,
        &syntax.files,
        &selector("caller"),
        &selector("store"),
    );

    assert_eq!(functions[0].function, "heavy");
    assert_eq!(functions[0].items, ["a", "b"]);
    assert_eq!(functions[0].sites, 3);
    let heaviest = quote_function(&functions[0], &[source]).unwrap();
    assert_eq!(heaviest.function, "heavy");
    assert_eq!(
        heaviest.site_lines,
        [heavy.line + 10, heavy.line + 40, heavy.line + 80]
    );
    assert!(heaviest.source.starts_with("fn heavy() {"));
    assert!(heaviest.source.contains("let value_8 = 8;"));
    assert!(heaviest.source.contains("let value_39 = 39;"));
    assert!(heaviest.source.contains("let value_79 = 79;"));
    assert!(!heaviest.source.contains("let value_20 = 20;"));
    assert!(heaviest.source.contains("… 27 lines"));
}

#[test]
fn heaviest_quote_keeps_a_short_function_whole_and_short_gaps_inline() {
    let statements = (0..120)
        .map(|index| format!("    let value_{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = Source::new(
        "crates/demo/src/caller.rs",
        format!(
            "fn heavy() {{\n{statements}\n}}\n\nfn short() {{\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}}\n"
        ),
    );
    let row = |name: &str, line, end_line, site_lines: Vec<usize>| FunctionRow {
        function: name.to_owned(),
        path: source.path.clone(),
        line,
        end_line,
        items: vec!["a".to_owned()],
        items_unfolded: 1,
        sites: site_lines.len(),
        site_lines,
        also: Vec::new(),
    };

    let short = quote_function(
        &row("short", 124, 128, vec![126]),
        std::slice::from_ref(&source),
    )
    .unwrap();
    assert_eq!(short.source.lines().count(), 5, "{}", short.source);
    assert!(!short.source.contains('…'));

    // Sites 30 and 38 sit 6 lines apart: quoted through; 38 to 90 is elided.
    let long = quote_function(&row("heavy", 1, 122, vec![30, 38, 90]), &[source]).unwrap();
    assert!(
        long.source.contains("let value_32 = 32;"),
        "{}",
        long.source
    );
    assert!(!long.source.contains("let value_60 = 60;"));
    assert!(long.source.contains("… 49 lines"), "{}", long.source);
}

#[test]
fn heaviest_quote_caps_site_windows_and_reports_omitted_sites() {
    let statements = (0..200)
        .map(|index| format!("    let value_{index} = {index};"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = Source::new(
        "crates/demo/src/caller.rs",
        format!("fn heavy() {{\n{statements}\n}}\n"),
    );
    let site_lines = (5..=150).step_by(5).collect::<Vec<_>>();
    let function = FunctionRow {
        function: "heavy".to_owned(),
        path: source.path.clone(),
        line: 1,
        end_line: 202,
        items: vec!["a".to_owned()],
        items_unfolded: 1,
        sites: site_lines.len(),
        site_lines,
        also: Vec::new(),
    };

    let quote = quote_function(&function, &[source]).unwrap();

    assert!(quote.source.starts_with("fn heavy() {"));
    assert!(quote.source.lines().last().unwrap().contains("more sites"));
    assert!(quote.source.contains("let value_2 = 2;"));
    assert!(!quote.source.contains("let value_148 = 148;"));
    assert!(
        quote
            .source
            .lines()
            .filter(|line| !line.starts_with('…'))
            .count()
            <= 80
    );
}

#[test]
fn call_shapes_group_functions_by_ordered_sequence_and_skip_single_items() {
    let mut edges = Vec::new();
    for (item, line) in [("A", 11), ("B", 12), ("B", 13), ("C", 14)] {
        let mut edge = edge(item, "left", Some(("build", 10)), false);
        edge.from_line = line;
        edges.push(edge);
    }
    for (item, line) in [("A", 21), ("B", 22), ("C", 23)] {
        let mut edge = edge(item, "right", Some(("assemble", 20)), false);
        edge.from_line = line;
        edges.push(edge);
    }
    for (item, line) in [("C", 41), ("A", 42)] {
        let mut edge = edge(item, "left", Some(("other", 40)), false);
        edge.from_line = line;
        edges.push(edge);
    }
    for (item, line) in [("E", 61), ("D", 62), ("C", 63), ("B", 64), ("A", 65)] {
        let mut edge = edge(item, "right", Some(("heavy", 60)), false);
        edge.from_line = line;
        edges.push(edge);
    }
    edges.push(edge("A", "right", Some(("restore", 30)), false));
    edges.push(edge("A", "store", Some(("inside", 50)), false));

    let rows = call_shapes(&edges, &selector("store"), &[]);

    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].shape, ["A", "B", "C"]);
    assert_eq!(rows[0].items, 3);
    assert_eq!(rows[0].functions.len(), 2);
    assert_eq!(rows[1].shape, ["E", "D", "C", "B", "A"]);
    assert_eq!(rows[1].functions.len(), 1);
}

#[test]
fn inspect_groups_repeated_assembly_across_caller_modules() {
    let edges = ["A", "B", "C", "D"]
        .into_iter()
        .map(|item| edge(item, "left", Some(("build", 10)), false))
        .chain(
            ["A", "B", "C", "D"]
                .into_iter()
                .map(|item| edge(item, "right", Some(("assemble", 20)), false)),
        )
        .chain(
            ["A", "B", "C"]
                .into_iter()
                .map(|item| edge(item, "right", Some(("restore", 30)), false)),
        )
        .collect::<Vec<_>>();

    let rows = repeated_assembly(&edges, &selector("store"), &[]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].items, ["A", "B", "C"]);
    assert_eq!(rows[0].functions.len(), 3);
    assert_eq!(rows[0].score, 9);
    assert_eq!(rows[0].children.len(), 1);
    assert_eq!(rows[0].children[0].extra_items, ["D"]);
    assert_eq!(rows[0].children[0].functions.len(), 2);
}

#[test]
fn builder_chain_folds_for_callers_heaviest_and_call_shapes() {
    let target = Source::new(
        "crates/demo/src/store.rs",
        "pub struct MessageRecord;\nimpl MessageRecord {\n    pub fn new() {}\n    pub fn with_channel() {}\n    pub fn with_sender() {}\n    pub fn with_automated() {}\n    pub fn with_body() {}\n    pub fn with_pane_id() {}\n}\npub fn load() {}\npub fn route() {}\npub fn park() {}\npub fn send() {}\npub fn wake() {}\npub fn record() {}\n",
    );
    let caller = Source::new(
        "crates/demo/src/caller.rs",
        "fn run_idle_compact() {\n    // target references supplied by the fixture\n}\n",
    );
    let syntax = super::super::super::syntax::analyze_sources(&[target, caller], &BTreeSet::new());
    let target_file = syntax
        .files
        .iter()
        .find(|file| file.module_path == "store")
        .unwrap();
    let names = [
        "MessageRecord",
        "new",
        "with_channel",
        "with_sender",
        "with_automated",
        "with_body",
        "with_pane_id",
        "load",
        "route",
        "park",
        "send",
        "wake",
        "record",
    ];
    let mut edges = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut reference = edge(name, "caller", Some(("run_idle_compact", 1)), false);
            reference.from_line = index + 2;
            reference.to_line = target_file
                .pub_items
                .iter()
                .find(|item| item.name == *name)
                .unwrap()
                .line;
            reference
        })
        .collect::<Vec<_>>();
    for (item, line) in [("queue", 1), ("deliver", 2), ("queue", 1)] {
        let mut reference = edge(item, "caller", Some(("run_idle_compact", 1)), false);
        reference.from_line = names.len() + line + 2;
        reference.to = "third::service".to_owned();
        reference.to_path = PathBuf::from("crates/demo/src/third/service.rs");
        reference.to_line = line;
        edges.push(reference);
    }
    edges.sort_by_key(|edge| edge.from_line);

    let assembly = assembly_functions(
        &edges,
        &syntax.files,
        &selector("caller"),
        &selector("store"),
    );
    assert_eq!(assembly[0].items.len(), 7, "{:?}", assembly[0].items);
    assert_eq!(assembly[0].items_unfolded, 13);
    assert_eq!(assembly[0].also, [("third".to_owned(), 2)]);
    assert_eq!(
        assembly[0].items[0],
        "MessageRecord::{new, with_channel, with_sender, +3}"
    );

    let callers = callers_from_edges(&edges, &selector("store"), &syntax.files);
    assert_eq!(callers[0].max_fn_items, 7);
    assert_eq!(callers[0].top_fns[0].items_unfolded, 13);

    let shapes = call_shapes(&edges, &selector("store"), &syntax.files);
    assert_eq!(shapes[0].items, 7);
    assert_eq!(shapes[0].items_unfolded, 13);
    assert_eq!(shapes[0].functions[0].items, 7);
    assert_eq!(shapes[0].functions[0].items_unfolded, 13);
}

#[test]
fn type_aliases_are_not_assembly_items() {
    let target = Source::new(
        "crates/demo/src/store.rs",
        "pub type Result<T> = std::result::Result<T, ()>;\npub fn load() {}\npub fn route() {}\npub fn park() {}\npub fn send() {}\npub fn wake() {}\n",
    );
    let caller = Source::new(
        "crates/demo/src/caller.rs",
        "fn assemble() {\n    // target references supplied by the fixture\n}\n",
    );
    let syntax = super::super::super::syntax::analyze_sources(&[target, caller], &BTreeSet::new());
    let target_file = syntax
        .files
        .iter()
        .find(|file| file.module_path == "store")
        .unwrap();
    let edges = ["Result", "load", "route", "park", "send", "wake"]
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut reference = edge(name, "caller", Some(("assemble", 1)), false);
            reference.from_line = index + 2;
            reference.to_line = target_file
                .pub_items
                .iter()
                .find(|item| item.name == *name)
                .unwrap()
                .line;
            reference
        })
        .collect::<Vec<_>>();

    let assembly = assembly_functions(
        &edges,
        &syntax.files,
        &selector("caller"),
        &selector("store"),
    );
    assert!(!assembly[0].items.iter().any(|item| item == "Result"));
    assert_eq!(assembly[0].items_unfolded, 5);
    let shapes = call_shapes(&edges, &selector("store"), &syntax.files);
    assert!(!shapes[0].shape.iter().any(|item| item == "Result"));
    assert_eq!(shapes[0].items_unfolded, 5);
}
