use super::*;
use crate::agents::{AgentStatus, ConcernCoverage, IntegrationConcern};
use crate::ids::AgentKind;
#[test]
fn prompt_cache_lifetimes_are_provider_defaults() {
    use crate::agents::context::SignIn;
    use std::time::Duration;
    let sign_ins = [Some(SignIn::Subscription), Some(SignIn::ApiKey), None];
    let actual: Vec<_> = ["claude", "codex", "amp"]
        .into_iter()
        .map(|kind| {
            sign_ins.map(|sign_in| find_definition(kind).unwrap().prompt_cache_ttl(sign_in))
        })
        .collect();
    assert_eq!(
        actual,
        [
            [Some(Duration::from_secs(3600)); 3],
            [
                Some(Duration::from_secs(43200)),
                Some(Duration::from_secs(1800)),
                Some(Duration::from_secs(1800))
            ],
            [None; 3],
        ]
    );
    for definition in all_definitions() {
        for ttl in sign_ins
            .into_iter()
            .filter_map(|sign_in| definition.prompt_cache_ttl(sign_in))
        {
            assert!(
                crate::config::HarnessConfig::default().prompt_cache_margin < ttl,
                "{}: idle compaction needs a TTL above the cache margin",
                definition.spec().kind
            );
        }
    }
}

#[test]
fn compact_command_carries_the_brief_for_the_agent_seat() {
    let harness = HarnessConfig::default();
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-compact"),
        AgentStatus::Idle,
        jiff::Timestamp::from_second(1_700_000_000).expect("valid timestamp"),
    );
    let solo = compact_command(&agent, &harness).expect("claude compacts");
    assert!(
        solo.starts_with(
            "/compact Summarize the transcript inside <summary></summary> tags. Include"
        ),
        "{solo}"
    );

    agent.team = Some("forge".to_owned());
    let team = compact_command(&agent, &harness).expect("claude compacts");
    assert!(team.starts_with("/compact Summarize the transcript inside <summary></summary> tags so a new context window continues"), "{team}");
    assert!(team.contains("This seat is a team member"), "{team}");
}

#[test]
fn registry_resolves_kinds_and_sub_providers_without_collisions() {
    // Every kind round-trips through resolution, an unknown kind errors, and
    // no two adapters claim the same kind.
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        assert_eq!(
            definition_by_kind(kind).unwrap().spec().kind,
            kind,
            "registry round-trip for {kind}"
        );
    }
    assert!(definition_by_kind("unknown-agent").is_err());

    let mut kinds: Vec<_> = known_kinds().collect();
    kinds.sort_unstable();
    let before = kinds.len();
    kinds.dedup();
    assert_eq!(kinds.len(), before, "duplicate kind in BUILTINS");
}

#[test]
fn registered_definitions_are_valid_and_names_are_unique() {
    let mut names = std::collections::BTreeSet::new();
    for definition in BUILTINS {
        definition.validate().expect("valid built-in definition");
        for name in
            std::iter::once(definition.spec().kind).chain(definition.spec().aliases.iter().copied())
        {
            assert!(names.insert(name), "duplicate registered name `{name}`");
        }
    }
}

#[test]
fn command_agent_kind_combines_descriptors_with_process_policy() {
    for (command, expected) in [
        ("sudo npm install -g @openai/codex", None),
        ("sudo codex", Some("codex")),
        ("codex-aarch64-apple-darwin", Some("codex")),
        ("/usr/local/bin/codex-x86_64-apple-darwin", Some("codex")),
        ("claude", Some("claude")),
        ("agy", Some("antigravity")),
        ("antigravity", None),
        // No adapter claims the generic `agent` name Cursor's installer
        // also links; only `cursor-agent` classifies as Cursor.
        ("agent", None),
        ("cursor-agent", Some("cursor")),
        ("kiro-cli-chat", Some("kiro")),
        ("kiro-cli-term", None),
        ("node /usr/bin/codex", Some("codex")),
        ("node /opt/claude/cli.js", Some("claude")),
        ("node /tmp/claude-test/cli.js", None),
        ("node", None),
        ("node /srv/app/server.js", None),
        ("codex app-server", None),
        ("codex remote-control start", None),
        (
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt",
            Some("codex"),
        ),
        ("rimz agents exec unknown", None),
    ] {
        assert_eq!(command_agent_kind(command), expected, "{command}");
    }
    assert_eq!(
        command_agent_kind(
            "/home/u/.local/lib/qwen-code/node/bin/node --expose-gc /home/u/.local/lib/qwen-code/lib/cli.js"
        ),
        Some("qwen")
    );
    assert!(!command_may_be_agent_kind("agent", "cursor"));
    assert!(command_may_be_agent_kind("cursor-agent", "cursor"));
    assert!(!command_may_be_agent_kind("agent", "grok"));
    assert!(command_may_be_agent_kind("kiro-cli-chat", "kiro"));
    assert_eq!(command_agent_kind_candidate("kiro-cli-chat"), Some("kiro"));
}

#[test]
fn command_agent_kind_uses_precise_comm_without_claiming_launchers() {
    for (comm, expected) in [
        ("claude", Some("claude")),
        ("codex-aarch64-a", Some("codex")),
        ("agy", Some("antigravity")),
        ("kiro-cli-chat", Some("kiro")),
        ("node", None),
        ("hermes", Some("hermes")),
        ("python3", None),
        ("bun", None),
        ("zsh", None),
    ] {
        assert_eq!(command_agent_kind_with_comm("", Some(comm)), expected);
    }
}

#[test]
fn ordinary_adapters_accept_default_hook_ingress_ownership() {
    assert_eq!(
        find_definition("amp").unwrap().hook_ingress(Some(42)),
        crate::agents::HookIngressDecision::Accept(crate::agents::HookIngressAcceptance::agent(
            Some(42)
        ),)
    );
}

#[test]
fn adapters_without_a_manual_compaction_command_declare_compaction_unsupported() {
    // `--smart-compact` types this into the agent's composer; every wired
    // agent exposes a slash command (`/compact`, Cursor's `/summarize`), so
    // a new adapter that forgets to opt in fails
    // here rather than silently never compacting.
    for adapter in BUILTINS {
        if let Some(command) = adapter.spec().launch.compact_command {
            assert!(!command.command.is_empty() && command.command.starts_with('/'));
            continue;
        }
        assert!(
            matches!(
                adapter
                    .spec()
                    .concern_coverage(IntegrationConcern::Compaction),
                ConcernCoverage::Unsupported { .. }
            ),
            "missing compact command for {}",
            adapter.spec().kind
        );
    }
}

#[test]
fn soft_compact_participation_pins_every_builtin_skip_and_reason() {
    let mut skipped: Vec<_> = BUILTINS
        .iter()
        .filter_map(|definition| {
            soft_compact_skip(definition).map(|reason| (definition.spec().kind, reason.to_string()))
        })
        .collect();
    skipped.sort_unstable();
    assert_eq!(
        skipped,
        [
            ("amp", "no compact command".into()),
            ("antigravity", "no compact command".into()),
            ("hermes", "no compaction signal".into()),
            ("kiro", "no compaction signal".into()),
            ("minimax", "no live context reading".into()),
            ("muse", "no compaction signal".into()),
            ("qoder", "no live context reading".into()),
        ]
    );
}

#[test]
fn soft_compact_prompt_preserves_the_exact_endings_and_seat_briefs() {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("soft-compact"),
        AgentStatus::Running,
        jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    );
    let harness = HarnessConfig::default();
    let prefix = "Context is getting high, so I'm going to compact you once this turn ends. Wrap up the step you're on, without leaving any file half-edited, and don't start anything new. Then end your turn";
    for (kind, team, instruction, expected) in [
        ("claude", None, None, format!("{prefix}.")),
        (
            "codex",
            None,
            None,
            format!(
                "{prefix} with a note for yourself after the compaction:\n\n{}",
                harness.compact_instruction(crate::config::CompactSeat::Solo)
            ),
        ),
        (
            "codex",
            Some("forge"),
            None,
            format!(
                "{prefix} with a note for yourself after the compaction:\n\n{}",
                harness.compact_instruction(crate::config::CompactSeat::Team)
            ),
        ),
        (
            "codex",
            Some("forge"),
            Some("  Keep exact decisions.\nAnd open questions.  "),
            format!(
                "{prefix} with a note for yourself after the compaction:\n\n  Keep exact decisions.\nAnd open questions.  "
            ),
        ),
        ("codex", None, Some(""), format!("{prefix}.")),
    ] {
        agent.kind = AgentKind::new_unchecked(kind);
        agent.team = team.map(str::to_owned);
        let mut harness = harness.clone();
        harness.compact_instruction = instruction.map(str::to_owned);
        assert_eq!(soft_compact_prompt(&agent, &harness), Some(expected));
    }
}

#[test]
fn sub_providers_are_unique() {
    let mut providers: Vec<_> = BUILTINS
        .iter()
        .flat_map(|adapter| adapter.spec().sub_providers)
        .collect();
    providers.sort_unstable();
    let before = providers.len();
    providers.dedup();
    assert_eq!(providers.len(), before, "sub provider claimed twice");
}

#[test]
fn resume_dispatch_walks_single_child_chains_and_rejects_branches() {
    let session = "sess_11111111-1111-4111-8111-111111111111";
    assert_eq!(
        resumed_session_id_for_root_with(
            1,
            &|pid| (pid == 1).then(|| format!("kiro-cli-chat --resume-id={session}")),
            &|_| Vec::new(),
        )
        .as_deref(),
        Some(session)
    );
    assert_eq!(
        resumed_session_id_for_root_with(
            1,
            &|pid| match pid {
                1 => Some("zsh".to_owned()),
                2 => Some(
                    "kiro-cli-chat --resume-id=sess_11111111-1111-4111-8111-111111111111"
                        .to_owned()
                ),
                _ => None,
            },
            &|pid| (pid == 1).then_some(vec![2]).unwrap_or_default(),
        )
        .as_deref(),
        Some("sess_11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(
        resumed_session_id_for_root_with(
            1,
            &|pid| match pid {
                1 => Some("zsh".to_owned()),
                2 => Some("chezmoi cd".to_owned()),
                3 => Some("/bin/zsh".to_owned()),
                4 => Some(format!("kiro-cli-chat --resume-id={session}")),
                _ => None,
            },
            &|pid| match pid {
                1 => vec![2],
                2 => vec![3],
                3 => vec![4],
                _ => Vec::new(),
            },
        )
        .as_deref(),
        Some(session)
    );
    assert!(
        resumed_session_id_for_root_with(1, &|_| Some("zsh".to_owned()), &|pid| (pid == 1)
            .then_some(vec![2, 3])
            .unwrap_or_default(),)
        .is_none()
    );
}

const CODEX_ID: &str = "019a1699-334b-76a3-a907-f58a8fde9cd6";
const KIRO_ID: &str = "sess_019a1699-334b-76a3-a907-f58a8fde9cd6";
const DUTY: &str = r#"/home/me/.cargo/bin/rimz agents supervise-duty --request {"duty":"compact"}"#;
const DRAINER: &str = "/home/me/.cargo/bin/rimz hooks drain --project-root /repo";

/// The exec wrapper's argv for a resume in `plan_exec_argv`'s shape.
fn exec_wrapper(kind: &str, session: &str) -> String {
    format!(
        "/home/me/.cargo/bin/rimz agents exec {kind} --request-file \
         /runtime/live/prompt/exec.{session}.json"
    )
}

/// The same wrapper after a park re-exec.
fn parked_exec_wrapper(kind: &str, session: &str) -> String {
    let wrapper = exec_wrapper(kind, session);
    format!("{wrapper} --supervise /tmp/rimz/run/supervise.json")
}

/// Walk a fixture tree of `(pid, cmdline, children)` rows from pid 1.
fn walk(tree: &[(u32, Option<&str>, &[u32])]) -> Option<String> {
    let row = |pid| tree.iter().find(|(row, ..)| *row == pid);
    resumed_session_id_for_root_with(
        1,
        &|pid| row(pid).and_then(|(_, cmdline, _)| cmdline.map(str::to_owned)),
        &|pid| {
            row(pid)
                .map(|(.., children)| children.to_vec())
                .unwrap_or_default()
        },
    )
    .as_deref()
    .map(str::to_owned)
}

#[test]
fn resume_walk_sees_past_rimz_helpers_under_the_exec_wrapper() {
    let codex = format!("codex resume {CODEX_ID} --dangerously-bypass-approvals-and-sandbox");
    let kiro = format!("kiro-cli-chat --resume-id={KIRO_ID}");
    let found: Vec<_> = [
        (exec_wrapper("codex", CODEX_ID), &codex, DUTY),
        (parked_exec_wrapper("codex", CODEX_ID), &codex, DRAINER),
        (exec_wrapper("kiro", KIRO_ID), &kiro, DUTY),
    ]
    .iter()
    .map(|(root, provider, helper)| {
        let tree: &[(u32, Option<&str>, &[u32])] = &[
            (1, Some(root), &[2, 5]),
            (2, Some("/usr/bin/bwrap --args 3 --die-with-parent"), &[3]),
            (3, Some("/usr/bin/bwrap --args 4 --unshare-pid"), &[4]),
            (4, Some(provider), &[]),
            (5, Some(helper), &[]),
        ];
        walk(tree)
    })
    .collect();
    assert_eq!(
        found,
        [CODEX_ID, CODEX_ID, KIRO_ID].map(|id| Some(id.to_owned()))
    );
}

#[test]
fn resume_walk_skips_dead_and_unreadable_children() {
    let root = exec_wrapper("codex", CODEX_ID);
    let codex = format!("codex resume {CODEX_ID}");
    let found = [Some(""), None].map(|sibling| {
        let tree: &[(u32, Option<&str>, &[u32])] = &[
            (1, Some(&root), &[2, 3]),
            (2, Some(&codex), &[]),
            (3, sibling, &[]),
        ];
        walk(tree)
    });
    assert_eq!(found, [CODEX_ID, CODEX_ID].map(|id| Some(id.to_owned())));
}

#[test]
fn resume_walk_abstains_between_agent_candidates() {
    let root = exec_wrapper("codex", CODEX_ID);
    let first = format!("codex resume {CODEX_ID}");
    for sibling in [
        "codex resume 019a1699-0000-7000-8000-000000000000",
        "make test",
    ] {
        let tree: &[(u32, Option<&str>, &[u32])] = &[
            (1, Some(&root), &[2, 3, 4]),
            (2, Some(&first), &[]),
            (3, Some(sibling), &[]),
            (4, Some(DUTY), &[]),
        ];
        assert_eq!(walk(tree), None, "resume beside {sibling}");
    }
}

#[test]
fn resume_walk_counts_the_exec_wrapper_as_an_agent() {
    let wrapper = exec_wrapper("kiro", KIRO_ID);
    let kiro = format!("kiro-cli-chat --resume-id={KIRO_ID}");
    let tree: &[(u32, Option<&str>, &[u32])] = &[
        (1, Some("/bin/zsh"), &[2]),
        (2, Some(&wrapper), &[3]),
        (3, Some(&kiro), &[]),
    ];
    assert_eq!(walk(tree).as_deref(), Some(KIRO_ID));
    for root in [
        exec_wrapper("codex", CODEX_ID),
        parked_exec_wrapper("codex", CODEX_ID),
    ] {
        assert_eq!(command_agent_kind(&root), Some("codex"), "{root}");
    }
}

fn probe(tree: &[(u32, Option<&str>, &[u32])], provider: Option<u32>) -> ResumeProbe {
    let row = |pid| tree.iter().find(|(row, ..)| *row == pid);
    resume_probe_for_root_with(
        1,
        &|pid| row(pid).and_then(|(_, command, _)| command.map(str::to_owned)),
        &|pid| {
            assert_ne!(Some(pid), provider, "never list the provider's children");
            row(pid)
                .map(|(.., children)| children.to_vec())
                .unwrap_or_default()
        },
    )
}

#[test]
fn resume_probe_stops_at_fresh_providers_instead_of_their_tools() {
    let tool = format!("codex resume {CODEX_ID}");
    let source_build = format!("xai-grok-pager --resume {CODEX_ID}");
    for command in [
        "claude --resume ignored",
        "codex",
        "kiro-cli-chat",
        "grok",
        "agy",
        &source_build,
        "node /opt/claude/cli.js",
        "node /usr/bin/codex",
        "node /usr/lib/node_modules/@xai-official/grok/bin/grok",
    ] {
        assert_eq!(
            probe(&[(1, Some(command), &[2]), (2, Some(&tool), &[])], Some(1)),
            ResumeProbe::Settled,
            "{command}"
        );
    }
    let wrapper = exec_wrapper("claude", "fresh");
    assert_eq!(
        probe(
            &[
                (1, Some(&wrapper), &[2, 4]),
                (2, Some("bwrap --args 3"), &[3]),
                (3, Some("claude"), &[5]),
                (4, Some(DUTY), &[]),
                (5, Some(&tool), &[]),
            ],
            Some(3),
        ),
        ResumeProbe::Settled
    );
}

#[test]
fn resume_probe_preserves_resumes_through_shells_wrappers_and_launchers() {
    let codex = format!("codex resume {CODEX_ID}");
    let grok = format!("grok --resume {CODEX_ID}");
    let kiro = format!("kiro-cli chat --resume-id={KIRO_ID}");
    let agy = format!("agy --conversation {CODEX_ID}");
    for (command, id) in [
        (codex.clone(), CODEX_ID),
        (format!("node /usr/bin/{codex}"), CODEX_ID),
        (grok.clone(), CODEX_ID),
        (kiro.clone(), KIRO_ID),
        (agy, CODEX_ID),
    ] {
        assert_eq!(
            probe(&[(1, Some(&command), &[])], Some(1)),
            ResumeProbe::Resumed(id.into()),
            "{command}"
        );
    }
    for (parent, child, id) in [
        ("zsh".to_owned(), codex.clone(), CODEX_ID),
        (format!("sudo {grok}"), grok.clone(), CODEX_ID),
        (format!("sh -c '{kiro}; echo done'"), kiro, KIRO_ID),
        (
            format!("node /opt/@openai/codex/bin/codex.js resume {CODEX_ID}"),
            format!("codex-x86_64-unknown-linux-musl resume {CODEX_ID}"),
            CODEX_ID,
        ),
    ] {
        assert_eq!(
            probe(&[(1, Some(&parent), &[2]), (2, Some(&child), &[])], Some(2)),
            ResumeProbe::Resumed(id.into()),
            "{parent}"
        );
    }
    // Grok's npm shim spawns the native binary with its own arguments.
    let native = format!("/home/u/.grok/bin/{grok}");
    let shim = format!("node /usr/lib/node_modules/@xai-official/grok/bin/{grok}");
    let npx = format!("npx @xai-official/{grok}");
    let wrapper = exec_wrapper("grok", "shim");
    for tree in [
        [(1, Some(&*shim), &[2][..]), (2, Some(&native), &[])],
        [(1, Some(&npx), &[2]), (2, Some(&native), &[])],
    ] {
        assert_eq!(probe(&tree, Some(1)), ResumeProbe::Resumed(CODEX_ID.into()));
    }
    assert_eq!(
        probe(
            &[
                (1, Some(&wrapper), &[2]),
                (2, Some(&shim), &[3]),
                (3, Some(&native), &[]),
            ],
            Some(2),
        ),
        ResumeProbe::Resumed(CODEX_ID.into())
    );
}

#[test]
fn resume_probe_retries_incomplete_and_changing_prefix_chains() {
    let resumed = format!("codex resume {CODEX_ID}");
    let wrapper = exec_wrapper("codex", CODEX_ID);
    assert_eq!(
        probe(&[(1, None, &[2]), (2, Some(&resumed), &[])], Some(2)),
        ResumeProbe::Resumed(CODEX_ID.into())
    );
    for parent in [
        "sh -c codex --model gpt; codex resume later",
        "sudo codex",
        "doas codex",
        "time codex",
        "timeout 30 codex",
    ] {
        assert_eq!(
            probe(&[(1, Some(parent), &[2]), (2, Some("codex"), &[])], Some(2)),
            ResumeProbe::Unsettled,
            "{parent}"
        );
        assert_eq!(
            probe(
                &[(1, Some(parent), &[3]), (3, Some(&resumed), &[])],
                Some(3)
            ),
            ResumeProbe::Resumed(CODEX_ID.into()),
            "the next provider under {parent} still binds"
        );
    }
    for child in [None, Some("")] {
        assert_eq!(
            probe(&[(1, Some(&wrapper), &[2]), (2, child, &[])], None),
            ResumeProbe::Unsettled
        );
    }
    assert_eq!(probe(&[(1, None, &[])], None), ResumeProbe::Unsettled);
    assert_eq!(
        probe(&[(1, Some(&wrapper), &[2]), (2, Some("bwrap"), &[])], None),
        ResumeProbe::Unsettled
    );
    assert_eq!(
        probe(
            &[
                (1, Some(&wrapper), &[2, 3]),
                (2, Some("codex"), &[]),
                (3, Some("codex"), &[]),
            ],
            None,
        ),
        ResumeProbe::Unsettled
    );
    assert_eq!(
        resume_probe_for_root_with(1, &|_| Some("bwrap".into()), &|pid| vec![pid + 1]),
        ResumeProbe::Unsettled
    );
}

#[cfg(target_os = "linux")]
#[test]
fn resume_provider_stop_avoids_real_multithreaded_proc_children_reads() {
    use crate::testkit::host_pass::{Counter::*, thread_counters};
    std::thread::scope(|scope| {
        let (ready, ready_rx) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            ready.send(()).unwrap();
            let _ = released.recv();
        });
        ready_rx.recv().unwrap();
        let counts = thread_counters();
        let root = std::process::id();
        counts.reset();
        let outcome =
            resume_probe_for_root_with(root, &|_| Some("claude".into()), &crate::proc::children);
        assert_eq!(outcome, ResumeProbe::Settled);
        assert_eq!(counts.get(ProcTaskDir), 0);
        assert_eq!(counts.get(ProcChildren), 0);
        counts.reset();
        let outcome = resume_probe_for_root_with(
            root,
            &|_| Some(exec_wrapper("claude", "fresh")),
            &crate::proc::children,
        );
        assert_eq!(outcome, ResumeProbe::Unsettled);
        assert!(counts.get(ProcTaskDir) >= 1);
        assert!(counts.get(ProcChildren) >= 2);
        drop(release);
    });
}
