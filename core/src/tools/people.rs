//! People from Immich: who Immich found in each photo, written into the photo itself. One
//! question per named person, answered with the people tag that person is; then each photo gets
//! a face region for every answered person Immich found in it, the persons named, and their tags.
//!
//! Immich is the master for the faces and the files for the tags: the region list is replaced as
//! a whole, a tag is only ever added. What Immich knows comes from the snapshot beside the cache,
//! fetched on a button; this tool never talks to Immich itself.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;

use super::{Answer, Answers, Offer, Question, Tool};
use crate::browse::{self, TagTree};
use crate::cache::{self, Cache};
use crate::changeset::{self, Wanted};
use crate::filter::Filter;
use crate::geo::Geo;
use crate::immich::{self, Asset, Face, Person, Snapshot, boxes};
use crate::scope::Scope;
use crate::tags;
use crate::write::{self, Change, Faces, Field};

pub struct PeopleFromImmich;

/// The root every person's tag is under, in either spelling.
const PEOPLE: &str = "people";
/// How far the shape of the picture Immich measured on may be from the file's.
const SHAPE: f64 = 0.02;

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

fn failed(why: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(1), Some(why))
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

/// Every person tag of the tree: the tags under `people` nothing is below, in either spelling
/// of the root, with how many photos carry each.
fn person_tags(tree: &TagTree) -> Vec<(String, i64)> {
    tree.nodes()
        .filter(|(path, _)| is_people(path) && path.contains('/'))
        .filter(|(path, _)| tree.children(path).is_empty())
        .map(|(path, count)| (path.to_string(), count))
        .collect()
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

/// A new tag for a person no tag names yet.
fn new_tag(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .replace(['/', '|'], " ")
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    tags::path(&format!("{PEOPLE}/{cleaned}")).ok()
}

/// What a person could be tagged as, best first: the one tag of the same name is sure; tags of
/// nearly the same name follow, then a new tag.
fn offers(name: &str, known: &[(String, i64)]) -> Vec<Offer> {
    let tag = |path: &str, confidence: f64, words: String| Offer {
        confidence,
        ..Offer::of_answer(Answer::Tag(path.to_string()), words)
    };
    let exact: Vec<&(String, i64)> = known
        .iter()
        .filter(|(path, _)| fold(leaf(path)) == fold(name))
        .collect();
    let mut offered: Vec<Offer> = exact
        .iter()
        .map(|(path, _)| {
            Offer {
                exact: true,
                ..tag(path, 1.0, path.clone())
            }
            .with_sure(exact.len() == 1)
        })
        .collect();
    let mut near: Vec<(f64, i64, &String)> = known
        .iter()
        .filter(|(path, _)| fold(leaf(path)) != fold(name))
        .filter_map(|(path, count)| Some((nearness(name, path)?, *count, path)))
        .collect();
    near.sort_by(|one, other| {
        other
            .0
            .total_cmp(&one.0)
            .then(other.1.cmp(&one.1))
            .then(one.2.cmp(other.2))
    });
    offered.extend(
        near.iter()
            .take(5)
            .map(|(confidence, _, path)| tag(path, *confidence, (*path).clone())),
    );
    if exact.is_empty()
        && let Some(path) = new_tag(name)
    {
        offered.push(tag(&path, 0.3, format!("{path}, a new tag")));
    }
    offered
}

/// The tag each person was answered with, by Immich person id. Left alone is no tag.
fn tag_of(answers: &Answers, person: &Person) -> Option<String> {
    match answers.get(&person.id) {
        Some(Answer::Tag(path)) => Some(path.clone()),
        _ => None,
    }
}

impl Tool for PeopleFromImmich {
    type Settings = Answers;

    fn questions(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> Result<Vec<Question>, String> {
        let Some(known) = Known::load(cache)? else {
            return Ok(Vec::new());
        };
        let mut photos: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for rel_path in scope.paths(cache).map_err(|error| error.to_string())? {
            let Some(asset) = known.assets.get(&rel_path) else {
                continue;
            };
            for (_, person) in known.named(asset) {
                photos.entry(person.id.as_str()).or_default().insert(asset.id.as_str());
            }
        }
        let tags = person_tags(&TagTree::take(cache).map_err(|error| error.to_string())?);
        let mut questions: Vec<Question> = photos
            .into_iter()
            .filter_map(|(id, assets)| {
                let person = known.people.get(id)?;
                Some(Question {
                    answer: answers.get(id).cloned(),
                    ..Question::place(id, person.name.clone(), assets.len(), offers(&person.name, &tags))
                })
            })
            .collect();
        questions.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.title.cmp(&other.title)));
        Ok(questions)
    }

    fn wanted(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        answers: &Answers,
    ) -> cache::Result<Vec<Wanted>> {
        let Some(known) = Known::load(cache).map_err(failed)? else {
            return Ok(Vec::new());
        };
        let paths = scope.paths(cache)?;
        let in_scope: HashSet<&String> = paths.iter().collect();
        let mut chosen: Vec<String> = Vec::new();
        for rel_path in &paths {
            if let Some(asset) = known.assets.get(rel_path)
                && known.named(asset).any(|(_, person)| tag_of(answers, person).is_some())
            {
                chosen.push(rel_path.clone());
            }
        }
        let stated = cache.stated(&chosen)?;
        let shapes = cache.shapes(&chosen)?;

        let mut wanted = Vec::new();
        for rel_path in &chosen {
            let (Some(asset), Some(photo)) = (known.assets.get(rel_path), stated.get(rel_path)) else {
                continue;
            };
            let shape = shapes.get(rel_path).copied().unwrap_or_default();
            let found: Vec<(&Face, String)> = known
                .named(asset)
                .filter_map(|(face, person)| Some((face, tag_of(answers, person)?)))
                .collect();

            let mut fields = Vec::new();
            let mut then = photo.said.tags.clone();
            let mut added = false;
            for (_, tag) in &found {
                if !then.contains(tag) {
                    then.push(tag.clone());
                    added = true;
                }
            }
            if added {
                fields.push(Field::Tags(tags::deepest(&then)));
            }
            let faces = shape.width.zip(shape.height).map(|(width, height)| {
                let mut faces: Vec<write::Face> = found
                    .iter()
                    .filter_map(|(face, tag)| {
                        let area = boxes::stored(face, shape.orientation)?;
                        Some(write::Face {
                            name: leaf(tag).to_string(),
                            x: area.x,
                            y: area.y,
                            width: area.w,
                            height: area.h,
                        })
                    })
                    .collect();
                faces.sort_by(|one, other| one.x.total_cmp(&other.x).then(one.y.total_cmp(&other.y)));
                Faces { width, height, faces }
            });
            if let Some(faces) = faces.filter(|faces| !faces.faces.is_empty())
                && !changeset::regions_say(photo.said.regions.as_ref(), &faces)
            {
                fields.push(Field::Faces(Some(faces)));
            }
            if fields.is_empty() {
                continue;
            }
            wanted.push(match refusal(asset, shape, &found) {
                Some(why) => Wanted::refused(rel_path.clone(), why),
                None => Wanted::new(rel_path.clone(), Change::of(fields)),
            });
        }

        if *scope == Scope::Filter(Filter::all()) {
            let mut unknown: Vec<&String> = known
                .assets
                .iter()
                .filter(|(rel_path, asset)| {
                    !in_scope.contains(rel_path)
                        && known.named(asset).any(|(_, person)| tag_of(answers, person).is_some())
                })
                .map(|(rel_path, _)| rel_path)
                .collect();
            unknown.sort();
            wanted.extend(
                unknown
                    .into_iter()
                    .map(|rel_path| Wanted::refused(rel_path.clone(), "Immich knows it, the library does not")),
            );
        }
        Ok(wanted)
    }
}

/// The persons whose tag is sure - one people tag has their exact name - each with how many
/// photos writing them would change. A person every photo already says is not among them.
pub fn sure(cache: &Cache) -> Result<Vec<(Question, usize)>, String> {
    let whole = Scope::Filter(Filter::all());
    let asked: Vec<Question> = PeopleFromImmich
        .questions(cache, None, &whole, &Answers::default())?
        .into_iter()
        .filter(Question::confirmable)
        .collect();
    let (Some(known), false) = (Known::load(cache)?, asked.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut answers = Answers::default();
    for question in &asked {
        answers.set(&question.key, question.sure().map(|offer| offer.answer.clone()));
    }
    let changed: HashSet<String> = PeopleFromImmich
        .wanted(cache, None, &whole, &answers)
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|wanted| wanted.refused.is_none())
        .map(|wanted| wanted.rel_path)
        .collect();
    let mut photos: HashMap<&str, usize> = HashMap::new();
    for rel_path in &changed {
        let Some(asset) = known.assets.get(rel_path) else {
            continue;
        };
        let persons: HashSet<&str> = known.named(asset).map(|(_, person)| person.id.as_str()).collect();
        for person in persons {
            *photos.entry(person).or_default() += 1;
        }
    }
    Ok(asked
        .into_iter()
        .filter_map(|question| {
            let count = photos.get(question.key.as_str()).copied().unwrap_or_default();
            (count > 0).then_some((question, count))
        })
        .collect())
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
fn refusal(asset: &Asset, shape: cache::Shape, found: &[(&Face, String)]) -> Option<String> {
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

#[cfg(all(test, feature = "fixtures"))]
mod tests;
