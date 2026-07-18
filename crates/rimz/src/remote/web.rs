//! SSH argv builders for remote browser access.
//!
//! The CLI owns process I/O and supervision. This module keeps shell quoting
//! and argv construction testable beside the existing remote attach builders.

use crate::mux::CommandSpec;
use crate::web::WebEngine;

use super::{
    REMOTE_CLIENT_VERSION_ENV, REMOTE_FORCE_VERSION_ENV, RemoteSpec, RemoteTarget,
    client_size_env_setup, quote_remote_path, remote_exec_snippet, sh_quote, ssh_program,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WebPrepOptions {
    pub confirm_resume: bool,
    pub no_resume: bool,
    pub force_version: bool,
    pub client_size: Option<(u16, u16)>,
}

pub fn web_prep_spec(target: &RemoteTarget, options: WebPrepOptions) -> CommandSpec {
    let mut flags = String::from("open --print --json");
    if options.confirm_resume {
        flags.push_str(" --confirm-resume");
    }
    if options.no_resume {
        flags.push_str(" --no-resume");
    }
    let rimz_args = match &target.spec {
        RemoteSpec::Path(path) => format!("{flags} -- {}", quote_remote_path(path)),
        RemoteSpec::Session(name) => {
            format!("{flags} --session {}", sh_quote(name))
        }
    };
    one_shot_spec(
        target,
        &format!("rimz web {rimz_args}"),
        options.client_size,
        options.force_version,
    )
}

pub fn web_token_ensure_spec(target: &RemoteTarget, engine: WebEngine) -> CommandSpec {
    let mux = match engine {
        WebEngine::Zellij => "zellij",
        WebEngine::Ttyd => "tmux",
    };
    one_shot_spec(
        target,
        &format!("rimz --mux {mux} web token ensure"),
        None,
        false,
    )
}

pub fn web_tunnel_spec(target: &RemoteTarget, local_port: u16, remote_port: u16) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args([
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "Compression=yes",
            "-L",
        ])
        .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
        .args(["--", target.ssh_destination().as_str()])
}

fn one_shot_spec(
    target: &RemoteTarget,
    rimz: &str,
    client_size: Option<(u16, u16)>,
    force_version: bool,
) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args(["-o", "ConnectTimeout=10", "--"])
        .arg(target.ssh_destination().as_str())
        .arg(web_snippet(target, rimz, client_size, force_version))
}

fn web_snippet(
    target: &RemoteTarget,
    rimz: &str,
    client_size: Option<(u16, u16)>,
    force_version: bool,
) -> String {
    let mut env_setup = format!(
        "export TERM=xterm-256color; export COLORTERM=truecolor; export {REMOTE_CLIENT_VERSION_ENV}={}; ",
        sh_quote(crate::build_id::VERSION),
    );
    if force_version {
        env_setup.push_str(&format!("export {REMOTE_FORCE_VERSION_ENV}=1; "));
    }
    env_setup.push_str(&client_size_env_setup(client_size));
    remote_exec_snippet(target.host_display(), &env_setup, rimz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> RemoteTarget {
        RemoteTarget::parse(input).expect("target parses")
    }

    #[test]
    fn web_prep_builds_session_and_path_one_shots() {
        let session = web_prep_spec(
            &parse("dev-box:rimz-project-a1b2c3"),
            WebPrepOptions {
                confirm_resume: true,
                no_resume: true,
                force_version: true,
                client_size: Some((180, 50)),
            },
        );
        assert_eq!(session.args[0..3], ["-o", "ConnectTimeout=10", "--"]);
        assert_eq!(session.args[3], "dev-box");
        assert!(
            session.args[4]
                .contains("exec rimz web open --print --json --confirm-resume --no-resume --session 'rimz-project-a1b2c3'"),
            "{}",
            session.args[4]
        );
        assert!(
            session.args[4].contains(
                "export TERM=xterm-256color; export COLORTERM=truecolor; export RIMZ_REMOTE_CLIENT_VERSION="
            ),
            "{}",
            session.args[4]
        );
        assert!(
            session.args[4].contains("export RIMZ_CLIENT_SIZE=180x50; exec rimz web open"),
            "{}",
            session.args[4]
        );
        assert!(
            session.args[4].contains("export RIMZ_REMOTE_FORCE_VERSION=1;"),
            "{}",
            session.args[4]
        );

        let path = web_prep_spec(
            &parse("dev-box:~/code/query-engine"),
            WebPrepOptions::default(),
        );
        assert!(
            path.args[4]
                .contains("exec rimz web open --print --json -- \"$HOME\"'/code/query-engine'"),
            "{}",
            path.args[4]
        );
        assert!(
            !path.args[4].contains(REMOTE_FORCE_VERSION_ENV),
            "force stays opt-in: {}",
            path.args[4]
        );
    }

    #[test]
    fn web_tunnel_builds_local_forward() {
        let spec = web_tunnel_spec(&parse("dev-box:query-engine"), 8301, 8082);
        assert_eq!(
            spec.args,
            [
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "Compression=yes",
                "-L",
                "127.0.0.1:8301:127.0.0.1:8082",
                "--",
                "dev-box"
            ]
        );
    }

    #[test]
    fn web_token_ensure_builds_one_shot() {
        let spec = web_token_ensure_spec(&parse("dev-box:query-engine"), WebEngine::Ttyd);
        assert!(
            spec.args[4].ends_with("exec rimz --mux tmux web token ensure"),
            "{}",
            spec.args[4]
        );
        assert!(
            !spec.args[4].contains(crate::mux::CLIENT_SIZE_ENV),
            "the token one-shot does not birth a room: {}",
            spec.args[4],
        );
    }
}
