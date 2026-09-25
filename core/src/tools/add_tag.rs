//! Add a Tag: one tag onto every photo of the scope, most often a selection handed over from the
//! gallery. What a photo carries already stays; the tag is written with every level into every
//! tag field, like any tag write.

use super::{Page, Settings, Tool};
use crate::cache::{self, Cache};
use crate::changeset::Wanted;
use crate::geo::Geo;
use crate::scope::Scope;
use crate::tags;
use crate::write::{Change, Field};

pub struct AddATag;

/// The tag to add. Without one the tool changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tag(pub Option<String>);

impl Settings for Tag {
    fn read(text: &str) -> Result<Tag, String> {
        match text.trim().is_empty() {
            true => Ok(Tag(None)),
            false => Ok(Tag(Some(tags::path(text)?))),
        }
    }

    fn written(&self) -> String {
        self.0.clone().unwrap_or_default()
    }
}

impl Tool for AddATag {
    type Settings = Tag;

    fn key(&self) -> &'static str {
        "add-a-tag"
    }

    fn title(&self) -> &'static str {
        "Add a Tag"
    }

    fn fixes(&self) -> &'static str {
        "Adds one tag to every photo of the scope, such as a selection from the gallery"
    }

    fn named(&self, tag: &Tag) -> String {
        match &tag.0 {
            Some(path) => format!("Add the tag {path}"),
            None => "Add a tag".to_string(),
        }
    }

    fn page(&self) -> Option<Page> {
        Some(Page::Entry {
            title: "Tag",
            description: "The tag every photo of the scope gets, its levels separated by /, such as people/family/Anna",
        })
    }

    fn wanted(&self, cache: &Cache, _geo: Option<&Geo>, scope: &Scope, tag: &Tag) -> cache::Result<Vec<Wanted>> {
        let Some(path) = &tag.0 else {
            return Ok(Vec::new());
        };
        Ok(cache
            .tagged(&scope.paths(cache)?)?
            .into_iter()
            .map(|photo| {
                let mut then = photo.tags.clone();
                then.push(path.clone());
                Wanted::new(photo.rel_path, Change::of([Field::Tags(tags::deepest(&then))]))
            })
            .collect())
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::tools::testing::Library;
    use crate::tools::{self, AnyTool};

    const PICKED: [&str; 2] = [
        "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG",
        "Germany/2019-07-13 Sommerfest/IMAG0001.jpg",
    ];

    fn tool() -> &'static dyn AnyTool {
        tools::find("add-a-tag").expect("the tool is listed")
    }

    #[test]
    fn a_tag_over_a_selection_writes_exactly_those_photos() {
        let mut library = Library::new("add-a-tag");
        let scope = Scope::Photos {
            title: "2 photos".to_string(),
            paths: PICKED.iter().map(|path| path.to_string()).collect(),
        };
        assert!(
            tool()
                .change_set(&library.cache, None, &scope, None)
                .unwrap()
                .is_empty(),
            "no tag, nothing to do"
        );
        assert!(
            tool()
                .change_set(&library.cache, None, &scope, Some("people//Anna"))
                .is_err()
        );

        let set = tool()
            .change_set(&library.cache, None, &scope, Some("people/family/Anna"))
            .unwrap();
        assert_eq!(set.title, "Add the tag people/family/Anna");
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rows, PICKED);
        assert!(set.rows.iter().all(|row| row.verdict == Verdict::Change));
        let summary = library.apply(&set);
        assert_eq!(summary.written, 2, "{summary:?}");

        library.rescan();
        let tags = library.cache.stated(&[PICKED[0].to_string()]).unwrap()[PICKED[0]]
            .said
            .tags
            .clone();
        assert_eq!(tags, ["people", "people/family", "people/family/Anna"]);
        let kept = library.cache.stated(&[PICKED[1].to_string()]).unwrap()[PICKED[1]]
            .said
            .tags
            .clone();
        assert!(kept.contains(&"people/me".to_string()), "what it had stays: {kept:?}");

        let again = tool()
            .change_set(&library.cache, None, &scope, Some("people/family/Anna"))
            .unwrap();
        assert_eq!(again.counts().change, 0, "already carried, nothing to do");
    }
}
