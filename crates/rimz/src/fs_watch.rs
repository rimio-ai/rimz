//! Mutation watches without Linux open-event wakeups.

#[cfg(target_os = "linux")]
pub(crate) use linux::{RecommendedWatcher, recommended_watcher};
#[cfg(not(target_os = "linux"))]
pub(crate) use notify::{RecommendedWatcher, recommended_watcher};

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::io;
    use std::mem::MaybeUninit;
    use std::net::Shutdown;
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, Flag, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use notify::{Config, Event, EventHandler, EventKind, RecursiveMode, Watcher};
    use rustix::fs::inotify::{self, ReadFlags, WatchFlags};

    pub(crate) struct RecommendedWatcher {
        shared: Arc<Shared>,
        control: UnixStream,
        worker: Option<JoinHandle<()>>,
    }

    struct Shared {
        source: OwnedFd,
        watches: Mutex<Watches>,
        stopping: AtomicBool,
    }

    #[derive(Default)]
    struct Watches {
        paths: BTreeMap<i32, (PathBuf, bool)>,
        descriptors: BTreeMap<PathBuf, i32>,
        rename: Option<Event>,
    }

    pub(crate) fn recommended_watcher<F: EventHandler>(
        handler: F,
    ) -> notify::Result<RecommendedWatcher> {
        RecommendedWatcher::new(handler, Config::default())
    }

    impl Watcher for RecommendedWatcher {
        fn new<F: EventHandler>(mut handler: F, _config: Config) -> notify::Result<Self> {
            let source =
                inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
                    .map_err(|error| notify::Error::io(error.into()))?;
            let (control, receiver) = UnixStream::pair().map_err(notify::Error::io)?;
            let shared = Arc::new(Shared {
                source,
                watches: Mutex::new(Watches::default()),
                stopping: AtomicBool::new(false),
            });
            let watching = shared.clone();
            let worker = std::thread::Builder::new()
                .name("rimz-fs-watch".into())
                .stack_size(256 * 1024)
                .spawn(move || watch(&watching, receiver, &mut handler))
                .map_err(notify::Error::io)?;
            Ok(Self {
                shared,
                control,
                worker: Some(worker),
            })
        }

        fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            if mode == RecursiveMode::Recursive {
                return Err(notify::Error::generic(
                    "recursive watches are not supported",
                ));
            }
            let path = absolute(path)?;
            let mut watches = self
                .shared
                .watches
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let is_dir = std::fs::metadata(&path)
                .map_err(|error| notify::Error::io_watch(error).add_path(path.clone()))?
                .is_dir();
            let mask = WatchFlags::ATTRIB
                | WatchFlags::CREATE
                | WatchFlags::DELETE
                | WatchFlags::CLOSE_WRITE
                | WatchFlags::MODIFY
                | WatchFlags::MOVED_FROM
                | WatchFlags::MOVED_TO
                | WatchFlags::DELETE_SELF
                | WatchFlags::MOVE_SELF;
            let wd = inotify::add_watch(&self.shared.source, &path, mask)
                .map_err(|error| watch_error(error, path.clone()))?;
            watches.descriptors.insert(path.clone(), wd);
            watches.paths.insert(wd, (path, is_dir));
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            let path = absolute(path)?;
            self.shared
                .watches
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.shared.source, &path)
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::Inotify
        }
    }

    impl Drop for RecommendedWatcher {
        fn drop(&mut self) {
            self.shared.stopping.store(true, Ordering::Release);
            let _ = self.control.shutdown(Shutdown::Both);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn absolute(path: &Path) -> notify::Result<PathBuf> {
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        Ok(std::env::current_dir()
            .map_err(notify::Error::io)?
            .join(path))
    }

    fn watch_error(error: rustix::io::Errno, path: PathBuf) -> notify::Error {
        match error {
            rustix::io::Errno::NOSPC => notify::Error::new(notify::ErrorKind::MaxFilesWatch),
            rustix::io::Errno::NOENT => notify::Error::new(notify::ErrorKind::PathNotFound),
            error => notify::Error::io(error.into()),
        }
        .add_path(path)
    }

    impl Watches {
        fn remove(&mut self, source: &OwnedFd, path: &Path) -> notify::Result<()> {
            let wd = self
                .descriptors
                .remove(path)
                .ok_or_else(|| notify::Error::watch_not_found().add_path(path.to_owned()))?;
            self.paths.remove(&wd);
            inotify::remove_watch(source, wd)
                .map_err(|error| notify::Error::io(error.into()).add_path(path.to_owned()))
        }

        fn forget(&mut self, wd: i32) {
            if let Some((path, _)) = self.paths.remove(&wd)
                && self.descriptors.get(&path) == Some(&wd)
            {
                self.descriptors.remove(&path);
            }
        }
    }

    fn watch(shared: &Shared, receiver: UnixStream, handler: &mut impl EventHandler) {
        while !shared.stopping.load(Ordering::Acquire) {
            let mut fds = [
                PollFd::new(shared.source.as_fd(), PollFlags::POLLIN),
                PollFd::new(receiver.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut fds, PollTimeout::NONE) {
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => {
                    handler.handle_event(Err(notify::Error::io(error.into())));
                    return;
                }
                Ok(_) => {}
            }
            if shared.stopping.load(Ordering::Acquire) {
                return;
            }
            match read_batch(shared) {
                Ok(events) => {
                    for event in events {
                        handler.handle_event(Ok(event));
                    }
                }
                Err(error) => {
                    handler.handle_event(Err(notify::Error::io(error)));
                    return;
                }
            }
        }
    }

    fn read_batch(shared: &Shared) -> io::Result<Vec<Event>> {
        let mut watches = shared.watches.lock().unwrap_or_else(|e| e.into_inner());
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&shared.source, &mut buffer);
        let mut pending = Vec::new();
        let mut removals = BTreeSet::new();
        while !shared.stopping.load(Ordering::Acquire) {
            let event = match reader.next() {
                Ok(event) => event,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
            let mask = event.events();
            if mask.contains(ReadFlags::IGNORED) {
                watches.forget(event.wd());
                continue;
            }
            let watched = watches.paths.get(&event.wd());
            let path = watched.map(|(path, _)| match event.file_name() {
                Some(name) => path.join(OsStr::from_bytes(name.to_bytes())),
                None => path.clone(),
            });
            let is_dir = watched.is_some_and(|(_, is_dir)| *is_dir);
            // A queued self-removal must not retire a replacement at the same path.
            let removed = if mask.contains(ReadFlags::DELETE_SELF) {
                Some(event.wd())
            } else if mask.intersects(ReadFlags::MOVED_FROM | ReadFlags::DELETE) {
                path.as_ref()
                    .and_then(|path| watches.descriptors.get(path).copied())
            } else {
                None
            };
            removals.extend(removed);
            pending.extend(events(
                mask,
                event.cookie(),
                path,
                is_dir,
                &mut watches.rename,
            ));
        }
        for wd in removals {
            let Some((path, _)) = watches.paths.get(&wd) else {
                continue;
            };
            if watches.descriptors.get(path) != Some(&wd) {
                continue;
            }
            let descendants: Vec<_> = watches
                .descriptors
                .keys()
                .filter(|watched| watched.starts_with(path))
                .cloned()
                .collect();
            for path in descendants {
                let _ = watches.remove(&shared.source, &path);
            }
        }
        Ok(pending)
    }

    fn events(
        mask: ReadFlags,
        cookie: u32,
        path: Option<PathBuf>,
        is_dir: bool,
        rename: &mut Option<Event>,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        if mask.contains(ReadFlags::QUEUE_OVERFLOW) {
            events.push(Event::new(EventKind::Other).set_flag(Flag::Rescan));
        }
        let Some(path) = path else {
            return events;
        };
        if mask.contains(ReadFlags::MOVED_FROM) {
            let event = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(path.clone())
                .set_tracker(cookie as usize);
            *rename = Some(event.clone());
            events.push(event);
        } else if mask.contains(ReadFlags::MOVED_TO) {
            events.push(
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                    .add_path(path.clone())
                    .set_tracker(cookie as usize),
            );
            if rename.as_ref().and_then(Event::tracker) == Some(cookie as usize)
                && let Some(from) = rename.take()
            {
                events.push(
                    Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                        .add_some_path(from.paths.first().cloned())
                        .add_path(path.clone())
                        .set_tracker(cookie as usize),
                );
            }
        }
        let directory = mask.contains(ReadFlags::ISDIR);
        for (flag, kind) in [
            (
                ReadFlags::MOVE_SELF,
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
            ),
            (
                ReadFlags::CREATE,
                EventKind::Create(if directory {
                    CreateKind::Folder
                } else {
                    CreateKind::File
                }),
            ),
            (
                ReadFlags::DELETE,
                EventKind::Remove(if directory {
                    RemoveKind::Folder
                } else {
                    RemoveKind::File
                }),
            ),
            (
                ReadFlags::DELETE_SELF,
                EventKind::Remove(if is_dir {
                    RemoveKind::Folder
                } else {
                    RemoveKind::File
                }),
            ),
            (
                ReadFlags::MODIFY,
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            ),
            (
                ReadFlags::CLOSE_WRITE,
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
            ),
            (
                ReadFlags::ATTRIB,
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            ),
        ] {
            if mask.contains(flag) {
                events.push(Event::new(kind).add_path(path.clone()));
            }
        }
        events
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeMap;
        use std::ffi::OsString;
        use std::fs::{self, File, OpenOptions, Permissions};
        use std::io::Write;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::sync::mpsc::{self, Receiver};
        use std::time::Duration;

        use notify::event::{AccessKind, Flag};
        use notify::{ErrorKind, EventKind, RecursiveMode};

        fn collect(rx: &Receiver<notify::Result<Event>>, count: usize) -> Vec<Event> {
            let mut events = Vec::new();
            while events.len() < count {
                let event = rx
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap_or_else(|error| panic!("{error}; events so far: {events:?}"))
                    .unwrap();
                if !matches!(event.kind, EventKind::Access(AccessKind::Open(_))) {
                    events.push(event);
                }
            }
            events
        }

        fn observe<W: Watcher>(file_only: bool) -> Vec<Event> {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("watched");
            fs::create_dir(&root).unwrap();
            let original = root.join("original");
            let renamed = root.join(OsString::from_vec(b"renamed-\xff".to_vec()));
            let outside = temp.path().join("outside");
            let (tx, rx) = mpsc::channel();
            let mut watcher = W::new(tx, Config::default()).unwrap();
            let mut events = Vec::new();
            if file_only {
                fs::write(&original, b"before watch").unwrap();
                watcher
                    .watch(&original, RecursiveMode::NonRecursive)
                    .unwrap();
            } else {
                watcher.watch(&root, RecursiveMode::NonRecursive).unwrap();
                let mut file = File::create(&original).unwrap();
                file.write_all(b"created").unwrap();
                drop(file);
                events.extend(collect(&rx, 3));
            }
            let mut file = OpenOptions::new().append(true).open(&original).unwrap();
            file.write_all(b"appended").unwrap();
            drop(file);
            events.extend(collect(&rx, 2));
            fs::set_permissions(&original, Permissions::from_mode(0o600)).unwrap();
            events.extend(collect(&rx, 1));
            fs::rename(&original, &renamed).unwrap();
            events.extend(collect(&rx, if file_only { 1 } else { 3 }));
            if file_only {
                let mut file = OpenOptions::new().append(true).open(&renamed).unwrap();
                file.write_all(b"after move").unwrap();
                drop(file);
                events.extend(collect(&rx, 2));
                fs::remove_file(&renamed).unwrap();
                events.extend(collect(&rx, 2));
            } else {
                fs::rename(&renamed, &outside).unwrap();
                events.extend(collect(&rx, 1));
                fs::rename(&outside, &original).unwrap();
                events.extend(collect(&rx, 1));
                fs::remove_file(&original).unwrap();
                events.extend(collect(&rx, 1));
                let child = root.join("child");
                fs::create_dir(&child).unwrap();
                events.extend(collect(&rx, 1));
                fs::remove_dir(&child).unwrap();
                events.extend(collect(&rx, 1));
                fs::remove_dir(&root).unwrap();
                events.extend(collect(&rx, 1));
                fs::create_dir(&root).unwrap();
                watcher.watch(&root, RecursiveMode::NonRecursive).unwrap();
                fs::write(&original, b"after re-registration").unwrap();
                events.extend(collect(&rx, 3));
            }
            let extra = rx.recv_timeout(Duration::from_millis(50));
            assert!(extra.is_err(), "unexpected event: {extra:?}");
            let mut trackers = BTreeMap::new();
            for event in &mut events {
                for path in &mut event.paths {
                    *path = path.strip_prefix(temp.path()).unwrap().to_path_buf();
                }
                if let Some(tracker) = event.tracker() {
                    let next = trackers.len();
                    *event = event
                        .clone()
                        .set_tracker(*trackers.entry(tracker).or_insert(next));
                }
            }
            events
        }

        #[test]
        fn mutations_match_notify_for_directories_and_files() {
            for file_only in [false, true] {
                assert_eq!(
                    observe::<RecommendedWatcher>(file_only),
                    observe::<notify::INotifyWatcher>(file_only),
                    "file_only: {file_only}",
                );
            }
        }

        #[test]
        fn installed_watch_masks_never_include_open() {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("file");
            fs::write(&file, b"contents").unwrap();
            let inodes =
                [temp.path(), file.as_path()].map(|path| fs::metadata(path).unwrap().ino());
            let mut watcher = recommended_watcher(|_| {}).unwrap();
            for path in [temp.path(), file.as_path()] {
                watcher.watch(path, RecursiveMode::NonRecursive).unwrap();
            }
            let masks: Vec<u32> = fs::read_dir("/proc/self/fdinfo")
                .unwrap()
                .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
                .flat_map(|info| {
                    info.lines()
                        .filter(|line| line.starts_with("inotify wd:"))
                        .filter_map(|line| {
                            let fields: BTreeMap<_, _> = line
                                .split_whitespace()
                                .filter_map(|field| field.split_once(':'))
                                .collect();
                            let inode = u64::from_str_radix(fields.get("ino")?, 16).ok()?;
                            inodes.contains(&inode).then(|| {
                                u32::from_str_radix(fields.get("mask").unwrap(), 16).unwrap()
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            assert_eq!(masks.len(), 2, "masks: {masks:x?}");
            assert!(
                masks.iter().all(|mask| mask & 0x20 == 0),
                "masks: {masks:x?}"
            );
        }

        #[test]
        fn watch_limit_error_names_the_inotify_limit() {
            let path = PathBuf::from("/watched");
            let error = watch_error(rustix::io::Errno::NOSPC, path.clone());
            assert!(matches!(error.kind, ErrorKind::MaxFilesWatch), "{error:?}");
            assert_eq!(error.paths, [path]);
        }

        #[test]
        fn recursive_watches_are_refused() {
            let temp = tempfile::tempdir().unwrap();
            let mut watcher = recommended_watcher(|_| {}).unwrap();
            let result = watcher.watch(temp.path(), RecursiveMode::Recursive);
            assert!(result.is_err(), "recursive watch result: {result:?}");
        }

        #[test]
        fn queue_overflow_requests_a_rescan_without_a_path() {
            assert_eq!(
                events(ReadFlags::QUEUE_OVERFLOW, 0, None, false, &mut None),
                [Event::new(EventKind::Other).set_flag(Flag::Rescan)],
            );
        }

        #[test]
        fn ignored_descriptor_keeps_a_replacement_watch() {
            let path = PathBuf::from("/watched");
            let mut watches = Watches::default();
            watches.paths.insert(1, (path.clone(), false));
            watches.paths.insert(2, (path.clone(), false));
            watches.descriptors.insert(path.clone(), 2);
            watches.forget(1);
            assert_eq!(watches.descriptors.get(&path), Some(&2));
            assert_eq!(watches.paths.keys().copied().collect::<Vec<_>>(), [2]);
        }
    }
}
