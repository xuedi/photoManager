//! What we did to the photos, written down before we do it.
//!
//! This is the one database that is not disposable. The cache can be thrown away and filled from
//! the files again; an undo cannot, because the old values it puts back are the only copy of what
//! the photo used to say. So it lives in the data directory next to the place data, it is never
//! deleted to get past a schema it does not know, and every entry is committed before ExifTool is
//! asked to do anything.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::now;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE batch (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    undoes      INTEGER REFERENCES batch (id)
);
CREATE INDEX batch_undoes ON batch (undoes);

CREATE TABLE entry (
    id         INTEGER PRIMARY KEY,
    batch_id   INTEGER NOT NULL REFERENCES batch (id),
    rel_path   TEXT NOT NULL,
    content_id TEXT NOT NULL,
    before     TEXT NOT NULL,
    image_hash TEXT,
    outcome    TEXT,
    detail     TEXT
);
CREATE INDEX entry_batch ON entry (batch_id);
CREATE INDEX entry_path ON entry (rel_path);

CREATE TABLE swap (
    entry_id INTEGER NOT NULL REFERENCES entry (id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    key      TEXT NOT NULL,
    old      TEXT,
    new      TEXT
);
CREATE INDEX swap_entry ON swap (entry_id);
";

/// What an entry came to. Anything but `WRITTEN` means the photo was left alone.
pub const WRITTEN: &str = "written";
pub const FAILED: &str = "failed";
pub const REFUSED: &str = "refused";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Write,
    Undo,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Write => "write",
            Kind::Undo => "undo",
        }
    }
}

/// One tag's two sides. `None` means the tag was not there, or is not to be there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swap {
    pub tag: String,
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// Everything about one photo that has to be known before it is touched.
#[derive(Debug, Clone)]
pub struct Entry {
    pub rel_path: String,
    pub content_id: String,
    /// The whole metadata as ExifTool read it, so an undo never depends on our field list.
    pub before: String,
    pub image_hash: Option<String>,
    pub swaps: Vec<Swap>,
}

/// An entry as the journal holds it.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub id: i64,
    pub batch_id: i64,
    pub rel_path: String,
    pub content_id: String,
    pub before: String,
    pub image_hash: Option<String>,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub swaps: Vec<Swap>,
}

/// A batch as the journal holds it.
#[derive(Debug, Clone)]
pub struct Pass {
    pub id: i64,
    pub kind: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub undoes: Option<i64>,
    pub written: i64,
}

#[derive(Debug)]
pub enum Error {
    /// A journal from a version we do not know. It is kept, and nothing is written.
    Foreign(i64),
    /// An entry, batch or undo that is not there.
    Unknown(String),
    Io(std::io::Error),
    Db(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Foreign(version) => write!(f, "the journal is version {version}, we speak {SCHEMA_VERSION}"),
            Error::Unknown(what) => write!(f, "{what}"),
            Error::Io(error) => write!(f, "{error}"),
            Error::Db(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Db(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Journal {
    connection: Connection,
    file: PathBuf,
}

impl Journal {
    /// Opens the journal, creating an empty one only when there is nothing there at all.
    pub fn open(file: &Path) -> Result<Journal> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let fresh = !file.exists();
        let connection = Connection::open(file)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 if fresh || is_empty(&connection)? => {
                connection.execute_batch(SCHEMA)?;
                connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            SCHEMA_VERSION => {}
            other => return Err(Error::Foreign(other)),
        }
        Ok(Journal {
            connection,
            file: file.to_path_buf(),
        })
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Opens a batch. Everything one pass of a tool writes belongs to one.
    pub fn start(&mut self, kind: Kind, undoes: Option<i64>) -> Result<i64> {
        self.connection.execute(
            "INSERT INTO batch (kind, started_at, undoes) VALUES (?1, ?2, ?3)",
            params![kind.as_str(), now(), undoes],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Writes down what is about to happen to one photo, and commits before returning.
    pub fn record(&mut self, batch: i64, entry: &Entry) -> Result<i64> {
        let change = self.connection.transaction()?;
        change.execute(
            "INSERT INTO entry (batch_id, rel_path, content_id, before, image_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![batch, entry.rel_path, entry.content_id, entry.before, entry.image_hash],
        )?;
        let id = change.last_insert_rowid();
        {
            let mut insert =
                change.prepare("INSERT INTO swap (entry_id, tag, key, old, new) VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for swap in &entry.swaps {
                insert.execute(params![id, swap.tag, swap.key, swap.old, swap.new])?;
            }
        }
        change.commit()?;
        Ok(id)
    }

    /// Notes what became of an entry, once the photo is either changed or left alone.
    pub fn settle(&mut self, entry: i64, outcome: &str, detail: Option<&str>) -> Result<()> {
        let touched = self.connection.execute(
            "UPDATE entry SET outcome = ?2, detail = ?3 WHERE id = ?1",
            params![entry, outcome, detail],
        )?;
        match touched {
            0 => Err(Error::Unknown(format!("no entry {entry} to settle"))),
            _ => Ok(()),
        }
    }

    pub fn finish(&mut self, batch: i64) -> Result<()> {
        self.connection
            .execute("UPDATE batch SET finished_at = ?2 WHERE id = ?1", params![batch, now()])?;
        Ok(())
    }

    /// Batches that were interrupted, and the entries in them that never got an outcome. A pass
    /// that died halfway leaves these behind, and they are what tells us so.
    pub fn unfinished(&self) -> Result<Vec<i64>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT b.id FROM batch b LEFT JOIN entry e ON e.batch_id = b.id
             WHERE b.finished_at IS NULL OR e.outcome IS NULL ORDER BY b.id",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<i64>>>()?)
    }

    pub fn passes(&self, limit: i64) -> Result<Vec<Pass>> {
        let mut statement = self.connection.prepare(
            "SELECT b.id, b.kind, b.started_at, b.finished_at, b.undoes,
                    (SELECT count(*) FROM entry e WHERE e.batch_id = b.id AND e.outcome = ?1)
             FROM batch b ORDER BY b.id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![WRITTEN, limit], |row| {
            Ok(Pass {
                id: row.get(0)?,
                kind: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                undoes: row.get(4)?,
                written: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Pass>>>()?)
    }

    pub fn pass(&self, batch: i64) -> Result<Pass> {
        self.passes(i64::MAX)?
            .into_iter()
            .find(|pass| pass.id == batch)
            .ok_or_else(|| Error::Unknown(format!("there is no batch {batch}")))
    }

    /// The entries of a batch that did change a photo, newest first: what an undo has to put back.
    pub fn written(&self, batch: i64) -> Result<Vec<Recorded>> {
        let mut statement = self.connection.prepare(
            "SELECT id, batch_id, rel_path, content_id, before, image_hash, outcome, detail
             FROM entry WHERE batch_id = ?1 AND outcome = ?2 ORDER BY id DESC",
        )?;
        let rows = statement.query_map(params![batch, WRITTEN], read_entry)?;
        let mut entries = rows.collect::<rusqlite::Result<Vec<Recorded>>>()?;
        for entry in &mut entries {
            entry.swaps = self.swaps(entry.id)?;
        }
        Ok(entries)
    }

    pub fn entries(&self, batch: i64) -> Result<Vec<Recorded>> {
        let mut statement = self.connection.prepare(
            "SELECT id, batch_id, rel_path, content_id, before, image_hash, outcome, detail
             FROM entry WHERE batch_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![batch], read_entry)?;
        let mut entries = rows.collect::<rusqlite::Result<Vec<Recorded>>>()?;
        for entry in &mut entries {
            entry.swaps = self.swaps(entry.id)?;
        }
        Ok(entries)
    }

    fn swaps(&self, entry: i64) -> Result<Vec<Swap>> {
        let mut statement = self
            .connection
            .prepare("SELECT tag, key, old, new FROM swap WHERE entry_id = ?1 ORDER BY rowid")?;
        let rows = statement.query_map(params![entry], |row| {
            Ok(Swap {
                tag: row.get(0)?,
                key: row.get(1)?,
                old: row.get(2)?,
                new: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Swap>>>()?)
    }

    /// The batch that already undid this one, if there is one.
    pub fn undo_of(&self, batch: i64) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id FROM batch WHERE undoes = ?1 ORDER BY id LIMIT 1",
                params![batch],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Recorded> {
    Ok(Recorded {
        id: row.get(0)?,
        batch_id: row.get(1)?,
        rel_path: row.get(2)?,
        content_id: row.get(3)?,
        before: row.get(4)?,
        image_hash: row.get(5)?,
        outcome: row.get(6)?,
        detail: row.get(7)?,
        swaps: Vec::new(),
    })
}

fn is_empty(connection: &Connection) -> rusqlite::Result<bool> {
    let tables: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| {
        row.get(0)
    })?;
    Ok(tables == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photomanager-journal-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("app.db")
    }

    fn entry(rel_path: &str) -> Entry {
        Entry {
            rel_path: rel_path.to_string(),
            content_id: "0123456789abcdef0123456789abcdef".to_string(),
            before: r#"{"XMP-xmp:Rating":2}"#.to_string(),
            image_hash: Some("sha".to_string()),
            swaps: vec![Swap {
                tag: "XMP-xmp:Rating".to_string(),
                key: "XMP-xmp:Rating".to_string(),
                old: Some("2".to_string()),
                new: Some("4".to_string()),
            }],
        }
    }

    #[test]
    fn an_entry_holds_both_sides_of_every_field() {
        let mut journal = Journal::open(&temp("both-sides")).unwrap();
        let batch = journal.start(Kind::Write, None).unwrap();
        let id = journal.record(batch, &entry("a.jpg")).unwrap();
        journal.settle(id, WRITTEN, None).unwrap();
        journal.finish(batch).unwrap();

        let written = journal.written(batch).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].rel_path, "a.jpg");
        assert_eq!(written[0].before, r#"{"XMP-xmp:Rating":2}"#);
        assert_eq!(written[0].swaps[0].old.as_deref(), Some("2"));
        assert_eq!(written[0].swaps[0].new.as_deref(), Some("4"));
        assert_eq!(journal.pass(batch).unwrap().written, 1);
    }

    #[test]
    fn an_entry_that_was_never_finished_is_still_there_and_findable() {
        let file = temp("unfinished");
        let mut journal = Journal::open(&file).unwrap();
        let batch = journal.start(Kind::Write, None).unwrap();
        journal.record(batch, &entry("a.jpg")).unwrap();
        drop(journal);

        let journal = Journal::open(&file).unwrap();
        assert_eq!(journal.unfinished().unwrap(), vec![batch]);
        let entries = journal.entries(batch).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].outcome, None);
        assert_eq!(entries[0].swaps.len(), 1);
    }

    #[test]
    fn a_finished_batch_is_not_unfinished() {
        let mut journal = Journal::open(&temp("finished")).unwrap();
        let batch = journal.start(Kind::Write, None).unwrap();
        let id = journal.record(batch, &entry("a.jpg")).unwrap();
        journal.settle(id, FAILED, Some("verification")).unwrap();
        journal.finish(batch).unwrap();
        assert!(journal.unfinished().unwrap().is_empty());
        assert!(journal.written(batch).unwrap().is_empty(), "a failure is not a write");
    }

    #[test]
    fn a_journal_from_another_version_is_kept_not_thrown_away() {
        let file = temp("foreign");
        let mut journal = Journal::open(&file).unwrap();
        let batch = journal.start(Kind::Write, None).unwrap();
        journal.record(batch, &entry("a.jpg")).unwrap();
        journal
            .connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(journal);

        let error = Journal::open(&file).unwrap_err();
        assert!(matches!(error, Error::Foreign(_)), "{error}");
        assert!(file.exists(), "the journal was deleted");

        let connection = Connection::open(&file).unwrap();
        let entries: i64 = connection
            .query_row("SELECT count(*) FROM entry", [], |row| row.get(0))
            .unwrap();
        assert_eq!(entries, 1, "an undo would have been lost");
    }

    #[test]
    fn an_undo_says_which_batch_it_undoes() {
        let mut journal = Journal::open(&temp("undo-of")).unwrap();
        let first = journal.start(Kind::Write, None).unwrap();
        assert_eq!(journal.undo_of(first).unwrap(), None);
        let second = journal.start(Kind::Undo, Some(first)).unwrap();
        assert_eq!(journal.undo_of(first).unwrap(), Some(second));
        assert_eq!(journal.pass(second).unwrap().kind, "undo");
    }

    #[test]
    fn settling_an_entry_that_is_not_there_is_an_error() {
        let mut journal = Journal::open(&temp("no-entry")).unwrap();
        assert!(journal.settle(404, WRITTEN, None).is_err());
    }
}
