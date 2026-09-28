use super::*;
use rimz::remote::reachability::FooterPhase;

#[test]
fn panel_requires_stdout_tty_and_progress_permission() {
    assert!(panel_allowed(true, None, None, None));
    assert!(!panel_allowed(false, None, None, None));
    assert!(!panel_allowed(true, Some("1"), None, None));
    assert!(panel_allowed(true, Some("0"), None, None));
    assert!(!panel_allowed(true, None, Some("codex"), None));
    assert!(!panel_allowed(true, None, None, Some("dumb")));
}

#[test]
fn tiny_width_truncates_on_unicode_cell_boundaries() {
    assert_eq!(truncate_width("⚡ abc", 3), "⚡ ");
    assert_eq!(truncate_width("abc", 0), "");
}

#[test]
fn rows_share_one_centered_block_left_edge() {
    let rows = vec![
        DisplayRow {
            text: "short".to_owned(),
            color: Color::Reset,
            bold: false,
            dim: false,
        },
        DisplayRow {
            text: "twelve cells".to_owned(),
            color: Color::Reset,
            bold: false,
            dim: false,
        },
    ];

    let layout = panel_layout(40, 20, &rows);

    assert_eq!(layout.x0, 14);
    assert_eq!(layout.first_y, 9);
    assert_eq!(layout.row_count, 2);
}

#[test]
fn failure_rows_hold_the_error_fix_and_recent_warnings() {
    let details = [
        "remote path: workspace/missing".to_owned(),
        "fix alias".to_owned(),
    ];
    let logs = (1..=7)
        .map(|index| format!("warning {index}"))
        .collect::<Vec<_>>();

    let rows = failure_rows("remote path does not exist", &details, &logs);
    let text = rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>();

    assert_eq!(text[0], "✗ remote path does not exist");
    assert_eq!(text[1..3], details);
    assert_eq!(
        text[3..8],
        [
            "⚠  warning 3",
            "⚠  warning 4",
            "⚠  warning 5",
            "⚠  warning 6",
            "⚠  warning 7",
        ]
    );
    assert_eq!(text[9], "press any key to exit");
    assert!(rows[0].bold);
    assert_eq!(rows[0].color, Color::Red);
    assert!(rows[3..8].iter().all(|row| row.dim));
    assert!(rows[9].dim);
}

#[test]
fn recovery_panel_rows_pin_checkpoint_phase_and_handoff_matrix() {
    use rimz::remote::reachability::DialPlan;
    use rimz::remote::recovery::{HandoffStage, internet_probe_from_env};

    let internet = internet_probe_from_env().expect("default internet checkpoint");
    let server = DialPlan {
        host: "dev-box".to_owned(),
        port: 22,
    };
    for connect in [ConnectStage::Initial, ConnectStage::Recovery] {
        for has_internet in [false, true] {
            for error in [None, Some("Permission denied (publickey).".to_owned())] {
                for (reachable, tun, attempt, glyph, color, detail, success) in [
                    (
                        None,
                        false,
                        1,
                        '○',
                        Color::DarkGrey,
                        "dev-box:22",
                        "dev-box:22",
                    ),
                    (
                        Some(true),
                        false,
                        1,
                        '✓',
                        Color::Green,
                        "dev-box:22",
                        "dev-box:22",
                    ),
                    (
                        Some(false),
                        false,
                        1,
                        '✗',
                        Color::Red,
                        "dev-box:22",
                        "dev-box:22",
                    ),
                    (
                        Some(true),
                        false,
                        2,
                        '!',
                        Color::Yellow,
                        "dev-box:22 · answers TCP · SSH failing",
                        "dev-box:22",
                    ),
                    (
                        Some(true),
                        true,
                        1,
                        '✓',
                        Color::Green,
                        "dev-box:22 · via TUN tun0 · TCP check skipped",
                        "dev-box:22 · via TUN tun0 · TCP check skipped",
                    ),
                    (
                        Some(true),
                        true,
                        2,
                        '!',
                        Color::Yellow,
                        "dev-box:22 · via TUN tun0 · SSH failing",
                        "dev-box:22 · via TUN tun0 · TCP check skipped",
                    ),
                ] {
                    for handoff in [HandoffStage::Multiplexer, HandoffStage::WebTunnel] {
                        let mut panel = RecoveryPanel::new(
                            connect,
                            handoff,
                            "dev-box",
                            has_internet.then_some(&internet),
                            Some(&server),
                        );
                        panel.begin_wait();
                        panel.note_attempt(attempt);
                        panel.note_internet(false);
                        if let Some(reachable) = reachable {
                            panel.note_server(reachable);
                        }
                        if tun {
                            panel.note_server_tun("tun0");
                        }
                        panel.note_ssh_error(error.clone());
                        let (label, opening) = match handoff {
                            HandoffStage::Multiplexer => ("Multiplexer", "attaching…"),
                            HandoffStage::WebTunnel => ("Web tunnel", "opening…"),
                        };
                        for (phase, session, pending) in [
                            (
                                FooterPhase::Connecting,
                                if connect == ConnectStage::Initial {
                                    "connecting…".to_owned()
                                } else {
                                    "reconnecting…".to_owned()
                                },
                                false,
                            ),
                            (
                                FooterPhase::NextAttemptIn(Duration::from_millis(11_100)),
                                error.as_ref().map_or_else(
                                    || "retry in 12s".to_owned(),
                                    |error| format!("{error} · retry in 12s"),
                                ),
                                false,
                            ),
                            (
                                FooterPhase::WaitingForNetwork,
                                if has_internet {
                                    "waiting"
                                } else {
                                    "waiting for network"
                                }
                                .to_owned(),
                                has_internet,
                            ),
                        ] {
                            let rows =
                                display_rows(&panel.frame(Duration::from_secs(133), phase), 2);
                            let mut expected = vec![
                                (
                                    if connect == ConnectStage::Initial {
                                        "⚡ Connecting to dev-box"
                                    } else {
                                        "⚡ Connection to dev-box lost"
                                    }
                                    .to_owned(),
                                    Color::Yellow,
                                    true,
                                    false,
                                ),
                                (
                                    if connect == ConnectStage::Initial {
                                        format!("attempt {attempt} · 2m 13s · Ctrl-C stops")
                                    } else {
                                        format!("down 2m 13s · attempt {attempt} · Ctrl-C stops")
                                    },
                                    Color::DarkGrey,
                                    false,
                                    true,
                                ),
                                (String::new(), Color::Reset, false, false),
                            ];
                            if has_internet {
                                let (symbol, color, detail) =
                                    if phase == FooterPhase::WaitingForNetwork {
                                        (
                                            '⠹',
                                            Color::Yellow,
                                            "cp.cloudflare.com · waiting for network",
                                        )
                                    } else {
                                        ('✗', Color::Red, "cp.cloudflare.com")
                                    };
                                expected.push((
                                    format!("{symbol}  Internet     {detail}"),
                                    color,
                                    false,
                                    false,
                                ));
                            }
                            expected.push((
                                format!("{glyph}  Server       {detail}"),
                                color,
                                false,
                                false,
                            ));
                            expected.push((
                                format!(
                                    "{}  SSH session  {session}",
                                    if pending { '○' } else { '⠹' }
                                ),
                                if pending {
                                    Color::DarkGrey
                                } else {
                                    Color::Yellow
                                },
                                false,
                                pending,
                            ));
                            expected.push((
                                format!("○  {label:<12} waiting"),
                                Color::DarkGrey,
                                false,
                                true,
                            ));
                            assert_eq!(
                                rows.iter()
                                    .map(|r| (r.text.clone(), r.color, r.bold, r.dim))
                                    .collect::<Vec<_>>(),
                                expected
                            );
                        }
                        panel.note_master_ready();
                        let frame = panel.frame(Duration::from_secs(133), FooterPhase::Connecting);
                        for (rows, symbol) in [
                            (display_rows(&frame, 2), '⠹'),
                            (frame_rows(&frame, '→'), '→'),
                        ] {
                            let mut expected = vec![
                                (
                                    "⚡ Connected to dev-box".to_owned(),
                                    Color::Green,
                                    true,
                                    false,
                                ),
                                (
                                    "opening session… · this can take a few seconds".to_owned(),
                                    Color::DarkGrey,
                                    false,
                                    true,
                                ),
                                (String::new(), Color::Reset, false, false),
                            ];
                            if has_internet {
                                expected.push((
                                    "✓  Internet     cp.cloudflare.com".to_owned(),
                                    Color::Green,
                                    false,
                                    false,
                                ));
                            }
                            expected.extend([
                                (
                                    format!("✓  Server       {success}"),
                                    Color::Green,
                                    false,
                                    false,
                                ),
                                (
                                    "✓  SSH session  connected".to_owned(),
                                    Color::Green,
                                    false,
                                    false,
                                ),
                                (
                                    format!("{symbol}  {label:<12} {opening}"),
                                    Color::Yellow,
                                    false,
                                    false,
                                ),
                            ]);
                            assert_eq!(
                                rows.iter()
                                    .map(|r| (r.text.clone(), r.color, r.bold, r.dim))
                                    .collect::<Vec<_>>(),
                                expected
                            );
                        }
                    }
                }
            }
        }
    }
}
