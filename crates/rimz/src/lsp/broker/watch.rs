//! Filter and batch saved-file changes without counting them as queries.

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind};
use serde_json::{Value, json};
use std::path::Path;

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
            if relative.components().any(|part| {
                matches!(
                    part.as_os_str().to_str(),
                    Some(".git" | "target" | "node_modules")
                )
            }) || !path
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
