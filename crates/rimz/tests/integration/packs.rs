use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::packs::{self, ExclusionTarget, ItemKey, ItemType};
use serde_json::Value;

use crate::common::Env;

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn source(env: &Env) -> PathBuf {
    let source = env.home_root.join("source");
    write(
        source.join("rimz-packs.toml"),
        "future = true\n[[pack]]\nname = 'crew'\ndescription = 'The crew'\npath = 'crew'\n\
         [[pack]]\nname = 'tools'\ndescription = 'The tools'\npath = 'tools'\n",
    );
    write(
        source.join("crew/agents/pi.md"),
        "---\ndescription: Pi base\n---\nBase.",
    );
    write(
        source.join("crew/agents/coder.md"),
        "---\ndescription: Pack coder\nagent: pi\n---\nCode.",
    );
    write(
        source.join("crew/subagents/helper.md"),
        "---\ndescription: Pack helper\nagent: pi\n---\nHelp.",
    );
    write(source.join("crew/traits/craft.md"), "Craft.");
    write(
        source.join("crew/teams/pair.md"),
        "---\nleader: lead\nstages: [Work]\nroles:\n  - agent: coder\n    role: lead\n    owns: [Work]\n---\nPair.",
    );
    write(source.join("crew/skills/coder/SKILL.md"), "# Coder");
    write(source.join("tools/skills/extra/SKILL.md"), "# Extra");
    source
}

fn installed(env: &Env) -> PathBuf {
    let source = source(env);
    packs::install(&env.agents_home(), &source).unwrap();
    source
}

fn installed_with_home_profile(env: &Env) -> PathBuf {
    let source = installed(env);
    write(
        env.agents_home().join("agents/other.md"),
        "---\ndescription: Other\nagent: pi\n---\nOther.",
    );
    source
}

fn json(env: &Env, args: &[&str]) -> Value {
    let output = env.rimz().args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn item<'a>(show: &'a Value, kind: &str, name: &str) -> &'a Value {
    show["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == kind && item["name"] == name)
        .unwrap_or_else(|| panic!("missing {kind}/{name}: {show}"))
}

#[test]
fn pack_diagnostics_still_fail_validation_without_blocking_healthy_explanations() {
    for problem in [
        "manifest",
        "skill-conflict",
        "qualified-parent",
        "trait-conflict",
    ] {
        let env = Env::new();
        let source = installed(&env);
        write(
            env.agents_home().join("agents/good.md"),
            "---\ndescription: Good\nagent: pi\n---\nGood.",
        );
        write(
            env.agents_home().join("agents/pi.md"),
            "---\ndescription: Base\n---\nBase.",
        );
        match problem {
            "manifest" => fs::remove_file(source.join("rimz-packs.toml")).unwrap(),
            "skill-conflict" => write(source.join("tools/skills/coder/SKILL.md"), "# Coder"),
            "qualified-parent" => {
                write(
                    source.join("crew/agents/good.md"),
                    "---\ndescription: Bad parent\nagent: missing\n---\n",
                );
                write(
                    env.agents_home().join("agents/dependent.md"),
                    "---\ndescription: Dependent\nagent: crew:good\n---\n",
                );
            }
            "trait-conflict" => {
                write(source.join("crew/traits/good.md"), "Crew.");
                write(source.join("tools/traits/good.md"), "Tools.");
            }
            _ => unreachable!(),
        }
        env.rimz()
            .args(["agents", "validate"])
            .assert()
            .code(1)
            .stderr(contains("definition error"));
        env.rimz()
            .args(["agents", "explain", "good"])
            .assert()
            .success();
    }
}

#[test]
fn home_launch_names_override_pack_teams_even_when_broken() {
    for namespace in ["agents", "subagents", "commands"] {
        for broken in [false, true] {
            let env = Env::new();
            let source = installed_with_home_profile(&env);
            if namespace == "commands" {
                write(
                    env.rimz_home().join("config.toml"),
                    "[agents.commands]\ngood = 'true'\n",
                );
            } else {
                write(
                    env.agents_home().join(namespace).join("good.md"),
                    "---\ndescription: Home\nagent: pi\n---\nHome.",
                );
            }
            let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
            write(
                source.join("crew/teams/good.md"),
                if broken { "broken" } else { &team },
            );
            env.rimz()
                .args(["agents", "explain", "other"])
                .assert()
                .success();
            let crew = json(&env, &["packs", "show", "crew", "--json"]);
            assert_eq!(
                item(&crew, "teams", "good")["fate"]["state"],
                "overridden",
                "{crew}"
            );
            let path = if namespace == "commands" {
                env.rimz_home().join("config.toml")
            } else {
                env.agents_home().join(namespace).join("good.md")
            };
            assert_eq!(
                item(&crew, "teams", "good")["fate"]["path"],
                path.to_str().unwrap(),
                "{crew}"
            );
            env.rimz()
                .args(["packs", "show", "crew"])
                .assert()
                .success()
                .stdout(contains(format!("overridden by {}", path.display())));
            env.rimz().args(["agents", "validate"]).assert().success();
        }
    }
}

#[test]
fn home_teams_override_pack_profiles_and_their_conflicts() {
    for namespace in ["agents", "subagents"] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
        write(env.agents_home().join("teams/review.md"), &team);
        for pack in ["crew", "tools"] {
            write(
                source.join(pack).join(namespace).join("review.md"),
                "---\ndescription: Review\nagent: pi\n---\nReview.",
            );
        }
        env.rimz()
            .args(["agents", "explain", "other"])
            .assert()
            .success();
        for pack in ["crew", "tools"] {
            let show = json(&env, &["packs", "show", pack, "--json"]);
            assert_eq!(
                item(&show, namespace, "review")["fate"]["state"],
                "overridden",
                "{show}"
            );
        }
        env.rimz().args(["agents", "validate"]).assert().success();
    }
}

#[test]
fn pack_launch_name_conflicts_cross_profile_and_team_trees() {
    for namespace in ["agents", "subagents"] {
        for team_pack in ["crew", "tools"] {
            let env = Env::new();
            let source = installed_with_home_profile(&env);
            write(
                source.join("crew").join(namespace).join("review.md"),
                "---\ndescription: Review\nagent: pi\n---\nReview.",
            );
            let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
            write(source.join(team_pack).join("teams/review.md"), &team);
            let crew = json(&env, &["packs", "show", "crew", "--json"]);
            let tools = json(&env, &["packs", "show", team_pack, "--json"]);
            for row in [
                item(&crew, namespace, "review"),
                item(&tools, "teams", "review"),
            ] {
                assert_eq!(row["fate"]["state"], "conflict", "{row}");
                let message = row["fate"]["message"].as_str().unwrap();
                assert!(
                    message.contains(&format!(
                        "rimz packs exclude crew:review --type {namespace}"
                    )),
                    "{message}"
                );
                assert!(
                    message.contains(&format!(
                        "rimz packs exclude {team_pack}:review --type teams"
                    )),
                    "{message}"
                );
            }
            env.rimz()
                .args(["agents", "explain", "other"])
                .assert()
                .success();
            env.rimz().args(["agents", "validate"]).assert().code(1);
        }
    }
}

#[test]
fn reserved_pack_team_cells_are_item_failures_not_set_failures() {
    // A launch of a kind, a virtual shape, or a profile's fast form is not the team's.
    for (name, launches) in [
        ("claude", true),
        ("term", false),
        ("claude-yolo", true),
        ("claude-fast", true),
        ("other-fast", true),
        ("coder-fast", true),
    ] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
        write(source.join("crew/teams").join(format!("{name}.md")), &team);
        env.rimz()
            .args(["agents", "explain", "other"])
            .assert()
            .success();
        if launches {
            env.rimz()
                .args(["agents", "explain", name])
                .assert()
                .success();
        }
        let crew = json(&env, &["packs", "show", "crew", "--json"]);
        let fate = &item(&crew, "teams", name)["fate"];
        assert_eq!(fate["state"], "failed", "{crew}");
        assert!(
            fate["message"]
                .as_str()
                .unwrap()
                .contains(&format!("rimz packs exclude crew:{name} --type teams")),
            "{crew}"
        );
        env.rimz().args(["agents", "validate"]).assert().code(1);
    }
}

fn assert_kind_launches(env: &Env, kind: &str) {
    for spec in [
        kind.to_owned(),
        format!("{kind}-yolo"),
        format!("{kind}-fast"),
    ] {
        env.rimz()
            .args(["agents", "explain", &spec])
            .assert()
            .success();
    }
    let machine = rimz::config::MachineConfig::parse_text(
        &env.rimz_home().join("config.toml"),
        "",
        &env.agents_home(),
    )
    .unwrap();
    for error in &machine.notices.definition_errors {
        assert_eq!(
            error.cause,
            rimz::config::definitions::DefinitionCause::PackProblem,
            "{error}"
        );
    }
    let launch = rimz::config::effective::load(&machine, &env.project_root).unwrap();
    let peer = rimz::harness::plan::resolve_launch(
        &launch,
        rimz::config::effective::ProfileScope::Agents,
        &machine.agents.commands,
        Some("peer"),
        None,
    )
    .expect("the built-in peer team can still launch both kinds");
    assert_eq!(peer.layout.agent_cells().count(), 2);
}

#[test]
fn pack_teams_named_like_kinds_do_not_block_kind_launches() {
    let env = Env::new();
    let source = installed_with_home_profile(&env);
    let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
    write(source.join("crew/teams/claude.md"), &team);
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert_eq!(item(&crew, "teams", "claude")["fate"]["state"], "failed");
    env.rimz().args(["agents", "validate"]).assert().code(1);
    assert_kind_launches(&env, "claude");
}

#[test]
fn pack_profiles_named_like_kinds_do_not_block_kind_launches() {
    for (namespace, stem) in [("subagents", "claude"), ("agents", "renamed")] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        write(
            source
                .join("crew")
                .join(namespace)
                .join(format!("{stem}.md")),
            "---\nname: claude\ndescription: Invalid\nagent: pi\n---\nInvalid.",
        );
        let crew = json(&env, &["packs", "show", "crew", "--json"]);
        assert_eq!(item(&crew, namespace, "claude")["fate"]["state"], "failed");
        env.rimz().args(["agents", "validate"]).assert().code(1);
        assert_kind_launches(&env, "claude");
    }
}

#[test]
fn pack_team_conflicts_named_like_kinds_do_not_block_kind_launches() {
    let env = Env::new();
    let source = installed_with_home_profile(&env);
    let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
    for pack in ["crew", "tools"] {
        write(source.join(pack).join("teams/codex.md"), &team);
    }
    for pack in ["crew", "tools"] {
        let show = json(&env, &["packs", "show", pack, "--json"]);
        assert_eq!(item(&show, "teams", "codex")["fate"]["state"], "conflict");
    }
    env.rimz().args(["agents", "validate"]).assert().code(1);
    assert_kind_launches(&env, "codex");
}

#[test]
fn kind_named_pack_profiles_report_the_kind_name_error() {
    for namespace in ["agents", "subagents"] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        write(
            source.join("crew").join(namespace).join("invalid.md"),
            "---\nname: claude\ndescription: Invalid\nagent: pi\n---\nInvalid.",
        );
        let crew = json(&env, &["packs", "show", "crew", "--json"]);
        assert_eq!(
            item(&crew, namespace, "claude")["fate"]["message"],
            format!(
                "takes the name of an agent kind; rename it; run `rimz packs exclude crew:claude --type {namespace}`"
            ),
            "{crew}"
        );
    }
}

#[test]
fn pack_kind_base_conflicts_and_home_kind_names_keep_the_launch_failure() {
    for home in [false, true] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        if home {
            write(
                env.agents_home().join("subagents/claude.md"),
                "---\ndescription: Home\nagent: pi\n---\nHome.",
            );
        } else {
            for pack in ["crew", "tools"] {
                write(
                    source.join(pack).join("agents/claude.md"),
                    "---\ndescription: Base\n---\nBase.",
                );
            }
        }
        env.rimz()
            .args(["agents", "explain", "claude"])
            .assert()
            .code(1)
            .stderr(contains(if home {
                "takes the name of an agent kind; rename it"
            } else {
                "rimz packs exclude crew:claude --type agents"
            }));
        env.rimz().args(["agents", "validate"]).assert().code(1);
    }
}

#[test]
fn reserved_pack_profile_names_are_item_failures_not_set_failures() {
    for namespace in ["agents", "subagents"] {
        for name in [
            "show", "list", "stop", "user", "rimz", "all", "me", "peer", "claude-2", "a:b", "a#b",
            "a,b", "a+b", "a/b", "a(b)", "a b",
        ] {
            let env = Env::new();
            let source = installed_with_home_profile(&env);
            write(
                source.join("crew").join(namespace).join("invalid.md"),
                &format!("---\nname: '{name}'\ndescription: Invalid\nagent: pi\n---\nInvalid."),
            );
            env.rimz()
                .args(["agents", "explain", "other"])
                .assert()
                .success();
            let crew = json(&env, &["packs", "show", "crew", "--json"]);
            let fate = &item(&crew, namespace, name)["fate"];
            assert_eq!(fate["state"], "failed", "{crew}");
            let message = fate["message"].as_str().unwrap();
            let command = message
                .rsplit_once("; run `")
                .unwrap()
                .1
                .strip_suffix('`')
                .unwrap();
            assert_eq!(
                shlex::split(command).unwrap(),
                [
                    "rimz",
                    "packs",
                    "exclude",
                    &format!("crew:{name}"),
                    "--type",
                    namespace
                ],
                "{crew}"
            );
            env.rimz().args(["agents", "validate"]).assert().code(1);
        }
    }
}

#[test]
fn pack_profile_exclusion_fix_preserves_an_unsafe_name_as_one_argument() {
    let env = Env::new();
    let source = installed_with_home_profile(&env);
    let name = "bad (name)";
    write(
        source.join("crew/agents/invalid.md"),
        &format!("---\nname: '{name}'\ndescription: Invalid\nagent: pi\n---\nInvalid."),
    );
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    let message = item(&crew, "agents", name)["fate"]["message"]
        .as_str()
        .unwrap();
    let command = message
        .rsplit_once("; run `")
        .unwrap()
        .1
        .strip_suffix('`')
        .unwrap();
    let args = shlex::split(command).unwrap();
    assert_eq!(
        args,
        [
            "rimz",
            "packs",
            "exclude",
            "crew:bad (name)",
            "--type",
            "agents"
        ]
    );
    env.rimz().args(&args[1..]).assert().success();
    env.rimz().args(["agents", "validate"]).assert().success();
}

#[test]
fn pack_team_names_and_preparation_errors_stay_at_the_item() {
    for (name, role) in [
        ("show", "lead"),
        ("me", "lead"),
        ("bad.name", "lead"),
        ("bad/name", "lead"),
        ("bad-role", "claude"),
        ("bad-address", "all"),
    ] {
        let env = Env::new();
        let source = installed_with_home_profile(&env);
        write(
            source.join("crew/teams/invalid.md"),
            &format!(
                "---\nname: '{name}'\nleader: {role}\nstages: [Work]\nroles:\n  - agent: coder\n    role: {role}\n    owns: [Work]\n---\nTeam."
            ),
        );
        env.rimz()
            .args(["agents", "explain", "other"])
            .assert()
            .success();
        let crew = json(&env, &["packs", "show", "crew", "--json"]);
        let fate = &item(&crew, "teams", name)["fate"];
        assert_eq!(fate["state"], "failed", "{crew}");
        assert!(
            fate["message"]
                .as_str()
                .unwrap()
                .contains(&format!("rimz packs exclude crew:{name} --type teams")),
            "{crew}"
        );
        env.rimz().args(["agents", "validate"]).assert().code(1);
    }
}

#[test]
fn pack_team_layout_errors_stay_at_the_item() {
    let env = Env::new();
    let source = installed_with_home_profile(&env);
    write(
        source.join("crew/teams/invalid.md"),
        "---\nleader: lead\nstages: [Work]\nlayout: missing\nroles:\n  - agent: coder\n    role: lead\n    owns: [Work]\n---\nTeam.",
    );
    env.rimz()
        .args(["agents", "explain", "other"])
        .assert()
        .success();
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    let fate = &item(&crew, "teams", "invalid")["fate"];
    assert_eq!(fate["state"], "failed", "{crew}");
    assert!(
        fate["message"]
            .as_str()
            .unwrap()
            .contains("rimz packs exclude crew:invalid --type teams"),
        "{crew}"
    );
    env.rimz().args(["agents", "validate"]).assert().code(1);
}

#[test]
fn home_team_profile_collisions_keep_the_set_failure() {
    for broken in [false, true] {
        let env = Env::new();
        let source = installed(&env);
        let team = fs::read_to_string(source.join("crew/teams/pair.md")).unwrap();
        write(
            env.agents_home().join("teams/coder.md"),
            if broken { "broken" } else { &team },
        );
        write(
            env.agents_home().join("agents/coder.md"),
            "---\ndescription: Home\nagent: pi\n---\nHome.",
        );
        env.rimz()
            .args(["agents", "explain", "pi"])
            .assert()
            .failure();
    }
}

#[test]
fn pack_contributor_traits_are_documentation_but_home_names_remain_valid() {
    let env = Env::new();
    let source = installed(&env);
    for name in ["README", "AGENTS", "CLAUDE"] {
        for pack in ["crew", "tools"] {
            write(
                source.join(pack).join("traits").join(format!("{name}.md")),
                "Documentation.",
            );
        }
        write(
            env.agents_home().join("traits").join(format!("{name}.md")),
            "Home trait.",
        );
    }
    write(
        env.agents_home().join("agents/good.md"),
        "---\ndescription: Good\nagent: pi\ntraits: [README, AGENTS, CLAUDE]\n---\n${traits}",
    );
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert!(
        !crew["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "traits" && item["name"] == "README"),
        "{crew}"
    );
    env.rimz().args(["agents", "validate"]).assert().success();
    env.rimz()
        .args(["agents", "explain", "good"])
        .assert()
        .success();
}

#[test]
fn dangling_library_skill_claims_without_breaking_unlisted_consumers() {
    let env = Env::new();
    installed(&env);
    fs::create_dir_all(env.agents_home().join("skills")).unwrap();
    symlink("missing", env.agents_home().join("skills/coder")).unwrap();
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert_eq!(
        item(&crew, "skills", "coder")["fate"]["state"],
        "overridden",
        "{crew}"
    );
    env.rimz().args(["agents", "validate"]).assert().success();
    env.rimz()
        .args(["agents", "explain", "coder"])
        .assert()
        .success();
    let chain = rimz::agents::skills::SkillChain::from_home(&env.agents_home());
    let provider = env.home_root.join(".claude/skills");
    fs::create_dir_all(&provider).unwrap();
    let pack = packs::resolve(&env.agents_home()).packs.remove(0);
    for desired in [
        rimz::agents::skill_links::Desired::Library,
        rimz::agents::skill_links::Desired::Prune,
    ] {
        symlink(pack.root.join("skills/coder"), provider.join("coder")).unwrap();
        let plan = rimz::agents::skill_links::plan(&provider, &chain, desired).unwrap();
        rimz::agents::skill_links::apply(&plan).unwrap();
        assert!(provider.join("coder").symlink_metadata().is_err());
    }
}

#[test]
fn listed_dangling_library_skill_names_the_home_path() {
    let env = Env::new();
    let source = installed(&env);
    write(
        env.agents_home().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nBase.",
    );
    write(
        source.join("crew/skills/review/SKILL.md"),
        "---\nname: review\n---\nReview.",
    );
    write(
        env.agents_home().join("agents/worker.md"),
        "---\ndescription: Worker\nagent: claude\nisolation: host\ntools: [Skill]\nskills: [review]\n---\n",
    );
    fs::create_dir_all(env.agents_home().join("skills")).unwrap();
    let missing = env.agents_home().join("skills/review");
    symlink("missing", &missing).unwrap();
    env.rimz()
        .args(["agents", "explain", "worker"])
        .assert()
        .failure()
        .stderr(contains(missing.display().to_string()));
    env.rimz()
        .args(["agents", "validate"])
        .assert()
        .code(1)
        .stdout(contains(missing.display().to_string()));
}

fn assert_pack_skill_error_isolated(problem: &str) {
    let env = Env::new();
    let source = installed(&env);
    write(source.join("crew/skills/healthy/SKILL.md"), "# Healthy");
    write(
        env.agents_home().join("skills/library/SKILL.md"),
        "# Library",
    );
    let root = source.join("crew/skills");
    match problem {
        "marker-loop" => {
            fs::remove_file(root.join("coder/SKILL.md")).unwrap();
            symlink("SKILL.md", root.join("coder/SKILL.md")).unwrap();
        }
        "directory-loop" => {
            fs::remove_dir_all(&root).unwrap();
            symlink("skills", &root).unwrap();
        }
        "not-directory" => {
            fs::remove_dir_all(&root).unwrap();
            fs::write(&root, "not a directory").unwrap();
        }
        _ => unreachable!(),
    }
    let chain = rimz::agents::skills::SkillChain::from_home(&env.agents_home());
    let provider = env.home_root.join(".claude/skills");
    let plan = rimz::agents::skill_links::plan(
        &provider,
        &chain,
        rimz::agents::skill_links::Desired::Library,
    );
    assert!(plan.is_ok(), "{problem} escaped its pack: {plan:?}");
    rimz::agents::skill_links::apply(&plan.unwrap()).unwrap();
    assert!(provider.join("library/SKILL.md").is_file());
    assert!(provider.join("extra/SKILL.md").is_file());
    assert!(!provider.join("healthy").exists());
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert_eq!(crew["counts"]["skills"], 0);
    assert_eq!(crew["problems"].as_array().unwrap().len(), 1, "{crew}");
    if problem == "marker-loop" {
        assert_eq!(
            item(&crew, "skills", "healthy")["fate"]["message"],
            "withheld: skill `coder` in this pack could not be read"
        );
        let message = crew["problems"][0]["message"].as_str().unwrap();
        assert!(message.contains("coder/SKILL.md"), "{message}");
        assert!(
            message.ends_with("; run `rimz packs exclude crew:coder --type skills`"),
            "{message}"
        );
        assert_eq!(item(&crew, "skills", "coder")["fate"]["message"], message);
    }
    let tools = json(&env, &["packs", "show", "tools", "--json"]);
    assert_eq!(tools["counts"]["skills"], 1);
    assert!(tools["problems"].as_array().unwrap().is_empty(), "{tools}");
    env.rimz()
        .args(["agents", "validate"])
        .assert()
        .code(1)
        .stdout(contains("crew/skills"));
    env.rimz()
        .args(["packs", "exclude", "crew", "--type", "skills"])
        .assert()
        .success();
    assert!(
        json(&env, &["packs", "show", "crew", "--json"])["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pack_skill_marker_errors_stay_in_their_pack() {
    assert_pack_skill_error_isolated("marker-loop");
}

#[test]
fn pack_skill_directory_errors_stay_in_their_pack() {
    assert_pack_skill_error_isolated("directory-loop");
}

#[test]
fn pack_skill_non_directory_errors_stay_in_their_pack() {
    assert_pack_skill_error_isolated("not-directory");
}

fn broken_skill(env: &Env) {
    let source = installed(env);
    let marker = source.join("crew/skills/coder/SKILL.md");
    fs::remove_file(&marker).unwrap();
    symlink("SKILL.md", marker).unwrap();
}

#[test]
fn excluding_a_broken_skill_type_completes_cleanup_without_reading_it() {
    let env = Env::new();
    broken_skill(&env);
    env.rimz()
        .args(["packs", "exclude", "crew", "--type", "skills"])
        .assert()
        .success();
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert!(crew["problems"].as_array().unwrap().is_empty(), "{crew}");
    assert_eq!(
        item(&crew, "skills", "coder")["fate"]["state"],
        "excluded_type"
    );
}

#[test]
fn excluding_a_broken_skill_item_completes_cleanup_without_reading_it() {
    let env = Env::new();
    broken_skill(&env);
    env.rimz()
        .args(["packs", "exclude", "crew:coder", "--type", "skills"])
        .assert()
        .success();
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert!(crew["problems"].as_array().unwrap().is_empty(), "{crew}");
    assert_eq!(item(&crew, "skills", "coder")["fate"]["state"], "excluded");
}

#[test]
fn overriding_a_broken_pack_skill_does_not_read_its_marker() {
    let env = Env::new();
    broken_skill(&env);
    write(env.agents_home().join("skills/coder/SKILL.md"), "# Home");
    let crew = json(&env, &["packs", "show", "crew", "--json"]);
    assert!(crew["problems"].as_array().unwrap().is_empty(), "{crew}");
    assert_eq!(
        item(&crew, "skills", "coder")["fate"]["state"],
        "overridden"
    );
}

#[test]
fn host_link_plans_skip_unreadable_library_markers() {
    let env = Env::new();
    let source = installed(&env);
    write(source.join("crew/skills/broken/SKILL.md"), "# Pack copy");
    write(
        env.agents_home().join("skills/healthy/SKILL.md"),
        "# Healthy",
    );
    let broken = env.agents_home().join("skills/broken");
    fs::create_dir_all(&broken).unwrap();
    symlink("SKILL.md", broken.join("SKILL.md")).unwrap();
    let chain = rimz::agents::skills::SkillChain::from_home(&env.agents_home());
    let provider = env.home_root.join(".claude/skills");
    fs::create_dir_all(&provider).unwrap();
    let pack = packs::resolve(&env.agents_home()).packs.remove(0);
    for marker_is_directory in [false, true] {
        if marker_is_directory {
            fs::remove_file(broken.join("SKILL.md")).unwrap();
            fs::create_dir(broken.join("SKILL.md")).unwrap();
        }
        for desired in [
            rimz::agents::skill_links::Desired::Library,
            rimz::agents::skill_links::Desired::Prune,
        ] {
            symlink(pack.root.join("skills/broken"), provider.join("broken")).unwrap();
            let plan = rimz::agents::skill_links::plan(&provider, &chain, desired);
            assert!(
                plan.is_ok(),
                "unreadable library marker aborted host links: {plan:?}"
            );
            rimz::agents::skill_links::apply(&plan.unwrap()).unwrap();
            assert!(provider.join("healthy/SKILL.md").is_file());
            assert!(
                provider.join("broken").symlink_metadata().is_err(),
                "an unreadable library entry must not reveal or retain the pack copy"
            );
        }
    }
}

#[test]
fn listed_library_marker_errors_keep_the_read_failure() {
    assert_listed_library_marker_error(false);
}

#[test]
fn listed_library_marker_directories_keep_the_read_failure() {
    assert_listed_library_marker_error(true);
}

fn assert_listed_library_marker_error(directory: bool) {
    let env = Env::new();
    write(
        env.agents_home().join("agents/worker.md"),
        "---\ndescription: Worker\nagent: claude\nisolation: host\ntools: [Skill]\nskills: [broken]\n---\nWork.",
    );
    let marker = env.agents_home().join("skills/broken/SKILL.md");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    if directory {
        fs::create_dir(&marker).unwrap();
    } else {
        symlink("SKILL.md", &marker).unwrap();
    }
    let read_error = fs::read_to_string(&marker).unwrap_err().to_string();
    env.rimz()
        .args(["agents", "explain", "worker"])
        .assert()
        .failure()
        .stderr(contains(marker.display().to_string()))
        .stderr(contains(&read_error));
    env.rimz()
        .args(["agents", "validate"])
        .assert()
        .code(1)
        .stdout(contains(marker.display().to_string()))
        .stdout(contains(&read_error));
}

#[test]
fn relative_agents_home_keeps_linked_pack_roots_absolute() {
    let env = Env::new();
    let source = source(&env);
    let cwd = env.project_root.join("inner");
    fs::create_dir(&cwd).unwrap();
    env.rimz()
        .current_dir(&cwd)
        .env("RIMZ_AGENTS_HOME", "../relative-home")
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success();
    let output = env
        .rimz()
        .current_dir(&cwd)
        .env("RIMZ_AGENTS_HOME", "../relative-home")
        .args(["packs", "show", "crew", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let crew: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(crew["problems"].as_array().unwrap().is_empty(), "{crew}");
    assert_eq!(item(&crew, "agents", "coder")["fate"]["state"], "loads");
    assert!(Path::new(item(&crew, "skills", "coder")["path"].as_str().unwrap()).is_absolute());
}

#[test]
fn linked_install_lists_refreshes_and_removes_in_record_order() {
    let env = Env::new();
    let source = source(&env);
    env.rimz()
        .args(["packs"])
        .assert()
        .success()
        .stdout(contains("No packs installed"))
        .stdout(contains("rimz packs install"));
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success()
        .stdout(contains("crew"))
        .stdout(contains("tools"))
        .stdout(contains("linked"))
        .stdout(contains("rimz packs show crew"))
        .stderr(contains("future"));
    let bare = json(&env, &["packs", "--json"]);
    assert_eq!(bare, json(&env, &["packs", "list", "--json"]));
    let rows = bare["packs"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["crew", "tools"]
    );
    assert_eq!(rows[0]["description"], "The crew");
    assert_eq!(rows[0]["source"], source.to_str().unwrap());
    assert_eq!(rows[0]["linked"], true);
    assert_eq!(rows[0]["entry"], "source");
    assert_eq!(rows[0]["counts"]["skills"], 1);
    assert!(rows[0]["problems"].as_array().unwrap().is_empty());
    assert_eq!(
        item(
            &json(&env, &["packs", "show", "crew", "--json"]),
            "agents",
            "coder"
        )["fate"]["state"],
        "loads"
    );
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success()
        .stdout(contains("Already up to date"));
    env.rimz()
        .args(["packs", "exclude", "crew:coder", "--type", "skills"])
        .assert()
        .success();
    write(
        source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Edited crew'\npath = 'crew'\n\
         [[pack]]\nname = 'new'\ndescription = 'New pack'\npath = 'tools'\n",
    );
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .assert()
        .success()
        .stdout(contains("added: new"))
        .stdout(contains("dropped: tools"));
    let show = json(&env, &["packs", "show", "crew", "--json"]);
    assert_eq!(show["description"], "Edited crew");
    assert_eq!(item(&show, "skills", "coder")["fate"]["state"], "excluded");
    for pack in ["crew", "new"] {
        env.rimz()
            .args(["packs", "remove", pack])
            .assert()
            .success()
            .stdout(contains(pack));
    }
    assert!(!env.agents_home().join("packs/source").is_symlink());
    assert!(source.join("crew/agents/coder.md").is_file());
    assert!(
        json(&env, &["packs", "--json"])["packs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn install_refusals_have_the_documented_codes_and_fixes() {
    let env = Env::new();
    let source = installed(&env);
    env.rimz()
        .args(["packs", "install", "github:crew"])
        .assert()
        .code(2)
        .stderr(contains("local path"))
        .stderr(contains("./"));
    env.rimz()
        .args(["packs", "install", "/no-such-rimz-pack-source"])
        .assert()
        .code(1);
    let missing = env.home_root.join("no-manifest");
    fs::create_dir(&missing).unwrap();
    env.rimz()
        .args(["packs", "install"])
        .arg(&missing)
        .assert()
        .code(1)
        .stderr(contains("rimz-packs.toml"))
        .stderr(contains("[[pack]]"));
    write(missing.join("rimz-packs.toml"), "[[pack]]\nname = 5\n");
    env.rimz()
        .args(["packs", "install"])
        .arg(&missing)
        .assert()
        .code(1)
        .stderr(contains("rimz-packs.toml"))
        .stderr(contains("[[pack]]"));
    env.rimz()
        .args(["packs", "install"])
        .arg(env.agents_home())
        .assert()
        .code(1)
        .stderr(contains("overlaps agents home"));
    write(
        missing.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Collision'\npath = '.'\n",
    );
    env.rimz()
        .args(["packs", "install"])
        .arg(&missing)
        .assert()
        .code(1)
        .stderr(contains(source.to_str().unwrap()))
        .stderr(contains("rimz packs remove crew"));
    for verb in ["show", "remove", "exclude", "include"] {
        let mut command = env.rimz();
        command.args(["packs", verb, "unknown"]);
        if matches!(verb, "exclude" | "include") {
            command.args(["--type", "skills"]);
        }
        command
            .assert()
            .code(1)
            .stderr(contains("installed packs: crew, tools"));
    }
}

#[test]
fn exclusions_match_names_types_and_recorded_missing_items() {
    let env = Env::new();
    let source = installed(&env);
    env.rimz()
        .args(["packs", "exclude", "crew:coder"])
        .assert()
        .code(2)
        .stderr(contains("crew:coder --type agents"))
        .stderr(contains("crew:coder --type skills"));
    for verb in ["exclude", "include"] {
        env.rimz()
            .args(["packs", verb, "crew"])
            .assert()
            .code(2)
            .stderr(contains("rimz packs remove crew"));
        env.rimz()
            .args(["packs", verb, "crew:missing"])
            .assert()
            .code(1);
        env.rimz()
            .args(["packs", verb, "crew", "--type", "bogus"])
            .assert()
            .code(2);
    }
    for (name, kind) in [
        ("coder", "agents"),
        ("helper", "subagents"),
        ("pair", "teams"),
        ("craft", "traits"),
        ("coder", "skills"),
    ] {
        env.rimz()
            .args(["packs", "exclude", &format!("crew:{name}"), "--type", kind])
            .assert()
            .success();
        env.rimz()
            .args(["packs", "exclude", &format!("crew:{name}"), "--type", kind])
            .assert()
            .success()
            .stdout(contains("already"));
        env.rimz()
            .args(["packs", "include", &format!("crew:{name}"), "--type", kind])
            .assert()
            .success();
        env.rimz()
            .args(["packs", "include", &format!("crew:{name}"), "--type", kind])
            .assert()
            .success()
            .stdout(contains("already"));
    }
    for kind in ["agents", "subagents", "teams", "traits", "skills"] {
        env.rimz()
            .args(["packs", "exclude", "crew", "--type", kind])
            .assert()
            .success();
        env.rimz()
            .args(["packs", "exclude", "crew", "--type", kind])
            .assert()
            .success()
            .stdout(contains("already"));
        if kind == "skills" {
            env.rimz()
                .args(["packs", "include", "crew:coder", "--type", kind])
                .assert()
                .code(1)
                .stderr(contains("rimz packs include crew --type skills"));
        }
        env.rimz()
            .args(["packs", "include", "crew", "--type", kind])
            .assert()
            .success();
    }
    env.rimz()
        .args(["packs", "exclude", "tools:extra"])
        .assert()
        .success();
    fs::remove_dir_all(source.join("tools/skills/extra")).unwrap();
    let show = json(&env, &["packs", "show", "tools", "--json"]);
    assert_eq!(
        show["unshipped_exclusions"][0],
        serde_json::json!({"type":"skills", "name":"extra"})
    );
    env.rimz()
        .args(["packs", "include", "tools:extra"])
        .assert()
        .success();
}

#[test]
fn show_reports_definition_and_skill_fates_and_broken_recorded_packs() {
    let env = Env::new();
    let source = installed(&env);
    write(
        env.agents_home().join("agents/coder.md"),
        "---\ndescription: Home\nagent: pi\n---\n",
    );
    write(
        source.join("crew/agents/broken.md"),
        "---\nagent: unknown\n---\n",
    );
    write(source.join("tools/traits/craft.md"), "Conflicting craft.");
    write(
        source.join("tools/skills/coder/SKILL.md"),
        "Conflicting coder.",
    );
    packs::exclude(
        &env.agents_home(),
        &"crew".parse().unwrap(),
        &ExclusionTarget::Type(ItemType::Subagents),
    )
    .unwrap();
    packs::exclude(
        &env.agents_home(),
        &"crew".parse().unwrap(),
        &ExclusionTarget::Item(ItemKey {
            kind: ItemType::Agents,
            name: "pi".into(),
        }),
    )
    .unwrap();
    let show = json(&env, &["packs", "show", "crew", "--json"]);
    for (kind, name, fate) in [
        ("agents", "coder", "overridden"),
        ("agents", "broken", "failed"),
        ("agents", "pi", "excluded"),
        ("subagents", "helper", "excluded_type"),
        ("traits", "craft", "conflict"),
        ("skills", "coder", "conflict"),
    ] {
        assert_eq!(item(&show, kind, name)["fate"]["state"], fate, "{show}");
    }
    env.rimz()
        .args(["packs", "show", "crew"])
        .assert()
        .success()
        .stdout(contains("overridden by"))
        .stdout(contains("excluded (type)"))
        .stdout(contains("conflicts with tools"))
        .stdout(contains("failed:"));
    env.rimz()
        .args(["packs", "exclude", "tools:craft"])
        .assert()
        .success()
        .stdout(contains("conflict resolved"));
    env.rimz()
        .args(["packs", "include", "tools:craft"])
        .assert()
        .success()
        .stdout(contains("conflict opened"));
    fs::remove_file(source.join("rimz-packs.toml")).unwrap();
    let listing = json(&env, &["packs", "--json"]);
    assert_eq!(listing["packs"].as_array().unwrap().len(), 2);
    assert!(
        !listing["packs"][0]["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    env.rimz()
        .args(["packs", "show", "crew"])
        .assert()
        .success()
        .stdout(contains("rimz-packs.toml"));
}

#[test]
fn every_mutation_prunes_actual_links_in_native_and_named_roots_without_adding() {
    for verb in ["install", "remove", "exclude", "include"] {
        let env = Env::new();
        let source = installed(&env);
        let target = ExclusionTarget::Item(ItemKey {
            kind: ItemType::Skills,
            name: "coder".into(),
        });
        if verb == "include" {
            packs::exclude(&env.agents_home(), &"crew".parse().unwrap(), &target).unwrap();
        }
        let account = env.home_root.join("account");
        write(
            env.rimz_home().join("config.toml"),
            &format!(
                "[accounts.claude.work]\nhome = {:?}\n",
                account.to_str().unwrap()
            ),
        );
        let roots = [
            env.home_root.join(".claude/skills"),
            env.home_root.join(".agents/skills"),
            account.join("skills"),
        ];
        for root in &roots {
            fs::create_dir_all(root).unwrap();
            symlink(
                env.agents_home().join("packs/source/crew/skills/coder"),
                root.join("coder"),
            )
            .unwrap();
            symlink(
                env.agents_home().join("packs/removed/skills/stale"),
                root.join("stale"),
            )
            .unwrap();
            symlink("/foreign/skill", root.join("foreign")).unwrap();
        }
        let mut command = env.rimz();
        command.args(["packs", verb]);
        match verb {
            "install" => {
                command.arg(&source);
            }
            "remove" => {
                command.arg("crew");
            }
            _ => {
                command.args(["crew:coder", "--type", "skills"]);
            }
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{verb}: {output:?}");
        let receipt = String::from_utf8(output.stdout).unwrap();
        for root in &roots {
            assert!(!root.join("stale").is_symlink(), "{verb}: {root:?}");
            assert_eq!(
                root.join("coder").is_symlink(),
                matches!(verb, "install" | "include")
            );
            assert!(root.join("foreign").is_symlink());
            assert!(!root.join("extra").is_symlink());
            assert!(receipt.contains(root.to_str().unwrap()), "{receipt}");
            assert!(receipt.contains("stale"), "{receipt}");
        }
    }
}

#[test]
fn remove_receipt_survives_committed_entry_cleanup_failure() {
    let env = Env::new();
    installed(&env);
    packs::remove(&env.agents_home(), &"tools".parse().unwrap()).unwrap();
    let entry = env.agents_home().join("packs/source");
    fs::remove_file(&entry).unwrap();
    write(entry.join("unexpected"), "not an entry symlink");
    env.rimz()
        .args(["packs", "remove", "crew"])
        .assert()
        .success()
        .stdout(contains("Removed crew"))
        .stderr(contains(entry.to_str().unwrap()))
        .stderr(contains("ignored"))
        .stderr(contains("warning"));
    assert!(
        packs::Record::read(&env.agents_home())
            .unwrap()
            .sources
            .is_empty()
    );
    assert!(entry.join("unexpected").is_file());
}

#[test]
fn committed_mutation_prunes_even_when_the_receipt_pipe_is_closed() {
    let env = Env::new();
    installed(&env);
    let root = env.home_root.join(".claude/skills");
    fs::create_dir_all(&root).unwrap();
    symlink(
        env.agents_home().join("packs/gone/skills/stale"),
        root.join("stale"),
    )
    .unwrap();
    let mut child = env
        .rimz()
        .args(["packs", "exclude", "crew", "--type", "skills"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let _ = child.wait().unwrap();
    let record = packs::Record::read(&env.agents_home()).unwrap();
    assert!(
        record.sources[0].packs[0]
            .excluded
            .types
            .contains(&ItemType::Skills)
    );
    assert!(!root.join("stale").is_symlink());
}

#[test]
fn problems_belong_to_the_pack_that_supplies_the_failed_item() {
    let env = Env::new();
    let source = source(&env);
    write(
        source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n\
         [[pack]]\nname = 'tools'\ndescription = 'Tools'\npath = 'tools'\n",
    );
    write(
        source.join("agents/pi.md"),
        "---\ndescription: Pi base\n---\nBase.",
    );
    write(
        source.join("tools/agents/broken.md"),
        "---\nagent: unknown\n---\n",
    );
    packs::install(&env.agents_home(), &source).unwrap();
    let listing = json(&env, &["packs", "--json"]);
    assert!(
        listing["packs"][0]["problems"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{listing}"
    );
    assert!(
        !listing["packs"][1]["problems"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{listing}"
    );
}

#[test]
fn conflicts_belong_once_to_each_pack_with_matching_validation_messages() {
    let env = Env::new();
    let source = installed(&env);
    write(
        source.join("tools/agents/coder.md"),
        "---\ndescription: Tools coder\nagent: pi\n---\nCode.",
    );
    write(source.join("tools/skills/coder/SKILL.md"), "# Coder");
    let listing = json(&env, &["packs", "--json"]);
    assert_eq!(
        listing["packs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pack| pack["problems"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        [2, 2],
        "{listing}"
    );
    let output = env
        .rimz()
        .args(["agents", "validate", "--json"])
        .output()
        .unwrap();
    let validation: Value = serde_json::from_slice(&output.stdout).unwrap();
    for pack in ["crew", "tools"] {
        let show = json(&env, &["packs", "show", pack, "--json"]);
        let problems = show["problems"].as_array().unwrap();
        for kind in ["agents", "skills"] {
            let row = item(&show, kind, "coder");
            let path = PathBuf::from(row["path"].as_str().unwrap());
            let path = if kind == "skills" {
                path.join("SKILL.md")
            } else {
                path
            };
            let message = &row["fate"]["message"];
            assert_eq!(
                problems
                    .iter()
                    .filter(|problem| problem["path"] == path.to_str().unwrap()
                        && problem["message"] == *message)
                    .count(),
                1,
                "{show}"
            );
            assert_eq!(
                listing["problems"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|problem| problem["message"] == *message)
                    .count(),
                1,
                "{listing}"
            );
            assert!(
                validation["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|error| error["message"] == *message),
                "{validation}"
            );
        }
    }
}

#[test]
fn every_advertised_conflict_exclusion_runs_with_same_named_item_types() {
    for same_pack in [false, true] {
        let env = Env::new();
        let source = installed(&env);
        let other = if same_pack {
            "crew/subagents"
        } else {
            "tools/agents"
        };
        write(
            source.join(other).join("coder.md"),
            "---\ndescription: Other coder\nagent: pi\n---\nCode.",
        );
        write(source.join("tools/skills/coder/SKILL.md"), "# Coder");
        let show = json(&env, &["packs", "show", "crew", "--json"]);
        assert_eq!(show["problems"].as_array().unwrap().len(), 2, "{show}");
        let mut commands = Vec::new();
        for row in show["items"].as_array().unwrap() {
            assert_eq!(
                row.as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["fate", "name", "path", "type"],
                "{row}"
            );
            if row["fate"]["state"] != "conflict" {
                continue;
            }
            for claim in row["fate"]["claims"].as_array().unwrap() {
                assert_eq!(
                    claim
                        .as_object()
                        .unwrap()
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                    ["pack", "path"],
                    "{claim}"
                );
            }
            let message = row["fate"]["message"].as_str().unwrap();
            commands.extend(
                message
                    .split('`')
                    .filter(|part| part.starts_with("rimz packs exclude "))
                    .map(str::to_owned),
            );
        }
        assert!(!commands.is_empty(), "{show}");
        for command in commands {
            let argv = shlex::split(&command).unwrap();
            env.rimz().args(&argv[1..]).assert().success();
        }
    }
}

#[test]
fn packs_follow_explicit_agents_home_and_report_newer_records_without_failing_lists() {
    let env = Env::new();
    let source = source(&env);
    let home = env.home_root.join("separate-agents");
    env.rimz()
        .args(["packs", "install"])
        .arg(&source)
        .env("RIMZ_AGENTS_HOME", &home)
        .assert()
        .success();
    assert!(
        packs::Record::read(&env.agents_home())
            .unwrap()
            .sources
            .is_empty()
    );
    assert_eq!(
        json(&env, &["packs", "--json"])["packs"],
        serde_json::json!([])
    );
    write(
        env.agents_home().join("packs/installed.json"),
        "{\"version\":2}",
    );
    let listing = json(&env, &["packs", "--json"]);
    assert!(
        listing["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("newer RimZ")
    );
    env.rimz()
        .args(["packs"])
        .assert()
        .success()
        .stdout(contains("newer RimZ"));
}
