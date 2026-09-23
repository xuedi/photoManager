//! What the library is and what it is missing, in one look at the cache. Every number that can be
//! clicked carries the filter it counts, so a click shows exactly the photos the number promised.

use std::collections::BTreeMap;

use crate::browse::TagTree;
use crate::cache::{Cache, Result};
use crate::filter::{Filter, Gap, Kind};
use crate::scan::IssueKind;

/// How many photos a gap can be asked about, and how many of those have it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measure {
    pub of: i64,
    pub missing: i64,
}

impl Measure {
    /// The share that is there, from 0 to 1. Nothing to measure counts as complete.
    pub fn present(&self) -> f64 {
        match self.of {
            0 => 1.0,
            of => (of - self.missing) as f64 / of as f64,
        }
    }
}

/// A country or an event, with its gaps in the order of `Gap::ALL`.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub name: String,
    /// The folder inside the library, which is what a filter is narrowed to.
    pub folder: String,
    pub photos: i64,
    pub gaps: [Measure; 6],
    pub events: Vec<Place>,
}

impl Place {
    fn new(name: &str, folder: &str) -> Place {
        Place {
            name: name.to_string(),
            folder: folder.to_string(),
            photos: 0,
            gaps: [Measure::default(); 6],
            events: Vec::new(),
        }
    }

    pub fn gap(&self, gap: Gap) -> Measure {
        self.gaps[index(gap)]
    }

    pub fn filter(&self, gap: Gap) -> Filter {
        Filter::missing(gap).within(&self.folder)
    }

    fn add(&mut self, photos: i64, gaps: &[Measure; 6]) {
        self.photos += photos;
        for (sum, gap) in self.gaps.iter_mut().zip(gaps) {
            sum.of += gap.of;
            sum.missing += gap.missing;
        }
    }
}

/// Each gap and how much of the library has it.
pub type Coverage = Vec<(Gap, Measure)>;

/// Something untidy worth a look.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub title: String,
    pub detail: String,
    pub count: i64,
    pub filter: Filter,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Survey {
    pub photos: i64,
    pub events: i64,
    pub bytes: i64,
    /// The first and the last date a photo carries, `YYYY-MM-DD`.
    pub first: Option<String>,
    pub last: Option<String>,
    /// Camera models by how many photos they took, the unnamed ones as `None`.
    pub cameras: Vec<(Option<String>, i64)>,
    /// File extensions as they are spelled, by how many photos carry them.
    pub file_types: Vec<(String, i64)>,
    /// The whole library, in the order of `Gap::ALL`.
    pub coverage: Coverage,
    pub countries: Vec<Place>,
    pub tidy: Vec<Finding>,
}

impl Survey {
    pub fn take(cache: &Cache) -> Result<Survey> {
        let connection = cache.connection();
        let (photos, bytes, first, last) = connection.query_row(
            "SELECT count(*), coalesce(sum(size), 0), min(substr(taken_at, 1, 10)), max(substr(taken_at, 1, 10))
             FROM photo",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        let mut statement = connection
            .prepare("SELECT nullif(trim(camera_model), ''), count(*) FROM photo GROUP BY 1 ORDER BY 2 DESC, 1")?;
        let cameras = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>>>()?;

        let (coverage, countries) = gaps(cache)?;
        Ok(Survey {
            photos,
            events: cache.event_count()?,
            bytes,
            first,
            last,
            cameras,
            file_types: file_types(&cache.paths()?),
            coverage,
            countries,
            tidy: tidy(cache)?,
        })
    }

    /// The cameras that have a name.
    pub fn camera_count(&self) -> usize {
        self.cameras.iter().filter(|(model, _)| model.is_some()).count()
    }

    /// Every number on the dashboard that can be clicked, with what the click shows.
    pub fn numbers(&self) -> Vec<(i64, Filter)> {
        let mut numbers = vec![(self.photos, Filter::all())];
        numbers.extend(
            self.coverage
                .iter()
                .map(|(gap, measure)| (measure.missing, Filter::missing(*gap))),
        );
        for country in &self.countries {
            for place in std::iter::once(country).chain(&country.events) {
                numbers.extend(Gap::ALL.map(|gap| (place.gap(gap).missing, place.filter(gap))));
            }
        }
        numbers.extend(self.tidy.iter().map(|finding| (finding.count, finding.filter.clone())));
        numbers
    }
}

fn index(gap: Gap) -> usize {
    Gap::ALL
        .iter()
        .position(|each| *each == gap)
        .expect("every gap is listed")
}

/// The gaps of the whole library, and per country and event, from the predicates the filters use.
fn gaps(cache: &Cache) -> Result<(Coverage, Vec<Place>)> {
    let sums: Vec<String> = Gap::ALL
        .iter()
        .map(|gap| {
            format!(
                "sum(CASE WHEN {} THEN 1 ELSE 0 END), sum(CASE WHEN {} THEN 1 ELSE 0 END)",
                gap.measured(),
                gap.predicate()
            )
        })
        .collect();
    let sums = sums.join(", ");
    let measures = |row: &rusqlite::Row, from: usize| -> rusqlite::Result<[Measure; 6]> {
        let mut gaps = [Measure::default(); 6];
        for (at, gap) in gaps.iter_mut().enumerate() {
            gap.of = row.get::<_, Option<i64>>(from + at * 2)?.unwrap_or(0);
            gap.missing = row.get::<_, Option<i64>>(from + at * 2 + 1)?.unwrap_or(0);
        }
        Ok(gaps)
    };

    let mut statement = cache.connection().prepare(&format!(
        "SELECT country, event_dir, count(*), {sums} FROM photo p
         GROUP BY country, event_dir ORDER BY country, event_dir"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
            measures(row, 3)?,
        ))
    })?;

    // The whole library is the sum of its groups, photos outside any country included.
    let mut whole = Place::new("", "");
    let mut countries: Vec<Place> = Vec::new();
    for row in rows {
        let (country, event_dir, photos, gaps) = row?;
        whole.add(photos, &gaps);
        let Some(country) = country else {
            continue;
        };
        if countries.last().is_none_or(|last| last.folder != country) {
            countries.push(Place::new(&country, &country));
        }
        let place = countries.last_mut().expect("just pushed");
        place.add(photos, &gaps);
        if let Some(folder) = event_dir {
            let name = folder
                .strip_prefix(&format!("{country}/"))
                .unwrap_or(&folder)
                .to_string();
            let mut event = Place::new(&name, &folder);
            event.add(photos, &gaps);
            place.events.push(event);
        }
    }
    let coverage = Gap::ALL.iter().copied().zip(whole.gaps).collect();
    Ok((coverage, countries))
}

fn file_types(paths: &[String]) -> Vec<(String, i64)> {
    let mut counted: BTreeMap<String, i64> = BTreeMap::new();
    for path in paths {
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = match name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() => extension.to_string(),
            _ => String::new(),
        };
        *counted.entry(extension).or_default() += 1;
    }
    let mut types: Vec<(String, i64)> = counted.into_iter().collect();
    types.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    types
}

fn tidy(cache: &Cache) -> Result<Vec<Finding>> {
    let mut found = Vec::new();
    let mut add = |title: String, detail: String, filter: Filter| -> Result<()> {
        let count = filter.count(cache)?;
        if count > 0 {
            found.push(Finding {
                title,
                detail,
                count,
                filter,
            });
        }
        Ok(())
    };

    let mut statement = cache.connection().prepare("SELECT DISTINCT path FROM tag")?;
    let paths: Vec<String> = statement.query_map([], |row| row.get(0))?.collect::<Result<Vec<_>>>()?;
    let tags = TagTree::of(&paths);

    let mixed = tags.children("mixed").len();
    add(
        "Not yet sorted out of mixed".to_string(),
        format!("{mixed} tags in the mixed bucket"),
        Filter::of(Kind::Tagged(vec!["mixed".to_string()])),
    )?;
    for spellings in tags.case_twins() {
        add(
            spellings.join(" and "),
            "the same tag, spelled differently".to_string(),
            Filter::of(Kind::Tagged(spellings)),
        )?;
    }
    for (one, other) in tags.look_alikes() {
        add(
            format!("{one} and {other}"),
            "tags that look alike".to_string(),
            Filter::of(Kind::Tagged(vec![one, other])),
        )?;
    }

    add(
        "Loose files".to_string(),
        "directly in a country folder or the library, outside any event".to_string(),
        Filter::of(Kind::Loose),
    )?;
    add(
        "In event sub-folders".to_string(),
        "photos in a folder below their event's".to_string(),
        Filter::of(Kind::SubFolder),
    )?;
    add(
        "Folders without a date".to_string(),
        "where an event should be, a folder whose name carries no date".to_string(),
        Filter::of(Kind::OffConvention),
    )?;
    for (kind, title, detail) in [
        (IssueKind::Sidecar, "Sidecars", "XMP files next to the photos"),
        (IssueKind::NotAPhoto, "Not photos", "files that are not JPEG images"),
        (IssueKind::Unreadable, "Unreadable", "files that could not be read"),
        (
            IssueKind::DuplicateContent,
            "Duplicate content",
            "files with the same image data as another",
        ),
    ] {
        add(title.to_string(), detail.to_string(), Filter::of(Kind::Issue(kind)))?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_file_types_as_they_are_spelled() {
        let paths = ["a/1.JPG", "a/2.jpg", "b/3.JPG", "b/.hidden", "c/none"].map(String::from);
        assert_eq!(
            file_types(&paths),
            [(String::new(), 2), ("JPG".to_string(), 2), ("jpg".to_string(), 1)]
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod fixture_tests {
    use super::*;
    use crate::filter::tests::scanned;

    #[test]
    fn every_number_shows_exactly_its_photos() {
        let cache = scanned("survey-numbers");
        let survey = Survey::take(&cache).unwrap();
        let numbers = survey.numbers();
        assert!(numbers.len() > 50, "the survey has numbers to check");
        for (number, filter) in numbers {
            assert_eq!(filter.paths(&cache).unwrap().len() as i64, number, "{filter}");
            assert_eq!(filter.to_string().parse::<Filter>().unwrap(), filter);
        }
    }

    #[test]
    fn sums_up_the_fixture() {
        let cache = scanned("survey-totals");
        let survey = Survey::take(&cache).unwrap();
        let all = crate::fixtures::photo_count() as i64;

        assert_eq!(survey.photos, all);
        assert_eq!(survey.events, 9);
        assert!(survey.bytes > 0);
        assert_eq!(survey.first.as_deref(), Some("2006-08-21"));
        assert_eq!(survey.last.as_deref(), Some("2019-07-13"));
        assert_eq!(survey.cameras[0], (None, all - 5), "most fixture photos name no camera");
        assert_eq!(survey.camera_count(), 3);
        assert_eq!(survey.file_types, [("JPG".to_string(), 20), ("jpg".to_string(), 3)]);

        let coverage: BTreeMap<&str, Measure> = survey
            .coverage
            .iter()
            .map(|(gap, measure)| (gap.key(), *measure))
            .collect();
        assert_eq!(
            coverage["no-gps"],
            Measure {
                of: all,
                missing: all - 6
            }
        );
        assert_eq!(coverage["no-date"], Measure { of: all, missing: 2 });
        assert_eq!(coverage["date-off-folder"], Measure { of: 17, missing: 2 });
        assert_eq!(coverage["no-tag"], Measure { of: all, missing: 6 });
        assert_eq!(
            coverage["no-people"],
            Measure {
                of: all,
                missing: all - 3
            }
        );
        assert_eq!(
            coverage["no-location"],
            Measure {
                of: all,
                missing: all - 1
            }
        );
    }

    #[test]
    fn a_loose_file_counts_to_its_country_and_no_event() {
        let cache = scanned("survey-places");
        let survey = Survey::take(&cache).unwrap();
        let names: Vec<&str> = survey.countries.iter().map(|place| place.name.as_str()).collect();
        assert_eq!(names, ["China", "Denmark", "Germany", "Greece", "Ireland"]);

        let china = &survey.countries[0];
        assert_eq!(china.photos, 7, "six in events and the loose one");
        assert_eq!(china.events.iter().map(|event| event.photos).sum::<i64>(), 6);
        assert_eq!(china.events[0].name, "2006-09-00 Besuch Ben");
        assert_eq!(china.events[0].folder, "China/2006-09-00 Besuch Ben");
        assert_eq!(china.events[0].gap(Gap::DateOffFolder), Measure { of: 2, missing: 2 });
    }

    #[test]
    fn finds_what_is_untidy_and_nothing_else() {
        let cache = scanned("survey-tidy");
        let survey = Survey::take(&cache).unwrap();
        let found: Vec<(&str, i64)> = survey
            .tidy
            .iter()
            .map(|finding| (finding.title.as_str(), finding.count))
            .collect();
        assert_eq!(
            found,
            [
                ("Not yet sorted out of mixed", 4),
                ("mixed/Funny and mixed/funny", 2),
                ("People and people", 3),
                ("mixed/discusting and mixed/disgusting", 2),
                ("places/inGreece/Atens and places/inGreece/athens", 3),
                ("Loose files", 1),
                ("In event sub-folders", 2),
            ]
        );
    }
}
