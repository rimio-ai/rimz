//! `rimz remote` — named SSH room aliases and remote attach.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;

use super::{AttachAction, AttachFlags, GlobalFlags, attach_action, exec_attach_command};
use rimz::ids::MuxName;
use rimz::remote::aliases::{RemoteAlias, RemoteAliases};
use rimz::remote::{RemoteTarget, ssh_attach_spec};

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
    #[command(name = "del", visible_alias = "rm")]
    Delete { name: String },
    /// Rename a saved remote alias.
    Rename { old: String, new: String },
    /// List saved remote aliases.
    #[clap(visible_alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
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
            aliases.add(RemoteAlias {
                name,
                target,
                reconnect: !no_reconnect,
                no_resume,
                mux: add_persistent_mux(globals),
            })?;
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
        RemoteSubcmd::Delete { name } => {
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
            print_list(aliases.entries(), json);
            Ok(())
        }
    }
}

fn add_persistent_mux(globals: &GlobalFlags) -> Option<MuxName> {
    remote_add_scopes_mux_flag(std::env::args_os())
        .then_some(globals.mux)
        .flatten()
}

fn remote_add_scopes_mux_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut in_remote = false;
    let mut in_add = false;
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
            if in_add {
                return true;
            }
            mux_scoped_to_remote = true;
            continue;
        }
        if !in_add && arg == "add" {
            in_add = true;
        }
    }

    in_add && mux_scoped_to_remote
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
    let spec = ssh_attach_spec(&remote.target, remote.no_resume, remote.mux);

    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print => {
            print_remote_command(&spec);
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
                supervise_remote(&spec, &remote.target)
            } else {
                report_remote_connect(remote.target.host_display(), false);
                exec_attach_command(&spec)
            }
        }
    }
}

/// Run ssh and keep the link alive, autossh-style: a clean detach exits, a
/// dropped link on an established session reconnects with capped backoff, and
/// anything else fails with the remote's own error. The remote mux session
/// survives the drop by design, so reattaching is idempotent.
fn supervise_remote(spec: &rimz::mux::CommandSpec, target: &RemoteTarget) -> Result<()> {
    use rimz::remote::{ReconnectPolicy, Verdict};

    let policy = ReconnectPolicy::from_env();
    let host = target.host_display();
    let mut established = false;
    let mut consecutive_failures: u32 = 0;
    report_remote_connect(host, true);
    loop {
        let started = std::time::Instant::now();
        let status = spec
            .to_command()
            .status()
            .with_context(|| format!("running `{}`", rimz::remote::display_ssh_command(spec)))?;
        if started.elapsed() >= policy.gatetime {
            established = true;
            consecutive_failures = 0;
        }
        match rimz::remote::verdict(status.code(), established, consecutive_failures, &policy) {
            Verdict::CleanExit => return Ok(()),
            Verdict::Fatal { code } => bail!(
                "ssh to {host} exited with status {code}; not reconnecting \
                 (only a dropped link on an established session is retried)"
            ),
            Verdict::Retry { delay } => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "rimz: link to {host} lost — reconnecting in {}s (attempt {consecutive_failures}); Ctrl-C stops",
                    delay.as_secs(),
                );
                drop(stderr);
                std::thread::sleep(delay);
            }
        }
    }
}

/// One stderr line before the terminal belongs to ssh, so the user knows the
/// room they are about to see is remote.
fn report_remote_connect(host: &str, reconnect: bool) {
    let mut stderr = std::io::stderr().lock();
    let tail = if reconnect {
        " (auto-reconnect on; Ctrl-C stops)"
    } else {
        ""
    };
    let _ = writeln!(stderr, "rimz: attaching to {host} over ssh…{tail}");
}

fn print_remote_command(spec: &rimz::mux::CommandSpec) {
    #[expect(clippy::print_stdout, reason = "user-facing command suggestion")]
    {
        println!("{}", rimz::remote::display_ssh_command(spec));
    }
}

#[derive(Serialize)]
struct ListEntryJson<'a> {
    name: &'a str,
    target: &'a str,
    reconnect: bool,
    no_resume: bool,
    mux: Option<&'a str>,
}

fn print_list(entries: &[RemoteAlias], json: bool) {
    if json {
        let rendered = render_list_json(entries);
        #[expect(clippy::print_stdout, reason = "json emitter")]
        {
            println!("{rendered}");
        }
        return;
    }
    let rendered = render_list_human(entries);
    if rendered.is_empty() {
        return;
    }
    #[expect(clippy::print_stdout, reason = "human listing")]
    {
        println!("{rendered}");
    }
}

fn render_list_json(entries: &[RemoteAlias]) -> String {
    let rows: Vec<ListEntryJson<'_>> = entries
        .iter()
        .map(|entry| ListEntryJson {
            name: &entry.name,
            target: &entry.target,
            reconnect: entry.reconnect,
            no_resume: entry.no_resume,
            mux: entry.mux.map(|mux| mux.as_str()),
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "remotes": rows })).expect("rendered JSON serializes")
}

fn render_list_human(entries: &[RemoteAlias]) -> String {
    let mut buf = String::new();
    for entry in entries {
        let reconnect = if entry.reconnect {
            "reconnect"
        } else {
            "no-reconnect"
        };
        let no_resume = if entry.no_resume {
            "no-resume"
        } else {
            "resume"
        };
        let mux = entry
            .mux
            .map(|mux| mux.as_str().to_owned())
            .unwrap_or_else(|| "-".to_owned());
        use std::fmt::Write as _;
        writeln!(
            buf,
            "{}\t{}\t{}\t{}\t{}",
            entry.name, entry.target, reconnect, no_resume, mux,
        )
        .expect("write to string");
    }
    buf.trim_end().to_owned()
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
    fn remote_add_scopes_mux_from_remote_or_add_position() {
        assert!(!remote_add_scopes_mux_flag(args(&[
            "rimz", "--mux", "tmux", "remote", "add", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "--mux", "tmux", "add", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "--mux", "tmux", "name", "target",
        ])));
        assert!(remote_add_scopes_mux_flag(args(&[
            "rimz",
            "remote",
            "add",
            "name",
            "target",
            "--mux=tmux",
        ])));
        assert!(!remote_add_scopes_mux_flag(args(&[
            "rimz", "remote", "add", "name", "target", "--", "--mux", "tmux",
        ])));
    }

    #[test]
    fn connect_disambiguates_raw_targets_from_aliases() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false))
            .unwrap();

        let raw = resolve_connect("raw-box:session", false, false, None, &aliases).unwrap();
        let raw_spec = ssh_attach_spec(&raw.target, raw.no_resume, raw.mux);
        assert_eq!(raw_spec.args[8], "raw-box");

        let named = resolve_connect("prod", false, false, None, &aliases).unwrap();
        let named_spec = ssh_attach_spec(&named.target, named.no_resume, named.mux);
        assert_eq!(named_spec.args[8], "prod-box");
    }

    #[test]
    fn reset_and_alias_no_resume_force_no_resume() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("fresh", "prod-box:query-engine", true, true))
            .unwrap();
        aliases
            .add(alias("default", "dev-box:query-engine", true, false))
            .unwrap();

        let fresh = resolve_connect("fresh", false, false, None, &aliases).unwrap();
        assert!(fresh.no_resume);

        let reset = resolve_connect("default", true, false, None, &aliases).unwrap();
        assert!(reset.no_resume);
    }

    #[test]
    fn no_reconnect_overrides_alias_default() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false))
            .unwrap();
        let remote = resolve_connect("prod", false, true, None, &aliases).unwrap();
        assert!(!remote.reconnect);
    }

    #[test]
    fn list_json_emits_canonical_shape() {
        let entries = vec![
            RemoteAlias {
                name: "dev".to_owned(),
                target: "dev-box:query-engine".to_owned(),
                reconnect: true,
                no_resume: false,
                mux: None,
            },
            RemoteAlias {
                name: "prod".to_owned(),
                target: "agent@prod-box:~/code/query-engine".to_owned(),
                reconnect: false,
                no_resume: true,
                mux: Some(MuxName::Tmux),
            },
        ];
        insta::assert_snapshot!(render_list_json(&entries), @r#"
        {
          "remotes": [
            {
              "mux": null,
              "name": "dev",
              "no_resume": false,
              "reconnect": true,
              "target": "dev-box:query-engine"
            },
            {
              "mux": "tmux",
              "name": "prod",
              "no_resume": true,
              "reconnect": false,
              "target": "agent@prod-box:~/code/query-engine"
            }
          ]
        }
        "#);
    }

    #[test]
    fn list_human_emits_tab_separated_rows() {
        let entries = vec![RemoteAlias {
            name: "prod".to_owned(),
            target: "prod-box:query-engine".to_owned(),
            reconnect: true,
            no_resume: false,
            mux: Some(MuxName::Zellij),
        }];
        insta::assert_snapshot!(
            render_list_human(&entries),
            @"prod	prod-box:query-engine	reconnect	resume	zellij"
        );
    }
}
