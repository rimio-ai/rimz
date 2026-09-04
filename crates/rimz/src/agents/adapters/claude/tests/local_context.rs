use super::*;
use crate::agents::{
    AgentTokenUsage, FieldPatch, LocalContextRefreshCtx, LocalSpendFold, RefreshTrigger,
};
use std::io::Write as _;

fn usage_line(request: &str, input: u64, output: u64, cache_write: u64, cache_read: u64) -> String {
    usage_block_line(
        request,
        &format!("msg-{request}"),
        input,
        output,
        cache_write,
        cache_read,
    )
}

fn usage_block_line(
    request: &str,
    message: &str,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
) -> String {
    serde_json::json!({
        "timestamp": "2026-01-01T10:00:00.000Z",
        "requestId": request,
        "message": {
            "id": message,
            "model": "claude-sonnet-4-6",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_input_tokens": cache_write,
                "cache_read_input_tokens": cache_read,
            },
        },
    })
    .to_string()
}

fn usage_block_with_advisor_line(
    request: &str,
    message: &str,
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
) -> String {
    serde_json::json!({
        "timestamp": "2026-01-01T10:00:00.000Z",
        "requestId": request,
        "message": {
            "id": message,
            "model": "claude-sonnet-4-6",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_input_tokens": cache_write,
                "cache_read_input_tokens": cache_read,
                "iterations": [{
                    "type": "advisor_message",
                    "model": "claude-sonnet-4-6",
                    "input_tokens": 7,
                    "output_tokens": 3,
                    "cache_creation_input_tokens": 2,
                    "cache_read_input_tokens": 8,
                }],
            },
        },
    })
    .to_string()
}

#[test]
fn local_context_fold_counts_content_block_rows_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let repeated = usage_block_with_advisor_line("one", "msg-one", 10, 5, 20, 70);
    std::fs::write(&path, format!("{repeated}\n{repeated}\n")).unwrap();

    let first = refresh(&path, None, None).unwrap();
    assert_eq!(
        session_tokens(&first),
        &crate::agents::AgentSessionUsage {
            input_tokens: Some(17),
            output_tokens: Some(8),
            cache_creation_input_tokens: Some(22),
            cache_read_input_tokens: Some(78),
            thinking_tokens: None,
        }
    );
    let first_stat = first.transcript_stat.unwrap();
    let first_fold = first.spend_fold.into_set().unwrap();

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{repeated}").unwrap();
    writeln!(file, "{}", usage_block_line("two", "msg-two", 3, 2, 0, 5)).unwrap();
    let resumed = refresh(&path, Some(&first_stat), Some(&first_fold)).unwrap();

    assert_eq!(
        session_tokens(&resumed),
        &crate::agents::AgentSessionUsage {
            input_tokens: Some(20),
            output_tokens: Some(10),
            cache_creation_input_tokens: Some(22),
            cache_read_input_tokens: Some(83),
            thinking_tokens: None,
        }
    );
}

#[test]
fn local_context_fold_without_request_window_replays_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    let repeated = usage_block_line("one", "msg-one", 10, 5, 20, 70);
    std::fs::write(&path, format!("{repeated}\n{repeated}\n")).unwrap();
    let stat = crate::agents::TranscriptStat::from_path(&path).unwrap();
    let legacy = LocalSpendFold {
        cursor: crate::agents::spending::SpendCursor {
            offset: stat.len,
            ..crate::agents::spending::SpendCursor::default()
        },
        input: 999,
        ..LocalSpendFold::default()
    };

    let replayed = refresh(&path, Some(&stat), Some(&legacy)).unwrap();

    assert_eq!(session_tokens(&replayed).input_tokens, Some(10));
    assert!(
        replayed
            .spend_fold
            .as_set()
            .and_then(|fold| fold.last_requests.first())
            .is_some()
    );
}

fn refresh<'a>(
    path: &'a Path,
    prior_stat: Option<&'a crate::agents::TranscriptStat>,
    prior_fold: Option<&'a LocalSpendFold>,
) -> Option<crate::agents::LocalContextRefresh> {
    ClaudeAdapter.local_context_refresh(
        RefreshTrigger::Tick,
        &LocalContextRefreshCtx {
            agent_id: "session-1",
            model_hint: None,
            prior_session_name: None,
            current_transcript_path: Some(path.to_str().unwrap()),
            prior_transcript_path: Some(path.to_str().unwrap()),
            prior_transcript_stat: prior_stat,
            prior_spend_fold: prior_fold,
            shared_pricing_cache_path: &path.with_extension("pricing.json"),
        },
    )
}

fn session_tokens(
    refresh: &crate::agents::LocalContextRefresh,
) -> &crate::agents::AgentSessionUsage {
    refresh
        .context
        .tokens
        .as_value()
        .and_then(|tokens| tokens.session_usage.as_ref())
        .unwrap()
}

#[test]
fn local_context_fold_is_resumable_and_leaves_cost_statusline_owned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(&path, format!("{}\n", usage_line("one", 10, 5, 20, 70))).unwrap();

    let first = refresh(&path, None, None).unwrap();
    let first_usage = session_tokens(&first);
    assert_eq!(first_usage.cache_hit_percent(), Some(70));
    assert!(matches!(first.context.cost, FieldPatch::Keep));
    let first_stat = first.transcript_stat.unwrap();
    let first_fold = first.spend_fold.into_set().unwrap();

    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap(),
        "{}",
        usage_line("two", 10, 5, 0, 90)
    )
    .unwrap();
    let resumed = refresh(&path, Some(&first_stat), Some(&first_fold)).unwrap();
    let usage = session_tokens(&resumed);
    assert_eq!(
        usage,
        &crate::agents::AgentSessionUsage {
            input_tokens: Some(20),
            output_tokens: Some(10),
            cache_creation_input_tokens: Some(20),
            cache_read_input_tokens: Some(160),
            thinking_tokens: None,
        }
    );
    assert_eq!(usage.cache_hit_percent(), Some(80));
    assert!(matches!(resumed.context.cost, FieldPatch::Keep));
}

#[test]
fn empty_fold_keeps_an_established_current_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-1.jsonl");
    std::fs::write(&path, "{\"type\":\"user\"}\n").unwrap();

    let refresh = refresh(&path, None, None).unwrap();
    assert!(matches!(
        refresh.context.tokens,
        crate::agents::LocalTokenPatch::Keep
    ));

    let mut context = crate::agents::AgentContext {
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(200_000),
            used_percentage: Some(25),
            current_context_tokens: Some(50_000),
            ..AgentTokenUsage::default()
        }),
        ..Default::default()
    };
    refresh.context.apply(&mut context, ClaudeAdapter.spec());

    let tokens = context.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(200_000));
    assert_eq!(tokens.used_percentage, Some(25));
    assert_eq!(tokens.current_context_tokens, Some(50_000));
}

#[test]
fn local_context_refresh_stat_gates_and_discovers_session_file() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("projects").join("-repo");
    std::fs::create_dir_all(&project).unwrap();
    let path = project.join("session-1.jsonl");
    std::fs::write(&path, format!("{}\n", usage_line("one", 10, 5, 0, 90))).unwrap();

    assert_eq!(
        super::super::local_context::find_session_transcript_under(
            &[dir.path().to_path_buf()],
            "session-1",
        ),
        Some(path.clone())
    );
    let first = refresh(&path, None, None).unwrap();
    assert!(
        refresh(
            &path,
            first.transcript_stat.as_ref(),
            first.spend_fold.as_set(),
        )
        .is_none()
    );
}

#[test]
fn session_only_refresh_preserves_an_established_current_window() {
    let refresh = crate::agents::LocalTokenPatch::PreserveEstablished(Some(AgentTokenUsage {
        session_usage: Some(crate::agents::AgentSessionUsage {
            input_tokens: Some(10),
            cache_read_input_tokens: Some(90),
            ..Default::default()
        }),
        ..Default::default()
    }));
    let mut context = crate::agents::AgentContext {
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(200_000),
            used_percentage: Some(25),
            current_context_tokens: Some(50_000),
            ..AgentTokenUsage::default()
        }),
        ..Default::default()
    };
    crate::agents::LocalContextPatch {
        tokens: refresh,
        ..Default::default()
    }
    .apply(&mut context, ClaudeAdapter.spec());

    let tokens = context.tokens.unwrap();
    assert_eq!(tokens.used_percentage, Some(25));
    assert_eq!(
        tokens
            .session_usage
            .and_then(|usage| usage.cache_hit_percent()),
        Some(90)
    );
}
