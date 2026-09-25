//! Folder Migration: every event into the folder of its city, `Country/City/YYYY-MM-DD Event`.
//!
//! One question per event of the scope that is not in a city folder yet, answered with the folder
//! it belongs in. What is offered comes from what the photos already say, in this order: the city
//! the places tag of every photo names, the cities some of them name, the location text, where
//! its positions are, and a city named in the event's name. A city is spelled the way the library
//! already spells it - its city folder, else its places tag - so folders and tags agree. A photo
//! directly in a country folder is asked about too, offered the events of its country nearest to
//! its date.
//!
//! An event moves as a whole, with its sub-folders, by one rename; only the folder changes, never
//! a photo. Moves never run together with a tool that writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{Answer, Answers, EXACT, Finding, Kind, Located, Offer, Question, Tool, Wording};
use crate::cache::{self, Cache, Folder, Tagged};
use crate::changeset::Wanted;
use crate::geo::lookup::How;
use crate::geo::{Geo, fold};
use crate::layout::{Fit, Placement};
use crate::scope::Scope;
use crate::tags;
use crate::write::Move;

pub struct FolderMigration;

const PLACES: &str = "places";

/// A folder an event or a photo can be answered with: `Country/[City/]YYYY-MM-DD Name`, each
/// level a plain name. Returned as it should be kept.
pub fn event_folder(path: &str) -> Result<String, String> {
    let path = path.trim();
    let levels: Vec<&str> = path.split('/').collect();
    for level in &levels {
        if level.trim().is_empty() || level.trim() != *level {
            return Err(format!("{path} has an empty level or one with spaces around it"));
        }
        if level.starts_with('.') || level.contains('\\') {
            return Err(format!("{level} is not a folder name to make"));
        }
    }
    let placement = Placement::parse(&format!("{path}/-"));
    match placement.fits() && placement.event_dir.as_deref() == Some(path) {
        true => Ok(path.to_string()),
        false => Err(format!(
            "{path} is not Country/City/YYYY-MM-DD Event, the city and the event's name optional"
        )),
    }
}

/// A folder from its parts, the city and the name optional.
pub fn assembled(country: &str, city: &str, date: &str, name: &str) -> Result<String, String> {
    let (country, city, date, name) = (country.trim(), city.trim(), date.trim(), name.trim());
    if country.is_empty() {
        return Err("a folder needs its country".to_string());
    }
    if [country, city, name].iter().any(|part| part.contains('/')) {
        return Err("a part of a folder cannot contain /".to_string());
    }
    let event = match name.is_empty() {
        true => date.to_string(),
        false => format!("{date} {name}"),
    };
    let path = match city.is_empty() {
        true => format!("{country}/{event}"),
        false => format!("{country}/{city}/{event}"),
    };
    event_folder(&path).map_err(|_| format!("{date} is not a date as YYYY-MM-DD, with zeros for what is not known"))
}

/// The parts a question's answer is typed in, filled with its answer, else its best offer, else
/// where it is now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    pub country: String,
    pub city: String,
    pub date: String,
    pub name: String,
    /// An event keeps the date in its folder's name; a loose photo's new event is given one.
    pub date_fixed: bool,
}

impl Parts {
    pub fn of(question: &Question) -> Parts {
        let is_event = is_event(&question.key);
        let folder = question
            .answer
            .iter()
            .chain(question.offers.iter().map(|offer| &offer.answer))
            .find_map(|answer| match answer {
                Answer::Folder(path) => Some(path.clone()),
                _ => None,
            })
            .or_else(|| is_event.then(|| question.key.clone()));
        let placement = folder
            .map(|folder| Placement::parse(&format!("{folder}/-")))
            .unwrap_or_else(|| Placement::parse(&question.key));
        Parts {
            country: placement.country.unwrap_or_default(),
            city: placement.city.unwrap_or_default(),
            date: placement.event_text.unwrap_or_else(|| "0000-00-00".to_string()),
            name: placement.event_name.unwrap_or_default(),
            date_fixed: is_event,
        }
    }

    /// What the question's folder or photo would be called with these parts.
    pub fn after(&self, question: &Question) -> Result<String, String> {
        let folder = assembled(&self.country, &self.city, &self.date, &self.name)?;
        Ok(match is_event(&question.key) {
            true => folder,
            false => format!("{folder}/{}", file_name(&question.key)),
        })
    }
}

/// Whether a question is about an event folder rather than a loose photo.
fn is_event(key: &str) -> bool {
    Placement::parse(&format!("{key}/-")).event_dir.as_deref() == Some(key)
}

fn file_name(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

/// The folder an event is in, its name inside that folder: `2019-07-13 Sommerfest`.
fn event_name(dir: &str) -> &str {
    dir.rsplit('/').next().unwrap_or(dir)
}

/// A places tag: `places/inGermany/Hamburg` is Germany and Hamburg.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PlaceTag {
    country: String,
    city: Option<String>,
}

fn place_tag(path: &str) -> Option<PlaceTag> {
    let levels: Vec<&str> = path.split('/').collect();
    if !levels.first()?.eq_ignore_ascii_case(PLACES) {
        return None;
    }
    let country = levels.get(1)?;
    let country = match country.strip_prefix("in") {
        Some(rest) if !rest.is_empty() => rest,
        _ => country,
    };
    Some(PlaceTag {
        country: country.to_string(),
        city: levels.get(2).map(|city| city.to_string()),
    })
}

/// An event not in a city folder, and every photo in it.
struct Event {
    dir: String,
    country: String,
    photos: Vec<Tagged>,
    /// The folders inside it, which move with it.
    subs: BTreeSet<String>,
}

/// Countries by the codes the place data gives them, and by their spelling where it gives none.
struct Countries<'a> {
    geo: Option<&'a Geo>,
    folders: Vec<String>,
    codes: HashMap<String, Option<(String, String)>>,
}

impl<'a> Countries<'a> {
    fn new(geo: Option<&'a Geo>, folders: &[Folder]) -> Countries<'a> {
        let mut names: Vec<String> = folders.iter().filter_map(|folder| folder.country.clone()).collect();
        names.sort();
        names.dedup();
        Countries {
            geo,
            folders: names,
            codes: HashMap::new(),
        }
    }

    fn known(&mut self, text: &str) -> Option<(String, String)> {
        if let Some(known) = self.codes.get(text) {
            return known.clone();
        }
        let known = self.geo.and_then(|geo| geo.country(text).ok().flatten());
        self.codes.insert(text.to_string(), known.clone());
        known
    }

    fn same(&mut self, one: &str, other: &str) -> bool {
        match (self.known(one), self.known(other)) {
            (Some((one, _)), Some((other, _))) => one == other,
            _ => {
                let (one, other) = (fold(one), fold(other));
                one == other
                    || (one.len().abs_diff(other.len()) <= 2 && (one.starts_with(&other) || other.starts_with(&one)))
            }
        }
    }

    /// The folder a country goes in: the library's own when it has one, else the place data's
    /// name, else the tag's spelling.
    fn folder(&mut self, country: &str) -> String {
        let folders = self.folders.clone();
        if let Some(folder) = folders.into_iter().find(|folder| self.same(folder, country)) {
            return folder;
        }
        self.known(country)
            .map(|(_, name)| name.strip_prefix("The ").map(String::from).unwrap_or(name))
            .unwrap_or_else(|| country.to_string())
    }
}

/// How the library spells a city already: its city folder, else its places tag.
struct Spellings {
    folders: Vec<(String, String)>,
    tagged: Vec<String>,
}

impl Spellings {
    fn new(cache: &Cache, folders: &[Folder]) -> cache::Result<Spellings> {
        let mut tagged: Vec<String> = cache
            .tag_sets()?
            .into_iter()
            .flatten()
            .filter_map(|path| place_tag(&path)?.city)
            .collect();
        tagged.sort();
        tagged.dedup();
        let mut cities: Vec<(String, String)> = folders
            .iter()
            .filter_map(|folder| Some((folder.country.clone()?, folder.city.clone()?)))
            .collect();
        cities.sort();
        cities.dedup();
        Ok(Spellings {
            folders: cities,
            tagged,
        })
    }

    fn city(&self, country: &str, name: &str) -> String {
        let folded = fold(name);
        self.folders
            .iter()
            .find(|(within, city)| within == country && fold(city) == folded)
            .map(|(_, city)| city.clone())
            .or_else(|| self.tagged.iter().find(|city| fold(city) == folded).cloned())
            .unwrap_or_else(|| name.trim().to_string())
    }

    /// The cities the library knows that the text names as whole words.
    fn named_in(&self, text: &str) -> Vec<String> {
        let words = format!(" {} ", fold(text));
        let mut named: Vec<String> = self
            .folders
            .iter()
            .map(|(_, city)| city.clone())
            .chain(self.tagged.iter().cloned())
            .filter(|city| fold(city).len() >= 3 && words.contains(&format!(" {} ", fold(city))))
            .collect();
        named.sort();
        named.dedup();
        named
    }
}

/// What the photos of an event say about where it was.
#[derive(Default)]
struct Evidence {
    /// City by the places tags, with the folder's country or another, and how many photos say it.
    tagged: Vec<(String, String, usize)>,
    /// How many photos name a city in their places tag.
    with_city: usize,
    /// Every photo names exactly one city, the same one, in the folder's country.
    sure: bool,
    /// Other countries the places tags name, and how many photos name each.
    elsewhere: Vec<(String, usize)>,
    written: Vec<(String, usize)>,
    located: Vec<(String, usize)>,
    named: Vec<String>,
}

impl Evidence {
    fn several(&self) -> bool {
        self.tagged.len() > 1
    }
}

fn counted(counts: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut all: Vec<(String, usize)> = counts.into_iter().collect();
    all.sort_by(|one, other| other.1.cmp(&one.1).then(one.0.cmp(&other.0)));
    all
}

fn photos(count: usize) -> String {
    match count {
        1 => "1 photo".to_string(),
        count => format!("{count} photos"),
    }
}

/// What the tool knows about the library, gathered once per asking.
struct Survey<'a> {
    geo: Option<&'a Geo>,
    countries: Countries<'a>,
    spellings: Spellings,
    folders: Vec<Folder>,
    events: Vec<Event>,
    in_cities: usize,
    loose: Vec<Tagged>,
    positions: HashMap<String, Vec<(f64, f64)>>,
    towns: HashMap<(u64, u64), Option<Located>>,
}

impl<'a> Survey<'a> {
    fn take(cache: &Cache, geo: Option<&'a Geo>, scope: &Scope) -> cache::Result<Survey<'a>> {
        let paths = scope.paths(cache)?;
        let folders = cache.event_folders()?;
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let mut in_cities: BTreeSet<String> = BTreeSet::new();
        let mut loose_paths = Vec::new();
        for rel_path in &paths {
            let placement = Placement::parse(rel_path);
            match (&placement.event_dir, &placement.city) {
                (Some(dir), None) => {
                    dirs.insert(dir.clone());
                }
                (Some(dir), Some(_)) => {
                    in_cities.insert(dir.clone());
                }
                (None, _) if placement.fit == Fit::LooseInCountry => loose_paths.push(rel_path.clone()),
                _ => {}
            }
        }
        let mut events = Vec::new();
        for dir in &dirs {
            let inside = cache.under(dir)?;
            let placement = Placement::parse(&format!("{dir}/-"));
            let subs = inside
                .iter()
                .filter_map(|rel_path| Placement::parse(rel_path).sub_path)
                .filter_map(|sub| sub.split('/').next().map(String::from))
                .collect();
            events.push(Event {
                dir: dir.clone(),
                country: placement.country.unwrap_or_default(),
                photos: cache.tagged(&inside)?,
                subs,
            });
        }
        let mut positions: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        if geo.is_some() {
            let dirs: Vec<String> = dirs.into_iter().collect();
            for (dir, lat, lon) in cache.positions_in(&dirs)? {
                positions.entry(dir).or_default().push((lat, lon));
            }
        }
        Ok(Survey {
            geo,
            countries: Countries::new(geo, &folders),
            spellings: Spellings::new(cache, &folders)?,
            folders,
            events,
            in_cities: in_cities.len(),
            loose: cache.tagged(&loose_paths)?,
            positions,
            towns: HashMap::new(),
        })
    }

    fn town(&mut self, lat: f64, lon: f64) -> Option<Located> {
        let geo = self.geo?;
        self.towns
            .entry((lat.to_bits(), lon.to_bits()))
            .or_insert_with(|| geo.at(lat, lon).ok().and_then(|at| Located::near(&at)))
            .clone()
    }

    fn evidence(&mut self, at: usize) -> Evidence {
        let (country, dir) = (self.events[at].country.clone(), self.events[at].dir.clone());
        let mut evidence = Evidence::default();
        let mut cities: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut elsewhere: BTreeMap<String, usize> = BTreeMap::new();
        let mut one_each: Vec<Option<(String, String)>> = Vec::new();
        let mut written: BTreeMap<String, usize> = BTreeMap::new();
        let photos: Vec<(Vec<String>, Option<String>)> = self.events[at]
            .photos
            .iter()
            .map(|photo| (tags::deepest(&photo.tags), photo.city.clone()))
            .collect();
        for (deepest, city) in photos {
            let named: BTreeSet<PlaceTag> = deepest.iter().filter_map(|tag| place_tag(tag)).collect();
            let mut other_country = None;
            let mut named_cities = BTreeSet::new();
            for tag in &named {
                let home = self.countries.same(&country, &tag.country);
                let folder = match home {
                    true => country.clone(),
                    false => {
                        let folder = self.countries.folder(&tag.country);
                        other_country = Some(folder.clone());
                        folder
                    }
                };
                if let Some(city) = &tag.city {
                    named_cities.insert((folder.clone(), self.spellings.city(&folder, city)));
                }
            }
            if let Some(other) = other_country {
                *elsewhere.entry(other).or_default() += 1;
            }
            if !named_cities.is_empty() {
                evidence.with_city += 1;
            }
            for city in &named_cities {
                *cities.entry(city.clone()).or_default() += 1;
            }
            one_each.push(match named_cities.len() {
                1 => named_cities.into_iter().next(),
                _ => None,
            });
            if let Some(city) = city {
                *written.entry(self.spellings.city(&country, &city)).or_default() += 1;
            }
        }
        let first = one_each.first().cloned().flatten();
        evidence.sure = first.as_ref().is_some_and(|(within, _)| {
            *within == country && one_each.iter().all(|each| each.as_ref() == first.as_ref())
        });
        let mut tagged: Vec<(String, String, usize)> = cities
            .into_iter()
            .map(|((within, city), count)| (within, city, count))
            .collect();
        tagged.sort_by(|one, other| {
            other
                .2
                .cmp(&one.2)
                .then((one.0 != country).cmp(&(other.0 != country)))
                .then(one.1.cmp(&other.1))
        });
        evidence.tagged = tagged;
        evidence.elsewhere = counted(elsewhere);
        evidence.written = counted(written);

        let mut located: BTreeMap<String, usize> = BTreeMap::new();
        for (lat, lon) in self.positions.get(&dir).cloned().unwrap_or_default() {
            let Some(town) = self.town(lat, lon) else { continue };
            if !self.countries.same(&country, &town.code) && !self.countries.same(&country, &town.country) {
                continue;
            }
            *located.entry(self.spellings.city(&country, &town.name)).or_default() += 1;
        }
        evidence.located = counted(located);

        let name = Placement::parse(&format!("{dir}/-")).event_name.unwrap_or_default();
        let mut named = self.spellings.named_in(&name);
        if let Some(geo) = self.geo
            && !name.is_empty()
        {
            let hint = self.countries.known(&country).map(|(code, _)| code);
            if let Ok(found) = geo.find(&name, hint.as_deref().or(Some(country.as_str()))) {
                for candidate in found.candidates {
                    let inside = hint.as_ref().is_none_or(|code| &candidate.place.country == code);
                    if candidate.how == How::Exact && candidate.confidence >= EXACT && inside {
                        named.push(self.spellings.city(&country, &candidate.place.name));
                    }
                }
            }
        }
        named.dedup();
        evidence.named = named;
        evidence
    }

    /// What an event is offered, best first, and what its question should say besides.
    fn offers(&mut self, at: usize, evidence: &Evidence) -> (Vec<Offer>, Option<String>) {
        let event = &self.events[at];
        let (country, name, total) = (
            event.country.clone(),
            event_name(&event.dir).to_string(),
            event.photos.len(),
        );
        let mut offers: Vec<Offer> = Vec::new();
        let mut offer = |within: &str, city: &str, why: String, located: Option<usize>, sure: bool| {
            let target = format!("{within}/{city}/{name}");
            if event_folder(&target).is_err()
                || offers
                    .iter()
                    .any(|offer| offer.answer == Answer::Folder(target.clone()))
            {
                return;
            }
            offers.push(
                Offer {
                    located,
                    ..Offer::of_answer(Answer::Folder(target.clone()), format!("{target} - {why}"))
                }
                .with_sure(sure),
            );
        };
        for (within, city, count) in &evidence.tagged {
            let why = match (evidence.sure, *count == total) {
                (true, _) => "the places tag of every photo".to_string(),
                (false, true) => format!("the places tag of all {total} photos, with other cities"),
                (false, false) => format!("the places tag of {count} of {total} photos"),
            };
            offer(within, city, why, Some(*count), evidence.sure);
        }
        for (city, count) in &evidence.written {
            offer(
                &country,
                city,
                format!("the location of {count} of {total} photos"),
                Some(*count),
                false,
            );
        }
        for (city, count) in &evidence.located {
            let why = match count {
                1 => "where 1 of its photos is".to_string(),
                count => format!("where {count} of its photos are"),
            };
            offer(&country, city, why, Some(*count), false);
        }
        for city in &evidence.named {
            offer(&country, city, "named in the event's name".to_string(), None, false);
        }

        let mut notes = Vec::new();
        for (other, count) in &evidence.elsewhere {
            notes.push(format!("{count} of {total} photos say {other}"));
        }
        if evidence.several() {
            let cities: Vec<String> = evidence
                .tagged
                .iter()
                .map(|(_, city, count)| format!("{city} {count}"))
                .collect();
            notes.push(format!(
                "Its photos name several cities: {}. It moves as a whole",
                cities.join(", ")
            ));
        }
        if !event.subs.is_empty() {
            let subs: Vec<&str> = event.subs.iter().map(String::as_str).collect();
            notes.push(format!("Its folders {} move with it", subs.join(", ")));
        }
        let note = (!notes.is_empty()).then(|| notes.join(". "));
        (offers, note)
    }

    /// The events of a loose photo's country nearest its date, and a new event on its day.
    fn loose_offers(&self, photo: &Tagged) -> Vec<Offer> {
        let country = photo.country.clone().unwrap_or_default();
        let mut offers = Vec::new();
        let Some(taken) = photo.taken_at.as_deref().and_then(day_of) else {
            return offers;
        };
        let mut near: Vec<(i64, &Folder)> = self
            .folders
            .iter()
            .filter(|folder| folder.country.as_deref() == Some(country.as_str()))
            .filter_map(|folder| Some((days_between(folder, taken)?, folder)))
            .collect();
        near.sort_by(|one, other| one.0.cmp(&other.0).then(one.1.event_dir.cmp(&other.1.event_dir)));
        for (days, folder) in near.into_iter().take(3) {
            let why = match days {
                0 => "on the photo's day".to_string(),
                1 => "1 day from the photo".to_string(),
                days => format!("{days} days from the photo"),
            };
            offers.push(Offer {
                located: Some(folder.photos),
                ..Offer::of_answer(
                    Answer::Folder(folder.event_dir.clone()),
                    format!("{} - {why}, {}", folder.event_dir, photos(folder.photos)),
                )
            });
        }
        let (year, month, day) = taken;
        let new = format!("{country}/{year:04}-{month:02}-{day:02}");
        if event_folder(&new).is_ok() {
            offers.push(Offer::of_answer(
                Answer::Folder(new.clone()),
                format!("{new} - a new event on the photo's day"),
            ));
        }
        offers
    }
}

/// `YYYY-MM-DD` of a date in the one format.
fn day_of(at: &str) -> Option<(i64, i64, i64)> {
    let date = at.get(..10)?;
    let parts: Vec<i64> = date.split('-').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    match parts.as_slice() {
        [year, month, day] => Some((*year, *month, *day)),
        _ => None,
    }
}

/// Roughly how many days lie between a folder's date and a day: a month folder counts from its
/// middle, a year folder from its middle too. `None` for a folder without a year.
fn days_between(folder: &Folder, (year, month, day): (i64, i64, i64)) -> Option<i64> {
    let at = |year: i64, month: i64, day: i64| year * 372 + (month - 1) * 31 + (day - 1);
    let folder_day = at(folder.year?, folder.month.unwrap_or(7), folder.day.unwrap_or(15));
    Some((folder_day - at(year, month, day)).abs())
}

impl Tool for FolderMigration {
    type Settings = Answers;

    fn key(&self) -> &'static str {
        "folder-migration"
    }

    fn title(&self) -> &'static str {
        "Folder Migration"
    }

    fn fixes(&self) -> &'static str {
        "Moves each event into the folder of its city"
    }

    fn answers<'a>(&self, answers: &'a mut Answers) -> Option<&'a mut Answers> {
        Some(answers)
    }

    fn moves(&self) -> bool {
        true
    }

    fn waiting(&self, open: usize) -> String {
        match open {
            1 => "1 folder waits for an answer".to_string(),
            open => format!("{open} folders wait for an answer"),
        }
    }

    fn wording(&self) -> Wording {
        Wording {
            asked: "Events and Loose Photos",
            one: "event",
            many: "events",
            confirm: "Confirm Sure Cities",
            sure_one: "names one city on every photo",
            sure_many: "name one city on every photo",
            unasked: "Every event of the scope is in the folder of its city. Choose another scope on the Tools page.",
        }
    }

    fn questions(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let mut survey = Survey::take(cache, geo, scope).map_err(|error| error.to_string())?;
        let mut questions = Vec::new();
        for at in 0..survey.events.len() {
            let evidence = survey.evidence(at);
            let (offers, note) = survey.offers(at, &evidence);
            let event = &survey.events[at];
            questions.push(Question {
                key: event.dir.clone(),
                title: event.dir.clone(),
                kind: Kind::Folder,
                photos: event.photos.len(),
                offers,
                answer: answers.get(&event.dir).cloned(),
                apart: false,
                note,
                evidence: Vec::new(),
            });
        }
        for photo in &survey.loose {
            questions.push(Question {
                key: photo.rel_path.clone(),
                title: photo.rel_path.clone(),
                kind: Kind::Folder,
                photos: 1,
                offers: survey.loose_offers(photo),
                answer: answers.get(&photo.rel_path).cloned(),
                apart: false,
                note: Some("A photo directly in its country's folder: it goes into an event".to_string()),
                evidence: Vec::new(),
            });
        }
        Ok(questions)
    }

    fn report(
        &self,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
        _answers: &Answers,
    ) -> Result<Vec<Finding>, String> {
        let mut survey = Survey::take(cache, geo, scope).map_err(|error| error.to_string())?;
        let (mut sure, mut some, mut several, mut none) = (0, 0, Vec::new(), 0);
        let mut elsewhere = Vec::new();
        let mut subs = Vec::new();
        for at in 0..survey.events.len() {
            let evidence = survey.evidence(at);
            let event = &survey.events[at];
            match (evidence.sure, evidence.several(), evidence.with_city) {
                (true, _, _) => sure += 1,
                (false, true, _) => {
                    let cities: Vec<String> = evidence
                        .tagged
                        .iter()
                        .map(|(_, city, count)| format!("{city} {count}"))
                        .collect();
                    several.push((event.dir.clone(), cities.join(", ")));
                }
                (false, false, 0) => none += 1,
                (false, false, _) => some += 1,
            }
            for (other, count) in &evidence.elsewhere {
                elsewhere.push((
                    event.dir.clone(),
                    format!("{count} of {} say {other}", photos(event.photos.len())),
                ));
            }
            if !event.subs.is_empty() {
                let names: Vec<&str> = event.subs.iter().map(String::as_str).collect();
                subs.push((event.dir.clone(), names.join(", ")));
            }
        }
        let mut findings = vec![Finding {
            title: "Where the Events Are".to_string(),
            detail: format!(
                "In the folder of their city already: {}. Not yet: {}, of which {} name one city on every photo, {} \
                 a city on some photos, {} several cities and {} none.",
                survey.in_cities,
                survey.events.len(),
                sure,
                some,
                several.len(),
                none
            ),
            rows: Vec::new(),
        }];
        if !several.is_empty() {
            findings.push(Finding {
                title: "Events in Several Cities".to_string(),
                detail: "An event is never split: it goes to one city, or stays where it is.".to_string(),
                rows: several,
            });
        }
        if !elsewhere.is_empty() {
            findings.push(Finding {
                title: "Events Another Country Names".to_string(),
                detail: "Photos whose places tag names another country than their folder.".to_string(),
                rows: elsewhere,
            });
        }
        if !subs.is_empty() {
            findings.push(Finding {
                title: "Events with Folders Inside".to_string(),
                detail: "They move with their event, as they are.".to_string(),
                rows: subs,
            });
        }
        if !survey.loose.is_empty() {
            findings.push(Finding {
                title: "Photos Directly in a Country".to_string(),
                detail: format!("{} to be put into an event.", photos(survey.loose.len())),
                rows: Vec::new(),
            });
        }
        let waiting: Vec<String> = survey.events.iter().map(|event| event.dir.clone()).collect();
        let mut asked_about: Vec<String> = Vec::new();
        for dir in &waiting {
            asked_about.extend(cache.under(dir).map_err(|error| error.to_string())?);
        }
        match super::people::untold(cache, &asked_about)? {
            None => findings.push(Finding {
                title: "Nothing Fetched from Immich".to_string(),
                detail: "Without Immich's people nothing stops an event whose people only Immich knows. Get \
                         People from Immich on the dashboard first."
                    .to_string(),
                rows: Vec::new(),
            }),
            Some(untold) if !untold.is_empty() => {
                let mut by_event: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                for (rel_path, names) in untold {
                    if let Some(dir) = Placement::parse(&rel_path).event_dir {
                        by_event.entry(dir).or_default().extend(names);
                    }
                }
                findings.push(Finding {
                    title: "Events Waiting for Their People".to_string(),
                    detail: "Immich names people in their photos that the files do not. They are not moved until \
                             People from Immich has written them."
                        .to_string(),
                    rows: by_event
                        .into_iter()
                        .map(|(dir, names)| (dir, names.into_iter().collect::<Vec<String>>().join(", ")))
                        .collect(),
                });
            }
            Some(_) => {}
        }
        Ok(findings)
    }

    fn wanted(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> cache::Result<Vec<Wanted>> {
        let paths = scope.paths(cache)?;
        let mut asked: BTreeSet<String> = BTreeSet::new();
        for rel_path in &paths {
            let placement = Placement::parse(rel_path);
            match &placement.event_dir {
                Some(dir) if placement.city.is_none() => {
                    asked.insert(dir.clone());
                }
                None if placement.fit == Fit::LooseInCountry => {
                    asked.insert(rel_path.clone());
                }
                _ => {}
            }
        }

        let mut moves: Vec<(Move, Option<String>)> = Vec::new();
        for key in &asked {
            let Some(Answer::Folder(folder)) = answers.get(key) else {
                continue;
            };
            let event = is_event(key);
            let (to, inside) = match event {
                true => (folder.clone(), cache.under(key)?),
                false => (format!("{folder}/{}", file_name(key)), vec![key.clone()]),
            };
            if to == *key {
                continue;
            }
            let stated = cache.stated(&inside)?;
            let mut photos = Vec::new();
            let mut unread = None;
            for rel_path in &inside {
                match stated.get(rel_path).and_then(|stated| stated.content_id.clone()) {
                    Some(content) => photos.push((rel_path.clone(), content)),
                    None => unread = Some(format!("the scan could not read the image data of {rel_path}")),
                }
            }
            let mut refused = unread;
            if event {
                let date = Placement::parse(&format!("{key}/-")).event_text;
                if Placement::parse(&format!("{to}/-")).event_text != date {
                    refused.get_or_insert("the date in an event's folder name is not changed here".to_string());
                }
            }
            if !cache.under(&to)?.is_empty() || cache.known(&to)?.is_some() {
                refused.get_or_insert(format!("{to} is there already"));
            }
            moves.push((
                Move {
                    from: key.clone(),
                    to,
                    photos,
                },
                refused,
            ));
        }

        let mut targets: HashMap<String, Vec<String>> = HashMap::new();
        for (moved, _) in &moves {
            targets.entry(moved.to.clone()).or_default().push(moved.from.clone());
        }
        let everything: Vec<String> = moves
            .iter()
            .flat_map(|(moved, _)| moved.photos.iter().map(|(rel_path, _)| rel_path.clone()))
            .collect();
        let untold = super::people::untold(cache, &everything).map_err(people_failed)?;

        let mut wanted = Vec::new();
        for (moved, mut refused) in moves {
            if let Some(others) = targets.get(&moved.to).filter(|others| others.len() > 1) {
                let other = others
                    .iter()
                    .find(|other| **other != moved.from)
                    .cloned()
                    .unwrap_or_default();
                refused.get_or_insert(format!("{other} is to go to {} too", moved.to));
            }
            if let Some(untold) = &untold {
                let mut names: BTreeSet<&String> = BTreeSet::new();
                for (rel_path, _) in &moved.photos {
                    names.extend(untold.get(rel_path).into_iter().flatten());
                }
                if !names.is_empty() {
                    let names: Vec<&str> = names.into_iter().map(String::as_str).collect();
                    refused.get_or_insert(format!(
                        "Immich names {} in its photos and the files do not: write the people first",
                        names.join(", ")
                    ));
                }
            }
            wanted.push(Wanted::moving(moved, refused));
        }
        Ok(wanted)
    }
}

fn people_failed(why: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(why))
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
