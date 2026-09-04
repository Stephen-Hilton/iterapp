//! SQLite backend: one generic rows table + a versions table.
//! rusqlite is sync; calls are short and serialized behind a Mutex, which is
//! fine for the local single-file case this backend exists for.

use crate::storage::{Storage, StorageError};
use async_trait::async_trait;
use iter_core::{VersionRow, now_utc};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::sync::Mutex;

pub struct SqliteBackend {
    conn: Mutex<Connection>,
}

impl SqliteBackend {
    pub fn open(path: &str) -> Result<Self, StorageError> {
        let conn = Connection::open(path).map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS rows (
                 tbl     TEXT NOT NULL,
                 pk      TEXT NOT NULL,
                 sk      TEXT NOT NULL,
                 version INTEGER NOT NULL DEFAULT 0,
                 expires TEXT NOT NULL DEFAULT '',
                 workid  TEXT NOT NULL DEFAULT '',
                 body    TEXT NOT NULL,
                 PRIMARY KEY (tbl, pk, sk)
             );
             CREATE TABLE IF NOT EXISTS versions (
                 projectname TEXT NOT NULL,
                 tbl         TEXT NOT NULL,
                 seq         INTEGER NOT NULL DEFAULT 0,
                 updated     TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (projectname, tbl)
             );",
        )
        .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

fn berr<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Backend(e.to_string())
}

fn get_sync(conn: &Connection, table: &str, pk: &str, sk: &str) -> Option<Value> {
    conn.query_row(
        "SELECT body FROM rows WHERE tbl=?1 AND pk=?2 AND sk=?3",
        params![table, pk, sk],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
}

#[async_trait]
impl Storage for SqliteBackend {
    async fn get(&self, table: &str, pk: &str, sk: &str) -> Result<Option<Value>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT body FROM rows WHERE tbl=?1 AND pk=?2 AND sk=?3")
            .map_err(berr)?;
        let mut rows = stmt.query(params![table, pk, sk]).map_err(berr)?;
        match rows.next().map_err(berr)? {
            Some(row) => {
                let body: String = row.get(0).map_err(berr)?;
                Ok(serde_json::from_str(&body).ok())
            }
            None => Ok(None),
        }
    }

    async fn put(&self, table: &str, pk: &str, sk: &str, body: &Value) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        // keep the native version in step with the body (versioned writes
        // compare against the column, so a plain put must not leave it stale)
        let ver: Option<i64> = body.get("version").and_then(|v| v.as_i64());
        conn.execute(
            "INSERT INTO rows (tbl, pk, sk, version, body) VALUES (?1, ?2, ?3, COALESCE(?5, 0), ?4)
             ON CONFLICT(tbl, pk, sk) DO UPDATE SET body = excluded.body,
             version = COALESCE(?5, rows.version)",
            params![table, pk, sk, body.to_string(), ver],
        )
        .map_err(berr)?;
        Ok(())
    }

    async fn delete(&self, table: &str, pk: &str, sk: &str) -> Result<bool, StorageError> {
        let conn = self.conn.lock().unwrap();
        let n = conn
            .execute("DELETE FROM rows WHERE tbl=?1 AND pk=?2 AND sk=?3", params![table, pk, sk])
            .map_err(berr)?;
        Ok(n > 0)
    }

    async fn query(&self, table: &str, pk: &str) -> Result<Vec<Value>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT body FROM rows WHERE tbl=?1 AND pk=?2 ORDER BY sk")
            .map_err(berr)?;
        let out = stmt
            .query_map(params![table, pk], |row| row.get::<_, String>(0))
            .map_err(berr)?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        Ok(out)
    }

    async fn scan(&self, table: &str) -> Result<Vec<Value>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT body FROM rows WHERE tbl=?1 ORDER BY pk, sk")
            .map_err(berr)?;
        let out = stmt
            .query_map(params![table], |row| row.get::<_, String>(0))
            .map_err(berr)?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        Ok(out)
    }

    async fn put_versioned(
        &self,
        table: &str,
        pk: &str,
        sk: &str,
        body: &Value,
        expect: u64,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        if expect == 0 {
            let res = conn.execute(
                "INSERT INTO rows (tbl, pk, sk, version, body) VALUES (?1, ?2, ?3, 1, ?4)",
                params![table, pk, sk, body.to_string()],
            );
            match res {
                Ok(_) => Ok(()),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Err(StorageError::Conflict(get_sync(&conn, table, pk, sk)))
                }
                Err(e) => Err(berr(e)),
            }
        } else {
            let n = conn
                .execute(
                    "UPDATE rows SET version = ?1, body = ?2
                     WHERE tbl=?3 AND pk=?4 AND sk=?5 AND version = ?6",
                    params![(expect + 1) as i64, body.to_string(), table, pk, sk, expect as i64],
                )
                .map_err(berr)?;
            if n == 0 {
                Err(StorageError::Conflict(get_sync(&conn, table, pk, sk)))
            } else {
                Ok(())
            }
        }
    }

    async fn acquire_lock(
        &self,
        table: &str,
        pk: &str,
        sk: &str,
        body: &Value,
        now: &str,
        holder_workid: &str,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        // one transaction: inspect current holder, replace only if free/expired/self
        let tx = conn.unchecked_transaction().map_err(berr)?;
        let current: Option<(String, String)> = {
            let mut stmt = tx
                .prepare("SELECT expires, workid FROM rows WHERE tbl=?1 AND pk=?2 AND sk=?3")
                .map_err(berr)?;
            let mut rows = stmt.query(params![table, pk, sk]).map_err(berr)?;
            match rows.next().map_err(berr)? {
                Some(row) => Some((row.get(0).map_err(berr)?, row.get(1).map_err(berr)?)),
                None => None,
            }
        };
        if let Some((expires, workid)) = &current {
            let expired = !expires.is_empty() && expires.as_str() < now;
            if !expired && workid != holder_workid {
                let body_row: Option<Value> = {
                    let mut stmt = tx
                        .prepare("SELECT body FROM rows WHERE tbl=?1 AND pk=?2 AND sk=?3")
                        .map_err(berr)?;
                    let mut rows = stmt.query(params![table, pk, sk]).map_err(berr)?;
                    rows.next()
                        .map_err(berr)?
                        .and_then(|r| r.get::<_, String>(0).ok())
                        .and_then(|s| serde_json::from_str(&s).ok())
                };
                return Err(StorageError::Conflict(body_row));
            }
        }
        let expires = body.get("expires").and_then(|v| v.as_str()).unwrap_or("");
        tx.execute(
            "INSERT INTO rows (tbl, pk, sk, expires, workid, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tbl, pk, sk) DO UPDATE
             SET expires = excluded.expires, workid = excluded.workid, body = excluded.body",
            params![table, pk, sk, expires, holder_workid, body.to_string()],
        )
        .map_err(berr)?;
        tx.commit().map_err(berr)?;
        Ok(())
    }

    async fn bump_seq(&self, project: &str, logical_table: &str) -> Result<u64, StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO versions (projectname, tbl, seq, updated) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(projectname, tbl) DO UPDATE SET seq = seq + 1, updated = excluded.updated",
            params![project, logical_table, now_utc()],
        )
        .map_err(berr)?;
        let seq: i64 = conn
            .query_row(
                "SELECT seq FROM versions WHERE projectname=?1 AND tbl=?2",
                params![project, logical_table],
                |r| r.get(0),
            )
            .map_err(berr)?;
        Ok(seq as u64)
    }

    async fn get_versions(&self, project: &str) -> Result<Vec<VersionRow>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT projectname, tbl, seq, updated FROM versions WHERE projectname=?1")
            .map_err(berr)?;
        let out = stmt
            .query_map(params![project], |row| {
                Ok(VersionRow {
                    projectname: row.get(0)?,
                    table: row.get(1)?,
                    seq: row.get::<_, i64>(2)? as u64,
                    updated: row.get(3)?,
                })
            })
            .map_err(berr)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(out)
    }

    fn backend_name(&self) -> &'static str {
        "sqlite"
    }
}
