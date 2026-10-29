use super::*;
use std::fs;
use std::os::unix::fs::symlink;

fn name(value: &str) -> PackName {
    PackName(value.to_owned())
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn source(root: &Path, directory: &str, packs: &[(&str, &str)]) -> PathBuf {
    let source = root.join(directory);
    fs::create_dir_all(&source).unwrap();
    let mut manifest = String::new();
    for (name, path) in packs {
        fs::create_dir_all(source.join(path)).unwrap();
        manifest.push_str(&format!(
            "[[pack]]\nname = {name:?}\ndescription = 'A pack'\npath = {path:?}\n"
        ));
    }
    write(&source.join("rimz-packs.toml"), &manifest);
    source
}

fn seed(home: &Path, sources: &[(&Path, &str, &[&str])]) -> Record {
    let record = Record {
        version: 1,
        sources: sources
            .iter()
            .map(|(source, entry, packs)| {
                fs::create_dir_all(home.join("packs")).unwrap();
                symlink(source, home.join("packs").join(entry)).unwrap();
                InstalledSource {
                    entry: (*entry).to_owned(),
                    source: source.to_str().unwrap().to_owned(),
                    linked: true,
                    reference: None,
                    commit: None,
                    packs: packs
                        .iter()
                        .map(|pack| InstalledPack {
                            name: name(pack),
                            excluded: Exclusions::default(),
                        })
                        .collect(),
                }
            })
            .collect(),
    };
    write(
        &home.join("packs/installed.json"),
        &serde_json::to_string(&record).unwrap(),
    );
    record
}

fn item(kind: ItemType, value: &str) -> ExclusionTarget {
    ExclusionTarget::Item(ItemKey {
        kind,
        name: value.to_owned(),
    })
}

#[test]
fn manifest_reads_all_fields_and_reports_unknown_keys() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    write(
        &source.join("rimz-packs.toml"),
        "future = true\n[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\ndefault = true\nrequires = ['skills/essentials']\nrimz = '>=0.43'\nnew = 1\n",
    );
    let manifest = Manifest::read(&source);
    assert!(manifest.is_ok(), "{manifest:?}");
    let manifest = manifest.unwrap();
    let pack = &manifest.packs[0];
    assert_eq!(pack.name, name("crew"));
    assert_eq!(pack.description, "Crew");
    assert_eq!(pack.path, Path::new("."));
    assert!(pack._default);
    assert_eq!(pack.requires, ["skills/essentials"]);
    assert_eq!(pack.rimz.as_deref(), Some(">=0.43"));
    assert_eq!(manifest.warnings.len(), 2);
    for (warning, key) in manifest.warnings.iter().zip(["future", "new"]) {
        assert_eq!(warning.path, source.join("rimz-packs.toml"));
        assert!(warning.message.contains(key), "{warning}");
    }
}

#[test]
fn resolve_separates_manifest_warnings_from_pack_problems() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "linked", &["crew"])]);
    write(
        &source.join("rimz-packs.toml"),
        "future = true\n[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
    );
    let resolved = resolve(&home);
    assert!(resolved.problems.is_empty(), "{resolved:?}");
    assert_eq!(resolved.warnings.len(), 1);
    assert!(resolved.warnings[0].message.contains("future"));
    assert_eq!(resolved.packs.len(), 1);
}

#[test]
fn manifest_refusals_name_the_path_and_field() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let base = "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n";
    let cases = [
        (String::new(), "pack"),
        ("pack = []".to_owned(), "pack"),
        (base.replace("name = 'crew'\n", ""), "name"),
        (base.replace("description = 'Crew'\n", ""), "description"),
        (base.replace("path = '.'\n", ""), "path"),
        (base.replace("'crew'", "'bad:name'"), "name"),
        (base.replace("'crew'", "''"), "name"),
        (base.replace("'Crew'", "'  '"), "description"),
        (format!("{base}{base}"), "name"),
        (base.replace("path = '.'", "path = '/tmp'"), "path"),
        (base.replace("path = '.'", "path = '../outside'"), "path"),
        (format!("{base}default = 'yes'"), "default"),
        (format!("{base}requires = 'crew'"), "requires"),
        (format!("{base}requires = [1]"), "requires"),
        (format!("{base}requires = [' ']"), "requires"),
        (format!("{base}rimz = 43"), "rimz"),
        (format!("{base}rimz = ' '"), "rimz"),
    ];
    let results: Vec<_> = cases
        .iter()
        .map(|(text, field)| {
            write(&source.join("rimz-packs.toml"), text);
            let result = Manifest::read(&source);
            let valid = result.as_ref().err().is_some_and(|error| {
                error.path == source.join("rimz-packs.toml") && error.message.contains(field)
            });
            (field, valid, format!("{result:?}"))
        })
        .collect();
    assert_eq!(
        results
            .iter()
            .map(|(_, valid, _)| *valid)
            .collect::<Vec<_>>(),
        vec![true; cases.len()],
        "{results:#?}"
    );
}

#[test]
fn manifest_checks_missing_escaping_and_duplicate_pack_paths() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", "inside")]);
    fs::create_dir(temp.path().join("outside")).unwrap();
    symlink(temp.path().join("outside"), source.join("escape")).unwrap();
    symlink(source.join("inside"), source.join("alias")).unwrap();
    let cases = [
        (vec![("crew", "missing")], "missing"),
        (vec![("crew", "escape")], "path"),
        (vec![("crew", "inside"), ("other", "./inside/.")], "path"),
        (vec![("crew", "inside"), ("other", "alias")], "path"),
    ];
    let results: Vec<_> = cases
        .iter()
        .map(|(packs, expected)| {
            let text: String = packs
                .iter()
                .map(|(name, path)| {
                    format!("[[pack]]\nname = {name:?}\ndescription = 'Pack'\npath = {path:?}\n")
                })
                .collect();
            write(&source.join("rimz-packs.toml"), &text);
            let result = Manifest::read(&source);
            let valid = result.as_ref().err().is_some_and(|error| {
                error.path == source.join("rimz-packs.toml") && error.message.contains(expected)
            });
            (valid, format!("{result:?}"))
        })
        .collect();
    assert_eq!(
        results.iter().map(|(valid, _)| *valid).collect::<Vec<_>>(),
        vec![true; cases.len()],
        "{results:#?}"
    );
}

#[test]
fn record_reads_missing_as_empty_and_roundtrips_the_contract() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(Record::read(temp.path()), Ok(Record::default()));
    let text = r#"{"version":1,"future":true,"sources":[{"entry":"rimz-crew","source":"/src/crew","linked":true,"ref":null,"commit":null,"future":0,"packs":[{"name":"crew","excluded":{"types":["teams"],"items":[{"type":"agents","name":"ralph"}]}}]}]}"#;
    write(&temp.path().join("packs/installed.json"), text);
    let record = Record::read(temp.path()).unwrap();
    let encoded = serde_json::to_string(&record).unwrap();
    write(&temp.path().join("packs/installed.json"), &encoded);
    assert_eq!(Record::read(temp.path()), Ok(record.clone()));
    let excluded = &record.sources[0].packs[0].excluded;
    assert!(excluded.contains(ItemType::Teams, "any"));
    assert!(excluded.contains(ItemType::Agents, "ralph"));
    assert!(!excluded.contains(ItemType::Subagents, "ralph"));
    assert_eq!(
        serde_json::to_value(record).unwrap()["sources"][0]["ref"],
        serde_json::Value::Null
    );
}

#[test]
fn record_refuses_newer_versions_and_unsafe_or_duplicate_claims() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let home = temp.path().join("home");
    let record = seed(&home, &[(&source, "source", &["crew"])]);
    let mut cases = Vec::new();
    let mut newer = record.clone();
    newer.version = 2;
    cases.push((newer, "newer RimZ"));
    let mut unsafe_entry = record.clone();
    unsafe_entry.sources[0].entry = "../outside".into();
    cases.push((unsafe_entry, "entry"));
    let mut duplicate = record.clone();
    let duplicated_pack = duplicate.sources[0].packs[0].clone();
    duplicate.sources[0].packs.push(duplicated_pack);
    cases.push((duplicate, "crew"));
    let mut bad_name = record;
    bad_name.sources[0].packs[0].name = name("bad:name");
    cases.push((bad_name, "name"));
    let results: Vec<_> = cases
        .iter()
        .map(|(record, expected)| {
            write(
                &home.join("packs/installed.json"),
                &serde_json::to_string(record).unwrap(),
            );
            let resolved = resolve(&home);
            let valid = resolved.packs.is_empty()
                && resolved.problems.iter().any(|problem| {
                    problem.path == home.join("packs/installed.json")
                        && problem.message.contains(expected)
                });
            (valid, format!("{resolved:?}"))
        })
        .collect();
    assert_eq!(
        results.iter().map(|(valid, _)| *valid).collect::<Vec<_>>(),
        vec![true; cases.len()],
        "{results:#?}"
    );
}

#[test]
fn newer_record_is_diagnosed_before_decoding_its_schema() {
    let temp = tempfile::tempdir().unwrap();
    let cases = [
        r#"{"version":2}"#,
        r#"{"version":2,"sources":"future shape"}"#,
    ];
    let results: Vec<_> = cases
        .iter()
        .map(|text| {
            write(&temp.path().join("packs/installed.json"), text);
            let resolved = resolve(temp.path());
            let valid = resolved.packs.is_empty()
                && resolved
                    .problems
                    .iter()
                    .any(|error| error.message.contains("newer RimZ"));
            (valid, resolved)
        })
        .collect();
    assert_eq!(
        results.iter().map(|(valid, _)| *valid).collect::<Vec<_>>(),
        [true; 2],
        "{results:?}"
    );
}

#[test]
fn manifest_validates_the_normalized_root_it_exposes() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    fs::create_dir_all(source.join("deep/nested")).unwrap();
    fs::create_dir(source.join("deep/actual")).unwrap();
    symlink(source.join("deep/nested"), source.join("link")).unwrap();
    write(
        &source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = 'link/../actual'\n",
    );
    let result = Manifest::read(&source);
    assert!(
        result.is_err(),
        "the exposed source/actual directory does not exist: {result:?}"
    );
}

#[test]
fn installed_set_is_ordered_lexical_and_live() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(
        temp.path(),
        "source",
        &[("crew", "inside/../inside"), ("skills", ".")],
    );
    let home = temp.path().join("home");
    seed(&home, &[(&source, "linked", &["skills", "crew"])]);
    let resolved = resolve(&home);
    assert_eq!(
        resolved
            .packs
            .iter()
            .map(|pack| pack.name.as_str())
            .collect::<Vec<_>>(),
        ["skills", "crew"],
        "{resolved:?}"
    );
    assert!(resolved.problems.is_empty(), "{resolved:?}");
    assert_eq!(resolved.packs[1].root, home.join("packs/linked/inside"));
    assert_eq!(resolved.packs[1].source, source.to_str().unwrap());
    assert!(resolved.packs[1].linked);
    assert_eq!(resolved.packs[1].entry, "linked");
    write(
        &source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Edited live'\npath = '.'\n[[pack]]\nname = 'new'\ndescription = 'Not installed'\npath = 'inside'\n",
    );
    let resolved = resolve(&home);
    assert_eq!(resolved.packs.len(), 1);
    assert_eq!(resolved.packs[0].description, "Edited live");
    assert!(
        resolved
            .problems
            .iter()
            .any(|problem| problem.message.contains("skills")),
        "{resolved:?}"
    );
}

#[test]
fn unreadable_pack_does_not_hide_other_packs_and_orphans_never_load() {
    let mut results = Vec::new();
    for cause in [
        "missing manifest",
        "invalid manifest",
        "missing pack",
        "missing path",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let good = source(temp.path(), "good", &[("good", ".")]);
        let bad = source(temp.path(), "bad", &[("bad", "inner"), ("sibling", ".")]);
        let home = temp.path().join("home");
        seed(
            &home,
            &[
                (&bad, "bad", &["bad", "sibling"]),
                (&good, "good", &["good"]),
            ],
        );
        let orphan = source(temp.path(), "orphan", &[("orphan", ".")]);
        symlink(&orphan, home.join("packs/orphan")).unwrap();
        match cause {
            "missing manifest" => fs::remove_file(bad.join("rimz-packs.toml")).unwrap(),
            "invalid manifest" => write(&bad.join("rimz-packs.toml"), "not toml"),
            "missing pack" => {
                source(temp.path(), "bad", &[("sibling", ".")]);
            }
            "missing path" => fs::remove_dir(bad.join("inner")).unwrap(),
            _ => unreachable!(),
        }
        let resolved = resolve(&home);
        let expected = if matches!(cause, "missing pack" | "missing path") {
            vec!["sibling", "good"]
        } else {
            vec!["good"]
        };
        let valid = resolved
            .packs
            .iter()
            .map(|pack| pack.name.as_str())
            .collect::<Vec<_>>()
            == expected
            && !resolved.problems.is_empty()
            && resolved
                .problems
                .iter()
                .all(|problem| problem.path.starts_with(home.join("packs/bad")));
        results.push((cause, valid, format!("{resolved:?}")));
    }
    assert_eq!(
        results
            .iter()
            .map(|(_, valid, _)| *valid)
            .collect::<Vec<_>>(),
        vec![true; results.len()],
        "{results:#?}"
    );
}

#[test]
fn claims_share_home_precedence_exclusions_and_conflict_fixes() {
    let claim = |pack: &str, excluded| PackClaim {
        name: "coder".into(),
        pack: name(pack),
        path: PathBuf::from(format!("/{pack}/agents/coder.md")),
        excluded,
    };
    let a = claim("a", false);
    let b = claim("b", false);
    let mut results = Vec::new();
    let sole = resolve_claims([], [a.clone()]);
    results.push(sole.get("coder") == Some(&ClaimResolution::Pack(a.clone())));
    results.push(resolve_claims([], [claim("a", true)]).is_empty());
    results.push(
        resolve_claims([], [a.clone(), claim("b", true)]).get("coder")
            == Some(&ClaimResolution::Pack(a.clone())),
    );
    let home = PathBuf::from("/home/agents/coder.md");
    results.push(
        resolve_claims([("coder".into(), home.clone())], [a.clone(), b.clone()]).get("coder")
            == Some(&ClaimResolution::Home(home)),
    );
    let conflicts = resolve_claims([], [a.clone(), b.clone(), claim("c", false)]);
    results.push(match conflicts.get("coder") {
        Some(ClaimResolution::Conflict { claims, message }) => {
            claims.len() == 3
                && ["a", "b", "c"].iter().all(|pack| {
                    message.contains(&format!("/{pack}/agents/coder.md"))
                        && message.contains(&format!("rimz packs exclude {pack}:coder"))
                })
                && message.contains("file of your own named `coder`")
        }
        _ => false,
    });
    assert_eq!(results, [true; 5], "{conflicts:?}");
}

#[test]
fn install_arguments_accept_only_explicit_path_forms() {
    let accepted = [".", "..", "/src/crew", "./crew", "../crew", "~/crew", "~"];
    let refused = [
        "crew",
        "skills/git",
        "github:owner/repo",
        "https://github.com/a/b",
        "",
        "foo/../crew",
        "~user",
        "~user/crew",
    ];
    let results: Vec<_> = accepted
        .iter()
        .map(|value| (value, value.parse::<InstallSource>().is_ok()))
        .chain(refused.iter().map(|value| {
            (
                value,
                value.parse::<InstallSource>().err().is_some_and(|error| {
                    matches!(error, PackErr::NotPath { .. }) && error.to_string().contains("./")
                }),
            )
        }))
        .collect();
    assert_eq!(
        results.iter().map(|(_, valid)| *valid).collect::<Vec<_>>(),
        vec![true; results.len()],
        "{results:?}"
    );
}

#[test]
fn install_links_canonical_sources_replaces_orphans_and_suffixes_entries() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let first = source(temp.path(), "a/crew.repo", &[("crew", ".")]);
    let second = source(temp.path(), "b/crew.repo", &[("tools", ".")]);
    write(&home.join("packs/crew-repo"), "orphan");
    let alias = temp.path().join("alias");
    symlink(&first, &alias).unwrap();
    let installed = install(&home, &alias);
    assert!(installed.is_ok(), "{installed:?}");
    let installed = installed.unwrap();
    assert_eq!(installed.status, InstallStatus::Installed);
    assert_eq!(installed.entry, "crew-repo");
    assert_eq!(installed.added, [name("crew")]);
    assert_eq!(
        fs::read_link(home.join("packs/crew-repo")).unwrap(),
        fs::canonicalize(&first).unwrap()
    );
    assert_eq!(
        install(&home, &first).unwrap().status,
        InstallStatus::UpToDate
    );
    let installed = install(&home, &second).unwrap();
    assert_eq!(installed.entry, "crew-repo-2");
    assert_eq!(resolve(&home).packs.len(), 2);
    fs::remove_file(home.join("packs/crew-repo")).unwrap();
    fs::create_dir(home.join("packs/crew-repo")).unwrap();
    write(&home.join("packs/crew-repo/leftover"), "orphaned tree");
    assert_eq!(
        install(&home, &first).unwrap().status,
        InstallStatus::Refreshed
    );
    assert_eq!(fs::read_link(home.join("packs/crew-repo")).unwrap(), first);
}

#[test]
fn long_source_basenames_leave_room_for_atomic_publication_and_collisions() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let basename = "x".repeat(230);
    for (parent, pack) in [("first", "crew"), ("second", "tools")] {
        let source = source(temp.path(), &format!("{parent}/{basename}"), &[(pack, ".")]);
        let result = install(&home, &source);
        assert!(
            result.is_ok(),
            "long source basename failed to install: {result:?}"
        );
    }
    assert_eq!(resolve(&home).packs.len(), 2);
}

#[test]
fn reinstall_refreshes_names_keeps_surviving_exclusions_and_warns() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", "."), ("old", "old")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "source", &["crew", "old"])]);
    let mut record: Record =
        serde_json::from_str(&fs::read_to_string(home.join("packs/installed.json")).unwrap())
            .unwrap();
    record.sources[0].packs[0]
        .excluded
        .types
        .insert(ItemType::Teams);
    write(
        &home.join("packs/installed.json"),
        &serde_json::to_string(&record).unwrap(),
    );
    let manifest = "future = true\n[[pack]]\nname = 'new'\ndescription = 'New'\npath = 'old'\n[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n";
    write(&source.join("rimz-packs.toml"), manifest);
    let installed = install(&home, &source);
    assert!(installed.is_ok(), "{installed:?}");
    let installed = installed.unwrap();
    assert_eq!(installed.status, InstallStatus::Refreshed);
    assert_eq!(installed.packs, [name("new"), name("crew")]);
    assert_eq!(installed.added, [name("new")]);
    assert_eq!(installed.dropped, [name("old")]);
    assert_eq!(installed.warnings.len(), 1);
    let resolved = resolve(&home);
    assert_eq!(
        resolved.packs[1].excluded.types,
        BTreeSet::from([ItemType::Teams])
    );
}

#[test]
fn install_refuses_overlapping_homes_missing_sources_and_name_collisions() {
    let temp = tempfile::tempdir().unwrap();
    let home = source(temp.path(), "home", &[("home", ".")]);
    let parent = source(temp.path(), ".", &[("parent", ".")]);
    let inside = source(&home, "child", &[("child", ".")]);
    let first = source(temp.path(), "first", &[("crew", ".")]);
    let second = source(temp.path(), "second", &[("crew", ".")]);
    seed(&home, &[(&first, "first", &["crew"])]);
    let collision = install(&home, &second);
    let collision_refused = matches!(
        collision,
        Err(PackErr::NameTaken { name: pack, source_path })
            if pack == name("crew") && source_path == first.to_str().unwrap()
    );
    let missing = install(&home, &temp.path().join("missing"));
    let results: Vec<_> = [&home, &parent, &inside]
        .into_iter()
        .map(|source| matches!(install(&home, source), Err(PackErr::OverlappingHome { .. })))
        .chain([
            collision_refused,
            matches!(missing, Err(PackErr::Io { .. })),
        ])
        .collect();
    assert_eq!(results, [true; 5]);
    assert_eq!(Record::read(&home).unwrap().sources.len(), 1);
}

#[test]
fn remove_drops_only_the_named_pack_then_unlinks_without_touching_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", "."), ("tools", "tools")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "source", &["crew", "tools"])]);
    let removed = remove(&home, &name("crew"));
    assert!(removed.is_ok(), "{removed:?}");
    let removed = removed.unwrap();
    assert_eq!(removed.pack, name("crew"));
    assert_eq!(removed.source, source.to_str().unwrap());
    assert!(removed.removed_entry.is_none());
    assert!(home.join("packs/source").is_symlink());
    let removed = remove(&home, &name("tools")).unwrap();
    assert_eq!(removed.removed_entry, Some(home.join("packs/source")));
    assert_eq!(Record::read(&home).unwrap(), Record::default());
    assert!(!home.join("packs/source").is_symlink());
    assert!(source.join("rimz-packs.toml").is_file());
    assert!(matches!(
        remove(&home, &name("missing")),
        Err(PackErr::UnknownPack { .. })
    ));
}

#[test]
fn remove_commits_the_record_before_entry_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "source", &["crew"])]);
    fs::remove_file(home.join("packs/source")).unwrap();
    fs::create_dir(home.join("packs/source")).unwrap();
    write(&home.join("packs/source/unexpected"), "not a linked entry");
    let result = remove(&home, &name("crew"));
    assert!(result.is_err(), "{result:?}");
    let record: Record =
        serde_json::from_str(&fs::read_to_string(home.join("packs/installed.json")).unwrap())
            .unwrap();
    assert!(
        record.sources.is_empty(),
        "record must commit even if cleanup fails: {record:?}"
    );
    assert!(home.join("packs/source/unexpected").is_file());
    assert!(resolve(&home).packs.is_empty());
}

#[test]
fn exclusions_are_record_only_typed_idempotent_and_keep_item_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "source", &["crew"])]);
    let pack = name("crew");
    let target = item(ItemType::Agents, "no-longer-shipped");
    let whole = ExclusionTarget::Type(ItemType::Agents);
    let changed = exclude(&home, &pack, &target);
    assert!(
        matches!(changed, Ok(ExclusionOutcome::Changed)),
        "{changed:?}"
    );
    assert_eq!(
        exclude(&home, &pack, &target).unwrap(),
        ExclusionOutcome::AlreadyExcluded
    );
    assert_eq!(
        exclude(&home, &pack, &whole).unwrap(),
        ExclusionOutcome::Changed
    );
    assert_eq!(
        exclude(&home, &pack, &whole).unwrap(),
        ExclusionOutcome::AlreadyExcluded
    );
    assert_eq!(
        exclude(&home, &pack, &item(ItemType::Agents, "other")).unwrap(),
        ExclusionOutcome::AlreadyExcluded
    );
    let refused = include(&home, &pack, &target);
    assert!(
        matches!(refused, Err(PackErr::TypeExcluded { .. })),
        "{refused:?}"
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("rimz packs include crew --type agents")
    );
    assert_eq!(
        include(&home, &pack, &whole).unwrap(),
        ExclusionOutcome::Changed
    );
    let resolved = resolve(&home);
    assert!(
        resolved.packs[0]
            .excluded
            .contains(ItemType::Agents, "no-longer-shipped")
    );
    assert!(
        !resolved.packs[0]
            .excluded
            .contains(ItemType::Subagents, "no-longer-shipped")
    );
    assert_eq!(
        include(&home, &pack, &target).unwrap(),
        ExclusionOutcome::Changed
    );
    assert_eq!(
        include(&home, &pack, &target).unwrap(),
        ExclusionOutcome::AlreadyIncluded
    );
    assert_eq!(
        include(&home, &pack, &whole).unwrap(),
        ExclusionOutcome::AlreadyIncluded
    );
    for result in [
        exclude(&home, &name("missing"), &target),
        include(&home, &name("missing"), &target),
    ] {
        assert!(
            matches!(result, Err(PackErr::UnknownPack { .. })),
            "{result:?}"
        );
    }
}

#[test]
fn concurrent_mutations_do_not_lose_record_updates() {
    let temp = tempfile::tempdir().unwrap();
    let source = source(temp.path(), "source", &[("crew", ".")]);
    let home = temp.path().join("home");
    seed(&home, &[(&source, "source", &["crew"])]);
    let results = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let home = &home;
                scope.spawn(move || {
                    exclude(
                        home,
                        &name("crew"),
                        &item(ItemType::Skills, &format!("skill-{index}")),
                    )
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Ok(ExclusionOutcome::Changed))),
        "{results:?}"
    );
    let record = Record::read(&home).unwrap();
    assert_eq!(record.sources[0].packs[0].excluded.items.len(), 8);
}
