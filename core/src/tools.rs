//! What a tool is: something that looks at the photos of a scope and says what each should say.
//!
//! That is the whole contract. Everything after it - the change set, its counts, the preview, the
//! apply, the journal and the undo - is shared, so a tool never writes, never asks and never
//! shows anything itself. The application knows the tools only through [`ALL`]: it lists them,
//! counts them and opens them by key, and names none of them.
//!
//! A tool's settings are a plain value that can be written as text and read back, so what a
//! suggestion hands over later is a key, a scope and a line of text, not a form.
//!
//! A tool may also ask: which place a group of photos means, or how far a camera's clock was off.
//! It hands over [`Question`]s, each offer carrying the answer it would give, the application draws
//! them without knowing which tool asked, and the [`Answer`]s go back into the tool's settings. So
//! a question is answered once, and the next run over any scope finds it answered.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::cache::{self, Cache};
use crate::changeset::{ChangeSet, Wanted};
use crate::dates::Shift;
use crate::geo::Geo;
use crate::geo::lookup::{self, How};
use crate::geo::reverse::At;
use crate::scope::Scope;
use crate::write;
use crate::write::change::Derived;
use crate::write::{Change, Field, Gps};

pub mod add_tag;
pub mod dates_folder;
pub mod folders;
pub mod gps_from_event;
pub mod gps_from_places;
pub mod offsets;
pub mod people;
pub mod tag_vocabulary;
#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod testing;
pub mod time_zones;
pub mod undated;

/// What a tool can be told. A tool that needs nothing uses `()`.
pub trait Settings: Default + Sized {
    fn read(text: &str) -> Result<Self, String>;
    fn written(&self) -> String;
}

impl Settings for () {
    fn read(text: &str) -> Result<(), String> {
        match text.trim().is_empty() {
            true => Ok(()),
            false => Err(format!("this tool takes no settings, not {text}")),
        }
    }

    fn written(&self) -> String {
        String::new()
    }
}

/// How sure the place data has to be before Confirm Exact Matches takes its word.
pub const EXACT: f64 = 0.9;

/// A place's centre: a position given by one may be this many metres off.
pub const PLACE_METRES: f64 = 5000.0;
/// A point a person put roughly where it was on a map, not to the metre.
pub const PIN_METRES: f64 = 1000.0;

/// A place as an answer keeps it: everything a write needs, so an answer does not depend on the
/// place data it was chosen from, and survives that data being imported again.
#[derive(Debug, Clone, PartialEq)]
pub struct Located {
    /// The GeoNames id, to tell two answers apart.
    pub id: i64,
    pub name: String,
    pub region: Option<String>,
    pub country: String,
    pub code: String,
    pub lat: f64,
    pub lon: f64,
}

impl Located {
    pub fn of(place: &lookup::Place) -> Located {
        Located {
            id: place.id,
            name: place.name.clone(),
            region: place.area.clone(),
            country: place.country_name.clone(),
            code: place.country.clone(),
            lat: place.lat,
            lon: place.lon,
        }
    }

    /// The place in words, the way a photo says it.
    pub fn place(&self) -> write::Place {
        write::Place {
            city: Some(self.name.clone()),
            state: self.region.clone(),
            country: Some(self.country.clone()),
            country_code: Some(self.code.clone()),
            location: None,
        }
    }

    /// The town a point is in, in words.
    pub fn near(at: &At) -> Option<Located> {
        at.town.as_ref().map(|nearby| Located::of(&nearby.place))
    }

    /// `Beijing, Beijing, China`.
    pub fn tells(&self) -> String {
        [Some(&self.name), self.region.as_ref(), Some(&self.country)]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<&str>>()
            .join(", ")
    }

    fn written(&self) -> Value {
        let mut fields = Map::new();
        fields.insert("id".to_string(), Value::from(self.id));
        fields.insert("name".to_string(), Value::from(self.name.as_str()));
        if let Some(region) = &self.region {
            fields.insert("region".to_string(), Value::from(region.as_str()));
        }
        fields.insert("country".to_string(), Value::from(self.country.as_str()));
        fields.insert("code".to_string(), Value::from(self.code.as_str()));
        fields.insert("lat".to_string(), Value::from(self.lat));
        fields.insert("lon".to_string(), Value::from(self.lon));
        Value::Object(fields)
    }

    fn read(value: &Value) -> Result<Located, String> {
        let text = |name: &str| value.get(name).and_then(Value::as_str).map(String::from);
        let number = |name: &str| value.get(name).and_then(Value::as_f64);
        let wrong = || format!("{value} is not a place");
        let located = Located {
            id: value.get("id").and_then(Value::as_i64).ok_or_else(wrong)?,
            name: text("name").ok_or_else(wrong)?,
            region: text("region"),
            country: text("country").ok_or_else(wrong)?,
            code: text("code").ok_or_else(wrong)?,
            lat: number("lat").ok_or_else(wrong)?,
            lon: number("lon").ok_or_else(wrong)?,
        };
        if !(-90.0..=90.0).contains(&located.lat) || !(-180.0..=180.0).contains(&located.lon) {
            return Err(format!("{} is not on earth", located.name));
        }
        Ok(located)
    }
}

/// What a question was answered with.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Place(Located),
    /// A point a person chose on the map, with the place it is in for the words.
    Pin {
        lat: f64,
        lon: f64,
        near: Located,
    },
    /// Its photos are left as they are by this tool.
    Leave,
    /// Each named camera's clock was off by this much.
    Shift(Vec<Moved>),
    /// This date, and a second more for each photo after the first.
    Date(String),
    /// Each photo the time between the dated photos before and after it.
    Neighbours,
    /// Taken in this IANA time zone.
    Zone(String),
    /// This person is this tag.
    Tag(String),
    /// Into this folder of the library: an event's new place, or the event a loose photo joins.
    Folder(String),
}

/// One camera of an event, and how far its clock was off.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub camera: String,
    pub by: Shift,
    /// The camera's first date when the shift was given: once that has moved, the shift has been
    /// written, and it is never written twice.
    pub from: String,
}

const LEAVE: &str = "leave";
const PIN: &str = "pin";
const NEIGHBOURS: &str = "neighbours";
const SHIFT: &str = "shift";
const DATE: &str = "date";
const ZONE: &str = "zone";
const TAG: &str = "tag";
const FOLDER: &str = "folder";

/// Where an answer puts its photos.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot<'a> {
    pub lat: f64,
    pub lon: f64,
    /// The place in words.
    pub near: &'a Located,
    /// How far off the point may be.
    pub metres: f64,
}

impl Answer {
    /// A pin at a point, named after the place it is in. `None` where the place data knows
    /// nothing around it.
    pub fn pin(lat: f64, lon: f64, at: &At) -> Option<Answer> {
        Some(Answer::Pin {
            lat,
            lon,
            near: Located::near(at)?,
        })
    }

    /// `leave`, a place or a pin as a JSON object: what one answer is in the settings.
    pub fn read(text: &str) -> Result<Answer, String> {
        if text.trim() == LEAVE {
            return Ok(Answer::Leave);
        }
        let value: Value = serde_json::from_str(text).map_err(|_| format!("{text} is not an answer"))?;
        Answer::of(&value)
    }

    fn of(value: &Value) -> Result<Answer, String> {
        match value {
            Value::String(word) if word == LEAVE => Ok(Answer::Leave),
            Value::String(word) if word == NEIGHBOURS => Ok(Answer::Neighbours),
            Value::Object(fields) if fields.contains_key(SHIFT) => {
                let wrong = || format!("{value} is not a shift");
                let cameras = fields.get(SHIFT).and_then(Value::as_array).ok_or_else(wrong)?;
                let mut moved = Vec::new();
                for camera in cameras {
                    let text = |name: &str| camera.get(name).and_then(Value::as_str).ok_or_else(wrong);
                    moved.push(Moved {
                        camera: text("camera")?.to_string(),
                        by: Shift::read(text("by")?)?,
                        from: crate::dates::format(crate::dates::parse(text("from")?)?),
                    });
                }
                if moved.is_empty() {
                    return Err(wrong());
                }
                Ok(Answer::Shift(moved))
            }
            Value::Object(fields) if fields.contains_key(DATE) => {
                let at = fields
                    .get(DATE)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{value} is not a date"))?;
                Ok(Answer::Date(crate::dates::format(crate::dates::parse(at)?)))
            }
            Value::Object(fields) if fields.contains_key(ZONE) => {
                let zone = fields
                    .get(ZONE)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{value} is not a zone"))?;
                crate::dates::offset_in(zone, "2000-01-01 12:00:00")?;
                Ok(Answer::Zone(zone.to_string()))
            }
            Value::Object(fields) if fields.contains_key(TAG) => {
                let path = fields
                    .get(TAG)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{value} is not a tag"))?;
                Ok(Answer::Tag(crate::tags::path(path)?))
            }
            Value::Object(fields) if fields.contains_key(FOLDER) => {
                let path = fields
                    .get(FOLDER)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{value} is not a folder"))?;
                Ok(Answer::Folder(folders::event_folder(path)?))
            }
            Value::Object(fields) if fields.contains_key(PIN) => {
                let wrong = || format!("{value} is not a pin");
                let point = fields.get(PIN).and_then(Value::as_array).ok_or_else(wrong)?;
                let [lat, lon] = point.as_slice() else {
                    return Err(wrong());
                };
                let (lat, lon) = (lat.as_f64().ok_or_else(wrong)?, lon.as_f64().ok_or_else(wrong)?);
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(format!("{lat}, {lon} is not on earth"));
                }
                let near = Located::read(fields.get("near").ok_or_else(wrong)?)?;
                Ok(Answer::Pin { lat, lon, near })
            }
            Value::Object(_) => Ok(Answer::Place(Located::read(value)?)),
            other => Err(format!("{other} is not an answer")),
        }
    }

    pub fn written(&self) -> String {
        self.value().to_string()
    }

    fn value(&self) -> Value {
        match self {
            Answer::Leave => Value::from(LEAVE),
            Answer::Neighbours => Value::from(NEIGHBOURS),
            Answer::Shift(moved) => {
                let cameras: Vec<Value> = moved
                    .iter()
                    .map(|moved| {
                        let mut fields = Map::new();
                        fields.insert("camera".to_string(), Value::from(moved.camera.as_str()));
                        fields.insert("by".to_string(), Value::from(moved.by.written()));
                        fields.insert("from".to_string(), Value::from(moved.from.as_str()));
                        Value::Object(fields)
                    })
                    .collect();
                let mut fields = Map::new();
                fields.insert(SHIFT.to_string(), Value::from(cameras));
                Value::Object(fields)
            }
            Answer::Date(at) => {
                let mut fields = Map::new();
                fields.insert(DATE.to_string(), Value::from(at.as_str()));
                Value::Object(fields)
            }
            Answer::Zone(zone) => {
                let mut fields = Map::new();
                fields.insert(ZONE.to_string(), Value::from(zone.as_str()));
                Value::Object(fields)
            }
            Answer::Tag(path) => {
                let mut fields = Map::new();
                fields.insert(TAG.to_string(), Value::from(path.as_str()));
                Value::Object(fields)
            }
            Answer::Folder(path) => {
                let mut fields = Map::new();
                fields.insert(FOLDER.to_string(), Value::from(path.as_str()));
                Value::Object(fields)
            }
            Answer::Place(place) => place.written(),
            Answer::Pin { lat, lon, near } => {
                let mut fields = Map::new();
                fields.insert(PIN.to_string(), Value::from(vec![*lat, *lon]));
                fields.insert("near".to_string(), near.written());
                Value::Object(fields)
            }
        }
    }

    /// `Beijing, Beijing, China`, a point near it, a shift, a date, a zone, or that it is left
    /// alone.
    pub fn tells(&self) -> String {
        match self {
            Answer::Leave => "Left alone".to_string(),
            Answer::Neighbours => "Between its neighbours".to_string(),
            Answer::Date(at) => format!("From {at}"),
            Answer::Zone(zone) => format!("In {zone}"),
            Answer::Tag(path) => format!("Tagged {path}"),
            Answer::Folder(path) => format!("Into {path}"),
            Answer::Shift(moved) => moved
                .iter()
                .map(|moved| match moved.by.over_a_year() {
                    true => format!("{} shifted {}, more than a year", moved.camera, moved.by.written()),
                    false => format!("{} shifted {}", moved.camera, moved.by.written()),
                })
                .collect::<Vec<String>>()
                .join("; "),
            Answer::Place(place) => place.tells(),
            Answer::Pin { lat, lon, near } => format!("A point near {} ({lat:.5}, {lon:.5})", near.tells()),
        }
    }

    /// `Beijing`, or a point near it: short enough for a refusal.
    pub fn names(&self) -> String {
        match self {
            Answer::Leave => "nothing".to_string(),
            Answer::Neighbours
            | Answer::Date(_)
            | Answer::Zone(_)
            | Answer::Shift(_)
            | Answer::Tag(_)
            | Answer::Folder(_) => self.tells(),
            Answer::Place(place) => place.name.clone(),
            Answer::Pin { near, .. } => format!("a point near {}", near.name),
        }
    }

    /// Where its photos go, if anywhere.
    pub fn spot(&self) -> Option<Spot<'_>> {
        match self {
            Answer::Leave
            | Answer::Neighbours
            | Answer::Date(_)
            | Answer::Zone(_)
            | Answer::Shift(_)
            | Answer::Tag(_)
            | Answer::Folder(_) => None,
            Answer::Place(place) => Some(Spot {
                lat: place.lat,
                lon: place.lon,
                near: place,
                metres: PLACE_METRES,
            }),
            Answer::Pin { lat, lon, near } => Some(Spot {
                lat: *lat,
                lon: *lon,
                near,
                metres: PIN_METRES,
            }),
        }
    }

    /// Whether two answers put a photo in the same place: the same place, or a pin on the same
    /// point. A pin and a place are never the same, even a pin on the place's centre.
    pub fn same(&self, other: &Answer) -> bool {
        match (self, other) {
            (Answer::Place(one), Answer::Place(other)) => one.id == other.id,
            (
                Answer::Pin { lat, lon, .. },
                Answer::Pin {
                    lat: lat2, lon: lon2, ..
                },
            ) => lat == lat2 && lon == lon2,
            (Answer::Place(_) | Answer::Pin { .. }, _) => false,
            (one, other) => one == other,
        }
    }
}

/// What photos without a position get from the answers that place them: the point with the mark
/// of where it came from, and the place in words only where a photo says nothing yet, since
/// writing the words takes away every part it does not set.
pub fn place_photos(cache: &Cache, placed: &[(String, &Answer)], method: &'static str) -> cache::Result<Vec<Wanted>> {
    let paths: Vec<String> = placed.iter().map(|(rel_path, _)| rel_path.clone()).collect();
    let with_text = cache.with_place_text(&paths)?;
    let mut wanted = Vec::new();
    for (rel_path, answer) in placed {
        let Some(spot) = answer.spot() else {
            continue;
        };
        let mut fields = vec![Field::Gps(Some(Gps {
            lat: spot.lat,
            lon: spot.lon,
            altitude: None,
            derived: Some(Derived {
                method,
                metres: spot.metres,
            }),
        }))];
        if !with_text.contains(rel_path) {
            fields.push(Field::Place(Some(spot.near.place())));
        }
        wanted.push(Wanted::new(rel_path.clone(), Change::of(fields)));
    }
    Ok(wanted)
}

/// Every answer given so far, by question. Written as one JSON object, sorted by question, so
/// the same answers are always the same text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answers(pub BTreeMap<String, Answer>);

impl Answers {
    pub fn get(&self, key: &str) -> Option<&Answer> {
        self.0.get(key)
    }

    /// `None` forgets the answer, so the question is asked again.
    pub fn set(&mut self, key: &str, answer: Option<Answer>) {
        match answer {
            Some(answer) => self.0.insert(key.to_string(), answer),
            None => self.0.remove(key),
        };
    }
}

impl Settings for Answers {
    fn read(text: &str) -> Result<Answers, String> {
        if text.trim().is_empty() {
            return Ok(Answers::default());
        }
        let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) else {
            return Err(format!("{text} is not a list of answers"));
        };
        let mut answers = Answers::default();
        for (key, value) in fields {
            let answer = Answer::of(&value).map_err(|why| format!("{why} to {key}"))?;
            answers.0.insert(key, answer);
        }
        Ok(answers)
    }

    fn written(&self) -> String {
        let fields: Map<String, Value> = self
            .0
            .iter()
            .map(|(key, answer)| (key.clone(), answer.value()))
            .collect();
        Value::Object(fields).to_string()
    }
}

/// An answer offered for a question, in words, and how sure the offer is.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub answer: Answer,
    /// What the page says for it.
    pub words: String,
    pub confidence: f64,
    /// A name of the place, spelled the same once folded.
    pub exact: bool,
    /// Sure enough to be confirmed in one click with the others, by the tool's bulk button.
    pub sure: bool,
    /// How many photos that belong with the question are already there, when that is why it
    /// is offered.
    pub located: Option<usize>,
}

impl Offer {
    /// A candidate of the place data: sure when it matched a name exactly and is sure of it.
    pub fn of(candidate: &lookup::Candidate) -> Offer {
        let exact = candidate.how == How::Exact;
        Offer::of_place(Located::of(&candidate.place), candidate.confidence, exact)
            .with_sure(exact && candidate.confidence >= EXACT)
    }

    /// A place, in its words.
    pub fn of_place(place: Located, confidence: f64, exact: bool) -> Offer {
        Offer {
            words: place.tells(),
            answer: Answer::Place(place),
            confidence,
            exact,
            sure: false,
            located: None,
        }
    }

    /// Any other answer, in the words given.
    pub fn of_answer(answer: Answer, words: impl Into<String>) -> Offer {
        Offer {
            answer,
            words: words.into(),
            confidence: 1.0,
            exact: false,
            sure: false,
            located: None,
        }
    }

    pub fn with_sure(self, sure: bool) -> Offer {
        Offer { sure, ..self }
    }

    /// The place it offers, when it offers one.
    pub fn place(&self) -> Option<&Located> {
        match &self.answer {
            Answer::Place(place) => Some(place),
            _ => None,
        }
    }
}

/// What a question asks for, which decides what the page offers beside the offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A place: another one is chosen by name or on the map.
    Place,
    /// How far each camera's clock was off: a shift is typed.
    Shift,
    /// A date: one is typed.
    Date,
    /// A time zone: only the offers.
    Zone,
    /// Which tag a person is: one is typed.
    Person,
    /// Which folder an event or a photo belongs in: its parts are typed.
    Folder,
}

/// One camera of an event, as a question about its dates shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Evidence {
    /// The camera's name, or `no camera`.
    pub camera: String,
    pub photos: usize,
    pub first: String,
    pub last: String,
    /// How many days its photos are from the folder date, where they disagree with it.
    pub days_off: Option<i64>,
}

impl Evidence {
    /// `X100S: 49 photos, 2009-12-31 10:00:00 to 2009-12-31 18:00:00, 640 days after the folder`.
    pub fn tells(&self) -> String {
        format!("{}: {}", self.camera, self.facts())
    }

    /// The same without the camera's name.
    pub fn facts(&self) -> String {
        let photos = match self.photos {
            1 => "1 photo".to_string(),
            count => format!("{count} photos"),
        };
        let when = match self.first == self.last {
            true => self.first.clone(),
            false => format!("{} to {}", self.first, self.last),
        };
        let off = match self.days_off {
            None => "agrees with the folder".to_string(),
            Some(1) => "1 day after the folder".to_string(),
            Some(-1) => "1 day before the folder".to_string(),
            Some(days) if days > 0 => format!("{days} days after the folder"),
            Some(days) => format!("{} days before the folder", -days),
        };
        format!("{photos}, {when}, {off}")
    }
}

/// What a group of photos needs to be told: which place it means, how far a camera was off, or
/// when it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// What the answer is kept under.
    pub key: String,
    pub title: String,
    pub kind: Kind,
    pub photos: usize,
    /// Best first. Empty when there is no place data to ask.
    pub offers: Vec<Offer>,
    pub answer: Option<Answer>,
    /// Listed apart and left alone until someone answers it by hand.
    pub apart: bool,
    /// Something the person should know about the offers, such as that they were not narrowed.
    pub note: Option<String>,
    /// What the question is decided on, camera by camera, for a question about dates.
    pub evidence: Vec<Evidence>,
}

impl Question {
    /// A question about a place, as the GPS tools ask it.
    pub fn place(key: impl Into<String>, title: impl Into<String>, photos: usize, offers: Vec<Offer>) -> Question {
        Question {
            key: key.into(),
            title: title.into(),
            kind: Kind::Place,
            photos,
            offers,
            answer: None,
            apart: false,
            note: None,
            evidence: Vec::new(),
        }
    }
}

/// A shift typed by hand, one text per camera of the question, empty where that camera is right.
pub fn shift_answer(question: &Question, typed: &[(String, String)]) -> Result<Answer, String> {
    let mut moved = Vec::new();
    for (camera, text) in typed {
        if text.trim().is_empty() {
            continue;
        }
        let evidence = question
            .evidence
            .iter()
            .find(|evidence| &evidence.camera == camera)
            .ok_or_else(|| format!("{} has no camera called {camera}", question.title))?;
        let by = Shift::read(text).map_err(|why| format!("{camera}: {why}"))?;
        by.apply(&evidence.first).map_err(|why| format!("{camera}: {why}"))?;
        moved.push(Moved {
            camera: camera.clone(),
            by,
            from: evidence.first.clone(),
        });
    }
    match moved.is_empty() {
        true => Err("type a shift for at least one camera".to_string()),
        false => Ok(Answer::Shift(moved)),
    }
}

/// A date typed by hand.
pub fn date_answer(text: &str) -> Result<Answer, String> {
    Ok(Answer::Date(crate::dates::format(crate::dates::parse(text)?)))
}

/// A folder typed by hand, from its parts as the layout puts them.
pub fn folder_answer(layout: &crate::layout::Layout, parts: &folders::Parts) -> Result<Answer, String> {
    Ok(Answer::Folder(folders::assembled(
        layout,
        &parts.named,
        &parts.date,
        &parts.name,
    )?))
}

/// A tag typed by hand, its levels separated by `/`.
pub fn tag_answer(text: &str) -> Result<Answer, String> {
    Ok(Answer::Tag(crate::tags::path(text)?))
}

/// Something a tool found out about the scope that is no question: a line above its questions,
/// with a list below it where there is one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finding {
    pub title: String,
    pub detail: String,
    /// A name and what is said about it.
    pub rows: Vec<(String, String)>,
}

impl Question {
    pub fn waits(&self) -> bool {
        self.answer.is_none()
    }

    /// The best offer, when it is sure enough to be confirmed in one click with the others.
    pub fn sure(&self) -> Option<&Offer> {
        self.offers.first().filter(|offer| offer.sure)
    }

    /// Whether the tool's bulk button would answer it.
    pub fn confirmable(&self) -> bool {
        !self.apart && self.waits() && self.sure().is_some()
    }
}

/// What the Tools page opens for a tool. The page is chosen by what it is, never by which tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Straight to the preview.
    Preview,
    /// Its questions first.
    Questions,
    /// The tag tree with its rules and suggestions.
    Vocabulary,
    /// One line of text first, which becomes the settings.
    Entry {
        title: &'static str,
        description: &'static str,
    },
}

pub trait Tool: Sync {
    type Settings: Settings;

    /// Stays the same for as long as the tool exists: the journal keeps it.
    fn key(&self) -> &'static str;
    fn title(&self) -> &'static str;
    /// One line on what it fixes.
    fn fixes(&self) -> &'static str;

    /// What a pass with these settings is called, in the preview and in the history.
    fn named(&self, _settings: &Self::Settings) -> String {
        self.title().to_string()
    }

    /// What each photo of the scope should say. Reads the cache and the place data, never a photo.
    fn wanted(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: &Self::Settings,
    ) -> cache::Result<Vec<Wanted>>;

    /// The answers inside the settings, for a tool that asks. A tool that asks nothing has none.
    fn answers<'a>(&self, _settings: &'a mut Self::Settings) -> Option<&'a mut Answers> {
        None
    }

    /// What it asks about the scope, with the answers so far. Without place data the questions
    /// come without offers.
    fn questions(
        &self,
        _cache: &Cache,
        _geo: Option<&Geo>,
        _scope: &Scope,
        _settings: &Self::Settings,
    ) -> Result<Vec<Question>, String> {
        Ok(Vec::new())
    }

    /// What it found out about the scope beyond its questions, shown above them.
    fn report(
        &self,
        _cache: &Cache,
        _geo: Option<&Geo>,
        _scope: &Scope,
        _settings: &Self::Settings,
    ) -> Result<Vec<Finding>, String> {
        Ok(Vec::new())
    }

    /// Whether it needs the place data to know what to ask, not only for the offers.
    fn asks_with_place_data(&self) -> bool {
        false
    }

    /// A page of its own; without one a tool that asks shows its questions and any other its
    /// preview.
    fn page(&self) -> Option<Page> {
        None
    }

    /// What its row says while questions wait for an answer.
    fn waiting(&self, open: usize) -> String {
        match open {
            1 => "1 question waits for an answer".to_string(),
            open => format!("{open} questions wait for an answer"),
        }
    }

    /// The words its question page uses.
    fn wording(&self) -> Wording {
        Wording::default()
    }

    /// Whether it moves folders and photos rather than writing them. Such a tool runs alone.
    fn moves(&self) -> bool {
        false
    }
}

/// What a tool's question page calls things. The page is the same for every tool; only the
/// words change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wording {
    /// The heading over the questions.
    pub asked: &'static str,
    /// What one of them is, and many: `tag`, `tags`.
    pub one: &'static str,
    pub many: &'static str,
    /// The bulk button that answers the sure ones. A tool without one leaves it empty.
    pub confirm: &'static str,
    /// What makes a question sure, after "1 tag" or "3 tags": `matches a place by its exact name`.
    pub sure_one: &'static str,
    pub sure_many: &'static str,
    /// What the page says when there is nothing to ask.
    pub unasked: &'static str,
}

impl Default for Wording {
    fn default() -> Wording {
        Wording {
            asked: "Questions",
            one: "question",
            many: "questions",
            confirm: "Confirm Sure Answers",
            sure_one: "has a sure answer",
            sure_many: "have a sure answer",
            unasked: "No photo of the scope is waiting for this tool. Choose another scope on the Tools page.",
        }
    }
}

impl Wording {
    /// `3 tags match a place by its exact name. Every other tag is one click.`
    pub fn sure(&self, count: usize) -> String {
        let rest = format!("Every other {} is one click.", self.one);
        match count {
            0 => format!("Nothing waits that {}. {rest}", self.sure_one),
            1 => format!("1 {} {}. {rest}", self.one, self.sure_one),
            count => format!("{count} {} {}. {rest}", self.many, self.sure_many),
        }
    }
}

/// A tool as the list holds it, its settings as text.
pub trait AnyTool: Sync {
    fn key(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn fixes(&self) -> &'static str;
    /// What a pass with these settings is called, and what each photo of the scope should say.
    fn wanted(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<(String, Vec<Wanted>), String>;
    /// `None` is the tool's own defaults.
    fn change_set(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<ChangeSet, String>;
    /// Whether it asks before it can change anything.
    fn asks(&self) -> bool;
    fn asks_with_place_data(&self) -> bool;
    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<Vec<Question>, String>;
    fn report(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<Vec<Finding>, String>;
    /// The settings with one answer given, or forgotten with `None`, as text.
    fn answer(&self, settings: Option<&str>, question: &str, answer: Option<Answer>) -> Result<String, String>;
    /// What the settings answer a question with, if anything.
    fn answered(&self, settings: Option<&str>, question: &str) -> Result<Option<Answer>, String>;
    fn waiting(&self, open: usize) -> String;
    fn wording(&self) -> Wording;
    fn moves(&self) -> bool;
    fn page(&self) -> Page;
    /// Whether these settings can be read, and why not.
    fn check(&self, settings: &str) -> Result<(), String>;
}

fn settings_of<S: Settings>(text: Option<&str>) -> Result<S, String> {
    match text {
        Some(text) => S::read(text),
        None => Ok(S::default()),
    }
}

impl<T: Tool> AnyTool for T {
    fn key(&self) -> &'static str {
        Tool::key(self)
    }

    fn title(&self) -> &'static str {
        Tool::title(self)
    }

    fn fixes(&self) -> &'static str {
        Tool::fixes(self)
    }

    fn wanted(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<(String, Vec<Wanted>), String> {
        let settings = settings_of::<T::Settings>(settings)?;
        let wanted = Tool::wanted(self, cache, geo, scope, &settings).map_err(|error| error.to_string())?;
        Ok((self.named(&settings), wanted))
    }

    fn change_set(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<ChangeSet, String> {
        let (named, wanted) = AnyTool::wanted(self, cache, geo, scope, settings)?;
        let mut set = ChangeSet::build(cache, &named, &wanted).map_err(|error| error.to_string())?;
        set.tool = Some(Tool::key(self).to_string());
        Ok(set)
    }

    fn asks(&self) -> bool {
        Tool::answers(self, &mut T::Settings::default()).is_some()
    }

    fn asks_with_place_data(&self) -> bool {
        Tool::asks_with_place_data(self)
    }

    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<Vec<Question>, String> {
        Tool::questions(self, cache, geo, scope, &settings_of::<T::Settings>(settings)?)
    }

    fn report(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<Vec<Finding>, String> {
        Tool::report(self, cache, geo, scope, &settings_of::<T::Settings>(settings)?)
    }

    fn answer(&self, settings: Option<&str>, question: &str, answer: Option<Answer>) -> Result<String, String> {
        let mut settings = settings_of::<T::Settings>(settings)?;
        let answers = Tool::answers(self, &mut settings).ok_or_else(|| format!("{} asks nothing", self.title()))?;
        answers.set(question, answer);
        Ok(settings.written())
    }

    fn answered(&self, settings: Option<&str>, question: &str) -> Result<Option<Answer>, String> {
        let mut settings = settings_of::<T::Settings>(settings)?;
        Ok(Tool::answers(self, &mut settings).and_then(|answers| answers.get(question).cloned()))
    }

    fn waiting(&self, open: usize) -> String {
        Tool::waiting(self, open)
    }

    fn wording(&self) -> Wording {
        Tool::wording(self)
    }

    fn moves(&self) -> bool {
        Tool::moves(self)
    }

    fn check(&self, settings: &str) -> Result<(), String> {
        T::Settings::read(settings).map(|_| ())
    }

    fn page(&self) -> Page {
        match Tool::page(self) {
            Some(page) => page,
            None if AnyTool::asks(self) => Page::Questions,
            None => Page::Preview,
        }
    }
}

/// Every tool there is, in the order they are listed: the date tools in the order they are best
/// run, so a photo is written once.
pub const ALL: &[&dyn AnyTool] = &[
    &gps_from_places::GpsFromPlacesTag,
    &gps_from_event::GpsFromEvent,
    &dates_folder::DatesAgainstTheFolder,
    &undated::PhotosWithoutADate,
    &time_zones::TimeZones,
    &tag_vocabulary::TagVocabulary,
    &add_tag::AddATag,
    &people::PeopleFromImmich,
    &folders::FolderMigration,
    #[cfg(feature = "demo")]
    &demo::Rating,
];

pub fn find(key: &str) -> Option<&'static dyn AnyTool> {
    ALL.iter().copied().find(|tool| tool.key() == key)
}

/// Several tools as one pass, so a photo is written once: each photo's changes of every tool
/// merged into one, in the order the tools are given. Two tools that would set the same field
/// of one photo are a refusal naming both. A tool that refuses a photo leaves it to the others;
/// only a photo every tool refuses is refused, with each reason.
pub fn together(
    chosen: &[(&dyn AnyTool, Option<&str>)],
    cache: &Cache,
    geo: Option<&Geo>,
    scope: &Scope,
) -> Result<ChangeSet, String> {
    if chosen.len() > 1
        && let Some((tool, _)) = chosen.iter().find(|(tool, _)| tool.moves())
    {
        return Err(format!(
            "{} moves folders and runs alone, never together with a tool that writes",
            tool.title()
        ));
    }
    struct Merged {
        change: Change,
        by: Vec<&'static str>,
        refused: Vec<String>,
        clash: Option<String>,
    }
    let mut order: Vec<String> = Vec::new();
    let mut merged: BTreeMap<String, Merged> = BTreeMap::new();
    let mut names = Vec::new();
    for (tool, settings) in chosen {
        let (named, wanted) = tool.wanted(cache, geo, scope, *settings)?;
        names.push(named);
        for one in wanted {
            let entry = merged.entry(one.rel_path.clone()).or_insert_with(|| {
                order.push(one.rel_path.clone());
                Merged {
                    change: Change::default(),
                    by: Vec::new(),
                    refused: Vec::new(),
                    clash: None,
                }
            });
            if let Some(why) = one.refused {
                entry.refused.push(format!("{}: {why}", tool.title()));
                continue;
            }
            for field in one.change.fields {
                let kind = std::mem::discriminant(&field);
                if let Some(at) = entry
                    .change
                    .fields
                    .iter()
                    .position(|had| std::mem::discriminant(had) == kind)
                {
                    entry.clash.get_or_insert(format!(
                        "{} and {} would both set the {}",
                        entry.by[at],
                        tool.title(),
                        field_name(&field)
                    ));
                    continue;
                }
                entry.change.fields.push(field);
                entry.by.push(tool.title());
            }
        }
    }
    let wanted: Vec<Wanted> = order
        .into_iter()
        .map(|rel_path| {
            let one = merged.remove(&rel_path).expect("every path was merged");
            match (one.clash, one.change.is_empty()) {
                (Some(clash), _) => Wanted::refused(rel_path, clash),
                (None, true) => Wanted::refused(rel_path, one.refused.join("; ")),
                (None, false) => Wanted::new(rel_path, one.change),
            }
        })
        .collect();
    let mut set = ChangeSet::build(cache, &joined(&names), &wanted).map_err(|error| error.to_string())?;
    set.tool = Some(
        chosen
            .iter()
            .map(|(tool, _)| tool.key())
            .collect::<Vec<&str>>()
            .join("+"),
    );
    Ok(set)
}

fn field_name(field: &Field) -> &'static str {
    match field {
        Field::Tags(_) => "tags",
        Field::Rating(_) => "rating",
        Field::Gps(_) => "position",
        Field::Place(_) => "place words",
        Field::Taken(_) => "date",
        Field::Faces(_) => "faces",
        Field::DropLabel => "label",
        Field::DropCatalogSets => "catalog sets",
    }
}

/// `A`, `A and B`, `A, B and C`.
fn joined(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), lowercase_first(last)),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How many photos of the scope the tool would change right now, with these settings or its
/// own defaults: the same change set the preview shows, so the two numbers cannot differ.
pub fn count(
    tool: &dyn AnyTool,
    cache: &Cache,
    geo: Option<&Geo>,
    scope: &Scope,
    settings: Option<&str>,
) -> Result<usize, String> {
    Ok(tool.change_set(cache, geo, scope, settings)?.counts().change)
}

/// How many of its questions about the scope wait for an answer. Asked without the place data,
/// which most tools need only for the offers.
pub fn waiting(
    tool: &dyn AnyTool,
    cache: &Cache,
    geo: Option<&Geo>,
    scope: &Scope,
    settings: Option<&str>,
) -> Result<usize, String> {
    if !tool.asks() {
        return Ok(0);
    }
    let geo = geo.filter(|_| tool.asks_with_place_data());
    Ok(tool
        .questions(cache, geo, scope, settings)?
        .iter()
        .filter(|question| question.waits())
        .count())
}

/// The questions asked before, with the answers these settings give them: what a page shows
/// after one answer without asking the whole library again. A question listed apart that has
/// no answer is left alone, as it was when it was asked.
pub fn answer_again(tool: &dyn AnyTool, questions: &mut [Question], settings: Option<&str>) -> Result<(), String> {
    for question in questions {
        question.answer = match tool.answered(settings, &question.key)? {
            Some(answer) => Some(answer),
            None if question.apart => Some(Answer::Leave),
            None => None,
        };
    }
    Ok(())
}

/// The tool's bulk button, Confirm Exact Matches or Confirm Where the Rest Is: every question
/// still waiting whose best offer is sure is answered with it. A question listed apart is never
/// among them.
pub fn confirm_sure(tool: &dyn AnyTool, questions: &[Question], settings: Option<&str>) -> Result<String, String> {
    let mut text = settings.map(String::from);
    for question in questions.iter().filter(|question| question.confirmable()) {
        if let Some(offer) = question.sure() {
            text = Some(tool.answer(text.as_deref(), &question.key, Some(offer.answer.clone()))?);
        }
    }
    match text {
        Some(text) => Ok(text),
        None => tool.answer(None, "", None),
    }
}

/// A rating over the whole scope, so that the way from a tool to a photo can be driven before
/// the first real tool exists. Development builds only.
#[cfg(feature = "demo")]
pub mod demo {
    use super::*;
    use crate::write::{Change, Field};

    pub struct Rating;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Stars(pub i64);

    impl Default for Stars {
        fn default() -> Stars {
            Stars(3)
        }
    }

    impl Settings for Stars {
        fn read(text: &str) -> Result<Stars, String> {
            match text.trim().parse::<i64>() {
                Ok(stars) if (0..=5).contains(&stars) => Ok(Stars(stars)),
                _ => Err(format!("{text} is not a rating from 0 to 5")),
            }
        }

        fn written(&self) -> String {
            self.0.to_string()
        }
    }

    impl Tool for Rating {
        type Settings = Stars;

        fn key(&self) -> &'static str {
            "demo-rating"
        }

        fn title(&self) -> &'static str {
            "Demo Rating"
        }

        fn fixes(&self) -> &'static str {
            "Sets one rating on every photo in the scope"
        }

        fn named(&self, stars: &Stars) -> String {
            format!("Set a rating of {}", stars.0)
        }

        fn wanted(
            &self,
            cache: &Cache,
            _geo: Option<&Geo>,
            scope: &Scope,
            stars: &Stars,
        ) -> cache::Result<Vec<Wanted>> {
            Ok(scope
                .paths(cache)?
                .into_iter()
                .map(|rel_path| Wanted::new(rel_path, Change::of([Field::Rating(Some(stars.0))])))
                .collect())
        }
    }
}

#[cfg(all(test, feature = "fixtures", feature = "demo"))]
mod tests {
    use super::*;
    use crate::filter::Filter;
    use crate::filter::tests::scanned;

    #[test]
    fn the_demo_over_a_scope_is_exactly_the_scope() {
        let cache = scanned("tools-demo");
        let demo = find("demo-rating").expect("the demo is listed");
        let scope = Scope::Filter(Filter::all().within("Germany"));

        let set = demo.change_set(&cache, None, &scope, Some("4")).unwrap();
        let rows: Vec<String> = set.rows.iter().map(|row| row.rel_path.clone()).collect();
        assert_eq!(rows, scope.paths(&cache).unwrap());
        assert_eq!(set.title, "Set a rating of 4");
        assert_eq!(set.tool.as_deref(), Some("demo-rating"));

        let whole = Scope::Filter(Filter::all());
        assert_eq!(
            count(demo, &cache, None, &whole, None).unwrap(),
            crate::fixtures::photo_count()
        );
        assert!(count(demo, &cache, None, &scope, None).unwrap() < crate::fixtures::photo_count());
        assert!(demo.change_set(&cache, None, &scope, Some("nine")).is_err());
    }

    #[test]
    fn settings_round_trip_through_text() {
        assert_eq!(demo::Stars::read(&demo::Stars(2).written()), Ok(demo::Stars(2)));
        assert_eq!(<()>::read(""), Ok(()));
        assert!(<()>::read("anything").is_err());
    }
}

#[cfg(test)]
mod answer_tests {
    use super::*;

    #[test]
    fn every_answer_round_trips_through_text() {
        let answers = [
            Answer::Leave,
            Answer::Neighbours,
            Answer::Date("2014-03-22 12:00:00".to_string()),
            Answer::Zone("America/Chicago".to_string()),
            Answer::Tag("people/family/Anna".to_string()),
            Answer::Shift(vec![Moved {
                camera: "DMC-TZ7".to_string(),
                by: Shift::read("-640d 00:07").unwrap(),
                from: "2011-09-02 15:00:00".to_string(),
            }]),
        ];
        let mut all = Answers::default();
        for (index, answer) in answers.iter().enumerate() {
            assert_eq!(
                Answer::read(&answer.written()).as_ref(),
                Ok(answer),
                "{}",
                answer.written()
            );
            all.set(&index.to_string(), Some(answer.clone()));
        }
        assert_eq!(Answers::read(&all.written()), Ok(all));
        assert_eq!(answers[5].tells(), "DMC-TZ7 shifted -640d 00:07, more than a year");
        assert_eq!(answers[4].tells(), "Tagged people/family/Anna");
        assert_eq!(answers[2].tells(), "From 2014-03-22 12:00:00");
    }

    #[test]
    fn a_broken_answer_says_why() {
        assert!(Answer::read(r#"{"date": "yesterday"}"#).is_err());
        assert!(Answer::read(r#"{"zone": "Nowhere/Atlantis"}"#).is_err());
        assert!(Answer::read(r#"{"shift": []}"#).is_err());
        assert!(Answer::read(r#"{"tag": "people//Anna"}"#).is_err());
        assert!(Answer::read(r#"{"shift": [{"camera": "X", "by": "soon", "from": "2011-09-02 15:00:00"}]}"#).is_err());
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod together_tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::testing::{Library, geo};

    const SEASONS: &str = "Germany/2015-00-00 Seasons";
    const SUMMER: &str = "Germany/2015-00-00 Seasons/IMG_8002.JPG";

    fn zones() -> &'static dyn AnyTool {
        find("time-zones").unwrap()
    }

    fn tags() -> &'static dyn AnyTool {
        find("tag-vocabulary").unwrap()
    }

    #[test]
    fn two_tools_are_one_row_and_one_write_per_photo_and_one_undo() {
        let mut library = Library::new("together");
        let scope = Scope::Filter(Filter::all().within(SEASONS));
        let geo = geo();
        let one = zones().change_set(&library.cache, Some(&geo), &scope, None).unwrap();
        let other = tags().change_set(&library.cache, Some(&geo), &scope, None).unwrap();
        let set = together(&[(zones(), None), (tags(), None)], &library.cache, Some(&geo), &scope).unwrap();
        assert_eq!(set.title, "Write time zones and XMP dates and tidy the tags");
        assert_eq!(set.tool.as_deref(), Some("time-zones+tag-vocabulary"));

        let paths = |set: &ChangeSet| {
            let mut all: Vec<String> = set.rows.iter().map(|row| row.rel_path.clone()).collect();
            all.sort();
            all.dedup();
            all
        };
        let mut either = paths(&one);
        either.extend(paths(&other));
        either.sort();
        either.dedup();
        assert_eq!(paths(&set), either, "one row per photo, none counted twice");
        assert_eq!(set.rows.len(), either.len());
        assert_eq!(set.counts().change, either.len());

        let summer = set.rows.iter().find(|row| row.rel_path == SUMMER).unwrap();
        let kinds: Vec<&str> = summer.change.fields.iter().map(field_name).collect();
        assert_eq!(kinds, ["date", "tags", "label", "catalog sets"]);

        let before = library.dates(SUMMER);
        let summary = library.apply(&set);
        assert_eq!(summary.written, either.len(), "one write per photo: {summary:?}");
        let after = library.dates(SUMMER);
        assert_eq!(after["ExifIFD:OffsetTimeOriginal"], "+02:00");

        library.rescan();
        let tidy = library.cache.stated(&[SUMMER.to_string()]).unwrap()[SUMMER].clone();
        assert!(!tidy.said.tags_untidy);
        assert_eq!(
            tidy.said.tags,
            ["events", "events/2015 Seasons", "timeline", "timeline/2015"]
        );

        let undone = library.undo();
        assert_eq!(undone.written, either.len(), "{undone:?}");
        assert_eq!(library.dates(SUMMER), before, "the dates are back");
        library.rescan();
        let back = library.cache.stated(&[SUMMER.to_string()]).unwrap()[SUMMER].clone();
        assert!(
            back.said.tags.is_empty(),
            "and the tags are gone again: {:?}",
            back.said.tags
        );
    }

    #[test]
    fn two_tools_setting_one_field_are_refused_with_both_named() {
        let library = Library::new("together-clash");
        let scope = Scope::Photos {
            title: "one".to_string(),
            paths: vec![SUMMER.to_string()],
        };
        let add = find("add-a-tag").unwrap();
        let set = together(
            &[(tags(), None), (add, Some("people/family/Anna"))],
            &library.cache,
            None,
            &scope,
        )
        .unwrap();
        assert_eq!(set.rows.len(), 1);
        assert_eq!(
            set.rows[0].verdict,
            Verdict::Refused("Tag Vocabulary and Add a Tag would both set the tags".to_string())
        );
    }
}
