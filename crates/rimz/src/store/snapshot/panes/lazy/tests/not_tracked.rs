use super::*;
use crate::agents::{NotTracked, NotTrackedReason};

#[test]
fn unbound_ladder_prefers_hooks_then_mode_then_wired_idle() {
    let hook = NotTracked {
        reason: NotTrackedReason::HooksBlocked,
        fix: "enable hooks, then restart".to_owned(),
    };
    let live = pane("term1", "codex", "/repo/main");
    let actual: Vec<_> = [
        (true, true, true),
        (false, true, true),
        (false, true, false),
        (false, false, true),
        (false, false, false),
    ]
    .into_iter()
    .map(|(has_hook, has_mode, wired)| {
        let untracked = if has_hook {
            BTreeMap::from([("codex".to_owned(), hook.clone())])
        } else {
            BTreeMap::new()
        };
        let kinds = if wired {
            vec!["codex".to_owned()]
        } else {
            vec![]
        };
        let wiring = crate::store::snapshot::panes::PaneWiring {
            kinds: &kinds,
            default_models: &BTreeMap::new(),
            untracked: &untracked,
        };
        unbound_agent_row(
            &live,
            crate::agents::spec_by_kind("codex").unwrap(),
            "/repo/main",
            &wiring,
            || {
                assert!(
                    !has_hook,
                    "a hook blocker must win without evaluating the mode"
                );
                has_mode.then(|| "use interactive mode, then restart".to_owned())
            },
            ago(0),
        )
        .map(|bind| {
            let (row, has_identity) = match bind {
                AgentPaneRow::Idle(row) => (row, true),
                AgentPaneRow::NotTracked { kind, row } => (row, kind.is_some()),
                _ => unreachable!("the unbound rung yields only idle or not-tracked rows"),
            };
            (
                row.is_agent(),
                row.as_process().and_then(|card| card.not_tracked.clone()),
                has_identity,
            )
        })
    })
    .collect();
    let mode = NotTracked {
        reason: NotTrackedReason::ModeNotFollowed,
        fix: "use interactive mode, then restart".to_owned(),
    };
    assert_eq!(
        actual,
        [
            Some((false, Some(hook), true)),
            Some((false, Some(mode.clone()), false)),
            Some((false, Some(mode), false)),
            Some((true, None, true)),
            None
        ]
    );
}
