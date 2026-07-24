//! Foreground remote-web preparation, local auth relay, SSH tunnel, and browser open.
//!
//! One remote prep births the room, ensures ttyd, and returns its credential;
//! the process injects that credential into traffic forwarded over SSH.

use std::io::{IsTerminal, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rimz::remote::recovery::{ConnectStage, HandoffStage};

use super::RemoteConnect;
use super::outage_ui::{OutageUi, release_handoff_screen};
use super::supervisor::{OutageState, WaitOutcome};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RemoteWebOptions {
    pub(super) enabled: bool,
    pub(super) port: Option<u16>,
}

/// Foreground owner of one active SSH forwarding child.
struct RemoteTunnel {
    child: Option<rimz::child_process::SupervisedChild>,
    wake_rx: mpsc::Receiver<()>,
}

impl Drop for RemoteTunnel {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

impl RemoteTunnel {
    fn start(spec: &rimz::mux::CommandSpec, host: &str) -> Result<Self> {
        let (wake_tx, wake_rx) = mpsc::channel();
        let child = spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| anyhow::anyhow!("web tunnel to {host} failed to start: {err}"))?;
        Ok(Self {
            child: Some(rimz::child_process::SupervisedChild::adopt(child, wake_tx)),
            wake_rx,
        })
    }

    fn wait_until_ready(&mut self, port: u16) -> Result<PortWait> {
        let addr = ("127.0.0.1", port)
            .to_socket_addrs()?
            .next()
            .context("resolving local tunnel address")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
                return Ok(PortWait::Ready);
            }
            if let Some(exit_code) = self.poll_exit()? {
                return Ok(PortWait::Exited(exit_code));
            }
            if Instant::now() >= deadline {
                bail!(
                    "waiting for web tunnel on http://127.0.0.1:{port}: local web tunnel port {port} did not accept connections within 5s"
                );
            }
            rimz::child_process::wait_wake(
                &self.wake_rx,
                Some((Instant::now() + Duration::from_millis(50)).min(deadline)),
            );
        }
    }

    fn poll_exit(&mut self) -> Result<Option<Option<i32>>> {
        let child = self
            .child
            .as_mut()
            .context("remote web tunnel child is not running")?;
        let Some(status) = child.try_wait().context("polling remote web tunnel")? else {
            return Ok(None);
        };
        self.child = None;
        Ok(Some(status.code()))
    }

    fn wait_for_exit(&mut self) -> Result<Option<i32>> {
        loop {
            if let Some(exit_code) = self.poll_exit()? {
                return Ok(exit_code);
            }
            rimz::child_process::wait_wake(&self.wake_rx, None);
        }
    }

    fn kill_and_reap(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        child.signal_kill();
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => rimz::child_process::wait_wake(&self.wake_rx, None),
            }
        }
        self.child = None;
    }
}

struct HeldAlternateScreen(bool);

impl HeldAlternateScreen {
    fn new(held: bool) -> Self {
        Self(held)
    }

    fn release(&mut self) {
        if std::mem::take(&mut self.0) {
            release_handoff_screen();
        }
    }
}

impl Drop for HeldAlternateScreen {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) fn run_remote_web(
    remote: &RemoteConnect,
    client_size: Option<(u16, u16)>,
) -> Result<()> {
    if remote.reconnect {
        run_supervised_web(remote, client_size)
    } else {
        run_direct_web(remote, client_size)
    }
}

fn run_direct_web(remote: &RemoteConnect, client_size: Option<(u16, u16)>) -> Result<()> {
    let prep = run_web_prep(
        &rimz::remote::web::web_prep_spec(
            &remote.target,
            web_prep_options(remote, client_size, true),
            None,
        ),
        "preparing remote web access",
        remote.target.host_display(),
        remote.origin.as_str(),
    )?;
    let WebPrepOutcome::Ready(prep) = prep else {
        bail!("preparing remote web access failed with status 255");
    };
    let (payload, credential) = parse_web_payload(&prep)?;
    let relay_listener = rimz::remote::web::bind_local_relay(&payload.session, remote.web.port)
        .context("binding local web tunnel relay")?;
    let local_port = relay_listener.local_addr()?.port();
    let forward_port = rimz::remote::web::reserve_forward_port()
        .context("reserving local SSH web forward port")?;
    let relay_target = Arc::new(Mutex::new(make_relay_target(forward_port, &credential)));
    spawn_tunnel_relay(relay_listener, relay_target)?;
    let spec =
        rimz::remote::web::web_tunnel_spec(&remote.target, forward_port, tunnel_port(&payload));
    let mut tunnel = RemoteTunnel::start(&spec, remote.target.host_display())?;
    match tunnel.wait_until_ready(forward_port)? {
        PortWait::Ready => {}
        PortWait::Exited(_) => {
            bail!("web tunnel exited before local port accepted connections");
        }
    }
    let url = rimz::remote::web::local_url(&payload, local_port);
    writeln!(std::io::stdout().lock(), "{url}")?;
    super::super::open_browser_best_effort(&url);
    report_web_tunnel_up(remote.target.host_display(), false);
    match tunnel.wait_for_exit()? {
        Some(0) => Ok(()),
        exit_code => bail!(
            "web tunnel to {} exited with status {}; not reconnecting",
            remote.target.host_display(),
            exit_code.unwrap_or(1)
        ),
    }
}

fn run_supervised_web(remote: &RemoteConnect, client_size: Option<(u16, u16)>) -> Result<()> {
    let control = rimz::remote::link::validated_control_path()
        .context("checking SSH ControlMaster socket path")?;
    let plan = rimz::remote::SshAttachPlan::new(rimz::remote::SshAttachOptions {
        target: remote.target.clone(),
        lineage: super::local_remote_lineage(&remote.target)?,
        force_version: remote.force_version,
        no_resume: remote.no_resume,
        mux: remote.mux,
        term: super::remote_term_plan(),
        truecolor: rimz::tui::truecolor(),
        client_size,
    });
    let policy = rimz::remote::ReconnectPolicy::from_env();
    let mut reconnect = rimz::remote::ReconnectState::new();
    let dial_plan = super::supervisor::resolve_dial_plan(remote.target.ssh_destination().as_str());
    let stop = AtomicBool::new(false);
    let host = remote.target.host_display();
    let mut initial_ui = OutageUi::auto(ConnectStage::Initial, host);
    initial_ui.report_connecting();
    let mut initial_outage = OutageState::new(
        ConnectStage::Initial,
        HandoffStage::WebTunnel,
        host,
        super::supervisor::internet_probe_for_wait(&initial_ui),
        dial_plan.as_ref(),
    );
    let (mut master, initial_held_alt) = match super::supervisor::wait_for_master(
        &plan,
        &control,
        dial_plan.as_ref(),
        &policy,
        &mut initial_outage,
        &mut initial_ui,
        Some(&stop),
    )? {
        WaitOutcome::Connected { master, held_alt } => (Some(master), held_alt),
        WaitOutcome::NeedsInteractive => (None, false),
        WaitOutcome::Interrupted => return Ok(()),
    };
    let mut held_alt = HeldAlternateScreen::new(initial_held_alt);
    let mut first_prep = true;
    let mut first_round = true;
    let mut local_port = None;
    let mut relay_state = None;
    let mut outage = None;

    loop {
        let master_confirmed = master.is_some();
        let round_control = master_confirmed.then_some(control.as_path());
        let prep = run_web_prep(
            &rimz::remote::web::web_prep_spec(
                &remote.target,
                web_prep_options(remote, client_size, first_prep),
                round_control,
            ),
            "preparing remote web access",
            host,
            remote.origin.as_str(),
        )?;
        let prep = match prep {
            WebPrepOutcome::Ready(prep) => {
                first_prep = false;
                prep
            }
            WebPrepOutcome::TransportFailure => {
                match web_exit_action(
                    settle_web_exit(
                        &mut reconnect,
                        Some(rimz::remote::SSH_TRANSPORT_EXIT),
                        master_confirmed,
                        false,
                    ),
                    host,
                )? {
                    WebExitAction::Done => return Ok(()),
                    WebExitAction::Retry => {
                        held_alt.release();
                        drop(master.take());
                        let Some((next_master, next_held_alt)) = wait_for_web_recovery(
                            &plan,
                            &control,
                            dial_plan.as_ref(),
                            &policy,
                            &stop,
                            host,
                            &mut outage,
                        )?
                        else {
                            return Ok(());
                        };
                        master = Some(next_master);
                        held_alt = HeldAlternateScreen::new(next_held_alt);
                        continue;
                    }
                }
            }
        };
        let (payload, credential) = parse_web_payload(&prep)?;
        let forward_port = rimz::remote::web::reserve_forward_port()
            .context("reserving local SSH web forward port")?;
        let round_target = make_relay_target(forward_port, &credential);
        let local_port = match local_port {
            Some(port) => port,
            None => {
                let listener =
                    rimz::remote::web::bind_local_relay(&payload.session, remote.web.port)
                        .context("binding local web tunnel relay")?;
                let port = listener.local_addr()?.port();
                let target = Arc::new(Mutex::new(round_target.clone()));
                spawn_tunnel_relay(listener, Arc::clone(&target))?;
                relay_state = Some(target);
                local_port = Some(port);
                port
            }
        };
        let mut tunnel = if round_control.is_some() {
            None
        } else {
            let spec = rimz::remote::web::web_tunnel_spec(
                &remote.target,
                forward_port,
                tunnel_port(&payload),
            );
            Some(RemoteTunnel::start(&spec, host)?)
        };
        let readiness = match (round_control, tunnel.as_mut()) {
            (Some(control), None) => establish_control_forward(
                &rimz::remote::web::web_control_forward_spec(
                    &remote.target,
                    forward_port,
                    tunnel_port(&payload),
                    control,
                ),
                host,
            )?,
            (None, Some(tunnel)) => tunnel.wait_until_ready(forward_port)?,
            _ => unreachable!("web tunnel kind follows ControlMaster availability"),
        };
        let port_ready = match readiness {
            PortWait::Ready => true,
            PortWait::Exited(exit_code) => {
                if exit_code == Some(0) && !master_confirmed {
                    bail!("web tunnel exited before local port accepted connections");
                }
                match web_exit_action(
                    settle_web_exit(&mut reconnect, exit_code, master_confirmed, false),
                    host,
                )? {
                    WebExitAction::Done => return Ok(()),
                    WebExitAction::Retry => {
                        held_alt.release();
                        drop(master.take());
                        let Some((next_master, next_held_alt)) = wait_for_web_recovery(
                            &plan,
                            &control,
                            dial_plan.as_ref(),
                            &policy,
                            &stop,
                            host,
                            &mut outage,
                        )?
                        else {
                            return Ok(());
                        };
                        master = Some(next_master);
                        held_alt = HeldAlternateScreen::new(next_held_alt);
                        continue;
                    }
                }
            }
        };

        held_alt.release();
        replace_relay_target(
            relay_state
                .as_ref()
                .context("local web tunnel relay is not running")?,
            round_target,
        );
        if first_round {
            let url = rimz::remote::web::local_url(&payload, local_port);
            writeln!(std::io::stdout().lock(), "{url}")?;
            super::super::open_browser_best_effort(&url);
            report_web_tunnel_up(host, true);
            first_round = false;
        } else {
            let _ = writeln!(std::io::stderr().lock(), "rimz: tunnel to {host} restored");
        }
        outage = None;

        let exit_code = match tunnel.as_mut() {
            Some(tunnel) => tunnel.wait_for_exit()?,
            None => master
                .as_mut()
                .context("remote web ControlMaster is not running")?
                .wait_for_exit()?,
        };
        match web_exit_action(
            settle_web_exit(&mut reconnect, exit_code, master_confirmed, port_ready),
            host,
        )? {
            WebExitAction::Done => return Ok(()),
            WebExitAction::Retry => {
                drop(master.take());
                let Some((next_master, next_held_alt)) = wait_for_web_recovery(
                    &plan,
                    &control,
                    dial_plan.as_ref(),
                    &policy,
                    &stop,
                    host,
                    &mut outage,
                )?
                else {
                    return Ok(());
                };
                master = Some(next_master);
                held_alt = HeldAlternateScreen::new(next_held_alt);
            }
        }
    }
}

fn wait_for_web_recovery(
    plan: &rimz::remote::SshAttachPlan,
    control: &Path,
    dial_plan: Option<&rimz::remote::reachability::DialPlan>,
    policy: &rimz::remote::ReconnectPolicy,
    stop: &AtomicBool,
    host: &str,
    outage: &mut Option<OutageState>,
) -> Result<Option<(super::supervisor::MasterGuard, bool)>> {
    let mut ui = OutageUi::auto(ConnectStage::Recovery, host);
    let outage = outage.get_or_insert_with(|| {
        if ui.is_plain() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "rimz: web tunnel to {host} lost — reconnecting in the background; Ctrl-C stops",
            );
        }
        OutageState::new(
            ConnectStage::Recovery,
            HandoffStage::WebTunnel,
            host,
            super::supervisor::internet_probe_for_wait(&ui),
            dial_plan,
        )
    });
    match super::supervisor::wait_for_master(
        plan,
        control,
        dial_plan,
        policy,
        outage,
        &mut ui,
        Some(stop),
    )? {
        WaitOutcome::Connected { master, held_alt } => Ok(Some((master, held_alt))),
        WaitOutcome::Interrupted => {
            let _ = writeln!(std::io::stderr().lock(), "rimz: reconnect stopped");
            Ok(None)
        }
        WaitOutcome::NeedsInteractive => {
            unreachable!("web recovery stays in batch mode")
        }
    }
}

fn web_prep_options(
    remote: &RemoteConnect,
    client_size: Option<(u16, u16)>,
    initial_prep: bool,
) -> rimz::remote::web::WebPrepOptions {
    rimz::remote::web::WebPrepOptions {
        confirm_resume: initial_prep && std::io::stdin().is_terminal(),
        no_resume: initial_prep && remote.no_resume,
        force_version: remote.force_version,
        client_size,
    }
}

fn parse_web_payload(
    bytes: &[u8],
) -> Result<(rimz::web::WebOpenPayload, rimz::web::WebCredential)> {
    let payload: rimz::web::WebOpenPayload = serde_json::from_slice(bytes)
        .with_context(|| remote_output_context("parsing remote `rimz web open --json`", bytes))?;
    if !payload.version_ok() {
        bail!(
            "remote `rimz web open --json` returned schema `{}`; upgrade the remote rimz binary",
            payload.version
        );
    }
    if matches!(&payload.auth, rimz::web::WebAuth::TrustedHeader { .. })
        && payload.credential.is_none()
    {
        bail!(
            "the remote serves browser access behind a reverse proxy (trusted-header auth) — open `{}` directly; no SSH tunnel applies",
            payload.url
        );
    }
    let credential = payload
        .credential
        .clone()
        .context("remote `rimz web open --json` omitted the web credential")?;
    Ok((payload, credential))
}

fn tunnel_port(payload: &rimz::web::WebOpenPayload) -> u16 {
    payload.tunnel_port.unwrap_or(payload.port)
}

fn make_relay_target(
    forward_port: u16,
    credential: &rimz::web::WebCredential,
) -> rimz::web::RelayTarget {
    rimz::web::RelayTarget {
        upstream: SocketAddr::from(([127, 0, 0, 1], forward_port)),
        authorization: credential.authorization(),
    }
}

fn spawn_tunnel_relay(
    listener: TcpListener,
    target: Arc<Mutex<rimz::web::RelayTarget>>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("rimz-web-tunnel-relay".to_owned())
        .spawn(move || {
            if let Err(error) = rimz::web::serve_tunnel_relay(listener, target) {
                tracing::error!(%error, "local web tunnel relay stopped");
            }
        })
        .context("starting local web tunnel relay")?;
    Ok(())
}

fn replace_relay_target(
    target: &Mutex<rimz::web::RelayTarget>,
    replacement: rimz::web::RelayTarget,
) {
    *target.lock().unwrap_or_else(PoisonError::into_inner) = replacement;
}

enum WebPrepOutcome {
    Ready(Vec<u8>),
    TransportFailure,
}

fn establish_control_forward(spec: &rimz::mux::CommandSpec, host: &str) -> Result<PortWait> {
    let status = spec
        .to_command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("starting web tunnel to {host}"))?;
    if status.success() {
        Ok(PortWait::Ready)
    } else {
        Ok(PortWait::Exited(status.code()))
    }
}

fn run_web_prep(
    spec: &rimz::mux::CommandSpec,
    label: &str,
    host: &str,
    setup_hint: &str,
) -> Result<WebPrepOutcome> {
    let mut child = spec
        .to_command()
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "{label}: running `{}`",
                rimz::remote::display_ssh_command(spec)
            )
        })?;
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take()
        && let Err(err) = pipe.read_to_end(&mut stdout)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err).with_context(|| format!("{label}: reading remote prep stdout"));
    }
    let status = child
        .wait()
        .with_context(|| format!("{label}: waiting for remote prep"))?;
    if status.success() {
        return Ok(WebPrepOutcome::Ready(stdout));
    }
    if status.code() == Some(rimz::remote::SSH_TRANSPORT_EXIT) {
        return Ok(WebPrepOutcome::TransportFailure);
    }
    if let Some(code) = status.code()
        && matches!(
            code,
            rimz::remote::REMOTE_RIMZ_MISSING_EXIT
                | rimz::remote::REMOTE_VERSION_SKEW_EXIT
                | rimz::remote::REMOTE_VERSION_INCOMPATIBLE_EXIT
        )
    {
        bail!(
            "{}",
            super::supervisor::fatal_session_message(code, host, setup_hint, None)
        );
    }
    bail!("{label} failed with {status}");
}

fn remote_output_context(label: &str, bytes: &[u8]) -> String {
    let mut stdout = String::new();
    let _ = bytes.take(300).read_to_string(&mut stdout);
    format!("{label}; stdout={:?}", stdout.trim())
}

fn settle_web_exit(
    reconnect: &mut rimz::remote::ReconnectState,
    exit_code: Option<i32>,
    master_confirmed: bool,
    port_ready: bool,
) -> rimz::remote::Verdict {
    reconnect.settle(exit_code, master_confirmed || port_ready)
}

enum WebExitAction {
    Done,
    Retry,
}

fn web_exit_action(verdict: rimz::remote::Verdict, host: &str) -> Result<WebExitAction> {
    match verdict {
        rimz::remote::Verdict::CleanExit => Ok(WebExitAction::Done),
        rimz::remote::Verdict::Retry => Ok(WebExitAction::Retry),
        rimz::remote::Verdict::Fatal { code } => {
            bail!("web tunnel to {host} exited with status {code}; not reconnecting")
        }
    }
}

enum PortWait {
    Ready,
    Exited(Option<i32>),
}

fn report_web_tunnel_up(host: &str, reconnect: bool) {
    let message = if reconnect {
        "rimz: tunnel up — reconnects automatically; Ctrl-C stops"
    } else {
        "rimz: tunnel up — Ctrl-C stops"
    };
    tracing::debug!(host, reconnect, "remote web tunnel established");
    let _ = writeln!(std::io::stderr().lock(), "{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_trusted_header_payload_refuses_an_ssh_tunnel() {
        let bytes = br#"{
            "version":"rimz.web.v2",
            "url":"https://devbox.example/rimz/?arg=room",
            "session":"room",
            "port":8200,
            "auth":{"mode":"trusted_header","header":"X-Forwarded-User"}
        }"#;
        let err = parse_web_payload(bytes).expect_err("trusted-header tunnel refusal");
        let message = err.to_string();
        assert!(message.contains("behind a reverse proxy"), "{message}");
        assert!(
            message.contains("open `https://devbox.example/rimz/?arg=room` directly"),
            "{message}"
        );
    }

    #[test]
    fn trusted_header_payload_with_a_credential_tunnels_to_its_upstream_port() {
        let bytes = br#"{
            "version":"rimz.web.v2",
            "url":"https://devbox.example/rimz/?arg=room",
            "session":"room",
            "port":8200,
            "tunnel_port":41820,
            "auth":{"mode":"trusted_header","header":"X-Forwarded-User"},
            "credential":{"username":"rimz","secret":"secret"}
        }"#;
        let (payload, credential) = parse_web_payload(bytes).expect("trusted-header tunnel");
        assert_eq!(credential.username, "rimz");
        assert_eq!(tunnel_port(&payload), 41820);
    }

    #[test]
    fn legacy_basic_payload_tunnels_to_its_public_port() {
        let bytes = br#"{
            "version":"rimz.web.v2",
            "url":"http://127.0.0.1:8200/?arg=room",
            "session":"room",
            "port":8200,
            "credential":{"username":"rimz","secret":"secret"}
        }"#;
        let (payload, _) = parse_web_payload(bytes).expect("legacy Basic tunnel");
        assert_eq!(tunnel_port(&payload), 8200);
    }

    #[test]
    fn web_exit_settlement_uses_master_or_port_establishment() {
        let settle = |exit_code, master_confirmed, port_ready| {
            settle_web_exit(
                &mut rimz::remote::ReconnectState::new(),
                exit_code,
                master_confirmed,
                port_ready,
            )
        };

        assert_eq!(
            settle(Some(rimz::remote::SSH_TRANSPORT_EXIT), false, true),
            rimz::remote::Verdict::Retry
        );
        assert_eq!(
            settle(Some(rimz::remote::SSH_TRANSPORT_EXIT), true, false),
            rimz::remote::Verdict::Retry
        );
        assert_eq!(
            settle(Some(rimz::remote::SSH_TRANSPORT_EXIT), false, false),
            rimz::remote::Verdict::Fatal {
                code: rimz::remote::SSH_TRANSPORT_EXIT
            }
        );
        assert_eq!(
            settle(Some(0), false, false),
            rimz::remote::Verdict::CleanExit
        );
    }
}
