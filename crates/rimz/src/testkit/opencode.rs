//! OpenCode 2 SQLite fixtures shared by adapter and consumer tests.

use std::path::Path;

use rusqlite::{Connection, params};

pub struct OpenCodeDb {
    connection: Connection,
}

impl OpenCodeDb {
    pub fn new(path: &Path, wal: bool, credentials: bool) -> Self {
        let connection = Connection::open(path).expect("OpenCode fixture database");
        if wal {
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                .expect("OpenCode fixture WAL");
        }
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS session_v2 (
                    id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
                    title TEXT, version TEXT NOT NULL DEFAULT '2.0.27',
                    cost REAL NOT NULL DEFAULT 0, tokens_input INTEGER NOT NULL DEFAULT 0,
                    tokens_output INTEGER NOT NULL DEFAULT 0, tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                    tokens_cache_read INTEGER NOT NULL DEFAULT 0, tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                    agent TEXT, model TEXT, time_created INTEGER NOT NULL DEFAULT 0,
                    time_updated INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE IF NOT EXISTS session_message (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL,
                    seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL, data TEXT NOT NULL,
                    UNIQUE(session_id, seq));",
            )
            .expect("OpenCode fixture schema");
        if credentials {
            connection
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS credential (
                        id TEXT PRIMARY KEY, integration_id TEXT, label TEXT NOT NULL,
                        value TEXT NOT NULL, active INTEGER,
                        time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);",
                )
                .expect("OpenCode credential schema");
        }
        Self { connection }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn session(&self, id: &str, directory: &Path) {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO session_v2 (id, directory) VALUES (?1, ?2)",
                params![id, directory.to_str().expect("fixture directory")],
            )
            .expect("OpenCode fixture session");
    }

    pub fn message(&self, id: &str, session: &str, kind: &str, at: i64, data: &str) {
        self.connection
            .execute(
                "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(seq), 0) + 1 FROM session_message WHERE session_id = ?2), ?4, ?4, ?5)",
                params![id, session, kind, at, data],
            )
            .expect("OpenCode fixture message");
    }

    pub fn update_message(&self, id: &str, at: i64, data: &str) {
        self.connection
            .execute(
                "UPDATE session_message SET data = ?2, time_updated = ?3 WHERE id = ?1",
                params![id, data, at],
            )
            .expect("OpenCode fixture message update");
    }

    pub fn credential(
        &self,
        id: &str,
        provider: &str,
        active: Option<bool>,
        times: (i64, i64),
        value: &str,
    ) {
        self.connection
            .execute(
                "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated)
                 VALUES (?1, ?2, ?1, ?3, ?4, ?5, ?6)",
                params![id, provider, value, active, times.0, times.1],
            )
            .expect("OpenCode fixture credential");
    }
}
