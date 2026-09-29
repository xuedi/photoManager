//! People from tags: a person a `people` tag names becomes a person of the photo, in
//! `PersonInImage`, without a box, so the tag is no longer the only record of them.
//!
//! Sure only when the tag's last level is the name of one person the library knows, but for
//! case: a person some photo names, or a named person of the Immich snapshot. Every other name
//! is the person's to give, with Tag to Person. A tag with tags below it is a group, never a
//! person. The tags themselves are not touched.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::OptionalExtension;

use super::people::{self, fold};
use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::filter::Filter;
use crate::metadata::Regions;
use crate::scope::Scope;
use crate::write::{Change, Field};

/// A people tag whose last level names one person the library knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub tag: String,
    pub name: String,
    /// Photos of the tag that do not name the person yet.
    pub photos: usize,
}

/// Whether a tag is below the `people` root, in any case.
pub fn is_people(tag: &str) -> bool {
    let mut levels = tag.split('/');
    levels.next().is_some_and(|root| root.eq_ignore_ascii_case("people")) && levels.next().is_some()
}

/// Whether any photo carries a tag below this one: a group, not a person.
pub fn is_group(cache: &Cache, tag: &str) -> cache::Result<bool> {
    let below: Option<i64> = cache
        .connection()
        .query_row(
            "SELECT 1 FROM tag WHERE path >= ?1 || '/' AND path < ?1 || '0' LIMIT 1",
            [tag],
            |row| row.get(0),
        )
        .optional()?;
    Ok(below.is_some())
}

/// Every name a person of the library goes by: the persons the photos name, and Immich's named
/// persons. By the name folded, each spelling once.
fn known(cache: &Cache) -> Result<HashMap<String, BTreeSet<String>>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let mut statement = cache
        .connection()
        .prepare("SELECT DISTINCT name FROM person")
        .map_err(failed)?;
    let mut names: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(failed)?
        .collect::<rusqlite::Result<_>>()
        .map_err(failed)?;
    names.extend(people::immich_names(cache)?);
    let mut known: HashMap<String, BTreeSet<String>> = HashMap::new();
    for name in names {
        let name = name.trim().to_string();
        if !name.is_empty() {
            known.entry(fold(&name)).or_default().insert(name);
        }
    }
    Ok(known)
}

/// The people tags no other tag goes below: a tag with tags below it is a group.
fn leaves(all: &BTreeSet<String>) -> Vec<String> {
    all.iter()
        .filter(|tag| is_people(tag))
        .filter(|tag| {
            let below = format!("{tag}/");
            all.range(below.clone()..)
                .next()
                .is_none_or(|next| !next.starts_with(&below))
        })
        .cloned()
        .collect()
}

/// The last level of a tag.
fn leaf(tag: &str) -> &str {
    tag.rsplit('/').next().unwrap_or(tag).trim()
}

/// Every people tag that names one person the library knows and has photos that do not name
/// them yet, the most photos first.
pub fn sure(cache: &Cache) -> Result<Vec<Found>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let paths = Scope::Filter(Filter::all()).paths(cache).map_err(failed)?;
    let stated = cache.stated(&paths).map_err(failed)?;
    let all: BTreeSet<String> = stated.values().flat_map(|one| one.said.tags.iter().cloned()).collect();
    let known = known(cache)?;

    let mut found = Vec::new();
    for tag in leaves(&all) {
        let Some(spellings) = known.get(&fold(leaf(&tag))) else {
            continue;
        };
        let [name] = spellings.iter().collect::<Vec<&String>>()[..] else {
            continue;
        };
        let photos = stated
            .values()
            .filter(|one| one.said.tags.contains(&tag))
            .filter(|one| adding(one.said.regions.as_ref(), std::slice::from_ref(name)).is_some())
            .count();
        if photos > 0 {
            found.push(Found {
                tag,
                name: name.clone(),
                photos,
            });
        }
    }
    found.sort_by(|one, other| other.photos.cmp(&one.photos).then(one.tag.cmp(&other.tag)));
    Ok(found)
}

/// Each photo of the scope that carries one of these tags gets the persons they name, without a
/// box, where it does not name them yet. By tag, the name.
pub fn wanted(cache: &Cache, named: &BTreeMap<String, String>, scope: &Scope) -> cache::Result<Vec<Wanted>> {
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    let mut wanted = Vec::new();
    for rel_path in paths {
        let Some(one) = stated.get(&rel_path) else { continue };
        let names: Vec<String> = one.said.tags.iter().filter_map(|tag| named.get(tag).cloned()).collect();
        if names.is_empty() {
            continue;
        }
        if let Some(persons) = adding(one.said.regions.as_ref(), &names) {
            wanted.push(Wanted::new(rel_path, Change::of([Field::Persons(persons)])));
        }
    }
    Ok(wanted)
}

/// The whole list of persons a photo names once these are added: every name it has, then each
/// new one it does not name in any spelling. `None` when it names them all already.
pub fn adding(said: Option<&Regions>, names: &[String]) -> Option<Vec<String>> {
    let mut persons: Vec<String> = said
        .map(|said| said.named().into_iter().map(|(name, _)| name).collect())
        .unwrap_or_default();
    let before = persons.len();
    for name in names.iter().map(|name| name.trim()) {
        if !name.is_empty() && !persons.iter().any(|known| fold(known) == fold(name)) {
            persons.push(name.to_string());
        }
    }
    (persons.len() > before).then_some(persons)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::changeset::{ChangeSet, Verdict};
    use crate::immich::{self, fake, fake::FakeImmich};
    use crate::tools::testing::Library;

    /// A photo tagged with a person who has a box in another photo, but not in this one.
    const TAGGED: &str = "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG";

    fn found(library: &Library) -> Vec<(String, String, usize)> {
        sure(&library.cache)
            .unwrap()
            .into_iter()
            .map(|found| (found.tag, found.name, found.photos))
            .collect()
    }

    fn fetch(library: &Library, immich: &FakeImmich) {
        let mut client = immich::Client::new(&immich.url, fake::KEY).unwrap();
        immich::fetch(
            &mut client,
            &immich::beside(library.cache.file()),
            None,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
    }

    fn persons(library: &Library, rel_path: &str) -> Vec<(String, bool)> {
        library.cache.stated(&[rel_path.to_string()]).unwrap()[rel_path]
            .said
            .regions
            .as_ref()
            .map(Regions::named)
            .unwrap_or_default()
    }

    #[test]
    fn a_tag_of_a_known_name_is_sure_and_a_group_or_a_stranger_is_not() {
        let library = Library::new("people-from-tags-sure");
        assert_eq!(
            found(&library),
            [("people/family/Anna".to_string(), "Anna".to_string(), 1)],
            "Anna has a box in another photo; the photo that names her already is not counted, \
             people/family is a group, and nobody the library knows is called me, Ben or Kira yet"
        );

        let immich = FakeImmich::serve(fake::Data::over(&library.root));
        fetch(&library, &immich);
        let tags: Vec<(String, String)> = found(&library).into_iter().map(|(tag, name, _)| (tag, name)).collect();
        assert!(
            tags.contains(&("People/Kira".to_string(), "Kira".to_string())),
            "a name Immich knows is known: {tags:?}"
        );
        assert!(tags.contains(&("people/groupChina/Ben".to_string(), "Ben".to_string())));
        assert!(!tags.iter().any(|(tag, _)| tag == "people/me"));

        immich.change(|data| data.rename("p-lena", "anna"));
        fetch(&library, &immich);
        assert!(
            !found(&library).iter().any(|(tag, _, _)| tag == "people/family/Anna"),
            "Anna in the files and anna in Immich: two persons, so not sure"
        );
    }

    #[test]
    fn the_name_is_written_without_a_box_and_found_no_more() {
        let mut library = Library::new("people-from-tags-write");
        let named = BTreeMap::from([("people/family/Anna".to_string(), "Anna".to_string())]);
        let whole = Scope::Filter(Filter::all());
        let wanted = wanted(&library.cache, &named, &whole).unwrap();
        let set = ChangeSet::build(&library.cache, "Write people from tags", &wanted).unwrap();
        let rows: Vec<(&str, &Verdict)> = set
            .rows
            .iter()
            .map(|row| (row.rel_path.as_str(), &row.verdict))
            .collect();
        assert_eq!(rows, [(TAGGED, &Verdict::Change)], "the photo that names her is left");
        assert_eq!(set.rows[0].tells(), "people: none -> Anna (no box)");

        let tags = library.cache.stated(&[TAGGED.to_string()]).unwrap()[TAGGED]
            .said
            .tags
            .clone();
        assert_eq!(library.apply(&set).written, 1);
        library.rescan();
        assert_eq!(persons(&library, TAGGED), [("Anna".to_string(), false)]);
        assert_eq!(
            library.cache.stated(&[TAGGED.to_string()]).unwrap()[TAGGED].said.tags,
            tags,
            "the tag stays"
        );
        assert!(found(&library).is_empty(), "a second run finds nothing");
    }

    #[test]
    fn a_new_name_keeps_every_name_the_photo_had() {
        let regions = Regions {
            persons: vec!["Tom".to_string()],
            faces: vec![crate::write::Face {
                name: "Anna".to_string(),
                x: 0.5,
                y: 0.5,
                width: 0.1,
                height: 0.1,
            }],
            ..Regions::default()
        };
        assert_eq!(
            adding(Some(&regions), &["Mia".to_string()]),
            Some(vec!["Anna".to_string(), "Tom".to_string(), "Mia".to_string()])
        );
        assert_eq!(
            adding(Some(&regions), &["anna".to_string()]),
            None,
            "named, in any case"
        );
        assert!(is_people("People/Kira") && !is_people("people") && !is_people("places/inChina"));
    }
}
