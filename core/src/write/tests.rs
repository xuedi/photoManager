//! Every test here writes to copies of the stand-in library. Nothing in this file, and nothing it
//! calls, may ever be pointed at a real photo.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::metadata::Reader;

const RATED: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG";
const BARE: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";
const TAGGED: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";

struct Setup {
    root: PathBuf,
    engine: Engine,
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
        self.engine.write_one(&mut self.cache, &target).unwrap()
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

// One photo, written safely

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
    let id = content_id(&before).unwrap();
    let mut intent = setup
        .engine
        .intent(&file, &id, &Change::of([Field::Rating(Some(3))]))
        .unwrap();
    intent.want = vec![wrong];
    let outcome = setup.engine.put(&mut setup.cache, &file, intent, false).unwrap();

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
    let outcome = setup.engine.write_one(&mut setup.cache, &target).unwrap();
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
    let outcome = setup.engine.write_one(&mut setup.cache, &target).unwrap();
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
    let outcome = setup.engine.write_one(&mut setup.cache, &target).unwrap();

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
    let outcome = setup.engine.write_one(&mut setup.cache, &target).unwrap();
    match &outcome {
        Outcome::Refused(why) => assert!(why.contains("built against"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// The canonical field set

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
fn a_person_without_a_box_leaves_the_boxes_as_they_were() {
    let mut setup = Setup::new("persons");
    let boxed = Change::of([Field::Faces(Some(Faces {
        width: 640,
        height: 480,
        faces: vec![Face {
            name: "Ben".to_string(),
            x: 0.5,
            y: 0.4,
            width: 0.2,
            height: 0.3,
        }],
        persons: Vec::new(),
    }))]);
    assert_eq!(setup.write(BARE, boxed), Outcome::Written);
    let regions = setup.field(BARE, "XMP-mwg-rs:RegionInfo").expect("a region");

    let persons = |names: &[&str]| Change::of([Field::Persons(names.iter().map(|name| name.to_string()).collect())]);
    assert_eq!(setup.write(BARE, persons(&["Ben", "Mia"])), Outcome::Written);
    assert_eq!(
        setup.field(BARE, "XMP-mwg-rs:RegionInfo"),
        Some(regions),
        "the box is as it was"
    );
    assert_eq!(
        setup.field(BARE, "XMP-iptcExt:PersonInImage"),
        Some(Value::from(vec!["Ben", "Mia"]))
    );
    assert_eq!(
        setup.write(BARE, persons(&["Mia", "Ben"])),
        Outcome::Skipped,
        "the same persons in another order"
    );
    assert!(
        matches!(setup.write(BARE, persons(&["Mia", "Anna"])), Outcome::Refused(why) if why.contains("Ben")),
        "a name the photo has is never left out"
    );

    let path = setup.path(BARE);
    let read = crate::metadata::Exiv2
        .read(&path, &std::fs::read(&path).unwrap())
        .unwrap();
    assert_eq!(
        read.regions.unwrap().named(),
        [("Ben".to_string(), true), ("Mia".to_string(), false)],
        "the scan reads her as a person without a box"
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
            derived: None,
        })),
        Field::Taken(Some(Taken {
            at: "2006-09-14 10:12:00".to_string(),
            offset: Some("+08:00".to_string()),
        })),
        Field::Faces(Some(Faces {
            width: 640,
            height: 480,
            faces: vec![Face {
                name: "Park, Lena".to_string(),
                x: 0.5,
                y: 0.4,
                width: 0.2,
                height: 0.3,
            }],
            persons: Vec::new(),
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
    assert_eq!(region.pointer("/RegionList/0/Name"), Some(&Value::from("Park, Lena")));
    assert_eq!(region.pointer("/RegionList/0/Area/X"), Some(&Value::from(0.5)));
    assert_eq!(region.pointer("/AppliedToDimensions/W"), Some(&Value::from(640)));
    assert_eq!(
        fields.get("XMP-iptcExt:PersonInImage"),
        Some(&Value::from(vec!["Park, Lena"])),
        "read with -struct, a list of one is still a list"
    );

    assert_eq!(fields.get("XMP-photoshop:City"), Some(&Value::from("Santiago")));
    assert_eq!(fields.get("IPTC:City"), Some(&Value::from("Santiago")));
    assert_eq!(fields.get("XMP-iptcCore:CountryCode"), Some(&Value::from("CL")));
}

#[test]
fn a_derived_position_is_written_and_proved() {
    let mut setup = Setup::new("derived");
    let image = setup.image_data();
    let hash = setup.exiftool_image_hash(TAGGED);

    let derived = Change::of([Field::Gps(Some(Gps {
        lat: 39.9042,
        lon: 116.4074,
        altitude: None,
        derived: Some(change::Derived {
            method: "photoManager: places tag",
            metres: 5000.0,
        }),
    }))]);
    let target = setup.target(TAGGED, derived);
    let written = setup
        .engine
        .write(&mut setup.cache, "Derive", &[target], &quiet(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(written.written, 1, "{written:?}");

    let fields = setup.look(TAGGED);
    assert_eq!(fields.get("GPS:GPSLatitude"), Some(&Value::from(39.9042)));
    assert_eq!(
        fields.get("GPS:GPSProcessingMethod"),
        Some(&Value::from("photoManager: places tag"))
    );
    assert_eq!(fields.get("GPS:GPSHPositioningError"), Some(&Value::from(5000)));
    assert_eq!(setup.exiftool_image_hash(TAGGED), hash);
    assert_eq!(setup.image_data(), image, "the image data moved");
}

#[test]
fn a_measured_position_writes_no_mark() {
    let mut setup = Setup::new("measured");
    let measured = Change::of([Field::Gps(Some(Gps {
        lat: 39.9042,
        lon: 116.4074,
        altitude: None,
        derived: None,
    }))]);
    assert_eq!(setup.write(BARE, measured), Outcome::Written);
    let fields = setup.look(BARE);
    assert!(fields.contains_key("GPS:GPSLatitude"));
    assert_eq!(fields.get("GPS:GPSProcessingMethod"), None);
    assert_eq!(fields.get("GPS:GPSHPositioningError"), None);
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
    assert_eq!(fields.get("IPTC:CodedCharacterSet"), Some(&Value::from("\u{1b}%G")));

    let plain = std::process::Command::new("exiftool")
        .args(["-s3", "-IPTC:Keywords"])
        .arg(setup.path(rel_path))
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&plain.stdout).contains("Kyffhäuser"));
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

// Many photos

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
        .write(&mut setup.cache, "Pass", &targets, &quiet(), &AtomicBool::new(false))
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
        .write(&mut setup.cache, "Pass", &targets, &quiet(), &AtomicBool::new(false))
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
        .write(&mut setup.cache, "Pass", &targets, &stop, &cancel)
        .unwrap();

    assert!(summary.cancelled, "{summary:?}");
    assert_eq!(summary.written, 2);
    assert_eq!(summary.photos(), 2, "it stopped between photos");
    assert!(setup.leftovers().is_empty());
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
        .write(&mut setup.cache, "Pass", &targets, &quiet(), &AtomicBool::new(false))
        .unwrap();
    assert!(
        setup.engine.tool.commands() >= 6,
        "a read and a write per photo at least"
    );
}

// Moves - a folder or a photo to another place in the library

const BEN: &str = "China/2006-09-00 Besuch Ben";
const BEN_IN_CITY: &str = "China/Beijing/2006-09-00 Besuch Ben";

impl Setup {
    fn scan(&mut self) {
        let thumbs = crate::thumbs::Thumbs::new(self.root.parent().unwrap().join("thumbs"));
        crate::scan::run(
            &mut self.cache,
            &self.root,
            &crate::metadata::Exiv2,
            &thumbs,
            crate::scan::Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
    }

    /// A move of everything under `from` as the files are now.
    fn moving(&self, from: &str, to: &str) -> Move {
        let mut photos = Vec::new();
        for entry in walkdir::WalkDir::new(self.path(from)).sort_by_file_name() {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                let rel_path = entry.path().strip_prefix(&self.root).unwrap().display().to_string();
                let bytes = std::fs::read(entry.path()).unwrap();
                photos.push((rel_path, content_id(&bytes).unwrap()));
            }
        }
        Move {
            from: from.to_string(),
            to: to.to_string(),
            photos,
        }
    }

    fn relocate(&mut self, moves: &[Move]) -> Summary {
        self.engine
            .relocate(
                &mut self.cache,
                "Folder Migration",
                moves,
                &quiet(),
                &AtomicBool::new(false),
            )
            .unwrap()
    }

    /// Every file with its image data and its modification time, by path.
    fn files(&self) -> BTreeMap<String, (String, std::time::SystemTime)> {
        let mut all = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&self.root) {
            let entry = entry.unwrap();
            if entry.file_type().is_file() {
                let rel_path = entry.path().strip_prefix(&self.root).unwrap().display().to_string();
                let bytes = std::fs::read(entry.path()).unwrap();
                let mtime = entry.metadata().unwrap().modified().unwrap();
                all.insert(rel_path, (content_id(&bytes).unwrap_or_default(), mtime));
            }
        }
        all
    }
}

fn under(
    files: &BTreeMap<String, (String, std::time::SystemTime)>,
    from: &str,
    to: &str,
) -> BTreeMap<String, (String, std::time::SystemTime)> {
    files
        .iter()
        .map(|(path, file)| {
            let moved = match path.strip_prefix(from) {
                Some(rest) if rest.starts_with('/') => format!("{to}{rest}"),
                _ => path.clone(),
            };
            (moved, file.clone())
        })
        .collect()
}

#[test]
fn an_event_moves_into_its_city_with_every_photo_as_it_was() {
    let mut setup = Setup::new("move");
    setup.scan();
    let before = setup.files();

    let moved = setup.relocate(&[setup.moving(BEN, BEN_IN_CITY)]);
    assert_eq!((moved.written, moved.refused, moved.failed), (1, 0, 0), "{moved:?}");
    assert_eq!(moved.outcomes, [(BEN.to_string(), Outcome::Written)]);
    assert!(!setup.path(BEN).exists());
    assert_eq!(
        setup.files(),
        under(&before, BEN, BEN_IN_CITY),
        "every photo, its image data and its mtime"
    );

    let sub = format!("{BEN_IN_CITY}/2006-08-21/P1000002.JPG");
    assert!(setup.cache.known(&sub).unwrap().is_some(), "the cache rows followed");
    assert_eq!(
        setup.cache.event_dirs(std::slice::from_ref(&sub)).unwrap()[&sub],
        BEN_IN_CITY
    );
}

#[test]
fn a_target_that_is_there_already_refuses() {
    let mut setup = Setup::new("move-target-there");
    let there = "Germany/Hamburg/2014-08-00 Wedding";
    let one = setup.moving("Germany/2019-07-13 Sommerfest", there);
    let before = setup.files();
    let moved = setup.relocate(&[one]);
    assert_eq!(moved.refused, 1, "{moved:?}");
    assert!(matches!(&moved.outcomes[0].1, Outcome::Refused(why) if why.contains("is there already")));
    assert_eq!(setup.files(), before);
}

#[test]
fn a_file_the_move_does_not_know_refuses() {
    let mut setup = Setup::new("move-unknown-file");
    let one = setup.moving(BEN, BEN_IN_CITY);
    std::fs::write(setup.path(&format!("{BEN}/2006-08-21/notes.txt")), "a stray").unwrap();
    let moved = setup.relocate(std::slice::from_ref(&one));
    assert!(
        matches!(&moved.outcomes[0].1, Outcome::Refused(why) if why.contains("notes.txt is not a photo the scan knows")),
        "{moved:?}"
    );
    assert!(setup.path(BEN).exists());
    assert!(!setup.path("China/Beijing").exists(), "no folder was made for it");

    std::fs::remove_file(setup.path(&format!("{BEN}/2006-08-21/notes.txt"))).unwrap();
    std::fs::remove_file(setup.path(&format!("{BEN}/P1000001.JPG"))).unwrap();
    let moved = setup.relocate(&[one]);
    assert!(
        matches!(&moved.outcomes[0].1, Outcome::Refused(why) if why.contains("P1000001.JPG is no longer there")),
        "{moved:?}"
    );
}

#[test]
fn a_path_that_leaves_the_library_refuses() {
    let mut setup = Setup::new("move-outside");
    for to in ["../elsewhere/Ben", "/tmp/Ben", "China/./Ben", ""] {
        let one = setup.moving(BEN, to);
        let moved = setup.relocate(&[one]);
        assert_eq!(moved.refused, 1, "{to}: {moved:?}");
    }
    let into_itself = setup.moving(BEN, &format!("{BEN}/deeper"));
    assert_eq!(setup.relocate(&[into_itself]).refused, 1);
    assert!(setup.path(BEN).exists());
}

#[test]
fn a_loose_photo_moves_into_an_event_and_the_country_folder_stays() {
    let mut setup = Setup::new("move-loose");
    setup.scan();
    let to = format!("{BEN}/IMG_3140.JPG");
    let moved = setup.relocate(&[setup.moving("China/IMG_3140.JPG", &to)]);
    assert_eq!(moved.written, 1, "{moved:?}");
    assert!(setup.path(&to).is_file());
    assert!(setup.path("China").is_dir(), "a folder that is not empty is kept");
    assert!(setup.cache.known(&to).unwrap().is_some());
}

#[test]
fn the_old_parent_goes_when_the_move_left_it_empty() {
    let mut setup = Setup::new("move-prune");
    let moved = setup.relocate(&[setup.moving("Ireland/2008-10-03 Galway", "Netherlands/2008-10-03 Galway")]);
    assert_eq!(moved.written, 1, "{moved:?}");
    assert!(!setup.path("Ireland").exists(), "the country it left empty");
    assert!(
        setup.path("Netherlands/2008-10-03 Galway/Kira/IMG_0002.JPG").is_file(),
        "sub-folders go with it"
    );
}

// A maker note ExifTool doubts

const DOUBTED: &str = "Germany/2019-07-13 Sommerfest/doubted.jpg";
const SOMEWHERE: Gps = Gps {
    lat: 53.55,
    lon: 9.99,
    altitude: None,
    derived: None,
};

fn with_doubted(name: &str) -> Setup {
    let setup = Setup::new(name);
    std::fs::write(setup.path(DOUBTED), crate::fixtures::with_doubted_maker_note(None)).unwrap();
    setup
}

#[test]
fn a_doubted_maker_note_is_left_as_it_was_and_said_so() {
    let mut setup = with_doubted("doubted");
    let before = std::fs::read(setup.path(DOUBTED)).unwrap();
    let outcome = setup.write(DOUBTED, Change::of([Field::Gps(Some(SOMEWHERE))]));
    assert_eq!(outcome, Outcome::Doubted("Truncated MakerNotes directory".to_string()));
    assert_eq!(
        std::fs::read(setup.path(DOUBTED)).unwrap(),
        before,
        "not a byte changed"
    );
    assert!(setup.leftovers().is_empty());
}

#[test]
fn written_anyway_a_maker_note_that_would_lose_bytes_is_left_as_it_was() {
    let mut setup = with_doubted("anyway");
    let path = setup.path(DOUBTED);
    let before = std::fs::read(&path).unwrap();
    let note = setup.engine.maker_note(&path).unwrap();
    assert!(note.keys().any(|key| key.ends_with("SpecialMode")), "{note:?}");
    assert!(
        note.iter()
            .any(|(key, value)| key.ends_with("MakerNoteOlympus") && value.as_u64().is_some()),
        "the whole maker note is read as its length: {note:?}"
    );
    let target = setup.target(DOUBTED, Change::of([Field::Gps(Some(SOMEWHERE))]));
    let summary = setup
        .engine
        .write_anyway(&mut setup.cache, "anyway", &[target], &quiet(), &AtomicBool::new(false))
        .unwrap();
    let [(_, Outcome::Failed(why))] = summary.outcomes.as_slice() else {
        panic!("{:?}", summary.outcomes);
    };
    assert!(why.contains("the maker note would lose"), "{why}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "not a byte changed");
    assert!(setup.leftovers().is_empty());
}

#[test]
fn only_a_minor_maker_note_problem_is_doubted() {
    assert!(doubted("[minor] Truncated MakerNotes directory - a.jpg"));
    assert!(doubted(
        "[minor] MakerNotes offsets may be incorrect (fix or ignore?) - a.jpg"
    ));
    assert!(doubted("[minor] Maker notes could not be parsed - a.jpg"));
    assert!(!doubted("Truncated MakerNotes directory - a.jpg"), "not minor");
    assert!(
        !doubted("[minor] Bad format (16) for IFD0 entry 5 - a.jpg"),
        "not the maker note"
    );
    assert!(!doubted("Error opening file - a.jpg"));
    assert_eq!(
        reason("[minor] MakerNotes offsets may be incorrect (fix or ignore?) - /x/.a.jpg.writing-1"),
        "MakerNotes offsets may be incorrect (fix or ignore?)"
    );
}

#[test]
fn the_proof_ignores_where_a_block_sits_and_nothing_else() {
    let reading = |pairs: &[(&str, Value)]| -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    };
    let before = reading(&[
        ("Casio:PreviewImageStart", Value::from(7034)),
        ("Casio:PreviewImage", Value::from("base64:AAAA")),
        ("Casio:ISO", Value::from(800)),
    ]);
    let moved = reading(&[
        ("Casio:PreviewImageStart", Value::from(13676)),
        ("Casio:PreviewImage", Value::from("base64:AAAA")),
        ("Casio:ISO", Value::from(800)),
    ]);
    assert!(differs(&before, &moved).is_empty());
    assert_eq!(decoded_length("UVZDAAAA"), 6);
    assert_eq!(decoded_length("UVZDAA=="), 4);
    let changed = reading(&[
        ("Casio:PreviewImageStart", Value::from(13676)),
        ("Casio:PreviewImage", Value::from("base64:AAAB")),
    ]);
    assert_eq!(differs(&before, &changed), ["Casio:ISO", "Casio:PreviewImage"]);
}

#[test]
fn an_event_is_written_read_back_by_both_readers_and_taken_away() {
    let mut setup = Setup::new("event");
    let name = "Summer Party, the garden (late)";
    let change = || Change::of([Field::Event(Some(name.to_string()))]);
    assert_eq!(setup.write(BARE, change()), Outcome::Written);
    assert_eq!(setup.field(BARE, "XMP-iptcExt:Event"), Some(Value::from(name)));

    let bytes = std::fs::read(setup.path(BARE)).unwrap();
    let fast = crate::metadata::Exiv2.read(&setup.path(BARE), &bytes).unwrap();
    let reference = crate::metadata::ExifTool.read(&setup.path(BARE), &bytes).unwrap();
    assert_eq!(fast.event.as_deref(), Some(name), "the scan strips the language");
    assert_eq!(reference.event, fast.event);

    assert_eq!(
        setup.write(BARE, change()),
        Outcome::Skipped,
        "a second write is settled"
    );

    assert_eq!(setup.write(BARE, Change::of([Field::Event(None)])), Outcome::Written);
    assert_eq!(setup.field(BARE, "XMP-iptcExt:Event"), None);
}

#[test]
fn an_event_without_a_name_is_refused() {
    for name in ["", "  ", "one\ntwo"] {
        assert!(
            Change::of([Field::Event(Some(name.to_string()))]).assigns().is_err(),
            "{name:?}"
        );
    }
}
