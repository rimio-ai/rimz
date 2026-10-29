//! Symlink-safe, hardlink-aware disk usage for `doctor`, `uninstall`, and `gc`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::disk::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpace {
    pub free: u64,
    pub total: u64,
}

/// Available (unprivileged) space and capacity on the filesystem holding `path`.
// The statvfs field widths differ by platform; conversion is a no-op on Linux.
#[allow(clippy::useless_conversion)]
pub fn disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    #[cfg(any(test, feature = "testkit"))]
    if let Ok(raw) = std::env::var("RIMZ_TEST_DISK_SPACE") {
        let (free, total) = raw
            .split_once(',')
            .and_then(|(free, total)| Some((free.parse().ok()?, total.parse().ok()?)))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RIMZ_TEST_DISK_SPACE must be <free>,<total>",
                )
            })?;
        return Ok(DiskSpace { free, total });
    }
    let stat = nix::sys::statvfs::statvfs(path)?;
    let fragment = u64::from(stat.fragment_size());
    Ok(DiskSpace {
        free: u64::from(stat.blocks_available()).saturating_mul(fragment),
        total: u64::from(stat.blocks()).saturating_mul(fragment),
    })
}

/// Best-effort recursive size for a path without following symlinks.
pub(crate) fn dir_size(path: &Path) -> u64 {
    dir_size_inner(
        path,
        &mut HashSet::new(),
        &fs::Metadata::len,
        &mut |_, _, _| {},
    )
    .bytes
}

/// Best-effort allocated size, counting hardlinked files once and not following symlinks.
pub(crate) fn dir_allocated_size(path: &Path) -> u64 {
    dir_size_inner(
        path,
        &mut HashSet::new(),
        &|meta| meta.blocks().saturating_mul(512),
        &mut |_, _, _| {},
    )
    .bytes
}

#[derive(Debug, Default)]
struct TreeUsage {
    entries: u64,
    bytes: u64,
}

fn dir_size_inner(
    path: &Path,
    seen_files: &mut HashSet<FileIdentity>,
    size: &impl Fn(&fs::Metadata) -> u64,
    visit: &mut impl FnMut(&Path, &fs::Metadata, &TreeUsage),
) -> TreeUsage {
    match fs::symlink_metadata(path) {
        Ok(meta) => walk_entry(path, &meta, seen_files, size, visit),
        Err(_) => TreeUsage::default(),
    }
}

fn walk_entry(
    path: &Path,
    meta: &fs::Metadata,
    seen_files: &mut HashSet<FileIdentity>,
    size: &impl Fn(&fs::Metadata) -> u64,
    visit: &mut impl FnMut(&Path, &fs::Metadata, &TreeUsage),
) -> TreeUsage {
    let mut usage = TreeUsage {
        entries: 0,
        bytes: if file_identity(meta).is_some_and(|identity| !seen_files.insert(identity)) {
            0
        } else {
            size(meta)
        },
    };
    if meta.is_dir()
        && !meta.file_type().is_symlink()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            let child = dir_size_inner(&entry.path(), seen_files, size, visit);
            usage.entries = usage
                .entries
                .saturating_add(1)
                .saturating_add(child.entries);
            usage.bytes = usage.bytes.saturating_add(child.bytes);
        }
    }
    visit(path, meta, &usage);
    usage
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> Option<FileIdentity> {
    meta.is_file().then(|| FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_meta: &fs::Metadata) -> Option<FileIdentity> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    Home,
    Runtime,
}

impl StorageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageRoot {
    pub kind: StorageKind,
    pub path: PathBuf,
    pub bytes: u64,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStorage {
    pub roots: Vec<StorageRoot>,
    pub room: Option<RoomStorage>,
}

/// Room totals, including root identity files, attributed during the home walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomStorage {
    pub path: PathBuf,
    pub present: bool,
    pub entries: u64,
    pub bytes: u64,
    pub folders: Vec<FolderStorage>,
}

impl RoomStorage {
    fn record(&mut self, path: &Path, meta: &fs::Metadata, usage: &TreeUsage) {
        if path == self.path {
            self.present = true;
            self.entries = usage.entries;
            self.bytes = usage.bytes;
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        if parent == self.path {
            if let Some(folder) = self.folders.iter_mut().find(|folder| folder.path == path) {
                folder.present = true;
                folder.entries = usage.entries;
                folder.bytes = usage.bytes;
            }
            return;
        }
        if !meta.is_dir() || parent.parent() != Some(self.path.as_path()) {
            return;
        }
        let Some(folder) = self.folders.iter_mut().find(|folder| folder.path == parent) else {
            return;
        };
        let Some(name) = path.file_name() else {
            return;
        };
        let entries = usage.entries.saturating_add(1);
        if folder.largest.as_ref().is_none_or(|largest| {
            entries > largest.entries
                || (entries == largest.entries && name < largest.name.as_os_str())
        }) {
            folder.largest = Some(LargestDirectory {
                name: name.to_owned(),
                entries,
            });
        }
    }
}

/// Entries exclude the folder; apparent bytes include it and deduplicate files home-wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderStorage {
    pub class: paths::Class,
    pub path: PathBuf,
    pub present: bool,
    pub entries: u64,
    pub bytes: u64,
    pub largest: Option<LargestDirectory>,
}

/// A direct child directory, counted with its own name and every name below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LargestDirectory {
    pub name: std::ffi::OsString,
    pub entries: u64,
}

impl RuntimeStorage {
    pub fn total_bytes(&self) -> u64 {
        self.roots
            .iter()
            .fold(0_u64, |total, root| total.saturating_add(root.bytes))
    }
}

pub fn measure(room: Option<&paths::StatePaths>) -> RuntimeStorage {
    measure_under(
        &[
            (StorageKind::Home, paths::rimz_home()),
            (StorageKind::Runtime, paths::runtime_rimz_root()),
        ],
        room,
    )
}

fn measure_under(
    roots: &[(StorageKind, PathBuf)],
    room: Option<&paths::StatePaths>,
) -> RuntimeStorage {
    let mut room = room.map(|room| RoomStorage {
        path: room.root.clone(),
        present: false,
        entries: 0,
        bytes: 0,
        folders: paths::Class::STATE
            .into_iter()
            .map(|class| FolderStorage {
                class,
                path: class.path_under(&room.root),
                present: false,
                entries: 0,
                bytes: 0,
                largest: None,
            })
            .collect(),
    });
    let roots = roots
        .iter()
        .map(|(kind, path)| StorageRoot {
            kind: *kind,
            path: path.clone(),
            present: path.exists(),
            bytes: measure_root(path, &mut |path, meta, usage| {
                if *kind == StorageKind::Home
                    && let Some(room) = &mut room
                {
                    room.record(path, meta, usage);
                }
            }),
        })
        .collect();
    RuntimeStorage { roots, room }
}

/// Walks a storage root, following the root itself when it is a symlink (a `~/.rimz` linked
/// into a dotfiles checkout) so its entries keep the configured path; links below stay unfollowed.
fn measure_root(path: &Path, visit: &mut impl FnMut(&Path, &fs::Metadata, &TreeUsage)) -> u64 {
    let Ok(meta) = fs::metadata(path) else {
        return 0;
    };
    walk_entry(path, &meta, &mut HashSet::new(), &fs::Metadata::len, visit).bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn disk_space_reports_available_capacity() {
        let temp = tempdir().unwrap();
        let space = disk_space(temp.path()).unwrap();
        assert!(space.total > 0);
        assert!(space.free <= space.total);
        assert!(disk_space(&temp.path().join("missing")).is_err());
    }

    #[test]
    fn disk_space_override_is_process_local() {
        if let Ok(raw) = std::env::var("RIMZ_TEST_DISK_SPACE") {
            let result = disk_space(Path::new("/does-not-exist"));
            if raw == "1,100" {
                assert_eq!(
                    result.unwrap(),
                    DiskSpace {
                        free: 1,
                        total: 100
                    }
                );
            } else {
                assert!(result.is_err(), "malformed override must be refused");
            }
            return;
        }
        for raw in ["1,100", "invalid"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "disk::usage::tests::disk_space_override_is_process_local",
                ])
                .env("RIMZ_TEST_DISK_SPACE", raw)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn dir_size_sums_nested_tree() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(nested.join("b.txt"), b"bravo!").unwrap();

        assert!(
            dir_size(&root) >= 11,
            "file payload bytes are included with directory entries"
        );
        assert_eq!(dir_size(&temp.path().join("missing")), 0);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("large.bin"), vec![0_u8; 16 * 1024]).unwrap();
        symlink(&outside, root.join("linked")).unwrap();

        assert!(
            dir_size(&root) < 16 * 1024,
            "symlink target contents are not charged to the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_counts_hardlinked_payload_once() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        fs::write(&source, vec![0_u8; 16 * 1024]).unwrap();
        fs::hard_link(&source, root.join("alias.bin")).unwrap();

        let expected = fs::symlink_metadata(&root).unwrap().len()
            + fs::symlink_metadata(&source).unwrap().len();
        assert_eq!(dir_size(&root), expected);
    }

    #[cfg(unix)]
    #[test]
    fn allocated_size_counts_blocks_once_without_following_links() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let source = root.join("source");
        fs::write(&source, vec![1_u8; 16 * 1024]).unwrap();
        fs::hard_link(&source, nested.join("alias")).unwrap();
        let sparse = root.join("sparse");
        fs::File::create(&sparse)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, vec![2_u8; 32 * 1024]).unwrap();
        let link = root.join("link");
        symlink(outside, &link).unwrap();
        let expected: u64 = [&root, &nested, &source, &sparse, &link]
            .into_iter()
            .map(|path| fs::symlink_metadata(path).unwrap().blocks() * 512)
            .sum();

        assert!(expected > 0);
        assert_eq!(dir_allocated_size(&root), expected);
        assert!(dir_allocated_size(&root) < dir_size(&root));
        assert_eq!(dir_allocated_size(&temp.path().join("missing")), 0);
    }

    #[test]
    fn measure_under_reports_roots_and_total() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let runtime = temp.path().join("runtime");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("store.json"), b"{}").unwrap();

        let disk_usage = measure_under(
            &[
                (StorageKind::Home, state.clone()),
                (StorageKind::Runtime, runtime.clone()),
            ],
            None,
        );

        assert_eq!(disk_usage.roots[0].kind, StorageKind::Home);
        assert_eq!(disk_usage.roots[0].path, state);
        assert!(disk_usage.roots[0].present);
        assert!(disk_usage.roots[0].bytes >= 2);
        assert_eq!(disk_usage.roots[1].kind, StorageKind::Runtime);
        assert_eq!(disk_usage.roots[1].path, runtime);
        assert!(!disk_usage.roots[1].present);
        assert_eq!(disk_usage.roots[1].bytes, 0);
        assert!(disk_usage.room.is_none());
        assert_eq!(
            disk_usage.total_bytes(),
            disk_usage
                .roots
                .iter()
                .fold(0_u64, |total, root| total.saturating_add(root.bytes))
        );
    }

    #[test]
    fn measure_room_counts_folders_and_root_strays_inside_home_totals() {
        let temp = tempdir().unwrap();
        let paths = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        let shared = &paths.room_shared_dir;
        fs::create_dir_all(shared.join("run/nested")).unwrap();
        fs::write(shared.join("run/nested/file"), "payload").unwrap();
        fs::write(shared.join("flat"), "flat").unwrap();
        fs::create_dir_all(&paths.cache_dir).unwrap();
        for name in ["a", "b", "c"] {
            fs::create_dir(paths.cache_dir.join(name)).unwrap();
        }
        fs::write(&paths.workspace_record, "identity").unwrap();

        let usage = measure_under(&[(StorageKind::Home, temp.path().to_owned())], Some(&paths));
        let room = usage.room.unwrap();
        let actual: Vec<_> = room
            .folders
            .iter()
            .map(|folder| (folder.class, folder.present, folder.entries, folder.bytes))
            .collect();
        let expected: Vec<_> = paths::Class::STATE
            .into_iter()
            .map(|class| {
                let entries = match class {
                    paths::Class::Shared => 4,
                    paths::Class::Cache => 3,
                    _ => 0,
                };
                let path = class.path_under(&paths.root);
                (class, path.exists(), entries, dir_size(&path))
            })
            .collect();
        assert_eq!(actual, expected);
        assert!(room.present);
        assert_eq!(room.path, paths.root);
        assert_eq!(room.entries, 10);
        assert_eq!(room.bytes, dir_size(&paths.root));
        assert_eq!(usage.roots[0].bytes, dir_size(temp.path()));
        assert_eq!(
            room.folders[6].largest,
            Some(LargestDirectory {
                name: "run".into(),
                entries: 3,
            })
        );
        assert!(room.folders[0].largest.is_none());

        let missing = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(&temp.path().join("missing")),
            temp.path(),
        )
        .unwrap();
        let room = measure_under(
            &[(StorageKind::Home, temp.path().to_owned())],
            Some(&missing),
        )
        .room
        .unwrap();
        assert!(!room.present);
        assert_eq!((room.entries, room.bytes), (0, 0));
        assert_eq!(room.folders.len(), 10);
        assert!(room.folders.iter().all(|folder| !folder.present
            && folder.entries == 0
            && folder.bytes == 0
            && folder.largest.is_none()));
    }

    #[cfg(unix)]
    #[test]
    fn measure_room_counts_hardlink_names_without_following_symlinks() {
        let temp = tempdir().unwrap();
        let paths = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        fs::create_dir_all(&paths.room_shared_dir).unwrap();
        fs::create_dir_all(&paths.cache_dir).unwrap();
        let source = paths.room_shared_dir.join("source");
        fs::write(&source, [0; 16_384]).unwrap();
        fs::hard_link(&source, paths.room_shared_dir.join("alias")).unwrap();
        fs::hard_link(&source, paths.cache_dir.join("alias")).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), [0; 32_768]).unwrap();
        let link = paths.room_shared_dir.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let usage = measure_under(&[(StorageKind::Home, temp.path().to_owned())], Some(&paths));
        let room = usage.room.unwrap();
        assert_eq!(room.folders[6].entries, 3);
        assert_eq!(room.folders[3].entries, 1);
        assert!(room.folders[6].largest.is_none());
        let expected: u64 = [
            &paths.root,
            &paths.room_shared_dir,
            &paths.cache_dir,
            &source,
            &link,
        ]
        .into_iter()
        .map(|path| fs::symlink_metadata(path).unwrap().len())
        .sum();
        assert_eq!(room.bytes, expected);
        assert_eq!(room.entries, 6);
        assert_eq!(usage.roots[0].bytes, dir_size(temp.path()));
        assert_eq!(
            room.folders[3].bytes + room.folders[6].bytes,
            expected - fs::metadata(&paths.root).unwrap().len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn measure_follows_a_symlinked_home_root_only() {
        let temp = tempdir().unwrap();
        let real = temp.path().join("dotfiles-home");
        fs::create_dir(&real).unwrap();
        let home = temp.path().join("home");
        std::os::unix::fs::symlink(&real, &home).unwrap();
        let paths =
            paths::StatePaths::under(crate::WorkspaceId::from_project_root(temp.path()), &home)
                .unwrap();
        fs::create_dir_all(paths.room_shared_dir.join("run")).unwrap();
        fs::write(paths.room_shared_dir.join("run/report"), "report").unwrap();

        let usage = measure_under(&[(StorageKind::Home, home.clone())], Some(&paths));
        let room = usage.room.unwrap();
        assert!(room.present);
        assert_eq!(room.folders[6].entries, 2);
        assert_eq!(usage.roots[0].bytes, dir_size(&real));
        assert_eq!(dir_size(&home), fs::symlink_metadata(&home).unwrap().len());
    }

    #[test]
    fn measure_room_largest_directory_breaks_ties_by_name_and_ignores_files() {
        let temp = tempdir().unwrap();
        let paths = paths::StatePaths::under(
            crate::WorkspaceId::from_project_root(temp.path()),
            temp.path(),
        )
        .unwrap();
        for name in ["z", "a"] {
            fs::create_dir_all(paths.room_shared_dir.join(name)).unwrap();
        }
        fs::write(paths.room_shared_dir.join("large-file"), [0; 16_384]).unwrap();
        fs::create_dir_all(&paths.cache_dir).unwrap();
        fs::write(paths.cache_dir.join("flat"), "flat").unwrap();

        let room = measure_under(&[(StorageKind::Home, temp.path().to_owned())], Some(&paths))
            .room
            .unwrap();
        assert_eq!(
            room.folders[6].largest,
            Some(LargestDirectory {
                name: "a".into(),
                entries: 1,
            })
        );
        assert!(room.folders[3].largest.is_none());
    }
}
