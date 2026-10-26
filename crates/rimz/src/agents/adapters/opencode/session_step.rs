//! Checked model selection through a short-lived private OpenCode server.

use super::*;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use url::Url;

type CheckResult<T> = std::result::Result<T, CheckErr>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Model {
    #[serde(rename = "providerID")]
    provider_id: String,
    id: String,
}

#[derive(Debug, thiserror::Error)]
enum CheckErr {
    #[error("{0}")]
    Name(String),
    /// OpenCode's own message for an HTTP error no OpenCode version explains, such as a
    /// vanished session.
    #[error("{0}")]
    Answer(String),
    #[error("{0}")]
    Helper(String),
    #[error("helper I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("helper HTTP request: {0}")]
    Http(#[from] ureq::Error),
    #[error("invalid helper JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("helper deadline expired")]
    Deadline,
}

impl CheckErr {
    fn refusal(self, model: &str) -> SessionStepRefusal {
        let mut reason = self.to_string();
        if !matches!(self, Self::Name(_) | Self::Answer(_)) {
            reason.push_str("; model selection is verified on OpenCode 2.1.4");
            if let Some(fix) = OpencodeAdapter.version_upgrade_fix() {
                reason.push_str(&format!("; {fix}"));
            }
        }
        let reason: String = reason
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        SessionStepRefusal(format!("OpenCode model check refused {model:?}: {reason}"))
    }
}

pub(super) fn run(
    request: &SessionStepRequest<'_>,
) -> std::result::Result<Vec<String>, SessionStepRefusal> {
    run_with_timeout(request, Duration::from_secs(30))
}

fn run_with_timeout(
    request: &SessionStepRequest<'_>,
    timeout: Duration,
) -> std::result::Result<Vec<String>, SessionStepRefusal> {
    prepare(request, Instant::now() + timeout).map_err(|err| err.refusal(request.model))
}

fn prepare(request: &SessionStepRequest<'_>, deadline: Instant) -> CheckResult<Vec<String>> {
    let Some((program, args)) = request.provider_argv.split_first() else {
        return Err(CheckErr::Helper("provider argv is empty".into()));
    };
    let env = helper_env(request.env);
    let helper = Helper::spawn(program, request.cwd, &env)?;
    let origin = helper.origin(deadline)?;
    let credentials = base64::engine::general_purpose::STANDARD
        .encode(format!("opencode:{}", env["OPENCODE_PASSWORD"]));
    let auth = format!("Basic {credentials}");
    let directory = request
        .cwd
        .to_str()
        .ok_or_else(|| CheckErr::Helper("working directory is not UTF-8".into()))?;
    let mut url = origin.clone();
    url.set_path("/api/model");
    url.query_pairs_mut()
        .append_pair("location[directory]", directory);
    let mut response = http_agent(deadline)?
        .get(url.as_str())
        .header("Authorization", &auth)
        .call()?;
    if !response.status().is_success() {
        // An OpenCode older than the verified version can lack this route, so even its own
        // message keeps the version note.
        return Err(status_error("model list", response, CheckErr::Helper));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_vec()?;
    #[derive(Deserialize)]
    struct Catalog {
        data: Vec<Model>,
    }
    let catalog: Catalog = serde_json::from_slice(&bytes)?;
    let model = match_model(request.model, &catalog.data)?;
    let session = request.session.cloned().unwrap_or_else(mint_session_id);
    let (url, body) = if request.session.is_some() {
        let mut url = origin;
        // URL segments encode a provider session id without changing the route.
        url.path_segments_mut()
            .map_err(|_| CheckErr::Helper("invalid helper origin".into()))?
            .clear()
            .extend(["api", "session", session.as_str(), "model"]);
        (url, json!({"model":model}))
    } else {
        let mut url = origin;
        url.set_path("/api/session");
        (
            url,
            json!({"id":session, "location":{"directory":directory}, "model":model}),
        )
    };
    let response = http_agent(deadline)?
        .post(url.as_str())
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .send(body.to_string())?;
    if !response.status().is_success() {
        return Err(status_error("session model", response, CheckErr::Answer));
    }
    helper.finish(deadline)?;
    let mut argv = vec![program.clone()];
    if request.session.is_none() {
        argv.extend(["--session".into(), session.to_string()]);
    }
    argv.extend_from_slice(args);
    Ok(argv)
}

fn remaining(deadline: Instant) -> CheckResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|time| !time.is_zero())
        .ok_or(CheckErr::Deadline)
}

fn http_agent(deadline: Instant) -> CheckResult<ureq::Agent> {
    Ok(ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(true)
        .http_status_as_error(false)
        .timeout_global(Some(remaining(deadline)?))
        .build()
        .new_agent())
}

fn status_error(
    request: &str,
    mut response: ureq::http::Response<ureq::Body>,
    answered: fn(String) -> CheckErr,
) -> CheckErr {
    let status = response.status();
    let message = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_vec()
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|body| Some(body.get("message")?.as_str()?.to_owned()));
    match message {
        Some(message) => answered(format!("{request} returned HTTP {status}: {message}")),
        None => CheckErr::Helper(format!("{request} returned HTTP {status}")),
    }
}

fn match_model(name: &str, models: &[Model]) -> CheckResult<Model> {
    let full_names: Vec<_> = models
        .iter()
        .map(|model| format!("{}/{}", model.provider_id, model.id))
        .collect();
    if !name.contains('#') {
        if let Some(index) = full_names.iter().position(|full| full == name) {
            return Ok(models[index].clone());
        }
        // A name with `/` names its provider, as OpenCode's own model references do.
        let bare: Vec<_> = models
            .iter()
            .filter(|model| !name.contains('/') && model.id == name)
            .collect();
        if let [model] = bare.as_slice() {
            return Ok((*model).clone());
        }
        if !bare.is_empty() {
            let alternatives: Vec<_> = bare
                .iter()
                .map(|model| format!("{}/{}", model.provider_id, model.id))
                .collect();
            return Err(CheckErr::Name(format!(
                "ambiguous model: {}; name one as provider/model",
                alternatives.join(", ")
            )));
        }
    }
    let nearest = crate::utils::suggest::nearest_names(
        name,
        full_names
            .iter()
            .zip(models)
            .flat_map(|(full, model)| [full.as_str(), model.id.as_str()]),
    );
    let suggestions: Vec<_> = full_names
        .iter()
        .zip(models)
        .filter(|(full, model)| {
            nearest.contains(&full.as_str()) || nearest.contains(&model.id.as_str())
        })
        .map(|(full, _)| full.as_str())
        .take(3)
        .collect();
    let fix = if suggestions.is_empty() || name.contains('#') {
        "run `opencode models` to list configured models".into()
    } else {
        format!("did you mean {}?", suggestions.join(", "))
    };
    Err(CheckErr::Name(format!("unknown model; {fix}")))
}

fn helper_env(inherited: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = inherited
        .iter()
        .filter(|(key, _)| key.as_str() != "RIMZ" && !key.starts_with("RIMZ_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut config = inherited
        .get("OPENCODE_CONFIG_CONTENT")
        .and_then(|raw| crate::agents::jsonc::from_slice::<Value>(raw.as_bytes()).ok())
        .and_then(|value| value.as_object().cloned())
        .filter(|object| object.get("policies").is_none_or(Value::is_array))
        .unwrap_or_default();
    let mut policies = config
        .remove("policies")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    for resource in ["plugin:*", "mcp:*"] {
        policies.push(json!({"action":"integration.use", "resource":resource, "effect":"deny"}));
    }
    config.insert("policies".into(), Value::Array(policies));
    env.insert(
        "OPENCODE_CONFIG_CONTENT".into(),
        Value::Object(config).to_string(),
    );
    for key in [
        "OPENCODE_DISABLE_MODELS_FETCH",
        "OPENCODE_FILEWATCHER_DISABLE",
        "OPENCODE_DISABLE_FFF",
    ] {
        env.insert(key.into(), "1".into());
    }
    env.insert(
        "OPENCODE_PASSWORD".into(),
        format!(
            "{}{}",
            uuid::Uuid::now_v7().simple(),
            uuid::Uuid::now_v7().simple()
        ),
    );
    env
}

fn mint_session_id() -> AgentSessionId {
    static STAMP: AtomicU64 = AtomicU64::new(0);
    let now = crate::utils::time::unix_now_ms() << 12;
    // The update always returns Some, so fetch_update cannot fail.
    let previous = STAMP
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
            Some(now.max(previous + 1))
        })
        .expect("unconditional stamp update");
    let stamp = now.max(previous + 1);
    let mut id = format!("ses_{:012x}", !stamp & 0xffffffffffff);
    const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    for nonce in [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()] {
        for byte in &nonce.as_bytes()[9..] {
            id.push(char::from(BASE62[usize::from(*byte) % BASE62.len()]));
        }
    }
    id.into()
}

struct Helper {
    child: Child,
    ready: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Helper {
    fn spawn(program: &str, cwd: &Path, env: &BTreeMap<String, String>) -> CheckResult<Self> {
        let mut command = Command::new(program);
        command
            .args(["serve", "--stdio", "--hostname", "127.0.0.1", "--port", "0"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::child_process::clear_env_preserving_coverage(&mut command);
        command.envs(env);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        // stdout was piped above and has not been taken.
        let stdout = child.stdout.take().expect("piped helper stdout");
        let (send, ready) = mpsc::channel();
        let mut helper = Self {
            child,
            ready,
            reader: None,
        };
        helper.reader = Some(
            std::thread::Builder::new()
                .name("opencode-ready".into())
                .spawn(move || {
                    let mut line = Vec::new();
                    let result = std::io::BufReader::new(stdout)
                        .take(64 * 1024)
                        .read_until(b'\n', &mut line)
                        .map(|_| line);
                    let _ = send.send(result);
                })?,
        );
        Ok(helper)
    }

    fn origin(&self, deadline: Instant) -> CheckResult<Url> {
        let line = self
            .ready
            .recv_timeout(remaining(deadline)?)
            .map_err(|err| match err {
                mpsc::RecvTimeoutError::Timeout => CheckErr::Deadline,
                mpsc::RecvTimeoutError::Disconnected => {
                    CheckErr::Helper("helper readiness reader stopped".into())
                }
            })??;
        if !line.ends_with(b"\n") {
            return Err(CheckErr::Helper(
                "helper did not emit a bounded readiness line".into(),
            ));
        }
        #[derive(Deserialize)]
        struct Ready {
            url: String,
        }
        let ready: Ready = serde_json::from_slice(&line)?;
        let url =
            Url::parse(&ready.url).map_err(|_| CheckErr::Helper("invalid helper URL".into()))?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if url.scheme() != "http"
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(CheckErr::Helper(
                "helper URL must be a loopback HTTP origin without credentials or a query".into(),
            ));
        }
        Ok(url)
    }

    fn finish(mut self, deadline: Instant) -> CheckResult<()> {
        drop(self.child.stdin.take());
        loop {
            let time = remaining(deadline)?;
            if let Some(status) = self.child.try_wait()? {
                if !status.success() {
                    return Err(CheckErr::Helper(format!("helper exited with {status}")));
                }
                return Ok(());
            }
            std::thread::sleep(time.min(Duration::from_millis(10)));
        }
    }
}

impl Drop for Helper {
    // A worker the helper started outlives a clean exit in its group, whose id stays
    // reserved while any member lives, so the group is killed on every path.
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        #[cfg(unix)]
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests;
