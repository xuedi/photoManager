//! The few things the application has to remember that are not in the photos.
//!
//! They live in a table of their own in `app.db`, next to the [journal](crate::journal), because
//! like the journal they have to outlive a cache rebuild: the cache is thrown away whenever it is
//! convenient, and an acknowledgement the user gave once must not be asked for again because of
//! it. The table is created if it is not there and the journal's schema version is left alone, so
//! either of the two can open the file first.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::{now, stamp};
use crate::journal::{self, Journal};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS setting (
    name   TEXT PRIMARY KEY,
    value  TEXT NOT NULL,
    set_at TEXT NOT NULL
)";

/// That the user said, once, that their photos are backed up somewhere else.
pub const BACKUP_ACKNOWLEDGED: &str = "backup-acknowledged";
/// Where they said the backup is. Help for them, never proof for us.
pub const BACKUP_LOCATION: &str = "backup-location";

pub type Result<T> = rusqlite::Result<T>;

#[derive(Debug)]
pub struct Settings {
    connection: Connection,
    file: PathBuf,
}

impl Settings {
    pub fn open(file: &Path) -> Result<Settings> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(14), Some(error.to_string()))
            })?;
        }
        let connection = Connection::open(file)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(SCHEMA)?;
        Ok(Settings {
            connection,
            file: file.to_path_buf(),
        })
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn get(&self, name: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT value FROM setting WHERE name = ?1", params![name], |row| {
                row.get(0)
            })
            .optional()
    }

    /// When a setting was last written, in the one date format.
    pub fn set_at(&self, name: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT set_at FROM setting WHERE name = ?1", params![name], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn put(&mut self, name: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO setting (name, value, set_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET value = ?2, set_at = ?3",
            params![name, value, now()],
        )?;
        Ok(())
    }

    pub fn forget(&mut self, name: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM setting WHERE name = ?1", params![name])?;
        Ok(())
    }
}

/// What can be said about a place the user named as their backup. All of it is help for a person
/// reading a dialog, and none of it is proof that a single photo is safe anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub path: PathBuf,
    pub there: bool,
    pub empty: bool,
    /// When anything in it was last changed, in the one date format.
    pub last_touched: Option<String>,
}

impl Backup {
    /// One sentence for the dialog.
    pub fn tells(&self) -> String {
        let where_it_is = self.path.display();
        if !self.there {
            return format!("{where_it_is} is not there.");
        }
        if self.empty {
            return format!("{where_it_is} is empty.");
        }
        match &self.last_touched {
            Some(when) => format!("{where_it_is} was last changed on {when}."),
            None => format!("{where_it_is} is there and is not empty."),
        }
    }
}

/// Looks at a backup location: is it there, is there anything in it, and when was it last touched.
pub fn look_at(path: &Path) -> Backup {
    let mut backup = Backup {
        path: path.to_path_buf(),
        there: path.is_dir(),
        empty: true,
        last_touched: None,
    };
    let Ok(entries) = std::fs::read_dir(path) else {
        return backup;
    };
    let mut newest = None;
    for entry in entries.flatten() {
        backup.empty = false;
        let touched = entry.metadata().and_then(|data| data.modified()).ok();
        newest = newest.max(touched);
    }
    backup.last_touched = newest.map(stamp);
    backup
}

/// Whether the user still has to be asked before anything is ever written to a photo. Once they
/// have acknowledged it, or once something has been written, the question is settled for good.
pub fn must_ask(journal: &Journal, settings: &Settings) -> journal::Result<bool> {
    Ok(settings.get(BACKUP_ACKNOWLEDGED)?.is_none() && !journal.ever_written()?)
}

/// Records the acknowledgement, and the backup location if the user named one.
pub fn acknowledge(settings: &mut Settings, backup: Option<&Path>) -> Result<()> {
    settings.put(BACKUP_ACKNOWLEDGED, "yes")?;
    match backup {
        Some(path) => settings.put(BACKUP_LOCATION, &path.display().to_string()),
        None => settings.forget(BACKUP_LOCATION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photomanager-settings-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_setting_is_kept_and_can_be_taken_back() {
        let file = temp("kept").join("app.db");
        let mut settings = Settings::open(&file).unwrap();
        assert_eq!(settings.get("nothing").unwrap(), None);

        settings.put("a", "1").unwrap();
        assert_eq!(settings.get("a").unwrap().as_deref(), Some("1"));
        assert_eq!(settings.set_at("a").unwrap().map(|when| when.len()), Some(19));

        settings.put("a", "2").unwrap();
        assert_eq!(settings.get("a").unwrap().as_deref(), Some("2"), "it is one row");

        settings.forget("a").unwrap();
        assert_eq!(settings.get("a").unwrap(), None);
    }

    #[test]
    fn the_settings_and_the_journal_share_one_file_in_either_order() {
        for settings_first in [true, false] {
            let file = temp(&format!("shared-{settings_first}")).join("app.db");
            if settings_first {
                Settings::open(&file).unwrap().put("a", "1").unwrap();
                Journal::open(&file).unwrap();
            } else {
                Journal::open(&file).unwrap();
                Settings::open(&file).unwrap().put("a", "1").unwrap();
            }
            assert_eq!(Settings::open(&file).unwrap().get("a").unwrap().as_deref(), Some("1"));
            assert!(Journal::open(&file).is_ok(), "the journal still opens its own tables");
        }
    }

    #[test]
    fn the_acknowledgement_is_asked_for_once() {
        let file = temp("ask").join("app.db");
        let journal = Journal::open(&file).unwrap();
        let mut settings = Settings::open(&file).unwrap();
        assert!(must_ask(&journal, &settings).unwrap());

        acknowledge(&mut settings, None).unwrap();
        assert!(!must_ask(&journal, &settings).unwrap());
        assert_eq!(settings.get(BACKUP_LOCATION).unwrap(), None);
    }

    #[test]
    fn a_named_backup_is_described_and_never_believed() {
        let dir = temp("backup");
        let missing = look_at(&dir.join("not-there"));
        assert!(!missing.there);
        assert!(missing.tells().ends_with("is not there."));

        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let looked = look_at(&empty);
        assert!(looked.there && looked.empty);
        assert_eq!(looked.last_touched, None);
        assert!(looked.tells().ends_with("is empty."));

        std::fs::write(empty.join("a-photo.jpg"), b"not really").unwrap();
        let filled = look_at(&empty);
        assert!(!filled.empty);
        assert_eq!(filled.last_touched.as_ref().map(|when| when.len()), Some(19));
        assert!(filled.tells().contains("was last changed on"));
    }

    #[test]
    fn the_acknowledgement_outlives_the_cache() {
        let dir = temp("outlives");
        let file = dir.join("data/app.db");
        let mut settings = Settings::open(&file).unwrap();
        acknowledge(&mut settings, Some(&dir)).unwrap();
        drop(settings);

        let cache = crate::cache::Cache::open(&dir.join("cache/cache.db")).unwrap();
        cache.rebuild().unwrap();

        let settings = Settings::open(&file).unwrap();
        let journal = Journal::open(&file).unwrap();
        assert!(!must_ask(&journal, &settings).unwrap());
        assert_eq!(
            settings.get(BACKUP_LOCATION).unwrap().as_deref(),
            Some(dir.display().to_string().as_str())
        );
    }
}
