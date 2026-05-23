use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use jiff::Timestamp;
use serde_json::Value;

use super::{GlobalFlags, open_ledger};
use rimz::bridge::{self, BridgeOutcome, ExpectedFrame, SocketGuard};
use rimz::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, Surface,
};
use rimz::ids::{RequestId, ResolverId};
use rimz::workspace::WorkspaceResolver;

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
        /// Total seconds to wait before failing. Omit for unbounded.
        #[arg(long)]
        timeout_seconds: Option<u64>,
        /// Print the request id and return without blocking.
        #[arg(long)]
        no_block: bool,
    },
    /// List feed items, newest first.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
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
    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
    let ledger = open_ledger(&workspace)?;
    match args.command {
        FeedSubcmd::Push { kind, title, body } => {
            let mut item = FeedItem::new(
                workspace.workspace_id.clone(),
                Surface::NativeUi,
                FeedKind::from_cli(&kind),
                title,
                "rimz",
                "cli",
            );
            item.body = body;
            ledger.push_feed_item(&item, &workspace.session_name)?;
            #[expect(clippy::print_stdout, reason = "command result is the request id")]
            {
                println!("{}", item.request_id);
            }
            Ok(())
        }
        FeedSubcmd::Ask {
            title,
            options,
            timeout_seconds,
            no_block,
        } => {
            let mut item = FeedItem::new(
                workspace.workspace_id.clone(),
                Surface::Script,
                FeedKind::Question,
                title,
                "rimz",
                "cli",
            );
            item.options = options;
            item.hook_wait_timeout_seconds = timeout_seconds.unwrap_or(0);
            if let Some(seconds) = timeout_seconds {
                item.feed_deadline_at = Some(Timestamp::now() + Duration::from_secs(seconds));
            }
            let request_id = item.request_id.clone();

            if no_block {
                ledger.push_feed_item(&item, &workspace.session_name)?;
                #[expect(clippy::print_stdout, reason = "user-visible request id")]
                {
                    println!("{request_id}");
                }
                return Ok(());
            }

            // Bind before push so a fast resolver can't miss the socket.
            let expected = ExpectedFrame {
                workspace_id: item.workspace_id.clone(),
                request_id: request_id.clone(),
                nonce: item.nonce.clone(),
            };
            let (sock, sock_path) = bridge::bind(ledger.runtime_paths(), &request_id)
                .context("binding bridge socket")?;
            let _cleanup = SocketGuard::new(sock_path);

            ledger.push_feed_item(&item, &workspace.session_name)?;
            #[expect(clippy::print_stdout, reason = "user-visible request id")]
            {
                println!("{request_id}");
            }

            let cap = timeout_seconds.map(Duration::from_secs);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building bridge runtime")?;
            let outcome = runtime
                .block_on(bridge::wait_for_resolution_owning(sock, expected, cap))
                .context("waiting on bridge")?;

            match outcome {
                BridgeOutcome::Resolved => {
                    let resolved = ledger.load_feed_item(&request_id)?;
                    let decision = resolved
                        .resolution
                        .as_ref()
                        .map(|r| &r.decision)
                        .ok_or_else(|| {
                            anyhow::anyhow!("bridge signalled resolved but no resolution on disk")
                        })?;
                    let rendered = serde_json::to_string(decision)?;
                    #[expect(clippy::print_stdout, reason = "user-visible decision payload")]
                    {
                        println!("{rendered}");
                    }
                    Ok(())
                }
                BridgeOutcome::Neutral => {
                    let timeout = ledger.mark_feed_item_timed_out(
                        &request_id,
                        &workspace.session_name,
                        AbandonReason::ScriptWaitTimeout,
                    )?;
                    if timeout.status == FeedStatus::Resolved {
                        let resolved = ledger.load_feed_item(&request_id)?;
                        let decision = resolved
                            .resolution
                            .as_ref()
                            .map(|r| &r.decision)
                            .ok_or_else(|| {
                                anyhow::anyhow!("feed item resolved without a resolution payload")
                            })?;
                        let rendered = serde_json::to_string(decision)?;
                        #[expect(clippy::print_stdout, reason = "user-visible decision payload")]
                        {
                            println!("{rendered}");
                        }
                        return Ok(());
                    }
                    bail!("timed out waiting for resolution of {request_id}");
                }
            }
        }
        FeedSubcmd::List { json } => {
            let items = ledger.list_feed_items()?;
            if json {
                let rendered = serde_json::to_string_pretty(&items)?;
                #[expect(clippy::print_stdout, reason = "json emitter")]
                {
                    println!("{rendered}");
                }
            } else {
                for item in items {
                    #[expect(clippy::print_stdout, reason = "human listing")]
                    {
                        println!(
                            "{}\t{}\t{}\t{}",
                            item.request_id, item.status, item.surface, item.title
                        );
                    }
                }
            }
            Ok(())
        }
        FeedSubcmd::Show { request_id, json } => {
            let id = request_id.parse::<RequestId>()?;
            let item = ledger.load_feed_item(&id)?;
            if json {
                let rendered = serde_json::to_string_pretty(&item)?;
                #[expect(clippy::print_stdout, reason = "json emitter")]
                {
                    println!("{rendered}");
                }
            } else {
                #[expect(clippy::print_stdout, reason = "human display")]
                {
                    println!(
                        "{} [{}/{}] {}",
                        item.request_id, item.status, item.surface, item.title
                    );
                    if let Some(body) = item.body {
                        println!("{body}");
                    }
                }
            }
            Ok(())
        }
        FeedSubcmd::Resolve {
            request_id,
            decision,
            resolver_id,
            method,
            override_chain,
        } => {
            let id = request_id.parse::<RequestId>()?;
            let decision: Value =
                serde_json::from_str(&decision).context("parsing --decision as JSON")?;
            let mut resolution = Resolution::new(decision, method.into());
            resolution.resolver_id = resolver_id.map(|id| id.parse::<ResolverId>()).transpose()?;
            let outcome = ledger.resolve_feed_item(
                &id,
                resolution,
                override_chain,
                &workspace.session_name,
            )?;
            #[expect(clippy::print_stdout, reason = "command outcome")]
            {
                println!(
                    "{} effective={} late={}",
                    outcome.request_id, outcome.effective, outcome.late
                );
            }
            Ok(())
        }
        FeedSubcmd::Dismiss { request_id, reason } => {
            let id = request_id.parse::<RequestId>()?;
            ledger.dismiss_feed_item(&id, reason, &workspace.session_name)?;
            Ok(())
        }
        FeedSubcmd::Abstain {
            request_id,
            resolver_id,
            reason,
        } => {
            let id = request_id.parse::<RequestId>()?;
            let resolver = resolver_id.parse::<ResolverId>()?;
            let outcome =
                ledger.abstain_feed_item(&id, &resolver, reason, &workspace.session_name)?;
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
    }
}
