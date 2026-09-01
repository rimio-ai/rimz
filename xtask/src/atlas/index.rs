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
const RUST_PANIC_EXIT_CODE: i32 = 101;

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
    let output = run_scip(root, &staged, None);
    let mut output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = files::remove_stale_file(&staged);
            return Err(error).context("running rust-analyzer scip");
        }
    };
    let initial_panic = if rust_analyzer_panicked(&output) {
        let excerpt = stderr_excerpt(&output.stderr, 12);
        files::remove_stale_file(&staged)?;
        eprintln!(
            "atlas: rust-analyzer panicked; retrying the exact SCIP export with one cache-priming worker"
        );
        output = match run_scip(root, &staged, Some(1)) {
            Ok(output) => output,
            Err(error) => {
                let _ = files::remove_stale_file(&staged);
                return Err(error).context(format!(
                    "retrying rust-analyzer scip after an internal panic:\n{excerpt}"
                ));
            }
        };
        Some(excerpt)
    } else {
        None
    };
    if !output.status.success() {
        let _ = files::remove_stale_file(&staged);
        let stderr = stderr_excerpt(&output.stderr, 12);
        if let Some(initial_panic) = initial_panic {
            bail!(
                "rust-analyzer scip panicked, then its single-worker retry failed with {}:\n{stderr}\n\ninitial panic:\n{initial_panic}",
                output.status
            );
        }
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

fn run_scip(
    root: &Path,
    destination: &Path,
    num_threads: Option<usize>,
) -> std::io::Result<process::Output> {
    let mut command = Command::new("rust-analyzer");
    command
        .arg("scip")
        .arg(root)
        .arg("--output")
        .arg(destination);
    if let Some(num_threads) = num_threads {
        command.arg("--num-threads").arg(num_threads.to_string());
    }
    command.current_dir(root).stdout(Stdio::null()).output()
}

fn rust_analyzer_panicked(output: &process::Output) -> bool {
    is_rust_panic_exit_code(output.status.code())
}

fn is_rust_panic_exit_code(code: Option<i32>) -> bool {
    code == Some(RUST_PANIC_EXIT_CODE)
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

fn stderr_excerpt(stderr: &[u8], lines: usize) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let mut tail = stderr.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    let tail = tail.join("\n");

    let mut panic = stderr
        .lines()
        .skip_while(|line| !line.contains(" panicked at "))
        .take_while(|line| !line.starts_with("stack backtrace:"))
        .take(8)
        .collect::<Vec<_>>();
    while panic.last().is_some_and(|line| line.trim().is_empty()) {
        panic.pop();
    }
    let panic = panic.join("\n");
    if panic.is_empty() || tail.contains(&panic) {
        tail
    } else if tail.is_empty() {
        panic
    } else {
        format!("{panic}\n...\n{tail}")
    }
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

#[cfg(test)]
mod tests {
    use super::{is_rust_panic_exit_code, stderr_excerpt};

    #[test]
    fn only_a_rust_panic_exit_requests_the_exact_retry() {
        assert!(is_rust_panic_exit_code(Some(101)));
        assert!(!is_rust_panic_exit_code(Some(1)));
        assert!(!is_rust_panic_exit_code(None));
    }

    #[test]
    fn stderr_excerpt_keeps_the_panic_cause_and_backtrace_tail() {
        let stderr = b"loading\nthread 'main' panicked at crates/hir-ty/src/infer.rs:42:5:\nassertion failed: exact references\nstack backtrace:\n   0: first\n   1: second\n   2: third\n";

        assert_eq!(
            stderr_excerpt(stderr, 2),
            "thread 'main' panicked at crates/hir-ty/src/infer.rs:42:5:\nassertion failed: exact references\n...\n   1: second\n   2: third"
        );
    }

    #[test]
    fn stderr_excerpt_does_not_repeat_a_short_panic() {
        let stderr = b"thread 'main' panicked at infer.rs:1:1:\nbroken\n";

        assert_eq!(
            stderr_excerpt(stderr, 12),
            "thread 'main' panicked at infer.rs:1:1:\nbroken"
        );
    }
}
