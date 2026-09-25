//! The scan result, in SQLite. Disposable: on a schema change or damage it is thrown away and
//! filled again from the photos.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::layout::Placement;
use crate::metadata::{Metadata, Regions};
use crate::scan::Issue;

const SCHEMA_VERSION: i64 = 6;

/// SQLite takes a few hundred parameters happily; a library's worth of paths is asked for in
/// chunks of this size.
const CHUNK: usize = 500;

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
    event_dir   TEXT,
    taken_at    TEXT,
    taken_offset TEXT,
    xmp_taken_at TEXT,
    gps_lat     REAL,
    gps_lon     REAL,
    gps_method  TEXT,
    camera_make TEXT,
    camera_model TEXT,
    orientation INTEGER,
    rating      INTEGER,
    width       INTEGER,
    height      INTEGER,
    location_city TEXT,
    tags_untidy INTEGER NOT NULL DEFAULT 0,
    regions     TEXT,
    raw         TEXT NOT NULL
);
CREATE INDEX photo_content ON photo (content_id);
CREATE INDEX photo_event ON photo (country, event_text);
CREATE INDEX photo_event_dir ON photo (event_dir);
-- Everything the survey asks of a photo, so it never has to read the rows with their raw JSON.
CREATE INDEX photo_survey ON photo (country, event_dir, taken_at, event_year, event_month, event_day,
    gps_lat, gps_lon, location_city, camera_model, size);

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i64,
    pub inode: u64,
}

/// A photo as the thumbnail pass needs it: where it is, what it is, how it sits.
#[derive(Debug, Clone)]
pub struct Picture {
    pub rel_path: String,
    pub content_id: String,
    pub orientation: Option<i64>,
}

/// What a photo says, as a change set needs it: the fields a person thinks in, without the raw
/// JSON, so a whole library of them fits in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Said {
    pub taken_at: Option<String>,
    pub taken_offset: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// How the position was worked out, when the photo says; see [`crate::write::change::is_derived`].
    pub gps_method: Option<String>,
    pub rating: Option<i64>,
    pub tags: Vec<String>,
    /// Its tag fields disagree or hold leftovers; see [`crate::metadata::Metadata::tags_untidy`].
    pub tags_untidy: bool,
    /// The face regions and persons it names.
    pub regions: Option<Regions>,
}

/// A photo as a change set needs it: where it is, how big it is, what it is, and what it says.
#[derive(Debug, Clone)]
pub struct Stated {
    pub rel_path: String,
    pub size: u64,
    pub content_id: Option<String>,
    pub said: Said,
}

/// How a photo is stored: the size of its pixels before any turn, and the turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shape {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
}

/// A photo as the date tools need it: its date, where it was, which camera, and what its folder
/// says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dated {
    pub rel_path: String,
    pub taken_at: Option<String>,
    pub taken_offset: Option<String>,
    pub gps: Option<(f64, f64)>,
    pub camera: Option<String>,
    pub country: Option<String>,
    pub event_dir: Option<String>,
    pub event_year: Option<i64>,
    pub event_month: Option<i64>,
    pub event_day: Option<i64>,
}

/// A photo as the tag tools need it: its tags, whether its tag fields are tidy, and what its
/// date, its place words and its folder say, which the generated tags are made from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tagged {
    pub rel_path: String,
    pub tags: Vec<String>,
    pub untidy: bool,
    pub taken_at: Option<String>,
    /// The city and the country the place words name.
    pub city: Option<String>,
    pub country_named: Option<String>,
    /// The country folder.
    pub country: Option<String>,
    pub event_dir: Option<String>,
    pub event_year: Option<i64>,
    pub event_name: Option<String>,
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

    /// A second, read-only look at a cache someone else has open, for questions asked off the
    /// main thread while the cache itself stays where it is. `None` when there is no cache of
    /// this version to look at.
    pub fn read_only(file: &Path) -> Result<Option<Cache>> {
        if !file.exists() {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(Cache {
            connection,
            file: file.to_path_buf(),
        }))
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
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
        self.connection
            .query_row("SELECT count(DISTINCT event_dir) FROM photo", [], |row| row.get(0))
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

    /// Every photo we could make a thumbnail of.
    pub fn pictures(&self) -> Result<Vec<Picture>> {
        let mut statement = self
            .connection
            .prepare("SELECT rel_path, content_id, orientation FROM photo WHERE content_id IS NOT NULL")?;
        let rows = statement.query_map([], |row| {
            Ok(Picture {
                rel_path: row.get(0)?,
                content_id: row.get(1)?,
                orientation: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn content_ids(&self) -> Result<std::collections::HashSet<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT DISTINCT content_id FROM photo WHERE content_id IS NOT NULL")?;
        let rows = statement.query_map([], |row| row.get(0))?;
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

    /// What these photos say, by path: everything a change set needs to know without opening a
    /// single file. A photo the cache does not know is simply not in the result.
    pub fn stated(&self, rel_paths: &[String]) -> Result<std::collections::HashMap<String, Stated>> {
        let mut found = std::collections::HashMap::new();
        let mut names: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
        for chunk in rel_paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT id, rel_path, size, content_id, taken_at, taken_offset, gps_lat, gps_lon, rating,
                    gps_method, tags_untidy, regions
                 FROM photo WHERE rel_path IN ({})",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    Stated {
                        rel_path: row.get(1)?,
                        size: row.get::<_, i64>(2)? as u64,
                        content_id: row.get(3)?,
                        said: Said {
                            taken_at: row.get(4)?,
                            taken_offset: row.get(5)?,
                            gps_lat: row.get(6)?,
                            gps_lon: row.get(7)?,
                            rating: row.get(8)?,
                            gps_method: row.get(9)?,
                            tags: Vec::new(),
                            tags_untidy: row.get(10)?,
                            regions: row.get::<_, Option<String>>(11)?.as_deref().and_then(Regions::read),
                        },
                    },
                ))
            })?;
            for row in rows {
                let (id, stated) = row?;
                names.insert(id, stated.rel_path.clone());
                found.insert(stated.rel_path.clone(), stated);
            }
        }

        let ids: Vec<i64> = names.keys().copied().collect();
        for chunk in ids.chunks(CHUNK) {
            let sql = format!(
                "SELECT photo_id, path FROM tag WHERE photo_id IN ({}) ORDER BY path",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, path) = row?;
                if let Some(name) = names.get(&id)
                    && let Some(stated) = found.get_mut(name)
                {
                    stated.said.tags.push(path);
                }
            }
        }
        Ok(found)
    }

    /// The photos among these whose location text says anything, in any part and either spelling.
    pub fn with_place_text(&self, rel_paths: &[String]) -> Result<std::collections::HashSet<String>> {
        let said: Vec<String> = crate::details::PLACE_FIELDS
            .iter()
            .flatten()
            .map(|name| format!("coalesce(trim(json_extract(raw, '$.\"{name}\"')), '') != ''"))
            .collect();
        let mut found = std::collections::HashSet::new();
        for chunk in rel_paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT rel_path FROM photo WHERE rel_path IN ({}) AND ({})",
                holes(chunk.len()),
                said.join(" OR ")
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| row.get::<_, String>(0))?;
            for row in rows {
                found.insert(row?);
            }
        }
        Ok(found)
    }

    /// How each of these photos is stored: its size in pixels and its orientation.
    pub fn shapes(&self, rel_paths: &[String]) -> Result<std::collections::HashMap<String, Shape>> {
        let mut found = std::collections::HashMap::new();
        for chunk in rel_paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT rel_path, width, height, orientation FROM photo WHERE rel_path IN ({})",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    Shape {
                        width: row.get(1)?,
                        height: row.get(2)?,
                        orientation: row.get(3)?,
                    },
                ))
            })?;
            for row in rows {
                let (rel_path, shape) = row?;
                found.insert(rel_path, shape);
            }
        }
        Ok(found)
    }

    /// The event folder of each of these photos that is in one.
    pub fn event_dirs(&self, rel_paths: &[String]) -> Result<std::collections::HashMap<String, String>> {
        let mut found = std::collections::HashMap::new();
        for chunk in rel_paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT rel_path, event_dir FROM photo WHERE rel_path IN ({}) AND event_dir IS NOT NULL",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (rel_path, event_dir) = row?;
                found.insert(rel_path, event_dir);
            }
        }
        Ok(found)
    }

    /// Every position the photos of these event folders have, by folder: the whole event, not
    /// only the part of it a scope holds.
    pub fn positions_in(&self, event_dirs: &[String]) -> Result<Vec<(String, f64, f64)>> {
        let mut found = Vec::new();
        for chunk in event_dirs.chunks(CHUNK) {
            let sql = format!(
                "SELECT event_dir, gps_lat, gps_lon FROM photo
                 WHERE event_dir IN ({}) AND gps_lat IS NOT NULL AND gps_lon IS NOT NULL
                 ORDER BY rel_path",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
            })?;
            for row in rows {
                found.push(row?);
            }
        }
        Ok(found)
    }

    /// The date side of these photos, in path order.
    pub fn dated(&self, rel_paths: &[String]) -> Result<Vec<Dated>> {
        self.dated_by("rel_path", rel_paths)
    }

    /// The date side of every photo of these event folders, in path order: the whole event, not
    /// only the part of it a scope holds.
    pub fn dated_in(&self, event_dirs: &[String]) -> Result<Vec<Dated>> {
        self.dated_by("event_dir", event_dirs)
    }

    fn dated_by(&self, column: &str, keys: &[String]) -> Result<Vec<Dated>> {
        let mut found = Vec::new();
        for chunk in keys.chunks(CHUNK) {
            let sql = format!(
                "SELECT rel_path, taken_at, taken_offset, gps_lat, gps_lon, camera_model, country, event_dir,
                    event_year, event_month, event_day
                 FROM photo WHERE {column} IN ({})",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                let lat: Option<f64> = row.get(3)?;
                let lon: Option<f64> = row.get(4)?;
                let camera: Option<String> = row.get(5)?;
                Ok(Dated {
                    rel_path: row.get(0)?,
                    taken_at: row.get(1)?,
                    taken_offset: row
                        .get::<_, Option<String>>(2)?
                        .filter(|offset| !offset.trim().is_empty()),
                    gps: lat.zip(lon),
                    camera: camera
                        .map(|camera| camera.trim().to_string())
                        .filter(|camera| !camera.is_empty()),
                    country: row.get(6)?,
                    event_dir: row.get(7)?,
                    event_year: row.get(8)?,
                    event_month: row.get(9)?,
                    event_day: row.get(10)?,
                })
            })?;
            for row in rows {
                found.push(row?);
            }
        }
        found.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(found)
    }

    /// The tag side of these photos, in path order.
    pub fn tagged(&self, rel_paths: &[String]) -> Result<Vec<Tagged>> {
        let country = crate::details::PLACE_FIELDS[2]
            .iter()
            .map(|name| format!("nullif(trim(json_extract(raw, '$.\"{name}\"')), '')"))
            .collect::<Vec<String>>()
            .join(", ");
        let mut found = Vec::new();
        let mut ids: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        for chunk in rel_paths.chunks(CHUNK) {
            let sql = format!(
                "SELECT id, rel_path, tags_untidy, taken_at, nullif(trim(location_city), ''), coalesce({country}),
                    country, event_dir, event_year, event_name
                 FROM photo WHERE rel_path IN ({})",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    Tagged {
                        rel_path: row.get(1)?,
                        tags: Vec::new(),
                        untidy: row.get(2)?,
                        taken_at: row.get(3)?,
                        city: row.get(4)?,
                        country_named: row.get(5)?,
                        country: row.get(6)?,
                        event_dir: row.get(7)?,
                        event_year: row.get(8)?,
                        event_name: row.get(9)?,
                    },
                ))
            })?;
            for row in rows {
                let (id, tagged) = row?;
                ids.insert(id, found.len());
                found.push(tagged);
            }
        }
        let keys: Vec<i64> = ids.keys().copied().collect();
        for chunk in keys.chunks(CHUNK) {
            let sql = format!(
                "SELECT photo_id, path FROM tag WHERE photo_id IN ({}) ORDER BY path",
                holes(chunk.len())
            );
            let mut statement = self.connection.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, path) = row?;
                if let Some(at) = ids.get(&id) {
                    found[*at].tags.push(path);
                }
            }
        }
        found.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(found)
    }

    /// The tags of every photo that has any, one list per photo.
    pub fn tag_sets(&self) -> Result<Vec<Vec<String>>> {
        let mut statement = self
            .connection
            .prepare("SELECT photo_id, path FROM tag ORDER BY photo_id, path")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        let mut sets: Vec<Vec<String>> = Vec::new();
        let mut last = None;
        for row in rows {
            let (id, path) = row?;
            if last != Some(id) {
                sets.push(Vec::new());
                last = Some(id);
            }
            sets.last_mut().expect("just pushed").push(path);
        }
        Ok(sets)
    }

    pub fn transaction(&mut self) -> Result<Writer<'_>> {
        Ok(Writer {
            transaction: self.connection.transaction()?,
        })
    }

    /// A folder or a photo moved inside the library: every row under `from` follows it to `to`,
    /// keeping everything read from the files. Returns how many rows moved.
    pub fn relocate(&mut self, from: &str, to: &str) -> Result<usize> {
        let under = format!("{from}/");
        let paths: Vec<String> = {
            let mut statement = self
                .connection
                .prepare("SELECT rel_path FROM photo WHERE rel_path = ?1 OR substr(rel_path, 1, length(?2)) = ?2")?;
            let rows = statement.query_map(params![from, under], |row| row.get(0))?;
            rows.collect::<Result<Vec<String>>>()?
        };
        let writer = self.transaction()?;
        for path in &paths {
            let moved = match path.strip_prefix(&under) {
                Some(rest) => format!("{to}/{rest}"),
                None => to.to_string(),
            };
            writer.relocate(path, &moved)?;
        }
        writer.commit()?;
        Ok(paths.len())
    }

    /// The paths of the photos with this image data.
    pub fn paths_of(&self, content_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT rel_path FROM photo WHERE content_id = ?1 ORDER BY rel_path")?;
        let rows = statement.query_map(params![content_id], |row| row.get(0))?;
        rows.collect()
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

fn holes(count: usize) -> String {
    std::iter::repeat_n("?", count).collect::<Vec<&str>>().join(",")
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

    /// One photo's row follows its file to another path: what its folders say is read again from
    /// the new one, and so is whether it fits the convention.
    pub fn relocate(&self, from: &str, to: &str) -> Result<()> {
        let placement = Placement::parse(to);
        let id: Option<i64> = self
            .transaction
            .query_row("SELECT id FROM photo WHERE rel_path = ?1", params![from], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(id) = id else {
            return Ok(());
        };
        self.forget(to)?;
        self.transaction.execute(
            "UPDATE photo SET rel_path = ?2, country = ?3, city = ?4, event_text = ?5, event_year = ?6,
                event_month = ?7, event_day = ?8, event_name = ?9, sub_path = ?10, event_dir = ?11
             WHERE id = ?1",
            params![
                id,
                to,
                placement.country,
                placement.city,
                placement.event_text,
                placement.event_year,
                placement.event_month,
                placement.event_day,
                placement.event_name,
                placement.sub_path,
                placement.event_dir,
            ],
        )?;
        self.transaction.execute(
            "DELETE FROM issue WHERE rel_path = ?1 AND kind = ?2",
            params![from, crate::scan::IssueKind::OffConvention.as_str()],
        )?;
        self.transaction
            .execute("UPDATE issue SET rel_path = ?2 WHERE rel_path = ?1", params![from, to])?;
        if !placement.fits() {
            self.add_issue(
                &Issue {
                    rel_path: to.to_string(),
                    kind: crate::scan::IssueKind::OffConvention,
                    detail: Some(placement.fit.as_str().to_string()),
                },
                Some(id),
            )?;
        }
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
                width, height, raw, event_dir, location_city, gps_method, tags_untidy, regions)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)",
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
                placement.event_dir,
                metadata.location_city,
                metadata.gps_method,
                metadata.tags_untidy,
                metadata.regions.as_ref().map(Regions::written),
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
    fn a_read_only_look_sees_the_rows_and_cannot_change_them() {
        let file = temp("read-only");
        assert!(
            Cache::read_only(&file).unwrap().is_none(),
            "no cache, nothing to look at"
        );
        let mut cache = Cache::open(&file).unwrap();
        put_one(&mut cache, "China/2006-09-00 Besuch/P1.JPG");

        let look = Cache::read_only(&file).unwrap().expect("a cache of this version");
        assert_eq!(look.photo_count().unwrap(), 1);
        assert_eq!(look.event_count().unwrap(), 1);
        assert!(look.connection.execute("DELETE FROM photo", []).is_err());
        assert_eq!(cache.photo_count().unwrap(), 1);
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
