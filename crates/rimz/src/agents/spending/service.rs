//! Elected user-scoped service for the warm spending cursor.
//!
//! Any long-lived RimZ process may win the lifetime lock and host the service
//! thread. Durable spending publications remain authoritative; this socket only
//! serializes access to one in-memory [`super::SpendingWalker`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{HeadlineSpec, SPENDING_CACHE_VERSION, SpendProgress, SpendingCaches, SpendingWalker};
use crate::ids::WorkspaceId;
use crate::store::paths::RuntimePaths;

pub const SPENDING_SERVICE_PROTOCOL_VERSION: u32 = 1;
const CONNECT_WAIT_STEP: Duration = Duration::from_millis(20);
const CONNECT_WAIT_STEPS: u32 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_FRAME_BYTES: u64 = 4 * 1024 * 1024;

/// Validated inputs for one account-global refresh and optional workspace
/// publication. Output paths and scope hashes are deliberately absent: the
/// owner derives both from the typed workspace id and normalized roots.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpendingServiceRequest {
    protocol_version: u32,
    cache_version: u32,
    pub(crate) workspace_id: Option<WorkspaceId>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) worktree_roots: Vec<PathBuf>,
    pub(crate) worktree_home: Option<PathBuf>,
    pub(crate) origin_overrides: HashMap<PathBuf, PathBuf>,
    pub(crate) headline: HeadlineSpec,
    progress_frames: bool,
}

impl SpendingServiceRequest {
    pub fn global(headline: HeadlineSpec, progress_frames: bool) -> Self {
        Self {
            protocol_version: SPENDING_SERVICE_PROTOCOL_VERSION,
            cache_version: SPENDING_CACHE_VERSION,
            workspace_id: None,
            project_root: None,
            worktree_roots: Vec::new(),
            worktree_home: None,
            origin_overrides: HashMap::new(),
            headline,
            progress_frames,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn workspace(
        workspace_id: WorkspaceId,
        project_root: Option<PathBuf>,
        worktree_roots: Vec<PathBuf>,
        worktree_home: Option<PathBuf>,
        origin_overrides: HashMap<PathBuf, PathBuf>,
        headline: HeadlineSpec,
        progress_frames: bool,
    ) -> Self {
        Self {
            protocol_version: SPENDING_SERVICE_PROTOCOL_VERSION,
            cache_version: SPENDING_CACHE_VERSION,
            workspace_id: Some(workspace_id),
            project_root,
            worktree_roots,
            worktree_home,
            origin_overrides,
            headline,
            progress_frames,
        }
    }

    fn validate(mut self) -> std::result::Result<Self, SpendingServiceFailure> {
        if self.protocol_version != SPENDING_SERVICE_PROTOCOL_VERSION {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::VersionMismatch,
                format!(
                    "spending service protocol {} is incompatible with {}",
                    self.protocol_version, SPENDING_SERVICE_PROTOCOL_VERSION
                ),
            ));
        }
        if self.cache_version != SPENDING_CACHE_VERSION {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::VersionMismatch,
                format!(
                    "spending cache version {} is incompatible with {}",
                    self.cache_version, SPENDING_CACHE_VERSION
                ),
            ));
        }
        if self.workspace_id.is_none()
            && (self.project_root.is_some()
                || !self.worktree_roots.is_empty()
                || self.worktree_home.is_some()
                || !self.origin_overrides.is_empty())
        {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::InvalidRequest,
                "workspace roots require a workspace id",
            ));
        }
        normalize_optional(&mut self.project_root, "project root")?;
        for root in &mut self.worktree_roots {
            normalize_absolute(root, "worktree root")?;
        }
        normalize_optional(&mut self.worktree_home, "worktree home")?;
        let mut origins = HashMap::with_capacity(self.origin_overrides.len());
        for (mut transcript, mut origin) in self.origin_overrides {
            normalize_absolute(&mut transcript, "transcript path")?;
            normalize_absolute(&mut origin, "transcript origin")?;
            origins.insert(transcript, origin);
        }
        self.origin_overrides = origins;
        Ok(self)
    }
}

fn normalize_optional(
    path: &mut Option<PathBuf>,
    field: &'static str,
) -> std::result::Result<(), SpendingServiceFailure> {
    if let Some(path) = path {
        normalize_absolute(path, field)?;
    }
    Ok(())
}

fn normalize_absolute(
    path: &mut PathBuf,
    field: &'static str,
) -> std::result::Result<(), SpendingServiceFailure> {
    if !path.is_absolute() {
        return Err(SpendingServiceFailure::new(
            SpendingServiceErrorCode::InvalidPath,
            format!("{field} must be absolute: {}", path.display()),
        ));
    }
    *path = crate::worktree::normalize_path_lexical(path);
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum SpendingServiceFrame {
    Progress(SpendProgress),
    #[serde(with = "spending_caches_json")]
    Complete(Box<SpendingCaches>),
    Error(SpendingServiceFailure),
}

/// Serde's tagged-enum content layer does not preserve JSON's numeric-map-key
/// coercion. Keep the durable cache shape untouched by carrying the final typed
/// aggregate as one nested JSON string on this private wire.
mod spending_caches_json {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    use super::SpendingCaches;

    pub fn serialize<S>(value: &SpendingCaches, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&serde_json::to_string(value).map_err(serde::ser::Error::custom)?)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Box<SpendingCaches>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        serde_json::from_str(&value)
            .map(Box::new)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendingServiceErrorCode {
    InvalidRequest,
    InvalidPath,
    VersionMismatch,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct SpendingServiceFailure {
    pub code: SpendingServiceErrorCode,
    pub message: String,
}

impl SpendingServiceFailure {
    fn new(code: SpendingServiceErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpendingServiceClientError {
    #[error("spending service unavailable: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid spending service frame: {0}")]
    Protocol(String),
    #[error(transparent)]
    Service(#[from] SpendingServiceFailure),
}

pub type Result<T> = std::result::Result<T, SpendingServiceClientError>;

/// Connect to the current owner, electing an in-process service thread on
/// absence. A failed transaction retries ownership once so process exit between
/// connect and response heals without retaining a local fallback walker.
pub fn request(
    runtime: &RuntimePaths,
    request: SpendingServiceRequest,
    mut progress: impl FnMut(SpendProgress),
) -> Result<SpendingCaches> {
    let mut last_error = None;
    for attempt in 0..2 {
        match connect_or_start(runtime).and_then(|stream| transact(stream, &request, &mut progress))
        {
            Ok(caches) => return Ok(caches),
            Err(error) => {
                tracing::debug!(attempt, error = %error, "spending service request failed");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        SpendingServiceClientError::Protocol("spending service retry produced no result".to_owned())
    }))
}

fn transact(
    mut stream: UnixStream,
    request: &SpendingServiceRequest,
    progress: &mut dyn FnMut(SpendProgress),
) -> Result<SpendingCaches> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    write_json_line(&mut stream, request)?;
    let mut reader = BufReader::new(stream);
    loop {
        let frame: SpendingServiceFrame = read_json_line(&mut reader)?;
        match frame {
            SpendingServiceFrame::Progress(value) => progress(value),
            SpendingServiceFrame::Complete(caches) => return Ok(*caches),
            SpendingServiceFrame::Error(error) => return Err(error.into()),
        }
    }
}

fn connect_or_start(runtime: &RuntimePaths) -> Result<UnixStream> {
    runtime
        .ensure_dirs()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
    );
    crate::sock::validate_socket_path(&socket).map_err(std::io::Error::other)?;
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }

    let owner_lock = runtime.shared_spending_service_owner_lock(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
    );
    match crate::store::single_flight::coordinate::<()>(&owner_lock, CONNECT_WAIT_STEP, 0, || None)
    {
        crate::store::single_flight::Coordination::Produce(owner) => {
            // The lifetime-lock winner alone may distinguish a stale socket
            // from a live listener and unlink it.
            match std::fs::remove_file(&socket) {
                Ok(()) => {
                    tracing::debug!(socket = %socket.display(), "removed stale spending service socket")
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let listener = UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            let server_runtime = runtime.clone();
            std::thread::Builder::new()
                .name("rimz-spending-service".to_owned())
                .spawn(move || serve(listener, owner, server_runtime))?;
            tracing::debug!(socket = %socket.display(), "elected spending service owner");
        }
        crate::store::single_flight::Coordination::ContentionTimeout => {}
        crate::store::single_flight::Coordination::Unavailable => {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Unavailable,
                "spending service owner lock unavailable",
            )
            .into());
        }
        crate::store::single_flight::Coordination::Shared(()) => {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Internal,
                "spending service election returned an unexpected shared value",
            )
            .into());
        }
    }

    for _ in 0..CONNECT_WAIT_STEPS {
        if let Ok(stream) = UnixStream::connect(&socket) {
            return Ok(stream);
        }
        std::thread::sleep(CONNECT_WAIT_STEP);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("timed out connecting to {}", socket.display()),
    )
    .into())
}

fn serve(
    listener: UnixListener,
    _owner: crate::store::single_flight::ProducerGuard,
    runtime: RuntimePaths,
) {
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
    );
    let _socket_guard = crate::harness::run_wake::SocketGuard::new(socket);
    let mut walker = SpendingWalker::new();
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        if let Err(error) = serve_connection(stream, &runtime, &mut walker) {
            tracing::debug!(error = %error, "spending service connection failed");
        }
    }
}

fn serve_connection(
    stream: UnixStream,
    owner_runtime: &RuntimePaths,
    walker: &mut SpendingWalker,
) -> Result<()> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let request = match read_json_line::<SpendingServiceRequest>(&mut BufReader::new(stream)) {
        Ok(request) => match request.validate() {
            Ok(request) => request,
            Err(error) => {
                write_json_line(&mut writer, &SpendingServiceFrame::Error(error))?;
                return Ok(());
            }
        },
        Err(error) => {
            let failure = SpendingServiceFailure::new(
                SpendingServiceErrorCode::InvalidRequest,
                error.to_string(),
            );
            write_json_line(&mut writer, &SpendingServiceFrame::Error(failure))?;
            return Ok(());
        }
    };
    let runtime = match request.workspace_id.clone() {
        Some(workspace_id) => match owner_runtime.for_sibling_workspace(workspace_id) {
            Ok(runtime) => runtime,
            Err(error) => {
                write_json_line(
                    &mut writer,
                    &SpendingServiceFrame::Error(SpendingServiceFailure::new(
                        SpendingServiceErrorCode::Internal,
                        error.to_string(),
                    )),
                )?;
                return Ok(());
            }
        },
        None => owner_runtime.clone(),
    };
    if let Err(error) = runtime.ensure_dirs() {
        write_json_line(
            &mut writer,
            &SpendingServiceFrame::Error(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Unavailable,
                error.to_string(),
            )),
        )?;
        return Ok(());
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut on_progress = |progress| {
            if request.progress_frames {
                let _ = write_json_line(&mut writer, &SpendingServiceFrame::Progress(progress));
            }
        };
        crate::sidebar::refresh::spending::serve_spending_service_request(
            walker,
            &runtime,
            &request,
            &mut on_progress,
        )
    }));
    match result {
        Ok(caches) => {
            tracing::debug!("spending service request complete");
            write_json_line(
                &mut writer,
                &SpendingServiceFrame::Complete(Box::new(caches)),
            )?;
        }
        Err(_) => {
            *walker = SpendingWalker::new();
            let failure = SpendingServiceFailure::new(
                SpendingServiceErrorCode::Internal,
                "spending service refresh panicked",
            );
            write_json_line(&mut writer, &SpendingServiceFrame::Error(failure))?;
        }
    }
    Ok(())
}

fn write_json_line(mut writer: impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut writer, value).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_json_line<T: for<'de> Deserialize<'de>>(reader: &mut impl BufRead) -> Result<T> {
    let mut line = String::new();
    let mut limited = std::io::Read::take(&mut *reader, MAX_FRAME_BYTES + 1);
    let read = limited.read_line(&mut line)?;
    if read == 0 {
        return Err(SpendingServiceClientError::Protocol(
            "service closed before a final frame".to_owned(),
        ));
    }
    if read as u64 > MAX_FRAME_BYTES || !line.ends_with('\n') {
        return Err(SpendingServiceClientError::Protocol(
            "service frame exceeds the bounded newline protocol".to_owned(),
        ));
    }
    serde_json::from_str(&line)
        .map_err(|error| SpendingServiceClientError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    use std::path::Path;
    use std::sync::{Arc, Barrier};

    #[test]
    fn request_rejects_relative_paths_and_normalizes_absolute_roots() {
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/project"));
        let invalid = SpendingServiceRequest::workspace(
            workspace_id.clone(),
            Some(PathBuf::from("relative")),
            Vec::new(),
            None,
            HashMap::new(),
            HeadlineSpec::default(),
            false,
        );
        assert_eq!(
            invalid.validate().unwrap_err().code,
            SpendingServiceErrorCode::InvalidPath
        );

        let valid = SpendingServiceRequest::workspace(
            workspace_id,
            Some(PathBuf::from("/tmp/a/../project")),
            vec![PathBuf::from("/tmp/project/./worktree")],
            None,
            HashMap::new(),
            HeadlineSpec::default(),
            false,
        )
        .validate()
        .unwrap();
        assert_eq!(valid.project_root, Some(PathBuf::from("/tmp/project")));
        assert_eq!(
            valid.worktree_roots,
            vec![PathBuf::from("/tmp/project/worktree")]
        );

        let mut wrong_protocol = SpendingServiceRequest::global(HeadlineSpec::default(), false);
        wrong_protocol.protocol_version += 1;
        assert_eq!(
            wrong_protocol.validate().unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
        let mut wrong_cache = SpendingServiceRequest::global(HeadlineSpec::default(), false);
        wrong_cache.cache_version += 1;
        assert_eq!(
            wrong_cache.validate().unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
    }

    #[test]
    fn framed_protocol_round_trips_progress_and_result() {
        let frame = SpendingServiceFrame::Progress(SpendProgress {
            finished_files: 3,
            total_files: 9,
        });
        let mut bytes = Vec::new();
        write_json_line(&mut bytes, &frame).unwrap();
        let decoded: SpendingServiceFrame =
            read_json_line(&mut BufReader::new(bytes.as_slice())).unwrap();
        assert!(matches!(
            decoded,
            SpendingServiceFrame::Progress(SpendProgress {
                finished_files: 3,
                total_files: 9
            })
        ));
    }

    #[test]
    fn framed_service_matches_direct_global_and_workspace_aggregation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let workspace_id = WorkspaceId::from_project_root(&project);
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let transcript = dir.path().join("claude.jsonl");
        let now_secs = super::super::unix_secs_now();
        let tod = now_secs % 86_400;
        std::fs::write(
            &transcript,
            format!(
                r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","cwd":"{}","costUSD":2.5,"requestId":"req-service","message":{{"id":"msg-service","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#,
                super::super::utc_date(now_secs),
                tod / 3_600,
                (tod % 3_600) / 60,
                tod % 60,
                project.display()
            ),
        )
        .unwrap();
        let _discovery = super::super::override_discovered_spending_files_for_test(vec![(
            &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
            transcript.clone(),
        )]);
        let request = SpendingServiceRequest::workspace(
            workspace_id,
            Some(project.clone()),
            Vec::new(),
            None,
            HashMap::from([(transcript, project.clone())]),
            HeadlineSpec::default(),
            false,
        );

        let mut direct_walker = SpendingWalker::new();
        let mut ignore_progress = |_| {};
        let direct = crate::sidebar::refresh::spending::serve_spending_service_request(
            &mut direct_walker,
            &runtime,
            &request,
            &mut ignore_progress,
        );
        std::fs::remove_file(runtime.shared_provider_spending_path()).unwrap();
        let scope = super::super::SpendScope::from_roots(Some(&project), &[]);
        std::fs::remove_file(runtime.workspace_spending_path(&scope.hash())).unwrap();

        let (client, server) = UnixStream::pair().unwrap();
        let mut service_walker = SpendingWalker::new();
        let actual = std::thread::scope(|scope| {
            let client = scope.spawn(|| transact(client, &request, &mut |_| {}));
            serve_connection(server, &runtime, &mut service_walker).unwrap();
            client.join().unwrap().unwrap()
        });

        assert_eq!(actual.provider.spending, direct.provider.spending);
        assert_eq!(actual.workspace.tally, direct.workspace.tally);
        assert!((actual.workspace.tally.year.usd - 2.5).abs() < 1e-9);
    }

    #[test]
    fn concurrent_clients_elect_one_private_socket_owner() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        super::super::write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            crate::sidebar::timing::unix_now_ms(),
            &Default::default(),
        );

        let clients = 6;
        let barrier = Arc::new(Barrier::new(clients));
        let handles = (0..clients)
            .map(|_| {
                let runtime = runtime.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    request(
                        &runtime,
                        SpendingServiceRequest::global(HeadlineSpec::default(), false),
                        |_| {},
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let socket = runtime.shared_spending_service_socket_path(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        let owner_lock = runtime.shared_spending_service_owner_lock(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
        );
        assert!(matches!(
            crate::store::single_flight::coordinate::<()>(
                &owner_lock,
                CONNECT_WAIT_STEP,
                0,
                || None
            ),
            crate::store::single_flight::Coordination::ContentionTimeout
        ));
    }

    #[test]
    fn stale_socket_is_unlinked_only_after_winning_owner_lock() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let socket = runtime.shared_spending_service_socket_path(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
        );
        std::fs::write(&socket, b"stale").unwrap();
        let owner_lock = runtime.shared_spending_service_owner_lock(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
        );
        let held = match crate::store::single_flight::coordinate::<()>(
            &owner_lock,
            CONNECT_WAIT_STEP,
            0,
            || None,
        ) {
            crate::store::single_flight::Coordination::Produce(guard) => guard,
            _ => panic!("test owns the lifetime lock"),
        };

        assert!(connect_or_start(&runtime).is_err());
        assert_eq!(std::fs::read(&socket).unwrap(), b"stale");

        drop(held);
        drop(connect_or_start(&runtime).unwrap());
        assert!(std::fs::metadata(&socket).unwrap().file_type().is_socket());
    }
}
