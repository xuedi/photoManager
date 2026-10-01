//! Where a finding of the dashboard is fixed: a group of fixes on the Suggestions tab, or a tool
//! scoped to exactly the photos the finding counts. A finding the application cannot fix has none.

use crate::checks::Check;
use crate::edits::Edit;
use crate::filter::{Filter, Gap, Kind};
use crate::fixes::{self, Finder};

#[derive(Debug, Clone, PartialEq)]
pub enum Remedy {
    /// The fixes of this finder, where the sure ones are.
    Fixes(&'static Finder),
    /// This tool, over the photos of the filter.
    Tool { edit: Edit, scope: Filter },
}

impl Remedy {
    /// The remedy of a set of one kind, narrowed to a folder or not. A set of several kinds is
    /// no finding and has none.
    pub fn of(filter: &Filter) -> Option<Remedy> {
        let [kind] = filter.kinds() else {
            return None;
        };
        let tool = |edit: Edit| {
            Some(Remedy::Tool {
                edit,
                scope: filter.clone(),
            })
        };
        let fixes = |key: &str| fixes::finder(key).map(Remedy::Fixes);
        match kind {
            Kind::Missing(Gap::Gps | Gap::Location) => tool(Edit::SetPlace),
            Kind::Missing(Gap::Date) => tool(Edit::SetDate),
            Kind::Missing(Gap::DateOffFolder) => tool(Edit::ShiftDates),
            Kind::Missing(Gap::Tags) => tool(Edit::AddTag),
            Kind::Missing(Gap::People) => fixes("people"),
            Kind::Missing(Gap::Event) => fixes("events-from-folders"),
            Kind::Missing(Gap::EventOffFolder) => None,
            Kind::Tagged(_) => fixes("tags"),
            Kind::Person(_) => None,
            Kind::SubFolder | Kind::Loose | Kind::OffLayout => fixes("folders"),
            Kind::OffName => fixes("file-names"),
            Kind::Issue(_) => None,
            Kind::Checked(Check::GpsSure) => fixes("places-from-tags"),
            Kind::Checked(Check::GpsAsks | Check::GpsEvent | Check::GpsNothing) => tool(Edit::SetPlace),
            Kind::Checked(Check::PlaceDisagrees | Check::Kept(_)) => None,
        }
    }

    /// Where it leads, as a button's tooltip says it.
    pub fn tells(&self) -> String {
        match self {
            Remedy::Fixes(finder) => format!("Open {} in Suggestions", finder.title),
            Remedy::Tool { edit, .. } => format!("Open {} for These Photos", edit.title()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::IssueKind;

    fn remedy(written: &str) -> Option<Remedy> {
        Remedy::of(&written.parse().unwrap())
    }

    fn finder(written: &str) -> &'static str {
        match remedy(written) {
            Some(Remedy::Fixes(finder)) => finder.key,
            other => panic!("{written}: {other:?}"),
        }
    }

    fn tool(written: &str) -> (Edit, String) {
        match remedy(written) {
            Some(Remedy::Tool { edit, scope }) => (edit, scope.to_string()),
            other => panic!("{written}: {other:?}"),
        }
    }

    #[test]
    fn a_missing_field_opens_its_tool_on_exactly_those_photos() {
        assert_eq!(tool("no-gps"), (Edit::SetPlace, "no-gps".to_string()));
        assert_eq!(tool("no-location"), (Edit::SetPlace, "no-location".to_string()));
        assert_eq!(tool("no-date").0, Edit::SetDate);
        assert_eq!(tool("date-off-folder").0, Edit::ShiftDates);
        assert_eq!(tool("no-tag").0, Edit::AddTag);
        assert_eq!(
            tool("no-gps@Germany/2019-07-13 Sommerfest"),
            (Edit::SetPlace, "no-gps@Germany/2019-07-13 Sommerfest".to_string()),
            "a gap of one event stays that event's"
        );
    }

    #[test]
    fn what_the_app_can_be_sure_of_opens_its_fixes() {
        assert_eq!(finder("no-people"), "people");
        assert_eq!(finder("tag:mixed"), "tags");
        assert_eq!(finder("tag:mixed/funny|mixed/Funny"), "tags");
        for written in ["loose", "sub-folder", "off-layout"] {
            assert_eq!(finder(written), "folders", "{written}");
        }
        assert_eq!(finder("off-name"), "file-names");
        assert_eq!(finder("check:gps-sure"), "places-from-tags");
        assert_eq!(tool("check:gps-event"), (Edit::SetPlace, "check:gps-event".to_string()));
        assert_eq!(
            remedy("check:place-disagrees"),
            None,
            "the person looks which of the two is wrong"
        );
    }

    #[test]
    fn what_only_a_delete_would_fix_has_none() {
        for kind in IssueKind::ALL {
            if kind == IssueKind::OffLayout || kind == IssueKind::NoDate {
                continue;
            }
            assert_eq!(Remedy::of(&Filter::of(Kind::Issue(kind))), None, "{kind:?}");
        }
        assert_eq!(remedy("all"), None);
        assert_eq!(remedy("no-gps+tag:mixed"), None, "no finding of the dashboard");
    }
}
