use std::sync::atomic::AtomicBool;

use serde_json::Value;

use super::*;
use crate::changeset::ChangeSet;
use crate::immich::fake::{self, Data, FakeImmich};
use crate::tools::testing::Library;

fn whole() -> Scope {
    Scope::Filter(Filter::all())
}

/// The stand-in library with the fake Immich's people fetched into its snapshot.
fn fetched(name: &str) -> (Library, FakeImmich) {
    let library = Library::new(name);
    let immich = FakeImmich::serve(Data::over(&library.root));
    fetch_again(&library, &immich);
    (library, immich)
}

fn fetch_again(library: &Library, immich: &FakeImmich) {
    let mut client = immich::Client::new(&immich.url, fake::KEY).unwrap();
    immich::fetch(
        &mut client,
        &immich::beside(library.cache.file()),
        None,
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
}

/// A photo Immich knows nobody in, which names a person without a box.
const MIA: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";

fn persons(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn built(library: &Library, ids: &[&str]) -> ChangeSet {
    let wanted = wanted(&library.cache, &persons(ids), &whole()).unwrap();
    ChangeSet::build(&library.cache, "Write people from Immich", &wanted).unwrap()
}

fn found(library: &Library) -> Vec<(String, String, usize, bool)> {
    sure(&library.cache)
        .unwrap()
        .into_iter()
        .map(|found| (found.id, found.name, found.photos, found.refused.is_some()))
        .collect()
}

fn regions(library: &Library, rel_path: &str) -> Regions {
    library.cache.stated(&[rel_path.to_string()]).unwrap()[rel_path]
        .said
        .regions
        .clone()
        .unwrap_or_default()
}

fn names(regions: &Regions) -> Vec<&str> {
    regions.faces.iter().map(|face| face.name.as_str()).collect()
}

/// Every field a write here touches, as ExifTool reads it back with `-struct`.
fn read_back(library: &Library, rel_path: &str) -> serde_json::Map<String, Value> {
    let out = std::process::Command::new("exiftool")
        .args(["-j", "-G1", "-struct", "-XMP:all", "-IPTC:all"])
        .arg(library.root.join(rel_path))
        .output()
        .unwrap();
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut fields = parsed[0].as_object().unwrap().clone();
    fields.remove("SourceFile");
    fields
}

fn list(fields: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    match fields.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().unwrap_or_default().to_string())
            .collect(),
        Some(Value::String(one)) => vec![one.clone()],
        _ => Vec::new(),
    }
}

fn verdicts(set: &ChangeSet) -> Vec<(&str, String)> {
    set.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.verdict.tells()))
        .collect()
}

#[test]
fn every_named_person_is_sure_on_immichs_name_alone() {
    let (library, _immich) = fetched("people-sure");
    let person = |id: &str, name: &str, photos: usize| (id.to_string(), name.to_string(), photos, false);
    assert_eq!(
        found(&library),
        [
            person("p-ben", "Ben", 3),
            person("p-ann", "Ann", 2),
            person("p-kira", "Kira", 1),
            person("p-lena", "Lena Park", 1),
        ],
        "whatever their tags are called, neither hidden nor unnamed, the refused photos not counted"
    );
}

#[test]
fn nothing_fetched_finds_nothing() {
    let library = Library::new("people-none");
    assert!(sure(&library.cache).unwrap().is_empty());
    assert!(
        wanted(&library.cache, &persons(&["p-ben"]), &whole())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn two_persons_of_one_name_are_refused_until_one_is_renamed() {
    let (library, immich) = fetched("people-twins");
    immich.change(|data| data.rename("p-kira", "Ben"));
    fetch_again(&library, &immich);
    let twins: Vec<(String, usize, bool)> = found(&library)
        .into_iter()
        .filter(|(_, name, _, _)| name == "Ben")
        .map(|(id, _, photos, refused)| (id, photos, refused))
        .collect();
    assert_eq!(
        twins,
        [("p-ben".to_string(), 5, true), ("p-kira".to_string(), 1, true)],
        "listed, with every photo they are in"
    );
    let set = built(&library, &["p-ben", "p-kira"]);
    assert_eq!(set.counts().change, 0);
    let refused = set.rows.iter().find(|row| row.rel_path == fake::KIRA).unwrap();
    assert_eq!(
        refused.verdict.tells(),
        "refused: Immich has 2 persons named Ben: rename one there so the files can tell them apart"
    );
}

#[test]
fn the_persons_are_merged_into_what_the_photo_says_and_no_tag_is_touched() {
    let (mut library, immich) = fetched("people-write");
    immich.change(|data| data.add_face(MIA, Some("p-lena"), 0.3, 0.3, 0.6, 0.7));
    fetch_again(&library, &immich);
    let tags_before = |library: &Library| -> Vec<Vec<String>> {
        let every: Vec<String> = [fake::TWO, fake::TURNED, fake::KIRA, fake::LENA, MIA]
            .map(String::from)
            .to_vec();
        let stated = library.cache.stated(&every).unwrap();
        every
            .iter()
            .map(|rel_path| stated[rel_path].said.tags.clone())
            .collect()
    };
    let tags = tags_before(&library);

    let everyone = ["p-ben", "p-ann", "p-kira", "p-lena"];
    let set = built(&library, &everyone);
    assert_eq!(
        verdicts(&set),
        [
            (fake::BEN_TAGGED, "would change".to_string()),
            (fake::BEN_UNTAGGED, "would change".to_string()),
            (fake::LENA, "would change".to_string()),
            (MIA, "would change".to_string()),
            (
                fake::RESIZED,
                "refused: Immich saw it at 999 by 16, the file is 16 by 16".to_string()
            ),
            (fake::TWO, "would change".to_string()),
            (fake::TURNED, "would change".to_string()),
            (fake::OFFLINE, "refused: Immich has it offline".to_string()),
            (fake::KIRA, "would change".to_string()),
        ]
    );
    let two = set.rows.iter().find(|row| row.rel_path == fake::TWO).unwrap();
    assert!(
        two.tells().contains("people: Anna, Tom -> Ann, Tom, Ben"),
        "the box on Ann's face takes her name, Tom's box stays: {}",
        two.tells()
    );
    assert!(
        set.rows
            .iter()
            .all(|row| row.change.fields.iter().all(|field| matches!(field, Field::Faces(_)))),
        "only the people are written"
    );

    let summary = library.apply(&set);
    assert_eq!(summary.written, 7, "{summary:?}");
    library.rescan();
    assert_eq!(tags_before(&library), tags, "no tag is added or taken away");

    let two = regions(&library, fake::TWO);
    assert_eq!(names(&two), ["Ann", "Tom", "Ben"], "never both Anna and Ann");
    assert_eq!(two.persons, ["Ann", "Tom", "Ben"]);
    let tom = &two.faces[1];
    assert_eq!(
        (tom.x, tom.y, tom.width, tom.height),
        (0.5, 0.85, 0.1, 0.1),
        "as it was"
    );

    let mia = regions(&library, MIA);
    assert_eq!(names(&mia), ["Lena Park"]);
    assert_eq!(mia.persons, ["Lena Park", "Mia"], "a person without a box stays named");

    let kira = regions(&library, fake::KIRA);
    assert_eq!(names(&kira), ["Kira"], "the hidden person is left out");

    let turned = read_back(&library, fake::TURNED);
    let region = turned.get("XMP-mwg-rs:RegionInfo").expect("a region");
    assert_eq!(
        region.pointer("/AppliedToDimensions/W"),
        Some(&Value::from(24)),
        "the stored size"
    );
    assert_eq!(region.pointer("/AppliedToDimensions/H"), Some(&Value::from(16)));
    assert_eq!(
        region.pointer("/RegionList/0/Name"),
        Some(&Value::from("Ann")),
        "Immich's name, not the tag's"
    );
    assert_eq!(region.pointer("/RegionList/0/Type"), Some(&Value::from("Face")));
    let area = |name: &str| {
        region
            .pointer(&format!("/RegionList/0/Area/{name}"))
            .and_then(Value::as_f64)
            .unwrap()
    };
    assert_eq!(
        (area("X"), area("Y"), area("W"), area("H")),
        (0.208333, 0.8125, 0.25, 0.25),
        "shown top left, stored bottom left of the turned photo"
    );
    assert_eq!(list(&turned, "XMP-iptcExt:PersonInImage"), ["Ann"]);

    let again = built(&library, &everyone);
    assert_eq!(
        again.counts().change,
        0,
        "a second run changes nothing: {:?}",
        verdicts(&again)
    );
    assert_eq!(again.counts().refused, 2, "the refusals stay");
    assert!(
        found(&library).is_empty(),
        "written, nothing is left to suggest: {:?}",
        found(&library)
    );

    immich.change(|data| data.add_face(fake::LENA, Some("p-ben"), 0.05, 0.05, 0.2, 0.3));
    fetch_again(&library, &immich);
    let more = built(&library, &everyone);
    let changed: Vec<&str> = more
        .rows
        .iter()
        .filter(|row| row.would_change())
        .map(|row| row.rel_path.as_str())
        .collect();
    assert_eq!(changed, [fake::LENA], "one more face is one photo");
}

#[test]
fn a_person_ticked_alone_leaves_the_others_for_their_own_fix() {
    let (mut library, _immich) = fetched("people-alone");
    let set = built(&library, &["p-ben"]);
    let two = set.rows.iter().find(|row| row.rel_path == fake::TWO).unwrap();
    assert!(
        two.tells().contains("-> Anna, Tom, Ben"),
        "Ann is not Ben's to write: {}",
        two.tells()
    );
    library.apply(&set);
    library.rescan();
    let left: Vec<String> = sure(&library.cache)
        .unwrap()
        .into_iter()
        .map(|found| found.name)
        .collect();
    assert_eq!(left, ["Ann", "Kira", "Lena Park"]);
}

#[test]
fn a_box_without_a_name_is_not_lost_the_photo_is_refused() {
    let (mut library, _immich) = fetched("people-unnamed");
    let status = std::process::Command::new("exiftool")
        .args([
            "-q",
            "-overwrite_original",
            "-XMP-mwg-rs:RegionInfo={AppliedToDimensions={W=16,H=16,Unit=pixel},\
             RegionList=[{Area={X=0.1,Y=0.9,W=0.1,H=0.1,Unit=normalized},Type=Face}]}",
        ])
        .arg(library.root.join(fake::LENA))
        .status()
        .unwrap();
    assert!(status.success());
    library.rescan();
    let set = built(&library, &["p-lena"]);
    assert_eq!(
        verdicts(&set),
        [(
            fake::LENA,
            "refused: a face box without a name would be lost".to_string()
        )]
    );
}

/// Writes these face boxes into the photo as its only regions, as another program would.
fn write_regions(library: &mut Library, rel_path: &str, boxes: &[(&str, f64, f64, f64, f64)]) {
    let list: Vec<String> = boxes
        .iter()
        .map(|(name, x, y, w, h)| format!("{{Area={{X={x},Y={y},W={w},H={h},Unit=normalized}},Name={name},Type=Face}}"))
        .collect();
    let status = std::process::Command::new("exiftool")
        .args(["-q", "-overwrite_original"])
        .arg(format!(
            "-XMP-mwg-rs:RegionInfo={{AppliedToDimensions={{W=16,H=16,Unit=pixel}},RegionList=[{}]}}",
            list.join(",")
        ))
        .arg(library.root.join(rel_path))
        .status()
        .unwrap();
    assert!(status.success());
    library.rescan();
}

fn boxes(regions: &Regions) -> Vec<(&str, f64, f64, f64, f64)> {
    regions
        .faces
        .iter()
        .map(|face| (face.name.as_str(), face.x, face.y, face.width, face.height))
        .collect()
}

#[test]
fn a_face_immich_holds_several_times_is_written_once_the_largest() {
    let (mut library, immich) = fetched("people-copies");
    immich.change(|data| {
        data.add_face(fake::LENA, Some("p-lena"), 0.25, 0.25, 0.75, 0.75);
        data.add_face(fake::LENA, Some("p-lena"), 0.2, 0.2, 0.8, 0.8);
        data.add_face(fake::LENA, Some("p-lena"), 0.25, 0.25, 0.75, 0.75);
    });
    fetch_again(&library, &immich);
    let set = built(&library, &["p-lena"]);
    library.apply(&set);
    library.rescan();
    assert_eq!(
        boxes(&regions(&library, fake::LENA)),
        [("Lena Park", 0.5, 0.5, 0.6, 0.6)],
        "one box, the largest of the copies"
    );
    assert!(
        found(&library).iter().all(|(id, ..)| id != "p-lena"),
        "the copies left in Immich are no fix: {:?}",
        found(&library)
    );
}

#[test]
fn a_box_the_file_has_on_the_same_face_stays_as_it_is() {
    let (mut library, _immich) = fetched("people-settled");
    write_regions(&mut library, fake::LENA, &[("Lena Park", 0.51, 0.49, 0.48, 0.52)]);
    assert!(
        found(&library).iter().all(|(id, ..)| id != "p-lena"),
        "a little off is the same face: {:?}",
        found(&library)
    );
    assert_eq!(built(&library, &["p-lena"]).counts().change, 0);
}

#[test]
fn a_box_of_the_name_on_another_face_gives_way_to_one_of_immichs() {
    let (mut library, immich) = fetched("people-elsewhere");
    immich.change(|data| data.add_face(fake::LENA, Some("p-lena"), 0.25, 0.25, 0.75, 0.75));
    fetch_again(&library, &immich);
    write_regions(&mut library, fake::LENA, &[("Lena Park", 0.1, 0.1, 0.1, 0.1)]);
    let set = built(&library, &["p-lena"]);
    library.apply(&set);
    library.rescan();
    assert_eq!(
        boxes(&regions(&library, fake::LENA)),
        [("Lena Park", 0.5, 0.5, 0.5, 0.5)]
    );
}

#[test]
fn two_persons_held_twice_each_get_one_box_each() {
    let (mut library, immich) = fetched("people-two-copies");
    immich.change(|data| {
        data.add_face(fake::TWO, Some("p-ann"), 0.1, 0.1, 0.4, 0.5);
        data.add_face(fake::TWO, Some("p-ben"), 0.6, 0.2, 0.9, 0.6);
    });
    fetch_again(&library, &immich);
    let set = built(&library, &["p-ann", "p-ben"]);
    library.apply(&set);
    library.rescan();
    let two = regions(&library, fake::TWO);
    assert_eq!(names(&two), ["Ann", "Tom", "Ben"]);
    assert_eq!(two.persons, ["Ann", "Tom", "Ben"]);
}

#[test]
fn a_face_immich_names_two_persons_on_is_left_alone_for_either() {
    let (library, immich) = fetched("people-clash");
    immich.change(|data| data.add_face(fake::LENA, Some("p-ann"), 0.26, 0.25, 0.75, 0.76));
    fetch_again(&library, &immich);
    assert!(
        found(&library).iter().all(|(id, ..)| id != "p-lena"),
        "{:?}",
        found(&library)
    );
    let ann = sure(&library.cache)
        .unwrap()
        .into_iter()
        .find(|found| found.id == "p-ann")
        .unwrap();
    assert_eq!(ann.clashed, 1);
    for (id, why) in [
        ("p-lena", "Immich names Lena Park and Ann on the same face"),
        ("p-ann", "Immich names Ann and Lena Park on the same face"),
    ] {
        let set = built(&library, &[id]);
        assert!(
            verdicts(&set).contains(&(fake::LENA, format!("refused: {why}"))),
            "{:?}",
            verdicts(&set)
        );
    }
}
