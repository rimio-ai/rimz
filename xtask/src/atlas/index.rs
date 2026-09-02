use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::files;

use super::sources::{self, Source};

const CACHE_DIRECTORY: &str = "atlas";
const CACHE_PREFIX: &str = "index-";
const CACHE_SUFFIX: &str = ".scip";
const KEY_LENGTH: usize = 16;
const RUST_PANIC_EXIT_CODE: i32 = 101;

/// Returns the SCIP index for the exact working-tree snapshot in `sources`.
///
/// The cache key includes the lockfile because rust-analyzer indexes resolved
/// dependency versions as part of the workspace model.
pub(super) fn ensure(root: &Path, sources: &[Source]) -> Result<PathBuf> {
    let cache_dir = files::target_dir(root).join(CACHE_DIRECTORY);
    let lockfile = root.join("Cargo.lock");
    let lockfile_bytes = fs::read(&lockfile)
        .with_context(|| format!("reading {} for atlas index key", lockfile.display()))?;
    let key = cache_key(sources, &lockfile_bytes);
    ensure_keyed(&cache_dir, root, &key, None)
}

pub(super) fn ensure_revision(root: &Path, revision: &str, sources: &[Source]) -> Result<PathBuf> {
    let cache_dir = files::target_dir(root).join(CACHE_DIRECTORY);
    let lockfile = sources::revision_blob(root, revision, Path::new("Cargo.lock"))?;
    let key = cache_key(sources, &lockfile);
    let destination = cache_path(&cache_dir, &key);
    if destination.is_file() {
        touch(&destination)?;
        remove_old_indexes(&cache_dir)?;
        return Ok(destination);
    }

    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("creating atlas index cache {}", cache_dir.display()))?;
    let checkout = cache_dir.join(format!("checkout-{key}-{}", process::id()));
    if checkout.exists() {
        fs::remove_dir_all(&checkout)
            .with_context(|| format!("removing stale Atlas checkout {}", checkout.display()))?;
    }
    fs::create_dir(&checkout)
        .with_context(|| format!("creating Atlas checkout {}", checkout.display()))?;
    let result = materialize_revision(root, revision, &checkout)
        .and_then(|()| ensure_keyed(&cache_dir, &checkout, &key, Some(files::target_dir(root))));
    let cleanup = fs::remove_dir_all(&checkout)
        .with_context(|| format!("removing Atlas checkout {}", checkout.display()));
    match (result, cleanup) {
        (Ok(path), Ok(())) => Ok(path),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(error.context(format!(
            "Atlas indexing also failed to clean up its checkout: {cleanup:#}"
        ))),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn cache_path(cache_dir: &Path, key: &str) -> PathBuf {
    cache_dir.join(format!("{CACHE_PREFIX}{key}{CACHE_SUFFIX}"))
}

fn touch(path: &Path) -> Result<()> {
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()))
        .with_context(|| format!("touching Atlas index {}", path.display()))
}

fn materialize_revision(root: &Path, revision: &str, checkout: &Path) -> Result<()> {
    let mut archive = Command::new("git")
        .args(["archive", revision])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("archiving Atlas base `{revision}`"))?;
    let archive_stdout = archive
        .stdout
        .take()
        .context("opening git archive stdout")?;
    let mut archive_stderr = archive
        .stderr
        .take()
        .context("opening git archive stderr")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = Vec::new();
        archive_stderr.read_to_end(&mut stderr).map(|_| stderr)
    });
    let output = Command::new("tar")
        .args(["-x", "-C"])
        .arg(checkout)
        .stdin(archive_stdout)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("extracting Atlas base archive")?;
    let archive_status = archive.wait().context("waiting for git archive")?;
    let archive_stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("git archive stderr reader panicked"))?
        .context("reading git archive stderr")?;
    if !archive_status.success() {
        bail!(
            "git archive `{revision}` failed: {}",
            String::from_utf8_lossy(&archive_stderr).trim()
        );
    }
    if !output.status.success() {
        bail!(
            "extracting git archive `{revision}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[expect(
    clippy::print_stderr,
    reason = "atlas reports the start of an expensive SCIP regeneration"
)]
fn ensure_keyed(
    cache_dir: &Path,
    index_root: &Path,
    key: &str,
    cargo_target_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    let destination = cache_path(cache_dir, key);
    if destination.is_file() {
        touch(&destination)?;
        remove_old_indexes(cache_dir)?;
        return Ok(destination);
    }

    check_rust_analyzer(index_root)?;
    fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating atlas index cache {}", cache_dir.display()))?;

    let staged = cache_dir.join(format!(
        ".{CACHE_PREFIX}{key}{CACHE_SUFFIX}.tmp.{}",
        process::id()
    ));
    files::remove_stale_file(&staged)?;

    eprintln!("atlas: generating rust-analyzer SCIP index (this can take over a minute)");
    let output = run_scip(index_root, &staged, None, cargo_target_dir.as_deref());
    let mut output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = files::remove_stale_file(&staged);
            return Err(error).context("running rust-analyzer scip");
        }
    };
    let initial_panic = if rust_analyzer_panicked(&output) {
        let excerpt = stderr_excerpt(&output.stderr, 12);
        let guidance = scip_panic_guidance(&output.stderr);
        files::remove_stale_file(&staged)?;
        eprintln!(
            "atlas: rust-analyzer panicked; retrying the exact SCIP export with one cache-priming worker"
        );
        output = match run_scip(index_root, &staged, Some(1), cargo_target_dir.as_deref()) {
            Ok(output) => output,
            Err(error) => {
                let _ = files::remove_stale_file(&staged);
                return Err(error).context(format!(
                    "retrying rust-analyzer scip after an internal panic:\n{excerpt}\n\n{guidance}"
                ));
            }
        };
        Some((excerpt, guidance))
    } else {
        None
    };
    if !output.status.success() {
        let _ = files::remove_stale_file(&staged);
        let stderr = stderr_excerpt(&output.stderr, 12);
        if let Some((initial_panic, guidance)) = initial_panic {
            bail!(
                "rust-analyzer scip panicked, then its single-worker retry failed with {}:\n{stderr}\n\ninitial panic:\n{initial_panic}\n\n{guidance}",
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
    touch(&destination)?;
    remove_old_indexes(cache_dir)?;
    Ok(destination)
}

fn run_scip(
    root: &Path,
    destination: &Path,
    num_threads: Option<usize>,
    cargo_target_dir: Option<&Path>,
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
    if let Some(cargo_target_dir) = cargo_target_dir {
        command.env("CARGO_TARGET_DIR", cargo_target_dir);
    }
    command.current_dir(root).stdout(Stdio::null()).output()
}

fn rust_analyzer_panicked(output: &process::Output) -> bool {
    is_rust_panic_exit_code(output.status.code())
}

fn is_rust_panic_exit_code(code: Option<i32>) -> bool {
    code == Some(RUST_PANIC_EXIT_CODE)
}

fn scip_panic_guidance(stderr: &[u8]) -> &'static str {
    if String::from_utf8_lossy(stderr).contains("ide::inlay_hints::hints") {
        "failing rust-analyzer pass: ide::inlay_hints::hints during SCIP static-index export\n\
         This is an upstream rust-analyzer closure-inference bug. Work around it by giving the \
         offending closure parameter an explicit type or replacing the closure with a named function, \
         then rerun Atlas."
    } else {
        "failing rust-analyzer pass: SCIP static-index export\n\
         This is an internal rust-analyzer failure. Try rewriting the expression named by the panic \
         with more explicit types, then rerun Atlas."
    }
}

fn cache_key(sources: &[Source], lockfile_bytes: &[u8]) -> String {
    let mut ordered = sources.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));

    let mut hasher = Sha256::new();
    for source in ordered {
        hash_field(&mut hasher, source.path.as_os_str().as_encoded_bytes());
        hash_field(&mut hasher, source.text.as_bytes());
    }
    hash_field(&mut hasher, lockfile_bytes);

    let digest = hex::encode(hasher.finalize());
    digest[..KEY_LENGTH].to_owned()
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

fn remove_old_indexes(cache_dir: &Path) -> Result<()> {
    let mut indexes = Vec::new();
    for entry in fs::read_dir(cache_dir)
        .with_context(|| format!("reading atlas index cache {}", cache_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", cache_dir.display()))?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let is_index = {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(CACHE_PREFIX) && name.ends_with(CACHE_SUFFIX)
        };
        if !is_index {
            continue;
        }
        indexes.push((entry.metadata()?.modified()?, entry.path()));
    }
    indexes.sort_by(|left, right| right.cmp(left));
    for (_, path) in indexes.into_iter().skip(2) {
        fs::remove_file(&path)
            .with_context(|| format!("removing stale Atlas index {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::{
        cache_key, is_rust_panic_exit_code, remove_old_indexes, scip_panic_guidance,
        stderr_excerpt, touch,
    };
    use crate::atlas::sources::Source;

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

    #[test]
    fn scip_inlay_hint_panic_names_the_pass_and_closure_workaround() {
        let stderr = b"10: ide::inlay_hints::hints\n11: ide::static_index::StaticIndex::compute\n";
        let guidance = scip_panic_guidance(stderr);

        assert!(guidance.contains("ide::inlay_hints::hints"));
        assert!(guidance.contains("closure parameter an explicit type"));
        assert!(guidance.contains("upstream rust-analyzer"));
    }

    #[test]
    fn revision_cache_key_uses_the_base_lockfile_not_the_working_tree() {
        let sources = [Source::new("src/lib.rs", "pub fn stable() {}\n")];

        assert_ne!(
            cache_key(&sources, b"base lockfile"),
            cache_key(&sources, b"working lockfile")
        );
        assert_eq!(
            cache_key(&sources, b"base lockfile"),
            cache_key(&sources, b"base lockfile")
        );
    }

    #[test]
    fn eviction_keeps_the_two_newest_indexes() {
        let directory = tempfile::tempdir().unwrap();
        let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        for (name, age) in [
            ("index-old.scip", 0),
            ("index-middle.scip", 1),
            ("index-new.scip", 2),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, name).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(epoch + Duration::from_secs(age))
                .unwrap();
        }

        remove_old_indexes(directory.path()).unwrap();

        assert!(!directory.path().join("index-old.scip").exists());
        assert!(directory.path().join("index-middle.scip").exists());
        assert!(directory.path().join("index-new.scip").exists());
    }

    #[test]
    fn touching_the_base_before_refreshing_current_retains_both_indexes() {
        let directory = tempfile::tempdir().unwrap();
        let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let base = directory.path().join("index-base.scip");
        let current = directory.path().join("index-current.scip");
        let unrelated = directory.path().join("index-unrelated.scip");
        for (path, age) in [(&base, 0), (&current, 1), (&unrelated, 2)] {
            fs::write(path, b"index").unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(epoch + Duration::from_secs(age))
                .unwrap();
        }

        touch(&base).unwrap();
        remove_old_indexes(directory.path()).unwrap();
        assert!(base.exists());
        assert!(!current.exists());

        fs::write(&current, b"refreshed").unwrap();
        fs::File::options()
            .write(true)
            .open(&current)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        remove_old_indexes(directory.path()).unwrap();

        assert!(base.exists());
        assert!(current.exists());
        assert!(!unrelated.exists());
    }
}
