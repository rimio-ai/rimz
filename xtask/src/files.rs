use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub(crate) fn target_dir(root: &Path) -> PathBuf {
    let dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    if dir.is_absolute() {
        dir
    } else {
        root.join(dir)
    }
}

pub(crate) fn copy_atomically(source: &Path, dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = dest
        .file_name()
        .with_context(|| format!("{} has no file name", dest.display()))?
        .to_string_lossy();
    let staged = parent.join(format!(".{file_name}.tmp.{}", process::id()));
    remove_stale_file(&staged)?;
    fs::copy(source, &staged)
        .with_context(|| format!("staging {} to {}", source.display(), staged.display()))?;
    fs::rename(&staged, dest).with_context(|| format!("installing {}", dest.display()))?;
    Ok(())
}

pub(crate) fn write_atomically(dest: &Path, bytes: &[u8]) -> Result<()> {
    let parent = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = dest
        .file_name()
        .with_context(|| format!("{} has no file name", dest.display()))?
        .to_string_lossy();
    let staged = parent.join(format!(".{file_name}.tmp.{}", process::id()));
    remove_stale_file(&staged)?;
    fs::write(&staged, bytes).with_context(|| format!("writing {}", staged.display()))?;
    fs::rename(&staged, dest).with_context(|| format!("installing {}", dest.display()))?;
    Ok(())
}

pub(crate) fn remove_stale_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}
