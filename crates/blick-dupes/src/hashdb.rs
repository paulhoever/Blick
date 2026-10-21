//! Persistenter Cache für Hashes und Bildsignaturen. Schlüssel `(uri, size, mtime)`:
//! ändert sich die Datei, ist der Eintrag automatisch tot.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::source::Entry;

pub struct HashDb {
    conn: Mutex<Connection>,
}

#[derive(Clone, Copy)]
pub enum Col {
    Partial,
    Full,
    Sig,
    Sha1,
}

impl Col {
    fn name(self) -> &'static str {
        match self {
            Col::Partial => "partial",
            Col::Full => "full",
            Col::Sig => "sig",
            Col::Sha1 => "sha1",
        }
    }
}

impl HashDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=OFF;
             CREATE TABLE IF NOT EXISTS files(
               uri TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
               partial BLOB, full BLOB, sig BLOB, sha1 BLOB,
               PRIMARY KEY(uri, size, mtime)) WITHOUT ROWID;",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn get(&self, e: &Entry, col: Col) -> Option<Vec<u8>> {
        let sql = format!(
            "SELECT {} FROM files WHERE uri=?1 AND size=?2 AND mtime=?3",
            col.name()
        );
        let conn = self.conn.lock().ok()?;
        conn.query_row(&sql, params![e.uri, e.size as i64, e.mtime], |r| {
            r.get::<_, Option<Vec<u8>>>(0)
        })
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    pub fn put(&self, e: &Entry, col: Col, value: &[u8]) {
        let sql = format!(
            "INSERT INTO files(uri,size,mtime,{c}) VALUES(?1,?2,?3,?4)
             ON CONFLICT(uri,size,mtime) DO UPDATE SET {c}=excluded.{c}",
            c = col.name()
        );
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(&sql, params![e.uri, e.size as i64, e.mtime, value]);
        }
    }
}
