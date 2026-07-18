//! Resolves pet selectors into bytes from the built-in CDN, HTTPS URLs,
//! petdex installs, local sheets, and the per-machine cache.

use std::borrow::Cow;
use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::catalog::{Pet, pet_by_id};
use super::frames;

const CDN_BASE: &str = "https://persistent.oaistatic.com/codex/pets/v1";
const TIMEOUT_CONNECT_SECS: u64 = 5;
const TIMEOUT_RESPONSE_SECS: u64 = 10;
const TIMEOUT_BODY_SECS: u64 = 30;
const MAX_FETCH_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF_MS: u64 = 250;
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AssetErr {
    #[error("pet asset fetches are disabled")]
    Offline,
    #[error("pet asset fetch failed: {0}")]
    Fetch(String),
    #[error("pet asset decode failed: {0}")]
    Decode(#[from] frames::FrameErr),
    #[error("pet manifest error at {path}: {detail}")]
    Manifest { path: PathBuf, detail: String },
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Where a pet's spritesheet comes from: a built-in fetched from the public CDN
/// and cached, a user-supplied HTTPS URL fetched and cached the same way, a
/// petdex pet installed under `~/.codex/pets/<name>/`, or a local sheet read
/// straight off disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PetSource {
    Builtin(Pet),
    Remote(String),
    Petdex(String),
    Local(PathBuf),
}

impl PetSource {
    /// Stable identity for change detection and the failure latch: the catalog
    /// id for a built-in, the URL for a remote sheet, the name for a petdex pet,
    /// the path string for a local one.
    pub(crate) fn id(&self) -> Cow<'static, str> {
        match self {
            PetSource::Builtin(pet) => Cow::Borrowed(pet.id),
            PetSource::Remote(url) => Cow::Owned(url.clone()),
            PetSource::Petdex(name) => Cow::Owned(name.clone()),
            PetSource::Local(path) => Cow::Owned(path.to_string_lossy().into_owned()),
        }
    }
}

/// Resolve the configured `pet` string to its source. A built-in catalog id
/// wins; an `http(s)://` selector is a remote sheet; a path-like selector (one
/// with a `/`, a `.`, or a leading `~`) is a local file or directory; and a
/// bare slug is a petdex pet looked up under `~/.codex/pets/`. This mirrors how
/// the theme `scheme` field accepts a bundled name or a file path, extended with
/// URL and petdex sources. An empty selector resolves to nothing.
pub(crate) fn resolve_pet_source(spec: &str) -> Option<PetSource> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(pet) = pet_by_id(spec) {
        return Some(PetSource::Builtin(*pet));
    }
    if is_http_url(spec) {
        return Some(PetSource::Remote(spec.to_owned()));
    }
    if is_path_like(spec) {
        return Some(PetSource::Local(expand_home(Path::new(spec))));
    }
    Some(PetSource::Petdex(spec.to_owned()))
}

fn is_http_url(spec: &str) -> bool {
    spec.starts_with("https://") || spec.starts_with("http://")
}

/// Whether a selector names a filesystem path rather than a bare petdex slug:
/// a path component (`/`), an extension or dotted segment (`.`), or a home
/// prefix (`~`). Petdex slugs (`wall-e`, `kaka-2`) carry none of these.
fn is_path_like(spec: &str) -> bool {
    spec.contains('/') || spec.contains('.') || spec.starts_with('~')
}

pub(crate) struct ResolvedAsset {
    pub(crate) bytes: Vec<u8>,
    /// The cache file to evict if a later decode fails. `None` for a
    /// user-supplied local sheet, which RimZ reads but never deletes.
    pub(crate) evictable_cache: Option<PathBuf>,
}

fn resolve_asset(source: &PetSource) -> Result<ResolvedAsset, AssetErr> {
    match source {
        PetSource::Builtin(pet) => {
            let pet = *pet;
            resolve_cached(asset_path(pet.file), offline(), || {
                fetch_url(&builtin_pet_url(pet))
            })
        }
        PetSource::Remote(url) => {
            require_https(url)?;
            resolve_cached(asset_path(&remote_cache_file(url)), offline(), || {
                fetch_url(url)
            })
        }
        PetSource::Petdex(name) => {
            let Some(root) = petdex_root() else {
                return Err(AssetErr::Manifest {
                    path: PathBuf::from(name),
                    detail: "HOME is not set, so ~/.codex/pets cannot be located".to_owned(),
                });
            };
            resolve_petdex_dir(&root.join(name))
        }
        // A local selector can point at a single sheet or at a petdex pet
        // directory; the directory form reads its `pet.json` for the sheet.
        PetSource::Local(path) if path.is_dir() => resolve_petdex_dir(path),
        PetSource::Local(path) => resolve_local(path),
    }
}

/// Resolve an asset and decode it under one eviction rule. Only fetched-cache
/// bytes are removable; local and petdex sources stay untouched.
pub(crate) fn resolve_and_decode<T>(
    source: &PetSource,
    decode: impl FnOnce(&[u8]) -> Result<T, frames::FrameErr>,
) -> Result<T, AssetErr> {
    let resolved = resolve_asset(source)?;
    decode_resolved(resolved, decode)
}

fn decode_resolved<T>(
    resolved: ResolvedAsset,
    decode: impl FnOnce(&[u8]) -> Result<T, frames::FrameErr>,
) -> Result<T, AssetErr> {
    match decode(&resolved.bytes) {
        Ok(decoded) => Ok(decoded),
        Err(err) => {
            if let Some(path) = resolved.evictable_cache {
                let _ = remove_cached_asset(&path);
            }
            Err(AssetErr::Decode(err))
        }
    }
}

/// Cache-first resolution for a fetched sheet, with the environment lifted out:
/// a valid cached sheet is served as-is, a corrupt one is removed and
/// re-fetched, and `offline` keeps the path read-only. `offline` and `path` are
/// passed in, and the fetch is a thunk, so the branching is testable without
/// touching env or the network. Built-ins and remote URLs share this path; only
/// the fetch source and the cache filename differ.
fn resolve_cached(
    path: PathBuf,
    offline: bool,
    fetch: impl FnOnce() -> Result<Vec<u8>, AssetErr>,
) -> Result<ResolvedAsset, AssetErr> {
    if let Ok(bytes) = std::fs::read(&path) {
        match frames::validate_sheet_geometry(&bytes) {
            Ok(()) => {
                return Ok(ResolvedAsset {
                    bytes,
                    evictable_cache: Some(path),
                });
            }
            Err(err) => {
                remove_cached_asset_path(&path)?;
                if offline {
                    return Err(AssetErr::Decode(err));
                }
            }
        }
    }
    if offline {
        return Err(AssetErr::Offline);
    }
    let bytes = fetch()?;
    frames::validate_sheet_geometry(&bytes)?;
    write_bytes_atomic(&path, &bytes)?;
    Ok(ResolvedAsset {
        bytes,
        evictable_cache: Some(path),
    })
}

/// Read and geometry-check a user-supplied local sheet. No network, no cache,
/// and never deletes the file — it is the user's, not RimZ's to evict.
fn resolve_local(path: &Path) -> Result<ResolvedAsset, AssetErr> {
    let bytes = std::fs::read(path).map_err(|source| AssetErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    frames::validate_sheet_geometry(&bytes)?;
    Ok(ResolvedAsset {
        bytes,
        evictable_cache: None,
    })
}

/// A petdex pet's `pet.json`: only the spritesheet location is needed here. Its
/// `id`/`displayName`/`description` are metadata RimZ does not render.
#[derive(serde::Deserialize)]
struct PetManifest {
    #[serde(rename = "spritesheetPath")]
    spritesheet_path: String,
}

/// Resolve a petdex pet directory — a `pet.json` plus its spritesheet, as
/// installed under `~/.codex/pets/<name>/`. The manifest names the sheet
/// (relative to the directory, or absolute), which is then read and
/// geometry-checked like any local sheet and never deleted.
fn resolve_petdex_dir(dir: &Path) -> Result<ResolvedAsset, AssetErr> {
    let manifest_path = dir.join("pet.json");
    let raw = std::fs::read_to_string(&manifest_path).map_err(|source| AssetErr::Io {
        path: manifest_path.clone(),
        source,
    })?;
    let manifest: PetManifest = serde_json::from_str(&raw).map_err(|err| AssetErr::Manifest {
        path: manifest_path,
        detail: err.to_string(),
    })?;
    resolve_local(&dir.join(manifest.spritesheet_path))
}

/// The petdex install root, `$HOME/.codex/pets`, matching where Codex installs
/// pets (and how the rest of RimZ locates `~/.codex`). `None` when `HOME` is
/// unset.
fn petdex_root() -> Option<PathBuf> {
    petdex_root_from(env::var_os("HOME").map(PathBuf::from))
}

fn petdex_root_from(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join(".codex").join("pets"))
}

pub(crate) fn installed_petdex_pets() -> Vec<String> {
    petdex_root()
        .map(|root| installed_petdex_pets_in(&root))
        .unwrap_or_default()
}

fn installed_petdex_pets_in(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut pets = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("pet.json").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    pets.sort();
    pets
}

/// Reject a non-HTTPS pet URL with a clear message rather than a fetch failure;
/// remote sheets travel over TLS only, matching the built-in CDN.
fn require_https(url: &str) -> Result<(), AssetErr> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(AssetErr::Fetch(format!("pet URL must be https: {url}")))
    }
}

/// A stable cache filename for a remote sheet, derived from a SHA-256 of the
/// URL so distinct URLs never collide and the same URL reuses its cache across
/// runs.
fn remote_cache_file(url: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(url.as_bytes());
    let mut name = String::from("remote-");
    for byte in digest.iter().take(16) {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".webp");
    name
}

pub(crate) fn remove_cached_asset(path: &Path) -> Result<(), AssetErr> {
    remove_cached_asset_path(path)
}

pub(crate) fn builtin_pet_url(pet: Pet) -> String {
    format!("{CDN_BASE}/{}", pet.file)
}

fn fetch_url(url: &str) -> Result<Vec<u8>, AssetErr> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(TIMEOUT_CONNECT_SECS)))
        .timeout_recv_response(Some(Duration::from_secs(TIMEOUT_RESPONSE_SECS)))
        .timeout_recv_body(Some(Duration::from_secs(TIMEOUT_BODY_SECS)))
        .build()
        .new_agent();
    retrying(
        MAX_FETCH_ATTEMPTS,
        Duration::from_millis(RETRY_BACKOFF_MS),
        || fetch_once(&agent, url),
    )
}

fn fetch_once(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, AssetErr> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|err| AssetErr::Fetch(err.to_string()))?;
    if response.status().as_u16() != 200 {
        return Err(AssetErr::Fetch(format!(
            "non-200 status {}",
            response.status().as_u16()
        )));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_vec()
        .map_err(|err| AssetErr::Fetch(err.to_string()))
}

/// Run `op` up to `attempts` times and return the first success. Failed
/// attempts sleep with linear backoff; the final error returns when the attempt
/// budget is spent.
fn retrying<T>(
    attempts: u32,
    backoff: Duration,
    mut op: impl FnMut() -> Result<T, AssetErr>,
) -> Result<T, AssetErr> {
    let attempts = attempts.max(1);
    let mut attempt = 1;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) if attempt >= attempts => return Err(err),
            Err(_) => {
                std::thread::sleep(backoff * attempt);
                attempt += 1;
            }
        }
    }
}

fn offline() -> bool {
    env::var_os("RIMZ_PETS_OFFLINE").is_some()
}

fn asset_path(file: &str) -> PathBuf {
    cache_home().join("rimz/pets/v1/assets").join(file)
}

fn cache_home() -> PathBuf {
    cache_home_from(env_path("XDG_CACHE_HOME"), env_path("HOME"))
}

fn cache_home_from(xdg_cache_home: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    if let Some(value) = xdg_cache_home {
        return value;
    }
    if let Some(home) = home {
        return home.join(".cache");
    }
    env::temp_dir().join("rimz-cache")
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Expand a leading `~` / `~/` against `HOME` so a local pet path reads the way
/// users write it; any other path is taken verbatim (relative to the cwd).
fn expand_home(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    if raw == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), AssetErr> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AssetErr::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = temp_sibling(path);
    {
        let mut file = File::create(&tmp).map_err(|source| AssetErr::Io {
            path: tmp.clone(),
            source,
        })?;
        file.write_all(bytes).map_err(|source| AssetErr::Io {
            path: tmp.clone(),
            source,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|source| AssetErr::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_cached_asset_path(path: &Path) -> Result<(), AssetErr> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(AssetErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pet-asset");
    path.with_file_name(format!("{file}.tmp.{}.{stamp}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::pets::catalog::pet_by_id;

    #[test]
    fn builtin_pet_url_uses_public_cdn_path() {
        let pet = *pet_by_id("codex").expect("codex pet");
        assert_eq!(
            builtin_pet_url(pet),
            "https://persistent.oaistatic.com/codex/pets/v1/codex-spritesheet-v4.webp"
        );
    }

    #[test]
    fn cache_and_petdex_roots_resolve_from_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            cache_home_from(
                Some(dir.path().to_path_buf()),
                Some(PathBuf::from("/home/a"))
            ),
            dir.path()
        );
        assert_eq!(
            cache_home_from(None, Some(PathBuf::from("/home/a"))),
            PathBuf::from("/home/a/.cache")
        );
        assert_eq!(
            petdex_root_from(Some(PathBuf::from("/home/a"))),
            Some(PathBuf::from("/home/a/.codex/pets"))
        );
        assert_eq!(petdex_root_from(None), None);
        assert_eq!(
            petdex_root_from(Some(PathBuf::new())),
            None,
            "empty HOME is no root"
        );
    }

    #[test]
    fn offline_without_cache_reports_offline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sheet.webp");
        assert!(matches!(
            resolve_cached(path, true, || panic!("offline must not fetch")),
            Err(AssetErr::Offline)
        ));
    }

    #[test]
    fn offline_corrupt_cache_is_removed_and_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sheet.webp");
        std::fs::write(&path, b"not a webp").expect("seed corrupt cache");
        assert!(matches!(
            resolve_cached(path.clone(), true, || panic!("offline must not fetch")),
            Err(AssetErr::Decode(_))
        ));
        assert!(!path.exists(), "corrupt cache entry is removed");
    }

    #[test]
    fn retrying_returns_first_success_and_stops() {
        let calls = std::cell::Cell::new(0);
        let out = retrying(3, Duration::ZERO, || {
            calls.set(calls.get() + 1);
            if calls.get() < 2 {
                Err(AssetErr::Fetch("transient".to_owned()))
            } else {
                Ok(7)
            }
        });
        assert!(matches!(out, Ok(7)));
        assert_eq!(calls.get(), 2, "stops retrying once it succeeds");
    }

    #[test]
    fn retrying_gives_up_after_the_attempt_budget() {
        let calls = std::cell::Cell::new(0);
        let out: Result<(), AssetErr> = retrying(3, Duration::ZERO, || {
            calls.set(calls.get() + 1);
            Err(AssetErr::Fetch("down".to_owned()))
        });
        assert!(matches!(out, Err(AssetErr::Fetch(_))));
        assert_eq!(calls.get(), 3, "exactly the attempt budget");
    }

    #[test]
    fn failed_fetch_leaves_no_cache_entry_so_reruns_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sheet.webp");
        let result = resolve_cached(path.clone(), false, || {
            Err(AssetErr::Fetch("boom".to_owned()))
        });
        assert!(matches!(result, Err(AssetErr::Fetch(_))));
        assert!(!path.exists(), "a failed fetch writes no cache entry");
    }

    #[test]
    fn resolve_pet_source_maps_every_selector_form() {
        assert_eq!(
            resolve_pet_source("codex"),
            Some(PetSource::Builtin(*pet_by_id("codex").expect("codex pet"))),
            "a catalog id wins"
        );
        assert_eq!(
            resolve_pet_source(" https://ex.test/dragon.webp "),
            Some(PetSource::Remote("https://ex.test/dragon.webp".to_owned())),
            "an http(s) selector is a remote source, trimmed"
        );
        assert_eq!(
            resolve_pet_source("  /pets/dragon.webp "),
            Some(PetSource::Local(PathBuf::from("/pets/dragon.webp"))),
            "a path-like selector is a local source, trimmed"
        );
        assert_eq!(
            resolve_pet_source("my-pet.webp"),
            Some(PetSource::Local(PathBuf::from("my-pet.webp"))),
            "a dotted selector is a local source"
        );
        assert_eq!(
            resolve_pet_source(" wall-e "),
            Some(PetSource::Petdex("wall-e".to_owned())),
            "a bare slug is a petdex pet, trimmed"
        );
        assert_eq!(resolve_pet_source("   "), None, "empty selects nothing");
        assert!(is_path_like("~/x"), "a home prefix is path-like");
    }

    #[test]
    fn installed_petdex_pets_lists_manifest_dirs_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("b")).expect("mkdir b");
        std::fs::create_dir_all(dir.path().join("a")).expect("mkdir a");
        std::fs::create_dir_all(dir.path().join("no-manifest")).expect("mkdir no manifest");
        std::fs::write(dir.path().join("a/pet.json"), "{}").expect("write a manifest");
        std::fs::write(dir.path().join("b/pet.json"), "{}").expect("write b manifest");
        std::fs::write(dir.path().join("stray"), "not a pet").expect("write stray file");

        assert_eq!(installed_petdex_pets_in(dir.path()), ["a", "b"]);
        assert!(installed_petdex_pets_in(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn petdex_dir_reads_manifest_and_resolves_sheet_without_eviction() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            resolve_petdex_dir(dir.path()),
            Err(AssetErr::Io { .. })
        ));

        std::fs::write(
            dir.path().join("pet.json"),
            br#"{"id":"wall-e","spritesheetPath":"spritesheet.webp"}"#,
        )
        .expect("seed manifest");
        let sheet = dir.path().join("spritesheet.webp");
        std::fs::write(&sheet, b"not a webp").expect("seed sheet");
        // The manifest parses and the sheet path resolves; geometry fails on the
        // stub bytes, but the installed sheet is read-only to RimZ — never deleted.
        assert!(matches!(
            resolve_petdex_dir(dir.path()),
            Err(AssetErr::Decode(_))
        ));
        assert!(sheet.exists(), "a petdex sheet is never evicted");
    }

    #[test]
    fn require_https_rejects_plaintext_urls() {
        assert!(require_https("https://ex.test/p.webp").is_ok());
        assert!(matches!(
            require_https("http://ex.test/p.webp"),
            Err(AssetErr::Fetch(_))
        ));
    }

    #[test]
    fn remote_cache_file_is_stable_and_url_specific() {
        let a = remote_cache_file("https://ex.test/one.webp");
        let b = remote_cache_file("https://ex.test/two.webp");
        assert_eq!(a, remote_cache_file("https://ex.test/one.webp"), "stable");
        assert_ne!(a, b, "distinct URLs never collide");
        assert!(a.starts_with("remote-") && a.ends_with(".webp"));
    }

    #[test]
    fn local_source_reads_without_evicting_the_users_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sheet.webp");
        std::fs::write(&path, b"not a webp").expect("seed local sheet");
        // Geometry fails, but the user's file is read-only to RimZ — never deleted.
        assert!(matches!(resolve_local(&path), Err(AssetErr::Decode(_))));
        assert!(path.exists(), "a local sheet is never evicted");
    }

    #[test]
    fn decode_failure_evicts_only_fetched_cache_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cached = dir.path().join("cached.webp");
        let local = dir.path().join("local.webp");
        let petdex = dir.path().join("petdex.webp");
        for path in [&cached, &local, &petdex] {
            std::fs::write(path, b"sheet").expect("seed sheet");
        }
        let fail = |_: &[u8]| Err::<(), _>(frames::FrameErr::BufferSize);

        assert!(
            decode_resolved(
                ResolvedAsset {
                    bytes: b"sheet".to_vec(),
                    evictable_cache: Some(cached.clone()),
                },
                fail,
            )
            .is_err()
        );
        for path in [&local, &petdex] {
            assert!(
                decode_resolved(
                    ResolvedAsset {
                        bytes: std::fs::read(path).expect("read local sheet"),
                        evictable_cache: None,
                    },
                    fail,
                )
                .is_err()
            );
        }

        assert!(!cached.exists());
        assert!(local.exists());
        assert!(petdex.exists());
    }

    #[test]
    fn atomic_write_installs_complete_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("asset.webp");
        write_bytes_atomic(&path, b"sheet").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"sheet");
        let leftovers = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count();
        assert_eq!(leftovers, 0);
    }
}
