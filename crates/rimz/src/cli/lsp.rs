//! Shared language-server navigation and process inspection.

use super::{GlobalFlags, render};
use anyhow::Result;
use clap::{Args, Subcommand};
use rimz::lsp::{
    admission::Headroom,
    attach::{self, Outcome, Target},
    check,
    query::{self, QueryErr, Verb},
    registry, usage,
};
use std::collections::BTreeMap;
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
    #[command(mut_arg("targets", |arg| arg.help("File paths to outline")))]
    Symbols(Query),
    /// Search workspace symbols.
    #[command(mut_arg("targets", |arg| arg.help("Symbol-name search strings")))]
    Find(ListQuery),
    /// Read numbered source for one or more file anchors or positions.
    Show {
        /// Source targets: path::Symbol, path:line[:col], or path:start-end.
        #[arg(value_name = "ANCHOR", required = true)]
        anchors: Vec<String>,
        /// Print the full body instead of an outline for large items.
        #[arg(long)]
        full: bool,
    },
    /// Check every path::symbol anchor in Markdown files against the outline.
    Check {
        /// Markdown files containing code anchors to verify.
        #[arg(value_name = "FILE", required = true)]
        files: Vec<PathBuf>,
        /// Print structured anchor verdicts as JSON.
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
        /// Also list servers that hold no memory.
        #[arg(long)]
        all: bool,
        /// Print server registry entries as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect a shared server and its attached editors.
    Status {
        /// Checkout to inspect (default: the current checkout).
        checkout: Option<PathBuf>,
        /// Select a server by name when several match.
        #[arg(long)]
        server: Option<String>,
        /// Print the server registry entry as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Free a shared server's memory; the next query restarts it.
    Stop {
        /// Checkout whose server to stop (default: the current checkout).
        checkout: Option<PathBuf>,
        /// Select a server by name when several match.
        #[arg(long, conflicts_with = "all")]
        server: Option<String>,
        /// Stop every server in this room.
        #[arg(long, conflicts_with = "checkout")]
        all: bool,
    },
    /// Connect an editor over stdio to the checkout's shared server.
    Attach {
        /// Select a configured server by name.
        #[arg(long)]
        server: Option<String>,
        /// Use the stdio transport (the default).
        #[arg(long)]
        stdio: bool,
        /// Print the editor bridge version and exit.
        #[arg(long)]
        version: bool,
    },
    /// Install an executable editor bridge for a configured server.
    Shim {
        /// Configured server for the editor bridge.
        #[arg(long)]
        server: String,
        /// Installation directory (default: ~/.local/bin).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        request: String,
    },
}

#[derive(Debug, Args)]
struct Query {
    /// Targets: path:line:col (1-based, column required), path::Symbol, or a name.
    #[arg(value_name = "TARGET", required = true)]
    targets: Vec<String>,
    /// Select a configured or registered server by name.
    #[arg(long)]
    server: Option<String>,
    /// Print the complete query result as JSON.
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
                context.usage.as_ref(),
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
                context.usage.as_ref(),
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
    context.validate_server(args.server.as_deref())?;
    let kind: String = serde_json::from_value(serde_json::to_value(verb)?)?;
    let root = &context.root;
    let multiple = args.targets.len() > 1;
    let mut failures = QueryFailures::default();
    let mut code = 0;
    let mut single_error = None;
    let mut values = Vec::new();
    let mut out = render::out();
    for (index, target) in args.targets.iter().enumerate() {
        let result = (|| {
            let target = match query::Target::parse(verb, target) {
                Ok(target) => target,
                Err(rimz::lsp::LspErr::MissingColumn { path, line, .. }) => {
                    let outline = query::select(
                        root,
                        context.entries.clone(),
                        &context.servers,
                        args.server.as_deref(),
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
                args.server.as_deref(),
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
                                error => rimz::lsp::LspErr::Protocol(error.to_string()),
                            })
                        },
                    )?,
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
    usage: Option<usage::Log>,
}

impl QueryContext {
    fn validate_server(&self, server: Option<&str>) -> Result<()> {
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

fn validate_server_name<'a>(
    server: &str,
    configured: impl Iterator<Item = &'a str>,
    registered: impl Iterator<Item = &'a registry::Entry>,
) -> Result<()> {
    let names: std::collections::BTreeSet<_> = configured
        .chain(registered.map(|entry| entry.server.as_str()))
        .collect();
    if names.contains(server) {
        return Ok(());
    }
    let configured = if names.is_empty() {
        "none configured".to_owned()
    } else {
        names.into_iter().collect::<Vec<_>>().join(", ")
    };
    Err(super::usage::UsageError::new(format!(
        "no language server named {server}; configured: {configured}"
    ))
    .into())
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

fn query_context(globals: &GlobalFlags) -> Result<QueryContext> {
    let mut entries = registry::read_entries()?;
    let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
    let workspace = rimz::workspace::WorkspaceResolver::resolve(&cwd, globals.root.clone())?;
    let root = registry::enclosing_checkout(&cwd, &entries)
        .unwrap_or(&workspace.worktree_root)
        .to_owned();
    let machine = rimz::config::MachineConfig::load()?;
    let config = rimz::config::effective::load(&machine, workspace.launch_repo_root())?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    if rimz::lsp::admission::revive(&rimz::lsp::admission::AdmissionRequest {
        root: &root,
        project: workspace.launch_repo_root(),
        servers: &config.lsp_servers,
        untrusted_servers: &[],
        policy: &machine.lsp,
        runtime: &runtime,
    })? {
        entries = registry::read_entries()?;
    }
    Ok(QueryContext {
        root,
        entries,
        servers: config.lsp_servers,
        // The participant ladder departs from `resolve` only to follow a verified pane pin.
        usage: room_usage(globals, || Ok(workspace.project_root.clone())),
    })
}

/// The usage log of the participant room, the one `list` reads. A verified pane pin names it
/// without spawning git unless `--root` overrides the pin; otherwise `resolve` names it.
fn room_usage(
    globals: &GlobalFlags,
    resolve: impl FnOnce() -> Result<PathBuf>,
) -> Option<usage::Log> {
    let room = match &globals.root {
        None => super::pinned_room_root().map_or_else(resolve, Ok),
        Some(_) => resolve(),
    };
    match room.and_then(|room| Ok(usage::Log::new(&rimz::StatePaths::for_project_root(&room)?))) {
        Ok(log) => Some(log),
        Err(error) => {
            tracing::debug!(%error, "cannot resolve language-server usage room");
            None
        }
    }
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

struct ListView<'a> {
    entries: &'a [registry::Entry],
    others: &'a [registry::Entry],
    all: bool,
    now_ms: u64,
    headroom: Option<Headroom>,
}

impl ListView<'_> {
    fn render(
        &self,
        out: &mut impl Write,
        memory: impl Fn(&registry::Entry) -> rimz::lsp::memory::Reading,
        needs: impl Fn(&registry::Entry) -> u64,
    ) -> std::io::Result<()> {
        let readings: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                if entry.state.is_running() {
                    memory(entry)
                } else {
                    rimz::lsp::memory::Reading::default()
                }
            })
            .collect();
        let running = self
            .entries
            .iter()
            .filter(|entry| entry.state.is_running())
            .count();
        write!(out, "{running} running")?;
        if running > 0 {
            let total = readings.iter().fold(0u64, |total, reading| {
                total.saturating_add(tree_bytes(reading))
            });
            write!(out, ": {}", rimz::utils::size::decimal_bytes(total))?;
        }
        if let Some(headroom) = self.headroom {
            write!(out, "; {headroom}")?;
        }
        let mut estimates = BTreeMap::<&str, u64>::new();
        for entry in self.entries {
            let estimate = needs(entry);
            estimates
                .entry(&entry.server)
                .and_modify(|largest| *largest = (*largest).max(estimate))
                .or_insert(estimate);
        }
        if !estimates.is_empty() {
            let starts: Vec<_> = estimates
                .into_iter()
                .map(|(server, estimate)| {
                    format!("{} ({server})", rimz::utils::size::decimal_bytes(estimate))
                })
                .collect();
            write!(out, "; a start needs {}", starts.join(", "))?;
        }
        writeln!(out)?;

        let mut others = BTreeMap::<&std::path::Path, (u64, usize)>::new();
        for entry in self.others.iter().filter(|entry| entry.state.is_running()) {
            let reading = memory(entry);
            let (total, count) = others
                .entry(entry.project.as_deref().unwrap_or(&entry.root))
                .or_default();
            *total = total.saturating_add(tree_bytes(&reading));
            *count += 1;
        }
        let mut others: Vec<_> = others.into_iter().collect();
        others.sort_by_key(|(project, (total, _))| (std::cmp::Reverse(*total), *project));
        for (project, (total, count)) in others {
            let project = render::home_relative_path(project);
            let quoted = match project.strip_prefix("~/") {
                Some(relative) => {
                    format!(
                        "~/{}",
                        shlex::try_quote(relative).map_err(std::io::Error::other)?
                    )
                }
                None if project == "~" => project.clone(),
                None => shlex::try_quote(&project)
                    .map_err(std::io::Error::other)?
                    .into_owned(),
            };
            writeln!(
                out,
                "other room: {} in {project} ({count} running); rimz --root {quoted} lsp list",
                rimz::utils::size::decimal_bytes(total)
            )?;
        }
        if self.entries.is_empty() {
            return writeln!(out, "no language server registered in this room");
        }
        let visible: Vec<_> = self
            .entries
            .iter()
            .zip(&readings)
            .filter(|(entry, _)| self.all || entry.state.is_running() || entry.state.is_alarm())
            .collect();
        if !visible.is_empty() {
            list_table(visible.into_iter(), self.now_ms).render(out)?;
        }
        if self.all {
            return Ok(());
        }
        let mut hidden = BTreeMap::<String, usize>::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| !entry.state.is_running() && !entry.state.is_alarm())
        {
            let reason = match &entry.state {
                registry::State::Dormant { reason, .. } => {
                    reason.map_or_else(|| "not started".into(), |reason| reason.to_string())
                }
                registry::State::Stopped { reason, .. } => reason.to_string(),
                _ => continue,
            };
            *hidden.entry(reason).or_default() += 1;
        }
        if !hidden.is_empty() {
            let count: usize = hidden.values().sum();
            let mut reasons: Vec<_> = hidden.into_iter().collect();
            reasons.sort_by_key(|(reason, count)| (std::cmp::Reverse(*count), reason.clone()));
            let reasons: Vec<_> = reasons
                .into_iter()
                .map(|(reason, count)| format!("{count} {reason}"))
                .collect();
            writeln!(
                out,
                "{count} not running: {}; add --all to list them",
                reasons.join(", ")
            )?;
        }
        Ok(())
    }
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
    Ok(write_status(
        &mut out,
        &entry,
        rimz::utils::time::unix_now_ms(),
        rimz::lsp::memory::for_entry(&entry),
        host_headroom(),
        rimz::lsp::history::Needs::read().for_entry(&entry),
    )?)
}

/// Today's machine headroom, or `None` when the config or host memory cannot be read.
fn host_headroom() -> Option<Headroom> {
    let machine = rimz::config::MachineConfig::load().ok()?;
    Headroom::read(&machine.lsp).ok()
}

fn tree_bytes(reading: &rimz::lsp::memory::Reading) -> u64 {
    reading
        .rss_kb
        .saturating_add(reading.swap_kb)
        .saturating_mul(1024)
}

fn write_status(
    out: &mut impl Write,
    entry: &registry::Entry,
    now_ms: u64,
    memory: rimz::lsp::memory::Reading,
    headroom: Option<Headroom>,
    estimate: u64,
) -> std::io::Result<()> {
    use registry::State;
    let mut facts = render::KeyVals::new();
    facts.push("checkout", render::cell(entry.root.display().to_string()));
    facts.push("server", render::cell(&entry.server));
    facts.push(
        "kind",
        render::cell(
            entry
                .kind
                .map_or_else(|| "unknown".into(), |kind| kind.to_string()),
        ),
    );
    let since_ms = match &entry.state {
        State::Dormant { since_ms, .. } => *since_ms,
        State::Stopped { at_ms, .. } => *at_ms,
        State::Ready => entry.ready_at_ms.unwrap_or(entry.started_at_ms),
        State::Starting | State::Indexing => entry.started_at_ms,
    };
    facts.push(
        "state",
        render::cell(format!(
            "{}, {} ago",
            render::lsp_state_label(&entry.state),
            render::age_label(now_ms.saturating_sub(since_ms) / 1000)
        )),
    );
    let needs = rimz::utils::size::decimal_bytes(estimate);
    let next = match (&entry.state, headroom) {
        (State::Ready, _) => "answers at once".into(),
        (State::Starting | State::Indexing, _) => {
            "waits for indexing (up to 110 s), then exits 4".into()
        }
        (State::Stopped { reason, .. }, _) => format!("exits 3 (stopped: {reason})"),
        (State::Dormant { .. }, Some(headroom)) if headroom.fits(estimate) => {
            format!("starts it (needs {needs}, {headroom})")
        }
        (State::Dormant { .. }, Some(headroom)) => format!(
            "needs {needs}, {headroom}; exits 4 unless idle servers can be stopped to make room"
        ),
        (State::Dormant { .. }, None) => {
            format!("starts it if memory admits it (needs {needs}), otherwise exits 4")
        }
    };
    facts.push("next query", render::cell(next));
    if entry.state.is_running() {
        facts.push(
            "memory",
            render::cell(format!(
                "{} RSS + {} swap",
                rimz::utils::size::decimal_bytes(memory.rss_kb.saturating_mul(1024)),
                rimz::utils::size::decimal_bytes(memory.swap_kb.saturating_mul(1024))
            )),
        );
    }
    let peak = entry.peak_rss_kb.max(if entry.state.is_running() {
        memory.peak_kb
    } else {
        0
    });
    facts.push(
        "peak",
        if peak == 0 {
            render::cell("-").dash()
        } else {
            render::cell(rimz::utils::size::decimal_bytes(peak.saturating_mul(1024)))
        },
    );
    facts.push(
        "last query",
        render::cell(entry.last_request_at_ms.map_or_else(
            || "never".into(),
            |at| {
                format!(
                    "{} ago",
                    render::age_label(now_ms.saturating_sub(at) / 1000)
                )
            },
        )),
    );
    facts.push("requests", render::cell(entry.request_count.to_string()));
    facts.push("restarts", render::cell(entry.restarts.to_string()));
    facts.push("leases", render::cell(entry.leases.len().to_string()));
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
    if let Some(cause) = &entry.last_crash {
        let age = render::age_label(now_ms.saturating_sub(cause.at_ms) / 1000);
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
    facts.push("broker pid", render::cell(entry.broker_pid.to_string()));
    facts.push(
        "server pid",
        render::cell(
            entry
                .server_pid
                .map_or_else(|| "none".into(), |pid| pid.to_string()),
        ),
    );
    facts.render(out)?;
    for editor in &entry.attached {
        let mut details = render::KeyVals::new();
        details.push(
            "editor",
            render::cell(format!(
                "{}  {}  attached {}s ago",
                editor.pid,
                editor.name.as_deref().unwrap_or("unnamed"),
                now_ms.saturating_sub(editor.since_ms) / 1000
            )),
        );
        details.render(out)?;
        let mut buffers = render::KeyVals::new().indent(2);
        for document in &editor.open {
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
                .unwrap_or_else(|| document.uri.clone());
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
        buffers.render(out)?;
    }
    Ok(())
}

fn list_table<'a>(
    entries: impl Iterator<Item = (&'a registry::Entry, &'a rimz::lsp::memory::Reading)>,
    now_ms: u64,
) -> render::Table {
    use render::status;
    let mut entries: Vec<_> = entries.collect();
    entries.sort_by_key(|(entry, _)| status::lsp_order(entry));
    let mut table = render::Table::new([
        "STATE", "CHECKOUT", "SERVER", "RSS", "SWAP", "TOTAL", "PEAK", "LAST", "LEASES",
    ])
    .right(&[3, 4, 5, 6, 7, 8]);
    for (entry, reading) in entries {
        let running = entry.state.is_running();
        let (glyph, style) = render::verdict(status::lsp(&entry.state));
        let [rss, swap, total] = if running {
            [
                reading.rss_kb,
                reading.swap_kb,
                reading.rss_kb.saturating_add(reading.swap_kb),
            ]
            .map(|kb| render::cell(rimz::utils::size::decimal_bytes(kb.saturating_mul(1024))))
        } else {
            std::array::from_fn(|_| render::cell("-").dash())
        };
        let peak = entry
            .peak_rss_kb
            .max(if running { reading.peak_kb } else { 0 });
        table.row([
            render::cell(format!("{glyph} {}", render::lsp_state_label(&entry.state))).fg(style),
            render::cell(render::home_relative_path(&entry.root)),
            render::cell(&entry.server),
            rss,
            swap,
            total,
            if peak == 0 {
                render::cell("-").dash()
            } else {
                render::cell(rimz::utils::size::decimal_bytes(peak.saturating_mul(1024)))
            },
            entry.last_request_at_ms.map_or_else(
                || render::cell("-").dash(),
                |at| {
                    render::cell(format!(
                        "{} ago",
                        render::age_label(now_ms.saturating_sub(at) / 1000)
                    ))
                },
            ),
            render::cell(entry.leases.len().to_string()),
        ]);
    }
    table
}

/// Partitions entries into this room and the rest: attach records
/// the launch repo root, which is the room root unless a pinned room's cwd
/// names another repository.
fn room_entries(
    entries: Vec<registry::Entry>,
    workspace: &rimz::workspace::ResolvedWorkspace,
) -> (Vec<registry::Entry>, Vec<registry::Entry>) {
    entries.into_iter().partition(|entry| {
        entry.project.as_deref().is_some_and(|project| {
            project == workspace.project_root || project == workspace.launch_repo_root()
        })
    })
}

fn list(all: bool, json: bool, globals: &GlobalFlags) -> Result<()> {
    let entries = registry::sweep()?;
    let cwd = std::fs::canonicalize(globals.root.clone().unwrap_or(std::env::current_dir()?))?;
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(&cwd, globals.root.clone())?;
    let (entries, others) = room_entries(entries, &workspace);
    let mut out = render::out();
    if json {
        return render::finish(writeln!(out, "{}", serde_json::to_string_pretty(&entries)?));
    }
    let needs = rimz::lsp::history::Needs::read();
    render::finish(
        ListView {
            entries: &entries,
            others: &others,
            all,
            now_ms: rimz::utils::time::unix_now_ms(),
            headroom: host_headroom(),
        }
        .render(&mut out, rimz::lsp::memory::for_entry, |entry| {
            needs.for_entry(entry)
        }),
    )
}

fn checkout_entries(
    path: Option<PathBuf>,
    server: Option<String>,
    all: bool,
    globals: &GlobalFlags,
) -> Result<(PathBuf, Vec<registry::Entry>)> {
    let mut entries = registry::sweep()?;
    let path = path
        .or_else(|| globals.root.clone())
        .unwrap_or(std::env::current_dir()?);
    let cwd = std::fs::canonicalize(&path).map_err(|error| {
        super::usage::UsageError::new(format!("cannot open checkout {}: {error}", path.display()))
    })?;
    if let Some(server) = server.as_deref()
        && let Ok(workspace) = rimz::workspace::WorkspaceResolver::resolve(&cwd, None)
        && let Ok(machine) = rimz::config::MachineConfig::load()
        && let Ok(config) = rimz::config::effective::load(&machine, workspace.launch_repo_root())
    {
        let registered = registry::read_entries()?;
        validate_server_name(
            server,
            config
                .lsp_servers
                .keys()
                .map(String::as_str)
                .chain(config.untrusted_lsp_servers.iter().map(String::as_str)),
            registered.iter(),
        )?;
    }
    let root = registry::enclosing_checkout(&cwd, &entries)
        .unwrap_or(&cwd)
        .to_owned();
    if all {
        let workspace =
            rimz::workspace::WorkspaceResolver::resolve_participant(&cwd, globals.root.clone())?;
        return Ok((root, room_entries(entries, &workspace).0));
    }
    entries.retain(|entry| {
        entry.root == root && server.as_ref().is_none_or(|server| server == &entry.server)
    });
    if entries.len() > 1 {
        let mut names: Vec<_> = entries.iter().map(|entry| entry.server.as_str()).collect();
        names.sort_unstable();
        anyhow::bail!(
            "multiple language servers for {}: {}; choose --server NAME",
            root.display(),
            names.join(", ")
        );
    }
    Ok((root, entries))
}

fn stop(
    path: Option<PathBuf>,
    server: Option<String>,
    all: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let (root, entries) = checkout_entries(path, server, all, globals)?;
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
        if let Ok(response) = registry::request(
            entry,
            &serde_json::json!({"op": "status"}),
            Duration::from_secs(2),
        ) && let Ok(current) = serde_json::from_value::<registry::Entry>(response)
            && current.state.is_alarm()
        {
            errors.push(format!(
                "{} ({}): not acknowledged, still {} while its broker finishes stopping the server (run rimz lsp stop again in a second)",
                entry.server,
                entry.root.display(),
                render::lsp_state_label(&current.state)
            ));
            continue;
        }
        if entry.state.is_running() {
            stopped += &format!("stopped {} ({})\n", entry.server, entry.root.display());
        } else if entry.state.is_alarm() {
            stopped += &format!(
                "acknowledged {} ({}), was {}\n",
                entry.server,
                entry.root.display(),
                render::lsp_state_label(&entry.state)
            );
        }
    }
    if stopped.is_empty() && errors.is_empty() {
        let reason = if all {
            if entries.is_empty() {
                "no language server registered in this room".into()
            } else {
                "no language server in this room holds memory".into()
            }
        } else if entries.is_empty() {
            format!("no language server registered for {}", root.display())
        } else {
            entries
                .iter()
                .map(|entry| {
                    format!(
                        "{} for {} holds no memory ({})",
                        entry.server,
                        entry.root.display(),
                        render::lsp_state_label(&entry.state)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        stopped = format!("nothing to stop: {reason}\n");
    }
    let written = render::out().write_all(stopped.as_bytes());
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    render::finish(written)
}

#[cfg(test)]
#[path = "lsp_tests.rs"]
mod tests;
