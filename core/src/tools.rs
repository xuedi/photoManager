//! What a tool is: something that looks at the photos of a scope and says what each should say.
//!
//! That is the whole contract. Everything after it - the change set, its counts, the preview, the
//! apply, the journal and the undo - is shared, so a tool never writes, never asks and never
//! shows anything itself. The application knows the tools only through [`ALL`]: it lists them,
//! counts them and opens them by key, and names none of them.
//!
//! A tool's settings are a plain value that can be written as text and read back, so what a
//! suggestion hands over later is a key, a scope and a line of text, not a form.

use crate::cache::{self, Cache};
use crate::changeset::{ChangeSet, Wanted};
use crate::scope::Scope;

/// What a tool can be told. A tool that needs nothing uses `()`.
pub trait Settings: Default + Sized {
    fn read(text: &str) -> Result<Self, String>;
    fn written(&self) -> String;
}

impl Settings for () {
    fn read(text: &str) -> Result<(), String> {
        match text.trim().is_empty() {
            true => Ok(()),
            false => Err(format!("this tool takes no settings, not {text}")),
        }
    }

    fn written(&self) -> String {
        String::new()
    }
}

pub trait Tool: Sync {
    type Settings: Settings;

    /// Stays the same for as long as the tool exists: the journal keeps it.
    fn key(&self) -> &'static str;
    fn title(&self) -> &'static str;
    /// One line on what it fixes.
    fn fixes(&self) -> &'static str;

    /// What a pass with these settings is called, in the preview and in the history.
    fn named(&self, _settings: &Self::Settings) -> String {
        self.title().to_string()
    }

    /// What each photo of the scope should say. Reads the cache, never a photo.
    fn wanted(&self, cache: &Cache, scope: &Scope, settings: &Self::Settings) -> cache::Result<Vec<Wanted>>;
}

/// A tool as the list holds it, its settings as text.
pub trait AnyTool: Sync {
    fn key(&self) -> &'static str;
    fn title(&self) -> &'static str;
    fn fixes(&self) -> &'static str;
    /// `None` is the tool's own defaults.
    fn change_set(&self, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<ChangeSet, String>;
}

impl<T: Tool> AnyTool for T {
    fn key(&self) -> &'static str {
        Tool::key(self)
    }

    fn title(&self) -> &'static str {
        Tool::title(self)
    }

    fn fixes(&self) -> &'static str {
        Tool::fixes(self)
    }

    fn change_set(&self, cache: &Cache, scope: &Scope, settings: Option<&str>) -> Result<ChangeSet, String> {
        let settings = match settings {
            Some(text) => T::Settings::read(text)?,
            None => T::Settings::default(),
        };
        let wanted = self
            .wanted(cache, scope, &settings)
            .map_err(|error| error.to_string())?;
        let mut set = ChangeSet::build(cache, &self.named(&settings), &wanted).map_err(|error| error.to_string())?;
        set.tool = Some(Tool::key(self).to_string());
        Ok(set)
    }
}

/// Every tool there is, in the order they are listed.
pub const ALL: &[&dyn AnyTool] = &[
    #[cfg(feature = "demo")]
    &demo::Rating,
];

pub fn find(key: &str) -> Option<&'static dyn AnyTool> {
    ALL.iter().copied().find(|tool| tool.key() == key)
}

/// How many photos of the scope the tool would change right now, with its own defaults: the
/// same change set the preview shows, so the two numbers cannot differ.
pub fn count(tool: &dyn AnyTool, cache: &Cache, scope: &Scope) -> Result<usize, String> {
    Ok(tool.change_set(cache, scope, None)?.counts().change)
}

/// A rating over the whole scope, so that the way from a tool to a photo can be driven before
/// the first real tool exists. Development builds only.
#[cfg(feature = "demo")]
pub mod demo {
    use super::*;
    use crate::write::{Change, Field};

    pub struct Rating;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Stars(pub i64);

    impl Default for Stars {
        fn default() -> Stars {
            Stars(3)
        }
    }

    impl Settings for Stars {
        fn read(text: &str) -> Result<Stars, String> {
            match text.trim().parse::<i64>() {
                Ok(stars) if (0..=5).contains(&stars) => Ok(Stars(stars)),
                _ => Err(format!("{text} is not a rating from 0 to 5")),
            }
        }

        fn written(&self) -> String {
            self.0.to_string()
        }
    }

    impl Tool for Rating {
        type Settings = Stars;

        fn key(&self) -> &'static str {
            "demo-rating"
        }

        fn title(&self) -> &'static str {
            "Demo Rating"
        }

        fn fixes(&self) -> &'static str {
            "Sets one rating on every photo in the scope"
        }

        fn named(&self, stars: &Stars) -> String {
            format!("Set a rating of {}", stars.0)
        }

        fn wanted(&self, cache: &Cache, scope: &Scope, stars: &Stars) -> cache::Result<Vec<Wanted>> {
            Ok(scope
                .paths(cache)?
                .into_iter()
                .map(|rel_path| Wanted::new(rel_path, Change::of([Field::Rating(Some(stars.0))])))
                .collect())
        }
    }
}

#[cfg(all(test, feature = "fixtures", feature = "demo"))]
mod tests {
    use super::*;
    use crate::filter::Filter;
    use crate::filter::tests::scanned;

    #[test]
    fn the_demo_over_a_scope_is_exactly_the_scope() {
        let cache = scanned("tools-demo");
        let demo = find("demo-rating").expect("the demo is listed");
        let scope = Scope::Filter(Filter::all().within("Germany"));

        let set = demo.change_set(&cache, &scope, Some("4")).unwrap();
        let rows: Vec<String> = set.rows.iter().map(|row| row.rel_path.clone()).collect();
        assert_eq!(rows, scope.paths(&cache).unwrap());
        assert_eq!(set.title, "Set a rating of 4");
        assert_eq!(set.tool.as_deref(), Some("demo-rating"));

        let whole = Scope::Filter(Filter::all());
        assert_eq!(count(demo, &cache, &whole).unwrap(), crate::fixtures::photo_count());
        assert!(count(demo, &cache, &scope).unwrap() < crate::fixtures::photo_count());
        assert!(demo.change_set(&cache, &scope, Some("nine")).is_err());
    }

    #[test]
    fn settings_round_trip_through_text() {
        assert_eq!(demo::Stars::read(&demo::Stars(2).written()), Ok(demo::Stars(2)));
        assert_eq!(<()>::read(""), Ok(()));
        assert!(<()>::read("anything").is_err());
    }
}
