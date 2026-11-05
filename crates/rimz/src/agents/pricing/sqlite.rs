//! Version-one price tables: immutable reads and full, durably published rebuilds.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use super::{Pricing, fuzzy_candidates, normalize};
use crate::disk::{atomic, paths::PRICING_CACHE_VERSION};

#[derive(Debug, thiserror::Error)]
pub(super) enum TableErr {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Publish(#[from] atomic::AtomicErr),
}

#[derive(Debug)]
pub(super) struct Table {
    connection: Mutex<Connection>,
    pub(super) fetched_at_secs: u64,
    pub(super) build_id: String,
}

impl Table {
    pub(super) fn open(path: &Path) -> Option<Self> {
        let mut uri = url::Url::from_file_path(std::path::absolute(path).ok()?).ok()?;
        uri.set_query(Some("mode=ro&immutable=1"));
        let connection = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        connection.busy_timeout(Duration::ZERO).ok()?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .ok()?;
        if version != PRICING_CACHE_VERSION {
            return None;
        }
        let fetched_at: i64 = connection
            .query_row(
                "SELECT value FROM meta WHERE name = 'fetched_at_secs'",
                [],
                |row| row.get(0),
            )
            .ok()?;
        let build_id = connection
            .query_row(
                "SELECT value FROM meta WHERE name = 'build_id'",
                [],
                |row| row.get(0),
            )
            .ok()?;
        Some(Self {
            connection: Mutex::new(connection),
            fetched_at_secs: fetched_at as u64,
            build_id,
        })
    }

    pub(super) fn exact(&self, key: &str) -> rusqlite::Result<Option<Pricing>> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .prepare_cached("SELECT * FROM prices WHERE key = ?1")?
            .query_row([key], read_price)
            .optional()
    }

    pub(super) fn fuzzy(&self, want: &str) -> rusqlite::Result<Option<Pricing>> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut query = connection
            .prepare_cached("SELECT * FROM prices WHERE norm = ?1 ORDER BY key LIMIT 1")?;
        for candidate in fuzzy_candidates(want) {
            if let Some(price) = query.query_row([candidate], read_price).optional()? {
                return Ok(Some(price));
            }
        }
        Ok(None)
    }

    pub(super) fn rows(&self) -> rusqlite::Result<HashMap<String, Pricing>> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .prepare("SELECT * FROM prices")?
            .query_map([], |row| Ok((row.get(0)?, read_price(row)?)))?
            .collect()
    }
}

fn read_price(row: &Row<'_>) -> rusqlite::Result<Pricing> {
    Ok(Pricing {
        input: row.get(2)?,
        output: row.get(3)?,
        cache_read: row.get(4)?,
        cache_create: row.get(5)?,
        cache_create_1h: row.get(6)?,
        cache_read_explicit: row.get(7)?,
        input_above_200k: row.get(8)?,
        output_above_200k: row.get(9)?,
        cache_create_above_200k: row.get(10)?,
        cache_read_above_200k: row.get(11)?,
        long_context_threshold: unsigned(row, 12)?,
        fast_multiplier: row.get(13)?,
        max_input_tokens: unsigned(row, 14)?,
    })
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    // SQLite integers are signed; the codec preserves every bit of Rust's u64.
    let value: Option<i64> = row.get(index)?;
    Ok(value.map(|value| value as u64))
}

pub(super) fn build(
    path: &Path,
    rows: &HashMap<String, Pricing>,
    fetched_at_secs: u64,
) -> Result<(), TableErr> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    atomic::sweep_stale_temp_siblings(path, Duration::from_secs(3_600));
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));
    let staged = path.with_file_name(name);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    let result = build_staged(&staged, rows, fetched_at_secs)
        .and_then(|()| atomic::publish_staged_replace(&staged, path).map_err(TableErr::from));
    let _ = fs::remove_file(staged);
    result
}

fn build_staged(
    staged: &Path,
    rows: &HashMap<String, Pricing>,
    fetched_at_secs: u64,
) -> Result<(), TableErr> {
    let mut connection = Connection::open_with_flags(
        staged,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
    connection.pragma_update(None, "user_version", PRICING_CACHE_VERSION)?;
    let transaction = connection.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE prices (
            key TEXT PRIMARY KEY, norm TEXT NOT NULL,
            input REAL NOT NULL, output REAL NOT NULL,
            cache_read REAL NOT NULL, cache_create REAL NOT NULL,
            cache_create_1h REAL, cache_read_explicit INTEGER NOT NULL,
            input_above_200k REAL, output_above_200k REAL,
            cache_create_above_200k REAL, cache_read_above_200k REAL,
            long_context_threshold INTEGER, fast_multiplier REAL NOT NULL,
            max_input_tokens INTEGER
        );
        CREATE INDEX prices_norm_key ON prices (norm, key);
        CREATE TABLE meta (name TEXT PRIMARY KEY, value);",
    )?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO prices VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for (key, price) in rows {
            insert.execute(params![
                key,
                normalize(key),
                price.input,
                price.output,
                price.cache_read,
                price.cache_create,
                price.cache_create_1h,
                price.cache_read_explicit,
                price.input_above_200k,
                price.output_above_200k,
                price.cache_create_above_200k,
                price.cache_read_above_200k,
                price.long_context_threshold.map(|tokens| tokens as i64),
                price.fast_multiplier,
                price.max_input_tokens.map(|tokens| tokens as i64),
            ])?;
        }
    }
    transaction.execute(
        "INSERT INTO meta VALUES ('fetched_at_secs', ?1)",
        [fetched_at_secs as i64],
    )?;
    transaction.execute(
        "INSERT INTO meta VALUES ('build_id', ?1)",
        [uuid::Uuid::now_v7().to_string()],
    )?;
    transaction.commit()?;
    connection.close().map_err(|(_, error)| error)?;
    Ok(())
}
