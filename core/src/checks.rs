//! The place check: what the places tags say against the positions, and what is left to do for
//! the photos without one. Run after every scan, with the place data when there is any, and kept
//! in the cache beside the photos, so the dashboard counts and the gallery lists it with the one
//! predicate a filter is. Nothing here writes a photo.
//!
//! A photo whose places tag names a town more than a town's width from where it stands disagrees,
//! and so does one whose country-only tag names another country than the one it stands in: what
//! must be empty before the places tags can go. Each photo without a position is in exactly one
//! part of what is left: a sure fix waits for it, its places tag names a town the person has to
//! answer, its event decides, or there is nothing to go on.

use std::collections::{HashMap, HashSet};

use crate::cache::Cache;
use crate::filter::Filter;
use crate::geo::Geo;
use crate::geo::reverse::kilometres;
use crate::roles::{Role, Roles};
use crate::scope::Scope;
use crate::tools::gps_from_event::GpsFromEvent;
use crate::tools::gps_from_places::{self, GpsFromPlacesTag, deepest};
use crate::tools::{Answers, Tool};

/// How far a position may be from the town its tag names and still agree: a town's width.
pub const TOWN_KM: f64 = 25.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    /// The places tag names a town far from the position, or another country.
    PlaceDisagrees,
    /// No position, and a sure fix of the suggestions gives it one.
    GpsSure,
    /// No position, and a places tag names a town the person has to answer.
    GpsAsks,
    /// No position, no town to go by, and an event folder to decide by.
    GpsEvent,
    /// No position and nothing to go on.
    GpsNothing,
    /// A tag of a role whose tags may go, which its field does not prove.
    Kept(Role),
}

impl Check {
    pub const ALL: [Check; 9] = [
        Check::PlaceDisagrees,
        Check::GpsSure,
        Check::GpsAsks,
        Check::GpsEvent,
        Check::GpsNothing,
        Check::Kept(Role::People),
        Check::Kept(Role::Places),
        Check::Kept(Role::Year),
        Check::Kept(Role::Events),
    ];
    /// The tags of a role that stay, one finding per role.
    pub const KEPT: [Check; 4] = [
        Check::Kept(Role::People),
        Check::Kept(Role::Places),
        Check::Kept(Role::Year),
        Check::Kept(Role::Events),
    ];
    /// What is left to do for the photos without a position, in the order it is worked down.
    pub const GPS: [Check; 4] = [Check::GpsSure, Check::GpsAsks, Check::GpsEvent, Check::GpsNothing];

    pub fn key(self) -> &'static str {
        match self {
            Check::PlaceDisagrees => "place-disagrees",
            Check::GpsSure => "gps-sure",
            Check::GpsAsks => "gps-asks",
            Check::GpsEvent => "gps-event",
            Check::GpsNothing => "gps-nothing",
            Check::Kept(Role::People) => "kept-people",
            Check::Kept(Role::Places) => "kept-places",
            Check::Kept(Role::Year) => "kept-year",
            Check::Kept(Role::Events) => "kept-events",
        }
    }

    pub fn named(key: &str) -> Option<Check> {
        Check::ALL.into_iter().find(|check| check.key() == key)
    }

    pub fn title(self) -> &'static str {
        match self {
            Check::PlaceDisagrees => "Places tag and position disagree",
            Check::GpsSure => "A sure fix waits",
            Check::GpsAsks => "A tag the person answers",
            Check::GpsEvent => "The event decides",
            Check::GpsNothing => "Nothing to go on",
            Check::Kept(Role::People) => "People tags no field says yet",
            Check::Kept(Role::Places) => "Places tags no field says yet",
            Check::Kept(Role::Year) => "Year tags no field says yet",
            Check::Kept(Role::Events) => "Event tags no field says yet",
        }
    }

    /// What the photos of the finding are, as part of a title.
    pub fn phrase(self) -> &'static str {
        match self {
            Check::PlaceDisagrees => "whose places tag and position disagree",
            Check::GpsSure => "without GPS that a sure fix places",
            Check::GpsAsks => "without GPS whose places tag names a town to answer",
            Check::GpsEvent => "without GPS whose event decides",
            Check::GpsNothing => "without GPS and nothing to go on",
            Check::Kept(Role::People) => "whose people tag no person they name says",
            Check::Kept(Role::Places) => "whose places tag no position or place word says",
            Check::Kept(Role::Year) => "whose year tag their date does not say",
            Check::Kept(Role::Events) => "whose event tag their event field does not say",
        }
    }

    /// What it means, in one line.
    pub fn detail(self) -> &'static str {
        match self {
            Check::PlaceDisagrees => {
                "a places tag names a town more than 25 km from where the photo stands, or another country"
            }
            Check::GpsSure => "Places from Tags or Places from Events in Suggestions places them",
            Check::GpsAsks => "their places tag names a town the place data is not sure of",
            Check::GpsEvent => "no town tag; the event they are in is placed with Set Place",
            Check::GpsNothing => "no town tag and no event",
            Check::Kept(Role::People) => "Redundant Tags keeps them: the photo does not name the person",
            Check::Kept(Role::Places) => "Redundant Tags keeps them: no position, or it is elsewhere",
            Check::Kept(Role::Year) => "Redundant Tags keeps them: no date, or another year",
            Check::Kept(Role::Events) => "Redundant Tags keeps them: no event field, or another event",
        }
    }
}

/// What the check found, by finding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    pub counts: Vec<(Check, usize)>,
}

/// Checks the library and keeps what it found in the cache, instead of what the last check did.
pub fn run(cache: &mut Cache, geo: Option<&Geo>) -> Result<Checked, String> {
    let started = std::time::Instant::now();
    let mut found = find(cache, geo)?;
    for (rel_path, role, why) in crate::redundant::kept(cache, geo).map_err(|error| error.to_string())? {
        found.push((rel_path, Check::Kept(role), Some(why.to_string())));
    }
    let rows: Vec<(String, &str, Option<String>)> = found
        .iter()
        .map(|(rel_path, check, detail)| (rel_path.clone(), check.key(), detail.clone()))
        .collect();
    cache.note_checked(&rows).map_err(|error| error.to_string())?;
    let counts: Vec<(Check, usize)> = Check::ALL
        .into_iter()
        .map(|check| (check, found.iter().filter(|(_, each, _)| *each == check).count()))
        .collect();
    tracing::info!(
        disagree = counts[0].1,
        seconds = started.elapsed().as_secs_f64(),
        "places checked"
    );
    Ok(Checked { counts })
}

/// Every finding, by photo, with what about it.
pub fn find(cache: &Cache, geo: Option<&Geo>) -> Result<Vec<(String, Check, Option<String>)>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let whole = Scope::Filter(Filter::all());
    let paths = whole.paths(cache).map_err(failed)?;
    let stated = cache.stated(&paths).map_err(failed)?;
    let events = cache.event_dirs(&paths).map_err(failed)?;
    let sure: HashSet<String> = match geo {
        Some(geo) => {
            let mut sure = sure_paths(&GpsFromPlacesTag, cache, geo)?;
            sure.extend(sure_paths(&GpsFromEvent, cache, geo)?);
            sure
        }
        None => HashSet::new(),
    };
    let roles = cache.roles();
    let mut towns = Towns::default();

    let mut found = Vec::new();
    for rel_path in paths {
        let Some(said) = stated.get(&rel_path).map(|one| &one.said) else {
            continue;
        };
        let tags = deepest(&said.tags, &roles);
        match said.gps_lat.zip(said.gps_lon) {
            Some((lat, lon)) => {
                let Some(geo) = geo else { continue };
                if let Some(why) = towns.disagreement(geo, &roles, &tags, lat, lon)? {
                    found.push((rel_path, Check::PlaceDisagrees, Some(why)));
                }
            }
            None => {
                let check = if sure.contains(&rel_path) {
                    Check::GpsSure
                } else if tags.iter().any(|tag| !roles.names_a_country(tag)) {
                    Check::GpsAsks
                } else if events.contains_key(&rel_path) {
                    Check::GpsEvent
                } else {
                    Check::GpsNothing
                };
                found.push((rel_path, check, None));
            }
        }
    }
    Ok(found)
}

/// The photos the sure answers of a tool would give a position.
fn sure_paths<T: Tool<Settings = Answers>>(tool: &T, cache: &Cache, geo: &Geo) -> Result<HashSet<String>, String> {
    let whole = Scope::Filter(Filter::all());
    let mut answers = Answers::default();
    for question in tool
        .questions(cache, Some(geo), &whole, &Answers::default())?
        .iter()
        .filter(|question| question.confirmable())
    {
        answers.set(&question.key, question.sure().map(|offer| offer.answer.clone()));
    }
    Ok(tool
        .wanted(cache, Some(geo), &whole, &answers)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|wanted| wanted.refused.is_none())
        .map(|wanted| wanted.rel_path)
        .collect())
}

/// What the tags and the positions mean, each looked up once.
#[derive(Default)]
struct Towns {
    /// By tag: where the town it names is, when the place data is sure of it.
    towns: HashMap<String, Option<(f64, f64)>>,
    /// By tag: the code of the country a country-only tag names.
    countries: HashMap<String, Option<String>>,
    /// By spot: the code of the country it lies in.
    lying: HashMap<(i64, i64), Option<String>>,
}

impl Towns {
    /// Why a photo's places tags disagree with where it stands, if one does.
    fn disagreement(
        &mut self,
        geo: &Geo,
        roles: &Roles,
        tags: &[String],
        lat: f64,
        lon: f64,
    ) -> Result<Option<String>, String> {
        for tag in tags {
            if roles.names_a_country(tag) {
                let named = match self.countries.get(tag) {
                    Some(code) => code.clone(),
                    None => {
                        let code = geo
                            .country(&roles.country_of(tag).unwrap_or_default())
                            .map_err(|error| error.to_string())?
                            .map(|(code, _)| code);
                        self.countries.insert(tag.clone(), code.clone());
                        code
                    }
                };
                let spot = ((lat * 1000.0).round() as i64, (lon * 1000.0).round() as i64);
                let lies = self
                    .lying
                    .entry(spot)
                    .or_insert_with(|| geo.at(lat, lon).ok().and_then(|at| at.country).map(|(code, _)| code))
                    .clone();
                if let (Some(named), Some(lies)) = (named, lies)
                    && !named.eq_ignore_ascii_case(&lies)
                {
                    return Ok(Some(format!("{tag} names another country than {lies}")));
                }
                continue;
            }
            let town = match self.towns.get(tag) {
                Some(town) => *town,
                None => {
                    let town = gps_from_places::offers(geo, tag, roles)?
                        .first()
                        .filter(|offer| offer.sure)
                        .and_then(|offer| offer.place())
                        .map(|place| (place.lat, place.lon));
                    self.towns.insert(tag.clone(), town);
                    town
                }
            };
            if let Some((town_lat, town_lon)) = town {
                let km = kilometres(lat, lon, town_lat, town_lon);
                if km > TOWN_KM {
                    return Ok(Some(format!("{tag} is {km:.0} km away")));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::filter::{Gap, Kind};
    use crate::tools::testing::{Library, geo};

    fn listed(cache: &Cache, check: Check) -> Vec<String> {
        Filter::of(Kind::Checked(check)).paths(cache).unwrap()
    }

    #[test]
    fn a_town_far_away_disagrees_and_one_near_does_not() {
        let mut library = Library::new("checks-disagree");
        let checked = run(&mut library.cache, Some(&geo())).unwrap();
        assert_eq!(
            listed(&library.cache, Check::PlaceDisagrees),
            ["Germany/2013-05-18 Garden Party/P1060002.JPG"],
            "Bremen is a hundred km from the Hamburg it stands in; the Hamburg tags on and near \
             the centre agree, and a tag the place data does not know says nothing"
        );
        assert_eq!(checked.counts[0], (Check::PlaceDisagrees, 1));
        let found = find(&library.cache, Some(&geo())).unwrap();
        let (_, _, why) = found
            .iter()
            .find(|(_, check, _)| *check == Check::PlaceDisagrees)
            .unwrap();
        assert!(why.as_deref().is_some_and(|why| why.contains("Bremen")), "{why:?}");
    }

    #[test]
    fn what_is_left_for_gps_adds_up_to_the_gap() {
        let mut library = Library::new("checks-split");
        run(&mut library.cache, Some(&geo())).unwrap();
        let gap = Filter::missing(Gap::Gps).count(&library.cache).unwrap();
        let parts: Vec<(Check, usize)> = Check::GPS
            .iter()
            .map(|check| (*check, listed(&library.cache, *check).len()))
            .collect();
        assert_eq!(
            parts.iter().map(|(_, count)| *count as i64).sum::<i64>(),
            gap,
            "{parts:?}"
        );
        assert!(
            parts.iter().all(|(_, count)| *count > 0),
            "each part has photos: {parts:?}"
        );
        assert!(
            listed(&library.cache, Check::GpsSure).contains(&"China/2006-09-00 Besuch Ben/P1000001.JPG".to_string())
        );
        assert!(
            listed(&library.cache, Check::GpsAsks).contains(&"Greece/0000-00-00 Aeron ilands/IMG_0005.JPG".to_string()),
            "a typo of a town"
        );

        run(&mut library.cache, None).unwrap();
        assert!(
            listed(&library.cache, Check::GpsSure).is_empty(),
            "no place data, nothing is sure"
        );
        assert!(listed(&library.cache, Check::PlaceDisagrees).is_empty());
        let without: i64 = Check::GPS
            .iter()
            .map(|check| listed(&library.cache, *check).len() as i64)
            .sum();
        assert_eq!(without, gap, "without place data it still adds up");
    }
}
