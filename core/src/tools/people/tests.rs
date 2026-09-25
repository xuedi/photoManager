use std::sync::atomic::AtomicBool;

use serde_json::Value;

use super::*;
use crate::changeset::{ChangeSet, Verdict};
use crate::immich::fake::{self, Data, FakeImmich};
use crate::tools::testing::{Library, geo};
use crate::tools::{self, AnyTool, Settings};

fn tool() -> &'static dyn AnyTool {
    tools::find("people-from-immich").expect("the tool is listed")
}

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

fn answered(settings: Option<&str>, person: &str, tag: &str) -> String {
    tool()
        .answer(settings, person, Some(Answer::Tag(tag.to_string())))
        .unwrap()
}

/// Ann is Anna, and everyone else what is offered first.
fn every_answer() -> String {
    let settings = answered(None, "p-ann", "people/family/Anna");
    let settings = answered(Some(&settings), "p-ben", "people/groupChina/Ben");
    let settings = answered(Some(&settings), "p-kira", "People/Kira");
    answered(Some(&settings), "p-lena", "people/Lena Park")
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
fn every_named_person_is_asked_about_once_with_offers_from_the_people_tree() {
    let (library, _immich) = fetched("people-asked");
    let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
    let asked: Vec<(&str, &str, usize)> = questions
        .iter()
        .map(|question| (question.key.as_str(), question.title.as_str(), question.photos))
        .collect();
    assert_eq!(
        asked,
        [
            ("p-ben", "Ben", 5),
            ("p-ann", "Ann", 2),
            ("p-kira", "Kira", 1),
            ("p-lena", "Lena Park", 1)
        ],
        "neither the hidden nor the unnamed person, and only photos of the library"
    );
    assert!(questions.iter().all(|question| question.kind == Kind::Person));

    let offered = |key: &str| -> Vec<(String, bool)> {
        let question = questions.iter().find(|question| question.key == key).unwrap();
        question
            .offers
            .iter()
            .map(|offer| match &offer.answer {
                Answer::Tag(path) => (path.clone(), offer.sure),
                other => panic!("{other:?} is no tag"),
            })
            .collect()
    };
    assert_eq!(
        offered("p-ben")[0],
        ("people/groupChina/Ben".to_string(), true),
        "an exact name is sure"
    );
    assert_eq!(
        offered("p-kira")[0],
        ("People/Kira".to_string(), true),
        "in either spelling of the root"
    );
    assert_eq!(
        offered("p-ann"),
        [
            ("people/family/Anna".to_string(), false),
            ("people/Ann".to_string(), false)
        ],
        "a name that starts another is offered, never sure, and a new tag after it"
    );
    assert_eq!(offered("p-lena"), [("people/Lena Park".to_string(), false)]);

    let settings = tools::confirm_sure(tool(), &questions, None).unwrap();
    let answers = Answers::read(&settings).unwrap();
    let confirmed: Vec<&String> = answers.0.keys().collect();
    assert_eq!(confirmed, ["p-ben", "p-kira"], "only the exact ones");
    assert_eq!(
        tools::waiting(tool(), &library.cache, None, &whole(), Some(&settings)).unwrap(),
        2
    );
}

#[test]
fn an_answer_outlives_a_new_fetch_and_a_rename_in_immich() {
    let (library, immich) = fetched("people-rename");
    let settings = answered(None, "p-ann", "people/family/Anna");
    immich.change(|data| data.rename("p-ann", "Anna Maria"));
    fetch_again(&library, &immich);

    let questions = tool()
        .questions(&library.cache, None, &whole(), Some(&settings))
        .unwrap();
    let ann = questions.iter().find(|question| question.key == "p-ann").unwrap();
    assert_eq!(ann.title, "Anna Maria", "the name is Immich's newest");
    assert_eq!(ann.answer, Some(Answer::Tag("people/family/Anna".to_string())));
}

#[test]
fn nothing_fetched_asks_nothing_and_says_so() {
    let library = Library::new("people-none");
    assert!(
        tool()
            .questions(&library.cache, None, &whole(), None)
            .unwrap()
            .is_empty()
    );
    assert!(
        tool()
            .change_set(&library.cache, None, &whole(), None)
            .unwrap()
            .is_empty()
    );
    let report = tool().report(&library.cache, None, &whole(), None).unwrap();
    assert_eq!(report[0].title, "Nothing Fetched from Immich Yet");
}

#[test]
fn the_report_counts_what_immich_knows_that_the_files_do_not() {
    let (library, _immich) = fetched("people-report");
    let report = tool().report(&library.cache, None, &whole(), None).unwrap();
    let find = |title: &str| report.iter().find(|finding| finding.title == title).cloned().unwrap();
    assert!(find("From Immich").detail.starts_with("4 named persons, fetched on "));
    assert!(
        find("From Immich")
            .detail
            .contains("1 photo Immich knows lies outside the library")
    );
    assert_eq!(
        find("Photos Immich Names People In").detail,
        "6 photos of the scope do not say someone Immich found in them.",
        "Ben's and Kira's tagged photos say it by their tag's name"
    );
    let rows = find("People Tags Immich Has No Face For").rows;
    let tags: Vec<&str> = rows.iter().map(|(tag, _)| tag.as_str()).collect();
    assert_eq!(
        tags,
        [
            "People/Kira",
            "people/family/Anna",
            "people/family/Tom",
            "people/groupChina/Ben",
            "people/me"
        ]
    );
    assert!(rows[0].1.ends_with("no person from Immich is answered with it"));
    assert_eq!(
        find("Persons without a Name").detail.split('.').next(),
        Some("1 person Immich found faces of has no name")
    );

    let settings = every_answer();
    let report = tool().report(&library.cache, None, &whole(), Some(&settings)).unwrap();
    let find = |title: &str| report.iter().find(|finding| finding.title == title).cloned().unwrap();
    let rows = find("People Tags Immich Has No Face For").rows;
    let tags: Vec<&str> = rows.iter().map(|(tag, _)| tag.as_str()).collect();
    assert_eq!(
        tags,
        ["people/family/Tom", "people/me"],
        "a tag answered for a person Immich found there has its face"
    );
}

#[test]
fn a_photo_gets_its_regions_persons_and_tags_and_a_second_run_nothing() {
    let (mut library, immich) = fetched("people-write");
    let settings = every_answer();
    let before = read_back(&library, fake::TURNED);

    let set = tool()
        .change_set(&library.cache, None, &whole(), Some(&settings))
        .unwrap();
    assert_eq!(set.title, "Write people from Immich");
    assert_eq!(
        verdicts(&set),
        [
            (fake::BEN_TAGGED, "would change".to_string()),
            (fake::BEN_UNTAGGED, "would change".to_string()),
            (fake::LENA, "would change".to_string()),
            (
                fake::RESIZED,
                "refused: Immich saw it at 999 by 16, the file is 16 by 16".to_string()
            ),
            (fake::TWO, "would change".to_string()),
            (fake::TURNED, "would change".to_string()),
            (fake::OFFLINE, "refused: Immich has it offline".to_string()),
            (fake::KIRA, "would change".to_string()),
            (
                fake::GONE,
                "refused: the cache does not know it, so scan the library first".to_string()
            ),
        ]
    );
    let two = set.rows.iter().find(|row| row.rel_path == fake::TWO).unwrap();
    assert!(two.tells().contains("people: none -> Anna, Ben"), "{}", two.tells());
    let tagged = set.rows.iter().find(|row| row.rel_path == fake::BEN_TAGGED).unwrap();
    assert_eq!(
        tagged.change.fields.len(),
        1,
        "a tag the photo carries is not written again"
    );

    let summary = library.apply(&set);
    assert_eq!(summary.written, 6, "{summary:?}");

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
        Some(&Value::from("Anna")),
        "the tag's name"
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
    assert_eq!(list(&turned, "XMP-iptcExt:PersonInImage"), ["Anna"]);
    for (field, want) in [
        ("XMP-digiKam:TagsList", "people/family/Anna"),
        ("XMP-lr:HierarchicalSubject", "people|family|Anna"),
        ("XMP-microsoft:LastKeywordXMP", "people/family/Anna"),
        ("XMP-dc:Subject", "Anna"),
        ("IPTC:Keywords", "Anna"),
    ] {
        let found = list(&turned, field);
        assert!(found.contains(&want.to_string()), "{field}: {found:?}");
    }
    assert!(
        list(&turned, "XMP-digiKam:TagsList").contains(&"people/family".to_string()),
        "every level"
    );
    assert!(
        list(&turned, "XMP-digiKam:TagsList").contains(&"places/inGermany/Hamburg".to_string()),
        "and what it carried stays"
    );

    library.rescan();
    let again = tool()
        .change_set(&library.cache, None, &whole(), Some(&settings))
        .unwrap();
    assert_eq!(
        again.counts().change,
        0,
        "a second run changes nothing: {:?}",
        verdicts(&again)
    );
    assert_eq!(again.counts().refused, 3, "the refusals stay");

    immich.change(|data| data.add_face(fake::LENA, Some("p-ben"), 0.05, 0.05, 0.2, 0.3));
    fetch_again(&library, &immich);
    let more = tool()
        .change_set(&library.cache, None, &whole(), Some(&settings))
        .unwrap();
    let changed: Vec<&str> = more
        .rows
        .iter()
        .filter(|row| row.would_change())
        .map(|row| row.rel_path.as_str())
        .collect();
    assert_eq!(changed, [fake::LENA], "one more face is one photo");

    let undone = library.undo();
    assert_eq!(undone.written, 6, "{undone:?}");
    assert_eq!(
        read_back(&library, fake::TURNED),
        before,
        "every field exactly as it was"
    );
}

#[test]
fn run_together_with_the_tags_it_is_refused_and_with_time_zones_one_write() {
    let (mut library, _immich) = fetched("people-together");
    let settings = every_answer();
    let scope = Scope::Filter(Filter::all().within("Germany/2019-07-13 Sommerfest"));
    let vocabulary = tools::find("tag-vocabulary").unwrap();
    let clash = tools::together(
        &[(tool(), Some(&settings)), (vocabulary, None)],
        &library.cache,
        None,
        &scope,
    )
    .unwrap();
    let two = clash.rows.iter().find(|row| row.rel_path == fake::TWO).unwrap();
    assert_eq!(
        two.verdict,
        Verdict::Refused("People from Immich and Tag Vocabulary would both set the tags".to_string())
    );

    let zones = tools::find("time-zones").unwrap();
    let geo = geo();
    let set = tools::together(
        &[(zones, None), (tool(), Some(&settings))],
        &library.cache,
        Some(&geo),
        &scope,
    )
    .unwrap();
    let turned = set.rows.iter().find(|row| row.rel_path == fake::TURNED).unwrap();
    let kinds: Vec<&str> = turned.change.fields.iter().map(tools::field_name).collect();
    assert_eq!(kinds, ["date", "tags", "faces"]);
    let summary = library.apply(&set);
    assert_eq!(summary.written, set.counts().change, "one write per photo: {summary:?}");
    let fields = read_back(&library, fake::TURNED);
    assert!(fields.contains_key("XMP-mwg-rs:RegionInfo"));
    assert_eq!(library.dates(fake::TURNED)["ExifIFD:OffsetTimeOriginal"], "+02:00");
}
