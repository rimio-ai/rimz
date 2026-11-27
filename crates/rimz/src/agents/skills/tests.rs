use super::*;

#[test]
fn relative_homes_keep_absolute_owned_link_roots() {
    let temp = tempfile::tempdir().unwrap();
    let relative = std::env::current_dir()
        .unwrap()
        .ancestors()
        .skip(1)
        .map(|_| "..")
        .collect::<PathBuf>()
        .join(temp.path().strip_prefix("/").unwrap());
    let provider = temp.path().join("provider");
    std::fs::create_dir(&provider).unwrap();
    for target in [
        temp.path().join("skills/one"),
        temp.path().join("packs/one"),
    ] {
        let link = provider.join("owned");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for chain in [
            SkillChain::from_home(&relative),
            SkillChain::from_env(&BTreeMap::from([(
                "RIMZ_AGENTS_HOME".into(),
                relative.to_str().unwrap().into(),
            )]))
            .unwrap(),
        ] {
            assert!(chain.roots().iter().all(|root| root.is_absolute()));
            assert_eq!(chain.owned_link(&link).unwrap(), Some(target.clone()));
        }
        std::fs::remove_file(link).unwrap();
    }
}

#[test]
fn discovery_prefers_provider_skips_broken_links_and_records_sources() {
    let temp = tempfile::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let library = temp.path().join("rimz/skills");
    let foreign = temp.path().join("foreign");
    for path in [
        provider.join("same"),
        library.join("same"),
        library.join("extra"),
        foreign.clone(),
    ] {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "skill").unwrap();
    }
    std::os::unix::fs::symlink(provider.join("missing"), provider.join("broken")).unwrap();
    std::os::unix::fs::symlink(&foreign, provider.join("alias")).unwrap();
    std::os::unix::fs::symlink(library.join("extra"), provider.join("owned")).unwrap();
    std::fs::create_dir(provider.join("not-a-skill")).unwrap();
    let chain = SkillChain::from_home(library.parent().unwrap());
    let skills = enumerate(Some(&provider), None, Some(&chain), |_| true)
        .unwrap()
        .skills;
    assert_eq!(skills.len(), 3);
    assert_eq!(
        skills["same"].source,
        provider.join("same").canonicalize().unwrap()
    );
    assert_eq!(skills["alias"].source, foreign.canonicalize().unwrap());
}

#[test]
fn discovery_does_not_inspect_shadowed_rimz_skills() {
    let temp = tempfile::tempdir().unwrap();
    let provider = temp.path().join("provider");
    let chain = SkillChain::from_home(temp.path());
    std::fs::create_dir_all(provider.join("same")).unwrap();
    std::fs::write(provider.join("same/SKILL.md"), "provider").unwrap();
    std::fs::create_dir_all(chain.library.join("same")).unwrap();
    std::os::unix::fs::symlink("SKILL.md", chain.library.join("same/SKILL.md")).unwrap();
    let result = enumerate(Some(&provider), None, Some(&chain), |_| true);
    assert!(
        result.is_ok(),
        "the provider's copy wins without reading the lower marker: {result:?}"
    );
}

#[test]
fn discovery_loads_pack_skills_and_withholds_conflicts_and_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("rimz");
    let library = SkillChain::from_home(&home).library;
    for pack in ["one", "two"] {
        let source = temp.path().join(pack);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("rimz-packs.toml"),
            format!("[[pack]]\nname = '{pack}'\ndescription = 'Skills'\npath = '.'\n"),
        )
        .unwrap();
        for name in [pack, "conflict", "hidden", "excluded"] {
            let path = source.join(ItemType::Skills.as_str()).join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "skill").unwrap();
        }
        crate::packs::install(&home, &source).unwrap();
        crate::packs::exclude(
            &home,
            &pack.parse().unwrap(),
            &crate::packs::ExclusionTarget::Item(crate::packs::ItemKey {
                kind: crate::packs::ItemType::Skills,
                name: "excluded".to_owned(),
            }),
        )
        .unwrap();
    }
    std::fs::create_dir_all(library.join("hidden")).unwrap();
    std::fs::write(library.join("hidden/SKILL.md"), "home").unwrap();
    let chain = SkillChain::from_home(&home);
    let skills = enumerate(None, None, Some(&chain), |_| true)
        .unwrap()
        .skills;
    assert_eq!(
        skills.keys().map(String::as_str).collect::<Vec<_>>(),
        ["hidden", "one", "two"]
    );
    let marker = library.join("hidden/SKILL.md");
    std::fs::remove_file(&marker).unwrap();
    std::os::unix::fs::symlink("SKILL.md", &marker).unwrap();
    let result = enumerate(None, None, Some(&chain), |_| true);
    assert!(
        matches!(&result, Err(SkillErr::Io { path, .. }) if path == &marker),
        "an unreadable library winner must fail, not reveal pack copies: {result:?}"
    );
}

#[test]
fn available_skills_lists_all_skills_while_rimz_skills_keeps_only_rimz_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let provider = temp.path().join(".agents/skills");
    let library = temp.path().join("rimz/skills");
    for path in [
        provider.join("commit"),
        library.join("rimz-reflect"),
        library.join("_shared"),
    ] {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "skill").unwrap();
    }
    let chain = SkillChain::from_home(library.parent().unwrap());
    let env = BTreeMap::from([("HOME".to_owned(), temp.path().display().to_string())]);
    assert_eq!(
        available_skills("codex", &env, Some(&chain), None, false),
        ["_shared", "commit", "rimz-reflect"]
            .map(str::to_owned)
            .into()
    );
    assert_eq!(
        rimz_skills(Some(&provider), Some(&chain)),
        ["rimz-reflect".to_owned()].into()
    );
}

#[test]
fn is_model_callable_on_disk_drops_same_directory_alias_and_shared_provider_key() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let claude_skills = home.join(".claude/skills");
    let codex_skills = home.join(".agents/skills");
    let rimz_home = home.join(".rimz");
    std::fs::create_dir_all(&claude_skills).unwrap();
    std::fs::create_dir_all(&codex_skills).unwrap();
    std::fs::create_dir_all(&rimz_home).unwrap();

    let env = BTreeMap::from([
        ("HOME".to_owned(), home.display().to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            home.join(".claude").display().to_string(),
        ),
        (
            "RIMZ_AGENTS_HOME".to_owned(),
            rimz_home.display().to_string(),
        ),
    ]);
    let msg_name: SkillName = "rimz-message".parse().unwrap();

    // Branch 1: two names pointing at the same canonical directory (Claude keys by
    // directory name, so `other-alias` and `rimz-message` have distinct provider keys
    // and only the canonical-source check catches the collision).
    let claude_msg = claude_skills.join("rimz-message");
    std::fs::create_dir_all(&claude_msg).unwrap();
    std::fs::write(
        claude_msg.join("SKILL.md"),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&claude_msg, claude_skills.join("other-alias")).unwrap();
    let claude = crate::agents::find_definition("claude").unwrap();
    let alias_name: SkillName = "other-alias".parse().unwrap();
    assert!(
        !is_model_callable_on_disk(
            "rimz-message",
            claude,
            &env,
            std::slice::from_ref(&msg_name),
        ),
        "unlisted alias to the same directory must drop seat-added rimz-message"
    );
    assert!(
        is_model_callable_on_disk(
            "rimz-message",
            claude,
            &env,
            &[msg_name.clone(), alias_name],
        ),
        "listing both aliases removes the collision"
    );

    // Branch 2: two distinct directories sharing one provider key (Codex keys by
    // frontmatter `name`, so separate directories collide only on the provider-key check).
    let codex_msg = codex_skills.join("rimz-message");
    let codex_dup = codex_skills.join("duplicate");
    for dir in [&codex_msg, &codex_dup] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
        )
        .unwrap();
    }
    let codex = crate::agents::find_definition("codex").unwrap();
    let dup_name: SkillName = "duplicate".parse().unwrap();
    assert!(
        !is_model_callable_on_disk("rimz-message", codex, &env, std::slice::from_ref(&msg_name),),
        "unlisted directory with the same provider key must drop seat-added rimz-message"
    );
    assert!(
        is_model_callable_on_disk("rimz-message", codex, &env, &[msg_name, dup_name]),
        "listing both directories removes the provider-key collision"
    );
}

/// A checkout whose `.agents/skills` holds a skill directory for each of `names`.
fn project_checkout(names: &[&str]) -> tempfile::TempDir {
    let checkout = tempfile::tempdir().unwrap();
    for name in names {
        write_skill(&checkout.path().join(".agents/skills").join(name), name);
    }
    checkout
}

fn write_skill(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Project.\n---\nProject.\n"),
    )
    .unwrap();
}

fn host_apply(
    kind: &str,
    listed: &[&str],
    checkout: &Path,
) -> Result<HostSkillPlan, LaunchSettingsErr> {
    host_apply_in(kind, listed, checkout, None)
}

fn host_apply_in(
    kind: &str,
    listed: &[&str],
    checkout: &Path,
    chain: Option<&SkillChain>,
) -> Result<HostSkillPlan, LaunchSettingsErr> {
    let adapter = crate::agents::find_definition(kind).unwrap();
    let listed: Vec<SkillName> = listed.iter().map(|name| name.parse().unwrap()).collect();
    let provider = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    adapter
        .spec()
        .host_skills
        .apply(
            (Some(&listed), &[], None),
            Some(provider.path()),
            Some(&ProjectTier::new(checkout.to_owned(), adapter)),
            chain,
            (artifacts.path(), artifacts.path()),
            &mut Vec::new(),
        )
        .map(|(plan, _)| plan)
}

fn keys(names: &[&str]) -> Vec<ProviderSkillKey> {
    names
        .iter()
        .map(|name| ProviderSkillKey::new((*name).to_owned()))
        .collect()
}

#[test]
fn host_switches_govern_the_kinds_native_project_folder() {
    let checkout = project_checkout(&["listed", "unlisted"]);
    let codex = host_apply("codex", &["listed"], checkout.path()).unwrap_or_else(|error| {
        panic!("codex reads .agents/skills, so a listed project skill resolves: {error}")
    });
    assert_eq!(
        codex,
        HostSkillPlan::Applied {
            flag: "-c skills.config",
            effect: "hidden",
            listed: keys(&["listed"]),
            unlisted: keys(&["unlisted"]),
        }
    );
    let own = checkout.path().join(".claude/skills");
    write_skill(&own.join("own-listed"), "own-listed");
    write_skill(&own.join("own-unlisted"), "own-unlisted");
    let claude = host_apply("claude", &["own-listed"], checkout.path()).unwrap_or_else(|error| {
        panic!("claude reads .claude/skills, so a listed skill there resolves: {error}")
    });
    let HostSkillPlan::Applied {
        listed, unlisted, ..
    } = claude
    else {
        panic!("claude carries a switch")
    };
    assert_eq!(listed, keys(&["own-listed"]));
    assert_eq!(
        unlisted,
        keys(&["own-unlisted"]),
        "claude never reads .agents/skills, so the switch names only its own folder"
    );
}

#[test]
fn host_claude_in_a_linked_worktree_without_its_own_folder_reads_the_main_checkouts() {
    let main = tempfile::tempdir().unwrap();
    let admin = main.path().join(".git/worktrees/linked");
    for dir in ["objects", "refs"] {
        std::fs::create_dir_all(main.path().join(".git").join(dir)).unwrap();
    }
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(main.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/linked\n").unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let linked = main.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    write_skill(&main.path().join(".claude/skills/shared"), "shared");
    let HostSkillPlan::Applied { unlisted, .. } = host_apply("claude", &[], &linked).unwrap()
    else {
        panic!("claude carries a switch")
    };
    assert_eq!(unlisted, keys(&["shared"]));
    write_skill(&linked.join(".claude/skills/own"), "own");
    let HostSkillPlan::Applied { unlisted, .. } = host_apply("claude", &[], &linked).unwrap()
    else {
        panic!("claude carries a switch")
    };
    assert_eq!(unlisted, keys(&["own"]), "the worktree's own folder wins");
}

#[test]
fn host_refuses_a_listed_skill_only_the_unread_project_tier_holds() {
    let checkout = project_checkout(&["proj"]);
    let path = checkout.path().display();
    let refusal = |kind: &str| match host_apply(kind, &["proj"], checkout.path()) {
        Err(LaunchSettingsErr::Skills(error @ SkillErr::ProjectOnly { .. })) => error.to_string(),
        other => panic!("{kind} cannot reach a tier-only skill at host: {other:?}"),
    };
    let tail = |kind: &str| {
        format!(
            "listed skill 'proj' is only in the project skills at {path}/.agents/skills, \
             which {kind} does not read at host isolation; "
        )
    };
    assert_eq!(
        refusal("claude"),
        format!(
            "{}add {path}/.claude/skills/proj (a symlink to {path}/.agents/skills/proj \
             works), launch with scoped isolation, or remove it from skills",
            tail("claude")
        )
    );
    assert_eq!(
        refusal("kiro"),
        format!(
            "{}add {path}/.kiro/skills/proj (a symlink to {path}/.agents/skills/proj \
             works), or remove it from skills",
            tail("kiro")
        ),
        "kiro cannot mark skills user-only, so a scoped list is no fix"
    );
    assert_eq!(
        refusal("hermes"),
        format!("{}remove it from skills", tail("hermes"))
    );
    for kind in ["pi", "codex"] {
        assert!(
            host_apply(kind, &["proj"], checkout.path()).is_ok(),
            "{kind} reads the tier itself"
        );
    }
    assert_eq!(
        host_apply("kiro", &["absent"], checkout.path()).unwrap(),
        HostSkillPlan::Unenforced,
        "a wholly missing name stays unenforced on a kind without a switch"
    );
    let own = checkout.path().join(".claude/skills");
    std::fs::create_dir_all(&own).unwrap();
    std::os::unix::fs::symlink("../../.agents/skills/proj", own.join("proj")).unwrap();
    assert!(matches!(
        host_apply("claude", &["proj"], checkout.path()),
        Ok(HostSkillPlan::Applied { .. })
    ));
}

#[test]
fn a_host_unknown_skill_names_only_the_project_folder_the_kind_reads() {
    let checkout = project_checkout(&[]);
    for (kind, read, unread) in [
        ("claude", ".claude/skills", Some(".agents/skills")),
        ("codex", ".agents/skills", None),
    ] {
        let Err(LaunchSettingsErr::Skills(SkillErr::UnknownSkill { roots, .. })) =
            host_apply(kind, &["absent"], checkout.path())
        else {
            panic!("an absent name is unknown to {kind}")
        };
        assert!(
            roots.contains(&checkout.path().join(read)),
            "{kind}: {read} missing from {roots:?}"
        );
        if let Some(unread) = unread {
            assert!(
                !roots.contains(&checkout.path().join(unread)),
                "{kind} never reads {unread}, so installing there is no fix: {roots:?}"
            );
        }
    }
}

#[test]
fn a_host_list_refuses_a_listed_skill_its_author_marks_user_only() {
    let checkout = project_checkout(&["proj"]);
    let skill = checkout.path().join(".agents/skills/proj");
    std::fs::create_dir_all(skill.join("agents")).unwrap();
    std::fs::write(
        skill.join("agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();
    match host_apply("codex", &["proj"], checkout.path()) {
        Err(LaunchSettingsErr::Skills(error @ SkillErr::UserOnly { .. })) => assert_eq!(
            error.to_string(),
            format!(
                "listed skill 'proj' is marked user-only for codex at {}; listing cannot \
                 lift that marker: drop it from skills or remove the marker",
                skill.join("agents/openai.yaml").display()
            )
        ),
        other => panic!("a listed user-only skill refuses: {other:?}"),
    }
    let marked = checkout.path().join(".qoder/skills/proj");
    std::fs::create_dir_all(&marked).unwrap();
    std::fs::write(
        marked.join("SKILL.md"),
        "---\nname: proj\ndescription: Project.\ndisable-model-invocation: true\n---\nProject.\n",
    )
    .unwrap();
    assert!(
        matches!(
            host_apply("qoder", &["proj"], checkout.path()),
            Ok(HostSkillPlan::Unenforced)
        ),
        "a kind without a switch enforces no list, so its launch skips the marker check"
    );
}

#[test]
fn a_kind_without_a_switch_reads_only_the_listed_names() {
    let home = tempfile::tempdir().unwrap();
    let chain = SkillChain::from_home(home.path());
    std::fs::create_dir_all(&chain.library).unwrap();
    // A link to itself fails every read of its SKILL.md.
    std::os::unix::fs::symlink("unreadable", chain.library.join("unreadable")).unwrap();
    let checkout = project_checkout(&["proj"]);
    assert!(
        matches!(
            host_apply_in("kiro", &["proj"], checkout.path(), Some(&chain)),
            Err(LaunchSettingsErr::Skills(SkillErr::ProjectOnly { .. }))
        ),
        "an unrelated unreadable library skill never fails the launch"
    );
}

#[test]
fn a_project_folder_link_into_rimz_state_is_a_project_skill() {
    let home = tempfile::tempdir().unwrap();
    let chain = SkillChain::from_home(home.path());
    let pack = chain.packs_dir.join("tools");
    write_skill(&pack.join(".agents/skills/proj"), "proj");
    let own = pack.join(".claude/skills");
    std::fs::create_dir_all(&own).unwrap();
    std::os::unix::fs::symlink("../../.agents/skills/proj", own.join("proj")).unwrap();
    assert!(
        matches!(
            host_apply_in("claude", &["proj"], &pack, Some(&chain)),
            Ok(HostSkillPlan::Applied { .. })
        ),
        "a pack checkout's .claude/skills link is the checkout's own skill"
    );
}

#[test]
fn host_claude_in_a_bare_repos_worktree_reads_only_its_own_folder() {
    let parent = tempfile::tempdir().unwrap();
    let bare = parent.path().join("repo.git");
    let admin = bare.join("worktrees/linked");
    for dir in ["objects", "refs"] {
        std::fs::create_dir_all(bare.join(dir)).unwrap();
    }
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/linked\n").unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let linked = parent.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    write_skill(&parent.path().join(".claude/skills/outside"), "outside");
    let HostSkillPlan::Applied { unlisted, .. } = host_apply("claude", &[], &linked).unwrap()
    else {
        panic!("claude carries a switch")
    };
    assert_eq!(
        unlisted,
        keys(&[]),
        "a bare repository has no main checkout whose .claude/skills Claude reads"
    );
}

#[test]
fn a_project_folder_never_supplies_a_rimz_skill() {
    let home = tempfile::tempdir().unwrap();
    let chain = SkillChain::from_home(home.path());
    write_skill(&chain.library.join("rimz-message"), "rimz-message");
    let checkout = project_checkout(&["rimz-message", "rimz-local", "proj"]);
    let tier = checkout.path().join(".agents/skills");
    let inventory = enumerate(None, Some(&tier), Some(&chain), |_| true).unwrap();
    assert_eq!(
        inventory.skills["rimz-message"].path,
        chain.library.join("rimz-message"),
        "the library's rimz-message outranks the project copy"
    );
    assert!(
        !inventory.skills.contains_key("rimz-local"),
        "a rimz-* name only the project holds is not in RimZ's inventory"
    );
    assert!(inventory.skills.contains_key("proj"));
}
