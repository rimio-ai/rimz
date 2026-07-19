//! Read-only client for an already-running Antigravity CLI local service.

pub(super) mod process;
pub(super) mod wire;

use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::agents::context::AgentAccount;
use crate::agents::{AccountUsageProbe, AccountUsageSnapshot};

const GET_USER_STATUS_PATH: &str = "/exa.language_server_pb.LanguageServerService/GetUserStatus";
const RETRIEVE_QUOTA_PATH: &str =
    "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary";
pub(super) const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const ATTEMPT_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, thiserror::Error)]
pub(super) enum LocalApiError {
    #[error("no verified Antigravity local service is available")]
    Unavailable,
    #[error("the Antigravity process changed during discovery")]
    ProcessChanged,
    #[error("Antigravity local service discovery failed")]
    Discovery,
    #[error("Antigravity local service request failed")]
    Transport,
    #[error("Antigravity local service returned HTTP {0}")]
    Http(u16),
    #[error("Antigravity local service response exceeded the size limit")]
    ResponseTooLarge,
    #[error("Antigravity local service returned an invalid response")]
    InvalidResponse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LoopbackEndpoint {
    pub(super) address: IpAddr,
    pub(super) port: u16,
}

impl LoopbackEndpoint {
    pub(super) fn url(self, path: &str) -> String {
        match self.address {
            IpAddr::V4(address) => format!("https://{address}:{}{path}", self.port),
            IpAddr::V6(address) => format!("https://[{address}]:{}{path}", self.port),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) pid: u32,
    pub(super) uid: u32,
    pub(super) start_token: String,
    pub(super) endpoints: Vec<LoopbackEndpoint>,
}

pub(super) fn probe_account() -> Result<AgentAccount, LocalApiError> {
    query(GET_USER_STATUS_PATH, "{}", |body, _| {
        wire::parse_identity(body)
    })
}

pub(super) fn probe_account_usage() -> AccountUsageProbe {
    probe_account_usage_with(process::discover, process::revalidate, post)
}

pub(in crate::agents::adapters::antigravity) fn probe_account_usage_with(
    mut discover: impl FnMut(Instant) -> Result<Vec<Candidate>, LocalApiError>,
    mut revalidate: impl FnMut(&Candidate) -> Result<(), LocalApiError>,
    mut request: impl FnMut(LoopbackEndpoint, &str, &str, Duration) -> Result<String, LocalApiError>,
) -> AccountUsageProbe {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let Ok(candidates) = discover(deadline) else {
        return AccountUsageProbe::Failed(Default::default());
    };
    for candidate in candidates {
        for endpoint in &candidate.endpoints {
            if revalidate(&candidate).is_err() {
                break;
            }
            let Some(timeout) = request_timeout(deadline) else {
                return AccountUsageProbe::Failed(Default::default());
            };
            let Ok(status) = request(*endpoint, GET_USER_STATUS_PATH, "{}", timeout) else {
                continue;
            };
            let Ok((identity, plan)) = wire::parse_account_usage_identity(&status) else {
                continue;
            };

            if revalidate(&candidate).is_err() {
                return AccountUsageProbe::Failed(identity);
            }
            let Some(timeout) = request_timeout(deadline) else {
                return AccountUsageProbe::Failed(identity);
            };
            let quota = match request(
                *endpoint,
                RETRIEVE_QUOTA_PATH,
                r#"{"forceRefresh":true}"#,
                timeout,
            ) {
                Ok(body) => body,
                Err(_) => return AccountUsageProbe::Failed(identity),
            };
            let rate_limits = match wire::parse_rate_limits(&quota, jiff::Timestamp::now()) {
                Ok(rate_limits) => rate_limits,
                Err(_) => return AccountUsageProbe::Failed(identity),
            };
            return AccountUsageProbe::Found {
                identity,
                snapshot: AccountUsageSnapshot {
                    rate_limits: Some(rate_limits),
                    plan,
                    ..Default::default()
                },
            };
        }
    }
    AccountUsageProbe::Failed(Default::default())
}

fn query<T>(
    path: &str,
    request_body: &str,
    parse: impl Fn(&str, jiff::Timestamp) -> Result<T, LocalApiError>,
) -> Result<T, LocalApiError> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let candidates = process::discover(deadline)?;
    for candidate in candidates {
        for endpoint in &candidate.endpoints {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(LocalApiError::Unavailable);
            };
            if process::revalidate(&candidate).is_err() {
                break;
            }
            let timeout = remaining.min(ATTEMPT_TIMEOUT);
            if let Ok(body) = post(*endpoint, path, request_body, timeout)
                && let Ok(parsed) = parse(&body, jiff::Timestamp::now())
            {
                return Ok(parsed);
            }
        }
    }
    Err(LocalApiError::Unavailable)
}

fn request_timeout(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .map(|remaining| remaining.min(ATTEMPT_TIMEOUT))
}

fn post(
    endpoint: LoopbackEndpoint,
    path: &str,
    request_body: &str,
    timeout: Duration,
) -> Result<String, LocalApiError> {
    use ureq::tls::TlsConfig;

    let agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(true)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout.min(Duration::from_millis(300))))
        .timeout_recv_response(Some(timeout))
        .timeout_recv_body(Some(timeout))
        .tls_config(TlsConfig::builder()
                // The certificate is accepted only after exact-process, uid,
                // pid-start, owned-socket, and loopback verification.
                .disable_verification(true)
                .build())
        .build()
        .new_agent();
    let mut response = agent
        .post(endpoint.url(path))
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .send(request_body)
        .map_err(|error| match error {
            ureq::Error::StatusCode(code) => LocalApiError::Http(code),
            _ => LocalApiError::Transport,
        })?;
    if !response.status().is_success() {
        return Err(LocalApiError::Http(response.status().as_u16()));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|error| {
            if error.to_string().contains("limit") {
                LocalApiError::ResponseTooLarge
            } else {
                LocalApiError::Transport
            }
        })
}
