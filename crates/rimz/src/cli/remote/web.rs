use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::RemoteConnect;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RemoteWebOptions {
    pub(super) enabled: bool,
    pub(super) port: Option<u16>,
    pub(super) token: bool,
}

struct RemoteWebGuard {
    host: String,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<TunnelExit>>,
}

impl Drop for RemoteWebGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl RemoteWebGuard {
    fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }

    fn wait(mut self) -> Result<()> {
        let exit = self
            .thread
            .take()
            .context("remote web tunnel supervisor already joined")?
            .join()
            .map_err(|_| anyhow::anyhow!("remote web tunnel supervisor panicked"))?;
        match exit {
            TunnelExit::Clean => Ok(()),
            TunnelExit::Fatal(code) => {
                bail!(
                    "web tunnel to {} exited with status {code}; not reconnecting",
                    self.host
                )
            }
            TunnelExit::StartFailed(err) => {
                bail!("web tunnel to {} failed to start: {err}", self.host)
            }
        }
    }
}

pub(super) fn run_remote_web(remote: &RemoteConnect) -> Result<()> {
    let prep = run_one_shot(
        &rimz::remote::web::web_prep_spec(&remote.target),
        "preparing remote Zellij web",
    )?;
    let payload = rimz::web::parse_web_open_payload(&prep.stdout)
        .with_context(|| remote_output_context("parsing remote `rimz web open --json`", &prep))?;
    if !payload.version_ok() {
        bail!(
            "remote `rimz web open --json` returned schema `{}`; upgrade the remote rimz binary",
            payload.version
        );
    }
    match token_action(remote.web.token, payload.token_count) {
        TokenAction::Create => relay_web_token(remote)?,
        TokenAction::Hint => {
            writeln!(
                std::io::stderr().lock(),
                "no Zellij web login token on {}; re-run with --web-token to create one",
                remote.target.host_display()
            )?;
        }
        TokenAction::Nothing => {}
    }
    let local_port = rimz::web::choose_local_port(&payload.session, remote.web.port)
        .context("choosing local web tunnel port")?;
    let tunnel_spec = rimz::remote::web::web_tunnel_spec(&remote.target, local_port, payload.port);
    let guard = spawn_tunnel_supervisor(
        tunnel_spec,
        remote.target.host_display().to_owned(),
        remote.reconnect,
    )?;
    match wait_for_local_port(local_port, &guard)
        .with_context(|| format!("waiting for web tunnel on http://127.0.0.1:{local_port}"))?
    {
        PortWait::Ready => {}
        PortWait::SupervisorExited => {
            guard.wait()?;
            bail!("web tunnel exited before local port accepted connections");
        }
    }
    let (url, _) = super::super::web::local_tunnel_payload(&payload, local_port);
    super::super::web::print_url(&format!("web: {url}"))?;
    super::super::web::open_browser_best_effort(&url);
    report_web_tunnel_up(remote.target.host_display(), remote.reconnect);
    guard.wait()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenAction {
    Create,
    Hint,
    Nothing,
}

fn token_action(flag: bool, token_count: usize) -> TokenAction {
    match (flag, token_count) {
        (true, _) => TokenAction::Create,
        (false, 0) => TokenAction::Hint,
        (false, _) => TokenAction::Nothing,
    }
}

fn relay_web_token(remote: &RemoteConnect) -> Result<()> {
    let output = run_one_shot(
        &rimz::remote::web::web_token_create_spec(&remote.target),
        "creating Zellij web token",
    )?;
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "Zellij web login token — shown once")?;
    drop(stderr);
    std::io::stdout().lock().write_all(&output.stdout)?;
    std::io::stderr().lock().write_all(&output.stderr)?;
    Ok(())
}

fn run_one_shot(spec: &rimz::mux::CommandSpec, label: &str) -> Result<Output> {
    let output = spec.to_command().output().with_context(|| {
        format!(
            "{label}: running `{}`",
            rimz::remote::display_ssh_command(spec)
        )
    })?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{label} failed: {}", stderr.trim());
}

fn remote_output_context(label: &str, output: &Output) -> String {
    let mut stdout = String::new();
    let _ = (&output.stdout[..]).take(300).read_to_string(&mut stdout);
    let mut stderr = String::new();
    let _ = (&output.stderr[..]).take(300).read_to_string(&mut stderr);
    format!(
        "{label}; stdout={:?}, stderr={:?}",
        stdout.trim(),
        stderr.trim()
    )
}

fn spawn_tunnel_supervisor(
    spec: rimz::mux::CommandSpec,
    host: String,
    reconnect: bool,
) -> Result<RemoteWebGuard> {
    let stop = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let thread_stop = Arc::clone(&stop);
    let thread_child = Arc::clone(&child);
    let thread_host = host.clone();
    let thread = std::thread::Builder::new()
        .name("rimz-remote-web-tunnel".to_owned())
        .spawn(move || supervise_tunnel(spec, thread_host, reconnect, thread_stop, thread_child))
        .context("spawning remote web tunnel supervisor")?;
    Ok(RemoteWebGuard {
        host,
        stop,
        child,
        thread: Some(thread),
    })
}

#[derive(Debug, PartialEq, Eq)]
enum TunnelExit {
    Clean,
    Fatal(i32),
    StartFailed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TunnelStep {
    Clean,
    Fatal(i32),
    Retry(Duration),
}

fn tunnel_step(verdict: rimz::remote::Verdict, reconnect: bool) -> TunnelStep {
    use rimz::remote::Verdict;

    match verdict {
        Verdict::CleanExit => TunnelStep::Clean,
        Verdict::Fatal { code } => TunnelStep::Fatal(code),
        Verdict::Retry { delay } if reconnect => TunnelStep::Retry(delay),
        Verdict::Retry { .. } => TunnelStep::Fatal(rimz::remote::SSH_TRANSPORT_EXIT),
    }
}

fn supervise_tunnel(
    spec: rimz::mux::CommandSpec,
    host: String,
    reconnect: bool,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) -> TunnelExit {
    use rimz::remote::{ReconnectPolicy, verdict};

    let policy = ReconnectPolicy::from_env();
    let mut established = false;
    let mut consecutive_failures = 0;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let child = match spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => return TunnelExit::StartFailed(err.to_string()),
        };
        if let Ok(mut slot) = child_slot.lock() {
            *slot = Some(child);
        }
        let exit_code = wait_tunnel_child(&stop, &child_slot);
        if stop.load(Ordering::SeqCst) {
            return TunnelExit::Clean;
        }
        let ran_past_gatetime = started.elapsed() >= policy.gatetime;
        if ran_past_gatetime {
            established = true;
            consecutive_failures = 0;
        }
        match tunnel_step(
            verdict(exit_code, established, consecutive_failures, &policy),
            reconnect,
        ) {
            TunnelStep::Clean => return TunnelExit::Clean,
            TunnelStep::Fatal(code) => return TunnelExit::Fatal(code),
            TunnelStep::Retry(delay) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: web tunnel to {host} lost — reconnecting in {}s (attempt {consecutive_failures})",
                    delay.as_secs(),
                );
                sleep_interruptible(delay, &stop);
            }
        }
    }
    TunnelExit::Clean
}

fn wait_tunnel_child(stop: &AtomicBool, child_slot: &Mutex<Option<Child>>) -> Option<i32> {
    loop {
        if stop.load(Ordering::SeqCst) {
            if let Ok(mut slot) = child_slot.lock()
                && let Some(child) = slot.as_mut()
            {
                let _ = child.kill();
            }
            return None;
        }
        if let Ok(mut slot) = child_slot.lock()
            && let Some(child) = slot.as_mut()
        {
            match child.try_wait() {
                Ok(Some(status)) => {
                    *slot = None;
                    return status.code();
                }
                Ok(None) => {}
                Err(_) => {
                    *slot = None;
                    return Some(1);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn sleep_interruptible(duration: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

enum PortWait {
    Ready,
    SupervisorExited,
}

fn wait_for_local_port(port: u16, guard: &RemoteWebGuard) -> Result<PortWait> {
    let addr = ("127.0.0.1", port)
        .to_socket_addrs()?
        .next()
        .context("resolving local tunnel address")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(PortWait::Ready);
        }
        if guard.is_finished() {
            return Ok(PortWait::SupervisorExited);
        }
        if Instant::now() >= deadline {
            bail!("local web tunnel port {port} did not accept connections within 5s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn report_web_tunnel_up(host: &str, reconnect: bool) {
    let tail = if reconnect {
        " (auto-reconnect on; Ctrl-C stops)"
    } else {
        " (Ctrl-C stops)"
    };
    let _ = writeln!(
        std::io::stderr().lock(),
        "rimz: web tunnel to {host} up{tail}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_action_requires_explicit_creation() {
        assert_eq!(token_action(true, 0), TokenAction::Create);
        assert_eq!(token_action(true, 2), TokenAction::Create);
        assert_eq!(token_action(false, 0), TokenAction::Hint);
        assert_eq!(token_action(false, 2), TokenAction::Nothing);
    }

    #[test]
    fn no_reconnect_turns_retry_into_fatal_tunnel_exit() {
        let retry = rimz::remote::Verdict::Retry {
            delay: Duration::from_secs(1),
        };

        assert_eq!(
            tunnel_step(retry, true),
            TunnelStep::Retry(Duration::from_secs(1))
        );
        assert_eq!(
            tunnel_step(retry, false),
            TunnelStep::Fatal(rimz::remote::SSH_TRANSPORT_EXIT)
        );
    }
}
