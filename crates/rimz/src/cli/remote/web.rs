//! Foreground remote-web preparation, SSH tunnel ownership, and browser open.
//!
//! One `RemoteTunnel` owns each live child, readiness result, reconnect state,
//! and shutdown path for the command lifetime.

use std::io::{IsTerminal, Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::RemoteConnect;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RemoteWebOptions {
    pub(super) enabled: bool,
    pub(super) port: Option<u16>,
}

/// Foreground owner of one active SSH tunnel and its reconnect lifecycle.
struct RemoteTunnel {
    spec: rimz::mux::CommandSpec,
    host: String,
    reconnect: bool,
    policy: rimz::remote::ReconnectPolicy,
    reconnect_state: rimz::remote::ReconnectState,
    dial_plan: Option<rimz::remote::reachability::DialPlan>,
    child: Option<rimz::child_process::SupervisedChild>,
    started: Instant,
    wake_tx: mpsc::Sender<()>,
    wake_rx: mpsc::Receiver<()>,
}

impl Drop for RemoteTunnel {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

impl RemoteTunnel {
    fn start(
        spec: rimz::mux::CommandSpec,
        destination: String,
        host: String,
        reconnect: bool,
    ) -> Result<Self> {
        let policy = rimz::remote::ReconnectPolicy::from_env();
        let dial_plan = super::supervisor::resolve_dial_plan(&destination);
        let (wake_tx, wake_rx) = mpsc::channel();
        let mut tunnel = Self {
            spec,
            host,
            reconnect,
            policy,
            reconnect_state: rimz::remote::ReconnectState::new(),
            dial_plan,
            child: None,
            started: Instant::now(),
            wake_tx,
            wake_rx,
        };
        tunnel.spawn_child()?;
        Ok(tunnel)
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
                match self.settle_exit(exit_code)? {
                    TunnelFlow::Running => {}
                    TunnelFlow::Done => return Ok(PortWait::ExitedCleanly),
                }
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

    fn run(&mut self) -> Result<()> {
        loop {
            let exit_code = self.wait_for_exit()?;
            match self.settle_exit(exit_code)? {
                TunnelFlow::Running => {}
                TunnelFlow::Done => return Ok(()),
            }
        }
    }

    fn spawn_child(&mut self) -> Result<()> {
        let child = self
            .spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| anyhow::anyhow!("web tunnel to {} failed to start: {err}", self.host))?;
        self.started = Instant::now();
        self.child = Some(rimz::child_process::SupervisedChild::adopt(
            child,
            self.wake_tx.clone(),
        ));
        Ok(())
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

    fn settle_exit(&mut self, exit_code: Option<i32>) -> Result<TunnelFlow> {
        let established = self.started.elapsed() >= self.policy.gatetime;
        match tunnel_step(
            self.reconnect_state.settle(exit_code, established),
            self.reconnect,
        ) {
            TunnelStep::Clean => Ok(TunnelFlow::Done),
            TunnelStep::Fatal(code) => {
                bail!(
                    "web tunnel to {} exited with status {code}; not reconnecting",
                    self.host
                )
            }
            TunnelStep::Retry => {
                let consecutive_failures = self.reconnect_state.consecutive_failures();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: web tunnel to {} lost — reconnecting (attempt {consecutive_failures})",
                    self.host,
                );
                match super::supervisor::wait_for_plain_attempt(
                    self.dial_plan.as_ref(),
                    &self.policy,
                    &self.host,
                    None,
                ) {
                    super::supervisor::PlainWaitOutcome::AttemptNow => {}
                    super::supervisor::PlainWaitOutcome::Interrupted => {
                        return Ok(TunnelFlow::Done);
                    }
                }
                self.spawn_child()?;
                Ok(TunnelFlow::Running)
            }
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

enum TunnelFlow {
    Running,
    Done,
}

pub(super) fn run_remote_web(
    remote: &RemoteConnect,
    client_size: Option<(u16, u16)>,
) -> Result<()> {
    let prep = run_web_prep(
        &rimz::remote::web::web_prep_spec(
            &remote.target,
            rimz::remote::web::WebPrepOptions {
                confirm_resume: std::io::stdin().is_terminal(),
                no_resume: remote.no_resume,
                force_version: remote.force_version,
                client_size,
            },
        ),
        "preparing remote web access",
        remote.target.host_display(),
        remote.origin.as_str(),
    )?;
    let payload: rimz::web::WebOpenPayload = serde_json::from_slice(&prep)
        .with_context(|| remote_output_context("parsing remote `rimz web open --json`", &prep))?;
    if !payload.version_ok() {
        bail!(
            "remote `rimz web open --json` returned schema `{}`; upgrade the remote rimz binary",
            payload.version
        );
    }
    relay_web_token(remote, payload.engine);
    let local_port = rimz::web::choose_local_port(&payload.session, remote.web.port)
        .context("choosing local web tunnel port")?;
    let spec = rimz::remote::web::web_tunnel_spec(&remote.target, local_port, payload.port);
    let mut tunnel = RemoteTunnel::start(
        spec,
        remote.target.ssh_destination().as_str().to_owned(),
        remote.target.host_display().to_owned(),
        remote.reconnect,
    )?;
    match tunnel.wait_until_ready(local_port)? {
        PortWait::Ready => {}
        PortWait::ExitedCleanly => {
            bail!("web tunnel exited before local port accepted connections");
        }
    }
    let url = rimz::web::local_tunnel_url(&payload, local_port);
    writeln!(std::io::stdout().lock(), "{url}")?;
    super::super::open_browser_best_effort(&url);
    report_web_tunnel_up(remote.target.host_display(), remote.reconnect);
    tunnel.run()
}

fn relay_web_token(remote: &RemoteConnect, engine: rimz::web::WebEngine) {
    let spec = rimz::remote::web::web_token_ensure_spec(&remote.target, engine);
    let output = match spec.to_command().output() {
        Ok(output) => output,
        Err(err) => {
            write_web_token_error(remote.target.host_display(), engine, &err.to_string());
            return;
        }
    };
    if !output.status.success() {
        let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if detail.is_empty() {
            detail = output.status.to_string();
        }
        write_web_token_error(remote.target.host_display(), engine, &detail);
        return;
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if token.is_empty() {
        write_web_token_error(remote.target.host_display(), engine, "empty token");
        return;
    }
    let line = match engine {
        rimz::web::WebEngine::Zellij => format!(
            "rimz: login token for {}: {token}",
            remote.target.host_display()
        ),
        rimz::web::WebEngine::Ttyd => format!(
            "rimz: basic auth for {}: user rimz, password {token}",
            remote.target.host_display()
        ),
    };
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

fn write_web_token_error(host: &str, engine: rimz::web::WebEngine, detail: &str) {
    let engine = match engine {
        rimz::web::WebEngine::Zellij => "Zellij web login token",
        rimz::web::WebEngine::Ttyd => "ttyd basic-auth credential",
    };
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: could not mint a {engine} on {host} ({detail}); create one with `rimz web token create` on the remote host.",
    );
}

fn run_web_prep(
    spec: &rimz::mux::CommandSpec,
    label: &str,
    host: &str,
    setup_hint: &str,
) -> Result<Vec<u8>> {
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
        return Ok(stdout);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TunnelStep {
    Clean,
    Fatal(i32),
    Retry,
}

fn tunnel_step(verdict: rimz::remote::Verdict, reconnect: bool) -> TunnelStep {
    use rimz::remote::Verdict;

    match verdict {
        Verdict::CleanExit => TunnelStep::Clean,
        Verdict::Fatal { code } => TunnelStep::Fatal(code),
        Verdict::Retry if reconnect => TunnelStep::Retry,
        Verdict::Retry => TunnelStep::Fatal(rimz::remote::SSH_TRANSPORT_EXIT),
    }
}

enum PortWait {
    Ready,
    ExitedCleanly,
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
    fn no_reconnect_turns_retry_into_fatal_tunnel_exit() {
        let retry = rimz::remote::Verdict::Retry;

        assert_eq!(tunnel_step(retry, true), TunnelStep::Retry);
        assert_eq!(
            tunnel_step(retry, false),
            TunnelStep::Fatal(rimz::remote::SSH_TRANSPORT_EXIT)
        );
    }
}
