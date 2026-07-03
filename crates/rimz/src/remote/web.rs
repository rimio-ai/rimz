//! SSH argv builders for remote Zellij web access.
//!
//! The CLI owns process I/O and supervision. This module keeps shell quoting
//! and argv construction testable beside the existing remote attach builders.

use crate::mux::CommandSpec;

use super::{
    REMOTE_RIMZ_MISSING_EXIT, RemoteSpec, RemoteTarget, quote_remote_path, remote_path_prefix,
    sh_quote, ssh_program,
};

pub fn web_prep_spec(target: &RemoteTarget) -> CommandSpec {
    let rimz_args = match &target.spec {
        RemoteSpec::Path(path) => format!("open --print --json -- {}", quote_remote_path(path)),
        RemoteSpec::Session(name) => {
            format!("open --print --json --session {}", sh_quote(name))
        }
    };
    one_shot_spec(target, &format!("rimz web {rimz_args}"))
}

pub fn web_token_create_spec(target: &RemoteTarget) -> CommandSpec {
    one_shot_spec(target, "rimz web token create")
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
        .args(["--".to_owned(), target.destination.clone()])
}

fn one_shot_spec(target: &RemoteTarget, rimz: &str) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args(["-o", "ConnectTimeout=10", "--"])
        .arg(target.destination.clone())
        .arg(web_snippet(target, rimz))
}

fn web_snippet(target: &RemoteTarget, rimz: &str) -> String {
    let not_found = sh_quote(&format!(
        "rimz not found on {} — install: cargo install rimz",
        target.host_display(),
    ));
    format!(
        "{}; \
         command -v rimz >/dev/null 2>&1 || {{ echo {not_found} >&2; exit {code}; }}; \
         exec {rimz}",
        remote_path_prefix(),
        code = REMOTE_RIMZ_MISSING_EXIT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> RemoteTarget {
        RemoteTarget::parse(input).expect("target parses")
    }

    #[test]
    fn web_prep_builds_session_and_path_one_shots() {
        let session = web_prep_spec(&parse("dev-box:rimz-project-a1b2c3"));
        assert_eq!(session.args[0..3], ["-o", "ConnectTimeout=10", "--"]);
        assert_eq!(session.args[3], "dev-box");
        assert!(
            session.args[4]
                .contains("exec rimz web open --print --json --session 'rimz-project-a1b2c3'"),
            "{}",
            session.args[4]
        );

        let path = web_prep_spec(&parse("dev-box:~/code/query-engine"));
        assert!(
            path.args[4]
                .contains("exec rimz web open --print --json -- \"$HOME\"'/code/query-engine'"),
            "{}",
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
}
