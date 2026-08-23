//! Probes terminal and tmux capabilities for shared kitty graphics support.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::ids::MuxName;

const MIN_PIXEL_TMUX_VERSION: (u32, u32, u32) = (3, 6, 0);
pub const MIN_PIXEL_ZELLIJ_VERSION: (u32, u32, u32) = (0, 45, 0);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(500);
const KITTY_QUERY_TIMEOUT: Duration = Duration::from_millis(500);
const KITTY_PROBE_ID: u32 = 0x52_49_4d;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRenderCaps {
    pub pixel_transport: bool,
    pub kitty_clients: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZellijKittySupport {
    Supported,
    Unsupported,
    ProtocolDisabled,
    BelowMinimum,
    NotProbed,
}

pub fn probe_zellij_kitty(version: Option<(u32, u32, u32)>) -> ZellijKittySupport {
    let Some(version) = version else {
        return ZellijKittySupport::NotProbed;
    };
    if version < MIN_PIXEL_ZELLIJ_VERSION {
        return ZellijKittySupport::BelowMinimum;
    }
    if std::env::var_os("ZELLIJ").is_none_or(|value| value.is_empty()) {
        return ZellijKittySupport::NotProbed;
    }

    #[cfg(unix)]
    {
        let Ok(mut tty) = super::tty::TtyBarrierSource::open_raw() else {
            return ZellijKittySupport::NotProbed;
        };
        probe_zellij_kitty_with(version, &mut tty, KITTY_QUERY_TIMEOUT)
    }
    #[cfg(not(unix))]
    {
        ZellijKittySupport::NotProbed
    }
}

fn probe_zellij_kitty_with(
    version: (u32, u32, u32),
    source: &mut (impl super::tty::BarrierSource + Write),
    timeout: Duration,
) -> ZellijKittySupport {
    if version < MIN_PIXEL_ZELLIJ_VERSION {
        return ZellijKittySupport::BelowMinimum;
    }

    let query = format!("\x1b_Ga=q,i={KITTY_PROBE_ID},s=1,v=1,t=d,f=24;AAAA\x1b\\");
    if source
        .write_all(query.as_bytes())
        .and_then(|()| source.flush())
        .is_err()
    {
        return ZellijKittySupport::NotProbed;
    }

    let expected = format!("i={KITTY_PROBE_ID};");
    let mut scanner = super::tty::GraphicsReplyScanner::default();
    let deadline = Instant::now() + timeout;
    let mut buf = [0_u8; 256];
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return ZellijKittySupport::ProtocolDisabled;
        };
        match source.poll_read(&mut buf, remaining) {
            Ok(Some(0)) | Ok(None) => return ZellijKittySupport::ProtocolDisabled,
            Ok(Some(read)) => {
                for payload in scanner.push(&buf[..read]) {
                    let Some(status) = payload.strip_prefix(expected.as_bytes()) else {
                        continue;
                    };
                    return if status == b"OK" {
                        ZellijKittySupport::Supported
                    } else {
                        ZellijKittySupport::Unsupported
                    };
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return ZellijKittySupport::NotProbed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RenderingClient {
    termname: String,
    pid: u32,
}

pub(crate) fn detect(mux: MuxName, session_name: &str, prev: PixelRenderCaps) -> PixelRenderCaps {
    detect_with(mux, session_name, prev, &LiveProbe)
}

pub fn detect_env() -> (PixelRenderCaps, bool) {
    detect_env_with(&LiveProbe)
}

trait Probe {
    fn tmux_version(&self) -> io::Result<String>;
    fn tmux_allow_passthrough(&self, target: &str) -> io::Result<String>;
    fn tmux_set_pane_passthrough_all(&self, pane: &str) -> io::Result<()>;
    fn tmux_rendering_clients(&self, session_name: &str) -> io::Result<Vec<RenderingClient>>;
    fn tmux_session_name(&self) -> io::Result<String>;
    fn processes(&self) -> Vec<crate::proc::ProcInfo>;
    fn pixel_daemon_records(&self) -> Vec<(u32, u32)>;
    fn env_var(&self, key: &str) -> Option<String>;
}

pub(crate) fn escalate_own_pane_passthrough() -> io::Result<()> {
    escalate_with(&LiveProbe)
}

fn escalate_with(probe: &impl Probe) -> io::Result<()> {
    let Some(pane) = probe.env_var("TMUX_PANE").filter(|pane| !pane.is_empty()) else {
        return Ok(());
    };
    if probe.tmux_allow_passthrough(&pane)?.trim() == "on" {
        probe.tmux_set_pane_passthrough_all(&pane)?;
    }
    Ok(())
}

fn detect_with(
    probed_mux: MuxName,
    session_name: &str,
    prev: PixelRenderCaps,
    probe: &impl Probe,
) -> PixelRenderCaps {
    match probed_mux {
        MuxName::Tmux => detect_tmux(session_name, probe, prev),
        MuxName::Zellij => detect_zellij(prev),
    }
}

fn detect_env_with(probe: &impl Probe) -> (PixelRenderCaps, bool) {
    if env_present(probe, "TMUX") {
        let caps = probe
            .tmux_session_name()
            .map(|session_name| detect_tmux(&session_name, probe, PixelRenderCaps::default()))
            .unwrap_or_default();
        return (caps, true);
    }
    if env_present(probe, "ZELLIJ") {
        return (PixelRenderCaps::default(), false);
    }
    (detect_standalone(probe), false)
}

fn detect_tmux(session_name: &str, probe: &impl Probe, prev: PixelRenderCaps) -> PixelRenderCaps {
    let kitty_clients = match probe.tmux_rendering_clients(session_name) {
        Ok(clients) if !clients.is_empty() => rendering_clients_allowed(&clients, probe),
        _ => prev.kitty_clients,
    };
    let passthrough_target = probe
        .env_var("TMUX_PANE")
        .filter(|pane| !pane.is_empty())
        .unwrap_or_else(|| session_name.to_owned());
    let pixel_transport = match (
        probe.tmux_version(),
        probe.tmux_allow_passthrough(&passthrough_target),
    ) {
        (Ok(version), Ok(allow)) => {
            let version_ok = crate::mux::tmux::parse_version(&version)
                .is_some_and(|version| version >= MIN_PIXEL_TMUX_VERSION);
            let passthrough_ok = matches!(allow.trim(), "on" | "all");
            version_ok && passthrough_ok
        }
        _ => prev.pixel_transport,
    };
    PixelRenderCaps {
        pixel_transport,
        kitty_clients,
    }
}

fn detect_zellij(prev: PixelRenderCaps) -> PixelRenderCaps {
    prev
}

fn detect_standalone(probe: &impl Probe) -> PixelRenderCaps {
    PixelRenderCaps {
        pixel_transport: true,
        kitty_clients: standalone_term_allowed(probe),
    }
}

fn standalone_term_allowed(probe: &impl Probe) -> bool {
    probe
        .env_var("TERM")
        .as_deref()
        .is_some_and(termname_allowed)
}

fn env_present(probe: &impl Probe, key: &str) -> bool {
    probe.env_var(key).is_some_and(|value| !value.is_empty())
}

struct LiveProbe;

impl Probe for LiveProbe {
    fn tmux_version(&self) -> io::Result<String> {
        run_tmux(["-V"])
    }

    fn tmux_allow_passthrough(&self, target: &str) -> io::Result<String> {
        run_tmux([
            "display-message",
            "-p",
            "-t",
            target,
            "#{allow-passthrough}",
        ])
    }

    fn tmux_set_pane_passthrough_all(&self, pane: &str) -> io::Result<()> {
        run_tmux(["set-option", "-p", "-t", pane, "allow-passthrough", "all"]).map(|_| ())
    }

    fn tmux_rendering_clients(&self, session_name: &str) -> io::Result<Vec<RenderingClient>> {
        run_tmux([
            "list-clients",
            "-t",
            session_name,
            "-F",
            "#{client_control_mode} #{client_termname} #{client_pid}",
        ])
        .map(|out| rendering_clients(&out))
    }

    fn tmux_session_name(&self) -> io::Result<String> {
        run_tmux(["display-message", "-p", "#{session_name}"])
    }

    fn processes(&self) -> Vec<crate::proc::ProcInfo> {
        crate::proc::list_processes()
    }

    fn pixel_daemon_records(&self) -> Vec<(u32, u32)> {
        crate::web::pixel_daemon_records()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

fn run_tmux<const N: usize>(args: [&str; N]) -> io::Result<String> {
    let output = crate::mux::tmux::managed_cmd()
        .args(args)
        .run_with_timeout(COMMAND_TIMEOUT)
        .map_err(io::Error::other)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn rendering_clients_allowed(clients: &[RenderingClient], probe: &impl Probe) -> bool {
    if clients
        .iter()
        .all(|client| termname_allowed(&client.termname))
    {
        return true;
    }
    let processes = probe.processes();
    let daemons = probe
        .pixel_daemon_records()
        .into_iter()
        .filter(|(_, protocol)| *protocol == crate::web::TTYD_PIXEL_PROTOCOL)
        .map(|(pid, _)| pid)
        .filter(|pid| {
            processes.iter().any(|process| {
                process.pid == *pid
                    && crate::proc::command::program_label(&process.cmdline) == "ttyd"
            })
        })
        .collect::<Vec<_>>();
    !daemons.is_empty()
        && clients.iter().all(|client| {
            termname_allowed(&client.termname)
                || daemons
                    .iter()
                    .any(|daemon| descends_from(client.pid, *daemon, &processes))
        })
}

fn descends_from(pid: u32, ancestor: u32, processes: &[crate::proc::ProcInfo]) -> bool {
    let mut current = pid;
    for _ in 0..4 {
        let Some(process) = processes.iter().find(|process| process.pid == current) else {
            return false;
        };
        if process.ppid == ancestor {
            return true;
        }
        if process.ppid == current {
            return false;
        }
        current = process.ppid;
    }
    false
}

fn rendering_clients(list_clients_output: &str) -> Vec<RenderingClient> {
    list_clients_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let control_mode = fields.next()?;
            if control_mode == "1" {
                return None;
            }
            Some(RenderingClient {
                termname: fields.next().unwrap_or_default().to_owned(),
                pid: fields.next().and_then(|pid| pid.parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

fn termname_allowed(termname: &str) -> bool {
    matches!(
        termname.trim().to_ascii_lowercase().as_str(),
        "xterm-ghostty" | "ghostty" | "xterm-kitty" | "kitty"
    )
}

#[cfg(test)]
mod tests;
