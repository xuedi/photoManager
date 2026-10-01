//! The few things the application has to remember that are not in the photos.
//!
//! They live in `app.db` and not in the cache, because they have to outlive a cache rebuild: the
//! cache is thrown away whenever it is convenient, and an acknowledgement the user gave once must
//! not be asked for again because of it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::{now, stamp};
use crate::layout::Layout;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS setting (
    name   TEXT PRIMARY KEY,
    value  TEXT NOT NULL,
    set_at TEXT NOT NULL
)";

/// The tables older versions kept a journal of every write in, for an undo. A change is not taken
/// back any more, the user's backup is the way back, so they are dropped on open.
const OLD_JOURNAL: [&str; 5] = ["swap", "relocated", "entry", "relocation", "batch"];

/// That the user said, once, that their photos are backed up somewhere else.
pub const BACKUP_ACKNOWLEDGED: &str = "backup-acknowledged";
/// Where they said the backup is. Help for them, never proof for us.
pub const BACKUP_LOCATION: &str = "backup-location";

/// Where Immich is. Its API key is never here: it is in the keyring.
pub const IMMICH_URL: &str = "immich-url";
/// Where the library lies inside Immich, when its own import paths are not to be used.
pub const IMMICH_PREFIX: &str = "immich-prefix";

/// How the folders above an event are laid out, as [`Layout`] keeps it as text.
pub const FOLDER_LAYOUT: &str = "folder-layout";

/// The tag roles the person kept, as [`crate::roles::Roles`] writes them. Without it the roles are
/// proposed from the roots the tags have.
pub const TAG_ROLES: &str = "tag-roles";

/// What older versions remembered for their tools and suggestions: answers, tag rules and the
/// dismissed suggestions. Moved out on open into [`LEFT_OVER_FILE`] beside `app.db`, for a person
/// to read; what they said that was written is in the photos, and what was not is found again.
const LEFT_OVER: &str = "name LIKE 'tool.%' OR name = 'dismissed-suggestions'";
pub const LEFT_OVER_FILE: &str = "old-tool-settings.json";

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
        move_out_left_over(&connection, file)?;
        drop_the_old_journal(&connection, file)?;
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

/// Writes what older versions remembered into a file beside `app.db`, and only once that file is
/// written takes it out of the table. A file there already is never overwritten.
fn move_out_left_over(connection: &Connection, file: &Path) -> Result<()> {
    let mut statement = connection.prepare(&format!(
        "SELECT name, value FROM setting WHERE {LEFT_OVER} ORDER BY name"
    ))?;
    let old: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_>>()?;
    if old.is_empty() {
        return Ok(());
    }
    let mut kept = file.with_file_name(LEFT_OVER_FILE);
    let mut number = 1;
    while kept.exists() {
        number += 1;
        kept = file.with_file_name(format!("old-tool-settings-{number}.json"));
    }
    let written: serde_json::Map<String, serde_json::Value> = old
        .into_iter()
        .map(|(name, value)| (name, serde_json::Value::from(value)))
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(written)).unwrap_or_default();
    if let Err(error) = std::fs::write(&kept, text) {
        tracing::warn!(%error, "the old tool settings could not be written out, so they stay");
        return Ok(());
    }
    connection.execute(&format!("DELETE FROM setting WHERE {LEFT_OVER}"), [])?;
    tracing::info!(file = %kept.display(), "old tool settings moved out");
    Ok(())
}

/// Drops the tables of the old journal. A copy of the whole file is made beside it first, as
/// SQLite sees it, named after the version the journal was; a copy there already is from a drop
/// that did not finish, and is kept. Something written by the old journal means the backup
/// question was answered then, so it is not asked again.
fn drop_the_old_journal(connection: &Connection, file: &Path) -> Result<()> {
    let tables: Vec<String> = {
        let mut statement = connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>>>()?
            .into_iter()
            .filter(|name| OLD_JOURNAL.contains(&name.as_str()))
            .collect()
    };
    if tables.is_empty() {
        return Ok(());
    }
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".v{version}"));
    let copy = file.with_file_name(name);
    if !copy.exists() {
        connection.execute("VACUUM INTO ?1", params![copy.to_string_lossy()])?;
    }

    let has = |table: &str| tables.iter().any(|name| name == table);
    let mut written = Vec::new();
    if has("entry") {
        written.push("EXISTS(SELECT 1 FROM entry WHERE outcome = 'written')");
    }
    if has("relocation") {
        written.push("EXISTS(SELECT 1 FROM relocation WHERE outcome = 'written')");
    }
    let ever_written = !written.is_empty()
        && connection.query_row(&format!("SELECT {}", written.join(" OR ")), [], |row| {
            row.get::<_, bool>(0)
        })?;

    let change = connection.unchecked_transaction()?;
    if ever_written {
        change.execute(
            "INSERT OR IGNORE INTO setting (name, value, set_at) VALUES (?1, 'yes', ?2)",
            params![BACKUP_ACKNOWLEDGED, now()],
        )?;
    }
    for table in OLD_JOURNAL.iter().filter(|table| has(table)) {
        change.execute_batch(&format!("DROP TABLE {table}"))?;
    }
    change.pragma_update(None, "user_version", 0)?;
    change.commit()?;
    tracing::info!(copy = %copy.display(), "the old journal was dropped");
    Ok(())
}

/// The folder layout the user chose, or the default when they chose none or it no longer reads.
pub fn layout(settings: &Settings) -> Layout {
    match settings.get(FOLDER_LAYOUT) {
        Ok(Some(text)) => Layout::read(&text).unwrap_or_else(|why| {
            tracing::warn!(%why, "the kept folder layout does not read, using the default");
            Layout::default()
        }),
        _ => Layout::default(),
    }
}

/// The tag roles the person kept, if they kept any that still read.
pub fn roles(settings: &Settings) -> Option<crate::roles::Roles> {
    match settings.get(TAG_ROLES) {
        Ok(Some(text)) => crate::roles::Roles::read(&text)
            .map_err(|why| tracing::warn!(%why, "the kept tag roles do not read, using the proposal"))
            .ok(),
        _ => None,
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
/// have acknowledged it, the question is settled for good.
pub fn must_ask(settings: &Settings) -> Result<bool> {
    Ok(settings.get(BACKUP_ACKNOWLEDGED)?.is_none())
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
    fn the_layout_is_the_default_until_one_is_kept() {
        let file = temp("layout").join("app.db");
        let mut settings = Settings::open(&file).unwrap();
        assert_eq!(layout(&settings), Layout::default());
        settings.put(FOLDER_LAYOUT, "year/country").unwrap();
        assert_eq!(layout(&settings).to_string(), "year/country");
        settings.put(FOLDER_LAYOUT, "").unwrap();
        assert!(layout(&settings).levels.is_empty(), "events only is a layout too");
        settings.put(FOLDER_LAYOUT, "country?/city?").unwrap();
        assert_eq!(layout(&settings), Layout::default(), "one that does not read");
    }

    #[test]
    fn the_old_journal_is_copied_and_dropped_and_what_it_wrote_settles_the_question() {
        for (name, written) in [("old-journal-written", true), ("old-journal-unwritten", false)] {
            let file = temp(name).join("app.db");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let old = Connection::open(&file).unwrap();
            old.execute_batch(
                "CREATE TABLE batch (id INTEGER PRIMARY KEY, kind TEXT);
                 CREATE TABLE entry (id INTEGER PRIMARY KEY, batch_id INTEGER, outcome TEXT);
                 CREATE TABLE swap (entry_id INTEGER, tag TEXT);
                 CREATE TABLE relocation (id INTEGER PRIMARY KEY, batch_id INTEGER, outcome TEXT);
                 CREATE TABLE relocated (relocation_id INTEGER, rel_path TEXT);
                 INSERT INTO batch VALUES (1, 'write');
                 PRAGMA user_version = 3;",
            )
            .unwrap();
            let outcome = if written { "written" } else { "refused" };
            old.execute("INSERT INTO entry VALUES (1, 1, ?1)", params![outcome])
                .unwrap();
            drop(old);

            let settings = Settings::open(&file).unwrap();
            assert_eq!(must_ask(&settings).unwrap(), !written, "{name}");
            let left: i64 = settings
                .connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name IN ('batch', 'entry', 'swap', 'relocation', 'relocated')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(left, 0, "{name}");
            let copy = Connection::open(file.with_file_name("app.db.v3")).unwrap();
            let kept: i64 = copy
                .query_row("SELECT count(*) FROM batch", [], |row| row.get(0))
                .unwrap();
            assert_eq!(kept, 1, "the copy holds the journal as it was");
            drop(settings);
            assert!(Settings::open(&file).is_ok(), "and it opens again with nothing to drop");
        }
    }

    #[test]
    fn the_acknowledgement_is_asked_for_once() {
        let file = temp("ask").join("app.db");
        let mut settings = Settings::open(&file).unwrap();
        assert!(must_ask(&settings).unwrap());

        acknowledge(&mut settings, None).unwrap();
        assert!(!must_ask(&settings).unwrap());
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
    fn what_older_tools_remembered_is_moved_out_on_open() {
        let file = temp("left-over").join("data/app.db");
        let mut settings = Settings::open(&file).unwrap();
        settings.put("tool.gps-from-places-tag", "{}").unwrap();
        settings.put("dismissed-suggestions", "[\"tags:flat\"]").unwrap();
        settings.put(FOLDER_LAYOUT, "country/city").unwrap();
        drop(settings);

        let settings = Settings::open(&file).unwrap();
        assert_eq!(settings.get("tool.gps-from-places-tag").unwrap(), None);
        assert_eq!(settings.get("dismissed-suggestions").unwrap(), None);
        assert_eq!(settings.get(FOLDER_LAYOUT).unwrap().as_deref(), Some("country/city"));
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file.with_file_name(LEFT_OVER_FILE)).unwrap()).unwrap();
        assert_eq!(kept["tool.gps-from-places-tag"], "{}");
        assert_eq!(kept["dismissed-suggestions"], "[\"tags:flat\"]");
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
        assert!(!must_ask(&settings).unwrap());
        assert_eq!(
            settings.get(BACKUP_LOCATION).unwrap().as_deref(),
            Some(dir.display().to_string().as_str())
        );
    }
}
