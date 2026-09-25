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
) -> crate::lsp::Result<()> {
    if let Err(error) = watcher.watch(directory, notify::RecursiveMode::NonRecursive) {
        match &error.kind {
            notify::ErrorKind::PathNotFound => return Ok(()),
            notify::ErrorKind::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            _ => return Err(crate::lsp::LspErr::Protocol(error.to_string())),
        }
    }
    for entry in directory_entries(directory)? {
        let entry = entry?;
        if !excluded(Path::new(&entry.file_name())) && entry.file_type()?.is_dir() {
            register(watcher, &entry.path())?;
        }
    }
    Ok(())
}

fn directory_entries(
    directory: &Path,
) -> std::io::Result<impl Iterator<Item = std::io::Result<std::fs::DirEntry>>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(entries.into_iter().flatten())
}

pub(super) fn register_created(
    watcher: &mut impl notify::Watcher,
    root: &Path,
    mut events: Vec<Event>,
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
                register(watcher, path)?;
                directories.push(path.clone());
            }
        }
    }
    // Files can be saved before the new directory's watch is installed.
    while let Some(directory) = directories.pop() {
        for entry in directory_entries(&directory)? {
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
    events: impl IntoIterator<Item = Event>,
) -> Value {
    let mut changes = Vec::new();
    for event in events {
        for (index, path) in event.paths.iter().enumerate() {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if excluded(relative)
                || !path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| extensions.iter().any(|configured| configured == ext))
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
        register(&mut watcher, root.path()).unwrap();
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
        register_created(&mut watcher, root.path(), vec![event]).unwrap();
        std::fs::write(directory.join("saved.rs"), "fn saved() {}").unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let output = changes(root.path(), &["rs".into()], [event]);
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
            changes(Path::new("/checkout"), &["rs".into()], events),
            json!({"changes": [
                {"uri": "file:///checkout/new.rs", "type": 1}, {"uri": "file:///checkout/lib.rs", "type": 2}, {"uri": "file:///checkout/old.rs", "type": 3}, {"uri": "file:///checkout/a.rs", "type": 3}, {"uri": "file:///checkout/b.rs", "type": 1}
            ]})
        );
    }
}
