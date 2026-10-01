//! Redundant tags: a tag of a role whose own field says what it says already - a year tag beside
//! the date, an event tag beside the `Event` field, a people tag beside the person the photo
//! names, a places tag beside the position and the place words. Such a tag can go; one the field
//! does not prove stays, with why.
//!
//! The proof is made for each deepest tag of a role on each photo, as the cache reads it. A tag
//! with levels below it is decided by them: a branch goes when every tag below it went. A tag of no
//! role is never touched, and neither is one of a role kept as tags.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::checks::TOWN_KM;
use crate::geo::reverse::kilometres;
use crate::geo::{Geo, fold};
use crate::roles::{Role, Roles};
use crate::scope::Scope;
use crate::tags;
use crate::tools::gps_from_places;
use crate::write::{Change, Field, Place};

/// What a photo says, as far as the proof needs it.
#[derive(Debug, Clone, Default)]
pub struct Photo {
    pub rel_path: String,
    pub tags: Vec<String>,
    pub taken_at: Option<String>,
    /// The year its event folder states.
    pub event_year: Option<i64>,
    pub event: Option<String>,
    /// Every person it names, with a box or without.
    pub persons: Vec<String>,
    pub gps: Option<(f64, f64)>,
    pub words: Place,
}

/// One tag of a role on one photo, and whether its field says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub tag: String,
    pub role: Role,
    /// `None` when proven, else why it stays.
    pub kept: Option<&'static str>,
}

pub const NO_DATE: &str = "no date";
pub const ANOTHER_YEAR: &str = "another year";
pub const NOT_A_YEAR: &str = "not a year";
pub const NO_EVENT: &str = "no event field";
pub const ANOTHER_EVENT: &str = "another event";
pub const NOT_NAMED: &str = "the photo does not name the person";
pub const NO_POSITION: &str = "no position";
pub const FAR: &str = "far from where the photo was taken";
pub const ANOTHER_COUNTRY: &str = "another country";
pub const UNKNOWN: &str = "the place data is not sure of it";
pub const NO_PLACE_DATA: &str = "no place data";

/// Makes the proof, looking each place and position up once.
pub struct Judge<'a> {
    roles: Roles,
    geo: Option<&'a Geo>,
    towns: HashMap<String, Option<(f64, f64)>>,
    countries: HashMap<String, Option<String>>,
    lying: HashMap<(i64, i64), Option<String>>,
}

impl<'a> Judge<'a> {
    pub fn new(roles: Roles, geo: Option<&'a Geo>) -> Judge<'a> {
        Judge {
            roles,
            geo: geo.filter(|geo| geo.is_filled()),
            towns: HashMap::new(),
            countries: HashMap::new(),
            lying: HashMap::new(),
        }
    }

    pub fn roles(&self) -> &Roles {
        &self.roles
    }

    /// Every deepest tag of a role of the photo, proven or kept.
    pub fn judge(&mut self, photo: &Photo) -> Vec<Judged> {
        let mut judged = Vec::new();
        for tag in tags::deepest(&photo.tags) {
            let Some(role) = self.roles.of(&tag) else { continue };
            let kept = match role {
                Role::Year => self.year(&tag, photo),
                Role::Events => self.event(&tag, photo),
                Role::People => self.person(&tag, photo),
                Role::Places => self.place(&tag, photo),
            };
            judged.push(Judged { tag, role, kept });
        }
        judged
    }

    fn year(&self, tag: &str, photo: &Photo) -> Option<&'static str> {
        let Some(year) = self.roles.year_of(tag) else {
            return Some(NOT_A_YEAR);
        };
        match taken_year(photo) {
            None => Some(NO_DATE),
            Some(taken) if taken != year => Some(ANOTHER_YEAR),
            Some(_) => None,
        }
    }

    fn event(&self, tag: &str, photo: &Photo) -> Option<&'static str> {
        let (year, name) = self.roles.event_of(tag)?;
        let Some(event) = &photo.event else {
            return Some(NO_EVENT);
        };
        if fold(event) != fold(name) {
            return Some(ANOTHER_EVENT);
        }
        match year {
            Some(year) if taken_year(photo) != Some(year) && photo.event_year != Some(year) => Some(ANOTHER_YEAR),
            _ => None,
        }
    }

    fn person(&self, tag: &str, photo: &Photo) -> Option<&'static str> {
        let name = fold(tag.rsplit('/').next().unwrap_or(tag));
        match photo.persons.iter().any(|person| fold(person) == name) {
            true => None,
            false => Some(NOT_NAMED),
        }
    }

    fn place(&mut self, tag: &str, photo: &Photo) -> Option<&'static str> {
        let leaf = fold(tag.rsplit('/').next().unwrap_or(tag));
        let said = |word: &Option<String>| word.as_deref().is_some_and(|word| fold(word) == leaf);
        if self.roles.place_of(tag).is_some() && said(&photo.words.location) {
            return None;
        }
        let Some(geo) = self.geo else {
            return Some(NO_PLACE_DATA);
        };
        let Some((lat, lon)) = photo.gps else {
            return Some(NO_POSITION);
        };
        if self.roles.names_a_country(tag) {
            let named = self.country_named(geo, tag);
            let lies = self.country_at(geo, lat, lon);
            return match (named, lies) {
                (Some(named), Some(lies)) if named.eq_ignore_ascii_case(&lies) => None,
                (Some(_), Some(_)) => Some(ANOTHER_COUNTRY),
                _ => Some(UNKNOWN),
            };
        }
        if self.roles.place_of(tag).is_none() {
            return Some(UNKNOWN);
        }
        if said(&photo.words.city) {
            return None;
        }
        match self.town(geo, tag) {
            Some((town_lat, town_lon)) if kilometres(lat, lon, town_lat, town_lon) <= TOWN_KM => None,
            Some(_) => Some(FAR),
            None => Some(UNKNOWN),
        }
    }

    fn town(&mut self, geo: &Geo, tag: &str) -> Option<(f64, f64)> {
        let roles = &self.roles;
        *self.towns.entry(tag.to_string()).or_insert_with(|| {
            gps_from_places::offers(geo, tag, roles)
                .ok()?
                .into_iter()
                .next()
                .filter(|offer| offer.sure)
                .and_then(|offer| offer.place().map(|place| (place.lat, place.lon)))
        })
    }

    fn country_named(&mut self, geo: &Geo, tag: &str) -> Option<String> {
        let roles = &self.roles;
        self.countries
            .entry(tag.to_string())
            .or_insert_with(|| {
                let named = roles.country_of(tag)?;
                geo.country(&named).ok().flatten().map(|(code, _)| code)
            })
            .clone()
    }

    fn country_at(&mut self, geo: &Geo, lat: f64, lon: f64) -> Option<String> {
        let spot = ((lat * 1000.0).round() as i64, (lon * 1000.0).round() as i64);
        self.lying
            .entry(spot)
            .or_insert_with(|| geo.at(lat, lon).ok().and_then(|at| at.country).map(|(code, _)| code))
            .clone()
    }
}

fn taken_year(photo: &Photo) -> Option<i64> {
    photo.taken_at.as_deref()?.get(..4)?.parse().ok()
}

/// The photos of these paths, as the proof needs them.
pub fn photos(cache: &Cache, paths: &[String]) -> cache::Result<Vec<Photo>> {
    let stated = cache.stated(paths)?;
    let mut words = cache.place_words(paths)?;
    let years = event_years(cache)?;
    Ok(paths
        .iter()
        .filter_map(|rel_path| {
            let said = &stated.get(rel_path)?.said;
            Some(Photo {
                rel_path: rel_path.clone(),
                tags: said.tags.clone(),
                taken_at: said.taken_at.clone(),
                event_year: years.get(rel_path).copied(),
                event: said.event.clone(),
                persons: said
                    .regions
                    .as_ref()
                    .map(|regions| regions.named().into_iter().map(|(name, _)| name).collect())
                    .unwrap_or_default(),
                gps: said.gps_lat.zip(said.gps_lon),
                words: words.remove(rel_path).unwrap_or_default(),
            })
        })
        .collect())
}

fn event_years(cache: &Cache) -> cache::Result<HashMap<String, i64>> {
    let mut statement = cache
        .connection()
        .prepare("SELECT rel_path, event_year FROM photo WHERE event_year > 0")?;
    statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
}

/// The roles whose tags may go: a root, and not kept as tags.
pub fn droppable(roles: &Roles) -> Vec<Role> {
    Role::ALL
        .into_iter()
        .filter(|role| roles.root(*role).is_some() && !roles.keeps(*role))
        .collect()
}

/// What one role's tags come to over the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub role: Role,
    /// Photos with a tag of the role that can go.
    pub photos: usize,
    /// Tags that can go, over all photos.
    pub tags: usize,
    /// Why the others stay, with how many photos keep a tag for that reason, the most first.
    pub kept: Vec<(&'static str, usize)>,
}

/// One role's tags as they are counted: the photos with a tag that can go, how many tags, and the
/// photos that keep one, by why.
#[derive(Default)]
struct Tally {
    proven: BTreeSet<String>,
    tags: usize,
    kept: BTreeMap<&'static str, BTreeSet<String>>,
}

/// Every droppable role with a tag that can go, in the order of the roles.
pub fn sure(cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Found>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let roles = cache.roles();
    let droppable = droppable(&roles);
    if droppable.is_empty() {
        return Ok(Vec::new());
    }
    let paths = Scope::Filter(crate::filter::Filter::all())
        .paths(cache)
        .map_err(failed)?;
    let mut judge = Judge::new(roles, geo);
    let mut found: BTreeMap<Role, Tally> = BTreeMap::new();
    for photo in photos(cache, &paths).map_err(failed)? {
        for judged in judge.judge(&photo) {
            if !droppable.contains(&judged.role) {
                continue;
            }
            let tally = found.entry(judged.role).or_default();
            match judged.kept {
                None => {
                    tally.proven.insert(photo.rel_path.clone());
                    tally.tags += 1;
                }
                Some(why) => {
                    tally.kept.entry(why).or_default().insert(photo.rel_path.clone());
                }
            }
        }
    }
    Ok(found
        .into_iter()
        .filter(|(_, tally)| !tally.proven.is_empty())
        .map(|(role, tally)| {
            let mut kept: Vec<(&'static str, usize)> = tally
                .kept
                .into_iter()
                .map(|(why, photos)| (why, photos.len()))
                .collect();
            kept.sort_by(|one, other| other.1.cmp(&one.1).then(one.0.cmp(other.0)));
            Found {
                role,
                photos: tally.proven.len(),
                tags: tally.tags,
                kept,
            }
        })
        .collect())
}

/// Each photo of the scope written without the tags of these roles its fields prove, in every
/// tag field. A photo with nothing proven is left out.
pub fn wanted(cache: &Cache, geo: Option<&Geo>, chosen: &BTreeSet<Role>, scope: &Scope) -> cache::Result<Vec<Wanted>> {
    let roles = cache.roles();
    let droppable = droppable(&roles);
    let mut judge = Judge::new(roles, geo);
    let mut wanted = Vec::new();
    for photo in photos(cache, &scope.paths(cache)?)? {
        let gone: BTreeSet<String> = judge
            .judge(&photo)
            .into_iter()
            .filter(|judged| judged.kept.is_none() && chosen.contains(&judged.role) && droppable.contains(&judged.role))
            .map(|judged| judged.tag)
            .collect();
        if gone.is_empty() {
            continue;
        }
        let then: Vec<String> = tags::deepest(&photo.tags)
            .into_iter()
            .filter(|tag| !gone.contains(tag))
            .collect();
        wanted.push(Wanted::new(
            photo.rel_path,
            Change::of([Field::Tags(then), Field::DropLabel, Field::DropCatalogSets]),
        ));
    }
    Ok(wanted)
}

/// The photos that keep a tag of a droppable role its field does not prove, by role, each with
/// why: what the dashboard lists.
pub fn kept(cache: &Cache, geo: Option<&Geo>) -> cache::Result<Vec<(String, Role, &'static str)>> {
    let roles = cache.roles();
    let droppable = droppable(&roles);
    if droppable.is_empty() {
        return Ok(Vec::new());
    }
    let paths = Scope::Filter(crate::filter::Filter::all()).paths(cache)?;
    let mut judge = Judge::new(roles, geo);
    let mut kept = Vec::new();
    for photo in photos(cache, &paths)? {
        let mut seen = BTreeSet::new();
        for judged in judge.judge(&photo) {
            if let Some(why) = judged.kept
                && droppable.contains(&judged.role)
                && seen.insert(judged.role)
            {
                kept.push((photo.rel_path.clone(), judged.role, why));
            }
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles() -> Roles {
        Roles::proposed(
            &[
                ("people".to_string(), 1),
                ("places".to_string(), 1),
                ("timeline".to_string(), 1),
                ("events".to_string(), 1),
            ],
            &["inGermany".to_string()],
        )
    }

    fn photo(tags: &[&str]) -> Photo {
        Photo {
            rel_path: "Germany/2019-07-13 Sommerfest/a.jpg".to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..Photo::default()
        }
    }

    fn verdicts(photo: &Photo) -> Vec<(String, Option<&'static str>)> {
        Judge::new(roles(), None)
            .judge(photo)
            .into_iter()
            .map(|judged| (judged.tag, judged.kept))
            .collect()
    }

    #[test]
    fn a_year_tag_is_proven_by_the_date_only() {
        let tagged = |at: Option<&str>, tag: &str| Photo {
            taken_at: at.map(String::from),
            ..photo(&[tag])
        };
        assert_eq!(
            verdicts(&tagged(Some("2019-07-13 18:20:00"), "timeline/2019"))[0].1,
            None
        );
        assert_eq!(verdicts(&tagged(None, "timeline/2019"))[0].1, Some(NO_DATE));
        assert_eq!(
            verdicts(&tagged(Some("2018-07-13 18:20:00"), "timeline/2019"))[0].1,
            Some(ANOTHER_YEAR)
        );
        assert_eq!(
            verdicts(&tagged(Some("2019-07-13 18:20:00"), "timeline/old"))[0].1,
            Some(NOT_A_YEAR)
        );
    }

    #[test]
    fn an_event_tag_is_proven_by_the_event_field() {
        let tagged = |event: Option<&str>| Photo {
            taken_at: Some("2019-07-13 18:20:00".to_string()),
            event: event.map(String::from),
            ..photo(&["events/2019 Sommerfest"])
        };
        assert_eq!(verdicts(&tagged(Some("Sommerfest")))[0].1, None);
        assert_eq!(verdicts(&tagged(Some("sommerfest")))[0].1, None, "but for case");
        assert_eq!(verdicts(&tagged(None))[0].1, Some(NO_EVENT));
        assert_eq!(verdicts(&tagged(Some("Wedding")))[0].1, Some(ANOTHER_EVENT));
        let older = Photo {
            taken_at: Some("2017-01-01 10:00:00".to_string()),
            ..tagged(Some("Sommerfest"))
        };
        assert_eq!(verdicts(&older)[0].1, Some(ANOTHER_YEAR));
        let folder_says = Photo {
            event_year: Some(2019),
            ..older
        };
        assert_eq!(verdicts(&folder_says)[0].1, None, "the folder's year is the event's");
    }

    #[test]
    fn a_people_tag_is_proven_by_a_person_the_photo_names() {
        let named = Photo {
            persons: vec!["Anna".to_string()],
            ..photo(&["people/family/anna", "people/groupGermany/Travolta", "mixed/food"])
        };
        assert_eq!(
            verdicts(&named),
            [
                ("people/family/anna".to_string(), None),
                ("people/groupGermany/Travolta".to_string(), Some(NOT_NAMED)),
            ],
            "a tag of no role is not judged"
        );
    }

    #[test]
    fn a_place_tag_without_place_data_is_proven_only_by_its_sublocation() {
        let finer = Photo {
            words: Place {
                location: Some("Harbourside".to_string()),
                ..Place::default()
            },
            gps: Some((53.55, 9.99)),
            ..photo(&["places/inGermany/Harbourside", "places/inGermany/Hamburg/Altona"])
        };
        assert_eq!(
            verdicts(&finer),
            [
                ("places/inGermany/Hamburg/Altona".to_string(), Some(NO_PLACE_DATA)),
                ("places/inGermany/Harbourside".to_string(), None),
            ]
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod fixture_tests {
    use super::*;
    use crate::changeset::ChangeSet;
    use crate::filter::{Filter, Kind};
    use crate::tools::testing::{Library, geo};

    const UNDATED: &str = "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG";
    const DATED: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn found(library: &Library, role: Role) -> Option<Found> {
        sure(&library.cache, Some(&geo()))
            .unwrap()
            .into_iter()
            .find(|found| found.role == role)
    }

    #[test]
    fn places_are_proven_by_the_position_and_kept_with_why() {
        let library = Library::new("redundant-places");
        let geo = geo();
        let mut judge = Judge::new(library.cache.roles(), Some(&geo));
        let all = photos(&library.cache, &whole().paths(&library.cache).unwrap()).unwrap();
        let of = |rel_path: &str, judge: &mut Judge| {
            let photo = all.iter().find(|photo| photo.rel_path == rel_path).unwrap();
            judge
                .judge(photo)
                .into_iter()
                .filter(|judged| judged.role == Role::Places)
                .collect::<Vec<_>>()
        };
        let located = of("Germany/2019-07-13 Sommerfest/img_0657.jpg", &mut judge);
        assert!(located.iter().all(|judged| judged.kept.is_none()), "{located:?}");
        let far = of("Germany/2013-05-18 Garden Party/P1060002.JPG", &mut judge);
        assert!(far.iter().any(|judged| judged.kept == Some(FAR)), "{far:?}");
        let unplaced = of(DATED, &mut judge);
        assert!(
            unplaced.iter().all(|judged| judged.kept == Some(NO_POSITION)),
            "{unplaced:?}"
        );
    }

    #[test]
    fn the_year_role_drops_exactly_the_proven_tags_and_a_second_run_finds_nothing() {
        let mut library = Library::new("redundant-year");
        let year = found(&library, Role::Year).expect("year tags to drop");
        assert_eq!(year.kept, [(NO_DATE, 1)]);
        assert!(found(&library, Role::Events).is_none(), "the events are kept as tags");

        let wanted = wanted(&library.cache, Some(&geo()), &BTreeSet::from([Role::Year]), &whole()).unwrap();
        assert_eq!(wanted.len(), year.photos);
        assert!(
            !wanted.iter().any(|one| one.rel_path == UNDATED),
            "no date, the tag stays"
        );
        let set = ChangeSet::build(&library.cache, "drop", &wanted).unwrap();
        let summary = library.apply(&set);
        assert_eq!(summary.written, year.photos, "{summary:?}");
        library.rescan();

        let tags = |rel_path: &str| {
            library.cache.stated(&[rel_path.to_string()]).unwrap()[rel_path]
                .said
                .tags
                .clone()
        };
        assert!(
            !tags(DATED).iter().any(|tag| tag.starts_with("timeline")),
            "{:?}",
            tags(DATED)
        );
        assert!(
            tags(DATED).contains(&"places/inChina/Beijing".to_string()),
            "the other tags stay"
        );
        assert!(tags(UNDATED).contains(&"timeline/2008".to_string()));
        assert!(found(&library, Role::Year).is_none(), "a second run finds nothing");

        crate::checks::run(&mut library.cache, Some(&geo())).unwrap();
        let kept = Filter::of(Kind::Checked(crate::checks::Check::Kept(Role::Year)));
        assert_eq!(kept.paths(&library.cache).unwrap(), [UNDATED]);
        assert_eq!(kept.count(&library.cache).unwrap(), 1, "the number is its filter");
    }
}
