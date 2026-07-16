//! Self-update mechanics.
//!
//! The CLI owns process I/O and presentation. This module owns install-origin
//! detection, release download and verification, archive extraction, smoke
//! testing, and atomic binary replacement so each filesystem step stays
//! tempdir-testable.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use url::Url;

pub const LINUX_X86_64_ARCHIVE: &str = "rimz-x86_64-unknown-linux-gnu.tar.gz";
pub const DARWIN_AARCH64_ARCHIVE: &str = "rimz-aarch64-apple-darwin.tar.gz";
pub const DARWIN_X86_64_ARCHIVE: &str = "rimz-x86_64-apple-darwin.tar.gz";

const RELEASES_URL: &str = "https://github.com/rimio-ai/rimz/releases/";
const LATEST_URL: &str = "https://github.com/rimio-ai/rimz/releases/latest";
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const ARCHIVE_MAX_BYTES: u64 = 128 * 1024 * 1024;
const CHECKSUMS_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOrigin {
    Homebrew,
    Cargo,
    Standalone,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(
        "no prebuilt RimZ release exists for this target; install with: cargo install --locked rimz"
    )]
    UnsupportedTarget,
    #[error("release tag `{tag}` is invalid; use a tag such as `v0.3.1` or `latest-main`")]
    InvalidTag { tag: String },
    #[error("Cargo cannot install release tag `{tag}`; use a version tag such as `v0.3.1`")]
    InvalidCargoVersion { tag: String },
    #[error(
        "cannot check the latest RimZ release at {url}; check the network connection and retry: {source}"
    )]
    LatestRequest {
        url: &'static str,
        #[source]
        source: ureq::Error,
    },
    #[error("latest-release check returned HTTP {status}; retry, or pass `--version <TAG>`")]
    LatestStatus { status: u16 },
    #[error("latest-release response has no Location header; retry, or pass `--version <TAG>`")]
    LatestLocationMissing,
    #[error(
        "latest-release Location `{location}` is not a RimZ release tag URL; retry, or pass `--version <TAG>`"
    )]
    InvalidLatestLocation { location: String },
    #[error(
        "this RimZ build contains an invalid release URL; report the build and use the install script"
    )]
    ReleaseUrl,
    #[error(
        "cannot create update staging directory {path}; set TMPDIR to a writable directory and retry: {source}"
    )]
    Staging {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot download {url}; check the network connection and retry: {source}")]
    Download {
        url: String,
        #[source]
        source: ureq::Error,
    },
    #[error("download of {url} returned HTTP {status}; check the release tag and retry")]
    DownloadStatus { url: String, status: u16 },
    #[error(
        "cannot save download to {path}; set TMPDIR to a writable directory and retry: {source}"
    )]
    SaveDownload {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot read checksums from {path}; retry the update: {source}")]
    ReadChecksums {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SHA256SUMS has no entry for `{archive}`; the release is incomplete, retry later")]
    ChecksumMissing { archive: String },
    #[error("SHA256SUMS has an invalid digest for `{archive}`; the release is malformed")]
    InvalidChecksum { archive: String },
    #[error("checksum verification failed for `{archive}`; delete any proxy cache and retry")]
    ChecksumMismatch { archive: String },
    #[error("cannot read release archive {path}; retry the update: {source}")]
    ReadArchive {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("release archive `{archive}` does not contain `{entry}`; the release is malformed")]
    ArchiveEntryMissing { archive: String, entry: String },
    #[error(
        "cannot start staged RimZ binary {path}; install the target's runtime prerequisites and retry: {source}"
    )]
    SmokeStart {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("staged RimZ binary failed its `--version` smoke test ({status}): {detail}")]
    SmokeFailed { status: String, detail: String },
    #[error("staged RimZ binary returned unexpected version output `{output}`")]
    SmokeOutput { output: String },
    #[error("RimZ install directory {dir} is not writable; rerun with: sudo rimz update")]
    DestinationNotWritable { dir: PathBuf },
    #[error("cannot install RimZ at {path}; fix the destination and retry: {source}")]
    Install {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, UpdateError>;

pub fn detect_origin(canonical_exe: &Path, cargo_bin: Option<&Path>) -> InstallOrigin {
    if canonical_exe
        .components()
        .any(|component| component.as_os_str() == "Cellar")
    {
        return InstallOrigin::Homebrew;
    }
    if cargo_bin.is_some_and(|cargo_bin| canonical_exe.parent() == Some(cargo_bin)) {
        return InstallOrigin::Cargo;
    }
    InstallOrigin::Standalone
}

pub fn cargo_bin_dir() -> Option<PathBuf> {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"))
}

pub fn release_archive() -> Option<&'static str> {
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some(LINUX_X86_64_ARCHIVE)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(DARWIN_AARCH64_ARCHIVE)
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some(DARWIN_X86_64_ARCHIVE)
    } else {
        None
    }
}

pub fn resolve_latest_tag() -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .new_agent();
    let response = agent
        .get(LATEST_URL)
        .header("User-Agent", "rimz-update")
        .call()
        .map_err(|source| UpdateError::LatestRequest {
            url: LATEST_URL,
            source,
        })?;
    if !response.status().is_redirection() {
        return Err(UpdateError::LatestStatus {
            status: response.status().as_u16(),
        });
    }
    let location = response
        .headers()
        .get("Location")
        .and_then(|value| value.to_str().ok())
        .ok_or(UpdateError::LatestLocationMissing)?;
    parse_latest_tag(location)
}

pub fn is_current(installed: &str, tag: &str) -> bool {
    if installed.contains("+g") {
        return false;
    }
    normalize_numeric_tag(tag).is_some_and(|tag| installed == tag)
}

pub fn cargo_version_for_tag(tag: &str) -> Result<String> {
    normalize_numeric_tag(tag).ok_or_else(|| UpdateError::InvalidCargoVersion {
        tag: tag.to_owned(),
    })
}

pub struct DownloadedRelease {
    staging: StagingDir,
    archive_name: &'static str,
    archive_path: PathBuf,
    checksums_path: PathBuf,
}

impl DownloadedRelease {
    pub fn verify(&self) -> Result<()> {
        verify_download(&self.archive_path, &self.checksums_path, self.archive_name)
    }

    pub fn extract(&self) -> Result<PathBuf> {
        let output = self.staging.path().join("rimz");
        extract_archive(&self.archive_path, self.archive_name, &output)?;
        Ok(output)
    }
}

pub fn download_release(tag: &str, archive: &'static str) -> Result<DownloadedRelease> {
    validate_tag(tag)?;
    let staging = StagingDir::new()?;
    let archive_path = staging.path().join(archive);
    let checksums_path = staging.path().join("SHA256SUMS");
    let archive_url = release_asset_url(tag, archive)?;
    let checksums_url = release_asset_url(tag, "SHA256SUMS")?;
    download_to(&archive_url, &archive_path, ARCHIVE_MAX_BYTES)?;
    download_to(&checksums_url, &checksums_path, CHECKSUMS_MAX_BYTES)?;
    Ok(DownloadedRelease {
        staging,
        archive_name: archive,
        archive_path,
        checksums_path,
    })
}

pub fn smoke_test(path: &Path) -> Result<String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|source| UpdateError::SmokeStart {
            path: path.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(UpdateError::SmokeFailed {
            status: output.status.to_string(),
            detail: if detail.is_empty() {
                "no error output".to_owned()
            } else {
                detail
            },
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    stdout
        .strip_prefix("rimz ")
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .ok_or(UpdateError::SmokeOutput { output: stdout })
}

pub fn install_over(staged: &Path, dest: &Path) -> Result<()> {
    let dir = dest.parent().ok_or_else(|| UpdateError::Install {
        path: dest.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"),
    })?;
    preflight_destination(dir)?;

    let temp = dir.join(format!(
        ".rimz.update.{}.{}",
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));
    let mut guard = TempFileGuard::new(temp.clone());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut output = options
        .open(&temp)
        .map_err(|source| install_error(dir, &temp, source))?;
    let mut input = File::open(staged).map_err(|source| UpdateError::Install {
        path: staged.to_path_buf(),
        source,
    })?;
    io::copy(&mut input, &mut output).map_err(|source| UpdateError::Install {
        path: temp.clone(),
        source,
    })?;
    output.flush().map_err(|source| UpdateError::Install {
        path: temp.clone(),
        source,
    })?;
    output.sync_all().map_err(|source| UpdateError::Install {
        path: temp.clone(),
        source,
    })?;
    drop(output);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o755)).map_err(|source| {
            UpdateError::Install {
                path: temp.clone(),
                source,
            }
        })?;
    }
    fs::rename(&temp, dest).map_err(|source| install_error(dir, dest, source))?;
    guard.disarm();
    File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| UpdateError::Install {
            path: dir.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn normalize_numeric_tag(tag: &str) -> Option<String> {
    let raw = tag.strip_prefix('v').unwrap_or(tag);
    let components = raw.split('.').collect::<Vec<_>>();
    if components.is_empty() || components.len() > 3 {
        return None;
    }
    let mut numbers = Vec::with_capacity(3);
    for component in components {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        numbers.push(component.parse::<u64>().ok()?);
    }
    numbers.resize(3, 0);
    Some(format!("{}.{}.{}", numbers[0], numbers[1], numbers[2]))
}

fn parse_latest_tag(location: &str) -> Result<String> {
    let url = Url::parse(location).map_err(|_| UpdateError::InvalidLatestLocation {
        location: location.to_owned(),
    })?;
    let segments = url
        .path_segments()
        .ok_or_else(|| UpdateError::InvalidLatestLocation {
            location: location.to_owned(),
        })?
        .collect::<Vec<_>>();
    let [owner, repository, releases, marker, tag] = segments.as_slice() else {
        return Err(UpdateError::InvalidLatestLocation {
            location: location.to_owned(),
        });
    };
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || *owner != "rimio-ai"
        || *repository != "rimz"
        || *releases != "releases"
        || *marker != "tag"
    {
        return Err(UpdateError::InvalidLatestLocation {
            location: location.to_owned(),
        });
    }
    validate_tag(tag)?;
    Ok((*tag).to_owned())
}

fn validate_tag(tag: &str) -> Result<()> {
    let valid = tag
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(UpdateError::InvalidTag {
            tag: tag.to_owned(),
        })
    }
}

fn release_asset_url(tag: &str, asset: &str) -> Result<Url> {
    validate_tag(tag)?;
    let mut url = Url::parse(RELEASES_URL).map_err(|_| UpdateError::ReleaseUrl)?;
    url.path_segments_mut()
        .map_err(|_| UpdateError::ReleaseUrl)?
        .extend(["download", tag, asset]);
    Ok(url)
}

fn download_to(url: &Url, path: &Path, max_bytes: u64) -> Result<()> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .new_agent();
    let mut response = agent
        .get(url.as_str())
        .header("User-Agent", "rimz-update")
        .call()
        .map_err(|source| UpdateError::Download {
            url: url.to_string(),
            source,
        })?;
    if !response.status().is_success() {
        return Err(UpdateError::DownloadStatus {
            url: url.to_string(),
            status: response.status().as_u16(),
        });
    }
    let mut file = File::create(path).map_err(|source| UpdateError::SaveDownload {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = response.body_mut().with_config().limit(max_bytes).reader();
    io::copy(&mut reader, &mut file).map_err(|source| UpdateError::SaveDownload {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all()
        .map_err(|source| UpdateError::SaveDownload {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn verify_download(archive_path: &Path, checksums_path: &Path, archive: &str) -> Result<()> {
    let checksums =
        fs::read_to_string(checksums_path).map_err(|source| UpdateError::ReadChecksums {
            path: checksums_path.to_path_buf(),
            source,
        })?;
    let expected = expected_checksum(&checksums, archive)?;
    let mut file = File::open(archive_path).map_err(|source| UpdateError::ReadArchive {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|source| UpdateError::ReadArchive {
                path: archive_path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    if hasher.finalize().as_slice() != expected {
        return Err(UpdateError::ChecksumMismatch {
            archive: archive.to_owned(),
        });
    }
    Ok(())
}

fn expected_checksum(checksums: &str, archive: &str) -> Result<[u8; 32]> {
    let Some(digest) = checksums.lines().find_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == archive).then_some(digest)
    }) else {
        return Err(UpdateError::ChecksumMissing {
            archive: archive.to_owned(),
        });
    };
    let mut expected = [0_u8; 32];
    hex::decode_to_slice(digest, &mut expected).map_err(|_| UpdateError::InvalidChecksum {
        archive: archive.to_owned(),
    })?;
    Ok(expected)
}

fn extract_archive(archive_path: &Path, archive_name: &str, output: &Path) -> Result<()> {
    let file = File::open(archive_path).map_err(|source| UpdateError::ReadArchive {
        path: archive_path.to_path_buf(),
        source,
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let expected = format!("{}/rimz", archive_name.trim_end_matches(".tar.gz"));
    let entries = archive
        .entries()
        .map_err(|source| UpdateError::ReadArchive {
            path: archive_path.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let mut entry = entry.map_err(|source| UpdateError::ReadArchive {
            path: archive_path.to_path_buf(),
            source,
        })?;
        let path = entry.path().map_err(|source| UpdateError::ReadArchive {
            path: archive_path.to_path_buf(),
            source,
        })?;
        if path == Path::new(&expected) && entry.header().entry_type().is_file() {
            entry
                .unpack(output)
                .map_err(|source| UpdateError::ReadArchive {
                    path: archive_path.to_path_buf(),
                    source,
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(output, fs::Permissions::from_mode(0o755)).map_err(
                    |source| UpdateError::ReadArchive {
                        path: archive_path.to_path_buf(),
                        source,
                    },
                )?;
            }
            return Ok(());
        }
    }
    Err(UpdateError::ArchiveEntryMissing {
        archive: archive_name.to_owned(),
        entry: expected,
    })
}

fn preflight_destination(dir: &Path) -> Result<()> {
    let metadata = fs::metadata(dir).map_err(|source| install_error(dir, dir, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o222 == 0 {
            return Err(UpdateError::DestinationNotWritable {
                dir: dir.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn install_error(dir: &Path, path: &Path, source: io::Error) -> UpdateError {
    if source.kind() == io::ErrorKind::PermissionDenied {
        UpdateError::DestinationNotWritable {
            dir: dir.to_path_buf(),
        }
    } else {
        UpdateError::Install {
            path: path.to_path_buf(),
            source,
        }
    }
}

struct StagingDir(PathBuf);

impl StagingDir {
    fn new() -> Result<Self> {
        let path = env::temp_dir().join(format!(
            "rimz-update-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        fs::create_dir(&path).map_err(|source| UpdateError::Staging {
            path: path.clone(),
            source,
        })?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn detects_install_origin_from_exact_path_shapes() {
        assert_eq!(
            detect_origin(
                Path::new("/opt/homebrew/Cellar/rimz/0.3.0/bin/rimz"),
                Some(Path::new("/home/me/.cargo/bin"))
            ),
            InstallOrigin::Homebrew
        );
        assert_eq!(
            detect_origin(
                Path::new("/home/me/.cargo/bin/rimz"),
                Some(Path::new("/home/me/.cargo/bin"))
            ),
            InstallOrigin::Cargo
        );
        assert_eq!(
            detect_origin(
                Path::new("/usr/local/bin/rimz"),
                Some(Path::new("/home/me/.cargo/bin"))
            ),
            InstallOrigin::Standalone
        );
    }

    #[test]
    fn current_version_matches_only_equivalent_release_tags() {
        assert!(is_current("0.3.1", "v0.3.1"));
        assert!(is_current("0.3.0", "v0.3"));
        assert!(!is_current("0.3.0", "v0.2"));
        assert!(!is_current("0.3.0", "v0.4"));
        assert!(!is_current("0.3.0+g123456789abc", "v0.3"));
        assert!(!is_current("0.3.0+g123456789abc.dirty", "v0.3"));
        assert!(!is_current("0.3.0", "latest-main"));
    }

    #[test]
    fn cargo_version_pads_short_release_tags() {
        assert_eq!(cargo_version_for_tag("v0.3").unwrap(), "0.3.0");
        assert!(cargo_version_for_tag("latest-main").is_err());
    }

    #[test]
    fn checksum_parser_finds_named_archive() {
        let digest = "ab".repeat(32);
        assert_eq!(
            expected_checksum(&format!("{digest}  rimz-test.tar.gz\n"), "rimz-test.tar.gz")
                .unwrap(),
            [0xab; 32]
        );
    }

    #[test]
    fn checksum_parser_rejects_missing_archive() {
        let err = expected_checksum(
            &format!("{}  other.tar.gz\n", "ab".repeat(32)),
            "rimz.tar.gz",
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::ChecksumMissing { .. }));
    }

    #[test]
    fn verification_rejects_digest_mismatch() {
        let dir = tempdir().unwrap();
        let archive = dir.path().join("rimz.tar.gz");
        let sums = dir.path().join("SHA256SUMS");
        fs::write(&archive, b"archive bytes").unwrap();
        fs::write(&sums, format!("{}  rimz.tar.gz\n", "00".repeat(32))).unwrap();

        let err = verify_download(&archive, &sums, "rimz.tar.gz").unwrap_err();

        assert!(matches!(err, UpdateError::ChecksumMismatch { .. }));
    }

    #[test]
    fn extraction_writes_only_the_expected_binary() {
        let dir = tempdir().unwrap();
        let archive_name = "rimz-test-target.tar.gz";
        let archive = dir.path().join(archive_name);
        write_archive(&archive, "rimz-test-target/rimz", b"binary");
        let output = dir.path().join("staged-rimz");

        extract_archive(&archive, archive_name, &output).unwrap();

        assert_eq!(fs::read(output).unwrap(), b"binary");
    }

    #[test]
    fn extraction_rejects_an_archive_without_the_expected_binary() {
        let dir = tempdir().unwrap();
        let archive_name = "rimz-test-target.tar.gz";
        let archive = dir.path().join(archive_name);
        write_archive(&archive, "other/rimz", b"binary");

        let err = extract_archive(&archive, archive_name, &dir.path().join("rimz")).unwrap_err();

        assert!(matches!(err, UpdateError::ArchiveEntryMissing { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_a_symlink_at_the_expected_path() {
        let dir = tempdir().unwrap();
        let archive_name = "rimz-test-target.tar.gz";
        let archive = dir.path().join(archive_name);
        let file = File::create(&archive).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_link_name("/tmp/not-rimz").unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, "rimz-test-target/rimz", io::empty())
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        let err = extract_archive(&archive, archive_name, &dir.path().join("rimz")).unwrap_err();

        assert!(matches!(err, UpdateError::ArchiveEntryMissing { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn install_over_atomically_replaces_with_executable_mode() {
        let dir = tempdir().unwrap();
        let staged = dir.path().join("staged");
        let dest = dir.path().join("rimz");
        fs::write(&staged, b"new binary").unwrap();
        fs::write(&dest, b"old binary").unwrap();

        install_over(&staged, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"new binary");
        assert_eq!(
            fs::metadata(dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_over_reports_the_sudo_fix_for_unwritable_directory() {
        let source_dir = tempdir().unwrap();
        let staged = source_dir.path().join("staged");
        fs::write(&staged, b"new binary").unwrap();
        let dest_dir = tempdir().unwrap();
        let dest = dest_dir.path().join("rimz");
        fs::write(&dest, b"old binary").unwrap();
        fs::set_permissions(dest_dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let err = install_over(&staged, &dest).unwrap_err();

        fs::set_permissions(dest_dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(err, UpdateError::DestinationNotWritable { .. }));
        assert!(err.to_string().contains("sudo rimz update"));
    }

    #[test]
    fn latest_location_parser_extracts_the_tag() {
        assert_eq!(
            parse_latest_tag("https://github.com/rimio-ai/rimz/releases/tag/v0.3").unwrap(),
            "v0.3"
        );
        assert!(parse_latest_tag("https://example.com/releases/latest").is_err());
        assert!(parse_latest_tag("https://example.com/rimio-ai/rimz/releases/tag/v0.3").is_err());
    }

    fn write_archive(path: &Path, entry_path: &str, bytes: &[u8]) {
        let file = File::create(path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, entry_path, bytes).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }
}
