//! tmux command-output parsers.

use crate::ids::{MuxName, PaneId};
use crate::mux::{ClientPresence, ClientView, MuxErr, Result};
use crate::pane::{PaneRef, SIDEBAR_CHROME_TITLE};

/// Parse one comma-separated `list-panes -F` row into a [`PaneRef`]. Returns
/// `None` for a row missing the three load-bearing leading columns (session,
/// window, pane id) — a degraded answer the caller skips rather than surfaces.
///
/// Trailing columns are read with `.get(i)`, so a short row (an older tmux, or a
/// mid-tick race that truncated the line) yields `None`/default for the missing
/// field rather than erroring the whole read.
pub(super) fn parse_pane_line(line: &str) -> Option<PaneRef> {
    let cols: Vec<_> = line.split(',').collect();
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
        view_kind: Some(crate::mux::view_kind(MuxName::Tmux)),
        view_name: trimmed_nonempty(7),
        is_focused: cols.get(6).is_some_and(|value| value.trim() == "1"),
        // Added in tmux 3.7. On the supported 3.5/3.6 releases an unknown
        // format expands empty, so the optional trailing column stays false.
        is_floating: cols.get(9).is_some_and(|value| value.trim() == "1"),
        command: if cols
            .get(8)
            .is_some_and(|value| value.trim() == SIDEBAR_CHROME_TITLE)
        {
            Some(SIDEBAR_CHROME_TITLE.to_owned())
        } else {
            trimmed_nonempty(3)
        },
        spawn_command: trimmed_nonempty(10),
        cwd: trimmed_nonempty(4),
        pane_pid: cols
            .get(5)
            .and_then(|value| value.trim().parse::<u32>().ok()),
        // tmux has no per-pane process-start format variable; the sidebar
        // producer derives the stamp from `pane_pid` via `/proc`
        // (`sidebar::produce::panes::stamp_pane_process_starts`).
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    })
}

pub(super) fn parse_client_view(stdout: &[u8]) -> ClientView {
    let mut viewed = Vec::new();
    let mut human_clients = 0;
    let mut last_input_ms: Option<u64> = None;
    for line in String::from_utf8_lossy(stdout).lines() {
        let mut cols = line.split_whitespace();
        let Some(raw_pane) = cols.next().map(str::trim) else {
            continue;
        };
        if !raw_pane.starts_with('%') {
            continue;
        }
        let activity_s = cols.next().map(str::trim);
        let flags = cols.next().unwrap_or_default();
        if flags.split(',').any(|flag| flag.trim() == "ignore-size") {
            continue;
        }

        human_clients += 1;
        let pane = PaneId::from_parts(MuxName::Tmux, raw_pane);
        let activity_ms = activity_s
            .and_then(|activity| activity.parse::<u64>().ok())
            .map(|activity| activity.saturating_mul(1_000));
        if let Some(activity_ms) = activity_ms {
            last_input_ms = Some(last_input_ms.map_or(activity_ms, |known| known.max(activity_ms)));
        }
        viewed.push((pane, activity_ms));
    }
    viewed.sort_by_key(|(_, activity)| std::cmp::Reverse(activity.unwrap_or_default()));
    let mut viewed_panes = Vec::new();
    for (pane, _) in viewed {
        if !viewed_panes.iter().any(|known| known == &pane) {
            viewed_panes.push(pane);
        }
    }
    ClientView {
        viewed_panes,
        presence: ClientPresence {
            human_clients,
            last_input_ms,
        },
    }
}

pub(super) fn parse_floating_pane_ids(stdout: &[u8]) -> Vec<PaneId> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let (raw, floating) = line.split_once(',')?;
            (floating.trim() == "1" && raw.trim().starts_with('%'))
                .then(|| PaneId::from_parts(MuxName::Tmux, raw.trim()))
        })
        .collect()
}

pub(super) fn parse_new_window_ids(stdout: &[u8]) -> Result<(String, String)> {
    let raw = String::from_utf8_lossy(stdout);
    let mut cols = raw.split_whitespace();
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
        // window_name, pane_title, pane_floating_flag, pane_start_command.
        let row = "rimz-qe,@1,%3,rimz,/home/u/qe,4242,1,qe,,0,rimz loop watch --hold";
        let pane = parse_pane_line(row).expect("full row parses");
        assert_eq!(pane.pane_id.raw(), "%3");
        assert_eq!(pane.session_name, "rimz-qe");
        assert_eq!(pane.view_id.as_deref(), Some("@1"));
        assert_eq!(pane.view_name.as_deref(), Some("qe"));
        assert_eq!(pane.command.as_deref(), Some("rimz"));
        assert_eq!(
            pane.spawn_command.as_deref(),
            Some("rimz loop watch --hold")
        );
        assert_eq!(pane.cwd.as_deref(), Some("/home/u/qe"));
        assert_eq!(pane.pane_pid, Some(4242));
        assert!(pane.is_focused, "pane_active=1 is focused");
        assert!(!pane.is_floating, "pre-3.7 rows default to tiled");
        assert_eq!(
            pane.pane_process_start, None,
            "tmux has no per-pane process-start variable; the /proc stamp owns it",
        );

        let inactive = "rimz-qe,@1,%4,zsh,/home/u/qe,4243,0,qe";
        assert!(
            !parse_pane_line(inactive)
                .expect("inactive row parses")
                .is_focused
        );

        let floating = parse_pane_line("rimz-qe,@1,%5,codex,/home/u/qe,4244,0,qe,,1")
            .expect("floating row parses");
        assert!(floating.is_floating);

        // A truncated row that still carries the three load-bearing columns
        // parses; the absent optional fields read as `None`/default.
        let short = parse_pane_line("rimz-qe,@1,%3").expect("leading columns parse");
        assert_eq!(short.pane_id.raw(), "%3");
        assert_eq!(short.command, None);
        assert_eq!(short.spawn_command, None);
        assert_eq!(short.view_name, None);
        assert!(!short.is_focused);

        for malformed in ["rimz-qe,@1", ""] {
            assert!(
                parse_pane_line(malformed).is_none(),
                "needs session+window+pane: {malformed:?}",
            );
        }
    }

    #[test]
    fn parse_client_view_reads_panes_activity_and_human_clients() {
        let panes = parse_client_view(b"%10 100 \n%10 100 \n%11 100 \n").viewed_panes;
        assert_eq!(
            panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%10"),
                PaneId::from_parts(MuxName::Tmux, "%11"),
            ]
        );

        assert!(
            parse_client_view(b"\nno-pane 100 \n@1 100 \n")
                .viewed_panes
                .is_empty()
        );

        let view = parse_client_view(
            b"%10 1700000000 \n\
              %10 1700000001 attached\n\
              %11 1699999999 ignore-size,no-output\n\
              %12 bad attached\n\
              no-pane 1700000002 attached\n",
        );

        assert_eq!(
            view.viewed_panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%10"),
                PaneId::from_parts(MuxName::Tmux, "%12"),
            ]
        );
        assert_eq!(view.presence.human_clients, 3);
        assert_eq!(view.presence.last_input_ms, Some(1_700_000_001_000));

        let view = parse_client_view(b"\nno-pane 1700000000 \n");

        assert!(view.viewed_panes.is_empty());
        assert_eq!(view.presence.human_clients, 0);
        assert_eq!(view.presence.last_input_ms, None);
    }

    #[test]
    fn parse_floating_panes_tolerates_pre_3_7_empty_flags() {
        assert_eq!(
            parse_floating_pane_ids(b"%1,0\n%2,1\n%3,\nmalformed\n@4,1\n"),
            vec![PaneId::from_parts(MuxName::Tmux, "%2")],
        );
    }

    #[test]
    fn parse_new_window_ids_reads_whitespace_fields() {
        assert_eq!(
            parse_new_window_ids(b"@3 %9\n").expect("ids parse"),
            ("@3".to_owned(), "%9".to_owned()),
        );
        assert!(
            parse_new_window_ids(b"@3\n").is_err(),
            "both window and pane ids are required",
        );
    }
}
