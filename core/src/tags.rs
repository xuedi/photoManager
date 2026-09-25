//! The tag vocabulary as rules a person decides once. `rename A -> B` moves `A` and everything
//! below it to `B`, which is renaming a leaf, moving a branch and merging into a tag that is
//! already there, all at once. `delete A` takes `A` and everything below it away.
//!
//! The rules are applied in order to a photo's deepest tags, and what comes out is its new tag
//! set. A rule that could never do anything, or would take a tag back to where an earlier rule
//! moved it from, is refused when it is entered, with why.
//!
//! What the tree already shows - case twins, look-alikes, the `mixed` bucket - is offered as
//! [`Suggestion`]s, each one or two rules a click away.

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

/// Where the `mixed` bucket's leaves are offered to go.
pub const MIXED: &str = "mixed";
pub const TOPICS: &str = "topics";

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

/// What the tree offers to tidy, and the rules that would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What Leave Alone is remembered under.
    pub key: String,
    /// `People and people`.
    pub title: String,
    /// `Merge Into people`.
    pub offer: String,
    /// Photos carrying any of the tags it would move.
    pub photos: i64,
    pub rules: Vec<Rule>,
}

/// Case twins, look-alikes and the leaves of `mixed`, from the tree after the rules so far. A
/// suggestion left alone, or one the rules would refuse, is not offered.
pub fn suggestions(tree: &TagTree, rules: &Rules, left: &BTreeSet<String>) -> Vec<Suggestion> {
    let count = |path: &str| tree.count(path).unwrap_or_default();
    let mut found = Vec::new();

    for spellings in tree.case_twins() {
        let lower = spellings[0].to_lowercase();
        let into = spellings
            .iter()
            .find(|spelling| **spelling == lower)
            .unwrap_or_else(|| {
                spellings
                    .iter()
                    .max_by(|one, other| count(one).cmp(&count(other)).then(other.cmp(one)))
                    .expect("twins are two at least")
            })
            .clone();
        let moved: Vec<&String> = spellings.iter().filter(|spelling| **spelling != into).collect();
        found.push(Suggestion {
            key: format!("twin:{lower}"),
            title: spellings.join(" and "),
            offer: format!("Merge Into {into}"),
            photos: moved.iter().map(|path| count(path)).sum(),
            rules: moved.iter().filter_map(|from| Rule::rename(from, &into).ok()).collect(),
        });
    }

    for (one, other) in tree.look_alikes() {
        let (from, into) = match count(&one).cmp(&count(&other)) {
            std::cmp::Ordering::Greater => (other.clone(), one.clone()),
            std::cmp::Ordering::Less => (one.clone(), other.clone()),
            std::cmp::Ordering::Equal if leaf(&one).len() >= leaf(&other).len() => (other.clone(), one.clone()),
            std::cmp::Ordering::Equal => (one.clone(), other.clone()),
        };
        found.push(Suggestion {
            key: format!("alike:{one}|{other}"),
            title: format!("{one} and {other}"),
            offer: format!("Merge Into {into}"),
            photos: count(&from),
            rules: Rule::rename(&from, &into).into_iter().collect(),
        });
    }

    for name in tree.children(MIXED) {
        let from = format!("{MIXED}/{name}");
        let into = format!("{TOPICS}/{name}");
        found.push(Suggestion {
            key: format!("mixed:{name}"),
            title: from.clone(),
            offer: format!("Move to {into}"),
            photos: count(&from),
            rules: Rule::rename(&from, &into).into_iter().collect(),
        });
    }

    found
        .into_iter()
        .filter(|suggestion| !left.contains(&suggestion.key) && !suggestion.rules.is_empty())
        .filter(|suggestion| {
            let mut tried = rules.clone();
            suggestion.rules.iter().all(|rule| tried.add(rule.clone()).is_ok())
        })
        .collect()
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
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

    #[test]
    fn suggestions_come_from_the_tree_and_leave_alone_is_kept() {
        let photos = vec![
            tags(&["People/Kira"]),
            tags(&["people/Ben"]),
            tags(&["people/Anna"]),
            tags(&["mixed/discusting"]),
            tags(&["mixed/disgusting"]),
            tags(&["mixed/disgusting", "mixed/food"]),
        ];
        let none = Rules::default();
        let found = suggestions(&mapped_tree(&none, &photos), &none, &BTreeSet::new());
        let offers: Vec<(&str, &str)> = found
            .iter()
            .map(|suggestion| (suggestion.key.as_str(), suggestion.offer.as_str()))
            .collect();
        assert_eq!(
            offers,
            [
                ("twin:people", "Merge Into people"),
                ("alike:mixed/discusting|mixed/disgusting", "Merge Into mixed/disgusting"),
                ("mixed:discusting", "Move to topics/discusting"),
                ("mixed:disgusting", "Move to topics/disgusting"),
                ("mixed:food", "Move to topics/food"),
            ]
        );
        assert_eq!(found[0].photos, 1);

        let confirmed = Rules(found[0].rules.clone());
        let after = suggestions(&mapped_tree(&confirmed, &photos), &confirmed, &BTreeSet::new());
        assert!(
            after.iter().all(|suggestion| suggestion.key != "twin:people"),
            "a merged twin is gone"
        );

        let left = BTreeSet::from(["mixed:food".to_string()]);
        let after = suggestions(&mapped_tree(&none, &photos), &none, &left);
        assert!(after.iter().all(|suggestion| suggestion.key != "mixed:food"));
    }
}
