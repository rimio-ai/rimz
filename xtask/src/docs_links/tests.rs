use super::*;

#[test]
fn slugifies_headings_like_github() {
    assert_eq!(slugify("Sidebar Rendering"), "sidebar-rendering");
    assert_eq!(
        slugify("`config.toml` Per Machine"),
        "configtoml-per-machine"
    );
    assert_eq!(slugify("Sidecars And Privacy"), "sidecars-and-privacy");
}

#[test]
fn strip_md_links_keeps_visible_text() {
    assert_eq!(
        strip_md_links("`crates/rimz` — [local contract](./crates/rimz/AGENTS.md)"),
        "`crates/rimz` — local contract"
    );
    assert_eq!(strip_md_links("no links here"), "no links here");
}

#[test]
fn link_targets_skip_external_and_titles() {
    let line = "see [a](./a.md#x) and [b](../b.md) and [c](https://example.com)";
    assert_eq!(link_targets_in_line(line), vec!["./a.md#x", "../b.md"]);
}

#[test]
fn heading_slugs_skip_fenced_code() {
    let doc = "# Title\n```sh\n# not a heading\n```\n## Real Heading\n";
    let slugs = heading_slugs(doc);
    assert!(slugs.contains("title"));
    assert!(slugs.contains("real-heading"));
    assert!(!slugs.contains("not-a-heading"));
}
