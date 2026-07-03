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
}

pub(super) struct RemoteWebGuard {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
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

pub(super) fn prepare_remote_web(remote: &RemoteConnect) -> Result<RemoteWebGuard> {
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
    if payload.token_count == 0 {
        relay_first_token(remote)?;
    }
    let local_port = rimz::web::choose_local_port(&payload.session, remote.web.port)
        .context("choosing local web tunnel port")?;
    let tunnel_spec = rimz::remote::web::web_tunnel_spec(&remote.target, local_port, payload.port);
    let guard = spawn_tunnel_supervisor(tunnel_spec, remote.target.host_display().to_owned())?;
    wait_for_local_port(local_port)
        .with_context(|| format!("waiting for web tunnel on http://127.0.0.1:{local_port}"))?;
    let (url, _) = super::super::web::local_tunnel_payload(&payload, local_port);
    super::super::web::print_url(&format!("web: {url}"))?;
    super::super::web::open_browser_best_effort(&url);
    Ok(guard)
}

fn relay_first_token(remote: &RemoteConnect) -> Result<()> {
    let output = run_one_shot(
        &rimz::remote::web::web_token_create_spec(&remote.target),
        "creating first-run Zellij web token",
    )?;
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "first-run Zellij web token — shown once")?;
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

fn spawn_tunnel_supervisor(spec: rimz::mux::CommandSpec, host: String) -> Result<RemoteWebGuard> {
    let stop = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let thread_stop = Arc::clone(&stop);
    let thread_child = Arc::clone(&child);
    let thread = std::thread::Builder::new()
        .name("rimz-remote-web-tunnel".to_owned())
        .spawn(move || supervise_tunnel(spec, host, thread_stop, thread_child))
        .context("spawning remote web tunnel supervisor")?;
    Ok(RemoteWebGuard {
        stop,
        child,
        thread: Some(thread),
    })
}

fn supervise_tunnel(
    spec: rimz::mux::CommandSpec,
    host: String,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    use rimz::remote::{ReconnectPolicy, Verdict, verdict};

    let policy = ReconnectPolicy::from_env();
    let mut established = false;
    let mut consecutive_failures = 0;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let child = match spec
            .to_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: web tunnel to {host} failed to start: {err}"
                );
                return;
            }
        };
        if let Ok(mut slot) = child_slot.lock() {
            *slot = Some(child);
        }
        let exit_code = wait_tunnel_child(&stop, &child_slot);
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let ran_past_gatetime = started.elapsed() >= policy.gatetime;
        if ran_past_gatetime {
            established = true;
            consecutive_failures = 0;
        }
        match verdict(exit_code, established, consecutive_failures, &policy) {
            Verdict::CleanExit => return,
            Verdict::Fatal { code } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "rimz: web tunnel to {host} exited with status {code}; not reconnecting",
                );
                return;
            }
            Verdict::Retry { delay } => {
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

fn wait_for_local_port(port: u16) -> Result<()> {
    let addr = ("127.0.0.1", port)
        .to_socket_addrs()?
        .next()
        .context("resolving local tunnel address")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("local web tunnel port {port} did not accept connections within 5s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
