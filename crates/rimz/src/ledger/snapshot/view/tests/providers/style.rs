use super::*;

#[test]
fn default_emblems_keep_every_rows_leading_spaces() {
    // The emblem literals open with a bare newline so the art sits at
    // column 0 in source; the split must keep each row's leading spaces —
    // a `\` continuation once ate the first row's indent and the art
    // drifted a cell left on screen.
    let art = |kind: &str| default_provider_style(kind).1;
    assert_eq!(art("claude"), [" ▐▛███▜▌", "▝▜█████▛▘", "  ▘▘ ▝▝"]);
    assert_eq!(art("codex"), [" ▗▛███▜▖", "▐▜▌ ▚ ▐▛▌", " ▝▀▀▀▀▀▘"]);
    assert_eq!(art("pi"), [" █▜███▛█", "▝▜▛▀▀▀▜▛▘", " ▝▘   ▝▘"]);
}

#[test]
fn provider_without_the_rate_limit_capability_drops_stray_windows() {
    // Pi declares `rate_limit_windows: false`; Claude declares it true. The
    // same stray session reading must paint a budget bar only where the
    // descriptor declares the surface.
    let reading = window(40, 3_600);
    let pi = agent("pi", "p1", AgentStatus::Idle, 10).limits(vec![reading.clone()]);
    let claude = agent("claude", "c1", AgentStatus::Idle, 10).limits(vec![reading]);

    let snapshot = room(Vec::new(), vec![pi, claude]).with_provider_aggregates(
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let panel = |kind: &str| {
        snapshot
            .providers
            .iter()
            .find(|panel| panel.kind == kind)
            .unwrap_or_else(|| panic!("{kind} panel present"))
    };
    assert!(
        panel("pi").windows.is_empty(),
        "pi's declared absence drops the stray reading"
    );
    assert_eq!(panel("claude").windows.len(), 1);
}
