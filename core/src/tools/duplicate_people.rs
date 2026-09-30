//! Duplicate People: a person a photo names in more than one box on the same face, or more than
//! once among its persons, named once. The largest of the boxes stays as it is, the copies go,
//! every other box and person stays exactly as it was. Boxes of one name on different faces are
//! refused: which one is the person is not the app's to guess.
//!
//! It reads only what the files say; Immich plays no part.

use std::collections::{BTreeMap, BTreeSet};

use super::people::{SAME_FACE, fold, largest, shared};
use crate::cache::{self, Cache};
use crate::changeset::{self, Wanted};
use crate::filter::Filter;
use crate::metadata::Regions;
use crate::scope::Scope;
use crate::write::{Change, Faces, Field};

/// A person named more than once in some photos, and what naming them once would come to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name folded: what the fix stands for.
    pub key: String,
    /// The name as the first photo spells it.
    pub name: String,
    /// Photos that would change.
    pub photos: usize,
    /// The boxes and names that would go from them.
    pub copies: usize,
    /// Photos that name them more than once but are refused, and why, the first reason.
    pub refused: usize,
    pub why: Option<String>,
}

/// The names this photo names more than once, folded, with the spelling of the first.
fn doubled(said: &Regions) -> BTreeMap<String, String> {
    let mut seen: BTreeMap<String, (String, usize, usize)> = BTreeMap::new();
    for face in &said.faces {
        let entry = seen
            .entry(fold(&face.name))
            .or_insert((face.name.trim().to_string(), 0, 0));
        entry.1 += 1;
    }
    for name in &said.persons {
        let entry = seen.entry(fold(name)).or_insert((name.trim().to_string(), 0, 0));
        entry.2 += 1;
    }
    seen.into_iter()
        .filter(|(key, (_, boxes, persons))| !key.is_empty() && (*boxes > 1 || *persons > 1))
        .map(|(key, (name, _, _))| (key, name))
        .collect()
}

/// How the photo reads with the chosen names named once: `None` when it names none of them more
/// than once or naming them once changes nothing, `Err` when it cannot be done safely.
fn tidied(said: &Regions, chosen: &BTreeSet<String>) -> Option<Result<(Field, usize), String>> {
    let names: Vec<(String, String)> = doubled(said)
        .into_iter()
        .filter(|(key, _)| chosen.contains(key))
        .collect();
    if names.is_empty() {
        return None;
    }
    if said.faces.iter().any(|face| face.name.trim().is_empty()) {
        return Some(Err("a face box without a name would be lost".to_string()));
    }
    let mut dropped: BTreeSet<usize> = BTreeSet::new();
    for (key, name) in &names {
        let boxes: Vec<(usize, &crate::write::Face)> = said
            .faces
            .iter()
            .enumerate()
            .filter(|(_, face)| fold(&face.name) == *key)
            .collect();
        if boxes.len() < 2 {
            continue;
        }
        let one_face = boxes
            .iter()
            .enumerate()
            .all(|(at, (_, one))| boxes[at + 1..].iter().all(|(_, other)| shared(one, other) >= SAME_FACE));
        if !one_face {
            return Some(Err(format!("{name} has boxes on different faces in it")));
        }
        let kept = largest(boxes.iter().map(|(_, face)| (*face).clone()).collect())?;
        let kept_at = boxes.iter().find(|(_, face)| **face == kept).map(|(at, _)| *at)?;
        dropped.extend(boxes.iter().map(|(at, _)| *at).filter(|at| *at != kept_at));
    }

    let faces: Vec<crate::write::Face> = said
        .faces
        .iter()
        .enumerate()
        .filter(|(at, _)| !dropped.contains(at))
        .map(|(_, face)| face.clone())
        .collect();
    let mut persons: Vec<String> = Vec::new();
    for name in &said.persons {
        let key = fold(name);
        let doubled = names.iter().any(|(chosen, _)| *chosen == key);
        if doubled && persons.iter().any(|known| fold(known) == key) {
            continue;
        }
        persons.push(name.clone());
    }
    let copies = dropped.len() + said.persons.len() - persons.len();
    if copies == 0 {
        return None;
    }

    if faces.is_empty() {
        let unchanged = persons.iter().map(|name| name.trim()).collect::<Vec<_>>()
            == said.persons.iter().map(|name| name.trim()).collect::<Vec<_>>();
        return (!unchanged).then_some(Ok((Field::Persons(persons), copies)));
    }
    let (Some(width), Some(height)) = (said.width, said.height) else {
        return Some(Err("its face boxes have no size they were measured on".to_string()));
    };
    let boxed: BTreeSet<String> = faces.iter().map(|face| fold(&face.name)).collect();
    let mut unboxed: Vec<String> = Vec::new();
    for name in persons {
        let key = fold(&name);
        if !boxed.contains(&key) && !unboxed.iter().any(|known| fold(known) == key) {
            unboxed.push(name.trim().to_string());
        }
    }
    let wanted = Faces {
        width,
        height,
        faces,
        persons: unboxed,
    };
    match changeset::regions_say(Some(said), &wanted) {
        true => None,
        false => Some(Ok((Field::Faces(Some(wanted)), copies))),
    }
}

/// The photos of the scope that name someone, with what they say.
fn said_in(cache: &Cache, scope: &Scope) -> cache::Result<Vec<(String, Regions)>> {
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    Ok(paths
        .into_iter()
        .filter_map(|rel_path| {
            let regions = stated.get(&rel_path)?.said.regions.clone()?;
            Some((rel_path, regions))
        })
        .collect())
}

/// Every person some photo names more than once, the most photos first. A person whose photos
/// are all refused is no fix: nothing would change until they are put right by hand.
pub fn sure(cache: &Cache) -> Result<Vec<Found>, String> {
    let said = said_in(cache, &Scope::Filter(Filter::all())).map_err(|error| error.to_string())?;
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for (_, regions) in &said {
        for (key, name) in doubled(regions) {
            let chosen = BTreeSet::from([key.clone()]);
            let entry = found.entry(key.clone()).or_insert_with(|| Found {
                key,
                name,
                photos: 0,
                copies: 0,
                refused: 0,
                why: None,
            });
            match tidied(regions, &chosen) {
                Some(Ok((_, copies))) => {
                    entry.photos += 1;
                    entry.copies += copies;
                }
                Some(Err(why)) => {
                    entry.refused += 1;
                    entry.why.get_or_insert(why);
                }
                None => {}
            }
        }
    }
    let mut found: Vec<Found> = found.into_values().filter(|found| found.photos > 0).collect();
    found.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.name.cmp(&other.name)));
    Ok(found)
}

/// Names these persons, folded, once in every photo of the scope.
pub fn wanted(cache: &Cache, chosen: &BTreeSet<String>, scope: &Scope) -> cache::Result<Vec<Wanted>> {
    let mut wanted = Vec::new();
    for (rel_path, regions) in said_in(cache, scope)? {
        match tidied(&regions, chosen) {
            Some(Ok((field, _))) => wanted.push(Wanted::new(rel_path, Change::of([field]))),
            Some(Err(why)) => wanted.push(Wanted::refused(rel_path, why)),
            None => {}
        }
    }
    Ok(wanted)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
