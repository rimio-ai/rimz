//! Zellij command-output and layout parsing helpers.

use std::collections::BTreeSet;

use crate::ids::{MuxName, PaneId};
use crate::mux::{
    ClientPaneView, ClientPresence, ClientView, MuxClientId, MuxErr, SessionLiveness,
};

/// Whether action stdout is the transient empty race rather than a real answer.
/// Empty or whitespace-only output means the action client raced the session
/// server and is worth a retry — not an EOF parse error.
pub(super) fn is_transient_empty(stdout: &[u8]) -> bool {
    stdout.iter().all(u8::is_ascii_whitespace)
}

/// Whether `action`-client output is Zellij's "this session is not addressable"
/// answer. When the named session is absent, exited (resurrectable), or still
/// registering, `zellij --session <name> action ...` prints a `Session '<name>'
/// not found...` banner and still exits 0. The line is non-whitespace when it
/// lands on stdout, so [`is_transient_empty`] misses it; callers recognize it
/// here and surface a typed mux error instead of feeding the banner or session
/// list to a JSON or tab-name parser.
pub(super) fn is_session_not_found(stream: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stream);
    let Some(first) = text.lines().next() else {
        return false;
    };
    let clean = strip_ansi(first);
    let clean = clean.trim_start();
    (clean.starts_with("Session '") && clean.contains("' not found"))
        || clean == "There is no active session!"
}

pub(super) fn is_no_active_sessions(stream: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stream);
    let Some(first) = text.lines().next() else {
        return false;
    };
    let clean = strip_ansi(first);
    let clean = clean.trim();
    clean == "No active zellij sessions found." || clean == "There is no active session!"
}

/// Fold zellij's nonzero-exit "Session '<name>' not found" answer into the
/// typed error. Some versions print the banner on exit 0 (callers' post-run
/// stream checks catch those); newer versions exit nonzero, which arrives as
/// `MuxErr::Command` with the banner on stderr.
pub(super) fn classify_session_not_found(err: MuxErr, session: &str) -> MuxErr {
    match err {
        MuxErr::Command { ref stderr, .. } if is_session_not_found(stderr.as_bytes()) => {
            MuxErr::SessionNotFound {
                session: session.to_owned(),
            }
        }
        _ => err,
    }
}

/// Parse one `list-sessions` line for `name`. Lines look like
/// `name [Created 6m ago]` (live) or
/// `name [Created 6m ago] (EXITED - attach to resurrect)`. `strip_ansi` guards
/// against a colorized line even though `--no-formatting` should preclude one.
pub(super) fn session_state_from_line(line: &str, name: &str) -> Option<SessionLiveness> {
    let clean = strip_ansi(line);
    if clean.split_whitespace().next()? != name {
        return None;
    }
    Some(if clean.contains("EXITED") {
        SessionLiveness::Exited
    } else {
        SessionLiveness::Live
    })
}

pub(super) fn live_session_name_from_line(line: &str) -> Option<String> {
    let clean = strip_ansi(line);
    let name = clean.split_whitespace().next()?;
    matches!(
        session_state_from_line(&clean, name),
        Some(SessionLiveness::Live)
    )
    .then(|| name.to_owned())
}

/// Defensive ANSI strip for `list-sessions` output. Zellij ships a colored
/// banner in newer versions; the parser only cares about the bare name.
///
/// Handles the CSI subset (`ESC [ params final`) Zellij emits. The
/// introducer `[` lives at 0x5b which overlaps the final-byte range
/// (0x40..=0x7e), so we must consume the introducer first and only then
/// scan for the final byte.
pub(super) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for ch in chars.by_ref() {
                    if matches!(ch, '\x40'..='\x7e') {
                        break;
                    }
                }
            }
            // Non-CSI escape (single byte after ESC) or end-of-string: nothing
            // to skip; the next iteration resumes the scan.
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn parse_client_view(stdout: &[u8]) -> ClientView {
    let mut client_ids = BTreeSet::new();
    let mut clients = Vec::new();
    let mut viewed_panes = Vec::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let clean = strip_ansi(line);
        let mut cols = clean.split_whitespace();
        let (Some(raw_client), Some(raw_pane)) = (cols.next(), cols.next()) else {
            continue;
        };
        let Ok(client_id) = raw_client.parse::<u32>() else {
            continue;
        };
        client_ids.insert(client_id);
        if !raw_pane.starts_with("terminal_") && !raw_pane.starts_with("plugin_") {
            continue;
        }
        let pane_id = PaneId::from_parts(MuxName::Zellij, raw_pane);
        clients.push(ClientPaneView {
            client_id: MuxClientId::Zellij(client_id),
            pane_id: pane_id.clone(),
        });
        if raw_pane.starts_with("terminal_") && !viewed_panes.iter().any(|known| known == &pane_id)
        {
            viewed_panes.push(pane_id);
        }
    }
    clients.sort();
    clients.dedup();
    ClientView {
        clients,
        viewed_panes,
        presence: ClientPresence {
            human_clients: client_ids.len(),
            last_input_ms: None,
        },
    }
}

pub(super) fn terminal_client_ids(view: &ClientView) -> BTreeSet<u32> {
    view.clients
        .iter()
        .filter(|client| client.pane_id.raw().starts_with("terminal_"))
        .filter_map(|client| match &client.client_id {
            MuxClientId::Zellij(id) => Some(*id),
            MuxClientId::Tmux(_) => None,
        })
        .collect()
}

pub(super) fn trim_capture(raw_text: String, max_lines: Option<u16>) -> (String, Vec<String>) {
    let mut lines: Vec<String> = raw_text.lines().map(str::to_owned).collect();
    if let Some(max_lines) = max_lines {
        let keep = max_lines as usize;
        if keep == 0 {
            lines.clear();
        } else if lines.len() > keep {
            lines = lines.split_off(lines.len() - keep);
        }
    }

    let mut trimmed = lines.join("\n");
    if raw_text.ends_with('\n') && !trimmed.is_empty() {
        trimmed.push('\n');
    }
    (trimmed, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId};
    use crate::mux::MuxErr;

    #[test]
    fn terminal_client_ids_reads_terminals_and_skips_noise() {
        let output = b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                       1         terminal_30    codex\n\
                       2         terminal_30    codex\n\
                       3         terminal_4     claude\n\
                       4         plugin_2       rimz-presence-zellij\n\
                       5         -              unknown\n\
                       action    terminal_9     unknown\n";
        assert_eq!(
            terminal_client_ids(&parse_client_view(output)),
            BTreeSet::from([1, 2, 3])
        );

        assert!(
            terminal_client_ids(&parse_client_view(
                b"\x1b[32;1mCLIENT_ID\x1b[m ZELLIJ_PANE_ID\n"
            ))
            .is_empty()
        );
    }

    #[test]
    fn parse_client_view_retains_client_identity_and_plugin_views() {
        let view = parse_client_view(
            b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
              1 terminal_30 codex\n\
              2 terminal_30 codex\n\
              3 plugin_2 rimz-presence-zellij\n\
              4 - unknown\n",
        );

        assert_eq!(
            view.viewed_panes,
            vec![PaneId::from_parts(MuxName::Zellij, "terminal_30")],
        );
        assert_eq!(
            view.clients,
            vec![
                ClientPaneView {
                    client_id: MuxClientId::Zellij(1),
                    pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_30"),
                },
                ClientPaneView {
                    client_id: MuxClientId::Zellij(2),
                    pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_30"),
                },
                ClientPaneView {
                    client_id: MuxClientId::Zellij(3),
                    pane_id: PaneId::from_parts(MuxName::Zellij, "plugin_2"),
                },
            ],
        );
        assert_eq!(view.presence.human_clients, 4);
    }

    #[test]
    fn transient_empty_detects_blank_list_panes_output() {
        assert!(is_transient_empty(b""));
        assert!(is_transient_empty(b"  \n\t"));
        // A real, parseable answer — even an empty pane set — is not transient.
        assert!(!is_transient_empty(b"[]"));
        assert!(!is_transient_empty(b"[{\"id\":0}]"));
    }

    #[test]
    fn session_not_found_detects_action_banner_and_rejects_real_output() {
        let banner = b"Session 'rimz-rimz-f89e49' not found. The following sessions are active:\n\
                       \x1b[32;1mrimz-project-123456\x1b[m [Created 6m ago]\n";
        assert!(is_session_not_found(banner));
        assert!(is_session_not_found(
            b"Session 'rimz-rimz-f89e49' not found. The following sessions are active:\n"
        ));
        assert!(is_session_not_found(b"There is no active session!\n"));

        assert!(!is_session_not_found(b"[{\"id\":0}]"));
        assert!(!is_session_not_found(b"rimzd\nTab #2\n#start\n"));
        assert!(!is_session_not_found(b""));
        assert!(!is_session_not_found(b"  \n\t"));
    }

    #[test]
    fn no_active_sessions_detects_zellij_list_sessions_banner() {
        assert!(is_no_active_sessions(b"No active zellij sessions found.\n"));
        assert!(is_no_active_sessions(b"There is no active session!\n"));
        assert!(is_no_active_sessions(
            b"\x1b[31mNo active zellij sessions found.\x1b[m\n"
        ));

        assert!(!is_no_active_sessions(b"rimz-project [Created 6m ago]\n"));
        assert!(!is_no_active_sessions(b"permission denied\n"));
    }

    #[test]
    fn classify_session_not_found_maps_command_banner() {
        let err = MuxErr::Command {
            program: "zellij".to_owned(),
            args: format!(
                "--session missing-room action {}",
                concat!("list", "-panes")
            ),
            stderr: "Session 'missing-room' not found. The following sessions are active:\n\
                     rimz-other [Created 6m ago]\n"
                .to_owned(),
        };

        assert!(matches!(
            classify_session_not_found(err, "missing-room"),
            MuxErr::SessionNotFound { ref session } if session == "missing-room"
        ));
    }

    #[test]
    fn classify_session_not_found_preserves_unrelated_errors() {
        let err = classify_session_not_found(
            MuxErr::Command {
                program: "zellij".to_owned(),
                args: format!("action {}", concat!("list", "-panes")),
                stderr: "permission denied".to_owned(),
            },
            "missing-room",
        );
        assert!(matches!(err, MuxErr::Command { ref stderr, .. } if stderr == "permission denied"));

        let err = classify_session_not_found(
            MuxErr::Timeout {
                program: "zellij".to_owned(),
                args: format!("action {}", concat!("list", "-panes")),
                seconds: 8,
            },
            "missing-room",
        );
        assert!(matches!(err, MuxErr::Timeout { seconds: 8, .. }));
    }

    #[test]
    fn capture_trim_keeps_last_requested_lines() {
        let (raw, lines) = trim_capture("a\nb\nc\nd\n".to_owned(), Some(2));
        assert_eq!(lines, vec!["c", "d"]);
        assert_eq!(raw, "c\nd\n");
    }

    #[test]
    fn session_state_classifies_list_sessions_lines() {
        assert_eq!(
            session_state_from_line("rimz-query-engine [Created 6m ago]", "rimz-query-engine"),
            Some(SessionLiveness::Live),
        );
        assert_eq!(
            session_state_from_line(
                "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionLiveness::Exited),
        );
        // A colorized line (no `--no-formatting`) still parses via `strip_ansi`.
        assert_eq!(
            session_state_from_line(
                "\x1b[32;1mrimz-query-engine\x1b[m [Created ago] (\x1b[31;1mEXITED\x1b[m - resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionLiveness::Exited),
        );
        // A different session's line is not a match.
        assert_eq!(
            session_state_from_line("other [Created 6m ago]", "rimz-query-engine"),
            None,
        );
        assert_eq!(
            live_session_name_from_line("rimz-query-engine [Created 6m ago]"),
            Some("rimz-query-engine".to_owned()),
        );
        assert_eq!(
            live_session_name_from_line(
                "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
            ),
            None,
        );
    }
}
