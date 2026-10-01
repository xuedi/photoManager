//! The tag tree: one vocabulary instead of four. Each photo whose tags a rule changes, or whose
//! five tag fields do not agree, is written with its whole set: every field, every level, and the
//! label and catalog sets that older writers left emptied.
//!
//! The generated tags - the year, the place, the event - can be made from the data, so they can
//! never disagree with it; they can also be dropped, or left as they are.
//!
//! What the shape of the tree says is off - roots spelled two ways, flat keywords with one home,
//! leaves above the usual depth of their branch - is found here as rules, each a fix.

use std::collections::BTreeSet;

use crate::browse::TagTree;
use crate::cache::{self, Cache, Tagged};
use crate::changeset::Wanted;
use crate::scope::Scope;
use crate::tags::{self, Rule, Rules};
use crate::write::change::expand;
use crate::write::{Change, Field};

pub const TIMELINE: &str = "timeline";
pub const PLACES: &str = "places";
pub const EVENTS: &str = "events";

/// What becomes of the tags that only say what the date, the place words or the folder say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Generated {
    /// Made from the data: `timeline/<year>`, `places/in<Country>/<City>`, `events/<year> <name>`.
    #[default]
    Derived,
    /// Taken away: the date, the position and the folder already say it.
    Dropped,
    /// Left as they are.
    Kept,
}

impl Generated {
    pub const ALL: [Generated; 3] = [Generated::Derived, Generated::Dropped, Generated::Kept];

    pub fn key(self) -> &'static str {
        match self {
            Generated::Derived => "derived",
            Generated::Dropped => "dropped",
            Generated::Kept => "kept",
        }
    }

    pub fn named(key: &str) -> Option<Generated> {
        Generated::ALL.into_iter().find(|generated| generated.key() == key)
    }

    pub fn tells(self) -> &'static str {
        match self {
            Generated::Derived => "Derived From the Data",
            Generated::Dropped => "Dropped",
            Generated::Kept => "Left as They Are",
        }
    }
}

/// What a photo's tags become: mapped by the rules, a bare root dropped, and the generated tags
/// made, dropped or kept.
fn tags_of(photo: &Tagged, rules: &Rules, generated: Generated, tree: &TagTree) -> Vec<String> {
    let mapped = tags::without_bare_roots(rules.map(&photo.tags), tree);
    let then = match generated {
        Generated::Kept => mapped,
        Generated::Dropped => mapped
            .into_iter()
            .filter(|path| ![TIMELINE, PLACES, EVENTS].iter().any(|root| tags::within(path, root)))
            .collect(),
        Generated::Derived => derived(mapped, photo),
    };
    tags::deepest(&then)
}

fn write(photo: Tagged, then: Vec<String>) -> Wanted {
    Wanted::new(
        photo.rel_path,
        Change::of([Field::Tags(then), Field::DropLabel, Field::DropCatalogSets]),
    )
}

/// Every photo of the scope written the same in every tag field, with the generated tags made,
/// dropped or kept. A tidy photo that would say the same is left out.
pub fn tidied(cache: &Cache, scope: &Scope, generated: Generated) -> cache::Result<Vec<Wanted>> {
    let rules = Rules::default();
    let tree = tags::mapped_tree(&rules, &cache.tag_sets()?);
    let mut wanted = Vec::new();
    for photo in cache.tagged(&scope.paths(cache)?)? {
        let then = tags_of(&photo, &rules, generated, &tree);
        if !photo.untidy && levels(&then) == levels(&photo.tags) {
            continue;
        }
        wanted.push(write(photo, then));
    }
    Ok(wanted)
}

/// The photos of the scope whose tags the rules change, each written with its whole set; with
/// `untidy`, also the ones whose tag fields do not agree. Nothing else about the tags changes.
pub fn renamed(cache: &Cache, scope: &Scope, rules: &Rules, untidy: bool) -> cache::Result<Vec<Wanted>> {
    let tree = tags::mapped_tree(rules, &cache.tag_sets()?);
    let mut wanted = Vec::new();
    for photo in cache.tagged(&scope.paths(cache)?)? {
        let then = tags_of(&photo, rules, Generated::Kept, &tree);
        let changed = levels(&rules.map(&photo.tags)) != levels(&photo.tags);
        if changed || (untidy && photo.untidy) {
            wanted.push(write(photo, then));
        }
    }
    Ok(wanted)
}

/// Each generated root replaced by what the data says, where it says anything: the year of the
/// date, the city and country of the place words, the event the photo's own field names, else its
/// folder's. A photo the data says nothing about keeps what it has.
fn derived(tags: Vec<String>, photo: &Tagged) -> Vec<String> {
    let mut made: Vec<(&str, String)> = Vec::new();
    if let Some(year) = photo.taken_at.as_deref().and_then(|at| at.get(..4)) {
        made.push((TIMELINE, format!("{TIMELINE}/{year}")));
    }
    let country = photo.country_named.as_ref().or(photo.country.as_ref());
    if photo.city.is_some() || photo.country_named.is_some() {
        let country = country.map(|country| format!("in{}", level(country).replace(' ', "")));
        let place = [Some(PLACES.to_string()), country, photo.city.as_deref().map(level)]
            .into_iter()
            .flatten()
            .collect::<Vec<String>>();
        if place.len() > 1 {
            made.push((PLACES, place.join("/")));
        }
    }
    let event = match photo.event_dir {
        Some(_) => photo.event_field.as_deref().or(photo.event_name.as_deref()),
        None => photo.event_field.as_deref(),
    };
    if let Some(name) = event.map(level).filter(|name| !name.is_empty()) {
        made.push((
            EVENTS,
            match photo.event_year.filter(|year| *year > 0) {
                Some(year) => format!("{EVENTS}/{year} {name}"),
                None => format!("{EVENTS}/{name}"),
            },
        ));
    }
    let mut then: Vec<String> = tags
        .into_iter()
        .filter(|path| !made.iter().any(|(root, _)| tags::within(path, root)))
        .collect();
    then.extend(made.into_iter().map(|(_, path)| path));
    then
}

/// A name as one level of a tag: nothing that separates levels.
fn level(name: &str) -> String {
    name.trim().replace(['/', '|'], "-")
}

/// Every level of a set of paths, to compare two sets by what a write makes of them.
fn levels(paths: &[String]) -> BTreeSet<String> {
    expand(paths).unwrap_or_else(|_| paths.to_vec()).into_iter().collect()
}

/// One rule the shape of the tree asks for, and how many photos carry the tag it moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub rule: Rule,
    /// Why: `People and people are one root`.
    pub why: String,
    pub photos: usize,
}

/// What the shape of the tree says is off, as rules in the order they are to be applied: the
/// roots spelled two ways first, then the flat keywords into their one branch, then the leaves
/// above their branch's usual depth. What is not sure - a keyword two tags have the name of, a
/// root that stands apart - is not found; it is renamed by hand.
pub fn shape(cache: &Cache) -> cache::Result<Vec<Found>> {
    let photos = cache.tag_sets()?;
    let tree = tags::mapped_tree(&Rules::default(), &photos);
    let carrying = |path: &str| {
        photos
            .iter()
            .filter(|tags| tags.iter().any(|tag| tags::within(tag, path)))
            .count()
    };
    let mut found = Vec::new();
    let mut rules = Rules::default();
    let mut add = |rule: Result<Rule, String>, why: String| {
        let Ok(rule) = rule else { return };
        if rules.add(rule.clone()).is_ok() {
            found.push(Found {
                photos: carrying(rule.from()),
                rule,
                why,
            });
        }
    };
    for (spellings, into) in tags::twin_roots(&tree) {
        for from in spellings.iter().filter(|spelling| **spelling != into) {
            add(
                Rule::rename(from, &into),
                format!("{} are one root", spellings.join(" and ")),
            );
        }
    }
    for (bare, path) in tags::flat(&tree).into {
        add(
            Rule::rename(&bare, &path),
            "A flat keyword, and exactly one tag of the tree has its name".to_string(),
        );
    }
    let branches = tags::branches(&tree);
    for (path, to) in tags::misplaced(&tree, &branches) {
        add(
            Rule::rename(&path, &to),
            "Higher than the rest of its branch, where one tag has its name".to_string(),
        );
    }
    Ok(found)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn photo(tags: &[&str]) -> Tagged {
        Tagged {
            rel_path: "Germany/2019-07-13 Sommerfest/a.jpg".to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..Tagged::default()
        }
    }

    #[test]
    fn the_generated_tags_come_from_the_data() {
        let tagged = Tagged {
            taken_at: Some("2019-07-13 18:20:00".to_string()),
            city: Some("Hamburg".to_string()),
            country: Some("Germany".to_string()),
            event_dir: Some("Germany/2019-07-13 Sommerfest".to_string()),
            event_year: Some(2019),
            event_name: Some("Sommerfest".to_string()),
            ..photo(&[
                "timeline/2018",
                "places/inNetherland/Amsterdam",
                "people/Anna",
                "events",
            ])
        };
        let tree = TagTree::of(&["events/x".to_string()]);
        let derived = tags_of(&tagged, &Rules::default(), Generated::Derived, &tree);
        assert_eq!(
            derived,
            [
                "events/2019 Sommerfest",
                "people/Anna",
                "places/inGermany/Hamburg",
                "timeline/2019"
            ]
        );
        assert_eq!(
            tags_of(&tagged, &Rules::default(), Generated::Dropped, &tree),
            ["people/Anna"]
        );

        let bare = Tagged {
            country: Some("China".to_string()),
            ..photo(&["places/inChina/Beijing"])
        };
        assert_eq!(
            tags_of(&bare, &Rules::default(), Generated::Derived, &tree),
            ["places/inChina/Beijing"],
            "without a date, place words or an event there is nothing to derive"
        );

        let renamed = Tagged {
            event_field: Some("Summer Party".to_string()),
            ..tagged.clone()
        };
        assert!(
            tags_of(&renamed, &Rules::default(), Generated::Derived, &tree)
                .contains(&"events/2019 Summer Party".to_string()),
            "the event the photo names comes before its folder's"
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::changeset::{ChangeSet, Verdict};
    use crate::filter::Filter;
    use crate::tools::testing::Library;

    const KIRA: &str = "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG";
    const LABELLED: &str = "Germany/2019-07-13 Sommerfest/p1000003.jpg";
    const CATALOGUED: &str = "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG";
    const BARE_ROOT: &str = "China/2012-04-00 Rail Trip/IMG_5003.JPG";
    const APART: &str = "Germany/2019-07-13 Sommerfest/IMAG0001.jpg";
    const UNTAGGED: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";

    fn kept(rules: &[&str]) -> Rules {
        let mut kept = Rules::default();
        for rule in rules {
            kept.add(Rule::read(rule).unwrap()).unwrap();
        }
        kept
    }

    fn built(library: &Library, wanted: cache::Result<Vec<Wanted>>) -> ChangeSet {
        ChangeSet::build(&library.cache, "", &wanted.unwrap()).unwrap()
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn tags_in(set: &ChangeSet, rel_path: &str) -> Vec<String> {
        let row = set.rows.iter().find(|row| row.rel_path == rel_path).expect("a row");
        match &row.change.fields[0] {
            Field::Tags(paths) => paths.clone(),
            other => panic!("{other:?}"),
        }
    }

    /// Every tag field of a photo, the label and the catalog sets, as ExifTool reads them.
    fn fields(library: &Library, rel_path: &str) -> Vec<(String, String)> {
        let out = std::process::Command::new("exiftool")
            .args([
                "-j",
                "-G1",
                "-XMP-digiKam:TagsList",
                "-XMP-lr:HierarchicalSubject",
                "-XMP-microsoft:LastKeywordXMP",
                "-XMP-dc:Subject",
                "-IPTC:Keywords",
                "-XMP-xmp:Label",
                "-XMP-mediapro:CatalogSets",
            ])
            .arg(library.root.join(rel_path))
            .output()
            .unwrap();
        let read: Value = serde_json::from_slice(&out.stdout).unwrap();
        read[0]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| *key != "SourceFile")
            .map(|(key, value)| {
                let text = match value {
                    Value::Array(items) => {
                        let mut all: Vec<String> = items
                            .iter()
                            .map(|item| item.as_str().map(String::from).unwrap_or(item.to_string()))
                            .collect();
                        all.sort();
                        all.join(", ")
                    }
                    other => other.as_str().map(String::from).unwrap_or(other.to_string()),
                };
                (key.clone(), text)
            })
            .collect()
    }

    #[test]
    fn a_tidy_photo_the_rules_do_not_touch_is_left_out_and_an_untidy_one_kept_as_it_is() {
        let library = Library::new("tags-tidy");
        let set = built(&library, tidied(&library.cache, &whole(), Generated::Kept));
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert!(!rows.contains(&UNTAGGED), "no tag and no leftovers is tidy");
        assert!(rows.contains(&LABELLED), "a keyword in the label is untidy");
        assert!(rows.contains(&CATALOGUED), "so is a catalog set");
        assert_eq!(
            tags_in(&set, KIRA),
            ["People/Kira", "mixed/disgusting", "places/inIreland/Galway"],
            "without rules an untidy photo keeps its tags"
        );
        assert_eq!(
            tags_in(&set, BARE_ROOT),
            ["places/inChina"],
            "a bare root goes when the tree has tags below it"
        );
        assert_eq!(
            tags_in(&set, APART),
            [
                "people/family/Anna",
                "people/family/Tom",
                "people/me",
                "places/inGermany"
            ],
            "a tag written to one field only is kept"
        );
        assert!(set.rows.iter().all(|row| row.verdict == Verdict::Change));
    }

    #[test]
    fn a_rule_is_applied_to_every_photo_it_touches() {
        let library = Library::new("tags-rules");
        let rules = kept(&[
            "rename People -> people",
            "rename Apartmens -> apartments",
            "delete mixed/funny",
        ]);
        let set = built(&library, renamed(&library.cache, &whole(), &rules, false));
        assert_eq!(
            tags_in(&set, KIRA),
            ["mixed/disgusting", "people/Kira", "places/inIreland/Galway"]
        );
        assert_eq!(
            tags_in(&set, "Germany/Hamburg/2014-08-00 Wedding/IMG_2001.JPG"),
            ["apartments/Harbour Flat", "places/inGermany"]
        );
        assert_eq!(
            tags_in(&set, "China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG"),
            ["mixed/food", "places/inChina"]
        );
        assert!(
            set.rows.iter().all(|row| row.rel_path != LABELLED),
            "a photo the rules do not touch is left out"
        );
    }

    #[test]
    fn written_every_field_agrees() {
        let mut library = Library::new("tags-write");
        let rules = kept(&["rename People -> people"]);
        let scope = Scope::Photos {
            title: "three".to_string(),
            paths: vec![KIRA.to_string(), LABELLED.to_string(), CATALOGUED.to_string()],
        };
        let set = built(&library, renamed(&library.cache, &scope, &rules, true));
        assert_eq!(set.counts().change, 3);
        let summary = library.apply(&set);
        assert_eq!(summary.written, 3, "{summary:?}");

        let kira: std::collections::BTreeMap<String, String> = fields(&library, KIRA).into_iter().collect();
        let every = "mixed, mixed/disgusting, people, people/Kira, places, places/inIreland, places/inIreland/Galway";
        assert_eq!(kira["XMP-digiKam:TagsList"], every);
        assert_eq!(kira["XMP-microsoft:LastKeywordXMP"], every);
        assert_eq!(
            kira["XMP-lr:HierarchicalSubject"],
            "mixed, mixed|disgusting, people, people|Kira, places, places|inIreland, places|inIreland|Galway"
        );
        let names = "Galway, Kira, disgusting, inIreland, mixed, people, places";
        assert_eq!(kira["XMP-dc:Subject"], names);
        assert_eq!(kira["IPTC:Keywords"], names);
        let labelled: std::collections::BTreeMap<String, String> = fields(&library, LABELLED).into_iter().collect();
        assert!(!labelled.contains_key("XMP-xmp:Label"), "{labelled:?}");
        let catalogued: std::collections::BTreeMap<String, String> = fields(&library, CATALOGUED).into_iter().collect();
        assert!(!catalogued.contains_key("XMP-mediapro:CatalogSets"), "{catalogued:?}");

        library.rescan();
        let again = built(&library, renamed(&library.cache, &scope, &rules, true));
        assert!(again.is_empty(), "a second run changes nothing: {:?}", again.rows);
    }

    #[test]
    fn derived_tags_follow_the_data_and_a_moved_photo_after_the_next_run() {
        let mut library = Library::new("tags-derived");
        let set = built(&library, tidied(&library.cache, &whole(), Generated::Derived));
        assert_eq!(
            tags_in(&set, UNTAGGED),
            ["events/2018 Wedding Trip to Copenhagen", "timeline/2018"],
            "an untagged photo gets its generated tags for free"
        );
        assert_eq!(
            tags_in(&set, "Ireland/2008-10-03 Galway/IMG_0003.JPG"),
            [
                "events/2008 Galway",
                "mixed/Funny",
                "mixed/disgusting",
                "places/inIreland/Galway",
                "timeline/2008"
            ],
            "the place words win over a places tag that disagrees"
        );

        let from = library.root.join(UNTAGGED);
        let to = library.root.join("Denmark/2017-09-00 Autumn Walk/DSCF0002.JPG");
        std::fs::rename(&from, &to).unwrap();
        library.rescan();
        let moved = built(&library, tidied(&library.cache, &whole(), Generated::Derived));
        assert_eq!(
            tags_in(&moved, "Denmark/2017-09-00 Autumn Walk/DSCF0002.JPG"),
            ["events/2017 Autumn Walk", "timeline/2018"]
        );
    }

    #[test]
    fn the_shape_of_the_tree_finds_where_flat_keywords_go_and_the_write_puts_them_there() {
        const FLAT: &str = "Germany/2014-03-22 Museum/IMG_9003.JPG";
        let mut library = Library::new("tags-pattern");
        let found = shape(&library.cache).unwrap();
        let rules: Vec<String> = found.iter().map(|one| one.rule.written()).collect();
        assert_eq!(
            rules,
            [
                "rename People -> people",
                "rename Funny -> mixed/funny",
                "rename inChina -> places/inChina"
            ],
            "the twin roots first, and a name spelled two ways is one tag once they merge"
        );
        assert_eq!(found[0].why, "People and people are one root");
        assert_eq!(found[1].photos, 1);

        let flat = kept(&["rename Funny -> mixed/funny", "rename inChina -> places/inChina"]);
        let set = built(&library, renamed(&library.cache, &whole(), &flat, false));
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rows, [FLAT], "the rules change the one photo with those keywords");
        let tags = tags_in(&set, FLAT);
        assert!(tags.contains(&"places/inChina".to_string()), "{tags:?}");
        assert!(tags.contains(&"mixed/funny".to_string()), "{tags:?}");

        assert_eq!(library.apply(&set).written, 1);
        library.rescan();
        let said = library.cache.stated(&[FLAT.to_string()]).unwrap()[FLAT]
            .said
            .tags
            .clone();
        assert!(!said.contains(&"inChina".to_string()), "{said:?}");
        let again: Vec<String> = shape(&library.cache)
            .unwrap()
            .iter()
            .map(|one| one.rule.written())
            .collect();
        assert_eq!(again, ["rename People -> people"], "nothing flat is left to move");
    }
}
