//! The conversation follow driver shared by `rimz transcript -f` and `rimz agents logs -f`.
//!
//! The polled view is recomputed from the whole log on every poll, so it is not append-only:
//! an archive split shrinks it, a later entry can hide an earlier turn, and an arrival can sort
//! before the tail. Progress is therefore the set of printed entry identities, not a count.

use std::collections::HashSet;

use super::*;

pub(crate) fn follow(
    workspace: &rimz::ResolvedWorkspace,
    target: Option<&str>,
    worktree: Option<&str>,
    tail: Option<usize>,
    all: bool,
    json: bool,
    flat: bool,
) -> Result<()> {
    let paths = rimz::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing state paths")?;
    let read = || {
        chat_view_with_mode(
            workspace,
            &paths,
            target,
            worktree,
            None,
            all,
            ViewMode {
                hidden: Hidden::for_json(json),
                flat,
            },
        )
    };
    let mut initial = read()?;
    let mut cursor = FollowCursor::seed(&initial);
    initial.last = tail;
    if json {
        for entry in selected_lines(&initial) {
            render::finish(write_json_line(&entry))?;
        }
    } else if !selected_lines(&initial).is_empty() {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_render(render_lines_to(
            &mut out,
            &initial,
            &tz,
            Prose::for_stdout(),
        ))?;
    }

    let tz = crate::cli::machine_config().time_zone();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let view = read()?;
        let new_indexes = cursor.advance(&view);
        if new_indexes.is_empty() {
            continue;
        }
        if json {
            for index in new_indexes {
                render::finish(write_json_line(&view.entries[index].chat))?;
            }
        } else {
            let mut out = render::out();
            finish_render(render_selected_lines_to(
                &mut out,
                &view,
                &new_indexes,
                &tz,
                Prose::for_stdout(),
            ))?;
        }
    }
}

pub(crate) fn finish_render(write: Result<()>) -> Result<()> {
    render::finish(write.map_err(|err| match err.downcast::<std::io::Error>() {
        Ok(err) => err,
        Err(err) => std::io::Error::other(err),
    }))
}

fn write_json_line(value: &impl serde::Serialize) -> std::io::Result<()> {
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
}

#[derive(PartialEq, Eq, Hash)]
enum EntryKey {
    Log {
        agent: AgentKey,
        kind: TranscriptKind,
        at: Option<jiff::Timestamp>,
        delivered_at: Option<jiff::Timestamp>,
        message_id: Option<String>,
        text: String,
    },
    Stage {
        at: Option<jiff::Timestamp>,
        team: String,
        to: String,
        by: String,
    },
}

impl EntryKey {
    fn for_entry(entry: &RenderEntry) -> Self {
        match &entry.source {
            LineSource::Log { kind, agent, .. } => Self::Log {
                agent: agent.clone(),
                kind: *kind,
                at: entry.chat.at,
                delivered_at: entry.chat.delivered_at,
                message_id: entry.chat.message_id.clone(),
                text: entry.chat.text.clone(),
            },
            LineSource::Stage => {
                // render_entry_for_flip always pairs LineSource::Stage with Some(StageLine).
                let StageLine { team, to, by, .. } = entry
                    .chat
                    .stage
                    .as_ref()
                    .expect("stage entries carry stage metadata");
                Self::Stage {
                    at: entry.chat.at,
                    team: team.clone(),
                    to: to.clone(),
                    by: by.clone(),
                }
            }
        }
    }
}

pub(super) struct FollowCursor {
    seen: HashSet<EntryKey>,
}

impl FollowCursor {
    pub(super) fn seed(view: &RenderedChat) -> Self {
        Self {
            seen: view.entries.iter().map(EntryKey::for_entry).collect(),
        }
    }

    pub(super) fn advance(&mut self, view: &RenderedChat) -> Vec<usize> {
        view.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                self.seen
                    .insert(EntryKey::for_entry(entry))
                    .then_some(index)
            })
            .collect()
    }
}
