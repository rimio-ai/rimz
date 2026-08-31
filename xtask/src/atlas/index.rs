use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::files;

use super::sources::Source;

const CACHE_DIRECTORY: &str = "atlas";
const CACHE_PREFIX: &str = "index-";
const CACHE_SUFFIX: &str = ".scip";
const KEY_LENGTH: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum IndexPolicy {
    #[default]
    Required,
    Skip,
}

/// Returns the SCIP index for the exact working-tree snapshot in `sources`.
///
/// The cache key includes the lockfile because rust-analyzer indexes resolved
/// dependency versions as part of the workspace model.
#[expect(
    clippy::print_stderr,
    reason = "atlas reports the start of an expensive SCIP regeneration"
)]
pub(super) fn ensure(root: &Path, sources: &[Source]) -> Result<PathBuf> {
    let cache_dir = files::target_dir(root).join(CACHE_DIRECTORY);
    let key = cache_key(root, sources)?;
    let destination = cache_dir.join(format!("{CACHE_PREFIX}{key}{CACHE_SUFFIX}"));
    if destination.is_file() {
        return Ok(destination);
    }

    check_rust_analyzer(root)?;
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("creating atlas index cache {}", cache_dir.display()))?;

    let staged = cache_dir.join(format!(
        ".{CACHE_PREFIX}{key}{CACHE_SUFFIX}.tmp.{}",
        process::id()
    ));
    files::remove_stale_file(&staged)?;

    eprintln!("atlas: generating rust-analyzer SCIP index (this can take over a minute)");
    let output = Command::new("rust-analyzer")
        .arg("scip")
        .arg(root)
        .arg("--output")
        .arg(&staged)
        .current_dir(root)
        .stdout(Stdio::null())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = files::remove_stale_file(&staged);
            return Err(error).context("running rust-analyzer scip");
        }
    };
    if !output.status.success() {
        let _ = files::remove_stale_file(&staged);
        let stderr = stderr_tail(&output.stderr, 12);
        if stderr.is_empty() {
            bail!("rust-analyzer scip failed with {}", output.status);
        }
        bail!(
            "rust-analyzer scip failed with {}:\n{stderr}",
            output.status
        );
    }
    if !staged.is_file() {
        bail!(
            "rust-analyzer scip succeeded without writing {}",
            staged.display()
        );
    }

    // Another atlas process may have populated the same content-addressed
    // destination while rust-analyzer was running. Its output is equivalent.
    if destination.is_file() {
        files::remove_stale_file(&staged)?;
    } else {
        fs::rename(&staged, &destination)
            .with_context(|| format!("installing atlas SCIP index {}", destination.display()))?;
    }
    remove_old_indexes(&cache_dir, &destination)?;
    Ok(destination)
}

fn cache_key(root: &Path, sources: &[Source]) -> Result<String> {
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));

    let mut hasher = Sha256::new();
    for source in ordered {
        hash_field(&mut hasher, source.path.as_os_str().as_encoded_bytes());
        hash_field(&mut hasher, source.text.as_bytes());
    }
    let lockfile = root.join("Cargo.lock");
    let lockfile_bytes = fs::read(&lockfile)
        .with_context(|| format!("reading {} for atlas index key", lockfile.display()))?;
    hash_field(&mut hasher, &lockfile_bytes);

    let digest = hex::encode(hasher.finalize());
    Ok(digest[..KEY_LENGTH].to_owned())
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn check_rust_analyzer(root: &Path) -> Result<()> {
    let output = match Command::new("rust-analyzer")
        .arg("--version")
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(rust_analyzer_install_message())
        }
        Err(error) => return Err(error).context("checking rust-analyzer --version"),
    };
    if !output.status.success() {
        bail!(
            "rust-analyzer --version failed with {}\n\n{}",
            output.status,
            rust_analyzer_install_message()
        );
    }
    Ok(())
}

fn rust_analyzer_install_message() -> &'static str {
    "rust-analyzer is required to build the Atlas reference index\n\nInstall it with:\n  rustup component add rust-analyzer\n\nor install rust-analyzer on PATH"
}

fn stderr_tail(stderr: &[u8], lines: usize) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let mut tail = stderr.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    tail.join("\n")
}

fn remove_old_indexes(cache_dir: &Path, current: &Path) -> Result<()> {
    for entry in fs::read_dir(cache_dir)
        .with_context(|| format!("reading atlas index cache {}", cache_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", cache_dir.display()))?;
        let path = entry.path();
        if path == current || !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(CACHE_PREFIX) && name.ends_with(CACHE_SUFFIX) {
            fs::remove_file(&path)
                .with_context(|| format!("removing stale Atlas index {}", path.display()))?;
        }
    }
    Ok(())
}
