//! `rimz notes` integration tests: the binary writes and lists the home-wide
//! note store with no room, no checkout, and the caller's launch environment.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

use crate::common::Env;

fn run(command: &mut Command, stdin: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rimz notes");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait rimz notes")
}

/// A command run from a plain directory that is no checkout and holds no room.
fn notes(env: &Env, args: &[&str]) -> Command {
    let plain = env.home_root.join("plain");
    std::fs::create_dir_all(&plain).expect("mkdir plain dir");
    let mut command = env.rimz();
    command.arg("notes").args(args).current_dir(plain);
    command
}

fn added_path(output: &Output) -> PathBuf {
    assert!(
        output.status.success(),
        "notes add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).expect("utf-8 stdout");
    let path = stdout.strip_suffix('\n').expect("one line");
    assert!(
        !path.contains('\n'),
        "the path is alone on stdout: {stdout:?}"
    );
    PathBuf::from(path)
}

#[test]
fn add_from_stdin_prints_the_absolute_path_of_the_whole_note() {
    let env = Env::new();
    let body = b"---\nown: header\n---\nbody\n";
    let path = added_path(&run(
        notes(&env, &["add", "handoff", "Ship notes"])
            .env("RIMZ_AGENT_NAME", "level-spark")
            .env("RIMZ_AGENT_ROLE", "coder")
            .env("RIMZ_CHANNEL", "")
            .env("RIMZ_PROJECT_ROOT", "/src/repo"),
        body,
    ));
    assert!(path.is_absolute(), "{}", path.display());
    let day = path.parent().expect("day folder");
    assert_eq!(day.parent(), Some(&*env.rimz_home().join("notes/handoff")));
    assert_eq!(path.file_name().unwrap(), "ship-notes.md");
    let text = std::fs::read_to_string(&path).expect("read note");
    assert!(text.contains("author: \"level-spark\"\n"), "{text}");
    assert!(text.contains("project: \"/src/repo\"\n"), "{text}");
    assert!(!text.contains("channel"), "{text}");
    assert!(
        text.ends_with("\n---\n---\nown: header\n---\nbody\n"),
        "{text}"
    );
}

#[test]
fn a_plain_shell_writes_no_author_and_its_project_from_the_cwd() {
    let env = Env::new();
    let path = added_path(&run(&mut notes(&env, &["add", "diary", "Day"]), b"text\n"));
    let text = std::fs::read_to_string(&path).expect("read note");
    assert!(!text.contains("author"), "{text}");
    let project = crate::common::canonical(&env.home_root.join("plain"));
    assert!(
        text.contains(&format!("project: \"{}\"\n", project.display())),
        "{text}"
    );

    let other = env.home_root.join("other");
    std::fs::create_dir_all(&other).expect("mkdir other project");
    let path = added_path(&run(
        notes(
            &env,
            &["--root", other.to_str().unwrap(), "add", "diary", "Root"],
        )
        .env("RIMZ_PROJECT_ROOT", "/pinned"),
        b"text\n",
    ));
    let text = std::fs::read_to_string(&path).expect("read note");
    let other = crate::common::canonical(&other);
    assert!(
        text.contains(&format!("project: \"{}\"\n", other.display())),
        "--root names the project ahead of the pin: {text}"
    );
}

#[test]
fn add_reads_a_file_fences_it_and_takes_caller_keys() {
    let env = Env::new();
    let log = env.home_root.join("build.log");
    std::fs::write(&log, "error ```x```\n").expect("write log");
    let output = run(
        &mut notes(
            &env,
            &[
                "add",
                "log",
                "Build",
                "--file",
                log.to_str().unwrap(),
                "--fence",
                "--meta",
                "ticket=42",
                "--meta",
                "author=mallory",
            ],
        ),
        b"stdin is ignored\n",
    );
    let path = added_path(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--meta author dropped: RimZ writes that header key itself"),
        "{stderr}"
    );
    let text = std::fs::read_to_string(&path).expect("read note");
    assert!(
        text.ends_with("ticket: \"42\"\n---\n````\nerror ```x```\n````\n"),
        "{text}"
    );
    assert!(
        !text.contains("mallory") && !text.contains("stdin"),
        "{text}"
    );
}

#[test]
fn refused_adds_leave_no_note() {
    let env = Env::new();
    let missing = env.home_root.join("missing.md");
    let cases: [(&[&str], &[u8], i32, String); 4] = [
        (
            &["add", "diary", "Day", "--file", missing.to_str().unwrap()],
            b"",
            1,
            format!("reading {}", missing.display()),
        ),
        (
            &["add", "diary", "Day"],
            b"  \n",
            1,
            "the note body is empty".into(),
        ),
        (
            &["add", "diary", "Day", "--meta", "1st=x"],
            b"x",
            2,
            "--meta `1st=x`: a key starts with a lowercase letter".into(),
        ),
        (
            &["add", "..", "Day"],
            b"x",
            2,
            "kind `..` has no ASCII letter or digit".into(),
        ),
    ];
    for (args, stdin, code, message) in cases {
        let output = run(&mut notes(&env, args), stdin);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(stderr.contains(&message), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!env.rimz_home().join("notes").exists(), "{args:?}");
    }
}

#[test]
fn list_filters_a_store_under_a_symlinked_home() {
    let env = Env::new();
    let real = env.home_root.join("dotfiles/rimz");
    std::fs::create_dir_all(&real).expect("mkdir real home");
    let linked = env.home_root.join("linked-home");
    std::os::unix::fs::symlink(&real, &linked).expect("link home");
    let with_home = |args: &[&str]| {
        let mut command = notes(&env, args);
        command.env("RIMZ_HOME", &linked);
        command
    };
    let empty = run(&mut with_home(&["list", "--json"]), b"");
    assert!(empty.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap(),
        Value::Array(vec![])
    );

    let first = added_path(&run(
        with_home(&["add", "reflection", "First", "--meta", "topic=a"])
            .env("RIMZ_AGENT_NAME", "otter")
            .env("RIMZ_PROJECT_ROOT", "/src/a"),
        b"one\n",
    ));
    let second = added_path(&run(
        with_home(&["add", "reflection", "First"])
            .env("RIMZ_AGENT_NAME", "fox")
            .env("RIMZ_PROJECT_ROOT", "/src/b"),
        b"two\n",
    ));
    assert_eq!(second.file_name().unwrap(), "first-2.md");
    assert!(real.join("notes/reflection").is_dir());

    let list = |args: &[&str]| {
        let output = run(&mut with_home(&[&["list", "--json"], args].concat()), b"");
        assert!(output.status.success());
        serde_json::from_slice::<Value>(&output.stdout).expect("json")
    };
    let all = list(&[]);
    let paths: Vec<_> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|note| PathBuf::from(note["path"].as_str().unwrap()))
        .collect();
    assert_eq!(paths, [first.clone(), second]);
    assert_eq!(all[0]["author"], "otter");
    assert_eq!(all[0]["meta"], serde_json::json!({"topic": "a"}));
    let since = all[0]["written"].as_str().unwrap().to_owned();
    let since = format!("written>={since}");
    assert_eq!(list(&["--filter", &since]).as_array().unwrap().len(), 2);
    let one = list(&[
        "--filter",
        "author=@otter project=/src/a",
        "--filter",
        "kind=Reflection",
    ]);
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert_eq!(Path::new(one[0]["path"].as_str().unwrap()), first);
    let quiet = run(&mut with_home(&["list", "-q", "--filter", "topic=a"]), b"");
    assert!(quiet.status.success());
    assert_eq!(
        String::from_utf8_lossy(&quiet.stdout),
        format!("{}\n", first.display())
    );

    let table = run(&mut with_home(&["list"]), b"");
    let stdout = String::from_utf8_lossy(&table.stdout);
    assert!(
        stdout.contains("WRITTEN") && stdout.contains(first.to_str().unwrap()),
        "{stdout}"
    );
}

#[test]
fn list_warns_once_for_a_foreign_note_and_keeps_going() {
    let env = Env::new();
    let path = added_path(&run(&mut notes(&env, &["add", "diary", "Kept"]), b"x\n"));
    let foreign = path.parent().unwrap().join("foreign.md");
    std::fs::write(&foreign, "# mine\n").expect("write foreign note");
    let output = run(&mut notes(&env, &["list", "--json"]), b"");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("skipped").count(), 1, "{stderr}");
    assert!(
        stderr.contains(&format!(
            "skipped {}: has no RimZ header",
            foreign.display()
        )),
        "{stderr}"
    );
    let notes: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(notes.as_array().unwrap().len(), 1);
}

#[test]
fn add_writes_the_note_and_warns_when_the_project_cannot_resolve() {
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).expect("mkdir home");
    std::fs::write(env.rimz_home().join("ws"), "not a directory").expect("block ws");
    let output = run(&mut notes(&env, &["add", "diary", "Lost"]), b"text\n");
    let path = added_path(&output);
    let text = std::fs::read_to_string(&path).expect("read note");
    assert!(!text.contains("project"), "{text}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning: no project header: "), "{stderr}");
}

#[test]
fn a_title_with_a_line_break_stays_on_its_table_row() {
    let env = Env::new();
    let path = added_path(&run(
        &mut notes(&env, &["add", "diary", "two\nlines\u{7}"]),
        b"x\n",
    ));
    let table = run(&mut notes(&env, &["list"]), b"");
    let stdout = String::from_utf8_lossy(&table.stdout);
    let row = stdout
        .lines()
        .find(|line| line.contains(path.to_str().unwrap()))
        .expect("the row holding the path");
    assert!(row.contains("two lines"), "{stdout}");
    assert!(!stdout.contains('\u{7}'), "{stdout}");
    let listed = run(&mut notes(&env, &["list", "--json"]), b"");
    let notes: Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert_eq!(notes[0]["title"], "two\nlines\u{7}");
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_hidden_line_closes_the_table_and_goes_to_stderr_under_quiet_and_json() {
    let env = Env::new();
    let paths: Vec<PathBuf> = ["A", "B", "C"]
        .into_iter()
        .map(|title| added_path(&run(&mut notes(&env, &["add", "diary", title]), b"x\n")))
        .collect();
    let list = |args: &[&str]| {
        let output = run(&mut notes(&env, &[&["list"], args].concat()), b"");
        assert!(
            output.status.success(),
            "{args:?}: {:?}",
            output_text(&output)
        );
        output_text(&output)
    };

    let (stdout, stderr) = list(&["-n", "2"]);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.last(),
        Some(&"1 older note not shown; pass -n to see more"),
        "{stdout}"
    );
    assert!(lines[1].contains(paths[1].to_str().unwrap()), "{stdout}");
    assert!(lines[2].contains(paths[2].to_str().unwrap()), "{stdout}");
    assert!(!stdout.contains(paths[0].to_str().unwrap()), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");

    let (stdout, stderr) = list(&["-q", "-n", "1"]);
    assert_eq!(stdout, format!("{}\n", paths[2].display()));
    assert_eq!(stderr, "2 older notes not shown; pass -n to see more\n");

    let (stdout, stderr) = list(&["--json", "-n", "2"]);
    let json: Value = serde_json::from_str(&stdout).expect("json");
    let listed: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|note| PathBuf::from(note["path"].as_str().unwrap()))
        .collect();
    assert_eq!(listed, paths[1..]);
    assert_eq!(stderr, "1 older note not shown; pass -n to see more\n");

    for args in [&["-n", "0"][..], &["-n", "3"], &[]] {
        let (stdout, stderr) = list(&[&["-q"], args].concat());
        assert_eq!(stdout.lines().count(), 3, "{args:?}: {stdout}");
        assert!(stderr.is_empty(), "{args:?}: {stderr}");
    }
    let (stdout, _) = list(&["-n", "0"]);
    assert!(!stdout.contains("not shown"), "{stdout}");

    let (stdout, stderr) = list(&["-q", "--filter", "kind=handoff"]);
    assert!(stdout.is_empty() && stderr.is_empty(), "{stdout}{stderr}");
}

#[test]
fn filter_help_lists_the_note_fields_without_reading_the_store() {
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).expect("mkdir home");
    std::fs::write(env.rimz_home().join("notes"), "not a directory").expect("block store");
    let broken = run(&mut notes(&env, &["list"]), b"");
    assert_eq!(broken.status.code(), Some(1), "{:?}", output_text(&broken));

    let help = run(
        &mut notes(&env, &["list", "--filter", " help ", "--filter", "kind=x"]),
        b"",
    );
    let (stdout, stderr) = output_text(&help);
    assert!(help.status.success(), "{stderr}");
    for field in [
        "FIELD",
        "kind",
        "title",
        "loop-task",
        "workflow-run",
        "age",
        "written",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.split_whitespace().next() == Some(field)),
            "{field}: {stdout}"
        );
    }
    assert!(
        stdout.contains("any other field is a --meta key"),
        "{stdout}"
    );
    for agents_only in ["status", "ctx", "tokens", "live", "parent", "lane"] {
        assert!(
            !stdout
                .split_whitespace()
                .any(|word| word.trim_matches(',') == agents_only),
            "{agents_only}: {stdout}"
        );
    }
}

#[test]
fn a_malformed_term_exits_2_and_an_unknown_key_warns_and_lists_nothing() {
    let env = Env::new();
    added_path(&run(
        &mut notes(&env, &["add", "diary", "Day", "--meta", "pr=482"]),
        b"x\n",
    ));
    for (args, message) in [
        (
            &["--filter", "kind"][..],
            "filter term `kind` has no operator: use one of = != > >= < <=",
        ),
        (
            &["--filter", "title>a"],
            "filter term `title>a`: title is not ordered; use = or !=",
        ),
        (
            &["--filter", "kind=!!!"],
            "filter term `kind=!!!`: `!!!` has no ASCII letter or digit to name a kind by",
        ),
        (&["-q", "--json"], "cannot be used with"),
    ] {
        let output = run(&mut notes(&env, &[&["list"], args].concat()), b"");
        let (stdout, stderr) = output_text(&output);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
        assert!(!stderr.contains("status"), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }

    let output = run(
        &mut notes(
            &env,
            &[
                "list",
                "-q",
                "--filter",
                "nosuch=1 pr=482",
                "--filter",
                "nosuch=2",
            ],
        ),
        b"",
    );
    let (stdout, stderr) = output_text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr,
        "warning: no note carries the key `nosuch`, so nothing matches\n"
    );
    let output = run(&mut notes(&env, &["list", "-q", "--filter", "pr=482"]), b"");
    assert_eq!(output_text(&output).0.lines().count(), 1);
}
