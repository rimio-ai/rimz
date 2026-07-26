//! Resumable Claude transcript fold for session-cumulative token usage.

use std::fs;
use std::path::{Path, PathBuf};

use super::spend::{claude_config_dirs, parse_claude_spend};
use crate::agents::pricing;
use crate::agents::{
    AgentTokenUsage, FieldPatch, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx,
    LocalSpendFold, LocalTokenPatch, TranscriptStat,
};

pub(super) fn refresh(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    let path = existing_path(ctx.current_transcript_path)
        .or_else(|| existing_path(ctx.prior_transcript_path))
        .or_else(|| find_session_transcript(ctx.agent_id))?;
    let stat = TranscriptStat::from_path(&path)?;
    let reuses_prior_path = ctx.prior_transcript_path.map(Path::new) == Some(path.as_path());
    if reuses_prior_path
        && ctx
            .prior_transcript_stat
            .is_some_and(|prior| *prior == stat)
    {
        return None;
    }

    let mut fold = if reuses_prior_path {
        ctx.prior_spend_fold.cloned().unwrap_or_default()
    } else {
        LocalSpendFold::default()
    };
    if fold.cursor.offset > stat.len {
        fold = LocalSpendFold::default();
    }
    let prices = pricing::cached_book(ctx.shared_pricing_cache_path);
    let parsed = parse_claude_spend(&path, fold.cursor.offset, &prices);
    fold.absorb(&parsed.entries);
    fold.cursor = parsed.cursor;
    let session_usage = fold.session_usage();
    let tokens = session_usage.map_or(LocalTokenPatch::Keep, |session_usage| {
        LocalTokenPatch::PreserveEstablished(Some(AgentTokenUsage {
            session_usage: Some(session_usage),
            ..AgentTokenUsage::default()
        }))
    });

    Some(LocalContextRefresh {
        context: LocalContextPatch {
            tokens,
            ..LocalContextPatch::default()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        spend_fold: FieldPatch::Set(fold),
    })
}

fn existing_path(path: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(path?);
    path.is_file().then_some(path)
}

fn find_session_transcript(session_id: &str) -> Option<PathBuf> {
    find_session_transcript_under(&claude_config_dirs(), session_id)
}

pub(super) fn find_session_transcript_under(
    config_dirs: &[PathBuf],
    session_id: &str,
) -> Option<PathBuf> {
    let file_name = format!("{}.jsonl", session_id.trim());
    if session_id.trim().is_empty() {
        return None;
    }
    config_dirs
        .iter()
        .filter_map(|config_dir| fs::read_dir(config_dir.join("projects")).ok())
        .flat_map(|projects| projects.filter_map(Result::ok))
        .filter_map(|project| {
            let path = project.path().join(&file_name);
            let metadata = fs::metadata(&path).ok()?;
            metadata
                .is_file()
                .then_some((metadata.modified().ok(), path))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, path)| path)
}
