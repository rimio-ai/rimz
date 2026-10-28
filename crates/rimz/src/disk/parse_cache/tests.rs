use std::cell::Cell;

use super::*;

fn stamp(inode: u64, len: u64, modified_nanos: u32) -> FileStamp {
    FileStamp {
        len,
        modified_secs: 100,
        modified_nanos,
        device: 1,
        inode,
    }
}

fn read(
    cache: &KeyedParseCache<u64>,
    path: &str,
    stamp: Option<FileStamp>,
    bytes: &[u8],
    reads: &Cell<usize>,
) -> Option<Arc<u64>> {
    cache
        .read_entry(
            Path::new(path),
            stamp,
            || {
                reads.set(reads.get() + 1);
                Ok(bytes.to_vec())
            },
            |bytes| serde_json::from_slice(bytes).ok(),
        )
        .unwrap()
}

#[test]
fn warm_paths_share_values_and_cache_missing_and_malformed_outcomes() {
    let cache = KeyedParseCache::default();
    let reads = Cell::new(0);
    let first = read(&cache, "valid", Some(stamp(1, 1, 0)), b"1", &reads).unwrap();
    read(&cache, "missing", None, b"", &reads);
    read(&cache, "malformed", Some(stamp(2, 1, 0)), b"{", &reads);
    let warm = read(&cache, "valid", Some(stamp(1, 1, 0)), b"2", &reads).unwrap();
    read(&cache, "missing", None, b"", &reads);
    read(&cache, "malformed", Some(stamp(2, 1, 0)), b"{", &reads);
    assert_eq!(
        (
            Arc::ptr_eq(&first, &warm),
            reads.get(),
            cache.entries.borrow().len()
        ),
        (true, 2, 3),
    );
}

#[test]
fn changed_identity_reloads_each_outcome() {
    let cache = KeyedParseCache::default();
    let reads = Cell::new(0);
    let mut values = Vec::new();
    for (identity, bytes) in [
        (None, b"".as_slice()),
        (Some(stamp(1, 1, 0)), b"1"),
        (Some(stamp(2, 1, 0)), b"2"),
        (Some(stamp(2, 2, 0)), b"12"),
        (Some(stamp(2, 2, 1)), b"13"),
        (Some(stamp(2, 1, 2)), b"{"),
        (Some(stamp(2, 1, 3)), b"4"),
    ] {
        values.push(
            read(&cache, "file", identity, bytes, &reads)
                .as_deref()
                .copied(),
        );
        read(&cache, "file", identity, bytes, &reads);
    }
    assert_eq!(
        (values, reads.get()),
        (
            vec![None, Some(1), Some(2), Some(12), Some(13), None, Some(4)],
            6
        ),
    );
}

#[test]
fn io_failures_are_retried_without_caching_them() {
    let cache = KeyedParseCache::<u64>::default();
    let path = Path::new("file");
    for _ in 0..2 {
        let error = cache
            .read_entry(
                path,
                Some(stamp(1, 1, 0)),
                || Err(io::Error::from(io::ErrorKind::PermissionDenied)),
                |_| panic!("an I/O failure must not parse"),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(cache.entries.borrow().is_empty());
    }
    let reads = Cell::new(0);
    read(&cache, "file", Some(stamp(1, 1, 0)), b"1", &reads);
    read(&cache, "file", Some(stamp(1, 1, 0)), b"1", &reads);
    assert_eq!(reads.get(), 1, "a recovered file must become warm");
}

#[test]
fn churn_is_bounded_and_repeated_keys_do_not_fill_the_eviction_queue() {
    let cache = KeyedParseCache::default();
    let reads = Cell::new(0);
    for round in 0..10 {
        for slot in 0..200 {
            let path = format!("{round}/{slot}");
            for _ in 0..2 {
                read(&cache, &path, None, b"", &reads);
            }
        }
    }
    assert_eq!(cache.entries.borrow().len(), KEYED_CACHE_LIMIT);
    assert_eq!(cache.order.borrow().len(), KEYED_CACHE_LIMIT);
    assert_eq!(reads.get(), 0);
}
