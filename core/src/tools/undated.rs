//! Photos without a date get one: the time between the dated photos before and after it, or
//! noon on the day the folder names, or a date typed by hand. A month or a year is no date, and
//! what none of these reach is left for the photo page.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::civil::DateTime;

use super::dates_folder::{Folder, NO_CAMERA};
use super::offsets::Offsets;
use super::{Answer, Answers, Kind, Offer, Question, Tool, Wording};
use crate::cache::{self, Cache, Dated};
use crate::changeset::Wanted;
use crate::dates;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::write::{Change, Field, Taken};

/// Two neighbours further apart than this say nothing about the photo between them.
const NEAR: i64 = 86_400;

pub const NO_NEIGHBOURS: &str = "no dated photo of the same camera on both sides, less than a day apart";

pub struct PhotosWithoutADate;

impl Tool for PhotosWithoutADate {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "photos-without-a-date"
    }

    fn title(&self) -> &'static str {
        "Photos without a Date"
    }

    fn fixes(&self) -> &'static str {
        "Gives photos without a date the time between their neighbours or their folder day"
    }

    fn named(&self, _answers: &Answers) -> String {
        "Date photos without a date".to_string()
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
            confirm: "",
            unasked: "No photo of the scope without a date is in an event. Choose another scope on the \
                      Tools page.",
            ..Wording::default()
        }
    }

    fn questions(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let events = events(cache, scope).map_err(|error| error.to_string())?;
        let mut questions: Vec<Question> = events
            .iter()
            .map(|event| {
                let reached = event.undated.iter().filter(|(_, between)| between.is_some()).count();
                let total = event.undated.len();
                let mut offers = Vec::new();
                if reached > 0 {
                    let words = match (reached, total) {
                        (1, 1) => "Between its neighbours".to_string(),
                        (reached, total) if reached == total => format!("Between their neighbours, all {total}"),
                        (reached, total) => format!("Between their neighbours, {reached} of {total}"),
                    };
                    offers.push(Offer::of_answer(Answer::Neighbours, words));
                }
                if let Some(day) = event.folder.and_then(|folder| folder.day()) {
                    let at = format!("{} 12:00:00", day.strftime("%Y-%m-%d"));
                    offers.push(Offer::of_answer(
                        Answer::Date(at),
                        format!("12:00:00 from the folder, {}", day.strftime("%Y-%m-%d")),
                    ));
                }
                let note = (reached < total).then(|| match total - reached {
                    1 => format!("1 photo has {NO_NEIGHBOURS}"),
                    left => format!("{left} photos have {NO_NEIGHBOURS}"),
                });
                Question {
                    kind: Kind::Date,
                    answer: answers.get(&event.dir).cloned(),
                    note,
                    ..Question::place(
                        event.dir.clone(),
                        event.dir.rsplit('/').next().unwrap_or(&event.dir),
                        total,
                        offers,
                    )
                }
            })
            .collect();
        questions.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.key.cmp(&other.key)));
        Ok(questions)
    }

    fn wanted(&self, cache: &Cache, geo: Option<&Geo>, scope: &Scope, answers: &Answers) -> cache::Result<Vec<Wanted>> {
        let mut offsets = Offsets::new(geo);
        let mut wanted = Vec::new();
        for event in events(cache, scope)? {
            let dated: Vec<(&Dated, Result<String, String>)> = match answers.get(&event.dir) {
                Some(Answer::Neighbours) => event
                    .undated
                    .iter()
                    .map(|(photo, between)| {
                        (
                            photo,
                            between.map(dates::format).ok_or_else(|| NO_NEIGHBOURS.to_string()),
                        )
                    })
                    .collect(),
                Some(Answer::Date(at)) => {
                    let Ok(start) = dates::parse(at) else { continue };
                    let start = dates::seconds(start);
                    event
                        .undated
                        .iter()
                        .enumerate()
                        .map(|(step, (photo, _))| {
                            let at = dates::from_seconds(start + step as i64)
                                .map(dates::format)
                                .ok_or_else(|| format!("{at} is off the calendar"));
                            (photo, at)
                        })
                        .collect()
                }
                _ => continue,
            };
            for (photo, at) in dated {
                let taken = at.and_then(|at| offsets.offset(photo, &at, None).map(|offset| (at, offset)));
                wanted.push(match taken {
                    Ok((at, offset)) => Wanted::new(
                        photo.rel_path.clone(),
                        Change::of([Field::Taken(Some(Taken {
                            at,
                            offset: Some(offset),
                        }))]),
                    ),
                    Err(why) => Wanted::refused(photo.rel_path.clone(), why),
                });
            }
        }
        wanted.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(wanted)
    }
}

/// An event's photos of the scope without a date, in file name order, each with the time its
/// neighbours give it, if they give one.
struct Event {
    dir: String,
    folder: Option<Folder>,
    undated: Vec<(Dated, Option<DateTime>)>,
}

fn events(cache: &Cache, scope: &Scope) -> cache::Result<Vec<Event>> {
    let paths = scope.paths(cache)?;
    let undated: BTreeSet<String> = cache
        .dated(&paths)?
        .into_iter()
        .filter(|photo| photo.taken_at.is_none() && photo.event_dir.is_some())
        .map(|photo| photo.rel_path)
        .collect();
    let dirs: Vec<String> = cache
        .dated(&undated.iter().cloned().collect::<Vec<String>>())?
        .into_iter()
        .filter_map(|photo| photo.event_dir)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();

    let mut events: BTreeMap<String, Event> = BTreeMap::new();
    let mut groups: HashMap<(String, String), Vec<Dated>> = HashMap::new();
    for photo in cache.dated_in(&dirs)? {
        let Some(event_dir) = photo.event_dir.clone() else {
            continue;
        };
        events.entry(event_dir.clone()).or_insert_with(|| Event {
            folder: Folder::of(photo.event_year, photo.event_month, photo.event_day),
            dir: event_dir,
            undated: Vec::new(),
        });
        let folder = photo
            .rel_path
            .rsplit_once('/')
            .map(|(folder, _)| folder)
            .unwrap_or_default();
        let camera = photo.camera.clone().unwrap_or_else(|| NO_CAMERA.to_string());
        groups.entry((folder.to_string(), camera)).or_default().push(photo);
    }
    for group in groups.values_mut() {
        group.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        let times: Vec<Option<DateTime>> = group
            .iter()
            .map(|photo| photo.taken_at.as_deref().and_then(|at| dates::parse(at).ok()))
            .collect();
        for (index, photo) in group.iter().enumerate() {
            if !undated.contains(&photo.rel_path) {
                continue;
            }
            let Some(event) = photo.event_dir.as_ref().and_then(|dir| events.get_mut(dir)) else {
                continue;
            };
            event.undated.push((photo.clone(), between(&times, index)));
        }
    }
    let mut events: Vec<Event> = events.into_values().filter(|event| !event.undated.is_empty()).collect();
    for event in &mut events {
        event
            .undated
            .sort_by(|(one, _), (other, _)| one.rel_path.cmp(&other.rel_path));
    }
    Ok(events)
}

/// The earlier neighbour's time plus a second per step, where there is a dated neighbour on
/// both sides, they are less than a day apart, and the steps do not run past the later one.
fn between(times: &[Option<DateTime>], index: usize) -> Option<DateTime> {
    let (before, earlier) = times[..index]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(at, time)| time.map(|time| (at, time)))?;
    let later = times[index + 1..].iter().find_map(|time| *time)?;
    let (earlier, later) = (dates::seconds(earlier), dates::seconds(later));
    let guess = earlier + (index - before) as i64;
    (later >= earlier && later - earlier < NEAR && guess <= later)
        .then(|| dates::from_seconds(guess))
        .flatten()
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::testing::{Library, geo};
    use crate::tools::{self, AnyTool};

    const MUSEUM: &str = "Germany/2014-03-22 Museum";
    const BETWEEN: &str = "Germany/2014-03-22 Museum/IMG_9002.JPG";
    const AT_THE_END: &str = "Germany/2014-03-22 Museum/IMG_9004.JPG";
    const SOUTHTOUR: &str = "China/2008-01-00 Holiday SOUTHTOUR";
    const AERON: &str = "Greece/0000-00-00 Aeron ilands";

    fn tool() -> &'static dyn AnyTool {
        tools::find("photos-without-a-date").expect("the tool is listed")
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn written(set: &crate::changeset::ChangeSet) -> Vec<(&str, String)> {
        set.rows
            .iter()
            .map(|row| match (&row.verdict, row.change.fields.first()) {
                (Verdict::Change, Some(Field::Taken(Some(taken)))) => (
                    row.rel_path.as_str(),
                    format!("{} {}", taken.at, taken.offset.clone().unwrap_or_default()),
                ),
                (verdict, _) => (row.rel_path.as_str(), verdict.tells()),
            })
            .collect()
    }

    #[test]
    fn one_question_per_event_and_noon_only_for_a_full_folder_date() {
        let library = Library::new("undated-questions");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let keys: Vec<(&str, usize)> = questions
            .iter()
            .map(|question| (question.key.as_str(), question.photos))
            .collect();
        assert_eq!(keys, [(MUSEUM, 2), (SOUTHTOUR, 1), (AERON, 1)]);
        assert!(questions.iter().all(|question| question.kind == Kind::Date));

        let museum = &questions[0];
        let offered: Vec<(&Answer, &str)> = museum
            .offers
            .iter()
            .map(|offer| (&offer.answer, offer.words.as_str()))
            .collect();
        assert_eq!(
            offered,
            [
                (&Answer::Neighbours, "Between their neighbours, 1 of 2"),
                (
                    &Answer::Date("2014-03-22 12:00:00".to_string()),
                    "12:00:00 from the folder, 2014-03-22"
                ),
            ]
        );
        assert_eq!(
            museum.note.as_deref(),
            Some("1 photo has no dated photo of the same camera on both sides, less than a day apart")
        );
        assert!(
            questions[1].offers.is_empty(),
            "a month is no date, and it has no neighbours"
        );
        assert!(questions[2].offers.is_empty());
        assert_eq!(tools::waiting(tool(), &library.cache, None, &whole(), None).unwrap(), 3);
    }

    #[test]
    fn between_its_neighbours_is_the_earlier_time_and_a_second_a_step_with_its_offset() {
        let library = Library::new("undated-neighbours");
        let settings = tool().answer(None, MUSEUM, Some(Answer::Neighbours)).unwrap();
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
            .unwrap();
        assert_eq!(set.title, "Date photos without a date");
        assert_eq!(
            written(&set),
            [
                (BETWEEN, "2014-03-22 10:00:01 +01:00".to_string()),
                (AT_THE_END, format!("refused: {NO_NEIGHBOURS}")),
            ]
        );
    }

    #[test]
    fn a_date_goes_to_the_first_photo_and_a_second_more_to_each_after() {
        let library = Library::new("undated-date");
        let settings = tool()
            .answer(None, MUSEUM, Some(Answer::Date("2014-03-22 12:00:00".to_string())))
            .unwrap();
        let settings = tool()
            .answer(
                Some(&settings),
                SOUTHTOUR,
                Some(tools::date_answer("2008-01-12 09:30:00").unwrap()),
            )
            .unwrap();
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
            .unwrap();
        assert_eq!(
            written(&set),
            [
                (
                    "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG",
                    "2008-01-12 09:30:00 +08:00".to_string()
                ),
                (BETWEEN, "2014-03-22 12:00:00 +01:00".to_string()),
                (AT_THE_END, "2014-03-22 12:00:01 +01:00".to_string()),
            ]
        );
        assert!(tools::date_answer("2008-01-12").is_err());
    }

    #[test]
    fn neighbours_far_apart_or_on_one_side_give_nothing() {
        let at = |text: &str| Some(dates::parse(text).unwrap());
        let times = [
            at("2014-03-22 10:00:00"),
            None,
            None,
            at("2014-03-22 10:20:00"),
            None,
            at("2014-03-24 10:00:00"),
            None,
        ];
        assert_eq!(
            between(&times, 1).map(dates::format).as_deref(),
            Some("2014-03-22 10:00:01")
        );
        assert_eq!(
            between(&times, 2).map(dates::format).as_deref(),
            Some("2014-03-22 10:00:02")
        );
        assert_eq!(between(&times, 4), None, "more than a day between its neighbours");
        assert_eq!(between(&times, 6), None, "the end of its folder");
        let close = [at("2014-03-22 10:00:00"), None, None, at("2014-03-22 10:00:01")];
        assert_eq!(between(&close, 2), None, "the steps would run past the later one");
    }
}
