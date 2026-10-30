use super::*;
use crate::config::Profile;

fn profile(
    mode: Option<PermissionMode>,
    isolation: Option<Isolation>,
    tools: Option<&[&str]>,
    allowed: &[&str],
) -> Profile {
    let mut profile = super::super::agent::empty_profile("claude");
    profile.mode = mode;
    profile.isolation = isolation;
    profile.tools = tools.map(|tools| {
        crate::agents::ToolSet::parse(
            &tools
                .iter()
                .map(|tool| tool.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    });
    profile.allowed_tools = Some(allowed.iter().map(|rule| rule.parse().unwrap()).collect());
    profile
}

fn grants(profile: Profile, machine: Isolation) -> EffectiveGrants {
    let profiles = super::super::ProfilesConfig(BTreeMap::from([("probe".to_owned(), profile)]));
    EffectiveGrants::of(
        &crate::harness::spec::resolve_profile("probe", &profiles).unwrap(),
        machine,
    )
}

#[test]
fn widening_compares_effective_grants_in_both_directions() {
    use Isolation::{Host, Sandbox, Scoped};
    use PermissionMode::{Ask, Auto};
    let cases = [
        (
            profile(Some(Ask), None, None, &[]),
            profile(Some(Auto), None, None, &[]),
            Host,
        ),
        (
            profile(None, None, None, &[]),
            profile(Some(Ask), None, None, &[]),
            Host,
        ),
        (
            profile(None, None, None, &[]),
            profile(None, Some(Host), None, &[]),
            Scoped,
        ),
        (
            profile(None, None, None, &[]),
            profile(None, Some(Sandbox), None, &[]),
            Scoped,
        ),
        (
            profile(None, None, Some(&["Read"]), &[]),
            profile(None, None, None, &[]),
            Host,
        ),
        (
            profile(None, None, Some(&["Read"]), &[]),
            profile(None, None, Some(&["Read", "Write"]), &[]),
            Host,
        ),
        (
            profile(None, None, Some(&["Agent(a)"]), &[]),
            profile(None, None, Some(&["Agent(a,b)"]), &[]),
            Host,
        ),
        (
            profile(None, None, Some(&["Agent(a)"]), &[]),
            profile(None, None, Some(&["Agent"]), &[]),
            Host,
        ),
        (
            profile(None, None, Some(&["Bash(git *)"]), &[]),
            profile(None, None, Some(&["Bash(cargo *)"]), &[]),
            Host,
        ),
        (
            profile(None, None, None, &[]),
            profile(None, None, None, &["Bash(git *)"]),
            Host,
        ),
        (
            profile(None, None, None, &["Bash(git *)"]),
            profile(None, None, None, &["Bash(git diff*)"]),
            Host,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(old, new, machine)| {
            let old = grants(old.clone(), *machine);
            let new = grants(new.clone(), *machine);
            (new.widened_from(&old), old.widened_from(&new))
        })
        .collect();
    let expected = vec![
        (vec!["mode"], vec![]),
        (vec![], vec![]),
        (vec!["isolation"], vec![]),
        (vec![], vec!["isolation"]),
        (vec!["tools"], vec![]),
        (vec!["tools"], vec![]),
        (vec!["tools"], vec![]),
        (vec!["tools"], vec![]),
        (vec![], vec![]),
        (vec!["allowed-tools"], vec![]),
        (vec!["allowed-tools"], vec!["allowed-tools"]),
    ];
    assert_eq!(actual, expected);
}

#[test]
fn a_resolved_kind_change_alone_widens_in_neither_direction() {
    let claude = profile(Some(PermissionMode::Ask), None, Some(&["Read"]), &[]);
    let mut codex = claude.clone();
    codex.agent = "codex".to_owned();
    let claude = grants(claude, Isolation::Host);
    let codex = grants(codex, Isolation::Host);
    assert_ne!(codex.kind, claude.kind);
    assert!(codex.widened_from(&claude).is_empty());
    assert!(claude.widened_from(&codex).is_empty());
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn definition(root: &Path, name: &str, frontmatter: &str) {
    write(
        root,
        &format!("agents/{name}.md"),
        &format!("---\ndescription: Test\n{frontmatter}\n---\nPrompt."),
    );
}

#[test]
fn candidate_review_sees_staged_skills_inheritance_defaults_and_launch_names_once() {
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let staged = tempfile::tempdir().unwrap();
    definition(home.path(), "claude", "");
    definition(home.path(), "home-worker", "agent: crew:worker");
    for root in [source.path(), staged.path()] {
        write(
            root,
            "rimz-packs.toml",
            "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
        );
    }
    definition(
        source.path(),
        "worker",
        "agent: claude\nmode: ask\ntools: [Read]",
    );
    definition(
        staged.path(),
        "worker",
        "agent: claude\nmode: auto\ntools: [Read]",
    );
    definition(
        staged.path(),
        "new-worker",
        "agent: claude\ntools: [Skill]\nskills: [staged]",
    );
    write(
        staged.path(),
        "skills/staged/SKILL.md",
        "---\ndescription: Staged\n---\n# Staged",
    );
    write(
        staged.path(),
        "subagents/helper.md",
        "---\ndescription: Helper\nagent: claude\ntools: [Read]\n---\nHelp.",
    );
    write(
        staged.path(),
        "teams/pair.md",
        "---\nleader: lead\nstages: [Work]\nroles:\n  - agent: worker\n    role: lead\n    owns: [Work]\n---\nPair.",
    );
    crate::packs::install(home.path(), source.path()).unwrap();
    let before = crate::packs::resolve(home.path());
    let mut after = before.clone();
    after.packs[0].root = staged.path().to_owned();
    let env = BTreeMap::new();
    let machine = super::super::super::MachineConfig::default();
    let review = compare(
        home.path(),
        super::super::SkillCheck::Check {
            env: &env,
            machine_isolation: Isolation::Scoped,
        },
        &machine,
        &before,
        &after,
    );
    let names: Vec<_> = review
        .profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["helper", "home-worker", "new-worker", "pair.lead", "worker"],
        "{:?}",
        review
            .inventory
            .packs
            .iter()
            .map(|pack| &pack.items)
            .collect::<Vec<_>>()
    );
    for name in ["helper", "home-worker", "worker", "new-worker", "pair.lead"] {
        let profile = review
            .profiles
            .iter()
            .find(|profile| profile.name == name)
            .unwrap();
        assert_eq!(profile.grants.mode, PermissionMode::Auto);
        assert!(profile.widened.contains(&"mode"), "{profile:?}");
    }
    assert_eq!(
        review
            .profiles
            .iter()
            .find(|profile| profile.name == "new-worker")
            .unwrap()
            .widened,
        ["mode"]
    );
    let pack = &review.inventory.packs[0];
    assert!(
        pack.items
            .iter()
            .any(|item| item.kind == crate::packs::ItemType::Skills
                && item.name == "staged"
                && item.fate == crate::packs::ItemFate::Loads),
        "{:?}",
        pack.items
    );
    assert_eq!(
        crate::packs::Record::read(home.path()).unwrap(),
        before.record
    );
}

#[test]
fn changing_only_skill_content_does_not_repeat_unchanged_profiles() {
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let staged = tempfile::tempdir().unwrap();
    definition(home.path(), "claude", "");
    for root in [source.path(), staged.path()] {
        write(
            root,
            "rimz-packs.toml",
            "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
        );
        definition(root, "worker", "agent: claude\ntools: [Read]");
    }
    write(
        staged.path(),
        "skills/staged/SKILL.md",
        "---\ndescription: Staged\n---\n# Staged",
    );
    crate::packs::install(home.path(), source.path()).unwrap();
    let before = crate::packs::resolve(home.path());
    let mut after = before.clone();
    after.packs[0].root = staged.path().to_owned();
    let review = compare(
        home.path(),
        super::super::SkillCheck::Skip,
        &super::super::super::MachineConfig::default(),
        &before,
        &after,
    );
    assert!(review.profiles.is_empty(), "{:?}", review.profiles);
    assert!(
        review.inventory.packs[0]
            .items
            .iter()
            .any(|item| { item.name == "worker" && item.fate == crate::packs::ItemFate::Loads })
    );
    assert_eq!(
        review
            .inventory
            .packs
            .first()
            .and_then(|pack| pack.counts.get(&crate::packs::ItemType::Skills)),
        Some(&1)
    );
}

#[test]
fn a_new_kind_base_is_reported_once_for_both_launch_surfaces() {
    let home = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    write(
        source.path(),
        "rimz-packs.toml",
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
    );
    definition(source.path(), "claude", "");
    definition(source.path(), "worker", "agent: claude\ntools: [Read]");
    crate::packs::install(home.path(), source.path()).unwrap();
    let after = crate::packs::resolve(home.path());
    let review = compare(
        home.path(),
        super::super::SkillCheck::Skip,
        &super::super::super::MachineConfig::default(),
        &ResolvedPacks::default(),
        &after,
    );
    assert_eq!(
        review
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["claude", "worker"],
        "{:?}",
        review
            .inventory
            .packs
            .iter()
            .map(|pack| &pack.items)
            .collect::<Vec<_>>()
    );
    assert_eq!(review.profiles[0].namespace, "agents/subagents");
    assert!(review.profiles[0].widened.is_empty());
}
