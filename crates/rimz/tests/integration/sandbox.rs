//! Agent mount-view coverage at the real exec boundary.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use predicates::str::contains;
use rimz::agents::ManualSkill;
use rimz::config::Isolation;
use rimz::harness::launch::ExecRequest;
use rimz::ids::AgentKind;
use rimz::sandbox::{SandboxErr, SandboxInputs, SkillInputs};

use crate::common::{
    CommandTimeoutExt, Env, exec_args, path_with_front, write_env_dump_shim, write_fake_login_shell,
};

#[expect(clippy::print_stderr, reason = "optional bubblewrap test dependency")]
fn available() -> bool {
    match rimz::sandbox::preflight(Isolation::Sandbox) {
        Ok(_) => true,
        Err(err) => {
            eprintln!("skipping bubblewrap execution: {err}");
            false
        }
    }
}

fn enable(env: &Env) {
    let path = env.config_root().join("rimz/agents.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "[agents]\nisolation = \"sandbox\"\n").unwrap();
}

fn environment(env: &Env) -> BTreeMap<String, String> {
    [
        ("HOME", env.home_root.clone()),
        ("XDG_CONFIG_HOME", env.config_root()),
        ("XDG_RUNTIME_DIR", env.runtime_root.clone()),
        ("XDG_STATE_HOME", env.state_root()),
        ("TMPDIR", "/tmp".into()),
    ]
    .into_iter()
    .map(|(key, path)| (key.to_owned(), path.display().to_string()))
    .collect()
}

fn skill_argv(
    env: &Env,
    vars: &BTreeMap<String, String>,
    skills: SkillInputs<'_>,
) -> Result<Vec<String>, SandboxErr> {
    let state = env.store();
    let prepared = rimz::sandbox::prepare(&SandboxInputs {
        env: vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().tmp_dir,
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        skills,
    })?;
    Ok(rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    ))
}

fn skill_bind_source(argv: &[String], target: &Path) -> PathBuf {
    let args = argv
        .windows(3)
        .find(|args| args[0] == "--ro-bind" && Path::new(&args[2]) == target)
        .unwrap();
    PathBuf::from(&args[1])
}

#[test]
fn sandbox_unconfigured_skills_without_library_add_no_mounts() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("native")).unwrap();
    // Even a shadowed library entry leaves the native view unchanged.
    let library = env.config_root().join("rimz/skills/native");
    for shadowed in [false, true] {
        if shadowed {
            std::fs::create_dir_all(&library).unwrap();
        }
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: None,
            },
        )
        .unwrap();
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "--tmpfs" || arg == "--ro-bind")
        );
        assert!(!env.store().paths().skills_dir.exists());
    }
}

#[test]
fn sandbox_merges_rimz_library_and_provider_root_wins() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("shared/SKILL.md"), "provider\n").unwrap();
    for xdg in [true, false] {
        let mut vars = environment(&env);
        let library = if xdg {
            env.config_root().join("rimz/skills")
        } else {
            vars.remove("XDG_CONFIG_HOME");
            env.home_root.join(".config/rimz/skills")
        };
        for name in ["shared", "library-only"] {
            std::fs::create_dir_all(library.join(name)).unwrap();
            std::fs::write(library.join(name).join("SKILL.md"), "library\n").unwrap();
        }
        let argv = skill_argv(
            &env,
            &vars,
            SkillInputs {
                kind: "claude",
                home: Some(root.clone()),
                manual: ManualSkill::Frontmatter,
                callable: None,
            },
        )
        .unwrap();
        assert_eq!(
            skill_bind_source(&argv, &root.join("shared")),
            root.join("shared").canonicalize().unwrap()
        );
        assert_eq!(
            skill_bind_source(&argv, &root.join("library-only")),
            library.join("library-only").canonicalize().unwrap()
        );
        assert_eq!(
            argv.windows(2).filter(|args| args[0] == "--tmpfs").count(),
            1
        );
        assert!(
            argv.windows(2)
                .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
        );
        assert!(!root.join("library-only").exists());
        assert!(!env.store().paths().skills_dir.exists());
    }
}

#[test]
fn sandbox_empty_skill_list_makes_every_skill_manual() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    let library = env.config_root().join("rimz/skills");
    for (source, name) in [(&root, "native"), (&library, "library-only")] {
        std::fs::create_dir_all(source.join(name)).unwrap();
        std::fs::write(source.join(name).join("SKILL.md"), format!("{name}\n")).unwrap();
    }
    let argv = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&[]),
        },
    )
    .unwrap();
    for (source, name) in [(&root, "native"), (&library, "library-only")] {
        let copy = skill_bind_source(&argv, &root.join(name));
        assert!(copy.starts_with(&env.store().paths().skills_dir));
        assert_eq!(
            std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
            format!("---\ndisable-model-invocation: true\n---\n{name}\n")
        );
        assert_eq!(
            std::fs::read_to_string(source.join(name).join("SKILL.md")).unwrap(),
            format!("{name}\n")
        );
    }
}

#[test]
fn sandbox_manual_copies_are_content_addressed() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    let source = root.join("manual");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(source.join("SKILL.md"), "manual body\n").unwrap();
    std::fs::write(source.join("scripts/run.sh"), "echo original\n").unwrap();
    std::fs::set_permissions(
        source.join("scripts/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let companion = env.home_root.join("companion.bin");
    std::fs::write(&companion, [0, 0xff, 1]).unwrap();
    std::os::unix::fs::symlink(&companion, source.join("companion.bin")).unwrap();
    std::os::unix::fs::symlink("missing", source.join("broken")).unwrap();
    let prepare = || {
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "claude",
                home: Some(root.clone()),
                manual: ManualSkill::Frontmatter,
                callable: Some(&[]),
            },
        )
        .unwrap();
        skill_bind_source(&argv, &source)
    };
    let barrier = std::sync::Barrier::new(4);
    let first = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    prepare()
                })
            })
            .collect();
        let copies: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(copies.iter().all(|copy| copy == &copies[0]));
        copies[0].clone()
    });
    assert_eq!(first, prepare());
    let state = env.store();
    assert_eq!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read(first.join("companion.bin")).unwrap(),
        [0, 0xff, 1]
    );
    assert!(!first.join("companion.bin").is_symlink());
    assert!(!first.join("broken").exists());
    assert_eq!(
        std::fs::metadata(first.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::read_to_string(first.join("scripts/run.sh")).unwrap(),
        "echo original\n"
    );
    std::fs::write(source.join("scripts/run.sh"), "echo edited\n").unwrap();
    let second = prepare();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(first.join("scripts/run.sh")).unwrap(),
        "echo original\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("scripts/run.sh")).unwrap(),
        "echo edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("SKILL.md")).unwrap(),
        "manual body\n"
    );
    assert!(source.join("companion.bin").is_symlink());
    assert!(source.join("broken").is_symlink());
    assert_eq!(std::fs::read(companion).unwrap(), [0, 0xff, 1]);
}

#[test]
fn sandbox_codex_manual_writes_openai_policy() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let metadata =
        "interface:\n  display_name: Keep me\npolicy:\n  allow_implicit_invocation: true\n";
    for name in ["existing", "missing", "callable"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("SKILL.md"), "codex body\n").unwrap();
    }
    std::fs::create_dir_all(root.join("existing/agents")).unwrap();
    std::fs::write(root.join("existing/agents/openai.yaml"), metadata).unwrap();
    let argv = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&["callable".parse().unwrap()]),
        },
    )
    .unwrap();
    for name in ["existing", "missing"] {
        let copy = skill_bind_source(&argv, &root.join(name));
        let expected = if name == "existing" {
            metadata.replace("true", "false")
        } else {
            "policy:\n  allow_implicit_invocation: false\n".to_owned()
        };
        assert_eq!(
            std::fs::read_to_string(copy.join("agents/openai.yaml")).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
            "codex body\n"
        );
    }
    assert_eq!(
        skill_bind_source(&argv, &root.join("callable")),
        root.join("callable").canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("existing/agents/openai.yaml")).unwrap(),
        metadata
    );
    assert!(!root.join("missing/agents").exists());
    assert!(!root.join("callable/agents").exists());
}

#[test]
fn sandbox_unsupported_provider_refuses_skill_list() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    for installed in [false, true] {
        if installed {
            std::fs::create_dir_all(root.join("native")).unwrap();
        }
        let callable = ["native".parse().unwrap()];
        for list in [&[][..], &callable[..]] {
            let err = skill_argv(
                &env,
                &environment(&env),
                SkillInputs {
                    kind: "amp",
                    home: Some(root.clone()),
                    manual: ManualSkill::Unsupported,
                    callable: Some(list),
                },
            )
            .unwrap_err();
            assert!(matches!(&err, SandboxErr::ManualSkillsUnsupported { kind } if kind == "amp"));
            assert!(err.to_string().ends_with("remove the profile skills list"));
            assert!(!env.store().paths().tmp_dir.exists());
            assert!(!env.store().paths().skills_dir.exists());
        }
    }
    let err = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "plugin",
            home: None,
            manual: ManualSkill::Unsupported,
            callable: Some(&[]),
        },
    )
    .unwrap_err();
    assert!(matches!(err, SandboxErr::SkillsNeedRoot { .. }));
}

#[test]
fn sandbox_prepare_resolves_symlinked_skill_sources() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let source = env.home_root.join("library/visible");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(root.join("hidden")).unwrap();
    std::fs::write(root.join("hidden/SKILL.md"), "manual body\n").unwrap();
    std::os::unix::fs::symlink(&source, root.join("visible")).unwrap();
    std::os::unix::fs::symlink("missing", root.join("broken")).unwrap();
    let mut vars = environment(&env);
    vars.insert(
        "CLAUDE_CONFIG_DIR".to_owned(),
        env.home_root.join(".claude").display().to_string(),
    );
    let callable = ["visible".parse().unwrap()];
    let state = env.store();
    let mut inputs = SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().tmp_dir,
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &["CODEX_HOME"],
        skills: SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&callable),
        },
    };
    let plan = rimz::sandbox::prepare(&inputs).unwrap();
    assert_eq!(plan.pins["CODEX_HOME"], rimz::sandbox::EnvPin::Unset);
    assert!(!plan.pins.contains_key("CLAUDE_CONFIG_DIR"));
    assert_eq!(
        plan.pins["HOME"],
        rimz::sandbox::EnvPin::Set(env.home_root.display().to_string())
    );
    assert_eq!(
        plan.pins["TMPDIR"],
        rimz::sandbox::EnvPin::Set("/tmp".to_owned())
    );
    assert!(!plan.pins.contains_key("PATH"));
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &plan.plan,
        inputs.cwd,
        &["true".into()],
    );
    assert!(argv.windows(3).any(|args| args
        == [
            "--ro-bind",
            source.to_str().unwrap(),
            root.join("visible").to_str().unwrap()
        ]));
    assert!(argv.iter().any(|arg| arg.ends_with("/hidden")));
    let copy = skill_bind_source(&argv, &root.join("hidden"));
    assert!(copy.starts_with(&state.paths().skills_dir));
    assert_eq!(
        std::fs::read_to_string(copy.join("agents/openai.yaml")).unwrap(),
        "policy:\n  allow_implicit_invocation: false\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hidden/SKILL.md")).unwrap(),
        "manual body\n"
    );
    assert!(!root.join("hidden/agents").exists());
    assert!(!argv.iter().any(|arg| arg.ends_with("/broken")));
    let unknown = ["absent".parse().unwrap()];
    inputs.skills.callable = Some(&unknown);
    let err = rimz::sandbox::prepare(&inputs).err().unwrap();
    assert!(matches!(err, SandboxErr::UnknownSkill { .. }));
    assert!(err.to_string().contains(root.to_str().unwrap()));
}

#[test]
fn sandbox_prepare_rebinds_tmp_rooted_runtime() {
    let env = Env::new();
    let vars = environment(&env);
    let state = env.store();
    let mut inputs = SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().tmp_dir,
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        skills: SkillInputs {
            kind: "codex",
            home: Some(env.home_root.join(".agents/skills")),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    };
    let plan = rimz::sandbox::prepare(&inputs).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, inputs.cwd, &[]);
    assert!(argv.windows(3).any(|args| args
        == [
            "--bind",
            env.runtime_root.to_str().unwrap(),
            env.runtime_root.to_str().unwrap()
        ]));
    inputs.cwd = Path::new("/tmp");
    assert!(matches!(
        rimz::sandbox::prepare(&inputs),
        Err(SandboxErr::TmpCollision)
    ));
}

#[test]
fn sandboxed_exec_shows_profile_skill_view_and_room_tmp() {
    if !available() {
        return;
    }
    let env = Env::new();
    enable(&env);
    for dir in [
        ".claude/skills/a",
        ".agents/skills/b",
        ".agents/skills/c",
        ".codex",
    ] {
        std::fs::create_dir_all(env.home_root.join(dir)).unwrap();
    }
    std::fs::write(env.home_root.join(".codex/config.toml"), "sandbox-test").unwrap();
    std::os::unix::fs::symlink(
        env.home_root.join(".agents/skills/b"),
        env.home_root.join(".claude/skills/b"),
    )
    .unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        r#"#!/bin/sh
set -eu
ls "$HOME/.claude/skills" > /tmp/claude-skills
ls "$HOME/.agents/skills" > /tmp/agent-skills
printf '%s\n' "$TMPDIR" > /tmp/tmpdir
printf '%s\n' "$HOME" > /tmp/provider-home
printf '%s' "$RIMZ_TEST_NON_UTF8" > /tmp/non-utf8
test "$CLAUDE_CONFIG_DIR" = "$HOME/elsewhere"
test "${CODEX_HOME+x}" != x
test ! -e "$HOME/.agents/skills/b/agents/openai.yaml"
test "$(cat "$HOME/.agents/skills/c/agents/openai.yaml")" = 'policy:
  allow_implicit_invocation: false'
test -d "$XDG_RUNTIME_DIR/rimz/$RIMZ_TEST_WORKSPACE_ID"
test -c /dev/null
test "$(cat "$HOME/.codex/config.toml")" = sandbox-test
if touch "$HOME/.agents/skills/b/changed" 2>/dev/null; then exit 1; fi
test ! -e "$RIMZ_TEST_HOST_TMP_FILE"
printf '%s\n' shared > /tmp/team-file
"#,
    )
    .unwrap();
    let shell = write_fake_login_shell(&env, "sandbox-shell", &[("TMPDIR", "wrong-shell-tmp")]);
    let shell_body = std::fs::read_to_string(&shell).unwrap();
    std::fs::write(&shell, shell_body.replacen("#!/bin/sh\n", "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=$HOME/elsewhere\nexport CODEX_HOME=$HOME/elsewhere\nexport XDG_RUNTIME_DIR=/tmp/wrong-runtime\nexport HOME=/tmp/evil\n", 1)).unwrap();
    let host_tmp = tempfile::NamedTempFile::new_in("/tmp").unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.skills = Some(vec!["b".parse().unwrap()]);
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env("SHELL", shell)
        .env("RIMZ_TEST_HOST_TMP_FILE", host_tmp.path())
        .env("RIMZ_TEST_WORKSPACE_ID", env.workspace_id.as_str())
        .env(
            "RIMZ_TEST_NON_UTF8",
            std::ffi::OsString::from_vec(vec![0xff, 0xfe]),
        )
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "sandbox skill view: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = env.store();
    let tmp = &store.paths().tmp_dir;
    assert_eq!(std::fs::read(tmp.join("non-utf8")).unwrap(), [0xff, 0xfe]);
    assert_eq!(
        std::fs::read_to_string(tmp.join("claude-skills")).unwrap(),
        "a\nb\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.join("agent-skills")).unwrap(),
        "b\nc\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.join("tmpdir")).unwrap(),
        "/tmp\n"
    );
    assert!(env.home_root.join(".agents/skills/c").is_dir());
    assert!(!env.home_root.join(".agents/skills/c/agents").exists());
    assert!(env.home_root.join(".claude/skills/b").is_symlink());
    assert_eq!(
        std::fs::read_to_string(tmp.join("provider-home"))
            .unwrap()
            .trim(),
        env.home_root.to_str().unwrap()
    );

    std::fs::write(shim_dir.join("codex"), "#!/bin/sh\nset -eu\ntest \"$(cat /tmp/team-file)\" = shared\nprintf child > /tmp/child-file\n").unwrap();
    request.subagent = true;
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox subagent shared tmp");
    assert_eq!(
        std::fs::read_to_string(tmp.join("child-file")).unwrap(),
        "child"
    );
}

#[test]
fn sandboxed_exec_uses_probed_bwrap_with_trusted_path() {
    if !available() {
        return;
    }
    let env = Env::new();
    enable(&env);
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nprintf '%s' \"$PATH\" > /tmp/provider-path\n",
    )
    .unwrap();
    env.write_config(
        &env.project_root,
        &format!(
            "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
            shim_dir.to_str().unwrap()
        ),
    );
    env.rimz()
        .args(["trust", "grant"])
        .assert_success_within_timeout("grant trusted provider PATH");
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    env.rimz()
        .args(exec_args(&env, &request))
        .env("SHELL", "/definitely/not/a/shell")
        .assert_success_within_timeout("sandbox with provider-only PATH");
    assert_eq!(
        std::fs::read_to_string(env.store().paths().tmp_dir.join("provider-path")).unwrap(),
        shim_dir.to_str().unwrap()
    );
}

#[test]
fn sandbox_skills_under_host_refuse_before_provider_exec() {
    use assert_cmd::assert::OutputAssertExt;
    let env = Env::new();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    for skills in [vec!["merge".parse().unwrap()], vec![]] {
        request.skills = Some(skills);
        env.rimz()
            .args(exec_args(&env, &request))
            .assert()
            .failure()
            .stderr(contains(
                "a profile skills list needs agents.isolation = \"sandbox\"",
            ));
        assert!(!env.store().paths().tmp_dir.exists());
    }
}

#[test]
fn sandbox_skill_root_symlink_keeps_its_manual_view() {
    if !available() {
        return;
    }
    let env = Env::new();
    let real = env.home_root.join("skill-library");
    for name in ["visible", "manual"] {
        std::fs::create_dir_all(real.join(name)).unwrap();
        std::fs::write(real.join(name).join("SKILL.md"), "skill body\n").unwrap();
    }
    std::fs::create_dir_all(env.home_root.join(".agents")).unwrap();
    let root = env.home_root.join(".agents/skills");
    std::os::unix::fs::symlink(&real, &root).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let plan = rimz::sandbox::prepare(&SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().tmp_dir,
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        skills: SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&["visible".parse().unwrap()]),
        },
    })
    .unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Sandbox)
            .unwrap()
            .unwrap(),
        &plan.plan,
        &env.project_root,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "test \"$(cat \"$1/visible/SKILL.md\")\" = 'skill body' && test \"$(cat \"$1/manual/SKILL.md\")\" = '---\ndisable-model-invocation: true\n---\nskill body'".into(),
            "skill-view".into(),
            root.display().to_string(),
        ],
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(&vars)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.is_symlink());
    assert_eq!(
        std::fs::read_to_string(real.join("manual/SKILL.md")).unwrap(),
        "skill body\n"
    );
}

#[test]
fn sandboxed_run_timeout_stops_provider() {
    use rimz::store::run::{RunRecord, RunStatus};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    if !available() {
        return;
    }
    let env = Env::new();
    enable(&env);
    env.record(&env.project_root);
    let store = env.store();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "timeout task".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Running;
    run.subagent = true;
    run.keep = true;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nprintf '%s' \"$$\" > /tmp/provider-pid\nexec /usr/bin/sleep 60\n",
    )
    .unwrap();
    let mut request = ExecRequest::bare_launch(run.kind.clone(), Vec::new());
    request.run_id = Some(run.run_id.clone());
    request.exit_on_run_completion = true;
    let mut child = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid_path = store.paths().tmp_dir.join("provider-pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let provider = loop {
        run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        if let Some(provider) = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|text| text.parse::<u32>().ok())
            && run.provider_pid.is_some()
        {
            break provider;
        }
        assert!(Instant::now() < deadline, "provider did not become ready");
        assert!(
            child.try_wait().unwrap().is_none(),
            "wrapper exited before provider startup"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let start = rimz::proc::process_start_token(provider).unwrap();
    assert_ne!(
        run.provider_pid,
        Some(provider),
        "supervisor records the bubblewrap wrapper, not the provider child"
    );
    run.deadline_at = Some(jiff::Timestamp::now() - Duration::from_secs(1));
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let timeout = rimz::harness::run_timeout::RunTimeoutRequest {
        workspace_id: env.workspace_id.clone(),
        run_id: run.run_id.clone(),
    };
    env.rimz()
        .args(rimz::child_process::agent_helper_argv(
            "run-timeout",
            &timeout,
        ))
        .assert_success_within_timeout("sandbox provider timeout");
    let deadline = Instant::now() + Duration::from_secs(5);
    while rimz::proc::process_is_live(provider, Some(&start)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let live = rimz::proc::process_is_live(provider, Some(&start));
    let _ = child.kill();
    let _ = child.wait();
    assert!(!live, "provider survived timeout of its bubblewrap parent");
    assert_eq!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .status,
        RunStatus::TimedOut
    );
}
