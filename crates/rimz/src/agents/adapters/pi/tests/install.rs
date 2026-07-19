use super::*;

use crate::agents::AgentErr;

fn managed_event_names() -> Vec<String> {
    PI_HOOKS.iter().map(|hook| hook.event.to_owned()).collect()
}

#[test]
fn managed_extension_install_preview_and_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions").join("rimz.ts");

    let report = PI_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(report.agent, "pi");
    assert!(!report.files[0].existed);
    assert_eq!(report.installed_events, managed_event_names());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(PI_MANAGED_SOURCE.installed_at(&path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));

    let stale = "// still _rimz_managed\n// older RimZ source\n";
    std::fs::write(&path, stale).unwrap();
    assert!(PI_MANAGED_SOURCE.installed_at(&path));
    assert!(PI_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(PI_MANAGED_SOURCE.install_into(&path).unwrap().files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION_SOURCE);
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));

    let preview = PI_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert_eq!(preview.agent, "pi");
    assert!(preview.files[0].existed);
    assert_eq!(preview.files[0].candidate, EXTENSION_SOURCE);

    let removed = PI_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert!(removed.files[0].existed);
    assert_eq!(removed.removed_events, managed_event_names());
    assert!(!path.exists());
    assert!(!PI_MANAGED_SOURCE.installed_at(&path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&path));
    assert!(!PI_MANAGED_SOURCE.uninstall_from(&path).unwrap().files[0].existed);

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// the user's own extension\n").unwrap();
    assert!(matches!(
        PI_MANAGED_SOURCE.install_into(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    assert!(matches!(
        PI_MANAGED_SOURCE.preview_at(&user_path).unwrap_err(),
        AgentErr::Install { agent: "pi", .. }
    ));
    let report = PI_MANAGED_SOURCE.uninstall_from(&user_path).unwrap();
    assert!(report.files[0].existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// the user's own extension\n"
    );
    assert!(!PI_MANAGED_SOURCE.installed_at(&user_path));
    assert!(!PI_MANAGED_SOURCE.upgrade_available_at(&user_path));
}

/// The extension is TypeScript RimZ owns but never executes under `cargo`, so
/// this is the only cross-check that the Rust hook catalog and the wire the
/// extension actually posts agree.
#[test]
fn extension_source_wires_every_catalog_event() {
    for (marker, contract) in [
        (
            "_rimz_managed",
            "the install marker install/uninstall keys on",
        ),
        (
            r#"["hooks", "feed", "--source", "pi"]"#,
            "the decision channel every event is posted through",
        ),
        ("RIMZ_AGENT_PID", "PID attribution for the owning agent"),
        ("RIMZ_BIN", "the resolved rimz binary the extension spawns"),
        ("PI_VERSION", "the upstream version stamped on the wire"),
        (
            "has_ui: ctx?.hasUI === true",
            "the headless gate `decode_hook` reads to suppress an ask",
        ),
        (
            "hasAgentSettled",
            "the settled boundary the terminal verdict rides",
        ),
        (
            "getContextUsage",
            "the context gauge stamped on every envelope",
        ),
        ("costBySession", "cumulative cost accumulation per session"),
        ("verdictBySession", "the in-band turn verdict"),
        ("last_assistant_message", "the supervised-run final message"),
        ("total_cost_usd", "the realtime cost field"),
        ("cache_write_input_tokens", "the per-call token split"),
        ("rate_limits", "best-effort provider windows"),
        ("compaction_reason", "the compaction cause"),
        ("compaction_will_retry", "overflow compaction retry"),
        ("session_name", "the `/name` title"),
        ("tool_details:", "the questionnaire answer payload"),
        ("ev?.result?.details", "where the answers are read from"),
        (
            r#"const PARENT_SESSION_ENV = "RIMZ_PI_PARENT_SESSION""#,
            "the child-session lineage marker",
        ),
        (
            r#"Symbol.for("rimz.pi.primary-session")"#,
            "primary-session identification",
        ),
        (
            "!isPrimary && id && parentId && parentId !== id",
            "the child admission rule mirroring subagent quarantine",
        ),
        (
            "process.env.PI_SUBAGENT_CHILD_AGENT",
            "child agent detection",
        ),
        ("feedChildStart(ctx)", "child start feed"),
        ("feedChildStop(ctx, verdict)", "child stop feed"),
        (
            r#"subagent_source: "pi-session""#,
            "the subagent source discriminator",
        ),
        (r#"ev?.reason === "reload""#, "the `/reload` shutdown skip"),
        ("block: true", "the awaited pre-tool gate"),
    ] {
        assert!(
            EXTENSION_SOURCE.contains(marker),
            "extension must carry {contract}: {marker}"
        );
    }

    // Retired approaches, each replaced by the lineage markers above
    // (`cb43773a9 refactor(pi): self-identify child session lineage`).
    for (marker, why) in [
        ("pi.events.on", "superseded by the `pi.on` registration API"),
        (
            "pi-subagents:manager",
            "child rows now self-identify through RimZ lineage markers",
        ),
        (
            "addSessionCost(sessionId(ctx), last?.usage",
            "agent_end's last message is the final turn_end usage and must not add cost again",
        ),
    ] {
        assert!(!EXTENSION_SOURCE.contains(marker), "{why}: {marker}");
    }

    for hook in PI_HOOKS {
        let event = hook.event;
        let registered = match event {
            "subagent_started" | "subagent_stopped" => {
                EXTENSION_SOURCE.contains(&format!("feedSubagent(\"{event}\""))
            }
            _ => EXTENSION_SOURCE.contains(&format!("pi.on(\"{event}\"")),
        };
        assert!(registered, "extension registers {event}",);
    }
}
