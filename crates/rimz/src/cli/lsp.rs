//! Shared language-server navigation and process inspection.

use super::{GlobalFlags, render};
use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::lsp::{
    query::{self, QueryErr, Verb},
    registry::{self, State},
};
use std::io::Write;
use std::path::{Path, PathBuf};
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
    Callers(Query),
    /// Find functions called by this symbol.
    Callees(Query),
    /// Show a file's outline.
    Symbols(Query),
    /// Search workspace symbols.
    Find(Query),
    /// List shared servers on this machine.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Stop a shared server without restarting it.
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

pub fn run(args: LspArgs, globals: &GlobalFlags) -> Result<()> {
    let (verb, args) = match args.command {
        Command::Serve { request } => {
            return Ok(rimz::lsp::broker::serve(serde_json::from_str(&request)?)?);
        }
        Command::List { json } => return list(json),
        Command::Stop {
            checkout,
            server,
            all,
        } => return stop(checkout, server, all, globals),
        Command::Def(args) => (Verb::Def, args),
        Command::Refs(args) => (Verb::Refs, args),
        Command::Hover(args) => (Verb::Hover, args),
        Command::Impl(args) => (Verb::Impl, args),
        Command::Callers(args) => (Verb::Callers, args),
        Command::Callees(args) => (Verb::Callees, args),
        Command::Symbols(args) => (Verb::Symbols, args),
        Command::Find(args) => (Verb::Find, args),
    };
    let entries = registry::read_entries()?;
    let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
    let workspace = rimz::workspace::WorkspaceResolver::resolve(&cwd, globals.root.clone())?;
    let root = checkout(&cwd, &entries).unwrap_or(&workspace.worktree_root);
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
    let result = query::select(
        root,
        entries.clone(),
        &config.lsp_servers,
        args.server.as_deref(),
        path.as_deref(),
    )
    .and_then(|entry| query::execute(&entry, verb, &args.target));
    let output = match result {
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
                    query::render(verb, root, document_uri.as_deref(), result)?
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

fn checkout<'a>(cwd: &Path, entries: &'a [registry::Entry]) -> Option<&'a Path> {
    entries
        .iter()
        .filter(|entry| cwd.starts_with(&entry.root))
        .max_by_key(|entry| entry.root.components().count())
        .map(|entry| entry.root.as_path())
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
        "CHECKOUT", "SERVER", "STATE", "RSS", "PEAK", "REQUESTS", "LAST", "LEASES",
    ]);
    for entry in entries {
        let state = match &entry.state {
            State::Starting => "starting".into(),
            State::Indexing => "indexing".into(),
            State::Ready => "ready".into(),
            State::Stopped { reason, .. } => format!("stopped: {reason}"),
        };
        table.row(
            [
                entry.root.display().to_string(),
                entry.server,
                state,
                format!(
                    "{} KiB",
                    entry
                        .server_pid
                        .filter(|_| !matches!(entry.state, State::Stopped { .. }))
                        .and_then(rimz::proc::tree_totals)
                        .map_or(0, |totals| totals.rss_kb)
                ),
                format!("{} KiB", entry.peak_rss_kb),
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
    let entries = registry::read_entries()?;
    let cwd = std::fs::canonicalize(
        path.or_else(|| globals.root.clone())
            .unwrap_or(std::env::current_dir()?),
    )?;
    let root = checkout(&cwd, &entries).unwrap_or(&cwd);
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
    for entry in entries {
        let response = registry::request(
            entry,
            &serde_json::json!({"op": "stop", "reason": "stopped by hand"}),
            Duration::from_secs(2),
        )?;
        if response["ok"] != true {
            anyhow::bail!("{} did not acknowledge stop", entry.server);
        }
        writeln!(
            render::out(),
            "stopped {} ({})",
            entry.server,
            entry.root.display()
        )?;
    }
    Ok(())
}
