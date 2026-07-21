//! SSH argv builders for remote browser access.
//!
//! The CLI owns process I/O and supervision. This module keeps shell quoting
//! and argv construction testable beside the existing remote attach builders.

use std::io;
use std::net::TcpListener;
use std::ops::RangeInclusive;
use std::path::Path;

use crate::mux::CommandSpec;
use crate::web::WebOpenPayload;

use super::{
    REMOTE_CLIENT_VERSION_ENV, REMOTE_FORCE_VERSION_ENV, RemoteSpec, RemoteTarget,
    client_size_env_setup, quote_remote_path, remote_exec_snippet, sh_quote, ssh_program,
};

const LOCAL_PORT_RANGE: RangeInclusive<u16> = 8300..=8399;

pub fn choose_local_port(session: &str, override_port: Option<u16>) -> io::Result<u16> {
    if let Some(port) = override_port {
        probe_local_port(port)?;
        return Ok(port);
    }
    let preferred = derive_port(session, &LOCAL_PORT_RANGE);
    for port in port_scan(preferred, &LOCAL_PORT_RANGE) {
        if probe_local_port(port).is_ok() {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "no free local web tunnel port in 8300..8399",
    ))
}

pub fn local_url(remote: &WebOpenPayload, local_port: u16) -> String {
    format!(
        "http://127.0.0.1:{local_port}/?arg={}",
        encode_query_value(&remote.session)
    )
}

fn derive_port(session: &str, range: &RangeInclusive<u16>) -> u16 {
    let span = u32::from(*range.end()) - u32::from(*range.start()) + 1;
    let offset = crc32fast::hash(session.as_bytes()) % span;
    // CRC modulo span bounds the offset to the inclusive u16 port range.
    *range.start() + u16::try_from(offset).expect("CRC offset fits in port range")
}

fn port_scan(preferred: u16, range: &RangeInclusive<u16>) -> impl Iterator<Item = u16> + use<> {
    let start = *range.start();
    let end = *range.end();
    (preferred..=end).chain(start..preferred)
}

fn probe_local_port(port: u16) -> io::Result<()> {
    TcpListener::bind(("127.0.0.1", port)).map(|_| ())
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WebPrepOptions {
    pub confirm_resume: bool,
    pub no_resume: bool,
    pub force_version: bool,
    pub client_size: Option<(u16, u16)>,
}

pub fn web_prep_spec(
    target: &RemoteTarget,
    options: WebPrepOptions,
    control: Option<&Path>,
) -> CommandSpec {
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
        control,
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
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ControlPersist=no",
            "-L",
        ])
        .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
        .args(["--", target.ssh_destination().as_str()])
}

/// Add the browser forward to an established ControlMaster. The control
/// command exits after the master confirms the listener; the master owns the
/// forward for the rest of its lifetime.
pub fn web_control_forward_spec(
    target: &RemoteTarget,
    local_port: u16,
    remote_port: u16,
    control: &Path,
) -> CommandSpec {
    CommandSpec::new(ssh_program()).args([
        "-S".to_owned(),
        control.display().to_string(),
        "-O".to_owned(),
        "forward".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-L".to_owned(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "--".to_owned(),
        target.ssh_destination().as_str().to_owned(),
    ])
}

fn one_shot_spec(
    target: &RemoteTarget,
    rimz: &str,
    client_size: Option<(u16, u16)>,
    force_version: bool,
    control: Option<&Path>,
) -> CommandSpec {
    CommandSpec::new(ssh_program())
        .args(["-o", "ConnectTimeout=10"])
        .args(control.into_iter().flat_map(super::link::control_options))
        .args(["--"])
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
    fn local_port_derivation_is_stable_and_in_range() {
        let first = derive_port("rimz-project-a1b2c3", &LOCAL_PORT_RANGE);
        assert_eq!(first, derive_port("rimz-project-a1b2c3", &LOCAL_PORT_RANGE));
        assert!(LOCAL_PORT_RANGE.contains(&first));
    }

    #[test]
    fn local_port_scan_wraps_at_the_range_end() {
        assert_eq!(
            port_scan(8398, &LOCAL_PORT_RANGE)
                .take(4)
                .collect::<Vec<_>>(),
            [8398, 8399, 8300, 8301]
        );
    }

    #[test]
    fn explicit_local_port_collision_fails() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture");
        let port = listener.local_addr().expect("fixture address").port();
        assert_eq!(
            choose_local_port("rimz-project-a1b2c3", Some(port))
                .expect_err("occupied explicit port")
                .kind(),
            io::ErrorKind::AddrInUse
        );
    }

    #[test]
    fn local_url_percent_encodes_the_ttyd_session_argument() {
        let payload = WebOpenPayload::for_session("rimz/a b", "https://remote", 8200, None);
        assert_eq!(
            local_url(&payload, 8301),
            "http://127.0.0.1:8301/?arg=rimz%2Fa%20b"
        );
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
            None,
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
            None,
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
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPath=none",
                "-o",
                "ControlPersist=no",
                "-L",
                "127.0.0.1:8301:127.0.0.1:8082",
                "--",
                "dev-box"
            ]
        );
    }

    #[test]
    fn web_commands_reuse_a_control_master() {
        let target = parse("dev-box:query-engine");
        let control = Path::new("/tmp/rimz-web.sock");
        let control_args = [
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPath=/tmp/rimz-web.sock",
            "-o",
            "ControlPersist=no",
        ];

        let prep = web_prep_spec(&target, WebPrepOptions::default(), Some(control));
        assert_eq!(&prep.args[2..8], &control_args);
        assert_eq!(prep.args[8], "--");

        let forward = web_control_forward_spec(&target, 8301, 8082, control);
        assert_eq!(
            forward.args,
            [
                "-S",
                "/tmp/rimz-web.sock",
                "-O",
                "forward",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
                "127.0.0.1:8301:127.0.0.1:8082",
                "-o",
                "BatchMode=yes",
                "--",
                "dev-box",
            ]
        );
    }
}
