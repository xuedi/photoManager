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

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE batch (
    id          INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    undoes      INTEGER REFERENCES batch (id),
    title       TEXT,
    tool        TEXT
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

/// From version 1: a batch learns what ran it. Only columns are added, so every row a version-1
/// journal holds reads the same afterwards, and a batch from before it has no title.
const TO_2: &str = "
ALTER TABLE batch ADD COLUMN title TEXT;
ALTER TABLE batch ADD COLUMN tool TEXT;
";

/// What a batch from before batches had names is called.
pub const EARLIER: &str = "Earlier change";

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
    /// What ran it, as a person reads it. `None` for a batch from before batches had names.
    pub title: Option<String>,
    /// The key of the tool that ran it, if a tool did.
    pub tool: Option<String>,
}

impl Pass {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(EARLIER)
    }
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
            1 => migrate(&connection, file)?,
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
    pub fn start(&mut self, kind: Kind, undoes: Option<i64>, title: &str, tool: Option<&str>) -> Result<i64> {
        self.connection.execute(
            "INSERT INTO batch (kind, started_at, undoes, title, tool) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind.as_str(), now(), undoes, title, tool],
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

    /// Newest first.
    pub fn passes(&self, limit: i64) -> Result<Vec<Pass>> {
        self.passes_from(0, limit)
    }

    /// Newest first, leaving out the `skip` newest: the history asks for them a page at a time.
    pub fn passes_from(&self, skip: i64, limit: i64) -> Result<Vec<Pass>> {
        let mut statement = self
            .connection
            .prepare(&format!("{PASS} ORDER BY b.id DESC LIMIT ?2 OFFSET ?3"))?;
        let rows = statement.query_map(params![WRITTEN, limit, skip], read_pass)?;
        Ok(rows.collect::<rusqlite::Result<Vec<Pass>>>()?)
    }

    pub fn pass(&self, batch: i64) -> Result<Pass> {
        self.connection
            .query_row(&format!("{PASS} WHERE b.id = ?2"), params![WRITTEN, batch], read_pass)
            .optional()?
            .ok_or_else(|| Error::Unknown(format!("there is no batch {batch}")))
    }

    /// For the queries that are about the whole history rather than one batch.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
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

    /// Whether anything was ever written to a photo. The very first write of all is a moment the
    /// application asks about, and this is what answers it.
    pub fn ever_written(&self) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM entry WHERE outcome = ?1)",
            params![WRITTEN],
            |row| row.get(0),
        )?)
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

const PASS: &str = "SELECT b.id, b.kind, b.started_at, b.finished_at, b.undoes,
        (SELECT count(*) FROM entry e WHERE e.batch_id = b.id AND e.outcome = ?1), b.title, b.tool
    FROM batch b";

fn read_pass(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pass> {
    Ok(Pass {
        id: row.get(0)?,
        kind: row.get(1)?,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        undoes: row.get(4)?,
        written: row.get(5)?,
        title: row.get(6)?,
        tool: row.get(7)?,
    })
}

/// Takes a version-1 journal to version 2. A copy of the whole file is made beside it first, as
/// SQLite sees it, so what the write-ahead log still holds is in it too. A copy already there is
/// from a migration that did not finish, of the same version-1 journal, and is kept as it is.
fn migrate(connection: &Connection, file: &Path) -> Result<()> {
    let copy = beside(file, ".v1");
    if !copy.exists() {
        let target = copy
            .to_str()
            .ok_or_else(|| Error::Unknown(format!("{} is not a name SQLite can take", copy.display())))?;
        connection.execute("VACUUM INTO ?1", params![target])?;
    }
    let change = connection.unchecked_transaction()?;
    change.execute_batch(TO_2)?;
    change.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    change.commit()?;
    tracing::info!(copy = %copy.display(), "the journal was migrated to version {SCHEMA_VERSION}");
    Ok(())
}

fn beside(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    file.with_file_name(name)
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

/// Whether the journal's own tables are there yet. The settings sit in the same file and may have
/// made it first, so the question is about `batch`, not about the file being bare.
fn is_empty(connection: &Connection) -> rusqlite::Result<bool> {
    let tables: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'batch'",
        [],
        |row| row.get(0),
    )?;
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
        let batch = journal.start(Kind::Write, None, "Rate", None).unwrap();
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
        let batch = journal.start(Kind::Write, None, "Rate", None).unwrap();
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
        let batch = journal.start(Kind::Write, None, "Rate", None).unwrap();
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
        let batch = journal.start(Kind::Write, None, "Rate", None).unwrap();
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
        let first = journal.start(Kind::Write, None, "Rate", None).unwrap();
        assert_eq!(journal.undo_of(first).unwrap(), None);
        let second = journal.start(Kind::Undo, Some(first), "Take back: Rate", None).unwrap();
        assert_eq!(journal.undo_of(first).unwrap(), Some(second));
        assert_eq!(journal.pass(second).unwrap().kind, "undo");
    }

    /// The journal as version 1 wrote it, before batches had names.
    const SCHEMA_V1: &str = "
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
    PRAGMA user_version = 1;
    ";

    /// Two passes, the second undoing the first, the way version 1 left them.
    pub(crate) fn version_one(file: &Path) {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let connection = Connection::open(file).unwrap();
        connection.pragma_update(None, "journal_mode", "WAL").unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection
            .execute_batch(
                "INSERT INTO batch VALUES (1, 'write', '2026-09-20 10:00:00', '2026-09-20 10:00:01', NULL);
                 INSERT INTO batch VALUES (2, 'undo', '2026-09-20 11:00:00', '2026-09-20 11:00:01', 1);
                 INSERT INTO entry VALUES (1, 1, 'a.jpg', 'c1', '{}', 'h1', 'written', NULL);
                 INSERT INTO entry VALUES (2, 1, 'b.jpg', 'c2', '{}', 'h2', 'refused', 'drifted');
                 INSERT INTO entry VALUES (3, 2, 'a.jpg', 'c1', '{}', 'h1', 'written', NULL);
                 INSERT INTO swap VALUES (1, 'XMP-xmp:Rating', 'XMP-xmp:Rating', NULL, '3');
                 INSERT INTO swap VALUES (3, 'XMP-xmp:Rating', 'XMP-xmp:Rating', '3', NULL);",
            )
            .unwrap();
    }

    fn count(file: &Path, table: &str) -> i64 {
        Connection::open(file)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_version_one_journal_is_copied_then_learns_names_and_keeps_every_row() {
        let file = temp("migrate");
        version_one(&file);

        let mut journal = Journal::open(&file).unwrap();
        let version: i64 = journal
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let passes = journal.passes(10).unwrap();
        assert_eq!(passes.iter().map(|pass| pass.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(passes[1].written, 1);
        assert_eq!(passes[0].undoes, Some(1));
        assert!(passes.iter().all(|pass| pass.title.is_none() && pass.tool.is_none()));
        assert_eq!(passes[1].title(), EARLIER);
        assert_eq!(journal.entries(1).unwrap().len(), 2);
        assert_eq!(journal.written(1).unwrap()[0].swaps[0].new.as_deref(), Some("3"));
        assert_eq!(journal.undo_of(1).unwrap(), Some(2));

        let copy = file.with_file_name("app.db.v1");
        assert!(copy.exists(), "no copy was left beside it");
        let old: i64 = Connection::open(&copy)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(old, 1, "the copy is of the journal as it was");
        for (table, rows) in [("batch", 2), ("entry", 3), ("swap", 2)] {
            assert_eq!(count(&copy, table), rows, "{table} in the copy");
            assert_eq!(count(&file, table), rows, "{table} after the migration");
        }

        let third = journal.start(Kind::Write, None, "Rate", Some("demo")).unwrap();
        assert_eq!(journal.pass(third).unwrap().title(), "Rate");
        drop(journal);
        assert!(Journal::open(&file).is_ok(), "a migrated journal opens as it is");
    }

    #[test]
    fn a_pass_is_written_with_its_title_and_read_back() {
        let mut journal = Journal::open(&temp("titled")).unwrap();
        let batch = journal
            .start(Kind::Write, None, "Set a rating", Some("rating"))
            .unwrap();
        let pass = journal.pass(batch).unwrap();
        assert_eq!(pass.title.as_deref(), Some("Set a rating"));
        assert_eq!(pass.tool.as_deref(), Some("rating"));
        assert!(journal.pass(batch + 1).is_err());
    }

    #[test]
    fn passes_come_a_page_at_a_time_newest_first() {
        let mut journal = Journal::open(&temp("pages")).unwrap();
        let ids: Vec<i64> = (0..5)
            .map(|at| journal.start(Kind::Write, None, &format!("pass {at}"), None).unwrap())
            .collect();
        let first: Vec<i64> = journal.passes_from(0, 2).unwrap().iter().map(|pass| pass.id).collect();
        let second: Vec<i64> = journal.passes_from(2, 2).unwrap().iter().map(|pass| pass.id).collect();
        assert_eq!(first, [ids[4], ids[3]]);
        assert_eq!(second, [ids[2], ids[1]]);
    }

    #[test]
    fn settling_an_entry_that_is_not_there_is_an_error() {
        let mut journal = Journal::open(&temp("no-entry")).unwrap();
        assert!(journal.settle(404, WRITTEN, None).is_err());
    }
}
