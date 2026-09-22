//! Walking the library and filling the cache. Reads files, never writes one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rayon::prelude::*;

use std::collections::HashMap;

use crate::cache::{Cache, Fingerprint, Known};
use crate::identity::content_id;
use crate::layout::Placement;
use crate::metadata::{Metadata, Reader};

const BATCH: usize = 500;

/// A photo to look at: its full path and its path inside the library.
type Candidate = (PathBuf, String);
/// A file that is not a photo, and what is wrong with it.
type Stray = (PathBuf, IssueKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Read only what is new or has changed.
    Reconcile,
    /// Read every photo again.
    Reread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    Unreadable,
    NotAPhoto,
    Sidecar,
    NoDate,
    OffConvention,
    DuplicateContent,
}

impl IssueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueKind::Unreadable => "unreadable",
            IssueKind::NotAPhoto => "not a photo",
            IssueKind::Sidecar => "sidecar",
            IssueKind::NoDate => "no date",
            IssueKind::OffConvention => "off the convention",
            IssueKind::DuplicateContent => "duplicate content",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub rel_path: String,
    pub kind: IssueKind,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub photos: usize,
    pub read: usize,
    pub unchanged: usize,
    pub added: usize,
    pub changed: usize,
    pub moved: usize,
    pub gone: usize,
    pub issues: usize,
    pub seconds: u64,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Progress {
    Counted(usize),
    Done(usize, usize),
}

struct Found {
    rel_path: String,
    fingerprint: Fingerprint,
    outcome: Outcome,
}

enum Outcome {
    Unchanged,
    Read {
        content_id: Option<String>,
        metadata: Box<Metadata>,
        existed: bool,
    },
    Failed(String),
}

pub fn run(
    cache: &mut Cache,
    library: &Path,
    reader: &dyn Reader,
    mode: Mode,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> crate::cache::Result<Summary> {
    let started = Instant::now();
    let (photos, strays) = walk(library);
    progress(Progress::Counted(photos.len()));

    let known = cache.all_known()?;
    let gone = missing(&known, &photos);
    let mut summary = Summary {
        photos: photos.len(),
        gone: gone.len(),
        ..Summary::default()
    };

    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| -> crate::cache::Result<()> {
        scope.spawn(move || {
            photos.par_iter().for_each_with(sender, |sender, (file, rel_path)| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let _ = sender.send(examine(&known, file, rel_path, reader, mode));
            });
        });

        let total = summary.photos;
        let mut done = 0;
        let mut pending: Vec<Found> = Vec::with_capacity(BATCH);
        for found in receiver {
            pending.push(found);
            done += 1;
            if pending.len() >= BATCH {
                store(cache, &mut summary, &mut pending, &gone)?;
                progress(Progress::Done(done, total));
            }
        }
        store(cache, &mut summary, &mut pending, &gone)?;
        progress(Progress::Done(done, total));
        Ok(())
    })?;

    summary.cancelled = cancel.load(Ordering::Relaxed);
    if !summary.cancelled {
        cache.forget(&gone)?;
        summary.issues += note_strays(cache, &strays)?;
        summary.issues += note_duplicates(cache)?;
    }
    summary.seconds = started.elapsed().as_secs();
    summary.photos = cache.photo_count()? as usize;
    summary.issues = cache.issue_count()? as usize;
    Ok(summary)
}

fn examine(known: &HashMap<String, Known>, file: &Path, rel_path: &str, reader: &dyn Reader, mode: Mode) -> Found {
    let Ok(fingerprint) = fingerprint(file) else {
        return Found {
            rel_path: rel_path.to_string(),
            fingerprint: Fingerprint {
                size: 0,
                mtime_ns: 0,
                inode: 0,
            },
            outcome: Outcome::Failed("cannot be opened".to_string()),
        };
    };
    let before = known.get(rel_path);
    let existed = before.is_some();

    if mode == Mode::Reconcile && before.map(|known| known.fingerprint) == Some(fingerprint) {
        return Found {
            rel_path: rel_path.to_string(),
            fingerprint,
            outcome: Outcome::Unchanged,
        };
    }

    let outcome = match std::fs::read(file) {
        Err(error) => Outcome::Failed(error.to_string()),
        Ok(bytes) => match reader.read(file, &bytes) {
            Err(error) => Outcome::Failed(error.to_string()),
            Ok(metadata) => Outcome::Read {
                content_id: content_id(&bytes),
                metadata: Box::new(metadata),
                existed,
            },
        },
    };
    Found {
        rel_path: rel_path.to_string(),
        fingerprint,
        outcome,
    }
}

fn store(
    cache: &mut Cache,
    summary: &mut Summary,
    found: &mut Vec<Found>,
    gone: &[String],
) -> crate::cache::Result<()> {
    if found.is_empty() {
        return Ok(());
    }
    let mut moved = Vec::new();
    {
        let writer = cache.transaction()?;
        for entry in found.iter() {
            match &entry.outcome {
                Outcome::Unchanged => summary.unchanged += 1,
                Outcome::Failed(why) => {
                    summary.read += 1;
                    writer.forget(&entry.rel_path)?;
                    writer.forget_issues(&entry.rel_path)?;
                    writer.add_issue(
                        &Issue {
                            rel_path: entry.rel_path.clone(),
                            kind: IssueKind::Unreadable,
                            detail: Some(why.clone()),
                        },
                        None,
                    )?;
                    summary.issues += 1;
                }
                Outcome::Read {
                    content_id,
                    metadata,
                    existed,
                } => {
                    summary.read += 1;
                    match existed {
                        true => summary.changed += 1,
                        false => summary.added += 1,
                    }
                    let placement = Placement::parse(&entry.rel_path);
                    writer.forget_issues(&entry.rel_path)?;
                    let id = writer.put(
                        &entry.rel_path,
                        entry.fingerprint,
                        content_id.as_deref(),
                        &placement,
                        metadata,
                    )?;

                    let mut issues = Vec::new();
                    if !placement.fits() {
                        issues.push((IssueKind::OffConvention, Some(placement.fit.as_str().to_string())));
                    }
                    if metadata.taken_at.is_none() {
                        issues.push((IssueKind::NoDate, None));
                    }
                    if content_id.is_none() {
                        issues.push((IssueKind::NotAPhoto, Some("no JPEG image data".to_string())));
                    }
                    for (kind, detail) in issues {
                        writer.add_issue(
                            &Issue {
                                rel_path: entry.rel_path.clone(),
                                kind,
                                detail,
                            },
                            Some(id),
                        )?;
                        summary.issues += 1;
                    }
                    if !existed && let Some(id) = content_id {
                        moved.push(id.clone());
                    }
                }
            }
        }
        writer.commit()?;
    }

    for content in moved {
        if cache.moved_from(&content, gone)?.is_some() {
            summary.moved += 1;
            summary.added = summary.added.saturating_sub(1);
        }
    }
    found.clear();
    Ok(())
}

fn note_strays(cache: &mut Cache, strays: &[Stray]) -> crate::cache::Result<usize> {
    let writer = cache.transaction()?;
    for kind in [IssueKind::Sidecar, IssueKind::NotAPhoto] {
        writer.forget_issues_of_kind(kind.as_str())?;
    }
    for (path, kind) in strays {
        writer.add_issue(
            &Issue {
                rel_path: path.to_string_lossy().to_string(),
                kind: *kind,
                detail: None,
            },
            None,
        )?;
    }
    writer.commit()?;
    Ok(strays.len())
}

fn note_duplicates(cache: &mut Cache) -> crate::cache::Result<usize> {
    let writer = cache.transaction()?;
    writer.forget_issues_of_kind(IssueKind::DuplicateContent.as_str())?;
    let found = writer.note_duplicates(IssueKind::DuplicateContent.as_str())?;
    writer.commit()?;
    Ok(found)
}

fn missing(known: &HashMap<String, Known>, photos: &[Candidate]) -> Vec<String> {
    let seen: std::collections::HashSet<&str> = photos.iter().map(|(_, rel)| rel.as_str()).collect();
    known
        .keys()
        .filter(|path| !seen.contains(path.as_str()))
        .cloned()
        .collect()
}

fn fingerprint(file: &Path) -> std::io::Result<Fingerprint> {
    use std::os::unix::fs::MetadataExt;
    let data = file.metadata()?;
    Ok(Fingerprint {
        size: data.len(),
        mtime_ns: data.mtime() * 1_000_000_000 + i64::from(data.mtime_nsec() as u32),
        inode: data.ino(),
    })
}

/// Photos first, everything else as something to look at.
fn walk(library: &Path) -> (Vec<Candidate>, Vec<Stray>) {
    let mut photos = Vec::new();
    let mut strays = Vec::new();

    for entry in walkdir::WalkDir::new(library)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let Ok(relative) = path.strip_prefix(library) else {
            continue;
        };
        let rel_path = relative.to_string_lossy().to_string();
        if rel_path.starts_with('.') || rel_path.contains("/.") {
            continue;
        }
        let extension = path
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "jpg" | "jpeg" => photos.push((path, rel_path)),
            "xmp" => strays.push((relative.to_path_buf(), IssueKind::Sidecar)),
            _ => strays.push((relative.to_path_buf(), IssueKind::NotAPhoto)),
        }
    }
    photos.sort_by(|one, other| one.1.cmp(&other.1));
    (photos, strays)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::metadata::Exiv2;

    struct Setup {
        library: PathBuf,
        cache: Cache,
    }

    fn setup(name: &str) -> Setup {
        let base = std::env::temp_dir().join(format!("photomanager-scan-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let library = base.join("library");
        crate::fixtures::build(&library).expect("build the fixture library");
        Setup {
            cache: Cache::open(&base.join("cache.db")).unwrap(),
            library,
        }
    }

    impl Setup {
        fn scan(&mut self, mode: Mode) -> Summary {
            run(
                &mut self.cache,
                &self.library,
                &Exiv2,
                mode,
                &|_| {},
                &AtomicBool::new(false),
            )
            .unwrap()
        }

        fn issues(&self) -> Vec<(String, i64)> {
            self.cache.issue_counts().unwrap()
        }
    }

    fn snapshot(root: &Path) -> Vec<(String, u64, String)> {
        let mut files: Vec<(String, u64, String)> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                let bytes = std::fs::read(entry.path()).unwrap();
                let digest = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes));
                let rel = entry.path().strip_prefix(root).unwrap().to_string_lossy().to_string();
                (rel, bytes.len() as u64, digest)
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn a_full_scan_finds_every_photo_and_changes_nothing() {
        let mut setup = setup("full");
        let before = snapshot(&setup.library);

        let summary = setup.scan(Mode::Reconcile);
        assert_eq!(summary.photos, crate::fixtures::photo_count());
        assert_eq!(summary.added, crate::fixtures::photo_count());
        assert_eq!((summary.changed, summary.unchanged, summary.gone), (0, 0, 0));
        assert!(!summary.cancelled);

        let issues = setup.issues();
        let kind = |name: &str| issues.iter().find(|(k, _)| k == name).map(|(_, n)| *n).unwrap_or(0);
        assert_eq!(kind("no date"), 2, "two fixture photos carry no date");
        assert_eq!(kind("off the convention"), 1, "the loose file in China/");
        assert_eq!(kind("sidecar"), 0);

        assert_eq!(before, snapshot(&setup.library), "the scan changed the library");
    }

    #[test]
    fn a_second_scan_reads_nothing() {
        let mut setup = setup("second");
        setup.scan(Mode::Reconcile);

        let again = setup.scan(Mode::Reconcile);
        assert_eq!(again.read, 0, "nothing changed, nothing to read");
        assert_eq!(again.unchanged, crate::fixtures::photo_count());
        assert_eq!(again.photos, crate::fixtures::photo_count());

        let forced = setup.scan(Mode::Reread);
        assert_eq!(forced.read, crate::fixtures::photo_count());
        assert_eq!(forced.changed, crate::fixtures::photo_count());
    }

    #[test]
    fn only_what_changed_is_read_again() {
        let mut setup = setup("changed");
        setup.scan(Mode::Reconcile);

        let touched = setup.library.join("Ireland/2008-10-03 Galway/IMG_0003.JPG");
        let written = std::process::Command::new("exiftool")
            .args(["-overwrite_original", "-q", "-Rating=4"])
            .arg(&touched)
            .status()
            .unwrap();
        assert!(written.success());

        let summary = setup.scan(Mode::Reconcile);
        assert_eq!((summary.read, summary.changed), (1, 1));
        assert_eq!(summary.unchanged, crate::fixtures::photo_count() - 1);
    }

    #[test]
    fn a_deleted_photo_is_dropped_and_a_moved_one_keeps_its_id() {
        let mut setup = setup("moved");
        setup.scan(Mode::Reconcile);
        let content = setup
            .cache
            .known("Greece/0000-00-00 Aeron ilands/IMG_0004.JPG")
            .unwrap()
            .unwrap()
            .content_id;

        std::fs::remove_file(setup.library.join("China/IMG_3140.JPG")).unwrap();
        let from = setup.library.join("Greece/0000-00-00 Aeron ilands/IMG_0004.JPG");
        let to = setup.library.join("Greece/0000-00-00 Aran Islands/IMG_0004.JPG");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(&from, &to).unwrap();

        let summary = setup.scan(Mode::Reconcile);
        assert_eq!(summary.gone, 2, "one deleted, one at its old path");
        assert_eq!(summary.moved, 1);
        assert_eq!(summary.photos, crate::fixtures::photo_count() - 1);
        assert!(setup.cache.known("China/IMG_3140.JPG").unwrap().is_none());

        let after = setup
            .cache
            .known("Greece/0000-00-00 Aran Islands/IMG_0004.JPG")
            .unwrap()
            .unwrap();
        assert_eq!(after.content_id, content, "a move keeps the image id");
    }

    #[test]
    fn strays_and_duplicates_are_reported() {
        let mut setup = setup("strays");
        let event = setup.library.join("Germany/2019-07-13 Sommerfest");
        std::fs::copy(event.join("img_0657.jpg"), event.join("img_0657_copy.jpg")).unwrap();
        std::fs::write(event.join("img_0657.xmp"), "<x:xmpmeta/>").unwrap();
        std::fs::write(event.join("notes.txt"), "not a photo").unwrap();

        setup.scan(Mode::Reconcile);
        let issues = setup.issues();
        let kind = |name: &str| issues.iter().find(|(k, _)| k == name).map(|(_, n)| *n).unwrap_or(0);
        assert_eq!(kind("sidecar"), 1);
        assert_eq!(kind("not a photo"), 1);
        assert_eq!(kind("duplicate content"), 2, "both copies are reported");

        std::fs::remove_file(event.join("img_0657_copy.jpg")).unwrap();
        std::fs::remove_file(event.join("img_0657.xmp")).unwrap();
        setup.scan(Mode::Reconcile);
        let issues = setup.issues();
        let kind = |name: &str| issues.iter().find(|(k, _)| k == name).map(|(_, n)| *n).unwrap_or(0);
        assert_eq!(kind("sidecar"), 0, "a fixed issue disappears");
        assert_eq!(kind("duplicate content"), 0);
    }

    #[test]
    fn a_cancelled_scan_leaves_a_usable_cache() {
        let mut setup = setup("cancelled");
        let cancel = AtomicBool::new(true);
        let summary = run(
            &mut setup.cache,
            &setup.library,
            &Exiv2,
            Mode::Reconcile,
            &|_| {},
            &cancel,
        )
        .unwrap();

        assert!(summary.cancelled);
        assert_eq!(setup.cache.photo_count().unwrap(), 0);
        assert_eq!(setup.scan(Mode::Reconcile).photos, crate::fixtures::photo_count());
    }

    #[test]
    fn progress_counts_up_to_the_total() {
        let mut setup = setup("progress");
        let seen = std::sync::Mutex::new(Vec::new());
        let summary = run(
            &mut setup.cache,
            &setup.library,
            &Exiv2,
            Mode::Reconcile,
            &|progress| seen.lock().unwrap().push(progress),
            &AtomicBool::new(false),
        )
        .unwrap();

        let seen = seen.into_inner().unwrap();
        assert!(matches!(seen.first(), Some(Progress::Counted(n)) if *n == summary.photos));
        assert!(matches!(seen.last(), Some(Progress::Done(done, total)) if done == total));
    }
}
