//! Zellij command-output and layout parsing helpers.

use std::collections::BTreeSet;
use std::num::NonZeroU16;

use crate::ids::{MuxName, PaneId};
use crate::pane::SIDEBAR_CHROME_TITLE;

/// Whether `list-panes` stdout is the transient empty race rather than a real
/// answer. Zellij spells "zero panes" as `[]`, so empty (or whitespace-only)
/// output means the action client raced the session server and is worth a
/// retry — not an EOF parse error.
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
    clean.starts_with("Session '") && clean.contains("' not found")
}

/// Liveness of a Zellij session, as reported by `zellij list-sessions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionState {
    /// No session by that name.
    Absent,
    /// Running and attachable.
    Live,
    /// Present but exited — `attach` would resurrect a stale serialized layout.
    Exited,
}

/// Parse one `list-sessions` line for `name`. Lines look like
/// `name [Created 6m ago]` (live) or
/// `name [Created 6m ago] (EXITED - attach to resurrect)`. `strip_ansi` guards
/// against a colorized line even though `--no-formatting` should preclude one.
pub(super) fn session_state_from_line(line: &str, name: &str) -> Option<SessionState> {
    let clean = strip_ansi(line);
    if clean.split_whitespace().next()? != name {
        return None;
    }
    Some(if clean.contains("EXITED") {
        SessionState::Exited
    } else {
        SessionState::Live
    })
}

pub(super) fn live_session_name_from_line(line: &str) -> Option<String> {
    let clean = strip_ansi(line);
    let name = clean.split_whitespace().next()?;
    matches!(
        session_state_from_line(&clean, name),
        Some(SessionState::Live)
    )
    .then(|| name.to_owned())
}

pub(super) fn new_tab_template_sidebar_cols(layout: &str) -> Option<NonZeroU16> {
    let tokens = tokenize_zellij_layout(layout)?;
    let template_open = tokens
        .iter()
        .enumerate()
        .find_map(|(index, token)| match &token.kind {
            KdlTokenKind::Ident(name) if name == "new_tab_template" => tokens[index + 1..]
                .iter()
                .position(|token| matches!(token.kind, KdlTokenKind::LBrace))
                .map(|offset| index + 1 + offset),
            _ => None,
        })?;
    let template_close = matching_brace(&tokens, template_open)?;
    sidebar_cols_from_template_tokens(&tokens[template_open + 1..template_close])
}

fn sidebar_cols_from_template_tokens(tokens: &[KdlToken]) -> Option<NonZeroU16> {
    for (index, token) in tokens.iter().enumerate() {
        let KdlTokenKind::Ident(node) = &token.kind else {
            continue;
        };
        if node != "pane" {
            continue;
        }

        let line = token.line;
        let mut name_is_sidebar = false;
        let mut size = None;
        let mut cursor = index + 1;
        while let Some(token) = tokens.get(cursor) {
            if token.line != line
                || matches!(token.kind, KdlTokenKind::LBrace | KdlTokenKind::RBrace)
            {
                break;
            }
            let Some((key, value)) = kdl_property_on_line(tokens, cursor, line) else {
                cursor += 1;
                continue;
            };
            match (key, value) {
                ("name", KdlTokenKind::String(value)) if value == SIDEBAR_CHROME_TITLE => {
                    name_is_sidebar = true;
                }
                ("size", KdlTokenKind::Number(value)) => {
                    size = u16::try_from(*value).ok().and_then(NonZeroU16::new);
                }
                _ => {}
            }
            cursor += 3;
        }
        if name_is_sidebar && let Some(size) = size {
            return Some(size);
        }
    }
    None
}

fn kdl_property_on_line(
    tokens: &[KdlToken],
    index: usize,
    line: usize,
) -> Option<(&str, &KdlTokenKind)> {
    let key = match &tokens.get(index)?.kind {
        KdlTokenKind::Ident(key) => key.as_str(),
        _ => return None,
    };
    let equals = tokens.get(index + 1)?;
    let value = tokens.get(index + 2)?;
    (equals.line == line && value.line == line && matches!(equals.kind, KdlTokenKind::Equals))
        .then_some((key, &value.kind))
}

fn matching_brace(tokens: &[KdlToken], open: usize) -> Option<usize> {
    if !matches!(tokens.get(open)?.kind, KdlTokenKind::LBrace) {
        return None;
    }
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            KdlTokenKind::LBrace => depth += 1,
            KdlTokenKind::RBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
struct KdlToken {
    kind: KdlTokenKind,
    line: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum KdlTokenKind {
    Ident(String),
    String(String),
    Number(i64),
    Equals,
    LBrace,
    RBrace,
}

/// Tokenize the subset of Zellij-generated KDL needed from `dump-layout`.
/// Strings and braces are honored so the template walk never matches text
/// inside argv, cwd, or command values; unsupported shapes degrade to `None`.
fn tokenize_zellij_layout(input: &str) -> Option<Vec<KdlToken>> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();
    let mut line = 1;
    while let Some((_, ch)) = chars.peek().copied() {
        match ch {
            '\n' => {
                line += 1;
                chars.next();
            }
            ch if ch.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek().is_some_and(|(_, next)| *next == '/') {
                    for (_, skipped) in chars.by_ref() {
                        if skipped == '\n' {
                            line += 1;
                            break;
                        }
                    }
                } else {
                    tokens.push(KdlToken {
                        kind: KdlTokenKind::Ident("/".to_owned()),
                        line,
                    });
                }
            }
            '{' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::LBrace,
                    line,
                });
                chars.next();
            }
            '}' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::RBrace,
                    line,
                });
                chars.next();
            }
            '=' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::Equals,
                    line,
                });
                chars.next();
            }
            '"' => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::String(read_kdl_string(&mut chars, &mut line)?),
                    line,
                });
            }
            '-' | '+' | '0'..='9' => {
                tokens.push(KdlToken {
                    kind: read_kdl_number_or_ident(&mut chars),
                    line,
                });
            }
            _ => {
                tokens.push(KdlToken {
                    kind: KdlTokenKind::Ident(read_kdl_ident(&mut chars)),
                    line,
                });
            }
        }
    }
    Some(tokens)
}

fn read_kdl_string(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: &mut usize,
) -> Option<String> {
    let (_, quote) = chars.next()?;
    if quote != '"' {
        return None;
    }
    let mut value = String::new();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '"' => return Some(value),
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    if escaped == '\n' {
                        *line += 1;
                    }
                    value.push(escaped);
                } else {
                    return None;
                }
            }
            '\n' => {
                *line += 1;
                value.push(ch);
            }
            _ => value.push(ch),
        }
    }
    None
}

fn read_kdl_number_or_ident(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> KdlTokenKind {
    let raw = read_kdl_ident(chars);
    raw.parse::<i64>()
        .map(KdlTokenKind::Number)
        .unwrap_or(KdlTokenKind::Ident(raw))
}

fn read_kdl_ident(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> String {
    let mut value = String::new();
    while let Some((_, ch)) = chars.peek().copied() {
        if ch.is_whitespace() || matches!(ch, '{' | '}' | '=' | '"') {
            break;
        }
        value.push(ch);
        chars.next();
    }
    value
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

pub(super) fn parse_focused_client_panes(stdout: &[u8]) -> Vec<PaneId> {
    let mut panes = Vec::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let clean = strip_ansi(line);
        let mut cols = clean.split_whitespace();
        let Some(first) = cols.next() else {
            continue;
        };
        let Some(raw_pane) = cols.next() else {
            continue;
        };
        if first == "CLIENT_ID" || raw_pane == "ZELLIJ_PANE_ID" {
            continue;
        }
        if !raw_pane.starts_with("terminal_") {
            continue;
        }
        let pane = PaneId::from_parts(MuxName::Zellij, raw_pane);
        if !panes.iter().any(|known| known == &pane) {
            panes.push(pane);
        }
    }
    panes
}

pub(super) fn parse_focused_terminal_client_ids(stdout: &[u8]) -> BTreeSet<u32> {
    let mut clients = BTreeSet::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let clean = strip_ansi(line);
        let mut cols = clean.split_whitespace();
        let Some(first) = cols.next() else {
            continue;
        };
        let Some(raw_pane) = cols.next() else {
            continue;
        };
        if first == "CLIENT_ID" || raw_pane == "ZELLIJ_PANE_ID" {
            continue;
        }
        if !raw_pane.starts_with("terminal_") {
            continue;
        }
        if let Ok(client) = first.parse::<u32>() {
            clients.insert(client);
        }
    }
    clients
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
    use std::num::NonZeroU16;

    use super::*;
    use crate::ids::{MuxName, PaneId};

    #[test]
    fn parse_focused_client_panes_reads_unique_terminals_and_skips_noise() {
        let output = b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                       1         terminal_30    codex\n\
                       2         terminal_30    codex\n\
                       3         terminal_4     claude\n\
                       4         plugin_2       rimz-presence-zellij\n\
                       5         -              unknown\n";
        let panes = parse_focused_client_panes(output);
        assert_eq!(
            panes,
            vec![
                PaneId::from_parts(MuxName::Zellij, "terminal_30"),
                PaneId::from_parts(MuxName::Zellij, "terminal_4"),
            ]
        );

        assert!(
            parse_focused_client_panes(b"\x1b[32;1mCLIENT_ID\x1b[m ZELLIJ_PANE_ID\n").is_empty()
        );
    }

    #[test]
    fn parse_focused_terminal_client_ids_reads_terminals_and_skips_noise() {
        let output = b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                       1         terminal_30    codex\n\
                       2         terminal_30    codex\n\
                       3         terminal_4     claude\n\
                       4         plugin_2       rimz-presence-zellij\n\
                       5         -              unknown\n\
                       action    terminal_9     unknown\n";
        assert_eq!(
            parse_focused_terminal_client_ids(output),
            BTreeSet::from([1, 2, 3])
        );

        assert!(
            parse_focused_terminal_client_ids(b"\x1b[32;1mCLIENT_ID\x1b[m ZELLIJ_PANE_ID\n")
                .is_empty()
        );
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

        assert!(!is_session_not_found(b"[{\"id\":0}]"));
        assert!(!is_session_not_found(b"rimzd\nTab #2\n#start\n"));
        assert!(!is_session_not_found(b""));
        assert!(!is_session_not_found(b"  \n\t"));
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
            Some(SessionState::Live),
        );
        assert_eq!(
            session_state_from_line(
                "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionState::Exited),
        );
        // A colorized line (no `--no-formatting`) still parses via `strip_ansi`.
        assert_eq!(
            session_state_from_line(
                "\x1b[32;1mrimz-query-engine\x1b[m [Created ago] (\x1b[31;1mEXITED\x1b[m - resurrect)",
                "rimz-query-engine",
            ),
            Some(SessionState::Exited),
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

    #[test]
    fn new_tab_template_sidebar_cols_reads_fixed_width_only() {
        let fixed = r#"
            layout {
                tab {
                    pane split_direction="vertical" {
                        pane name="rimz-sidebar" size="24%"
                        pane
                    }
                }
                new_tab_template {
                    pane split_direction="vertical" {
                        pane name="rimz-sidebar" size=72
                        pane focus=true
                    }
                }
            }
        "#;
        let percentage = r#"
            layout {
                new_tab_template {
                    pane split_direction="vertical" {
                        pane name="rimz-sidebar" size="24%"
                        pane focus=true
                    }
                }
            }
        "#;

        for (layout, expected) in [(fixed, NonZeroU16::new(72)), (percentage, None)] {
            assert_eq!(new_tab_template_sidebar_cols(layout), expected);
        }
    }
}
