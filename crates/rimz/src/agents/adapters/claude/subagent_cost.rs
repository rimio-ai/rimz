//! Exact incremental cost fold for one Claude child transcript.

use std::path::Path;

use crate::agents::context::{PricedRequest, SubagentUsageCursor};
use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{SplitPrice, lookup_split_price, should_replace_usage_duplicate};
use crate::agents::transcript_fs::{bytes_contains, read_transcript_lines};

use super::spend::{ClaudeEntry, ClaudeUsage, has_unsupported_null_field, request_split};
use super::subagents::subagents_dir;

const USAGE_MARKER: &[u8] = br#""usage":{"#;

/// Advance one child's cumulative cost through complete records appended since
/// `prior`. An unreadable child transcript returns `None` so the caller can
/// retain its last published figure.
pub(super) fn advance_cursor(
    parent_transcript: &Path,
    child_id: &str,
    prior: Option<&SubagentUsageCursor>,
    prices: &PriceBook,
    book_fingerprint: Option<&str>,
) -> Option<SubagentUsageCursor> {
    let filename = format!("agent-{child_id}.jsonl");
    if Path::new(&filename).file_name()?.to_str()? != filename {
        return None;
    }
    let path = subagents_dir(parent_transcript)?.join(filename);
    let len = std::fs::metadata(&path).ok()?.len();
    let transcript_path = path.to_string_lossy().into_owned();
    let replay_unpriced = prior.is_some_and(|cursor| {
        cursor.unpriced && cursor.book_fingerprint.as_deref() != book_fingerprint
    });
    let mut cursor = prior
        .filter(|cursor| {
            cursor.transcript_path == transcript_path && cursor.offset <= len && !replay_unpriced
        })
        .cloned()
        .unwrap_or(SubagentUsageCursor {
            transcript_path,
            offset: 0,
            model: None,
            cost_usd: 0.0,
            unpriced: false,
            book_fingerprint: None,
            last_request: None,
        });

    let Some((content, next_offset)) = read_transcript_lines(&path, cursor.offset) else {
        return Some(cursor);
    };
    for line in content.split(|byte| *byte == b'\n') {
        if line.is_empty()
            || !bytes_contains(line, USAGE_MARKER)
            || has_unsupported_null_field(line)
        {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<ClaudeEntry>(line) else {
            continue;
        };
        if entry.agent_id.as_deref() != Some(child_id) {
            continue;
        }
        if let Some(model) = entry
            .message
            .model
            .as_deref()
            .filter(|model| !model.is_empty() && !model.starts_with('<'))
            && cursor.model.as_deref() != Some(model)
        {
            cursor.model = Some(model.to_owned());
        }
        fold_entry(&mut cursor, &entry, prices);
    }
    cursor.offset = next_offset;
    cursor.book_fingerprint = if cursor.unpriced {
        book_fingerprint.map(str::to_owned)
    } else {
        None
    };
    Some(cursor)
}

fn fold_entry(cursor: &mut SubagentUsageCursor, entry: &ClaudeEntry, prices: &PriceBook) {
    let main = request_split(&entry.message.usage);
    if main.is_empty() {
        return;
    }
    let Some(mut request) = price_usage(
        &entry.message.usage,
        entry.message.model.as_deref(),
        entry.cost_usd,
        prices,
        &mut cursor.unpriced,
    ) else {
        cursor.last_request = None;
        return;
    };

    for iteration in entry
        .message
        .usage
        .iterations
        .iter()
        .filter(|iteration| iteration.kind == "advisor_message")
    {
        let Some(advisor) = price_usage(
            &iteration.usage,
            iteration.model.as_deref(),
            None,
            prices,
            &mut cursor.unpriced,
        ) else {
            continue;
        };
        request.cost_usd += advisor.cost_usd;
        request.token_total = request.token_total.saturating_add(advisor.token_total);
        request.has_speed |= advisor.has_speed;
    }

    let key = entry
        .message
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|message_id| {
            format!(
                "{message_id}\0{}",
                entry.request_id.as_deref().unwrap_or_default()
            )
        });
    let Some(key) = key else {
        cursor.cost_usd += request.cost_usd;
        cursor.last_request = None;
        return;
    };
    let request = PricedRequest {
        key,
        cost_usd: request.cost_usd,
        token_total: request.token_total,
        has_speed: request.has_speed,
    };

    if let Some(previous) = cursor
        .last_request
        .as_ref()
        .filter(|previous| previous.key == request.key)
    {
        if should_replace_usage_duplicate(
            request.token_total,
            request.has_speed,
            previous.token_total,
            previous.has_speed,
        ) {
            cursor.cost_usd = cursor.cost_usd - previous.cost_usd + request.cost_usd;
            cursor.last_request = Some(request);
        }
        return;
    }

    cursor.cost_usd += request.cost_usd;
    cursor.last_request = Some(request);
}

struct PricedUsage {
    cost_usd: f64,
    token_total: u64,
    has_speed: bool,
}

fn price_usage(
    usage: &ClaudeUsage,
    model: Option<&str>,
    logged_cost: Option<f64>,
    prices: &PriceBook,
    unpriced: &mut bool,
) -> Option<PricedUsage> {
    let split = request_split(usage);
    if split.is_empty() {
        return None;
    }
    let cost_usd = match logged_cost {
        Some(cost) if cost > 0.0 => cost,
        _ => match lookup_split_price(prices, model.unwrap_or_default(), split) {
            SplitPrice::Priced(cost) => cost,
            SplitPrice::Unpriced => {
                *unpriced = true;
                return None;
            }
            SplitPrice::NotPriceable => return None,
        },
    };
    Some(PricedUsage {
        cost_usd,
        token_total: token_total(split),
        has_speed: usage.speed.is_some(),
    })
}

fn token_total(split: TokenSplit) -> u64 {
    split
        .input
        .saturating_add(split.output)
        .saturating_add(split.cache_write)
        .saturating_add(split.cache_write_1h)
        .saturating_add(split.cache_read)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;

    const BOOK_FINGERPRINT: &str = "1700000000:100";

    fn prices() -> PriceBook {
        PriceBook::from_litellm_json(
            r#"{
                "model-a": {
                    "input_cost_per_token": 1.0,
                    "output_cost_per_token": 2.0,
                    "cache_creation_input_token_cost": 1.25,
                    "cache_read_input_token_cost": 0.1,
                    "provider_specific_entry": {"fast": 2.0}
                },
                "model-b": {
                    "input_cost_per_token": 3.0,
                    "output_cost_per_token": 4.0
                }
            }"#,
        )
    }

    fn session() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("session").join("chat.jsonl");
        let child = dir
            .path()
            .join("session")
            .join("subagents")
            .join("agent-child-1.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&parent, "").unwrap();
        (dir, parent, child)
    }

    fn line(message_id: &str, request_id: &str, model: &str, usage: Value) -> String {
        json!({
            "type": "assistant",
            "agentId": "child-1",
            "requestId": request_id,
            "message": {
                "id": message_id,
                "model": model,
                "usage": usage,
            },
        })
        .to_string()
    }

    fn write_lines(path: &Path, lines: &[String]) {
        let mut file = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn append_line(path: &Path, line: &str) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn advance(parent: &Path, prior: Option<&SubagentUsageCursor>) -> SubagentUsageCursor {
        advance_cursor(parent, "child-1", prior, &prices(), Some(BOOK_FINGERPRINT)).unwrap()
    }

    #[test]
    fn sums_each_request_at_its_own_model_price() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line(
                    "msg-1",
                    "req-1",
                    "model-a",
                    json!({"input_tokens": 2, "output_tokens": 3}),
                ),
                line(
                    "msg-2",
                    "req-2",
                    "model-b",
                    json!({"input_tokens": 5, "output_tokens": 7}),
                ),
            ],
        );

        let cursor = advance(&parent, None);

        assert_eq!(cursor.display_cost(), Some(51.0));
    }

    #[test]
    fn cursor_captures_newest_model() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line("msg-1", "req-1", "model-a", json!({"input_tokens": 2})),
                line("msg-2", "req-2", "model-b", json!({"input_tokens": 3})),
            ],
        );

        assert_eq!(advance(&parent, None).model.as_deref(), Some("model-b"));
    }

    #[test]
    fn prices_cache_creation_tiers_and_advisor_iterations() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[line(
                "msg-1",
                "req-1",
                "model-a",
                json!({
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 10,
                        "ephemeral_1h_input_tokens": 20
                    },
                    "cache_read_input_tokens": 30,
                    "iterations": [{
                        "type": "advisor_message",
                        "model": "model-b",
                        "input_tokens": 2,
                        "output_tokens": 3
                    }]
                }),
            )],
        );

        let cursor = advance(&parent, None);

        // Main: 10*1.25 + 20*2 + 30*0.1 = 55.5. Advisor: 2*3 + 3*4 = 18.
        assert_eq!(cursor.display_cost(), Some(73.5));
    }

    #[test]
    fn contiguous_duplicates_keep_the_richer_request() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line("msg-1", "req-1", "model-a", json!({"input_tokens": 2})),
                line("msg-1", "req-1", "model-a", json!({"input_tokens": 5})),
                line("msg-2", "req-2", "model-a", json!({"input_tokens": 3})),
            ],
        );

        assert_eq!(advance(&parent, None).display_cost(), Some(8.0));
    }

    #[test]
    fn equal_token_duplicate_with_speed_replaces_base_price() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line("msg-1", "req-1", "model-a", json!({"input_tokens": 2})),
                line(
                    "msg-1",
                    "req-1",
                    "model-a",
                    json!({"input_tokens": 2, "speed": "fast"}),
                ),
            ],
        );

        assert_eq!(advance(&parent, None).display_cost(), Some(4.0));
    }

    #[test]
    fn ignores_usage_replayed_for_another_agent() {
        let (_dir, parent, child) = session();
        let mut other: Value = serde_json::from_str(&line(
            "msg-1",
            "req-1",
            "model-a",
            json!({"input_tokens": 50}),
        ))
        .unwrap();
        other["agentId"] = json!("parent");
        write_lines(
            &child,
            &[
                other.to_string(),
                line("msg-2", "req-2", "model-a", json!({"input_tokens": 2})),
            ],
        );

        assert_eq!(advance(&parent, None).display_cost(), Some(2.0));
    }

    #[test]
    fn unknown_model_hides_the_cumulative_figure() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[line(
                "msg-1",
                "req-1",
                "future-model",
                json!({"input_tokens": 2}),
            )],
        );

        let cursor = advance(&parent, None);

        assert!(cursor.unpriced);
        assert_eq!(cursor.book_fingerprint.as_deref(), Some(BOOK_FINGERPRINT));
        assert_eq!(cursor.display_cost(), None);
    }

    #[test]
    fn changed_book_fingerprint_heals_while_unchanged_resumes() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[line(
                "msg-1",
                "req-1",
                "future-model",
                json!({"input_tokens": 2}),
            )],
        );
        let cursor = advance(&parent, None);
        let refreshed = PriceBook::from_litellm_json(
            r#"{
                "future-model": {
                    "input_cost_per_token": 5.0,
                    "output_cost_per_token": 1.0
                }
            }"#,
        );

        let unchanged = advance_cursor(
            &parent,
            "child-1",
            Some(&cursor),
            &refreshed,
            Some(BOOK_FINGERPRINT),
        )
        .unwrap();
        assert_eq!(unchanged, cursor);

        let healed = advance_cursor(
            &parent,
            "child-1",
            Some(&unchanged),
            &refreshed,
            Some("1700000001:200"),
        )
        .unwrap();

        assert!(!healed.unpriced);
        assert_eq!(healed.display_cost(), Some(10.0));
    }

    #[test]
    fn synthetic_model_skips_without_poisoning() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line(
                    "msg-1",
                    "req-1",
                    "<synthetic>",
                    json!({"input_tokens": 200}),
                ),
                line("msg-2", "req-2", "model-a", json!({"input_tokens": 2})),
            ],
        );

        let cursor = advance(&parent, None);

        assert!(!cursor.unpriced);
        assert_eq!(cursor.display_cost(), Some(2.0));
    }

    #[test]
    fn synthetic_and_empty_models_do_not_claim_the_slot() {
        let (_dir, parent, child) = session();
        write_lines(
            &child,
            &[
                line("msg-1", "req-1", "model-a", json!({"input_tokens": 2})),
                line("msg-2", "req-2", "<synthetic>", json!({"input_tokens": 2})),
                line("msg-3", "req-3", "", json!({"input_tokens": 2})),
            ],
        );

        assert_eq!(advance(&parent, None).model.as_deref(), Some("model-a"));
    }

    #[test]
    fn positive_logged_cost_beats_the_price_table() {
        let (_dir, parent, child) = session();
        let mut logged: Value = serde_json::from_str(&line(
            "msg-1",
            "req-1",
            "unknown-model",
            json!({"input_tokens": 200}),
        ))
        .unwrap();
        logged["costUSD"] = json!(0.42);
        write_lines(&child, &[logged.to_string()]);

        let cursor = advance(&parent, None);

        assert!(!cursor.unpriced);
        assert_eq!(cursor.display_cost(), Some(0.42));
    }

    #[test]
    fn append_then_advance_matches_a_full_parse() {
        let (_dir, parent, child) = session();
        let first = line("msg-1", "req-1", "model-a", json!({"input_tokens": 2}));
        let second = line("msg-2", "req-2", "model-b", json!({"output_tokens": 3}));
        write_lines(&child, std::slice::from_ref(&first));
        let cursor = advance(&parent, None);
        assert_eq!(cursor.model.as_deref(), Some("model-a"));
        append_line(&child, &second);
        let incremental = advance(&parent, Some(&cursor));
        assert_eq!(incremental.model.as_deref(), Some("model-b"));

        let full =
            advance_cursor(&parent, "child-1", None, &prices(), Some(BOOK_FINGERPRINT)).unwrap();

        assert_eq!(incremental, full);
    }

    #[test]
    fn torn_trailing_record_heals_on_the_next_advance() {
        let (_dir, parent, child) = session();
        let complete = line("msg-1", "req-1", "model-a", json!({"input_tokens": 2}));
        let split = complete.len() / 2;
        std::fs::write(&child, &complete.as_bytes()[..split]).unwrap();
        let cursor = advance(&parent, None);
        assert_eq!(cursor.offset, 0);
        assert_eq!(cursor.display_cost(), Some(0.0));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&child)
            .unwrap();
        file.write_all(&complete.as_bytes()[split..]).unwrap();
        file.write_all(b"\n").unwrap();

        assert_eq!(advance(&parent, Some(&cursor)).display_cost(), Some(2.0));
    }

    #[test]
    fn truncation_and_path_change_restart_the_fold() {
        let (dir, parent, child) = session();
        write_lines(
            &child,
            &[line(
                "msg-1",
                "req-1",
                "model-a",
                json!({"input_tokens": 20}),
            )],
        );
        let cursor = advance(&parent, None);
        write_lines(
            &child,
            &[line("m", "r", "model-a", json!({"input_tokens": 2}))],
        );
        let truncated = advance(&parent, Some(&cursor));
        assert_eq!(truncated.display_cost(), Some(2.0));
        assert_eq!(truncated.model.as_deref(), Some("model-a"));

        let other_parent = dir.path().join("other.jsonl");
        let other_child = dir
            .path()
            .join("other")
            .join("subagents")
            .join("agent-child-1.jsonl");
        std::fs::create_dir_all(other_child.parent().unwrap()).unwrap();
        std::fs::write(&other_parent, "").unwrap();
        write_lines(
            &other_child,
            &[line(
                "msg-3",
                "req-3",
                "model-b",
                json!({"input_tokens": 3}),
            )],
        );

        let changed = advance_cursor(
            &other_parent,
            "child-1",
            Some(&truncated),
            &prices(),
            Some(BOOK_FINGERPRINT),
        )
        .unwrap();
        assert_eq!(changed.display_cost(), Some(9.0));
        assert_eq!(changed.model.as_deref(), Some("model-b"));
        assert_ne!(changed.transcript_path, truncated.transcript_path);
    }

    #[test]
    fn missing_child_transcript_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("missing.jsonl");

        assert!(
            advance_cursor(&parent, "child-1", None, &prices(), Some(BOOK_FINGERPRINT)).is_none()
        );
    }

    #[test]
    fn child_id_cannot_escape_the_subagents_directory() {
        let (_dir, parent, _child) = session();

        assert!(
            advance_cursor(
                &parent,
                "x/../../outside",
                None,
                &prices(),
                Some(BOOK_FINGERPRINT)
            )
            .is_none()
        );
    }
}
