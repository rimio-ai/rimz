use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::WorkspaceId;
use rimz::mux::{
    LayoutColumn, MuxBackend, PaneCmd, SidebarPaneOptions, SidebarWidth, WidthPermille,
    ZellijBackend,
};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, ScrubSessionEnvExt, ZellijNamespace};

pub(in crate::backend::zellij) const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);
pub(in crate::backend::zellij) const LIST_PANES_JSON_TIMEOUT: Duration =
    Duration::from_millis(1500);
pub(in crate::backend::zellij) const LIST_PANES_JSON_ATTEMPTS: u32 = 5;
pub(in crate::backend::zellij) const LIST_PANES_JSON_RETRY_DELAY: Duration =
    Duration::from_millis(50);
pub(in crate::backend::zellij) const DUMP_LAYOUT_ATTEMPTS: u32 = 10;
pub(in crate::backend::zellij) const DUMP_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(100);

pub(in crate::backend::zellij) fn tiled_column(panes: Vec<PaneCmd>) -> LayoutColumn {
    LayoutColumn {
        panes,
        stacked: false,
    }
}

pub(in crate::backend::zellij) fn sidebar_opts(
    name: &str,
    cwd: &Path,
    stub: PathBuf,
    detected_cols: u16,
) -> SidebarPaneOptions {
    let workspace_root = PathBuf::from(format!("/tmp/rimz-{name}"));
    let width = SidebarWidth::default();
    let view_cols = std::num::NonZeroU16::new(detected_cols).expect("nonzero test view");
    let requested_cols = std::num::NonZeroU16::new(
        u16::try_from(width.target_cols(u64::from(detected_cols))).expect("test target"),
    )
    .expect("nonzero test width");
    let share =
        WidthPermille::from_cols(requested_cols, view_cols).snap_to_rung(rimz::MuxName::Zellij);
    let target = rimz::mux::SidebarTarget {
        share,
        max_cols: width.max_cols,
        pinned: false,
    };
    SidebarPaneOptions {
        session_name: name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(&workspace_root),
        project_root: cwd.to_path_buf(),
        extra_env: BTreeMap::from([(
            "RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT".to_owned(),
            "1".to_owned(),
        )]),
        cwd: cwd.to_path_buf(),
        target,
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

/// Publish the build-stable room executable that production records before a
/// Zellij birth. Presence topology must use this pointer rather than falling
/// back to an unrelated `rimz` installed on the test runner's PATH.
pub(in crate::backend::zellij) fn publish_room_bin(state_root: &Path, opts: &SidebarPaneOptions) {
    let state = rimz::StatePaths::under(opts.workspace_id.clone(), state_root)
        .expect("test room state paths");
    state.ensure_dirs().expect("test room state dirs");
    std::fs::copy(&opts.rimz_bin, &state.room_bin).expect("publish test room binary");
    rimz::store::workspace_record::write(
        &state,
        &rimz::WorkspaceRecord {
            workspace_id: opts.workspace_id.clone(),
            project_root: opts.project_root.clone(),
            worktree_root: None,
            session_name: opts.session_name.clone(),
            root_class: rimz::workspace::RootClass::Directory,
            rimz_bin: Some(state.room_bin.clone()),
            rimz_build: None,
            updated_at: jiff::Timestamp::now(),
        },
    )
    .expect("publish test workspace record");
}

/// Owns one live Zellij session and the namespace all of its processes share.
pub(in crate::backend::zellij) struct LiveZellijSession {
    namespace: ZellijNamespace,
    name: String,
    backend: ZellijBackend,
}

impl LiveZellijSession {
    /// Create an isolated, not-yet-started session scope.
    pub(in crate::backend::zellij) fn new(prefix: &str) -> Self {
        Self::from_namespace(ZellijNamespace::new(), unique_session_name(prefix))
    }

    /// Adopt a preconfigured namespace and explicit session name.
    pub(in crate::backend::zellij) fn from_namespace(
        namespace: ZellijNamespace,
        name: impl Into<String>,
    ) -> Self {
        let backend = ZellijBackend::with_runtime_dir(namespace.path());
        Self {
            namespace,
            name: name.into(),
            backend,
        }
    }

    pub(in crate::backend::zellij) fn name(&self) -> &str {
        &self.name
    }

    pub(in crate::backend::zellij) fn path(&self) -> &Path {
        self.namespace.path()
    }

    pub(in crate::backend::zellij) fn namespace(&self) -> &ZellijNamespace {
        &self.namespace
    }

    pub(in crate::backend::zellij) fn backend(&self) -> &ZellijBackend {
        &self.backend
    }

    pub(in crate::backend::zellij) fn command(&self) -> std::process::Command {
        self.namespace.command()
    }

    /// Birth a detached session with Zellij's default shell pane.
    pub(in crate::backend::zellij) fn create_background(&self) {
        let output = self
            .command()
            .args(["attach", "--create-background", self.name()])
            .bounded_output()
            .expect("create background session");
        assert!(
            output.status.success(),
            "create-background failed for {}: {}",
            self.name,
            String::from_utf8_lossy(&output.stderr),
        );
        self.wait_until_ready();
    }

    /// Birth a detached session with one long-lived work pane.
    pub(in crate::backend::zellij) fn create_plain_background(&self, cwd: &Path, sleep: &str) {
        let layout = cwd.join(format!("{}.kdl", self.name));
        std::fs::write(
            &layout,
            format!(
                "layout {{\n    pane command=\"sleep\" {{\n        args \"{sleep}\"\n    }}\n}}\n"
            ),
        )
        .expect("write plain layout");
        let created = self
            .command()
            .args(["attach", "--create-background", self.name(), "options"])
            .arg("--default-cwd")
            .arg(cwd)
            .arg("--default-layout")
            .arg(&layout)
            .bounded_status()
            .expect("create plain session");
        assert!(
            created.success(),
            "create-background failed for {}",
            self.name
        );
        self.wait_until_ready();
    }

    /// Gate on the session action server rather than its earlier listing.
    pub(in crate::backend::zellij) fn wait_until_ready(&self) {
        let deadline = Instant::now() + SPAWN_TIMEOUT;
        loop {
            let ready = self
                .command()
                .args(["--session", self.name(), "action", "query-tab-names"])
                .bounded_output()
                .is_ok_and(|out| out.status.success());
            if ready {
                return;
            }
            if Instant::now() > deadline {
                panic!(
                    "zellij session {} never became ready for actions",
                    self.name
                );
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

impl Drop for LiveZellijSession {
    fn drop(&mut self) {
        self.namespace.delete_session(&self.name);
    }
}

pub(in crate::backend::zellij) fn wait_for_live_session(
    backend: &ZellijBackend,
    name: &str,
) -> Vec<String> {
    super::actions::poll_until(
        Duration::from_secs(15),
        || backend.list_sessions().map_err(|err| err.to_string()),
        |sessions| sessions.iter().any(|session| session == name),
        &format!("live session {name}"),
    )
}

pub(in crate::backend::zellij) fn capture_pty_output_until(
    spec: &rimz::mux::CommandSpec,
    timeout: Duration,
    mut ready: impl FnMut(&[u8]) -> bool,
) -> Vec<u8> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.scrub_session_env();
    cmd.args(spec.args.iter().map(String::as_str));
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn zellij");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    while !ready(&output) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match output_rx.recv_timeout(Duration::from_millis(100).min(deadline - now)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    reader_thread.join().expect("join reader");
    for chunk in output_rx.try_iter() {
        output.extend_from_slice(&chunk);
    }
    output
}

/// A session name no concurrent test can also draw. Keep random UUID bits
/// because the lineage reaper scans `/proc` outside the namespace boundary.
pub(in crate::backend::zellij) fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}-{}", &id[6..12], &id[26..32])
}

pub(in crate::backend::zellij) fn sidebar_command_stub() -> (TempDir, PathBuf) {
    sidebar_stub_alive_for(30)
}

pub(in crate::backend::zellij) fn sidebar_stub_alive_for(seconds: u32) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    let rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = sidebar ] && [ \"$2\" = wake ]; then\n\
             \texec {} \"$@\"\n\
             fi\n\
             sleep {seconds}\n",
            shell_quote(&rimz.display().to_string()),
        ),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    (dir, path)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
