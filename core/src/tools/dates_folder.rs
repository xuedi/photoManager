//! Dates against the folder: where an event's photos and the date its folder names disagree, the
//! person sees both, camera by camera, and decides. The photos are right, and the folder name is
//! for another day; or a camera's clock was off, and that camera's photos are shifted.
//!
//! A shift is per event and camera, never across events: the same camera a year later may be
//! right. It writes the shifted date with the offset of where the photo was, and remembers the
//! camera's first date, so a shift once written is never written again.

use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::civil::{Date, DateTime};

use super::offsets::Offsets;
use super::{Answer, Answers, Evidence, Kind, Moved, Offer, Question, Tool, Wording};
use crate::cache::{self, Cache, Dated};
use crate::changeset::Wanted;
use crate::dates::{self, Shift};
use crate::geo::Geo;
use crate::scope::Scope;
use crate::write::{Change, Field, Taken};

/// What the photos without a camera name are grouped as.
pub const NO_CAMERA: &str = "no camera";

pub struct DatesAgainstTheFolder;

impl Tool for DatesAgainstTheFolder {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "dates-against-the-folder"
    }

    fn title(&self) -> &'static str {
        "Dates against the Folder"
    }

    fn fixes(&self) -> &'static str {
        "Shifts a camera whose clock was off, where an event's photos disagree with its folder"
    }

    fn named(&self, _answers: &Answers) -> String {
        "Shift dates against the folder".to_string()
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
            unasked: "No event of the scope has photos that disagree with its folder. Choose another scope \
                      on the Tools page.",
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
        let asked = Asked::of(cache, scope).map_err(|error| error.to_string())?;
        let mut questions: Vec<Question> = asked
            .events
            .iter()
            .filter(|event| event.disagrees())
            .map(|event| {
                let photos = event
                    .cameras
                    .iter()
                    .filter(|camera| !camera.agrees)
                    .flat_map(|camera| &camera.photos)
                    .filter(|(rel_path, _)| asked.in_scope.contains(rel_path))
                    .count();
                let evidence: Vec<Evidence> = event
                    .cameras
                    .iter()
                    .map(|camera| camera.evidence(&event.folder))
                    .collect();
                let note = evidence.iter().map(Evidence::tells).collect::<Vec<String>>().join("\n");
                Question {
                    kind: Kind::Shift,
                    answer: answers.get(&event.dir).and_then(|answer| event.fresh(answer)),
                    note: Some(note),
                    evidence,
                    ..Question::place(event.dir.clone(), event.title(), photos, event.offers())
                }
            })
            .collect();
        questions.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.key.cmp(&other.key)));
        Ok(questions)
    }

    fn wanted(&self, cache: &Cache, geo: Option<&Geo>, scope: &Scope, answers: &Answers) -> cache::Result<Vec<Wanted>> {
        let asked = Asked::of(cache, scope)?;
        let mut offsets = Offsets::new(geo);
        let mut wanted = Vec::new();
        for event in &asked.events {
            let Some(Answer::Shift(moved)) = answers.get(&event.dir).and_then(|answer| event.fresh(answer)) else {
                continue;
            };
            for moved in &moved {
                let Some(camera) = event.cameras.iter().find(|camera| camera.name == moved.camera) else {
                    continue;
                };
                for (rel_path, at) in &camera.photos {
                    if !asked.in_scope.contains(rel_path) {
                        continue;
                    }
                    let Some(photo) = asked.photos.get(rel_path) else {
                        continue;
                    };
                    wanted.push(shifted(&mut offsets, photo, *at, &moved.by));
                }
            }
        }
        wanted.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
        Ok(wanted)
    }
}

/// One photo moved by a shift, with the offset it states or the one of where it was.
fn shifted(offsets: &mut Offsets, photo: &Dated, at: DateTime, by: &Shift) -> Wanted {
    let moved = by.apply(&dates::format(at)).and_then(|at| match &photo.taken_offset {
        Some(offset) => Ok((at, offset.clone())),
        None => offsets.offset(photo, &at, None).map(|offset| (at, offset)),
    });
    match moved {
        Ok((at, offset)) => Wanted::new(
            photo.rel_path.clone(),
            Change::of([Field::Taken(Some(Taken {
                at,
                offset: Some(offset),
            }))]),
        ),
        Err(why) => Wanted::refused(photo.rel_path.clone(), why),
    }
}

/// The date a folder names: a year, perhaps a month, perhaps a day.
#[derive(Debug, Clone, Copy)]
pub(super) struct Folder {
    first: Date,
    last: Date,
    /// Whether it names a whole day.
    day: bool,
}

impl Folder {
    pub(super) fn of(year: Option<i64>, month: Option<i64>, day: Option<i64>) -> Option<Folder> {
        let year = i16::try_from(year?).ok()?;
        match (month, day) {
            (Some(month), Some(day)) => {
                let date = Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()?;
                Some(Folder {
                    first: date,
                    last: date,
                    day: true,
                })
            }
            (Some(month), None) => {
                let first = Date::new(year, i8::try_from(month).ok()?, 1).ok()?;
                Some(Folder {
                    first,
                    last: first.last_of_month(),
                    day: false,
                })
            }
            _ => Some(Folder {
                first: Date::new(year, 1, 1).ok()?,
                last: Date::new(year, 12, 31).ok()?,
                day: false,
            }),
        }
    }

    /// The day it names, when it names one.
    pub(super) fn day(&self) -> Option<Date> {
        self.day.then_some(self.first)
    }

    /// How many days a day lies before or after the folder's date; none inside a month or a year.
    fn days_off(&self, date: Date) -> i64 {
        if date < self.first {
            day_number(date) - day_number(self.first)
        } else if date > self.last {
            day_number(date) - day_number(self.last)
        } else {
            0
        }
    }

    /// A day either side of a folder's day still agrees, as on the dashboard: a photo after
    /// midnight, a camera in another zone.
    fn agrees(&self, date: Date) -> bool {
        match self.day {
            true => self.days_off(date).abs() <= 1,
            false => self.days_off(date) == 0,
        }
    }
}

fn day_number(date: Date) -> i64 {
    dates::seconds(date.at(0, 0, 0, 0)).div_euclid(86_400)
}

/// One camera's photos of an event, in time order.
struct Camera {
    name: String,
    photos: Vec<(String, DateTime)>,
    agrees: bool,
    /// Not one of its photos agrees: its clock may have been off. A camera with some photos on
    /// the folder's date and some around it was on a trip longer than the folder says.
    off: bool,
}

impl Camera {
    fn first(&self) -> DateTime {
        self.photos[0].1
    }

    fn evidence(&self, folder: &Folder) -> Evidence {
        let middle = middle(self.photos.iter().map(|(_, at)| *at));
        let days_off = (!self.agrees).then(|| {
            let off = folder.days_off(middle.date());
            match folder.agrees(middle.date()) {
                true => self
                    .photos
                    .iter()
                    .map(|(_, at)| folder.days_off(at.date()))
                    .max_by_key(|days| days.abs())
                    .unwrap_or(off),
                false => off,
            }
        });
        Evidence {
            camera: self.name.clone(),
            photos: self.photos.len(),
            first: dates::format(self.first()),
            last: dates::format(self.photos[self.photos.len() - 1].1),
            days_off,
        }
    }
}

/// The median of some times: the middle one, or halfway between the middle two.
fn middle(times: impl Iterator<Item = DateTime>) -> DateTime {
    let mut seconds: Vec<i64> = times.map(dates::seconds).collect();
    seconds.sort();
    let half = seconds.len() / 2;
    let median = match seconds.len() % 2 {
        1 => seconds[half],
        _ => (seconds[half - 1] + seconds[half]) / 2,
    };
    dates::from_seconds(median).unwrap_or_default()
}

struct Event {
    dir: String,
    folder: Folder,
    cameras: Vec<Camera>,
}

impl Event {
    fn disagrees(&self) -> bool {
        self.cameras.iter().any(|camera| !camera.agrees)
    }

    fn title(&self) -> &str {
        self.dir.rsplit('/').next().unwrap_or(&self.dir)
    }

    /// Align the cameras that are off with the ones that agree, move them onto the folder day, or
    /// say the photos are right: that first when no camera agrees with the folder.
    fn offers(&self) -> Vec<Offer> {
        let right: Vec<&Camera> = self.cameras.iter().filter(|camera| camera.agrees).collect();
        let off: Vec<&Camera> = self.cameras.iter().filter(|camera| camera.off).collect();
        let mut offers = Vec::new();
        if !right.is_empty() {
            let target = dates::seconds(middle(
                right.iter().flat_map(|camera| camera.photos.iter().map(|(_, at)| *at)),
            ));
            let moved: Vec<Moved> = off
                .iter()
                .map(|camera| {
                    let median = dates::seconds(middle(camera.photos.iter().map(|(_, at)| *at)));
                    moved(camera, Shift::rounded(target - median))
                })
                .filter(|moved| !moved.by.is_nothing())
                .collect();
            if !moved.is_empty() {
                let names: Vec<&str> = right.iter().map(|camera| camera.name.as_str()).collect();
                offers.push(Offer::of_answer(
                    Answer::Shift(moved.clone()),
                    format!("Shift {} to agree with {}", told(&moved), names.join(", ")),
                ));
            }
        }
        if let Some(day) = self.folder.day() {
            let moved: Vec<Moved> = off
                .iter()
                .map(|camera| moved(camera, Shift::days(day_number(day) - day_number(camera.first().date()))))
                .filter(|moved| !moved.by.is_nothing())
                .collect();
            if !moved.is_empty() {
                offers.push(Offer::of_answer(
                    Answer::Shift(moved.clone()),
                    format!("Shift {} onto the folder day", told(&moved)),
                ));
            }
        }
        let are_right = Offer::of_answer(Answer::Leave, "The photos are right");
        match right.is_empty() {
            true => offers.insert(0, are_right),
            false => offers.push(are_right),
        }
        offers
    }

    /// The answer as it still stands: a shift of a camera whose first photo has moved since it
    /// was given has been written, and is not given again.
    fn fresh(&self, answer: &Answer) -> Option<Answer> {
        let Answer::Shift(moved) = answer else {
            return Some(answer.clone());
        };
        let standing: Vec<Moved> = moved
            .iter()
            .filter(|moved| {
                self.cameras
                    .iter()
                    .any(|camera| camera.name == moved.camera && dates::format(camera.first()) == moved.from)
            })
            .cloned()
            .collect();
        (!standing.is_empty()).then_some(Answer::Shift(standing))
    }
}

fn moved(camera: &Camera, by: Shift) -> Moved {
    Moved {
        camera: camera.name.clone(),
        by,
        from: dates::format(camera.first()),
    }
}

/// `X100S by -640d 00:07 (more than a year)`.
fn told(moved: &[Moved]) -> String {
    moved
        .iter()
        .map(|moved| match moved.by.over_a_year() {
            true => format!("{} by {} (more than a year)", moved.camera, moved.by.written()),
            false => format!("{} by {}", moved.camera, moved.by.written()),
        })
        .collect::<Vec<String>>()
        .join(", ")
}

/// Each camera's photos with their dates, by camera.
type Cameras = BTreeMap<String, Vec<(String, DateTime)>>;

/// The events the scope reaches into, each whole, with which of its photos the scope holds.
struct Asked {
    events: Vec<Event>,
    in_scope: HashSet<String>,
    photos: HashMap<String, Dated>,
}

impl Asked {
    fn of(cache: &Cache, scope: &Scope) -> cache::Result<Asked> {
        let paths = scope.paths(cache)?;
        let dirs: Vec<String> = cache
            .dated(&paths)?
            .into_iter()
            .filter(|photo| photo.taken_at.is_some() && photo.event_year.is_some())
            .filter_map(|photo| photo.event_dir)
            .collect::<std::collections::BTreeSet<String>>()
            .into_iter()
            .collect();
        let mut grouped: BTreeMap<String, (Option<Folder>, Cameras)> = BTreeMap::new();
        let mut photos = HashMap::new();
        for photo in cache.dated_in(&dirs)? {
            let (Some(event_dir), Some(at)) = (&photo.event_dir, photo.taken_at.as_deref()) else {
                continue;
            };
            let Ok(at) = dates::parse(at) else { continue };
            let entry = grouped.entry(event_dir.clone()).or_insert_with(|| {
                (
                    Folder::of(photo.event_year, photo.event_month, photo.event_day),
                    BTreeMap::new(),
                )
            });
            let camera = photo.camera.clone().unwrap_or_else(|| NO_CAMERA.to_string());
            entry.1.entry(camera).or_default().push((photo.rel_path.clone(), at));
            photos.insert(photo.rel_path.clone(), photo);
        }
        let events = grouped
            .into_iter()
            .filter_map(|(dir, (folder, cameras))| {
                let folder = folder?;
                let cameras = cameras
                    .into_iter()
                    .map(|(name, mut photos)| {
                        photos.sort_by_key(|(rel_path, at)| (*at, rel_path.clone()));
                        let agreeing = photos.iter().filter(|(_, at)| folder.agrees(at.date())).count();
                        Camera {
                            name,
                            agrees: agreeing == photos.len(),
                            off: agreeing == 0,
                            photos,
                        }
                    })
                    .collect();
                Some(Event { dir, folder, cameras })
            })
            .collect();
        Ok(Asked {
            events,
            in_scope: paths.into_iter().collect(),
            photos,
        })
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::filter::Filter;
    use crate::tools::testing::{Library, geo};
    use crate::tools::{self, AnyTool, Settings};

    const BESUCH: &str = "China/2006-09-00 Besuch Ben";
    const AUTUMN: &str = "Denmark/2017-09-00 Autumn Walk";
    const PARTY: &str = "Germany/2013-05-18 Garden Party";
    const BEACH: &str = "Greece/2010-04-10 Beach";
    const LATE: &str = "Germany/2013-05-18 Garden Party/P1060001.JPG";
    const LATER: &str = "Germany/2013-05-18 Garden Party/P1060002.JPG";

    fn tool() -> &'static dyn AnyTool {
        tools::find("dates-against-the-folder").expect("the tool is listed")
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn question<'a>(questions: &'a [Question], key: &str) -> &'a Question {
        questions
            .iter()
            .find(|question| question.key == key)
            .unwrap_or_else(|| panic!("nothing asks about {key}"))
    }

    fn shifts(answer: &Answer) -> Vec<(String, String, String)> {
        let Answer::Shift(moved) = answer else {
            panic!("not a shift: {answer:?}");
        };
        moved
            .iter()
            .map(|moved| (moved.camera.clone(), moved.by.written(), moved.from.clone()))
            .collect()
    }

    fn taken(set: &crate::changeset::ChangeSet) -> Vec<(&str, String)> {
        set.rows
            .iter()
            .map(|row| match &row.change.fields.first() {
                Some(Field::Taken(Some(taken))) => (
                    row.rel_path.as_str(),
                    format!("{} {}", taken.at, taken.offset.clone().unwrap_or_default()),
                ),
                _ => (row.rel_path.as_str(), row.verdict.tells()),
            })
            .collect()
    }

    #[test]
    fn one_question_per_event_that_disagrees_with_its_cameras() {
        let library = Library::new("folder-questions");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let keys: Vec<(&str, usize)> = questions
            .iter()
            .map(|question| (question.key.as_str(), question.photos))
            .collect();
        assert_eq!(keys, [(BESUCH, 2), (AUTUMN, 2), (PARTY, 2), (BEACH, 2)]);
        assert!(questions.iter().all(|question| question.kind == Kind::Shift));

        let party = question(&questions, PARTY);
        let cameras: Vec<(&str, usize, Option<i64>)> = party
            .evidence
            .iter()
            .map(|evidence| (evidence.camera.as_str(), evidence.photos, evidence.days_off))
            .collect();
        assert_eq!(cameras, [("Canon EOS 5D", 3, None), ("DMC-TZ7", 2, Some(-624))]);
        assert_eq!(
            party.evidence[1].tells(),
            "DMC-TZ7: 2 photos, 2011-09-02 15:00:00 to 2011-09-02 15:30:00, 624 days before the folder"
        );
        assert!(party.note.as_deref().unwrap().contains("Canon EOS 5D: 3 photos"));

        let beach = question(&questions, BEACH);
        assert_eq!(beach.evidence[0].camera, NO_CAMERA, "the scans name no camera");
        assert_eq!(tools::waiting(tool(), &library.cache, None, &whole(), None).unwrap(), 4);
        assert_eq!(tool().waiting(4), "4 events wait for an answer");
        assert_eq!(tools::count(tool(), &library.cache, None, &whole(), None).unwrap(), 0);
    }

    #[test]
    fn the_offers_align_the_camera_move_it_onto_the_folder_or_leave_the_photos() {
        let library = Library::new("folder-offers");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();

        let party = question(&questions, PARTY);
        assert_eq!(
            shifts(&party.offers[0].answer),
            [(
                "DMC-TZ7".to_string(),
                "+623d 23:45".to_string(),
                "2011-09-02 15:00:00".to_string()
            )],
            "the difference of the medians, to the minute"
        );
        assert_eq!(
            party.offers[0].words,
            "Shift DMC-TZ7 by +623d 23:45 (more than a year) to agree with Canon EOS 5D"
        );
        assert_eq!(
            shifts(&party.offers[1].answer)[0].1,
            "+624d",
            "whole days onto the folder"
        );
        assert_eq!(party.offers[2].answer, Answer::Leave);
        assert_eq!(party.offers[2].words, "The photos are right");

        let beach = question(&questions, BEACH);
        assert_eq!(
            beach.offers[0].answer,
            Answer::Leave,
            "no camera agrees: the photos first"
        );
        assert_eq!(shifts(&beach.offers[1].answer)[0].1, "+365d");

        let autumn = question(&questions, AUTUMN);
        assert_eq!(
            autumn.offers.iter().map(|offer| &offer.answer).collect::<Vec<_>>(),
            [&Answer::Leave],
            "a month folder has no day to move onto"
        );
    }

    #[test]
    fn a_camera_on_a_trip_around_the_folder_day_is_not_offered_a_shift() {
        let at = |text: &str| dates::parse(text).unwrap();
        let camera = |name: &str, times: &[&str], folder: &Folder| {
            let photos: Vec<(String, DateTime)> = times
                .iter()
                .enumerate()
                .map(|(index, time)| (format!("{name}-{index}.jpg"), at(time)))
                .collect();
            let agreeing = photos.iter().filter(|(_, at)| folder.agrees(at.date())).count();
            Camera {
                name: name.to_string(),
                agrees: agreeing == photos.len(),
                off: agreeing == 0,
                photos,
            }
        };
        let folder = Folder::of(Some(2005), Some(10), Some(1)).unwrap();
        let trip = Event {
            dir: "Germany/2005-10-01 City Trip".to_string(),
            folder,
            cameras: vec![
                camera("Phone", &["2005-10-01 12:00:00"], &folder),
                camera(
                    "Compact",
                    &["2005-09-26 10:00:00", "2005-10-01 18:00:00", "2005-10-03 09:00:00"],
                    &folder,
                ),
            ],
        };
        assert!(trip.disagrees());
        let offered: Vec<Answer> = trip.offers().into_iter().map(|offer| offer.answer).collect();
        assert_eq!(
            offered,
            [Answer::Leave],
            "some of its photos are on the day: no clock was off"
        );
    }

    #[test]
    fn onto_the_folder_keeps_the_time_of_day_and_writes_the_offset() {
        let library = Library::new("folder-onto");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let onto = question(&questions, PARTY).offers[1].answer.clone();
        let settings = tool().answer(None, PARTY, Some(onto)).unwrap();
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
            .unwrap();
        assert_eq!(set.title, "Shift dates against the folder");
        assert_eq!(
            taken(&set),
            [
                (LATE, "2013-05-18 15:00:00 +02:00".to_string()),
                (LATER, "2013-05-18 15:30:00 +02:00".to_string()),
            ]
        );

        let aligned = question(&questions, PARTY).offers[0].answer.clone();
        let settings = tool().answer(None, PARTY, Some(aligned)).unwrap();
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
            .unwrap();
        assert_eq!(taken(&set)[0], (LATE, "2013-05-18 14:45:00 +02:00".to_string()));

        let set = tool()
            .change_set(&library.cache, None, &whole(), Some(&settings))
            .unwrap();
        assert!(
            set.rows.iter().all(|row| row.verdict.tells().contains("no place data")),
            "no offset, no write"
        );
    }

    #[test]
    fn the_photos_are_right_writes_nothing_and_asks_no_more() {
        let library = Library::new("folder-right");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let mut settings = String::new();
        for question in &questions {
            settings = tool()
                .answer(Some(&settings), &question.key, Some(Answer::Leave))
                .unwrap();
        }
        assert!(
            tool()
                .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tools::waiting(tool(), &library.cache, None, &whole(), Some(&settings)).unwrap(),
            0
        );
        assert_eq!(Answers::read(&settings).unwrap().get(PARTY), Some(&Answer::Leave));
    }

    #[test]
    fn a_typed_shift_parses_and_a_bad_one_says_why() {
        let library = Library::new("folder-typed");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let party = question(&questions, PARTY);
        let typed = |camera: &str, text: &str| tools::shift_answer(party, &[(camera.to_string(), text.to_string())]);

        let answer = typed("DMC-TZ7", "+1y 2d 03:00").unwrap();
        assert_eq!(
            shifts(&answer),
            [(
                "DMC-TZ7".to_string(),
                "+1y 2d 03:00".to_string(),
                "2011-09-02 15:00:00".to_string()
            )]
        );
        assert_eq!(Answer::read(&answer.written()), Ok(answer.clone()), "it round-trips");
        assert!(typed("DMC-TZ7", "soon").unwrap_err().starts_with("DMC-TZ7: "));
        assert!(typed("Leica", "-1d").unwrap_err().contains("no camera called Leica"));
        assert!(typed("DMC-TZ7", "").unwrap_err().contains("at least one camera"));
    }

    #[test]
    fn a_shifted_camera_is_settled_and_never_shifted_twice() {
        let mut library = Library::new("folder-written");
        let questions = tool().questions(&library.cache, None, &whole(), None).unwrap();
        let typed = tools::shift_answer(
            question(&questions, PARTY),
            &[("DMC-TZ7".to_string(), "+600d".to_string())],
        )
        .unwrap();
        let settings = tool().answer(None, PARTY, Some(typed)).unwrap();
        let set = tool()
            .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
            .unwrap();
        assert_eq!(library.apply(&set).written, 2);
        library.rescan();

        let again = tool()
            .questions(&library.cache, None, &whole(), Some(&settings))
            .unwrap();
        assert_eq!(
            question(&again, PARTY).answer,
            None,
            "still off the folder, so asked again, not answered with a shift already written"
        );
        assert!(
            tool()
                .change_set(&library.cache, Some(&geo()), &whole(), Some(&settings))
                .unwrap()
                .is_empty(),
            "the shift is not written a second time"
        );
        let zones = tools::find("time-zones").unwrap();
        let rows = zones
            .change_set(
                &library.cache,
                Some(&geo()),
                &Scope::Filter(Filter::all().within(PARTY)),
                None,
            )
            .unwrap();
        let paths: Vec<&str> = rows.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert!(
            !paths.contains(&LATE) && !paths.contains(&LATER),
            "a shifted photo has its offset: {paths:?}"
        );
        assert_eq!(paths.len(), 3, "the other camera still waits for its offset");
    }
}
