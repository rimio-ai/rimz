use super::*;

fn publish(repo: &Path) -> String {
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "update fixture"]);
    git(
        repo,
        &["tag", "-f", &format!("v{}", env!("CARGO_PKG_VERSION"))],
    );
    git_stdout(repo, &["rev-parse", "HEAD"])
}

fn install(fixture: &Fixture, source: &str) {
    let output = fixture.run(&["packs", "install", source, "--yes"]);
    assert!(output.status.success(), "{output:?}");
}

fn worker_mode(fixture: &Fixture, mode: &str) {
    let path = fixture.crew.join("agents/worker.md");
    let text = fs::read_to_string(&path).unwrap();
    write(path, &text.replace("mode: auto", &format!("mode: {mode}")));
}

fn add_cross_source_requirement(fixture: &Fixture) -> (String, String) {
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    let manifest = fs::read_to_string(skills.join("rimz-packs.toml")).unwrap();
    write(skills.join("extra/traits/extra.md"), "Extra.");
    write(
        skills.join("rimz-packs.toml"),
        &format!("{manifest}[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\n"),
    );
    write(
        skills.join("agents/helper.md"),
        "---\ndescription: Helper\nagent: claude\nmode: yolo\ntools: [Read]\n---\nHelp.",
    );
    let skills_commit = publish(&skills);
    let path = fixture.crew.join("rimz-packs.toml");
    let manifest = fs::read_to_string(&path).unwrap();
    write(
        path,
        &manifest.replace("['skills/essential']", "['skills/extra']"),
    );
    (publish(&fixture.crew), skills_commit)
}

#[test]
fn one_update_reviews_and_moves_a_following_dependency_that_lacks_a_new_requirement() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let (crew_commit, skills_commit) = add_cross_source_requirement(&fixture);
    let held = fixture.run(&["packs", "update", "crew"]);
    assert_eq!(held.status.code(), Some(1), "{held:?}");
    let stdout = String::from_utf8_lossy(&held.stdout);
    assert!(
        stdout.contains("helper")
            && stdout.contains("mode: yolo [widened]")
            && stdout.contains("Held crew"),
        "the dependency's existing pack must be reviewed: {held:?}"
    );
    assert_eq!(fixture.record(), before);
    let output = fixture.run(&["packs", "update", "--yes"]);
    assert!(
        output.status.success(),
        "one update must move both sources: {output:?}"
    );
    let record = fixture.record();
    assert_eq!(record["sources"][0]["commit"], crew_commit);
    assert_eq!(record["sources"][1]["commit"], skills_commit);
    assert_eq!(record["sources"][1]["pinned"], false);
    assert_eq!(record["sources"][1]["packs"].as_array().unwrap().len(), 2);
}

#[test]
fn a_pinned_dependency_missing_a_new_requirement_refuses_with_a_reinstall_command() {
    let fixture = Fixture::new();
    install(&fixture, "skills/essential@main");
    install(&fixture, "crew");
    let before = fixture.record();
    add_cross_source_requirement(&fixture);
    let output = fixture.run(&["packs", "update", "crew", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "Git source skills: no pack extra at its installed commit; available packs: essential"
        ) && stderr.contains(
            "retry after `rimz packs install skills/extra`, which unpins the source, or pin a ref \
             that ships it"
        ) && !stderr.contains(".checkouts/"),
        "{stderr}"
    );
    assert_eq!(fixture.record(), before);
}

#[test]
fn a_following_dependency_still_missing_a_requirement_names_no_reinstall() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let path = fixture.crew.join("rimz-packs.toml");
    let manifest = fs::read_to_string(&path).unwrap();
    write(
        path,
        &manifest.replace("['skills/essential']", "['skills/missing']"),
    );
    publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "crew", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "Git source skills: no pack missing at its installed commit; available packs: essential"
        ) && !stderr.contains("retry after"),
        "{stderr}"
    );
    assert_eq!(fixture.record(), before);
}

#[test]
fn provider_switches_with_unchanged_grants_apply_without_asking_and_print_the_new_kind() {
    let fixture = Fixture::new();
    write(
        fixture.crew.join("agents/codex.md"),
        "---\ndescription: Base\n---\nBase.",
    );
    worker_mode(&fixture, "ask");
    publish(&fixture.crew);
    install(&fixture, "crew");
    let path = fixture.crew.join("agents/worker.md");
    let text = fs::read_to_string(&path).unwrap();
    write(path, &text.replace("agent: claude", "agent: codex"));
    let commit = publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "crew"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("worker") && stdout.contains("  kind: codex\n"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("[widened]") && !stdout.contains("Held crew"),
        "{stdout}"
    );
    assert_eq!(fixture.record()["sources"][0]["commit"], commit);
}

#[test]
fn pinned_skip_hints_select_an_installed_pack_instead_of_adding_defaults() {
    let fixture = Fixture::new();
    install(&fixture, "skills/essential@main");
    let output = fixture.run(&["packs", "update"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rimz packs install skills/essential`"),
        "{stdout}"
    );
}

#[test]
fn non_widening_updates_apply_without_a_terminal_and_preserve_retired_checkouts() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let entry = fixture
        .env
        .agents_home()
        .join("packs")
        .join(before["sources"][0]["entry"].as_str().unwrap());
    let retired = fs::canonicalize(&entry).unwrap();
    worker_mode(&fixture, "ask");
    let commit = publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "crew"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Updated crew") && stdout.contains("mode: ask"),
        "{stdout}"
    );
    assert!(!stdout.contains("[widened]"), "{stdout}");
    assert!(
        !stdout.contains("(named)"),
        "update reasons are not recorded: {stdout}"
    );
    assert_eq!(fixture.record()["sources"][0]["commit"], commit);
    assert_ne!(fs::canonicalize(&entry).unwrap(), retired);
    assert!(retired.is_dir());
    let output = fixture.run(&["packs", "update", "crew"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Already up to date"),
        "{output:?}"
    );
    assert!(!retired.exists());
}

#[test]
fn one_update_retains_every_sources_retired_checkout_until_the_next_command() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let retired: Vec<_> = before["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            fs::canonicalize(
                fixture
                    .env
                    .agents_home()
                    .join("packs")
                    .join(source["entry"].as_str().unwrap()),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(retired.len(), 2);
    worker_mode(&fixture, "ask");
    let crew_commit = publish(&fixture.crew);
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    write(skills.join("skills/review/SKILL.md"), "# Revised review");
    let skills_commit = publish(&skills);
    let output = fixture.run(&["packs", "update"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record["sources"][0]["commit"], crew_commit);
    assert_eq!(record["sources"][1]["commit"], skills_commit);
    for checkout in &retired {
        assert!(
            checkout.is_dir(),
            "every retired checkout must survive the update command: {checkout:?}"
        );
    }
    let output = fixture.run(&["packs", "include", "crew", "--type", "traits"]);
    assert!(output.status.success(), "{output:?}");
    for checkout in &retired {
        assert!(
            !checkout.exists(),
            "the next pack mutation must collect every retired checkout: {checkout:?}"
        );
    }
}

#[test]
fn widening_updates_hold_without_a_terminal_until_yes_and_leave_no_hold_record() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let path = fixture.env.agents_home().join("packs/installed.json");
    let before = fs::read(&path).unwrap();
    worker_mode(&fixture, "yolo");
    let commit = publish(&fixture.crew);
    for _ in 0..2 {
        let output = fixture.run(&["packs", "update", "crew"]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for text in [
            "Held crew",
            "worker",
            "mode",
            "[widened]",
            "rimz packs update crew",
            "--yes",
        ] {
            assert!(stdout.contains(text), "missing {text:?}: {stdout}");
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let output = fixture.run(&["packs", "update", "crew", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.record()["sources"][0]["commit"], commit);
}

#[test]
fn declining_a_widening_update_on_a_terminal_holds_successfully() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    worker_mode(&fixture, "yolo");
    publish(&fixture.crew);
    let (code, output) = answer(
        &fixture,
        &["packs", "update", "crew"],
        "Apply this widening update?",
        b"n\n",
    );
    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("Held crew")
            && output.contains("mode")
            && output.contains("rimz packs update crew"),
        "{output}"
    );
    assert_eq!(fixture.record(), before);
}

#[test]
fn pinned_and_linked_sources_are_skipped_even_with_yes() {
    let fixture = Fixture::new();
    install(&fixture, "crew@main");
    let linked = fixture.env.home_root.join("linked");
    write(
        linked.join("rimz-packs.toml"),
        "[[pack]]\nname = 'local'\ndescription = 'Local'\npath = '.'\n",
    );
    write(linked.join("traits/local.md"), "Local.");
    install(&fixture, linked.to_str().unwrap());
    let before = fixture.record();
    worker_mode(&fixture, "yolo");
    publish(&fixture.crew);
    fs::remove_dir_all(fixture.crew.parent().unwrap().join("rimz-skills")).unwrap();
    let output = fixture.run(&["packs", "update", "--yes"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for text in [
        "crew",
        "pinned at main",
        "rimz packs install crew",
        "essential",
        "local",
        "linked edits show at once",
    ] {
        assert!(stdout.contains(text), "missing {text:?}: {stdout}");
    }
    assert_eq!(fixture.record(), before);
}

#[test]
fn a_failed_source_does_not_stop_later_sources_and_failures_are_reported_last() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    git(
        &fixture.crew,
        &["tag", "-d", &format!("v{}", env!("CARGO_PKG_VERSION"))],
    );
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    write(skills.join("skills/review/SKILL.md"), "# Revised review");
    let commit = publish(&skills);
    let output = fixture.run(&["packs", "update"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Updated essential"), "{stdout}");
    assert!(
        stderr.contains("crew")
            && stderr.contains("v0.")
            && stderr.contains("rimz packs install crew@main"),
        "{stderr}"
    );
    let record = fixture.record();
    assert_eq!(record["sources"][0]["commit"], fixture.commit);
    assert_eq!(record["sources"][1]["commit"], commit);
}

#[test]
fn source_failures_survive_a_simultaneous_host_link_cleanup_failure() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    git(&fixture.crew, &["tag", "-d", &version]);
    write(
        fixture.env.home_root.join(".claude/skills"),
        "not a directory",
    );
    let output = fixture.run(&["packs", "update", "crew"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("missing tag {version}"))
            && stderr.contains("host skill link cleanup is incomplete")
            && !stderr.contains("pack change committed"),
        "{stderr}"
    );
    assert_eq!(fixture.record(), before);
}

#[test]
fn update_preserves_installed_selection_and_exclusions_but_resolves_new_requirements() {
    let fixture = Fixture::new();
    let skills = fixture.crew.parent().unwrap().join("rimz-skills");
    let manifest = fs::read_to_string(skills.join("rimz-packs.toml")).unwrap();
    write(skills.join("support/traits/support.md"), "Support.");
    write(
        skills.join("rimz-packs.toml"),
        &format!(
            "{manifest}[[pack]]\nname = 'support'\ndescription = 'Support'\npath = 'support'\n"
        ),
    );
    let skills_commit = publish(&skills);
    let manifest = fs::read_to_string(fixture.crew.join("rimz-packs.toml")).unwrap();
    write(fixture.crew.join("extra/traits/extra.md"), "Extra.");
    write(
        fixture.crew.join("rimz-packs.toml"),
        &format!("{manifest}[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\n"),
    );
    publish(&fixture.crew);
    install(&fixture, "crew");
    packs::exclude(
        &fixture.env.agents_home(),
        &"extra".parse().unwrap(),
        &ExclusionTarget::Type(ItemType::Traits),
    )
    .unwrap();
    let before = fixture.record();
    let manifest = fs::read_to_string(fixture.crew.join("rimz-packs.toml")).unwrap();
    let manifest = manifest.replace(
        "['skills/essential']",
        "['skills/essential', 'skills/support']",
    );
    write(fixture.crew.join("new/traits/new.md"), "New.");
    write(
        fixture.crew.join("rimz-packs.toml"),
        &format!(
            "{manifest}[[pack]]\nname = 'new-default'\ndescription = 'New'\npath = 'new'\ndefault = true\n"
        ),
    );
    let commit = publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "extra"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record["sources"][0]["commit"], commit);
    assert_eq!(record["sources"][0]["packs"], before["sources"][0]["packs"]);
    assert_eq!(record["sources"][1]["commit"], skills_commit);
    assert_eq!(
        record["sources"][1]["packs"].as_array().unwrap().len(),
        2,
        "{record}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("required by crew"),
        "{output:?}"
    );
}

#[test]
fn a_removed_installed_pack_refuses_the_whole_source_update() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.crew.join("rimz-packs.toml")).unwrap();
    write(fixture.crew.join("extra/traits/extra.md"), "Extra.");
    write(
        fixture.crew.join("rimz-packs.toml"),
        &format!("{manifest}[[pack]]\nname = 'extra'\ndescription = 'Extra'\npath = 'extra'\n"),
    );
    publish(&fixture.crew);
    install(&fixture, "crew");
    let before = fixture.record();
    write(fixture.crew.join("rimz-packs.toml"), &manifest);
    publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "crew", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Git source crew: no pack extra; available packs: crew")
            && stderr.contains("rimz packs remove extra"),
        "{stderr}"
    );
    assert_eq!(fixture.record(), before);
}

#[test]
fn unpinned_third_party_updates_follow_the_default_branch() {
    let fixture = Fixture::new();
    let url = url::Url::from_directory_path(&fixture.crew)
        .unwrap()
        .to_string();
    install(&fixture, &url);
    worker_mode(&fixture, "ask");
    let commit = publish(&fixture.crew);
    let output = fixture.run(&["packs", "update", "crew"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record["sources"][0]["commit"], commit);
    assert_eq!(record["sources"][0]["ref"], "main");
    assert_eq!(record["sources"][0]["pinned"], false);
}

#[test]
fn updates_fetch_the_recorded_transport_not_a_normalized_neighbor() {
    let fixture = Fixture::new();
    let repo = fixture.env.home_root.join("transport.git");
    let neighbor = fixture.env.home_root.join("transport");
    for target in [&repo, &neighbor] {
        git(
            &fixture.env.project_root,
            &[
                "clone",
                "--",
                fixture.crew.to_str().unwrap(),
                target.to_str().unwrap(),
            ],
        );
    }
    git(&repo, &["config", "user.name", "Pack Test"]);
    git(&repo, &["config", "user.email", "packs@example.test"]);
    let url = url::Url::from_directory_path(&repo).unwrap().to_string();
    install(&fixture, &url);
    let path = repo.join("agents/worker.md");
    let text = fs::read_to_string(&path).unwrap();
    write(path, &text.replace("mode: auto", "mode: ask"));
    let commit = publish(&repo);
    let output = fixture.run(&["packs", "update", "crew"]);
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(
        record["sources"][0]["commit"], commit,
        "updates must fetch the recorded URL"
    );
    assert_eq!(record["sources"][0]["url"], url);
}

#[test]
fn unknown_update_packs_fail_and_list_installed_names() {
    let fixture = Fixture::new();
    install(&fixture, "crew");
    let before = fixture.record();
    let output = fixture.run(&["packs", "update", "absent"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown pack absent; installed packs: crew, essential"),
        "{stderr}"
    );
    assert_eq!(fixture.record(), before);
}
