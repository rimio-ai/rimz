use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use headless_chrome::protocol::cdp::{Network, types::Event};
use headless_chrome::{Browser, LaunchOptions, Tab};
use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend};

use crate::backend::tmux::TmuxServer;
use crate::common::{CommandTimeoutExt, Env, ZellijNamespace, daemon_test_guard};

const MIN_TTYD_VERSION: (u32, u32, u32) = (1, 7, 5);

pub(super) struct WebStack {
    pub(super) browser: PathBuf,
    ttyd: PathBuf,
}

impl WebStack {
    pub(super) fn resolve(mux: &str) -> Result<Self, String> {
        which::which(mux).map_err(|_| format!("{mux} not on PATH"))?;
        let browser = resolve_override_or_candidates(
            "RIMZ_TEST_BROWSER",
            &[
                "chromium",
                "chromium-browser",
                "google-chrome",
                "chrome",
                "chrome-headless-shell",
            ],
        )
        .ok_or_else(|| "no Chromium binary found (set RIMZ_TEST_BROWSER to override)".to_owned())?;
        let ttyd = resolve_override_or_candidates("RIMZ_TTYD_BIN", &["ttyd"])
            .ok_or_else(|| "ttyd not found (set RIMZ_TTYD_BIN to override)".to_owned())?;
        let output = Command::new(&ttyd)
            .arg("--version")
            .output()
            .map_err(|err| format!("could not run {} --version: {err}", ttyd.display()))?;
        let reported = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        let reported = String::from_utf8_lossy(reported);
        let version = parse_ttyd_version(reported.trim())
            .ok_or_else(|| format!("could not parse ttyd version `{}`", reported.trim()))?;
        if version < MIN_TTYD_VERSION {
            return Err(format!(
                "ttyd {}.{}.{} is below the 1.7.5 browser minimum",
                version.0, version.1, version.2
            ));
        }
        Ok(Self { browser, ttyd })
    }
}

fn resolve_override_or_candidates(env: &str, candidates: &[&str]) -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(env).filter(|value| !value.is_empty()) {
        return which::which(PathBuf::from(value)).ok();
    }
    candidates
        .iter()
        .find_map(|candidate| which::which(candidate).ok())
}

fn parse_ttyd_version(reported: &str) -> Option<(u32, u32, u32)> {
    let version = reported
        .strip_prefix("ttyd version ")?
        .split_whitespace()
        .next()?;
    let mut parts = version.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    let patch = std::str::from_utf8(&patch).ok()?.parse().ok()?;
    Some((major, minor, patch))
}

pub(super) struct BrowserHandle {
    browser: Browser,
}

impl BrowserHandle {
    pub(super) fn launch(path: &Path) -> Self {
        let options = LaunchOptions::default_builder()
            .path(Some(path.to_path_buf()))
            .sandbox(false)
            .window_size(Some((1280, 800)))
            .args(vec![OsStr::new("--disable-gpu")])
            .idle_browser_timeout(Duration::from_secs(120))
            .build()
            .expect("build Chromium launch options");
        let browser = Browser::new(options).expect("launch headless Chromium");
        Self { browser }
    }

    pub(super) fn authed_tab(&self, url: &str, secret: &str) -> Arc<Tab> {
        let tab = self.configured_authed_tab(secret);
        tab.navigate_to(url).expect("navigate authenticated tab");
        tab
    }

    pub(super) fn authed_tab_allow_error(&self, url: &str, secret: &str) -> Arc<Tab> {
        let tab = self.configured_authed_tab(secret);
        let _ = tab.navigate_to(url);
        tab
    }

    fn configured_authed_tab(&self, secret: &str) -> Arc<Tab> {
        let tab = self.browser.new_tab().expect("open browser tab");
        tab.enable_fetch(None, Some(true))
            .expect("enable browser auth challenges");
        tab.authenticate(Some("rimz".to_owned()), Some(secret.to_owned()))
            .expect("configure browser credentials");
        tab
    }

    pub(super) fn tab(&self, url: &str) -> Arc<Tab> {
        let tab = self.browser.new_tab().expect("open browser tab");
        tab.navigate_to(url).expect("navigate browser tab");
        tab
    }

    pub(super) fn tab_with_websocket_capture(
        &self,
        url: &str,
        secret: Option<&str>,
        capture_text: bool,
    ) -> (Arc<Tab>, WebSocketCapture) {
        let tab = secret.map_or_else(
            || self.browser.new_tab().expect("open browser tab"),
            |secret| self.configured_authed_tab(secret),
        );
        tab.call_method(Network::Enable {
            max_total_buffer_size: None,
            max_resource_buffer_size: None,
            max_post_data_size: None,
            report_direct_socket_traffic: None,
            enable_durable_messages: None,
        })
        .expect("enable browser network events");
        let capture = WebSocketCapture::new(&tab, capture_text);
        tab.navigate_to(url).expect("navigate browser tab");
        (tab, capture)
    }
}

pub(super) struct WebSocketCapture {
    closes: Arc<AtomicUsize>,
    received: Arc<AtomicUsize>,
    sent: Arc<AtomicUsize>,
    text: Arc<Mutex<String>>,
    urls: Arc<Mutex<Vec<String>>>,
}

impl WebSocketCapture {
    fn new(tab: &Tab, capture_text: bool) -> Self {
        let closes = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(AtomicUsize::new(0));
        let text = Arc::new(Mutex::new(String::new()));
        let urls = Arc::new(Mutex::new(Vec::new()));
        let listener_closes = Arc::clone(&closes);
        let listener_received = Arc::clone(&received);
        let listener_sent = Arc::clone(&sent);
        let listener_text = Arc::clone(&text);
        let listener_urls = Arc::clone(&urls);
        tab.add_event_listener(Arc::new(move |event: &Event| match event {
            Event::NetworkWebSocketCreated(event) => {
                listener_urls
                    .lock()
                    .expect("lock WebSocket URLs")
                    .push(event.params.url.clone());
            }
            Event::NetworkWebSocketFrameReceived(event) => {
                listener_received.fetch_add(1, Ordering::Relaxed);
                if !capture_text {
                    return;
                }
                let payload = &event.params.response.payload_data;
                let mut text = listener_text.lock().expect("lock WebSocket capture");
                text.push_str(payload);
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(payload) {
                    text.push_str(&String::from_utf8_lossy(&decoded));
                }
            }
            Event::NetworkWebSocketFrameSent(_) => {
                listener_sent.fetch_add(1, Ordering::Relaxed);
            }
            Event::NetworkWebSocketClosed(_) => {
                listener_closes.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }))
        .expect("listen for browser WebSocket frames");
        Self {
            closes,
            received,
            sent,
            text,
            urls,
        }
    }

    pub(super) fn closes(&self) -> usize {
        self.closes.load(Ordering::Relaxed)
    }

    pub(super) fn received(&self) -> usize {
        self.received.load(Ordering::Relaxed)
    }

    pub(super) fn wait_until_sent(&self, budget: Duration) {
        let deadline = Instant::now() + budget;
        while self.sent.load(Ordering::Relaxed) == 0 {
            assert!(
                Instant::now() < deadline,
                "browser sent no WebSocket frames after opening the terminal"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(super) fn wait_url_contains(&self, needle: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let urls = self.urls.lock().expect("lock WebSocket URLs").clone();
            if urls.iter().any(|url| url.contains(needle)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "browser WebSocket URL did not contain {needle:?}; opened URLs: {urls:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(super) fn wait_contains(&self, needle: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let text = self.text.lock().expect("lock WebSocket capture").clone();
            if text.contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "browser WebSocket did not contain {needle:?}; captured frames:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub(super) struct OpenedWeb {
    pub(super) url: String,
    pub(super) secret: String,
}

pub(super) struct SharedWeb {
    pub(super) url: String,
}

pub(super) struct LiveWebFixture {
    _guard: rimz::store::lock::WorkspaceLock,
    pub(super) env: Env,
    pub(super) workspace: rimz::ResolvedWorkspace,
    server: TmuxServer,
    ttyd: PathBuf,
    web_port: u16,
    share_port: u16,
}

impl LiveWebFixture {
    pub(super) fn new(stack: &WebStack) -> Self {
        let guard = daemon_test_guard();
        let env = Env::new();
        env.write_config(&env.project_root, "");
        env.record(&env.project_root);
        let workspace =
            rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let server = TmuxServer::in_runtime_root(&env.runtime_root);
        let root = env.project_root.to_string_lossy();
        server.output(&[
            "new-session",
            "-d",
            "-s",
            &workspace.session_name,
            "-c",
            &root,
            "sh",
        ]);
        let web_port = free_loopback_port();
        let share_port = free_loopback_port();
        write_machine_config(
            &env,
            &format!("[web]\nport = {web_port}\nshare_port = {share_port}\n"),
        );
        Self {
            _guard: guard,
            env,
            workspace,
            server,
            ttyd: stack.ttyd.clone(),
            web_port,
            share_port,
        }
    }

    pub(super) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.web_port)
    }

    pub(super) fn display_name(&self) -> String {
        workspace_display_name(&self.workspace)
    }

    pub(super) fn share_base_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.share_port)
    }

    pub(super) fn open(&self) -> OpenedWeb {
        let output = self
            .command()
            .args(["--mux", "tmux", "web", "open", "--session"])
            .arg(&self.workspace.session_name)
            .args(["--print", "--json"])
            .bounded_output()
            .expect("open real ttyd web daemon");
        let payload: rimz::web::WebOpenPayload = success_json(&output, "web open");
        let credential = payload.credential.expect("web open credential");
        OpenedWeb {
            url: payload.url,
            secret: credential.secret,
        }
    }

    pub(super) fn share(&self) -> SharedWeb {
        let output = self
            .command()
            .args(["--mux", "tmux", "web", "share", "--session"])
            .arg(&self.workspace.session_name)
            .args(["--print", "--json"])
            .bounded_output()
            .expect("start real ttyd broadcast");
        let payload: rimz::web::WebSharePayload = success_json(&output, "web share");
        SharedWeb { url: payload.url }
    }

    pub(super) fn send_line(&self, line: &str) {
        self.server.output(&[
            "send-keys",
            "-t",
            &self.workspace.session_name,
            line,
            "Enter",
        ]);
    }

    pub(super) fn interrupt(&self) {
        self.server
            .output(&["send-keys", "-t", &self.workspace.session_name, "C-c"]);
    }

    pub(super) fn wait_capture_contains(&self, needle: &str, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        loop {
            let capture = self.capture(&self.workspace.session_name);
            if capture.contains(needle) {
                return capture;
            }
            assert!(
                Instant::now() < deadline,
                "tmux pane did not contain {needle:?}; last capture:\n{capture}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(super) fn assert_capture_absent(&self, needle: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let capture = self.capture(&self.workspace.session_name);
            assert!(
                !capture.contains(needle),
                "read-only browser input reached tmux:\n{capture}"
            );
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(super) fn add_room(&self, name: &str) -> rimz::ResolvedWorkspace {
        let root = self.env.project_root.join(name);
        self.env.write_config(&root, "");
        self.env.record(&root);
        let workspace = rimz::WorkspaceResolver::resolve(&root, None).expect("resolve second room");
        self.server.output(&[
            "new-session",
            "-d",
            "-s",
            &workspace.session_name,
            "-c",
            &root.to_string_lossy(),
            "sh",
        ]);
        workspace
    }

    fn capture(&self, session: &str) -> String {
        String::from_utf8_lossy(
            &self
                .server
                .output(&["capture-pane", "-p", "-S", "-", "-t", session])
                .stdout,
        )
        .into_owned()
    }

    fn command(&self) -> Command {
        let mut command = self.env.rimz();
        command
            .env("RIMZ_TTYD_BIN", &self.ttyd)
            .env("RIMZ_WEB_FONTS_OFFLINE", "1");
        command
    }
}

impl Drop for LiveWebFixture {
    fn drop(&mut self) {
        let _ = self.command().args(["web", "stop"]).bounded_output();
    }
}

pub(super) struct LiveZellijWebFixture {
    _guard: rimz::store::lock::WorkspaceLock,
    pub(super) env: Env,
    pub(super) workspace: rimz::ResolvedWorkspace,
    namespace: ZellijNamespace,
    ttyd: PathBuf,
}

impl LiveZellijWebFixture {
    pub(super) fn new(stack: &WebStack) -> Self {
        let guard = daemon_test_guard();
        let env = Env::new();
        env.write_config(&env.project_root, "");
        env.record(&env.project_root);
        let workspace =
            rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
        let namespace = ZellijNamespace::new();
        std::fs::write(namespace.path().join(".zshrc"), "").expect("disable zsh first-run menu");
        let output = namespace
            .command()
            .args([
                "attach",
                "--create-background",
                &workspace.session_name,
                "options",
                "--default-cwd",
            ])
            .arg(&env.project_root)
            .bounded_output()
            .expect("create live Zellij room");
        assert_success(&output, "create live Zellij room");
        let web_port = free_loopback_port();
        let share_port = free_loopback_port();
        write_machine_config(
            &env,
            &format!("[web]\nport = {web_port}\nshare_port = {share_port}\n"),
        );
        Self {
            _guard: guard,
            env,
            workspace,
            namespace,
            ttyd: stack.ttyd.clone(),
        }
    }

    pub(super) fn open(&self) -> OpenedWeb {
        let output = self
            .command()
            .args(["--mux", "zellij", "web", "open", "--session"])
            .arg(&self.workspace.session_name)
            .args(["--print", "--json"])
            .bounded_output()
            .expect("open real ttyd Zellij daemon");
        let payload: rimz::web::WebOpenPayload = success_json(&output, "Zellij web open");
        let credential = payload.credential.expect("Zellij web open credential");
        OpenedWeb {
            url: payload.url,
            secret: credential.secret,
        }
    }

    pub(super) fn display_name(&self) -> String {
        workspace_display_name(&self.workspace)
    }

    pub(super) fn send_line(&self, line: &str) {
        self.wait_until_attached();
        let typed = self
            .namespace
            .command()
            .args([
                "--session",
                &self.workspace.session_name,
                "action",
                "write-chars",
                line,
            ])
            .bounded_output()
            .expect("type into Zellij room");
        assert_success(&typed, "type into Zellij room");
        let enter = self
            .namespace
            .command()
            .args([
                "--session",
                &self.workspace.session_name,
                "action",
                "write",
                "13",
            ])
            .bounded_output()
            .expect("submit Zellij room input");
        assert_success(&enter, "submit Zellij room input");
    }

    fn wait_until_attached(&self) {
        let backend = ZellijBackend::with_runtime_dir(self.namespace.path());
        let deadline = Instant::now() + super::ATTACH_TIMEOUT;
        let mut consecutive_matches = 0;
        let mut last_human_clients = 0;
        let mut last_viewed_panes = Vec::new();
        let mut last_error = String::new();
        loop {
            match backend.client_view(ClientFocusOptions {
                session_name: Some(self.workspace.session_name.clone()),
                ..Default::default()
            }) {
                Ok(view) => {
                    last_human_clients = view.presence.human_clients;
                    last_viewed_panes = view.viewed_panes;
                    last_error.clear();
                    consecutive_matches =
                        if last_human_clients == 1 && !last_viewed_panes.is_empty() {
                            consecutive_matches + 1
                        } else {
                            0
                        };
                    if consecutive_matches == 2 {
                        return;
                    }
                }
                Err(error) => {
                    consecutive_matches = 0;
                    last_error = error.to_string();
                }
            }
            assert!(
                Instant::now() < deadline,
                "Zellij web client did not become attached; last human client count: \
                 {last_human_clients}; last viewed panes: {last_viewed_panes:?}; last error: \
                 {last_error}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn command(&self) -> Command {
        let mut command = self.env.rimz();
        command
            .env("XDG_RUNTIME_DIR", self.namespace.path())
            .env("TMPDIR", self.namespace.path())
            .env(
                "ZELLIJ_CONFIG_DIR",
                self.namespace.path().join(".config/zellij"),
            )
            .env("RIMZ_TTYD_BIN", &self.ttyd)
            .env("RIMZ_WEB_FONTS_OFFLINE", "1");
        command
    }
}

impl Drop for LiveZellijWebFixture {
    fn drop(&mut self) {
        let _ = self.command().args(["web", "stop"]).bounded_output();
        self.namespace.delete_session(&self.workspace.session_name);
    }
}

pub(super) fn wait_term_contains(tab: &Tab, needle: &str, budget: Duration) {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    let mut last_error = String::new();
    loop {
        match term_text(tab) {
            Ok(Some(text)) => {
                last = text;
                if last.contains(needle) {
                    return;
                }
            }
            Ok(None) => {}
            Err(err) => last_error = err,
        }
        assert!(
            Instant::now() < deadline,
            "terminal did not contain {needle:?}; last text:\n{last}\nlast error: {last_error}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn assert_room_attached(tab: &Tab, session: &str, display_name: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let search = eval_string(tab, "window.location.search").unwrap_or_default();
        let title = eval_string(tab, "document.title").unwrap_or_default();
        if search.contains(&format!("room={session}")) && title == format!("{display_name} · RimZ")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "room state did not synchronize; search={search:?}, title={title:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn assert_no_term(tab: &Tab, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        let has_term = eval_value(tab, "Boolean(window.term)")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        assert!(!has_term, "rejected browser loaded xterm");
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn eval_string(tab: &Tab, expression: &str) -> Result<String, String> {
    eval_value(tab, expression)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{expression} did not return a string"))
}

fn term_text(tab: &Tab) -> Result<Option<String>, String> {
    let expression = r#"(()=>{
const term=window.term;
if(!term||!term.buffer)return null;
const buffer=term.buffer.active;
const lines=[];
for(let index=0;index<buffer.length;index++){
  const line=buffer.getLine(index);
  lines.push(line?line.translateToString(true):"");
}
return lines.join("\n");
})()"#;
    let value = eval_value(tab, expression)?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| "terminal buffer evaluation did not return a string".to_owned())
}

fn eval_value(tab: &Tab, expression: &str) -> Result<serde_json::Value, String> {
    tab.evaluate(expression, false)
        .map_err(|err| err.to_string())?
        .value
        .ok_or_else(|| format!("{expression} returned no value"))
}

fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind test web port")
        .local_addr()
        .expect("test web address")
        .port()
}

fn write_machine_config(env: &Env, text: &str) {
    let path = env.config_root().join("rimz").join("config.toml");
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config parent");
    std::fs::write(path, text).expect("write machine config");
}

fn workspace_display_name(workspace: &rimz::ResolvedWorkspace) -> String {
    workspace
        .project_root
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(&workspace.session_name)
        .to_owned()
}

fn success_json<T: serde::de::DeserializeOwned>(output: &Output, action: &str) -> T {
    assert_success(output, action);
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "{action} emits JSON: {err}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} succeeds\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
mod tests {
    use super::parse_ttyd_version;

    #[test]
    fn ttyd_version_parser_accepts_packaged_suffixes() {
        assert_eq!(
            parse_ttyd_version("ttyd version 1.7.7-1+deb13u1"),
            Some((1, 7, 7))
        );
        assert_eq!(parse_ttyd_version("ttyd version 1.7"), None);
    }
}
