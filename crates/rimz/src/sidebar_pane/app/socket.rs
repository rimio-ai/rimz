use super::*;

pub(super) fn heartbeat_write_due(last_heartbeat: Option<Instant>) -> bool {
    last_heartbeat.is_none_or(|last| last.elapsed() >= HEARTBEAT_WRITE_INTERVAL)
}

/// Refresh this instance's liveness heartbeat. Written in-process — no `rimz
/// sidebar heartbeat` fork per tick — through the shared liveness helper, which
/// keeps the JSON shape and atomic write identical to what the store wakeup
/// fanout and launch freshness gate expect.
pub(super) fn write_heartbeat(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    socket_path: &Path,
) -> Result<()> {
    crate::sidebar::write_heartbeat(
        runtime,
        config.workspace_id.clone(),
        &config.instance_id,
        config.mux,
        &config.session_name,
        socket_path,
        config.own_pane.clone(),
    )
    .map_err(|err| SidebarAppErr::Heartbeat(err.to_string()))
}

pub(super) fn sidebar_socket_path(
    runtime: &RuntimePaths,
    instance_id: &SidebarInstanceId,
) -> PathBuf {
    // Use the short (12-hex) id, not the full `sb_<32 hex>`: the bound path must
    // fit the platform AF_UNIX budget, same as the per-run socket. The
    // heartbeat carries this path verbatim, so senders stay in sync.
    runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance_id.short()))
}

pub(super) fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    crate::sock::validate_socket_path(path).map_err(io::Error::other)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

/// How long the resize watcher blocks per poll. A resize event wakes it
/// immediately regardless; this only bounds how often it loops while idle.
pub(super) const RESIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Watch the terminal for resize and key events and wake the serve loop. Runs
/// on its own thread for the life of the process; it self-wakes by sending to
/// `wake_path` (the loop's bound wakeup socket), which keeps redraw and input
/// on one path. Stops quietly if the event source or socket goes away.
pub(super) fn spawn_event_waker(wake_path: PathBuf, keymap: NavKeymap) {
    std::thread::spawn(move || {
        let waker = match UnixDatagram::unbound() {
            Ok(socket) => socket,
            Err(err) => {
                warn!(error = %err, "event waker disabled; input waits for the tick");
                return;
            }
        };
        loop {
            match event::poll(RESIZE_POLL_INTERVAL) {
                Ok(true) => match event::read() {
                    Ok(Event::Resize(_, _)) => {
                        if waker.send_to(b"resize", &wake_path).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if let Some(encoded) = encode_key(&keymap, key.code, key.modifiers)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(Event::Mouse(mouse)) => {
                        if let Some(encoded) = encode_mouse(mouse.kind, mouse.column, mouse.row)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "event waker stopping: event read failed");
                        return;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    warn!(error = %err, "event waker stopping: event poll failed");
                    return;
                }
            }
        }
    });
}
