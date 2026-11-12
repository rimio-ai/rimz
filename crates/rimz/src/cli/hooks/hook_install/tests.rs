use std::io::Cursor;
use std::path::PathBuf;

use super::*;

fn preview(agent: &'static str, original: Option<&str>, candidate: &str) -> HookInstallPreview {
    let path = home_config_path(agent);
    HookInstallPreview {
        agent,
        files: vec![HookInstallFilePreview {
            path,
            original: original.map(str::to_owned),
            candidate: candidate.to_owned(),
            existed: original.is_some(),
        }],
        planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
        status_line_change: None,
        subagent_status_line_change: None,
    }
}

fn home_config_path(agent: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME is set for CLI render tests");
    PathBuf::from(home).join(format!(".{agent}/settings.json"))
}

fn strip<E: std::fmt::Debug>(
    render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::result::Result<(), E>,
) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render");
    String::from_utf8(stream.into_inner()).expect("utf8")
}

fn drive(previews: &[HookInstallPreview], input: &[u8]) -> (Vec<&'static str>, String) {
    let mut input = Cursor::new(input.to_vec());
    let mut stream = anstream::StripStream::new(Vec::new());
    let selected = prompt_consent(previews, None, &mut input, &mut stream).expect("prompt");
    let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
    (selected, rendered)
}

#[test]
fn prompt_accepts_or_declines_all_agents() {
    let previews = [
        preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
        preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
    ];

    let (selected, _) = drive(&previews, b"y\n");
    assert_eq!(selected, vec!["claude", "codex"]);

    let (selected, _) = drive(&previews, b"\n");
    assert_eq!(selected, vec!["claude", "codex"]);

    let (selected, _) = drive(&previews, b"n\n");
    assert!(selected.is_empty());

    let (selected, rendered) = drive(&previews, b"maybe\nn\n");
    assert!(selected.is_empty());
    assert!(
        rendered.contains("Enter installs or refreshes hooks for every listed agent; n skips.")
    );

    let (selected, rendered) = drive(&previews, b"");
    assert!(selected.is_empty());
    assert!(rendered.starts_with("rimz · first-run setup\n"));

    let mut stream = anstream::StripStream::new(Vec::new());
    let heading = Some("1 of 6 · Reporting hooks");
    prompt_consent(
        &previews,
        heading,
        &mut Cursor::new(b"n\n".to_vec()),
        &mut stream,
    )
    .expect("prompt");
    let rendered = String::from_utf8(stream.into_inner()).expect("utf8");
    assert!(rendered.starts_with("1 of 6 · Reporting hooks\n\nRimZ found 2 coding agents"));
    assert!(!rendered.contains("first-run setup"));

    let rendered = strip(|out| install_selected(&selected, &PendingNotes::default(), out));
    assert!(rendered.contains(
        "Nothing changed - install or refresh agents any time with `rimz hooks install`."
    ));
}

#[test]
fn untrusted_notice_of_a_current_agent_follows_the_heading() {
    let previews = [preview("claude", Some("{}\n"), "{\"hooks\": []}\n")];
    let notes = PendingNotes {
        refused: vec!["pi hooks not installed: refused".to_owned()],
        untrusted: vec![("codex", vec!["stop".to_owned(), "start".to_owned()])],
    };
    let mut stream = anstream::StripStream::new(Vec::new());
    let mut input = Cursor::new(b"n\n".to_vec());
    let selected = prompt_consent(
        &previews,
        Some("1 of 6 · Reporting hooks"),
        &mut input,
        &mut stream,
    )
    .expect("prompt");
    install_selected(&selected, &notes, &mut stream).expect("install");
    let rendered = String::from_utf8(stream.into_inner()).expect("utf8");

    assert!(
        rendered.starts_with("1 of 6 · Reporting hooks\n"),
        "{rendered}"
    );
    assert_eq!(
        rendered
            .matches("! codex ignores 2 hooks until trusted")
            .count(),
        1
    );
    let nothing = rendered.find("Nothing changed").expect("decline line");
    let notice = rendered.find("! codex ignores").expect("trust notice");
    let refused = rendered.find("! pi hooks not installed").expect("refusal");
    assert!(nothing < notice && notice < refused, "{rendered}");
    assert!(!rendered.contains("All set"));
}

#[test]
fn codex_rules_consent_and_dry_run_show_creation_and_removal() {
    for removed in [false, true] {
        let mut codex = preview("codex", Some("hooks\n"), "hooks\n");
        let path = home_config_path("codex")
            .with_file_name("rules")
            .join("rimz.rules");
        codex.files.push(HookInstallFilePreview {
            path,
            original: removed.then(|| "# _rimz_managed\nrule\n".into()),
            candidate: if removed {
                ""
            } else {
                "# _rimz_managed\nrule\n"
            }
            .into(),
            existed: removed,
        });
        let previews = [codex];
        let (_, consent) = drive(&previews, b"n\n");
        let notice = strip(|out| {
            write_noninteractive_notice(out, &previews, HookInstallMode::NoTerminal, None)
        });
        for rendered in [&consent, &notice] {
            assert!(rendered.contains(if removed {
                "Removing Codex's RimZ permission rules file ends its prompt and sandbox bypass for routine `rimz` commands"
            } else {
                "Codex also gets a permission rules file: routine `rimz` commands run without a prompt or sandbox"
            }));
            assert!(rendered.contains("in every Codex session, including outside RimZ"));
            assert!(rendered.contains("[agents] allow-routine-rimz = false"));
        }
        let dry_run = strip(|out| render_dry_run(out, &previews));
        for rendered in [&consent, &dry_run] {
            assert!(rendered.contains("rules/rimz.rules"));
            assert!(rendered.contains(if removed { "removes file" } else { "new file" }));
        }
        assert!(dry_run.contains(if removed {
            "-# _rimz_managed"
        } else {
            "+# _rimz_managed"
        }));
        assert!(dry_run.contains(if removed { "-rule" } else { "@@ new file @@" }));
    }
}

#[test]
fn hook_notices_cover_interactive_and_noninteractive_workflows() {
    let mut additive = preview("claude", Some("{}\n"), "{\"hooks\": []}\n");
    additive.status_line_change = Some(StatusLineChange::Wrapping {
        original: "RIMZ_AGENT_PID=$PPID exec rimz statusline feed".to_owned(),
    });
    let created = preview("codex", None, "{\"hooks\": []}\n");
    let previews = [additive, created];

    let (_, interactive) = drive(&previews, b"n\n");
    let noninteractive =
        strip(|out| write_noninteractive_notice(out, &previews, HookInstallMode::NoTerminal, None));
    let reconnect =
        strip(|out| write_noninteractive_notice(out, &previews, HookInstallMode::Reconnect, None));

    for rendered in [&interactive, &noninteractive, &reconnect] {
        assert!(rendered.contains("RimZ found 2 coding agents: claude, codex."));
        assert!(rendered.contains("~/.claude/settings.json"));
        assert!(rendered.contains("~/.codex/settings.json"));
        assert!(rendered.contains(
            "Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you."
        ));
        assert!(rendered.contains("rimz hooks uninstall"));
        assert!(rendered.contains("rimz hooks install --dry-run"));
        assert!(
            rendered
                .contains("+ wraps your statusline for live context — yours restored on uninstall")
        );
    }
    assert!(interactive.contains("Install or refresh reporting hooks? [Y/n]"));
    assert!(!interactive.contains("No terminal input"));
    assert!(!noninteractive.contains("Install or refresh reporting hooks? [Y/n]"));
    assert!(noninteractive.contains(
        "No terminal input — nothing installed or refreshed. RimZ continues into the room; install or refresh agents later with rimz hooks install."
    ));
    assert!(!reconnect.contains("Install or refresh reporting hooks? [Y/n]"));
    assert!(!reconnect.contains("No terminal input"));
    assert!(reconnect.contains("Not asked on a reconnect — nothing installed or refreshed. Run `rimz hooks install` in a room pane to install."));
    assert!(reconnect.contains("rimz doctor"));
}

#[test]
fn mixed_install_and_upgrade_share_one_summary_and_consent() {
    let created = preview("claude", None, "new-hook\n");
    let upgraded = preview("pi", Some("stale-extension\n"), "current-extension\n");
    let mut antigravity = preview("antigravity", None, "new-hooks\n");
    antigravity.files.push(HookInstallFilePreview {
        path: home_config_path("antigravity-statusline"),
        original: Some("old-statusline\n".to_owned()),
        candidate: "new-statusline\n".to_owned(),
        existed: true,
    });
    let previews = [created, upgraded, antigravity];

    let (selected, summary) = drive(&previews, b"y\n");
    assert_eq!(selected, vec!["claude", "pi", "antigravity"]);
    assert_eq!(
        summary
            .matches("Install or refresh reporting hooks? [Y/n]")
            .count(),
        1
    );
    for path in [
        "~/.claude/settings.json",
        "~/.pi/settings.json",
        "~/.antigravity/settings.json",
        "~/.antigravity-statusline/settings.json",
    ] {
        assert!(summary.contains(path), "missing {path}:\n{summary}");
    }
    assert!(summary.contains("new file"));
    assert!(summary.contains("updates existing config"));
    assert!(!summary.contains("@@"), "consent stays concise:\n{summary}");

    let (selected, _) = drive(&previews, b"n\n");
    assert!(selected.is_empty());

    let dry_run = strip(|out| render_dry_run(out, &previews));
    for content in [
        "+new-hook",
        "-stale-extension",
        "+current-extension",
        "+new-hooks",
        "-old-statusline",
        "+new-statusline",
    ] {
        assert!(dry_run.contains(content), "missing {content}:\n{dry_run}");
    }
}

#[test]
fn dry_run_renders_existing_and_new_unified_diffs_without_prompt() {
    let mut claude = preview(
        "claude",
        Some("alpha\nkeep\nold\nomega\n"),
        "alpha\nkeep\nnew\nomega\n",
    );
    claude.files.push(HookInstallFilePreview {
        path: home_config_path("claude-statusline"),
        original: Some("old-status\n".to_owned()),
        candidate: "new-status\n".to_owned(),
        existed: true,
    });
    let previews = [claude, preview("codex", None, "one\ntwo\n")];

    let rendered = strip(|w| render_dry_run(w, &previews));

    assert!(rendered.contains("/dev/null"));
    assert!(rendered.contains("~/.claude/settings.json"));
    assert!(rendered.contains("~/.codex/settings.json"));
    assert!(rendered.contains("@@"));
    assert!(rendered.contains("@@ new file @@"));
    assert!(rendered.contains("-old"));
    assert!(rendered.contains("+new"));
    assert!(rendered.contains(".claude-statusline/settings.json"));
    assert!(rendered.contains("-old-status"));
    assert!(rendered.contains("+new-status"));
    assert!(rendered.contains("+one"));
    assert!(!rendered.contains("Install or refresh reporting hooks?"));
}

#[test]
fn dry_run_names_every_cursor_config_file() {
    let mut cursor = preview("cursor", Some("{}\n"), "{\"hooks\": {}}\n");
    cursor.files[0].path = cursor.files[0].path.with_file_name("hooks.json");
    let cli_config = home_config_path("cursor").with_file_name("cli-config.json");
    cursor.files.push(HookInstallFilePreview {
        path: cli_config.clone(),
        original: None,
        candidate: "{\"statusLine\": {}}\n".to_owned(),
        existed: false,
    });
    let rendered = strip(|out| render_dry_run(out, &[cursor]));
    assert!(rendered.contains("~/.cursor/hooks.json"));
    assert!(rendered.contains("~/.cursor/cli-config.json"));
    assert_eq!(rendered.matches("@@").count(), 4);
}

#[test]
fn completed_hook_reports_render_dispositions_files_and_footer() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks.json");
    let cli_config = dir.path().join("cli-config.json");
    std::fs::write(&hooks, "{}").unwrap();
    std::fs::write(&cli_config, "{}").unwrap();
    let report = HookInstallReport {
        agent: "cursor",
        files: vec![
            rimz::agents::HookInstallFileReport {
                path: hooks.clone(),
                existed: true,
            },
            rimz::agents::HookInstallFileReport {
                path: cli_config.clone(),
                existed: false,
            },
            rimz::agents::HookInstallFileReport {
                path: dir.path().join("rimz.rules"),
                existed: true,
            },
        ],
        installed_events: vec!["stop".to_owned()],
    };

    for (disposition, summary) in [
        (InstallDisposition::Installed, "installed 1 hooks"),
        (InstallDisposition::Refreshed, "refreshed 1 hooks"),
        (InstallDisposition::Current, "hooks up to date (1)"),
    ] {
        let rendered = strip(|out| write_install_result(out, &report, disposition, 8));
        let mut lines = rendered.lines();
        let headline = lines.next().unwrap();
        assert!(headline.starts_with(&format!("✓ cursor    {summary}")));
        assert!(
            headline.width() <= RESULT_WIDTH,
            "headline passes 80 columns:\n{rendered}"
        );
        let path = format!(" → {}", hooks.display());
        let named = format!("✓ cursor    {summary}{path}").width() <= RESULT_WIDTH;
        assert_eq!(
            headline,
            if named {
                format!("✓ cursor    {summary}{path}")
            } else {
                format!("✓ cursor    {summary}")
            },
            "the headline names the first file exactly when it fits:\n{rendered}"
        );
        let unnamed = if named {
            &report.files[1..]
        } else {
            &report.files[..]
        };
        let rest: Vec<_> = lines.collect();
        let expected: Vec<_> = unnamed
            .iter()
            .map(|file| {
                let label = if file.path.exists() {
                    "config"
                } else {
                    "removed"
                };
                format!("            {label} → {}", file.path.display())
            })
            .collect();
        assert_eq!(
            rest, expected,
            "every unnamed file gets its own line:\n{rendered}"
        );
        assert!(!rendered.contains(" +"), "no file is counted:\n{rendered}");
    }
    let long = HookInstallReport {
        agent: "cursor",
        files: vec![rimz::agents::HookInstallFileReport {
            path: PathBuf::from(format!("/{}/hooks.json", "d".repeat(80))),
            // A missing path that existed reads as removed, so this
            // stand-in for an on-disk file says it was not there before.
            existed: false,
        }],
        installed_events: vec!["stop".to_owned()],
    };
    let rendered = strip(|out| write_install_result(out, &long, InstallDisposition::Current, 6));
    assert_eq!(
        rendered,
        format!(
            "✓ cursor  hooks up to date (1)\n          config → {}\n",
            long.files[0].path.display()
        )
    );
    let two = HookInstallReport {
        agent: "codex",
        files: vec![
            rimz::agents::HookInstallFileReport {
                path: PathBuf::from("/x/config.toml"),
                existed: false,
            },
            rimz::agents::HookInstallFileReport {
                path: PathBuf::from("/x/rules/rimz.rules"),
                existed: false,
            },
        ],
        installed_events: vec!["stop".to_owned()],
    };
    let rendered = strip(|out| write_install_result(out, &two, InstallDisposition::Installed, 6));
    assert_eq!(
        rendered,
        "✓ codex   installed 1 hooks → /x/config.toml\n          config → /x/rules/rimz.rules\n"
    );
    let gone = HookInstallReport {
        agent: "codex",
        files: vec![
            rimz::agents::HookInstallFileReport {
                path: PathBuf::from("/x/config.toml"),
                existed: false,
            },
            rimz::agents::HookInstallFileReport {
                path: PathBuf::from("/x/rules/rimz.rules"),
                existed: true,
            },
        ],
        installed_events: vec!["stop".to_owned()],
    };
    let rendered = strip(|out| write_install_result(out, &gone, InstallDisposition::Refreshed, 6));
    assert_eq!(
        rendered,
        "✓ codex   refreshed 1 hooks → /x/config.toml\n          removed → /x/rules/rimz.rules\n"
    );
    let short = HookInstallReport {
        agent: "pi",
        files: vec![rimz::agents::HookInstallFileReport {
            path: PathBuf::from("/x/hooks.json"),
            existed: false,
        }],
        installed_events: vec!["stop".to_owned()],
    };
    let rendered = strip(|out| write_install_result(out, &short, InstallDisposition::Installed, 2));
    assert_eq!(rendered, "✓ pi  installed 1 hooks → /x/hooks.json\n");

    let uninstall = rimz::agents::HookUninstallReport {
        agent: "cursor",
        files: report.files,
        removed_events: vec!["stop".to_owned()],
    };
    let rendered = strip(|out| write_uninstall_result(out, &uninstall));
    assert!(rendered.contains(&format!("✓ cursor  removed 1 hooks → {}", hooks.display())));
    assert!(rendered.contains(&format!("config → {}", cli_config.display())));

    let empty = rimz::agents::HookUninstallReport {
        agent: "codex",
        files: Vec::new(),
        removed_events: Vec::new(),
    };
    let rendered = strip(|out| write_uninstall_result(out, &empty));
    assert_eq!(rendered, "codex — no RimZ-managed hooks found\n");

    for pending in [false, true] {
        let rendered = strip(|out| write_post_install_footer(out, pending));
        assert!(rendered.contains("rimz hooks uninstall"));
        assert!(rendered.contains("rimz hooks install --dry-run"));
        assert_eq!(
            rendered.contains("All set — your agents appear in the sidebar as they run."),
            !pending,
            "All set prints only when nothing is pending:\n{rendered}"
        );
    }

    let refusal = format!(
        "pi hooks not installed: refusing to overwrite an unmarked user extension at {}; move it aside or remove it",
        "/home/me/.pi/agent/extensions/rimz.ts"
    );
    let rendered = strip(|out| write_pending_line(out, &refusal));
    let lines: Vec<_> = rendered.lines().collect();
    assert!(lines.len() > 1, "{rendered}");
    assert!(
        lines[0].starts_with("! pi hooks not installed: "),
        "{rendered}"
    );
    for line in &lines {
        assert!(line.width() <= RESULT_WIDTH, "{line:?} passes 80 columns");
    }
    for line in &lines[1..] {
        assert!(
            line.starts_with("  ") && !line.starts_with("   "),
            "{line:?}"
        );
    }
    assert!(rendered.contains(" /home/me/.pi/agent/extensions/rimz.ts;"));

    let rendered = strip(|out| write_untrusted_hooks_notice("codex", &["stop".into()], out));
    let mut lines = rendered.lines();
    assert_eq!(lines.next(), Some("! codex ignores 1 hook until trusted"));
    assert!(lines.next().is_some_and(|line| line.starts_with("  fix: ")));
    assert_eq!(lines.next(), None);
    assert_eq!(
        strip(|out| write_untrusted_hooks_notice("codex", &[], out)),
        ""
    );
}
