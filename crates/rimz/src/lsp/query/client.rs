//! Checkout selection and semantic query composition over the broker protocol.

use super::*;
use crate::lsp::registry::{self, Entry, State};
use serde_json::json;
use std::time::Duration;

pub enum Output {
    Answer {
        result: Value,
        document_uri: Option<String>,
    },
    Ambiguous {
        name: String,
        symbols: Vec<SymbolInformation>,
    },
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
        } else if crate::diag::lsp::recent().iter().any(|record| {
            record.root == root
                && record.event == "refused"
                && servers.get(&record.server).is_some_and(|config| {
                    server.map_or_else(
                        || {
                            extension.is_none_or(|extension| {
                                config.extensions.iter().any(|ext| ext == extension)
                            })
                        },
                        |server| record.server == server,
                    )
                })
                && record.details.get("estimate_bytes").is_some()
        }) {
            UnavailableReason::MemoryShort
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
            result: request(entry, "workspace/symbol", json!({"query": target}))?,
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
        Target::Symbol(name) => match resolve_symbol(
            &name,
            request(
                entry,
                "workspace/symbol",
                json!({"query": name.rsplit("::").next().unwrap_or(&name)}),
            )?,
        )? {
            SymbolResolution::Missing => {
                return Ok(Output::Answer {
                    result: Value::Null,
                    document_uri: None,
                });
            }
            SymbolResolution::Ambiguous(symbols) => return Ok(Output::Ambiguous { name, symbols }),
            SymbolResolution::Unique(location) => (location.uri, location.range.start),
        },
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
        for item in result.as_array().into_iter().flatten() {
            let result = request(entry, method, json!({"item": item}))?;
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
