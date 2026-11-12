//! The context a command adds when it cannot write the RimZ home, as inside a provider's
//! command sandbox, and the remedy that sandbox needs.

/// How an agent runs `command` so its routine rule reaches past the sandbox.
pub(super) fn sandbox_remedy(command: &str) -> String {
    format!(
        "Run `{command} …` as its own command, without a chain, pipe, redirect, or `VAR=` \
         prefix, so its routine rule runs outside the sandbox; if `allow-routine-rimz` is off, \
         approve the sandbox's escalation. If Codex's routine rules predate this RimZ, refresh \
         them with `rimz hooks install codex` from a user shell and restart the agent"
    )
}

pub(super) fn is_read_only(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ReadOnlyFilesystem)
    })
}

/// Adds the sandbox context and remedy for `command` when `error` holds a read-only
/// filesystem; any other error passes through unchanged.
pub(super) fn hint(error: anyhow::Error, command: &str, action: Option<&str>) -> anyhow::Error {
    if !is_read_only(&error) {
        return error;
    }
    let unchanged = action
        .map(|action| format!("nothing was {action}: "))
        .unwrap_or_default();
    error.context(format!(
        "{unchanged}this command cannot write the RimZ home, for example inside a \
         provider's command sandbox. {}",
        sandbox_remedy(command)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_only_for_read_only_filesystem() {
        for (command, action) in [
            ("rimz wait", "armed"),
            ("rimz wait", "canceled"),
            ("rimz workflow", "started"),
        ] {
            for kind in [
                std::io::ErrorKind::ReadOnlyFilesystem,
                std::io::ErrorKind::PermissionDenied,
            ] {
                let error = anyhow::Error::from(rimz::harness::schedule::arm::ArmFailure::State(
                    Box::new(rimz::disk::lock::LockErr::Open {
                        path: "rimz-home/loops/loop-arming.lock".into(),
                        source: std::io::Error::from(kind),
                    }),
                ))
                .context("updating wait");
                let original = format!("{error:#}");
                let report = format!("{:#}", hint(error, command, Some(action)));
                if kind != std::io::ErrorKind::ReadOnlyFilesystem {
                    assert_eq!(report, original);
                    continue;
                }
                assert!(
                    report.contains(&format!("nothing was {action}")),
                    "{report}"
                );
                assert!(report.contains(&original), "{report}");
                for text in [
                    "provider's command sandbox",
                    &format!("Run `{command} …` as its own command"),
                    "chain, pipe, redirect, or `VAR=` prefix",
                    "allow-routine-rimz",
                    "escalation",
                    "`rimz hooks install codex`",
                ] {
                    assert!(report.contains(text), "{report}");
                }
            }
        }
    }
}
