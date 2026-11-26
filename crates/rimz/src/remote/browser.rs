//! The laptop browser relay: the room's `BROWSER` helper, the URL rule, and the wire
//! between that helper and the host end of a supervised link.
//!
//! Every room pane carries the room's helper (`StatePaths::browser_helper`) as
//! `BROWSER`, a shim that runs `rimz remote open-url -- <url>`. That helper offers the
//! URL to the live links of its room (one socket per capable `rimz remote link-stats
//! ingest`, see [`socket_path`]), most recently active laptop first, and falls back to
//! the user's own browser, the platform opener, then the clipboard.
//! This module only decides; `cli/remote/` owns the sockets, spawns, and terminal writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::disk::paths::RuntimePaths;
use crate::sock;

const OPEN_SCHEMA_VERSION: &str = "rimz.open.v1";
/// The helper's file name, which `StatePaths::browser_helper` spells.
const HELPER_FILE: &str = "rimz-browser";
const HELPER_MARKER: &str = "# rimz browser helper";
const SOCKET_PREFIX: &str = "link-open-";
const SOCKET_SUFFIX: &str = ".sock";

/// The user's own `BROWSER` from the environment the room was born in, kept beside
/// the helper the room sets. Present and empty means the user had none.
pub const USER_BROWSER_ENV: &str = "RIMZ_USER_BROWSER";
/// The variable browsers are opened through.
pub(crate) const BROWSER_ENV: &str = "BROWSER";

/// The helper's script: one `exec` of `rimz_bin`, so a `BROWSER` reader gets a plain
/// program path. `None` when the binary path cannot be quoted for `sh`.
pub(crate) fn helper_script(rimz_bin: &Path) -> Option<String> {
    let quoted = shlex::try_quote(rimz_bin.to_str()?).ok()?;
    Some(format!(
        "#!/bin/sh\n{HELPER_MARKER}\nexec {quoted} remote open-url -- \"$@\"\n"
    ))
}

/// The `BROWSER` value for the helper: one absolute path with no `:` or `%`, the
/// one form Claude Code, the `webbrowser` crate, and `xdg-open` read alike.
pub(crate) fn browser_value(helper: &Path) -> Option<String> {
    let value = helper.to_str()?;
    (helper.is_absolute() && !value.contains([':', '%'])).then(|| value.to_owned())
}

/// Whether a `BROWSER` value is a room's helper, such as the one a room born from
/// inside another room inherits.
pub(crate) fn is_helper(browser: &str) -> bool {
    Path::new(browser).file_name() == Some(HELPER_FILE.as_ref())
}

/// The room environment for the helper: `BROWSER` set to it and the user's own value
/// saved in [`USER_BROWSER_ENV`]. A birth environment whose `BROWSER` is already a
/// helper carries its saved value forward instead of saving the helper as the user's browser.
pub(crate) fn room_env(
    helper: &str,
    ambient_browser: Option<&str>,
    ambient_saved: Option<&str>,
) -> BTreeMap<String, String> {
    let saved = match ambient_browser {
        Some(browser) if is_helper(browser) => ambient_saved.unwrap_or_default(),
        Some(browser) => browser,
        None => "",
    };
    BTreeMap::from([
        (BROWSER_ENV.to_owned(), helper.to_owned()),
        (USER_BROWSER_ENV.to_owned(), saved.to_owned()),
    ])
}

/// What a child spawned to open a page gets as `BROWSER`, so the helper never runs
/// itself again through `xdg-open`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BrowserRestore<'a> {
    /// Not a room process: leave the child's environment alone.
    Keep,
    /// Set `BROWSER` to the user's value and drop the save variable.
    Set(&'a str),
    /// Remove `BROWSER` and the save variable.
    Remove,
}

/// Decide [`BrowserRestore`] from this process's `BROWSER` and [`USER_BROWSER_ENV`].
pub(crate) fn browser_restore<'a>(
    browser: Option<&str>,
    saved: Option<&'a str>,
) -> BrowserRestore<'a> {
    match saved {
        Some("") => BrowserRestore::Remove,
        Some(saved) if saved.split(':').any(is_helper) => BrowserRestore::Remove,
        Some(saved) => BrowserRestore::Set(saved),
        None if browser.is_some_and(is_helper) => BrowserRestore::Remove,
        None => BrowserRestore::Keep,
    }
}

/// Give `cmd` the user's own `BROWSER` in place of the room's helper, as
/// `browser_restore` decides from this process's environment, so a page opener
/// a room process spawns never runs the helper again.
pub fn restore_user_browser_env(cmd: &mut std::process::Command) {
    let browser = std::env::var(BROWSER_ENV).ok();
    let saved = std::env::var(USER_BROWSER_ENV).ok();
    match browser_restore(browser.as_deref(), saved.as_deref()) {
        BrowserRestore::Keep => {}
        BrowserRestore::Set(saved) => {
            cmd.env(BROWSER_ENV, saved).env_remove(USER_BROWSER_ENV);
        }
        BrowserRestore::Remove => {
            cmd.env_remove(BROWSER_ENV).env_remove(USER_BROWSER_ENV);
        }
    }
}

/// How long after its last probe a link still takes a URL: three probe intervals,
/// the laptop's own when its probe names one. A laptop that slept or lost its
/// network leaves the host's stream open with nobody reading it, so a link this
/// quiet is skipped rather than trusted.
pub fn open_fresh_for(interval_ms: Option<u64>) -> Duration {
    interval_ms
        .filter(|ms| *ms > 0)
        .map_or(super::link::LINK_PROBE_INTERVAL, Duration::from_millis)
        .saturating_mul(3)
}

/// The argvs that open `url` with the user's saved `BROWSER`, in the order to try
/// them: each `:` entry in turn, an entry naming a program path first as that one
/// program (spaces included), then split as a shell word list with a `%s` word
/// replaced by the URL or the URL appended. The helper itself is never among them.
pub fn user_browser_argvs(saved: &str, url: &str) -> Vec<Vec<String>> {
    let mut argvs = Vec::new();
    for entry in saved.split(':').filter(|entry| !entry.trim().is_empty()) {
        if entry.contains('/') && !entry.contains("%s") {
            argvs.push(vec![entry.to_owned(), url.to_owned()]);
        }
        if let Some(words) = shlex::split(entry) {
            argvs.push(with_url(words, url));
        }
    }
    argvs.dedup();
    argvs.retain(|argv| argv.first().is_some_and(|program| !is_helper(program)));
    argvs
}

fn with_url(mut argv: Vec<String>, url: &str) -> Vec<String> {
    let mut substituted = false;
    for word in &mut argv {
        if word.contains("%s") {
            *word = word.replace("%s", url);
            substituted = true;
        }
    }
    if !substituted {
        argv.push(url.to_owned());
    }
    argv
}

/// An http(s) URL the relay and the clipboard may carry, with what an assist
/// record may name of it.
#[derive(Debug, PartialEq, Eq)]
pub struct WebUrl<'a> {
    pub url: &'a str,
    pub scheme: String,
    /// The host only: a sign-in link's path and query carry secrets.
    pub host: String,
}

/// Why a URL may not cross the link or reach the clipboard.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UrlRefusal {
    #[error("the URL carries a control character or whitespace")]
    Control,
    #[error("the URL does not parse")]
    Malformed,
    #[error("only http and https URLs cross the link, not {0}:")]
    Scheme(String),
    #[error("the URL names no host")]
    NoHost,
}

/// Accept `url` only as an http(s) URL with a host and no control character or
/// whitespace, checked before parsing because the parser silently strips tabs and newlines.
pub fn web_url(url: &str) -> Result<WebUrl<'_>, UrlRefusal> {
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(UrlRefusal::Control);
    }
    let parsed = url::Url::parse(url).map_err(|_| UrlRefusal::Malformed)?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(UrlRefusal::Scheme(scheme.to_owned()));
    }
    let host = parsed.host_str().ok_or(UrlRefusal::NoHost)?;
    Ok(WebUrl {
        url,
        scheme: scheme.to_owned(),
        host: host.to_owned(),
    })
}

/// The OSC 52 sequence that puts `url` on the clipboard of the terminal that shows it.
/// The URL travels base64-encoded, so none of its bytes reach the terminal raw.
pub fn clipboard_sequence(url: &WebUrl<'_>) -> Vec<u8> {
    let payload = base64::engine::general_purpose::STANDARD.encode(url.url);
    format!("\x1b]52;c;{payload}\x07").into_bytes()
}

/// The socket a capable ingest with process id `pid` listens on.
pub fn socket_path(
    runtime: &RuntimePaths,
    pid: u32,
) -> std::result::Result<PathBuf, sock::SocketPathTooLong> {
    let path = runtime
        .sock_dir
        .join(format!("{SOCKET_PREFIX}{pid}{SOCKET_SUFFIX}"));
    sock::validate_socket_path(&path)?;
    Ok(path)
}

/// The ingest process id a socket file name belongs to, for every file
/// [`socket_path`] names.
pub fn socket_pid(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix(SOCKET_PREFIX)?
        .strip_suffix(SOCKET_SUFFIX)?
        .parse()
        .ok()
}

/// The order the helper offers a URL to its room's links in: the laptop whose
/// terminal took input most recently first, links with no activity reading last.
pub fn relay_order<T>(mut links: Vec<(T, Option<u64>)>) -> Vec<T> {
    links.sort_by_key(|(_, active_at_ms)| std::cmp::Reverse(*active_at_ms));
    links.into_iter().map(|(link, _)| link).collect()
}

/// When the laptop's terminal last took input on the host's clock: the probe's
/// receipt less the idle time it reported.
pub fn active_at_ms(received_at_ms: u64, idle_ms: Option<u64>) -> Option<u64> {
    idle_ms.map(|idle| received_at_ms.saturating_sub(idle))
}

/// The ingest's first line to a helper that connects: when its laptop last took input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenStatus {
    pub v: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_at_ms: Option<u64>,
}

impl OpenStatus {
    pub fn new(active_at_ms: Option<u64>) -> Self {
        Self {
            v: OPEN_SCHEMA_VERSION.to_owned(),
            active_at_ms,
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == OPEN_SCHEMA_VERSION
    }
}

/// The helper's request: open this URL on your laptop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub v: String,
    pub url: String,
}

impl OpenRequest {
    pub fn new(url: &str) -> Self {
        Self {
            v: OPEN_SCHEMA_VERSION.to_owned(),
            url: url.to_owned(),
        }
    }

    pub fn version_ok(&self) -> bool {
        self.v == OPEN_SCHEMA_VERSION
    }
}

/// The ingest's answer, sent once the URL is on the probe stream or refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenReply {
    pub v: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OpenReply {
    pub fn sent() -> Self {
        Self {
            v: OPEN_SCHEMA_VERSION.to_owned(),
            error: None,
        }
    }

    pub fn refused(error: impl Into<String>) -> Self {
        Self {
            v: OPEN_SCHEMA_VERSION.to_owned(),
            error: Some(error.into()),
        }
    }

    pub fn delivered(&self) -> bool {
        self.v == OPEN_SCHEMA_VERSION && self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_url_accepts_http_and_https_and_names_only_scheme_and_host() {
        let url = "https://user:pw@github.com:8443/rimio-ai/rimz/pull/1967?token=secret#frag";
        assert_eq!(
            web_url(url),
            Ok(WebUrl {
                url,
                scheme: "https".into(),
                host: "github.com".into(),
            })
        );
        assert_eq!(web_url("HTTP://localhost:3000/").unwrap().scheme, "http");
        for (input, refusal) in [
            ("https://a.test/x\nhttps://b.test", UrlRefusal::Control),
            ("https://a.test/\u{1b}]52;c;x\u{7}", UrlRefusal::Control),
            ("https://a.test/\tx", UrlRefusal::Control),
            ("https://a.test/ x", UrlRefusal::Control),
            ("vscode://file/tmp/x", UrlRefusal::Scheme("vscode".into())),
            (
                "javascript:alert(1)",
                UrlRefusal::Scheme("javascript".into()),
            ),
            ("file:///etc/passwd", UrlRefusal::Scheme("file".into())),
            ("not a url", UrlRefusal::Control),
            ("github.com/x", UrlRefusal::Malformed),
        ] {
            assert_eq!(web_url(input), Err(refusal), "{input:?}");
        }
    }

    #[test]
    fn clipboard_sequence_carries_the_url_base64_encoded() {
        let url = web_url("https://a.test/?q=1").unwrap();
        assert_eq!(
            clipboard_sequence(&url),
            b"\x1b]52;c;aHR0cHM6Ly9hLnRlc3QvP3E9MQ==\x07"
        );
    }

    #[test]
    fn room_env_saves_the_users_browser_and_carries_a_nested_rooms_save_forward() {
        let helper = "/run/user/1/rimz/ws/a/rimz-browser";
        let env = |browser, saved| {
            let env = room_env(helper, browser, saved);
            assert_eq!(env[BROWSER_ENV], helper);
            env[USER_BROWSER_ENV].clone()
        };
        assert_eq!(env(Some("/usr/bin/firefox"), None), "/usr/bin/firefox");
        assert_eq!(
            env(Some("/usr/bin/firefox"), Some("/stale")),
            "/usr/bin/firefox"
        );
        assert_eq!(env(None, None), "");
        assert_eq!(
            env(
                Some("/run/user/1/rimz/ws/outer/rimz-browser"),
                Some("/opt/w3m")
            ),
            "/opt/w3m"
        );
        assert_eq!(
            env(Some(helper), None),
            "",
            "a helper is never the user's browser"
        );
    }

    #[test]
    fn browser_value_is_one_absolute_path_without_list_or_format_syntax() {
        assert_eq!(
            browser_value(Path::new("/run/rimz/rimz-browser")).as_deref(),
            Some("/run/rimz/rimz-browser")
        );
        for refused in [
            "rimz-browser",
            "/run/a:b/rimz-browser",
            "/run/100%/rimz-browser",
        ] {
            assert_eq!(browser_value(Path::new(refused)), None, "{refused}");
        }
    }

    #[test]
    fn helper_script_execs_the_open_verb_with_the_url() {
        let script = helper_script(Path::new("/state/my room/rimz")).unwrap();
        assert_eq!(
            script,
            "#!/bin/sh\n# rimz browser helper\nexec '/state/my room/rimz' remote open-url -- \"$@\"\n"
        );
    }

    #[test]
    fn browser_restore_never_hands_a_child_the_helper() {
        let helper = Some("/run/rimz/rimz-browser");
        assert_eq!(browser_restore(helper, Some("")), BrowserRestore::Remove);
        assert_eq!(
            browser_restore(helper, Some("/usr/bin/firefox")),
            BrowserRestore::Set("/usr/bin/firefox")
        );
        assert_eq!(browser_restore(helper, None), BrowserRestore::Remove);
        assert_eq!(browser_restore(helper, helper), BrowserRestore::Remove);
        assert_eq!(
            browser_restore(helper, Some("w3m:/run/rimz/rimz-browser")),
            BrowserRestore::Remove
        );
        assert_eq!(browser_restore(Some("firefox"), None), BrowserRestore::Keep);
        assert_eq!(browser_restore(None, None), BrowserRestore::Keep);
    }

    #[test]
    fn user_browser_argvs_try_each_entry_and_a_program_path_whole_first() {
        let url = "https://a.test/";
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|&word| word.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            user_browser_argvs("/usr/bin/firefox", url),
            [argv(&["/usr/bin/firefox", url])]
        );
        assert_eq!(
            user_browser_argvs("firefox --new-tab %s:chromium", url),
            [
                argv(&["firefox", "--new-tab", url]),
                argv(&["chromium", url])
            ]
        );
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        assert_eq!(
            user_browser_argvs(chrome, url),
            [
                argv(&[chrome, url]),
                argv(&[
                    "/Applications/Google",
                    "Chrome.app/Contents/MacOS/Google",
                    "Chrome",
                    url
                ]),
            ]
        );
        assert!(user_browser_argvs(":", url).is_empty());
        assert_eq!(
            user_browser_argvs("/run/x/rimz-browser:/usr/bin/firefox", url),
            [argv(&["/usr/bin/firefox", url])]
        );
    }

    #[test]
    fn a_link_takes_urls_for_three_probe_intervals() {
        assert_eq!(open_fresh_for(None), Duration::from_secs(6));
        assert_eq!(open_fresh_for(Some(0)), Duration::from_secs(6));
        assert_eq!(open_fresh_for(Some(100)), Duration::from_millis(300));
    }

    #[test]
    fn relay_order_puts_the_most_recently_active_laptop_first() {
        assert_eq!(
            relay_order(vec![
                ("idle", Some(1_000)),
                ("unknown", None),
                ("active", Some(5_000))
            ]),
            ["active", "idle", "unknown"]
        );
        assert_eq!(active_at_ms(10_000, Some(4_000)), Some(6_000));
        assert_eq!(active_at_ms(1_000, Some(4_000)), Some(0));
        assert_eq!(active_at_ms(1_000, None), None);
    }

    #[test]
    fn socket_names_round_trip_to_their_ingest_pid() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(Path::new("/p")),
            root.path(),
        )
        .unwrap();
        let path = socket_path(&runtime, 4242).unwrap();
        assert!(path.starts_with(&runtime.sock_dir));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(socket_pid(name), Some(4242));
        for other in [
            "link-open-x.sock",
            "link-4242.sock",
            "link-open-4242.sock.tmp",
        ] {
            assert_eq!(socket_pid(other), None, "{other}");
        }
    }

    #[test]
    fn host_lines_keep_open_lines_apart_from_acks() {
        use crate::remote::link::{HostLine, LinkAck, LinkOpen, parse_host_line};

        let open = LinkOpen::new("https://a.test/x");
        let open_text = serde_json::to_string(&open).unwrap();
        assert_eq!(parse_host_line(&open_text), Some(HostLine::Open(open)));
        assert!(
            serde_json::from_str::<LinkAck>(&open_text).is_err(),
            "an open line is never read as an ack, so health accounting never sees it"
        );
        let ack_text = serde_json::to_string(&LinkAck::new(42)).unwrap();
        assert_eq!(
            parse_host_line(&ack_text),
            Some(HostLine::Ack(LinkAck::new(42)))
        );
        assert_eq!(
            parse_host_line(r#"{"v":"rimz.link.v2","open":"https://a.test/"}"#),
            None
        );
        assert_eq!(parse_host_line("not json"), None);
    }

    #[test]
    fn open_wire_round_trips_and_reads_an_unknown_version_as_undelivered() {
        let status = OpenStatus::new(Some(7));
        assert_eq!(
            serde_json::from_str::<OpenStatus>(&serde_json::to_string(&status).unwrap()).unwrap(),
            status
        );
        assert!(
            serde_json::from_str::<OpenStatus>(r#"{"v":"rimz.open.v1"}"#)
                .unwrap()
                .version_ok()
        );
        let request = OpenRequest::new("https://a.test/");
        assert_eq!(
            serde_json::from_str::<OpenRequest>(&serde_json::to_string(&request).unwrap()).unwrap(),
            request
        );
        assert!(OpenReply::sent().delivered());
        assert!(!OpenReply::refused("stream closed").delivered());
        assert!(
            !serde_json::from_str::<OpenReply>(r#"{"v":"rimz.open.v2"}"#)
                .unwrap()
                .delivered()
        );
    }
}
