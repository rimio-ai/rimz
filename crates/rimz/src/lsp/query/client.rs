//! Checkout selection and semantic query composition over the broker protocol.

use super::*;
use crate::lsp::registry::{self, Entry, State};
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    let failed = AtomicBool::new(false);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        for _ in 0..QUERY_WORKERS.min(items.len()) {
            scope.spawn(|| {
                // After an error, claim nothing new: every lower index is already claimed, so the
                // first error in input order still completes and wins.
                while !failed.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = map(item);
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
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
        unresolved: usize,
    },
    NotFound {
        name: String,
        symbols: Vec<SymbolInformation>,
        unresolved: usize,
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
    if server.is_none()
        && !servers.is_empty()
        && let Some(extension) = extension
        && !servers
            .values()
            .any(|config| config.extensions.iter().any(|ext| ext == extension))
    {
        return Err(QueryErr::UncoveredExtension {
            extension: extension.into(),
            coverage: servers
                .iter()
                .map(|(name, config)| (name.clone(), config.extensions.clone()))
                .collect(),
        });
    }
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
        return Err(QueryErr::MultipleServers {
            root: root.into(),
            servers: entries.iter().map(|entry| entry.server.clone()).collect(),
        });
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
        return Err(QueryErr::stopped(&entry, reason));
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
        &json!({"op": "query", "method": method, "params": params, "wait_ms": 110_000}),
        // Outlasts the broker's 110s bound so exit 4 reports before a 120s agent shell timeout.
        Duration::from_secs(115),
    )?;
    if let Some(refused) = response.get("refused") {
        return Err(QueryErr::MemoryShort {
            root: entry.root.clone(),
            server: entry.server.clone(),
            shortfall: serde_json::from_value(refused.clone()).ok().map(Box::new),
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
            let reason = serde_json::from_value(error["message"].clone()).map_err(LspErr::from)?;
            return Err(QueryErr::stopped(entry, &reason));
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

fn request_outlines(
    entry: &Entry,
    symbols: &[SymbolInformation],
) -> std::result::Result<BTreeMap<String, Symbols>, QueryErr> {
    let uris: BTreeSet<_> = symbols
        .iter()
        .map(|symbol| symbol.location.uri.as_str())
        .collect();
    if uris.len() > MEMBER_CONTAINER_CAP {
        return Ok(BTreeMap::new());
    }
    let outlines = bounded_map(&uris.into_iter().collect::<Vec<_>>(), |uri| {
        let outline = request(
            entry,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}}),
        )?;
        if outline.is_null() {
            return Ok::<_, QueryErr>(None);
        }
        Ok(Some((
            uri.to_string(),
            serde_json::from_value(outline).map_err(LspErr::from)?,
        )))
    })?;
    Ok(outlines.into_iter().flatten().collect())
}

pub fn execute(
    entry: &Entry,
    verb: Verb,
    target: &Target,
) -> std::result::Result<Output, QueryErr> {
    let (uri, position) = match (verb, target) {
        (Verb::Find, Target::Find(target)) => {
            return Ok(Output::Answer {
                result: rank_find(
                    &entry.root,
                    target,
                    request(entry, "workspace/symbol", json!({"query": target}))?,
                )?,
                document_uri: None,
            });
        }
        (Verb::Symbols, Target::File(path)) => {
            let uri = uri(&entry.root, path)?;
            return Ok(Output::Answer {
                result: request(
                    entry,
                    "textDocument/documentSymbol",
                    json!({"textDocument": {"uri": uri}}),
                )?,
                document_uri: Some(uri),
            });
        }
        (Verb::Find | Verb::Symbols, _) | (_, Target::Find(_) | Target::File(_)) => {
            return Err(
                LspErr::Protocol(format!("{verb:?} cannot answer target {target:?}")).into(),
            );
        }
        (_, Target::Position { path, position }) => (uri(&entry.root, path)?, *position),
        (_, Target::Symbol(name)) => {
            let mut resolution = resolve_symbol(
                &entry.root,
                name,
                request(
                    entry,
                    "workspace/symbol",
                    json!({"query": name_segments(name).last().cloned().unwrap_or_default()}),
                )?,
            )?;
            let mut enriched = false;
            if let SymbolResolution::Missing { candidates } = &resolution
                && name_segments(name).len() > 1
            {
                let narrowed = narrow_candidates(&entry.root, name, candidates)?;
                let mut outlines = request_outlines(entry, &narrowed)?;
                let mut rematched = rematch_enriched(&entry.root, name, &narrowed, &outlines)?;
                if rematched.is_none()
                    && candidates
                        .iter()
                        .map(|symbol| &symbol.location.uri)
                        .collect::<BTreeSet<_>>()
                        .len()
                        <= MEMBER_CONTAINER_CAP
                {
                    let requested: BTreeSet<_> =
                        narrowed.iter().map(|symbol| &symbol.location.uri).collect();
                    let remaining: Vec<_> = candidates
                        .iter()
                        .filter(|symbol| !requested.contains(&symbol.location.uri))
                        .cloned()
                        .collect();
                    outlines.extend(request_outlines(entry, &remaining)?);
                    rematched = rematch_enriched(&entry.root, name, candidates, &outlines)?;
                }
                if let Some(rematched) = rematched {
                    resolution = rematched;
                    enriched = true;
                }
            }
            // At most COLLAPSE_CAP ranked candidates, or one unique match.
            let definition = |symbols: &[SymbolInformation]| {
                bounded_map(symbols, |symbol| {
                    Ok::<_, QueryErr>(locations(request(
                        entry,
                        "textDocument/definition",
                        json!({
                            "textDocument": {"uri": symbol.location.uri},
                            "position": symbol.location.range.start,
                        }),
                    )?)?)
                })
            };
            let mut unresolved = 0;
            let resolution = match resolution {
                SymbolResolution::Missing { candidates } => {
                    let (file, chain) = file_qualified(name)
                        .map_or((None, name.as_str()), |(file, chain)| (Some(file), chain));
                    let mut qualifier = name_segments(chain);
                    let member = qualifier.pop().unwrap_or_default();
                    let mut members = Vec::new();
                    if !qualifier.is_empty() {
                        let container = qualifier.join("::");
                        let containers = resolve_symbol(
                            &entry.root,
                            &file.map_or(container.clone(), |file| format!("{file}::{container}")),
                            request(
                                entry,
                                "workspace/symbol",
                                json!({"query": qualifier.last()}),
                            )?,
                        )?;
                        let mut containers = match containers {
                            SymbolResolution::Missing { .. } => Vec::new(),
                            found => found.into_symbols(),
                        };
                        if containers.len() > MEMBER_CONTAINER_CAP {
                            containers.clear();
                        }
                        containers.sort_by(|a, b| a.location.uri.cmp(&b.location.uri));
                        let outlines = request_outlines(entry, &containers)?;
                        for container in &containers {
                            let Some(outline) = outlines.get(&container.location.uri) else {
                                continue;
                            };
                            members.extend(outline_members(container, &member, outline));
                        }
                    }
                    if members.is_empty() {
                        let (resolution, unresolved) =
                            collapse_ranked(&entry.root, name, candidates, definition)?;
                        return Ok(Output::NotFound {
                            name: name.clone(),
                            symbols: resolution.into_symbols(),
                            unresolved,
                        });
                    }
                    if members.len() == 1 {
                        SymbolResolution::Unique(members.remove(0))
                    } else {
                        SymbolResolution::Ambiguous(members)
                    }
                }
                SymbolResolution::Unique(symbol) => collapse_symbols(vec![symbol], definition)?,
                SymbolResolution::Ambiguous(mut symbols) => {
                    if !enriched {
                        let outlines = request_outlines(entry, &symbols)?;
                        if enrich_containers(&mut symbols, &outlines) {
                            symbols = prefer_exact(&entry.root, name, symbols)?;
                        }
                    }
                    let (resolution, remaining) =
                        collapse_ranked(&entry.root, name, symbols, definition)?;
                    unresolved = remaining;
                    resolution
                }
            };
            match resolution {
                SymbolResolution::Missing { .. } => {
                    unreachable!("collapse_symbols receives a nonempty match set")
                }
                SymbolResolution::Ambiguous(symbols) => {
                    return Ok(Output::Ambiguous {
                        name: name.clone(),
                        symbols,
                        unresolved,
                    });
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
        Verb::Symbols | Verb::Find => {
            unreachable!("answered or refused before position resolution")
        }
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
