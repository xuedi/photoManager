//! The tag vocabulary as rules a person decides once. `rename A -> B` moves `A` and everything
//! below it to `B`, which is renaming a leaf, moving a branch and merging into a tag that is
//! already there, all at once. `delete A` takes `A` and everything below it away.
//!
//! The rules are applied in order to a photo's deepest tags, and what comes out is its new tag
//! set. A rule that could never do anything, or would take a tag back to where an earlier rule
//! moved it from, is refused when it is entered, with why.
//!
//! What the shape of the tree says is off - roots spelled two ways, flat keywords, leaves above
//! their branch's usual depth - is found here, for the fixes to make rules of.

use std::collections::BTreeSet;

use crate::browse::TagTree;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    Rename { from: String, to: String },
    Delete { path: String },
}

const RENAME: &str = "rename ";
const DELETE: &str = "delete ";
const ARROW: &str = " -> ";

impl Rule {
    /// A rename, its paths checked.
    pub fn rename(from: &str, to: &str) -> Result<Rule, String> {
        let (from, to) = (path(from)?, path(to)?);
        if from == to {
            return Err(format!("{from} is already called that"));
        }
        if within(&to, &from) {
            return Err(format!("{from} cannot move inside itself"));
        }
        Ok(Rule::Rename { from, to })
    }

    pub fn delete(path: &str) -> Result<Rule, String> {
        Ok(Rule::Delete {
            path: self::path(path)?,
        })
    }

    /// `rename A -> B` or `delete A`.
    pub fn read(text: &str) -> Result<Rule, String> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix(RENAME) {
            let (from, to) = rest
                .split_once(ARROW.trim())
                .ok_or_else(|| format!("{text:?} does not say what to rename it to"))?;
            return Rule::rename(from, to);
        }
        if let Some(path) = text.strip_prefix(DELETE) {
            return Rule::delete(path);
        }
        Err(format!("{text:?} is not a rule"))
    }

    pub fn written(&self) -> String {
        match self {
            Rule::Rename { from, to } => format!("{RENAME}{from}{ARROW}{to}"),
            Rule::Delete { path } => format!("{DELETE}{path}"),
        }
    }

    /// The tag the rule is about.
    pub fn from(&self) -> &str {
        match self {
            Rule::Rename { from, .. } => from,
            Rule::Delete { path } => path,
        }
    }

    /// `Rename People to people`, `Delete mixed/wired`.
    pub fn tells(&self) -> String {
        match self {
            Rule::Rename { from, to } => format!("Rename {from} to {to}"),
            Rule::Delete { path } => format!("Delete {path}"),
        }
    }

    /// What the rule makes of one tag path: `None` takes it away.
    pub fn map(&self, path: &str) -> Option<String> {
        match self {
            Rule::Rename { from, to } => Some(match path.strip_prefix(from.as_str()) {
                Some("") => to.clone(),
                Some(rest) if rest.starts_with('/') => format!("{to}{rest}"),
                _ => path.to_string(),
            }),
            Rule::Delete { path: gone } => (!within(path, gone)).then(|| path.to_string()),
        }
    }
}

/// A tag path with every level trimmed, or why it cannot be one.
pub fn path(path: &str) -> Result<String, String> {
    let levels: Vec<&str> = path.trim().split('/').map(str::trim).collect();
    if levels.iter().any(|level| level.is_empty()) {
        return Err(match path.trim().is_empty() {
            true => "a tag needs a name".to_string(),
            false => format!("{:?} has an empty level", path.trim()),
        });
    }
    if let Some(bad) = levels.iter().find(|level| level.contains('|')) {
        return Err(format!("{bad:?} has a | in it, which separates the levels of a tag"));
    }
    Ok(levels.join("/"))
}

/// Whether `path` is `branch` or below it.
pub fn within(path: &str, branch: &str) -> bool {
    path.strip_prefix(branch)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The rules, in the order they are applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules(pub Vec<Rule>);

impl Rules {
    /// One more rule at the end, unless it could never do anything or makes a cycle.
    pub fn add(&mut self, rule: Rule) -> Result<(), String> {
        if let Some(why) = self.refusal(&rule) {
            return Err(why);
        }
        self.0.push(rule);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Option<Rule> {
        (index < self.0.len()).then(|| self.0.remove(index))
    }

    fn refusal(&self, rule: &Rule) -> Option<String> {
        if self.0.contains(rule) {
            return Some(format!("{} is already a rule", rule.tells()));
        }
        for earlier in &self.0 {
            if within(rule.from(), earlier.from()) {
                return Some(match earlier {
                    Rule::Rename { from, to } => format!(
                        "an earlier rule already renames {from} to {to}, so {} is not there any more",
                        rule.from()
                    ),
                    Rule::Delete { path } => format!(
                        "an earlier rule already deletes {path}, so {} is not there any more",
                        rule.from()
                    ),
                });
            }
        }
        let mut all = self.clone();
        all.0.push(rule.clone());
        for moved in all.0.iter().filter(|rule| matches!(rule, Rule::Rename { .. })) {
            let origin = moved.from();
            if all.follow(origin).is_some_and(|end| within(&end, origin)) {
                return Some(format!("{} would take {origin} back to where it was", rule.tells()));
            }
        }
        None
    }

    /// Where one path ends up after every rule.
    pub fn follow(&self, path: &str) -> Option<String> {
        self.0.iter().try_fold(path.to_string(), |path, rule| rule.map(&path))
    }

    /// A photo's tags after the rules: its deepest tags, each followed through every rule.
    pub fn map(&self, tags: &[String]) -> Vec<String> {
        let mapped: Vec<String> = deepest(tags).iter().filter_map(|path| self.follow(path)).collect();
        deepest(&mapped)
    }

    /// How many of these photos' tag sets the rule at `index` changes, after the rules before it.
    pub fn touched(&self, index: usize, photos: &[Vec<String>]) -> usize {
        let before = Rules(self.0[..index].to_vec());
        let rule = &self.0[index];
        photos
            .iter()
            .filter(|tags| {
                before
                    .map(tags)
                    .iter()
                    .any(|path| rule.map(path).as_deref() != Some(path.as_str()))
            })
            .count()
    }
}

/// The paths no other path of the set is below, each once, in order: `places/inChina` says
/// nothing `places/inChina/Beijing` does not.
pub fn deepest(tags: &[String]) -> Vec<String> {
    let all: BTreeSet<&str> = tags
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .collect();
    all.iter()
        .filter(|path| !all.iter().any(|other| other.len() > path.len() && within(other, path)))
        .map(|path| path.to_string())
        .collect()
}

/// A root on its own says nothing when the tree has tags below it: `events` alone is dropped.
pub fn without_bare_roots(tags: Vec<String>, tree: &TagTree) -> Vec<String> {
    tags.into_iter()
        .filter(|path| path.contains('/') || tree.children(path).is_empty())
        .collect()
}

/// The spelling twins are merged into: the one in lower case, or else the one most photos carry.
fn twin_into(tree: &TagTree, spellings: &[String]) -> String {
    let count = |path: &str| tree.count(path).unwrap_or_default();
    let lower = spellings[0].to_lowercase();
    spellings
        .iter()
        .find(|spelling| **spelling == lower)
        .unwrap_or_else(|| {
            spellings
                .iter()
                .max_by(|one, other| count(one).cmp(&count(other)).then(other.cmp(one)))
                .expect("twins are two at least")
        })
        .clone()
}

/// The rules that merge every pair of case twins.
fn twin_rules(tree: &TagTree) -> Rules {
    let mut rules = Rules::default();
    for spellings in tree.case_twins() {
        let into = twin_into(tree, &spellings);
        for from in spellings.iter().filter(|spelling| **spelling != into) {
            if let Ok(rule) = Rule::rename(from, &into) {
                let _ = rules.add(rule);
            }
        }
    }
    rules
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn depth(path: &str) -> usize {
    path.split('/').count()
}

/// A root with tags below it, and how deep its leaves sit most often: `places` 3 for
/// `places/inChina/Beijing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub root: String,
    pub depth: usize,
    pub photos: i64,
}

/// The roots with tags below them, learned from the tree: the shape the tags follow.
pub fn branches(tree: &TagTree) -> Vec<Branch> {
    tree.children("")
        .into_iter()
        .filter(|root| !tree.children(root).is_empty())
        .map(|root| {
            let mut depths: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
            for (path, _) in tree.nodes() {
                if within(path, root) && path != root && tree.children(path).is_empty() {
                    *depths.entry(depth(path)).or_default() += 1;
                }
            }
            let usual = depths
                .iter()
                .max_by(|one, other| one.1.cmp(other.1).then(one.0.cmp(other.0)))
                .map(|(depth, _)| *depth)
                .unwrap_or(2);
            Branch {
                root: root.to_string(),
                depth: usual,
                photos: tree.count(root).unwrap_or_default(),
            }
        })
        .collect()
}

/// Where the flat keywords belong: a tag without a level that is no branch root, and the paths of
/// the tree whose last level has its name, compared case-folded and after the case twins merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flat {
    /// Into the one path that has its name.
    pub into: Vec<(String, String)>,
    /// Two paths or more have its name.
    pub several: Vec<(String, Vec<String>)>,
    /// No path has its name.
    pub nowhere: Vec<String>,
}

pub fn flat(tree: &TagTree) -> Flat {
    let twins = twin_rules(tree);
    let mut found = Flat::default();
    for bare in tree.children("") {
        if !tree.children(bare).is_empty() {
            continue;
        }
        let folded = bare.to_lowercase();
        let mut paths: Vec<String> = tree
            .nodes()
            .filter(|(path, _)| path.contains('/') && leaf(path).to_lowercase() == folded)
            .filter_map(|(path, _)| twins.follow(path))
            .collect();
        paths.sort();
        paths.dedup();
        match paths.len() {
            0 => found.nowhere.push(bare.to_string()),
            1 => found.into.push((bare.to_string(), paths.remove(0))),
            _ => found.several.push((bare.to_string(), paths)),
        }
    }
    found
}

/// Leaves that sit higher than the rest of their branch, each with the one path at the usual
/// depth of the same branch that has its name: `places/Beijing` into `places/inChina/Beijing`.
pub fn misplaced(tree: &TagTree, branches: &[Branch]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for branch in branches {
        let below: Vec<&str> = tree
            .nodes()
            .map(|(path, _)| path)
            .filter(|path| within(path, &branch.root) && *path != branch.root)
            .collect();
        for path in &below {
            if depth(path) >= branch.depth || !tree.children(path).is_empty() {
                continue;
            }
            let folded = leaf(path).to_lowercase();
            let usual: Vec<&&str> = below
                .iter()
                .filter(|other| depth(other) == branch.depth && leaf(other).to_lowercase() == folded)
                .collect();
            if let [only] = usual.as_slice() {
                found.push((path.to_string(), only.to_string()));
            }
        }
    }
    found
}

/// The roots spelled two ways, each with the spelling they merge into.
pub fn twin_roots(tree: &TagTree) -> Vec<(Vec<String>, String)> {
    tree.case_twins()
        .into_iter()
        .filter(|spellings| !spellings[0].contains('/'))
        .map(|spellings| {
            let into = twin_into(tree, &spellings);
            (spellings, into)
        })
        .collect()
}

/// The tree after the rules, counted from every photo's tags.
pub fn mapped_tree(rules: &Rules, photos: &[Vec<String>]) -> TagTree {
    TagTree::counted(photos.iter().map(|tags| rules.map(tags)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    fn rules(written: &[&str]) -> Rules {
        let mut rules = Rules::default();
        for text in written {
            rules.add(Rule::read(text).unwrap()).unwrap();
        }
        rules
    }

    #[test]
    fn a_rename_moves_a_branch_and_merges_into_one_that_is_there() {
        let merged = rules(&["rename People -> people"]);
        assert_eq!(
            merged.map(&tags(&["People", "People/Kira", "people/Ben", "places/inIreland"])),
            tags(&["people/Ben", "people/Kira", "places/inIreland"])
        );
        let moved = rules(&["rename mixed/food -> topics/food"]);
        assert_eq!(
            moved.map(&tags(&["mixed/food", "mixed/funny"])),
            tags(&["mixed/funny", "topics/food"])
        );
        assert_eq!(
            rules(&["rename mixed -> topics"]).map(&tags(&["mixedup/one"])),
            tags(&["mixedup/one"]),
            "a name that only starts the same is another tag"
        );
    }

    #[test]
    fn a_delete_takes_the_branch() {
        let gone = rules(&["delete mixed"]);
        assert_eq!(
            gone.map(&tags(&["mixed/food", "mixed", "people/Ben"])),
            tags(&["people/Ben"])
        );
        assert_eq!(gone.map(&tags(&["mixed/food"])), Vec::<String>::new());
    }

    #[test]
    fn rules_compose_in_order() {
        let chained = rules(&[
            "rename Apartmens -> apartments",
            "rename apartments/Harbour Flat -> places/inGermany/Harbour Flat",
        ]);
        assert_eq!(
            chained.map(&tags(&["Apartmens/Harbour Flat", "Apartmens/Other"])),
            tags(&["apartments/Other", "places/inGermany/Harbour Flat"])
        );
        assert_eq!(
            chained.touched(0, &[tags(&["Apartmens/Harbour Flat"]), tags(&["people/Ben"])]),
            1
        );
        assert_eq!(
            chained.touched(1, &[tags(&["Apartmens/Harbour Flat"]), tags(&["Apartmens/Other"])]),
            1
        );
    }

    #[test]
    fn a_cycle_is_refused() {
        let mut two = rules(&["rename a -> b"]);
        let why = two.add(Rule::read("rename b -> a").unwrap()).unwrap_err();
        assert!(why.contains("back to where it was"), "{why}");
        let mut three = rules(&["rename a -> b", "rename b -> c"]);
        assert!(three.add(Rule::read("rename c -> a/x").unwrap()).is_err());
        assert!(Rule::read("rename a -> a/b").is_err(), "not inside itself");
        assert!(Rule::read("rename a -> a").is_err());
    }

    #[test]
    fn a_rule_that_could_never_do_anything_is_refused() {
        let mut moved = rules(&["rename People -> people"]);
        let why = moved.add(Rule::read("delete People/Kira").unwrap()).unwrap_err();
        assert!(why.contains("not there any more"), "{why}");
        assert!(moved.add(Rule::read("rename People -> people").unwrap()).is_err());
        assert!(moved.add(Rule::read("delete people/Kira").unwrap()).is_ok());
    }

    #[test]
    fn separators_and_empty_paths_are_refused() {
        for bad in [
            "rename a -> b|c",
            "rename a -> ",
            "rename  -> b",
            "delete a//b",
            "delete /a",
            "delete",
            "move a",
        ] {
            assert!(Rule::read(bad).is_err(), "{bad} was accepted");
        }
        let rule = Rule::read(" rename  People / Kira  ->  people/Kira ").unwrap();
        assert_eq!(rule.written(), "rename People/Kira -> people/Kira");
        assert_eq!(Rule::read(&rule.written()), Ok(rule));
    }

    #[test]
    fn only_the_deepest_tags_count() {
        assert_eq!(
            deepest(&tags(&[
                "places",
                "places/inChina",
                "places/inChina/Beijing",
                "people",
                "people/Ben"
            ])),
            tags(&["people/Ben", "places/inChina/Beijing"])
        );
    }

    #[test]
    fn a_bare_root_goes_when_the_tree_has_tags_below_it() {
        let tree = TagTree::of(&tags(&["events/2006 Trip", "wired"]));
        assert_eq!(
            without_bare_roots(tags(&["events", "wired", "places/inChina"]), &tree),
            tags(&["wired", "places/inChina"])
        );
    }

    fn shaped() -> TagTree {
        TagTree::counted(
            [
                tags(&["places/inChina/Beijing", "people/groupChina/Ben", "timeline/2006"]),
                tags(&["places/inChina/Dalian", "places/Hamburg", "events/2006 Trip"]),
                tags(&["places/inGermany/Hamburg", "People/groupChina/Ben", "timeline/2007"]),
                tags(&["places/inIreland/Galway", "People/groupChina/Kira", "events/2008 Walk"]),
                tags(&["topics/food", "mixed/food", "Apartmens/Flat"]),
                tags(&["Ben", "2007", "inChina", "food", "landscape"]),
                tags(&["Kira", "places/inDenmark/Copenhagen"]),
            ]
            .into_iter()
            .chain((0..20).map(|_| tags(&["places/inChina/Beijing", "timeline/2006"]))),
        )
    }

    #[test]
    fn the_shape_is_learned_from_the_tree() {
        let tree = shaped();
        let found = branches(&tree);
        let depths: Vec<(&str, usize)> = found
            .iter()
            .map(|branch| (branch.root.as_str(), branch.depth))
            .collect();
        assert_eq!(
            depths,
            [
                ("Apartmens", 2),
                ("People", 3),
                ("events", 2),
                ("mixed", 2),
                ("people", 3),
                ("places", 3),
                ("timeline", 2),
                ("topics", 2),
            ]
        );
        assert_eq!(twin_roots(&tree), [(tags(&["People", "people"]), "people".to_string())]);
    }

    #[test]
    fn a_flat_keyword_goes_into_the_one_branch_that_has_its_name() {
        let found = flat(&shaped());
        assert_eq!(
            found.into,
            [
                ("2007".to_string(), "timeline/2007".to_string()),
                ("Ben".to_string(), "people/groupChina/Ben".to_string()),
                ("Kira".to_string(), "people/groupChina/Kira".to_string()),
                ("inChina".to_string(), "places/inChina".to_string()),
            ],
            "Ben is under both spellings of people, which are one after the twins merge"
        );
        assert_eq!(
            found.several,
            [("food".to_string(), tags(&["mixed/food", "topics/food"]))]
        );
        assert_eq!(found.nowhere, ["landscape"]);
    }

    #[test]
    fn a_leaf_above_its_usual_depth_goes_where_its_name_is() {
        let tree = shaped();
        assert_eq!(
            misplaced(&tree, &branches(&tree)),
            [("places/Hamburg".to_string(), "places/inGermany/Hamburg".to_string())]
        );
    }
}
