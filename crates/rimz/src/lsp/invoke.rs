//! The query, `check`, and `show` verbs run from a directory against the room's language
//! servers, answering the text `rimz lsp` prints and the exit code it ends with. The caller
//! presents the text and ends the process; nothing here writes stdout or exits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::check;
use super::query::{self, ListOptions, QueryErr, Verb};
use super::{registry, usage};

/// The checkout a call queries, its language servers, and the room usage log it counts in.
pub struct QueryContext {
    root: PathBuf,
    entries: Vec<registry::Entry>,
    servers: BTreeMap<String, crate::config::LspServerConfig>,
    usage: Option<usage::Log>,
    /// The call's directory below `root`, which relative targets resolve against.
    folder: PathBuf,
    dir: PathBuf,
}

/// A `server` no configured or registered language server is named.
#[derive(Debug, thiserror::Error)]
#[error("no language server named {server}; configured: {configured}")]
pub struct UnknownServer {
    server: String,
    configured: String,
}

/// What a verb answered beyond its text: the exit code its outcomes rank to, and the error
/// of a call on one target that failed, which ends the answer in its place.
pub struct Answered {
    pub code: i32,
    pub error: Option<QueryErr>,
}

impl QueryContext {
    /// Open the checkout `dir` (or `root_override`) lies in, reviving its admitted servers.
    /// `pinned_room` is the room a verified pane pin names, which counts the usage unless
    /// `root_override` is given.
    pub fn open(
        dir: &Path,
        root_override: Option<PathBuf>,
        pinned_room: Option<PathBuf>,
    ) -> Result<Self> {
        let mut entries = registry::read_entries()?;
        let cwd = std::fs::canonicalize(root_override.clone().unwrap_or_else(|| dir.to_owned()))?;
        let workspace = crate::workspace::WorkspaceResolver::resolve(&cwd, root_override.clone())?;
        let root = registry::enclosing_checkout(&cwd, &entries)
            .unwrap_or(&workspace.worktree_root)
            .to_owned();
        let machine = crate::config::MachineConfig::load()?;
        let config = crate::config::effective::load(&machine, workspace.launch_repo_root())?;
        let runtime = crate::RuntimePaths::for_project_root(&workspace.project_root)?;
        if super::admission::revive(&super::admission::AdmissionRequest {
            root: &root,
            project: workspace.launch_repo_root(),
            servers: &config.lsp_servers,
            untrusted_servers: &[],
            policy: &machine.lsp,
            runtime: &runtime,
        })? {
            entries = registry::read_entries()?;
        }
        // The participant ladder departs from `resolve` only to follow a verified pane pin.
        let room = match root_override {
            None => pinned_room.unwrap_or(workspace.project_root),
            Some(_) => workspace.project_root,
        };
        let folder = query_folder(&root, std::fs::canonicalize(dir));
        Ok(Self {
            root,
            entries,
            servers: config.lsp_servers,
            usage: room_usage(&room),
            folder,
            dir: dir.to_owned(),
        })
    }

    /// Refuse a `server` that is neither configured nor registered for this checkout.
    pub fn validate_server(&self, server: Option<&str>) -> Result<(), UnknownServer> {
        let Some(server) = server else {
            return Ok(());
        };
        validate_server_name(
            server,
            self.servers.keys().map(String::as_str),
            self.entries.iter().filter(|entry| entry.root == self.root),
        )
    }
}

/// Refuse a `server` named by neither `configured` nor `registered`.
pub fn validate_server_name<'a>(
    server: &str,
    configured: impl Iterator<Item = &'a str>,
    registered: impl Iterator<Item = &'a registry::Entry>,
) -> Result<(), UnknownServer> {
    let names: std::collections::BTreeSet<_> = configured
        .chain(registered.map(|entry| entry.server.as_str()))
        .collect();
    if names.contains(server) {
        return Ok(());
    }
    let configured = if names.is_empty() {
        "none configured".to_owned()
    } else {
        names
            .into_iter()
            .map(crate::packs::one_line)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(UnknownServer {
        server: server.to_owned(),
        configured,
    })
}

/// The part of the canonical `dir` below `root`, or nothing when it lies elsewhere.
fn query_folder(root: &Path, dir: std::io::Result<PathBuf>) -> PathBuf {
    dir.ok()
        .and_then(|dir| dir.strip_prefix(root).ok().map(Path::to_owned))
        .unwrap_or_default()
}

/// The usage log of the room at `room`, the one `rimz lsp list` reads.
fn room_usage(room: &Path) -> Option<usage::Log> {
    match crate::StatePaths::for_project_root(room) {
        Ok(paths) => Some(usage::Log::new(&paths)),
        Err(error) => {
            tracing::debug!(%error, "cannot resolve language-server usage room");
            None
        }
    }
}

/// The exit code of two outcomes together: the more telling of `current` and `next`.
pub fn exit_rank(current: i32, next: i32) -> i32 {
    [3, 4, 1, 7, 6, 5, 0]
        .into_iter()
        .find(|code| *code == current || *code == next)
        .unwrap_or(0)
}

#[derive(Default)]
struct QueryFailures(BTreeMap<String, QueryErr>);

impl QueryFailures {
    fn execute(
        &mut self,
        server: &str,
        request: impl FnOnce() -> std::result::Result<query::Output, QueryErr>,
    ) -> std::result::Result<query::Output, QueryErr> {
        if let Some(error) = self.0.get(server).and_then(QueryErr::server_error) {
            return Err(error);
        }
        let result = request();
        if let Err(error) = &result
            && let Some(error) = error.server_error()
        {
            self.0.insert(server.to_owned(), error);
        }
        result
    }
}

/// Ask `verb` of each target in turn, handing `emit` each target's text as it is answered,
/// headed by the target when there are several. A lone target's failure ends the answer as
/// its `error`.
pub fn query(
    context: &QueryContext,
    verb: Verb,
    targets: &[String],
    server: Option<&str>,
    json: bool,
    options: ListOptions,
    mut emit: impl FnMut(&str) -> Result<()>,
) -> Result<Answered> {
    let kind: String = serde_json::from_value(serde_json::to_value(verb)?)?;
    let root = &context.root;
    let folder = &context.folder;
    let multiple = targets.len() > 1;
    let mut failures = QueryFailures::default();
    let mut code = 0;
    let mut values = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        let result = (|| {
            let target = match query::Target::locate(verb, target, root, folder) {
                Ok(target) => target,
                Err(super::LspErr::MissingColumn { path, line, .. }) => {
                    let path = query::typed_path(root, folder, &path)?;
                    let outline = query::select(
                        root,
                        context.entries.clone(),
                        &context.servers,
                        server,
                        Some(&path),
                    )
                    .ok()
                    .and_then(|entry| {
                        failures
                            .execute(&entry.server, || {
                                query::execute(
                                    &entry,
                                    Verb::Symbols,
                                    &query::Target::File(path.clone()),
                                )
                            })
                            .ok()
                    })
                    .and_then(|output| match output {
                        query::Output::Answer { result, .. } => Some(result),
                        _ => None,
                    });
                    return Err(query::missing_column(root, &path, line, outline));
                }
                Err(error) => return Err(error.into()),
            };
            let entry = query::select(
                root,
                context.entries.clone(),
                &context.servers,
                server,
                target.path(),
            )?;
            let output = failures.execute(&entry.server, || query::execute(&entry, verb, &target));
            if let Some(usage) = &context.usage {
                usage.record(&entry, &kind, &output);
            }
            Ok::<_, QueryErr>((entry, output?))
        })();
        let text = match result {
            Err(error) => {
                if !multiple {
                    return Ok(Answered {
                        code,
                        error: Some(error),
                    });
                }
                code = exit_rank(code, error.exit_code());
                if json {
                    values.push(serde_json::json!({
                        "target": target, "outcome": "error", "exit": error.exit_code(),
                        "error": error.to_string(),
                    }));
                    continue;
                }
                format!("error: {error}\n")
            }
            Ok((entry, output)) => {
                code = exit_rank(code, output.exit_code());
                let output = match output {
                    query::Output::Answer {
                        result,
                        document_uri,
                    } if json => query::Output::Answer {
                        result: query::json_targets(verb, root, document_uri.as_deref(), result)?,
                        document_uri,
                    },
                    output => output,
                };
                if multiple && json {
                    let mut value = query::outcome_json(root, &output)?;
                    value["target"] = target.clone().into();
                    value["exit"] = output.exit_code().into();
                    values.push(value);
                    continue;
                }
                let dirty = query::dirty_documents(&entry);
                match output {
                    query::Output::Answer { result, .. } if json => {
                        format!("{}\n", serde_json::to_string_pretty(&result)?)
                    }
                    query::Output::Answer {
                        result,
                        document_uri,
                    } => query::render(
                        verb,
                        root,
                        document_uri.as_deref(),
                        result,
                        options,
                        &dirty,
                        |path| {
                            query::classification_outline(&entry, path).map_err(|error| match error
                            {
                                QueryErr::Failed(error) => error,
                                error => super::LspErr::Protocol(error.to_string()),
                            })
                        },
                    )?,
                    output => query::render_outcome(root, &output, json)?,
                }
            }
        };
        if multiple {
            emit(&format!(
                "{}==> {target} <==\n",
                if index == 0 { "" } else { "\n" }
            ))?;
        }
        emit(&text)?;
    }
    if multiple && json {
        emit(&format!("{}\n", serde_json::to_string_pretty(&values)?))?;
    }
    Ok(Answered { code, error: None })
}

/// Check the anchors in each of `files` (relative to the call's directory), fixing them per
/// `mode`, and hand `emit` each file's report. A lone file's failure ends the answer as its
/// `error`.
pub fn check(
    context: &QueryContext,
    files: &[PathBuf],
    json: bool,
    mode: check::Mode,
    mut emit: impl FnMut(&str) -> Result<()>,
) -> Result<Answered> {
    let reports = match check::run(
        files,
        &context.dir,
        &context.root,
        &context.entries,
        &context.servers,
        mode,
        context.usage.as_ref(),
    ) {
        Ok(reports) => reports,
        Err(error) => {
            return Ok(Answered {
                code: 0,
                error: Some(error),
            });
        }
    };
    let multiple = files.len() > 1;
    let mut code = 0;
    let mut values = Vec::new();
    for (file, result) in files.iter().zip(reports) {
        match result {
            Ok(report) => {
                code = exit_rank(code, report.exit_code());
                if multiple && json {
                    let mut value = serde_json::to_value(&report)?;
                    value["exit"] = report.exit_code().into();
                    values.push(value);
                } else {
                    emit(&report.render(json)?)?;
                }
            }
            Err(error) => {
                if !multiple {
                    return Ok(Answered {
                        code,
                        error: Some(error),
                    });
                }
                code = exit_rank(code, error.exit_code());
                if json {
                    values.push(serde_json::json!({
                        "notes": file.display().to_string(),
                        "exit": error.exit_code(),
                        "error": error.to_string(),
                    }));
                } else {
                    emit(&format!("{}  error  {error}\n", file.display()))?;
                }
            }
        }
    }
    if multiple && json {
        emit(&format!("{}\n", serde_json::to_string_pretty(&values)?))?;
    }
    Ok(Answered { code, error: None })
}

/// What `show` read: its blocks as one text, the exit code they rank to, and, when one anchor
/// gave one block that shows no code (a nonzero exit), that block as the error, as `query`
/// and `check` answer one target that fails.
pub struct Shown {
    pub text: String,
    pub code: i32,
    pub error: Option<String>,
}

/// Show the code each of `anchors` names, as one text with a blank line between blocks.
pub fn show(
    context: &QueryContext,
    anchors: &[String],
    full: bool,
    server: Option<&str>,
) -> super::Result<Shown> {
    let blocks = check::show(
        anchors,
        full,
        &context.root,
        &context.folder,
        &context.entries,
        &context.servers,
        server,
        context.usage.as_ref(),
    )?;
    let mut text = String::new();
    let mut code = 0;
    for (index, (block, exit)) in blocks.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        text.push_str(block);
        code = exit_rank(code, *exit);
    }
    let error = match (anchors, blocks.as_slice()) {
        ([_], [(block, 1..)]) => Some(block.trim_end().to_owned()),
        _ => None,
    };
    Ok(Shown { text, code, error })
}

#[cfg(test)]
mod tests;
