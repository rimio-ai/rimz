//! `rimz remote` — named SSH room aliases and remote attach.

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{AttachAction, AttachFlags, GlobalFlags, attach_action, exec_attach_command};
use rimz::ids::MuxName;
use rimz::remote::aliases::{RemoteAlias, RemoteAliases};
use rimz::remote::{
    RemoteTarget, TermPlan, infocmp_program, ssh_attach_spec, ssh_attach_spec_with_control,
    term_plan_from,
};

mod bandwidth;
mod link_stats;
mod list;
mod supervisor;

#[derive(Debug, Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteSubcmd,
}

#[derive(Debug, Subcommand)]
enum RemoteSubcmd {
    /// Save a named remote target.
    #[command(
        after_help = "With `remote add`, --mux <name> pins the saved alias when written under `remote` or `add`. A top-level `rimz --mux <name> remote add ...` is not saved."
    )]
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
        after_help = "Like `remote add`, --mux <name> pins the saved alias when scoped to `remote` or `update`. Flags not passed reset to their defaults."
    )]
    Update {
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
        alias_or_target: String,
        /// Force a fresh remote room by passing `--no-resume` to the remote rimz.
        #[arg(long)]
        reset: bool,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Connect to a remote alias or raw target with `--no-resume`.
    Reset {
        alias_or_target: String,
        /// Hand the link to a single ssh run instead of supervising reconnects.
        #[arg(long)]
        no_reconnect: bool,
        #[command(flatten)]
        attach: AttachFlags,
    },
    /// Delete a saved remote alias.
    Rm { name: String },
    /// Rename a saved remote alias.
    Rename { old: String, new: String },
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
        mux: add_persistent_mux(globals),
    }
}

impl RemoteArgs {
    /// The low-cardinality command label for the Sentry command scope.
    pub(crate) fn command_label(&self) -> &'static str {
        match &self.command {
            RemoteSubcmd::Add { .. } => "remote add",
            RemoteSubcmd::Update { .. } => "remote update",
            RemoteSubcmd::Connect { .. } => "remote connect",
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
            attach,
        } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            let remote =
                resolve_connect(&alias_or_target, reset, no_reconnect, globals.mux, &aliases)?;
            attach_remote(remote, attach.mode())
        }
        RemoteSubcmd::Reset {
            alias_or_target,
            no_reconnect,
            attach,
        } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            let remote =
                resolve_connect(&alias_or_target, true, no_reconnect, globals.mux, &aliases)?;
            attach_remote(remote, attach.mode())
        }
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

fn add_persistent_mux(globals: &GlobalFlags) -> Option<MuxName> {
    remote_writer_scopes_mux_flag(std::env::args_os())
        .then_some(globals.mux)
        .flatten()
}

fn remote_writer_scopes_mux_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut in_remote = false;
    let mut in_writer = false;
    let mut mux_scoped_to_remote = false;

    for arg in args.into_iter().skip(1) {
        let Some(arg) = arg.as_ref().to_str() else {
            continue;
        };
        if !in_remote {
            in_remote = arg == "remote";
            continue;
        }
        if arg == "--" {
            break;
        }
        if is_mux_flag(arg) {
            if in_writer {
                return true;
            }
            mux_scoped_to_remote = true;
            continue;
        }
        if !in_writer && matches!(arg, "add" | "update") {
            in_writer = true;
        }
    }

    in_writer && mux_scoped_to_remote
}

fn is_mux_flag(arg: &str) -> bool {
    arg == "--mux" || arg.starts_with("--mux=")
}

#[derive(Debug)]
struct RemoteConnect {
    target: RemoteTarget,
    reconnect: bool,
    no_resume: bool,
    mux: Option<MuxName>,
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
            target: RemoteTarget::parse(input)?,
            reconnect: !no_reconnect,
            no_resume: reset,
            mux: cli_mux,
        });
    }
    let Some(alias) = aliases.get(input) else {
        bail!("no such remote alias `{input}`; run `rimz remote list`");
    };
    Ok(RemoteConnect {
        target: RemoteTarget::parse(&alias.target)?,
        reconnect: alias.reconnect && !no_reconnect,
        no_resume: alias.no_resume || reset,
        mux: cli_mux.or(alias.mux),
    })
}

/// SSH remote attach: the local rimz is a launcher and link supervisor only.
/// Workspace resolution, session birth, the sidebar, and the health gate all
/// run on the remote host's own `rimz`; the room renders here over `ssh -t`.
fn attach_remote(remote: RemoteConnect, mode: super::AttachMode) -> Result<()> {
    let term = remote_term_plan();
    let plain_spec = ssh_attach_spec(&remote.target, remote.no_resume, remote.mux, &term);

    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print => {
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
            if remote.reconnect {
                let control = rimz::remote::link::validated_control_path()
                    .context("checking SSH ControlMaster socket path")?;
                let control_spec = ssh_attach_spec_with_control(
                    &remote.target,
                    remote.no_resume,
                    remote.mux,
                    &term,
                    Some(&control),
                );
                supervisor::supervise_remote(&control_spec, &plain_spec, &remote.target, &control)
            } else {
                supervisor::report_remote_connect(remote.target.host_display(), false);
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn alias(name: &str, target: &str, reconnect: bool, no_resume: bool) -> RemoteAlias {
        RemoteAlias {
            name: name.to_owned(),
            target: target.to_owned(),
            reconnect,
            no_resume,
            mux: None,
        }
    }

    #[test]
    fn remote_writer_scopes_mux_from_remote_or_writer_position() {
        assert!(!remote_writer_scopes_mux_flag(args(&[
            "rimz", "--mux", "tmux", "remote", "add", "name", "target",
        ])));
        assert!(remote_writer_scopes_mux_flag(args(&[
            "rimz", "remote", "--mux", "tmux", "add", "name", "target",
        ])));
        assert!(remote_writer_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "--mux", "tmux", "name", "target",
        ])));
        assert!(remote_writer_scopes_mux_flag(args(&[
            "rimz",
            "remote",
            "add",
            "name",
            "target",
            "--mux=tmux",
        ])));
        assert!(remote_writer_scopes_mux_flag(args(&[
            "rimz", "remote", "update", "--mux", "tmux", "name", "target",
        ])));
        assert!(!remote_writer_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "name", "target", "--", "--mux", "tmux",
        ])));
    }

    #[test]
    fn connect_resolution_applies_alias_reset_and_reconnect_policy() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false))
            .unwrap();
        aliases
            .add(alias("fresh", "fresh-box:query-engine", true, true))
            .unwrap();
        aliases
            .add(alias("default", "dev-box:query-engine", true, false))
            .unwrap();

        let raw = resolve_connect("raw-box:session", false, false, None, &aliases).unwrap();
        let raw_spec = ssh_attach_spec(&raw.target, raw.no_resume, raw.mux, &TermPlan::Keep);
        assert_eq!(raw_spec.args[10], "raw-box");

        let named = resolve_connect("prod", false, false, None, &aliases).unwrap();
        let named_spec =
            ssh_attach_spec(&named.target, named.no_resume, named.mux, &TermPlan::Keep);
        assert_eq!(named_spec.args[10], "prod-box");

        let fresh = resolve_connect("fresh", false, false, None, &aliases).unwrap();
        assert!(fresh.no_resume);

        let reset = resolve_connect("default", true, false, None, &aliases).unwrap();
        assert!(reset.no_resume);

        let remote = resolve_connect("prod", false, true, None, &aliases).unwrap();
        assert!(!remote.reconnect);
    }
}
