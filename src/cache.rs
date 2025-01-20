use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension};

use anyhow::{Context, Result};
pub struct Cache {
    // SQLite connection.
    connection: Connection,
}

pub struct CacheEntry {
    pub etag: Vec<u8>,
    pub file_path: PathBuf,
    pub size: u64,
}

impl Cache {
    // Use dirs::cache_dir().
    pub fn open(dir: &Path) -> Result<Self> {
        let mut cache = Cache {
            connection: Connection::open(dir.join("idx.sql"))?,
        };
        cache.init_tables()?;
        Ok(cache)
    }

    /// Get the entry for a URL, including its etag and file path.
    /// If none exists then return None.
    pub fn get(&self, url: &str) -> Result<Option<CacheEntry>> {
        Ok(self
            .connection
            .query_row(
                "SELECT etag, file_path, size FROM idx WHERE url = ?1",
                params![url],
                |row| {
                    let file_path: Vec<u8> = row.get::<usize, Vec<u8>>(1)?;
                    // SAFETY: This isn't safe if someone modifies the database manually
                    // but otherwise it should be safe. Unfortunately there's no
                    // `from_encoded_bytes() -> Option<>`.
                    let file_path = unsafe { OsString::from_encoded_bytes_unchecked(file_path) };
                    Ok(CacheEntry {
                        etag: row.get(0)?,
                        file_path: file_path.into(),
                        size: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Set the etag and file path for a URL.
    pub fn set(&mut self, url: &str, entry: &CacheEntry) -> Result<()> {
        self.connection.execute(
            "INSERT OR REPLACE INTO idx (url, etag, file_path, size) values (?1, ?2, ?3, ?4)",
            params![
                url,
                entry.etag,
                OsStr::new(&entry.file_path).as_encoded_bytes(),
                entry.size
            ],
        )?;
        Ok(())
    }

    /// Remove an entry.
    pub fn delete(&mut self, url: &str) -> Result<()> {
        self.connection
            .execute("DROP FROM idx WHERE url = ?1", params![url])?;
        Ok(())
    }

    fn init_tables(&mut self) -> Result<()> {
        self.connection
            .execute(
                "
CREATE TABLE IF NOT EXISTS idx (
    -- Download URL.
    url TEXT NOT NULL PRIMARY KEY UNIQUE,
    -- Etag from server.
    etag BLOB NOT NULL,
    -- Full absolute file path.
    file_path BLOB NOT NULL,
    -- Size of the file.
    size INT NOT NULL
    -- TODO: Usage count.
) STRICT",
                params![],
            )
            .context("Initialising SQL tables")?;
        Ok(())
    }

    // TODO: Cache clearing. We could just have a second table that lists files
    // that are in use. Then you can update the index without deleting the
    // files, and later delete the files if they aren't in the index and
    // aren't in use.
}
