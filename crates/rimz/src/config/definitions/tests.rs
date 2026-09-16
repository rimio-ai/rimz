use super::*;
use crate::config::SkillName;

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for kind in ["claude", "codex", "pi"] {
        write(
            root.path(),
            &format!("agents/{kind}.md"),
            "---\ndescription: Foundation.\n---\nBase.",
        );
    }
    root
}

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn definition(root: &Path, name: &str, fields: &str, body: &str) {
    write(
        root,
        name,
        &format!("---\ndescription: Probe.\n{fields}\n---\n{body}"),
    );
}

fn clean(root: &Path) -> LoadedDefinitions {
    let loaded = load(root, SkillLibraryCheck::Skip);
    assert!(loaded.errors.is_empty(), "{:#?}", loaded.errors);
    loaded
}

fn error(root: &Path, needle: &str) {
    let loaded = load(root, SkillLibraryCheck::Skip);
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message.contains(needle)),
        "expected {needle:?}: {:#?}",
        loaded.errors
    );
}

fn texts(profile: &Profile) -> Vec<&str> {
    profile
        .append_system_prompt_files
        .iter()
        .map(|source| match source {
            PromptSource::Text { text, .. } => text.as_str(),
            PromptSource::File(_) => panic!("definitions carry text"),
        })
        .collect()
}

#[test]
fn missing_trees_and_documentation_are_not_definitions() {
    let root = tempfile::tempdir().unwrap();
    assert!(clean(root.path()).rows.is_empty());
    for namespace in ["agents", "subagents", "teams", "traits"] {
        for name in ["AGENTS.md", "CLAUDE.md", "README.md", "nested/probe.md"] {
            write(
                root.path(),
                &format!("{namespace}/{name}"),
                "not frontmatter",
            );
        }
    }
    assert!(clean(root.path()).rows.is_empty());
    assert_eq!(source_paths(root.path()).len(), 4);
}

#[test]
fn bases_are_emitted_in_both_namespaces_with_sources() {
    let root = fixture();
    let loaded = clean(root.path());
    assert_eq!(loaded.rows.len(), 6);
    for kind in ["claude", "codex", "pi"] {
        let profile = &loaded.agent_profiles.0[kind];
        assert_eq!(profile, &loaded.subagent_profiles.0[kind]);
        assert_eq!(profile.agent, kind);
        assert_eq!(
            profile.system_prompt_file,
            Some(PromptSource::Text {
                origin: root.path().join(format!("agents/{kind}.md")),
                text: "Base.".to_owned()
            })
        );
        assert_eq!(
            loaded.sources.agent_profiles[kind],
            root.path().join(format!("agents/{kind}.md"))
        );
        assert!(profile.args.is_none());
    }
    assert_eq!(source_paths(root.path()).len(), 7);
}

#[test]
fn runtime_resolution_aliases_and_defaults() {
    let root = fixture();
    for (name, fields, kind, model, effort) in [
        (
            "alias",
            "model: astra\ntools: [Bash]",
            "codex",
            "gpt-6-astra",
            "xhigh",
        ),
        (
            "fable",
            "model: fable\ntools: [Bash]",
            "claude",
            "fable",
            "high",
        ),
        (
            "custom",
            "agent: claude\nmodel: custom\ntools: [Bash]",
            "claude",
            "custom",
            "xhigh",
        ),
        (
            "prefix",
            "model: gpt-custom\ntools: [Bash]",
            "codex",
            "gpt-custom",
            "xhigh",
        ),
        (
            "pi-seat",
            "agent: pi\nmodel: custom",
            "pi",
            "custom",
            "xhigh",
        ),
    ] {
        definition(root.path(), &format!("agents/{name}.md"), fields, "");
        let loaded = clean(root.path());
        let profile = &loaded.agent_profiles.0[name];
        assert_eq!(profile.agent, kind);
        assert_eq!(profile.model.as_deref(), Some(model));
        assert_eq!(profile.effort.as_deref(), Some(effort));
        assert_eq!(
            profile.auto_compact.as_deref(),
            if kind == "pi" { None } else { Some("258k") }
        );
        assert!(profile.append_system_prompt_files.is_empty());
        if kind == "claude" {
            assert_eq!(profile.mode, Some(crate::agents::PermissionMode::Auto));
        }
    }
}

#[test]
fn chains_inherit_launch_fields_but_render_each_own_craft() {
    let root = fixture();
    write(root.path(), "traits/asking.md", "Ask well.");
    write(root.path(), "traits/writing.md", "Write well.");
    definition(
        root.path(),
        "agents/parent.md",
        "model: astra\ntools: [Bash, Skill]\nskills: [one]\nsubagents: [general]\nbudget: 2.5\nmodel-reminder: false\ntraits: [asking]",
        "Parent\n\n${traits}",
    );
    definition(
        root.path(),
        "agents/child.md",
        "agent: parent\neffort: low\nauto-compact: 200000\ntraits: [writing]\nskills: null\nsubagents: null",
        "Child\n\n${traits}\n\n${shell}",
    );
    let loaded = clean(root.path());
    let child = &loaded.agent_profiles.0["child"];
    assert_eq!(child.agent, "codex");
    assert_eq!(child.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(child.effort.as_deref(), Some("low"));
    assert_eq!(child.budget.as_deref(), Some("2.5"));
    assert_eq!(child.auto_compact.as_deref(), Some("200000"));
    assert_eq!(child.model_reminder, Some(false));
    assert_eq!(child.skills, Some(vec![]));
    assert_eq!(child.subagents, Some(vec![]));
    assert_eq!(
        texts(child),
        ["Parent\n\nAsk well.", "Child\n\nWrite well.\n\n${shell}"]
    );
    assert_eq!(child.args, loaded.agent_profiles.0["parent"].args);
}

#[test]
fn failed_parents_exclude_dependents_and_keep_independent_profiles() {
    let root = fixture();
    definition(
        root.path(),
        "agents/parent.md",
        "model: astra\ntools: [Bash]\ntraits: [missing]",
        "${traits}",
    );
    definition(root.path(), "agents/child.md", "agent: parent", "Child.");
    definition(root.path(), "agents/independent.md", "agent: pi", "");
    let loaded = load(root.path(), SkillLibraryCheck::Skip);
    assert_eq!(loaded.errors.len(), 2);
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message == "follows `parent`, which failed to load")
    );
    assert!(!loaded.agent_profiles.0.contains_key("child"));
    assert!(!loaded.agent_profiles.0.contains_key("parent"));
    assert!(loaded.agent_profiles.0.contains_key("independent"));
}

#[test]
fn invalid_chain_targets_and_cycles_are_diagnosed() {
    let root = fixture();
    definition(root.path(), "subagents/foreign.md", "agent: pi", "");
    for (fields, expected) in [
        ("model: custom", "model 'custom' names no runtime"),
        ("", "it sets no model"),
        ("agent: missing", "follows unknown profile 'missing'"),
        ("agent: foreign", "rimz resolves `agent:` within agents"),
        (
            "agent: probe",
            "follows itself through `agent:` (probe -> probe)",
        ),
        (
            "agent: claude\nmodel: astra\ntools: []",
            "runs on 'claude' but model 'astra' runs on 'codex'",
        ),
        ("model: astra", "lists no `tools`, which codex needs"),
    ] {
        definition(root.path(), "agents/probe.md", fields, "");
        error(root.path(), expected);
    }
    definition(root.path(), "agents/probe.md", "agent: other", "");
    definition(root.path(), "agents/other.md", "agent: probe", "");
    error(root.path(), "other -> probe -> other");
}

#[test]
fn frontmatter_rejects_bad_shapes_unknown_and_retired_keys() {
    let root = fixture();
    for (text, expected) in [
        ("body", "missing YAML frontmatter"),
        ("---\nmodel: astra", "never closes"),
        ("---\n[one, two]\n---", "not a mapping"),
        ("---\n[\n---", "malformed"),
        (
            "---\ndescription: Probe\nmodle: astra\n---",
            "unknown field",
        ),
        ("---\nsoul: old\n---", "still selects `soul:`"),
        ("---\nmeka: old\n---", "still sets `meka:`"),
        ("---\nsignals: []\n---", "reads on a team role"),
        ("---\nflip-compact: off\n---", "a solo seat flips no stage"),
        ("---\nmodel: astra\n---", "no non-empty `description:`"),
        (
            "---\ndescription: |\n  first\n  second\n---",
            "multi-line `description:`",
        ),
        (
            "---\ndescription: Probe\nmodel-reminder: wrong\n---",
            "malformed frontmatter",
        ),
        (
            "---\ndescription: Probe\nmode: wrong\n---",
            "malformed frontmatter",
        ),
        (
            "---\ndescription: Probe\ntools: Bash\n---",
            "malformed frontmatter",
        ),
    ] {
        write(root.path(), "agents/probe.md", text);
        error(root.path(), expected);
    }
}

#[test]
fn names_and_kind_bases_are_validated() {
    let root = fixture();
    for name in ["claude", "a.b", "a/b", ""] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("name: '{name}'\nagent: pi"),
            "",
        );
        assert!(!load(root.path(), SkillLibraryCheck::Skip).errors.is_empty());
    }
    definition(
        root.path(),
        "agents/probe.md",
        "name: duplicate\nagent: pi",
        "",
    );
    definition(
        root.path(),
        "subagents/probe.md",
        "name: duplicate\nagent: pi",
        "",
    );
    let loaded = load(root.path(), SkillLibraryCheck::Skip);
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.message.contains("declared by both"))
    );
    assert!(!loaded.agent_profiles.0.contains_key("duplicate"));
    assert!(!loaded.subagent_profiles.0.contains_key("duplicate"));
    write(
        root.path(),
        "agents/claude.md",
        "---\ndescription: Base\nmodel: fable\n---\nBase.",
    );
    error(root.path(), "unknown field");
    write(
        root.path(),
        "agents/claude.md",
        "---\ndescription: Base\n---",
    );
    error(root.path(), "no prompt body");
}

#[test]
fn crafts_require_a_kind_base_but_bodyless_profiles_do_not() {
    let root = tempfile::tempdir().unwrap();
    definition(root.path(), "agents/probe.md", "agent: pi", "");
    assert!(clean(root.path()).agent_profiles.0.contains_key("probe"));
    definition(root.path(), "agents/probe.md", "agent: pi", "Craft.");
    error(root.path(), "kind base `agents/pi.md` is missing");
}

#[test]
fn traits_deduplicate_and_preserve_unrelated_placeholders() {
    let root = fixture();
    write(root.path(), "traits/one.md", " One.\n");
    write(root.path(), "traits/two.md", "Two.");
    let source = root.path().join("agents/probe.md");
    let render = |body, names: &[&str]| {
        traits::render(
            root.path(),
            &source,
            body,
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        render("Head\n\n${traits}\n\nTail ${HOME}", &["one", "two", "one"]).unwrap(),
        "Head\n\nOne.\n\nTwo.\n\nTail ${HOME}"
    );
    assert_eq!(
        render("Head\n\n${traits}\n\nTail", &[]).unwrap(),
        "Head\n\n\n\nTail"
    );
    assert_eq!(
        render("A\n${traits}\nB\n\n${traits}\n\nC", &["two"]).unwrap(),
        "A\nTwo.\nB\n\nC"
    );
    assert!(
        render("No token", &["one"])
            .unwrap_err()
            .message
            .contains("no `${traits}` token")
    );
    assert!(render("${traits}", &["missing"]).is_err());
    assert!(render("${traits}", &["../outside"]).is_err());
}

#[test]
fn auto_compact_bounds_and_spelling() {
    let root = fixture();
    for value in ["100k", "500k", "200000", "1m", "1M"] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: []\nauto-compact: {value}"),
            "",
        );
        assert_eq!(
            clean(root.path()).agent_profiles.0["probe"]
                .auto_compact
                .as_deref(),
            Some(value)
        );
    }
    for value in [
        "200",
        "2m",
        "80%",
        "99999",
        "1000001",
        "-200000",
        "0.2m",
        "18446744073709551615m",
    ] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: []\nauto-compact: '{value}'"),
            "",
        );
        error(root.path(), "token count from 100k through 1M");
    }
}

#[test]
fn subagent_allowlists_replace_native_delegation() {
    let root = fixture();
    definition(root.path(), "subagents/child.md", "agent: pi", "");
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Bash]\nsubagents: [' general ', child, claude, child]",
        "",
    );
    assert_eq!(
        clean(root.path()).agent_profiles.0["probe"]
            .subagents
            .as_deref(),
        Some(
            [
                "general".to_owned(),
                "child".to_owned(),
                "claude".to_owned()
            ]
            .as_slice()
        )
    );
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Agent]\nsubagents: []",
        "",
    );
    error(root.path(), "lists the Agent tool");
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: []\nsubagents: [missing]",
        "",
    );
    error(root.path(), "unknown subagent profile(s)");
    definition(
        root.path(),
        "subagents/child.md",
        "agent: pi\nsubagents: []",
        "",
    );
    error(root.path(), "a child cannot launch again");
}

#[test]
fn skills_are_structural_without_a_library_and_deduplicated() {
    let root = fixture();
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Skill]\nskills: [' one ', one, two]",
        "",
    );
    let loaded = clean(root.path());
    let skills = loaded.agent_profiles.0["probe"].skills.as_ref().unwrap();
    assert_eq!(
        skills.iter().map(SkillName::as_str).collect::<Vec<_>>(),
        ["one", "two"]
    );
    for name in ["../etc/passwd", "one:auto", "white space", ".", "a/b"] {
        definition(
            root.path(),
            "agents/probe.md",
            &format!("model: astra\ntools: [Skill]\nskills: ['{name}']"),
            "",
        );
        error(root.path(), "one bare directory name");
    }
    definition(
        root.path(),
        "agents/probe.md",
        "model: astra\ntools: [Bash]\nskills: []",
        "",
    );
    error(root.path(), "lists no Skill tool");
    definition(
        root.path(),
        "agents/probe.md",
        "agent: pi\nskills: null",
        "",
    );
    assert_eq!(
        clean(root.path()).agent_profiles.0["probe"].skills,
        Some(vec![])
    );
}

#[test]
fn skill_library_checks_existence_and_runtime_specific_markers() {
    let root = fixture();
    let library = root.path().join("skills");
    for (kind, fields) in [
        ("claude", "tools: [Skill]"),
        ("codex", "tools: [Skill]"),
        ("pi", ""),
    ] {
        definition(
            root.path(),
            &format!("agents/{kind}-seat.md"),
            &format!("agent: {kind}\n{fields}\nskills: [one]"),
            "",
        );
    }
    let loaded = load(root.path(), SkillLibraryCheck::Check(&library));
    assert_eq!(loaded.errors.len(), 3);
    assert!(
        loaded
            .errors
            .iter()
            .all(|error| error.message.contains("missing at"))
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: colons: are accepted\ndisable-model-invocation: true\n---\nSkill.",
    );
    let loaded = load(root.path(), SkillLibraryCheck::Check(&library));
    assert_eq!(loaded.errors.len(), 2);
    assert!(loaded.agent_profiles.0.contains_key("codex-seat"));
    write(
        root.path(),
        "skills/one/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: false\n",
    );
    assert_eq!(
        load(root.path(), SkillLibraryCheck::Check(&library))
            .errors
            .len(),
        3
    );
    write(
        root.path(),
        "skills/one/SKILL.md",
        "---\ndescription: colons: accepted\n---\nSkill.",
    );
    write(
        root.path(),
        "skills/one/agents/openai.yaml",
        "policy:\n  allow_implicit_invocation: true\n",
    );
    assert!(
        load(root.path(), SkillLibraryCheck::Check(&library))
            .errors
            .is_empty()
    );
}

#[test]
fn yaml_comments_quotes_and_null_tools_are_supported() {
    let root = fixture();
    definition(
        root.path(),
        "agents/probe.md",
        "model: 'astra' # comment\ntools: null\nbudget: '3.50'\nmodel-reminder: true",
        "",
    );
    let loaded = clean(root.path());
    assert_eq!(
        loaded.agent_profiles.0["probe"].budget.as_deref(),
        Some("3.50")
    );
    let argv = shlex::split(loaded.agent_profiles.0["probe"].args.as_deref().unwrap()).unwrap();
    assert!(argv.contains(&"features.shell_tool=false".to_owned()));
}
