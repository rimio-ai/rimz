//! The inotify guard thread, view discovery, and the namespace-joining repair helper.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::mount::{MsFlags, mount};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sched::{CloneFlags, setns};
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};
use nix::sys::statvfs::{FsFlags, statvfs};
use rustix::fs::inotify;
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};

use super::{BindGuardErr, BindRestore};

const VIEW_WAIT: Duration = Duration::from_secs(5);
const HELPER_WAIT: Duration = Duration::from_secs(5);
const DISCOVERY_STEP: Duration = Duration::from_millis(25);
const HELPER_ARG: &str = "--scoped-bind-restore";

pub(super) struct Guard {
    source: Arc<OwnedFd>,
    control: UnixStream,
    target: Arc<Target>,
    worker: Option<JoinHandle<()>>,
}

struct Target {
    pid: AtomicU32,
    stop: AtomicBool,
}

impl Guard {
    pub(super) fn arm(
        entries: &[PathBuf],
        helper: &Path,
        sink: impl Fn(BindRestore) + Send + 'static,
    ) -> Result<Self, BindGuardErr> {
        let source = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
            .map_err(|error| BindGuardErr::Init(error.into()))?;
        Self::start(source, entries, helper, sink)
    }

    pub(super) fn handover(
        inherited: i32,
        entries: &[PathBuf],
        helper: &Path,
        sink: impl Fn(BindRestore) + Send + 'static,
    ) -> Result<Self, BindGuardErr> {
        let source = adopt(inherited).map_err(BindGuardErr::Worker)?;
        Self::start(source, entries, helper, sink)
    }

    fn start(
        source: OwnedFd,
        entries: &[PathBuf],
        helper: &Path,
        sink: impl Fn(BindRestore) + Send + 'static,
    ) -> Result<Self, BindGuardErr> {
        let mut parents = BTreeMap::new();
        let mut watched = Vec::new();
        for path in entries {
            let parent = path.parent().ok_or_else(|| BindGuardErr::Watch {
                path: path.clone(),
                source: io::ErrorKind::InvalidInput.into(),
            })?;
            let wd = match parents.get(parent) {
                Some(wd) => *wd,
                None => {
                    let wd = inotify::add_watch(
                        &source,
                        parent,
                        inotify::WatchFlags::CREATE
                            | inotify::WatchFlags::MOVED_TO
                            | inotify::WatchFlags::ONLYDIR,
                    )
                    .map_err(|error| BindGuardErr::Watch {
                        path: parent.to_path_buf(),
                        source: error.into(),
                    })?;
                    parents.insert(parent.to_path_buf(), wd);
                    wd
                }
            };
            watched.push((wd, path.clone()));
        }
        let own_namespace = namespace(std::process::id()).ok_or_else(|| {
            BindGuardErr::Worker(io::Error::other("cannot read the guard's mount namespace"))
        })?;
        let (control, receiver) = UnixStream::pair().map_err(BindGuardErr::Worker)?;
        control
            .set_nonblocking(true)
            .map_err(BindGuardErr::Worker)?;
        receiver
            .set_nonblocking(true)
            .map_err(BindGuardErr::Worker)?;
        let source = Arc::new(source);
        let target = Arc::new(Target {
            pid: AtomicU32::new(0),
            stop: AtomicBool::new(false),
        });
        let worker = {
            let source = source.clone();
            let target = target.clone();
            let helper = helper.to_path_buf();
            std::thread::Builder::new()
                .name("scoped-binds".into())
                .stack_size(256 * 1024)
                .spawn(move || {
                    watch(
                        &source,
                        receiver,
                        &target,
                        &watched,
                        &helper,
                        own_namespace,
                        sink,
                    )
                })
                .map_err(BindGuardErr::Worker)?
        };
        Ok(Self {
            source,
            control,
            target,
            worker: Some(worker),
        })
    }

    pub(super) fn point(&self, pid: u32) {
        self.target.pid.store(pid, Ordering::Release);
        let _ = (&self.control).write(&[0]);
    }

    pub(super) fn prepare_handoff(&mut self) -> Result<i32, BindGuardErr> {
        self.stop();
        fcntl(&*self.source, FcntlArg::F_SETFD(FdFlag::empty()))
            .map_err(|error| BindGuardErr::Worker(error.into()))?;
        Ok(self.source.as_raw_fd())
    }

    fn stop(&mut self) {
        self.target.stop.store(true, Ordering::Release);
        let _ = (&self.control).write(&[0]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn adopt(inherited: i32) -> io::Result<OwnedFd> {
    // SCM_RIGHTS supplies an IO-safe owner of the same inotify instance and queue.
    // Owning the raw inherited number directly would need an additional unsafe site.
    let result = (|| {
        let (sender, receiver) = UnixStream::pair()?;
        sendmsg::<()>(
            sender.as_raw_fd(),
            &[IoSlice::new(&[0])],
            &[ControlMessage::ScmRights(&[inherited])],
            MsgFlags::empty(),
            None,
        )?;
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        recvmsg(
            &receiver,
            &mut [IoSliceMut::new(&mut [0])],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        )?;
        control
            .drain()
            .find_map(|message| match message {
                RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
                _ => None,
            })
            .ok_or_else(|| io::Error::other("inherited inotify fd was not received"))
    })();
    let _ = nix::unistd::close(inherited);
    result
}

fn live_point(pid: &AtomicU32, is_live: impl FnOnce(u32) -> bool) -> bool {
    let pointed = pid.load(Ordering::Acquire);
    if pointed == 0 {
        return false;
    }
    if is_live(pointed) {
        return true;
    }
    let _ = pid.compare_exchange(pointed, 0, Ordering::AcqRel, Ordering::Acquire);
    false
}

fn watch(
    source: &OwnedFd,
    receiver: UnixStream,
    target: &Target,
    entries: &[(i32, PathBuf)],
    helper: &Path,
    own_namespace: (u64, u64),
    sink: impl Fn(BindRestore),
) {
    while !target.stop.load(Ordering::Acquire) {
        // Before point, retain every event on the fd, including across the park.
        let flags = if target.pid.load(Ordering::Acquire) == 0 {
            PollFlags::empty()
        } else {
            PollFlags::POLLIN
        };
        let mut fds = [
            PollFd::new(source.as_fd(), flags),
            PollFd::new(receiver.as_fd(), PollFlags::POLLIN),
        ];
        match poll(&mut fds, PollTimeout::NONE) {
            Err(Errno::EINTR) => continue,
            Err(error) => {
                sink(BindRestore {
                    paths: entries.iter().map(|(_, p)| p.clone()).collect(),
                    result: Err(error.to_string()),
                });
                return;
            }
            Ok(_) => {}
        }
        if target.stop.load(Ordering::Acquire) {
            return;
        }
        if fds[1]
            .revents()
            .is_some_and(|flags| flags.contains(PollFlags::POLLIN))
        {
            let _ = nix::unistd::read(&receiver, &mut [0; 64]);
        }
        if !fds[0]
            .revents()
            .is_some_and(|flags| flags.contains(PollFlags::POLLIN))
        {
            continue;
        }
        if !live_point(&target.pid, |pid| crate::proc::process_is_live(pid, None)) {
            continue;
        }
        let paths = match read_batch(source, entries) {
            Ok(paths) if paths.is_empty() => continue,
            Ok(paths) => paths,
            Err(error) => {
                sink(BindRestore {
                    paths: entries.iter().map(|(_, p)| p.clone()).collect(),
                    result: Err(error.to_string()),
                });
                return;
            }
        };
        let pointed = target.pid.load(Ordering::Acquire);
        let result = restore(pointed, &target.pid, helper, &paths, own_namespace);
        if let Some(result) = result {
            sink(BindRestore { paths, result });
        }
    }
}

fn read_batch(source: &OwnedFd, entries: &[(i32, PathBuf)]) -> io::Result<Vec<PathBuf>> {
    let mut buffer = [MaybeUninit::uninit(); 4096];
    let mut reader = inotify::Reader::new(source, &mut buffer);
    let mut affected = BTreeSet::new();
    loop {
        let event = match reader.next() {
            Ok(event) => event,
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => break,
            Err(error) => return Err(error.into()),
        };
        affected.extend(event_paths(
            entries,
            event.wd(),
            event.events(),
            event
                .file_name()
                .map(|name| OsStr::from_bytes(name.to_bytes())),
        ));
        if reader.is_buffer_empty() {
            break;
        }
    }
    Ok(entries
        .iter()
        .filter(|(_, path)| affected.contains(path))
        .map(|(_, path)| path.clone())
        .collect())
}

fn event_paths(
    entries: &[(i32, PathBuf)],
    wd: i32,
    mask: inotify::ReadFlags,
    name: Option<&OsStr>,
) -> Vec<PathBuf> {
    let overflow = mask.contains(inotify::ReadFlags::QUEUE_OVERFLOW);
    if !overflow && !mask.intersects(inotify::ReadFlags::CREATE | inotify::ReadFlags::MOVED_TO) {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|(watch, path)| {
            overflow || (*watch == wd && name.is_some() && path.file_name() == name)
        })
        .map(|(_, path)| path.clone())
        .collect()
}

fn namespace(pid: u32) -> Option<(u64, u64)> {
    let metadata = std::fs::metadata(format!("/proc/{pid}/ns/mnt")).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn assembled_view(root: u32, own_namespace: (u64, u64)) -> Option<u32> {
    let mut pending = VecDeque::from([root]);
    while let Some(parent) = pending.pop_front() {
        for child in crate::proc::children(parent) {
            if namespace(child).is_some_and(|ns| ns != own_namespace) {
                let parent_exe = crate::proc::exe_path(parent)?;
                let child_exe = crate::proc::exe_path(child)?;
                return (parent_exe != child_exe).then_some(child);
            }
            pending.push_back(child);
        }
    }
    None
}

fn restore(
    root: u32,
    pid: &AtomicU32,
    helper: &Path,
    paths: &[PathBuf],
    own_namespace: (u64, u64),
) -> Option<Result<(), String>> {
    let deadline = Instant::now() + VIEW_WAIT;
    let view = loop {
        let view = assembled_view(root, own_namespace);
        // /proc reads can outlast the provider, including on the timeout path.
        if pid.load(Ordering::Acquire) != root || !crate::proc::process_is_live(root, None) {
            return None;
        }
        if let Some(view) = view {
            break view;
        }
        let now = Instant::now();
        if now >= deadline {
            return Some(Err("scoped view lookup timed out".into()));
        }
        std::thread::sleep(DISCOVERY_STEP.min(deadline.saturating_duration_since(now)));
    };
    Some(run_repair(helper, view, paths))
}

fn run_repair(helper: &Path, view: u32, paths: &[PathBuf]) -> Result<(), String> {
    let child = Command::new(helper)
        .arg(HELPER_ARG)
        .arg(view.to_string())
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawning scoped bind helper: {error}"))?;
    let output = crate::proc::pump_child(
        child,
        None,
        Instant::now() + HELPER_WAIT,
        crate::proc::KillScope::Process,
    )
    .map_err(|error| error.to_string())?;
    if output.timed_out {
        return Err("scoped bind helper timed out".into());
    }
    if !output.status.success() {
        return Err(format!(
            "scoped bind helper {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub(super) fn run_bind_helper() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(HELPER_ARG)) {
        return None;
    }
    let mut stderr = io::stderr().lock();
    let Some(pid) = args
        .next()
        .and_then(|pid| pid.to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != 0)
    else {
        let _ = writeln!(
            stderr,
            "scoped bind helper requires a view pid and absolute paths"
        );
        return Some(ExitCode::from(2));
    };
    let joined = (|| -> io::Result<()> {
        let user = File::open(format!("/proc/{pid}/ns/user"))?;
        let mount = File::open(format!("/proc/{pid}/ns/mnt"))?;
        setns(user, CloneFlags::CLONE_NEWUSER)?;
        setns(mount, CloneFlags::CLONE_NEWNS)?;
        Ok(())
    })();
    if let Err(error) = joined {
        let _ = writeln!(stderr, "joining scoped view {pid}: {error}");
        return Some(ExitCode::FAILURE);
    }
    let mut failed = false;
    for path in args.map(PathBuf::from) {
        let result = if path.is_absolute() {
            restore_entry(&path)
        } else {
            Err(Errno::EINVAL)
        };
        if let Err(error) = result {
            failed = true;
            let _ = writeln!(stderr, "{path:?}: {}", io::Error::from(error));
        }
    }
    Some(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn restore_entry(path: &Path) -> nix::Result<()> {
    match nix::sys::stat::lstat(path) {
        Err(Errno::ENOENT) => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let flags = statvfs(path)?.flags();
    if flags.contains(FsFlags::ST_RDONLY) {
        return Ok(());
    }
    mount(
        Some(path),
        path,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )?;
    mount(
        None::<&str>,
        path,
        None::<&str>,
        remount_flags(flags),
        None::<&str>,
    )
}

fn remount_flags(flags: FsFlags) -> MsFlags {
    let mut mount = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY;
    for (st, ms) in [
        (FsFlags::ST_NOSUID, MsFlags::MS_NOSUID),
        (FsFlags::ST_NODEV, MsFlags::MS_NODEV),
        (FsFlags::ST_NOEXEC, MsFlags::MS_NOEXEC),
        (FsFlags::ST_NOATIME, MsFlags::MS_NOATIME),
        (FsFlags::ST_NODIRATIME, MsFlags::MS_NODIRATIME),
        (FsFlags::ST_RELATIME, MsFlags::MS_RELATIME),
    ] {
        if flags.contains(st) {
            mount |= ms;
        }
    }
    mount
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_points_pause_reads_without_clobbering_a_retarget() {
        let actual: Vec<_> = [
            (42, true, None),
            (42, false, None),
            (0, false, None),
            (42, false, Some(84)),
        ]
        .into_iter()
        .map(|(pointed, alive, retarget)| {
            let pid = AtomicU32::new(pointed);
            let mut probes = 0;
            let active = live_point(&pid, |seen| {
                assert_eq!(seen, pointed);
                probes += 1;
                if let Some(retarget) = retarget {
                    pid.store(retarget, Ordering::Release);
                }
                alive
            });
            (active, pid.load(Ordering::Acquire), probes)
        })
        .collect();
        assert_eq!(
            actual,
            vec![(true, 42, 1), (false, 0, 1), (false, 0, 0), (false, 84, 1)]
        );
    }

    #[test]
    fn events_select_only_guarded_names_and_overflow_selects_every_entry() {
        let entries = vec![
            (1, PathBuf::from("/home/user/.gitconfig")),
            (2, PathBuf::from("/repo/.git/config")),
        ];
        let cases = [
            (
                1,
                inotify::ReadFlags::MOVED_TO,
                Some(OsStr::new(".gitconfig")),
            ),
            (2, inotify::ReadFlags::CREATE, Some(OsStr::new("config"))),
            (1, inotify::ReadFlags::CREATE, Some(OsStr::new("config"))),
            (
                1,
                inotify::ReadFlags::MODIFY,
                Some(OsStr::new(".gitconfig")),
            ),
            (
                1,
                inotify::ReadFlags::MOVED_FROM,
                Some(OsStr::new(".gitconfig")),
            ),
            (1, inotify::ReadFlags::CREATE, None),
            (-1, inotify::ReadFlags::QUEUE_OVERFLOW, None),
        ];
        let actual: Vec<_> = cases
            .into_iter()
            .map(|(wd, flags, name)| event_paths(&entries, wd, flags, name))
            .collect();
        assert_eq!(
            actual,
            vec![
                vec![entries[0].1.clone()],
                vec![entries[1].1.clone()],
                vec![],
                vec![],
                vec![],
                vec![],
                entries.iter().map(|(_, p)| p.clone()).collect()
            ]
        );
    }

    #[test]
    fn remount_restates_restrictions_and_translates_relatime() {
        let keep = [
            (FsFlags::ST_NOSUID, MsFlags::MS_NOSUID),
            (FsFlags::ST_NODEV, MsFlags::MS_NODEV),
            (FsFlags::ST_NOEXEC, MsFlags::MS_NOEXEC),
            (FsFlags::ST_NOATIME, MsFlags::MS_NOATIME),
            (FsFlags::ST_NODIRATIME, MsFlags::MS_NODIRATIME),
            (FsFlags::ST_RELATIME, MsFlags::MS_RELATIME),
        ];
        let actual: Vec<_> = keep.iter().map(|(st, _)| remount_flags(*st)).collect();
        let required = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY;
        assert_eq!(
            actual,
            keep.iter()
                .map(|(_, ms)| required | *ms)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            remount_flags(FsFlags::ST_SYNCHRONOUS | FsFlags::ST_MANDLOCK),
            required
        );
    }
}
