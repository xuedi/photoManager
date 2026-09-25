//! Tag Vocabulary: one vocabulary instead of four. The person's decisions are [`Rules`] over the
//! tag paths, kept in the tool's settings and applied to every later run, and each photo whose
//! tags they change, or whose five tag fields do not agree, is written with its whole mapped set:
//! every field, every level, and the label and catalog sets that older writers left emptied.
//!
//! The generated tags - the year, the place, the event - are made from the data by default, so
//! they can never disagree with it; they can also be dropped, or left as the rules make them.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::{Page, Settings, Tool};
use crate::browse::TagTree;
use crate::cache::{self, Cache, Tagged};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::tags::{self, Rule, Rules, Suggestion};
use crate::write::change::expand;
use crate::write::{Change, Field};

pub struct TagVocabulary;

pub const TIMELINE: &str = "timeline";
pub const PLACES: &str = "places";
pub const EVENTS: &str = "events";

/// What becomes of the tags that only say what the date, the place words or the folder say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Generated {
    /// Made from the data with every write: `timeline/<year>`, `places/in<Country>/<City>`,
    /// `events/<year> <name>`.
    #[default]
    Derived,
    /// Taken away: the date, the position and the folder already say it.
    Dropped,
    /// Left as the rules make them.
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

/// The tool's settings: the rules in order, the suggestions left alone, and the generated tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vocabulary {
    pub rules: Rules,
    pub left: BTreeSet<String>,
    pub generated: Generated,
}

impl Settings for Vocabulary {
    fn read(text: &str) -> Result<Vocabulary, String> {
        if text.trim().is_empty() {
            return Ok(Vocabulary::default());
        }
        let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) else {
            return Err(format!("{text} is not a tag vocabulary"));
        };
        let texts = |name: &str| -> Result<Vec<String>, String> {
            match fields.get(name) {
                None => Ok(Vec::new()),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(String::from).ok_or(format!("{item} is not text")))
                    .collect(),
                Some(other) => Err(format!("{other} is not a list")),
            }
        };
        let mut vocabulary = Vocabulary::default();
        for written in texts("rules")? {
            vocabulary.rules.add(Rule::read(&written)?)?;
        }
        vocabulary.left = texts("left")?.into_iter().collect();
        if let Some(generated) = fields.get("generated") {
            let key = generated.as_str().unwrap_or_default();
            vocabulary.generated =
                Generated::named(key).ok_or_else(|| format!("{generated} is not a way with generated tags"))?;
        }
        Ok(vocabulary)
    }

    fn written(&self) -> String {
        let mut fields = Map::new();
        fields.insert(
            "rules".to_string(),
            Value::from(self.rules.0.iter().map(Rule::written).collect::<Vec<String>>()),
        );
        fields.insert(
            "left".to_string(),
            Value::from(self.left.iter().cloned().collect::<Vec<String>>()),
        );
        fields.insert("generated".to_string(), Value::from(self.generated.key()));
        Value::Object(fields).to_string()
    }
}

impl Vocabulary {
    /// The tree after the rules, from every photo's tags.
    pub fn tree(&self, photos: &[Vec<String>]) -> TagTree {
        tags::mapped_tree(&self.rules, photos)
    }

    /// What a photo's tags become: mapped by the rules, a bare root dropped, and the generated
    /// tags made, dropped or kept.
    pub fn tags_of(&self, photo: &Tagged, tree: &TagTree) -> Vec<String> {
        let mapped = tags::without_bare_roots(self.rules.map(&photo.tags), tree);
        let then = match self.generated {
            Generated::Kept => mapped,
            Generated::Dropped => mapped
                .into_iter()
                .filter(|path| ![TIMELINE, PLACES, EVENTS].iter().any(|root| tags::within(path, root)))
                .collect(),
            Generated::Derived => derived(mapped, photo),
        };
        tags::deepest(&then)
    }

    /// Confirm on a suggestion: its rules, after the ones there are.
    pub fn confirm(&mut self, suggestion: &Suggestion) -> Result<(), String> {
        let mut rules = self.rules.clone();
        for rule in &suggestion.rules {
            rules.add(rule.clone())?;
        }
        self.rules = rules;
        Ok(())
    }
}

/// Each generated root replaced by what the data says, where it says anything: the year of the
/// date, the city and country of the place words, the event of the folder. A photo the data says
/// nothing about keeps what it has.
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
    if photo.event_dir.is_some()
        && let Some(name) = photo.event_name.as_deref().map(level).filter(|name| !name.is_empty())
    {
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

/// What the tag page shows: the tree after the rules, how many photos each rule changes, and
/// what is still worth suggesting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overview {
    pub tree: TagTree,
    pub rules: Vec<(Rule, usize)>,
    pub suggestions: Vec<Suggestion>,
}

pub fn overview(cache: &Cache, vocabulary: &Vocabulary) -> cache::Result<Overview> {
    let photos = cache.tag_sets()?;
    let tree = vocabulary.tree(&photos);
    Ok(Overview {
        rules: (0..vocabulary.rules.0.len())
            .map(|index| {
                (
                    vocabulary.rules.0[index].clone(),
                    vocabulary.rules.touched(index, &photos),
                )
            })
            .collect(),
        suggestions: tags::suggestions(&tree, &vocabulary.rules, &vocabulary.left),
        tree,
    })
}

impl Tool for TagVocabulary {
    type Settings = Vocabulary;

    fn key(&self) -> &'static str {
        "tag-vocabulary"
    }

    fn title(&self) -> &'static str {
        "Tag Vocabulary"
    }

    fn fixes(&self) -> &'static str {
        "Merges, renames and moves tags once, and writes every tag field the same"
    }

    fn named(&self, vocabulary: &Vocabulary) -> String {
        match vocabulary.rules.0.len() {
            0 => "Tidy the tags".to_string(),
            1 => "Tidy the tags with 1 rule".to_string(),
            count => format!("Tidy the tags with {count} rules"),
        }
    }

    fn page(&self) -> Option<Page> {
        Some(Page::Vocabulary)
    }

    fn wanted(
        &self,
        cache: &Cache,
        _geo: Option<&Geo>,
        scope: &Scope,
        vocabulary: &Vocabulary,
    ) -> cache::Result<Vec<Wanted>> {
        let tree = vocabulary.tree(&cache.tag_sets()?);
        let mut wanted = Vec::new();
        for photo in cache.tagged(&scope.paths(cache)?)? {
            let then = vocabulary.tags_of(&photo, &tree);
            if !photo.untidy && levels(&then) == levels(&photo.tags) {
                continue;
            }
            wanted.push(Wanted::new(
                photo.rel_path,
                Change::of([Field::Tags(then), Field::DropLabel, Field::DropCatalogSets]),
            ));
        }
        Ok(wanted)
    }
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
    fn settings_round_trip_and_a_bad_rule_is_refused() {
        let mut vocabulary = Vocabulary {
            generated: Generated::Kept,
            ..Vocabulary::default()
        };
        vocabulary
            .rules
            .add(Rule::read("rename People -> people").unwrap())
            .unwrap();
        vocabulary.left.insert("mixed:food".to_string());
        assert_eq!(Vocabulary::read(&vocabulary.written()), Ok(vocabulary));
        assert_eq!(Vocabulary::read(""), Ok(Vocabulary::default()));
        assert!(Vocabulary::read(r#"{"rules": ["rename a -> b", "rename b -> a"]}"#).is_err());
        assert!(Vocabulary::read(r#"{"generated": "sometimes"}"#).is_err());
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
        let derived = Vocabulary::default().tags_of(&tagged, &tree);
        assert_eq!(
            derived,
            [
                "events/2019 Sommerfest",
                "people/Anna",
                "places/inGermany/Hamburg",
                "timeline/2019"
            ]
        );
        let dropped = Vocabulary {
            generated: Generated::Dropped,
            ..Vocabulary::default()
        };
        assert_eq!(dropped.tags_of(&tagged, &tree), ["people/Anna"]);

        let bare = Tagged {
            country: Some("China".to_string()),
            ..photo(&["places/inChina/Beijing"])
        };
        assert_eq!(
            Vocabulary::default().tags_of(&bare, &tree),
            ["places/inChina/Beijing"],
            "without a date, place words or an event there is nothing to derive"
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::testing::Library;
    use crate::tools::{self, AnyTool};

    const KIRA: &str = "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG";
    const LABELLED: &str = "Germany/2019-07-13 Sommerfest/p1000003.jpg";
    const CATALOGUED: &str = "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG";
    const BARE_ROOT: &str = "China/2012-04-00 Rail Trip/IMG_5003.JPG";
    const APART: &str = "Germany/2019-07-13 Sommerfest/IMAG0001.jpg";
    const UNTAGGED: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";

    fn tool() -> &'static dyn AnyTool {
        tools::find("tag-vocabulary").expect("the tool is listed")
    }

    fn kept(rules: &[&str]) -> String {
        let mut vocabulary = Vocabulary {
            generated: Generated::Kept,
            ..Vocabulary::default()
        };
        for rule in rules {
            vocabulary.rules.add(Rule::read(rule).unwrap()).unwrap();
        }
        vocabulary.written()
    }

    fn whole() -> Scope {
        Scope::Filter(Filter::all())
    }

    fn tags_in(set: &crate::changeset::ChangeSet, rel_path: &str) -> Vec<String> {
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
        let set = tool()
            .change_set(&library.cache, None, &whole(), Some(&kept(&[])))
            .unwrap();
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
        assert_eq!(set.title, "Tidy the tags");
    }

    #[test]
    fn a_rule_is_applied_to_every_photo_it_touches() {
        let library = Library::new("tags-rules");
        let rules = kept(&[
            "rename People -> people",
            "rename Apartmens -> apartments",
            "delete mixed/funny",
        ]);
        let set = tool().change_set(&library.cache, None, &whole(), Some(&rules)).unwrap();
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
        assert_eq!(set.title, "Tidy the tags with 3 rules");
    }

    #[test]
    fn written_every_field_agrees_and_the_undo_puts_them_back_exactly() {
        let mut library = Library::new("tags-write");
        let rules = kept(&["rename People -> people"]);
        let scope = Scope::Photos {
            title: "three".to_string(),
            paths: vec![KIRA.to_string(), LABELLED.to_string(), CATALOGUED.to_string()],
        };
        let before: Vec<Vec<(String, String)>> = [KIRA, LABELLED, CATALOGUED]
            .iter()
            .map(|path| fields(&library, path))
            .collect();
        let set = tool().change_set(&library.cache, None, &scope, Some(&rules)).unwrap();
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
        let again = tool().change_set(&library.cache, None, &scope, Some(&rules)).unwrap();
        assert!(again.is_empty(), "a second run changes nothing: {:?}", again.rows);

        let undone = library.undo();
        assert_eq!(undone.written, 3, "{undone:?}");
        let after: Vec<Vec<(String, String)>> = [KIRA, LABELLED, CATALOGUED]
            .iter()
            .map(|path| fields(&library, path))
            .collect();
        assert_eq!(after, before, "every field is back exactly");
    }

    #[test]
    fn derived_tags_follow_the_data_and_a_moved_photo_after_the_next_run() {
        let mut library = Library::new("tags-derived");
        let set = tool().change_set(&library.cache, None, &whole(), None).unwrap();
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
        let moved = tool().change_set(&library.cache, None, &whole(), None).unwrap();
        assert_eq!(
            tags_in(&moved, "Denmark/2017-09-00 Autumn Walk/DSCF0002.JPG"),
            ["events/2017 Autumn Walk", "timeline/2018"]
        );
    }

    #[test]
    fn the_overview_counts_the_tree_after_the_rules_and_suggests_the_rest() {
        let library = Library::new("tags-overview");
        let plain = Vocabulary::read(&kept(&[])).unwrap();
        let seen = overview(&library.cache, &plain).unwrap();
        assert_eq!(seen.tree.count("People"), Some(1));
        assert_eq!(seen.tree.count("people"), Some(2));
        let offers: Vec<(&str, &str)> = seen
            .suggestions
            .iter()
            .map(|suggestion| (suggestion.key.as_str(), suggestion.offer.as_str()))
            .collect();
        assert!(offers.contains(&("twin:people", "Merge Into people")), "{offers:?}");
        assert!(
            offers.contains(&("alike:mixed/discusting|mixed/disgusting", "Merge Into mixed/disgusting")),
            "{offers:?}"
        );
        assert!(offers.contains(&("mixed:food", "Move to topics/food")), "{offers:?}");

        let mut merged = plain.clone();
        let twin = seen.suggestions.iter().find(|one| one.key == "twin:people").unwrap();
        merged.confirm(twin).unwrap();
        let after = overview(&library.cache, &merged).unwrap();
        assert_eq!(after.tree.count("People"), None);
        assert_eq!(after.tree.count("people"), Some(3), "the merged node sums its photos");
        assert_eq!(after.rules, [(Rule::read("rename People -> people").unwrap(), 1)]);
        assert!(after.suggestions.iter().all(|one| one.key != "twin:people"));

        merged.left.insert("mixed:food".to_string());
        let left = overview(&library.cache, &merged).unwrap();
        assert!(
            left.suggestions.iter().all(|one| one.key != "mixed:food"),
            "left alone stays left"
        );
    }
}
