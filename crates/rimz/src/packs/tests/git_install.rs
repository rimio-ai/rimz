use super::git_sources::git;
use super::*;
use crate::packs::transaction::Transaction;

fn repository(root: &Path, packs: &[(&str, bool, &[&str])]) -> GitSource {
    fs::create_dir_all(root).unwrap();
    let mut manifest = String::new();
    for (name, default, requires) in packs {
        fs::create_dir_all(root.join(name)).unwrap();
        write(&root.join(name).join("README.md"), name);
        manifest.push_str(&format!("[[pack]]\nname = {name:?}\ndescription = 'Test'\npath = {name:?}\ndefault = {default}\nrequires = {requires:?}\n"));
    }
    write(&root.join("rimz-packs.toml"), &manifest);
    git(root, &["init", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "packs"]);
    crate::packs::source::parse(url::Url::from_directory_path(root).unwrap().as_str()).unwrap()
}

fn stage(home: &Path, source: &GitSource) -> Transaction {
    let result = Transaction::stage(home, source);
    assert!(
        result.is_ok(),
        "staging failed: {:?}",
        result.as_ref().err()
    );
    result.unwrap()
}

fn names(home: &Path) -> BTreeSet<String> {
    Record::read(home)
        .unwrap()
        .sources
        .into_iter()
        .flat_map(|source| source.packs)
        .map(|pack| pack.name.to_string())
        .collect()
}

#[test]
fn a_transport_only_record_change_aborts_the_reviewed_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = repository(&temp.path().join("repo"), &[("a", false, &[])]);
    stage(&home, &source).commit().unwrap();
    let staged = stage(&home, &source);
    let path = home.join("packs/installed.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["sources"][0]["url"] = "https://example.test/changed.git".into();
    let changed = serde_json::to_string(&record).unwrap();
    write(&path, &changed);
    assert!(
        matches!(staged.commit(), Err(PackErr::RecordChanged)),
        "transport changes must invalidate the reviewed snapshot"
    );
    assert_eq!(fs::read_to_string(path).unwrap(), changed);
}

#[test]
fn git_install_selects_defaults_and_adds_selectors_without_losing_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let mut source = repository(&repo, &[("a", true, &[]), ("b", false, &[])]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    stage(&home, &source).commit().unwrap();
    assert_eq!(names(&home), BTreeSet::from(["a".to_owned()]));
    let record = Record::read(&home).unwrap();
    let installed = &record.sources[0];
    assert!(!installed.linked && !installed.pinned);
    assert_eq!(installed.reference.as_deref(), Some("main"));
    assert_eq!(installed.commit.as_deref(), Some(commit.as_str()));
    let entry = home.join("packs").join(&installed.entry);
    assert!(entry.is_symlink());
    let checkout = fs::read_link(&entry).unwrap();
    assert_eq!(
        checkout,
        Path::new(".checkouts").join(format!("{}-{commit}", installed.entry))
    );
    assert_eq!(
        git(&home.join("packs").join(&checkout), &["rev-parse", "HEAD"]),
        commit
    );
    assert!(
        resolve(&home)
            .packs
            .iter()
            .all(|pack| pack.root.starts_with(&entry))
    );
    exclude(&home, &name("a"), &ExclusionTarget::Type(ItemType::Teams)).unwrap();
    source.pack = Some(name("b"));
    source.reference = Some("main".to_owned());
    stage(&home, &source).commit().unwrap();
    let record = Record::read(&home).unwrap();
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "b".to_owned()])
    );
    assert!(record.sources[0].pinned);
    assert!(
        record.sources[0].packs[0]
            .excluded
            .types
            .contains(&ItemType::Teams)
    );
    assert_eq!(fs::read_link(&entry).unwrap(), checkout);
    source.reference = None;
    stage(&home, &source).commit().unwrap();
    assert!(!Record::read(&home).unwrap().sources[0].pinned);
}

#[test]
fn copied_packs_resolve_and_load_before_any_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let source = repository(&repo, &[("crew", false, &[])]);
    write(
        &repo.join("crew/agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    );
    write(
        &repo.join("crew/agents/worker.md"),
        "---\ndescription: Worker\nagent: claude\ntools: [Read]\n---\nWork.",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "definitions"]);
    stage(&home, &source).commit().unwrap();
    let copied = temp.path().join("copied");
    fs::create_dir(&copied).unwrap();
    let output = std::process::Command::new("cp")
        .arg("-a")
        .arg(home.join("packs"))
        .arg(copied.join("packs"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::remove_dir_all(&home).unwrap();
    let resolved = resolve(&copied);
    assert_eq!(resolved.packs.len(), 1, "{resolved:?}");
    assert!(resolved.problems.is_empty(), "{resolved:?}");
    let loaded = crate::config::definitions::load(
        &copied,
        crate::config::definitions::SkillCheck::Skip,
        &Default::default(),
        &copied.join("config.toml"),
        &Default::default(),
        None,
    );
    assert!(loaded.errors.is_empty(), "{loaded:?}");
    assert!(loaded.agent_profiles.0.contains_key("worker"), "{loaded:?}");
}

#[test]
fn git_install_pins_tags_branches_and_commits_and_reinstalling_a_branch_moves_it() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let source = repository(&repo, &[("a", false, &[]), ("b", false, &[])]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["tag", "v1"]);
    let mut actual = Vec::new();
    for (index, reference) in ["v1", "main", first.as_str()].iter().enumerate() {
        let home = temp.path().join(format!("home-{index}"));
        let mut pinned = source.clone();
        pinned.reference = Some(reference.to_string());
        stage(&home, &pinned).commit().unwrap();
        let record = Record::read(&home).unwrap();
        actual.push(record.sources.first().map(|source| {
            (
                source.pinned,
                source.reference.clone(),
                source.commit.clone(),
                source.packs.len(),
            )
        }));
    }
    assert_eq!(
        actual,
        ["v1", "main", first.as_str()].map(|reference| Some((
            true,
            Some(reference.to_owned()),
            Some(first.clone()),
            2
        )))
    );
    let home = temp.path().join("home-1");
    write(&repo.join("new"), "New commit.");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "next"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);
    let mut pinned = source;
    pinned.reference = Some("main".to_owned());
    stage(&home, &pinned).commit().unwrap();
    assert_eq!(
        Record::read(&home).unwrap().sources[0].commit.as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn reinstalling_a_ref_without_an_installed_pack_refuses_without_committing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let mut source = repository(&repo, &[("a", false, &[]), ("b", false, &[])]);
    stage(&home, &source).commit().unwrap();
    let record_path = home.join("packs/installed.json");
    let before = fs::read(&record_path).unwrap();
    let entry = home
        .join("packs")
        .join(&Record::read(&home).unwrap().sources[0].entry);
    let checkout = fs::read_link(&entry).unwrap();
    write(
        &repo.join("rimz-packs.toml"),
        "[[pack]]\nname = 'a'\ndescription = 'Test'\npath = 'a'\ndefault = true\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "remove b"]);
    source.reference = Some("main".to_owned());
    for selector in [None, Some(name("a"))] {
        source.pack = selector;
        let error = Transaction::stage(&home, &source)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            error.contains(&format!(
                "Git source {}: no pack b; available packs: a",
                source.url
            )) && error.contains("rimz packs remove b"),
            "{error}"
        );
        assert_eq!(fs::read(&record_path).unwrap(), before);
        assert_eq!(fs::read_link(&entry).unwrap(), checkout);
    }
}

#[test]
fn unknown_selectors_and_foreign_pack_names_refuse_without_committing() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let mut source = repository(&temp.path().join("repo"), &[("a", false, &[])]);
    source.pack = Some(name("missing"));
    let error = Transaction::stage(&home, &source)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains("missing") && error.contains("a"), "{error}");
    assert_eq!(Record::read(&home).unwrap(), Record::default());
    let linked = super::source(temp.path(), "linked", &[("a", ".")]);
    install(&home, &linked).unwrap();
    source.pack = None;
    let error = Transaction::stage(&home, &source)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains("already provided") && error.contains("rimz packs remove a"),
        "{error}"
    );
}

#[test]
fn a_record_change_while_staged_aborts_before_publication_or_gc() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let linked = super::source(temp.path(), "linked", &[("linked", ".")]);
    install(&home, &linked).unwrap();
    let source = repository(&temp.path().join("repo"), &[("a", false, &[])]);
    let staged = stage(&home, &source);
    exclude(
        &home,
        &name("linked"),
        &ExclusionTarget::Type(ItemType::Teams),
    )
    .unwrap();
    let before = fs::read(home.join("packs/installed.json")).unwrap();
    let result = staged.commit();
    let error = result
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        error.contains("nothing changed") && error.contains("again"),
        "{error}"
    );
    assert_eq!(fs::read(home.join("packs/installed.json")).unwrap(), before);
    assert_eq!(names(&home), BTreeSet::from(["linked".to_owned()]));
}

#[test]
fn sibling_requirements_are_transitive_and_cycles_end() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = repository(
        &temp.path().join("repo"),
        &[
            ("a", true, &["b"]),
            ("b", false, &["c"]),
            ("c", false, &["a"]),
        ],
    );
    let staged = stage(&home, &source);
    assert_eq!(
        staged
            .candidate()
            .unwrap()
            .record
            .sources
            .iter()
            .flat_map(|source| &source.packs)
            .map(|pack| pack.name.to_string())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])
    );
    staged.commit().unwrap();
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "b".to_owned(), "c".to_owned()])
    );
}

fn redirected(root: &Path, url: &str, target: &Path) -> crate::packs::git::Client {
    let config = root.join("gitconfig");
    write(
        &config,
        &format!(
            "[url {:?}]\n insteadOf = {url}\n",
            url::Url::from_directory_path(target).unwrap().as_str()
        ),
    );
    let mut client = crate::packs::git::Client::default();
    client.env.insert(
        "GIT_CONFIG_GLOBAL".to_owned(),
        config.to_str().unwrap().to_owned(),
    );
    client
}

#[test]
fn cross_source_requirements_move_a_following_source_only_when_the_pack_is_absent() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let other = temp.path().join("other");
    repository(&other, &[("one", false, &[]), ("two", false, &[])]);
    let client = redirected(temp.path(), "https://github.com/o/other", &other);
    let required = GitSource {
        source: "https://github.com/o/other".to_owned(),
        url: "https://github.com/o/other".to_owned(),
        pack: Some(name("one")),
        reference: None,
    };
    Transaction::stage_with(&home, &required, &client)
        .unwrap()
        .commit()
        .unwrap();
    write(
        &other.join("rimz-packs.toml"),
        "[[pack]]\nname = 'one'\ndescription = 'One'\npath = 'one'\n[[pack]]\nname = 'two'\ndescription = 'Two'\npath = 'two'\nrequires = ['last']\n[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\n[[pack]]\nname = 'last'\ndescription = 'Last'\npath = 'last'\n",
    );
    for pack in ["extra", "last"] {
        write(&other.join(pack).join("README.md"), pack);
    }
    git(&other, &["add", "."]);
    git(&other, &["commit", "-m", "add extra"]);
    // `two` joins and is visited at the installed commit before the sibling `b` moves the
    // source for `extra`, and `two`'s new requirement is resolved at the commit it moved to.
    let source = repository(
        &temp.path().join("repo"),
        &[
            ("a", true, &["github:o/other/two", "b"]),
            ("b", false, &["github:o/other/extra"]),
        ],
    );
    let before = Record::read(&home).unwrap();
    let transaction = Transaction::stage_with(&home, &source, &client).unwrap();
    let candidate = transaction.candidate().unwrap();
    assert_eq!(
        candidate.record.sources[0].commit,
        Some(git(&other, &["rev-parse", "HEAD"]))
    );
    assert!(!candidate.record.sources[0].pinned);
    assert_eq!(
        candidate.record.sources[0]
            .packs
            .iter()
            .map(|pack| pack.name.as_str())
            .collect::<Vec<_>>(),
        ["one", "two", "extra", "last"]
    );
    assert_eq!(Record::read(&home).unwrap(), before);
}

#[test]
fn a_pinned_source_missing_its_own_sibling_requirement_names_no_reinstall() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let mut source = repository(&temp.path().join("repo"), &[("a", true, &["b"])]);
    source.reference = Some("main".to_owned());
    let error = Transaction::stage(&home, &source)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert_eq!(
        error,
        format!("Git source {}: no pack b; available packs: a", source.url)
    );
    assert!(!home.join("packs/installed.json").exists());
}

#[test]
fn cross_source_requirements_fetch_transitively_but_an_installed_name_needs_no_fetch() {
    let temp = tempfile::tempdir().unwrap();
    let other = temp.path().join("other");
    repository(&other, &[("extra", false, &["last"]), ("last", false, &[])]);
    let client = redirected(temp.path(), "https://github.com/o/other", &other);
    let source = repository(
        &temp.path().join("repo"),
        &[("a", false, &["github:o/other/extra"])],
    );
    let home = temp.path().join("home");
    Transaction::stage_with(&home, &source, &client)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "extra".to_owned(), "last".to_owned()])
    );
    let home = temp.path().join("linked-home");
    let linked = super::source(temp.path(), "linked", &[("extra", ".")]);
    install(&home, &linked).unwrap();
    fs::remove_dir_all(&other).unwrap();
    let result = Transaction::stage_with(&home, &source, &client).and_then(Transaction::commit);
    assert!(
        result.is_ok(),
        "an installed name must avoid fetching its source: {result:?}"
    );
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "extra".to_owned()])
    );
}

#[test]
fn an_additional_requirement_from_an_installed_source_uses_its_existing_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let other = temp.path().join("other");
    repository(&other, &[("one", true, &[]), ("extra", false, &[])]);
    let client = redirected(temp.path(), "https://github.com/o/other", &other);
    let required = GitSource {
        source: "https://github.com/o/other".to_owned(),
        url: "https://github.com/o/other".to_owned(),
        pack: None,
        reference: None,
    };
    Transaction::stage_with(&home, &required, &client)
        .unwrap()
        .commit()
        .unwrap();
    let before = Record::read(&home).unwrap();
    fs::remove_dir_all(&other).unwrap();
    let source = repository(
        &temp.path().join("repo"),
        &[("a", false, &["github:o/other/extra"])],
    );
    let result = Transaction::stage_with(&home, &source, &client).and_then(Transaction::commit);
    assert!(
        result.is_ok(),
        "installed-source requirements must not resolve or fetch: {result:?}"
    );
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "one".to_owned(), "extra".to_owned()])
    );
    assert_eq!(
        Record::read(&home).unwrap().sources[0].commit,
        before.sources[0].commit
    );
}

#[test]
fn malformed_requirements_warn_and_do_not_withhold_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = repository(
        &temp.path().join("repo"),
        &[(
            "a",
            false,
            &["https://example.test/repo", "crew/", "a@v1", " "],
        )],
    );
    let staged = stage(&home, &source);
    let candidate = staged.candidate().unwrap();
    assert_eq!(candidate.warnings.len(), 4, "{candidate:?}");
    assert_eq!(
        candidate
            .packs
            .iter()
            .map(|pack| pack.name.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
}

#[test]
fn linked_installs_report_unmet_requirements_without_fetching_or_selecting_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = super::source(temp.path(), "linked", &[("a", "a"), ("b", "b")]);
    write(
        &source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'a'\ndescription = 'A'\npath = 'a'\ndefault = true\nrequires = ['b', 'skills/essential']\n[[pack]]\nname = 'b'\ndescription = 'B'\npath = 'b'\n",
    );
    let installed = install(&home, &source).unwrap();
    assert_eq!(
        names(&home),
        BTreeSet::from(["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(installed.warnings.len(), 1, "{installed:?}");
    assert!(
        installed.warnings[0]
            .message
            .contains("rimz packs install skills/essential"),
        "{installed:?}"
    );
    assert!(!home.join("packs/.checkouts").exists());
}

fn seeded_git(home: &Path, source: &GitSource) -> PathBuf {
    let old = super::source(
        &home.join("packs/.checkouts"),
        &format!("repo-{}", "a".repeat(40)),
        &[("crew", ".")],
    );
    let mut record = seed(home, &[(&old, "repo", &["crew"])]);
    record.sources[0].source = source.source.clone();
    record.sources[0].url = Some(source.url.clone());
    record.sources[0].linked = false;
    record.sources[0].reference = Some("main".to_owned());
    record.sources[0].commit = Some("a".repeat(40));
    write_record(&home.join("packs"), &record).unwrap();
    old
}

#[test]
fn a_retired_checkout_survives_publication_then_the_next_mutation_collects_it() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let source = repository(&repo, &[("crew", false, &[])]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    let old = seeded_git(&home, &source);
    stage(&home, &source).commit().unwrap();
    let record = Record::read(&home).unwrap();
    assert!(
        record.sources[0].commit.as_deref() == Some(commit.as_str()) && old.is_dir(),
        "publication must retain the old tree: {record:?}"
    );
    let entry = home.join("packs/repo");
    let current = fs::canonicalize(&entry).unwrap();
    include(
        &home,
        &name("crew"),
        &ExclusionTarget::Type(ItemType::Teams),
    )
    .unwrap();
    assert!(!old.exists() && current.is_dir());
    remove(&home, &name("crew")).unwrap();
    assert!(!entry.is_symlink() && current.is_dir());
    let linked = super::source(temp.path(), "linked", &[("other", ".")]);
    install(&home, &linked).unwrap();
    assert!(!current.exists() && repo.join("rimz-packs.toml").is_file());
}

#[test]
fn a_staged_checkout_is_held_during_concurrent_gc_then_collectible_on_abandonment() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let source = repository(&repo, &[("crew", false, &[])]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    seeded_git(&home, &source);
    let staged = stage(&home, &source);
    let checkout = home.join("packs/.checkouts").join(format!("repo-{commit}"));
    exclude(
        &home,
        &name("crew"),
        &ExclusionTarget::Type(ItemType::Teams),
    )
    .unwrap();
    assert!(
        checkout.is_dir(),
        "staging must hold its checkout through GC: {checkout:?}"
    );
    drop(staged);
    include(
        &home,
        &name("crew"),
        &ExclusionTarget::Type(ItemType::Teams),
    )
    .unwrap();
    assert!(
        !checkout.exists(),
        "abandoned checkout must be collectible: {checkout:?}"
    );
}

#[test]
fn staging_the_same_commit_reuses_the_checkout_without_fetching_again() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = repository(&temp.path().join("repo"), &[("crew", false, &[])]);
    stage(&home, &source).commit().unwrap();
    let trace = temp.path().join("git.trace");
    let mut client = crate::packs::git::Client::default();
    client
        .env
        .insert("GIT_TRACE".to_owned(), trace.to_str().unwrap().to_owned());
    let staged = Transaction::stage_with(&home, &source, &client).unwrap();
    assert!(
        staged.up_to_date(),
        "the same source/ref/commit/selection must be up to date"
    );
    let trace = fs::read_to_string(&trace).unwrap_or_default();
    assert!(
        trace.contains("ls-remote") && !trace.contains(" fetch "),
        "{trace}"
    );
}
