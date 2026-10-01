//! Events from folders: the event a photo belongs to, written into its own field, IPTC's
//! `Event`, so a photo that leaves its folder keeps it. The name is the event folder's name part
//! as the folder layout reads it, without the date, which has fields of its own.
//!
//! Sure for every event folder with a name. A photo whose field names another event is never
//! overwritten: it is refused with both names, and the dashboard lists it. A photo in no event
//! folder gets nothing.

use std::collections::{BTreeMap, BTreeSet};

use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::scope::Scope;
use crate::write::{Change, Field};

/// An event folder whose photos do not all name its event yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub dir: String,
    pub name: String,
    /// Photos without the field.
    pub photos: usize,
    /// Photos whose field names another event.
    pub refused: usize,
}

struct Row {
    rel_path: String,
    dir: String,
    name: String,
    field: Option<String>,
}

/// Every photo in an event folder whose name part says something.
fn rows(cache: &Cache) -> cache::Result<Vec<Row>> {
    let mut statement = cache.connection().prepare(
        "SELECT rel_path, event_dir, trim(event_name), nullif(trim(event_field), '')
         FROM photo
         WHERE event_dir IS NOT NULL AND trim(coalesce(event_name, '')) != ''
         ORDER BY event_dir, rel_path",
    )?;
    statement
        .query_map([], |row| {
            Ok(Row {
                rel_path: row.get(0)?,
                dir: row.get(1)?,
                name: row.get(2)?,
                field: row.get(3)?,
            })
        })?
        .collect()
}

/// Why a photo whose field names another event is left as it is.
fn refusal(field: &str, name: &str) -> String {
    format!("its field names the event {field}, its folder {name}")
}

/// Every event folder with photos that do not name its event, by folder.
pub fn sure(cache: &Cache) -> Result<Vec<Found>, String> {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for row in rows(cache).map_err(|error| error.to_string())? {
        let event = found.entry(row.dir.clone()).or_insert_with(|| Found {
            dir: row.dir.clone(),
            name: row.name.clone(),
            photos: 0,
            refused: 0,
        });
        match row.field {
            None => event.photos += 1,
            Some(field) if field != row.name => event.refused += 1,
            Some(_) => {}
        }
    }
    Ok(found.into_values().filter(|event| event.photos > 0).collect())
}

/// The photos of the scope in these event folders: each without the field gets the folder's
/// event, each naming another is refused.
pub fn wanted(cache: &Cache, dirs: &BTreeSet<String>, scope: &Scope) -> cache::Result<Vec<Wanted>> {
    let inside: BTreeSet<String> = scope.paths(cache)?.into_iter().collect();
    Ok(rows(cache)?
        .into_iter()
        .filter(|row| dirs.contains(&row.dir) && inside.contains(&row.rel_path))
        .filter_map(|row| match row.field {
            None => Some(Wanted::new(row.rel_path, Change::of([Field::Event(Some(row.name))]))),
            Some(field) if field != row.name => Some(Wanted::refused(row.rel_path, refusal(&field, &row.name))),
            Some(_) => None,
        })
        .collect())
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::{ChangeSet, Verdict};
    use crate::filter::Filter;
    use crate::tools::testing::Library;

    const WEDDING: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    const OTHER: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG";
    const SOMMERFEST: &str = "Germany/2019-07-13 Sommerfest";

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn every(library: &Library) -> BTreeSet<String> {
        sure(&library.cache)
            .unwrap()
            .into_iter()
            .map(|found| found.dir)
            .collect()
    }

    #[test]
    fn one_fix_per_event_folder_and_another_name_is_refused() {
        let library = Library::new("events-sure");
        let found = sure(&library.cache).unwrap();
        let wedding = found.iter().find(|found| found.dir == WEDDING).unwrap();
        assert_eq!(
            (wedding.name.as_str(), wedding.photos, wedding.refused),
            ("Wedding Trip to Copenhagen", 1, 1)
        );
        let sommerfest = found.iter().find(|found| found.dir == SOMMERFEST).unwrap();
        assert_eq!(sommerfest.photos, 2, "the one that names it already is left out");
        assert!(
            found
                .iter()
                .all(|found| !found.dir.is_empty() && found.name != found.dir),
            "the name without the date"
        );

        let wanted = wanted(&library.cache, &every(&library), &whole()).unwrap();
        let other = wanted.iter().find(|one| one.rel_path == OTHER).unwrap();
        assert!(
            other.refused.as_deref().is_some_and(|why| why.contains("Wedding Trip")),
            "{other:?}"
        );
        assert!(
            !wanted.iter().any(|one| one.rel_path == "China/IMG_3140.JPG"),
            "a loose photo gets nothing"
        );
        assert!(
            wanted
                .iter()
                .filter(|one| one.rel_path.starts_with("Germany/2018-05-12 Canal Tour/"))
                .all(|one| one.change == Change::of([Field::Event(Some("Canal Tour".to_string()))])),
            "a sub-folder is its event's"
        );
    }

    #[test]
    fn written_a_second_look_finds_nothing() {
        let mut library = Library::new("events-written");
        let dirs = every(&library);
        let wanted = wanted(&library.cache, &dirs, &whole()).unwrap();
        let set = ChangeSet::build(&library.cache, "Write the event from the folder", &wanted).unwrap();
        assert!(
            set.rows
                .iter()
                .any(|row| row.verdict == Verdict::Refused(refusal("Wedding Trip", "Wedding Trip to Copenhagen")))
        );
        library.apply(&set);
        assert!(sure(&library.cache).unwrap().is_empty(), "{:?}", sure(&library.cache));
        let again = super::wanted(&library.cache, &dirs, &whole()).unwrap();
        assert_eq!(
            again.iter().map(|one| one.rel_path.as_str()).collect::<Vec<_>>(),
            [OTHER],
            "only the refused one is left"
        );
    }

    #[test]
    fn an_event_below_a_city_folder_is_named_too() {
        let library = Library::new("events-layout");
        let found = sure(&library.cache).unwrap();
        let wedding = found
            .iter()
            .find(|found| found.dir.ends_with("2014-08-00 Wedding"))
            .unwrap();
        assert_eq!(wedding.name, "Wedding");
    }
}
