use super::*;

fn home_copy(env: &Env, source: &Path, relative: &str) -> PathBuf {
    let target = env.agents_home().join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source.join("crew").join(relative), &target).unwrap();
    target
}

#[test]
fn linked_install_hints_at_identical_overrides_without_removing_them() {
    let env = Env::new();
    let source = source(&env);
    let helper = home_copy(&env, &source, "subagents/helper.md");
    let skill = home_copy(&env, &source, "skills/coder/SKILL.md");
    write(
        env.agents_home().join("agents/coder.md"),
        "---\ndescription: Edited\nagent: pi\n---\nMine.",
    );
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success()
        .stdout(contains("2 identical overrides"))
        .stdout(contains("rimz packs reset crew --identical"));
    assert!(helper.exists());
    assert!(skill.exists());
}

#[test]
fn linked_home_trees_do_not_produce_an_identical_cleanup_hint() {
    let env = Env::new();
    let source = source(&env);
    for kind in ["agents", "skills"] {
        symlink(source.join("crew").join(kind), env.agents_home().join(kind)).unwrap();
    }
    let output = env
        .rimz()
        .args(["packs", "install"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let receipt = String::from_utf8(output.stdout).unwrap();
    assert!(!receipt.contains("identical override"), "{receipt}");
}

#[test]
fn identical_cleanup_keeps_shipped_items_reached_through_linked_home_trees() {
    let env = Env::new();
    let source = installed(&env);
    for kind in ["agents", "skills"] {
        symlink(source.join("crew").join(kind), env.agents_home().join(kind)).unwrap();
    }
    let paths = ["agents/pi.md", "agents/coder.md", "skills/coder/SKILL.md"];
    let originals: Vec<_> = paths
        .iter()
        .map(|path| fs::read(source.join("crew").join(path)).unwrap())
        .collect();
    let output = env
        .rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let receipt = String::from_utf8(output.stdout).unwrap();
    assert!(!receipt.contains("Removed"), "{receipt}");
    for (kind, name) in [("agents", "pi"), ("agents", "coder"), ("skills", "coder")] {
        assert!(
            receipt.contains(&format!("Kept crew:{name} ({kind}):")),
            "{receipt}"
        );
    }
    assert!(
        receipt.contains("home entry resolves to the shipped item"),
        "{receipt}"
    );
    for (path, original) in paths.iter().zip(originals) {
        assert_eq!(fs::read(source.join("crew").join(path)).unwrap(), original);
    }
}

#[test]
fn identical_cleanup_counts_and_unlinks_per_item_links_to_the_pack_only() {
    let env = Env::new();
    let source = source(&env);
    for (kind, entry) in [("agents", "coder.md"), ("skills", "coder")] {
        fs::create_dir_all(env.agents_home().join(kind)).unwrap();
        symlink(
            source.join("crew").join(kind).join(entry),
            env.agents_home().join(kind).join(entry),
        )
        .unwrap();
    }
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success()
        .stdout(contains("2 identical overrides"));
    env.rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .assert()
        .success();
    for relative in ["agents/coder.md", "skills/coder"] {
        assert!(!env.agents_home().join(relative).is_symlink());
        assert!(source.join("crew").join(relative).exists());
    }
}

#[test]
fn identical_cleanup_reports_unreadable_overrides_and_finishes_other_removals_and_prune() {
    let env = Env::new();
    let source = installed(&env);
    let base = home_copy(&env, &source, "agents/pi.md");
    let craft = home_copy(&env, &source, "traits/craft.md");
    let helper = env.agents_home().join("subagents/helper.md");
    fs::create_dir_all(helper.parent().unwrap()).unwrap();
    symlink("missing-helper", &helper).unwrap();
    let root = env.home_root.join(".claude/skills");
    fs::create_dir_all(&root).unwrap();
    let stale = root.join("stale");
    symlink(env.agents_home().join("packs/removed/skills/stale"), &stale).unwrap();
    env.rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .assert()
        .success()
        .stdout(contains(format!("Removed {}.", base.display())))
        .stdout(contains(format!("Removed {}.", craft.display())))
        .stdout(contains("Kept crew:helper (subagents):"))
        .stdout(contains("No such file or directory"))
        .stdout(contains("Skill links removed from "));
    assert!(!base.exists());
    assert!(!craft.exists());
    assert!(helper.is_symlink());
    assert!(!stale.is_symlink());
}

#[test]
fn identical_cleanup_removes_a_shared_home_entry_once_for_duplicate_pack_names() {
    let env = Env::new();
    let source = installed(&env);
    let text = "---\nname: duplicate\ndescription: Duplicate\nagent: pi\n---\nSame.\n";
    write(source.join("crew/agents/first.md"), text);
    write(source.join("crew/agents/second.md"), text);
    let home = env.agents_home().join("agents/duplicate.md");
    write(&home, text);
    let output = env
        .rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let receipt = String::from_utf8(output.stdout).unwrap();
    assert_eq!(receipt.matches("Removed ").count(), 1, "{receipt}");
    assert!(!home.exists());
    assert!(source.join("crew/agents/first.md").exists());
    assert!(source.join("crew/agents/second.md").exists());
}

#[test]
fn identical_cleanup_reports_removal_failures_after_receipts_and_prune() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let source = installed(&env);
    let base = home_copy(&env, &source, "agents/pi.md");
    let craft = home_copy(&env, &source, "traits/craft.md");
    let directory = craft.parent().unwrap();
    let permissions = fs::metadata(directory).unwrap().permissions();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(directory.join("write-probe"), "").is_ok() {
        fs::set_permissions(directory, permissions).unwrap();
        crate::common::skip("this user bypasses directory write permissions");
        return;
    }
    let root = env.home_root.join(".claude/skills");
    fs::create_dir_all(&root).unwrap();
    let stale = root.join("stale");
    symlink(env.agents_home().join("packs/removed/skills/stale"), &stale).unwrap();
    let output = env
        .rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .output();
    fs::set_permissions(directory, permissions).unwrap();
    output
        .unwrap()
        .assert()
        .code(1)
        .stdout(contains(format!("Removed {}.", base.display())))
        .stdout(contains("Skill links removed from "))
        .stderr(contains("Pack reset incomplete"))
        .stderr(contains(craft.to_str().unwrap()));
    assert!(!base.exists());
    assert!(craft.exists());
    assert!(!stale.is_symlink());
}

#[test]
fn identical_cleanup_removes_only_matching_overrides_and_is_repeatable() {
    let env = Env::new();
    let source = installed(&env);
    let record = fs::read(env.agents_home().join("packs/installed.json")).unwrap();
    let helper = home_copy(&env, &source, "subagents/helper.md");
    write(
        source.join("crew/skills/coder/extra.bin"),
        "ancillary\0bytes",
    );
    let skill = home_copy(&env, &source, "skills/coder/SKILL.md");
    home_copy(&env, &source, "skills/coder/extra.bin");
    let coder = env.agents_home().join("agents/coder.md");
    write(&coder, "---\ndescription: Edited\nagent: pi\n---\nMine.");
    env.rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .assert()
        .success()
        .stdout(contains(helper.to_str().unwrap()))
        .stdout(contains("loads"))
        .stdout(contains("Kept crew:coder"))
        .stdout(contains("rimz packs diff crew:coder --type agents"));
    assert!(!helper.exists());
    assert!(!skill.parent().unwrap().exists());
    assert!(coder.exists());
    let again = env
        .rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .output()
        .unwrap();
    assert!(again.status.success(), "{again:?}");
    let receipt = String::from_utf8(again.stdout).unwrap();
    assert!(!receipt.contains("Removed"), "{receipt}");
    assert!(receipt.contains("Kept crew:coder"), "{receipt}");
    assert_eq!(
        fs::read(env.agents_home().join("packs/installed.json")).unwrap(),
        record
    );
}

#[test]
fn identical_item_and_type_selectors_keep_other_overrides_and_bare_reset_requires_the_flag() {
    let env = Env::new();
    let source = installed(&env);
    let helper = home_copy(&env, &source, "subagents/helper.md");
    let coder = home_copy(&env, &source, "agents/coder.md");
    let skill = home_copy(&env, &source, "skills/coder/SKILL.md");
    env.rimz()
        .args(["packs", "reset", "crew"])
        .assert()
        .code(2)
        .stderr(contains("--identical"));
    env.rimz()
        .args(["packs", "reset", "crew:helper", "--identical"])
        .assert()
        .success();
    assert!(!helper.exists());
    assert!(coder.exists());
    assert!(skill.exists());
    home_copy(&env, &source, "subagents/helper.md");
    env.rimz()
        .args(["packs", "reset", "crew", "--type", "agents", "--identical"])
        .assert()
        .success();
    assert!(!coder.exists());
    assert!(helper.exists());
    assert!(skill.exists());
    write(&helper, "---\ndescription: Edited\nagent: pi\n---\nMine.");
    env.rimz()
        .args(["packs", "reset", "crew:helper", "--identical"])
        .assert()
        .success()
        .stdout(contains("Kept crew:helper"));
    assert!(helper.exists());
}

#[test]
fn identical_skills_compare_relative_regular_file_sets_without_following_inner_symlinks() {
    for difference in ["extra", "missing", "bytes", "symlink"] {
        let env = Env::new();
        let source = installed(&env);
        write(source.join("crew/skills/coder/rules.md"), "Original\n");
        let skill = home_copy(&env, &source, "skills/coder/SKILL.md");
        let rules = home_copy(&env, &source, "skills/coder/rules.md");
        match difference {
            "extra" => write(skill.parent().unwrap().join("extra"), "Extra"),
            "missing" => fs::remove_file(&rules).unwrap(),
            "bytes" => write(&rules, "Changed\n"),
            "symlink" => {
                fs::remove_file(&rules).unwrap();
                symlink(source.join("crew/skills/coder/rules.md"), &rules).unwrap();
            }
            _ => unreachable!(),
        }
        env.rimz()
            .args([
                "packs",
                "reset",
                "crew:coder",
                "--type",
                "skills",
                "--identical",
            ])
            .assert()
            .success()
            .stdout(contains("Kept crew:coder"))
            .stdout(contains("rimz packs diff crew:coder --type skills"));
        assert!(skill.exists(), "{difference}");
    }
}

#[test]
fn overrides_work_when_the_agents_home_itself_is_a_symlink() {
    let env = Env::new();
    installed(&env);
    let actual = env.home_root.join("actual-rimz");
    fs::rename(env.agents_home(), &actual).unwrap();
    symlink(&actual, env.agents_home()).unwrap();
    env.rimz()
        .args([
            "packs",
            "customize",
            "crew:coder",
            "--type",
            "skills",
            "--copy",
        ])
        .assert()
        .success();
    assert!(actual.join("skills/coder/SKILL.md").exists());
    env.rimz()
        .args(["packs", "reset", "crew", "--identical"])
        .assert()
        .success();
    assert!(!actual.join("skills/coder").exists());
    assert!(env.agents_home().is_symlink());
}

#[test]
fn identical_cleanup_reloads_shared_name_claims_after_removing_the_home_file() {
    for home_tree in ["agents", "subagents"] {
        let env = Env::new();
        let source = installed(&env);
        let text = fs::read_to_string(source.join("crew/agents/coder.md")).unwrap();
        write(source.join("crew/subagents/coder.md"), &text);
        let path = home_copy(&env, &source, &format!("{home_tree}/coder.md"));
        env.rimz()
            .args(["packs", "reset", "crew", "--identical"])
            .assert()
            .success();
        assert!(!path.exists());
        let show = json(&env, &["packs", "show", "crew", "--json"]);
        for kind in ["agents", "subagents"] {
            assert_eq!(item(&show, kind, "coder")["fate"]["state"], "conflict");
        }
    }
}
