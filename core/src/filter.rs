//! A set of photos named in a few words: what the dashboard counts, and what a click on a count
//! shows. The count and the list come from one predicate, so they cannot disagree.
//!
//! Written out as `no-gps`, `no-gps@Germany`, `no-gps@Germany/2019-07-13 Sommerfest`,
//! `tag:mixed/food`, `tag:mixed/funny|mixed/Funny`, `issue:sidecar`. What follows the `@` is a
//! folder inside the library.

use rusqlite::params_from_iter;

use crate::cache::{Cache, Result};
use crate::layout::Fit;
use crate::scan::IssueKind;

/// A field a photo should carry, and so can be missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gap {
    Gps,
    Date,
    /// Not missing as such: the photo's date and its folder's date disagree.
    DateOffFolder,
    Tags,
    People,
    Location,
}

impl Gap {
    pub const ALL: [Gap; 6] = [
        Gap::Gps,
        Gap::Date,
        Gap::DateOffFolder,
        Gap::Tags,
        Gap::People,
        Gap::Location,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Gap::Gps => "no-gps",
            Gap::Date => "no-date",
            Gap::DateOffFolder => "date-off-folder",
            Gap::Tags => "no-tag",
            Gap::People => "no-people",
            Gap::Location => "no-location",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Gap::Gps => "GPS",
            Gap::Date => "Date",
            Gap::DateOffFolder => "Date agrees with the folder",
            Gap::Tags => "Tags",
            Gap::People => "People",
            Gap::Location => "Location text",
        }
    }

    /// What the photos in a set of this gap lack, in words.
    pub fn lacking(self) -> &'static str {
        match self {
            Gap::Gps => "without GPS",
            Gap::Date => "without a date",
            Gap::DateOffFolder => "whose date disagrees with the folder",
            Gap::Tags => "without any tag",
            Gap::People => "without people",
            Gap::Location => "without location text",
        }
    }

    /// The photos the gap can be asked about at all. Only the folder date needs both a date and
    /// a folder that states a year.
    pub(crate) fn measured(self) -> &'static str {
        match self {
            Gap::DateOffFolder => "p.taken_at IS NOT NULL AND p.event_year IS NOT NULL",
            _ => "1",
        }
    }

    /// The photos, among the measured ones, that have the gap.
    pub(crate) fn missing(self) -> &'static str {
        match self {
            Gap::Gps => "p.gps_lat IS NULL OR p.gps_lon IS NULL",
            Gap::Date => "p.taken_at IS NULL",
            // A day either side still agrees: a photo after midnight, a camera in another zone.
            Gap::DateOffFolder => {
                "CASE
                    WHEN p.event_month IS NOT NULL AND p.event_day IS NOT NULL THEN
                        abs(julianday(substr(p.taken_at, 1, 10))
                            - julianday(printf('%04d-%02d-%02d', p.event_year, p.event_month, p.event_day))) > 1
                    WHEN p.event_month IS NOT NULL THEN
                        CAST(substr(p.taken_at, 1, 4) AS INTEGER) != p.event_year
                        OR CAST(substr(p.taken_at, 6, 2) AS INTEGER) != p.event_month
                    ELSE CAST(substr(p.taken_at, 1, 4) AS INTEGER) != p.event_year
                END"
            }
            Gap::Tags => "NOT EXISTS (SELECT 1 FROM tag t WHERE t.photo_id = p.id)",
            Gap::People => {
                "NOT EXISTS (SELECT 1 FROM tag t WHERE t.photo_id = p.id
                    AND (lower(t.path) = 'people' OR lower(substr(t.path, 1, 7)) = 'people/'))"
            }
            Gap::Location => "p.location_city IS NULL",
        }
    }

    /// Measured and missing in one, the predicate every count of this gap uses.
    pub(crate) fn predicate(self) -> String {
        format!("({}) AND ({})", self.measured(), self.missing())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    All,
    Missing(Gap),
    /// Photos carrying any of these tags or a tag below them.
    Tagged(Vec<String>),
    /// Photos in a folder below their event's.
    SubFolder,
    /// Files directly in a country folder or in the library root.
    Loose,
    /// Folders where an event should be, but whose name carries no date.
    OffConvention,
    Issue(IssueKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub kind: Kind,
    /// A folder inside the library: a country, or an event's folder.
    pub within: Option<String>,
}

impl Filter {
    pub fn of(kind: Kind) -> Filter {
        Filter { kind, within: None }
    }

    pub fn missing(gap: Gap) -> Filter {
        Filter::of(Kind::Missing(gap))
    }

    pub fn within(mut self, folder: &str) -> Filter {
        self.within = Some(folder.to_string());
        self
    }

    /// What a person would call this set of photos.
    pub fn title(&self) -> String {
        let what = match &self.kind {
            Kind::All => "All photos".to_string(),
            Kind::Missing(gap) => format!("Photos {}", gap.lacking()),
            Kind::Tagged(paths) => format!("Photos tagged {}", paths.join(" or ")),
            Kind::SubFolder => "Photos in event sub-folders".to_string(),
            Kind::Loose => "Loose files".to_string(),
            Kind::OffConvention => "Files in folders without a date".to_string(),
            Kind::Issue(kind) => format!("Files with the issue {}", kind.as_str()),
        };
        match &self.within {
            Some(folder) => format!("{what} in {folder}"),
            None => what,
        }
    }

    /// Whether the set is of files that may not be photos at all.
    pub fn is_of_files(&self) -> bool {
        matches!(self.kind, Kind::Loose | Kind::OffConvention | Kind::Issue(_))
    }

    pub fn count(&self, cache: &Cache) -> Result<i64> {
        let (sql, params) = self.sql("count(DISTINCT {path})", "");
        cache
            .connection()
            .query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))
    }

    /// The paths inside the library, in path order.
    pub fn paths(&self, cache: &Cache) -> Result<Vec<String>> {
        let (sql, params) = self.sql("DISTINCT {path}", "ORDER BY {path}");
        let mut statement = cache.connection().prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(params.iter()), |row| row.get(0))?;
        rows.collect()
    }

    /// Photos are asked of the photo table; files that may not be photos at all, of the issues.
    fn sql(&self, select: &str, order: &str) -> (String, Vec<String>) {
        let mut params = Vec::new();
        let off_convention = IssueKind::OffConvention.as_str();
        let loose = [Fit::LooseInCountry.as_str(), Fit::LooseAtRoot.as_str()];
        let (table, predicate) = match &self.kind {
            Kind::All => ("photo", "1".to_string()),
            Kind::Missing(gap) => ("photo", gap.predicate()),
            Kind::Tagged(paths) => {
                let mut any = Vec::new();
                for path in paths {
                    params.push(path.clone());
                    let n = params.len();
                    any.push(format!(
                        "t.path = ?{n} OR substr(t.path, 1, length(?{n}) + 1) = ?{n} || '/'"
                    ));
                }
                (
                    "photo",
                    format!(
                        "EXISTS (SELECT 1 FROM tag t WHERE t.photo_id = p.id AND ({}))",
                        any.join(" OR ")
                    ),
                )
            }
            Kind::SubFolder => (
                "photo",
                "p.event_dir IS NOT NULL AND p.sub_path IS NOT NULL".to_string(),
            ),
            Kind::Loose => (
                "issue",
                format!(
                    "x.kind = '{off_convention}' AND x.detail IN ('{}', '{}')",
                    loose[0], loose[1]
                ),
            ),
            Kind::OffConvention => (
                "issue",
                format!(
                    "x.kind = '{off_convention}' AND x.detail NOT IN ('{}', '{}')",
                    loose[0], loose[1]
                ),
            ),
            Kind::Issue(kind) => {
                params.push(kind.as_str().to_string());
                ("issue", format!("x.kind = ?{}", params.len()))
            }
        };
        let alias = match table {
            "photo" => "p",
            _ => "x",
        };
        let select = select.replace("{path}", &format!("{alias}.rel_path"));
        let order = order.replace("{path}", &format!("{alias}.rel_path"));
        let mut sql = format!("SELECT {select} FROM {table} {alias} WHERE ({predicate})");
        if let Some(folder) = &self.within {
            params.push(folder.clone());
            let n = params.len();
            sql.push_str(&format!(
                " AND substr({alias}.rel_path, 1, length(?{n}) + 1) = ?{n} || '/'"
            ));
        }
        sql.push(' ');
        sql.push_str(&order);
        (sql, params)
    }
}

impl std::fmt::Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Kind::All => write!(f, "all")?,
            Kind::Missing(gap) => write!(f, "{}", gap.key())?,
            Kind::Tagged(paths) => write!(f, "tag:{}", paths.join("|"))?,
            Kind::SubFolder => write!(f, "sub-folder")?,
            Kind::Loose => write!(f, "loose")?,
            Kind::OffConvention => write!(f, "off-convention")?,
            Kind::Issue(kind) => write!(f, "issue:{}", kind.as_str())?,
        }
        if let Some(folder) = &self.within {
            write!(f, "@{folder}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Filter {
    type Err = String;

    fn from_str(text: &str) -> std::result::Result<Filter, String> {
        let (kind, within) = match text.split_once('@') {
            Some((kind, folder)) if !folder.is_empty() => (kind, Some(folder.to_string())),
            Some(_) => return Err(format!("{text}: nothing after the @")),
            None => (text, None),
        };
        let kind = match kind {
            "all" => Kind::All,
            "sub-folder" => Kind::SubFolder,
            "loose" => Kind::Loose,
            "off-convention" => Kind::OffConvention,
            _ => {
                if let Some(gap) = Gap::ALL.into_iter().find(|gap| gap.key() == kind) {
                    Kind::Missing(gap)
                } else if let Some(paths) = kind.strip_prefix("tag:") {
                    let paths: Vec<String> = paths.split('|').map(str::to_string).collect();
                    if paths.iter().any(String::is_empty) {
                        return Err(format!("{text}: an empty tag"));
                    }
                    Kind::Tagged(paths)
                } else if let Some(name) = kind.strip_prefix("issue:") {
                    match IssueKind::named(name) {
                        Some(IssueKind::OffConvention) => {
                            return Err(format!("{text}: ask for loose or off-convention"));
                        }
                        Some(kind) => Kind::Issue(kind),
                        None => return Err(format!("{text}: no such issue")),
                    }
                } else {
                    return Err(format!("{text}: not a set of photos"));
                }
            }
        };
        Ok(Filter { kind, within })
    }
}

#[cfg(all(test, feature = "fixtures"))]
pub(crate) mod tests {
    use super::*;
    use crate::metadata::Exiv2;
    use crate::scan::{self, Mode};
    use crate::thumbs::Thumbs;
    use std::sync::atomic::AtomicBool;

    pub(crate) fn scanned(name: &str) -> Cache {
        let base = std::env::temp_dir().join(format!("photomanager-filter-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");
        let mut cache = Cache::open(&base.join("cache.db")).unwrap();
        let thumbs = Thumbs::new(base.join("thumbs"));
        scan::run(
            &mut cache,
            &root,
            &Exiv2,
            &thumbs,
            Mode::Reconcile,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        cache
    }

    fn filter(text: &str) -> Filter {
        text.parse().unwrap()
    }

    #[test]
    fn survives_its_written_form() {
        for text in [
            "all",
            "no-gps",
            "no-gps@Germany",
            "date-off-folder@China/2006-09-00 Besuch Ben",
            "tag:mixed/food",
            "tag:mixed/funny|mixed/Funny@Ireland",
            "sub-folder",
            "loose",
            "off-convention",
            "issue:sidecar",
            "issue:duplicate content",
        ] {
            assert_eq!(filter(text).to_string(), text);
        }
    }

    #[test]
    fn refuses_what_it_cannot_name() {
        for text in [
            "",
            "nonsense",
            "no-gps@",
            "tag:",
            "tag:a||b",
            "issue:nonsense",
            "issue:off the convention",
        ] {
            assert!(text.parse::<Filter>().is_err(), "{text:?} was accepted");
        }
    }

    #[test]
    fn counts_what_it_lists() {
        let cache = scanned("counts");
        for text in [
            "all",
            "no-gps",
            "no-date",
            "date-off-folder",
            "no-tag",
            "no-people",
            "no-location",
            "no-gps@Germany",
            "tag:mixed",
            "tag:people",
            "sub-folder",
            "loose",
            "issue:no date",
        ] {
            let filter = filter(text);
            assert_eq!(
                filter.count(&cache).unwrap(),
                filter.paths(&cache).unwrap().len() as i64,
                "{text}"
            );
        }
    }

    #[test]
    fn finds_the_fixture_shapes() {
        let cache = scanned("shapes");
        let count = |text: &str| filter(text).count(&cache).unwrap();
        let all = crate::fixtures::photo_count() as i64;

        assert_eq!(count("all"), all);
        assert_eq!(count("no-gps"), all - 1, "one photo carries GPS");
        assert_eq!(count("no-date"), 2);
        assert_eq!(
            filter("date-off-folder").paths(&cache).unwrap(),
            [
                "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG",
                "China/2006-09-00 Besuch Ben/P1000001.JPG",
            ],
            "the folder says September, the photos August"
        );
        assert_eq!(count("no-tag"), 2);
        assert_eq!(count("no-location"), all - 1, "one photo names its city");
        assert_eq!(count("no-gps@Germany"), 2);
        assert_eq!(count("no-gps@Germany/2019-07-13 Sommerfest"), 2);
        assert_eq!(count("no-gps@Germ"), 0, "a folder is a whole name, not a prefix");
        assert_eq!(count("tag:mixed/Funny"), 1, "tags are compared as they are spelled");
        assert_eq!(count("tag:mixed"), 4);
        assert_eq!(count("sub-folder"), 2);
        assert_eq!(count("loose"), 1);
        assert_eq!(count("loose@China"), 1);
        assert_eq!(count("off-convention"), 0);
    }

    #[test]
    fn people_are_found_under_either_spelling_of_the_root() {
        let cache = scanned("people");
        let without = filter("no-people").paths(&cache).unwrap();
        assert!(!without.contains(&"Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG".to_string()));
        assert!(!without.contains(&"Germany/2019-07-13 Sommerfest/IMAG0001.jpg".to_string()));
        assert_eq!(without.len(), crate::fixtures::photo_count() - 3);
    }
}
