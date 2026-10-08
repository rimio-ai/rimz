//! Agent mount-view coverage at the real exec boundary.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use rimz::agents::ManualSkill;
use rimz::config::Isolation;
use rimz::harness::launch::ExecRequest;
use rimz::ids::AgentKind;
use rimz::sandbox::{SandboxErr, SandboxInputs, SandboxPlan, SkillInputs, SkipReason};

use crate::common::{
    CommandTimeoutExt, Env, exec_args, path_with_front, write_env_dump_shim, write_fake_login_shell,
};

fn available() -> bool {
    match rimz::sandbox::preflight(Isolation::Sandbox) {
        Ok(_) => true,
        Err(err) => {
            crate::common::skip(&format!("bubblewrap unusable: {err}"));
            false
        }
    }
}

fn enable(env: &Env) {
    let path = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "[agents]\nisolation = \"sandbox\"\n").unwrap();
}

fn child_cap_launch(explicit_host: bool) {
    use rimz::agents::LaunchParams;
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    if !explicit_host && !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::write_definition(
        &env,
        "subagents",
        "sysadmin",
        "description: Host worker\nagent: claude\ntools: []\nisolation: host",
        "Work.",
    );
    env.install_agent_hooks("claude");
    let store = env.store();
    // The caller's `/tmp` is its own temp unit.
    let cwd = store.paths().temp_unit_dir(Some("parent")).join("clean");
    std::fs::create_dir_all(&cwd).unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: "parent-session".into(),
                launch_id: Some("parent-launch".into()),
                agent_name: "parent".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams::default(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(rimz::ids::PaneId::from_parts(
                    rimz::ids::MuxName::Zellij,
                    "terminal_2",
                )),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    crate::common::room::seed_live_zellij_room(
        store.runtime_paths(),
        &workspace.session_name,
        serde_json::from_value(serde_json::json!([
            {"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},
            {"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}
        ]))
        .unwrap(),
    );
    let shim = crate::common::write_failing_agent_shim(&env, "claude", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let presence = env.project_root.join("presence.wasm");
    std::fs::write(&presence, b"test-presence").unwrap();
    let mut command = env.rimz();
    command.args(["--mux", "zellij", "subagents", "sysadmin", "work"]);
    if explicit_host {
        command.args(["--isolation", "host"]);
    } else {
        command.args(["--cwd", "/tmp/clean"]);
    }
    command
        .env("RIMZ_ISOLATION", "sandbox")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "parent-launch")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "parent")
        .env("SHELL", shell).env("PATH", path_with_front(&shim))
        .env("RIMZ_ZELLIJ_BIN", crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")))
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("mux.log"))
        .env("RIMZ_PRESENCE_PLUGIN", presence).env("ZELLIJ_PANE_ID", "2")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", format!("{} [Created 1s ago]\n", workspace.session_name))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#);
    let launched = std::sync::atomic::AtomicBool::new(false);
    let trace = env.project_root.join("mux.log");
    let output = std::thread::scope(|scope| {
        scope.spawn(|| {
            if explicit_host {
                return; // A sandboxed parent cannot launch a host child.
            }
            crate::common::room::bind_child_panes(
                &store,
                &trace,
                &workspace.session_name,
                &launched,
            );
        });
        let output = command.bounded_output();
        launched.store(true, std::sync::atomic::Ordering::Relaxed);
        output
    })
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if explicit_host {
        assert!(!output.status.success(), "{stderr}");
        assert!(
            stderr.contains("launch it from a host agent or a host shell"),
            "{stderr}"
        );
        assert_eq!(store.read_events().unwrap().len(), 1);
    } else {
        assert!(output.status.success(), "{stderr}");
        assert!(
            stderr.contains("sysadmin runs sandboxed: its definition asks for host isolation"),
            "{stderr}"
        );
        let agents = store.snapshot().unwrap().agents;
        let child = agents
            .iter()
            .find(|agent| agent.profile.as_deref() == Some("sysadmin"))
            .unwrap();
        assert_eq!(child.isolation, Some(Isolation::Sandbox));
        assert_eq!(child.worktree_path.as_deref(), cwd.to_str());
        let trace = std::fs::read_to_string(env.project_root.join("mux.log")).unwrap();
        assert!(
            trace.contains(&format!("\t--cwd\t{}\t", cwd.display())),
            "{trace}"
        );
    }
}

#[test]
fn sandboxed_parent_refuses_explicit_host_subagent_without_launch_event() {
    child_cap_launch(true);
}

#[test]
fn sandboxed_parent_clamps_host_profile_subagent_and_records_override() {
    child_cap_launch(false);
}

fn environment(env: &Env) -> BTreeMap<String, String> {
    [
        ("HOME", env.home_root.clone()),
        ("XDG_CONFIG_HOME", env.config_root()),
        ("XDG_RUNTIME_DIR", env.runtime_root.clone()),
        ("RIMZ_HOME", env.rimz_home()),
        ("XDG_STATE_HOME", env.state_root()),
        ("TMPDIR", "/tmp".into()),
    ]
    .into_iter()
    .map(|(key, path)| (key.to_owned(), path.display().to_string()))
    .collect()
}

fn skill_prepare(
    env: &Env,
    vars: &BTreeMap<String, String>,
    skills: SkillInputs<'_>,
) -> Result<SandboxPlan, SandboxErr> {
    let state = env.store();
    let plan = rimz::sandbox::plan(&SandboxInputs {
        env: vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills,
    })?;
    rimz::sandbox::apply(&plan)?;
    Ok(plan)
}

fn skill_argv(
    env: &Env,
    vars: &BTreeMap<String, String>,
    skills: SkillInputs<'_>,
) -> Result<Vec<String>, SandboxErr> {
    let prepared = skill_prepare(env, vars, skills)?;
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
    let library = env.rimz_home().join("skills/native");
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
fn sandbox_unconfigured_unreadable_skills_keep_native_launch_behavior() {
    let execute = available();
    for invalid_name in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        let root = env.home_root.join(".agents/skills");
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        if invalid_name {
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join(std::ffi::OsString::from_vec(vec![0xff])),
                "native",
            )
            .unwrap();
        } else {
            std::os::unix::fs::symlink("skills", &root).unwrap();
        }
        let vars = environment(&env);
        for callable in [None, Some(&[][..])] {
            let result = skill_argv(
                &env,
                &vars,
                SkillInputs {
                    kind: "codex",
                    home: Some(root.clone()),
                    manual: ManualSkill::OpenAiPolicy,
                    callable,
                },
            );
            if callable.is_some() {
                assert!(result.is_err());
            } else {
                assert!(
                    !result
                        .unwrap()
                        .iter()
                        .any(|arg| arg == "--tmpfs" || arg == "--ro-bind")
                );
            }
        }
        if !execute {
            continue;
        }
        enable(&env);
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\nprintf ran > /tmp/skill-probe\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        let probe = env.store().paths().temp_unit_dir(None).join("skill-probe");
        env.rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .assert_success_within_timeout("unconfigured native skill discovery");
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), "ran");
        std::fs::remove_file(&probe).unwrap();
        request.skills = Some(Vec::new());
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .bounded_output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !probe.exists(),
            "configured discovery errors must refuse before provider exec"
        );
    }
}

#[test]
fn sandbox_unusable_unlisted_skill_is_omitted_with_warning() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    for name in ["listed", "plain", "bad"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("SKILL.md"), "body\n").unwrap();
    }
    let metadata = "---\nname: bad\ndescription: &d x\nsummary: *d\n---\nbody\n";
    let bad = root.join("bad/SKILL.md");
    std::fs::write(&bad, metadata).unwrap();
    std::os::unix::fs::symlink("bad", root.join("alias")).unwrap();
    let library = env.rimz_home().join("skills");
    std::fs::create_dir_all(&library).unwrap();
    std::os::unix::fs::symlink(root.join("bad"), library.join("library-alias")).unwrap();
    std::fs::write(root.join("listed/SKILL.md"), metadata).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let prepared = rimz::sandbox::plan(&SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&["listed".parse().unwrap()]),
        },
    })
    .unwrap();
    assert!(!state.paths().skills_dir.exists());
    assert!(!state.paths().tmp_dir.exists());
    assert_eq!(prepared.skipped.len(), 3);
    for (skipped, name) in prepared
        .skipped
        .iter()
        .zip(["alias", "bad", "library-alias"])
    {
        assert_eq!(skipped.name, name);
        assert_eq!(skipped.path, bad);
        assert!(matches!(&skipped.reason, SkipReason::Metadata(_)));
        assert!(
            skipped
                .to_string()
                .contains(&format!("starting without skill {name:?}"))
        );
    }
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    );
    assert!(
        argv.windows(2)
            .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
    );
    assert_eq!(
        skill_bind_source(&argv, &root.join("listed")),
        root.join("listed").canonicalize().unwrap()
    );
    let copy = skill_bind_source(&argv, &root.join("plain"));
    assert!(copy.starts_with(&state.paths().skills_dir));
    assert!(!copy.starts_with(&state.paths().tmp_dir));
    assert!(!copy.exists());
    rimz::sandbox::apply(&prepared).unwrap();
    assert!(state.paths().tmp_dir.is_dir());
    assert_eq!(
        std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
        "---\ndisable-model-invocation: true\n---\nbody\n"
    );
    assert!(!argv.iter().any(|arg| Path::new(arg) == root.join("bad")));
    for name in ["alias", "library-alias"] {
        assert!(!argv.iter().any(|arg| Path::new(arg) == root.join(name)));
    }
    assert_eq!(std::fs::read(&bad).unwrap(), metadata.as_bytes());
    assert!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"."))
    );
}

#[test]
fn sandbox_unreadable_unlisted_skill_is_omitted_with_warning() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("bad/scripts")).unwrap();
    std::fs::write(root.join("bad/SKILL.md"), "body\n").unwrap();
    let unreadable = root.join("bad/scripts/run.sh");
    std::fs::write(&unreadable, "echo original\n").unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&unreadable).is_ok() {
        crate::common::skip("mode 000 files remain readable");
        return;
    }
    let vars = environment(&env);
    let prepared = skill_prepare(
        &env,
        &vars,
        SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&[]),
        },
    )
    .unwrap();
    assert_eq!(prepared.skipped.len(), 1);
    let skipped = &prepared.skipped[0];
    assert_eq!(skipped.name, "bad");
    assert_eq!(skipped.path, unreadable);
    assert!(matches!(&skipped.reason, SkipReason::Unreadable(_)));
    assert!(
        skipped
            .to_string()
            .contains("starting without skill \"bad\"")
    );
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    );
    assert!(
        argv.windows(2)
            .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
    );
    assert!(!argv.iter().any(|arg| Path::new(arg) == root.join("bad")));
}

#[test]
fn sandbox_skills_state_failure_still_refuses() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("plain")).unwrap();
    std::fs::write(root.join("plain/SKILL.md"), "body\n").unwrap();
    let state = env.store();
    std::fs::create_dir_all(state.paths().skills_dir.parent().unwrap()).unwrap();
    std::fs::write(&state.paths().skills_dir, "not a directory").unwrap();
    assert!(
        skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "claude",
                home: Some(root),
                manual: ManualSkill::Frontmatter,
                callable: Some(&[]),
            },
        )
        .is_err()
    );
}

#[test]
fn sandboxed_exec_merges_library_without_native_skill_root() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let root = env.home_root.join(".agents/skills");
    let library = env.rimz_home().join("skills/library-only");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(library.join("SKILL.md"), "library body").unwrap();
    assert!(!root.exists());
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        r#"#!/bin/sh
set -eu
test "$(cat "$HOME/.agents/skills/library-only/SKILL.md")" = 'library body'
if touch "$HOME/.agents/skills/library-only/changed" 2>/dev/null; then exit 1; fi
printf consumed > /tmp/library-probe
"#,
    )
    .unwrap();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .assert_success_within_timeout("library-only skill discovery");
    assert_eq!(
        std::fs::read_to_string(
            env.store()
                .paths()
                .temp_unit_dir(None)
                .join("library-probe")
        )
        .unwrap(),
        "consumed"
    );
    assert!(root.is_dir());
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_to_string(library.join("SKILL.md")).unwrap(),
        "library body"
    );
    assert!(!library.join("changed").exists());
}

#[test]
fn sandbox_merges_rimz_library_and_provider_root_wins() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("shared/SKILL.md"), "provider\n").unwrap();
    for source in ["rimz_home", "home", "override"] {
        let mut vars = environment(&env);
        let library = match source {
            "rimz_home" => env.rimz_home().join("skills"),
            "home" => {
                vars.remove("RIMZ_HOME");
                env.home_root.join(".rimz/skills")
            }
            _ => {
                let root = env.home_root.join("relocated");
                vars.insert("RIMZ_AGENTS_HOME".to_owned(), root.display().to_string());
                root.join("skills")
            }
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
    let library = env.rimz_home().join("skills");
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
    assert_eq!(
        argv.windows(3)
            .filter(|args| args[0] == "--ro-bind")
            .count(),
        2,
        "library copies bind only at the discovery path, not over the host library"
    );
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
fn sandbox_prepare_preserves_symlinked_skill_sources() {
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
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &["CODEX_HOME"],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&callable),
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
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
    assert_eq!(
        plan.pins["SCCACHE_CLIENT_SIDE"],
        rimz::sandbox::EnvPin::Set("1".to_owned())
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
            "--symlink",
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
    let err = rimz::sandbox::plan(&inputs).err().unwrap();
    assert!(matches!(err, SandboxErr::UnknownSkill { .. }));
    assert!(err.to_string().contains(root.to_str().unwrap()));
}

#[test]
fn sandbox_symlinked_manual_skills_resolve_shared_modules() {
    let env = Env::new();
    env.record(&env.project_root);
    let root = env.home_root.join(".claude/skills");
    let source = env.home_root.join(".agents/skills/manual");
    let shared = source.parent().unwrap().join("_shared");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(source.join("SKILL.md"), "manual body\n").unwrap();
    std::fs::write(shared.join("probe.py"), "value = 'shared module'\n").unwrap();
    std::fs::write(
        source.join("scripts/run.py"),
        "from pathlib import Path\nimport sys\nroot = Path(__file__).resolve().parents[2]\nsys.path.insert(0, str(root / '_shared'))\nfrom probe import value\nassert 'disable-model-invocation: true' in (root / 'manual/SKILL.md').read_text()\nprint(value)\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../.agents/skills/manual", root.join("relative")).unwrap();
    std::os::unix::fs::symlink(&source, root.join("absolute")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "root instructions\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();
    let execute = available() && {
        let python3 = which::which("python3").is_ok();
        if !python3 {
            crate::common::skip("python3 not on PATH");
        }
        python3
    };
    for workaround in [false, true] {
        if workaround {
            std::os::unix::fs::symlink("../../.agents/skills/_shared", root.join("_shared"))
                .unwrap();
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
        for (name, target) in [
            ("relative", Path::new("../../.agents/skills/manual")),
            ("absolute", source.as_path()),
            ("CLAUDE.md", Path::new("AGENTS.md")),
        ] {
            assert!(argv.windows(3).any(|args| args[0] == "--symlink"
                && Path::new(&args[1]) == target
                && Path::new(&args[2]) == root.join(name)));
        }
        let copy = skill_bind_source(&argv, &source);
        assert!(copy.starts_with(&env.store().paths().skills_dir));
        assert_eq!(
            argv.windows(3)
                .filter(|args| args[0] == "--ro-bind" && Path::new(&args[2]) == source)
                .count(),
            1
        );
        assert!(!argv.windows(3).any(|args| args[0] == "--ro-bind"
            && (Path::new(&args[2]) == shared || Path::new(&args[2]) == root.join("_shared"))));
        assert_eq!(
            std::fs::read_dir(&env.store().paths().skills_dir)
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "manual body\n"
        );
        if !execute {
            continue;
        }
        enable(&env);
        let shim_dir = write_env_dump_shim(&env, "claude");
        std::fs::write(
            shim_dir.join("claude"),
            "#!/bin/sh\nset -eu\ntest -L \"$HOME/.claude/skills/relative\"\ntest -L \"$HOME/.claude/skills/absolute\"\ntest -L \"$HOME/.claude/skills/CLAUDE.md\"\ntest \"$(cat \"$HOME/.claude/skills/CLAUDE.md\")\" = 'root instructions'\npython3 \"$HOME/.claude/skills/relative/scripts/run.py\" > /tmp/probe-shared\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.skills = Some(vec![]);
        env.rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .env_remove("CLAUDE_CONFIG_DIR")
            .assert_success_within_timeout("symlinked skill shared module import");
        assert_eq!(
            std::fs::read_to_string(env.store().paths().temp_unit_dir(None).join("probe-shared"))
                .unwrap(),
            "shared module\n"
        );
    }
}

#[test]
fn sandbox_skill_aliases_require_matching_invocation_policies() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let source = root.join("native");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("SKILL.md"), "native body\n").unwrap();
    std::os::unix::fs::symlink("native", root.join("alias")).unwrap();
    for names in [
        vec!["alias"],
        vec!["native"],
        vec![],
        vec!["alias", "native"],
    ] {
        let callable: Vec<_> = names.iter().map(|name| name.parse().unwrap()).collect();
        let result = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: Some(&callable),
            },
        );
        if names.len() == 1 {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, SandboxErr::ConflictingSkillAliases { listed, unlisted, path }
                if listed == names[0] && unlisted != listed && path == &source)
            );
            assert!(
                err.to_string()
                    .ends_with("list both names or neither in the profile skills list")
            );
            assert!(!env.store().paths().skills_dir.exists());
            continue;
        }
        let argv = result.unwrap();
        if names.is_empty() {
            assert!(skill_bind_source(&argv, &source).starts_with(&env.store().paths().skills_dir));
            assert_eq!(
                argv.windows(3)
                    .filter(|args| args[0] == "--ro-bind")
                    .count(),
                1
            );
        } else {
            assert!(!argv.iter().any(|arg| arg == "--tmpfs"));
        }
    }
}

#[test]
fn sandbox_non_skill_entries_are_never_materialized() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("_shared")).unwrap();
    std::fs::write(root.join("_shared/module.py"), "shared\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "instructions\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();
    for overlay in [false, true] {
        if overlay {
            let library = env.rimz_home().join("skills/library-only");
            std::fs::create_dir_all(&library).unwrap();
            std::fs::write(library.join("SKILL.md"), "library\n").unwrap();
        }
        let callable = if overlay {
            vec!["library-only".parse().unwrap()]
        } else {
            vec![]
        };
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: Some(&callable),
            },
        )
        .unwrap();
        assert!(!env.store().paths().skills_dir.exists());
        assert!(!root.join("_shared/agents").exists());
        if overlay {
            assert_eq!(
                skill_bind_source(&argv, &root.join("_shared")),
                root.join("_shared")
            );
            assert_eq!(
                skill_bind_source(&argv, &root.join("AGENTS.md")),
                root.join("AGENTS.md")
            );
        } else {
            assert!(!argv.iter().any(|arg| arg == "--tmpfs"));
        }
    }
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
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: Some(env.home_root.join(".agents/skills")),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, inputs.cwd, &[]);
    assert!(argv.windows(3).any(|args| args
        == [
            "--bind",
            env.runtime_root.to_str().unwrap(),
            env.runtime_root.to_str().unwrap()
        ]));
    let var_tmp = tempfile::tempdir_in("/var/tmp").unwrap();
    inputs.cwd = var_tmp.path();
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, inputs.cwd, &[]);
    let cwd = var_tmp.path().to_str().unwrap();
    assert!(
        argv.windows(3).any(|args| args == ["--bind", cwd, cwd]),
        "a required path under /var/tmp stays reachable"
    );
    for mount in ["/tmp", "/var/tmp"] {
        inputs.cwd = Path::new(mount);
        assert!(matches!(
            rimz::sandbox::plan(&inputs),
            Err(SandboxErr::TmpCollision { path }) if path == Path::new(mount)
        ));
    }
}

#[test]
fn sandboxed_exec_shows_profile_skill_view_and_shared_temp_unit() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let cwd = env
        .store()
        .paths()
        .temp_unit_dir(Some("scout"))
        .join("clean");
    std::fs::create_dir_all(&cwd).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(cwd.parent().unwrap())
            .status()
            .unwrap()
            .success()
    );
    for dir in [
        ".claude/skills/a",
        ".agents/skills/b",
        ".agents/skills/c",
        ".agents/skills/d/agents",
        ".codex",
    ] {
        std::fs::create_dir_all(env.home_root.join(dir)).unwrap();
    }
    for name in ["b", "c", "d"] {
        std::fs::write(
            env.home_root
                .join(".agents/skills")
                .join(name)
                .join("SKILL.md"),
            "skill body\n",
        )
        .unwrap();
    }
    std::fs::write(env.home_root.join(".codex/config.toml"), "sandbox-test").unwrap();
    let bad = env.home_root.join(".agents/skills/d/agents/openai.yaml");
    let metadata = "policy: *alias\n";
    std::fs::write(&bad, metadata).unwrap();
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
printf '%s\n' "$PWD" > /tmp/provider-cwd
ls "$HOME/.agents/skills" > /tmp/agent-skills
printf '%s\n' "$TMPDIR" > /tmp/tmpdir
printf '%s\n' "$HOME" > /tmp/provider-home
printf '%s' "$RIMZ_TEST_NON_UTF8" > /tmp/non-utf8
test "$CLAUDE_CONFIG_DIR" = "$HOME/elsewhere"
test "$SCCACHE_CLIENT_SIDE" = 1
test "${CODEX_HOME+x}" != x
test ! -e "$HOME/.agents/skills/b/agents/openai.yaml"
test "$(cat "$HOME/.agents/skills/c/agents/openai.yaml")" = 'policy:
  allow_implicit_invocation: false'
test -d "$XDG_RUNTIME_DIR/rimz/ws/$RIMZ_TEST_WORKSPACE_DIR"
test -c /dev/null
test "$RIMZ_SHARED" = "$RIMZ_TEST_SHARED"
test -d "$RIMZ_SHARED"
printf parent > /tmp/same-name
test "$(cat /var/tmp/same-name)" = parent
test "$(cat "$HOME/.codex/config.toml")" = sandbox-test
if touch "$HOME/.agents/skills/b/changed" 2>/dev/null; then exit 1; fi
test ! -e "$RIMZ_TEST_HOST_TMP_FILE"
test ! -e "$RIMZ_TEST_HOST_VAR_TMP_FILE"
"#,
    )
    .unwrap();
    let shell = write_fake_login_shell(
        &env,
        "sandbox-shell",
        &[("TMPDIR", "wrong-shell-tmp"), ("SCCACHE_CLIENT_SIDE", "0")],
    );
    let shell_body = std::fs::read_to_string(&shell).unwrap();
    std::fs::write(&shell, shell_body.replacen("#!/bin/sh\n", "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=$HOME/elsewhere\nexport CODEX_HOME=$HOME/elsewhere\nexport XDG_RUNTIME_DIR=/tmp/wrong-runtime\nexport HOME=/tmp/evil\n", 1)).unwrap();
    let host_tmp = tempfile::NamedTempFile::new_in("/tmp").unwrap();
    let host_var_tmp = tempfile::NamedTempFile::new_in("/var/tmp").unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.skills = Some(vec!["b".parse().unwrap()]);
    request.identity.name = Some("scout".to_owned());
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .current_dir(&cwd)
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("PATH", path_with_front(&shim_dir))
        .env("SHELL", shell)
        .env("RIMZ_TEST_HOST_TMP_FILE", host_tmp.path())
        .env("RIMZ_TEST_HOST_VAR_TMP_FILE", host_var_tmp.path())
        .env("RIMZ_TEST_SHARED", &env.store().paths().room_shared_dir)
        .env(
            "RIMZ_TEST_WORKSPACE_DIR",
            env.state_path_for(&env.project_root).dir_name.as_str(),
        )
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("rimz: starting without skill \"d\""));
    assert_eq!(std::fs::read(&bad).unwrap(), metadata.as_bytes());
    let store = env.store();
    let tmp = &store.paths().temp_unit_dir(Some("scout"));
    assert_eq!(
        std::fs::read_to_string(tmp.join("provider-cwd")).unwrap(),
        "/tmp/clean\n"
    );
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

    assert_eq!(
        std::fs::read_to_string(tmp.join("same-name")).unwrap(),
        "parent"
    );

    // A launched child shares its parent's unit, found through the parent's row.
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            rimz::store::event::AgentLaunchPayload {
                agent_id: "scout-session".into(),
                launch_id: Some("scout-launch".into()),
                agent_name: "scout".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: None,
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nset -eu\ntest \"$(cat /tmp/same-name)\" = parent\nprintf child > /tmp/child-file\n",
    )
    .unwrap();
    request.subagent = true;
    request.identity.name = Some("otter".to_owned());
    request.identity.params.parent_agent_id = Some("scout-launch".into());
    request.identity.params.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox subagent shares its parent's tmp");
    assert_eq!(
        std::fs::read_to_string(tmp.join("child-file")).unwrap(),
        "child"
    );

    // A peer gets a unit of its own.
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nset -eu\ntest ! -e /tmp/same-name\ntest ! -e /var/tmp/child-file\nprintf peer > /tmp/peer-file\n",
    )
    .unwrap();
    request.subagent = false;
    request.identity.name = Some("fox".to_owned());
    request.identity.params.parent_agent_id = None;
    request.identity.params.parent_agent_kind = None;
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox peer sees its own tmp");
    assert_eq!(
        std::fs::read_to_string(store.paths().temp_unit_dir(Some("fox")).join("peer-file"))
            .unwrap(),
        "peer"
    );
}

#[test]
fn sandboxed_exec_uses_probed_bwrap_with_trusted_path() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
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
        std::fs::read_to_string(
            env.store()
                .paths()
                .temp_unit_dir(None)
                .join("provider-path")
        )
        .unwrap(),
        shim_dir.to_str().unwrap()
    );
}

#[test]
fn a_shared_account_writes_its_history_into_the_default_home_under_both_isolations() {
    for isolation in ["host", "sandbox"] {
        if isolation == "sandbox" && !available() {
            continue;
        }
        let env = Env::new();
        env.record(&env.project_root);
        let work = env.home_root.join("work");
        let native = env.home_root.join(".codex");
        std::fs::create_dir_all(native.join("sessions")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("auth.json"), "work").unwrap();
        let config = env.rimz_home().join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            config,
            format!(
                "[agents]\nisolation = {isolation:?}\n[accounts.codex.work]\nhome = {:?}\n",
                work.to_str().unwrap()
            ),
        )
        .unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\nprintf turn > \"$CODEX_HOME/sessions/rollout.jsonl\"\n\
             printf '%s' \"$CODEX_SQLITE_HOME\" > \"$CODEX_HOME/sessions/databases\"\n",
        )
        .unwrap();
        env.write_config(
            &env.project_root,
            &format!(
                "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
                path_with_front(&shim_dir).to_str().unwrap()
            ),
        );
        env.rimz()
            .args(["trust", "grant"])
            .assert_success_within_timeout("grant trusted provider PATH");
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        request.identity.params.login = Some("work".parse().unwrap());
        env.rimz()
            .args(exec_args(&env, &request))
            .env_remove("CODEX_HOME")
            .env("SHELL", "/definitely/not/a/shell")
            .assert_success_within_timeout("launch on a shared account");
        assert_eq!(
            std::fs::read_link(work.join("sessions")).unwrap(),
            native.join("sessions"),
            "{isolation}"
        );
        assert_eq!(
            std::fs::read_to_string(native.join("sessions/rollout.jsonl")).unwrap(),
            "turn",
            "{isolation}"
        );
        assert_eq!(
            std::fs::read_to_string(native.join("sessions/databases")).unwrap(),
            native.to_str().unwrap(),
            "{isolation}"
        );
        assert!(!work.join("auth.json").is_symlink(), "{isolation}");
        assert!(!native.join("auth.json").exists(), "{isolation}");
    }
}

#[test]
fn a_sandboxed_named_account_reaches_a_default_home_under_tmp_outside_home() {
    if !available() {
        return;
    }
    // A shared account links its history into the provider's own home, and a
    // standalone one its settings: both reach through the account home.
    for history in ["shared", "standalone"] {
        let env = Env::new();
        env.record(&env.project_root);
        let work = env.home_root.join("work");
        // The provider's own home, moved by its native override to a `/tmp`
        // path no rebound root contains: the temp unit mounted over `/tmp`
        // hides it unless the mount plan rebinds it.
        let native_root = tempfile::tempdir_in("/tmp").unwrap();
        let native = native_root.path().join("codex-home");
        assert!(!native.starts_with(&env.home_root));
        std::fs::create_dir_all(native.join("sessions")).unwrap();
        std::fs::write(native.join("AGENTS.md"), "native manual").unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let config = env.rimz_home().join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            config,
            format!(
                "[agents]\nisolation = \"sandbox\"\n[accounts.codex.work]\nhome = {:?}\nhistory = {history:?}\n",
                work.to_str().unwrap()
            ),
        )
        .unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\ncat \"$CODEX_HOME/AGENTS.md\" > \"$CODEX_HOME/seen\"\n\
             if [ -d \"$CODEX_HOME/sessions\" ]; then printf turn > \"$CODEX_HOME/sessions/rollout.jsonl\"; fi\n",
        )
        .unwrap();
        env.write_config(
            &env.project_root,
            &format!(
                "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
                path_with_front(&shim_dir).to_str().unwrap()
            ),
        );
        env.rimz()
            .args(["trust", "grant"])
            .assert_success_within_timeout("grant trusted provider PATH");
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        request.identity.params.login = Some("work".parse().unwrap());
        env.rimz()
            .args(exec_args(&env, &request))
            .env("CODEX_HOME", &native)
            .env("SHELL", "/definitely/not/a/shell")
            .assert_success_within_timeout("launch on a named account");
        assert_eq!(
            std::fs::read_link(work.join("AGENTS.md")).unwrap(),
            native.join("AGENTS.md"),
            "{history}"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("seen")).unwrap(),
            "native manual",
            "{history}"
        );
        if history == "shared" {
            assert_eq!(
                std::fs::read_link(work.join("sessions")).unwrap(),
                native.join("sessions")
            );
            assert_eq!(
                std::fs::read_to_string(native.join("sessions/rollout.jsonl")).unwrap(),
                "turn"
            );
        } else {
            assert!(!work.join("sessions").is_symlink());
        }
    }
}

#[test]
fn sandbox_skills_under_host_use_provider_switches() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "host-skills-shell", &[]);
    let probe = env.home_root.join("provider-env");
    let library_skill = env.agents_home().join("skills/unlisted-dir");
    std::fs::create_dir_all(&library_skill).unwrap();
    std::fs::write(
        library_skill.join("SKILL.md"),
        "---\nname: provider-name\n---\nSkill.\n",
    )
    .unwrap();
    let settings = env.home_root.join("settings.json");
    std::fs::write(
        &settings,
        r#"{"env":{"ANTHROPIC_API_KEY":"sk-secret-123"},"theme":"dark","skillOverrides":{"kept":"enabled"}}"#,
    )
    .unwrap();
    for kind in ["codex", "amp", "claude"] {
        if kind == "claude" {
            for name in ["rimz-probe", "commit"] {
                let dir = env.agents_home().join("skills").join(name);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("SKILL.md"),
                    format!("---\nname: {name}\ndescription: test\n---\nSkill."),
                )
                .unwrap();
            }
        }
        let shim_dir = write_env_dump_shim(&env, kind);
        let args = if kind == "claude" {
            vec!["--settings".into(), settings.display().to_string()]
        } else {
            Vec::new()
        };
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), args);
        request.identity.name = Some(format!("host-{kind}"));
        let unit = env
            .store()
            .paths()
            .temp_unit_dir(request.identity.name.as_deref());
        let mut lists = vec![vec!["missing-skill"], vec![]];
        if kind == "claude" {
            lists.push(vec!["commit"]);
        }
        for skills in lists {
            let missing = skills.contains(&"missing-skill");
            let commit_listed = skills.contains(&"commit");
            request.skills = Some(skills.iter().map(|name| name.parse().unwrap()).collect());
            let output = env
                .rimz()
                .args(exec_args(&env, &request))
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&shim_dir))
                .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
                .bounded_output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if kind != "amp" && missing {
                assert!(!output.status.success(), "missing host skill must refuse");
                assert!(stderr.contains("missing-skill"), "{stderr}");
                assert!(
                    stderr.contains(if kind == "claude" {
                        ".claude/skills"
                    } else {
                        ".agents/skills"
                    }),
                    "{stderr}"
                );
                assert!(
                    stderr.contains(&env.agents_home().join("skills").display().to_string()),
                    "{stderr}"
                );
                continue;
            }
            if kind == "amp" {
                assert!(stderr.contains("no per-launch skill switch"), "{stderr}");
            }
            assert!(
                output.status.success(),
                "host launch applies profile skills for {kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let dump = std::fs::read_to_string(&probe).unwrap();
            let argv: Vec<_> = dump
                .lines()
                .filter(|line| line.starts_with("ARGV_"))
                .map(|line| line.split_once('=').unwrap().1)
                .collect();
            if kind == "codex" {
                assert!(
                    argv.contains(&"skills.config=[{name=\"provider-name\",enabled=false}]"),
                    "{argv:?}"
                );
            } else if kind == "claude" {
                use std::os::unix::fs::PermissionsExt;
                assert!(!dump.contains("sk-secret-123"));
                assert_eq!(argv.iter().filter(|arg| **arg == "--settings").count(), 1);
                let index = argv.iter().position(|arg| *arg == "--settings").unwrap();
                let path = Path::new(argv[index + 1]);
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                let mut settings: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                let routine = settings.as_object_mut().unwrap();
                let permissions = routine.remove("permissions").unwrap();
                let allow = permissions["allow"].as_array().unwrap();
                assert!(allow.contains(&serde_json::json!("Skill(rimz-probe)")));
                assert_eq!(
                    allow.contains(&serde_json::json!("Skill(commit)")),
                    commit_listed
                );
                assert!(!allow.contains(&serde_json::json!("Skill(unlisted-dir)")));
                assert!(routine.remove("autoMode").is_some(), "{routine:?}");
                let mut overrides = serde_json::json!({"kept":"enabled", "unlisted-dir":"user-invocable-only", "rimz-probe":"user-invocable-only"});
                if !commit_listed {
                    overrides["commit"] = serde_json::json!("user-invocable-only");
                }
                assert_eq!(
                    settings,
                    serde_json::json!({"env":{"ANTHROPIC_API_KEY":"sk-secret-123"}, "theme":"dark", "skillOverrides":overrides})
                );
            }
            assert!(
                dump.lines()
                    .any(|line| line == format!("TMPDIR={}", unit.display())),
                "host launch exports its temp unit's host path for {kind}"
            );
            assert_eq!(
                dump.lines()
                    .any(|line| line == format!("CLAUDE_CODE_TMPDIR={}", unit.display())),
                kind == "claude",
                "only Claude's own temp root follows the unit, {kind}"
            );
            assert!(unit.is_dir());
            std::fs::remove_file(&probe).unwrap();
            assert!(!env.store().paths().skills_dir.exists());
        }
    }
}

/// A launch inside another agent's tree unsets the temp root that agent listed
/// unless it owns the key, so a nested provider never writes into the parent's
/// unit. `SHELL` is set, so the unset also survives shell startup.
#[test]
fn a_nested_launch_drops_a_parent_temp_root_it_does_not_own() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "nested-temp-shell", &[]);
    let probe = env.home_root.join("provider-env");
    let parent = env.home_root.join("parent-unit");
    std::fs::create_dir_all(&parent).unwrap();
    for kind in ["codex", "claude"] {
        let shim_dir = write_env_dump_shim(&env, kind);
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.name = Some(format!("nested-{kind}"));
        let unit = env
            .store()
            .paths()
            .temp_unit_dir(request.identity.name.as_deref());
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&shim_dir))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
            .env("CLAUDE_CODE_TMPDIR", &parent)
            .env("RIMZ_TEMP_ROOT_KEYS", "CLAUDE_CODE_TMPDIR")
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let dump = std::fs::read_to_string(&probe).unwrap();
        let line = |key: &str| {
            dump.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        if kind == "codex" {
            assert_eq!(line("CLAUDE_CODE_TMPDIR"), None, "{dump}");
            assert_eq!(line("RIMZ_TEMP_ROOT_KEYS").as_deref(), Some(""), "{dump}");
        } else {
            assert_eq!(
                line("CLAUDE_CODE_TMPDIR"),
                Some(unit.display().to_string()),
                "{dump}"
            );
            assert_eq!(
                line("RIMZ_TEMP_ROOT_KEYS").as_deref(),
                Some("CLAUDE_CODE_TMPDIR"),
                "{dump}"
            );
        }
        std::fs::remove_file(&probe).unwrap();
    }
}

#[test]
fn launch_warnings_reach_agents_show_and_clean_relaunch_clears_them() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "warning-shell", &[]);
    let shim = write_env_dump_shim(&env, "amp");
    let probe = env.home_root.join("warning-provider-env");
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::new(
            env.workspace_id.clone(),
            &workspace.session_name,
            "amp",
            "agent",
            "agent.launched",
            serde_json::json!({"agent_id": "launch-warning", "launch_id": "launch-warning",
            "agent_name": "warning-agent", "agent_name_explicit": true, "state": "starting"}),
        ))
        .unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
    request.identity.launch_id = Some("launch-warning".into());
    request.identity.name = Some("warning-agent".into());
    request.identity.params.isolation = Some(Isolation::Host);
    for skills in [Some(Vec::new()), None] {
        request.skills = skills;
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&shim))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
            .bounded_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let warnings: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("rimz: "))
            .collect();
        if request.skills.is_some() {
            assert!(
                warnings
                    .iter()
                    .any(|text| text.contains("no per-launch skill switch")),
                "{stderr}"
            );
        } else {
            assert!(warnings.is_empty(), "{stderr}");
        }
        let shown = env
            .rimz()
            .args(["agents", "show", "@warning-agent", "--json"])
            .bounded_output()
            .unwrap();
        assert!(
            shown.status.success(),
            "{}",
            String::from_utf8_lossy(&shown.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(
            report["agent"]["launch_warnings"],
            serde_json::json!(warnings)
        );
    }
}

#[test]
fn unseen_resume_launch_warnings_reach_agents_show() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "warning-shell", &[]);
    let shim = write_env_dump_shim(&env, "amp");
    let probe = env.home_root.join("warning-provider-env");
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
    request.action = rimz::harness::launch::ExecAction::Resume {
        session_id: "unseen-warning-session".into(),
        extra_args: Vec::new(),
    };
    request.identity.launch_id = Some("resume-warning-launch".into());
    request.identity.params.isolation = Some(Isolation::Host);
    request.skills = Some(Vec::new());
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
        .env("TMUX_PANE", "%4")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let warnings: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("rimz: "))
        .collect();
    assert!(
        warnings
            .iter()
            .any(|text| text.contains("no per-launch skill switch")),
        "{stderr}"
    );
    let shown = env
        .rimz()
        .args(["agents", "show", "unseen-warning-session", "--json"])
        .bounded_output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        report["agent"]["launch_warnings"],
        serde_json::json!(warnings)
    );
}

#[test]
fn host_skill_links_reconcile_only_owned_entries() {
    use rimz::agents::skill_links::{self, Desired, SkillLinkAction};
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    let empty = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert!(empty.is_empty());
    skill_links::apply(&empty).unwrap();
    assert!(!root.exists());
    assert!(!library.exists());
    for name in ["created", "directory", "foreign", "wrong"] {
        std::fs::create_dir_all(library.join(name)).unwrap();
        std::fs::write(library.join(name).join("SKILL.md"), "skill").unwrap();
    }
    std::fs::create_dir_all(library.join("not-a-skill")).unwrap();
    symlink("created", library.join("alias")).unwrap();
    std::fs::create_dir_all(root.join("directory")).unwrap();
    std::fs::write(root.join("directory/keep"), "mine").unwrap();
    symlink("/foreign/missing", root.join("foreign")).unwrap();
    symlink(library.join("gone"), root.join("stale")).unwrap();
    symlink(library.join("created"), root.join("wrong")).unwrap();
    let plan = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(plan.shadowed(), ["directory", "foreign"]);
    assert!(plan.actions().windows(2).any(|actions| matches!(actions,
        [SkillLinkAction::Unlink { name }, SkillLinkAction::Link { name: next, .. }]
            if name == "wrong" && next == name
    )));
    let outcome = skill_links::apply(&plan).unwrap();
    assert_eq!((outcome.linked, outcome.unlinked), (3, 2));
    assert!(outcome.shadowed.is_empty());
    for name in ["created", "alias", "wrong"] {
        assert_eq!(
            std::fs::read_link(root.join(name)).unwrap(),
            library.join(name)
        );
    }
    assert!(!root.join("stale").is_symlink());
    assert!(!root.join("not-a-skill").exists());
    let repeated = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert!(!repeated.has_owned_changes());
    assert_eq!(repeated.shadowed(), ["directory", "foreign"]);
    for name in ["directory", "foreign"] {
        std::fs::remove_dir_all(library.join(name)).unwrap();
    }
    assert!(
        skill_links::plan(&root, &library, Desired::Library)
            .unwrap()
            .is_empty()
    );
    let remove = skill_links::plan(&root, &library, Desired::None).unwrap();
    assert!(remove.shadowed().is_empty());
    assert_eq!(skill_links::apply(&remove).unwrap().unlinked, 3);
    assert_eq!(skill_links::apply(&remove).unwrap().unlinked, 0);
    assert_eq!(
        std::fs::read_to_string(root.join("directory/keep")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_link(root.join("foreign")).unwrap(),
        Path::new("/foreign/missing")
    );
    symlink(library.join("gone"), root.join("stale")).unwrap();
    std::fs::remove_dir_all(&library).unwrap();
    let stale = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(skill_links::apply(&stale).unwrap().unlinked, 1);
}

#[test]
fn host_skill_links_leave_a_library_overlapping_the_provider_root_untouched() {
    use rimz::agents::skill_links::{self, Desired};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("created")).unwrap();
    std::fs::write(root.join("created/SKILL.md"), "skill").unwrap();
    symlink("created", root.join("alias")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    std::fs::write(root.join(std::ffi::OsStr::from_bytes(b"\xff")), "mine").unwrap();

    let library = env.home_root.join(".agents/skills");
    for desired in [Desired::Library, Desired::None] {
        let plan = skill_links::plan(&root, &library, desired).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        skill_links::apply(&plan).unwrap();
    }
    let nested = skill_links::plan(&root, &root.join("created"), Desired::Library).unwrap();
    assert!(nested.is_empty(), "{nested:?}");
    assert_eq!(
        std::fs::read_link(root.join("alias")).unwrap(),
        Path::new("created")
    );

    let separate = env.agents_home().join("skills");
    std::fs::create_dir_all(separate.join("shared")).unwrap();
    std::fs::write(separate.join("shared/SKILL.md"), "skill").unwrap();
    symlink("loop", separate.join("loop")).unwrap();
    let plan = skill_links::plan(&root, &separate, Desired::Library).unwrap();
    assert_eq!(plan.shadowed(), Vec::<String>::new());
    assert_eq!(plan.actions().len(), 1, "{plan:?}");
}

#[test]
fn host_skill_links_apply_tolerates_siblings_and_reports_foreign_races() {
    use rimz::agents::skill_links::{self, Desired};
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let root = env.home_root.join("provider/skills");
    let library = env.agents_home().join("skills");
    std::fs::create_dir_all(library.join("skill")).unwrap();
    std::fs::write(library.join("skill/SKILL.md"), "skill").unwrap();
    let plan = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(skill_links::apply(&plan).unwrap().linked, 1);
    let sibling = skill_links::apply(&plan).unwrap();
    assert_eq!(sibling.linked, 0);
    assert!(sibling.shadowed.is_empty());
    std::fs::remove_file(root.join("skill")).unwrap();
    std::fs::create_dir(root.join("skill")).unwrap();
    assert_eq!(skill_links::apply(&plan).unwrap().shadowed, ["skill"]);
    std::fs::remove_dir(root.join("skill")).unwrap();
    symlink(library.join("gone"), root.join("skill")).unwrap();
    let replace = skill_links::plan(&root, &library, Desired::Library).unwrap();
    std::fs::remove_file(root.join("skill")).unwrap();
    symlink("/foreign/skill", root.join("skill")).unwrap();
    assert_eq!(skill_links::apply(&replace).unwrap().shadowed, ["skill"]);
    assert_eq!(
        std::fs::read_link(root.join("skill")).unwrap(),
        Path::new("/foreign/skill")
    );
}

#[test]
fn host_exec_links_library_into_codex_and_claude_account_roots_once() {
    for kind in ["codex", "claude"] {
        let env = Env::new();
        env.record(&env.project_root);
        let library = env.agents_home().join("skills");
        std::fs::create_dir_all(library.join("shared")).unwrap();
        std::fs::write(library.join("shared/SKILL.md"), "shared skill").unwrap();
        let account = env.home_root.join("named-claude");
        let root = if kind == "claude" {
            account.join("skills")
        } else {
            env.home_root.join(".agents/skills")
        };
        let shell = write_fake_login_shell(&env, "host-skills-shell", &[]);
        let shim_dir = write_env_dump_shim(&env, kind);
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        for first in [true, false] {
            let output = env
                .rimz()
                .args(exec_args(&env, &request))
                .env("CLAUDE_CONFIG_DIR", &account)
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&shim_dir))
                .env(
                    "RIMZ_TEST_AGENT_ENV_DUMP",
                    env.home_root.join("provider-env"),
                )
                .bounded_output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stderr}");
            assert_eq!(
                stderr.contains(&format!("linked 1 skill(s) into {}", root.display())),
                first,
                "{stderr}"
            );
            assert_eq!(
                std::fs::read_link(root.join("shared")).unwrap(),
                library.join("shared")
            );
            assert!(!env.store().paths().skills_dir.exists());
        }
        assert!(!env.home_root.join(".claude/skills").exists());
    }
}

#[test]
fn host_skill_links_explain_is_read_only_and_reports_shadowed() {
    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    let empty = env
        .rimz()
        .args(["agents", "explain", "codex", "--json"])
        .bounded_output()
        .unwrap();
    assert!(empty.status.success());
    let empty: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert!(empty.get("skill_links").is_none());
    assert!(!root.exists());
    for name in ["shared", "foreign"] {
        std::fs::create_dir_all(library.join(name)).unwrap();
        std::fs::write(library.join(name).join("SKILL.md"), "skill").unwrap();
    }
    std::fs::create_dir_all(root.join("foreign")).unwrap();
    let output = env
        .rimz()
        .args(["agents", "explain", "codex", "--json"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["skill_links"]["root"], root.to_str().unwrap());
    assert_eq!(report["skill_links"]["library"], library.to_str().unwrap());
    assert_eq!(
        report["skill_links"]["shadowed"],
        serde_json::json!(["foreign"])
    );
    assert_eq!(
        report["skill_links"]["actions"][0]["Link"]["name"],
        "shared"
    );
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("these entries are yours"))
    );
    let human = env
        .rimz()
        .args(["agents", "explain", "codex"])
        .bounded_output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("Skill links"));
    assert!(human.contains("link: shared"));
    assert!(human.contains("shadowed: foreign"));
    assert!(!root.join("shared").exists());
    assert!(!env.store().paths().tmp_dir.exists());
    assert!(!env.store().paths().skills_dir.exists());
}

#[test]
fn sandbox_preserves_host_library_link_without_duplicate_bind() {
    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(library.join("shared")).unwrap();
    std::fs::write(library.join("shared/SKILL.md"), "shared skill").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(library.join("shared"), root.join("shared")).unwrap();
    let native = skill_prepare(
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
    assert!(!native.plan.mounts.iter().any(|mount| matches!(mount,
        rimz::sandbox::Mount::RoBind { target, .. } | rimz::sandbox::Mount::Bind { target, .. } if target == &root.join("shared")
    )));
    assert_eq!(
        std::fs::read_link(root.join("shared")).unwrap(),
        library.join("shared")
    );
    std::fs::create_dir_all(library.join("library-only")).unwrap();
    std::fs::write(library.join("library-only/SKILL.md"), "library skill").unwrap();
    let plan = skill_prepare(
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
    assert_eq!(plan.plan.mounts.iter().filter(|mount| matches!(mount,
        rimz::sandbox::Mount::Symlink { target, path } if target == &library.join("shared") && path == &root.join("shared")
    )).count(), 1);
    assert!(!plan.plan.mounts.iter().any(|mount| matches!(mount,
        rimz::sandbox::Mount::RoBind { target, .. } | rimz::sandbox::Mount::Bind { target, .. } if target == &root.join("shared")
    )));
    assert!(plan.copies.is_empty());
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
    let plan = rimz::sandbox::plan(&SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&["visible".parse().unwrap()]),
        },
    })
    .unwrap();
    rimz::sandbox::apply(&plan).unwrap();
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
    let pid_path = store.paths().temp_unit_dir(None).join("provider-pid");
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

/// A sandboxed agent's detached writer on its `/tmp`, alive after the agent's
/// wrapper and bubblewrap are gone.
struct ViewWriter {
    env: Env,
    pid: u32,
    start: String,
    unit: PathBuf,
}

impl ViewWriter {
    fn start() -> Option<Self> {
        use rimz::store::run::{RunRecord, RunStatus};
        use std::process::Stdio;
        use std::time::{Duration, Instant};

        if !available() {
            return None;
        }
        let env = Env::new();
        enable(&env);
        env.record(&env.project_root);
        let store = env.store();
        let mut run = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "detached tmp writer".to_owned(),
            env.project_root.clone(),
        );
        run.status = RunStatus::Running;
        run.subagent = true;
        run.keep = true;
        rimz::harness::run::create(store.paths(), &run).unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        // The wrapper probes `--version` on the host, outside the view, and a
        // writer that inherits the shim's stdio would hang the wrapper.
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\n\
             [ \"$1\" = --version ] && exit 0\n\
             setsid sh -c 'echo $$ > /tmp/writer-pid; i=0; while :; do i=$((i+1)); mkdir -p /tmp/d$((i%50)); : > /tmp/d$((i%50))/f$i; done' </dev/null >/dev/null 2>&1 &\n\
             echo $$ > /tmp/agent-pid\n\
             exec /usr/bin/sleep 60\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(run.kind.clone(), Vec::new());
        request.run_id = Some(run.run_id.clone());
        request.exit_on_run_completion = true;
        let mut wrapper = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let unit = store.paths().temp_unit_dir(None);
        let read_pid = |name: &str| {
            std::fs::read_to_string(unit.join(name))
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let (pid, agent) = loop {
            if let (Some(pid), Some(agent)) = (read_pid("writer-pid"), read_pid("agent-pid"))
                && unit.join("d3").is_dir()
            {
                break (pid, agent);
            }
            assert!(Instant::now() < deadline, "writer did not start");
            assert!(
                wrapper.try_wait().unwrap().is_none(),
                "wrapper exited before writer startup"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let start = rimz::proc::process_start_token(pid).unwrap();
        wrapper.kill().unwrap();
        wrapper.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while rimz::proc::process_is_live(agent, None) {
            assert!(Instant::now() < deadline, "agent outlived its wrapper");
            std::thread::sleep(Duration::from_millis(20));
        }
        let writer = Self {
            env,
            pid,
            start,
            unit,
        };
        assert!(writer.is_live(), "writer died with its agent");
        Some(writer)
    }

    fn is_live(&self) -> bool {
        rimz::proc::process_is_live(self.pid, Some(&self.start))
    }

    fn reset(&self, hard: bool) -> std::process::Output {
        let mut args = vec!["--mux", "zellij", "reset", "--no-start", "--yes"];
        if hard {
            args.push("--hard");
        }
        self.env.rimz().args(args).bounded_output().unwrap()
    }

    fn assert_hard_reset_ends_it(&self) {
        let output = self.reset(true);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "hard reset failed: {stderr}");
        assert!(stderr.contains("Sandbox views: ended "), "{stderr}");
        assert!(!self.is_live(), "writer survived the hard reset");
        let tmp = &self.env.state_path_for(&self.env.project_root).tmp_dir;
        assert!(!tmp.exists(), "tmp/ survived the hard reset");
        assert!(
            !tmp.with_extension("reset").exists(),
            "tmp.reset/ survived the hard reset"
        );
    }
}

#[test]
fn hard_reset_ends_a_detached_writer_inside_a_sandbox_view() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let mut bystander = std::process::Command::new("/usr/bin/sleep");
    bystander.arg("60");
    for (key, value) in writer.env.rimz().get_envs() {
        match value {
            Some(value) => bystander.env(key, value),
            None => bystander.env_remove(key),
        };
    }
    let mut bystander = bystander.spawn().unwrap();
    writer.assert_hard_reset_ends_it();
    let spared = bystander.try_wait().unwrap().is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(spared, "hard reset ended a host process outside any view");
}

#[test]
fn hard_reset_ends_a_writer_an_earlier_failed_reset_left_detached() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let tmp = writer.env.state_path_for(&writer.env.project_root).tmp_dir;
    std::fs::rename(&tmp, tmp.with_extension("reset")).unwrap();
    writer.assert_hard_reset_ends_it();
}

#[test]
fn soft_reset_spares_a_detached_writer_inside_a_sandbox_view() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let output = writer.reset(false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "soft reset failed: {stderr}");
    assert!(!stderr.contains("Sandbox views"), "{stderr}");
    assert!(writer.is_live(), "soft reset ended the writer");
    assert!(writer.unit.join("writer-pid").is_file());
}

#[test]
fn sandboxed_caller_prompt_file_under_tmp_resolves_to_its_temp_unit() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let unit = env.store().paths().temp_unit_dir(Some("scout"));
    std::fs::create_dir_all(&unit).unwrap();
    std::fs::write(unit.join("brief.md"), "Brief.\n").unwrap();
    // The caller writes `/tmp/brief.md` in its view; the launch reads it on the host.
    let output = env
        .rimz()
        .args([
            "agents",
            "explain",
            "codex",
            "--system-prompt-file",
            "/tmp/brief.md",
            "--json",
        ])
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("RIMZ_ISOLATION", "sandbox")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "scout")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["prompt"]["sources"][0]["path"],
        unit.join("brief.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
}
