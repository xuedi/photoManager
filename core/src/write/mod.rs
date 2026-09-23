//! The one place that changes what a photo says about itself.
//!
//! One write is: copy, write the copy, prove the copy, then rename. ExifTool is never pointed at a
//! photo in the library. The engine copies the file to a temporary name beside it, lets ExifTool
//! rewrite the copy, checks the copy is what we asked for, gives it the original's mode and only
//! then renames it over the original. If anything is off the temporary file goes and the library
//! never changed.
//!
//! Proving a write is three questions, and all of them have to answer yes: is our content id the
//! same, is ExifTool's `ImageDataHash` the same, and does every field we set read back as what we
//! asked for. The first two are what "never lose a byte of the original image data" means in code.
//!
//! Before any of it, the whole intent is written down in the journal and committed. Nothing is
//! written to a photo that is not already in the journal, so every change can be taken back.
//!
//! The modification time is deliberately not preserved: Nextcloud and Immich both notice a changed
//! file only by its mtime, and a write nothing notices is worse than no write at all.

pub mod change;
pub mod tool;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{Map, Value};

pub use change::{Assign, Change, Face, Faces, Field, Gps, Place, Taken};
use tool::Tool;

use crate::cache::Cache;
use crate::identity::content_id;
use crate::journal::{self, Journal, Kind};

#[derive(Debug)]
pub enum Error {
    /// ExifTool is not there, or could not be started.
    NoTool(String),
    /// ExifTool said nothing for this many seconds.
    Stuck(u64),
    /// The process died. Sending the command again is the driver's own business; this is what is
    /// left when that did not help either.
    ToolGone(String),
    /// This one photo will not be touched, and why. Never fatal to a pass.
    Refusing(String),
    Journal(journal::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoTool(why) => write!(f, "exiftool: {why}"),
            Error::Stuck(seconds) => write!(f, "exiftool said nothing for {seconds} seconds"),
            Error::ToolGone(why) => write!(f, "exiftool went away: {why}"),
            Error::Refusing(why) => write!(f, "{why}"),
            Error::Journal(error) => write!(f, "the journal: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<journal::Error> for Error {
    fn from(error: journal::Error) -> Error {
        Error::Journal(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What became of one photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Written,
    /// It already said what the change wanted, so it was not touched.
    Skipped,
    /// We would not try, and why.
    Refused(String),
    /// We tried, it did not work out, and the photo is as it was.
    Failed(String),
}

impl Outcome {
    fn settlement(&self) -> (&'static str, Option<&str>) {
        match self {
            Outcome::Written => (journal::WRITTEN, None),
            Outcome::Skipped => (journal::REFUSED, Some("there was nothing left to do")),
            Outcome::Refused(why) => (journal::REFUSED, Some(why)),
            Outcome::Failed(why) => (journal::FAILED, Some(why)),
        }
    }
}

/// One photo and what it should say.
#[derive(Debug, Clone)]
pub struct Target {
    pub path: PathBuf,
    /// The content id the change was built against. A photo whose image data moved is refused.
    pub content_id: String,
    pub change: Change,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub batch: i64,
    pub written: usize,
    pub skipped: usize,
    pub refused: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub seconds: u64,
    /// Every photo and what became of it, in the order they were done.
    pub outcomes: Vec<(String, Outcome)>,
}

impl Summary {
    fn note(&mut self, rel_path: &str, outcome: Outcome) {
        match &outcome {
            Outcome::Written => self.written += 1,
            Outcome::Skipped => self.skipped += 1,
            Outcome::Refused(_) => self.refused += 1,
            Outcome::Failed(_) => self.failed += 1,
        }
        self.outcomes.push((rel_path.to_string(), outcome));
    }

    pub fn photos(&self) -> usize {
        self.written + self.skipped + self.refused + self.failed
    }
}

/// What a photo says, as ExifTool reads it back.
#[derive(Debug, Clone)]
struct Snapshot {
    fields: Map<String, Value>,
    image_hash: String,
}

/// Either a fresh intent, or the values a batch is putting back.
enum Wish<'a> {
    New(&'a Change),
    /// Put these values back, but only if the photo still says what the write left in it.
    Back {
        want: Vec<Assign>,
        expect: Vec<Assign>,
    },
}

#[derive(Debug)]
pub struct Engine {
    tool: Tool,
    library: PathBuf,
}

impl Engine {
    /// The library root is the only place the engine will ever write.
    pub fn new(library: &Path) -> Result<Engine> {
        let library = library
            .canonicalize()
            .map_err(|error| Error::Refusing(format!("{}: {error}", library.display())))?;
        Ok(Engine {
            tool: Tool::new(),
            library,
        })
    }

    pub fn library(&self) -> &Path {
        &self.library
    }

    /// One photo, in a batch of its own. A batch is the unit an undo works on, so a single write
    /// gets one too.
    pub fn write_one(&mut self, journal: &mut Journal, cache: &mut Cache, target: &Target) -> Result<Outcome> {
        let batch = journal.start(Kind::Write, None)?;
        let outcome = self.one(
            journal,
            cache,
            batch,
            &target.path,
            &target.content_id,
            Wish::New(&target.change),
        );
        journal.finish(batch)?;
        outcome
    }

    /// Photos one after another on one process. A refusal on one does not stop the others.
    pub fn write(
        &mut self,
        journal: &mut Journal,
        cache: &mut Cache,
        targets: &[Target],
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        let started = Instant::now();
        let batch = journal.start(Kind::Write, None)?;
        let mut summary = Summary {
            batch,
            ..Summary::default()
        };

        for (done, target) in targets.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            let outcome = self.one(
                journal,
                cache,
                batch,
                &target.path,
                &target.content_id,
                Wish::New(&target.change),
            )?;
            summary.note(&self.name(&target.path), outcome);
            progress(done + 1, targets.len());
        }

        journal.finish(batch)?;
        summary.seconds = started.elapsed().as_secs();
        tracing::info!(
            batch,
            written = summary.written,
            skipped = summary.skipped,
            refused = summary.refused,
            failed = summary.failed,
            cancelled = summary.cancelled,
            "write pass done"
        );
        Ok(summary)
    }

    /// Puts a batch back: the old value of every field it changed, and away with the fields that
    /// were not there before. Through the same engine, with the same proof, journaled itself.
    pub fn undo(
        &mut self,
        journal: &mut Journal,
        cache: &mut Cache,
        undoes: i64,
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        let started = Instant::now();
        if let Some(already) = journal.undo_of(undoes)? {
            return Err(Error::Refusing(format!(
                "batch {undoes} was already undone by {already}"
            )));
        }
        let entries = journal.written(undoes)?;
        if entries.is_empty() {
            return Err(Error::Refusing(format!("batch {undoes} changed no photo")));
        }

        let batch = journal.start(Kind::Undo, Some(undoes))?;
        let mut summary = Summary {
            batch,
            ..Summary::default()
        };
        for (done, entry) in entries.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            let path = self.library.join(&entry.rel_path);
            let mut want = Vec::new();
            let mut expect = Vec::new();
            for swap in &entry.swaps {
                want.push(Assign {
                    tag: swap.tag.clone(),
                    key: swap.key.clone(),
                    value: stored(swap.old.as_deref()),
                });
                expect.push(Assign {
                    tag: swap.tag.clone(),
                    key: swap.key.clone(),
                    value: stored(swap.new.as_deref()),
                });
            }
            let outcome = self.one(
                journal,
                cache,
                batch,
                &path,
                &entry.content_id,
                Wish::Back { want, expect },
            )?;
            summary.note(&entry.rel_path, outcome);
            progress(done + 1, entries.len());
        }

        journal.finish(batch)?;
        summary.seconds = started.elapsed().as_secs();
        tracing::info!(batch, undoes, written = summary.written, "undo pass done");
        Ok(summary)
    }

    /// A refusal is about this photo alone, so it never comes back as an error.
    fn one(
        &mut self,
        journal: &mut Journal,
        cache: &mut Cache,
        batch: i64,
        path: &Path,
        expected: &str,
        wish: Wish<'_>,
    ) -> Result<Outcome> {
        match self.attempt(journal, cache, batch, path, expected, wish) {
            Err(Error::Refusing(why)) => {
                tracing::debug!(photo = %path.display(), why, "not written");
                Ok(Outcome::Refused(why))
            }
            other => other,
        }
    }

    fn attempt(
        &mut self,
        journal: &mut Journal,
        cache: &mut Cache,
        batch: i64,
        path: &Path,
        expected: &str,
        wish: Wish<'_>,
    ) -> Result<Outcome> {
        let rel_path = self.inside(path)?;
        let bytes = std::fs::read(path).map_err(|error| Error::Refusing(format!("cannot be read: {error}")))?;
        let found = content_id(&bytes).ok_or_else(|| Error::Refusing("not a JPEG we understand".to_string()))?;
        if found != expected {
            return Err(Error::Refusing(
                "its image data is not what this change was built against".to_string(),
            ));
        }

        let before = self.look(path)?;
        let mut want = match &wish {
            Wish::New(change) => change.assigns().map_err(Error::Refusing)?,
            Wish::Back { want, expect } => {
                if let Some(drifted) = expect.iter().find(|assign| !change::settled(assign, &before.fields)) {
                    return Err(Error::Refusing(format!(
                        "{} no longer says what the write left in it",
                        drifted.tag
                    )));
                }
                want.clone()
            }
        };
        want.retain(|assign| !change::settled(assign, &before.fields));
        if want.is_empty() {
            return Ok(Outcome::Skipped);
        }

        let entry = journal::Entry {
            rel_path: rel_path.clone(),
            content_id: found.clone(),
            before: Value::Object(before.fields.clone()).to_string(),
            image_hash: Some(before.image_hash.clone()),
            swaps: want
                .iter()
                .map(|assign| journal::Swap {
                    tag: assign.tag.clone(),
                    key: assign.key.clone(),
                    old: as_stored(before.fields.get(&assign.key)),
                    new: as_stored(Some(&assign.value)),
                })
                .collect(),
        };
        let recorded = journal.record(batch, &entry)?;

        let temp = beside(path)?;
        let attempt = self.write_copy(&temp, path, &want, &before, &found);
        if !matches!(attempt, Ok(None)) {
            let _ = std::fs::remove_file(&temp);
        }
        let outcome = match attempt {
            Ok(None) => Outcome::Written,
            Ok(Some(why)) => Outcome::Failed(why),
            Err(error) => {
                let (name, detail) = (journal::FAILED, error.to_string());
                let _ = journal.settle(recorded, name, Some(&detail));
                return Err(error);
            }
        };

        let (name, detail) = outcome.settlement();
        journal.settle(recorded, name, detail)?;
        if outcome == Outcome::Written {
            if let Err(error) = cache.forget(std::slice::from_ref(&rel_path)) {
                tracing::warn!(%error, photo = rel_path, "the cache row stayed behind");
            }
            tracing::info!(photo = rel_path, fields = want.len(), "written");
        }
        Ok(outcome)
    }

    /// Writes the copy and proves it. `Ok(None)` means the original has been replaced.
    fn write_copy(
        &mut self,
        temp: &Path,
        original: &Path,
        want: &[Assign],
        before: &Snapshot,
        content: &str,
    ) -> Result<Option<String>> {
        if let Err(error) = std::fs::copy(original, temp) {
            return Ok(Some(format!("no copy could be made beside it: {error}")));
        }

        let mut args = vec![
            "-overwrite_original".to_string(),
            "-charset".to_string(),
            "iptc=UTF8".to_string(),
        ];
        args.extend(change::arguments(want));
        args.push(tool::as_argument(temp)?);
        let reply = self.tool.run(&args)?;
        if !reply.updated() {
            return Ok(Some(reply.complaint()));
        }

        if let Some(why) = self.unproven(temp, want, before, content)? {
            return Ok(Some(why));
        }

        let mode = match std::fs::metadata(original) {
            Ok(metadata) => metadata.permissions(),
            Err(error) => return Ok(Some(format!("its mode could not be read: {error}"))),
        };
        if let Err(error) = std::fs::set_permissions(temp, mode) {
            return Ok(Some(format!("the copy could not be given its mode: {error}")));
        }
        if let Err(error) = std::fs::File::open(temp).and_then(|file| file.sync_all()) {
            return Ok(Some(format!("the copy did not reach the disk: {error}")));
        }
        if let Err(error) = std::fs::rename(temp, original) {
            return Ok(Some(format!("the copy could not take its place: {error}")));
        }
        if let Some(parent) = original.parent() {
            let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
        }
        Ok(None)
    }

    /// The three questions. `Ok(None)` means the copy is what we asked for.
    fn unproven(&mut self, temp: &Path, want: &[Assign], before: &Snapshot, content: &str) -> Result<Option<String>> {
        let bytes = match std::fs::read(temp) {
            Ok(bytes) => bytes,
            Err(error) => return Ok(Some(format!("the copy could not be read back: {error}"))),
        };
        match content_id(&bytes) {
            Some(found) if found == content => {}
            Some(_) => return Ok(Some("the image data moved".to_string())),
            None => return Ok(Some("the copy is no longer a JPEG we understand".to_string())),
        }

        let after = self.look(temp)?;
        if after.image_hash != before.image_hash {
            return Ok(Some("exiftool says the image data moved".to_string()));
        }
        let missed: Vec<&str> = want
            .iter()
            .filter(|assign| !change::settled(assign, &after.fields))
            .map(|assign| assign.tag.as_str())
            .collect();
        match missed.is_empty() {
            true => Ok(None),
            false => Ok(Some(format!("{} did not read back", missed.join(", ")))),
        }
    }

    /// Everything the file says, plus ExifTool's hash of the image data alone.
    fn look(&mut self, path: &Path) -> Result<Snapshot> {
        let args = [
            "-j",
            "-n",
            "-G1",
            "-struct",
            "-charset",
            "iptc=UTF8",
            "-api",
            "ImageHashType=SHA256",
            "-ImageDataHash",
            "-All",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .chain([tool::as_argument(path)?])
        .collect::<Vec<String>>();

        let reply = self.tool.run(&args)?;
        let unreadable = || Error::Refusing(format!("exiftool cannot read it: {}", reply.complaint()));
        let parsed: Value = serde_json::from_str(reply.out.trim()).map_err(|_| unreadable())?;
        let fields = parsed
            .get(0)
            .and_then(|entry| entry.as_object())
            .ok_or_else(unreadable)?
            .clone();
        let image_hash = fields
            .get("File:ImageDataHash")
            .and_then(|value| value.as_str())
            .map(String::from)
            .ok_or_else(|| Error::Refusing("exiftool cannot hash its image data".to_string()))?;
        Ok(Snapshot { fields, image_hash })
    }

    /// The path inside the library, or a refusal. Nothing outside it is ever written.
    fn inside(&self, path: &Path) -> Result<String> {
        let real = path
            .canonicalize()
            .map_err(|error| Error::Refusing(format!("{}: {error}", path.display())))?;
        let rel_path = real
            .strip_prefix(&self.library)
            .map_err(|_| Error::Refusing(format!("{} is outside the library", real.display())))?;
        rel_path
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| Error::Refusing(format!("{} is not a name we can keep", rel_path.display())))
    }

    fn name(&self, path: &Path) -> String {
        self.inside(path).unwrap_or_else(|_| path.display().to_string())
    }
}

/// A temporary name beside the original, so the rename that follows is on one filesystem and
/// therefore atomic. Hidden and not photo-named, so a scan in between does not pick it up.
fn beside(original: &Path) -> Result<PathBuf> {
    let parent = original
        .parent()
        .ok_or_else(|| Error::Refusing(format!("{} has nowhere to sit beside", original.display())))?;
    let name = original
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Refusing(format!("{} has no name we can copy", original.display())))?;
    Ok(parent.join(format!(".{name}.writing-{}", std::process::id())))
}

/// A value as the journal keeps it: the JSON ExifTool gave us, or nothing for a tag that is not
/// there and is not to be there.
fn as_stored(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.to_string()),
    }
}

fn stored(text: Option<&str>) -> Value {
    text.and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
