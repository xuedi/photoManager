//! What the fix finders share: a tool looks at the photos of a scope and says what each should
//! say, given its [`Answers`]. It may ask first - which place a tag means, which tag a person is,
//! which folder an event goes into - as [`Question`]s, each with offers best first. A question
//! whose best offer is sure becomes a suggestion; the others are left to the edits, where the
//! person gives the value by hand.
//!
//! The answers never outlive a suggestion: they are made from the sure offers when the list is
//! found, and applied with it. The photos stay the truth.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::geo::lookup::{self, How};
use crate::geo::reverse::At;
use crate::scope::Scope;
use crate::write;
use crate::write::change::Derived;
use crate::write::{Change, Field, Gps};

pub mod folders;
pub mod gps_from_event;
pub mod gps_from_places;
pub mod offsets;
pub mod people;
pub mod tag_vocabulary;
#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod testing;
pub mod time_zones;

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
    /// This person is this tag.
    Tag(String),
    /// Into this folder of the library: an event's new place, or the event a loose photo joins.
    Folder(String),
}

const LEAVE: &str = "leave";
const PIN: &str = "pin";
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

    /// `leave`, or an answer as a JSON object: what one answer is written as.
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
        let one = |name: &str, text: &str| {
            let mut fields = Map::new();
            fields.insert(name.to_string(), Value::from(text));
            Value::Object(fields)
        };
        match self {
            Answer::Leave => Value::from(LEAVE),
            Answer::Tag(path) => one(TAG, path),
            Answer::Folder(path) => one(FOLDER, path),
            Answer::Place(place) => place.written(),
            Answer::Pin { lat, lon, near } => {
                let mut fields = Map::new();
                fields.insert(PIN.to_string(), Value::from(vec![*lat, *lon]));
                fields.insert("near".to_string(), near.written());
                Value::Object(fields)
            }
        }
    }

    /// `Beijing, Beijing, China`, a point near it, a tag, a folder, or that it is left alone.
    pub fn tells(&self) -> String {
        match self {
            Answer::Leave => "Left alone".to_string(),
            Answer::Tag(path) => format!("Tagged {path}"),
            Answer::Folder(path) => format!("Into {path}"),
            Answer::Place(place) => place.tells(),
            Answer::Pin { lat, lon, near } => format!("A point near {} ({lat:.5}, {lon:.5})", near.tells()),
        }
    }

    /// `Beijing`, or a point near it: short enough for a refusal.
    pub fn names(&self) -> String {
        match self {
            Answer::Leave => "nothing".to_string(),
            Answer::Tag(_) | Answer::Folder(_) => self.tells(),
            Answer::Place(place) => place.name.clone(),
            Answer::Pin { near, .. } => format!("a point near {}", near.name),
        }
    }

    /// Where its photos go, if anywhere.
    pub fn spot(&self) -> Option<Spot<'_>> {
        match self {
            Answer::Leave | Answer::Tag(_) | Answer::Folder(_) => None,
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

/// What a group of photos needs to be told: which place it means, which tag a person is, or
/// which folder an event goes into.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// What the answer is kept under.
    pub key: String,
    pub title: String,
    pub photos: usize,
    /// Best first. Empty when there is no place data to ask.
    pub offers: Vec<Offer>,
    pub answer: Option<Answer>,
    /// Left alone until someone gives it a value by hand.
    pub apart: bool,
    /// Something to know about the offers, such as that they were not narrowed.
    pub note: Option<String>,
}

impl Question {
    pub fn place(key: impl Into<String>, title: impl Into<String>, photos: usize, offers: Vec<Offer>) -> Question {
        Question {
            key: key.into(),
            title: title.into(),
            photos,
            offers,
            answer: None,
            apart: false,
            note: None,
        }
    }

    pub fn waits(&self) -> bool {
        self.answer.is_none()
    }

    /// The best offer, when it is sure enough to be taken without asking.
    pub fn sure(&self) -> Option<&Offer> {
        self.offers.first().filter(|offer| offer.sure)
    }

    /// Whether it is waiting and its best offer is sure: what becomes a suggestion.
    pub fn confirmable(&self) -> bool {
        !self.apart && self.waits() && self.sure().is_some()
    }
}

/// Something that looks at a scope and says what each of its photos should say, given its
/// answers. The finders drive it: its questions whose best offer is sure become fixes, and the
/// answers of the ticked ones make the pass.
pub trait Tool: Sync {
    type Settings: Settings;

    /// What each photo of the scope should say. Reads the cache and the place data, never a photo.
    fn wanted(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: &Self::Settings,
    ) -> cache::Result<Vec<Wanted>>;

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
}

#[cfg(test)]
mod answer_tests {
    use super::*;

    #[test]
    fn every_answer_round_trips_through_text() {
        let answers = [
            Answer::Leave,
            Answer::Tag("people/family/Anna".to_string()),
            Answer::Folder("Germany/Hamburg/2019-07-13 Sommerfest".to_string()),
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
        assert_eq!(answers[1].tells(), "Tagged people/family/Anna");
    }

    #[test]
    fn a_broken_answer_says_why() {
        assert!(Answer::read(r#"{"tag": "people//Anna"}"#).is_err());
        assert!(Answer::read(r#"{"folder": "Germany/nowhere"}"#).is_err());
        assert!(Answer::read(r#"{"pin": [91, 0], "near": {}}"#).is_err());
    }
}
