//! GPS from the event: a photo without a position and without a city in its places tag is given
//! the position of its event - where the rest of the event already is, a place the folders name,
//! or a point the person chose on the map.
//!
//! One question per event folder. The offers stay inside the folder's country, and only where the
//! rest of the event already is can be sure: a name is never an answer on its own, since an event
//! called `Wedding` is not in the Berlin district of that name.

use std::collections::{BTreeMap, HashMap};

use super::gps_from_places::{deepest, names_a_country};
use super::{Answer, Answers, Located, Offer, Question, Tool, Wording};
use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::layout::Placement;
use crate::scope::Scope;

/// What the file is told about where its position came from.
pub const METHOD: &str = "photoManager: event";
/// How many located photos have to agree before where they are is sure.
pub const SURE: usize = 3;

pub struct GpsFromEvent;

impl Tool for GpsFromEvent {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "gps-from-the-event"
    }

    fn title(&self) -> &'static str {
        "GPS from the Event"
    }

    fn fixes(&self) -> &'static str {
        "Gives photos without GPS or a city tag the position of their event"
    }

    fn named(&self, _answers: &Answers) -> String {
        "Set GPS from the event".to_string()
    }

    fn answers<'a>(&self, answers: &'a mut Answers) -> Option<&'a mut Answers> {
        Some(answers)
    }

    fn waiting(&self, open: usize) -> String {
        match open {
            1 => "1 event waits for an answer".to_string(),
            open => format!("{open} events wait for an answer"),
        }
    }

    fn wording(&self) -> Wording {
        Wording {
            asked: "Events",
            one: "event",
            many: "events",
            confirm: "Confirm Where the Rest Is",
            sure_one: "has all its located photos in one place",
            sure_many: "have all their located photos in one place",
            ..Wording::default()
        }
    }

    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let events = asked(cache, scope).map_err(|error| error.to_string())?;
        let dirs: Vec<String> = events.keys().cloned().collect();
        let mut positions: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        if geo.is_some() {
            for (event_dir, lat, lon) in cache.positions_in(&dirs).map_err(|error| error.to_string())? {
                positions.entry(event_dir).or_default().push((lat, lon));
            }
        }

        let mut near = Near::default();
        let mut questions = Vec::new();
        for (event_dir, photos) in &events {
            let (offers, note) = match geo {
                Some(geo) => offers(
                    geo,
                    &mut near,
                    &Placement::of_folder(event_dir, cache.layout()),
                    positions.get(event_dir).map(Vec::as_slice).unwrap_or_default(),
                )
                .map_err(|error| error.to_string())?,
                None => (Vec::new(), None),
            };
            let title = event_dir.rsplit('/').next().unwrap_or(event_dir);
            questions.push(Question {
                answer: answers.get(event_dir).cloned(),
                note,
                ..Question::place(event_dir.clone(), title, photos.len(), offers)
            });
        }
        questions.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.key.cmp(&other.key)));
        Ok(questions)
    }

    fn wanted(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> cache::Result<Vec<Wanted>> {
        let mut placed: Vec<(String, &Answer)> = Vec::new();
        for (event_dir, photos) in asked(cache, scope)? {
            if let Some(answer) = answers.get(&event_dir) {
                placed.extend(photos.into_iter().map(|rel_path| (rel_path, answer)));
            }
        }
        let mut wanted = super::place_photos(cache, &placed, METHOD)?;
        wanted.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(wanted)
    }
}

/// The photos of the scope this tool is for, by event folder: no position, in an event, and no
/// places tag that names a city - a photo with one is the places tag's, whatever it was answered.
fn asked(cache: &Cache, scope: &Scope) -> cache::Result<BTreeMap<String, Vec<String>>> {
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    let events = cache.event_dirs(&paths)?;
    let mut asked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rel_path in paths {
        let (Some(stated), Some(event_dir)) = (stated.get(&rel_path), events.get(&rel_path)) else {
            continue;
        };
        let said = &stated.said;
        if said.gps_lat.is_some() && said.gps_lon.is_some() {
            continue;
        }
        if deepest(&said.tags).iter().any(|tag| !names_a_country(tag)) {
            continue;
        }
        asked.entry(event_dir.clone()).or_default().push(rel_path);
    }
    Ok(asked)
}

/// The place each point is in, asked of the place data once per point: the photos a tag placed
/// all stand on the same one.
#[derive(Default)]
struct Near(HashMap<(u64, u64), Option<Located>>);

impl Near {
    fn of(&mut self, geo: &Geo, lat: f64, lon: f64) -> crate::geo::Result<Option<Located>> {
        let key = (lat.to_bits(), lon.to_bits());
        if let Some(known) = self.0.get(&key) {
            return Ok(known.clone());
        }
        let near = Located::near(&geo.at(lat, lon)?);
        self.0.insert(key, near.clone());
        Ok(near)
    }
}

/// Where the rest of the event is, then what its folders name, all inside the folder's country.
/// Without a country level, a folder above the event the place data knows as a country narrows
/// the offers; without either nothing does. The note says when a country folder is one the place
/// data does not know, so nothing was narrowed.
fn offers(
    geo: &Geo,
    near: &mut Near,
    placement: &Placement,
    positions: &[(f64, f64)],
) -> crate::geo::Result<(Vec<Offer>, Option<String>)> {
    let mut folder_country = placement.country.clone().unwrap_or_default();
    if placement.country.is_none() {
        for folder in &placement.above {
            if geo.country(folder)?.is_some() {
                folder_country = folder.clone();
                break;
            }
        }
    }
    let country = match folder_country.is_empty() {
        true => None,
        false => geo.country(&folder_country)?.map(|(code, _)| code),
    };
    let inside = |place: &Located| country.as_ref().is_none_or(|code| &place.code == code);

    let mut places: Vec<(Located, usize)> = Vec::new();
    let mut all_in_one = true;
    for &(lat, lon) in positions {
        let Some(place) = near.of(geo, lat, lon)? else {
            all_in_one = false;
            continue;
        };
        match places.iter_mut().find(|(known, _)| known.id == place.id) {
            Some((_, count)) => *count += 1,
            None => places.push((place, 1)),
        }
    }
    places.sort_by(|(one, one_count), (other, other_count)| other_count.cmp(one_count).then(one.name.cmp(&other.name)));
    let located = positions.len();
    let sure = all_in_one && places.len() == 1 && located >= SURE && places.iter().all(|(place, _)| inside(place));
    let mut offers: Vec<Offer> = places
        .into_iter()
        .filter(|(place, _)| inside(place))
        .map(|(place, count)| Offer {
            located: Some(count),
            ..Offer::of_place(place, count as f64 / located as f64, false).with_sure(sure)
        })
        .collect();

    let hint = country
        .as_deref()
        .or(Some(folder_country.as_str()).filter(|text| !text.is_empty()));
    for text in [placement.city.as_deref(), placement.event_name.as_deref()]
        .into_iter()
        .flatten()
    {
        for candidate in geo.find(text, hint)?.candidates {
            let offer = Offer::of(&candidate);
            let Some(place) = offer.place() else { continue };
            if !inside(place)
                || offers
                    .iter()
                    .any(|known| known.place().is_some_and(|known| known.id == place.id))
            {
                continue;
            }
            offers.push(Offer { sure: false, ..offer });
        }
    }

    let note = (country.is_none() && placement.country.is_some()).then(|| {
        format!("The place data knows no country called {folder_country}, so the offers are not narrowed to it")
    });
    Ok((offers, note))
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Once;

    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::filter::tests::scanned;
    use crate::tools::{self, AnyTool, PIN_METRES, PLACE_METRES, Settings};
    use crate::write::Field;
    use crate::write::change::Derived;

    const HARBOUR: &str = "Germany/2016-06-00 Harbour Walk";
    const RAIL: &str = "China/2012-04-00 Rail Trip";
    const WEDDING: &str = "Germany/Hamburg/2014-08-00 Wedding";
    const COPENHAGEN: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    const SOMMERFEST: &str = "Germany/2019-07-13 Sommerfest";
    const SOUTHTOUR: &str = "China/2008-01-00 Holiday SOUTHTOUR";
    const BARE: &str = "Germany/2016-06-00 Harbour Walk/DSC_0104.JPG";

    fn geo() -> Geo {
        static IMPORTED: Once = Once::new();
        let file = std::env::temp_dir().join("photomanager-gps-from-event").join("geo.db");
        IMPORTED.call_once(|| {
            let _ = std::fs::remove_dir_all(file.parent().unwrap());
            let mut geo = Geo::open(&file).unwrap();
            let dumps: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps");
            crate::geo::import::run(&mut geo, &dumps, &|_| {}).unwrap();
        });
        Geo::open(&file).unwrap()
    }

    fn tool() -> &'static dyn AnyTool {
        tools::find("gps-from-the-event").expect("the tool is listed")
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn asked_all(cache: &Cache, settings: Option<&str>) -> Vec<Question> {
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

    fn names(question: &Question) -> Vec<(&str, &str)> {
        question
            .offers
            .iter()
            .map(|offer| {
                (
                    offer.place().unwrap().name.as_str(),
                    offer.place().unwrap().code.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn one_question_per_event_with_its_photos() {
        let cache = scanned("event-questions");
        let questions = asked_all(&cache, None);
        let keys: Vec<(&str, usize)> = questions
            .iter()
            .map(|question| (question.key.as_str(), question.photos))
            .collect();
        assert_eq!(
            keys,
            [
                (SOUTHTOUR, 1),
                (RAIL, 1),
                (COPENHAGEN, 1),
                (HARBOUR, 1),
                (SOMMERFEST, 1),
                (WEDDING, 1),
            ],
            "the loose file, the located photos and the city-tagged ones are not asked about"
        );
        assert_eq!(question(&questions, HARBOUR).title, "2016-06-00 Harbour Walk");
        assert_eq!(tools::waiting(tool(), &cache, None, &whole(), None).unwrap(), 6);
        assert_eq!(tool().waiting(6), "6 events wait for an answer");
        assert_eq!(tools::count(tool(), &cache, None, &whole(), None).unwrap(), 0);
        assert!(
            tool()
                .questions(&cache, None, &whole(), None)
                .unwrap()
                .iter()
                .all(|question| question.offers.is_empty()),
            "no place data, no offers"
        );

        let germany = tool()
            .questions(
                &cache,
                Some(&geo()),
                &Scope::Filter(Filter::all().within("Germany")),
                None,
            )
            .unwrap();
        assert_eq!(germany.len(), 3, "the scope narrows the events");
    }

    #[test]
    fn where_the_rest_is_is_sure_only_in_one_place_with_enough_photos() {
        let cache = scanned("event-rest");
        let questions = asked_all(&cache, None);

        let harbour = question(&questions, HARBOUR);
        let best = harbour.sure().expect("three photos in one city");
        assert_eq!(
            (best.place().unwrap().name.as_str(), best.located),
            ("Hamburg", Some(3))
        );
        assert_eq!(harbour.offers.iter().filter(|offer| offer.sure).count(), 1);

        let rail = question(&questions, RAIL);
        let rest: Vec<(&str, Option<usize>)> = rail
            .offers
            .iter()
            .filter(|offer| offer.located.is_some())
            .map(|offer| (offer.place().unwrap().name.as_str(), offer.located))
            .collect();
        assert_eq!(
            rest,
            [("Beijing", Some(1)), ("Dalian", Some(1))],
            "two places, two offers"
        );
        assert!(rail.offers.iter().all(|offer| !offer.sure));

        let sommerfest = question(&questions, SOMMERFEST);
        assert_eq!(sommerfest.offers[0].place().unwrap().name, "Hamburg");
        assert_eq!(sommerfest.offers[0].located, Some(1));
        assert!(sommerfest.sure().is_none(), "one photo is not enough");
        assert!(
            question(&questions, SOUTHTOUR)
                .offers
                .iter()
                .all(|offer| offer.located.is_none())
        );
    }

    #[test]
    fn a_name_is_kept_inside_the_country_and_never_sure() {
        let cache = scanned("event-names");
        let questions = asked_all(&cache, None);

        let wedding = question(&questions, WEDDING);
        assert_eq!(names(wedding)[0], ("Hamburg", "DE"), "the folder's city first");
        let district = wedding
            .offers
            .iter()
            .find(|offer| offer.place().unwrap().name == "Wedding")
            .expect("the district is offered, it is in the country");
        assert!(district.exact && district.confidence >= tools::EXACT);
        assert!(wedding.offers.iter().all(|offer| !offer.sure), "{:?}", wedding.offers);
        assert!(wedding.sure().is_none());

        let copenhagen = question(&questions, COPENHAGEN);
        assert_eq!(
            names(copenhagen),
            [("Copenhagen", "DK")],
            "the Berlin district stays in Germany"
        );
        assert!(copenhagen.sure().is_none());
        assert!(copenhagen.note.is_none());
    }

    #[test]
    fn confirm_where_the_rest_is_answers_exactly_the_sure_ones() {
        let cache = scanned("event-confirm");
        let questions = asked_all(&cache, None);
        assert_eq!(tool().wording().confirm, "Confirm Where the Rest Is");
        let settings = tools::confirm_sure(tool(), &questions, None).unwrap();
        let answers = Answers::read(&settings).unwrap();
        let confirmed: Vec<&str> = answers.0.keys().map(String::as_str).collect();
        assert_eq!(confirmed, [HARBOUR]);
        let Some(Answer::Place(place)) = answers.get(HARBOUR) else {
            panic!("not a place: {settings}");
        };
        assert_eq!(place.name, "Hamburg");
    }

    #[test]
    fn a_place_and_a_pin_give_position_mark_and_words() {
        let cache = scanned("event-answers");
        let questions = asked_all(&cache, None);
        let hamburg = Answer::Place(question(&questions, HARBOUR).offers[0].place().unwrap().clone());
        let at = geo().at(55.6800, 12.5900).unwrap();
        let pin = Answer::pin(55.6800, 12.5900, &at).expect("a place near the point");
        let settings = answered(Some(&answered(None, HARBOUR, hamburg)), COPENHAGEN, pin);

        let set = tool().change_set(&cache, None, &whole(), Some(&settings)).unwrap();
        assert_eq!(set.title, "Set GPS from the event");
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(
            rows,
            ["Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG", BARE,],
            "only the asked photos, never the located or tagged ones"
        );
        for row in &set.rows {
            assert_eq!(row.verdict, Verdict::Change);
            let Field::Gps(Some(gps)) = &row.change.fields[0] else {
                panic!("no position: {:?}", row.change);
            };
            let Field::Place(Some(words)) = &row.change.fields[1] else {
                panic!("no words: {:?}", row.change);
            };
            if row.rel_path == BARE {
                assert!((gps.lat - 53.55073).abs() < 1e-6 && (gps.lon - 9.99302).abs() < 1e-6);
                assert_eq!(
                    gps.derived,
                    Some(Derived {
                        method: METHOD,
                        metres: PLACE_METRES
                    })
                );
                assert_eq!(words.city.as_deref(), Some("Hamburg"));
            } else {
                assert_eq!((gps.lat, gps.lon), (55.68, 12.59), "a pin is written at the point");
                assert_eq!(
                    gps.derived,
                    Some(Derived {
                        method: METHOD,
                        metres: PIN_METRES
                    })
                );
                assert_eq!(words.city.as_deref(), Some("Copenhagen"));
                assert_eq!(words.country_code.as_deref(), Some("DK"));
            }
        }
        assert_eq!(
            tools::waiting(tool(), &cache, None, &whole(), Some(&settings)).unwrap(),
            4
        );
    }

    #[test]
    fn a_photo_with_words_keeps_them() {
        const RAIL_BARE: &str = "China/2012-04-00 Rail Trip/IMG_5003.JPG";
        let cache = crate::filter::tests::scanned_after("event-words", |root| {
            let status = std::process::Command::new("exiftool")
                .args(["-q", "-overwrite_original", "-XMP-photoshop:City=Somewhere"])
                .arg(root.join(RAIL_BARE))
                .status()
                .unwrap();
            assert!(status.success());
        });
        let questions = asked_all(&cache, None);
        let beijing = question(&questions, RAIL).offers[0].place().unwrap().clone();
        let settings = answered(None, RAIL, Answer::Place(beijing));
        let set = tool().change_set(&cache, None, &whole(), Some(&settings)).unwrap();
        assert_eq!(set.rows.len(), 1);
        assert_eq!(set.rows[0].rel_path, RAIL_BARE);
        assert_eq!(set.rows[0].change.fields.len(), 1, "{:?}", set.rows[0].change);
        assert!(matches!(set.rows[0].change.fields[0], Field::Gps(Some(_))));
    }

    #[test]
    fn left_alone_gives_nothing_and_answers_outlive_the_place_data() {
        let cache = scanned("event-leave");
        let questions = asked_all(&cache, None);
        let mut settings = String::new();
        for question in &questions {
            settings = answered(Some(&settings), &question.key, Answer::Leave);
        }
        assert!(
            tool()
                .change_set(&cache, None, &whole(), Some(&settings))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tools::waiting(tool(), &cache, None, &whole(), Some(&settings)).unwrap(),
            0
        );

        let hamburg = Answer::Place(question(&questions, HARBOUR).offers[0].place().unwrap().clone());
        let settings = answered(None, HARBOUR, hamburg.clone());
        let bare = tool().questions(&cache, None, &whole(), Some(&settings)).unwrap();
        assert_eq!(question(&bare, HARBOUR).answer, Some(hamburg));
        assert_eq!(
            tool()
                .change_set(&cache, None, &whole(), Some(&settings))
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}
