use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;

fn noon() -> Zoned {
    "2026-11-12T12:30:05.250-06:00"
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(-6)))
}

fn request(kind: &str, title: &str, body: &[u8]) -> AddRequest {
    AddRequest {
        kind: kind.into(),
        title: title.into(),
        body: body.to_vec(),
        ..AddRequest::default()
    }
}

fn agent_facts() -> Vec<(&'static str, String)> {
    let env = BTreeMap::from([
        ("RIMZ_AGENT_NAME", "level-spark"),
        ("RIMZ_AGENT_PROFILE", "coder"),
        ("RIMZ_TEAM", "forge"),
        ("RIMZ_AGENT_ROLE", "coder"),
        ("RIMZ_AGENT_MODEL", "opus"),
        ("RIMZ_WORKTREE_PATH", "/src/repo-wt"),
        ("RIMZ_CHANNEL", "notes"),
        ("RIMZ_LOOP_TASK", "nightly"),
        ("RIMZ_WORKFLOW_RUN", "wr_1"),
    ]);
    caller_facts(
        |key| env.get(key).map(|value| (*value).to_owned()),
        Some("/src/repo".into()),
    )
}

fn store() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("notes");
    (temp, root)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_note_carries_every_known_fact_in_order_above_the_body_as_read() {
    let (_temp, root) = store();
    let body = b"---\nown: front matter\n---\n# Body\n";
    let added = add_at(
        &root,
        &AddRequest {
            meta: vec!["topic=notes".into(), "ticket=42".into()],
            ..request("Hand Off", "Ship the notes command", body)
        },
        &agent_facts(),
        &noon(),
    )
    .unwrap();

    let day = root.join("hand-off/20261112");
    assert_eq!(added.path, day.join("ship-the-notes-command.md"));
    assert!(added.dropped_meta.is_empty());
    let mut expected = concat!(
        "---\n",
        "kind: \"hand-off\"\n",
        "title: \"Ship the notes command\"\n",
        "written: \"2026-11-12T12:30:05.250-06:00\"\n",
        "author: \"level-spark\"\n",
        "profile: \"coder\"\n",
        "team: \"forge\"\n",
        "role: \"coder\"\n",
        "model: \"opus\"\n",
        "project: \"/src/repo\"\n",
        "worktree: \"/src/repo-wt\"\n",
        "channel: \"notes\"\n",
        "loop-task: \"nightly\"\n",
        "workflow-run: \"wr_1\"\n",
        "topic: \"notes\"\n",
        "ticket: \"42\"\n",
        "---\n",
    )
    .as_bytes()
    .to_vec();
    expected.extend_from_slice(body);
    assert_eq!(std::fs::read(&added.path).unwrap(), expected);
    for level in [&root, &root.join("hand-off"), &day] {
        assert_eq!(mode(level), 0o700, "{}", level.display());
    }
    assert_eq!(mode(&added.path), 0o600);
}

#[test]
fn empty_launch_variables_and_a_plain_shell_write_no_field() {
    let env = BTreeMap::from([
        ("RIMZ_CHANNEL", ""),
        ("RIMZ_AGENT_ID", ""),
        ("RIMZ_LOOP_TASK", ""),
        ("RIMZ_AGENT_NAME", ""),
    ]);
    let facts = caller_facts(
        |key| env.get(key).map(|value| (*value).to_owned()),
        Some("/from/cwd".into()),
    );
    assert_eq!(facts, [("project", "/from/cwd".to_owned())]);
    assert_eq!(caller_facts(|_| None, None), []);

    let (_temp, root) = store();
    let added = add_at(
        &root,
        &request("diary", "Day one", b"text\n"),
        &facts,
        &noon(),
    )
    .unwrap();
    let text = std::fs::read_to_string(added.path).unwrap();
    assert!(!text.contains("author"), "{text}");
    assert!(!text.contains("channel"), "{text}");
    assert!(!text.contains("loop-task"), "{text}");
}

#[test]
fn a_headless_check_names_its_workflow_run_and_no_author() {
    let env = BTreeMap::from([("RIMZ_WORKFLOW_RUN", "wr_7")]);
    let facts = caller_facts(
        |key| env.get(key).map(|value| (*value).to_owned()),
        Some("/src/repo".into()),
    );
    assert_eq!(
        facts,
        [
            ("workflow-run", "wr_7".to_owned()),
            ("project", "/src/repo".to_owned())
        ]
    );
}

#[test]
fn a_taken_name_gets_the_next_suffix_and_keeps_its_own_note() {
    let (_temp, root) = store();
    let paths: Vec<_> = (1..=3)
        .map(|n| {
            add_at(
                &root,
                &request("molt", "Same title", format!("body {n}\n").as_bytes()),
                &[],
                &noon(),
            )
            .unwrap()
            .path
        })
        .collect();
    let day = root.join("molt/20261112");
    assert_eq!(
        paths,
        [
            day.join("same-title.md"),
            day.join("same-title-2.md"),
            day.join("same-title-3.md")
        ]
    );
    for (n, path) in paths.iter().enumerate() {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.ends_with(&format!("---\nbody {}\n", n + 1)), "{text}");
    }
}

#[test]
fn racing_claims_on_one_name_get_two_files() {
    for _ in 0..20 {
        let (_temp, root) = store();
        let (first, second) = std::thread::scope(|scope| {
            let claim = |body: &'static [u8]| {
                let root = &root;
                scope.spawn(move || {
                    add_at(root, &request("molt", "Race", body), &[], &noon())
                        .unwrap()
                        .path
                })
            };
            let first = claim(b"first\n");
            let second = claim(b"second\n");
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_ne!(first, second);
        assert!(
            std::fs::read_to_string(&first)
                .unwrap()
                .ends_with("first\n")
        );
        assert!(
            std::fs::read_to_string(&second)
                .unwrap()
                .ends_with("second\n")
        );
    }
}

#[test]
fn a_day_folder_removed_mid_claim_is_recreated() {
    let (_temp, root) = store();
    let levels = [root.clone(), root.join("molt"), root.join("molt/20261112")];
    let calls = std::cell::Cell::new(0);
    let path = claim(&levels, "title", b"note\n", |levels| {
        calls.set(calls.get() + 1);
        for level in levels {
            atomic::create_dir_all_mode(level, DIR_MODE)?;
        }
        if calls.get() == 1 {
            std::fs::remove_dir(&levels[2]).unwrap();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(std::fs::read(path).unwrap(), b"note\n");
}

#[test]
fn refused_input_leaves_no_file_and_no_directory() {
    let (_temp, root) = store();
    let refusals = [
        (request("diary", "Title", b""), "empty"),
        (request("diary", "Title", b" \n\t\r\n"), "blank"),
        (
            request("diary", "Title", "\u{a0}\u{2003}\n".as_bytes()),
            "unicode blank",
        ),
        (request("diary", "!!! ---", b"x"), "title"),
        (request("..", "Title", b"x"), "kind dots"),
        (request("/", "Title", b"x"), "kind slash"),
        (
            AddRequest {
                meta: vec!["no-equals".into()],
                ..request("diary", "Title", b"x")
            },
            "meta without =",
        ),
        (
            AddRequest {
                meta: vec!["Upper=x".into()],
                ..request("diary", "Title", b"x")
            },
            "meta key grammar",
        ),
        (
            AddRequest {
                meta: vec!["1st=x".into()],
                ..request("diary", "Title", b"x")
            },
            "meta key digit lead",
        ),
        (
            AddRequest {
                meta: vec!["_lead=x".into()],
                ..request("diary", "Title", b"x")
            },
            "meta key lead",
        ),
        (
            AddRequest {
                meta: vec!["key=two\nlines".into()],
                ..request("diary", "Title", b"x")
            },
            "meta value lines",
        ),
    ];
    for (request, case) in refusals {
        let error = add_at(&root, &request, &[], &noon()).unwrap_err();
        let expected = match case {
            "empty" | "blank" | "unicode blank" => matches!(error, NotesErr::EmptyBody),
            "title" | "kind dots" | "kind slash" => matches!(error, NotesErr::NoSlug { .. }),
            _ => matches!(error, NotesErr::BadMeta { .. }),
        };
        assert!(expected, "{case}: {error}");
        assert!(!root.exists(), "{case} left {}", root.display());
    }
}

#[test]
fn a_caller_key_naming_a_rimz_key_is_dropped_and_repeats_keep_the_last_value() {
    let (_temp, root) = store();
    let added = add_at(
        &root,
        &AddRequest {
            meta: vec![
                "author=mallory".into(),
                "topic=first".into(),
                "author=again".into(),
                "workflow-run=wr_x".into(),
                "topic=last".into(),
            ],
            ..request("diary", "Day", b"x\n")
        },
        &[],
        &noon(),
    )
    .unwrap();
    assert_eq!(added.dropped_meta, ["author", "workflow-run"]);
    let text = std::fs::read_to_string(added.path).unwrap();
    assert!(
        !text.contains("mallory") && !text.contains("author"),
        "{text}"
    );
    assert!(!text.contains("wr_x"), "{text}");
    assert!(text.contains("topic: \"last\"\n---\n"), "{text}");
    assert_eq!(text.matches("topic").count(), 1, "{text}");
}

#[test]
fn no_kind_or_title_escapes_the_day_folder() {
    let (_temp, root) = store();
    let day = |kind: &str| root.join(kind).join("20261112");
    for (kind, title, expected) in [
        ("../x", "../../etc/passwd", day("x").join("etc-passwd.md")),
        ("a/b", "c/../d", day("a-b").join("c-d.md")),
        ("..x..", "..\\..\\y", day("x").join("y.md")),
    ] {
        let added = add_at(&root, &request(kind, title, b"x"), &[], &noon()).unwrap();
        assert_eq!(added.path, expected);
    }
}

#[test]
fn a_slug_cut_at_48_drops_the_dash_it_ends_on_and_suffixes_after_the_cut() {
    let title = format!("{} tail", "a".repeat(47));
    assert_eq!(slug(&title).unwrap(), "a".repeat(47));
    assert_eq!(slug("Ünïcode — Title!").unwrap(), "n-code-title");
    assert_eq!(slug("ü"), None);
    let (_temp, root) = store();
    let long = format!("{} more words", "b".repeat(60));
    let first = add_at(&root, &request("k", &long, b"x"), &[], &noon()).unwrap();
    let second = add_at(&root, &request("k", &long, b"x"), &[], &noon()).unwrap();
    let stem = "b".repeat(48);
    assert_eq!(first.path.file_name().unwrap(), &*format!("{stem}.md"));
    assert_eq!(second.path.file_name().unwrap(), &*format!("{stem}-2.md"));
}

#[test]
fn a_fence_outruns_every_backtick_run_in_the_body() {
    let (_temp, root) = store();
    let body = b"log ```three``` and ````four````\nend";
    let added = add_at(
        &root,
        &AddRequest {
            fence: true,
            ..request("log", "Build", body)
        },
        &[],
        &noon(),
    )
    .unwrap();
    let bytes = std::fs::read(added.path).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let after_header = text.split_once("\n---\n").unwrap().1;
    assert_eq!(
        after_header,
        "`````\nlog ```three``` and ````four````\nend\n`````\n"
    );
    let plain = add_at(
        &root,
        &AddRequest {
            fence: true,
            ..request("log", "Plain", b"no ticks\n")
        },
        &[],
        &noon(),
    )
    .unwrap();
    let text = std::fs::read_to_string(plain.path).unwrap();
    assert!(text.ends_with("---\n```\nno ticks\n```\n"), "{text}");
}

#[test]
fn a_body_that_is_not_utf8_is_kept_byte_for_byte_and_still_lists() {
    let (_temp, root) = store();
    let body = [0xff, 0xfe, b'\n', 0x80];
    let added = add_at(&root, &request("bin", "Raw", &body), &[], &noon()).unwrap();
    assert!(std::fs::read(&added.path).unwrap().ends_with(&body));
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert_eq!(listing.notes.len(), 1);
    assert!(listing.skipped.is_empty());
}

#[test]
fn characters_yaml_forbids_in_a_quoted_value_are_escaped_and_read_back() {
    let (_temp, root) = store();
    let odd = "a\u{7f}b\u{80}c\u{9f}d\u{fffe}e\u{ffff}f";
    let title = format!("Odd {odd}");
    let added = add_at(
        &root,
        &AddRequest {
            meta: vec![format!("odd={odd}")],
            ..request("diary", &title, b"x\n")
        },
        &[("author", odd.to_owned())],
        &noon(),
    )
    .unwrap();
    let text = std::fs::read_to_string(&added.path).unwrap();
    assert!(
        text.contains("author: \"a\\u007Fb\\u0080c\\u009Fd\\uFFFEe\\uFFFFf\"\n"),
        "{text}"
    );
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert!(listing.skipped.is_empty(), "{:?}", listing.skipped);
    let note = &listing.notes[0];
    assert_eq!(note.keys["title"], title);
    assert_eq!(note.keys["author"], odd);
    assert_eq!(note.meta["odd"], odd);
}

#[test]
fn a_header_still_open_at_the_read_bound_is_skipped() {
    let (_temp, root) = store();
    let day = root.join("diary/20261112");
    std::fs::create_dir_all(&day).unwrap();
    let mut header = String::from("---\nwritten: \"2026-11-12T12:30:05Z\"\n");
    let pad = "x".repeat(100);
    for line in 0.. {
        if header.len() as u64 > HEADER_MAX {
            break;
        }
        header.push_str(&format!("pad{line}: \"{pad}\"\n"));
    }
    std::fs::write(day.join("long.md"), header + "---\nbody\n").unwrap();
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert!(listing.notes.is_empty());
    let (path, reason) = &listing.skipped[0];
    assert_eq!(path, &day.join("long.md"));
    assert!(reason.contains("never closes its header"), "{reason}");
}

#[test]
fn a_store_that_does_not_exist_lists_nothing() {
    let (_temp, root) = store();
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert!(listing.notes.is_empty() && listing.skipped.is_empty());
}

#[test]
fn foreign_files_temp_siblings_and_headerless_notes_are_not_listed() {
    let (_temp, root) = store();
    let added = add_at(&root, &request("diary", "Kept", b"x\n"), &[], &noon()).unwrap();
    let day = added.path.parent().unwrap();
    std::fs::write(day.join("foreign.md"), "# no header\n").unwrap();
    std::fs::write(day.join("undated.md"), "---\nkind: \"diary\"\n---\nx\n").unwrap();
    std::fs::write(day.join("notes.txt"), "---\nwritten: x\n---\n").unwrap();
    std::fs::write(
        day.join(format!("kept.md.tmp.42.{}", "a".repeat(32))),
        "partial",
    )
    .unwrap();
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert_eq!(
        listing
            .notes
            .iter()
            .map(|note| &note.path)
            .collect::<Vec<_>>(),
        [&added.path]
    );
    let skipped: Vec<_> = listing
        .skipped
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(skipped, [day.join("foreign.md"), day.join("undated.md")]);
}

#[test]
fn a_symlinked_home_holds_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("dotfiles/rimz");
    std::fs::create_dir_all(&real).unwrap();
    let home = temp.path().join(".rimz");
    std::os::unix::fs::symlink(&real, &home).unwrap();
    let root = home.join("notes");
    let added = add_at(&root, &request("diary", "Linked", b"x\n"), &[], &noon()).unwrap();
    assert!(real.join("notes/diary/20261112/linked.md").is_file());
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    assert_eq!(listing.notes[0].path, added.path);
}

fn titles(listing: &Listing) -> Vec<&str> {
    listing
        .notes
        .iter()
        .map(|note| note.keys["title"].as_str())
        .collect()
}

#[test]
fn list_filters_alone_and_combined_in_write_order() {
    let (_temp, root) = store();
    let at = |stamp: &str| {
        stamp
            .parse::<Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(-6)))
    };
    let facts = |author: &str, project: &str| {
        vec![
            ("author", author.to_owned()),
            ("project", project.to_owned()),
        ]
    };
    for (kind, title, stamp, author, project) in [
        (
            "reflection",
            "Late",
            "2026-11-12T15:00:00Z",
            "otter",
            "/src/a",
        ),
        (
            "reflection",
            "Early",
            "2026-11-10T15:00:00Z",
            "otter",
            "/src/b",
        ),
        ("diary", "Middle", "2026-11-11T15:00:00Z", "fox", "/src/a"),
        (
            "Diary",
            "Same instant",
            "2026-11-12T15:00:00Z",
            "fox",
            "/src/a/./",
        ),
    ] {
        add_at(
            &root,
            &request(kind, title, b"x\n"),
            &facts(author, project),
            &at(stamp),
        )
        .unwrap();
    }
    let all = list_at(&root, &ListFilter::default()).unwrap();
    assert_eq!(titles(&all), ["Early", "Middle", "Same instant", "Late"]);

    let filter = |filter: ListFilter| list_at(&root, &filter).unwrap();
    let since: Timestamp = "2026-11-12T09:00:00-06:00".parse().unwrap();
    assert_eq!(
        titles(&filter(ListFilter {
            kind: Some("DIARY".into()),
            ..ListFilter::default()
        })),
        ["Middle", "Same instant"]
    );
    assert_eq!(
        titles(&filter(ListFilter {
            since: Some(since),
            ..ListFilter::default()
        })),
        ["Same instant", "Late"],
        "a note written at the bookmark's instant is kept"
    );
    assert_eq!(
        titles(&filter(ListFilter {
            author: Some("@otter".into()),
            ..ListFilter::default()
        })),
        ["Early", "Late"]
    );
    assert_eq!(
        titles(&filter(ListFilter {
            project: Some("/src/b/../a".into()),
            ..ListFilter::default()
        })),
        ["Middle", "Same instant", "Late"]
    );
    assert_eq!(
        titles(&filter(ListFilter {
            kind: Some("reflection".into()),
            since: Some(since),
            author: Some("otter".into()),
            project: Some("/src/a".into()),
        })),
        ["Late"]
    );
    assert!(
        filter(ListFilter {
            kind: Some("missing".into()),
            ..ListFilter::default()
        })
        .notes
        .is_empty()
    );
}

#[test]
fn listed_json_keeps_caller_keys_apart_from_rimz_keys() {
    let (_temp, root) = store();
    let added = add_at(
        &root,
        &AddRequest {
            meta: ["null=n", "true=t", "no=n", "on=o", "y=y"]
                .map(String::from)
                .to_vec(),
            ..request("diary", "Keys", b"x\n")
        },
        &[("author", "otter".to_owned())],
        &noon(),
    )
    .unwrap();
    let text = std::fs::read_to_string(&added.path).unwrap();
    assert!(
        text.contains("\n\"null\": \"n\"\ntrue: \"t\"\nno: \"n\"\non: \"o\"\ny: \"y\"\n---\n"),
        "only `null` is quoted: {text}"
    );
    let listing = list_at(&root, &ListFilter::default()).unwrap();
    let json = serde_json::to_value(&listing.notes[0]).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "path": added.path,
            "kind": "diary",
            "title": "Keys",
            "written": "2026-11-12T12:30:05.250-06:00",
            "author": "otter",
            "meta": {"null": "n", "true": "t", "no": "n", "on": "o", "y": "y"},
        })
    );
}
