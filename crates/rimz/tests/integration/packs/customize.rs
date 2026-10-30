use super::*;

#[test]
fn customize_prints_the_receipt_and_prune_output_before_opening_the_editor() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;

    let env = Env::new();
    installed(&env);
    let root = env.home_root.join(".claude/skills");
    fs::create_dir_all(&root).unwrap();
    symlink(
        env.agents_home().join("packs/removed/skills/stale"),
        root.join("stale"),
    )
    .unwrap();
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.args(["packs", "customize", "crew:coder", "--type", "agents"]);
    command.env("VISUAL", "sh -c 'echo editor-opened; exit 7' editor");
    command.env("NO_COLOR", "1");
    command.cwd(&env.project_root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..read]);
        }
        let _ = tx.send(output);
    });
    let bytes = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|error| {
            let _ = child.kill();
            panic!("customize did not close its terminal: {error}");
        });
    let output = String::from_utf8_lossy(&bytes);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}: {output}");
    let created = output.find("Created ").expect(&output);
    let pruned = output.find("Skill links removed from ").expect(&output);
    let editor = output.find("editor-opened").expect(&output);
    assert!(created < pruned && pruned < editor, "{output}");
    let path = env.agents_home().join("agents/coder.md");
    assert!(
        output.contains(&format!(
            "warning: editor exited with exit status: 7; override kept at {}",
            path.display()
        )),
        "{output}"
    );
    assert!(path.is_file());
    assert!(!root.join("stale").is_symlink());
}

#[test]
fn customize_inherits_agents_and_subagents_and_does_not_open_a_nonterminal_editor() {
    for (tree, name, listing) in [
        ("agents", "coder", "agents"),
        ("subagents", "helper", "subagents"),
    ] {
        let env = Env::new();
        let source = installed(&env);
        let shipped = source.join(format!("crew/{tree}/{name}.md"));
        write(
            &shipped,
            "---\ndescription: 'A coder: with # punctuation'\nagent: pi\nmodel: custom\neffort: high\ntools: [Read]\n---\nShipped craft.",
        );
        let path = env.agents_home().join(format!("{tree}/{name}.md"));
        let marker = env.home_root.join("editor-ran");
        env.rimz()
            .args([
                "packs",
                "customize",
                &format!("crew:{name}"),
                "--type",
                tree,
            ])
            .env("VISUAL", format!("touch {}", marker.display()))
            .env("EDITOR", format!("touch {}", marker.display()))
            .assert()
            .success()
            .stdout(contains(path.to_str().unwrap()))
            .stdout(contains(format!("inherits crew:{name}")))
            .stdout(contains(format!("rimz packs diff crew:{name}")))
            .stdout(contains("rimz agents validate"));
        assert!(!marker.exists());
        let text = fs::read_to_string(&path).unwrap();
        let (yaml, _) = text
            .strip_prefix("---\n")
            .unwrap()
            .split_once("\n---")
            .unwrap();
        let frontmatter: Value = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(frontmatter["agent"], format!("crew:{name}"));
        assert!(!text.contains("Shipped craft."));
        let profiles = json(&env, &[listing, "profiles", "--json"]);
        let row = profiles
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["description"], "A coder: with # punctuation");
        assert_eq!(row["model"], "custom");
        assert_eq!(row["effort"], "high");
        env.rimz()
            .args(["packs", "show", "crew"])
            .assert()
            .success()
            .stdout(contains(format!("overridden by {}", path.display())));
        env.rimz()
            .args(["packs", "diff", &format!("crew:{name}"), "--type", tree])
            .assert()
            .success()
            .stdout(contains(format!("inherits crew:{name}")))
            .stdout(contains(text));
        env.rimz()
            .args(["packs", "reset", &format!("crew:{name}"), "--type", tree])
            .assert()
            .success()
            .stdout(contains(path.to_str().unwrap()))
            .stdout(contains("loads"));
        assert!(!path.exists());
    }
}

#[test]
fn customize_uses_the_shipped_filename_and_preserves_an_explicit_name() {
    let env = Env::new();
    let source = installed(&env);
    write(
        source.join("crew/agents/filename.md"),
        "---\nname: named\ndescription: Named\nagent: pi\n---\nCraft.",
    );
    env.rimz()
        .args(["packs", "customize", "crew:named"])
        .assert()
        .success();
    let path = env.agents_home().join("agents/filename.md");
    let text = fs::read_to_string(path).unwrap();
    let (yaml, _) = text
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---")
        .unwrap();
    let frontmatter: Value = serde_saphyr::from_str(yaml).unwrap();
    assert_eq!(frontmatter["name"], "named");
    assert!(
        json(&env, &["agents", "profiles", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "named")
    );
}

#[test]
fn customize_copy_preserves_definition_bytes_and_skill_ancillary_files() {
    let env = Env::new();
    let source = installed(&env);
    write(
        source.join("crew/skills/coder/scripts/run.sh"),
        "#!/bin/sh\necho craft\n",
    );
    for (target, kind, relative) in [
        ("crew:coder", "agents", "agents/coder.md"),
        ("crew:coder", "skills", "skills/coder/SKILL.md"),
    ] {
        env.rimz()
            .args(["packs", "customize", target, "--type", kind, "--copy"])
            .assert()
            .success()
            .stdout(contains("copy, no longer follows crew"))
            .stdout(contains(format!(
                "Edit {}",
                env.agents_home().join(relative).display()
            )));
        assert_eq!(
            fs::read(env.agents_home().join(relative)).unwrap(),
            fs::read(source.join("crew").join(relative)).unwrap()
        );
    }
    assert_eq!(
        fs::read(env.agents_home().join("skills/coder/scripts/run.sh")).unwrap(),
        b"#!/bin/sh\necho craft\n"
    );
    let show = json(&env, &["packs", "show", "crew", "--json"]);
    assert_eq!(
        item(&show, "skills", "coder")["fate"]["path"],
        env.agents_home()
            .join("skills/coder/SKILL.md")
            .to_str()
            .unwrap()
    );
}

#[test]
fn customize_refuses_existing_overrides_and_unclaimed_destinations_without_replacing_bytes() {
    for claims_name in [true, false] {
        let env = Env::new();
        installed(&env);
        let path = env.agents_home().join("agents/coder.md");
        let original = if claims_name {
            "---\ndescription: Home\nagent: pi\n---\nKeep."
        } else {
            "---\nname: another\ndescription: Home\nagent: pi\n---\nKeep."
        };
        write(&path, original);
        let output = env
            .rimz()
            .args(["packs", "customize", "crew:coder", "--type", "agents"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains(path.to_str().unwrap()), "{error}");
        if claims_name {
            assert!(
                error.contains("rimz packs diff crew:coder --type agents"),
                "{error}"
            );
            assert!(
                error.contains("rimz packs reset crew:coder --type agents"),
                "{error}"
            );
        }
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn copy_only_types_name_the_copy_command_and_failed_parents_refuse_inheritance() {
    for (name, kind) in [
        ("pair", "teams"),
        ("craft", "traits"),
        ("pi", "agents"),
        ("coder", "skills"),
    ] {
        let env = Env::new();
        installed(&env);
        env.rimz()
            .args([
                "packs",
                "customize",
                &format!("crew:{name}"),
                "--type",
                kind,
            ])
            .assert()
            .code(2)
            .stderr(contains(format!(
                "rimz packs customize crew:{name} --type {kind} --copy"
            )));
    }
    let env = Env::new();
    let source = installed(&env);
    write(
        source.join("crew/agents/broken.md"),
        "---\ndescription: Broken\nagent: missing\n---\n",
    );
    env.rimz()
        .args(["packs", "customize", "crew:broken"])
        .assert()
        .code(1)
        .stderr(contains(
            "rimz packs customize crew:broken --type agents --copy",
        ));
    env.rimz()
        .args(["packs", "customize", "crew:broken", "--copy"])
        .assert()
        .success();
}

#[test]
fn customize_can_resolve_a_pack_conflict_with_a_qualified_parent() {
    let env = Env::new();
    let source = installed(&env);
    write(
        source.join("tools/agents/coder.md"),
        "---\ndescription: Rival\nagent: pi\n---\nRival.",
    );
    env.rimz()
        .args(["packs", "customize", "crew:coder", "--type", "agents"])
        .assert()
        .success();
    env.rimz().args(["agents", "validate"]).assert().success();
}

#[test]
fn excluded_items_refuse_every_override_verb_and_name_the_working_include_command() {
    for whole_type in [false, true] {
        let env = Env::new();
        installed(&env);
        let mut exclude = env.rimz();
        exclude
            .args([
                "packs",
                "exclude",
                if whole_type { "crew" } else { "crew:coder" },
                "--type",
                "agents",
            ])
            .assert()
            .success();
        for verb in ["customize", "diff", "reset"] {
            env.rimz()
                .args(["packs", verb, "crew:coder", "--type", "agents"])
                .assert()
                .code(1)
                .stderr(contains(if whole_type {
                    "rimz packs include crew --type agents"
                } else {
                    "rimz packs include crew:coder --type agents"
                }));
        }
    }
}

#[test]
fn override_verbs_share_selector_errors() {
    let env = Env::new();
    installed(&env);
    for verb in ["customize", "diff", "reset"] {
        for target in ["bad!:coder", "crew:", "crew:coder"] {
            env.rimz().args(["packs", verb, target]).assert().code(2);
        }
        env.rimz()
            .args(["packs", verb, "absent:coder"])
            .assert()
            .code(1);
        env.rimz()
            .args(["packs", verb, "crew:absent"])
            .assert()
            .code(1);
    }
}

#[test]
fn skill_copy_refuses_symlinks_without_publishing_a_partial_override() {
    let env = Env::new();
    let source = installed(&env);
    let link = source.join("crew/skills/coder/linked");
    symlink("SKILL.md", &link).unwrap();
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
        .code(1)
        .stderr(contains("skills/coder/linked"));
    assert!(!env.agents_home().join("skills/coder").exists());
}

#[test]
fn diff_handles_no_override_copies_and_skill_additions_and_deletions() {
    let env = Env::new();
    let source = installed(&env);
    let show = json(&env, &["packs", "show", "crew", "--json"]);
    let shipped_path = item(&show, "subagents", "helper")["path"].as_str().unwrap();
    env.rimz()
        .args(["packs", "diff", "crew:helper"])
        .assert()
        .success()
        .stdout(contains("no override"))
        .stdout(contains(shipped_path));
    env.rimz()
        .args(["packs", "reset", "crew:helper"])
        .assert()
        .success()
        .stdout(contains("already"));
    write(
        env.agents_home().join("subagents/helper.md"),
        "---\ndescription: Mine\nagent: pi\n---\nMine.",
    );
    env.rimz()
        .args(["packs", "diff", "crew:helper"])
        .assert()
        .success()
        .stdout(contains(format!("--- {shipped_path}\n")))
        .stdout(contains(format!(
            "+++ {}\n",
            env.agents_home().join("subagents/helper.md").display()
        )))
        .stdout(contains("-Help."))
        .stdout(contains("+Mine."));
    write(source.join("crew/skills/coder/removed.txt"), "Removed\n");
    write(env.agents_home().join("skills/coder/SKILL.md"), "# Home\n");
    write(env.agents_home().join("skills/coder/added.txt"), "Added\n");
    env.rimz()
        .args(["packs", "diff", "crew:coder", "--type", "skills"])
        .assert()
        .success()
        .stdout(contains("-# Coder"))
        .stdout(contains("+# Home"))
        .stdout(contains("removed.txt"))
        .stdout(contains("-Removed"))
        .stdout(contains("added.txt"))
        .stdout(contains("+Added"));
}

#[test]
fn diff_distinguishes_a_deleted_empty_skill_file_from_an_unchanged_file() {
    let env = Env::new();
    let source = installed(&env);
    write(source.join("crew/skills/coder/py.typed"), "");
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
    fs::remove_file(env.agents_home().join("skills/coder/py.typed")).unwrap();
    env.rimz()
        .args(["packs", "diff", "crew:coder", "--type", "skills"])
        .assert()
        .success()
        .stdout(contains(
            "py.typed\n    +++ /dev/null\n    @@ deleted file @@",
        ));
}

#[test]
fn diff_of_a_copy_prints_only_differing_files_or_one_matching_line() {
    let env = Env::new();
    let source = installed(&env);
    write(source.join("crew/skills/coder/notes.md"), "Notes\n");
    let skill = env.agents_home().join("skills/coder");
    let helper = env.agents_home().join("subagents/helper.md");
    for (name, tree, path) in [
        ("coder", "skills", &skill),
        ("helper", "subagents", &helper),
    ] {
        let item = format!("crew:{name}");
        env.rimz()
            .args(["packs", "customize", &item, "--type", tree, "--copy"])
            .assert()
            .success();
        env.rimz()
            .args(["packs", "diff", &item, "--type", tree])
            .assert()
            .success()
            .stdout(format!(
                "{item} override at {} matches the shipped item.\n",
                path.display()
            ));
    }
    write(skill.join("notes.md"), "Edited\n");
    let assert = env
        .rimz()
        .args(["packs", "diff", "crew:coder", "--type", "skills"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(stdout.contains("    -Notes\n    +Edited\n"), "{stdout}");
    assert!(!stdout.contains("SKILL.md"), "{stdout}");
    assert!(!stdout.contains("matches the shipped item"), "{stdout}");
}

#[test]
fn diff_renders_home_skill_symlinks_without_following_their_targets() {
    let env = Env::new();
    installed(&env);
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
    symlink(
        "missing-target",
        env.agents_home().join("skills/coder/link"),
    )
    .unwrap();
    env.rimz()
        .args(["packs", "diff", "crew:coder", "--type", "skills"])
        .assert()
        .success()
        .stdout(contains("+++ "))
        .stdout(contains("+symlink to missing-target"));
}

#[test]
fn reset_refuses_a_home_tree_linked_to_the_shipped_items() {
    for (kind, relative) in [("agents", "coder.md"), ("skills", "coder/SKILL.md")] {
        let env = Env::new();
        let source = installed(&env);
        symlink(source.join("crew").join(kind), env.agents_home().join(kind)).unwrap();
        let before = json(&env, &["packs", "show", "crew", "--json"]);
        let shipped = item(&before, kind, "coder")["path"].as_str().unwrap();
        let home = env.agents_home().join(kind).join(relative);
        let original = fs::read(&home).unwrap();
        env.rimz()
            .args(["packs", "reset", "crew:coder", "--type", kind])
            .assert()
            .code(1)
            .stderr(contains("home entry resolves to the shipped item"))
            .stderr(contains("nothing was removed"))
            .stderr(contains(home.parent().unwrap().to_str().unwrap()))
            .stderr(contains(shipped));
        assert_eq!(fs::read(&home).unwrap(), original);
        assert!(source.join("crew").join(kind).join(relative).exists());
    }
}

#[test]
fn reset_refuses_a_configured_command_even_inside_the_teams_home_tree() {
    let env = Env::new();
    installed(&env);
    let teams = env.agents_home().join("teams");
    let config = teams.join("config.toml");
    let original = "[agents.commands]\npair = 'true'\n";
    write(&config, original);
    env.rimz()
        .env("RIMZ_HOME", &teams)
        .env("RIMZ_AGENTS_HOME", env.agents_home())
        .args(["packs", "reset", "crew:pair"])
        .assert()
        .code(1)
        .stderr(contains("configured command"))
        .stderr(contains(config.to_str().unwrap()));
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
}

#[test]
fn reset_refuses_cross_tree_and_configured_command_overrides_and_diff_names_the_command() {
    let env = Env::new();
    installed(&env);
    let cross_tree = env.agents_home().join("subagents/coder.md");
    write(&cross_tree, "---\ndescription: Home\nagent: pi\n---\nKeep.");
    env.rimz()
        .args(["packs", "reset", "crew:coder", "--type", "agents"])
        .assert()
        .code(1)
        .stderr(contains(cross_tree.to_str().unwrap()));
    assert!(cross_tree.exists());
    env.rimz()
        .args(["packs", "customize", "crew:coder", "--type", "agents"])
        .assert()
        .code(1)
        .stderr(contains("only overrides in this item's own home tree"));
    let config = env.rimz_home().join("config.toml");
    write(&config, "[agents.commands]\npair = 'true'\n");
    env.rimz()
        .args(["packs", "customize", "crew:pair", "--copy"])
        .assert()
        .code(1)
        .stderr(contains("only overrides in this item's own home tree"));
    env.rimz()
        .args(["packs", "reset", "crew:pair"])
        .assert()
        .code(1)
        .stderr(contains(config.to_str().unwrap()));
    env.rimz()
        .args(["packs", "diff", "crew:pair"])
        .assert()
        .success()
        .stdout(contains(config.to_str().unwrap()))
        .stdout(contains("command pair"));
}

#[test]
fn reset_unlinks_home_symlinks_without_touching_definition_or_skill_targets() {
    for skill in [false, true] {
        let env = Env::new();
        installed(&env);
        let target = env.home_root.join("keep");
        let (path, kind) = if skill {
            write(target.join("SKILL.md"), "# Keep");
            (env.agents_home().join("skills/coder"), "skills")
        } else {
            write(&target, "---\ndescription: Keep\nagent: pi\n---\nKeep.");
            (env.agents_home().join("agents/coder.md"), "agents")
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&target, &path).unwrap();
        env.rimz()
            .args(["packs", "reset", "crew:coder", "--type", kind])
            .assert()
            .success();
        assert!(!path.is_symlink());
        assert!(target.exists());
    }
}

#[test]
fn skill_overrides_reset_with_a_relative_agents_home() {
    let env = Env::new();
    let source = source(&env);
    env.rimz()
        .env("RIMZ_AGENTS_HOME", "library")
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success();
    env.rimz()
        .env("RIMZ_AGENTS_HOME", "library")
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
    let path = env.project_root.join("library/skills/coder");
    assert!(path.join("SKILL.md").exists());
    env.rimz()
        .env("RIMZ_AGENTS_HOME", "library")
        .args(["packs", "reset", "crew:coder", "--type", "skills"])
        .assert()
        .success();
    assert!(!path.exists());
}
