//! A notice about a room that names no agent: the user's notification handlers, then a sidebar
//! toast in the room.

use std::path::Path;

use rimz::sidebar::notify::{Notification, NotificationKind};

/// Run the notification handlers for `kind` with this title and body, then broadcast the same
/// text as a sidebar toast to the room at `root`. Both are best-effort.
pub(super) fn send(root: &Path, kind: NotificationKind, title: String, body: String) {
    let notification = Notification {
        agents: Vec::new(),
        notification_kind: kind,
        title,
        body,
        unread_count: None,
    };
    let prefs = rimz::config::MachineConfig::load_lenient()
        .notifications
        .clone();
    rimz::sidebar::notify::spawn_notify_handlers(&prefs, &notification);
    let runtime = match rimz::RuntimePaths::for_project_root(root) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::debug!(root = %root.display(), error = %err, "notice runtime unavailable");
            return;
        }
    };
    let notification_kind = notification.kind_env().to_owned();
    if let Err(err) = rimz::wakeup::broadcast(
        &runtime,
        None,
        rimz::wakeup::events::SidebarEvent::Notify {
            title: notification.title,
            body: notification.body,
            panes: Vec::new(),
            recheck_unread: false,
            notification_kind: Some(notification_kind),
        },
    ) {
        tracing::debug!(root = %root.display(), error = %err, "notice broadcast failed");
    }
}
