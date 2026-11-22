//! `notes`: the job of `rimz notes add`.

use super::Tools;
use crate::harness::notes::{self, AddRequest};
use crate::mcp::catalog::NotesArgs;
use crate::mcp::{Answer, Failure};

pub(super) fn add(tools: &Tools, args: NotesArgs) -> Result<Answer, Failure> {
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
