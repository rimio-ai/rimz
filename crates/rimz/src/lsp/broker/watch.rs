//! Filter and batch saved-file changes without counting them as queries.

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind};
use serde_json::{Value, json};
use std::path::Path;

fn excluded(path: &Path) -> bool {
    path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(".git" | "target" | "node_modules")
        )
    })
}

pub(super) fn register(
    watcher: &mut impl notify::Watcher,
    directory: &Path,
    report: &impl Fn(&Path, &str),
) -> crate::lsp::Result<()> {
    if let Err(error) = watcher.watch(directory, notify::RecursiveMode::NonRecursive) {
        match &error.kind {
            notify::ErrorKind::PathNotFound => return Ok(()),
            notify::ErrorKind::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            _ => {
                report(directory, &error.to_string());
                return Ok(());
            }
        }
    }
    for entry in directory_entries(directory, report)? {
        let entry = entry?;
        if !excluded(Path::new(&entry.file_name())) && entry.file_type()?.is_dir() {
            register(watcher, &entry.path(), report)?;
        }
    }
    Ok(())
}

fn directory_entries(
    directory: &Path,
    report: &impl Fn(&Path, &str),
) -> std::io::Result<impl Iterator<Item = std::io::Result<std::fs::DirEntry>>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            report(directory, &error.to_string());
            None
        }
        Err(error) => return Err(error),
    };
    Ok(entries.into_iter().flatten())
}

pub(super) fn register_created(
    watcher: &mut impl notify::Watcher,
    root: &Path,
    mut events: Vec<Event>,
    report: &impl Fn(&Path, &str),
) -> crate::lsp::Result<Vec<Event>> {
    let mut directories = Vec::new();
    for event in &events {
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
        ) {
            continue;
        }
        for path in &event.paths {
            if path
                .strip_prefix(root)
                .is_ok_and(|relative| !excluded(relative))
                && path
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.is_dir())
            {
                register(watcher, path, report)?;
                directories.push(path.clone());
            }
        }
    }
    // Files can be saved before the new directory's watch is installed.
    while let Some(directory) = directories.pop() {
        for entry in directory_entries(&directory, report)? {
            let entry = entry?;
            if excluded(Path::new(&entry.file_name())) {
                continue;
            }
            if entry.file_type()?.is_dir() {
                directories.push(entry.path());
            } else if entry.file_type()?.is_file() {
                events.push(
                    Event::new(EventKind::Create(notify::event::CreateKind::File))
                        .add_path(entry.path()),
                );
            }
        }
    }
    Ok(events)
}

pub(super) fn changes(
    root: &Path,
    extensions: &[String],
    root_markers: &[String],
    events: impl IntoIterator<Item = Event>,
) -> Value {
    let mut changes = Vec::new();
    for event in events {
        for (index, path) in event.paths.iter().enumerate() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let deleted_tree = matches!(
                event.kind,
                EventKind::Remove(notify::event::RemoveKind::Folder)
            ) || matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::From))
            ) || (matches!(
                event.kind,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both))
            ) && index == 0);
            let marker = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| root_markers.iter().any(|marker| marker == name));
            if excluded(relative)
                || (!deleted_tree
                    && !marker
                    && !path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| extensions.iter().any(|configured| configured == ext)))
            {
                continue;
            }
            let kind = match event.kind {
                EventKind::Create(_) => 1,
                EventKind::Remove(_) => 3,
                EventKind::Modify(ModifyKind::Name(RenameMode::From)) => 3,
                EventKind::Modify(ModifyKind::Name(RenameMode::To)) => 1,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                    if index == 0 {
                        3
                    } else {
                        1
                    }
                }
                EventKind::Modify(_) => 2,
                _ => continue,
            };
            if let Ok(uri) = url::Url::from_file_path(path) {
                changes.push(json!({"uri": uri.as_str(), "type": kind}));
            }
        }
    }
    json!({"changes": changes})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwatchable_directories_are_reported_without_stopping() {
        struct RefusingWatcher(Option<i32>);
        impl notify::Watcher for RefusingWatcher {
            fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
                Ok(Self(None))
            }
            fn watch(&mut self, _: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
                Err(notify::Error::new(
                    self.0.map_or(notify::ErrorKind::MaxFilesWatch, |code| {
                        notify::ErrorKind::Io(std::io::Error::from_raw_os_error(code))
                    }),
                ))
            }
            fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
                Ok(())
            }
            fn kind() -> notify::WatcherKind {
                notify::WatcherKind::PollWatcher
            }
        }
        let root = tempfile::tempdir().unwrap();
        for code in [None, Some(nix::libc::EACCES), Some(nix::libc::ENOSPC)] {
            let reports = std::cell::RefCell::new(Vec::new());
            let result = register(&mut RefusingWatcher(code), root.path(), &|path, error| {
                reports
                    .borrow_mut()
                    .push((path.to_owned(), error.to_owned()));
            });
            assert!(result.is_ok(), "watch registration must degrade, not crash");
            let reports = reports.into_inner();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].0, root.path());
            assert!(!reports[0].1.is_empty());
        }
    }

    #[test]
    fn excluded_directories_are_unwatched_and_new_directories_forward_saves() {
        let root = tempfile::tempdir().unwrap();
        for name in ["target", ".git", "node_modules"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            if event
                .as_ref()
                .is_ok_and(|event| !matches!(event.kind, EventKind::Access(_)))
            {
                let _ = tx.send(event);
            }
        })
        .unwrap();
        register(&mut watcher, root.path(), &|_, _| {}).unwrap();
        for name in ["target", ".git", "node_modules"] {
            std::fs::write(root.path().join(name).join("hidden.rs"), "").unwrap();
        }
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err()
        );
        let directory = root.path().join("new");
        std::fs::create_dir(&directory).unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        register_created(&mut watcher, root.path(), vec![event], &|_, _| {}).unwrap();
        std::fs::write(directory.join("saved.rs"), "fn saved() {}").unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let output = changes(root.path(), &["rs".into()], &[], [event]);
        assert!(
            output["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["uri"].as_str().unwrap().ends_with("/new/saved.rs"))
        );
    }

    #[test]
    fn watch_batch_filters_outputs_and_preserves_event_types() {
        let events = [
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/checkout/Cargo.toml".into()),
            Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                .add_path("/checkout/old".into()),
            Event::new(EventKind::Create(notify::event::CreateKind::File))
                .add_path("/checkout/new.rs".into()),
            Event::new(EventKind::Modify(ModifyKind::Data(
                notify::event::DataChange::Any,
            )))
            .add_path("/checkout/lib.rs".into()),
            Event::new(EventKind::Remove(notify::event::RemoveKind::File))
                .add_path("/checkout/old.rs".into()),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path("/checkout/a.rs".into())
                .add_path("/checkout/b.rs".into()),
            Event::new(EventKind::Create(notify::event::CreateKind::File))
                .add_path("/checkout/target/generated.rs".into())
                .add_path("/checkout/readme.md".into())
                .add_path("/elsewhere/lib.rs".into()),
        ];
        assert_eq!(
            changes(
                Path::new("/checkout"),
                &["rs".into()],
                &["Cargo.toml".into()],
                events
            ),
            json!({"changes": [
                {"uri": "file:///checkout/Cargo.toml", "type": 2},
                {"uri": "file:///checkout/old", "type": 3},
                {"uri": "file:///checkout/new.rs", "type": 1}, {"uri": "file:///checkout/lib.rs", "type": 2}, {"uri": "file:///checkout/old.rs", "type": 3}, {"uri": "file:///checkout/a.rs", "type": 3}, {"uri": "file:///checkout/b.rs", "type": 1}
            ]})
        );
    }
}
