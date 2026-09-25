use std::sync::atomic::AtomicBool;

use super::*;
use crate::changeset::{self, ChangeSet, Verdict};
use crate::filter::Filter;
use crate::immich::{self, fake};
use crate::tools::testing::{Library, geo};
use crate::tools::{self, AnyTool, Settings};
use crate::write::{Engine, Outcome};

const BEN: &str = "China/2006-09-00 Besuch Ben";
const RAIL: &str = "China/2012-04-00 Rail Trip";
const SOMMERFEST: &str = "Germany/2019-07-13 Sommerfest";
const HARBOUR: &str = "Germany/2016-06-00 Harbour Walk";
const GARDEN: &str = "Germany/2013-05-18 Garden Party";
const AUTUMN: &str = "Denmark/2017-09-00 Autumn Walk";
const GALWAY: &str = "Ireland/2008-10-03 Galway";
const ISLANDS: &str = "Greece/0000-00-00 Aeron ilands";
const IN_A_CITY: &str = "Germany/Hamburg/2014-08-00 Wedding";
const LOOSE: &str = "China/IMG_3140.JPG";

fn tool() -> &'static dyn AnyTool {
    tools::find("folder-migration").expect("the tool is listed")
}

fn whole() -> Scope {
    Scope::Filter(Filter::all())
}

fn asked(library: &Library, settings: Option<&str>) -> Vec<Question> {
    tool()
        .questions(&library.cache, Some(&geo()), &whole(), settings)
        .unwrap()
}

fn question<'a>(questions: &'a [Question], key: &str) -> &'a Question {
    questions
        .iter()
        .find(|question| question.key == key)
        .unwrap_or_else(|| panic!("nothing asks about {key}"))
}

/// Each offer as the folder it answers with and how many photos say so.
fn offered(question: &Question) -> Vec<(&str, Option<usize>)> {
    question
        .offers
        .iter()
        .map(|offer| match &offer.answer {
            Answer::Folder(path) => (path.as_str(), offer.located),
            other => panic!("not a folder: {other:?}"),
        })
        .collect()
}

fn answered(settings: Option<&str>, key: &str, folder: &str) -> String {
    tool()
        .answer(settings, key, Some(Answer::Folder(folder.to_string())))
        .unwrap()
}

/// The change set as the window builds it: from the cache, then a look at the folders.
fn looked(library: &Library, settings: Option<&str>) -> ChangeSet {
    let mut set = tool()
        .change_set(&library.cache, Some(&geo()), &whole(), settings)
        .unwrap();
    set.look(&library.root);
    set
}

fn verdicts(set: &ChangeSet) -> Vec<(&str, String)> {
    set.rows
        .iter()
        .map(|row| (row.rel_path.as_str(), row.verdict.tells()))
        .collect()
}

/// Every file under the library with its image data, by path.
fn tree(library: &Library) -> Vec<(String, String)> {
    let mut all = Vec::new();
    for entry in walkdir::WalkDir::new(&library.root).sort_by_file_name() {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let rel_path = entry.path().strip_prefix(&library.root).unwrap().display().to_string();
            let bytes = std::fs::read(entry.path()).unwrap();
            all.push((rel_path, crate::identity::content_id(&bytes).unwrap_or_default()));
        }
    }
    all
}

// Phase 2 - where each event belongs

#[test]
fn one_question_per_event_not_in_a_city_and_per_loose_photo() {
    let library = Library::new("folders-asked");
    let questions = asked(&library, None);
    let keys: Vec<&str> = questions.iter().map(|question| question.key.as_str()).collect();
    assert!(
        !keys.contains(&IN_A_CITY),
        "an event in a city folder is not asked about"
    );
    assert!(keys.contains(&LOOSE));
    assert_eq!(keys.len(), 14, "{keys:?}");
    assert!(questions.iter().all(|question| question.kind == Kind::Folder));
    assert_eq!(question(&questions, RAIL).photos, 3);
    assert_eq!(question(&questions, RAIL).title, RAIL, "the whole path before");

    let germany = tool()
        .questions(
            &library.cache,
            None,
            &Scope::Filter(Filter::all().within("Germany")),
            None,
        )
        .unwrap();
    assert_eq!(germany.len(), 5, "the scope narrows the events");
    assert_eq!(tools::count(tool(), &library.cache, None, &whole(), None).unwrap(), 0);
    assert_eq!(
        tools::waiting(tool(), &library.cache, None, &whole(), None).unwrap(),
        14
    );
}

#[test]
fn one_city_on_every_photo_is_sure_and_one_on_some_is_offered_with_its_count() {
    let library = Library::new("folders-sure");
    let questions = asked(&library, None);

    let ben = question(&questions, BEN);
    assert_eq!(offered(ben), [("China/Beijing/2006-09-00 Besuch Ben", Some(2))]);
    assert!(ben.sure().is_some());
    assert!(ben.note.as_deref().unwrap().contains("2006-08-21 move with it"));

    let sommerfest = question(&questions, SOMMERFEST);
    assert_eq!(
        offered(sommerfest)[0],
        ("Germany/Hamburg/2019-07-13 Sommerfest", Some(2))
    );
    assert!(sommerfest.offers[0].words.ends_with("the places tag of 2 of 3 photos"));
    assert!(sommerfest.sure().is_none());

    let settings = tools::confirm_sure(tool(), &questions, None).unwrap();
    let answers = Answers::read(&settings).unwrap();
    assert_eq!(answers.0.keys().collect::<Vec<_>>(), [BEN], "only the sure one");
}

#[test]
fn several_cities_are_offered_by_count_and_never_sure() {
    let library = Library::new("folders-several");
    let questions = asked(&library, None);
    assert_eq!(
        offered(question(&questions, ISLANDS)),
        [
            ("Greece/Atens/0000-00-00 Aeron ilands", Some(2)),
            ("Greece/athens/0000-00-00 Aeron ilands", Some(2)),
            ("Greece/AthensSeaSide/0000-00-00 Aeron ilands", Some(1)),
        ]
    );
    let rail = question(&questions, RAIL);
    assert_eq!(offered(rail).len(), 2);
    assert!(rail.offers.iter().all(|offer| !offer.sure));
    assert!(
        rail.note
            .as_deref()
            .unwrap()
            .contains("several cities: Beijing 1, Dalian 1")
    );
}

#[test]
fn an_event_without_a_city_tag_is_offered_where_its_photos_are() {
    let library = Library::new("folders-gps");
    let questions = asked(&library, None);
    assert_eq!(
        offered(question(&questions, HARBOUR)),
        [("Germany/Hamburg/2016-06-00 Harbour Walk", Some(3))]
    );
    assert!(
        question(&questions, HARBOUR).offers[0]
            .words
            .ends_with("where 3 of its photos are")
    );
    let without = tool().questions(&library.cache, None, &whole(), None).unwrap();
    assert!(
        question(&without, HARBOUR).offers.is_empty(),
        "without place data nothing is known of the positions"
    );
}

#[test]
fn a_country_mismatch_is_said_and_the_other_country_offered() {
    let library = Library::new("folders-country");
    let questions = asked(&library, None);
    let galway = question(&questions, GALWAY);
    assert_eq!(
        offered(galway),
        [
            ("Ireland/Galway/2008-10-03 Galway", Some(1)),
            ("Netherlands/Amsterdam/2008-10-03 Galway", Some(1)),
        ]
    );
    assert!(
        galway
            .note
            .as_deref()
            .unwrap()
            .starts_with("1 of 2 photos say Netherlands")
    );
    let report = tool().report(&library.cache, Some(&geo()), &whole(), None).unwrap();
    let other = report
        .iter()
        .find(|finding| finding.title == "Events Another Country Names")
        .unwrap();
    assert_eq!(
        other.rows,
        [(GALWAY.to_string(), "1 of 2 photos say Netherlands".to_string())]
    );
    assert!(
        report[0]
            .detail
            .starts_with("In the folder of their city already: 1. Not yet: 13")
    );
}

#[test]
fn a_loose_photo_is_offered_the_events_of_its_country_nearest_its_day() {
    let library = Library::new("folders-loose");
    let questions = asked(&library, None);
    let loose = question(&questions, LOOSE);
    assert_eq!(offered(loose)[0].0, BEN);
    assert!(loose.offers[0].words.contains("1 day from the photo"));
    assert_eq!(
        offered(loose).last().unwrap().0,
        "China/2006-09-14",
        "or a new event on its day"
    );
}

#[test]
fn a_folder_answer_is_checked_and_round_trips() {
    let answer = Answer::Folder("Germany/Hamburg/2019-07-13 Sommerfest".to_string());
    assert_eq!(Answer::read(&answer.written()), Ok(answer.clone()));
    assert_eq!(answer.tells(), "Into Germany/Hamburg/2019-07-13 Sommerfest");
    for wrong in [
        "Germany",
        "Germany/Hamburg",
        "Germany/Hamburg/Harbour/2019-07-13 Sommerfest",
        "Germany//2019-07-13 Sommerfest",
        "../2019-07-13 Sommerfest",
        "Germany/.hidden/2019-07-13 Sommerfest",
        "Germany/2019-7-13 Sommerfest",
    ] {
        assert!(event_folder(wrong).is_err(), "{wrong}");
    }
    assert_eq!(
        tools::folder_answer("Germany", " Hamburg ", "2019-07-13", "Sommerfest"),
        Ok(answer)
    );
    assert_eq!(
        tools::folder_answer("Germany", "", "2019-07-13", ""),
        Ok(Answer::Folder("Germany/2019-07-13".to_string()))
    );
    assert!(tools::folder_answer("Germany", "Ham/burg", "2019-07-13", "x").is_err());
    assert!(tools::folder_answer("", "Hamburg", "2019-07-13", "x").is_err());
    assert!(tools::folder_answer("Germany", "Hamburg", "yesterday", "x").is_err());
}

#[test]
fn the_parts_come_from_the_answer_the_offer_or_where_it_is() {
    let library = Library::new("folders-parts");
    let questions = asked(&library, None);
    let ben = Parts::of(question(&questions, BEN));
    assert_eq!(
        (
            ben.country.as_str(),
            ben.city.as_str(),
            ben.date.as_str(),
            ben.name.as_str()
        ),
        ("China", "Beijing", "2006-09-00", "Besuch Ben")
    );
    assert!(ben.date_fixed);
    let southtour = Parts::of(question(&questions, "China/2008-01-00 Holiday SOUTHTOUR"));
    assert_eq!(southtour.city, "", "nothing offered, so where it is");
    let loose = question(&questions, LOOSE);
    let parts = Parts::of(loose);
    assert!(!parts.date_fixed);
    assert_eq!(parts.after(loose).unwrap(), format!("{BEN}/IMG_3140.JPG"));
}

// Phase 3 - the change set and the apply

#[test]
fn two_events_move_the_cache_follows_and_a_second_run_asks_nothing() {
    let mut library = Library::new("folders-apply");
    let before = tree(&library);
    let settings = answered(None, GARDEN, "Germany/Hamburg/2013-05-18 Garden Party");
    let settings = answered(Some(&settings), AUTUMN, "Denmark/Copenhagen/2017-09-00 Autumn Walk");
    let set = looked(&library, Some(&settings));
    assert_eq!(set.title, "Folder Migration");
    assert!(set.moves());
    assert_eq!(set.counts().change, 2);
    assert_eq!(set.counts().traffic, 0, "a move uploads nothing");
    assert_eq!(
        set.rows[0].tells(),
        "to Denmark/Copenhagen/2017-09-00 Autumn Walk, 2 photos"
    );
    assert_eq!(
        set.exact(0, &mut Engine::new(&library.root).unwrap()).unwrap_err(),
        "A move changes no field of any photo, only the folder it is in"
    );

    let summary = library.apply(&set);
    assert_eq!(summary.written, 2, "{summary:?}");
    let moved: Vec<(String, String)> = before
        .iter()
        .map(|(path, content)| {
            let path = path
                .replace(GARDEN, "Germany/Hamburg/2013-05-18 Garden Party")
                .replace(AUTUMN, "Denmark/Copenhagen/2017-09-00 Autumn Walk");
            (path, content.clone())
        })
        .collect();
    let mut expected = moved;
    expected.sort();
    assert_eq!(tree(&library), expected, "every photo, byte for byte, in its new place");

    library.rescan();
    let dirs = library
        .cache
        .event_dirs(&["Germany/Hamburg/2013-05-18 Garden Party/IMG_6001.JPG".to_string()])
        .unwrap();
    assert_eq!(dirs.len(), 1, "the cache knows it where it is");
    let again = asked(&library, Some(&settings));
    assert!(
        again
            .iter()
            .all(|question| question.key != GARDEN && question.key != AUTUMN)
    );
    assert_eq!(looked(&library, Some(&settings)).counts().change, 0);

    let pass = crate::history::pass(&library.journal, summary.batch).unwrap();
    assert_eq!((pass.title.as_str(), pass.written), ("Folder Migration", 7));
    assert!(pass.can_take_back());
    let taken = changeset::take_back(
        &mut Engine::new(&library.root).unwrap(),
        &mut library.journal,
        &mut library.cache,
        summary.batch,
        &|_, _| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(taken.written, 2, "{taken:?}");
    assert_eq!(tree(&library), before, "and back again");
    assert!(!library.root.join("Denmark/Copenhagen").exists());
    library.rescan();
    assert_eq!(
        looked(&library, Some(&settings)).counts().change,
        2,
        "running the tool again puts it back on"
    );
}

#[test]
fn what_would_refuse_is_refused_in_the_preview() {
    let library = Library::new("folders-refused");
    let settings = answered(None, SOMMERFEST, "Germany/Hamburg/2014-08-00 Wedding");
    let settings = answered(Some(&settings), GARDEN, "Germany/Hamburg/2099-01-01 Garden Party");
    let settings = answered(Some(&settings), AUTUMN, "Denmark/Aarhus/2017-09-00 Autumn Walk");
    let settings = answered(
        Some(&settings),
        "Germany/2014-03-22 Museum",
        "Denmark/Aarhus/2017-09-00 Autumn Walk",
    );
    let settings = answered(
        Some(&settings),
        "Germany/2015-00-00 Seasons",
        "Germany/Hamburg/2015-00-00 Seasons",
    );
    std::fs::write(library.root.join("Germany/2015-00-00 Seasons/notes.txt"), "a stray").unwrap();
    let set = looked(&library, Some(&settings));
    let told = verdicts(&set);
    let verdict = |key: &str| told.iter().find(|(path, _)| *path == key).unwrap().1.clone();
    assert_eq!(
        verdict(SOMMERFEST),
        "refused: the date in an event's folder name is not changed here"
    );
    assert_eq!(
        verdict(GARDEN),
        "refused: the date in an event's folder name is not changed here"
    );
    assert!(verdict(AUTUMN).contains("is to go to Denmark/Aarhus/2017-09-00 Autumn Walk too"));
    assert!(verdict("Germany/2015-00-00 Seasons").contains("notes.txt is not a photo the scan knows"));
    assert_eq!(set.counts().change, 0);
}

#[test]
fn an_event_whose_people_only_immich_knows_waits_for_them() {
    let mut library = Library::new("folders-people");
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

    let settings = answered(None, BEN, "China/Beijing/2006-09-00 Besuch Ben");
    let settings = answered(Some(&settings), GARDEN, "Germany/Hamburg/2013-05-18 Garden Party");
    let set = looked(&library, Some(&settings));
    assert_eq!(
        verdicts(&set),
        [
            (
                BEN,
                "refused: Immich names Ben in its photos and the files do not: write the people first".to_string()
            ),
            (GARDEN, "would change".to_string()),
        ]
    );
    let report = tool().report(&library.cache, None, &whole(), None).unwrap();
    let waiting = report
        .iter()
        .find(|finding| finding.title == "Events Waiting for Their People")
        .unwrap();
    assert!(waiting.rows.contains(&(BEN.to_string(), "Ben".to_string())));

    let people = tools::find("people-from-immich").unwrap();
    let answers = people
        .answer(None, "p-ben", Some(Answer::Tag("people/groupChina/Ben".to_string())))
        .unwrap();
    let written = people
        .change_set(
            &library.cache,
            None,
            &Scope::Filter(Filter::all().within(BEN)),
            Some(&answers),
        )
        .unwrap();
    assert!(library.apply(&written).written > 0);
    library.rescan();
    let set = looked(&library, Some(&settings));
    assert_eq!(set.rows[0].verdict, Verdict::Change, "{:?}", verdicts(&set));
}

#[test]
fn a_loose_photo_goes_into_the_chosen_event() {
    let mut library = Library::new("folders-loose-apply");
    let settings = answered(None, LOOSE, BEN);
    let set = looked(&library, Some(&settings));
    assert_eq!(verdicts(&set), [(LOOSE, "would change".to_string())]);
    let summary = library.apply(&set);
    assert_eq!(summary.outcomes, [(LOOSE.to_string(), Outcome::Written)]);
    assert!(library.root.join(BEN).join("IMG_3140.JPG").is_file());
    assert!(library.root.join("China").is_dir());
    library.rescan();
    assert!(
        asked(&library, Some(&settings))
            .iter()
            .all(|question| question.key != LOOSE)
    );
}

#[test]
fn it_never_runs_together_with_a_tool_that_writes() {
    let library = Library::new("folders-together");
    let zones = tools::find("time-zones").unwrap();
    let error = tools::together(&[(zones, None), (tool(), None)], &library.cache, None, &whole()).unwrap_err();
    assert_eq!(
        error,
        "Folder Migration moves folders and runs alone, never together with a tool that writes"
    );
}
