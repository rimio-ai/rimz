//! Shared language-server navigation and process inspection.

use super::{GlobalFlags, render};
use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::lsp::{
    attach::{self, Outcome, Target},
    check,
    query::{self, QueryErr, Verb},
    registry,
};
use std::io::Write;
use std::num::NonZeroUsize;
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
    /// Find a definition by position, exact symbol name, or path::Symbol.
    Def(Query),
    /// Find references by position, exact symbol name, or path::Symbol.
    Refs(ReferencesQuery),
    /// Show type and documentation at a position, symbol, or path::Symbol.
    Hover(Query),
    /// Find implementations of a trait or interface, by name or path::Symbol.
    Impl(ListQuery),
    /// Find functions calling this symbol, by name or path::Symbol.
    Callers(ReferencesQuery),
    /// Find functions called by this symbol, by name or path::Symbol.
    Callees(ListQuery),
    /// Show a file's outline.
    Symbols(Query),
    /// Search workspace symbols.
    Find(ListQuery),
    /// Read numbered source for one or more file anchors or positions.
    Show {
        #[arg(value_name = "ANCHOR", required = true)]
        anchors: Vec<String>,
        /// Print the full body instead of an outline for large items.
        #[arg(long)]
        full: bool,
    },
    /// Check every path::symbol anchor in Markdown files against the outline.
    Check {
        #[arg(value_name = "FILE", required = true)]
        files: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Complete unique short paths or those one file's symbol singles out, and rewrite existing hints for uniquely resolved saved symbols.
        #[arg(long)]
        fix: bool,
        /// With --fix, insert a hint on every symbol anchor that names one item.
        #[arg(long, requires = "fix")]
        hints: bool,
    },
    /// List the current room's shared servers.
    List {
        /// List every shared server on this machine.
        #[arg(long)]
        all: bool,
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
    #[arg(value_name = "TARGET", required = true)]
    targets: Vec<String>,
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
    /// Maximum locations to show (default: 50).
    #[arg(long, conflicts_with_all = ["all", "json"])]
    limit: Option<NonZeroUsize>,
    /// Show every location in the selected scope.
    #[arg(long, conflicts_with_all = ["limit", "json"])]
    all: bool,
}

#[derive(Debug, Args)]
struct ReferencesQuery {
    #[command(flatten)]
    list: ListQuery,
    /// Hide references in test files and test modules.
    #[arg(long, conflicts_with = "json")]
    no_tests: bool,
}

pub fn run(args: LspArgs, globals: &GlobalFlags) -> Result<()> {
    let mut options = match &args.command {
        Command::Refs(ReferencesQuery { list: args, .. })
        | Command::Callers(ReferencesQuery { list: args, .. })
        | Command::Impl(args)
        | Command::Callees(args)
        | Command::Find(args) => {
            let scope = if args.external {
                query::Scope::External
            } else {
                query::Scope::Checkout
            };
            let mut options = query::ListOptions::from(scope);
            options.limit = if args.all {
                None
            } else {
                args.limit.or(options.limit)
            };
            options
        }
        _ => query::Scope::Checkout.into(),
    };
    if let Command::Refs(args) | Command::Callers(args) = &args.command {
        options.no_tests = args.no_tests;
    }
    let (verb, args) = match args.command {
        Command::Attach {
            server, version, ..
        } => return attach(server, version, globals),
        Command::Shim { server, dir } => return shim(&server, dir),
        Command::Serve { request } => {
            return Ok(rimz::lsp::broker::serve(serde_json::from_str(&request)?)?);
        }
        Command::List { all, json } => return list(all, json, globals),
        Command::Show { anchors, full } => {
            let context = query_context(globals)?;
            let blocks = match check::show(
                &anchors,
                full,
                &context.root,
                &context.entries,
                &context.servers,
            ) {
                Ok(blocks) => blocks,
                Err(error) => {
                    writeln!(render::err(), "{error}")?;
                    std::process::exit(2);
                }
            };
            let mut out = render::out();
            let mut code = 0;
            for (index, (text, exit)) in blocks.iter().enumerate() {
                if index > 0 {
                    writeln!(out)?;
                }
                write!(out, "{text}")?;
                code = query_exit(code, *exit);
            }
            out.flush()?;
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
        Command::Check {
            files,
            json,
            fix,
            hints,
        } => {
            let mode = match (fix, hints) {
                (false, _) => check::Mode::Check,
                (true, false) => check::Mode::Fix,
                (true, true) => check::Mode::FixHints,
            };
            let context = query_context(globals)?;
            let reports = match check::run(
                &files,
                &context.root,
                &context.entries,
                &context.servers,
                mode,
            ) {
                Ok(reports) => reports,
                Err(error) => return query_error(error),
            };
            let mut out = render::out();
            let multiple = files.len() > 1;
            let mut code = 0;
            let mut values = Vec::new();
            for (file, result) in files.iter().zip(reports) {
                match result {
                    Ok(report) => {
                        code = query_exit(code, report.exit_code());
                        if multiple && json {
                            let mut value = serde_json::to_value(&report)?;
                            value["exit"] = report.exit_code().into();
                            values.push(value);
                        } else {
                            write!(out, "{}", report.render(json)?)?;
                        }
                    }
                    Err(error) => {
                        if !multiple {
                            return query_error(error);
                        }
                        code = query_exit(code, error.exit_code());
                        if json {
                            values.push(serde_json::json!({
                                "notes": file.display().to_string(),
                                "exit": error.exit_code(),
                                "error": error.to_string(),
                            }));
                        } else {
                            writeln!(out, "{}  error  {error}", file.display())?;
                        }
                    }
                }
            }
            if multiple && json {
                writeln!(out, "{}", serde_json::to_string_pretty(&values)?)?;
            }
            out.flush()?;
            if code != 0 {
                std::process::exit(code);
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
        Command::Refs(args) => (Verb::Refs, args.list.query),
        Command::Hover(args) => (Verb::Hover, args),
        Command::Impl(args) => (Verb::Impl, args.query),
        Command::Callers(args) => (Verb::Callers, args.list.query),
        Command::Callees(args) => (Verb::Callees, args.query),
        Command::Symbols(args) => (Verb::Symbols, args),
        Command::Find(args) => (Verb::Find, args.query),
    };
    let context = query_context(globals)?;
    let root = &context.root;
    let multiple = args.targets.len() > 1;
    let mut failures = QueryFailures::default();
    let mut code = 0;
    let mut single_error = None;
    let mut values = Vec::new();
    let mut out = render::out();
    for (index, target) in args.targets.iter().enumerate() {
        let result = (|| {
            let target = query::Target::parse(verb, target)?;
            let entry = query::select(
                root,
                context.entries.clone(),
                &context.servers,
                args.server.as_deref(),
                target.path(),
            )?;
            let output =
                failures.execute(&entry.server, || query::execute(&entry, verb, &target))?;
            Ok::<_, QueryErr>((entry, output))
        })();
        let text = match result {
            Err(error) => {
                if !multiple {
                    single_error = Some(error);
                    break;
                }
                code = query_exit(code, error.exit_code());
                if args.json {
                    values.push(serde_json::json!({
                        "target": target, "outcome": "error", "exit": error.exit_code(),
                        "error": error.to_string(),
                    }));
                    continue;
                }
                format!("error: {error}\n")
            }
            Ok((entry, output)) => {
                code = query_exit(code, output.exit_code());
                if multiple && args.json {
                    let mut value = query::outcome_json(root, &output)?;
                    value["target"] = target.clone().into();
                    value["exit"] = output.exit_code().into();
                    values.push(value);
                    continue;
                }
                let dirty = query::dirty_documents(&entry);
                match output {
                    query::Output::Answer { result, .. } if args.json => {
                        format!("{}\n", serde_json::to_string_pretty(&result)?)
                    }
                    query::Output::Answer {
                        result,
                        document_uri,
                    } => {
                        query::render(
                            verb,
                            root,
                            document_uri.as_deref(),
                            result,
                            options,
                            &dirty,
                            |path| {
                                let output = query::execute(
                                    &entry,
                                    Verb::Symbols,
                                    &query::Target::File(path.to_owned()),
                                )
                                .map_err(|error| match error {
                                    QueryErr::Failed(error) => error,
                                    error => rimz::lsp::LspErr::Protocol(error.to_string()),
                                })?;
                                // File-symbol queries return answers directly, without name resolution.
                                let query::Output::Answer { result, .. } = output else {
                                    unreachable!("file-symbol queries do not resolve names")
                                };
                                Ok(result)
                            },
                        )?
                    }
                    output => query::render_outcome(root, &output, args.json)?,
                }
            }
        };
        if multiple {
            render::finish(writeln!(
                out,
                "{}==> {target} <==",
                if index == 0 { "" } else { "\n" }
            ))?;
        }
        render::finish(out.write_all(text.as_bytes()).and_then(|()| out.flush()))?;
    }
    if let Some(error) = single_error {
        return query_error(error);
    }
    if multiple && args.json {
        let text = format!("{}\n", serde_json::to_string_pretty(&values)?);
        render::finish(out.write_all(text.as_bytes()).and_then(|()| out.flush()))?;
    }
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

fn query_exit(current: i32, next: i32) -> i32 {
    [3, 4, 1, 7, 6, 5, 0]
        .into_iter()
        .find(|code| *code == current || *code == next)
        .unwrap_or(0)
}

#[derive(Default)]
struct QueryFailures(std::collections::BTreeMap<String, QueryErr>);

impl QueryFailures {
    fn execute(
        &mut self,
        server: &str,
        request: impl FnOnce() -> std::result::Result<query::Output, QueryErr>,
    ) -> std::result::Result<query::Output, QueryErr> {
        if let Some(error) = self.0.get(server).and_then(Self::server_error) {
            return Err(error);
        }
        let result = request();
        if let Err(error) = &result
            && let Some(error) = Self::server_error(error)
        {
            self.0.insert(server.to_owned(), error);
        }
        result
    }

    fn server_error(error: &QueryErr) -> Option<QueryErr> {
        match error {
            QueryErr::Unavailable { root, reason } => Some(QueryErr::Unavailable {
                root: root.clone(),
                reason: reason.clone(),
            }),
            QueryErr::Indexing { server, seconds } => Some(QueryErr::Indexing {
                server: server.clone(),
                seconds: *seconds,
            }),
            QueryErr::Failed(_) => None,
        }
    }
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
    let (root, entries) = checkout_entries(path, server, false, globals)?;
    let Some(entry) = entries.first() else {
        return query_error(QueryErr::Unavailable {
            root,
            reason: query::UnavailableReason::NotRunning,
        });
    };
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
    if let Some(enabled) = entry.editor_check_on_save {
        facts.push(
            "check on save",
            render::cell(if enabled {
                "on (editor attached)"
            } else {
                "off (no editor attached)"
            }),
        );
    }
    facts.push(
        "kind",
        render::cell(
            entry
                .kind
                .map_or_else(|| "unknown".into(), |kind| kind.to_string()),
        ),
    );
    facts.push("state", render::cell(render::lsp_state_label(&entry.state)));
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
    if let Some(cause) = &entry.last_crash {
        let age =
            render::age_label(rimz::utils::time::unix_now_ms().saturating_sub(cause.at_ms) / 1000);
        facts.push(
            "last crash",
            render::cell(format!("{}, {age} ago", cause.exit_summary())),
        );
        let lines: Vec<_> = cause.stderr_tail.lines().collect();
        if !lines.is_empty() {
            facts.push(
                "stderr",
                render::cell(lines[lines.len().saturating_sub(5)..].join("\n")),
            );
        }
    }
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
        let running = entry.state.is_running();
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

/// Keeps the entries registered from the room's repository: attach records
/// the launch repo root, which is the room root unless a pinned room's cwd
/// names another repository.
fn room_entries(
    mut entries: Vec<registry::Entry>,
    workspace: &rimz::workspace::ResolvedWorkspace,
) -> Vec<registry::Entry> {
    entries.retain(|entry| {
        entry.project.as_deref().is_some_and(|project| {
            project == workspace.project_root || project == workspace.launch_repo_root()
        })
    });
    entries
}

fn list(all: bool, json: bool, globals: &GlobalFlags) -> Result<()> {
    let mut entries = registry::sweep()?;
    if !all {
        let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
        let workspace =
            rimz::workspace::WorkspaceResolver::resolve_participant(&cwd, globals.root.clone())?;
        entries = room_entries(entries, &workspace);
    }
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

fn checkout_entries(
    path: Option<PathBuf>,
    server: Option<String>,
    all: bool,
    globals: &GlobalFlags,
) -> Result<(PathBuf, Vec<registry::Entry>)> {
    let mut entries = registry::sweep()?;
    let cwd = std::fs::canonicalize(
        path.or_else(|| globals.root.clone())
            .unwrap_or(std::env::current_dir()?),
    )?;
    let root = registry::enclosing_checkout(&cwd, &entries)
        .unwrap_or(&cwd)
        .to_owned();
    entries.retain(|entry| {
        all || (entry.root == root && server.as_ref().is_none_or(|server| server == &entry.server))
    });
    if !all && entries.len() > 1 {
        anyhow::bail!("multiple language servers; choose --server NAME");
    }
    Ok((root, entries))
}

fn stop(
    path: Option<PathBuf>,
    server: Option<String>,
    all: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let (_, entries) = checkout_entries(path, server, all, globals)?;
    let mut errors = Vec::new();
    let mut stopped = String::new();
    for entry in &entries {
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
    use registry::{State, StopReason};
    use render::status::{self, StateRole};

    #[test]
    fn no_tests_is_refs_and_callers_only_and_conflicts_with_json() {
        use clap::Parser;
        for verb in ["refs", "callers"] {
            assert!(
                crate::cli::Cli::try_parse_from([
                    "rimz",
                    "lsp",
                    verb,
                    "work",
                    "--no-tests",
                    "--all"
                ])
                .is_ok()
            );
            let error = crate::cli::Cli::try_parse_from([
                "rimz",
                "lsp",
                verb,
                "work",
                "--no-tests",
                "--json",
            ])
            .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
        for verb in ["impl", "callees", "find"] {
            let error =
                crate::cli::Cli::try_parse_from(["rimz", "lsp", verb, "work", "--no-tests"])
                    .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn query_exit_uses_severity_not_argument_order() {
        let codes = [0, 5, 6, 7, 1, 4, 3];
        for (rank, code) in codes.iter().enumerate() {
            for other in &codes[..=rank] {
                assert_eq!(query_exit(*code, *other), *code);
                assert_eq!(query_exit(*other, *code), *code);
            }
        }
    }

    #[test]
    fn query_failures_reuse_server_conditions_only() {
        for code in [1, 3, 4] {
            let mut failures = QueryFailures::default();
            let mut requests = 0;
            for _ in 0..3 {
                let result = failures.execute("rust", || {
                    requests += 1;
                    Err(match code {
                        3 => QueryErr::Unavailable {
                            root: "/repo".into(),
                            reason: query::UnavailableReason::NotRunning,
                        },
                        4 => QueryErr::Indexing {
                            server: "rust".into(),
                            seconds: 30,
                        },
                        _ => rimz::lsp::LspErr::Protocol("bad request".into()).into(),
                    })
                });
                assert_eq!(result.err().unwrap().exit_code(), code);
            }
            assert_eq!(requests, if code == 1 { 3 } else { 1 });
            assert!(
                failures
                    .execute("python", || Ok(query::Output::Answer {
                        result: serde_json::Value::Null,
                        document_uri: None
                    }))
                    .is_ok()
            );
        }
    }

    fn entry(root: &str, server: &str, state: State) -> registry::Entry {
        registry::Entry {
            kind: None,
            editor_check_on_save: None,
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
            last_crash: None,
            leases: vec![],
            attached: vec![],
        }
    }

    #[test]
    fn room_entries_keep_the_room_repository() {
        let registered = |root: &str, project: Option<&str>| registry::Entry {
            project: project.map(Into::into),
            ..entry(root, "rust", State::Ready)
        };
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/room")),
            project_root: "/room".into(),
            cwd_project_root: Some("/other".into()),
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: "/other".into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let kept = room_entries(
            vec![
                registered("/room", Some("/room")),
                registered("/worktrees/feat", Some("/room")),
                registered("/other", Some("/other")),
                registered("/elsewhere", Some("/elsewhere")),
                registered("/room", None),
            ],
            &workspace,
        );
        let roots: Vec<_> = kept.iter().map(|entry| entry.root.as_path()).collect();
        assert_eq!(
            roots,
            ["/room", "/worktrees/feat", "/other"].map(std::path::Path::new)
        );
    }

    #[test]
    fn list_table_orders_and_styles_server_states() {
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
