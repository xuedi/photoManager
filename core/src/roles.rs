//! Tag roles: which root of a library's tag tree holds people, places, the year and the events, and
//! how a places tag names its country. Every finder that reads a role asks it here, so a library
//! with `Persons/...`, `Location/Germany/Berlin` or no such tags at all is read as it is.
//!
//! A role is only ever a root the library has. Until the person keeps roles, the ones proposed
//! from the roots are used; a library without such roots has none.

use std::collections::BTreeMap;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    People,
    Places,
    Year,
    Events,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::People, Role::Places, Role::Year, Role::Events];

    pub fn key(self) -> &'static str {
        match self {
            Role::People => "people",
            Role::Places => "places",
            Role::Year => "year",
            Role::Events => "events",
        }
    }

    pub fn named(key: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.key() == key)
    }

    pub fn title(self) -> &'static str {
        match self {
            Role::People => "People",
            Role::Places => "Places",
            Role::Year => "Year",
            Role::Events => "Events",
        }
    }

    /// What its tags say, which a field of its own says too.
    pub fn says(self) -> &'static str {
        match self {
            Role::People => "who is in the photo",
            Role::Places => "where the photo was taken",
            Role::Year => "the year the photo was taken",
            Role::Events => "the event the photo belongs to",
        }
    }

    /// The usual names of its root, in the order they are preferred.
    fn usual(self) -> &'static [&'static str] {
        match self {
            Role::People => &["people", "persons", "person"],
            Role::Places => &["places", "location", "locations", "place"],
            Role::Year => &["timeline", "years", "year"],
            Role::Events => &["events", "event"],
        }
    }

    /// Whether the app makes its tags from the data, so they can be kept as tags or not.
    pub fn generated(self) -> bool {
        self != Role::People
    }
}

/// How a places tag names its country, the level below the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CountryLevel {
    /// `places/inGermany/Berlin`, Shotwell's habit.
    #[default]
    In,
    /// `places/Germany/Berlin`.
    Name,
    /// `places/Berlin`: no country level.
    None,
}

impl CountryLevel {
    pub const ALL: [CountryLevel; 3] = [CountryLevel::In, CountryLevel::Name, CountryLevel::None];

    pub fn key(self) -> &'static str {
        match self {
            CountryLevel::In => "in",
            CountryLevel::Name => "name",
            CountryLevel::None => "none",
        }
    }

    pub fn named(key: &str) -> Option<CountryLevel> {
        CountryLevel::ALL.into_iter().find(|level| level.key() == key)
    }

    pub fn tells(self) -> &'static str {
        match self {
            CountryLevel::In => "inCountry, as inGermany",
            CountryLevel::Name => "Country, as Germany",
            CountryLevel::None => "No Country Level",
        }
    }

    /// A country as this level writes it.
    pub fn level(self, country: &str) -> Option<String> {
        let country = country.trim().replace(['/', '|'], "-");
        match self {
            CountryLevel::In => Some(format!("in{}", country.replace(' ', ""))),
            CountryLevel::Name => Some(country),
            CountryLevel::None => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roles {
    /// The root of each role, as the library spells it.
    pub roots: BTreeMap<Role, String>,
    pub country: CountryLevel,
    /// The generated roles whose tags stay tags: made from the data by Tidy Tags, and never
    /// dropped as redundant.
    pub kept: Vec<Role>,
    /// The topics - every tag of no role - are written as flat keywords, their last level only,
    /// the same in every tag field. A role's tags keep their tree.
    pub flat: bool,
}

/// The roles kept as tags when nothing else is said: the events, since a viewer that reads tags
/// and not the event field has no other way to browse them.
const KEPT: [Role; 1] = [Role::Events];

impl Roles {
    pub fn root(&self, role: Role) -> Option<&str> {
        self.roots.get(&role).map(String::as_str)
    }

    pub fn keeps(&self, role: Role) -> bool {
        self.kept.contains(&role)
    }

    /// The role whose root a tag is below, if any. The root alone says nothing and has none.
    pub fn of(&self, tag: &str) -> Option<Role> {
        let (first, rest) = tag.split_once('/')?;
        if rest.trim().is_empty() {
            return None;
        }
        Role::ALL.into_iter().find(|role| {
            self.root(*role)
                .is_some_and(|root| root.eq_ignore_ascii_case(first.trim()))
        })
    }

    pub fn is(&self, role: Role, tag: &str) -> bool {
        self.of(tag) == Some(role)
    }

    /// Whether a tag is the root of a role or below it.
    pub fn within(&self, role: Role, tag: &str) -> bool {
        let first = tag.split('/').next().unwrap_or(tag).trim();
        self.root(role).is_some_and(|root| root.eq_ignore_ascii_case(first))
    }

    /// Whether a tag is a topic: below no role's root, and not a role's root itself.
    pub fn is_topic(&self, tag: &str) -> bool {
        !Role::ALL.into_iter().any(|role| self.within(role, tag))
    }

    /// A photo's tags as a write puts them: as they are, or with the topics flat. The deepest
    /// paths, each once.
    pub fn keywords(&self, tags: &[String]) -> Vec<String> {
        let deepest = crate::tags::deepest(tags);
        if !self.flat {
            return deepest;
        }
        let flat: Vec<String> = deepest
            .into_iter()
            .map(|tag| match self.is_topic(&tag) {
                true => tag.rsplit('/').next().unwrap_or(&tag).trim().to_string(),
                false => tag,
            })
            .collect();
        crate::tags::deepest(&flat)
    }

    /// The levels of a places tag below the root, and the country among them.
    fn place_levels<'a>(&self, tag: &'a str) -> Option<Vec<&'a str>> {
        self.is(Role::Places, tag)
            .then(|| tag.split('/').skip(1).map(str::trim).collect())
    }

    /// `places/inChina`: a country and no place in it.
    pub fn names_a_country(&self, tag: &str) -> bool {
        self.country != CountryLevel::None && self.place_levels(tag).is_some_and(|levels| levels.len() == 1)
    }

    /// The country a places tag names, as the place data is asked: `inChina` is China.
    pub fn country_of(&self, tag: &str) -> Option<String> {
        let first = *self.place_levels(tag)?.first()?;
        match self.country {
            CountryLevel::None => None,
            CountryLevel::Name => Some(first.to_string()),
            CountryLevel::In => Some(match first.strip_prefix("in") {
                Some(rest) if rest.starts_with(char::is_uppercase) => rest.to_string(),
                _ => first.to_string(),
            }),
        }
    }

    /// The place a places tag names below its country, deepest first: `Berlin` of
    /// `places/inGermany/Berlin`. `None` for a tag of a country alone.
    pub fn place_of<'a>(&self, tag: &'a str) -> Option<&'a str> {
        let levels = self.place_levels(tag)?;
        let first_place = match self.country {
            CountryLevel::None => 0,
            _ => 1,
        };
        (levels.len() > first_place).then(|| levels[levels.len() - 1])
    }

    /// The year a year tag names: `timeline/2019` is 2019.
    pub fn year_of(&self, tag: &str) -> Option<i64> {
        if !self.is(Role::Year, tag) {
            return None;
        }
        let level = tag.split('/').nth(1)?.trim();
        (level.len() == 4 && level.chars().all(|c| c.is_ascii_digit())).then(|| level.parse().ok())?
    }

    /// The event an events tag names, and the year in front of it if it has one:
    /// `events/2019 Sommerfest` is Sommerfest of 2019.
    pub fn event_of<'a>(&self, tag: &'a str) -> Option<(Option<i64>, &'a str)> {
        if !self.is(Role::Events, tag) {
            return None;
        }
        let level = tag.split('/').next_back()?.trim();
        match level.split_once(' ') {
            Some((year, name)) if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) => {
                Some((year.parse().ok(), name.trim()))
            }
            _ => Some((None, level)),
        }
    }

    /// Proposed from the roots a library has, each with how many photos carry it: for each role
    /// the usual name it has with the most photos, in any case; how the places tags spell a
    /// country from the level below their root.
    pub fn proposed(roots: &[(String, i64)], places_below: &[String]) -> Roles {
        let mut chosen = BTreeMap::new();
        for role in Role::ALL {
            let best = roots
                .iter()
                .filter(|(root, _)| role.usual().iter().any(|usual| usual.eq_ignore_ascii_case(root)))
                .max_by_key(|(_, photos)| *photos);
            if let Some((root, _)) = best {
                chosen.insert(role, root.clone());
            }
        }
        let shaped_in = places_below
            .iter()
            .filter(|level| {
                level
                    .strip_prefix("in")
                    .is_some_and(|rest| rest.starts_with(char::is_uppercase))
            })
            .count();
        Roles {
            roots: chosen,
            country: match shaped_in * 2 >= places_below.len() && !places_below.is_empty() {
                true => CountryLevel::In,
                false => CountryLevel::Name,
            },
            kept: KEPT.to_vec(),
            flat: false,
        }
    }

    /// As one line of JSON, the way the settings keep it.
    pub fn written(&self) -> String {
        let roots: serde_json::Map<String, Value> = self
            .roots
            .iter()
            .map(|(role, root)| (role.key().to_string(), Value::from(root.clone())))
            .collect();
        json!({
            "roots": roots,
            "country": self.country.key(),
            "kept": self.kept.iter().map(|role| role.key()).collect::<Vec<_>>(),
            "flat": self.flat,
        })
        .to_string()
    }

    pub fn read(text: &str) -> Result<Roles, String> {
        let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let mut roots = BTreeMap::new();
        if let Some(Value::Object(kept)) = value.get("roots") {
            for (key, root) in kept {
                let role = Role::named(key).ok_or_else(|| format!("{key} is no role"))?;
                let root = root
                    .as_str()
                    .map(str::trim)
                    .filter(|root| !root.is_empty() && !root.contains('/'));
                let root = root.ok_or_else(|| format!("the root of {key} is no root"))?;
                roots.insert(role, root.to_string());
            }
        }
        let country = match value.get("country").and_then(Value::as_str) {
            Some(key) => CountryLevel::named(key).ok_or_else(|| format!("{key} is no country level"))?,
            None => CountryLevel::default(),
        };
        let kept = match value.get("kept") {
            Some(Value::Array(kept)) => kept
                .iter()
                .filter_map(Value::as_str)
                .filter_map(Role::named)
                .filter(|role| role.generated())
                .collect(),
            _ => KEPT.to_vec(),
        };
        let flat = value.get("flat").and_then(Value::as_bool).unwrap_or(false);
        Ok(Roles {
            roots,
            country,
            kept,
            flat,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours() -> Roles {
        Roles::proposed(
            &[
                ("events".to_string(), 10),
                ("timeline".to_string(), 10),
                ("places".to_string(), 9),
                ("People".to_string(), 1),
                ("people".to_string(), 5),
                ("mixed".to_string(), 3),
            ],
            &["inChina".to_string(), "inGermany".to_string()],
        )
    }

    #[test]
    fn this_library_s_roots_are_proposed() {
        let roles = ours();
        assert_eq!(roles.root(Role::People), Some("people"), "the twin with more photos");
        assert_eq!(roles.root(Role::Places), Some("places"));
        assert_eq!(roles.root(Role::Year), Some("timeline"));
        assert_eq!(roles.root(Role::Events), Some("events"));
        assert_eq!(roles.country, CountryLevel::In);
        assert!(roles.keeps(Role::Events) && !roles.keeps(Role::Year));
    }

    #[test]
    fn another_library_is_read_as_it_is() {
        let roles = Roles::proposed(
            &[
                ("Persons".to_string(), 3),
                ("Location".to_string(), 4),
                ("food".to_string(), 1),
            ],
            &["Germany".to_string()],
        );
        assert_eq!(roles.root(Role::People), Some("Persons"));
        assert_eq!(roles.root(Role::Places), Some("Location"));
        assert_eq!(roles.root(Role::Year), None);
        assert_eq!(roles.country, CountryLevel::Name);
        assert!(roles.is(Role::People, "Persons/family/Anna") && roles.is(Role::People, "persons/Anna"));
        assert_eq!(roles.country_of("Location/Germany/Berlin").as_deref(), Some("Germany"));
        assert_eq!(roles.place_of("Location/Germany/Berlin"), Some("Berlin"));
        assert!(roles.names_a_country("Location/Germany"));

        let none = Roles::proposed(&[("food".to_string(), 1)], &[]);
        assert!(none.roots.is_empty());
        assert_eq!(none.of("people/Anna"), None, "nothing is assumed");
    }

    #[test]
    fn a_tag_says_what_its_role_reads() {
        let roles = ours();
        assert_eq!(roles.of("people"), None, "the root alone says nothing");
        assert!(roles.is(Role::People, "People/Kira"));
        assert_eq!(roles.country_of("places/inChina/Beijing").as_deref(), Some("China"));
        assert_eq!(roles.place_of("places/inChina"), None);
        assert_eq!(roles.place_of("places/inChina/Beijing/Hutong"), Some("Hutong"));
        assert_eq!(roles.year_of("timeline/2019"), Some(2019));
        assert_eq!(roles.year_of("timeline/old"), None);
        assert_eq!(
            roles.event_of("events/2019 Sommerfest"),
            Some((Some(2019), "Sommerfest"))
        );
        assert_eq!(roles.event_of("events/Sommerfest"), Some((None, "Sommerfest")));

        let flat = Roles {
            country: CountryLevel::None,
            ..ours()
        };
        assert!(!flat.names_a_country("places/Berlin"));
        assert_eq!(flat.place_of("places/Berlin"), Some("Berlin"));
        assert_eq!(flat.country_of("places/Berlin"), None);
    }

    #[test]
    fn roles_survive_their_written_form() {
        let roles = ours();
        assert_eq!(Roles::read(&roles.written()).unwrap(), roles);
        assert!(Roles::read("{\"roots\":{\"nonsense\":\"x\"}}").is_err());
        assert!(Roles::read("{\"roots\":{\"people\":\"a/b\"}}").is_err());
    }
}
