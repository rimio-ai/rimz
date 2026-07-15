//! `rimz remote` — named SSH room aliases and remote attach.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{AttachFlags, GlobalFlags};
use crate::cli::room::{AttachAction, AttachMode, attach_action, exec_attach_command};
use rimz::ids::MuxName;
use rimz::remote::aliases::{RemoteAlias, RemoteAliases};
use rimz::remote::{
    RemoteTarget, RemoteTargetError, SshAttachOptions, SshAttachPlan, SshDestination, TermPlan,
    infocmp_program, term_plan_from,
};

mod bandwidth;
mod link_stats;
mod list;
mod setup;
mod supervisor;
mod web;

#[derive(Debug, Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteSubcmd,
}

#[derive(Debug, Subcommand)]
enum RemoteSubcmd {
    /// Save a named remote target.
    #[command(after_help = "With `remote add`, --mux <name> pins the saved alias.")]
    Add {
        name: String,
        target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        /// Come up empty when this alias births a remote room.
        #[arg(long)]
        no_resume: bool,
    },
    /// Replace a saved remote target.
    #[command(
        after_help = "Like `remote add`, --mux <name> pins the saved alias. Flags not passed reset to their defaults."
    )]
    Update {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        name: String,
        target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        /// Come up empty when this alias births a remote room.
        #[arg(long)]
        no_resume: bool,
    },
    /// Connect to a remote alias or raw `[user@]host:<session-or-path>` target.
    Connect {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        alias_or_target: String,
        /// Force a fresh remote room by passing `--no-resume` to the remote rimz.
        #[arg(long)]
        reset: bool,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        /// Open the remote Zellij room in the local browser through an SSH tunnel.
        #[arg(long)]
        web: bool,
        /// Local tunnel port for `--web`.
        #[arg(long, requires = "web")]
        web_port: Option<u16>,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Install rimz on a remote alias, `[user@]host:<session-or-path>` target, or `[user@]host`.
    Setup {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        alias_or_host: String,
    },
    /// Connect to a remote alias or raw target with `--no-resume`.
    Reset {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        alias_or_target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        /// Open the remote Zellij room in the local browser through an SSH tunnel.
        #[arg(long)]
        web: bool,
        /// Local tunnel port for `--web`.
        #[arg(long, requires = "web")]
        web_port: Option<u16>,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Delete a saved remote alias.
    Rm {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        name: String,
    },
    /// Rename a saved remote alias.
    Rename {
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        old: String,
        new: String,
    },
    /// List saved remote aliases.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Profile the current room's per-pane render output (run on the host serving the room).
    Bandwidth {
        /// Sampling window in seconds.
        #[arg(long, default_value_t = 5)]
        secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Hidden remote-link stats plumbing. The SSH probe stream calls this.
    #[command(name = "link-stats", hide = true)]
    LinkStats {
        #[command(subcommand)]
        command: LinkStatsSubcmd,
    },
}

#[derive(Debug, Subcommand)]
enum LinkStatsSubcmd {
    /// Ingest JSONL link probes for one remote room and publish link-stats.json.
    Ingest(link_stats::LinkStatsIngestArgs),
}

fn build_alias(
    name: String,
    target: String,
    no_reconnect: bool,
    no_resume: bool,
    globals: &GlobalFlags,
) -> RemoteAlias {
    RemoteAlias {
        name,
        target,
        reconnect: !no_reconnect,
        no_resume,
        mux: globals.mux,
    }
}

impl RemoteArgs {
    /// The low-cardinality command label for the Sentry command scope.
    pub(crate) fn command_label(&self) -> &'static str {
        match &self.command {
            RemoteSubcmd::Add { .. } => "remote add",
            RemoteSubcmd::Update { .. } => "remote update",
            RemoteSubcmd::Connect { .. } => "remote connect",
            RemoteSubcmd::Setup { .. } => "remote setup",
            RemoteSubcmd::Reset { .. } => "remote reset",
            RemoteSubcmd::Rm { .. } => "remote rm",
            RemoteSubcmd::Rename { .. } => "remote rename",
            RemoteSubcmd::List { .. } => "remote list",
            RemoteSubcmd::Bandwidth { .. } => "remote bandwidth",
            RemoteSubcmd::LinkStats { .. } => "remote link-stats",
        }
    }
}

pub fn run(args: RemoteArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        RemoteSubcmd::Add {
            name,
            target,
            no_reconnect,
            no_resume,
        } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            let entry = build_alias(name, target, no_reconnect, no_resume, globals);
            if aliases.contains(&entry.name)
                && std::io::stdin().is_terminal()
                && super::confirm(&format!(
                    "remote alias `{}` already exists; update it?",
                    entry.name
                ))?
            {
                aliases.update(entry)?;
            } else {
                aliases.add(entry)?;
            }
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::Update {
            name,
            target,
            no_reconnect,
            no_resume,
        } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.update(build_alias(name, target, no_reconnect, no_resume, globals))?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::Connect {
            alias_or_target,
            reset,
            no_reconnect,
            web,
            web_port,
            attach,
        } => connect(
            alias_or_target,
            reset,
            no_reconnect,
            web::RemoteWebOptions {
                enabled: web,
                port: web_port,
            },
            attach,
            globals,
        ),
        RemoteSubcmd::Setup { alias_or_host } => setup::run(alias_or_host, globals),
        RemoteSubcmd::Reset {
            alias_or_target,
            no_reconnect,
            web,
            web_port,
            attach,
        } => connect(
            alias_or_target,
            true,
            no_reconnect,
            web::RemoteWebOptions {
                enabled: web,
                port: web_port,
            },
            attach,
            globals,
        ),
        RemoteSubcmd::Rm { name } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.remove(&name)?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::Rename { old, new } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.rename(&old, new)?;
            aliases.save().context("saving remote aliases")?;
            Ok(())
        }
        RemoteSubcmd::List { json } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            list::print(aliases.entries(), json)?;
            Ok(())
        }
        RemoteSubcmd::Bandwidth { secs, json } => bandwidth::run(secs, json, globals),
        RemoteSubcmd::LinkStats { command } => match command {
            LinkStatsSubcmd::Ingest(args) => link_stats::ingest(args),
        },
    }
}

#[derive(Debug)]
struct RemoteConnect {
    origin: String,
    target: RemoteTarget,
    reconnect: bool,
    no_resume: bool,
    mux: Option<MuxName>,
    web: web::RemoteWebOptions,
}

fn resolve_connect(
    input: &str,
    reset: bool,
    no_reconnect: bool,
    cli_mux: Option<MuxName>,
    aliases: &RemoteAliases,
) -> Result<RemoteConnect> {
    if input.contains(':') {
        return Ok(RemoteConnect {
            origin: input.to_owned(),
            target: RemoteTarget::parse(input)?,
            reconnect: !no_reconnect,
            no_resume: reset,
            mux: cli_mux,
            web: web::RemoteWebOptions::default(),
        });
    }
    let Some(alias) = aliases.get(input) else {
        bail!("no such remote alias `{input}`; run `rimz remote list`");
    };
    Ok(RemoteConnect {
        origin: input.to_owned(),
        target: RemoteTarget::parse(&alias.target)?,
        reconnect: alias.reconnect && !no_reconnect,
        no_resume: alias.no_resume || reset,
        mux: cli_mux.or(alias.mux),
        web: web::RemoteWebOptions::default(),
    })
}

fn resolve_setup_destination(input: &str, aliases: &RemoteAliases) -> Result<SshDestination> {
    if let Some(alias) = aliases.get(input) {
        return Ok(RemoteTarget::parse(&alias.target)?
            .ssh_destination()
            .clone());
    }
    match RemoteTarget::parse(input) {
        Ok(target) => Ok(target.ssh_destination().clone()),
        Err(RemoteTargetError::MissingColon(_)) => Ok(SshDestination::parse(input)?),
        Err(err) => Err(err.into()),
    }
}

fn connect(
    alias_or_target: String,
    reset: bool,
    no_reconnect: bool,
    web: web::RemoteWebOptions,
    attach: AttachFlags,
    globals: &GlobalFlags,
) -> Result<()> {
    let aliases = RemoteAliases::load().context("loading remote aliases")?;
    let mut remote = resolve_connect(&alias_or_target, reset, no_reconnect, globals.mux, &aliases)?;
    remote.web = web;
    attach_remote(remote, attach.mode())
}

/// SSH remote attach: the local rimz is a launcher and link supervisor only.
/// Workspace resolution, session birth, the sidebar, and the health gate all
/// run on the remote host's own `rimz`; terminal mode renders here over
/// `ssh -t`, and web mode opens a supervised local-forward tunnel.
fn attach_remote(remote: RemoteConnect, mode: AttachMode) -> Result<()> {
    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print => {
            if remote.web.enabled {
                bail!("--web is web-only and has no SSH attach command; drop --print");
            }
            let term = remote_term_plan();
            let plan = SshAttachPlan::new(SshAttachOptions {
                target: remote.target,
                no_resume: remote.no_resume,
                mux: remote.mux,
                term,
                truecolor: rimz::tui::truecolor(),
            });
            let plain_spec = plan.initial().plain();
            supervisor::print_remote_command(&plain_spec);
            Ok(())
        }
        AttachAction::Exec => {
            let program = rimz::remote::ssh_program();
            which::which(&program).map_err(|_| {
                anyhow::anyhow!(
                    "`{program}` is not on PATH; install an OpenSSH client to attach \
                     remotely, or run with --print to emit the command"
                )
            })?;
            if remote.web.enabled {
                return web::run_remote_web(&remote);
            }
            let term = remote_term_plan();
            let plan = SshAttachPlan::new(SshAttachOptions {
                target: remote.target,
                no_resume: remote.no_resume,
                mux: remote.mux,
                term,
                truecolor: rimz::tui::truecolor(),
            });
            if remote.reconnect {
                let control = rimz::remote::link::validated_control_path()
                    .context("checking SSH ControlMaster socket path")?;
                supervisor::supervise_remote(&plan, &control, remote.origin.as_str())
            } else {
                supervisor::report_remote_connect(plan.target().host_display(), false);
                let plain_spec = plan.initial().plain();
                exec_attach_command(&plain_spec)
            }
        }
    }
}

fn remote_term_plan() -> TermPlan {
    term_plan_from(std::env::var("TERM").ok().as_deref(), run_infocmp)
}

fn run_infocmp(term: &str) -> Option<String> {
    let out = std::process::Command::new(infocmp_program())
        .arg("-x")
        .arg(term)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn sleep_interruptibly(duration: Duration, stop: &AtomicBool) {
    if duration.is_zero() {
        return;
    }
    let step = Duration::from_millis(50);
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(step));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(
        name: &str,
        target: &str,
        reconnect: bool,
        no_resume: bool,
        mux: Option<MuxName>,
    ) -> RemoteAlias {
        RemoteAlias {
            name: name.to_owned(),
            target: target.to_owned(),
            reconnect,
            no_resume,
            mux,
        }
    }

    #[test]
    fn connect_resolution_applies_alias_cli_overrides() {
        let globals = GlobalFlags {
            mux: Some(MuxName::Tmux),
            zellij: false,
            tmux: false,
            root: None,
            color: super::super::ColorWhen::Auto,
        };

        let built_alias = build_alias(
            "prod".to_owned(),
            "prod-box:query-engine".to_owned(),
            false,
            false,
            &globals,
        );

        assert_eq!(built_alias.mux, Some(MuxName::Tmux));

        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false, None))
            .unwrap();
        aliases
            .add(alias("fresh", "fresh-box:query-engine", true, true, None))
            .unwrap();
        aliases
            .add(alias(
                "tmuxed",
                "tmux-box:query-engine",
                true,
                false,
                Some(MuxName::Tmux),
            ))
            .unwrap();

        let raw = resolve_connect("prod:raw-session", false, false, None, &aliases).unwrap();
        assert_eq!(raw.target.ssh_destination().as_str(), "prod");
        assert_eq!(raw.origin, "prod:raw-session");
        assert!(raw.reconnect);
        assert!(!raw.no_resume);

        let named = resolve_connect("prod", false, false, None, &aliases).unwrap();
        assert_eq!(named.target.ssh_destination().as_str(), "prod-box");
        assert_eq!(named.origin, "prod");
        assert!(named.reconnect);
        assert!(!named.no_resume);

        let fresh = resolve_connect("fresh", false, false, None, &aliases).unwrap();
        assert!(fresh.no_resume);

        let reset = resolve_connect("prod", true, false, None, &aliases).unwrap();
        assert!(reset.no_resume);

        let remote = resolve_connect("prod", false, true, None, &aliases).unwrap();
        assert!(!remote.reconnect);

        let alias_mux = resolve_connect("tmuxed", false, false, None, &aliases).unwrap();
        assert_eq!(alias_mux.mux, Some(MuxName::Tmux));

        let cli_mux =
            resolve_connect("tmuxed", false, false, Some(MuxName::Zellij), &aliases).unwrap();
        assert_eq!(cli_mux.mux, Some(MuxName::Zellij));
    }

    #[test]
    fn setup_resolution_accepts_alias_target_and_bare_host() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false, None))
            .unwrap();

        let named = resolve_setup_destination("prod", &aliases).unwrap();
        assert_eq!(named.destination, "prod-box");
        assert_eq!(named.host, "prod-box");

        let target =
            resolve_setup_destination("agent@prod-box:/srv/query-engine", &aliases).unwrap();
        assert_eq!(target.destination, "agent@prod-box");
        assert_eq!(target.host, "prod-box");

        let bare = resolve_setup_destination("alice@new-box", &aliases).unwrap();
        assert_eq!(bare.destination, "alice@new-box");
        assert_eq!(bare.host, "new-box");

        let ipv6 = resolve_setup_destination("user@[::1]", &aliases).unwrap();
        assert_eq!(ipv6.destination, "user@[::1]");
        assert_eq!(ipv6.host, "::1");
    }
}
