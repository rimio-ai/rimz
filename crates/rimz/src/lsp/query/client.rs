//! Checkout selection and semantic query composition over the broker protocol.

use super::*;
use crate::lsp::registry::{self, Entry, State};
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A qualifier matching more symbols than this skips the member fallback: each match costs an
/// outline request, and a broad qualifier like `tests` matches hundreds of modules.
const MEMBER_CONTAINER_CAP: usize = 20;

const QUERY_WORKERS: usize = 4;

fn bounded_map<T: Sync, R: Send, E: Send>(
    items: &[T],
    map: impl Fn(&T) -> std::result::Result<R, E> + Sync,
) -> std::result::Result<Vec<R>, E> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        for _ in 0..QUERY_WORKERS.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = map(item);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

pub enum Output {
    Answer {
        result: Value,
        document_uri: Option<String>,
    },
    Ambiguous {
        name: String,
        symbols: Vec<SymbolInformation>,
    },
    NotFound {
        name: String,
        symbols: Vec<SymbolInformation>,
    },
}

impl Output {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Answer { .. } => 0,
            Self::NotFound { .. } => 5,
            Self::Ambiguous { .. } => 6,
        }
    }
}

pub fn select(
    root: &Path,
    entries: Vec<Entry>,
    servers: &BTreeMap<String, crate::config::LspServerConfig>,
    server: Option<&str>,
    path: Option<&Path>,
) -> std::result::Result<Entry, QueryErr> {
    let extension = path.and_then(Path::extension).and_then(|ext| ext.to_str());
    let mut entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.root == root)
        .filter(|entry| {
            if let Some(server) = server {
                return entry.server == server;
            }
            extension.is_none_or(|extension| {
                servers
                    .get(&entry.server)
                    .is_some_and(|config| config.extensions.iter().any(|ext| ext == extension))
            })
        })
        .collect();
    if entries.len() > 1 {
        return Err(LspErr::Configuration(
            "multiple language servers; choose --server NAME".into(),
        )
        .into());
    }
    let entry = entries.pop().ok_or_else(|| QueryErr::Unavailable {
        root: root.into(),
        reason: if servers.is_empty() {
            UnavailableReason::NoneConfigured
        } else {
            UnavailableReason::NotRunning
        },
    })?;
    if let State::Stopped { reason, .. } = &entry.state {
        return Err(QueryErr::Unavailable {
            root: root.into(),
            reason: UnavailableReason::stopped(reason),
        });
    }
    if !registry::is_live(&entry) {
        return Err(QueryErr::Unavailable {
            root: root.into(),
            reason: UnavailableReason::Crashed,
        });
    }
    Ok(entry)
}

fn request(entry: &Entry, method: &str, params: Value) -> std::result::Result<Value, QueryErr> {
    let response = registry::request(
        entry,
        &json!({"op": "query", "method": method, "params": params, "wait_ms": 30_000}),
        Duration::from_secs(95),
    )?;
    if response.get("refused").is_some() {
        return Err(QueryErr::Unavailable {
            root: entry.root.clone(),
            reason: UnavailableReason::MemoryShort,
        });
    }
    if let Some(indexing) = response.get("indexing") {
        return Err(QueryErr::Indexing {
            server: entry.server.clone(),
            seconds: indexing["elapsed_ms"].as_u64().unwrap_or(0) / 1000,
        });
    }
    if let Some(error) = response.get("error") {
        if error["code"] == -32003 {
            return Err(QueryErr::Unavailable {
                root: entry.root.clone(),
                reason: UnavailableReason::stopped(
                    &serde_json::from_value(error["message"].clone()).map_err(LspErr::from)?,
                ),
            });
        }
        return Err(LspErr::Protocol(error.to_string()).into());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| LspErr::Protocol("query response has no result".into()).into())
}

fn uri(root: &Path, path: &Path) -> Result<String> {
    let path = std::fs::canonicalize(root.join(path))?;
    url::Url::from_file_path(path)
        .map(|uri| uri.to_string())
        .map_err(|()| LspErr::Protocol("invalid file URI".into()))
}

pub fn execute(entry: &Entry, verb: Verb, target: &str) -> std::result::Result<Output, QueryErr> {
    if verb == Verb::Find {
        return Ok(Output::Answer {
            result: rank_find(
                &entry.root,
                target,
                request(entry, "workspace/symbol", json!({"query": target}))?,
            )?,
            document_uri: None,
        });
    }
    if verb == Verb::Symbols {
        let uri = uri(&entry.root, Path::new(target))?;
        return Ok(Output::Answer {
            result: request(
                entry,
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": uri}}),
            )?,
            document_uri: Some(uri),
        });
    }
    let (uri, position) = match parse_target(target)? {
        Target::Position { path, position } => (uri(&entry.root, &path)?, position),
        Target::Symbol(name) => {
            let resolution = resolve_symbol(
                &entry.root,
                &name,
                request(
                    entry,
                    "workspace/symbol",
                    json!({"query": name_segments(&name).last().cloned().unwrap_or_default()}),
                )?,
            )?;
            let definition = |location: &Location| {
                Ok(locations(request(
                    entry,
                    "textDocument/definition",
                    json!({
                        "textDocument": {"uri": location.uri},
                        "position": location.range.start,
                    }),
                )?)?)
            };
            let resolution = match resolution {
                SymbolResolution::Missing { candidates } => {
                    let mut qualifier = name_segments(&name);
                    let member = qualifier.pop().unwrap_or_default();
                    let mut members = Vec::new();
                    if !qualifier.is_empty() {
                        let containers = resolve_symbol(
                            &entry.root,
                            &qualifier.join("::"),
                            request(
                                entry,
                                "workspace/symbol",
                                json!({"query": qualifier.last()}),
                            )?,
                        )?;
                        let mut containers = match containers {
                            SymbolResolution::Missing { .. } => Vec::new(),
                            SymbolResolution::Unique(symbol) => vec![symbol],
                            SymbolResolution::Ambiguous(symbols) => symbols,
                        };
                        if containers.len() > MEMBER_CONTAINER_CAP {
                            containers.clear();
                        }
                        containers.sort_by(|a, b| a.location.uri.cmp(&b.location.uri));
                        let groups: Vec<_> = containers
                            .chunk_by(|a, b| a.location.uri == b.location.uri)
                            .collect();
                        // At most MEMBER_CONTAINER_CAP distinct-file outline requests.
                        let outlines = bounded_map(&groups, |group| {
                            let outline = request(
                                entry,
                                "textDocument/documentSymbol",
                                json!({"textDocument": {"uri": group[0].location.uri}}),
                            )?;
                            if outline.is_null() {
                                return Ok::<_, QueryErr>(None);
                            }
                            Ok(Some(
                                serde_json::from_value::<Symbols>(outline).map_err(LspErr::from)?,
                            ))
                        })?;
                        for (group, outline) in groups.into_iter().zip(outlines) {
                            let Some(outline) = outline else { continue };
                            for container in group {
                                members.extend(outline_members(container, &member, &outline));
                            }
                        }
                    }
                    if members.is_empty() {
                        return Ok(Output::NotFound {
                            name,
                            symbols: collapse_candidates(candidates, definition)?,
                        });
                    }
                    if members.len() == 1 {
                        SymbolResolution::Unique(members.remove(0))
                    } else {
                        SymbolResolution::Ambiguous(members)
                    }
                }
                SymbolResolution::Unique(symbol) => collapse_symbols(vec![symbol], definition)?,
                SymbolResolution::Ambiguous(symbols) => collapse_symbols(symbols, definition)?,
            };
            match resolution {
                SymbolResolution::Missing { .. } => {
                    unreachable!("collapse_symbols receives a nonempty match set")
                }
                SymbolResolution::Ambiguous(symbols) => {
                    return Ok(Output::Ambiguous { name, symbols });
                }
                SymbolResolution::Unique(symbol) => {
                    (symbol.location.uri, symbol.location.range.start)
                }
            }
        }
    };
    let mut params = json!({"textDocument": {"uri": uri}, "position": position});
    let method = match verb {
        Verb::Def => "textDocument/definition",
        Verb::Refs => {
            params["context"] = json!({"includeDeclaration": true});
            "textDocument/references"
        }
        Verb::Hover => "textDocument/hover",
        Verb::Impl => "textDocument/implementation",
        Verb::Callers | Verb::Callees => "textDocument/prepareCallHierarchy",
        Verb::Symbols | Verb::Find => unreachable!("handled before position resolution"),
    };
    let mut result = request(entry, method, params)?;
    if matches!(verb, Verb::Callers | Verb::Callees) {
        let method = if verb == Verb::Callers {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        let mut calls = Vec::new();
        // One request per prepared item (normally one); four in flight, no item-count cap.
        let results = bounded_map(
            result.as_array().map(Vec::as_slice).unwrap_or_default(),
            |item| request(entry, method, json!({"item": item})),
        )?;
        for result in results {
            if let Some(results) = result.as_array() {
                calls.extend(results.iter().cloned());
            }
        }
        result = Value::Array(calls);
    }
    Ok(Output::Answer {
        result,
        document_uri: Some(uri),
    })
}
