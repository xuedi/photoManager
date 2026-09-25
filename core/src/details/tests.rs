use super::*;

#[test]
fn a_date_round_trips_and_a_bad_one_says_why() {
    assert_eq!(
        parse_date(" 2019-07-13 18:20:00 ").unwrap().as_deref(),
        Some("2019-07-13 18:20:00")
    );
    assert_eq!(parse_date("").unwrap(), None, "empty is no date");
    assert_eq!(
        parse_date("2024-02-29 00:00:00").unwrap().as_deref(),
        Some("2024-02-29 00:00:00")
    );

    let month = parse_date("2019-13-01 10:00:00").unwrap_err();
    assert!(month.contains("no month 13"), "{month}");
    let day = parse_date("2023-02-29 10:00:00").unwrap_err();
    assert!(day.contains("no day 29"), "{day}");
    assert!(parse_date("2019-07-13 24:00:00").is_err());
    assert!(parse_date("13.07.2019 18:20").is_err(), "one date format only");
    assert!(parse_date("2019:07:13 18:20:00").is_err(), "not the EXIF spelling");
}

#[test]
fn an_offset_round_trips_and_a_bad_one_says_why() {
    assert_eq!(parse_offset("+02:00").unwrap().as_deref(), Some("+02:00"));
    assert_eq!(parse_offset(" -05:30 ").unwrap().as_deref(), Some("-05:30"));
    assert_eq!(parse_offset("").unwrap(), None);
    assert!(parse_offset("02:00").is_err(), "the sign is needed");
    assert!(parse_offset("+2:00").is_err());
    let far = parse_offset("+15:00").unwrap_err();
    assert!(far.contains("further from UTC"), "{far}");
}

#[test]
fn coordinates_round_trip_and_bad_ones_say_why() {
    let (lat, lon) = parse_position("53.5511, 9.9937").unwrap().unwrap();
    assert_eq!((lat, lon), (53.5511, 9.9937));
    assert_eq!(parse_position(&position(lat, lon)).unwrap(), Some((lat, lon)));
    assert_eq!(parse_position("-33.8688 151.2093").unwrap(), Some((-33.8688, 151.2093)));
    assert_eq!(parse_position("  ").unwrap(), None);

    let north = parse_position("91, 10").unwrap_err();
    assert!(north.contains("latitude of 91"), "{north}");
    let swapped = parse_position("151.2093, -33.8688").unwrap_err();
    assert!(swapped.contains("longitude came first"), "{swapped}");
    let east = parse_position("10, 181").unwrap_err();
    assert!(east.contains("longitude of 181"), "{east}");
    assert!(parse_position("53.5511").is_err());
    assert!(parse_position("north, east").is_err());
}

#[test]
fn people_come_from_both_roots_and_from_face_regions() {
    let tags: Vec<String> = [
        "people",
        "people/family",
        "people/family/Anna",
        "People/Kira",
        "places/inGermany",
    ]
    .map(String::from)
    .to_vec();
    let raw = vec![
        (
            "Xmp.mwg-rs.Regions/mwg-rs:RegionList[1]/mwg-rs:Name".to_string(),
            "Ben".to_string(),
        ),
        (
            "Xmp.mwg-rs.Regions/mwg-rs:RegionList[1]/mwg-rs:Type".to_string(),
            "Face".to_string(),
        ),
        (PERSON_IN_IMAGE.to_string(), "Anna, Lu".to_string()),
    ];
    assert_eq!(
        people(&tags, &raw),
        ["Anna", "Ben", "Kira", "Lu"],
        "a group is not a person, a name is only listed once"
    );
}

#[test]
fn an_altitude_is_read_from_a_rational() {
    let raw = |value: &str, below: &str| {
        vec![
            (ALTITUDE.to_string(), value.to_string()),
            (ALTITUDE_REF.to_string(), below.to_string()),
        ]
    };
    assert_eq!(altitude(&raw("1234/10", "0")), Some(123.4));
    assert_eq!(altitude(&raw("20/1", "1")), Some(-20.0));
    assert_eq!(altitude(&raw("1/0", "0")), None);
    assert_eq!(altitude(&[]), None);
}

fn sample() -> Details {
    Details {
        rel_path: "Germany/2019-07-13 Sommerfest/img_0657.jpg".to_string(),
        taken_at: Some("2019-07-13 18:20:00".to_string()),
        gps: Some((53.5511, 9.9937)),
        altitude: Some(12.0),
        place: Place {
            city: Some("Hamburg".to_string()),
            ..Place::default()
        },
        tags: vec!["places/inGermany/Hamburg".to_string()],
        rating: Some(2),
        ..Details::default()
    }
}

#[test]
fn an_untouched_form_is_an_empty_change() {
    let details = sample();
    assert!(details.change_to(&details.edited()).unwrap().is_empty());

    let mut edited = details.edited();
    edited.tags.reverse();
    edited.taken_at.push(' ');
    edited.place.city = Some(" Hamburg ".to_string());
    assert!(
        details.change_to(&edited).unwrap().is_empty(),
        "spaces and order are not a change"
    );
}

#[test]
fn one_edited_field_is_one_field() {
    let details = sample();
    let mut edited = details.edited();
    edited.taken_at = "2019-07-13 18:21:00".to_string();
    assert_eq!(
        details.change_to(&edited).unwrap().fields,
        [Field::Taken(Some(Taken {
            at: "2019-07-13 18:21:00".to_string(),
            offset: None,
        }))]
    );

    let mut edited = details.edited();
    edited.position = "53.6, 10.0".to_string();
    assert_eq!(
        details.change_to(&edited).unwrap().fields,
        [Field::Gps(Some(Gps {
            lat: 53.6,
            lon: 10.0,
            altitude: Some(12.0),
            derived: None,
        }))],
        "a moved point keeps its altitude"
    );

    let mut edited = details.edited();
    edited.tags.push("people/family/Anna".to_string());
    edited.rating = Some(4);
    assert_eq!(
        details.change_to(&edited).unwrap().fields,
        [
            Field::Tags(vec![
                "people/family/Anna".to_string(),
                "places/inGermany/Hamburg".to_string()
            ]),
            Field::Rating(Some(4)),
        ],
        "the whole tag set, the way the engine wants it"
    );
}

#[test]
fn an_emptied_field_is_a_removal() {
    let details = sample();
    let mut edited = details.edited();
    edited.taken_at.clear();
    edited.position.clear();
    edited.place.city = Some(String::new());
    edited.tags.clear();
    edited.rating = None;
    assert_eq!(
        details.change_to(&edited).unwrap().fields,
        [
            Field::Taken(None),
            Field::Gps(None),
            Field::Place(None),
            Field::Tags(Vec::new()),
            Field::Rating(None),
        ]
    );
}

#[test]
fn a_form_that_does_not_parse_is_refused() {
    let details = sample();
    let mut edited = details.edited();
    edited.taken_at = "2019-13-13 18:20:00".to_string();
    assert!(details.change_to(&edited).is_err());

    let mut edited = details.edited();
    edited.taken_at.clear();
    edited.offset = "+02:00".to_string();
    assert!(details.change_to(&edited).unwrap_err().contains("offset needs a date"));

    let mut edited = details.edited();
    edited.rating = Some(6);
    assert!(details.change_to(&edited).is_err());

    let mut edited = details.edited();
    edited.tags.push("places//Hamburg".to_string());
    assert!(details.change_to(&edited).unwrap_err().contains("empty level"));
}

#[cfg(feature = "fixtures")]
mod on_the_fixture {
    use std::process::Command;
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::metadata::Exiv2;
    use crate::scan::{self, Mode};
    use crate::thumbs::Thumbs;

    const LOCATED: &str = "Germany/2019-07-13 Sommerfest/img_0657.jpg";
    const PEOPLE: &str = "Germany/2019-07-13 Sommerfest/IMAG0001.jpg";
    const KIRA: &str = "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG";
    const OFF: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";

    fn scanned(name: &str, before: impl Fn(&std::path::Path)) -> (std::path::PathBuf, Cache) {
        let base = std::env::temp_dir().join(format!("photomanager-details-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");
        before(&root);
        let mut cache = Cache::open(&base.join("cache.db")).unwrap();
        scan::run(
            &mut cache,
            &root,
            &Exiv2,
            &Thumbs::new(base.join("thumbs")),
            Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .expect("scan the stand-in library");
        (root, cache)
    }

    #[test]
    fn every_photo_has_the_details_of_its_row() {
        let (root, cache) = scanned("rows", |_| {});
        let stated = cache
            .stated(
                &crate::fixtures::photo_paths()
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        for path in crate::fixtures::photo_paths() {
            let details = Details::of(&cache, path).unwrap().expect("a known photo");
            let row = &stated[path];
            assert_eq!(details.size, row.size, "{path}");
            assert_eq!(details.content_id, row.content_id, "{path}");
            assert_eq!(details.taken_at, row.said.taken_at, "{path}");
            assert_eq!(details.rating, row.said.rating, "{path}");
            assert_eq!(details.tags, row.said.tags, "{path}");
            assert_eq!(details.gps, row.said.gps_lat.zip(row.said.gps_lon), "{path}");
            assert_eq!(details.changed.len(), 19, "{path}: the one date format");
            let on_disk = std::fs::metadata(root.join(path)).unwrap().len();
            assert_eq!(details.size, on_disk, "{path}");
        }
        assert!(Details::of(&cache, "nowhere.jpg").unwrap().is_none());

        let located = Details::of(&cache, LOCATED).unwrap().unwrap();
        assert_eq!(located.orientation, Some(6));
        assert_eq!(
            (located.width, located.height),
            (Some(24), Some(16)),
            "stored on its side"
        );
        assert_eq!(located.place_tags(), ["places/inGermany/Hamburg"]);
        assert_eq!(located.folder_date.as_deref(), Some("2019-07-13"));
        assert_eq!(located.agrees, Some(true));
        assert_eq!(located.event.as_deref(), Some("Sommerfest"));

        let off = Details::of(&cache, OFF).unwrap().unwrap();
        assert_eq!(off.folder_date.as_deref(), Some("2006-09"));
        assert_eq!(off.agrees, Some(false), "taken in August, filed under September");

        let undated = Details::of(&cache, "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG")
            .unwrap()
            .unwrap();
        assert_eq!(undated.agrees, None, "nothing to compare");
        assert!(undated.issues.iter().any(|(kind, _)| kind == "no date"));
    }

    #[test]
    fn the_raw_fields_hold_what_exiftool_wrote() {
        let (_, cache) = scanned("raw", |_| {});
        let details = Details::of(&cache, "Ireland/2008-10-03 Galway/IMG_0003.JPG")
            .unwrap()
            .unwrap();
        assert_eq!(details.field("Xmp.photoshop.City"), Some("Galway"));
        assert_eq!(details.field("Iptc.Application2.City"), Some("Galway"));
        assert_eq!(
            details.field("Exif.Photo.DateTimeOriginal"),
            Some("2008:10:03 12:47:00")
        );
        assert_eq!(details.place.city.as_deref(), Some("Galway"));
        let names: Vec<&str> = details.raw.iter().map(|(name, _)| name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "sorted by name");
    }

    #[test]
    fn people_come_from_either_spelling_and_from_regions() {
        let (_, cache) = scanned("people", |root| {
            let status = Command::new("exiftool")
                .args([
                    "-q",
                    "-overwrite_original",
                    "-XMP-mwg-rs:RegionInfo={AppliedToDimensions={W=16,H=16,Unit=pixel},\
                     RegionList=[{Area={X=0.5,Y=0.5,W=0.2,H=0.2,Unit=normalized},Name=Ben,Type=Face}]}",
                ])
                .arg(root.join(LOCATED))
                .status()
                .unwrap();
            assert!(status.success());
        });
        assert_eq!(
            Details::of(&cache, PEOPLE).unwrap().unwrap().people,
            ["Anna", "Tom", "me"]
        );
        assert_eq!(Details::of(&cache, KIRA).unwrap().unwrap().people, ["Kira"]);
        assert_eq!(
            Details::of(&cache, LOCATED).unwrap().unwrap().people,
            ["Ben"],
            "a face region names a person too"
        );
    }

    #[test]
    fn a_derived_position_is_read_into_the_cache_and_said() {
        let (root, cache) = scanned("derived", |root| {
            let status = Command::new("exiftool")
                .args([
                    "-q",
                    "-overwrite_original",
                    "-EXIF:GPSLatitude=39.9042",
                    "-EXIF:GPSLatitudeRef=N",
                    "-EXIF:GPSLongitude=116.4074",
                    "-EXIF:GPSLongitudeRef=E",
                    "-EXIF:GPSProcessingMethod=photoManager: places tag",
                    "-EXIF:GPSHPositioningError=5000",
                ])
                .arg(root.join(OFF))
                .status()
                .unwrap();
            assert!(status.success());
        });
        let derived = Details::of(&cache, OFF).unwrap().unwrap();
        assert_eq!(derived.gps_method.as_deref(), Some("photoManager: places tag"));
        assert_eq!(derived.derived_from(), Some("places tag"));

        let measured = Details::of(&cache, LOCATED).unwrap().unwrap();
        assert!(measured.gps.is_some());
        assert_eq!(measured.derived_from(), None, "a camera's position is not ours");

        let file = root.join(OFF);
        let bytes = std::fs::read(&file).unwrap();
        let fast = crate::metadata::Reader::read(&Exiv2, &file, &bytes).unwrap();
        let reference = crate::metadata::Reader::read(&crate::metadata::ExifTool, &file, &bytes).unwrap();
        assert_eq!(fast.gps_method, reference.gps_method, "both readers say the same");
    }
}
