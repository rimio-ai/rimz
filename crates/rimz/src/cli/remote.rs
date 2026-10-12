//! `rimz remote` — named SSH room aliases and remote attach.

use std::io::{IsTerminal, Write};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use super::{AttachFlags, GlobalFlags};
use crate::cli::room::{
    AttachAction, AttachMode, PrintCause, attach_action, launch_attach_command,
};
use rimz::ids::MuxName;
use rimz::remote::aliases::{RemoteAlias, RemoteAliases};
use rimz::remote::{
    RemoteTarget, RemoteTargetError, SshAttachOptions, SshAttachPlan, SshDestination, TermPlan,
    infocmp_program, remote_lineage, term_plan_from,
};

mod link_stats;
mod list;
mod outage_ui;
mod setup;
mod supervisor;
mod tty;
mod web;

#[derive(Debug, Args)]
pub struct RemoteArgs {
    #[command(subcommand)]
    command: RemoteSubcmd,
}

#[derive(Debug, Subcommand)]
enum RemoteSubcmd {
    /// Save a named remote target.
    #[command(
        after_help = "With `remote add`, --mux <name> pins the saved alias. --no-auto-forward disables listener forwarding for it."
    )]
    Add {
        /// Name to save for this remote target.
        name: String,
        #[command(flatten)]
        args: SavedAliasArgs,
    },
    /// Change a saved remote target or settings, keeping omitted values.
    #[command(
        after_help = "--mux <name> pins the saved alias; --no-mux clears the pin. Omitted settings stay unchanged."
    )]
    Update {
        /// Saved alias whose target or settings to change.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        name: String,
        #[command(flatten)]
        args: UpdateAliasArgs,
    },
    /// Connect to a remote alias or raw `[user@]host:<session-or-path>` target.
    Connect {
        #[command(flatten)]
        args: ConnectionArgs,
    },
    /// Install rimz on a remote alias, `[user@]host:<session-or-path>` target, or `[user@]host`.
    Setup {
        /// Saved alias, `[user@]host:<session-or-path>` target, or `[user@]host` to install on.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        alias_or_host: String,
        /// Install the latest release instead of matching this client's release.
        #[arg(long)]
        latest: bool,
    },
    /// Delete a saved remote alias.
    Rm {
        /// Saved alias to remove from this machine.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        name: String,
    },
    /// Rename a saved remote alias.
    Rename {
        /// Existing saved alias name.
        #[arg(add = clap_complete::ArgValueCandidates::new(
            crate::cli::complete::remote_aliases
        ))]
        old: String,
        /// New name for the saved alias.
        new: String,
    },
    /// List saved remote aliases.
    #[clap(visible_alias = "ls")]
    List {
        /// Print saved aliases and their settings as JSON.
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

#[derive(Debug, Args)]
struct SavedAliasArgs {
    /// `[user@]host:<session-or-path>`; use `host:session:<name>` to force a session.
    target: String,
    #[command(flatten)]
    settings: SettingFlags,
}

#[derive(Debug, Args)]
struct UpdateAliasArgs {
    /// Replacement `[user@]host:<session-or-path>`; omit to keep the saved target.
    target: Option<String>,
    #[command(flatten)]
    settings: SettingFlags,
    /// Clear the saved backend pin.
    #[arg(long, conflicts_with_all = ["mux", "tmux", "zellij"])]
    no_mux: bool,
}

#[derive(Debug, Default, Args)]
struct SettingFlags {
    /// Supervise the link and reconnect after drops.
    #[arg(long, overrides_with = "no_reconnect")]
    reconnect: bool,
    /// Hand the link to a single ssh run instead of supervising reconnects.
    #[arg(long, overrides_with = "reconnect")]
    no_reconnect: bool,
    /// Recover prior agents when the remote room is born.
    #[arg(long, overrides_with = "no_resume")]
    resume: bool,
    /// A newly born room has no prior agents; a running room's parked agents are ended. The host reports either count.
    #[arg(long, overrides_with = "resume")]
    no_resume: bool,
    /// Automatically forward new remote listeners.
    #[arg(long, overrides_with = "no_auto_forward")]
    auto_forward: bool,
    /// Disable automatic forwarding of new remote listeners.
    #[arg(long, overrides_with = "auto_forward")]
    no_auto_forward: bool,
}

impl SettingFlags {
    fn apply(&self, entry: &mut RemoteAlias) {
        for (value, positive, negative) in [
            (&mut entry.resume, self.resume, self.no_resume),
            (&mut entry.reconnect, self.reconnect, self.no_reconnect),
            (
                &mut entry.auto_forward,
                self.auto_forward,
                self.no_auto_forward,
            ),
        ] {
            if let Some(setting) = positive.then_some(true).or(negative.then_some(false)) {
                *value = setting;
            }
        }
    }
}

impl SavedAliasArgs {
    fn into_alias(self, name: String, mux: Option<MuxName>) -> RemoteAlias {
        let mut entry = RemoteAlias {
            name,
            target: self.target,
            reconnect: true,
            resume: true,
            mux,
            auto_forward: true,
        };
        self.settings.apply(&mut entry);
        entry
    }
}

fn resolve_update(entry: &RemoteAlias, args: UpdateAliasArgs, mux: Option<MuxName>) -> RemoteAlias {
    let mut entry = entry.clone();
    if let Some(target) = args.target {
        entry.target = target;
    }
    args.settings.apply(&mut entry);
    entry.mux = if args.no_mux { None } else { mux.or(entry.mux) };
    entry
}

fn unknown_alias_message(input: &str, aliases: &RemoteAliases) -> String {
    let names: Vec<_> = aliases
        .entries()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    let suggestion = rimz::utils::suggest::nearest_name(input, names.iter().copied())
        .map(|name| format!("; did you mean `{name}`?"))
        .unwrap_or_default();
    let saved = if names.is_empty() {
        "none saved; save one with: rimz remote add <name> [user@]host:<session-or-path>".to_owned()
    } else {
        let mut saved = format!(
            "saved: {}",
            names.iter().take(8).copied().collect::<Vec<_>>().join(", ")
        );
        if names.len() > 8 {
            saved.push_str(&format!(", … and {} more", names.len() - 8));
        }
        saved
    };
    format!("no such remote alias `{input}`{suggestion}\n{saved}")
}

fn alias_receipt(verb: &str, entry: &RemoteAlias) -> String {
    let settings = list::settings(entry);
    let suffix = if settings.is_empty() {
        String::new()
    } else {
        format!(" ({settings})")
    };
    format!("{verb} {} -> {}{suffix}", entry.name, entry.target)
}

#[derive(Debug, Args)]
#[command(
    mut_arg("no_attach", |arg| arg.help("Print the SSH command instead of connecting; opens nothing")),
    mut_arg("attach", |arg| arg.help("Connect to the remote room instead of only printing the SSH command"))
)]
struct ConnectionArgs {
    /// Alias or `[user@]host:<session-or-path>`; bare names prefer remote home directories, `host:session:<name>` forces a session.
    #[arg(add = clap_complete::ArgValueCandidates::new(
        crate::cli::complete::remote_aliases
    ))]
    alias_or_target: String,
    #[command(flatten)]
    settings: SettingFlags,
    /// Attach despite a minor version mismatch with the host.
    #[arg(long)]
    force_version: bool,
    /// Open the remote room in the local browser through an SSH tunnel.
    #[arg(long)]
    web: bool,
    /// Local browser port for `--web`; by default derived from the session name in 8300..8399.
    #[arg(long, requires = "web")]
    web_port: Option<u16>,
    #[command(flatten)]
    attach: AttachFlags,
}

impl RemoteArgs {
    /// The low-cardinality command label for the Sentry command scope.
    pub(crate) fn command_label(&self) -> &'static str {
        match &self.command {
            RemoteSubcmd::Add { .. } => "remote add",
            RemoteSubcmd::Update { .. } => "remote update",
            RemoteSubcmd::Connect { .. } => "remote connect",
            RemoteSubcmd::Setup { .. } => "remote setup",
            RemoteSubcmd::Rm { .. } => "remote rm",
            RemoteSubcmd::Rename { .. } => "remote rename",
            RemoteSubcmd::List { .. } => "remote list",
            RemoteSubcmd::LinkStats { .. } => "remote link-stats",
        }
    }
}

pub fn run(args: RemoteArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        RemoteSubcmd::Add { name, args } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            let entry = args.into_alias(name, globals.mux);
            let receipt = format!(
                "{}; connect: rimz remote connect {}",
                alias_receipt("saved", &entry),
                entry.name
            );
            if aliases.contains(&entry.name) && !std::io::stdin().is_terminal() {
                bail!(
                    "remote alias `{0}` already exists; replace it with `rimz remote update {0} <target>`",
                    entry.name
                );
            }
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
            writeln!(super::render::out(), "{receipt}")?;
            Ok(())
        }
        RemoteSubcmd::Update { name, args } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            let entry = aliases
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!(unknown_alias_message(&name, &aliases)))?;
            let entry = resolve_update(entry, args, globals.mux);
            let receipt = alias_receipt("updated", &entry);
            aliases.update(entry)?;
            aliases.save().context("saving remote aliases")?;
            writeln!(super::render::out(), "{receipt}")?;
            Ok(())
        }
        RemoteSubcmd::Connect { args } => connect(args, globals),
        RemoteSubcmd::Setup {
            alias_or_host,
            latest,
        } => setup::run(alias_or_host, latest, globals),
        RemoteSubcmd::Rm { name } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            let target = aliases.get(&name).map(|entry| entry.target.clone());
            aliases.remove(&name)?;
            aliases.save().context("saving remote aliases")?;
            // Successful removal proves the entry existed.
            writeln!(
                super::render::out(),
                "removed {name} (was {})",
                target.expect("removed entry existed")
            )?;
            Ok(())
        }
        RemoteSubcmd::Rename { old, new } => {
            let mut aliases = RemoteAliases::load().context("loading remote aliases")?;
            aliases.rename(&old, new.clone())?;
            aliases.save().context("saving remote aliases")?;
            writeln!(super::render::out(), "renamed {old} -> {new}")?;
            Ok(())
        }
        RemoteSubcmd::List { json } => {
            let aliases = RemoteAliases::load().context("loading remote aliases")?;
            list::print(aliases.entries(), json)?;
            Ok(())
        }
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
    resume: bool,
    force_version: bool,
    mux: Option<MuxName>,
    web: web::RemoteWebOptions,
    auto_forward: bool,
}

impl RemoteConnect {
    fn attach_plan(&self, client_size: Option<(u16, u16)>) -> Result<SshAttachPlan> {
        Ok(SshAttachPlan::new(SshAttachOptions {
            target: self.target.clone(),
            lineage: local_remote_lineage(&self.target)?,
            supervised: self.reconnect,
            force_version: self.force_version,
            resume: self.resume,
            mux: self.mux,
            term: remote_term_plan(),
            truecolor: rimz::tui::truecolor(),
            client_size,
        }))
    }
}

fn resolve_connect(
    input: &str,
    settings: &SettingFlags,
    cli_mux: Option<MuxName>,
    aliases: &RemoteAliases,
) -> Result<RemoteConnect> {
    let mut entry = if input.contains(':') {
        SavedAliasArgs {
            target: input.to_owned(),
            settings: SettingFlags::default(),
        }
        .into_alias(input.to_owned(), None)
    } else {
        aliases.get(input).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "{}\nor connect straight to a host: rimz remote connect {input}:<session-or-path>",
                unknown_alias_message(input, aliases)
            )
        })?
    };
    settings.apply(&mut entry);
    Ok(RemoteConnect {
        origin: input.to_owned(),
        target: RemoteTarget::parse(&entry.target)?,
        reconnect: entry.reconnect,
        resume: entry.resume,
        force_version: false,
        mux: cli_mux.or(entry.mux),
        web: web::RemoteWebOptions::default(),
        auto_forward: entry.auto_forward,
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

fn connect(args: ConnectionArgs, globals: &GlobalFlags) -> Result<()> {
    let ConnectionArgs {
        alias_or_target,
        settings,
        force_version,
        web,
        web_port,
        attach,
    } = args;
    let aliases = RemoteAliases::load().context("loading remote aliases")?;
    let mut remote = resolve_connect(&alias_or_target, &settings, globals.mux, &aliases)?;
    remote.force_version = force_version;
    remote.web = web::RemoteWebOptions {
        enabled: web,
        port: web_port,
    };
    attach_remote(remote, attach.mode())
}

/// SSH remote attach: the local rimz is a launcher and link supervisor only.
/// Workspace resolution, session birth, the sidebar, and the health gate all
/// run on the remote host's own `rimz`; terminal mode renders here over
/// `ssh -t`, and web mode opens a supervised local-forward tunnel.
fn attach_remote(remote: RemoteConnect, mode: AttachMode) -> Result<()> {
    // The local nesting block does not apply: a remote room inside a local
    // pane is a legitimate shape (the remote rimz checks its own env).
    let client_size = rimz::mux::detect_terminal_size();
    match attach_action(
        mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        false,
    ) {
        AttachAction::Print(cause) => {
            if remote.web.enabled {
                if cause == PrintCause::Explicit {
                    bail!(
                        "--web opens a browser tunnel and has no ssh command to print; drop --print"
                    );
                }
                bail!(
                    "--web needs a terminal to run the tunnel and has no ssh command to print; pass --attach to run it anyway"
                );
            }
            let remote = RemoteConnect {
                reconnect: false,
                ..remote
            };
            let plan = remote.attach_plan(client_size)?;
            let plain_spec = plan.initial().plain();
            if cause == PrintCause::NoTerminal {
                writeln!(
                    crate::cli::render::err(),
                    "rimz: no terminal here, so nothing was connected; printed the ssh command instead (same as --print). Pass --attach to connect anyway."
                )?;
            }
            supervisor::print_remote_command(&plain_spec);
            Ok(())
        }
        AttachAction::Launch => {
            let program = rimz::remote::ssh_program();
            which::which(&program).map_err(|_| {
                anyhow::anyhow!(
                    "`{program}` is not on PATH; install an OpenSSH client to attach \
                     remotely, or run with --print to emit the command"
                )
            })?;
            if remote.web.enabled {
                return web::run_remote_web(&remote, client_size);
            }
            let plan = remote.attach_plan(client_size)?;
            if remote.reconnect {
                let control = rimz::remote::link::validated_control_path()
                    .context("checking SSH ControlMaster socket path")?;
                supervisor::supervise_remote(
                    plan,
                    control,
                    remote.origin.as_str(),
                    remote.auto_forward,
                )
            } else {
                // The local process owns the outer terminal bracket for a
                // one-shot SSH attach. Tell a current remote RimZ not to nest
                // XTSAVE/XTRESTORE, whose saved mode slot is not a stack.
                // The environment marker stays compatible with older remotes.
                let plain_spec = plan.initial().with_outer_scroll_bracket(true).plain();
                tty::sanitize_local_tty();
                launch_attach_command(&plain_spec)
            }
        }
    }
}

fn local_remote_lineage(target: &RemoteTarget) -> Result<String> {
    let hostname = nix::unistd::gethostname()
        .context("reading the local hostname for remote reconnect identity")?
        .to_string_lossy()
        .into_owned();
    if hostname.is_empty() {
        bail!("the local hostname is empty; cannot derive remote reconnect identity");
    }
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .context("reading the local user for remote reconnect identity")?
        .context("the local user has no passwd entry for remote reconnect identity")?;
    Ok(remote_lineage(target, &hostname, &user.name))
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
    use super::*;

    #[test]
    fn help_describes_every_positional_and_json_output() {
        use clap::CommandFactory;
        let cli = crate::cli::Cli::command();
        let remote = cli.find_subcommand("remote").unwrap();
        for command in remote
            .get_subcommands()
            .filter(|command| !command.is_hide_set())
        {
            for arg in command
                .get_arguments()
                .filter(|arg| arg.is_positional() || arg.get_id() == "json")
            {
                assert!(
                    arg.get_help()
                        .is_some_and(|help| !help.to_string().is_empty()),
                    "remote {} {} has no help",
                    command.get_name(),
                    arg.get_id()
                );
            }
        }
    }

    #[test]
    fn settings_pairs_and_update_surface() {
        use clap::Parser;
        for verb in ["add", "update", "connect"] {
            for pair in [
                ["--resume", "--no-resume"],
                ["--reconnect", "--no-reconnect"],
                ["--auto-forward", "--no-auto-forward"],
            ] {
                for flags in [pair, [pair[1], pair[0]]] {
                    let mut argv = vec!["rimz", "remote", verb, "dev"];
                    if verb == "add" {
                        argv.push("dev-box:room");
                    }
                    argv.extend(flags);
                    assert!(crate::cli::Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
                }
            }
        }
        assert!(crate::cli::Cli::try_parse_from(["rimz", "remote", "reset", "dev"]).is_err());
        assert!(
            crate::cli::Cli::try_parse_from(["rimz", "remote", "connect", "dev", "--reset"])
                .is_err()
        );
    }

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
            resume: !no_resume,
            mux,
            auto_forward: true,
        }
    }

    fn parsed_settings(flags: &[&str]) -> SettingFlags {
        use clap::Parser;
        let argv = ["rimz", "remote", "connect", "dev"]
            .into_iter()
            .chain(flags.iter().copied());
        let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
        let Some(crate::cli::Subcmd::Remote(RemoteArgs {
            command: RemoteSubcmd::Connect { args },
        })) = cli.subcommand
        else {
            panic!("connect");
        };
        args.settings
    }

    #[test]
    fn connect_resolution_applies_alias_cli_overrides() {
        for saved in [false, true] {
            let mut aliases = RemoteAliases::default();
            let mut entry = alias("dev", "dev-box:room", saved, !saved, Some(MuxName::Tmux));
            entry.auto_forward = saved;
            aliases.add(entry).unwrap();
            for (positive, negative, index) in [
                ("--resume", "--no-resume", 0),
                ("--reconnect", "--no-reconnect", 1),
                ("--auto-forward", "--no-auto-forward", 2),
            ] {
                for (flags, expected) in [
                    (vec![], saved),
                    (vec![positive], true),
                    (vec![negative], false),
                    (vec![positive, negative], false),
                    (vec![negative, positive], true),
                ] {
                    let settings = parsed_settings(&flags);
                    let resolved = resolve_connect("dev", &settings, None, &aliases).unwrap();
                    assert_eq!(
                        [resolved.resume, resolved.reconnect, resolved.auto_forward][index],
                        expected,
                        "{flags:?}, saved={saved}"
                    );
                    assert_eq!(resolved.mux, Some(MuxName::Tmux));
                    let raw =
                        resolve_connect("raw:room", &settings, Some(MuxName::Zellij), &aliases)
                            .unwrap();
                    assert_eq!(
                        [raw.resume, raw.reconnect, raw.auto_forward][index],
                        if flags.is_empty() { true } else { expected }
                    );
                    assert_eq!(raw.mux, Some(MuxName::Zellij));
                }
            }
        }
    }

    #[test]
    fn update_merges_only_explicit_values() {
        let original = alias("dev", "dev-box:room", false, true, Some(MuxName::Tmux));
        let args = |target, no_mux| UpdateAliasArgs {
            target,
            no_mux,
            settings: SettingFlags::default(),
        };
        assert_eq!(resolve_update(&original, args(None, false), None), original);
        let changed = resolve_update(&original, args(Some("other:room".to_owned()), false), None);
        assert_eq!(changed.target, "other:room");
        assert_eq!(changed.resume, original.resume);
        assert_eq!(changed.reconnect, original.reconnect);
        assert_eq!(changed.mux, original.mux);
        assert_eq!(resolve_update(&original, args(None, true), None).mux, None);
        assert_eq!(
            resolve_update(&original, args(None, false), Some(MuxName::Zellij)).mux,
            Some(MuxName::Zellij)
        );
        let mut overrides = args(None, false);
        overrides.settings = parsed_settings(&["--resume", "--reconnect", "--no-auto-forward"]);
        let changed = resolve_update(&original, overrides, None);
        assert!(changed.resume && changed.reconnect && !changed.auto_forward);
    }

    #[test]
    fn unknown_alias_lists_suggestions_and_bounded_saved_names() {
        let mut aliases = RemoteAliases::default();
        assert_eq!(
            unknown_alias_message("dve", &aliases),
            "no such remote alias `dve`\nnone saved; save one with: rimz remote add <name> [user@]host:<session-or-path>"
        );
        for name in ["prod", "dev"] {
            aliases
                .add(alias(name, "box:room", true, false, None))
                .unwrap();
        }
        assert_eq!(
            unknown_alias_message("dve", &aliases),
            "no such remote alias `dve`; did you mean `dev`?\nsaved: dev, prod"
        );
        assert!(!unknown_alias_message("unrelated", &aliases).contains("did you mean"));
        for name in ["a", "b", "c", "d", "e", "f", "g"] {
            aliases
                .add(alias(name, "box:room", true, false, None))
                .unwrap();
        }
        assert!(
            unknown_alias_message("unrelated", &aliases)
                .contains("saved: a, b, c, d, dev, e, f, g, … and 1 more")
        );
    }

    #[test]
    fn setup_resolution_accepts_alias_target_and_bare_host() {
        let mut aliases = RemoteAliases::default();
        aliases
            .add(alias("prod", "prod-box:query-engine", true, false, None))
            .unwrap();

        let named = resolve_setup_destination("prod", &aliases).unwrap();
        assert_eq!(named.as_str(), "prod-box");
        assert_eq!(named.host_display(), "prod-box");

        let target =
            resolve_setup_destination("agent@prod-box:/srv/query-engine", &aliases).unwrap();
        assert_eq!(target.as_str(), "agent@prod-box");
        assert_eq!(target.host_display(), "prod-box");

        let bare = resolve_setup_destination("alice@new-box", &aliases).unwrap();
        assert_eq!(bare.as_str(), "alice@new-box");
        assert_eq!(bare.host_display(), "new-box");

        let ipv6 = resolve_setup_destination("user@[::1]", &aliases).unwrap();
        assert_eq!(ipv6.as_str(), "user@[::1]");
        assert_eq!(ipv6.host_display(), "::1");
    }
}
