//! The scan result, in SQLite. Disposable: on a schema change or damage it is thrown away and
//! filled again from the photos.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::layout::Placement;
use crate::metadata::Metadata;
use crate::scan::Issue;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE photo (
    id          INTEGER PRIMARY KEY,
    rel_path    TEXT NOT NULL UNIQUE,
    size        INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    inode       INTEGER NOT NULL,
    content_id  TEXT,
    country     TEXT,
    city        TEXT,
    event_text  TEXT,
    event_year  INTEGER,
    event_month INTEGER,
    event_day   INTEGER,
    event_name  TEXT,
    sub_path    TEXT,
    taken_at    TEXT,
    taken_offset TEXT,
    xmp_taken_at TEXT,
    gps_lat     REAL,
    gps_lon     REAL,
    camera_make TEXT,
    camera_model TEXT,
    orientation INTEGER,
    rating      INTEGER,
    width       INTEGER,
    height      INTEGER,
    raw         TEXT NOT NULL
);
CREATE INDEX photo_content ON photo (content_id);
CREATE INDEX photo_event ON photo (country, event_text);

CREATE TABLE tag (
    photo_id INTEGER NOT NULL REFERENCES photo (id) ON DELETE CASCADE,
    path     TEXT NOT NULL,
    leaf     TEXT NOT NULL
);
CREATE INDEX tag_photo ON tag (photo_id);
CREATE INDEX tag_path ON tag (path);

CREATE TABLE issue (
    photo_id INTEGER REFERENCES photo (id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    kind     TEXT NOT NULL,
    detail   TEXT
);
CREATE INDEX issue_kind ON issue (kind);
";

/// What the filesystem alone says about a file, enough to decide whether to read it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i64,
    pub inode: u64,
}

#[derive(Debug, Clone)]
pub struct Known {
    pub id: i64,
    pub fingerprint: Fingerprint,
    pub content_id: Option<String>,
}

#[derive(Debug)]
pub struct Cache {
    connection: Connection,
    file: PathBuf,
}

pub type Result<T> = rusqlite::Result<T>;

impl Cache {
    /// Opens the cache, replacing it when it is from another schema or unreadable.
    pub fn open(file: &Path) -> Result<Cache> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(14), Some(error.to_string()))
            })?;
        }
        match Cache::open_existing(file) {
            Ok(Some(cache)) => return Ok(cache),
            Ok(None) => tracing::info!(cache = %file.display(), "cache from another version, starting over"),
            Err(error) => tracing::warn!(%error, "cache unreadable, starting over"),
        }
        let _ = std::fs::remove_file(file);
        Cache::create(file)
    }

    fn open_existing(file: &Path) -> Result<Option<Cache>> {
        if !file.exists() {
            return Ok(None);
        }
        let connection = Connection::open(file)?;
        prepare(&connection)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Ok(None);
        }
        connection.query_row("SELECT count(*) FROM photo", [], |row| row.get::<_, i64>(0))?;
        Ok(Some(Cache {
            connection,
            file: file.to_path_buf(),
        }))
    }

    fn create(file: &Path) -> Result<Cache> {
        let connection = Connection::open(file)?;
        prepare(&connection)?;
        connection.execute_batch(SCHEMA)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Cache {
            connection,
            file: file.to_path_buf(),
        })
    }

    /// Throws the cache away and starts an empty one.
    pub fn rebuild(self) -> Result<Cache> {
        let file = self.file.clone();
        drop(self);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", file.display()));
        }
        Cache::create(&file)
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn photo_count(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT count(*) FROM photo", [], |row| row.get(0))
    }

    pub fn event_count(&self) -> Result<i64> {
        self.connection.query_row(
            "SELECT count(*) FROM (SELECT DISTINCT country, city, event_text, event_name FROM photo WHERE event_text IS NOT NULL)",
            [],
            |row| row.get(0),
        )
    }

    pub fn issue_count(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT count(*) FROM issue", [], |row| row.get(0))
    }

    pub fn issue_counts(&self) -> Result<Vec<(String, i64)>> {
        let mut statement = self
            .connection
            .prepare("SELECT kind, count(*) FROM issue GROUP BY kind ORDER BY count(*) DESC")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn known(&self, rel_path: &str) -> Result<Option<Known>> {
        self.connection
            .query_row(
                "SELECT id, size, mtime_ns, inode, content_id FROM photo WHERE rel_path = ?1",
                params![rel_path],
                |row| {
                    Ok(Known {
                        id: row.get(0)?,
                        fingerprint: Fingerprint {
                            size: row.get::<_, i64>(1)? as u64,
                            mtime_ns: row.get(2)?,
                            inode: row.get::<_, i64>(3)? as u64,
                        },
                        content_id: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    /// Every row's fingerprint, so the workers can decide without touching the connection.
    pub fn all_known(&self) -> Result<std::collections::HashMap<String, Known>> {
        let mut statement = self
            .connection
            .prepare("SELECT rel_path, id, size, mtime_ns, inode, content_id FROM photo")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Known {
                    id: row.get(1)?,
                    fingerprint: Fingerprint {
                        size: row.get::<_, i64>(2)? as u64,
                        mtime_ns: row.get(3)?,
                        inode: row.get::<_, i64>(4)? as u64,
                    },
                    content_id: row.get(5)?,
                },
            ))
        })?;
        rows.collect()
    }

    pub fn paths(&self) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare("SELECT rel_path FROM photo")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// The row of a photo with this image data that is no longer where it was: a move.
    pub fn moved_from(&self, content_id: &str, gone: &[String]) -> Result<Option<String>> {
        if gone.is_empty() {
            return Ok(None);
        }
        let mut statement = self
            .connection
            .prepare("SELECT rel_path FROM photo WHERE content_id = ?1")?;
        let rows = statement.query_map(params![content_id], |row| row.get::<_, String>(0))?;
        for row in rows {
            let path = row?;
            if gone.contains(&path) {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    pub fn transaction(&mut self) -> Result<Writer<'_>> {
        Ok(Writer {
            transaction: self.connection.transaction()?,
        })
    }

    pub fn forget(&mut self, rel_paths: &[String]) -> Result<usize> {
        let writer = self.transaction()?;
        let mut removed = 0;
        for path in rel_paths {
            removed += writer.forget(path)?;
        }
        writer.commit()?;
        Ok(removed)
    }
}

fn prepare(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))
}

pub struct Writer<'a> {
    transaction: rusqlite::Transaction<'a>,
}

impl Writer<'_> {
    pub fn forget(&self, rel_path: &str) -> Result<usize> {
        self.transaction
            .execute("DELETE FROM photo WHERE rel_path = ?1", params![rel_path])
    }

    pub fn forget_issues(&self, rel_path: &str) -> Result<()> {
        self.transaction
            .execute("DELETE FROM issue WHERE rel_path = ?1", params![rel_path])?;
        Ok(())
    }

    pub fn put(
        &self,
        rel_path: &str,
        fingerprint: Fingerprint,
        content_id: Option<&str>,
        placement: &Placement,
        metadata: &Metadata,
    ) -> Result<i64> {
        self.forget(rel_path)?;
        self.transaction.execute(
            "INSERT INTO photo (rel_path, size, mtime_ns, inode, content_id, country, city, event_text,
                event_year, event_month, event_day, event_name, sub_path, taken_at, taken_offset,
                xmp_taken_at, gps_lat, gps_lon, camera_make, camera_model, orientation, rating,
                width, height, raw)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                rel_path,
                fingerprint.size as i64,
                fingerprint.mtime_ns,
                fingerprint.inode as i64,
                content_id,
                placement.country,
                placement.city,
                placement.event_text,
                placement.event_year,
                placement.event_month,
                placement.event_day,
                placement.event_name,
                placement.sub_path,
                metadata.taken_at,
                metadata.taken_offset,
                metadata.xmp_taken_at,
                metadata.gps_lat,
                metadata.gps_lon,
                metadata.camera_make,
                metadata.camera_model,
                metadata.orientation,
                metadata.rating,
                metadata.width,
                metadata.height,
                metadata.raw,
            ],
        )?;
        let id = self.transaction.last_insert_rowid();

        let mut tags = self
            .transaction
            .prepare_cached("INSERT INTO tag (photo_id, path, leaf) VALUES (?1, ?2, ?3)")?;
        for path in &metadata.tags {
            let leaf = path.rsplit('/').next().unwrap_or(path);
            tags.execute(params![id, path, leaf])?;
        }
        Ok(id)
    }

    pub fn add_issue(&self, issue: &Issue, photo_id: Option<i64>) -> Result<()> {
        self.transaction.execute(
            "INSERT INTO issue (photo_id, rel_path, kind, detail) VALUES (?1, ?2, ?3, ?4)",
            params![photo_id, issue.rel_path, issue.kind.as_str(), issue.detail],
        )?;
        Ok(())
    }

    pub fn forget_issues_of_kind(&self, kind: &str) -> Result<()> {
        self.transaction
            .execute("DELETE FROM issue WHERE kind = ?1", params![kind])?;
        Ok(())
    }

    /// Photos whose image data appears more than once in the library.
    pub fn note_duplicates(&self, kind: &str) -> Result<usize> {
        self.transaction.execute(
            "INSERT INTO issue (photo_id, rel_path, kind, detail)
             SELECT id, rel_path, ?1, content_id FROM photo
             WHERE content_id IN (
                 SELECT content_id FROM photo WHERE content_id IS NOT NULL
                 GROUP BY content_id HAVING count(*) > 1
             )",
            params![kind],
        )
    }

    pub fn commit(self) -> Result<()> {
        self.transaction.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photomanager-cache-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("cache.db")
    }

    fn put_one(cache: &mut Cache, rel_path: &str) {
        let writer = cache.transaction().unwrap();
        writer
            .put(
                rel_path,
                Fingerprint {
                    size: 10,
                    mtime_ns: 20,
                    inode: 30,
                },
                Some("abc"),
                &Placement::parse(rel_path),
                &Metadata::empty(),
            )
            .unwrap();
        writer.commit().unwrap();
    }

    #[test]
    fn creates_reopens_and_keeps_its_rows() {
        let file = temp("reopen");
        let mut cache = Cache::open(&file).unwrap();
        put_one(&mut cache, "China/2006-09-00 Besuch/P1.JPG");
        assert_eq!(cache.photo_count().unwrap(), 1);
        drop(cache);

        let cache = Cache::open(&file).unwrap();
        assert_eq!(cache.photo_count().unwrap(), 1);
        assert!(cache.known("China/2006-09-00 Besuch/P1.JPG").unwrap().is_some());
        assert!(cache.known("nothing").unwrap().is_none());
    }

    #[test]
    fn starts_over_when_the_schema_moved_on() {
        let file = temp("schema");
        let mut cache = Cache::open(&file).unwrap();
        put_one(&mut cache, "a.jpg");
        cache
            .connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(cache);

        let cache = Cache::open(&file).unwrap();
        assert_eq!(cache.photo_count().unwrap(), 0, "a foreign schema is discarded");
    }

    #[test]
    fn replaces_a_damaged_file_instead_of_failing() {
        let file = temp("damaged");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"this is not a database").unwrap();

        let cache = Cache::open(&file).unwrap();
        assert_eq!(cache.photo_count().unwrap(), 0);
    }

    #[test]
    fn rebuild_empties_it() {
        let file = temp("rebuild");
        let mut cache = Cache::open(&file).unwrap();
        put_one(&mut cache, "a.jpg");
        let cache = cache.rebuild().unwrap();
        assert_eq!(cache.photo_count().unwrap(), 0);
    }

    #[test]
    fn writes_nothing_outside_its_directory() {
        let file = temp("contained");
        let mut cache = Cache::open(&file).unwrap();
        put_one(&mut cache, "a.jpg");
        let dir = file.parent().unwrap();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            assert!(path.starts_with(dir), "{} is outside the cache", path.display());
        }
    }
}
