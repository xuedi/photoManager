//! Every test here writes to copies of the stand-in library. Nothing in this file, and nothing it
//! calls, may ever be pointed at a real photo.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::journal::{self, Journal};

const RATED: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG";
const BARE: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";
const TAGGED: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";

struct Setup {
    root: PathBuf,
    engine: Engine,
    journal: Journal,
    cache: Cache,
}

impl Setup {
    fn new(name: &str) -> Setup {
        let base = std::env::temp_dir().join(format!("photomanager-write-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");

        Setup {
            engine: Engine::new(&root).unwrap(),
            journal: Journal::open(&base.join("data/app.db")).unwrap(),
            cache: Cache::open(&base.join("cache/cache.db")).unwrap(),
            root,
        }
    }

    fn path(&self, rel_path: &str) -> PathBuf {
        self.root.join(rel_path)
    }

    fn target(&self, rel_path: &str, change: Change) -> Target {
        let path = self.path(rel_path);
        Target {
            content_id: content_id(&std::fs::read(&path).unwrap()).expect("a jpeg"),
            path,
            change,
        }
    }

    fn write(&mut self, rel_path: &str, change: Change) -> Outcome {
        let target = self.target(rel_path, change);
        self.engine
            .write_one(&mut self.journal, &mut self.cache, "Edit", &target)
            .unwrap()
    }

    /// Everything one photo says now, as ExifTool reads it.
    fn look(&mut self, rel_path: &str) -> Map<String, Value> {
        let path = self.path(rel_path);
        self.engine.look(&path).unwrap().fields
    }

    fn field(&mut self, rel_path: &str, key: &str) -> Option<Value> {
        self.look(rel_path).get(key).cloned()
    }

    /// The image data of every photo in the library, by path. A metadata write must not move one.
    fn image_data(&self) -> BTreeMap<String, String> {
        let mut all = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel_path = entry.path().strip_prefix(&self.root).unwrap().display().to_string();
            let bytes = std::fs::read(entry.path()).unwrap();
            all.insert(rel_path, content_id(&bytes).unwrap_or_else(|| "not a jpeg".to_string()));
        }
        all
    }

    /// Any temporary file the engine left behind.
    fn leftovers(&self) -> Vec<String> {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".writing-"))
            .collect()
    }

    fn exiftool_image_hash(&self, rel_path: &str) -> String {
        let out = std::process::Command::new("exiftool")
            .args([
                "-s3",
                "-api",
                "RequestAll=3",
                "-api",
                "ImageHashType=SHA256",
                "-ImageDataHash",
            ])
            .arg(self.path(rel_path))
            .output()
            .unwrap();
        let hash = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(!hash.is_empty(), "exiftool gave no image data hash for {rel_path}");
        hash
    }
}

fn quiet() -> impl Fn(usize, usize) {
    |_, _| {}
}

// Phase 1 - one photo, written safely

#[test]
fn a_rating_is_written_and_reads_back() {
    let mut setup = Setup::new("rating");
    assert_eq!(
        setup.write(BARE, Change::of([Field::Rating(Some(4))])),
        Outcome::Written
    );
    assert_eq!(setup.field(BARE, "XMP-xmp:Rating"), Some(Value::from(4)));
}

#[test]
fn the_image_data_is_untouched_by_both_measures() {
    let mut setup = Setup::new("image-data");
    let before = setup.image_data();
    let hash = setup.exiftool_image_hash(BARE);

    assert_eq!(
        setup.write(BARE, Change::of([Field::Rating(Some(2))])),
        Outcome::Written
    );

    assert_eq!(setup.image_data(), before, "our content id moved");
    assert_eq!(setup.exiftool_image_hash(BARE), hash, "exiftool's image hash moved");
}

#[test]
fn the_mode_survives_and_the_modification_time_does_not() {
    let mut setup = Setup::new("mode");
    let file = setup.path(BARE);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = std::fs::metadata(&file).unwrap();

    assert_eq!(
        setup.write(BARE, Change::of([Field::Rating(Some(5))])),
        Outcome::Written
    );

    let after = std::fs::metadata(&file).unwrap();
    assert_eq!(after.permissions().mode() & 0o777, 0o444, "a read-only file stayed so");
    assert_ne!(
        after.modified().unwrap(),
        before.modified().unwrap(),
        "nothing would notice the change"
    );
}

#[test]
fn a_write_that_cannot_be_proved_leaves_the_original_alone() {
    let mut setup = Setup::new("unproven");
    let file = setup.path(BARE);
    let before = std::fs::read(&file).unwrap();

    // The value is written, but proved against a name it can never read back under.
    let wrong = Assign {
        tag: "XMP-xmp:Rating".to_string(),
        key: "XMP-xmp:NoSuchTag".to_string(),
        value: Value::from(3),
    };
    let batch = setup.journal.start(journal::Kind::Write, None, "Pass", None).unwrap();
    let id = content_id(&before).unwrap();
    let outcome = setup
        .engine
        .one(
            &mut setup.journal,
            &mut setup.cache,
            batch,
            &file,
            &id,
            Wish::Back {
                want: vec![wrong],
                expect: vec![],
            },
        )
        .unwrap();

    match &outcome {
        Outcome::Failed(why) => assert!(why.contains("did not read back"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&file).unwrap(), before, "the original was changed");
    assert!(setup.leftovers().is_empty(), "{:?}", setup.leftovers());
}

#[test]
fn a_missing_file_is_a_reason_not_a_panic() {
    let mut setup = Setup::new("missing");
    let target = Target {
        path: setup.path("China/nothing-here.jpg"),
        content_id: "0".repeat(32),
        change: Change::of([Field::Rating(Some(1))]),
    };
    let outcome = setup
        .engine
        .write_one(&mut setup.journal, &mut setup.cache, "Edit", &target)
        .unwrap();
    assert!(matches!(outcome, Outcome::Refused(_)), "{outcome:?}");
}

#[test]
fn a_file_that_is_not_a_photo_is_refused() {
    let mut setup = Setup::new("not-a-photo");
    let file = setup.path("China/notes.jpg");
    std::fs::write(&file, b"this is not an image at all").unwrap();

    let target = Target {
        path: file,
        content_id: "0".repeat(32),
        change: Change::of([Field::Rating(Some(1))]),
    };
    let outcome = setup
        .engine
        .write_one(&mut setup.journal, &mut setup.cache, "Edit", &target)
        .unwrap();
    match &outcome {
        Outcome::Refused(why) => assert!(why.contains("JPEG"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_directory_that_cannot_be_written_is_a_reason_not_a_panic() {
    let mut setup = Setup::new("read-only-dir");
    let file = setup.path(BARE);
    let before = std::fs::read(&file).unwrap();
    let dir = file.parent().unwrap().to_path_buf();
    let was = std::fs::metadata(&dir).unwrap().permissions();

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Root ignores the write bit, so where the restriction does not bite there is nothing to test.
    let probe = dir.join(".probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(&dir, was).unwrap();
        eprintln!("skipped: this user writes into a directory that has no write bit");
        return;
    }

    let outcome = setup.write(BARE, Change::of([Field::Rating(Some(1))]));
    std::fs::set_permissions(&dir, was).unwrap();

    match &outcome {
        Outcome::Failed(why) => assert!(why.contains("no copy could be made"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn a_photo_outside_the_library_is_refused() {
    let mut setup = Setup::new("outside");
    let elsewhere = std::env::temp_dir().join("photomanager-write-outside-stray");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let file = elsewhere.join("stray.jpg");
    std::fs::write(&file, include_bytes!("../fixtures/p01.jpg")).unwrap();
    let before = std::fs::read(&file).unwrap();

    let target = Target {
        content_id: content_id(&before).unwrap(),
        path: file.clone(),
        change: Change::of([Field::Rating(Some(1))]),
    };
    let outcome = setup
        .engine
        .write_one(&mut setup.journal, &mut setup.cache, "Edit", &target)
        .unwrap();

    match &outcome {
        Outcome::Refused(why) => assert!(why.contains("outside the library"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_photo_whose_image_data_moved_under_us_is_refused() {
    let mut setup = Setup::new("moved");
    let target = Target {
        path: setup.path(BARE),
        content_id: "0".repeat(32),
        change: Change::of([Field::Rating(Some(1))]),
    };
    let outcome = setup
        .engine
        .write_one(&mut setup.journal, &mut setup.cache, "Edit", &target)
        .unwrap();
    match &outcome {
        Outcome::Refused(why) => assert!(why.contains("built against"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// Phase 2 - written down before it happens

#[test]
fn a_write_leaves_one_entry_with_both_sides_of_every_field() {
    let mut setup = Setup::new("journal-entry");
    let was = setup.field(RATED, "XMP-dc:Subject");
    assert_eq!(
        setup.write(RATED, Change::of([Field::Tags(vec!["mixed/food".to_string()])])),
        Outcome::Written
    );

    let batch = setup.journal.passes(1).unwrap().remove(0);
    let entries = setup.journal.entries(batch.id).unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.rel_path, RATED);
    assert_eq!(entry.outcome.as_deref(), Some(journal::WRITTEN));
    assert!(entry.image_hash.is_some());
    assert!(
        entry.before.contains("XMP-digiKam:TagsList"),
        "the whole metadata is kept"
    );
    assert_eq!(entry.swaps.len(), 5, "one per tag field");

    let subject = entry.swaps.iter().find(|swap| swap.tag == "XMP-dc:Subject").unwrap();
    assert_eq!(subject.old, was.map(|value| value.to_string()));
    assert_eq!(subject.new.as_deref(), Some(r#"["food","mixed"]"#));
}

#[test]
fn nothing_is_written_when_the_journal_cannot_be() {
    let mut setup = Setup::new("no-journal");
    let file = setup.path(BARE);
    let before = std::fs::read(&file).unwrap();
    let id = content_id(&before).unwrap();

    // A batch that does not exist: the entry cannot be recorded, so the photo is not touched.
    let error = setup
        .engine
        .one(
            &mut setup.journal,
            &mut setup.cache,
            404,
            &file,
            &id,
            Wish::New(&Change::of([Field::Rating(Some(3))])),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Journal(_)), "{error}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(setup.leftovers().is_empty());
}

#[test]
fn the_journal_survives_a_cache_rebuild() {
    let mut setup = Setup::new("survives-rebuild");
    setup.write(BARE, Change::of([Field::Rating(Some(3))]));
    let before = setup.journal.passes(10).unwrap().len();

    setup.cache = setup.cache.rebuild().unwrap();

    let journal = Journal::open(setup.journal.file()).unwrap();
    assert_eq!(journal.passes(10).unwrap().len(), before);
    assert_eq!(journal.passes(1).unwrap()[0].written, 1);
}

#[test]
fn a_pass_from_before_the_migration_can_still_be_taken_back() {
    let mut setup = Setup::new("pre-migration");
    let target = setup.target(BARE, Change::of([Field::Rating(Some(4))]));
    let written = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &[target],
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();

    // Back to how version 1 kept it: no names on the batches.
    let file = setup.journal.file().to_path_buf();
    setup.journal = Journal::open(&file.with_file_name("other.db")).unwrap();
    let connection = rusqlite::Connection::open(&file).unwrap();
    connection
        .execute_batch(
            "ALTER TABLE batch DROP COLUMN title; ALTER TABLE batch DROP COLUMN tool; PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(connection);

    setup.journal = Journal::open(&file).unwrap();
    assert!(file.with_file_name("app.db.v1").exists());
    assert_eq!(setup.journal.pass(written.batch).unwrap().title(), journal::EARLIER);
    let undone = setup
        .engine
        .undo(
            &mut setup.journal,
            &mut setup.cache,
            written.batch,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(undone.written, 1);
    assert_eq!(setup.field(BARE, "XMP-xmp:Rating"), None);
    assert_eq!(
        setup.journal.pass(undone.batch).unwrap().title(),
        "Take back: Earlier change"
    );
}

// Phase 3 - taking it back

#[test]
fn an_undo_puts_back_exactly_what_was_there() {
    let mut setup = Setup::new("undo");
    let before = setup.look(TAGGED);
    let image = setup.image_data();

    let change = Change::of([
        Field::Tags(vec!["places/inChina/Shanghai".to_string()]),
        Field::Rating(Some(5)),
        Field::Gps(Some(Gps {
            lat: 31.2304,
            lon: 121.4737,
            altitude: None,
        })),
    ]);
    let target = setup.target(TAGGED, change);
    let written = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &[target],
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(written.written, 1, "{written:?}");
    assert_eq!(setup.field(TAGGED, "GPS:GPSLatitude"), Some(Value::from(31.2304)));

    let undone = setup
        .engine
        .undo(
            &mut setup.journal,
            &mut setup.cache,
            written.batch,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(undone.written, 1, "{undone:?}");

    let after = setup.look(TAGGED);
    for key in [
        "XMP-digiKam:TagsList",
        "XMP-lr:HierarchicalSubject",
        "XMP-microsoft:LastKeywordXMP",
        "XMP-dc:Subject",
        "IPTC:Keywords",
        "XMP-xmp:Rating",
        "GPS:GPSLatitude",
        "GPS:GPSLongitude",
        "GPS:GPSMapDatum",
    ] {
        assert_eq!(after.get(key), before.get(key), "{key} did not come back");
    }
    assert_eq!(setup.image_data(), image, "the image data moved");
}

#[test]
fn an_undo_is_itself_in_the_journal() {
    let mut setup = Setup::new("undo-journaled");
    let target = setup.target(BARE, Change::of([Field::Rating(Some(4))]));
    let written = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &[target],
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let undone = setup
        .engine
        .undo(
            &mut setup.journal,
            &mut setup.cache,
            written.batch,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();

    let pass = setup.journal.pass(undone.batch).unwrap();
    assert_eq!(pass.kind, "undo");
    assert_eq!(pass.undoes, Some(written.batch));
    assert_eq!(pass.written, 1);
    assert!(pass.finished_at.is_some());
    assert!(setup.journal.unfinished().unwrap().is_empty());
}

#[test]
fn undoing_the_same_batch_twice_refuses() {
    let mut setup = Setup::new("undo-twice");
    let target = setup.target(BARE, Change::of([Field::Rating(Some(4))]));
    let written = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &[target],
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let undo = |setup: &mut Setup| {
        setup.engine.undo(
            &mut setup.journal,
            &mut setup.cache,
            written.batch,
            &quiet(),
            &AtomicBool::new(false),
        )
    };
    undo(&mut setup).unwrap();
    let error = undo(&mut setup).unwrap_err();
    assert!(matches!(error, Error::Refusing(_)), "{error}");
}

#[test]
fn a_photo_changed_by_something_else_is_not_undone() {
    let mut setup = Setup::new("undo-drifted");
    let target = setup.target(BARE, Change::of([Field::Rating(Some(4))]));
    let written = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &[target],
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();

    let meddled = std::process::Command::new("exiftool")
        .args(["-overwrite_original", "-q", "-XMP-xmp:Rating=1"])
        .arg(setup.path(BARE))
        .status()
        .unwrap();
    assert!(meddled.success());

    let undone = setup
        .engine
        .undo(
            &mut setup.journal,
            &mut setup.cache,
            written.batch,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(undone.refused, 1, "{undone:?}");
    assert_eq!(undone.written, 0);
    assert_eq!(
        setup.field(BARE, "XMP-xmp:Rating"),
        Some(Value::from(1)),
        "it was left as it was found"
    );
}

// Phase 4 - the canonical field set

#[test]
fn one_tag_lands_in_all_five_fields_with_every_ancestor() {
    let mut setup = Setup::new("five-fields");
    assert_eq!(
        setup.write(
            BARE,
            Change::of([Field::Tags(vec!["places/inChina/Beijing".to_string()])])
        ),
        Outcome::Written
    );

    let fields = setup.look(BARE);
    let paths = Value::from(vec!["places", "places/inChina", "places/inChina/Beijing"]);
    assert_eq!(fields.get("XMP-digiKam:TagsList"), Some(&paths));
    assert_eq!(fields.get("XMP-microsoft:LastKeywordXMP"), Some(&paths));
    assert_eq!(
        fields.get("XMP-lr:HierarchicalSubject"),
        Some(&Value::from(vec!["places", "places|inChina", "places|inChina|Beijing"]))
    );
    let names = Value::from(vec!["Beijing", "inChina", "places"]);
    assert_eq!(fields.get("XMP-dc:Subject"), Some(&names));
    assert_eq!(fields.get("IPTC:Keywords"), Some(&names));
}

#[test]
fn the_same_change_twice_touches_the_file_once() {
    let mut setup = Setup::new("idempotent");
    let change = || {
        Change::of([
            Field::Tags(vec!["events/2018 Wedding Trip".to_string()]),
            Field::Rating(Some(3)),
            Field::Taken(Some(Taken {
                at: "2018-10-06 14:03:40".to_string(),
                offset: Some("+02:00".to_string()),
            })),
        ])
    };
    assert_eq!(setup.write(BARE, change()), Outcome::Written);
    let after = std::fs::read(setup.path(BARE)).unwrap();

    assert_eq!(setup.write(BARE, change()), Outcome::Skipped);
    assert_eq!(std::fs::read(setup.path(BARE)).unwrap(), after, "it was written again");

    let passes = setup.journal.passes(10).unwrap();
    assert_eq!(passes[0].written, 0, "the second pass wrote nothing down either");
}

#[test]
fn a_date_without_an_offset_is_written_without_a_zone_made_up() {
    let mut setup = Setup::new("no-offset");
    let change = Change::of([Field::Taken(Some(Taken {
        at: "2019-07-13 21:00:00".to_string(),
        offset: None,
    }))]);
    assert_eq!(setup.write(BARE, change), Outcome::Written);

    let fields = setup.look(BARE);
    assert_eq!(
        fields.get("ExifIFD:DateTimeOriginal"),
        Some(&Value::from("2019:07:13 21:00:00"))
    );
    assert_eq!(fields.get("ExifIFD:OffsetTimeOriginal"), None);
    assert_eq!(fields.get("IPTC:DateCreated"), Some(&Value::from("2019:07:13")));
    assert_eq!(
        fields.get("IPTC:TimeCreated"),
        None,
        "IPTC's time needs a zone, and this photo has none to give"
    );
}

#[test]
fn a_position_a_date_and_a_region_each_round_trip() {
    let mut setup = Setup::new("round-trip");
    let change = Change::of([
        Field::Gps(Some(Gps {
            lat: -33.8568,
            lon: -70.6693,
            altitude: Some(520.5),
        })),
        Field::Taken(Some(Taken {
            at: "2006-09-14 10:12:00".to_string(),
            offset: Some("+08:00".to_string()),
        })),
        Field::Faces(Some(Faces {
            width: 640,
            height: 480,
            faces: vec![Face {
                name: "Koch, Daniel".to_string(),
                x: 0.5,
                y: 0.4,
                width: 0.2,
                height: 0.3,
            }],
        })),
        Field::Place(Some(Place {
            city: Some("Santiago".to_string()),
            country: Some("Chile".to_string()),
            country_code: Some("CL".to_string()),
            ..Place::default()
        })),
    ]);
    assert_eq!(setup.write(BARE, change), Outcome::Written);

    let fields = setup.look(BARE);
    assert_eq!(fields.get("GPS:GPSLatitude"), Some(&Value::from(33.8568)));
    assert_eq!(fields.get("GPS:GPSLatitudeRef"), Some(&Value::from("S")));
    assert_eq!(fields.get("GPS:GPSLongitudeRef"), Some(&Value::from("W")));
    assert_eq!(fields.get("GPS:GPSAltitude"), Some(&Value::from(520.5)));
    assert_eq!(fields.get("GPS:GPSMapDatum"), Some(&Value::from("WGS-84")));

    assert_eq!(
        fields.get("ExifIFD:DateTimeOriginal"),
        Some(&Value::from("2006:09:14 10:12:00"))
    );
    assert_eq!(fields.get("ExifIFD:OffsetTimeOriginal"), Some(&Value::from("+08:00")));
    assert_eq!(
        fields.get("XMP-xmp:CreateDate"),
        Some(&Value::from("2006:09:14 10:12:00+08:00"))
    );
    assert_eq!(fields.get("IPTC:TimeCreated"), Some(&Value::from("10:12:00+08:00")));

    let region = fields.get("XMP-mwg-rs:RegionInfo").expect("a region");
    assert_eq!(region.pointer("/RegionList/0/Name"), Some(&Value::from("Koch, Daniel")));
    assert_eq!(region.pointer("/RegionList/0/Area/X"), Some(&Value::from(0.5)));
    assert_eq!(region.pointer("/AppliedToDimensions/W"), Some(&Value::from(640)));
    assert_eq!(
        fields.get("XMP-iptcExt:PersonInImage"),
        Some(&Value::from(vec!["Koch, Daniel"])),
        "read with -struct, a list of one is still a list"
    );

    assert_eq!(fields.get("XMP-photoshop:City"), Some(&Value::from("Santiago")));
    assert_eq!(fields.get("IPTC:City"), Some(&Value::from("Santiago")));
    assert_eq!(fields.get("XMP-iptcCore:CountryCode"), Some(&Value::from("CL")));
}

#[test]
fn a_non_ascii_tag_and_file_name_survive() {
    let mut setup = Setup::new("non-ascii");
    let rel_path = "Germany/2019-07-13 Sommerfest/Kyffhäuser Ausflug.jpg";
    std::fs::copy(setup.path(BARE), setup.path(rel_path)).unwrap();

    let tag = "places/inDeutschland/Kyffhäuser".to_string();
    assert_eq!(
        setup.write(rel_path, Change::of([Field::Tags(vec![tag.clone()])])),
        Outcome::Written
    );

    let fields = setup.look(rel_path);
    assert_eq!(
        fields.get("XMP-digiKam:TagsList"),
        Some(&Value::from(vec![
            "places",
            "places/inDeutschland",
            "places/inDeutschland/Kyffhäuser"
        ]))
    );
    assert_eq!(
        fields.get("IPTC:Keywords"),
        Some(&Value::from(vec!["Kyffhäuser", "inDeutschland", "places"]))
    );
}

#[test]
fn clearing_a_field_removes_it() {
    let mut setup = Setup::new("clearing");
    assert!(setup.field(TAGGED, "XMP-digiKam:TagsList").is_some());

    let change = Change::of([
        Field::Tags(vec![]),
        Field::Gps(None),
        Field::Rating(None),
        Field::DropLabel,
        Field::DropCatalogSets,
    ]);
    assert_eq!(setup.write(TAGGED, change), Outcome::Written);

    let fields = setup.look(TAGGED);
    for key in [
        "XMP-digiKam:TagsList",
        "XMP-lr:HierarchicalSubject",
        "XMP-microsoft:LastKeywordXMP",
        "XMP-dc:Subject",
        "IPTC:Keywords",
        "XMP-xmp:Rating",
        "XMP-xmp:Label",
        "GPS:GPSLatitude",
    ] {
        assert_eq!(fields.get(key), None, "{key} is still there");
    }
}

#[test]
fn a_label_that_was_a_keyword_is_taken_away() {
    let mut setup = Setup::new("label");
    let rel_path = "Germany/2019-07-13 Sommerfest/p1000003.jpg";
    assert_eq!(setup.field(rel_path, "XMP-xmp:Label"), Some(Value::from("timeline")));
    assert_eq!(setup.write(rel_path, Change::of([Field::DropLabel])), Outcome::Written);
    assert_eq!(setup.field(rel_path, "XMP-xmp:Label"), None);
    assert_eq!(
        setup.write(rel_path, Change::of([Field::DropLabel])),
        Outcome::Skipped,
        "there is nothing left to take away"
    );
}

// Phase 5 - many photos

#[test]
fn a_batch_writes_every_photo_and_changes_nothing_else() {
    let mut setup = Setup::new("batch");
    let before = setup.image_data();
    let targets: Vec<Target> = crate::fixtures::photo_paths()
        .iter()
        .map(|rel_path| setup.target(rel_path, Change::of([Field::Rating(Some(3))])))
        .collect();

    let summary = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &targets,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(summary.written, crate::fixtures::photo_count(), "{summary:?}");
    assert_eq!(summary.photos(), targets.len(), "the summary adds up");
    assert_eq!(summary.refused + summary.failed, 0);
    assert!(!summary.cancelled);
    assert_eq!(setup.image_data(), before, "the image data of the library moved");
    assert!(setup.leftovers().is_empty());

    for rel_path in crate::fixtures::photo_paths() {
        assert_eq!(
            setup.field(rel_path, "XMP-xmp:Rating"),
            Some(Value::from(3)),
            "{rel_path}"
        );
    }
    assert_eq!(
        setup.journal.pass(summary.batch).unwrap().written as usize,
        targets.len()
    );
}

#[test]
fn one_broken_photo_does_not_stop_the_others() {
    let mut setup = Setup::new("one-broken");
    let broken = setup.path("China/broken.jpg");
    std::fs::write(&broken, b"not an image").unwrap();

    let mut targets = vec![Target {
        path: broken,
        content_id: "0".repeat(32),
        change: Change::of([Field::Rating(Some(3))]),
    }];
    targets.extend(
        [BARE, TAGGED]
            .iter()
            .map(|rel_path| setup.target(rel_path, Change::of([Field::Rating(Some(3))]))),
    );

    let summary = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &targets,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(summary.refused, 1, "{summary:?}");
    assert_eq!(summary.written, 2);
    assert_eq!(summary.photos(), 3);
    for rel_path in [BARE, TAGGED] {
        assert_eq!(setup.field(rel_path, "XMP-xmp:Rating"), Some(Value::from(3)));
    }
}

#[test]
fn a_cancelled_batch_stops_between_photos() {
    let mut setup = Setup::new("cancelled");
    let paths = crate::fixtures::photo_paths();
    let targets: Vec<Target> = paths
        .iter()
        .map(|rel_path| setup.target(rel_path, Change::of([Field::Rating(Some(1))])))
        .collect();

    let cancel = AtomicBool::new(false);
    let stop = |done: usize, _total: usize| {
        if done == 2 {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let summary = setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &targets,
            &stop,
            &cancel,
        )
        .unwrap();

    assert!(summary.cancelled, "{summary:?}");
    assert_eq!(summary.written, 2);
    assert_eq!(summary.photos(), 2, "it stopped between photos");
    assert!(setup.leftovers().is_empty());

    let pass = setup.journal.pass(summary.batch).unwrap();
    assert_eq!(pass.written, 2);
    assert!(pass.finished_at.is_some(), "the batch was closed off");
    assert!(
        setup.journal.unfinished().unwrap().is_empty(),
        "every entry got an outcome"
    );
    assert_eq!(setup.field(paths[3], "XMP-xmp:Rating"), None, "it never got there");
}

#[test]
fn a_batch_of_one_process_does_not_start_one_per_photo() {
    let mut setup = Setup::new("one-process");
    let targets: Vec<Target> = [BARE, TAGGED, RATED]
        .iter()
        .map(|rel_path| setup.target(rel_path, Change::of([Field::Rating(Some(2))])))
        .collect();
    setup
        .engine
        .write(
            &mut setup.journal,
            &mut setup.cache,
            "Pass",
            None,
            &targets,
            &quiet(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(
        setup.engine.tool.commands() >= 6,
        "a read and a write per photo at least"
    );
}
