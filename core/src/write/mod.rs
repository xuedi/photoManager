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
//! Some cameras wrote a maker note ExifTool doubts, and it rewrites such a photo only when told to
//! ignore that minor problem. The engine never tells it on its own: the photo comes back
//! [`Outcome::Doubted`], and only a write the user asked to go ahead anyway passes `-m`. That
//! write has a fourth question to answer: does every value of the maker note, and every preview
//! and thumbnail picture in the file, read back exactly as before, and is the maker note as long
//! as it was. ExifTool drops a part of a maker note it cannot read rather than move it, so a
//! maker note that would come out shorter is no write: not a byte of it is lost.
//!
//! The modification time is deliberately not preserved: Nextcloud and Immich both notice a changed
//! file only by its mtime, and a write nothing notices is worse than no write at all.

pub mod change;
pub mod relocate;
pub mod tool;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{Map, Value};

pub use change::{Assign, Assignment, Change, Face, Faces, Field, Gps, Place, Taken};
pub use relocate::Move;
use tool::Tool;

use crate::cache::Cache;
use crate::identity::content_id;

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
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoTool(why) => write!(f, "exiftool: {why}"),
            Error::Stuck(seconds) => write!(f, "exiftool said nothing for {seconds} seconds"),
            Error::ToolGone(why) => write!(f, "exiftool went away: {why}"),
            Error::Refusing(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Error {}

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
    /// ExifTool would write it only if told to ignore a minor problem with the camera's maker
    /// note, and why. The photo is as it was; it can be written anyway, on the user's word.
    Doubted(String),
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
    pub written: usize,
    pub skipped: usize,
    pub refused: usize,
    pub failed: usize,
    pub doubted: usize,
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
            Outcome::Doubted(_) => self.doubted += 1,
        }
        self.outcomes.push((rel_path.to_string(), outcome));
    }

    pub fn photos(&self) -> usize {
        self.written + self.skipped + self.refused + self.failed + self.doubted
    }

    /// The photos it doubted, each with why.
    pub fn doubts(&self) -> impl Iterator<Item = (&str, &str)> {
        self.outcomes.iter().filter_map(|(rel_path, outcome)| match outcome {
            Outcome::Doubted(why) => Some((rel_path.as_str(), why.as_str())),
            _ => None,
        })
    }

    /// This pass with what a second one over some of its photos made of them.
    pub fn and_then(mut self, again: Summary) -> Summary {
        let mut redone: Vec<(String, Outcome)> = Vec::new();
        for (rel_path, outcome) in self.outcomes.drain(..) {
            let later = again.outcomes.iter().find(|(path, _)| *path == rel_path);
            redone.push(later.cloned().unwrap_or((rel_path, outcome)));
        }
        let mut summary = Summary {
            cancelled: self.cancelled || again.cancelled,
            seconds: self.seconds + again.seconds,
            ..Summary::default()
        };
        for (rel_path, outcome) in redone {
            summary.note(&rel_path, outcome);
        }
        summary
    }
}

/// Whether ExifTool's complaint is a minor problem with a maker note, the only one a photo is
/// ever written anyway for.
fn doubted(complaint: &str) -> bool {
    let lower = complaint.to_lowercase();
    complaint.starts_with("[minor]") && (lower.contains("makernote") || lower.contains("maker note"))
}

/// ExifTool's complaint without the `[minor]` and the file it names.
fn reason(complaint: &str) -> String {
    let said = complaint.trim_start_matches("[minor]").trim();
    said.rsplit_once(" - ")
        .map_or(said, |(said, _)| said)
        .trim()
        .to_string()
}

/// Every maker note value and every embedded picture that is not where it is but what it is:
/// the tags that only say where a block sits move with it.
fn kept(fields: &Map<String, Value>) -> Map<String, Value> {
    fields
        .iter()
        .filter(|(key, _)| {
            let tag = key.rsplit(':').next().unwrap_or(key);
            !(tag.ends_with("Start") || tag.ends_with("Offset") || tag.ends_with("Offsets"))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// How many bytes a base64 text holds.
fn decoded_length(encoded: &str) -> usize {
    let encoded = encoded.trim();
    let padding = encoded.chars().rev().take_while(|letter| *letter == '=').count();
    (encoded.len() / 4 * 3).saturating_sub(padding)
}

/// What differs between two readings of the maker note and the pictures, by tag.
fn differs(before: &Map<String, Value>, after: &Map<String, Value>) -> Vec<String> {
    let (before, after) = (kept(before), kept(after));
    let mut tags: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

/// What a photo says, as ExifTool reads it back.
#[derive(Debug, Clone)]
struct Snapshot {
    fields: Map<String, Value>,
    image_hash: String,
}

/// Everything a write needs to know before it touches anything: where the photo is, what it says
/// now, and the assignments that are not settled yet. A dry run stops right here.
#[derive(Debug)]
struct Intent {
    rel_path: String,
    content_id: String,
    before: Snapshot,
    want: Vec<Assign>,
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

    /// One photo on its own.
    pub fn write_one(&mut self, cache: &mut Cache, target: &Target) -> Result<Outcome> {
        self.one(cache, &target.path, &target.content_id, &target.change, false)
    }

    /// Photos one after another on one process. A refusal on one does not stop the others.
    /// `title` names the pass in the log.
    pub fn write(
        &mut self,
        cache: &mut Cache,
        title: &str,
        targets: &[Target],
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        self.pass(cache, title, targets, false, progress, cancel)
    }

    /// Photos the user said to write anyway: ExifTool ignores a minor problem with the maker
    /// note, and each copy has to prove that the maker note and the pictures in it read back as
    /// they were.
    pub fn write_anyway(
        &mut self,
        cache: &mut Cache,
        title: &str,
        targets: &[Target],
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        self.pass(cache, title, targets, true, progress, cancel)
    }

    fn pass(
        &mut self,
        cache: &mut Cache,
        title: &str,
        targets: &[Target],
        anyway: bool,
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        let started = Instant::now();
        let mut summary = Summary::default();

        for (done, target) in targets.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            let outcome = self.one(cache, &target.path, &target.content_id, &target.change, anyway)?;
            summary.note(&self.name(&target.path), outcome);
            progress(done + 1, targets.len());
        }

        summary.seconds = started.elapsed().as_secs();
        tracing::info!(
            title,
            written = summary.written,
            skipped = summary.skipped,
            refused = summary.refused,
            failed = summary.failed,
            doubted = summary.doubted,
            anyway,
            cancelled = summary.cancelled,
            "write pass done"
        );
        Ok(summary)
    }

    /// A refusal is about this photo alone, so it never comes back as an error.
    fn one(
        &mut self,
        cache: &mut Cache,
        path: &Path,
        expected: &str,
        change: &Change,
        anyway: bool,
    ) -> Result<Outcome> {
        match self.attempt(cache, path, expected, change, anyway) {
            Err(Error::Refusing(why)) => {
                tracing::debug!(photo = %path.display(), why, "not written");
                Ok(Outcome::Refused(why))
            }
            other => other,
        }
    }

    /// What one write would do, tag by tag, without doing any of it. The same code path as a
    /// write, up to and not including the copy: one read of the photo, nothing written, nothing
    /// left behind.
    pub fn dry_run(&mut self, target: &Target) -> Result<Vec<Assignment>> {
        let intent = self.intent(&target.path, &target.content_id, &target.change)?;
        Ok(intent
            .want
            .iter()
            .map(|assign| Assignment::of(assign, &intent.before.fields))
            .collect())
    }

    fn intent(&mut self, path: &Path, expected: &str, change: &Change) -> Result<Intent> {
        let rel_path = self.inside(path)?;
        let bytes = std::fs::read(path).map_err(|error| Error::Refusing(format!("cannot be read: {error}")))?;
        let found = content_id(&bytes).ok_or_else(|| Error::Refusing("not a JPEG we understand".to_string()))?;
        if found != expected {
            return Err(Error::Refusing(
                "its image data is not what this change was built against".to_string(),
            ));
        }

        let before = self.look(path)?;
        change.fits(&before.fields).map_err(Error::Refusing)?;
        let mut want = change.assigns().map_err(Error::Refusing)?;
        want.retain(|assign| !change::settled(assign, &before.fields));
        Ok(Intent {
            rel_path,
            content_id: found,
            before,
            want,
        })
    }

    fn attempt(
        &mut self,
        cache: &mut Cache,
        path: &Path,
        expected: &str,
        change: &Change,
        anyway: bool,
    ) -> Result<Outcome> {
        let intent = self.intent(path, expected, change)?;
        if intent.want.is_empty() {
            return Ok(Outcome::Skipped);
        }
        self.put(cache, path, intent, anyway)
    }

    /// Writes what the intent still wants, on a copy that has to prove itself first.
    fn put(&mut self, cache: &mut Cache, path: &Path, intent: Intent, anyway: bool) -> Result<Outcome> {
        let Intent {
            rel_path,
            content_id: found,
            before,
            want,
        } = intent;
        let temp = beside(path)?;
        let attempt = self.write_copy(&temp, path, &want, &before, &found, anyway);
        if !matches!(attempt, Ok(None)) {
            let _ = std::fs::remove_file(&temp);
        }
        let outcome = match attempt? {
            None => Outcome::Written,
            Some(why) if !anyway && doubted(&why) => Outcome::Doubted(reason(&why)),
            Some(why) => Outcome::Failed(why),
        };
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
        anyway: bool,
    ) -> Result<Option<String>> {
        if let Err(error) = std::fs::copy(original, temp) {
            return Ok(Some(format!("no copy could be made beside it: {error}")));
        }

        let mut args = vec![
            "-overwrite_original".to_string(),
            "-charset".to_string(),
            "iptc=UTF8".to_string(),
        ];
        if anyway {
            args.push("-m".to_string());
        }
        args.extend(change::arguments(want));
        args.push(tool::as_argument(temp)?);
        let reply = self.tool.run(&args)?;
        if !reply.updated() {
            return Ok(Some(reply.complaint()));
        }

        if let Some(why) = self.unproven(temp, want, before, content)? {
            return Ok(Some(why));
        }
        if anyway {
            let (before, after) = (self.maker_note(original)?, self.maker_note(temp)?);
            let changed = differs(&before, &after);
            let block = changed
                .iter()
                .find(|key| key.rsplit(':').next().is_some_and(|tag| tag.starts_with("MakerNote")));
            if let Some(block) = block {
                let length = |reading: &Map<String, Value>| reading.get(block).and_then(Value::as_u64).unwrap_or(0);
                return Ok(Some(format!(
                    "written anyway, the maker note would lose {} of its {} bytes, the part ExifTool cannot read",
                    length(&before).saturating_sub(length(&after)),
                    length(&before)
                )));
            }
            if !changed.is_empty() {
                return Ok(Some(format!(
                    "written anyway, {} did not read back as before",
                    changed.join(", ")
                )));
            }
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

    /// Every value of the maker note, and every picture the file carries besides the image, the
    /// pictures as their bytes.
    fn maker_note(&mut self, path: &Path) -> Result<Map<String, Value>> {
        let args = [
            "-j",
            "-n",
            "-b",
            "-G1",
            "-MakerNotes:All",
            "-MakerNotes",
            "-PreviewImage",
            "-ThumbnailImage",
            "-OtherImage",
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .chain([tool::as_argument(path)?])
        .collect::<Vec<String>>();
        let reply = self.tool.run(&args)?;
        let unreadable = || Error::Refusing(format!("exiftool cannot read its maker note: {}", reply.complaint()));
        let parsed: Value = serde_json::from_str(reply.out.trim()).map_err(|_| unreadable())?;
        let mut fields = parsed
            .get(0)
            .and_then(|entry| entry.as_object())
            .ok_or_else(unreadable)?
            .clone();
        fields.retain(|key, _| key != "SourceFile");
        // The whole maker note moves, so its bytes differ where it says where its parts are; how
        // many there are must not.
        for (key, value) in fields.iter_mut() {
            if key.rsplit(':').next().is_some_and(|tag| tag.starts_with("MakerNote"))
                && let Some(encoded) = value.as_str().and_then(|text| text.strip_prefix("base64:"))
            {
                *value = Value::from(decoded_length(encoded));
            }
        }
        Ok(fields)
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

#[cfg(all(test, feature = "fixtures"))]
mod tests;
