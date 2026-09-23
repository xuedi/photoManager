//! Places without the network: the GeoNames dumps turned into one small database that answers
//! "where is this name" and "what is at these coordinates".
//!
//! The data is from [GeoNames](https://www.geonames.org/), licensed under CC BY 4.0. Like the
//! photo cache this file is disposable: it is built from the dumps and can be made again.

pub mod download;
pub mod import;
pub mod lookup;
pub mod reverse;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use unicode_normalization::UnicodeNormalization;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE country (
    code       TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    continent  TEXT,
    geoname_id INTEGER
);

CREATE TABLE area (
    code    TEXT PRIMARY KEY,
    country TEXT NOT NULL,
    name    TEXT NOT NULL
);

CREATE TABLE place (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    country    TEXT NOT NULL,
    area       TEXT,
    feature    TEXT NOT NULL,
    population INTEGER NOT NULL,
    lat        REAL NOT NULL,
    lon        REAL NOT NULL
);
CREATE INDEX place_country ON place (country);

CREATE TABLE name (
    id       INTEGER PRIMARY KEY,
    place_id INTEGER NOT NULL,
    folded   TEXT NOT NULL,
    own      INTEGER NOT NULL
);
CREATE INDEX name_folded ON name (folded);
CREATE INDEX name_place ON name (place_id);

-- The search index keeps no copy of the names: it reads them back from `name`.
CREATE VIRTUAL TABLE name_search USING fts5(
    folded, content='name', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE place_at USING rtree(id, min_lat, max_lat, min_lon, max_lon);

CREATE TABLE ring (
    id      INTEGER PRIMARY KEY,
    country TEXT NOT NULL,
    shape   INTEGER NOT NULL,
    hole    INTEGER NOT NULL,
    points  BLOB NOT NULL
);
CREATE VIRTUAL TABLE ring_at USING rtree(id, min_lat, max_lat, min_lon, max_lon);
";

#[derive(Debug)]
pub enum Error {
    /// A dump the import needs is not in the directory.
    Missing(PathBuf),
    /// A dump is there but not what we expect.
    Malformed(String),
    Io(std::io::Error),
    Db(rusqlite::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing(path) => write!(f, "{} is not there", path.display()),
            Error::Malformed(why) => write!(f, "{why}"),
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

/// What the database holds, for the dashboard and for the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub countries: i64,
    pub areas: i64,
    pub places: i64,
    pub names: i64,
    pub rings: i64,
}

#[derive(Debug)]
pub struct Geo {
    connection: Connection,
    file: PathBuf,
}

impl Geo {
    /// Opens the place database, starting an empty one when it is from another schema or damaged.
    pub fn open(file: &Path) -> Result<Geo> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match Geo::open_existing(file) {
            Ok(Some(geo)) => return Ok(geo),
            Ok(None) => tracing::info!(geo = %file.display(), "place data from another version, starting over"),
            Err(error) => tracing::warn!(%error, "place data unreadable, starting over"),
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", file.display()));
        }
        Geo::create(file)
    }

    fn open_existing(file: &Path) -> Result<Option<Geo>> {
        if !file.exists() {
            return Ok(None);
        }
        let connection = Connection::open(file)?;
        prepare(&connection)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Ok(None);
        }
        connection.query_row("SELECT count(*) FROM place", [], |row| row.get::<_, i64>(0))?;
        // Place data imported before the index existed gains it here, in a moment, instead of
        // being thrown away: a typo search measures every name of a place by it.
        connection.execute_batch("CREATE INDEX IF NOT EXISTS name_place ON name (place_id)")?;
        Ok(Some(Geo {
            connection,
            file: file.to_path_buf(),
        }))
    }

    /// A second, read-only look at the place data someone else has open, for questions asked off
    /// the main thread. `None` when there is none of this version to look at.
    pub fn read_only(file: &Path) -> Result<Option<Geo>> {
        if !file.exists() {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(Geo {
            connection,
            file: file.to_path_buf(),
        }))
    }

    fn create(file: &Path) -> Result<Geo> {
        let connection = Connection::open(file)?;
        prepare(&connection)?;
        connection.execute_batch(SCHEMA)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Geo {
            connection,
            file: file.to_path_buf(),
        })
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Whether there is anything to look up.
    pub fn is_filled(&self) -> bool {
        self.counts().map(|counts| counts.places > 0).unwrap_or(false)
    }

    pub fn counts(&self) -> Result<Counts> {
        let count = |table: &str| -> Result<i64> {
            Ok(self
                .connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0))?)
        };
        Ok(Counts {
            countries: count("country")?,
            areas: count("area")?,
            places: count("place")?,
            names: count("name")?,
            rings: count("ring")?,
        })
    }

    /// When the dumps this was built from were last changed, as the files said.
    pub fn dump_date(&self) -> Option<String> {
        self.meta("dump date")
    }

    pub fn imported_at(&self) -> Option<String> {
        self.meta("imported at")
    }

    fn meta(&self, key: &str) -> Option<String> {
        self.connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0))
            .optional()
            .ok()
            .flatten()
    }
}

fn prepare(connection: &Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.busy_timeout(std::time::Duration::from_secs(5))
}

/// One spelling for comparing: lower case, no accents, no punctuation, single spaces. What a
/// person types, what a folder is called and what GeoNames holds all have to meet somewhere.
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    let mut space = false;
    for character in text.nfd() {
        let replacement = match character {
            'ß' => "ss",
            'ø' | 'Ø' => "o",
            'đ' | 'Đ' => "d",
            'ł' | 'Ł' => "l",
            'æ' | 'Æ' => "ae",
            'œ' | 'Œ' => "oe",
            'þ' | 'Þ' => "th",
            'ð' | 'Ð' => "d",
            _ => "",
        };
        if !replacement.is_empty() {
            folded.push_str(replacement);
            space = false;
            continue;
        }
        if is_combining(character) {
            continue;
        }
        if character.is_alphanumeric() {
            folded.extend(character.to_lowercase());
            space = false;
        } else if !folded.is_empty() && !space {
            folded.push(' ');
            space = true;
        }
    }
    folded.trim_end().to_string()
}

fn is_combining(character: char) -> bool {
    matches!(character as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x20d0..=0x20ff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photomanager-geo-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("geo.db")
    }

    #[test]
    fn folding_brings_spellings_together() {
        assert_eq!(fold("Zürich"), "zurich");
        assert_eq!(fold("Köln"), "koln");
        assert_eq!(fold("Aarhus"), "aarhus");
        assert_eq!(fold("Århus"), "arhus");
        assert_eq!(fold("Malmö"), "malmo");
        assert_eq!(fold("København"), "kobenhavn");
        assert_eq!(fold("Weil am Rhein"), "weil am rhein");
        assert_eq!(fold("Saint-Louis"), "saint louis");
        assert_eq!(fold("  ATHENS  "), "athens");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("北京"), "北京", "a name we cannot fold is left as it is");
        assert_eq!(fold(""), "");
        assert_eq!(fold("..."), "");
    }

    #[test]
    fn creates_reopens_and_starts_over_when_the_schema_moved_on() {
        let file = temp("open");
        let geo = Geo::open(&file).unwrap();
        assert!(!geo.is_filled());
        assert_eq!(geo.counts().unwrap(), Counts::default());
        assert_eq!(geo.dump_date(), None);
        geo.connection
            .execute(
                "INSERT INTO meta (key, value) VALUES ('dump date', '2026-09-22 00:00:00')",
                [],
            )
            .unwrap();
        drop(geo);

        let geo = Geo::open(&file).unwrap();
        assert_eq!(geo.dump_date().as_deref(), Some("2026-09-22 00:00:00"));
        geo.connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(geo);

        let geo = Geo::open(&file).unwrap();
        assert_eq!(geo.dump_date(), None, "a foreign schema is discarded");
    }

    #[test]
    fn place_data_from_before_the_place_index_gains_it_on_open() {
        let file = temp("index");
        let geo = Geo::open(&file).unwrap();
        geo.connection.execute_batch("DROP INDEX name_place").unwrap();
        geo.connection
            .execute("INSERT INTO meta (key, value) VALUES ('kept', 'yes')", [])
            .unwrap();
        drop(geo);

        let geo = Geo::open(&file).unwrap();
        let indexed: i64 = geo
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'name_place'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);
        assert_eq!(geo.meta("kept").as_deref(), Some("yes"), "nothing was thrown away");
    }

    #[test]
    fn replaces_a_damaged_file_instead_of_failing() {
        let file = temp("damaged");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"this is not a database").unwrap();
        assert!(!Geo::open(&file).unwrap().is_filled());
    }
}
