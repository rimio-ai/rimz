//! Shared language-server navigation and process inspection.

use super::{GlobalFlags, render};
use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::lsp::{
    attach::{self, Outcome, Target},
    check,
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
    /// Connect an editor over stdio to the checkout's shared server.
    Attach {
        #[arg(long)]
        server: Option<String>,
        /// Use the stdio transport (the default).
        #[arg(long)]
        stdio: bool,
        #[arg(long)]
        version: bool,
    },
    /// Install an executable editor bridge for a configured server.
    Shim {
        #[arg(long)]
        server: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Find a definition by position or exact symbol name.
    Def(Query),
    /// Find references by position or exact symbol name.
    Refs(ListQuery),
    /// Show type and documentation at a position or symbol.
    Hover(Query),
    /// Find implementations of a trait or interface.
    Impl(ListQuery),
    /// Find functions calling this symbol.
    Callers(ListQuery),
    /// Find functions called by this symbol.
    Callees(ListQuery),
    /// Show a file's outline.
    Symbols(Query),
    /// Search workspace symbols.
    Find(ListQuery),
    /// Check every path::symbol anchor in a Markdown file against the outline.
    Check {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
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
struct ListQuery {
    #[command(flatten)]
    query: Query,
    /// Include results outside the checkout.
    #[arg(long)]
    external: bool,
}

pub fn run(args: LspArgs, globals: &GlobalFlags) -> Result<()> {
    let scope = match &args.command {
        Command::Refs(args)
        | Command::Impl(args)
        | Command::Callers(args)
        | Command::Callees(args)
        | Command::Find(args)
            if args.external =>
        {
            query::Scope::External
        }
        _ => query::Scope::Checkout,
    };
    let (verb, args) = match args.command {
        Command::Attach {
            server, version, ..
        } => return attach(server, version, globals),
        Command::Shim { server, dir } => return shim(&server, dir),
        Command::Serve { request } => {
            return Ok(rimz::lsp::broker::serve(serde_json::from_str(&request)?)?);
        }
        Command::List { json } => return list(json),
        Command::Check { file, json } => {
            let context = query_context(globals)?;
            let report = match check::run(&file, &context.root, &context.entries, &context.servers)
            {
                Ok(report) => report,
                Err(error) => return query_error(error),
            };
            let mut out = render::out();
            write!(out, "{}", report.render(json)?)?;
            out.flush()?;
            if report.exit_code() != 0 {
                std::process::exit(report.exit_code());
            }
            return Ok(());
        }
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
        Command::Refs(args) => (Verb::Refs, args.query),
        Command::Hover(args) => (Verb::Hover, args),
        Command::Impl(args) => (Verb::Impl, args.query),
        Command::Callers(args) => (Verb::Callers, args.query),
        Command::Callees(args) => (Verb::Callees, args.query),
        Command::Symbols(args) => (Verb::Symbols, args),
        Command::Find(args) => (Verb::Find, args.query),
    };
    let context = query_context(globals)?;
    let root = &context.root;
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
        context.entries,
        &context.servers,
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
    let code = output.exit_code();
    let text = match output {
        query::Output::Answer { result, .. } if args.json => {
            format!("{}\n", serde_json::to_string_pretty(&result)?)
        }
        query::Output::Answer {
            result,
            document_uri,
        } => query::render(verb, root, document_uri.as_deref(), result, scope, &dirty)?,
        output => query::render_outcome(root, &output, args.json)?,
    };
    let mut out = render::out();
    render::finish(out.write_all(text.as_bytes()).and_then(|()| out.flush()))?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

struct QueryContext {
    root: PathBuf,
    entries: Vec<registry::Entry>,
    servers: std::collections::BTreeMap<String, rimz::config::LspServerConfig>,
}

fn query_context(globals: &GlobalFlags) -> Result<QueryContext> {
    let entries = registry::read_entries()?;
    let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
    let workspace = rimz::workspace::WorkspaceResolver::resolve(&cwd, globals.root.clone())?;
    let root = registry::enclosing_checkout(&cwd, &entries)
        .unwrap_or(&workspace.worktree_root)
        .to_owned();
    let machine = rimz::config::MachineConfig::load()?;
    let config = rimz::config::effective::load(&machine, workspace.launch_repo_root())?;
    Ok(QueryContext {
        root,
        entries,
        servers: config.lsp_servers,
    })
}

fn attach(server: Option<String>, version: bool, globals: &GlobalFlags) -> Result<()> {
    if version {
        writeln!(
            std::io::stdout().lock(),
            "rimz lsp attach {}",
            rimz::build_id::VERSION
        )?;
        return Ok(());
    }
    let mut input = std::io::BufReader::new(std::io::stdin());
    let first = rimz::lsp::protocol::read_frame(&mut input)?;
    let target = Target::resolve(globals.root.as_deref(), &first, server.as_deref())?;
    let mut queue = rimz::lsp::admission::WaitQueue::default();
    let outcome = loop {
        let admitted = match target.admit(&mut queue) {
            Ok(admitted) => admitted,
            Err(error @ rimz::lsp::LspErr::QueueTimeout(_)) => {
                drop(queue);
                writeln!(render::err(), "{error}")?;
                std::process::exit(3)
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(message) = admitted.startup_refused.into_iter().next() {
            drop(queue);
            writeln!(render::err(), "{message}")?;
            std::process::exit(3)
        }
        if let Some(entry) = admitted.admitted.first() {
            break attach::bridge(entry, input, std::io::stdout(), &first)?;
        }
        if admitted.wait_for_required.is_empty() {
            anyhow::bail!("{target}: no matching root-markers");
        }
        for wait in admitted.wait_for_required {
            super::lsp_admission::write_wait(&wait)?;
        }
        std::thread::sleep(Duration::from_secs(5));
    };
    drop(queue);
    match outcome {
        Outcome::EditorClosed => Ok(()),
        Outcome::EditorFailed(reason) => anyhow::bail!("editor stream failed: {reason}"),
        Outcome::BrokerClosed(reason) | Outcome::Refused(reason) => {
            writeln!(render::err(), "{target} is {reason}")?;
            std::process::exit(3)
        }
    }
}

fn shim(server: &str, dir: Option<PathBuf>) -> Result<()> {
    let dir = match dir {
        Some(dir) => dir,
        None => PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| anyhow::anyhow!("HOME is not set; choose --dir DIR"))?,
        )
        .join(".local/bin"),
    };
    let path = attach::install_shim(server, &dir)?;
    writeln!(render::out(), "{}", path.display())?;
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

fn list_table(
    entries: &[registry::Entry],
    now_ms: u64,
    memory: impl Fn(u32) -> (u64, u64),
) -> render::Table {
    use render::status;
    let mut entries: Vec<_> = entries.iter().collect();
    entries.sort_by_key(|entry| status::lsp_order(entry));
    let mut table = render::Table::new([
        "STATE", "CHECKOUT", "SERVER", "RSS", "PEAK", "REQUESTS", "LAST", "RESTARTS", "LEASES",
    ])
    .right(&[3, 4, 5, 6, 7, 8]);
    for entry in entries {
        let running = matches!(
            entry.state,
            State::Starting | State::Indexing | State::Ready
        );
        let (glyph, style) = render::verdict(status::lsp(&entry.state));
        let (rss, peak) = if running {
            let (rss_kb, peak_kb) = entry.server_pid.map_or((0, 0), &memory);
            (
                render::cell(rimz::utils::size::decimal_bytes(
                    rss_kb.saturating_mul(1024),
                )),
                render::cell(rimz::utils::size::decimal_bytes(
                    entry.peak_rss_kb.max(peak_kb).saturating_mul(1024),
                )),
            )
        } else {
            (render::cell("-").dash(), render::cell("-").dash())
        };
        table.row([
            render::cell(format!("{glyph} {}", render::lsp_state_label(&entry.state))).fg(style),
            render::cell(render::home_relative_path(&entry.root)),
            render::cell(&entry.server),
            rss,
            peak,
            render::cell(entry.request_count.to_string()),
            entry.last_request_at_ms.map_or_else(
                || render::cell("-").dash(),
                |at| {
                    render::cell(format!(
                        "{} ago",
                        render::age_label(now_ms.saturating_sub(at) / 1000)
                    ))
                },
            ),
            render::cell(entry.restarts.to_string()),
            render::cell(entry.leases.len().to_string()),
        ]);
    }
    table
}

fn list(json: bool) -> Result<()> {
    let entries = {
        let _lock = registry::lock()?;
        registry::sweep_locked()?
    };
    let mut out = render::out();
    if json {
        return render::finish(writeln!(out, "{}", serde_json::to_string_pretty(&entries)?));
    }
    render::finish(
        list_table(&entries, rimz::utils::time::unix_now_ms(), |pid| {
            (
                rimz::proc::tree_totals(pid).map_or(0, |totals| totals.rss_kb),
                rimz::lsp::memory::tree_peak_kb(pid),
            )
        })
        .render(&mut out),
    )
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
    let mut stopped = String::new();
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
        stopped += &format!("stopped {} ({})\n", entry.server, entry.root.display());
    }
    let written = render::out().write_all(stopped.as_bytes());
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    render::finish(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry::StopReason;
    use render::status::{self, StateRole};

    #[test]
    fn list_table_orders_and_styles_server_states() {
        let entry = |root: &str, server: &str, state| registry::Entry {
            root: root.into(),
            project: None,
            server: server.into(),
            nonce: String::new(),
            broker_pid: 1,
            broker_start_token: String::new(),
            server_pid: Some(2),
            server_start_token: None,
            state,
            started_at_ms: 0,
            ready_at_ms: None,
            estimate_bytes: 0,
            settings_hash: String::new(),
            request_count: 3,
            last_request_at_ms: None,
            peak_rss_kb: 4,
            restarts: 2,
            leases: vec![],
            attached: vec![],
        };
        let mut ready = entry("/z", "rust", State::Ready);
        ready.last_request_at_ms = Some(11_000);
        let entries = vec![
            entry(
                "/b",
                "rust",
                State::Dormant {
                    since_ms: 0,
                    reason: None,
                },
            ),
            entry(
                "/a",
                "z",
                State::Dormant {
                    since_ms: 0,
                    reason: Some(StopReason::Idle),
                },
            ),
            entry(
                "/c",
                "rust",
                State::Dormant {
                    since_ms: 0,
                    reason: Some(StopReason::Crashed),
                },
            ),
            ready,
            entry(
                "/a",
                "a",
                State::Dormant {
                    since_ms: 0,
                    reason: None,
                },
            ),
        ];
        let table = list_table(&entries, 56_000, |pid| {
            assert_eq!(pid, 2);
            (2, 8)
        });
        let mut stripped = anstream::StripStream::new(Vec::new());
        table.render(&mut stripped).unwrap();
        let text = String::from_utf8(stripped.into_inner()).unwrap();
        let rows: Vec<Vec<&str>> = text
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        assert_eq!(
            rows,
            vec![
                vec![
                    "STATE", "CHECKOUT", "SERVER", "RSS", "PEAK", "REQUESTS", "LAST", "RESTARTS",
                    "LEASES"
                ],
                vec![
                    "✓", "ready", "/z", "rust", "2", "KB", "8.2", "KB", "3", "45s", "ago", "2", "0"
                ],
                vec![
                    "✗", "dormant:", "crashed", "/c", "rust", "-", "-", "3", "-", "2", "0"
                ],
                vec![
                    "·", "not", "started", "/a", "a", "-", "-", "3", "-", "2", "0"
                ],
                vec![
                    "·", "dormant:", "idle", "/a", "z", "-", "-", "3", "-", "2", "0"
                ],
                vec![
                    "·", "not", "started", "/b", "rust", "-", "-", "3", "-", "2", "0"
                ],
            ]
        );
        let mut raw = Vec::new();
        table.render(&mut raw).unwrap();
        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.contains(&render::paint(status::role(StateRole::Success), "✓ ready")));
        assert!(raw.contains(&render::paint(
            status::role(StateRole::Failed),
            "✗ dormant: crashed"
        )));
    }
}
