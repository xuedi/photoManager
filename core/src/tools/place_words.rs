//! The place in words, where the position says it and the photo does not: the town, the state
//! and the country a position is in, from the offline reverse lookup, for photos with a position
//! and no place word at all. And the finer place a places tag names below the town - a district,
//! a venue - kept in `Sublocation`, spelled as the person confirms it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::Located;
use super::gps_from_places::deepest;
use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::filter::Filter;
use crate::geo::reverse::kilometres;
use crate::geo::{Geo, fold};
use crate::roles::Roles;
use crate::scope::Scope;
use crate::write::{Change, Field, Place};

/// A country whose photos stand somewhere and say nothing of where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    pub code: String,
    pub name: String,
    pub photos: usize,
    /// The towns they are in, the most photos first.
    pub towns: Vec<String>,
}

/// The place words of a position, as the tools write them with a derived one. `None` where the
/// place data knows no town around it.
pub fn words_at(geo: &Geo, lat: f64, lon: f64) -> Option<Located> {
    geo.at(lat, lon).ok().as_ref().and_then(Located::near)
}

/// The photos of the scope with a position and no place word at all, each with the place its
/// position is in. A position with no town near is left out.
fn unworded(cache: &Cache, geo: &Geo, scope: &Scope) -> cache::Result<Vec<(String, Located)>> {
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    let worded = cache.with_place_text(&paths)?;
    let mut near: HashMap<(i64, i64), Option<Located>> = HashMap::new();
    let mut found = Vec::new();
    for rel_path in paths {
        if worded.contains(&rel_path) {
            continue;
        }
        let Some(said) = stated.get(&rel_path).map(|one| &one.said) else {
            continue;
        };
        let (Some(lat), Some(lon)) = (said.gps_lat, said.gps_lon) else {
            continue;
        };
        // A few metres apart is one place: the lookup is asked once per spot.
        let spot = ((lat * 10_000.0).round() as i64, (lon * 10_000.0).round() as i64);
        let place = near.entry(spot).or_insert_with(|| words_at(geo, lat, lon)).clone();
        if let Some(place) = place {
            found.push((rel_path, place));
        }
    }
    Ok(found)
}

/// The countries with photos that stand somewhere and say nothing of where, the most photos
/// first. Nothing without place data.
pub fn sure(cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Country>, String> {
    let Some(geo) = geo else {
        return Ok(Vec::new());
    };
    let whole = Scope::Filter(Filter::all());
    let found = unworded(cache, geo, &whole).map_err(|error| error.to_string())?;
    let mut countries: BTreeMap<String, (String, BTreeMap<String, usize>, usize)> = BTreeMap::new();
    for (_, place) in found {
        let country = countries
            .entry(place.code.clone())
            .or_insert_with(|| (place.country.clone(), BTreeMap::new(), 0));
        *country.1.entry(place.name.clone()).or_default() += 1;
        country.2 += 1;
    }
    let mut sure: Vec<Country> = countries
        .into_iter()
        .map(|(code, (name, towns, photos))| {
            let mut towns: Vec<(String, usize)> = towns.into_iter().collect();
            towns.sort_by(|one, other| other.1.cmp(&one.1).then(one.0.cmp(&other.0)));
            Country {
                code,
                name,
                photos,
                towns: towns.into_iter().map(|(town, _)| town).collect(),
            }
        })
        .collect();
    sure.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.name.cmp(&other.name)));
    Ok(sure)
}

/// The photos of the scope in these countries, by code, that stand somewhere and say nothing of
/// where, given the words of their position.
pub fn wanted(
    cache: &Cache,
    geo: Option<&Geo>,
    countries: &BTreeSet<String>,
    scope: &Scope,
) -> cache::Result<Vec<Wanted>> {
    let Some(geo) = geo else {
        return Ok(Vec::new());
    };
    Ok(unworded(cache, geo, scope)?
        .into_iter()
        .filter(|(_, place)| countries.contains(&place.code))
        .map(|(rel_path, place)| Wanted::new(rel_path, Change::of([Field::Place(Some(place.place()))])))
        .collect())
}

/// A places tag whose located photos stand in a town of another name: a place finer than the
/// town, or no place at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finer {
    pub tag: String,
    /// The towns its located photos stand in, the most photos first.
    pub towns: Vec<String>,
    /// Every photo of the tag, located or not.
    pub photos: usize,
}

/// Whether a tag is a place below a country's places tag: `places/inGermany/Harbourside`, or with
/// no country level, any place below the root.
pub fn is_below_a_country(tag: &str, roles: &Roles) -> bool {
    roles.place_of(tag).is_some()
}

/// The last level of a places tag, the way a person would spell it to begin with.
pub fn leaf(tag: &str) -> &str {
    tag.rsplit('/').next().unwrap_or(tag).trim()
}

/// A places tag's photos: how many, the towns the located ones stand in, and where one stands.
#[derive(Default)]
struct Tagged {
    photos: usize,
    towns: BTreeMap<String, usize>,
    at: Option<(f64, f64)>,
}

/// Every places tag below a country whose located photos stand in a town not of its name,
/// the most photos first. Nothing without place data.
pub fn finer(cache: &Cache, geo: Option<&Geo>) -> Result<Vec<Finer>, String> {
    let Some(geo) = geo else {
        return Ok(Vec::new());
    };
    let failed = |error: rusqlite::Error| error.to_string();
    let paths = Scope::Filter(Filter::all()).paths(cache).map_err(failed)?;
    let stated = cache.stated(&paths).map_err(failed)?;
    let mut tags: BTreeMap<String, Tagged> = BTreeMap::new();
    let mut near: HashMap<(i64, i64), Option<Located>> = HashMap::new();
    let roles = cache.roles();
    for one in stated.values() {
        let located = one.said.gps_lat.zip(one.said.gps_lon);
        for tag in deepest(&one.said.tags, &roles) {
            if !is_below_a_country(&tag, &roles) {
                continue;
            }
            let entry = tags.entry(tag).or_default();
            entry.photos += 1;
            if let Some((lat, lon)) = located {
                entry.at.get_or_insert((lat, lon));
                let spot = ((lat * 10_000.0).round() as i64, (lon * 10_000.0).round() as i64);
                if let Some(town) = near.entry(spot).or_insert_with(|| words_at(geo, lat, lon)) {
                    *entry.towns.entry(town.name.clone()).or_default() += 1;
                }
            }
        }
    }
    let mut finer: Vec<Finer> = tags
        .into_iter()
        .filter_map(|(tag, found)| Some((tag, found.photos, found.towns, found.at?)))
        .filter(|(tag, _, towns, at)| is_finer(geo, &roles, tag, towns, *at))
        .map(|(tag, photos, towns, _)| {
            let mut towns: Vec<(String, usize)> = towns.into_iter().collect();
            towns.sort_by(|one, other| other.1.cmp(&one.1).then(one.0.cmp(&other.0)));
            Finer {
                tag,
                towns: towns.into_iter().map(|(town, _)| town).collect(),
                photos,
            }
        })
        .collect();
    finer.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.tag.cmp(&other.tag)));
    Ok(finer)
}

/// Whether a tag names something finer than the towns its photos stand in. Not when it names one
/// of them, but for case and accents, or another name of the same place the place data is sure
/// of, such as a name in another language; and not when that place is a town somewhere else, as that is a tag and
/// a position that disagree ([`crate::checks`]). `at` is where one of its photos stands.
fn is_finer(geo: &Geo, roles: &Roles, tag: &str, towns: &BTreeMap<String, usize>, at: (f64, f64)) -> bool {
    let named = |name: &str| towns.keys().any(|town| fold(town) == fold(name));
    if named(leaf(tag)) {
        return false;
    }
    let sure = super::gps_from_places::offers(geo, tag, roles)
        .ok()
        .and_then(|offers| offers.into_iter().next())
        .filter(|offer| offer.sure)
        .and_then(|offer| offer.place().cloned());
    match sure {
        Some(place) => !named(&place.name) && kilometres(at.0, at.1, place.lat, place.lon) <= crate::checks::TOWN_KM,
        None => true,
    }
}

/// Every photo of the scope that carries the tag, or a tag below it, gets the name as its
/// `Sublocation`; every other place word it has is carried along as it is. With `untag`, the tag
/// goes in the same write.
pub fn sublocation(cache: &Cache, scope: &Scope, tag: &str, name: &str, untag: bool) -> cache::Result<Vec<Wanted>> {
    let name = name.trim();
    let tagged: BTreeSet<String> = Filter::of(crate::filter::Kind::Tagged(vec![tag.to_string()]))
        .paths(cache)?
        .into_iter()
        .collect();
    let paths: Vec<String> = scope
        .paths(cache)?
        .into_iter()
        .filter(|rel_path| tagged.contains(rel_path))
        .collect();
    let words = cache.place_words(&paths)?;
    let stated = cache.stated(&paths)?;
    Ok(paths
        .into_iter()
        .map(|rel_path| {
            let now = words.get(&rel_path).cloned().unwrap_or_default();
            let place = Place {
                location: Some(name.to_string()),
                ..now
            };
            let mut fields = vec![Field::Place(Some(place))];
            if untag && let Some(one) = stated.get(&rel_path) {
                fields.extend([
                    Field::Tags(crate::tags::without(&one.said.tags, tag)),
                    Field::DropLabel,
                    Field::DropCatalogSets,
                ]);
            }
            Wanted::new(rel_path, Change::of(fields))
        })
        .collect())
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::{ChangeSet, Verdict};
    use crate::tools::testing::{Library, geo};

    const FINER: &str = "places/inGermany/Harbourside";
    const FINER_PHOTO: &str = "Germany/2013-05-18 Garden Party/P1060001.JPG";

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    #[test]
    fn a_position_without_words_gets_its_town_and_one_with_words_is_left() {
        let mut library = Library::new("place-words");
        let geo = geo();
        let countries = sure(&library.cache, Some(&geo)).unwrap();
        let germany = countries.iter().find(|country| country.code == "DE").unwrap();
        assert_eq!(germany.name, "Germany");
        assert_eq!(germany.towns, ["Hamburg"]);
        assert!(sure(&library.cache, None).unwrap().is_empty(), "no place data, no fix");

        let codes = BTreeSet::from(["DE".to_string()]);
        let wanted = wanted(&library.cache, Some(&geo), &codes, &whole()).unwrap();
        assert_eq!(wanted.len(), germany.photos);
        let paths: Vec<&str> = wanted.iter().map(|one| one.rel_path.as_str()).collect();
        assert!(paths.contains(&"Germany/2016-06-00 Harbour Walk/DSC_0101.JPG"));
        let Field::Place(Some(place)) = &wanted[0].change.fields[0] else {
            panic!("{:?}", wanted[0].change);
        };
        assert_eq!(place.city.as_deref(), Some("Hamburg"));
        assert_eq!(place.country.as_deref(), Some("Germany"));
        assert_eq!(place.country_code.as_deref(), Some("DE"));
        assert!(place.state.is_some());

        let set = ChangeSet::build(&library.cache, "Write the place from the position", &wanted).unwrap();
        assert_eq!(library.apply(&set).written, wanted.len());
        library.rescan();
        assert!(
            !sure(&library.cache, Some(&geo))
                .unwrap()
                .iter()
                .any(|country| country.code == "DE"),
            "a second run finds nothing"
        );
        assert!(words_at(&geo, -40.0, -140.0).is_none(), "nothing near in the ocean");
    }

    #[test]
    fn a_tag_finer_than_its_town_asks_and_keeps_the_other_words() {
        let mut library = Library::new("sublocation");
        let geo = geo();
        let finer = finer(&library.cache, Some(&geo)).unwrap();
        let tags: Vec<&str> = finer.iter().map(|one| one.tag.as_str()).collect();
        assert!(tags.contains(&FINER), "{tags:?}");
        assert!(
            !tags.contains(&"places/inGermany/Hamburg"),
            "a tag that is its town asks nothing: {tags:?}"
        );
        assert_eq!(finer.iter().find(|one| one.tag == FINER).unwrap().towns, ["Hamburg"]);
        let towns = BTreeMap::from([("Lörrach".to_string(), 1)]);
        assert!(
            !is_finer(
                &geo,
                &library.cache.roles(),
                "places/inGermany/Lorrach",
                &towns,
                (47.6, 7.66)
            ),
            "a town spelled without its accents is its town"
        );
        assert!(
            !tags.contains(&"places/inGermany/Bremen"),
            "a town elsewhere is no finer place but a disagreement: {tags:?}"
        );

        let words = BTreeSet::from(["DE".to_string()]);
        let placed = wanted(&library.cache, Some(&geo), &words, &whole()).unwrap();
        library.apply(&ChangeSet::build(&library.cache, "", &placed).unwrap());
        library.rescan();

        let set = ChangeSet::build(
            &library.cache,
            "",
            &sublocation(&library.cache, &whole(), FINER, " Harbour Side ", false).unwrap(),
        )
        .unwrap();
        let rows: Vec<(&str, &Verdict)> = set
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), &row.verdict))
            .collect();
        assert_eq!(rows, [(FINER_PHOTO, &Verdict::Change)]);
        assert!(
            set.rows[0].tells().contains("Harbour Side, Hamburg"),
            "{}",
            set.rows[0].tells()
        );
        assert_eq!(library.apply(&set).written, 1);
        library.rescan();
        let again = ChangeSet::build(
            &library.cache,
            "",
            &sublocation(&library.cache, &whole(), FINER, "Harbour Side", false).unwrap(),
        )
        .unwrap();
        assert_eq!(again.counts().change, 0, "a second run has nothing to do");
        let now = library.cache.place_words(&[FINER_PHOTO.to_string()]).unwrap()[FINER_PHOTO].clone();
        assert_eq!(now.location.as_deref(), Some("Harbour Side"));
        assert_eq!(now.city.as_deref(), Some("Hamburg"), "the city is kept");
        assert_eq!(now.country_code.as_deref(), Some("DE"));
    }
}
