//! Fold-free command-cell supervision, bound to one durable leader run.

use super::*;
use std::sync::Arc;

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    #[arg(long)]
    workspace: rimz::ids::WorkspaceId,
    #[arg(long)]
    run: rimz::ids::RunId,
    #[arg(last = true)]
    argv: Vec<String>,
}

pub(super) fn run(args: Args, globals: &GlobalFlags) -> Result<()> {
    let paths = rimz::StatePaths::for_workspace(args.workspace.clone())?;
    let session = rimz::workspace::record::read(&paths.workspace_record)?.session_name;
    let result = supervise(args, &paths);
    exec::close_own_pane(globals, &session);
    result
}

fn run_is_live(paths: &rimz::StatePaths, run: &rimz::ids::RunId) -> Result<bool> {
    match rimz::harness::run::load(paths, run) {
        Ok(record) => Ok(!record.status.is_terminal()),
        Err(rimz::store::run::RunStoreErr::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn supervise(args: Args, paths: &rimz::StatePaths) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for signal in [
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    let mut argv = args.argv;
    if argv.is_empty() {
        argv.push(rimz::proc::user_shell_program());
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if stop.load(Ordering::Relaxed) || !run_is_live(paths, &args.run)? {
        return Ok(());
    }
    let mut child = CommandChild(command.spawn().context("starting command cell")?);
    let _foreground = Foreground::enter(child.0.id())?;
    let mut shutdown = Shutdown::default();
    loop {
        if child.0.try_wait()?.is_some() && matches!(shutdown, Shutdown::Live | Shutdown::KillSent)
        {
            return Ok(());
        }
        let stop = stop.load(Ordering::Relaxed) || !run_is_live(paths, &args.run)?;
        if let Some(signal) = shutdown.poll(Instant::now(), stop, CHILD_SIGNAL_GRACE) {
            child.signal(signal);
        }
        std::thread::sleep(RUN_MONITOR_POLL);
    }
}

struct CommandChild(Child);

impl CommandChild {
    fn signal(&mut self, signal: CommandSignal) {
        #[cfg(unix)]
        {
            use nix::sys::signal::{Signal, killpg};
            let signal = match signal {
                CommandSignal::Term => Signal::SIGTERM,
                CommandSignal::Kill => Signal::SIGKILL,
            };
            let _ = killpg(nix::unistd::Pid::from_raw(self.0.id() as i32), signal);
        }
        #[cfg(not(unix))]
        {
            let _ = signal;
            let _ = self.0.kill();
        }
    }
}

impl Drop for CommandChild {
    fn drop(&mut self) {
        self.signal(CommandSignal::Kill);
        let _ = self.0.wait();
    }
}

struct Foreground {
    #[cfg(unix)]
    previous: Option<nix::unistd::Pid>,
}

impl Foreground {
    fn enter(pid: u32) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::io::IsTerminal;
            let previous = std::io::stdin()
                .is_terminal()
                .then(|| nix::unistd::tcgetpgrp(std::io::stdin()))
                .transpose()?;
            if previous.is_some() {
                let group = nix::unistd::Pid::from_raw(pid as i32);
                set_foreground(group)?;
                // A reader can stop on SIGTTIN between spawn and foreground transfer.
                let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGCONT);
            }
            Ok(Self { previous })
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Ok(Self {})
        }
    }
}

#[cfg(unix)]
fn set_foreground(group: nix::unistd::Pid) -> nix::Result<()> {
    use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))?;
    let result = nix::unistd::tcsetpgrp(std::io::stdin(), group);
    let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
    result.and(restored)
}

impl Drop for Foreground {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(previous) = self.previous
            && let Err(error) = set_foreground(previous)
        {
            tracing::debug!(%error, "could not restore command terminal foreground group");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CommandSignal {
    Term,
    Kill,
}

#[derive(Default)]
enum Shutdown {
    #[default]
    Live,
    TermSent(Instant),
    KillSent,
}

impl Shutdown {
    fn poll(&mut self, now: Instant, stop: bool, grace: Duration) -> Option<CommandSignal> {
        match self {
            Self::Live if stop => {
                *self = Self::TermSent(now);
                Some(CommandSignal::Term)
            }
            Self::TermSent(sent) if now.duration_since(*sent) >= grace => {
                *self = Self::KillSent;
                Some(CommandSignal::Kill)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_run_sends_term_then_kill_at_the_grace_boundary() {
        let now = Instant::now();
        let grace = Duration::from_millis(300);
        let mut shutdown = Shutdown::default();
        let signals = [
            shutdown.poll(now, false, grace),
            shutdown.poll(now, true, grace),
            shutdown.poll(now + grace / 2, true, grace),
            shutdown.poll(now + grace, true, grace),
            shutdown.poll(now + grace * 2, true, grace),
        ];
        assert_eq!(
            signals,
            [
                None,
                Some(CommandSignal::Term),
                None,
                Some(CommandSignal::Kill),
                None
            ]
        );
    }
}
