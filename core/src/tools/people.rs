//! People from Immich: who Immich found in each photo, written into the photo itself under the
//! name Immich gives them, with a face box on each face. A person is sure when Immich has no one
//! else of that name.
//!
//! What the file says about people is merged, never replaced: a box on a face Immich found too
//! takes Immich's name and box, every other box stays as it is, and a person named without a box
//! stays named. Tags are not touched: a person is not a tag. What Immich knows comes from the
//! snapshot beside the cache, fetched on a button; this never talks to Immich itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use unicode_normalization::UnicodeNormalization;

use crate::browse;
use crate::cache::{self, Cache};
use crate::changeset::{self, Wanted};
use crate::filter::Filter;
use crate::immich::{self, Asset, Face, Person, Snapshot, boxes};
use crate::metadata::Regions;
use crate::scope::Scope;
use crate::tags;
use crate::write::{self, Change, Faces, Field};

/// The root every person's tag is under, in either spelling.
const PEOPLE: &str = "people";
/// How far the shape of the picture Immich measured on may be from the file's.
const SHAPE: f64 = 0.02;
/// How much of the smaller of two boxes the other must cover for both to be on one face.
const SAME_FACE: f64 = 0.5;

/// What the snapshot says, arranged for asking about photos.
struct Known {
    people: HashMap<String, Person>,
    /// By path inside the library.
    assets: HashMap<String, Asset>,
    /// By asset id.
    faces: HashMap<String, Vec<Face>>,
}

impl Known {
    fn load(cache: &Cache) -> Result<Option<Known>, String> {
        let file = immich::beside(cache.file());
        let Some(snapshot) = Snapshot::open(&file).map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        let failed = |error: immich::Error| error.to_string();
        let mut faces: HashMap<String, Vec<Face>> = HashMap::new();
        for face in snapshot.faces().map_err(failed)? {
            faces.entry(face.asset_id.clone()).or_default().push(face);
        }
        let mut assets = HashMap::new();
        for asset in snapshot.assets().map_err(failed)? {
            if let Some(rel_path) = asset.rel_path.clone() {
                assets.insert(rel_path, asset);
            }
        }
        Ok(Some(Known {
            people: snapshot
                .people()
                .map_err(failed)?
                .into_iter()
                .map(|person| (person.id.clone(), person))
                .collect(),
            assets,
            faces,
        }))
    }

    /// The faces of the asset whose person is named and not hidden, with the person.
    fn named<'a>(&'a self, asset: &Asset) -> impl Iterator<Item = (&'a Face, &'a Person)> {
        self.faces
            .get(&asset.id)
            .into_iter()
            .flatten()
            .filter_map(|face| Some((face, self.people.get(face.person_id.as_ref()?)?)))
            .filter(|(_, person)| person.named())
    }
}

/// Lower case and one spelling of every letter, so `Anna` and `anna` are one name.
fn fold(name: &str) -> String {
    name.trim().nfc().collect::<String>().to_lowercase()
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_people(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|root| root.eq_ignore_ascii_case(PEOPLE))
}

fn words(name: &str) -> Vec<&str> {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    words.sort_unstable();
    words
}

/// How near a tag's name is to a person's, if near at all: the same words in another order
/// (family name first or last), a name that starts the other, the same first name, or a letter
/// or two apart.
fn nearness(name: &str, tag: &str) -> Option<f64> {
    let (name, tag) = (fold(name), fold(leaf(tag)));
    if words(&name).len() > 1 && words(&name) == words(&tag) {
        return Some(0.9);
    }
    let shorter = name.chars().count().min(tag.chars().count());
    if shorter >= 3 && (tag.starts_with(&name) || name.starts_with(&tag)) {
        return Some(0.6);
    }
    let first = |text: &str| text.split_whitespace().next().map(String::from);
    if first(&name).is_some_and(|word| word.chars().count() >= 3) && first(&name) == first(&tag) {
        return Some(0.55);
    }
    let allowed = if shorter < 8 { 1 } else { 2 };
    (shorter >= 4 && browse::distance(&name, &tag) <= allowed).then_some(0.5)
}

/// A person Immich names in the library's photos, and what writing them would come to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Immich's id of the person.
    pub id: String,
    pub name: String,
    /// Photos writing them would change.
    pub photos: usize,
    /// Why nothing can be written for them, when nothing can.
    pub refused: Option<String>,
}

/// Why a person cannot be written at all: another person in Immich has the same name, and the
/// files would not tell the two apart.
fn ambiguous(known: &Known, person: &Person) -> Option<String> {
    let twins = known
        .people
        .values()
        .filter(|other| other.named() && fold(&other.name) == fold(&person.name))
        .count();
    (twins > 1).then(|| {
        format!(
            "Immich has {twins} persons named {}: rename one there so the files can tell them apart",
            person.name.trim()
        )
    })
}

/// Every person Immich names in the photos of the library whose photos do not all say them yet,
/// the most photos first. A person the files cannot tell from another is listed as refused.
pub fn sure(cache: &Cache) -> Result<Vec<Found>, String> {
    let Some(known) = Known::load(cache)? else {
        return Ok(Vec::new());
    };
    let failed = |error: rusqlite::Error| error.to_string();
    let mut photos: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for rel_path in Scope::Filter(Filter::all()).paths(cache).map_err(failed)? {
        let Some(asset) = known.assets.get(&rel_path) else {
            continue;
        };
        let persons: BTreeSet<&str> = known.named(asset).map(|(_, person)| person.id.as_str()).collect();
        for id in persons {
            photos.entry(id).or_default().push(rel_path.clone());
        }
    }
    let every: Vec<String> = photos
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let stated = cache.stated(&every).map_err(failed)?;
    let shapes = cache.shapes(&every).map_err(failed)?;

    let mut found = Vec::new();
    for (id, rel_paths) in photos {
        let Some(person) = known.people.get(id) else {
            continue;
        };
        let refused = ambiguous(&known, person);
        let chosen = BTreeSet::from([id.to_string()]);
        let changed = rel_paths
            .iter()
            .filter(|rel_path| {
                let said = stated.get(*rel_path).and_then(|stated| stated.said.regions.as_ref());
                let shape = shapes.get(*rel_path).copied().unwrap_or_default();
                let asset = &known.assets[*rel_path];
                matches!(merged(&known, asset, said, shape, &chosen), Some(Ok(_)))
            })
            .count();
        let photos = match refused {
            Some(_) => rel_paths.len(),
            None => changed,
        };
        if photos > 0 && (refused.is_none() || changed > 0) {
            found.push(Found {
                id: id.to_string(),
                name: person.name.trim().to_string(),
                photos,
                refused,
            });
        }
    }
    found.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.name.cmp(&other.name)));
    Ok(found)
}

/// Writes these persons, by Immich id, into the photos of the scope Immich finds them in.
pub fn wanted(cache: &Cache, persons: &BTreeSet<String>, scope: &Scope) -> cache::Result<Vec<Wanted>> {
    let Some(known) = Known::load(cache).map_err(failed)? else {
        return Ok(Vec::new());
    };
    let chosen: Vec<String> = scope
        .paths(cache)?
        .into_iter()
        .filter(|rel_path| {
            known
                .assets
                .get(rel_path)
                .is_some_and(|asset| known.named(asset).any(|(_, person)| persons.contains(&person.id)))
        })
        .collect();
    let stated = cache.stated(&chosen)?;
    let shapes = cache.shapes(&chosen)?;
    let refused: HashMap<&str, String> = persons
        .iter()
        .filter_map(|id| Some((id.as_str(), ambiguous(&known, known.people.get(id)?)?)))
        .collect();

    let mut wanted = Vec::new();
    for rel_path in &chosen {
        let asset = &known.assets[rel_path];
        let why = known
            .named(asset)
            .find_map(|(_, person)| refused.get(person.id.as_str()).cloned());
        if let Some(why) = why {
            wanted.push(Wanted::refused(rel_path.clone(), why));
            continue;
        }
        let said = stated.get(rel_path).and_then(|stated| stated.said.regions.as_ref());
        let shape = shapes.get(rel_path).copied().unwrap_or_default();
        match merged(&known, asset, said, shape, persons) {
            Some(Ok(faces)) => wanted.push(Wanted::new(rel_path.clone(), Change::of([Field::Faces(Some(faces))]))),
            Some(Err(why)) => wanted.push(Wanted::refused(rel_path.clone(), why)),
            None => {}
        }
    }
    Ok(wanted)
}

/// How much of the smaller box the two share, from 0 to 1.
fn shared(one: &write::Face, other: &write::Face) -> f64 {
    let span = |centre: f64, size: f64| (centre - size / 2.0, centre + size / 2.0);
    let overlap = |(a0, a1): (f64, f64), (b0, b1): (f64, f64)| (a1.min(b1) - a0.max(b0)).max(0.0);
    let across = overlap(span(one.x, one.width), span(other.x, other.width));
    let down = overlap(span(one.y, one.height), span(other.y, other.height));
    let smaller = (one.width * one.height).min(other.width * other.height);
    match smaller > 0.0 {
        true => across * down / smaller,
        false => 0.0,
    }
}

/// The regions a photo gets when the chosen persons Immich found in it are merged into what it
/// says: `None` when it says that already or Immich finds none of them in it, `Err` when a box
/// would land somewhere else than the face or something the file says would be lost.
fn merged(
    known: &Known,
    asset: &Asset,
    said: Option<&Regions>,
    shape: cache::Shape,
    persons: &BTreeSet<String>,
) -> Option<Result<Faces, String>> {
    let found: Vec<(&Face, &Person)> = known
        .named(asset)
        .filter(|(_, person)| persons.contains(&person.id))
        .collect();
    if found.is_empty() {
        return None;
    }
    if let Some(why) = refusal(asset, shape, &found) {
        return Some(Err(why));
    }
    let (Some(width), Some(height)) = (shape.width, shape.height) else {
        return Some(Err("the scan found no size for it".to_string()));
    };
    let immich: Vec<write::Face> = found
        .iter()
        .filter_map(|(face, person)| {
            let area = boxes::stored(face, shape.orientation)?;
            Some(write::Face {
                name: person.name.trim().to_string(),
                x: area.x,
                y: area.y,
                width: area.w,
                height: area.h,
            })
        })
        .collect();
    if immich.is_empty() {
        return None;
    }
    let names: BTreeSet<String> = immich.iter().map(|face| fold(&face.name)).collect();

    let (old, persons) = match said {
        Some(said) => (said.faces.as_slice(), said.persons.as_slice()),
        None => (&[][..], &[][..]),
    };
    let kept: Vec<&write::Face> = old
        .iter()
        .filter(|face| !names.contains(&fold(&face.name)))
        .filter(|face| !immich.iter().any(|new| shared(face, new) >= SAME_FACE))
        .collect();
    if kept.iter().any(|face| face.name.trim().is_empty()) {
        return Some(Err("a face box without a name would be lost".to_string()));
    }
    if let Some(said) = said
        && !kept.is_empty()
        && let (Some(their_width), Some(their_height)) = (said.width, said.height)
    {
        let theirs = their_width as f64 / their_height.max(1) as f64;
        let ours = width as f64 / height as f64;
        if (theirs / ours - 1.0).abs() > SHAPE {
            return Some(Err(format!(
                "its face boxes were measured on a picture of {their_width} by {their_height}, the file is {width} by {height}"
            )));
        }
    }

    let mut faces: Vec<write::Face> = kept.into_iter().cloned().chain(immich).collect();
    faces.sort_by(|one, other| one.x.total_cmp(&other.x).then(one.y.total_cmp(&other.y)));
    let boxed_before: BTreeSet<String> = old.iter().map(|face| fold(&face.name)).collect();
    let boxed_after: BTreeSet<String> = faces.iter().map(|face| fold(&face.name)).collect();
    let mut unboxed: Vec<String> = Vec::new();
    for name in persons.iter().map(|name| name.trim()) {
        let folded = fold(name);
        if !boxed_before.contains(&folded)
            && !boxed_after.contains(&folded)
            && !unboxed.iter().any(|known| fold(known) == folded)
        {
            unboxed.push(name.to_string());
        }
    }
    let faces = Faces {
        width,
        height,
        faces,
        persons: unboxed,
    };
    match changeset::regions_say(said, &faces) {
        true => None,
        false => Some(Ok(faces)),
    }
}

/// The photos among these in which Immich names someone the file does not: no face region and no
/// people tag of that name, or of one near it. By photo, the names. `None` when nothing was
/// fetched from Immich, so nothing is known either way.
pub fn untold(cache: &Cache, rel_paths: &[String]) -> Result<Option<BTreeMap<String, Vec<String>>>, String> {
    let Some(known) = Known::load(cache)? else {
        return Ok(None);
    };
    let stated = cache.stated(rel_paths).map_err(|error| error.to_string())?;
    let mut untold = BTreeMap::new();
    for rel_path in rel_paths {
        let Some(asset) = known.assets.get(rel_path) else {
            continue;
        };
        let said = stated.get(rel_path).map(|stated| &stated.said);
        let mut names: Vec<String> = said
            .map(|said| {
                tags::deepest(&said.tags)
                    .into_iter()
                    .filter(|tag| is_people(tag))
                    .map(|tag| leaf(&tag).to_string())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(regions) = said.and_then(|said| said.regions.as_ref()) {
            names.extend(regions.faces.iter().map(|face| face.name.clone()));
            names.extend(regions.persons.iter().cloned());
        }
        let mut missing: Vec<String> = known
            .named(asset)
            .map(|(_, person)| person.name.trim().to_string())
            .filter(|name| {
                !names
                    .iter()
                    .any(|said| fold(said) == fold(name) || nearness(name, said).is_some())
            })
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            untold.insert(rel_path.clone(), missing);
        }
    }
    Ok(Some(untold))
}

/// Why a box would land somewhere else than the face, if it would: the photo is offline in
/// Immich, or the file is not the picture Immich measured the faces on.
fn refusal(asset: &Asset, shape: cache::Shape, found: &[(&Face, &Person)]) -> Option<String> {
    if asset.offline {
        return Some("Immich has it offline".to_string());
    }
    let (Some(width), Some(height)) = (shape.width, shape.height) else {
        return Some("the scan found no size for it".to_string());
    };
    if let (Some(seen_width), Some(seen_height)) = (asset.width, asset.height)
        && (seen_width, seen_height) != (width, height)
    {
        return Some(format!(
            "Immich saw it at {seen_width} by {seen_height}, the file is {width} by {height}"
        ));
    }
    let turned = |orientation: Option<i64>| orientation.filter(|turn| (1..=8).contains(turn)).unwrap_or(1);
    if turned(asset.orientation) != turned(shape.orientation) {
        return Some(format!(
            "Immich saw it turned {}, the file says {}",
            turned(asset.orientation),
            turned(shape.orientation)
        ));
    }
    let shown = match boxes::sideways(shape.orientation) {
        true => height as f64 / width as f64,
        false => width as f64 / height as f64,
    };
    let measured = found
        .iter()
        .map(|(face, _)| face.image_width as f64 / face.image_height.max(1) as f64)
        .find(|measured| (measured / shown - 1.0).abs() > SHAPE);
    measured.map(|_| "Immich measured the faces on a picture of another shape".to_string())
}

fn failed(why: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(why))
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
