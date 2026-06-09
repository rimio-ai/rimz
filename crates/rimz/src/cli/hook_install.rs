use super::*;

pub(super) fn ensure_detected_agent_hooks() -> Result<()> {
    let mut missing = Vec::new();

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
        print_hook_consent_gate(&missing, false)?;
        return Ok(());
    }

    let agents_to_install = if std::io::stderr().is_terminal() {
        hook_consent::run_consent_gate(&missing)?
    } else {
        approve_hook_install_text(&missing)?
    };

    for name in agents_to_install {
        let agent = rimz::agents::adapter_by_kind(name)?;
        let report = agent.install_hooks()?;
        {
            let mut stderr = std::io::stderr().lock();
            writeln!(
                stderr,
                "Installed {} hooks at {}",
                report.agent,
                report.config_path.display(),
            )?;
        }
        // A fresh install lands untrusted, so the notice must follow it here
        // — the user is one `/hooks` away from a live channel, not done.
        warn_untrusted_hooks(name, &agent.untrusted_installed_hooks())?;
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
    print_hook_consent_gate(previews, true)?;
    loop {
        let mut stderr = std::io::stderr().lock();
        write!(stderr, "Choose [Enter/d/s]: ")?;
        stderr.flush()?;
        drop(stderr);

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        match answer.trim() {
            "" | "y" | "Y" | "yes" | "YES" | "Yes" => {
                return Ok(previews.iter().map(|preview| preview.agent).collect());
            }
            "d" | "D" => {
                let mut stderr = std::io::stderr().lock();
                for preview in previews {
                    writeln!(stderr, "{}", hook_consent::preview_diff(preview))?;
                }
            }
            "s" | "S" | "n" | "N" | "no" | "NO" | "No" => return Ok(Vec::new()),
            _ => {
                writeln!(
                    std::io::stderr().lock(),
                    "Enter installs all, d shows the diff, s skips."
                )?;
            }
        }
    }
}

/// One consent line for a statusline-style wrap (`statusLine` or
/// `subagentStatusLine`), keeping the change a visible security surface. An
/// unchanged re-install or an agent that manages no such command prints nothing.
fn write_status_line_consent(
    w: &mut impl std::io::Write,
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

fn print_hook_consent_gate(previews: &[HookInstallPreview], interactive: bool) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Rimz: agent hooks are not currently installed for {}.",
        join_agent_names(previews.iter().map(|preview| preview.agent)),
    )?;
    writeln!(stderr, "{}", hook_consent::CONSENT_TEXT_CHANGE_SUMMARY)?;
    writeln!(stderr, "{}", hook_consent::CONSENT_BOUNDARY)?;
    for preview in previews {
        writeln!(
            stderr,
            "  + {}: {} events at {}",
            preview.agent,
            preview.planned_events.len(),
            preview.config_path.display(),
        )?;
        write_status_line_consent(
            &mut stderr,
            "statusLine",
            "report context to Rimz",
            &preview.status_line_change,
        )?;
        write_status_line_consent(
            &mut stderr,
            "subagentStatusLine",
            "report subagent activity to Rimz",
            &preview.subagent_status_line_change,
        )?;
    }
    writeln!(stderr, "{}", hook_consent::CONSENT_REVERSIBLE)?;
    if interactive {
        writeln!(
            stderr,
            "[Enter] install all    [d] show full diff    [s] skip",
        )?;
    }
    if !interactive {
        writeln!(
            stderr,
            "No terminal input is available, so Rimz installs nothing and continues into the room.",
        )?;
    }
    Ok(())
}

fn join_agent_names(names: impl IntoIterator<Item = &'static str>) -> String {
    names.into_iter().collect::<Vec<_>>().join(", ")
}
