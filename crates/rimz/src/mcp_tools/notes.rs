//! `notes_add` and `notes_list`: the jobs of `rimz notes add` and `rimz notes list`.

use serde::Serialize;

use super::Tools;
use crate::harness::notes::{
    self, AddRequest, FILTER_GRAMMAR, FilterField, LIST_DEFAULT, ListRequest,
};
use crate::mcp::catalog::{NotesAddArgs, NotesListArgs};
use crate::mcp::{Answer, Failure};

pub(super) fn add(tools: &Tools, args: NotesAddArgs) -> Result<Answer, Failure> {
    let body = match (args.body, &args.file) {
        (Some(body), _) => body.into_bytes(),
        (None, Some(file)) => {
            let path = tools.start_dir.join(file);
            std::fs::read(&path)
                .map_err(|err| Failure::new(format!("reading {}: {err}", path.display())))?
        }
        (None, None) => Vec::new(),
    };
    let request = AddRequest {
        kind: args.kind,
        title: args.title,
        body,
        fence: args.fence,
        meta: args
            .meta
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        root: None,
    };
    let added =
        notes::add(&request, &tools.start_dir).map_err(|err| Failure::new(err.to_string()))?;
    Ok(Answer::data(&added))
}

/// What the filter `help` answers: every field a filter names and the grammar.
#[derive(Serialize)]
struct FilterHelp {
    fields: Vec<FilterField>,
    grammar: [&'static str; 4],
}

pub(super) fn list(args: NotesListArgs) -> Result<Answer, Failure> {
    if notes::asks_filter_help(&args.filter) {
        return Ok(Answer::data(&FilterHelp {
            fields: notes::filter_fields(),
            grammar: FILTER_GRAMMAR,
        }));
    }
    let listing = notes::list(&ListRequest {
        filters: args.filter,
        limit: LIST_DEFAULT,
    })
    .map_err(|err| Failure::new(err.to_string()))?;
    Ok(Answer::data(&listing))
}
