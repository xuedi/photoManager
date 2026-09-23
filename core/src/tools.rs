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
//! A tool may also ask: which place a group of photos means. It hands over [`Question`]s, the
//! application draws them without knowing which tool asked, and the [`Answer`]s go back into the
//! tool's settings. So a question is answered once, and the next run over any scope finds it
//! answered.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::cache::{self, Cache};
use crate::changeset::{ChangeSet, Wanted};
use crate::geo::Geo;
use crate::geo::lookup::{self, How};
use crate::geo::reverse::At;
use crate::scope::Scope;
use crate::write;
use crate::write::change::Derived;
use crate::write::{Change, Field, Gps};

pub mod gps_from_event;
pub mod gps_from_places;

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

    /// The place a point is in, in words: of the places around it, the nearest one inside the
    /// country whose outline holds the point, since the nearest of all can be over a border.
    pub fn near(at: &At) -> Option<Located> {
        let inside = at
            .country
            .as_ref()
            .and_then(|(code, _)| at.places.iter().find(|nearby| &nearby.place.country == code));
        inside.or(at.places.first()).map(|nearby| Located::of(&nearby.place))
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
    /// Its photos are not given a place by this tool.
    Leave,
}

const LEAVE: &str = "leave";
const PIN: &str = "pin";

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
            Answer::Place(place) => place.written(),
            Answer::Pin { lat, lon, near } => {
                let mut fields = Map::new();
                fields.insert(PIN.to_string(), Value::from(vec![*lat, *lon]));
                fields.insert("near".to_string(), near.written());
                Value::Object(fields)
            }
        }
    }

    /// `Beijing, Beijing, China`, a point near it, or that it is left alone.
    pub fn tells(&self) -> String {
        match self {
            Answer::Leave => "Left alone".to_string(),
            Answer::Place(place) => place.tells(),
            Answer::Pin { lat, lon, near } => format!("A point near {} ({lat:.5}, {lon:.5})", near.tells()),
        }
    }

    /// `Beijing`, or a point near it: short enough for a refusal.
    pub fn names(&self) -> String {
        match self {
            Answer::Leave => "nothing".to_string(),
            Answer::Place(place) => place.name.clone(),
            Answer::Pin { near, .. } => format!("a point near {}", near.name),
        }
    }

    /// Where its photos go, if anywhere.
    pub fn spot(&self) -> Option<Spot<'_>> {
        match self {
            Answer::Leave => None,
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
            (Answer::Leave, Answer::Leave) => true,
            _ => false,
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

/// A place offered for a question, and how sure the offer is.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub place: Located,
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
        Offer {
            place: Located::of(&candidate.place),
            confidence: candidate.confidence,
            exact,
            sure: exact && candidate.confidence >= EXACT,
            located: None,
        }
    }
}

/// Which place a group of photos means.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    /// What the answer is kept under.
    pub key: String,
    pub title: String,
    pub photos: usize,
    /// Best first. Empty when there is no place data to ask.
    pub offers: Vec<Offer>,
    pub answer: Option<Answer>,
    /// Listed apart and left alone until someone answers it by hand.
    pub apart: bool,
    /// Something the person should know about the offers, such as that they were not narrowed.
    pub note: Option<String>,
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

    /// What each photo of the scope should say. Reads the cache, never a photo.
    fn wanted(&self, cache: &Cache, scope: &Scope, settings: &Self::Settings) -> cache::Result<Vec<Wanted>>;

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
    /// The bulk button that answers the sure ones.
    pub confirm: &'static str,
    /// What makes a question sure, after "1 tag" or "3 tags": `matches a place by its exact name`.
    pub sure_one: &'static str,
    pub sure_many: &'static str,
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
    /// `None` is the tool's own defaults.
    fn change_set(&self, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<ChangeSet, String>;
    /// Whether it asks before it can change anything.
    fn asks(&self) -> bool;
    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        settings: Option<&str>,
    ) -> Result<Vec<Question>, String>;
    /// The settings with one answer given, or forgotten with `None`, as text.
    fn answer(&self, settings: Option<&str>, question: &str, answer: Option<Answer>) -> Result<String, String>;
    /// What the settings answer a question with, if anything.
    fn answered(&self, settings: Option<&str>, question: &str) -> Result<Option<Answer>, String>;
    fn waiting(&self, open: usize) -> String;
    fn wording(&self) -> Wording;
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

    fn change_set(&self, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<ChangeSet, String> {
        let settings = settings_of::<T::Settings>(settings)?;
        let wanted = self
            .wanted(cache, scope, &settings)
            .map_err(|error| error.to_string())?;
        let mut set = ChangeSet::build(cache, &self.named(&settings), &wanted).map_err(|error| error.to_string())?;
        set.tool = Some(Tool::key(self).to_string());
        Ok(set)
    }

    fn asks(&self) -> bool {
        Tool::answers(self, &mut T::Settings::default()).is_some()
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
}

/// Every tool there is, in the order they are listed.
pub const ALL: &[&dyn AnyTool] = &[
    &gps_from_places::GpsFromPlacesTag,
    &gps_from_event::GpsFromEvent,
    #[cfg(feature = "demo")]
    &demo::Rating,
];

pub fn find(key: &str) -> Option<&'static dyn AnyTool> {
    ALL.iter().copied().find(|tool| tool.key() == key)
}

/// How many photos of the scope the tool would change right now, with these settings or its
/// own defaults: the same change set the preview shows, so the two numbers cannot differ.
pub fn count(tool: &dyn AnyTool, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<usize, String> {
    Ok(tool.change_set(cache, scope, settings)?.counts().change)
}

/// How many of its questions about the scope wait for an answer. Asked without the place data,
/// which only the offers need.
pub fn waiting(tool: &dyn AnyTool, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<usize, String> {
    if !tool.asks() {
        return Ok(0);
    }
    Ok(tool
        .questions(cache, None, scope, settings)?
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
            text = Some(tool.answer(text.as_deref(), &question.key, Some(Answer::Place(offer.place.clone())))?);
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

        fn wanted(&self, cache: &Cache, scope: &Scope, stars: &Stars) -> cache::Result<Vec<Wanted>> {
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

        let set = demo.change_set(&cache, &scope, Some("4")).unwrap();
        let rows: Vec<String> = set.rows.iter().map(|row| row.rel_path.clone()).collect();
        assert_eq!(rows, scope.paths(&cache).unwrap());
        assert_eq!(set.title, "Set a rating of 4");
        assert_eq!(set.tool.as_deref(), Some("demo-rating"));

        let whole = Scope::Filter(Filter::all());
        assert_eq!(
            count(demo, &cache, &whole, None).unwrap(),
            crate::fixtures::photo_count()
        );
        assert!(count(demo, &cache, &scope, None).unwrap() < crate::fixtures::photo_count());
        assert!(demo.change_set(&cache, &scope, Some("nine")).is_err());
    }

    #[test]
    fn settings_round_trip_through_text() {
        assert_eq!(demo::Stars::read(&demo::Stars(2).written()), Ok(demo::Stars(2)));
        assert_eq!(<()>::read(""), Ok(()));
        assert!(<()>::read("anything").is_err());
    }
}
