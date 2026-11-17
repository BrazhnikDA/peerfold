use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::model::{FileRec, PeerRow, ShareRow, VersionInfo, HISTORY_KEEP};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identity (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  device_id TEXT NOT NULL,
  name TEXT NOT NULL,
  secret_hex TEXT NOT NULL,
  pubkey_hex TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS peers (
  device_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  pubkey_hex TEXT NOT NULL,
  trusted INTEGER NOT NULL,
  last_seen INTEGER
);
CREATE TABLE IF NOT EXISTS shares (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  path TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS files (
  share_id TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  deleted INTEGER NOT NULL,
  device_id TEXT NOT NULL,
  synced_hash TEXT,
  PRIMARY KEY (share_id, rel_path)
);
CREATE TABLE IF NOT EXISTS versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  share_id TEXT NOT NULL,
  rel_path TEXT NOT NULL,
  hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  device_id TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS versions_lookup ON versions(share_id, rel_path, created_at DESC);
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn load_identity(&self) -> Result<Option<(String, String, String, String)>> {
        let row = self
            .conn
            .query_row(
                "SELECT device_id, name, secret_hex, pubkey_hex FROM identity WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn save_identity(&self, device_id: &str, name: &str, secret_hex: &str, pubkey_hex: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO identity (id, device_id, name, secret_hex, pubkey_hex)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name",
            params![device_id, name, secret_hex, pubkey_hex],
        )?;
        Ok(())
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        self.conn
            .execute("UPDATE identity SET name = ?1 WHERE id = 1", params![name])?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0))
            .optional()?;
        Ok(value)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn upsert_peer(&self, peer: &PeerRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO peers (device_id, name, pubkey_hex, trusted, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(device_id) DO UPDATE SET
               name = excluded.name,
               pubkey_hex = excluded.pubkey_hex,
               last_seen = excluded.last_seen",
            params![
                peer.device_id,
                peer.name,
                peer.pubkey_hex,
                peer.trusted as i64,
                peer.last_seen
            ],
        )?;
        Ok(())
    }

    pub fn set_trusted(&self, device_id: &str, trusted: bool) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE peers SET trusted = ?1 WHERE device_id = ?2",
            params![trusted as i64, device_id],
        )?;
        if changed == 0 {
            return Err(Error::msg("устройство ещё не видно"));
        }
        Ok(())
    }

    pub fn is_trusted(&self, device_id: &str) -> Result<bool> {
        let trusted: Option<i64> = self
            .conn
            .query_row(
                "SELECT trusted FROM peers WHERE device_id = ?1",
                params![device_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(trusted.unwrap_or(0) != 0)
    }

    pub fn peer(&self, device_id: &str) -> Result<Option<PeerRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT device_id, name, pubkey_hex, trusted, last_seen FROM peers WHERE device_id = ?1",
                params![device_id],
                map_peer,
            )
            .optional()?;
        Ok(row)
    }

    pub fn peers(&self) -> Result<Vec<PeerRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT device_id, name, pubkey_hex, trusted, last_seen FROM peers ORDER BY name")?;
        let rows = stmt.query_map([], map_peer)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn forget_peer(&self, device_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM peers WHERE device_id = ?1", params![device_id])?;
        Ok(())
    }

    pub fn insert_share(&self, share: &ShareRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO shares (id, name, path) VALUES (?1, ?2, ?3)",
            params![share.id, share.name, share.path],
        )?;
        Ok(())
    }

    pub fn shares(&self) -> Result<Vec<ShareRow>> {
        let mut stmt = self.conn.prepare("SELECT id, name, path FROM shares ORDER BY name")?;
        let rows = stmt.query_map([], map_share)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn share(&self, id: &str) -> Result<Option<ShareRow>> {
        Ok(self
            .conn
            .query_row("SELECT id, name, path FROM shares WHERE id = ?1", params![id], map_share)
            .optional()?)
    }

    pub fn remove_share(&self, id: &str) -> Result<Vec<String>> {
        let hashes = self.hashes_for_share(id)?;
        self.conn.execute("DELETE FROM versions WHERE share_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM files WHERE share_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM shares WHERE id = ?1", params![id])?;
        Ok(hashes)
    }

    pub fn file(&self, share_id: &str, rel_path: &str) -> Result<Option<FileRec>> {
        Ok(self
            .conn
            .query_row(
                "SELECT share_id, rel_path, hash, size, mtime_ns, deleted, device_id, synced_hash
                 FROM files WHERE share_id = ?1 AND rel_path = ?2",
                params![share_id, rel_path],
                map_file,
            )
            .optional()?)
    }

    pub fn files(&self, share_id: &str) -> Result<Vec<FileRec>> {
        let mut stmt = self.conn.prepare(
            "SELECT share_id, rel_path, hash, size, mtime_ns, deleted, device_id, synced_hash
             FROM files WHERE share_id = ?1",
        )?;
        let rows = stmt.query_map(params![share_id], map_file)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn upsert_file(&self, file: &FileRec) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files (share_id, rel_path, hash, size, mtime_ns, deleted, device_id, synced_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(share_id, rel_path) DO UPDATE SET
               hash = excluded.hash,
               size = excluded.size,
               mtime_ns = excluded.mtime_ns,
               deleted = excluded.deleted,
               device_id = excluded.device_id,
               synced_hash = excluded.synced_hash",
            params![
                file.share_id,
                file.rel_path,
                file.hash,
                file.size as i64,
                file.mtime_ns,
                file.deleted as i64,
                file.device_id,
                file.synced_hash
            ],
        )?;
        Ok(())
    }

    pub fn set_synced(&self, share_id: &str, rel_path: &str, synced: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET synced_hash = ?1 WHERE share_id = ?2 AND rel_path = ?3",
            params![synced, share_id, rel_path],
        )?;
        Ok(())
    }

    pub fn file_count(&self, share_id: &str) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE share_id = ?1 AND deleted = 0",
            params![share_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    pub fn add_version(&self, version: &VersionInfo, share_id: &str) -> Result<Vec<String>> {
        if version.hash.is_empty() {
            return Ok(Vec::new());
        }
        self.conn.execute(
            "INSERT INTO versions (share_id, rel_path, hash, size, mtime_ns, device_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                share_id,
                version.rel_path,
                version.hash,
                version.size as i64,
                version.mtime_ns,
                version.device_id,
                version.created_at
            ],
        )?;
        self.prune_versions(share_id, &version.rel_path, HISTORY_KEEP)
    }

    pub fn prune_versions(&self, share_id: &str, rel_path: &str, keep: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hash FROM versions WHERE share_id = ?1 AND rel_path = ?2 ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![share_id, rel_path], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut removed = Vec::new();
        for (index, row) in rows.enumerate() {
            let (id, hash) = row?;
            if index >= keep {
                self.conn.execute("DELETE FROM versions WHERE id = ?1", params![id])?;
                removed.push(hash);
            }
        }
        Ok(removed)
    }

    pub fn find_version(&self, share_id: &str, rel_path: &str, hash: &str) -> Result<Option<VersionInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT rel_path, hash, size, mtime_ns, device_id, created_at
                 FROM versions WHERE share_id = ?1 AND rel_path = ?2 AND hash = ?3
                 ORDER BY created_at DESC LIMIT 1",
                params![share_id, rel_path, hash],
                |row| {
                    Ok(VersionInfo {
                        rel_path: row.get(0)?,
                        hash: row.get(1)?,
                        size: row.get::<_, i64>(2)? as u64,
                        mtime_ns: row.get(3)?,
                        device_id: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn recent_versions(&self, share_id: &str, limit: usize) -> Result<Vec<VersionInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path, hash, size, mtime_ns, device_id, created_at
             FROM versions WHERE share_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![share_id, limit as i64], |row| {
            Ok(VersionInfo {
                rel_path: row.get(0)?,
                hash: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
                mtime_ns: row.get(3)?,
                device_id: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn hash_in_use(&self, hash: &str) -> Result<bool> {
        let used: i64 = self.conn.query_row(
            "SELECT
                (SELECT COUNT(*) FROM files WHERE hash = ?1) +
                (SELECT COUNT(*) FROM versions WHERE hash = ?1)",
            params![hash],
            |row| row.get(0),
        )?;
        Ok(used > 0)
    }

    fn hashes_for_share(&self, share_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT hash FROM files WHERE share_id = ?1
             UNION SELECT hash FROM versions WHERE share_id = ?1",
        )?;
        let rows = stmt.query_map(params![share_id], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

fn map_peer(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerRow> {
    Ok(PeerRow {
        device_id: row.get(0)?,
        name: row.get(1)?,
        pubkey_hex: row.get(2)?,
        trusted: row.get::<_, i64>(3)? != 0,
        last_seen: row.get(4)?,
    })
}

fn map_share(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShareRow> {
    Ok(ShareRow {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
    })
}

fn map_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRec> {
    Ok(FileRec {
        share_id: row.get(0)?,
        rel_path: row.get(1)?,
        hash: row.get(2)?,
        size: row.get::<_, i64>(3)? as u64,
        mtime_ns: row.get(4)?,
        deleted: row.get::<_, i64>(5)? != 0,
        device_id: row.get(6)?,
        synced_hash: row.get(7)?,
    })
}
