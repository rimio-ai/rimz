use super::*;

pub(super) fn checkout(root: &Path, source: &InstalledSource) -> Result<PathBuf> {
    let target = checkout_target(root, source)?;
    let root = std::path::absolute(root).map_err(|error| io_err(root, error))?;
    Ok(normalize_path_lexical(&root).join(target))
}

pub(super) fn checkout_target(root: &Path, source: &InstalledSource) -> Result<PathBuf> {
    let commit = source
        .commit
        .as_deref()
        .filter(|commit| is_commit(commit))
        .ok_or_else(|| {
            problem(
                &root.join("installed.json"),
                format!("Git source {} needs a full 40-hex commit", source.source),
            )
        })?;
    Ok(Path::new(".checkouts").join(format!("{}-{commit}", source.entry)))
}

pub(super) fn maintain(root: &Path, record: &Record) -> Result<()> {
    let mut keep = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(|error| io_err(root, error))? {
        let entry = entry.map_err(|error| io_err(root, error))?;
        if let Ok(target) = fs::read_link(entry.path()) {
            let target =
                std::path::absolute(root.join(target)).map_err(|error| io_err(root, error))?;
            keep.insert(normalize_path_lexical(&target));
        }
    }
    for source in record.sources.iter().filter(|source| !source.linked) {
        keep.insert(checkout(root, source)?);
        let target = checkout_target(root, source)?;
        let entry = root.join(&source.entry);
        if !fs::read_link(&entry).is_ok_and(|current| current == target) {
            atomic::symlink_atomically(&target, &entry)?;
        }
    }
    let checkouts = root.join(".checkouts");
    let entries = match fs::read_dir(&checkouts) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_err(&checkouts, error)),
    };
    let mut candidates = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_err(&checkouts, error))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| io_err(&path, error))?;
        if kind.is_dir() {
            candidates.insert(path);
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "lock")
        {
            candidates.insert(path.with_extension(""));
        }
    }
    let mut collected = false;
    for path in candidates {
        let absolute = std::path::absolute(&path).map_err(|error| io_err(&path, error))?;
        if keep.contains(&normalize_path_lexical(&absolute)) {
            continue;
        }
        let lock = path.with_extension("lock");
        let Some(_hold) = WorkspaceLock::try_acquire(&lock)? else {
            continue;
        };
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_err(&path, error)),
        }
        fs::remove_file(&lock).map_err(|error| io_err(&lock, error))?;
        collected = true;
    }
    if collected {
        atomic::sync_dir(&checkouts)?;
    }
    Ok(())
}
