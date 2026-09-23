//! GPS from the places tag: a photo without a position, whose places tag names a city, is given
//! that city's coordinates and, where it says nothing yet, its name in words.
//!
//! One question per distinct tag, not per photo. Which place a tag means is the person's answer,
//! never a guess of this module's, and the position it writes says in the file that it is a city
//! centre derived from a tag.

use std::collections::BTreeMap;

use super::{Answer, Answers, Offer, Question, Tool};
use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::tools::Located;
use crate::write::change::Derived;
use crate::write::{Change, Field, Gps};

/// What the file is told about where its position came from.
pub const METHOD: &str = "photoManager: places tag";
/// A city centre: a position given by one may be this many metres off.
pub const METRES: f64 = 5000.0;

pub struct GpsFromPlacesTag;

impl Tool for GpsFromPlacesTag {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "gps-from-places-tag"
    }

    fn title(&self) -> &'static str {
        "GPS from the Places Tag"
    }

    fn fixes(&self) -> &'static str {
        "Gives photos without GPS the coordinates of the city their places tag names"
    }

    fn named(&self, _answers: &Answers) -> String {
        "Set GPS from the places tag".to_string()
    }

    fn answers<'a>(&self, answers: &'a mut Answers) -> Option<&'a mut Answers> {
        Some(answers)
    }

    fn waiting(&self, open: usize) -> String {
        match open {
            1 => "1 tag waits for an answer".to_string(),
            open => format!("{open} tags wait for an answer"),
        }
    }

    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let photos = without_gps(cache, scope).map_err(|error| error.to_string())?;
        let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, tags) in &photos {
            for tag in tags {
                *groups.entry(tag.as_str()).or_default() += 1;
            }
        }

        let mut questions = Vec::new();
        for (tag, photos) in groups {
            let apart = names_a_country(tag);
            let offers = match geo {
                Some(geo) => offers(geo, tag)?,
                None => Vec::new(),
            };
            let answer = match answers.get(tag) {
                Some(answer) => Some(answer.clone()),
                None if apart => Some(Answer::Leave),
                None => None,
            };
            questions.push(Question {
                key: tag.to_string(),
                title: tag.split_once('/').map(|(_, rest)| rest).unwrap_or(tag).to_string(),
                photos,
                offers,
                answer,
                apart,
            });
        }
        questions.sort_by(|one, other| {
            one.apart
                .cmp(&other.apart)
                .then(other.photos.cmp(&one.photos))
                .then(one.key.cmp(&other.key))
        });
        Ok(questions)
    }

    fn wanted(&self, cache: &Cache, scope: &Scope, answers: &Answers) -> cache::Result<Vec<Wanted>> {
        let mut wanted = Vec::new();
        let mut placed = Vec::new();
        for (rel_path, tags) in without_gps(cache, scope)? {
            match decide(&tags, answers) {
                Decision::Nothing => {}
                Decision::Refused(why) => wanted.push(Wanted::refused(rel_path, why)),
                Decision::Place(place) => placed.push((rel_path, place)),
            }
        }

        let paths: Vec<String> = placed.iter().map(|(rel_path, _)| rel_path.clone()).collect();
        let with_text = cache.with_place_text(&paths)?;
        for (rel_path, place) in placed {
            let mut fields = vec![Field::Gps(Some(Gps {
                lat: place.lat,
                lon: place.lon,
                altitude: None,
                derived: Some(Derived {
                    method: METHOD,
                    metres: METRES,
                }),
            }))];
            // Writing the place takes away every part it does not set, so text a photo already
            // has is never replaced by a city centre's.
            if !with_text.contains(&rel_path) {
                fields.push(Field::Place(Some(place.place())));
            }
            wanted.push(Wanted::new(rel_path, Change::of(fields)));
        }
        wanted.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(wanted)
    }
}

enum Decision {
    Nothing,
    Refused(String),
    Place(Located),
}

/// What one photo gets from the answers to its tags. A tag still waiting holds back a photo that
/// has another, so a later answer cannot find it already placed by the first.
fn decide(tags: &[String], answers: &Answers) -> Decision {
    let mut places: Vec<(&str, &Located)> = Vec::new();
    let mut waiting = false;
    for tag in tags {
        match answers.get(tag) {
            Some(Answer::Place(place)) => places.push((tag, place)),
            Some(Answer::Leave) => {}
            None if names_a_country(tag) => {}
            None => waiting = true,
        }
    }
    let Some((first_tag, first)) = places.first().copied() else {
        return Decision::Nothing;
    };
    if waiting {
        return Decision::Nothing;
    }
    match places.iter().find(|(_, place)| place.id != first.id) {
        Some((other_tag, other)) => Decision::Refused(format!(
            "{first_tag} says {} and {other_tag} says {}",
            first.name, other.name
        )),
        None => Decision::Place(first.clone()),
    }
}

/// The photos of the scope without a position, each with its deepest places tags.
fn without_gps(cache: &Cache, scope: &Scope) -> cache::Result<Vec<(String, Vec<String>)>> {
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    Ok(paths
        .into_iter()
        .filter_map(|rel_path| {
            let said = &stated.get(&rel_path)?.said;
            if said.gps_lat.is_some() && said.gps_lon.is_some() {
                return None;
            }
            let tags = deepest(&said.tags);
            (!tags.is_empty()).then_some((rel_path, tags))
        })
        .collect())
}

/// The places tags that no other places tag of the photo goes below: `places/inChina/Beijing`,
/// not `places/inChina` beside it. The bare root says nothing.
fn deepest(tags: &[String]) -> Vec<String> {
    let places: Vec<&String> = tags
        .iter()
        .filter(|tag| {
            let mut levels = tag.split('/');
            levels.next().is_some_and(|root| root.eq_ignore_ascii_case("places")) && levels.next().is_some()
        })
        .collect();
    let mut deepest: Vec<String> = places
        .iter()
        .filter(|tag| !places.iter().any(|other| other.starts_with(&format!("{tag}/"))))
        .map(|tag| tag.to_string())
        .collect();
    deepest.sort();
    deepest.dedup();
    deepest
}

/// `places/inChina`: a country and no place in it.
fn names_a_country(tag: &str) -> bool {
    tag.split('/').count() == 2
}

/// The place data's candidates for the deepest level, the country level as the hint.
fn offers(geo: &Geo, tag: &str) -> Result<Vec<Offer>, String> {
    let levels: Vec<&str> = tag.split('/').collect();
    if levels.len() < 3 {
        return Ok(Vec::new());
    }
    let country = levels[1];
    let hint = match country.strip_prefix("in") {
        Some(rest) if rest.starts_with(char::is_uppercase) => rest,
        _ => country,
    };
    let found = geo
        .find(levels[levels.len() - 1], Some(hint))
        .map_err(|error| error.to_string())?;
    Ok(found.candidates.iter().map(Offer::of).collect())
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Once;

    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::filter::tests::scanned;
    use crate::tools::{self, AnyTool, EXACT, Settings};

    const BEIJING: &str = "places/inChina/Beijing";
    const ATHENS: &str = "places/inGreece/athens";
    const ATENS: &str = "places/inGreece/Atens";
    const AMSTERDAM: &str = "places/inNetherland/Amsterdam";
    const TWO_TAGS: &str = "Greece/0000-00-00 Aeron ilands/IMG_0007.JPG";
    const WITH_TEXT: &str = "Ireland/2008-10-03 Galway/IMG_0003.JPG";
    const LOCATED: &str = "Germany/2019-07-13 Sommerfest/img_0657.jpg";

    /// The excerpt, imported once for the whole file.
    fn geo() -> Geo {
        static IMPORTED: Once = Once::new();
        let file = std::env::temp_dir().join("photomanager-gps-from-places").join("geo.db");
        IMPORTED.call_once(|| {
            let _ = std::fs::remove_dir_all(file.parent().unwrap());
            let mut geo = Geo::open(&file).unwrap();
            let dumps: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
            crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        });
        Geo::open(&file).unwrap()
    }

    fn tool() -> &'static dyn AnyTool {
        tools::find("gps-from-places-tag").expect("the tool is listed")
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn asked(cache: &Cache, settings: Option<&str>) -> Vec<Question> {
        tool().questions(cache, Some(&geo()), &whole(), settings).unwrap()
    }

    fn question<'a>(questions: &'a [Question], key: &str) -> &'a Question {
        questions
            .iter()
            .find(|question| question.key == key)
            .unwrap_or_else(|| panic!("nothing asks about {key}"))
    }

    fn answered(settings: Option<&str>, key: &str, answer: Answer) -> String {
        tool().answer(settings, key, Some(answer)).unwrap()
    }

    fn best(questions: &[Question], key: &str) -> Answer {
        Answer::Place(question(questions, key).offers[0].place.clone())
    }

    #[test]
    fn one_question_per_distinct_tag_with_its_photos() {
        let cache = scanned("gps-questions");
        let questions = asked(&cache, None);
        let keys: Vec<(&str, usize)> = questions
            .iter()
            .map(|question| (question.key.as_str(), question.photos))
            .collect();
        assert_eq!(
            keys,
            [
                (BEIJING, 2),
                (ATENS, 2),
                (ATHENS, 2),
                ("places/inDenmark/Copenhagen", 1),
                ("places/inGermany/Hamburg", 1),
                ("places/inGreece/AthensSeaSide", 1),
                ("places/inIreland/Galway", 1),
                (AMSTERDAM, 1),
                ("places/inChina", 1),
                ("places/inGermany", 1),
            ],
            "most photos first, the countries apart at the end, and the Hamburg photo with GPS not at all"
        );
        assert_eq!(question(&questions, BEIJING).title, "inChina/Beijing");
        assert_eq!(tools::waiting(tool(), &cache, &whole(), None).unwrap(), 8);
        assert_eq!(tool().waiting(8), "8 tags wait for an answer");
        assert_eq!(
            tools::count(tool(), &cache, &whole(), None).unwrap(),
            0,
            "nothing is answered"
        );

        let beijing = question(&questions, BEIJING);
        assert_eq!(beijing.offers[0].place.name, "Beijing");
        assert_eq!(beijing.offers[0].place.code, "CN");
        assert!(beijing.exact().is_some(), "{:?}", beijing.offers[0]);
        let typo = question(&questions, ATENS);
        assert_eq!(typo.offers[0].place.name, "Athens");
        assert!(typo.exact().is_none(), "a typo is offered, never confirmed in bulk");
        let pseudo = question(&questions, "places/inGreece/AthensSeaSide");
        assert!(pseudo.offers.first().is_none_or(|offer| offer.confidence < EXACT));

        let bare = tool().questions(&cache, None, &whole(), None).unwrap();
        assert!(
            bare.iter().all(|question| question.offers.is_empty()),
            "no place data, no offers"
        );
    }

    #[test]
    fn a_country_alone_starts_left_alone_and_gives_nothing_until_answered() {
        let cache = scanned("gps-country");
        let questions = asked(&cache, None);
        let china = question(&questions, "places/inChina");
        assert!(china.apart);
        assert_eq!(china.answer, Some(Answer::Leave));
        assert!(!china.waits());

        let confirmed = tools::confirm_exact(tool(), &questions, None).unwrap();
        assert!(
            !confirmed.contains("\"places/inChina\""),
            "never confirmed in bulk: {confirmed}"
        );

        let beijing = best(&questions, BEIJING);
        let by_hand = answered(None, "places/inChina", beijing);
        let set = tool().change_set(&cache, &whole(), Some(&by_hand)).unwrap();
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rows, ["China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG"]);
    }

    #[test]
    fn a_confirmed_tag_gives_its_photos_the_position_the_mark_and_the_words() {
        let cache = scanned("gps-confirmed");
        let questions = asked(&cache, None);
        let settings = answered(None, BEIJING, best(&questions, BEIJING));
        let set = tool().change_set(&cache, &whole(), Some(&settings)).unwrap();
        assert_eq!(set.title, "Set GPS from the places tag");
        assert_eq!(set.rows.len(), 2);
        for row in &set.rows {
            assert!(
                row.rel_path.starts_with("China/2006-09-00 Besuch Ben/"),
                "{}",
                row.rel_path
            );
            assert_eq!(row.verdict, Verdict::Change);
            let Field::Gps(Some(gps)) = &row.change.fields[0] else {
                panic!("no position: {:?}", row.change);
            };
            assert!((gps.lat - 39.9075).abs() < 0.01 && (gps.lon - 116.3972).abs() < 0.01);
            assert_eq!(
                gps.derived,
                Some(Derived {
                    method: METHOD,
                    metres: METRES
                })
            );
            let Field::Place(Some(place)) = &row.change.fields[1] else {
                panic!("no words: {:?}", row.change);
            };
            assert_eq!(place.city.as_deref(), Some("Beijing"));
            assert_eq!(place.country.as_deref(), Some("China"));
            assert_eq!(place.country_code.as_deref(), Some("CN"));
            assert!(row.tells().contains("(derived)"), "{}", row.tells());
        }
        assert_eq!(tools::count(tool(), &cache, &whole(), Some(&settings)).unwrap(), 2);
        assert_eq!(tools::waiting(tool(), &cache, &whole(), Some(&settings)).unwrap(), 7);
    }

    #[test]
    fn a_photo_with_words_keeps_them_and_gets_the_position() {
        let cache = scanned("gps-words");
        let questions = asked(&cache, None);
        assert_eq!(question(&questions, AMSTERDAM).offers[0].place.code, "NL");
        let settings = answered(None, AMSTERDAM, best(&questions, AMSTERDAM));
        let set = tool().change_set(&cache, &whole(), Some(&settings)).unwrap();
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].rel_path, WITH_TEXT);
        assert_eq!(set.rows[0].change.fields.len(), 1, "{:?}", set.rows[0].change);
        assert!(matches!(set.rows[0].change.fields[0], Field::Gps(Some(_))));
    }

    #[test]
    fn a_photo_with_gps_is_never_in_the_set() {
        let cache = scanned("gps-located");
        let questions = asked(&cache, None);
        let hamburg = tool()
            .questions(
                &cache,
                Some(&geo()),
                &Scope::Filter(Filter::all().within("Germany")),
                None,
            )
            .unwrap();
        assert_eq!(question(&hamburg, "places/inGermany/Hamburg").photos, 1);
        let mut settings = tools::confirm_exact(tool(), &questions, None).unwrap();
        for key in ["places/inChina", "places/inGermany"] {
            settings = answered(Some(&settings), key, best(&questions, BEIJING));
        }
        let set = tool().change_set(&cache, &whole(), Some(&settings)).unwrap();
        assert!(set.rows.iter().all(|row| row.rel_path != LOCATED));
        assert!(!set.rows.is_empty());
    }

    #[test]
    fn two_places_refuse_and_two_tags_with_one_answer_do_not() {
        let cache = scanned("gps-two");
        let questions = asked(&cache, None);
        let athens = best(&questions, ATHENS);
        let row_of = |settings: &str| {
            let set = tool().change_set(&cache, &whole(), Some(settings)).unwrap();
            set.rows.into_iter().find(|row| row.rel_path == TWO_TAGS)
        };

        let one = answered(None, ATHENS, athens.clone());
        assert!(row_of(&one).is_none(), "its other tag still waits, so it waits too");

        let same = answered(Some(&one), ATENS, athens);
        assert_eq!(row_of(&same).unwrap().verdict, Verdict::Change);

        let elsewhere = answered(Some(&one), ATENS, best(&questions, BEIJING));
        let refused = row_of(&elsewhere).unwrap();
        let Verdict::Refused(why) = &refused.verdict else {
            panic!("two places were written: {:?}", refused.verdict);
        };
        assert!(why.contains("Athens") && why.contains("Beijing"), "{why}");
        assert!(!refused.selected());

        let left = answered(Some(&one), ATENS, Answer::Leave);
        assert_eq!(
            row_of(&left).unwrap().verdict,
            Verdict::Change,
            "left alone is no second place"
        );
    }

    #[test]
    fn left_alone_and_unanswered_give_nothing() {
        let cache = scanned("gps-nothing");
        let questions = asked(&cache, None);
        let mut settings = String::new();
        for question in questions.iter().filter(|question| !question.apart) {
            settings = answered(Some(&settings), &question.key, Answer::Leave);
        }
        assert!(tool().change_set(&cache, &whole(), Some(&settings)).unwrap().is_empty());
        assert!(tool().change_set(&cache, &whole(), None).unwrap().is_empty());
        assert_eq!(tools::waiting(tool(), &cache, &whole(), Some(&settings)).unwrap(), 0);
    }

    #[test]
    fn confirm_exact_matches_confirms_exactly_the_exact_ones() {
        let cache = scanned("gps-exact");
        let questions = asked(&cache, None);
        let settings = tools::confirm_exact(tool(), &questions, None).unwrap();
        let answers = Answers::read(&settings).unwrap();
        let confirmed: Vec<&str> = answers.0.keys().map(String::as_str).collect();
        let expected: Vec<&str> = questions
            .iter()
            .filter(|question| question.exact().is_some() && !question.apart)
            .map(|question| question.key.as_str())
            .collect();
        let mut expected = expected;
        expected.sort();
        assert_eq!(confirmed, expected);
        assert!(confirmed.contains(&BEIJING));
        assert!(!confirmed.contains(&ATENS));

        let kept = answered(Some(&settings), ATENS, Answer::Leave);
        let again = tools::confirm_exact(tool(), &asked(&cache, Some(&kept)), Some(&kept)).unwrap();
        assert_eq!(
            Answers::read(&again).unwrap(),
            Answers::read(&kept).unwrap(),
            "an answer stays"
        );
    }

    #[test]
    fn answers_round_trip_through_text_and_outlive_the_place_data() {
        let cache = scanned("gps-text");
        let questions = asked(&cache, None);
        let settings = answered(
            Some(&answered(None, BEIJING, best(&questions, BEIJING))),
            ATENS,
            Answer::Leave,
        );
        let read = Answers::read(&settings).unwrap();
        assert_eq!(Answers::read(&read.written()).unwrap(), read);
        assert_eq!(read.written(), settings, "the same answers are the same text");
        assert!(
            settings.find(BEIJING) < settings.find(ATENS),
            "sorted by tag: {settings}"
        );
        assert!(Answers::read("not json").is_err());
        assert!(Answers::read(r#"{"places/inChina/Beijing": 3}"#).is_err());
        assert_eq!(Answers::read("").unwrap(), Answers::default());

        let before = tool().change_set(&cache, &whole(), Some(&settings)).unwrap();
        let without_data = tool().questions(&cache, None, &whole(), Some(&settings)).unwrap();
        assert_eq!(question(&without_data, BEIJING).answer, Some(best(&questions, BEIJING)));
        let after = tool().change_set(&cache, &whole(), Some(&settings)).unwrap();
        assert_eq!(
            before.rows.iter().map(|row| &row.change).collect::<Vec<_>>(),
            after.rows.iter().map(|row| &row.change).collect::<Vec<_>>(),
            "the answer carries the place, so the change needs no place data at all"
        );
    }

    #[test]
    fn one_answer_reads_back_onto_the_questions_asked_before() {
        let cache = scanned("gps-again");
        let mut questions = asked(&cache, None);
        let beijing = best(&questions, BEIJING);
        let settings = answered(None, BEIJING, beijing.clone());
        tools::answer_again(tool(), &mut questions, Some(&settings)).unwrap();
        assert_eq!(question(&questions, BEIJING).answer, Some(beijing.clone()));
        assert_eq!(question(&questions, "places/inChina").answer, Some(Answer::Leave));
        assert!(question(&questions, ATENS).waits());

        assert_eq!(Answer::read(&beijing.written()).unwrap(), beijing);
        assert_eq!(Answer::read("leave").unwrap(), Answer::Leave);
        assert!(Answer::read("somewhere").is_err());
        assert!(
            Answer::read(r#"{"id": 1, "name": "Nowhere"}"#).is_err(),
            "a place says where it is"
        );
    }

    #[test]
    fn only_the_deepest_places_tags_count() {
        let tags = |all: &[&str]| all.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();
        assert_eq!(
            deepest(&tags(&["places", "places/inChina", BEIJING, "people/me"])),
            [BEIJING]
        );
        assert_eq!(
            deepest(&tags(&["Places/inGermany", "places/inChina"])),
            ["Places/inGermany", "places/inChina"]
        );
        assert!(deepest(&tags(&["places"])).is_empty());
    }
}
