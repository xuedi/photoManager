use std::sync::atomic::AtomicBool;

use super::*;
use crate::changeset::ChangeSet;
use crate::immich::{self, fake};
use crate::tools::testing::Library;

const LENA: &str = fake::LENA;
const TWO: &str = fake::TWO;

fn whole() -> Scope {
    Scope::Filter(Filter::all())
}

/// Writes these boxes and persons into a 16 by 16 photo as its only people, as another program
/// would.
fn write_people(library: &mut Library, rel_path: &str, boxes: &[(&str, f64, f64, f64, f64)], persons: &[&str]) {
    let list: Vec<String> = boxes
        .iter()
        .map(|(name, x, y, w, h)| {
            let name = match name.is_empty() {
                true => String::new(),
                false => format!("Name={name},"),
            };
            format!("{{Area={{X={x},Y={y},W={w},H={h},Unit=normalized}},{name}Type=Face}}")
        })
        .collect();
    let mut command = std::process::Command::new("exiftool");
    command.args(["-q", "-overwrite_original", "-XMP-mwg-rs:RegionInfo="]);
    if !boxes.is_empty() {
        command.arg(format!(
            "-XMP-mwg-rs:RegionInfo={{AppliedToDimensions={{W=16,H=16,Unit=pixel}},RegionList=[{}]}}",
            list.join(",")
        ));
    }
    match persons.is_empty() {
        true => {
            command.arg("-XMP-iptcExt:PersonInImage=");
        }
        false => {
            command.args(
                persons
                    .iter()
                    .map(|person| format!("-XMP-iptcExt:PersonInImage={person}")),
            );
        }
    }
    assert!(command.arg(library.root.join(rel_path)).status().unwrap().success());
    library.rescan();
}

fn found(library: &Library) -> Vec<(String, usize, usize, usize)> {
    sure(&library.cache)
        .unwrap()
        .into_iter()
        .map(|found| (found.name, found.photos, found.copies, found.refused))
        .collect()
}

fn keys(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| fold(name)).collect()
}

fn built(library: &Library, names: &[&str]) -> ChangeSet {
    let wanted = wanted(&library.cache, &keys(names), &whole()).unwrap();
    ChangeSet::build(&library.cache, "Name people once", &wanted).unwrap()
}

fn regions(library: &Library, rel_path: &str) -> Regions {
    library.cache.stated(&[rel_path.to_string()]).unwrap()[rel_path]
        .said
        .regions
        .clone()
        .unwrap_or_default()
}

fn boxes(regions: &Regions) -> Vec<(&str, f64, f64, f64, f64)> {
    regions
        .faces
        .iter()
        .map(|face| (face.name.as_str(), face.x, face.y, face.width, face.height))
        .collect()
}

fn verdicts(set: &ChangeSet) -> Vec<(&str, String)> {
    set.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.verdict.tells()))
        .collect()
}

#[test]
fn copies_on_one_face_come_down_to_the_largest() {
    let mut library = Library::new("doubled-copies");
    write_people(
        &mut library,
        LENA,
        &[
            ("Lena Park", 0.5, 0.5, 0.5, 0.5),
            ("Lena Park", 0.51, 0.5, 0.6, 0.6),
            ("Lena Park", 0.5, 0.5, 0.5, 0.5),
        ],
        &[],
    );
    assert_eq!(found(&library), [("Lena Park".to_string(), 1, 2, 0)]);
    let set = built(&library, &["Lena Park"]);
    library.apply(&set);
    library.rescan();
    let lena = regions(&library, LENA);
    assert_eq!(boxes(&lena), [("Lena Park", 0.51, 0.5, 0.6, 0.6)]);
    assert_eq!(lena.persons, ["Lena Park"], "named once");
    assert!(found(&library).is_empty(), "{:?}", found(&library));
}

#[test]
fn each_person_is_a_fix_of_their_own() {
    let mut library = Library::new("doubled-two");
    write_people(
        &mut library,
        TWO,
        &[
            ("Ann", 0.25, 0.3, 0.3, 0.4),
            ("Tom", 0.5, 0.85, 0.1, 0.1),
            ("Ann", 0.25, 0.3, 0.3, 0.4),
            ("Ben", 0.75, 0.4, 0.3, 0.4),
            ("Ben", 0.76, 0.4, 0.3, 0.4),
        ],
        &["Ann", "Tom", "Ben"],
    );
    assert_eq!(
        found(&library),
        [("Ann".to_string(), 1, 1, 0), ("Ben".to_string(), 1, 1, 0)]
    );
    let set = built(&library, &["Ann"]);
    library.apply(&set);
    library.rescan();
    let two = regions(&library, TWO);
    let names: Vec<&str> = two.faces.iter().map(|face| face.name.as_str()).collect();
    assert_eq!(names, ["Ann", "Tom", "Ben", "Ben"], "Ben's copies wait for his own fix");
    assert_eq!(two.persons, ["Ann", "Tom", "Ben"]);
    assert_eq!(found(&library), [("Ben".to_string(), 1, 1, 0)]);
}

#[test]
fn a_name_twice_among_the_persons_is_named_once() {
    let mut library = Library::new("doubled-persons");
    write_people(&mut library, LENA, &[], &["Mia", "Lena Park", "Mia"]);
    assert_eq!(found(&library), [("Mia".to_string(), 1, 1, 0)]);
    let set = built(&library, &["Mia"]);
    library.apply(&set);
    library.rescan();
    let lena = regions(&library, LENA);
    assert!(lena.faces.is_empty());
    assert_eq!(lena.persons, ["Mia", "Lena Park"]);
}

#[test]
fn boxes_of_one_name_on_different_faces_are_refused() {
    let mut library = Library::new("doubled-apart");
    write_people(
        &mut library,
        LENA,
        &[("Lena Park", 0.2, 0.2, 0.2, 0.2), ("Lena Park", 0.8, 0.8, 0.2, 0.2)],
        &[],
    );
    assert!(
        found(&library).is_empty(),
        "nothing would change: {:?}",
        found(&library)
    );
    assert_eq!(
        verdicts(&built(&library, &["Lena Park"])),
        [(
            LENA,
            "refused: Lena Park has boxes on different faces in it".to_string()
        )]
    );
}

#[test]
fn a_photo_left_is_a_line_on_the_fix_of_the_others() {
    let mut library = Library::new("doubled-left");
    write_people(
        &mut library,
        LENA,
        &[("Lena Park", 0.2, 0.2, 0.2, 0.2), ("Lena Park", 0.8, 0.8, 0.2, 0.2)],
        &[],
    );
    write_people(
        &mut library,
        TWO,
        &[("Lena Park", 0.5, 0.5, 0.5, 0.5), ("Lena Park", 0.5, 0.5, 0.5, 0.5)],
        &[],
    );
    assert_eq!(found(&library), [("Lena Park".to_string(), 1, 1, 1)]);
    let fixes = crate::fixes::find(&library.cache, None);
    let fix = fixes.iter().find(|fix| fix.finder == "duplicate-people").unwrap();
    assert_eq!(fix.photos, 1);
    assert_eq!(
        fix.lines[1],
        (
            "Left".to_string(),
            "1 photo, Lena Park has boxes on different faces in it".to_string()
        )
    );
}

#[test]
fn a_box_without_a_name_is_refused() {
    let mut library = Library::new("doubled-unnamed");
    write_people(
        &mut library,
        LENA,
        &[
            ("Lena Park", 0.5, 0.5, 0.5, 0.5),
            ("Lena Park", 0.5, 0.5, 0.5, 0.5),
            ("", 0.1, 0.1, 0.1, 0.1),
        ],
        &[],
    );
    assert_eq!(
        verdicts(&built(&library, &["Lena Park"])),
        [(LENA, "refused: a face box without a name would be lost".to_string())]
    );
}

#[test]
fn a_photo_without_copies_is_no_fix() {
    let mut library = Library::new("doubled-none");
    write_people(
        &mut library,
        LENA,
        &[("Lena Park", 0.5, 0.5, 0.5, 0.5)],
        &["Lena Park", "Mia"],
    );
    assert!(found(&library).is_empty(), "{:?}", found(&library));
    assert!(built(&library, &["Lena Park"]).rows.is_empty());
}

#[test]
fn after_it_people_from_immich_finds_nothing_new() {
    let mut library = Library::new("doubled-then-people");
    let immich = fake::FakeImmich::serve(fake::Data::over(&library.root));
    let mut client = immich::Client::new(&immich.url, fake::KEY).unwrap();
    immich::fetch(
        &mut client,
        &immich::beside(library.cache.file()),
        None,
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    write_people(
        &mut library,
        LENA,
        &[("Lena Park", 0.5, 0.5, 0.5, 0.5), ("Lena Park", 0.5, 0.5, 0.52, 0.5)],
        &[],
    );
    let set = built(&library, &["Lena Park"]);
    library.apply(&set);
    library.rescan();
    assert_eq!(boxes(&regions(&library, LENA)), [("Lena Park", 0.5, 0.5, 0.52, 0.5)]);
    let people: Vec<String> = crate::tools::people::sure(&library.cache)
        .unwrap()
        .into_iter()
        .map(|found| found.name)
        .collect();
    assert!(!people.contains(&"Lena Park".to_string()), "{people:?}");
}

#[test]
fn it_is_a_suggestion_before_people_from_immich() {
    let mut library = Library::new("doubled-fix");
    write_people(
        &mut library,
        LENA,
        &[("Lena Park", 0.5, 0.5, 0.5, 0.5), ("Lena Park", 0.5, 0.5, 0.5, 0.5)],
        &[],
    );
    let fixes = crate::fixes::find(&library.cache, None);
    let fix = fixes
        .iter()
        .find(|fix| fix.finder == "duplicate-people")
        .expect("a fix");
    assert_eq!(
        (fix.key.as_str(), fix.title.as_str(), fix.photos),
        ("duplicate-people:lena park", "Lena Park", 1)
    );
    assert_eq!(fix.lines, [("Goes".to_string(), "1 copy".to_string())]);
    let order: Vec<&str> = crate::fixes::FINDERS.iter().map(|finder| finder.key).collect();
    assert!(
        order.iter().position(|key| *key == "duplicate-people") < order.iter().position(|key| *key == "people"),
        "{order:?}"
    );
    let finder = crate::fixes::finder("duplicate-people").unwrap();
    let set = crate::fixes::change_set(finder, &[fix], &library.cache, None).unwrap();
    assert_eq!(set.counts().change, 1);
}
