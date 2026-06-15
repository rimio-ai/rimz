//! tmux command-output parsers.

use super::options::SIDEBAR_PANE_TITLE;
use crate::feed::PaneRef;
use crate::ids::{MuxName, PaneId, ViewKind};
use crate::mux::{MuxErr, Result};

/// Parse one tab-separated `list-panes -F` row into a [`PaneRef`]. Returns
/// `None` for a row missing the three load-bearing leading columns (session,
/// window, pane id) — a degraded answer the caller skips rather than surfaces.
///
/// Trailing columns are read with `.get(i)`, so a short row (an older tmux, or a
/// mid-tick race that truncated the line) yields `None`/default for the missing
/// field rather than erroring the whole read.
pub(super) fn parse_pane_line(line: &str) -> Option<PaneRef> {
    let cols: Vec<_> = line.split('\t').collect();
    if cols.len() < 3 {
        return None;
    }
    let trimmed_nonempty = |i: usize| {
        cols.get(i)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    Some(PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, cols[2]),
        session_name: cols[0].to_owned(),
        view_id: Some(cols[1].to_owned()),
        view_kind: Some(ViewKind::Window),
        view_name: trimmed_nonempty(7),
        is_focused: cols.get(6).is_some_and(|value| value.trim() == "1"),
        is_floating: false,
        command: if cols
            .get(8)
            .is_some_and(|value| value.trim() == SIDEBAR_PANE_TITLE)
        {
            Some(SIDEBAR_PANE_TITLE.to_owned())
        } else {
            trimmed_nonempty(3)
        },
        spawn_command: None,
        cwd: trimmed_nonempty(4),
        pane_pid: cols
            .get(5)
            .and_then(|value| value.trim().parse::<u32>().ok()),
        // tmux has no per-pane process-start format variable; the sidebar
        // producer derives the stamp from `pane_pid` via `/proc`
        // (`sidebar::produce::panes::stamp_pane_process_starts`).
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    })
}

pub(super) fn parse_focused_client_panes(stdout: &[u8]) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for raw in String::from_utf8_lossy(stdout).lines().map(str::trim) {
        if !raw.starts_with('%') {
            continue;
        }
        let pane = PaneId::from_parts(MuxName::Tmux, raw);
        if !panes.iter().any(|known| known == &pane) {
            panes.push(pane);
        }
    }
    panes
}

pub(super) fn parse_new_window_ids(stdout: &[u8]) -> Result<(String, String)> {
    let raw = String::from_utf8_lossy(stdout);
    let mut cols = raw.trim().split('\t');
    let window = cols.next().unwrap_or_default().trim();
    let pane = cols.next().unwrap_or_default().trim();
    if window.is_empty() || pane.is_empty() {
        return Err(MuxErr::Output {
            program: "tmux".to_owned(),
            reason: format!("new-window did not print window and pane ids: {raw:?}"),
        });
    }
    Ok((window.to_owned(), pane.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId};

    #[test]
    fn parse_pane_line_handles_full_short_and_invalid_rows() {
        // session, window_id, pane_id, command, cwd, pid, pane_active,
        // window_name.
        let row = "rimz-qe\t@1\t%3\tnvim\t/home/u/qe\t4242\t1\tqe";
        let pane = parse_pane_line(row).expect("full row parses");
        assert_eq!(pane.pane_id.raw(), "%3");
        assert_eq!(pane.session_name, "rimz-qe");
        assert_eq!(pane.view_id.as_deref(), Some("@1"));
        assert_eq!(pane.view_name.as_deref(), Some("qe"));
        assert_eq!(pane.command.as_deref(), Some("nvim"));
        assert_eq!(pane.cwd.as_deref(), Some("/home/u/qe"));
        assert_eq!(pane.pane_pid, Some(4242));
        assert!(pane.is_focused, "pane_active=1 is focused");
        assert_eq!(
            pane.pane_process_start, None,
            "tmux has no per-pane process-start variable; the /proc stamp owns it",
        );

        let inactive = "rimz-qe\t@1\t%4\tzsh\t/home/u/qe\t4243\t0\tqe";
        assert!(
            !parse_pane_line(inactive)
                .expect("inactive row parses")
                .is_focused
        );

        // A truncated row that still carries the three load-bearing columns
        // parses; the absent optional fields read as `None`/default.
        let short = parse_pane_line("rimz-qe\t@1\t%3").expect("leading columns parse");
        assert_eq!(short.pane_id.raw(), "%3");
        assert_eq!(short.command, None);
        assert_eq!(short.view_name, None);
        assert!(!short.is_focused);

        for malformed in ["rimz-qe\t@1", ""] {
            assert!(
                parse_pane_line(malformed).is_none(),
                "needs session+window+pane: {malformed:?}",
            );
        }
    }

    #[test]
    fn parse_focused_client_panes_dedupes_and_ignores_malformed_rows() {
        let panes = parse_focused_client_panes(b"%10\n%10\n%11\n");
        assert_eq!(
            panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%10"),
                PaneId::from_parts(MuxName::Tmux, "%11"),
            ]
        );

        assert!(parse_focused_client_panes(b"\nno-pane\n@1\n").is_empty());
    }
}
