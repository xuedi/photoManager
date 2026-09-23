//! What a tool is to work on. A whole set named by a filter stays a filter, so a scope over
//! thousands of photos is not thousands of paths, and it still means the right photos after a
//! scan. A hand-picked selection is the paths themselves, in the order they were shown.

use crate::cache::{Cache, Result};
use crate::filter::Filter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Filter(Filter),
    Photos { title: String, paths: Vec<String> },
}

impl Scope {
    pub fn title(&self) -> String {
        match self {
            Scope::Filter(filter) => filter.title(),
            Scope::Photos { title, .. } => title.clone(),
        }
    }

    /// The photos the scope names, as paths inside the library.
    pub fn paths(&self, cache: &Cache) -> Result<Vec<String>> {
        match self {
            Scope::Filter(filter) => filter.paths(cache),
            Scope::Photos { paths, .. } => Ok(paths.clone()),
        }
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::filter::tests::scanned;

    #[test]
    fn a_filter_and_its_paths_name_the_same_photos() {
        let cache = scanned("scope");
        let filter: Filter = "no-gps@Germany".parse().unwrap();
        let whole = Scope::Filter(filter.clone());
        let picked = Scope::Photos {
            title: "2 photos".to_string(),
            paths: filter.paths(&cache).unwrap(),
        };
        assert_eq!(whole.paths(&cache).unwrap(), picked.paths(&cache).unwrap());
        assert_eq!(whole.paths(&cache).unwrap().len(), 2);
        assert_eq!(whole.title(), "Photos without GPS in Germany");
    }
}
