use super::*;
use std::collections::BTreeSet;
use std::io::{BufRead, Write};

pub(crate) fn detected_installable_adapters() -> Vec<&'static dyn rimz::agents::AgentAdapter> {
    let mut detected = Vec::new();
    for agent in rimz::agents::ADAPTERS {
        let descriptor = agent.descriptor();
        if which::which(descriptor.kind).is_err() {
            continue;
        }

        if !descriptor.capabilities.hook_install {
            let reason = descriptor
                .hook_install_unavailable
                .unwrap_or("hook install is not supported for this adapter");
            tracing::warn!(
                agent = descriptor.kind,
                reason,
                "detected agent cannot be wired automatically",
            );
            continue;
        }

        detected.push(*agent);
    }
    detected
}

pub(super) fn ensure_detected_agent_hooks() -> Result<()> {
    let mut missing = Vec::new();

    for agent in detected_installable_adapters() {
        let descriptor = agent.descriptor();
        if !agent.hooks_installed() {
            missing.push(agent.preview_hook_install()?);
            continue;
        }

        warn_untrusted_hooks(descriptor.kind, &agent.untrusted_installed_hooks())?;
    }

    if missing.is_empty() {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        print_hook_consent_gate(&missing)?;
        return Ok(());
    }

    let agents_to_install = if std::io::stderr().is_terminal() {
        hook_consent::run_consent_gate(&missing)?
    } else {
        approve_hook_install_text(&missing)?
    };

    if agents_to_install.is_empty() {
        writeln!(
            std::io::stderr().lock(),
            "Nothing changed - wire agents any time with `rimz hooks install`."
        )?;
        return Ok(());
    }

    let installed = agents_to_install.iter().copied().collect::<BTreeSet<_>>();
    for name in agents_to_install {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let report = agent.install_hooks()?;
        {
            let mut stderr = std::io::stderr().lock();
            writeln!(
                stderr,
                "✓ {} - {} hooks added at {}",
                report.agent,
                report.installed_events.len(),
                report.config_path.display(),
            )?;
        }
        // A fresh install lands untrusted, so the notice must follow it here
        // — the user is one `/hooks` away from a live channel, not done.
        warn_untrusted_hooks(name, &agent.untrusted_installed_hooks())?;
    }
    {
        let mut stderr = std::io::stderr().lock();
        for preview in &missing {
            if !installed.contains(preview.agent) {
                writeln!(
                    stderr,
                    "· {} - skipped; wire it later with `rimz hooks install {}`",
                    preview.agent, preview.agent,
                )?;
            }
        }
        writeln!(
            stderr,
            "All set - your agents appear in the sidebar as they run."
        )?;
    }

    Ok(())
}

/// Stderr notice for hooks the agent's own trust gate still skips: the gate
/// silences every untrusted hook with no signal of its own, so the start
/// notice is where the dead channel becomes visible. Rimz cannot trust on
/// the user's behalf — only the agent's own UI can — so this warns with the
/// fix rather than gating the start. No-op when `untrusted` is empty.
fn warn_untrusted_hooks(kind: &str, untrusted: &[String]) -> Result<()> {
    if untrusted.is_empty() {
        return Ok(());
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "{kind} hooks are installed but untrusted ({}) — {kind} silently skips them; {}",
        untrusted.join(", "),
        rimz::agents::hook_trust_fix(kind),
    )?;
    Ok(())
}

fn approve_hook_install_text(previews: &[HookInstallPreview]) -> Result<Vec<&'static str>> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut stderr = std::io::stderr().lock();
    wizard_text_flow(previews, &mut input, &mut stderr)
}

fn wizard_text_flow(
    previews: &[HookInstallPreview],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<&'static str>> {
    write_consent_intro(previews, out)?;
    let mut selected = Vec::new();
    for (idx, preview) in previews.iter().enumerate() {
        writeln!(out)?;
        writeln!(
            out,
            "rimz - first-run setup - {} ({} of {})",
            preview.agent,
            idx + 1,
            previews.len(),
        )?;
        write_agent_consent(preview, out)?;
        loop {
            write!(out, "Add {} hooks? [Y/n/d=diff]: ", preview.agent)?;
            out.flush()?;
            let mut answer = String::new();
            if input.read_line(&mut answer)? == 0 {
                return Ok(selected);
            }
            match answer.trim() {
                "" | "y" | "Y" | "yes" | "YES" | "Yes" => {
                    selected.push(preview.agent);
                    break;
                }
                "n" | "N" | "no" | "NO" | "No" => break,
                "d" | "D" => {
                    writeln!(out, "{}", hook_consent::preview_diff(preview))?;
                }
                _ => {
                    writeln!(out, "Enter adds this agent, n skips it, d shows its diff.")?;
                }
            }
        }
    }
    Ok(selected)
}

fn write_consent_intro(
    previews: &[HookInstallPreview],
    out: &mut dyn std::io::Write,
) -> Result<()> {
    let agent_word = if previews.len() == 1 {
        "agent"
    } else {
        "agents"
    };
    writeln!(out, "rimz - first-run setup")?;
    writeln!(
        out,
        "Rimz found {} coding {agent_word} on this machine: {}.",
        previews.len(),
        join_agent_names(previews.iter().map(|preview| preview.agent)),
    )?;
    writeln!(out, "{}", hook_consent::CONSENT_INTRO)?;
    writeln!(
        out,
        "To show what an agent is doing, it adds reporting hooks to the agent's config.",
    )?;
    writeln!(out, "{}", hook_consent::CONSENT_BOUNDARY)?;
    Ok(())
}

fn write_agent_consent(preview: &HookInstallPreview, out: &mut dyn std::io::Write) -> Result<()> {
    writeln!(
        out,
        "Add {} reporting hooks to {}?",
        preview.planned_events.len(),
        preview.agent,
    )?;
    writeln!(out, "  config   {}", preview.config_path.display())?;
    writeln!(out, "  change   additive - your existing hooks are kept")?;
    write_status_line_consent(
        out,
        "statusLine",
        "report context to Rimz",
        &preview.status_line_change,
    )?;
    write_status_line_consent(
        out,
        "subagentStatusLine",
        "report subagent activity to Rimz",
        &preview.subagent_status_line_change,
    )?;
    writeln!(out, "  undo     rimz hooks uninstall {}", preview.agent)?;
    Ok(())
}

fn print_hook_consent_gate(previews: &[HookInstallPreview]) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    write_consent_intro(previews, &mut stderr)?;
    for preview in previews {
        writeln!(stderr)?;
        write_agent_consent(preview, &mut stderr)?;
    }
    writeln!(stderr, "{}", hook_consent::CONSENT_REVERSIBLE)?;
    writeln!(
        stderr,
        "No terminal input is available, so Rimz installs nothing and continues into the room.",
    )?;
    Ok(())
}

/// One consent line for a statusline-style wrap (`statusLine` or
/// `subagentStatusLine`), keeping the change a visible security surface. An
/// unchanged re-install or an agent that manages no such command prints nothing.
fn write_status_line_consent(
    w: &mut dyn std::io::Write,
    key: &str,
    purpose: &str,
    change: &Option<StatusLineChange>,
) -> Result<()> {
    match change {
        Some(StatusLineChange::Added) => writeln!(
            w,
            "      also sets your {key} to {purpose} (removed on uninstall)",
        )?,
        Some(StatusLineChange::Wrapping { original }) => writeln!(
            w,
            "      also wraps your {key} command ({original}) — restored on uninstall",
        )?,
        Some(StatusLineChange::Unchanged) | None => {}
    }
    Ok(())
}

fn join_agent_names(names: impl IntoIterator<Item = &'static str>) -> String {
    names.into_iter().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use super::*;

    fn preview(agent: &'static str, original: Option<&str>, candidate: &str) -> HookInstallPreview {
        HookInstallPreview {
            agent,
            config_path: PathBuf::from(format!("/home/me/.{agent}/config")),
            planned_events: vec!["SessionStart".to_owned(), "PreToolUse".to_owned()],
            original_config: original.map(str::to_owned),
            candidate_config: candidate.to_owned(),
            merged: original.is_some(),
            status_line_change: None,
            subagent_status_line_change: None,
        }
    }

    #[test]
    fn text_wizard_accepts_default_yes_and_explicit_no() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];
        let mut input = Cursor::new(b"\nn\n".to_vec());
        let mut out = Vec::new();

        let selected = wizard_text_flow(&previews, &mut input, &mut out).expect("wizard");

        assert_eq!(selected, vec!["claude"]);
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("rimz - first-run setup - claude (1 of 2)"));
        assert!(rendered.contains("rimz - first-run setup - codex (2 of 2)"));
    }

    #[test]
    fn text_wizard_prints_current_agent_diff_and_reasks() {
        let previews = [preview("claude", Some("old\n"), "new\n")];
        let mut input = Cursor::new(b"d\n\n".to_vec());
        let mut out = Vec::new();

        let selected = wizard_text_flow(&previews, &mut input, &mut out).expect("wizard");

        assert_eq!(selected, vec!["claude"]);
        let rendered = String::from_utf8(out).expect("utf8");
        assert!(rendered.contains("--- /home/me/.claude/config"));
        assert!(rendered.contains("-old"));
        assert!(rendered.contains("+new"));
    }

    #[test]
    fn text_wizard_eof_keeps_prior_choices_without_approving_current_agent() {
        let previews = [
            preview("claude", Some("{}\n"), "{\"hooks\": []}\n"),
            preview("codex", Some("{}\n"), "{\"hooks\": []}\n"),
        ];
        let mut input = Cursor::new(b"\n".to_vec());
        let mut out = Vec::new();

        let selected = wizard_text_flow(&previews, &mut input, &mut out).expect("wizard");

        assert_eq!(selected, vec!["claude"]);
    }
}
