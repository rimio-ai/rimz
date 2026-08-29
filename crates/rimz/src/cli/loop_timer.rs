//! Opt-in operating-system timer for loop roots without an open room.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jiff::Zoned;

use rimz::harness::schedule::catalog::TaskCatalog;
use rimz::ids::WorkspaceId;
use rimz::store::atomic::write_bytes_atomically;
use rimz::store::paths::{RuntimePaths, config_home, env_path};

const SYSTEMD_SERVICE: &str = "rimz-loop.service";
const SYSTEMD_TIMER: &str = "rimz-loop.timer";
const LAUNCHD_LABEL: &str = "ai.rimz.loop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimerBackend {
    Systemd,
    Launchd,
}

impl TimerBackend {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Systemd => "systemd user",
            Self::Launchd => "launchd agent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TimerStatus {
    Installed {
        backend: TimerBackend,
        exec: PathBuf,
        active: bool,
    },
    NotInstalled,
}

impl TimerStatus {
    pub(super) const fn active(&self) -> bool {
        matches!(self, Self::Installed { active: true, .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TimerReport {
    pub(super) backend: TimerBackend,
    pub(super) path: PathBuf,
    pub(super) changed: bool,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TimerErr {
    #[error("cannot resolve the RimZ executable: {0}")]
    CurrentExe(#[source] std::io::Error),
    #[error("cannot prepare loop timer file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: rimz::store::atomic::AtomicErr,
    },
    #[error("cannot remove loop timer file {path}: {source}")]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot run `{program}`: {source}")]
    Spawn {
        program: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program} {args}` failed: {detail}")]
    Command {
        program: &'static str,
        args: String,
        detail: String,
    },
    #[error(
        "this platform has no managed loop timer backend; add this crontab entry instead:\n{hint}"
    )]
    Unsupported { hint: String },
}

type Result<T> = std::result::Result<T, TimerErr>;

pub(super) fn tick(now: &Zoned) {
    for root in task_roots() {
        let workspace_id = WorkspaceId::from_project_root(&root);
        let runtime = match RuntimePaths::for_workspace(workspace_id) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(root = %root.display(), error = %err, "loop tick could not resolve runtime paths");
                continue;
            }
        };
        if runtime_is_open(&runtime) {
            continue;
        }
        if let Err(err) = runtime.ensure_dirs() {
            tracing::warn!(root = %root.display(), error = %err, "loop tick could not prepare runtime paths");
            continue;
        }
        rimz::harness::schedule::fire::fire_due_tasks(&runtime, Some(&root), now);
    }
}

pub(super) fn uncovered_task_roots() -> usize {
    task_roots()
        .into_iter()
        .filter(|root| {
            let workspace_id = WorkspaceId::from_project_root(root);
            RuntimePaths::for_workspace(workspace_id)
                .ok()
                .is_none_or(|runtime| !runtime_is_open(&runtime))
        })
        .count()
}

fn task_roots() -> BTreeSet<PathBuf> {
    let mut roots = TaskCatalog::load_lenient(None)
        .visible()
        .values()
        .map(|task| task.entry().resolved_root())
        .collect::<BTreeSet<_>>();
    match rimz::trust::granted_roots() {
        Ok(granted) => roots.extend(granted.into_iter().filter(|root| {
            TaskCatalog::load_lenient(Some(root))
                .visible()
                .values()
                .any(|task| {
                    matches!(
                        task.source(),
                        rimz::harness::schedule::catalog::TaskSource::Project { .. }
                    ) && task.entry().resolved_root() == root.as_path()
                })
        })),
        Err(err) => {
            tracing::warn!(error = %err, "loop tick could not enumerate trusted project roots")
        }
    }
    roots
}

fn runtime_is_open(runtime: &RuntimePaths) -> bool {
    rimz::sidebar::fresh_sidebar_present(runtime)
}

fn detect() -> Result<TimerBackend> {
    let exe = current_exe()?;
    detect_for(
        std::env::consts::OS,
        |program| which::which(program).is_ok(),
        &exe,
    )
}

pub(super) fn install() -> Result<TimerReport> {
    let backend = detect()?;
    let exec = current_exe()?;
    match backend {
        TimerBackend::Systemd => install_systemd(&exec),
        TimerBackend::Launchd => install_launchd(&exec),
    }
}

pub(super) fn status() -> Result<TimerStatus> {
    match native_backend() {
        TimerBackend::Systemd => systemd_status(),
        TimerBackend::Launchd => launchd_status(),
    }
}

fn systemd_status() -> Result<TimerStatus> {
    let systemd = systemd_paths();
    if !systemd.timer.exists() && !systemd.service.exists() {
        return Ok(TimerStatus::NotInstalled);
    }
    let active = command_output("systemctl", &["--user", "is-active", SYSTEMD_TIMER])
        .map(|output| output.status.success())
        .unwrap_or(false);
    Ok(TimerStatus::Installed {
        backend: TimerBackend::Systemd,
        exec: installed_exec(&systemd.service).unwrap_or_else(current_exe_lossy),
        active,
    })
}

fn launchd_status() -> Result<TimerStatus> {
    let plist = launchd_path();
    if !plist.exists() {
        return Ok(TimerStatus::NotInstalled);
    }
    let target = launchd_target();
    let active = command_output("launchctl", &["print", &target])
        .map(|output| output.status.success())
        .unwrap_or(false);
    Ok(TimerStatus::Installed {
        backend: TimerBackend::Launchd,
        exec: installed_exec(&plist).unwrap_or_else(current_exe_lossy),
        active,
    })
}

pub(super) fn remove() -> Result<TimerReport> {
    match native_backend() {
        TimerBackend::Systemd => remove_systemd(),
        TimerBackend::Launchd => remove_launchd(),
    }
}

fn remove_systemd() -> Result<TimerReport> {
    let systemd = systemd_paths();
    if systemd.timer.exists() || systemd.service.exists() {
        command_success("systemctl", &["--user", "disable", "--now", SYSTEMD_TIMER])?;
        remove_file_if_present(&systemd.timer)?;
        remove_file_if_present(&systemd.service)?;
        command_success("systemctl", &["--user", "daemon-reload"])?;
        return Ok(TimerReport {
            backend: TimerBackend::Systemd,
            path: systemd.timer,
            changed: true,
        });
    }

    Ok(TimerReport {
        backend: TimerBackend::Systemd,
        path: systemd.timer,
        changed: false,
    })
}

fn remove_launchd() -> Result<TimerReport> {
    let plist = launchd_path();
    if plist.exists() {
        let target = launchd_target();
        launchctl_bootout(&target)?;
        remove_file_if_present(&plist)?;
        return Ok(TimerReport {
            backend: TimerBackend::Launchd,
            path: plist,
            changed: true,
        });
    }

    Ok(TimerReport {
        backend: TimerBackend::Launchd,
        path: plist,
        changed: false,
    })
}

fn install_systemd(exec: &Path) -> Result<TimerReport> {
    let paths = systemd_paths();
    write_timer_file(&paths.service, render_systemd_service(exec).as_bytes())?;
    write_timer_file(&paths.timer, render_systemd_timer().as_bytes())?;
    command_success("systemctl", &["--user", "daemon-reload"])?;
    command_success("systemctl", &["--user", "enable", "--now", SYSTEMD_TIMER])?;
    Ok(TimerReport {
        backend: TimerBackend::Systemd,
        path: paths.timer,
        changed: true,
    })
}

fn install_launchd(exec: &Path) -> Result<TimerReport> {
    let plist = launchd_path();
    write_timer_file(&plist, render_launchd_plist(exec).as_bytes())?;
    let target = launchd_target();
    let path = plist.to_string_lossy();
    launchctl_bootout(&target)?;
    command_success("launchctl", &["bootstrap", &launchd_domain(), &path])?;
    Ok(TimerReport {
        backend: TimerBackend::Launchd,
        path: plist,
        changed: true,
    })
}

fn detect_for(os: &str, command_exists: impl Fn(&str) -> bool, exe: &Path) -> Result<TimerBackend> {
    if os == "linux" && command_exists("systemctl") {
        return Ok(TimerBackend::Systemd);
    }
    if os == "macos" {
        return Ok(TimerBackend::Launchd);
    }
    Err(TimerErr::Unsupported {
        hint: format!("* * * * * {} loop tick", shell_quote(exe)),
    })
}

fn native_backend() -> TimerBackend {
    if cfg!(target_os = "macos") {
        TimerBackend::Launchd
    } else {
        TimerBackend::Systemd
    }
}

fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(TimerErr::CurrentExe)?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn current_exe_lossy() -> PathBuf {
    current_exe().unwrap_or_else(|_| PathBuf::from("rimz"))
}

struct SystemdPaths {
    service: PathBuf,
    timer: PathBuf,
}

fn systemd_paths() -> SystemdPaths {
    let root = config_home().join("systemd/user");
    SystemdPaths {
        service: root.join(SYSTEMD_SERVICE),
        timer: root.join(SYSTEMD_TIMER),
    }
}

fn launchd_path() -> PathBuf {
    env_path("HOME")
        .unwrap_or_else(std::env::temp_dir)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn launchd_domain() -> String {
    #[cfg(unix)]
    {
        format!("gui/{}", nix::unistd::Uid::current().as_raw())
    }
    #[cfg(not(unix))]
    {
        "gui/0".to_owned()
    }
}

fn launchd_target() -> String {
    format!("{}/{LAUNCHD_LABEL}", launchd_domain())
}

fn write_timer_file(path: &Path, bytes: &[u8]) -> Result<()> {
    write_bytes_atomically(path, bytes).map_err(|source| TimerErr::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(TimerErr::Remove {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn command_output(program: &'static str, args: &[&str]) -> Result<Output> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| TimerErr::Spawn { program, source })
}

fn command_success(program: &'static str, args: &[&str]) -> Result<()> {
    let output = command_output(program, args)?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(program, args, &output))
}

fn launchctl_bootout(target: &str) -> Result<()> {
    let args = ["bootout", target];
    let output = command_output("launchctl", &args)?;
    if output.status.success() || launchctl_service_absent(&output.stderr) {
        return Ok(());
    }
    Err(command_error("launchctl", &args, &output))
}

fn launchctl_service_absent(stderr: &[u8]) -> bool {
    let detail = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    detail.contains("no such process")
        || detail.contains("could not find")
        || detail.contains("not found")
}

fn command_error(program: &'static str, args: &[&str], output: &Output) -> TimerErr {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    TimerErr::Command {
        program,
        args: args.join(" "),
        detail: if detail.is_empty() {
            output.status.to_string()
        } else {
            detail
        },
    }
}

fn render_systemd_service(exec: &Path) -> String {
    format!(
        "[Unit]\nDescription=Run due RimZ loop tasks\n\n[Service]\nType=oneshot\nExecStart={} loop tick\n",
        systemd_quote(exec)
    )
}

fn render_systemd_timer() -> &'static str {
    "[Unit]\nDescription=Check RimZ loop tasks every minute\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec=1min\nAccuracySec=1s\nPersistent=false\n\n[Install]\nWantedBy=timers.target\n"
}

fn render_launchd_plist(exec: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LAUNCHD_LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n    <string>loop</string>\n    <string>tick</string>\n  </array>\n  <key>StartInterval</key>\n  <integer>60</integer>\n  <key>RunAtLoad</key>\n  <false/>\n</dict>\n</plist>\n",
        xml_escape(exec.to_string_lossy().as_ref())
    )
}

fn systemd_quote(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

fn shell_quote(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\\''"))
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn installed_exec(path: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(path).ok()?;
    if let Some(line) = contents.lines().find(|line| line.starts_with("ExecStart=")) {
        let value = line.strip_prefix("ExecStart=\"")?;
        let value = value.split("\" ").next()?;
        return Some(PathBuf::from(
            value
                .replace("%%", "%")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\"),
        ));
    }
    let arguments = contents.split("<key>ProgramArguments</key>").nth(1)?;
    let value = arguments
        .split("<string>")
        .nth(1)?
        .split("</string>")
        .next()?;
    Some(PathBuf::from(
        value
            .replace("&apos;", "'")
            .replace("&quot;", "\"")
            .replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("&amp;", "&"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_systemd_units_with_an_escaped_exec_path() {
        let exec = Path::new("/opt/RimZ % build/rimz");
        assert_eq!(
            render_systemd_service(exec),
            "[Unit]\nDescription=Run due RimZ loop tasks\n\n[Service]\nType=oneshot\nExecStart=\"/opt/RimZ %% build/rimz\" loop tick\n"
        );
        assert!(render_systemd_timer().contains("OnUnitActiveSec=1min"));
        assert!(render_systemd_timer().contains("AccuracySec=1s"));
        assert!(render_systemd_timer().contains("Persistent=false"));
    }

    #[test]
    fn renders_launchd_plist_without_shell_interpolation() {
        let rendered = render_launchd_plist(Path::new("/Users/a & b/rimz"));
        assert!(rendered.contains("<string>/Users/a &amp; b/rimz</string>"));
        assert!(rendered.contains("<integer>60</integer>"));
        assert!(rendered.contains("<false/>"));
    }

    #[test]
    fn unsupported_platform_returns_a_crontab_hint() {
        let err = detect_for("freebsd", |_| false, Path::new("/opt/rimz bin/rimz"))
            .expect_err("unsupported");
        assert!(
            err.to_string()
                .contains("* * * * * '/opt/rimz bin/rimz' loop tick")
        );
    }

    #[test]
    fn launchd_bootout_ignores_only_absent_services() {
        assert!(launchctl_service_absent(
            b"Boot-out failed: 3: No such process"
        ));
        assert!(!launchctl_service_absent(
            b"Boot-out failed: 5: Input/output error"
        ));
    }
}
