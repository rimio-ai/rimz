//! Shared language-server navigation and process inspection.

use super::{GlobalFlags, render};
use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::lsp::{
    query::{self, QueryErr, Verb},
    registry::{self, State},
};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Args)]
pub struct LspArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Find a definition by position or exact symbol name.
    Def(Query),
    /// Find references by position or exact symbol name.
    Refs(Query),
    /// Show type and documentation at a position or symbol.
    Hover(Query),
    /// Find implementations of a trait or interface.
    Impl(Query),
    /// Find functions calling this symbol.
    Callers(CallHierarchy),
    /// Find functions called by this symbol.
    Callees(CallHierarchy),
    /// Show a file's outline.
    Symbols(Query),
    /// Search workspace symbols.
    Find(Query),
    /// List shared servers on this machine.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Inspect a shared server and its attached editors.
    Status {
        checkout: Option<PathBuf>,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Free a shared server's memory; the next query restarts it.
    Stop {
        checkout: Option<PathBuf>,
        #[arg(long, conflicts_with = "all")]
        server: Option<String>,
        #[arg(long, conflicts_with = "checkout")]
        all: bool,
    },
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        request: String,
    },
}

#[derive(Debug, Args)]
struct Query {
    target: String,
    #[arg(long)]
    server: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CallHierarchy {
    #[command(flatten)]
    query: Query,
    /// Include callers/callees outside the checkout.
    #[arg(long)]
    external: bool,
}

pub fn run(args: LspArgs, globals: &GlobalFlags) -> Result<()> {
    let scope = match &args.command {
        Command::Callers(args) | Command::Callees(args) if args.external => query::Scope::External,
        _ => query::Scope::Checkout,
    };
    let (verb, args) = match args.command {
        Command::Serve { request } => {
            return Ok(rimz::lsp::broker::serve(serde_json::from_str(&request)?)?);
        }
        Command::List { json } => return list(json),
        Command::Status {
            checkout,
            server,
            json,
        } => return status(checkout, server, json, globals),
        Command::Stop {
            checkout,
            server,
            all,
        } => return stop(checkout, server, all, globals),
        Command::Def(args) => (Verb::Def, args),
        Command::Refs(args) => (Verb::Refs, args),
        Command::Hover(args) => (Verb::Hover, args),
        Command::Impl(args) => (Verb::Impl, args),
        Command::Callers(args) => (Verb::Callers, args.query),
        Command::Callees(args) => (Verb::Callees, args.query),
        Command::Symbols(args) => (Verb::Symbols, args),
        Command::Find(args) => (Verb::Find, args),
    };
    let entries = registry::read_entries()?;
    let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
    let workspace = rimz::workspace::WorkspaceResolver::resolve(&cwd, globals.root.clone())?;
    let root = registry::enclosing_checkout(&cwd, &entries).unwrap_or(&workspace.worktree_root);
    let machine = rimz::config::MachineConfig::load()?;
    let config = rimz::config::effective::load(&machine, workspace.launch_repo_root())?;
    let path = match verb {
        Verb::Symbols => Some(PathBuf::from(&args.target)),
        Verb::Find => None,
        _ => match query::parse_target(&args.target)? {
            query::Target::Position { path, .. } => Some(path),
            _ => None,
        },
    };
    let entry = match query::select(
        root,
        entries.clone(),
        &config.lsp_servers,
        args.server.as_deref(),
        path.as_deref(),
    ) {
        Ok(entry) => entry,
        Err(error) => return query_error(error),
    };
    let dirty = entry
        .attached
        .iter()
        .flat_map(|editor| &editor.open)
        .filter(|document| document.owner && document.dirty)
        .map(|document| document.uri.clone())
        .collect();
    let output = match query::execute(&entry, verb, &args.target) {
        Ok(output) => output,
        Err(error) => return query_error(error),
    };
    let mut out = render::out();
    match output {
        query::Output::Answer {
            result,
            document_uri,
        } => {
            if args.json {
                writeln!(out, "{}", serde_json::to_string_pretty(&result)?)?;
            } else {
                write!(
                    out,
                    "{}",
                    query::render(verb, root, document_uri.as_deref(), result, scope, &dirty)?
                )?;
            }
        }
        query::Output::Ambiguous { name, symbols } => {
            if args.json {
                writeln!(out, "{}", serde_json::to_string_pretty(&symbols)?)?;
            } else {
                write!(out, "{}", query::render_ambiguous(root, &name, &symbols)?)?;
            }
        }
    }
    Ok(())
}

fn query_error(error: QueryErr) -> Result<()> {
    let code = error.exit_code();
    if code == 1 {
        return Err(error.into());
    }
    writeln!(render::err(), "{error}")?;
    std::process::exit(code)
}

fn status(
    path: Option<PathBuf>,
    server: Option<String>,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let entries = {
        let _lock = registry::lock()?;
        registry::sweep_locked()?
    };
    let cwd = std::fs::canonicalize(
        path.or_else(|| globals.root.clone())
            .unwrap_or(std::env::current_dir()?),
    )?;
    let root = registry::enclosing_checkout(&cwd, &entries).unwrap_or(&cwd);
    let mut selected = entries.iter().filter(|entry| {
        entry.root == root && server.as_ref().is_none_or(|server| server == &entry.server)
    });
    let Some(entry) = selected.next() else {
        return query_error(QueryErr::Unavailable {
            root: root.to_owned(),
            reason: query::UnavailableReason::NotRunning,
        });
    };
    if selected.next().is_some() {
        anyhow::bail!("multiple language servers; choose --server NAME");
    }
    let entry: registry::Entry = serde_json::from_value(registry::request(
        entry,
        &serde_json::json!({"op":"status"}),
        Duration::from_secs(2),
    )?)?;
    let mut out = render::out();
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&entry)?)?;
        return Ok(());
    }
    let mut facts = render::KeyVals::new();
    facts.push("checkout", render::cell(entry.root.display().to_string()));
    facts.push("server", render::cell(&entry.server));
    facts.push(
        "state",
        render::cell(match entry.state {
            State::Starting => "starting".into(),
            State::Indexing => "indexing".into(),
            State::Ready => "ready".into(),
            State::Dormant { reason, .. } => {
                reason.map_or_else(|| "dormant".into(), |reason| format!("dormant: {reason}"))
            }
            State::Stopped { reason, .. } => format!("stopped: {reason}"),
        }),
    );
    facts.push("broker pid", render::cell(entry.broker_pid.to_string()));
    facts.push(
        "server pid",
        render::cell(
            entry
                .server_pid
                .map_or_else(|| "none".into(), |pid| pid.to_string()),
        ),
    );
    facts.push("requests", render::cell(entry.request_count.to_string()));
    facts.push("leases", render::cell(entry.leases.len().to_string()));
    facts.render(&mut out)?;
    for editor in entry.attached {
        let mut details = render::KeyVals::new();
        details.push(
            "editor",
            render::cell(format!(
                "{}  {}  attached {}s ago",
                editor.pid,
                editor.name.as_deref().unwrap_or("unnamed"),
                rimz::utils::time::unix_now_ms().saturating_sub(editor.since_ms) / 1000
            )),
        );
        details.render(&mut out)?;
        let mut buffers = render::KeyVals::new().indent(2);
        for document in editor.open {
            let path = url::Url::parse(&document.uri)
                .ok()
                .and_then(|uri| uri.to_file_path().ok());
            let path = path
                .as_deref()
                .map(|path| {
                    path.strip_prefix(&entry.root)
                        .unwrap_or(path)
                        .display()
                        .to_string()
                })
                .unwrap_or(document.uri);
            buffers.push(
                "buffer",
                render::cell(format!(
                    "{path}{}{}",
                    if document.owner { " (owner)" } else { "" },
                    if document.dirty {
                        " (unsaved in editor)"
                    } else {
                        ""
                    }
                )),
            );
        }
        buffers.render(&mut out)?;
    }
    Ok(())
}

fn list(json: bool) -> Result<()> {
    let entries = {
        let _lock = registry::lock()?;
        registry::sweep_locked()?
    };
    let mut out = render::out();
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&entries)?)?;
        return Ok(());
    }
    let mut table = render::Table::new([
        "CHECKOUT", "SERVER", "STATE", "RSS", "PEAK", "REQUESTS", "LAST", "RESTARTS", "LEASES",
    ]);
    for entry in entries {
        let running = matches!(
            entry.state,
            State::Starting | State::Indexing | State::Ready
        );
        let state = match &entry.state {
            State::Starting => "starting".into(),
            State::Indexing => "indexing".into(),
            State::Ready => "ready".into(),
            State::Dormant { reason, .. } => {
                reason.map_or_else(|| "dormant".into(), |reason| format!("dormant: {reason}"))
            }
            State::Stopped { reason, .. } => format!("stopped: {reason}"),
        };
        table.row(
            [
                entry.root.display().to_string(),
                entry.server,
                state,
                rimz::utils::size::decimal_bytes(
                    entry
                        .server_pid
                        .filter(|_| running)
                        .and_then(rimz::proc::tree_totals)
                        .map_or(0, |totals| totals.rss_kb.saturating_mul(1024)),
                ),
                rimz::utils::size::decimal_bytes(if running {
                    entry
                        .peak_rss_kb
                        .max(entry.server_pid.map_or(0, rimz::lsp::memory::tree_peak_kb))
                        .saturating_mul(1024)
                } else {
                    0
                }),
                entry.request_count.to_string(),
                entry.last_request_at_ms.map_or_else(
                    || "—".into(),
                    |at| {
                        format!(
                            "{}s ago",
                            rimz::utils::time::unix_now_ms().saturating_sub(at) / 1000
                        )
                    },
                ),
                entry.restarts.to_string(),
                entry.leases.len().to_string(),
            ]
            .map(render::cell),
        );
    }
    table.render(&mut out)?;
    Ok(())
}

fn stop(
    path: Option<PathBuf>,
    server: Option<String>,
    all: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let entries = {
        let _lock = registry::lock()?;
        registry::sweep_locked()?
    };
    let cwd = std::fs::canonicalize(
        path.or_else(|| globals.root.clone())
            .unwrap_or(std::env::current_dir()?),
    )?;
    let root = registry::enclosing_checkout(&cwd, &entries).unwrap_or(&cwd);
    let entries: Vec<_> = entries
        .iter()
        .filter(|entry| {
            all || (entry.root == root
                && server.as_ref().is_none_or(|server| server == &entry.server))
        })
        .collect();
    if !all && entries.len() > 1 {
        anyhow::bail!("multiple language servers; choose --server NAME");
    }
    let mut errors = Vec::new();
    for entry in entries {
        let response = registry::request(
            entry,
            &serde_json::json!({"op": "stop", "reason": rimz::lsp::registry::StopReason::StoppedByHand}),
            Duration::from_secs(2),
        );
        match response {
            Ok(response) if response["ok"] == true => {}
            result => {
                let error = result.err().map_or_else(
                    || "did not acknowledge stop".into(),
                    |error| error.to_string(),
                );
                errors.push(format!(
                    "{} ({}): {error}",
                    entry.server,
                    entry.root.display()
                ));
                continue;
            }
        }
        writeln!(
            render::out(),
            "stopped {} ({})",
            entry.server,
            entry.root.display()
        )?;
    }
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}
