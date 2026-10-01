//! A set of photos named in a few words: what the dashboard counts, and what a click on a count
//! shows. The count and the list come from one predicate, so they cannot disagree.
//!
//! Written out as `no-gps`, `no-gps@Germany`, `no-gps@Germany/2019-07-13 Sommerfest`,
//! `tag:mixed/food`, `tag:mixed/funny|mixed/Funny`, `person:Anna`, `issue:sidecar`. What follows the `@` is a
//! folder inside the library. Parts joined by `+` must all hold: `no-gps+tag:mixed@China`.
//!
//! Tags and people may be asked for together: `person:Anna&Ben` is the photos naming both, and
//! `tag:mixed/food&places/inChina` the photos carrying both. Each of them is a part of its own,
//! and `|` inside one still means any of: `tag:mixed/funny|mixed/Funny&mixed/food`.

use rusqlite::params_from_iter;

use crate::cache::{Cache, Result};
use crate::checks::Check;
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
    /// The event in the photo's own field, for a photo in a named event folder.
    Event,
    /// Not missing as such: the event the photo's field names is not its folder's.
    EventOffFolder,
}

/// A photo in an event folder whose name part says something.
const NAMED_EVENT: &str = "p.event_dir IS NOT NULL AND trim(coalesce(p.event_name, '')) != ''";

impl Gap {
    pub const COUNT: usize = 8;
    pub const ALL: [Gap; Gap::COUNT] = [
        Gap::Gps,
        Gap::Date,
        Gap::DateOffFolder,
        Gap::Tags,
        Gap::People,
        Gap::Location,
        Gap::Event,
        Gap::EventOffFolder,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Gap::Gps => "no-gps",
            Gap::Date => "no-date",
            Gap::DateOffFolder => "date-off-folder",
            Gap::Tags => "no-tag",
            Gap::People => "no-people",
            Gap::Location => "no-location",
            Gap::Event => "no-event",
            Gap::EventOffFolder => "event-off-folder",
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
            Gap::Event => "Event",
            Gap::EventOffFolder => "Event agrees with the folder",
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
            Gap::Event => "without the event in their own field",
            Gap::EventOffFolder => "whose event disagrees with the folder",
        }
    }

    /// The photos the gap can be asked about at all. The folder date needs both a date and a
    /// folder that states a year, the event a named event folder, and the event against the
    /// folder both names.
    pub(crate) fn measured(self) -> String {
        match self {
            Gap::DateOffFolder => "p.taken_at IS NOT NULL AND p.event_year IS NOT NULL".to_string(),
            Gap::Event => NAMED_EVENT.to_string(),
            Gap::EventOffFolder => format!("p.event_field IS NOT NULL AND {NAMED_EVENT}"),
            _ => "1".to_string(),
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
            // Set-based, not correlated: SQLite builds the set once instead of asking per photo.
            Gap::Tags => "p.id NOT IN (SELECT photo_id FROM tag)",
            Gap::People => "p.id NOT IN (SELECT photo_id FROM person)",
            Gap::Location => "p.location_city IS NULL",
            Gap::Event => "p.event_field IS NULL",
            Gap::EventOffFolder => "trim(p.event_field) != trim(p.event_name)",
        }
    }

    /// Measured and missing in one, the predicate every count of this gap uses.
    pub(crate) fn predicate(self) -> String {
        format!("({}) AND ({})", self.measured(), self.missing())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Missing(Gap),
    /// Photos carrying any of these tags or a tag below them.
    Tagged(Vec<String>),
    /// Photos naming any of these persons, with a face box or without.
    Person(Vec<String>),
    /// Photos in a folder below their event's.
    SubFolder,
    /// Photos with a date whose file name is not one the naming scheme gives that date.
    OffName,
    /// Files in a folder of the layout's levels, or in the library root, not in any event.
    Loose,
    /// Events in folders that are not the layout's, and folders where an event should be whose
    /// name carries no date.
    OffLayout,
    Issue(IssueKind),
    /// What the place check found.
    Checked(Check),
}

impl Kind {
    /// Where the kind sits in the written form. A filter holds at most one kind of each sort,
    /// but for tags and people, which may be asked for together.
    fn sort(&self) -> u8 {
        match self {
            Kind::Missing(_) => 0,
            Kind::Tagged(_) => 1,
            Kind::Person(_) => 2,
            Kind::SubFolder => 3,
            Kind::OffName => 4,
            Kind::Loose => 5,
            Kind::OffLayout => 6,
            Kind::Issue(_) => 7,
            Kind::Checked(_) => 8,
        }
    }

    fn repeats(&self) -> bool {
        matches!(self, Kind::Tagged(_) | Kind::Person(_))
    }

    /// Whether the kind is of files that may not be photos at all, asked of the issues.
    fn is_of_files(&self) -> bool {
        matches!(self, Kind::Loose | Kind::OffLayout | Kind::Issue(_))
    }

    /// The kind on its own, as the title of a set.
    pub fn title(&self) -> String {
        match self {
            Kind::Missing(gap) => format!("Photos {}", gap.lacking()),
            Kind::Tagged(paths) => format!("Photos tagged {}", paths.join(" or ")),
            Kind::Person(names) => format!("Photos of {}", names.join(" or ")),
            Kind::SubFolder => "Photos in event sub-folders".to_string(),
            Kind::OffName => "Photos not named by their date".to_string(),
            Kind::Loose => "Loose files".to_string(),
            Kind::OffLayout => "Files off the layout".to_string(),
            Kind::Issue(kind) => format!("Files with the issue {}", kind.as_str()),
            Kind::Checked(check) => format!("Photos {}", check.phrase()),
        }
    }

    /// The kind as a part of a longer title.
    fn phrase(&self) -> String {
        match self {
            Kind::Missing(gap) => gap.lacking().to_string(),
            Kind::Tagged(paths) => format!("tagged {}", paths.join(" or ")),
            Kind::Person(names) => format!("of {}", names.join(" or ")),
            Kind::SubFolder => "in event sub-folders".to_string(),
            Kind::OffName => "not named by their date".to_string(),
            Kind::Loose => "loose".to_string(),
            Kind::OffLayout => "off the layout".to_string(),
            Kind::Issue(kind) => format!("with the issue {}", kind.as_str()),
            Kind::Checked(check) => check.phrase().to_string(),
        }
    }

    /// A part as written, as the kinds it is: more than one for tags or people asked for together.
    fn parse(text: &str) -> std::result::Result<Vec<Kind>, String> {
        let groups = |text: &str, empty: &str| -> std::result::Result<Vec<Vec<String>>, String> {
            text.split('&')
                .map(|group| {
                    let each: Vec<String> = group.split('|').map(str::to_string).collect();
                    match each.iter().any(|one| one.trim().is_empty()) {
                        true => Err(empty.to_string()),
                        false => Ok(each),
                    }
                })
                .collect()
        };
        if let Some(paths) = text.strip_prefix("tag:") {
            return Ok(groups(paths, "an empty tag")?.into_iter().map(Kind::Tagged).collect());
        }
        if let Some(names) = text.strip_prefix("person:") {
            return Ok(groups(names, "an empty name")?.into_iter().map(Kind::Person).collect());
        }
        Ok(vec![match text {
            "sub-folder" => Kind::SubFolder,
            "off-name" => Kind::OffName,
            "loose" => Kind::Loose,
            "off-layout" => Kind::OffLayout,
            _ => {
                if let Some(gap) = Gap::ALL.into_iter().find(|gap| gap.key() == text) {
                    Kind::Missing(gap)
                } else if let Some(key) = text.strip_prefix("check:") {
                    Kind::Checked(Check::named(key).ok_or("no such check")?)
                } else if let Some(name) = text.strip_prefix("issue:") {
                    match IssueKind::named(name) {
                        Some(IssueKind::OffLayout) => return Err("ask for loose or off-layout".to_string()),
                        Some(kind) => Kind::Issue(kind),
                        None => return Err("no such issue".to_string()),
                    }
                } else {
                    return Err("not a set of photos".to_string());
                }
            }
        }])
    }

    /// The condition on one row, and the parameters it binds, numbered on from `params`.
    fn predicate(&self, params: &mut Vec<String>) -> String {
        let off_layout = IssueKind::OffLayout.as_str();
        let loose = [Fit::LooseInFolder.as_str(), Fit::LooseAtRoot.as_str()];
        let issue = |condition: String| format!("x.rel_path IN (SELECT rel_path FROM issue WHERE {condition})");
        match self {
            Kind::Missing(gap) => gap.predicate(),
            Kind::Tagged(paths) => {
                let mut any = Vec::new();
                for path in paths {
                    params.push(path.clone());
                    let n = params.len();
                    // A range over the path index: everything below `a/` sorts before `a0`.
                    any.push(format!("path = ?{n} OR (path >= ?{n} || '/' AND path < ?{n} || '0')"));
                }
                format!("p.id IN (SELECT photo_id FROM tag WHERE {})", any.join(" OR "))
            }
            Kind::Person(names) => {
                let first = params.len() + 1;
                params.extend(names.iter().cloned());
                let holes: Vec<String> = (first..=params.len()).map(|n| format!("?{n}")).collect();
                format!(
                    "p.id IN (SELECT photo_id FROM person WHERE name IN ({}))",
                    holes.join(", ")
                )
            }
            Kind::SubFolder => "p.event_dir IS NOT NULL AND p.sub_path IS NOT NULL".to_string(),
            Kind::OffName => off_name(),
            Kind::Loose => issue(format!(
                "kind = '{off_layout}' AND detail IN ('{}', '{}')",
                loose[0], loose[1]
            )),
            Kind::OffLayout => issue(format!(
                "kind = '{off_layout}' AND detail NOT IN ('{}', '{}')",
                loose[0], loose[1]
            )),
            Kind::Issue(kind) => {
                params.push(kind.as_str().to_string());
                issue(format!("kind = ?{}", params.len()))
            }
            Kind::Checked(check) => {
                params.push(check.key().to_string());
                format!("p.id IN (SELECT photo_id FROM checked WHERE kind = ?{})", params.len())
            }
        }
    }
}

/// Each kind as part of a title, the tags and the people asked for together each in one phrase:
/// "tagged food and Beijing", "of Anna and Ben".
fn phrases<'a>(kinds: impl Iterator<Item = &'a Kind>) -> Vec<String> {
    let mut phrases: Vec<String> = Vec::new();
    let mut last: Option<&Kind> = None;
    for kind in kinds {
        let together = match (last, kind) {
            (Some(Kind::Tagged(_)), Kind::Tagged(paths)) => Some(paths.join(" or ")),
            (Some(Kind::Person(_)), Kind::Person(names)) => Some(names.join(" or ")),
            _ => None,
        };
        match (together, phrases.last_mut()) {
            (Some(more), Some(phrase)) => phrase.push_str(&format!(" and {more}")),
            _ => phrases.push(kind.phrase()),
        }
        last = Some(kind);
    }
    phrases
}

/// The same test as `names::plan`, in SQL: a real date, and a name that is not `YYYY-MM-DD_HHMMSS`
/// with an optional `_N` from 2 on and `.jpg`. SQLite normalises an impossible date, so a date
/// that does not come back as itself is none.
fn off_name() -> String {
    let name = "substr(p.rel_path, length(rtrim(p.rel_path, replace(p.rel_path, '/', ''))) + 1)";
    let stem = "substr(p.taken_at, 1, 10) || '_' || substr(p.taken_at, 12, 2) || substr(p.taken_at, 15, 2) \
        || substr(p.taken_at, 18, 2)";
    let number = format!("substr({name}, 19, length({name}) - 22)");
    format!(
        "p.taken_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
        AND datetime(p.taken_at) = p.taken_at
        AND NOT (
            {name} = {stem} || '.{ext}'
            OR (substr({name}, 1, 18) = {stem} || '_' AND {name} GLOB '*.{ext}'
                AND {number} GLOB '[1-9]*' AND {number} NOT GLOB '*[^0-9]*' AND {number} != '1')
        )",
        ext = crate::names::EXTENSION
    )
}

/// How the photos of a set are listed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// The photo's own date, undated photos last, then the path.
    #[default]
    Date,
    /// The path, so an event reads in folder order.
    Name,
}

impl Order {
    pub fn key(self) -> &'static str {
        match self {
            Order::Date => "date",
            Order::Name => "name",
        }
    }

    pub fn named(key: &str) -> Option<Order> {
        [Order::Date, Order::Name].into_iter().find(|order| order.key() == key)
    }
}

/// One file of a set, with what a cell of the grid needs to draw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub rel_path: String,
    /// `None` for a file the cache knows only as an issue, not as a photo.
    pub content_id: Option<String>,
    pub taken_at: Option<String>,
    pub orientation: Option<i64>,
    pub is_photo: bool,
}

/// A set of photos: every kind must hold, inside the folder if there is one. No kind at all is
/// the whole library.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    kinds: Vec<Kind>,
    /// A folder inside the library: a country, or an event's folder.
    pub within: Option<String>,
}

impl Filter {
    pub fn all() -> Filter {
        Filter::default()
    }

    pub fn of(kind: Kind) -> Filter {
        Filter::all().with(kind)
    }

    pub fn missing(gap: Gap) -> Filter {
        Filter::of(Kind::Missing(gap))
    }

    pub fn within(mut self, folder: &str) -> Filter {
        self.within = Some(folder.to_string());
        self
    }

    pub fn anywhere(mut self) -> Filter {
        self.within = None;
        self
    }

    /// Adds a kind, replacing the one of the same sort.
    /// Sets the part of this sort to `kind`, taking out every part of that sort there was.
    pub fn with(mut self, kind: Kind) -> Filter {
        self.kinds.retain(|each| each.sort() != kind.sort());
        self.and(kind)
    }

    /// Adds a part beside those there are. Only tags and people repeat; any other replaces its
    /// sort as [`Filter::with`] does.
    pub fn and(mut self, kind: Kind) -> Filter {
        if !kind.repeats() {
            self.kinds.retain(|each| each.sort() != kind.sort());
        }
        if !self.kinds.contains(&kind) {
            self.kinds.push(kind);
            self.kinds.sort_by_key(Kind::sort);
        }
        self
    }

    pub fn without(mut self, kind: &Kind) -> Filter {
        self.kinds.retain(|each| each != kind);
        self
    }

    /// Sets the gap part, or clears it.
    pub fn with_gap(self, gap: Option<Gap>) -> Filter {
        let filter = match self.gap() {
            Some(old) => self.without(&Kind::Missing(old)),
            None => self,
        };
        match gap {
            Some(gap) => filter.with(Kind::Missing(gap)),
            None => filter,
        }
    }

    /// Sets the tags to one tag and everything below it, or clears them.
    pub fn with_tag(mut self, tag: Option<&str>) -> Filter {
        self.kinds.retain(|kind| !matches!(kind, Kind::Tagged(_)));
        match tag {
            Some(tag) => self.and(Kind::Tagged(vec![tag.to_string()])),
            None => self,
        }
    }

    /// Sets the people to one person, or clears them.
    pub fn with_person(mut self, name: Option<&str>) -> Filter {
        self.kinds.retain(|kind| !matches!(kind, Kind::Person(_)));
        match name {
            Some(name) => self.and(Kind::Person(vec![name.to_string()])),
            None => self,
        }
    }

    /// Adds a tag to those asked for together, or takes it out when it is one of them. A tag
    /// above or below one asked for takes its place, as the two together say no more than the
    /// narrower one.
    pub fn toggle_tag(mut self, tag: &str) -> Filter {
        let one = Kind::Tagged(vec![tag.to_string()]);
        if self.kinds.contains(&one) {
            return self.without(&one);
        }
        let related = |other: &str| other.starts_with(&format!("{tag}/")) || tag.starts_with(&format!("{other}/"));
        self.kinds.retain(
            |kind| !matches!(kind, Kind::Tagged(paths) if matches!(paths.as_slice(), [other] if related(other))),
        );
        self.and(one)
    }

    /// Adds a person to those asked for together, or takes them out when they are one of them.
    pub fn toggle_person(self, name: &str) -> Filter {
        let one = Kind::Person(vec![name.to_string()]);
        match self.kinds.contains(&one) {
            true => self.without(&one),
            false => self.and(one),
        }
    }

    pub fn kinds(&self) -> &[Kind] {
        &self.kinds
    }

    pub fn gap(&self) -> Option<Gap> {
        self.kinds.iter().find_map(|kind| match kind {
            Kind::Missing(gap) => Some(*gap),
            _ => None,
        })
    }

    /// The tags asked for together, each a group any of which will do.
    pub fn tags(&self) -> Vec<&[String]> {
        self.kinds
            .iter()
            .filter_map(|kind| match kind {
                Kind::Tagged(paths) => Some(paths.as_slice()),
                _ => None,
            })
            .collect()
    }

    /// The people asked for together, each a group any of whom will do.
    pub fn persons(&self) -> Vec<&[String]> {
        self.kinds
            .iter()
            .filter_map(|kind| match kind {
                Kind::Person(names) => Some(names.as_slice()),
                _ => None,
            })
            .collect()
    }

    /// What a person would call this set of photos.
    pub fn title(&self) -> String {
        let what = match self.kinds.as_slice() {
            [] => "All photos".to_string(),
            [one] => one.title(),
            kinds => {
                // The files a set is of come first: "Loose files, without GPS".
                let noun = kinds.iter().find(|kind| kind.is_of_files());
                let rest = phrases(kinds.iter().filter(|kind| Some(*kind) != noun));
                match noun {
                    Some(noun) => format!("{}, {}", noun.title(), rest.join(", ")),
                    None => format!("Photos {}", rest.join(", ")),
                }
            }
        };
        match &self.within {
            Some(folder) => format!("{what} in {folder}"),
            None => what,
        }
    }

    /// Whether the set is of files that may not be photos at all.
    pub fn is_of_files(&self) -> bool {
        self.kinds.iter().any(Kind::is_of_files)
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

    /// The files of the set in the order asked for, each with what a grid cell needs.
    pub fn photos(&self, cache: &Cache, order: Order) -> Result<Vec<Listed>> {
        let order = match order {
            Order::Date => "ORDER BY p.taken_at IS NULL, p.taken_at, {path}",
            Order::Name => "ORDER BY {path}",
        };
        let (sql, params) = self.sql(
            "DISTINCT {path}, p.content_id, p.taken_at, p.orientation, p.id IS NOT NULL",
            order,
        );
        let mut statement = cache.connection().prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(params.iter()), |row| {
            Ok(Listed {
                rel_path: row.get(0)?,
                content_id: row.get(1)?,
                taken_at: row.get(2)?,
                orientation: row.get(3)?,
                is_photo: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// The set as a query of photo ids, to be asked inside another, with its parameters.
    pub(crate) fn ids(&self) -> (String, Vec<String>) {
        self.sql("p.id", "")
    }

    /// Photos are asked of the photo table. Once one kind is of files that may not be photos,
    /// the set is asked of the issues instead, with the photo each one is, if it is one.
    fn sql(&self, select: &str, order: &str) -> (String, Vec<String>) {
        let mut params = Vec::new();
        let (from, path) = match self.is_of_files() {
            false => ("photo p", "p.rel_path"),
            true => (
                "(SELECT DISTINCT rel_path FROM issue) x LEFT JOIN photo p ON p.rel_path = x.rel_path",
                "x.rel_path",
            ),
        };
        let mut conditions: Vec<String> = self
            .kinds
            .iter()
            .map(|kind| {
                let predicate = kind.predicate(&mut params);
                match self.is_of_files() && !kind.is_of_files() {
                    true => format!("(p.id IS NOT NULL AND ({predicate}))"),
                    false => format!("({predicate})"),
                }
            })
            .collect();
        if let Some(folder) = &self.within {
            params.push(folder.clone());
            let n = params.len();
            conditions.push(format!("substr({path}, 1, length(?{n}) + 1) = ?{n} || '/'"));
        }
        if conditions.is_empty() {
            conditions.push("1".to_string());
        }
        let select = select.replace("{path}", path);
        let order = order.replace("{path}", path);
        let sql = format!("SELECT {select} FROM {from} WHERE {} {order}", conditions.join(" AND "));
        (sql, params)
    }
}

impl std::fmt::Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.kinds.is_empty() {
            write!(f, "all")?;
        }
        let mut last: Option<&Kind> = None;
        for (at, kind) in self.kinds.iter().enumerate() {
            match (last, kind) {
                (Some(Kind::Tagged(_)), Kind::Tagged(paths)) => write!(f, "&{}", paths.join("|"))?,
                (Some(Kind::Person(_)), Kind::Person(names)) => write!(f, "&{}", names.join("|"))?,
                _ if at > 0 => write!(f, "+")?,
                _ => {}
            }
            let repeated = last.is_some_and(|last| last.repeats() && last.sort() == kind.sort());
            last = Some(kind);
            if repeated {
                continue;
            }
            match kind {
                Kind::Missing(gap) => write!(f, "{}", gap.key())?,
                Kind::Tagged(paths) => write!(f, "tag:{}", paths.join("|"))?,
                Kind::Person(names) => write!(f, "person:{}", names.join("|"))?,
                Kind::SubFolder => write!(f, "sub-folder")?,
                Kind::OffName => write!(f, "off-name")?,
                Kind::Loose => write!(f, "loose")?,
                Kind::OffLayout => write!(f, "off-layout")?,
                Kind::Issue(kind) => write!(f, "issue:{}", kind.as_str())?,
                Kind::Checked(check) => write!(f, "check:{}", check.key())?,
            }
        }
        if let Some(folder) = &self.within {
            write!(f, "@{folder}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Filter {
    type Err = String;

    /// `no-gps+tag:mixed@China`: kinds joined by `+`, all of which must hold, then the folder.
    fn from_str(text: &str) -> std::result::Result<Filter, String> {
        let (kinds, within) = match text.split_once('@') {
            Some((kinds, folder)) if !folder.is_empty() => (kinds, Some(folder.to_string())),
            Some(_) => return Err(format!("{text}: nothing after the @")),
            None => (text, None),
        };
        let mut filter = Filter {
            kinds: Vec::new(),
            within,
        };
        if kinds == "all" {
            return Ok(filter);
        }
        for part in kinds.split('+') {
            let parsed = Kind::parse(part).map_err(|why| format!("{text}: {why}"))?;
            if filter.kinds.iter().any(|each| each.sort() == parsed[0].sort()) {
                return Err(format!("{text}: two parts of the same sort"));
            }
            for kind in parsed {
                if filter.kinds.contains(&kind) {
                    return Err(format!("{text}: the same part twice"));
                }
                filter = filter.and(kind);
            }
        }
        Ok(filter)
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
        scanned_after(name, |_| {})
    }

    /// The stand-in library scanned after `change` had its way with it.
    pub(crate) fn scanned_after(name: &str, change: impl FnOnce(&std::path::Path)) -> Cache {
        let base = std::env::temp_dir().join(format!("photomanager-filter-{name}"));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("library");
        crate::fixtures::build(&root).expect("build the stand-in library");
        change(&root);
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
            "person:Anna",
            "person:Anna Maria|Tom@Germany",
            "no-gps+tag:mixed+person:Anna",
            "person:Anna&Tom",
            "tag:mixed/funny|mixed/Funny&mixed/food+person:Anna Maria|Tom&Mia@Germany",
            "sub-folder",
            "off-name",
            "no-gps+off-name@Greece",
            "loose",
            "off-layout",
            "issue:sidecar",
            "issue:duplicate content",
        ] {
            assert_eq!(filter(text).to_string(), text);
        }
    }

    #[test]
    fn the_dashboard_forms_mean_what_they_meant() {
        assert_eq!(filter("all"), Filter::all());
        assert_eq!(filter("no-gps@Germany"), Filter::missing(Gap::Gps).within("Germany"));
        assert_eq!(
            filter("tag:mixed/funny|mixed/Funny@Ireland"),
            Filter::of(Kind::Tagged(vec!["mixed/funny".into(), "mixed/Funny".into()])).within("Ireland")
        );
        assert_eq!(filter("issue:sidecar"), Filter::of(Kind::Issue(IssueKind::Sidecar)));
        assert_eq!(filter("no-gps").title(), "Photos without GPS");
        assert_eq!(filter("loose@China").title(), "Loose files in China");
    }

    #[test]
    fn parts_combine_in_one_written_form() {
        let combined = filter("no-gps+tag:mixed@China/2008-01-00 Holiday SOUTHTOUR");
        assert_eq!(
            combined.to_string(),
            "no-gps+tag:mixed@China/2008-01-00 Holiday SOUTHTOUR"
        );
        assert_eq!(combined.gap(), Some(Gap::Gps));
        assert_eq!(combined.tags(), [&["mixed".to_string()][..]]);
        assert_eq!(
            filter("tag:mixed+no-gps"),
            filter("no-gps+tag:mixed"),
            "the parts are kept in one order"
        );
        assert_eq!(
            combined.title(),
            "Photos without GPS, tagged mixed in China/2008-01-00 Holiday SOUTHTOUR"
        );
        assert_eq!(filter("no-gps+loose").title(), "Loose files, without GPS");

        let changed = Filter::all()
            .with_gap(Some(Gap::Date))
            .with_tag(Some("people"))
            .within("Germany");
        assert_eq!(changed.to_string(), "no-date+tag:people@Germany");
        assert_eq!(
            changed.clone().with_gap(Some(Gap::Gps)).to_string(),
            "no-gps+tag:people@Germany"
        );
        assert_eq!(changed.clone().with_tag(None).anywhere().to_string(), "no-date");
        assert_eq!(
            changed.without(&Kind::Tagged(vec!["people".into()])).to_string(),
            "no-date@Germany"
        );
    }

    #[test]
    fn tags_and_people_asked_for_together_must_all_hold() {
        let cache = scanned("filter-together");
        let paths = |text: &str| filter(text).paths(&cache).unwrap();
        for (one, other) in [("person:Anna", "person:Tom"), ("tag:people", "tag:mixed")] {
            let (prefix, first) = one.split_once(':').unwrap();
            let second = other.split_once(':').unwrap().1;
            let both = paths(&format!("{prefix}:{first}&{second}"));
            let expected: Vec<String> = paths(one)
                .into_iter()
                .filter(|path| paths(other).contains(path))
                .collect();
            assert_eq!(both, expected, "{one} and {other}");
            let either = paths(&format!("{prefix}:{first}|{second}"));
            assert!(
                either.len() >= paths(one).len().max(paths(other).len()),
                "| still means any of"
            );
        }

        let chosen = Filter::all().toggle_person("Anna").toggle_person("Tom");
        assert_eq!(chosen.to_string(), "person:Anna&Tom", "in the order chosen");
        assert_eq!(chosen.title(), "Photos of Anna and Tom");
        assert_eq!(chosen.clone().toggle_person("Anna").to_string(), "person:Tom");
        assert_eq!(
            chosen.clone().with_person(None).to_string(),
            "all",
            "with_person clears every person"
        );
        assert_eq!(
            chosen.without(&Kind::Person(vec!["Tom".into()])).to_string(),
            "person:Anna",
            "a chip takes out one person"
        );

        let tags = Filter::all().toggle_tag("mixed/food").toggle_tag("people");
        assert_eq!(tags.to_string(), "tag:mixed/food&people");
        assert_eq!(tags.title(), "Photos tagged mixed/food and people");
        assert_eq!(
            tags.clone().toggle_tag("mixed").to_string(),
            "tag:people&mixed",
            "a tag above a chosen one takes its place"
        );
        assert_eq!(
            tags.clone().toggle_tag("people/family").to_string(),
            "tag:mixed/food&people/family",
            "and so does one below"
        );
        assert_eq!(
            tags.toggle_tag("mixed/food").to_string(),
            "tag:people",
            "a chosen tag again is taken out"
        );
        let twins = filter("tag:mixed/funny|mixed/Funny").toggle_tag("people");
        assert_eq!(
            twins.to_string(),
            "tag:mixed/funny|mixed/Funny&people",
            "a group of any of is left as it is"
        );
    }

    #[test]
    fn refuses_what_it_cannot_name() {
        for text in [
            "",
            "nonsense",
            "no-gps@",
            "tag:",
            "tag:a||b",
            "person:",
            "person:Anna| ",
            "person:Anna+person:Tom",
            "person:Anna&",
            "person:Anna&Anna",
            "tag:a&|b",
            "issue:nonsense",
            "issue:off the layout",
            "no-gps+no-date",
            "no-gps+",
            "all+no-gps",
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
            "no-event",
            "event-off-folder",
            "no-gps@Germany",
            "tag:mixed",
            "tag:people",
            "person:Anna",
            "person:Mia|Tom",
            "person:Nobody",
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
    fn a_combination_lists_what_every_part_lists() {
        let cache = scanned("combined");
        for (text, parts) in [
            (
                "no-gps+tag:places@Germany",
                vec!["no-gps@Germany", "tag:places@Germany"],
            ),
            ("no-date+tag:mixed", vec!["no-date", "tag:mixed"]),
            ("no-people+sub-folder", vec!["no-people", "sub-folder"]),
            ("no-gps+loose", vec!["no-gps", "loose"]),
            ("tag:people+issue:no date", vec!["tag:people", "issue:no date"]),
            ("tag:people+person:Tom", vec!["tag:people", "person:Tom"]),
            ("no-gps+person:Anna|Mia", vec!["no-gps", "person:Anna|Mia"]),
        ] {
            let combined = filter(text);
            assert_eq!(combined.to_string().parse::<Filter>().unwrap(), combined);
            let mut expected = filter(parts[0]).paths(&cache).unwrap();
            for part in &parts[1..] {
                let other = filter(part).paths(&cache).unwrap();
                expected.retain(|path| other.contains(path));
            }
            assert_eq!(combined.paths(&cache).unwrap(), expected, "{text}");
            assert_eq!(combined.count(&cache).unwrap(), expected.len() as i64, "{text}");
        }
        assert_eq!(filter("no-gps+tag:places@Germany").count(&cache).unwrap(), 3);
        assert_eq!(filter("no-gps+loose").count(&cache).unwrap(), 1);
    }

    #[test]
    fn lists_its_photos_in_the_order_asked_for() {
        let cache = scanned("order");
        for text in ["all", "no-gps@Germany", "tag:mixed", "loose", "issue:no date"] {
            let filter = filter(text);
            let listed = filter.photos(&cache, Order::Date).unwrap();
            assert_eq!(listed.len() as i64, filter.count(&cache).unwrap(), "{text}");
            let mut paths: Vec<String> = listed.iter().map(|one| one.rel_path.clone()).collect();
            paths.sort();
            assert_eq!(paths, filter.paths(&cache).unwrap(), "{text}");
        }

        let by_date = filter("all").photos(&cache, Order::Date).unwrap();
        let dated = by_date.iter().take_while(|one| one.taken_at.is_some()).count();
        assert_eq!(dated, by_date.len() - 5, "the five undated photos come last");
        assert!(by_date[dated..].iter().all(|one| one.taken_at.is_none()));
        let dates: Vec<&String> = by_date[..dated]
            .iter()
            .filter_map(|one| one.taken_at.as_ref())
            .collect();
        assert!(dates.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(by_date.iter().all(|one| one.is_photo && one.content_id.is_some()));

        let by_name: Vec<String> = filter("all")
            .photos(&cache, Order::Name)
            .unwrap()
            .into_iter()
            .map(|one| one.rel_path)
            .collect();
        assert_eq!(by_name, filter("all").paths(&cache).unwrap());
    }

    #[test]
    fn finds_the_fixture_shapes() {
        let cache = scanned("shapes");
        let count = |text: &str| filter(text).count(&cache).unwrap();
        let all = crate::fixtures::photo_count() as i64;

        assert_eq!(count("all"), all);
        assert_eq!(count("no-gps"), all - 26, "twenty-six photos carry GPS");
        assert_eq!(count("no-date"), 5);
        assert_eq!(
            filter("date-off-folder").paths(&cache).unwrap(),
            [
                "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG",
                "China/2006-09-00 Besuch Ben/P1000001.JPG",
                "Denmark/2017-09-00 Autumn Walk/DSCF0101.JPG",
                "Denmark/2017-09-00 Autumn Walk/DSCF0102.JPG",
                "Germany/2013-05-18 Garden Party/P1060001.JPG",
                "Germany/2013-05-18 Garden Party/P1060002.JPG",
                "Greece/2010-04-10 Beach/scan0001.jpg",
                "Greece/2010-04-10 Beach/scan0002.jpg",
            ],
            "a folder a month off, a camera years off, scans a year off"
        );
        assert_eq!(count("no-tag"), 21);
        assert_eq!(count("no-location"), all - 1, "one photo names its city");
        assert_eq!(count("no-gps@Germany"), 5);
        assert_eq!(count("no-gps@Germany/2019-07-13 Sommerfest"), 2);
        assert_eq!(count("no-gps@Germ"), 0, "a folder is a whole name, not a prefix");
        assert_eq!(count("tag:mixed/Funny"), 1, "tags are compared as they are spelled");
        assert_eq!(count("tag:mixed"), 4);
        assert_eq!(count("sub-folder"), 3);
        assert_eq!(count("loose"), 1);
        assert_eq!(count("loose@China"), 1);
        assert_eq!(count("off-layout"), 0);
        assert_eq!(
            count("no-event"),
            all - 1 - 2,
            "every photo but the loose one and the two that name an event"
        );
        assert_eq!(
            filter("event-off-folder").paths(&cache).unwrap(),
            ["Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG"]
        );
    }

    #[test]
    fn off_the_layout_is_every_event_another_layout_does_not_fit() {
        let mut cache = scanned("layout");
        let all = crate::fixtures::photo_count() as i64;
        let count = |cache: &Cache, text: &str| filter(text).count(cache).unwrap();
        cache
            .follow_layout(&crate::layout::Layout::read("year/country").unwrap())
            .unwrap();
        assert_eq!(count(&cache, "loose"), 0, "China/ is no year");
        assert_eq!(
            count(&cache, "off-layout"),
            all,
            "every event, and the photo in a folder that is no year"
        );
        let (off, events) = cache
            .events_off(&crate::layout::Layout::read("year/country").unwrap())
            .unwrap();
        assert_eq!(off, events);
        cache.follow_layout(&crate::layout::Layout::default()).unwrap();
        assert_eq!(count(&cache, "off-layout"), 0);
        assert_eq!(count(&cache, "loose"), 1);
    }

    #[test]
    fn the_people_gap_counts_the_persons_not_the_people_tags() {
        let cache = scanned("people");
        let without = filter("no-people").paths(&cache).unwrap();
        assert!(
            without.contains(&"Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG".to_string()),
            "a people tag names no person"
        );
        assert!(!without.contains(&"Germany/2019-07-13 Sommerfest/IMAG0001.jpg".to_string()));
        assert!(
            !without.contains(&"Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG".to_string()),
            "a person without a box is a person"
        );
        assert_eq!(without.len(), crate::fixtures::photo_count() - 2);
    }

    #[test]
    fn a_person_is_one_part_that_combines_with_the_rest() {
        let cache = scanned("persons");
        let count = |text: &str| filter(text).count(&cache).unwrap();
        assert_eq!(count("person:Anna"), 1);
        assert_eq!(count("person:Mia"), 1);
        assert_eq!(count("person:Anna|Mia"), 2, "either of them");
        assert_eq!(count("person:Anna|Tom"), 1, "one photo of both is one photo");
        assert_eq!(count("person:anna"), 0, "a name is compared as it is spelled");
        assert_eq!(count("person:Anna@Germany"), 1);
        assert_eq!(count("person:Anna@Denmark"), 0);
        assert_eq!(count("tag:people/family+person:Anna"), 1);

        let chosen = Filter::all().with_tag(Some("people")).with_person(Some("Tom"));
        assert_eq!(chosen.to_string(), "tag:people+person:Tom");
        assert_eq!(chosen.persons(), [&["Tom".to_string()][..]]);
        assert_eq!(chosen.title(), "Photos tagged people, of Tom");
        assert_eq!(
            chosen.clone().with_person(Some("Mia")).to_string(),
            "tag:people+person:Mia"
        );
        assert_eq!(chosen.with_person(None).to_string(), "tag:people");
        assert_eq!(filter("person:Mia|Tom").title(), "Photos of Mia or Tom");
    }
}
