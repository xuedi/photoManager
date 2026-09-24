//! What the date tools' tests share: the place data imported once, and a copy of the stand-in
//! library that can be written, read back with ExifTool and taken back. Never a real photo.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::AtomicBool;

use crate::cache::Cache;
use crate::changeset::{self, ChangeSet};
use crate::geo::Geo;
use crate::journal::Journal;
use crate::metadata::Exiv2;
use crate::scan::{self, Mode};
use crate::thumbs::Thumbs;
use crate::write::{Engine, Summary};

/// The excerpt of the place data, imported once for every test of the process.
pub fn geo() -> Geo {
    static IMPORTED: Once = Once::new();
    let file = std::env::temp_dir().join("photomanager-date-tools").join("geo.db");
    IMPORTED.call_once(|| {
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
        let mut geo = Geo::open(&file).unwrap();
        let dumps: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
        crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
    });
    Geo::open(&file).unwrap()
}

pub struct Library {
    base: PathBuf,
    pub root: PathBuf,
    pub cache: Cache,
    journal: Journal,
}

impl Library {
    pub fn new(name: &str) -> Library {
        let base = std::env::temp_dir().join(format!("photomanager-date-tools-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");
        let mut library = Library {
            cache: Cache::open(&base.join("cache/cache.db")).unwrap(),
            journal: Journal::open(&base.join("data/app.db")).unwrap(),
            base,
            root,
        };
        library.rescan();
        library
    }

    pub fn rescan(&mut self) {
        let thumbs = Thumbs::new(self.base.join("cache/thumbs"));
        scan::run(
            &mut self.cache,
            &self.root,
            &Exiv2,
            &thumbs,
            Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .expect("scan the stand-in library");
    }

    pub fn apply(&mut self, set: &ChangeSet) -> Summary {
        let mut engine = Engine::new(&self.root).unwrap();
        changeset::apply(
            set,
            &mut engine,
            &mut self.journal,
            &mut self.cache,
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("apply the change set")
    }

    pub fn undo(&mut self) -> Summary {
        let mut engine = Engine::new(&self.root).unwrap();
        changeset::undo_last(
            &mut engine,
            &mut self.journal,
            &mut self.cache,
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .expect("take the pass back")
    }

    /// Every date a photo holds, as ExifTool reads it back, by group and name.
    pub fn dates(&self, rel_path: &str) -> BTreeMap<String, String> {
        let out = std::process::Command::new("exiftool")
            .args(["-j", "-G1", "-time:all", "-IPTC:all"])
            .arg(self.root.join(rel_path))
            .output()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        parsed[0]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| {
                ["ExifIFD:", "XMP-", "IPTC:"].iter().any(|group| key.starts_with(group))
                    && ["Date", "Time", "Offset"].iter().any(|part| key.contains(part))
            })
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.as_str().map(String::from).unwrap_or(value.to_string()),
                )
            })
            .collect()
    }
}
