//! Taking a folder or a photo to another place in the library.
//!
//! A move is one rename on one filesystem, so it is atomic: an event is in its old folder or in
//! its new one, never half in each. Before it, every file in what moves is proved to be a photo
//! the move was built against, by its content id; then renamed; then proved to be all there, file by file, in the new
//! place. Nothing is ever overwritten, nothing is copied, and not a byte of a photo changes: a
//! rename keeps the content and the modification time.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::{Engine, Error, Outcome, Result, Summary};
use crate::cache::Cache;
use crate::identity::content_id;

/// A folder or a photo, where it goes, and every photo that goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: String,
    pub to: String,
    /// Every photo that goes with it, by its path now, and the content id the move was built
    /// against.
    pub photos: Vec<(String, String)>,
}

/// Every file in what moves, by its path relative to it (empty for a photo moved on its own),
/// with its inode: what a rename keeps, and so what proves nothing was lost on the way.
type Inventory = HashMap<String, u64>;

impl Engine {
    /// Folders and photos one after another. A refusal on one does not stop the others.
    /// `title` names the pass in the log.
    pub fn relocate(
        &mut self,
        cache: &mut Cache,
        title: &str,
        moves: &[Move],
        progress: &(dyn Fn(usize, usize) + Sync),
        cancel: &AtomicBool,
    ) -> Result<Summary> {
        let started = Instant::now();
        let mut summary = Summary::default();
        for (done, one) in moves.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            let outcome = self.shift(cache, one)?;
            summary.note(&one.from, outcome);
            progress(done + 1, moves.len());
        }
        summary.seconds = started.elapsed().as_secs();
        tracing::info!(
            title,
            moved = summary.written,
            refused = summary.refused,
            failed = summary.failed,
            cancelled = summary.cancelled,
            "move pass done"
        );
        Ok(summary)
    }

    /// One move. A refusal is about it alone, so it never comes back as an error.
    fn shift(&mut self, cache: &mut Cache, one: &Move) -> Result<Outcome> {
        match self.try_shift(cache, one) {
            Err(Error::Refusing(why)) => {
                tracing::debug!(from = one.from, to = one.to, why, "not moved");
                Ok(Outcome::Refused(why))
            }
            other => other,
        }
    }

    fn try_shift(&mut self, cache: &mut Cache, one: &Move) -> Result<Outcome> {
        let from = self.within(&one.from)?;
        let to = self.within(&one.to)?;
        if one.from == one.to {
            return Err(Error::Refusing(format!("{} is where it is to go", one.from)));
        }
        if Path::new(&one.to).starts_with(&one.from) {
            return Err(Error::Refusing(format!("{} cannot go inside itself", one.from)));
        }
        let found =
            std::fs::symlink_metadata(&from).map_err(|_| Error::Refusing(format!("{} is not there", one.from)))?;
        if found.file_type().is_symlink() {
            return Err(Error::Refusing(format!(
                "{} is a link, not a folder or a photo",
                one.from
            )));
        }
        self.inside(&from)?;
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(Error::Refusing(format!("{} is there already", one.to)));
        }
        let standing = standing(&to).ok_or_else(|| Error::Refusing(format!("{} has nowhere to go", one.to)))?;
        let standing = standing
            .canonicalize()
            .map_err(|error| Error::Refusing(format!("{}: {error}", standing.display())))?;
        if !standing.starts_with(&self.library) {
            return Err(Error::Refusing(format!("{} is outside the library", one.to)));
        }
        let place = std::fs::metadata(&standing)
            .map_err(|error| Error::Refusing(format!("{}: {error}", standing.display())))?;
        if place.dev() != found.dev() {
            return Err(Error::Refusing(format!(
                "{} is on another filesystem, and a photo is never copied",
                one.to
            )));
        }

        let before = inventory(&from, &one.from)?;
        prove(&from, &before, one)?;

        let made = missing_parents(&to);
        if let Some(parent) = to.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return Ok(failed(&made, format!("its new folder could not be made: {error}")));
        }
        if let Err(error) = std::fs::rename(&from, &to) {
            return Ok(failed(&made, format!("it could not be moved: {error}")));
        }
        for dir in [from.parent(), to.parent()].into_iter().flatten() {
            let _ = std::fs::File::open(dir).and_then(|dir| dir.sync_all());
        }

        let after = inventory(&to, &one.to).unwrap_or_default();
        if after != before {
            let why = "not every file arrived as it left".to_string();
            if std::fs::rename(&to, &from).is_ok() {
                return Ok(failed(&made, why));
            }
            tracing::error!(
                from = one.from,
                to = one.to,
                why,
                "moved, not proved, and not moved back"
            );
            return Ok(Outcome::Written);
        }

        if let Some(parent) = from.parent() {
            self.prune(parent);
        }
        if let Err(error) = cache.relocate(&one.from, &one.to) {
            tracing::warn!(%error, from = one.from, "the cache rows stayed behind");
        }
        tracing::info!(from = one.from, to = one.to, photos = one.photos.len(), "moved");
        Ok(Outcome::Written)
    }

    /// A path inside the library, spelled only with names: no root, no `.` and no `..`.
    fn within(&self, rel_path: &str) -> Result<PathBuf> {
        let plain = rel_path.split('/').all(|name| !matches!(name, "" | "." | ".."));
        match plain {
            true => Ok(self.library.join(rel_path)),
            false => Err(Error::Refusing(format!("{rel_path} is not a path inside the library"))),
        }
    }

    /// Takes away the folders a move left empty, from `dir` upwards, never the library itself.
    fn prune(&self, dir: &Path) {
        let mut dir = dir.to_path_buf();
        while dir != self.library && dir.starts_with(&self.library) {
            let empty = std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none());
            if !empty || std::fs::remove_dir(&dir).is_err() {
                break;
            }
            tracing::debug!(dir = %dir.display(), "left empty, taken away");
            match dir.parent() {
                Some(parent) => dir = parent.to_path_buf(),
                None => break,
            }
        }
    }
}

/// A move that did not happen: the folders it made for itself go again.
fn failed(made: &[PathBuf], why: String) -> Outcome {
    for dir in made {
        let _ = std::fs::remove_dir(dir);
    }
    Outcome::Failed(why)
}

/// The nearest folder of `path` that is there already.
fn standing(path: &Path) -> Option<PathBuf> {
    path.ancestors().skip(1).find(|dir| dir.is_dir()).map(Path::to_path_buf)
}

/// The folders above `path` a move would have to make, deepest first.
fn missing_parents(path: &Path) -> Vec<PathBuf> {
    path.ancestors()
        .skip(1)
        .take_while(|dir| std::fs::symlink_metadata(dir).is_err())
        .map(Path::to_path_buf)
        .collect()
}

/// Every file under `root`, or `root` itself when it is a file. A link or anything else that is
/// neither a file nor a folder is a refusal: it is not something a move proves.
fn inventory(root: &Path, rel_root: &str) -> Result<Inventory> {
    let mut found = Inventory::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|error| Error::Refusing(format!("{rel_root} cannot be read: {error}")))?;
        let kind = entry.file_type();
        if kind.is_dir() {
            continue;
        }
        let inside = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .map(String::from)
            .ok_or_else(|| Error::Refusing(format!("{} is not a name we can keep", entry.path().display())))?;
        if !kind.is_file() {
            return Err(Error::Refusing(format!("{} is not a file", joined(rel_root, &inside))));
        }
        let metadata = entry
            .metadata()
            .map_err(|error| Error::Refusing(format!("{}: {error}", joined(rel_root, &inside))))?;
        found.insert(inside, metadata.ino());
    }
    Ok(found)
}

/// Every file is a photo the move was built against, with the image data it was built against,
/// and every such photo is there.
fn prove(root: &Path, found: &Inventory, one: &Move) -> Result<()> {
    let expected: HashMap<String, &str> = one
        .photos
        .iter()
        .map(|(rel_path, content)| (inside(rel_path, &one.from), content.as_str()))
        .collect();
    let mut names: Vec<&String> = found.keys().collect();
    names.sort();
    for name in &names {
        if !expected.contains_key(*name) {
            return Err(Error::Refusing(format!(
                "{} is not a photo the scan knows, so scan the library first",
                joined(&one.from, name)
            )));
        }
    }
    let mut wanted: Vec<(&String, &&str)> = expected.iter().collect();
    wanted.sort();
    for (name, content) in wanted {
        if !found.contains_key(name) {
            return Err(Error::Refusing(format!(
                "{} is no longer there",
                joined(&one.from, name)
            )));
        }
        let file = match name.is_empty() {
            true => root.to_path_buf(),
            false => root.join(name),
        };
        let bytes = std::fs::read(&file)
            .map_err(|error| Error::Refusing(format!("{} cannot be read: {error}", joined(&one.from, name))))?;
        if content_id(&bytes).as_deref() != Some(*content) {
            return Err(Error::Refusing(format!(
                "the image data of {} is not what this move was built against",
                joined(&one.from, name)
            )));
        }
    }
    Ok(())
}

/// A photo's path relative to what moves, empty when it is what moves.
fn inside(rel_path: &str, root: &str) -> String {
    match rel_path.strip_prefix(root) {
        Some("") => String::new(),
        Some(rest) if rest.starts_with('/') => rest[1..].to_string(),
        _ => rel_path.to_string(),
    }
}

fn joined(root: &str, inside: &str) -> String {
    match inside.is_empty() {
        true => root.to_string(),
        false => format!("{root}/{inside}"),
    }
}
