//! The snapshot as SQLite: persons, assets and faces, and a line or two about the fetch. Its own
//! schema version; one of another version is no snapshot at all, and is fetched again.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{Error, Result};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE person (
    id     TEXT PRIMARY KEY,
    name   TEXT NOT NULL,
    hidden INTEGER NOT NULL
);
CREATE TABLE asset (
    id            TEXT PRIMARY KEY,
    original_path TEXT NOT NULL,
    rel_path      TEXT,
    offline       INTEGER NOT NULL,
    width         INTEGER,
    height        INTEGER,
    orientation   INTEGER
);
CREATE INDEX asset_path ON asset (rel_path);
CREATE TABLE face (
    id           TEXT PRIMARY KEY,
    asset_id     TEXT NOT NULL,
    person_id    TEXT,
    image_width  INTEGER NOT NULL,
    image_height INTEGER NOT NULL,
    x1 INTEGER NOT NULL,
    y1 INTEGER NOT NULL,
    x2 INTEGER NOT NULL,
    y2 INTEGER NOT NULL
);
CREATE INDEX face_asset ON face (asset_id);
CREATE TABLE about (
    name  TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// A person Immich knows. Only a named one that is not hidden is ever written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub hidden: bool,
}

impl Person {
    pub fn named(&self) -> bool {
        !self.hidden && !self.name.trim().is_empty()
    }
}

/// A photo as Immich knows it, and where it lies inside the library when it lies inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub id: String,
    pub original_path: String,
    pub rel_path: Option<String>,
    pub offline: bool,
    /// The stored size and the orientation, as Immich read them from the file.
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
}

/// One face box, in the pixels of the preview Immich found it on, which is turned the way the
/// photo is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Face {
    pub id: String,
    pub asset_id: String,
    pub person_id: Option<String>,
    pub image_width: i64,
    pub image_height: i64,
    pub x1: i64,
    pub y1: i64,
    pub x2: i64,
    pub y2: i64,
}

/// A snapshot, read only.
#[derive(Debug)]
pub struct Snapshot {
    connection: Connection,
}

impl Snapshot {
    /// `None` when there is no snapshot, or one of another version.
    pub fn open(file: &Path) -> Result<Option<Snapshot>> {
        if !file.exists() {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok((version == SCHEMA_VERSION).then_some(Snapshot { connection }))
    }

    pub fn people(&self) -> Result<Vec<Person>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, hidden FROM person ORDER BY name, id")?;
        let rows = statement.query_map([], |row| {
            Ok(Person {
                id: row.get(0)?,
                name: row.get(1)?,
                hidden: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Person>>>()?)
    }

    pub fn assets(&self) -> Result<Vec<Asset>> {
        let mut statement = self.connection.prepare(
            "SELECT id, original_path, rel_path, offline, width, height, orientation FROM asset ORDER BY original_path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Asset {
                id: row.get(0)?,
                original_path: row.get(1)?,
                rel_path: row.get(2)?,
                offline: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
                orientation: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Asset>>>()?)
    }

    pub fn faces(&self) -> Result<Vec<Face>> {
        let mut statement = self.connection.prepare(
            "SELECT id, asset_id, person_id, image_width, image_height, x1, y1, x2, y2 FROM face ORDER BY asset_id, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Face {
                id: row.get(0)?,
                asset_id: row.get(1)?,
                person_id: row.get(2)?,
                image_width: row.get(3)?,
                image_height: row.get(4)?,
                x1: row.get(5)?,
                y1: row.get(6)?,
                x2: row.get(7)?,
                y2: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<Face>>>()?)
    }

    /// A line about the fetch: `fetched-at`, `url`, `prefixes`.
    pub fn about(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row("SELECT value FROM about WHERE name = ?1", params![name], |row| {
                row.get(0)
            })
            .optional()?)
    }
}

/// Writes a whole snapshot next to `file` and puts it in place of the old one only once it is
/// complete, so a reader never sees half of one.
pub(super) fn keep(
    file: &Path,
    people: &[Person],
    assets: &[Asset],
    faces: &[Face],
    about: &BTreeMap<&str, String>,
) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::Store(error.to_string()))?;
    }
    let partial = file.with_extension("db.part");
    let _ = std::fs::remove_file(&partial);
    let written = write(&partial, people, assets, faces, about);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(error);
    }
    std::fs::rename(&partial, file).map_err(|error| Error::Store(error.to_string()))
}

fn write(
    file: &Path,
    people: &[Person],
    assets: &[Asset],
    faces: &[Face],
    about: &BTreeMap<&str, String>,
) -> Result<()> {
    let mut connection = Connection::open(file)?;
    connection.execute_batch(SCHEMA)?;
    let transaction = connection.transaction()?;
    {
        let mut person = transaction.prepare("INSERT OR REPLACE INTO person (id, name, hidden) VALUES (?1, ?2, ?3)")?;
        for one in people {
            person.execute(params![one.id, one.name, one.hidden])?;
        }
        let mut asset = transaction.prepare(
            "INSERT OR REPLACE INTO asset (id, original_path, rel_path, offline, width, height, orientation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for one in assets {
            asset.execute(params![
                one.id,
                one.original_path,
                one.rel_path,
                one.offline,
                one.width,
                one.height,
                one.orientation
            ])?;
        }
        let mut face = transaction.prepare(
            "INSERT OR REPLACE INTO face (id, asset_id, person_id, image_width, image_height, x1, y1, x2, y2)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for one in faces {
            face.execute(params![
                one.id,
                one.asset_id,
                one.person_id,
                one.image_width,
                one.image_height,
                one.x1,
                one.y1,
                one.x2,
                one.y2
            ])?;
        }
        let mut line = transaction.prepare("INSERT OR REPLACE INTO about (name, value) VALUES (?1, ?2)")?;
        for (name, value) in about {
            line.execute(params![name, value])?;
        }
    }
    transaction.commit()?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
