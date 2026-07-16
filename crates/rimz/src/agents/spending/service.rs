//! Elected user-scoped service for the warm spending cursor.
//!
//! A long-lived sidebar cache refresher or held stats process may win the
//! namespace-scoped lifetime lock and host the service thread. Durable spending
//! publications remain authoritative; this socket only coordinates access to
//! one in-memory [`super::SpendingWalker`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    HeadlineSpec, PROVIDER_SPENDING_VERSION, SPENDING_CACHE_VERSION, SpendingCaches,
    SpendingWalker, WORKSPACE_SPENDING_VERSION,
};
use crate::ids::WorkspaceId;
use crate::store::paths::RuntimePaths;

pub const SPENDING_SERVICE_PROTOCOL_VERSION: u32 = 1;
const CONNECT_WAIT_STEP: Duration = Duration::from_millis(20);
const CONNECT_WAIT_STEPS: u32 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME_BYTES: u64 = 4 * 1024 * 1024;
const DISCOVERY_ENV: [&str; 15] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "AMP_DATA_DIR",
    "PI_AGENT_DIR",
    "PI_CODING_AGENT_SESSION_DIR",
    "PI_CODING_AGENT_DIR",
    "KIMI_CODE_HOME",
    "RIMZ_OPENCODE_DATA_DIR",
    "QWEN_RUNTIME_DIR",
    "QWEN_HOME",
    "KIRO_HOME",
    "RIMZ_ANTIGRAVITY_HOME",
];

/// Persistent-cache and provider-discovery identity for one warm walker.
/// Different state homes or transcript-home environments must never share a
/// service even when they use the same runtime root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct SpendingServiceNamespace(String);

impl SpendingServiceNamespace {
    fn for_runtime(runtime: &RuntimePaths) -> Self {
        let persistent_shared_root =
            crate::worktree::normalize_path_lexical(&runtime.persistent_shared_root);
        Self::from_parts(
            &persistent_shared_root,
            DISCOVERY_ENV
                .into_iter()
                .map(|name| (name, std::env::var_os(name))),
        )
    }

    fn from_parts(
        persistent_shared_root: &Path,
        environment: impl IntoIterator<Item = (&'static str, Option<OsString>)>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"rimz.spending-service.namespace.v1\0");
        hash_namespace_part(
            &mut hasher,
            persistent_shared_root.as_os_str().as_encoded_bytes(),
        );
        for (name, value) in environment {
            hash_namespace_part(&mut hasher, name.as_bytes());
            match value {
                Some(value) => {
                    hasher.update([1]);
                    hash_namespace_part(&mut hasher, value.as_encoded_bytes());
                }
                None => hasher.update([0]),
            }
        }
        let digest = hasher.finalize();
        Self(hex::encode(&digest[..12]))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn hash_namespace_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value);
}

/// Validated inputs for one account-global refresh and optional workspace
/// publication. Output paths and scope hashes are deliberately absent: the
/// owner derives both from the typed workspace id and normalized roots.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpendingServiceRequest {
    protocol_version: u32,
    cache_version: u32,
    provider_version: u32,
    workspace_version: u32,
    namespace: SpendingServiceNamespace,
    pub(crate) workspace_id: Option<WorkspaceId>,
    pub(crate) project_root: Option<PathBuf>,
    pub(crate) worktree_roots: Vec<PathBuf>,
    pub(crate) worktree_home: Option<PathBuf>,
    pub(crate) origin_overrides: HashMap<PathBuf, PathBuf>,
    pub(crate) headline: HeadlineSpec,
}

impl SpendingServiceRequest {
    pub fn global(runtime: &RuntimePaths, headline: HeadlineSpec) -> Self {
        Self {
            protocol_version: SPENDING_SERVICE_PROTOCOL_VERSION,
            cache_version: SPENDING_CACHE_VERSION,
            provider_version: PROVIDER_SPENDING_VERSION,
            workspace_version: WORKSPACE_SPENDING_VERSION,
            namespace: SpendingServiceNamespace::for_runtime(runtime),
            workspace_id: None,
            project_root: None,
            worktree_roots: Vec::new(),
            worktree_home: None,
            origin_overrides: HashMap::new(),
            headline,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn workspace(
        runtime: &RuntimePaths,
        workspace_id: WorkspaceId,
        project_root: Option<PathBuf>,
        worktree_roots: Vec<PathBuf>,
        worktree_home: Option<PathBuf>,
        origin_overrides: HashMap<PathBuf, PathBuf>,
        headline: HeadlineSpec,
    ) -> Self {
        Self {
            protocol_version: SPENDING_SERVICE_PROTOCOL_VERSION,
            cache_version: SPENDING_CACHE_VERSION,
            provider_version: PROVIDER_SPENDING_VERSION,
            workspace_version: WORKSPACE_SPENDING_VERSION,
            namespace: SpendingServiceNamespace::for_runtime(runtime),
            workspace_id: Some(workspace_id),
            project_root,
            worktree_roots,
            worktree_home,
            origin_overrides,
            headline,
        }
    }

    fn validate(
        mut self,
        namespace: &SpendingServiceNamespace,
    ) -> std::result::Result<Self, SpendingServiceFailure> {
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
        if self.provider_version != PROVIDER_SPENDING_VERSION
            || self.workspace_version != WORKSPACE_SPENDING_VERSION
        {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::VersionMismatch,
                format!(
                    "spending publication versions provider={} workspace={} are incompatible with provider={} workspace={}",
                    self.provider_version,
                    self.workspace_version,
                    PROVIDER_SPENDING_VERSION,
                    WORKSPACE_SPENDING_VERSION
                ),
            ));
        }
        if &self.namespace != namespace {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::NamespaceMismatch,
                "spending service persistent/discovery namespace does not match its owner",
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
    NamespaceMismatch,
    Busy,
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

/// Whether this caller has a process lifetime long enough to own the warm
/// walker. One-shot CLI producers connect to an existing owner and otherwise
/// use one bounded direct walker without taking the lifetime lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpendingServiceStartup {
    HostEligible,
    OneShot,
}

/// Connect to the current owner. Host-eligible callers elect an in-process
/// service on absence and retry failover once; one-shot callers use a bounded
/// direct walker without taking the lifetime lock when no owner answers.
pub fn request(
    runtime: &RuntimePaths,
    request: SpendingServiceRequest,
    startup: SpendingServiceStartup,
) -> Result<SpendingCaches> {
    let namespace = SpendingServiceNamespace::for_runtime(runtime);
    let request = request.validate(&namespace)?;
    let mut last_error = None;
    let attempts = if startup == SpendingServiceStartup::HostEligible {
        2
    } else {
        1
    };
    for attempt in 0..attempts {
        match connect_or_start(runtime, &namespace, startup)
            .and_then(|stream| transact(stream, &request))
        {
            Ok(caches) => return Ok(caches),
            Err(error) => {
                tracing::debug!(attempt, error = %error, "spending service request failed");
                let retryable = request_error_is_retryable(&error);
                if startup == SpendingServiceStartup::OneShot {
                    return if retryable {
                        direct_fallback(runtime, &request)
                    } else {
                        Err(error)
                    };
                }
                if !retryable {
                    return Err(error);
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        SpendingServiceClientError::Protocol("spending service retry produced no result".to_owned())
    }))
}

fn direct_fallback(
    runtime: &RuntimePaths,
    request: &SpendingServiceRequest,
) -> Result<SpendingCaches> {
    runtime
        .ensure_shared_dirs()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if request.workspace_id.is_some() {
        runtime
            .ensure_workspace_root()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    let mut walker = SpendingWalker::new();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::sidebar::refresh::spending::serve_spending_direct_request(
            &mut walker,
            runtime,
            request,
        )
    }))
    .map_err(|_| {
        SpendingServiceFailure::new(
            SpendingServiceErrorCode::Internal,
            "direct spending fallback panicked",
        )
        .into()
    })
}

fn request_error_is_retryable(error: &SpendingServiceClientError) -> bool {
    !matches!(
        error,
        SpendingServiceClientError::Service(SpendingServiceFailure {
            code: SpendingServiceErrorCode::InvalidRequest
                | SpendingServiceErrorCode::InvalidPath
                | SpendingServiceErrorCode::VersionMismatch
                | SpendingServiceErrorCode::NamespaceMismatch
                | SpendingServiceErrorCode::Busy,
            ..
        })
    )
}

fn transact(mut stream: UnixStream, request: &SpendingServiceRequest) -> Result<SpendingCaches> {
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    write_json_line(&mut stream, request)?;
    let mut reader = BufReader::new(stream);
    let frame: SpendingServiceFrame = read_json_line(&mut reader)?;
    match frame {
        SpendingServiceFrame::Complete(caches) => Ok(*caches),
        SpendingServiceFrame::Error(error) => Err(error.into()),
    }
}

fn connect_or_start(
    runtime: &RuntimePaths,
    namespace: &SpendingServiceNamespace,
    startup: SpendingServiceStartup,
) -> Result<UnixStream> {
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    crate::sock::validate_socket_path(&socket).map_err(std::io::Error::other)?;
    let first_error = match UnixStream::connect(&socket) {
        Ok(stream) => return Ok(stream),
        Err(error) => error,
    };
    if startup == SpendingServiceStartup::OneShot {
        return Err(first_error.into());
    }

    runtime
        .ensure_shared_dirs()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if let Ok(stream) = UnixStream::connect(&socket) {
        return Ok(stream);
    }

    let owner_lock = runtime.shared_spending_service_owner_lock(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
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
            let server_namespace = namespace.clone();
            std::thread::Builder::new()
                .name("rimz-spending-service".to_owned())
                .spawn(move || serve(listener, owner, server_runtime, server_namespace))?;
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
    namespace: SpendingServiceNamespace,
) {
    let socket = runtime.shared_spending_service_socket_path(
        SPENDING_SERVICE_PROTOCOL_VERSION,
        SPENDING_CACHE_VERSION,
        PROVIDER_SPENDING_VERSION,
        WORKSPACE_SPENDING_VERSION,
        namespace.as_str(),
    );
    let _socket_guard = crate::harness::run_wake::SocketGuard::new(socket);
    let walker = Arc::new(Mutex::new(SpendingWalker::new()));
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        let runtime = runtime.clone();
        let namespace = namespace.clone();
        let walker = Arc::clone(&walker);
        if let Err(error) = std::thread::Builder::new()
            .name("rimz-spending-request".to_owned())
            .spawn(move || {
                if let Err(error) = serve_connection(stream, &runtime, &namespace, &walker) {
                    tracing::debug!(error = %error, "spending service connection failed");
                }
            })
        {
            tracing::debug!(error = %error, "spending service request thread unavailable");
        }
    }
}

fn serve_connection(
    stream: UnixStream,
    owner_runtime: &RuntimePaths,
    owner_namespace: &SpendingServiceNamespace,
    walker: &Mutex<SpendingWalker>,
) -> Result<()> {
    stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let request = match read_json_line::<SpendingServiceRequest>(&mut BufReader::new(stream)) {
        Ok(request) => match request.validate(owner_namespace) {
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
    if let Some(caches) =
        crate::sidebar::refresh::spending::fresh_spending_service_publication(&runtime, &request)
    {
        write_json_line(
            &mut writer,
            &SpendingServiceFrame::Complete(Box::new(caches)),
        )?;
        return Ok(());
    }

    let mut walker = match walker.try_lock() {
        Ok(walker) => walker,
        Err(TryLockError::WouldBlock) => {
            write_json_line(
                &mut writer,
                &SpendingServiceFrame::Error(SpendingServiceFailure::new(
                    SpendingServiceErrorCode::Busy,
                    "spending service refresh already in progress",
                )),
            )?;
            return Ok(());
        }
        Err(TryLockError::Poisoned(error)) => {
            let mut walker = error.into_inner();
            *walker = SpendingWalker::new();
            walker
        }
    };
    if request.workspace_id.is_some()
        && let Err(error) = runtime.ensure_workspace_root()
    {
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
        let mut ignore_progress = |_| {};
        crate::sidebar::refresh::spending::serve_spending_service_request(
            &mut walker,
            &runtime,
            &request,
            &mut ignore_progress,
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
        let runtime = RuntimePaths::under(
            WorkspaceId::from_project_root(Path::new("/tmp/project")),
            Path::new("/tmp/rimz-spending-service-test"),
        )
        .unwrap();
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/project"));
        let invalid = SpendingServiceRequest::workspace(
            &runtime,
            workspace_id.clone(),
            Some(PathBuf::from("relative")),
            Vec::new(),
            None,
            HashMap::new(),
            HeadlineSpec::default(),
        );
        assert_eq!(
            invalid.validate(&namespace).unwrap_err().code,
            SpendingServiceErrorCode::InvalidPath
        );

        let valid = SpendingServiceRequest::workspace(
            &runtime,
            workspace_id,
            Some(PathBuf::from("/tmp/a/../project")),
            vec![PathBuf::from("/tmp/project/./worktree")],
            None,
            HashMap::new(),
            HeadlineSpec::default(),
        )
        .validate(&namespace)
        .unwrap();
        assert_eq!(valid.project_root, Some(PathBuf::from("/tmp/project")));
        assert_eq!(
            valid.worktree_roots,
            vec![PathBuf::from("/tmp/project/worktree")]
        );

        let mut wrong_protocol = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        wrong_protocol.protocol_version += 1;
        assert_eq!(
            wrong_protocol.validate(&namespace).unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
        let mut wrong_cache = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        wrong_cache.cache_version += 1;
        assert_eq!(
            wrong_cache.validate(&namespace).unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
        let mut wrong_provider = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        wrong_provider.provider_version += 1;
        assert_eq!(
            wrong_provider.validate(&namespace).unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
        let mut wrong_workspace = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        wrong_workspace.workspace_version += 1;
        assert_eq!(
            wrong_workspace.validate(&namespace).unwrap_err().code,
            SpendingServiceErrorCode::VersionMismatch
        );
        let other_namespace = SpendingServiceNamespace::from_parts(
            Path::new("/tmp/other-state/rimz/shared"),
            std::iter::empty(),
        );
        assert_eq!(
            SpendingServiceRequest::global(&runtime, HeadlineSpec::default())
                .validate(&other_namespace)
                .unwrap_err()
                .code,
            SpendingServiceErrorCode::NamespaceMismatch
        );
    }

    #[test]
    fn framed_protocol_round_trips_result() {
        let frame = SpendingServiceFrame::Complete(Box::default());
        let mut bytes = Vec::new();
        write_json_line(&mut bytes, &frame).unwrap();
        let decoded: SpendingServiceFrame =
            read_json_line(&mut BufReader::new(bytes.as_slice())).unwrap();
        assert!(matches!(decoded, SpendingServiceFrame::Complete(_)));
    }

    #[test]
    fn namespace_separates_persistent_roots_and_discovery_environments() {
        let base = SpendingServiceNamespace::from_parts(
            Path::new("/state-a/rimz/shared"),
            [("HOME", Some(OsString::from("/home/a")))],
        );
        let other_state = SpendingServiceNamespace::from_parts(
            Path::new("/state-b/rimz/shared"),
            [("HOME", Some(OsString::from("/home/a")))],
        );
        let other_discovery = SpendingServiceNamespace::from_parts(
            Path::new("/state-a/rimz/shared"),
            [("HOME", Some(OsString::from("/home/b")))],
        );

        assert_ne!(base, other_state);
        assert_ne!(base, other_discovery);
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
            &runtime,
            workspace_id,
            Some(project.clone()),
            Vec::new(),
            None,
            HashMap::from([(transcript, project.clone())]),
            HeadlineSpec::default(),
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
        let service_walker = Mutex::new(SpendingWalker::new());
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let actual = std::thread::scope(|scope| {
            let client = scope.spawn(|| transact(client, &request));
            serve_connection(server, &runtime, &namespace, &service_walker).unwrap();
            client.join().unwrap().unwrap()
        });

        assert_eq!(actual.provider.spending, direct.provider.spending);
        assert_eq!(actual.provider.days, direct.provider.days);
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
                        SpendingServiceRequest::global(&runtime, HeadlineSpec::default()),
                        SpendingServiceStartup::HostEligible,
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }

        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let socket = runtime.shared_spending_service_socket_path(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
            PROVIDER_SPENDING_VERSION,
            WORKSPACE_SPENDING_VERSION,
            namespace.as_str(),
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        let owner_lock = runtime.shared_spending_service_owner_lock(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
            PROVIDER_SPENDING_VERSION,
            WORKSPACE_SPENDING_VERSION,
            namespace.as_str(),
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
        runtime.ensure_shared_dirs().unwrap();
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let socket = runtime.shared_spending_service_socket_path(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
            PROVIDER_SPENDING_VERSION,
            WORKSPACE_SPENDING_VERSION,
            namespace.as_str(),
        );
        std::fs::write(&socket, b"stale").unwrap();
        let owner_lock = runtime.shared_spending_service_owner_lock(
            SPENDING_SERVICE_PROTOCOL_VERSION,
            SPENDING_CACHE_VERSION,
            PROVIDER_SPENDING_VERSION,
            WORKSPACE_SPENDING_VERSION,
            namespace.as_str(),
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

        assert!(
            connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).is_err()
        );
        assert_eq!(std::fs::read(&socket).unwrap(), b"stale");

        drop(held);
        drop(connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).unwrap());
        assert!(std::fs::metadata(&socket).unwrap().file_type().is_socket());
    }

    #[test]
    fn busy_walker_returns_immediately_instead_of_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let request = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        let walker = Mutex::new(SpendingWalker::new());
        let held = walker.lock().unwrap();
        let (client, server) = UnixStream::pair().unwrap();

        let error = std::thread::scope(|scope| {
            let client = scope.spawn(|| transact(client, &request));
            serve_connection(server, &runtime, &namespace, &walker).unwrap();
            client.join().unwrap().unwrap_err()
        });
        drop(held);

        assert!(matches!(
            error,
            SpendingServiceClientError::Service(SpendingServiceFailure {
                code: SpendingServiceErrorCode::Busy,
                ..
            })
        ));
    }

    #[test]
    fn fresh_publication_bypasses_busy_walker() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_shared_dirs().unwrap();
        super::super::write_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
            crate::sidebar::timing::unix_now_ms(),
            &Default::default(),
        );
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);
        let request = SpendingServiceRequest::global(&runtime, HeadlineSpec::default());
        let walker = Mutex::new(SpendingWalker::new());
        let held = walker.lock().unwrap();
        let (client, server) = UnixStream::pair().unwrap();

        let caches = std::thread::scope(|scope| {
            let client = scope.spawn(|| transact(client, &request));
            serve_connection(server, &runtime, &namespace, &walker).unwrap();
            client.join().unwrap().unwrap()
        });
        drop(held);

        assert!(
            caches
                .provider
                .is_fresh(crate::sidebar::timing::unix_now_ms())
        );
    }

    #[test]
    fn service_election_prepares_shared_dirs_without_workspace_tree() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);

        drop(connect_or_start(&runtime, &namespace, SpendingServiceStartup::HostEligible).unwrap());

        assert!(runtime.shared_root.is_dir());
        assert!(!runtime.root.exists());
    }

    #[test]
    fn one_shot_client_does_not_start_a_service_or_create_directories() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let namespace = SpendingServiceNamespace::for_runtime(&runtime);

        assert!(connect_or_start(&runtime, &namespace, SpendingServiceStartup::OneShot).is_err());
        assert!(!runtime.shared_root.exists());
        assert!(!runtime.root.exists());
    }
}
