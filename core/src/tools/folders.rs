//! Folder Migration: every event into its place in the folder layout the user chose - by
//! default `Country/City/YYYY-MM-DD Event`, or `Year/Country/...`, `Topic/...` and the others.
//!
//! One question per event of the scope that is not in the layout yet, or that lacks one of its
//! optional levels, answered with the folder it belongs in. Each level is filled from what the
//! photos already say: the country from the folders, the places tags or the positions; the city
//! from the places tag of every photo, the cities some of them name, the location text, where its
//! positions are, and a city named in the event's name; the region from the city; the year and
//! month from the event's date; a tag level from the tags below its root. A city is spelled the
//! way the library already spells it - its city folder, else its places tag - so folders and tags
//! agree. A photo in a folder but in no event is asked about too, offered the events nearest to
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
use crate::layout::{Component, Fit, Layout, Placement, plain_name};
use crate::scope::Scope;
use crate::tags;
use crate::write::Move;

pub struct FolderMigration;

const PLACES: &str = "places";

/// A folder an event or a photo can be answered with: plain folder names, then the event folder
/// `YYYY-MM-DD Name`. Whether it is in the layout is asked when it is to be moved, so an answer
/// outlives a change of the layout. Returned as it should be kept.
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
    let placement = Placement::of_folder(path, &Layout::default());
    match placement.event_dir.as_deref() == Some(path) {
        true => Ok(path.to_string()),
        false => Err(format!(
            "{path} does not end in an event folder, YYYY-MM-DD and its name"
        )),
    }
}

/// Whether a folder is where the layout wants an event.
fn in_layout(path: &str, layout: &Layout) -> bool {
    Placement::of_folder(path, layout).fits()
}

/// A folder from its parts, as the layout puts them: the levels named by hand, the year and the
/// month taken from the date. An optional level may be empty.
pub fn assembled(layout: &Layout, named: &[(Component, String)], date: &str, name: &str) -> Result<String, String> {
    let (date, name) = (date.trim(), name.trim());
    if name.contains('/') {
        return Err("a part of a folder cannot contain /".to_string());
    }
    let event = match name.is_empty() {
        true => date.to_string(),
        false => format!("{date} {name}"),
    };
    let bad_date = || format!("{date} is not a date as YYYY-MM-DD, with zeros for what is not known");
    event_folder(&event).map_err(|_| bad_date())?;
    let mut folders = Vec::new();
    for level in &layout.levels {
        let text = match &level.component {
            Component::Year => date.get(..4).unwrap_or_default().to_string(),
            Component::Month => date.get(..7).unwrap_or_default().to_string(),
            component => named
                .iter()
                .find(|(named, _)| named == component)
                .map(|(_, text)| text.trim().to_string())
                .unwrap_or_default(),
        };
        if text.contains('/') {
            return Err("a part of a folder cannot contain /".to_string());
        }
        match (text.is_empty(), level.optional) {
            (true, true) => {}
            (true, false) => return Err(format!("a folder needs its {}", level.component.title().to_lowercase())),
            (false, _) => folders.push(text),
        }
    }
    folders.push(event);
    let path = folders.join("/");
    event_folder(&path)?;
    match in_layout(&path, layout) {
        true => Ok(path),
        false => Err(format!("{path} is not {}", layout.title())),
    }
}

/// The parts a question's answer is typed in, filled with its answer, else its best offer, else
/// where it is now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parts {
    /// The levels typed by hand - every level of the layout but the year and the month, which
    /// come from the date - each with its text.
    pub named: Vec<(Component, String)>,
    pub date: String,
    pub name: String,
    /// An event keeps the date in its folder's name; a loose photo's new event is given one.
    pub date_fixed: bool,
}

impl Parts {
    pub fn of(question: &Question, layout: &Layout) -> Parts {
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
        let placement = match &folder {
            Some(folder) => Placement::of_folder(folder, layout),
            None => Placement::parse(&question.key, layout),
        };
        let named = layout
            .levels
            .iter()
            .filter(|level| !matches!(level.component, Component::Year | Component::Month))
            .map(|level| {
                let text = placement
                    .levels
                    .iter()
                    .find(|(component, _)| *component == level.component)
                    .map(|(_, text)| text.clone())
                    .unwrap_or_default();
                (level.component.clone(), text)
            })
            .collect();
        Parts {
            named,
            date: placement.event_text.unwrap_or_else(|| "0000-00-00".to_string()),
            name: placement.event_name.unwrap_or_default(),
            date_fixed: is_event,
        }
    }

    /// The text typed for a level, empty when it has none.
    pub fn text(&self, component: &Component) -> &str {
        self.named
            .iter()
            .find(|(named, _)| named == component)
            .map(|(_, text)| text.as_str())
            .unwrap_or_default()
    }

    pub fn set(&mut self, component: &Component, text: &str) {
        if let Some((_, kept)) = self.named.iter_mut().find(|(named, _)| named == component) {
            *kept = text.to_string();
        }
    }

    /// What the question's folder or photo would be called with these parts.
    pub fn after(&self, question: &Question, layout: &Layout) -> Result<String, String> {
        let folder = assembled(layout, &self.named, &self.date, &self.name)?;
        Ok(match is_event(&question.key) {
            true => folder,
            false => format!("{folder}/{}", file_name(&question.key)),
        })
    }
}

/// Whether a question is about an event folder rather than a loose photo.
fn is_event(key: &str) -> bool {
    Placement::of_folder(key, &Layout::default()).event_dir.as_deref() == Some(key)
}

fn file_name(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

/// The folder an event is in, its name inside that folder: `2019-07-13 Sommerfest`.
fn event_name(dir: &str) -> &str {
    dir.rsplit('/').next().unwrap_or(dir)
}

/// The folder a photo is in.
fn parent(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or_default()
}

/// Whether the layout asks about an event: it is not in it, or an optional level is missing
/// that could be filled.
fn asks(placement: &Placement, layout: &Layout) -> bool {
    placement.event_dir.is_some() && !placement.complete(layout)
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

/// An event not in the layout yet, and every photo in it.
struct Event {
    dir: String,
    /// The country it is in: by its folders, else by its places tags; empty when neither says.
    country: String,
    /// Its folders say the country, rather than its tags.
    country_sure: bool,
    photos: Vec<Tagged>,
    /// The folders inside it, which move with it.
    subs: BTreeSet<String>,
}

/// One way to fill an event's place levels, before the tag, the year and the month are added.
struct Choice {
    country: String,
    city: Option<String>,
    why: String,
    located: Option<usize>,
    sure: bool,
}

/// Countries by the codes the place data gives them, and by their spelling where it gives none.
struct Countries<'a> {
    geo: Option<&'a Geo>,
    folders: Vec<String>,
    codes: HashMap<String, Option<(String, String)>>,
}

impl<'a> Countries<'a> {
    fn new(geo: Option<&'a Geo>, folders: &[Folder]) -> Countries<'a> {
        let geo = geo.filter(|geo| geo.is_filled());
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

    /// A folder name that is a country: the place data knows it, or, without place data, the
    /// library has it as one. With place data the library's own country folders are not taken
    /// on trust, as they are read with a layout that may have them in the wrong place.
    fn is_country(&mut self, text: &str) -> bool {
        match self.geo {
            Some(_) => self.known(text).is_some(),
            None => self.folders.iter().any(|folder| folder == text),
        }
    }

    /// A name the place data knows as a city, exactly, and not as a country.
    fn is_city(&mut self, text: &str) -> bool {
        let Some(geo) = self.geo else { return false };
        if self.known(text).is_some() {
            return false;
        }
        geo.find(text, None).is_ok_and(|found| {
            found
                .candidates
                .iter()
                .any(|candidate| candidate.how == How::Exact && candidate.confidence >= EXACT)
        })
    }

    fn same(&mut self, one: &str, other: &str) -> bool {
        if one.is_empty() || other.is_empty() {
            return false;
        }
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
    /// A city a folder above the event already names, from a layout it was in before.
    foldered: Option<String>,
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
    layout: Layout,
    countries: Countries<'a>,
    spellings: Spellings,
    folders: Vec<Folder>,
    events: Vec<Event>,
    /// Events of the scope already where the layout wants them.
    settled: usize,
    loose: Vec<Tagged>,
    positions: HashMap<String, Vec<(f64, f64)>>,
    towns: HashMap<(u64, u64), Option<Located>>,
}

impl<'a> Survey<'a> {
    fn take(cache: &Cache, geo: Option<&'a Geo>, scope: &Scope) -> cache::Result<Survey<'a>> {
        let layout = cache.layout().clone();
        let paths = scope.paths(cache)?;
        let folders = cache.event_folders()?;
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let mut settled: BTreeSet<String> = BTreeSet::new();
        // Events in the layout by their folders' shape, and whose folders agree with the photos.
        let mut loose_paths = Vec::new();
        for rel_path in &paths {
            let placement = Placement::parse(rel_path, &layout);
            match &placement.event_dir {
                Some(dir) if asks(&placement, &layout) => {
                    dirs.insert(dir.clone());
                }
                Some(dir) => {
                    settled.insert(dir.clone());
                }
                None if placement.fit == Fit::LooseInFolder => loose_paths.push(rel_path.clone()),
                None => {}
            }
        }
        let mut countries = Countries::new(geo, &folders);
        let checked = layout
            .levels
            .iter()
            .any(|level| matches!(level.component, Component::Tag(_) | Component::Country));
        if checked {
            for dir in settled.clone() {
                let photos = cache.tagged(&cache.under(&dir)?)?;
                if disagrees(&mut countries, &Placement::of_folder(&dir, &layout), &photos) {
                    settled.remove(&dir);
                    dirs.insert(dir);
                }
            }
        }
        let mut events = Vec::new();
        for dir in &dirs {
            let inside = cache.under(dir)?;
            let placement = Placement::of_folder(dir, &layout);
            let subs = inside
                .iter()
                .filter_map(|rel_path| Placement::parse(rel_path, &layout).sub_path)
                .filter_map(|sub| sub.split('/').next().map(String::from))
                .collect();
            let photos = cache.tagged(&inside)?;
            let by_folder = placement
                .country
                .clone()
                .filter(|country| country_folder(&mut countries, country, &photos))
                .or_else(|| {
                    placement
                        .above
                        .iter()
                        .find(|folder| countries.is_country(folder) || tags_name(&photos, folder))
                        .cloned()
                });
            let (country, country_sure) = match by_folder {
                Some(country) => (country, true),
                None => match tagged_country(&photos, &mut countries) {
                    Some(country) => (country, false),
                    None => {
                        let first = placement
                            .above
                            .iter()
                            .find(|folder| plain_name(folder) && country_folder(&mut countries, folder, &photos));
                        (first.cloned().unwrap_or_default(), false)
                    }
                },
            };
            events.push(Event {
                dir: dir.clone(),
                country,
                country_sure,
                photos,
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
            layout,
            countries,
            spellings: Spellings::new(cache, &folders)?,
            folders,
            events,
            settled: settled.len(),
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
                    false if country.is_empty() => self.countries.folder(&tag.country),
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
            if !country.is_empty()
                && !self.countries.same(&country, &town.code)
                && !self.countries.same(&country, &town.country)
            {
                continue;
            }
            *located.entry(self.spellings.city(&country, &town.name)).or_default() += 1;
        }
        evidence.located = counted(located);

        let name = Placement::of_folder(&dir, &self.layout).event_name.unwrap_or_default();
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
        evidence.foldered = self.foldered_city(at);
        evidence
    }

    /// A folder above the event that is not its country, but a city of it the library or the
    /// place data knows.
    fn foldered_city(&mut self, at: usize) -> Option<String> {
        let (country, dir) = (self.events[at].country.clone(), self.events[at].dir.clone());
        let placement = Placement::of_folder(&dir, &self.layout);
        let hint = self.countries.known(&country).map(|(code, _)| code);
        for folder in placement.above.iter().rev() {
            if *folder == country || self.countries.is_country(folder) {
                continue;
            }
            let known = self
                .spellings
                .folders
                .iter()
                .any(|(within, city)| *within == country && city == folder);
            let found = || {
                self.geo
                    .and_then(|geo| geo.find(folder, hint.as_deref()).ok())
                    .is_some_and(|found| {
                        found.candidates.iter().any(|candidate| {
                            candidate.how == How::Exact
                                && candidate.confidence >= EXACT
                                && hint.as_ref().is_none_or(|code| &candidate.place.country == code)
                        })
                    })
            };
            if known || found() {
                return Some(folder.clone());
            }
        }
        None
    }

    /// The region a city or the event's positions lie in, as the place data names it.
    fn region(&mut self, at: usize, country: &str, city: Option<&str>) -> Option<String> {
        let geo = self.geo?;
        if let Some(city) = city {
            let hint = self.countries.known(country).map(|(code, _)| code);
            let found = geo.find(city, hint.as_deref().or(Some(country))).ok()?;
            return found
                .candidates
                .into_iter()
                .find(|candidate| {
                    candidate.how == How::Exact && hint.as_ref().is_none_or(|code| &candidate.place.country == code)
                })
                .and_then(|candidate| candidate.place.area);
        }
        let dir = self.events[at].dir.clone();
        let mut regions: BTreeMap<String, usize> = BTreeMap::new();
        for (lat, lon) in self.positions.get(&dir).cloned().unwrap_or_default() {
            if let Some(region) = self.town(lat, lon).and_then(|town| town.region) {
                *regions.entry(region).or_default() += 1;
            }
        }
        counted(regions).into_iter().next().map(|(region, _)| region)
    }

    /// The ways to fill the place levels: the cities the photos name, then the event's own
    /// country, then the other countries its tags name.
    fn choices(&self, at: usize, evidence: &Evidence) -> Vec<Choice> {
        let event = &self.events[at];
        let (country, total) = (event.country.clone(), event.photos.len());
        let mut choices = Vec::new();
        if self.layout.has(&Component::City) || self.layout.has(&Component::Region) {
            if let Some(city) = &evidence.foldered {
                choices.push(Choice {
                    country: country.clone(),
                    city: Some(city.clone()),
                    why: "the city folder it is in".to_string(),
                    located: None,
                    sure: evidence.tagged.is_empty() || (evidence.sure && evidence.tagged[0].1 == *city),
                });
            }
            for (within, city, count) in &evidence.tagged {
                let why = match (evidence.sure, *count == total) {
                    (true, _) => "the places tag of every photo".to_string(),
                    (false, true) => format!("the places tag of all {total} photos, with other cities"),
                    (false, false) => format!("the places tag of {count} of {total} photos"),
                };
                choices.push(Choice {
                    country: within.clone(),
                    city: Some(city.clone()),
                    why,
                    located: Some(*count),
                    sure: evidence.sure,
                });
            }
            let mut add = |city: &str, why: String, located: Option<usize>| {
                choices.push(Choice {
                    country: country.clone(),
                    city: Some(city.to_string()),
                    why,
                    located,
                    sure: false,
                })
            };
            for (city, count) in &evidence.written {
                add(city, format!("the location of {count} of {total} photos"), Some(*count));
            }
            for (city, count) in &evidence.located {
                let why = match count {
                    1 => "where 1 of its photos is".to_string(),
                    count => format!("where {count} of its photos are"),
                };
                add(city, why, Some(*count));
            }
            for city in &evidence.named {
                add(city, "named in the event's name".to_string(), None);
            }
        }
        let why = match event.country_sure {
            true => "the country of its folder",
            false => "the country its places tags name",
        };
        choices.push(Choice {
            country: country.clone(),
            city: None,
            why: why.to_string(),
            located: None,
            sure: event.country_sure || !self.layout.has(&Component::Country),
        });
        if self.layout.has(&Component::Country) {
            for (other, count) in &evidence.elsewhere {
                if choices.iter().any(|choice| &choice.country == other) {
                    continue;
                }
                choices.push(Choice {
                    country: other.clone(),
                    city: None,
                    why: format!("the places tag of {count} of {total} photos"),
                    located: Some(*count),
                    sure: false,
                });
            }
        }
        choices
    }

    /// The tags right below a root the event's photos carry, most photos first, and whether
    /// every photo carries the same one and no other.
    fn tags_under(&self, at: usize, root: &str) -> (Vec<(String, usize)>, bool) {
        let photos = &self.events[at].photos;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut each: Vec<BTreeSet<String>> = Vec::new();
        for photo in photos {
            let below: BTreeSet<String> = photo.tags.iter().filter_map(|tag| under_root(tag, root)).collect();
            for tag in &below {
                *counts.entry(tag.clone()).or_default() += 1;
            }
            each.push(below);
        }
        let sure = each
            .first()
            .is_some_and(|first| first.len() == 1 && each.iter().all(|one| one == first));
        (counted(counts), sure)
    }

    /// What an event is offered, best first, and what its question should say besides.
    fn offers(&mut self, at: usize, evidence: &Evidence) -> (Vec<Offer>, Option<String>) {
        let layout = self.layout.clone();
        let choices = self.choices(at, evidence);
        let tag_root = layout.levels.iter().find_map(|level| match &level.component {
            Component::Tag(root) => Some(root.clone()),
            _ => None,
        });
        let (tagged, tags_sure) = match &tag_root {
            Some(root) => self.tags_under(at, root),
            None => (Vec::new(), true),
        };
        let total = self.events[at].photos.len();
        let mut tag_options: Vec<(Option<String>, String, bool, Option<usize>)> = tagged
            .iter()
            .map(|(tag, count)| {
                let root = tag_root.as_deref().unwrap_or_default();
                let why = match (tags_sure, *count == total) {
                    (true, _) => format!("the {root} tag of every photo"),
                    (false, true) => format!("the {root} tag of all {total} photos, with others"),
                    (false, false) => format!("the {root} tag of {count} of {total} photos"),
                };
                (Some(tag.clone()), why, tags_sure, Some(*count))
            })
            .collect();
        if tag_options.is_empty() {
            let optional = layout
                .levels
                .iter()
                .any(|level| matches!(level.component, Component::Tag(_)) && level.optional);
            tag_options.push((None, String::new(), tag_root.is_none() || optional, None));
        }
        let (dir, date) = {
            let event = &self.events[at];
            let placement = Placement::of_folder(&event.dir, &layout);
            (event.dir.clone(), placement.event_text.unwrap_or_default())
        };
        let name = event_name(&dir).to_string();
        let dated = !((layout.has(&Component::Year) || layout.has(&Component::Month)) && date.starts_with("0000"))
            && !(layout.has(&Component::Month) && date.get(5..7) == Some("00"));

        let mut offers: Vec<Offer> = Vec::new();
        for choice in &choices {
            let region = match layout.has(&Component::Region) {
                true => self.region(at, &choice.country, choice.city.as_deref()),
                false => None,
            };
            for (tag, tag_why, tag_sure, tag_count) in &tag_options {
                let mut folders = Vec::new();
                let mut complete = true;
                for level in &layout.levels {
                    let value = match &level.component {
                        Component::Country => Some(choice.country.clone()).filter(|country| !country.is_empty()),
                        Component::Region => region.clone(),
                        Component::City => choice.city.clone(),
                        Component::Year => date.get(..4).map(String::from),
                        Component::Month => date.get(..7).map(String::from),
                        Component::Tag(_) => tag.clone(),
                    };
                    match value {
                        Some(value) => folders.push(value),
                        None if level.optional => {}
                        None => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete {
                    continue;
                }
                folders.push(name.clone());
                let target = folders.join("/");
                if target == dir
                    || event_folder(&target).is_err()
                    || !in_layout(&target, &layout)
                    || offers
                        .iter()
                        .any(|offer| offer.answer == Answer::Folder(target.clone()))
                {
                    continue;
                }
                let mut why: Vec<&str> = Vec::new();
                if choice.city.is_some() || tag.is_none() {
                    why.push(&choice.why);
                }
                if !tag_why.is_empty() {
                    why.push(tag_why);
                }
                if !dated {
                    why.push("its date has no year or month");
                }
                offers.push(
                    Offer {
                        located: choice.located.or(*tag_count),
                        ..Offer::of_answer(Answer::Folder(target.clone()), format!("{target} - {}", why.join(", ")))
                    }
                    .with_sure(choice.sure && *tag_sure && dated),
                );
            }
        }

        let event = &self.events[at];
        let mut notes = Vec::new();
        for (other, count) in &evidence.elsewhere {
            notes.push(format!("{count} of {total} photos say {other}"));
        }
        if evidence.several() && layout.has(&Component::City) {
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

    /// The events in the folder a loose photo lies in nearest its date, and a new event on its day.
    fn loose_offers(&self, photo: &Tagged) -> Vec<Offer> {
        let within = parent(&photo.rel_path).to_string();
        let mut offers = Vec::new();
        let Some(taken) = photo.taken_at.as_deref().and_then(day_of) else {
            return offers;
        };
        let mut near: Vec<(i64, &Folder)> = self
            .folders
            .iter()
            .filter(|folder| folder.event_dir.starts_with(&format!("{within}/")))
            .filter(|folder| in_layout(&folder.event_dir, &self.layout))
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
        let new = format!("{within}/{year:04}-{month:02}-{day:02}");
        if event_folder(&new).is_ok() && in_layout(&new, &self.layout) {
            offers.push(Offer::of_answer(
                Answer::Folder(new.clone()),
                format!("{new} - a new event on the photo's day"),
            ));
        }
        offers
    }
}

/// Whether the folders of an event in the layout say what its photos do not: a tag level no
/// photo carries, a country level that is no country and no places tag names. The path alone
/// cannot tell two plain names apart, so this is what finds `Germany/Hamburg` read as a city
/// `Germany` in a country `Hamburg`.
fn disagrees(countries: &mut Countries, placement: &Placement, photos: &[Tagged]) -> bool {
    placement.levels.iter().any(|(component, folder)| match component {
        Component::Tag(root) => !photos.iter().any(|photo| {
            photo
                .tags
                .iter()
                .any(|tag| under_root(tag, root).is_some_and(|below| below == *folder))
        }),
        Component::Country => !country_folder(countries, folder, photos),
        _ => false,
    })
}

/// Whether the photos' places tags name this country.
fn tags_name(photos: &[Tagged], country: &str) -> bool {
    photos.iter().any(|photo| {
        photo
            .tags
            .iter()
            .filter_map(|tag| place_tag(tag))
            .any(|tag| fold(&tag.country) == fold(country))
    })
}

/// Whether a folder read as the country can be taken as one: the place data or the places tags
/// know it as a country, or at least not as a city. A library's own name for a country the place
/// data spells otherwise stays its country; `Hamburg` does not.
fn country_folder(countries: &mut Countries, folder: &str, photos: &[Tagged]) -> bool {
    countries.is_country(folder) || tags_name(photos, folder) || !countries.is_city(folder)
}

/// How many events a layout would find out of place, of how many: off it by their folders, or
/// in it by shape with a tag or country folder their photos do not bear out. Asked before a
/// layout is kept, so it reads nothing but the cache and the place data.
pub fn events_off(cache: &Cache, geo: Option<&Geo>, layout: &Layout) -> cache::Result<(usize, usize)> {
    let folders = cache.event_folders()?;
    let mut countries = Countries::new(geo, &folders);
    let checked = layout
        .levels
        .iter()
        .any(|level| matches!(level.component, Component::Tag(_) | Component::Country));
    let mut off = 0;
    for folder in &folders {
        let placement = Placement::of_folder(&folder.event_dir, layout);
        let fits = placement.fits()
            && !(checked
                && disagrees(
                    &mut countries,
                    &placement,
                    &cache.tagged(&cache.under(&folder.event_dir)?)?,
                ));
        if !fits {
            off += 1;
        }
    }
    Ok((off, folders.len()))
}

/// The tag right below a root: `topics/Sailing/Regatta` under `topics` is `Sailing`.
fn under_root(tag: &str, root: &str) -> Option<String> {
    let (top, rest) = tag.split_once('/')?;
    top.eq_ignore_ascii_case(root)
        .then(|| rest.split('/').next().unwrap_or(rest).to_string())
}

/// The country most of the photos' places tags name, spelled as its folder would be.
fn tagged_country(photos: &[Tagged], countries: &mut Countries) -> Option<String> {
    let mut named: BTreeMap<String, usize> = BTreeMap::new();
    for photo in photos {
        let said: BTreeSet<String> = tags::deepest(&photo.tags)
            .iter()
            .filter_map(|tag| place_tag(tag))
            .map(|tag| tag.country)
            .collect();
        for country in said {
            *named.entry(country).or_default() += 1;
        }
    }
    let (country, _) = counted(named).into_iter().next()?;
    Some(countries.folder(&country))
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
        "Moves each event into its place in the folder layout"
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
            confirm: "Confirm Sure Folders",
            sure_one: "has one sure folder",
            sure_many: "have one sure folder",
            unasked: "Every event of the scope is in the folder layout. Choose another scope on the Tools page.",
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
                note: Some("A photo in a folder but in no event: it goes into an event".to_string()),
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
        let mut settled_on = 0;
        let mut elsewhere = Vec::new();
        let mut subs = Vec::new();
        for at in 0..survey.events.len() {
            let evidence = survey.evidence(at);
            let (offers, _) = survey.offers(at, &evidence);
            if offers.first().is_some_and(|offer| offer.sure) {
                settled_on += 1;
            }
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
        let cities = match survey.layout.has(&Component::City) {
            true => format!(
                " - one city on every photo: {sure}, a city on some photos: {some}, several cities: {}, none: {none}",
                several.len()
            ),
            false => String::new(),
        };
        let mut findings = vec![Finding {
            title: "Where the Events Are".to_string(),
            detail: format!(
                "In the layout {} already: {}. Not yet: {}{cities}. Sure where they go: {settled_on}.",
                survey.layout.title(),
                survey.settled,
                survey.events.len(),
            ),
            rows: Vec::new(),
        }];
        if !several.is_empty() && survey.layout.has(&Component::City) {
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
                title: "Photos in No Event".to_string(),
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
                    if let Some(dir) = Placement::parse(&rel_path, &survey.layout).event_dir {
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

    fn wanted(&self, cache: &Cache, geo: Option<&Geo>, scope: &Scope, answers: &Answers) -> cache::Result<Vec<Wanted>> {
        let layout = cache.layout();
        let survey = Survey::take(cache, geo, scope)?;
        let asked: BTreeSet<String> = survey
            .events
            .iter()
            .map(|event| event.dir.clone())
            .chain(survey.loose.iter().map(|photo| photo.rel_path.clone()))
            .collect();

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
                let date = Placement::of_folder(key, layout).event_text;
                if Placement::of_folder(&to, layout).event_text != date {
                    refused.get_or_insert("the date in an event's folder name is not changed here".to_string());
                }
            }
            if !in_layout(folder, layout) {
                refused.get_or_insert(format!("{folder} is not {}", layout.title()));
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
