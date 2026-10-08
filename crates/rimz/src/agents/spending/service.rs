//! Elected user-scoped service for the warm spending cursor.
//!
//! A long-lived sidebar cache refresher or held stats process may win the namespace-scoped lifetime lock and host the service and walker threads. Durable spending publications remain authoritative; this socket only coordinates access to one in-memory [`super::SpendingWalker`].
//!
//! The service thread admits connections and answers fresh publications; one lifetime walker thread fulfils stale requests. A claim held through the reply write rejects busy work immediately, independent of receiver readiness.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    HeadlineSpec, PROVIDER_SPENDING_VERSION, SPENDING_CACHE_VERSION, SpendingCaches,
    SpendingWalker, WORKSPACE_SPENDING_VERSION,
};
use crate::disk::paths::RuntimePaths;
use crate::ids::WorkspaceId;

const SPENDING_SERVICE_PROTOCOL_VERSION: u32 = 1;
const CONNECT_WAIT_STEP: Duration = Duration::from_millis(20);
const CONNECT_WAIT_STEPS: u32 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME_BYTES: u64 = 4 * 1024 * 1024;
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
/// Persistent-cache and provider-discovery identity for one warm walker.
/// Different state homes or source declarations never share a service even
/// when they use the same runtime root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
struct SpendingServiceNamespace(String);

impl SpendingServiceNamespace {
    fn for_runtime(runtime: &RuntimePaths) -> Self {
        let declarations = super::discovery::runtime_logins(&crate::agents::ambient_env())
            .into_iter()
            .flat_map(|(login, adapter, env)| {
                let key = login.key().to_string();
                adapter
                    .spending_sources(&env)
                    .into_iter()
                    .map(|source| source.fingerprint())
                    // A login without history still changes the namespace.
                    .chain(std::iter::once(Vec::new()))
                    .map(move |source| (key.clone(), source))
            })
            .collect::<Vec<_>>();
        Self::from_declarations(&runtime.persistent_shared_root, declarations)
    }

    fn from_declarations(
        persistent_shared_root: &Path,
        mut declarations: Vec<(String, Vec<u8>)>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"rimz.spending-service.namespace.v2\0");
        let persistent_shared_root =
            crate::utils::path::normalize_path_lexical(persistent_shared_root);
        hash_namespace_part(
            &mut hasher,
            persistent_shared_root.as_os_str().as_encoded_bytes(),
        );
        declarations.sort_by(|(left_login, left_source), (right_login, right_source)| {
            left_login
                .as_bytes()
                .cmp(right_login.as_bytes())
                .then_with(|| left_source.cmp(right_source))
        });
        for (login, source) in declarations {
            hash_namespace_part(&mut hasher, login.as_bytes());
            hash_namespace_part(&mut hasher, &source);
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
    pub(super) workspace_id: Option<WorkspaceId>,
    pub(super) project_root: Option<PathBuf>,
    pub(super) worktree_roots: Vec<PathBuf>,
    pub(super) worktree_home: Option<PathBuf>,
    pub(super) origin_overrides: HashMap<PathBuf, PathBuf>,
    pub(super) headline: HeadlineSpec,
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
    pub(crate) fn workspace(
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
    *path = crate::utils::path::normalize_path_lexical(path);
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
enum SpendingServiceFrame {
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

type Result<T> = std::result::Result<T, SpendingServiceClientError>;

/// Whether this caller can own the warm walker. Without an owner, one-shot
/// callers serve any current-version publication regardless of age, walking
/// only when none exists and never taking the lifetime lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpendingServiceStartup {
    HostEligible,
    OneShot,
}

/// Connect to the current owner. Host-eligible callers elect an in-process
/// service on absence and retry failover once; without an owner, one-shot
/// callers serve any current-version publication regardless of age and walk
/// only when none exists.
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
    if let Some(caches) = super::engine::current_publication(runtime, request) {
        return Ok(caches);
    }
    runtime
        .ensure_shared_dirs()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if request.workspace_id.is_some() {
        runtime
            .ensure_workspace_root()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::engine::serve_direct(runtime, request)
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
    match crate::disk::single_flight::coordinate::<()>(&owner_lock, CONNECT_WAIT_STEP, 0, || None) {
        crate::disk::single_flight::Coordination::Produce(owner) => {
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
        crate::disk::single_flight::Coordination::ContentionTimeout => {}
        crate::disk::single_flight::Coordination::Unavailable => {
            return Err(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Unavailable,
                "spending service owner lock unavailable",
            )
            .into());
        }
        crate::disk::single_flight::Coordination::Shared(()) => {
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
    _owner: crate::disk::single_flight::ProducerGuard,
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
    let _socket_guard = crate::sock::SocketGuard::new(socket);
    let busy = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel(1);
    let walker = match std::thread::Builder::new()
        .name("rimz-spending-walker".to_owned())
        .spawn(move || {
            let mut walker = SpendingWalker::new();
            for admitted in receiver {
                if let Err(error) = fulfil_request(admitted, &mut walker) {
                    tracing::debug!(error = %error, "spending service request failed");
                }
            }
        }) {
        Ok(walker) => walker,
        Err(error) => {
            tracing::debug!(error = %error, "spending service walker thread unavailable");
            return;
        }
    };
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        if let Err(error) = admit_connection(stream, &runtime, &namespace, &sender, &busy) {
            tracing::debug!(error = %error, "spending service connection failed");
        }
        // An owner without its walker gives up the socket and the lock, so the next client elects a working one.
        if walker.is_finished() {
            tracing::debug!("spending service walker exited; releasing ownership");
            return;
        }
    }
}

struct WalkerClaim(Arc<AtomicBool>);

impl Drop for WalkerClaim {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct AdmittedRequest {
    stream: UnixStream,
    runtime: RuntimePaths,
    request: SpendingServiceRequest,
    _claim: WalkerClaim,
}

fn admit_connection(
    stream: UnixStream,
    owner_runtime: &RuntimePaths,
    owner_namespace: &SpendingServiceNamespace,
    sender: &SyncSender<AdmittedRequest>,
    busy: &Arc<AtomicBool>,
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
    if let Some(caches) = super::engine::fresh_publication(&runtime, &request) {
        write_json_line(
            &mut writer,
            &SpendingServiceFrame::Complete(Box::new(caches)),
        )?;
        return Ok(());
    }

    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        write_json_line(
            &mut writer,
            &SpendingServiceFrame::Error(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Busy,
                "spending service refresh already in progress",
            )),
        )?;
        return Ok(());
    }
    let admitted = AdmittedRequest {
        stream: writer,
        runtime,
        request,
        _claim: WalkerClaim(Arc::clone(busy)),
    };
    // One claimed job fits in the single slot, independent of receiver readiness.
    if let Err(error) = sender.send(admitted) {
        let mut admitted = error.0;
        write_json_line(
            &mut admitted.stream,
            &SpendingServiceFrame::Error(SpendingServiceFailure::new(
                SpendingServiceErrorCode::Unavailable,
                "spending service walker unavailable",
            )),
        )?;
    }
    Ok(())
}

fn fulfil_request(admitted: AdmittedRequest, walker: &mut SpendingWalker) -> Result<()> {
    let AdmittedRequest {
        stream: mut writer,
        runtime,
        request,
        _claim,
    } = admitted;
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
        super::engine::serve_request(walker, &runtime, &request, &mut ignore_progress)
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

fn write_json_line(writer: impl Write, value: &impl Serialize) -> std::io::Result<()> {
    let mut writer = std::io::BufWriter::with_capacity(WRITE_BUFFER_BYTES, writer);
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
mod tests;
