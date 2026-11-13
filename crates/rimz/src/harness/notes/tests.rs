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
        (
            AddRequest {
                meta: vec![format!("big={}", "x".repeat(1024 * 1024))],
                ..request("diary", "Title", b"x")
            },
            "header too large",
        ),
    ];
    for (request, case) in refusals {
        let error = add_at(&root, &request, &[], &noon()).unwrap_err();
        let expected = match case {
            "empty" | "blank" | "unicode blank" => matches!(error, NotesErr::EmptyBody),
            "title" | "kind dots" | "kind slash" => matches!(error, NotesErr::NoSlug { .. }),
            "header too large" => matches!(error, NotesErr::HeaderTooLarge { .. }),
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
    assert!(listing.notes.is_empty());
    let (path, reason) = &listing.skipped[0];
    assert_eq!(path, &day.join("long.md"));
    assert!(reason.contains("never closes its header"), "{reason}");
}

#[test]
fn a_store_that_does_not_exist_lists_nothing() {
    let (_temp, root) = store();
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
    assert_eq!(listing.notes[0].path, added.path);
}

fn titles(listing: &Listing) -> Vec<&str> {
    listing
        .notes
        .iter()
        .map(|note| note.keys["title"].as_str())
        .collect()
}

fn at(stamp: &str) -> Zoned {
    stamp
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(-6)))
}

fn listed(root: &Path, filters: &[&str], now: Timestamp) -> Listing {
    let request = ListRequest {
        filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
        limit: 0,
    };
    list_at(root, &request, now).unwrap()
}

fn refused(root: &Path, filters: &[&str]) -> String {
    let request = ListRequest {
        filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
        limit: 0,
    };
    let error = list_at(root, &request, noon().timestamp()).unwrap_err();
    assert!(
        matches!(
            error,
            NotesErr::EmptyFilter | NotesErr::Term(_) | NotesErr::BadTerm { .. }
        ),
        "{filters:?}: {error}"
    );
    error.to_string()
}

#[test]
fn list_filters_alone_and_combined_in_write_order() {
    let (_temp, root) = store();
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
    // A hand-placed note whose folder and header disagree lists by its header.
    let moved = root.join("diary/20261109");
    std::fs::create_dir_all(&moved).unwrap();
    std::fs::write(
        moved.join("moved.md"),
        "---\nkind: \"reflection\"\ntitle: \"Moved\"\nwritten: \"2026-11-09T15:00:00Z\"\n---\nx\n",
    )
    .unwrap();
    let now = noon().timestamp();
    let filter = |filters: &[&str]| listed(&root, filters, now);
    assert_eq!(
        titles(&filter(&[])),
        ["Moved", "Early", "Middle", "Same instant", "Late"]
    );
    assert_eq!(
        titles(&filter(&["kind=DIARY"])),
        ["Middle", "Same instant"],
        "a kind value is slugged as add names the folder"
    );
    assert_eq!(
        titles(&filter(&["kind=reflection"])),
        ["Moved", "Early", "Late"],
        "kind reads the header, not the folder"
    );
    assert_eq!(
        titles(&filter(&["written>=2026-11-12T09:00:00-06:00"])),
        ["Same instant", "Late"],
        "a note written at the bookmark's instant is kept"
    );
    assert_eq!(
        titles(&filter(&["written<2026-11-12T09:00:00-06:00"])),
        ["Moved", "Early", "Middle"]
    );
    assert_eq!(titles(&filter(&["author=@otter"])), ["Early", "Late"]);
    assert_eq!(
        titles(&filter(&["project=/src/b/../a"])),
        ["Middle", "Same instant", "Late"]
    );
    assert_eq!(
        titles(&filter(&[
            "kind=reflection written>=2026-11-12T09:00:00-06:00",
            "author=otter project=/src/a"
        ])),
        ["Late"]
    );
    assert!(filter(&["kind=missing"]).notes.is_empty());
    assert_eq!(
        refused(&root, &["kind=!!!"]),
        "filter term `kind=!!!`: `!!!` has no ASCII letter or digit to name a kind by"
    );
}

/// Three notes a day apart, and the clock a day after the middle one.
fn seeded() -> (tempfile::TempDir, PathBuf, Timestamp) {
    let (temp, root) = store();
    let notes = [
        (
            "handoff",
            "Token expiry fix",
            "2026-11-09T12:00:00Z",
            vec![("author", "otter".to_owned())],
            vec!["pr=482", "topic=two words"],
        ),
        (
            "handoff",
            "Other",
            "2026-11-11T12:00:00Z",
            vec![
                ("author", "fox".to_owned()),
                ("loop-task", "nightly".to_owned()),
            ],
            vec!["pr=483", "star=other"],
        ),
        (
            "reflection",
            "Mood",
            "2026-11-12T11:00:00Z",
            vec![],
            vec!["literal=none", "star=*"],
        ),
    ];
    for (kind, title, stamp, facts, meta) in notes {
        add_at(
            &root,
            &AddRequest {
                meta: meta.into_iter().map(String::from).collect(),
                ..request(kind, title, b"x\n")
            },
            &facts,
            &at(stamp),
        )
        .unwrap_or_else(|error| panic!("{title}: {error}"));
    }
    // `add` refuses an upper-case key; a hand-written header may hold one.
    let mixed = root.join("handoff/20261111/other.md");
    let text = std::fs::read_to_string(&mixed).unwrap();
    std::fs::write(
        &mixed,
        text.replace("pr: \"483\"\n", "pr: \"483\"\nMixed: \"Case\"\n"),
    )
    .unwrap();
    (temp, root, "2026-11-12T12:00:00Z".parse().unwrap())
}

#[test]
fn every_operator_reads_text_meta_and_hyphenated_keys() {
    let (_temp, root, now) = seeded();
    let filter = |filters: &[&str]| titles(&listed(&root, filters, now)).join(", ");
    for (filters, expected) in [
        (&["kind=handoff"][..], "Token expiry fix, Other"),
        (&["kind!=handoff"], "Mood"),
        (&["kind=hand*"], "Token expiry fix, Other"),
        (&["KIND=Reflection"], "Mood"),
        (&["author=otter,fox"], "Token expiry fix, Other"),
        (&["author!=otter,fox"], "Mood"),
        (&["author=@fox"], "Other"),
        (&["author=none"], "Mood"),
        (&["author=*"], "Token expiry fix, Other"),
        (&["title=*EXPIRY*"], "Token expiry fix"),
        (&["title=other"], "Other"),
        (&["pr=482"], "Token expiry fix"),
        (&["pr!=482"], "Other, Mood"),
        (&["pr=482,483"], "Token expiry fix, Other"),
        (&["pr=none"], "Mood"),
        (&["pr=*"], "Token expiry fix, Other"),
        (&["pr=48*"], "Token expiry fix, Other"),
        (&["loop-task=nightly"], "Other"),
        (&["LOOP-TASK=nightly"], "Other"),
        (&["workflow-run=none"], "Token expiry fix, Other, Mood"),
        (&["topic=\"two words\""], "Token expiry fix"),
        (&["literal=\"none\""], "Mood"),
        (&["literal=none"], "Token expiry fix, Other"),
        (&["star=\"*\""], "Other, Mood"),
        (&["star=\"oth*\""], "Other"),
        (&["Mixed=Case"], "Other"),
        (&["kind=handoff", "pr=483"], "Other"),
        (&["kind=handoff pr=483 author=otter"], ""),
    ] {
        assert_eq!(filter(filters), expected, "{filters:?}");
    }
}

#[test]
fn age_and_written_compare_on_the_pinned_clock() {
    let (_temp, root, now) = seeded();
    let filter = |filters: &[&str]| titles(&listed(&root, filters, now)).join(", ");
    for (filters, expected) in [
        (&["age<2d"][..], "Other, Mood"),
        (&["age>=1d"], "Token expiry fix, Other"),
        (&["age>1d"], "Token expiry fix"),
        (&["age<=1h"], "Mood"),
        (&["age<1h"], ""),
        (&["age=1h"], "Mood"),
        (&["age!=1h"], "Token expiry fix, Other"),
        (&["AGE<2d"], "Other, Mood"),
        (&["age=*"], "Token expiry fix, Other, Mood"),
        (&["age=none"], ""),
        (&["written>=2026-11-11T12:00:00Z"], "Other, Mood"),
        (&["written>2026-11-11T12:00:00Z"], "Mood"),
        (
            &["written<=2026-11-11T06:00:00-06:00"],
            "Token expiry fix, Other",
        ),
        (&["written=2026-11-11T12:00:00Z"], "Other"),
    ] {
        assert_eq!(filter(filters), expected, "{filters:?}");
    }
    let later: Timestamp = "2026-11-12T13:00:00Z".parse().unwrap();
    assert_eq!(
        titles(&listed(&root, &["age>=2h"], later)),
        ["Token expiry fix", "Other", "Mood"],
        "age moves with the clock passed in"
    );
}

#[test]
fn malformed_terms_refuse_naming_the_term_and_no_agents_field() {
    let (_temp, root, _now) = seeded();
    for (filters, expected) in [
        (
            &[""][..],
            "empty filter: write <field><op><value>, such as kind=handoff; see `--filter help`",
        ),
        (
            &["kind"],
            "filter term `kind` has no operator: use one of = != > >= < <=",
        ),
        (
            &["pr=\"open"],
            "filter term `pr=\"open` has an unterminated quote",
        ),
        (
            &["title>a"],
            "filter term `title>a`: title is not ordered; use = or !=",
        ),
        (
            &["pr<5"],
            "filter term `pr<5`: pr is not ordered; use = or !=",
        ),
        (
            &["age>soon"],
            "filter term `age>soon`: `soon` is not a duration such as 30m or 2d",
        ),
        (
            &["written>yesterday"],
            "filter term `written>yesterday`: `yesterday` is not an RFC 3339 instant such as 2026-11-12T09:00:00Z",
        ),
        (
            &["age>1h,2h"],
            "filter term `age>1h,2h`: a comparison takes one value",
        ),
        (
            &["age<none"],
            "filter term `age<none`: a comparison takes one value",
        ),
    ] {
        let message = refused(&root, filters);
        assert_eq!(message, expected, "{filters:?}");
        assert!(!message.contains("status"), "{message}");
    }
}

#[test]
fn a_key_no_note_carries_empties_the_listing_and_is_reported_once() {
    let (_temp, root, now) = seeded();
    for filters in [
        &["nosuch=1"][..],
        &["nosuch!=1"],
        &["nosuch=none kind=handoff"],
        &["nosuch=1", "nosuch=*"],
    ] {
        let listing = listed(&root, filters, now);
        assert!(listing.notes.is_empty(), "{filters:?}");
        assert_eq!(listing.unknown_keys, ["nosuch"], "{filters:?}");
        assert_eq!(listing.hidden, 0, "{filters:?}");
    }
    let listing = listed(&root, &["PR=482 zz=1"], now);
    assert_eq!(
        listing.unknown_keys,
        ["PR", "zz"],
        "a --meta key is matched as written"
    );
    let known = listed(&root, &["pr=1 workflow-run=x"], now);
    assert!(known.unknown_keys.is_empty() && known.notes.is_empty());
    let (_empty, missing) = store();
    let none = listed(&missing, &["kind=handoff age<1d"], now);
    assert!(none.notes.is_empty() && none.unknown_keys.is_empty());
}

#[test]
fn the_limit_keeps_the_newest_matches_oldest_first() {
    let (_temp, root, now) = seeded();
    std::fs::write(root.join("handoff/20261111/foreign.md"), "# none\n").unwrap();
    let limited = |filters: &[&str], limit| {
        let request = ListRequest {
            filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
            limit,
        };
        list_at(&root, &request, now).unwrap()
    };
    let two = limited(&[], 2);
    assert_eq!(titles(&two), ["Other", "Mood"]);
    assert_eq!(two.hidden, 1, "the skipped file is never counted");
    assert_eq!(two.skipped.len(), 1);
    let one = limited(&["kind=handoff"], 1);
    assert_eq!((titles(&one), one.hidden), (vec!["Other"], 1));
    let all = limited(&[], 0);
    assert_eq!((all.notes.len(), all.hidden), (3, 0));
    let roomy = limited(&["kind=handoff"], 10);
    assert_eq!(
        (titles(&roomy), roomy.hidden),
        (vec!["Token expiry fix", "Other"], 0)
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
    let listing = list_at(&root, &ListRequest::default(), noon().timestamp()).unwrap();
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
