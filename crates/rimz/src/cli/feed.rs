//! `rimz feed` — the feed surface: push, ask, list, show, resolve, dismiss, abstain.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use jiff::Timestamp;
use serde_json::Value;

use super::{GlobalFlags, open_ledger};
use crate::cli::render;
use rimz::bridge::{self, BridgeOutcome, ExpectedFrame, SocketGuard};
use rimz::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, Surface,
};
use rimz::ids::{RequestId, ResolverId};
use rimz::ledger::runtime::{RuntimeScope, current_process_owner};
use rimz::ledger::{FeedStoreErr, LedgerErr};
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, ResolvedWorkspace};

#[derive(Debug, Args)]
pub struct FeedArgs {
    #[command(subcommand)]
    command: FeedSubcmd,
}

#[derive(Debug, Subcommand)]
enum FeedSubcmd {
    /// Push a non-blocking feed item (no resolver wait).
    Push {
        #[arg(long)]
        kind: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
    },
    /// Ask a question; block until a human or resolver answers.
    Ask {
        #[arg(long)]
        title: String,
        #[arg(long, value_delimiter = ',')]
        options: Vec<String>,
        /// Time to wait before failing (`30s`, `5m`, `1h`, `4h`, `1d`). Omit for unbounded.
        #[arg(long, value_parser = parse_timeout)]
        timeout: Option<Duration>,
        /// Print the request id and return without blocking.
        #[arg(long)]
        no_block: bool,
    },
    /// List feed items, newest first.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        audit: bool,
    },
    /// Show one feed item by id.
    Show {
        request_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Apply a resolver decision (valid for surface = bridge | script).
    Resolve {
        request_id: String,
        #[arg(long)]
        decision: String,
        #[arg(long)]
        resolver_id: Option<String>,
        #[arg(long, value_enum, default_value_t = MethodArg::Cli)]
        method: MethodArg,
        /// Bypass the chain-active CAS check (`Human override`).
        #[arg(long)]
        override_chain: bool,
    },
    /// Dismiss a native-UI item without forwarding to the agent.
    Dismiss {
        request_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Active resolver passes on a request, advancing the chain.
    Abstain {
        request_id: String,
        #[arg(long)]
        resolver_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum MethodArg {
    HookBridge,
    PaneSend,
    Cli,
    Sidebar,
}

impl From<MethodArg> for ResolutionMethod {
    fn from(value: MethodArg) -> Self {
        match value {
            MethodArg::HookBridge => Self::HookBridge,
            MethodArg::PaneSend => Self::PaneSend,
            MethodArg::Cli => Self::Cli,
            MethodArg::Sidebar => Self::Sidebar,
        }
    }
}

pub fn run(args: FeedArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    match args.command {
        FeedSubcmd::Push { kind, title, body } => push(&ledger, &workspace, kind, title, body),
        FeedSubcmd::Ask {
            title,
            options,
            timeout,
            no_block,
        } => ask(&ledger, &workspace, title, options, timeout, no_block),
        FeedSubcmd::List { json, audit } => list(&ledger, json, audit),
        FeedSubcmd::Show { request_id, json } => show(&ledger, request_id, json),
        FeedSubcmd::Resolve {
            request_id,
            decision,
            resolver_id,
            method,
            override_chain,
        } => resolve(
            &ledger,
            &workspace,
            request_id,
            decision,
            resolver_id,
            method,
            override_chain,
        ),
        FeedSubcmd::Dismiss { request_id, reason } => {
            dismiss(&ledger, &workspace, request_id, reason)
        }
        FeedSubcmd::Abstain {
            request_id,
            resolver_id,
            reason,
        } => abstain(&ledger, &workspace, request_id, resolver_id, reason),
    }
}

fn push(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    kind: String,
    title: String,
    body: Option<String>,
) -> Result<()> {
    let mut item = FeedItem::new(
        workspace.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::from_cli(&kind),
        title,
        "rimz",
        "cli",
    );
    item.body = body;
    attach_worktree(&mut item, workspace);
    attach_current_owner(&mut item);
    ledger.push_feed_item(&item, &workspace.session_name)?;
    #[expect(clippy::print_stdout, reason = "command result is the request id")]
    {
        println!("{}", item.request_id);
    }
    Ok(())
}

fn ask(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    title: String,
    options: Vec<String>,
    timeout: Option<Duration>,
    no_block: bool,
) -> Result<()> {
    let item = ask_item(workspace, title, options, timeout);
    let request_id = item.request_id.clone();

    if no_block {
        ledger.push_feed_item(&item, &workspace.session_name)?;
        print_request_id(&request_id);
        return Ok(());
    }

    // Bind before push so a fast resolver can't miss the socket.
    let expected = ExpectedFrame {
        workspace_id: item.workspace_id.clone(),
        request_id: request_id.clone(),
        nonce: item.nonce.clone(),
    };
    let (sock, sock_path) =
        bridge::bind(ledger.runtime_paths(), &request_id).context("binding bridge socket")?;
    let _cleanup = SocketGuard::new(sock_path);

    ledger.push_feed_item(&item, &workspace.session_name)?;
    print_request_id(&request_id);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building bridge runtime")?;
    let outcome = runtime
        .block_on(bridge::wait_for_resolution_owning(sock, expected, timeout))
        .context("waiting on bridge")?;

    emit_bridge_outcome(ledger, workspace, &request_id, outcome)
}

fn ask_item(
    workspace: &ResolvedWorkspace,
    title: String,
    options: Vec<String>,
    timeout: Option<Duration>,
) -> FeedItem {
    let mut item = FeedItem::new(
        workspace.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        title,
        "rimz",
        "cli",
    );
    item.options = options;
    attach_worktree(&mut item, workspace);
    attach_current_owner(&mut item);
    // The pane the asking script runs inside, when it runs inside one; outside
    // a pane the ask stays rollup metadata served by feed list/resolve.
    item.pane = rimz::mux::ambient_pane_id().map(PaneRef::from_id);
    item.hook_wait_timeout_seconds = timeout.map(|d| d.as_secs()).unwrap_or(0);
    if let Some(deadline) = timeout {
        item.feed_deadline_at = Some(Timestamp::now() + deadline);
    }
    item
}

fn emit_bridge_outcome(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    request_id: &RequestId,
    outcome: BridgeOutcome,
) -> Result<()> {
    match outcome {
        BridgeOutcome::Resolved => emit_resolved_decision(
            ledger,
            request_id,
            "bridge signalled resolved but no resolution on disk",
        ),
        BridgeOutcome::Terminal => {
            bail!("request {request_id} closed before a decision was delivered")
        }
        BridgeOutcome::Neutral => emit_neutral_outcome(ledger, workspace, request_id),
    }
}

fn emit_neutral_outcome(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    request_id: &RequestId,
) -> Result<()> {
    let timeout = match ledger.mark_feed_item_timed_out(
        request_id,
        &workspace.session_name,
        AbandonReason::ScriptWaitTimeout,
    ) {
        Ok(timeout) => timeout,
        Err(err) if is_feed_item_not_found(&err, request_id) => {
            bail!("request {request_id} closed before a decision was delivered")
        }
        Err(err) => return Err(err.into()),
    };
    if timeout.status == FeedStatus::Resolved {
        return emit_resolved_decision(
            ledger,
            request_id,
            "feed item resolved without a resolution payload",
        );
    }
    bail!("timed out waiting for resolution of {request_id}");
}

fn emit_resolved_decision(ledger: &Ledger, request_id: &RequestId, missing: &str) -> Result<()> {
    let resolved = match ledger.load_feed_item(request_id) {
        Ok(resolved) => resolved,
        Err(err) if is_feed_item_not_found(&err, request_id) => {
            bail!("request {request_id} closed before a decision was delivered")
        }
        Err(err) => return Err(err.into()),
    };
    let decision = resolved
        .resolution
        .as_ref()
        .map(|r| &r.decision)
        .ok_or_else(|| anyhow::anyhow!(missing.to_owned()))?;
    let rendered = serde_json::to_string(decision)?;
    #[expect(clippy::print_stdout, reason = "user-visible decision payload")]
    {
        println!("{rendered}");
    }
    Ok(())
}

fn is_feed_item_not_found(err: &LedgerErr, request_id: &RequestId) -> bool {
    matches!(
        err,
        LedgerErr::FeedStore(FeedStoreErr::NotFound(missing))
            if missing.as_str() == request_id.as_str()
    )
}

fn list(ledger: &Ledger, json: bool, audit: bool) -> Result<()> {
    let items = if audit {
        ledger.list_feed_items()?
    } else {
        ledger.runtime_projection(RuntimeScope::Runtime)?.items
    };
    if json {
        let rendered = serde_json::to_string_pretty(&items)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
    } else {
        let mut table = render::Table::new(["REQUEST", "STATUS", "SURFACE", "TITLE"]);
        for item in items {
            table.row([
                render::cell(item.request_id.to_string()).fg(render::palette::ACCENT),
                render::cell(item.status.to_string()).fg(render::status::feed(item.status)),
                render::cell(item.surface.to_string()).fg(render::palette::META),
                render::cell(item.title),
            ]);
        }
        table.render(&mut render::out())?;
    }
    Ok(())
}

fn show(ledger: &Ledger, request_id: String, json: bool) -> Result<()> {
    let id = request_id.parse::<RequestId>()?;
    let item = ledger.load_feed_item(&id)?;
    if json {
        let rendered = serde_json::to_string_pretty(&item)?;
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
    } else {
        use std::io::Write;
        let mut out = render::out();
        writeln!(
            out,
            "{} [{}/{}] {}",
            render::paint(render::palette::ACCENT, &item.request_id.to_string()),
            render::paint(render::status::feed(item.status), item.status.as_str()),
            item.surface,
            item.title,
        )?;
        if let Some(body) = item.body {
            writeln!(out, "{body}")?;
        }
    }
    Ok(())
}

fn resolve(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    request_id: String,
    decision: String,
    resolver_id: Option<String>,
    method: MethodArg,
    override_chain: bool,
) -> Result<()> {
    let id = request_id.parse::<RequestId>()?;
    let decision: Value = serde_json::from_str(&decision).context("parsing --decision as JSON")?;
    let mut resolution = Resolution::new(decision, method.into());
    resolution.resolver_id = resolver_id.map(|id| id.parse::<ResolverId>()).transpose()?;
    let outcome =
        ledger.resolve_feed_item(&id, resolution, override_chain, &workspace.session_name)?;
    #[expect(clippy::print_stdout, reason = "command outcome")]
    {
        println!(
            "{} effective={} late={}",
            outcome.request_id, outcome.effective, outcome.late
        );
    }
    Ok(())
}

fn dismiss(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    request_id: String,
    reason: Option<String>,
) -> Result<()> {
    let id = request_id.parse::<RequestId>()?;
    ledger.dismiss_feed_item(&id, reason, &workspace.session_name)?;
    Ok(())
}

fn abstain(
    ledger: &Ledger,
    workspace: &ResolvedWorkspace,
    request_id: String,
    resolver_id: String,
    reason: Option<String>,
) -> Result<()> {
    let id = request_id.parse::<RequestId>()?;
    let resolver = resolver_id.parse::<ResolverId>()?;
    let outcome = ledger.abstain_feed_item(&id, &resolver, reason, &workspace.session_name)?;
    #[expect(clippy::print_stdout, reason = "command outcome")]
    {
        println!(
            "{} next_resolver={}",
            outcome.request_id,
            outcome
                .next_resolver
                .as_ref()
                .map(|r| r.as_str())
                .unwrap_or("(none)"),
        );
    }
    Ok(())
}

#[expect(clippy::print_stdout, reason = "user-visible request id")]
fn print_request_id(request_id: &RequestId) {
    println!("{request_id}");
}

fn attach_worktree(item: &mut FeedItem, workspace: &ResolvedWorkspace) {
    item.worktree_path = Some(workspace.worktree_root.display().to_string());
    item.worktree_branch = workspace.worktree_branch.clone();
}

fn attach_current_owner(item: &mut FeedItem) {
    item.runtime_owner = Some(current_process_owner(
        RuntimeOwnerKind::Script,
        item.request_id.to_string(),
    ));
}

/// Parse a `feed ask` timeout like `30s`, `5m`, `1h`, or `1d`. Scripts can gate
/// for days, so days join the resolver/gc units.
fn parse_timeout(raw: &str) -> std::result::Result<Duration, String> {
    super::parse::parse_duration_units(raw, &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)])
}
