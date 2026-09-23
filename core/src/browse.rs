//! What the gallery browses by: the countries and their events, and the tag tree, each with
//! how many photos it holds. Counts are of the whole library, so they do not move while the
//! other controls are changed.

use std::collections::{BTreeMap, BTreeSet};

use crate::cache::{Cache, Result};

/// A country, or an event inside one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    /// The folder inside the library, which is what a filter is narrowed to.
    pub folder: String,
    pub photos: i64,
    pub events: Vec<Place>,
}

/// The countries in name order, each with its events. A country's count includes its loose
/// photos, which belong to no event.
pub fn places(cache: &Cache) -> Result<Vec<Place>> {
    let mut statement = cache.connection().prepare(
        "SELECT country, event_dir, count(*) FROM photo WHERE country IS NOT NULL
         GROUP BY country, event_dir ORDER BY country, event_dir",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut countries: Vec<Place> = Vec::new();
    for row in rows {
        let (country, event_dir, photos) = row?;
        if countries.last().is_none_or(|last| last.folder != country) {
            countries.push(Place {
                name: country.clone(),
                folder: country.clone(),
                photos: 0,
                events: Vec::new(),
            });
        }
        let place = countries.last_mut().expect("just pushed");
        place.photos += photos;
        if let Some(folder) = event_dir {
            let name = folder.strip_prefix(&format!("{country}/")).unwrap_or(&folder);
            place.events.push(Place {
                name: name.to_string(),
                folder: folder.clone(),
                photos,
                events: Vec::new(),
            });
        }
    }
    Ok(countries)
}

/// A tag and everything below it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub path: String,
    /// The last part of the path.
    pub name: String,
    /// Photos carrying this tag or one below it.
    pub photos: i64,
    pub children: Vec<Tag>,
}

/// `a/b/c` is `a`, `a/b` and `a/b/c`.
fn levels(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(|(at, _)| &path[..at])
        .chain(std::iter::once(path))
}

/// Every tag path and every level above it, with how many photos carry it or a tag below it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagTree {
    nodes: BTreeMap<String, i64>,
}

impl TagTree {
    /// The paths alone, without counting anything.
    pub fn of(paths: &[String]) -> TagTree {
        let mut nodes = BTreeMap::new();
        for path in paths {
            for level in levels(path) {
                nodes.entry(level.to_string()).or_insert(0);
            }
        }
        TagTree { nodes }
    }

    /// The tags of the library, each counted once per photo however many tags below it the
    /// photo carries.
    pub fn take(cache: &Cache) -> Result<TagTree> {
        let mut statement = cache
            .connection()
            .prepare("SELECT photo_id, path FROM tag ORDER BY photo_id")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        let mut nodes: BTreeMap<String, i64> = BTreeMap::new();
        let mut photo = None;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut settle = |seen: &mut BTreeSet<String>| {
            for level in std::mem::take(seen) {
                *nodes.entry(level).or_default() += 1;
            }
        };
        for row in rows {
            let (id, path) = row?;
            if photo != Some(id) {
                settle(&mut seen);
                photo = Some(id);
            }
            seen.extend(levels(&path).map(str::to_string));
        }
        settle(&mut seen);
        Ok(TagTree { nodes })
    }

    pub fn count(&self, path: &str) -> Option<i64> {
        self.nodes.get(path).copied()
    }

    /// The tags as a tree, the roots first, each level in the order of its spelling.
    pub fn tree(&self) -> Vec<Tag> {
        self.below("")
    }

    fn below(&self, parent: &str) -> Vec<Tag> {
        self.children(parent)
            .into_iter()
            .map(|name| {
                let path = match parent {
                    "" => name.to_string(),
                    _ => format!("{parent}/{name}"),
                };
                Tag {
                    name: name.to_string(),
                    photos: self.nodes[&path],
                    children: self.below(&path),
                    path,
                }
            })
            .collect()
    }

    pub(crate) fn children(&self, parent: &str) -> Vec<&str> {
        self.nodes
            .keys()
            .filter_map(|node| match parent {
                "" => Some(node.as_str()),
                _ => node.strip_prefix(parent)?.strip_prefix('/'),
            })
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .collect()
    }

    pub(crate) fn siblings(&self) -> BTreeMap<&str, Vec<&str>> {
        let mut by_parent: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in self.nodes.keys() {
            let (parent, leaf) = node.rsplit_once('/').unwrap_or(("", node));
            by_parent.entry(parent).or_default().push(leaf);
        }
        by_parent
    }

    /// Paths that are the same but for case, highest level only: `People` and `people` are one
    /// finding, not one more for every tag below them.
    pub fn case_twins(&self) -> Vec<Vec<String>> {
        let mut by_lower: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for node in self.nodes.keys() {
            by_lower.entry(node.to_lowercase()).or_default().push(node.clone());
        }
        by_lower
            .iter()
            .filter(|(lower, spellings)| {
                let parent_is_twin = lower
                    .rsplit_once('/')
                    .and_then(|(parent, _)| by_lower.get(parent))
                    .is_some_and(|above| above.len() > 1);
                spellings.len() > 1 && !parent_is_twin
            })
            .map(|(_, spellings)| spellings.clone())
            .collect()
    }

    /// Sibling tags a letter or two apart: a hint that one is a typo of the other, never a verdict.
    pub fn look_alikes(&self) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for (parent, leaves) in self.siblings() {
            for (at, one) in leaves.iter().enumerate() {
                for other in &leaves[at + 1..] {
                    if alike(one, other) {
                        let path = |leaf: &str| match parent {
                            "" => leaf.to_string(),
                            _ => format!("{parent}/{leaf}"),
                        };
                        found.push((path(one), path(other)));
                    }
                }
            }
        }
        found
    }
}

/// Long enough to be a word, a letter apart (two in a long one), and not two years of the same
/// thing (`2006 Summer`, `2007 Summer`).
fn alike(one: &str, other: &str) -> bool {
    let (one, other) = (one.to_lowercase(), other.to_lowercase());
    let shorter = one.chars().count().min(other.chars().count());
    if one == other || shorter < 5 {
        return false;
    }
    let letters = |text: &str| text.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    if one.chars().any(|c| c.is_ascii_digit()) && letters(&one) == letters(&other) {
        return false;
    }
    let allowed = if shorter < 8 { 1 } else { 2 };
    distance(&one, &other) <= allowed
}

fn distance(one: &str, other: &str) -> usize {
    let other: Vec<char> = other.chars().collect();
    let mut previous: Vec<usize> = (0..=other.len()).collect();
    for (i, a) in one.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, b) in other.iter().enumerate() {
            let substitute = previous[j] + usize::from(a != *b);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[other.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> TagTree {
        TagTree::of(&paths.iter().map(|path| path.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn twins_are_reported_at_the_highest_level_only() {
        let tags = tree(&["People/Kira", "people/Kira", "people/Ben", "mixed/funny", "mixed/Funny"]);
        assert_eq!(
            tags.case_twins(),
            [
                vec!["mixed/Funny".to_string(), "mixed/funny".to_string()],
                vec!["People".to_string(), "people".to_string()],
            ]
        );
    }

    #[test]
    fn look_alikes_are_siblings_a_letter_apart() {
        let tags = tree(&[
            "mixed/discusting",
            "mixed/disgusting",
            "mixed/funny",
            "mixed/Funny",
            "people/Kira",
            "people/Kiri",
            "people/Ben",
            "events/2006 Summer",
            "events/2007 Summer",
            "places/inNetherland",
            "places/inNetherlands",
            "other/disgusting",
        ]);
        assert_eq!(
            tags.look_alikes(),
            [
                ("mixed/discusting".to_string(), "mixed/disgusting".to_string()),
                ("places/inNetherland".to_string(), "places/inNetherlands".to_string()),
            ],
            "short names, case twins, years and cousins are not look-alikes"
        );
    }

    #[test]
    fn edit_distance() {
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("same", "same"), 0);
    }

    #[test]
    fn a_path_is_every_level_above_it() {
        assert_eq!(levels("a/b/c").collect::<Vec<_>>(), ["a", "a/b", "a/b/c"]);
        assert_eq!(levels("a").collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn the_tree_nests_by_level_and_keeps_spellings_apart() {
        let tags = tree(&["People/Kira", "people/Ben", "a b", "a/c"]);
        let roots = tags.tree();
        let names: Vec<&str> = roots.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["People", "a", "a b", "people"]);
        let a = &roots[1];
        assert_eq!(a.children.len(), 1, "a b is a sibling of a, not its child");
        assert_eq!(a.children[0].path, "a/c");
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod fixture_tests {
    use super::*;
    use crate::filter::tests::scanned;
    use crate::filter::{Filter, Kind};

    #[test]
    fn a_tag_counts_the_photos_below_it_once() {
        let cache = scanned("browse-tags");
        let tags = TagTree::take(&cache).unwrap();
        for (path, count) in tags.nodes.iter() {
            let filter = Filter::of(Kind::Tagged(vec![path.clone()]));
            assert_eq!(filter.count(&cache).unwrap(), *count, "{path}");
        }
        assert_eq!(tags.count("mixed"), Some(4));
        assert_eq!(tags.count("people"), Some(2));
        assert_eq!(tags.count("People"), Some(1), "the other spelling is its own root");
        let roots = tags.tree();
        let roots: Vec<&str> = roots.iter().map(|tag| tag.path.as_str()).collect();
        assert_eq!(roots, ["People", "events", "mixed", "people", "places", "timeline"]);
    }

    #[test]
    fn the_places_add_up_to_the_library() {
        let cache = scanned("browse-places");
        let countries = places(&cache).unwrap();
        let total: i64 = countries.iter().map(|country| country.photos).sum();
        assert_eq!(total, crate::fixtures::photo_count() as i64);
        for country in &countries {
            let filter = Filter::all().within(&country.folder);
            assert_eq!(filter.count(&cache).unwrap(), country.photos, "{}", country.name);
            for event in &country.events {
                let filter = Filter::all().within(&event.folder);
                assert_eq!(filter.count(&cache).unwrap(), event.photos, "{}", event.folder);
            }
        }
        let china = &countries[0];
        assert_eq!((china.name.as_str(), china.photos), ("China", 7));
        assert_eq!(
            china.events.iter().map(|event| event.photos).sum::<i64>(),
            6,
            "the loose one is in no event"
        );
    }
}
