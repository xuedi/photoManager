//! What the library is and what it is missing, in one look at the cache. Every number that can be
//! clicked carries the filter it counts, so a click shows exactly the photos the number promised.

use std::collections::BTreeMap;

use crate::browse::TagTree;
use crate::cache::{Cache, Result};
use crate::checks::Check;
use crate::filter::{Filter, Gap, Kind};
use crate::scan::IssueKind;
use crate::tools::neighbour::{self, Neighboured};

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
    pub gaps: [Measure; Gap::COUNT],
    pub events: Vec<Place>,
}

impl Place {
    fn new(name: &str, folder: &str) -> Place {
        Place {
            name: name.to_string(),
            folder: folder.to_string(),
            photos: 0,
            gaps: [Measure::default(); Gap::COUNT],
            events: Vec::new(),
        }
    }

    pub fn gap(&self, gap: Gap) -> Measure {
        self.gaps[index(gap)]
    }

    pub fn filter(&self, gap: Gap) -> Filter {
        Filter::missing(gap).within(&self.folder)
    }

    fn add(&mut self, photos: i64, gaps: &[Measure; Gap::COUNT]) {
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

/// How much of the library sits exactly where the layout puts it, and each way it falls short.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Aligned {
    pub events: i64,
    /// The events with a photo that is not aligned.
    pub off_events: i64,
    /// The photos that are not aligned, which is what the line shows.
    pub photos: i64,
    /// One finding per reason; an event or a photo may be under several.
    pub reasons: Vec<Finding>,
}

impl Aligned {
    pub fn filter() -> Filter {
        Filter::of(Kind::Unaligned)
    }

    /// The share of the events that are aligned, from 0 to 1.
    pub fn present(&self) -> f64 {
        match self.events {
            0 => 1.0,
            events => (events - self.off_events) as f64 / events as f64,
        }
    }
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
    pub aligned: Aligned,
    pub countries: Vec<Place>,
    pub tidy: Vec<Finding>,
    /// The events where a photo measured its position and others did not.
    pub neighbours: Vec<Neighboured>,
    /// What is left to do for the photos without GPS, part by part: empty when the place check
    /// is older than the last scan and the parts would not add up to the gap.
    pub gps_left: Vec<(Check, i64)>,
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
        let gps_left = gps_left(cache, &coverage)?;
        Ok(Survey {
            photos,
            events: cache.event_count()?,
            bytes,
            first,
            last,
            cameras,
            file_types: file_types(&cache.paths()?),
            coverage,
            aligned: aligned(cache)?,
            countries,
            tidy: tidy(cache)?,
            neighbours: neighbour::events(cache)?,
            gps_left,
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
        numbers.push((self.aligned.photos, Aligned::filter()));
        numbers.extend(
            self.aligned
                .reasons
                .iter()
                .map(|finding| (finding.count, finding.filter.clone())),
        );
        numbers.extend(self.tidy.iter().map(|finding| (finding.count, finding.filter.clone())));
        numbers.extend(
            self.gps_left
                .iter()
                .map(|(check, count)| (*count, Filter::of(Kind::Checked(*check)))),
        );
        numbers
    }
}

fn index(gap: Gap) -> usize {
    Gap::ALL
        .iter()
        .position(|each| *each == gap)
        .expect("every gap is listed")
}

/// The gaps of the whole library, and per top folder and event, from the predicates the filters
/// use. The top folder is whatever the layout starts with: a country, a year, an event.
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
    let measures = |row: &rusqlite::Row, from: usize| -> rusqlite::Result<[Measure; Gap::COUNT]> {
        let mut gaps = [Measure::default(); Gap::COUNT];
        for (at, gap) in gaps.iter_mut().enumerate() {
            gap.of = row.get::<_, Option<i64>>(from + at * 2)?.unwrap_or(0);
            gap.missing = row.get::<_, Option<i64>>(from + at * 2 + 1)?.unwrap_or(0);
        }
        Ok(gaps)
    };

    let mut statement = cache.connection().prepare(&format!(
        "SELECT CASE WHEN instr(rel_path, '/') > 0 THEN substr(rel_path, 1, instr(rel_path, '/') - 1) END AS top,
            event_dir, count(*), {sums} FROM photo p
         GROUP BY top, event_dir ORDER BY top, event_dir"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
            measures(row, 3)?,
        ))
    })?;

    // The whole library is the sum of its groups, photos outside any folder included.
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
        if let Some(folder) = event_dir.filter(|folder| *folder != country) {
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

/// The parts of what is left for GPS, when they add up to the gap as it is now.
fn gps_left(cache: &Cache, coverage: &Coverage) -> Result<Vec<(Check, i64)>> {
    let gap = coverage
        .iter()
        .find(|(gap, _)| *gap == Gap::Gps)
        .map(|(_, measure)| measure.missing)
        .unwrap_or_default();
    let mut parts = Vec::new();
    for check in Check::GPS {
        parts.push((check, Filter::of(Kind::Checked(check)).count(cache)?));
    }
    let sum: i64 = parts.iter().map(|(_, count)| count).sum();
    Ok(match gap > 0 && sum == gap {
        true => parts,
        false => Vec::new(),
    })
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

/// The events that hold a photo of the set.
fn events_of(cache: &Cache, filter: &Filter) -> Result<i64> {
    let (ids, params) = filter.ids();
    cache.connection().query_row(
        &format!("SELECT count(DISTINCT event_dir) FROM photo WHERE id IN ({ids})"),
        rusqlite::params_from_iter(params.iter()),
        |row| row.get(0),
    )
}

fn events(count: i64) -> String {
    match count {
        1 => "1 event".to_string(),
        count => format!("{count} events"),
    }
}

fn aligned(cache: &Cache) -> Result<Aligned> {
    let layout = cache.layout().clone();
    let mut reasons = Vec::new();
    let mut add = |title: String, detail: String, filter: Filter, of_events: bool| -> Result<()> {
        let count = filter.count(cache)?;
        if count > 0 {
            let detail = match of_events {
                true => format!("{detail}, in {}", events(events_of(cache, &filter)?)),
                false => detail,
            };
            reasons.push(Finding {
                title,
                detail,
                count,
                filter,
            });
        }
        Ok(())
    };
    add(
        "Off the layout".to_string(),
        "events in other folders than the layout says, and folders without a date".to_string(),
        Filter::of(Kind::OffLayout),
        false,
    )?;
    add(
        "Loose files".to_string(),
        "in a folder of the layout or the library root, outside any event".to_string(),
        Filter::of(Kind::Loose),
        false,
    )?;
    for level in layout.levels.iter().filter(|level| level.optional) {
        let title = level.component.title();
        add(
            format!("No {} folder", title.to_lowercase()),
            format!("the {} level is left out", title.to_lowercase()),
            Filter::of(Kind::NoLevel(level.component.key())),
            true,
        )?;
    }
    if !layout.sub_folders {
        add(
            "In event sub-folders".to_string(),
            "flattened into their event once every photo has a date and a position".to_string(),
            Filter::of(Kind::SubFolder),
            true,
        )?;
    }
    add(
        "Not named by their date".to_string(),
        "photos whose file name is not the date they were taken".to_string(),
        Filter::of(Kind::OffName),
        true,
    )?;
    add(
        "Event disagrees with the folder".to_string(),
        "photos naming another event than their folder".to_string(),
        Filter::missing(Gap::EventOffFolder),
        true,
    )?;
    let unaligned = Aligned::filter();
    Ok(Aligned {
        events: cache.event_count()?,
        off_events: events_of(cache, &unaligned)?,
        photos: unaligned.count(cache)?,
        reasons,
    })
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

    for check in std::iter::once(Check::PlaceDisagrees).chain(Check::KEPT) {
        add(
            check.title().to_string(),
            check.detail().to_string(),
            Filter::of(Kind::Checked(check)),
        )?;
    }
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
        assert_eq!(survey.events, 15);
        assert!(survey.bytes > 0);
        assert_eq!(survey.first.as_deref(), Some("2006-08-21"));
        assert_eq!(survey.last.as_deref(), Some("2019-07-13"));
        assert_eq!(
            survey.cameras[0],
            (None, all - 24),
            "most fixture photos name no camera"
        );
        assert_eq!(survey.camera_count(), 6);
        assert_eq!(survey.file_types, [("JPG".to_string(), 37), ("jpg".to_string(), 7)]);

        let coverage: BTreeMap<&str, Measure> = survey
            .coverage
            .iter()
            .map(|(gap, measure)| (gap.key(), *measure))
            .collect();
        assert_eq!(
            coverage["no-gps"],
            Measure {
                of: all,
                missing: all - 26
            }
        );
        assert_eq!(coverage["no-date"], Measure { of: all, missing: 5 });
        assert_eq!(coverage["date-off-folder"], Measure { of: 35, missing: 8 });
        assert_eq!(coverage["no-tag"], Measure { of: all, missing: 21 });
        assert_eq!(
            coverage["no-people"],
            Measure {
                of: all,
                missing: all - 2
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
    fn the_place_check_is_a_finding_and_what_is_left_for_gps_adds_up() {
        let mut library = crate::tools::testing::Library::new("survey-checked");
        let stale = Survey::take(&library.cache).unwrap();
        assert!(
            stale.gps_left.is_empty(),
            "no check yet, so no parts that would not add up"
        );
        crate::checks::run(&mut library.cache, Some(&crate::tools::testing::geo())).unwrap();
        let survey = Survey::take(&library.cache).unwrap();
        let disagree = survey
            .tidy
            .iter()
            .find(|finding| finding.title == Check::PlaceDisagrees.title())
            .expect("the finding");
        assert_eq!(disagree.count, 1);
        let gap = survey.coverage.iter().find(|(gap, _)| *gap == Gap::Gps).unwrap().1;
        assert_eq!(survey.gps_left.len(), 4);
        assert_eq!(survey.gps_left.iter().map(|(_, count)| count).sum::<i64>(), gap.missing);
        for (number, filter) in survey.numbers() {
            assert_eq!(filter.paths(&library.cache).unwrap().len() as i64, number, "{filter}");
        }
    }

    #[test]
    fn counts_the_events_where_a_neighbour_knows_the_position() {
        let cache = scanned("survey-neighbours");
        let survey = Survey::take(&cache).unwrap();
        let events: Vec<(&str, usize)> = survey
            .neighbours
            .iter()
            .map(|event| (event.event.as_str(), event.measured))
            .collect();
        assert_eq!(
            events,
            [
                ("Germany/2016-06-00 Harbour Walk", 3),
                ("Germany/2018-05-12 Canal Tour", 2),
                ("Germany/2019-07-13 Sommerfest", 1),
            ]
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
                ("mixed/discusting and mixed/disgusting", 3),
                ("places/inGreece/Atens and places/inGreece/athens", 3),
            ]
        );
    }

    #[test]
    fn says_how_much_is_aligned_with_the_layout_and_why_the_rest_is_not() {
        let mut cache = scanned("survey-aligned");
        let survey = Survey::take(&cache).unwrap();
        let reasons = |survey: &Survey| -> Vec<(String, i64)> {
            survey
                .aligned
                .reasons
                .iter()
                .map(|finding| (finding.title.clone(), finding.count))
                .collect()
        };
        assert_eq!(
            (survey.aligned.events, survey.aligned.off_events, survey.aligned.photos),
            (15, 15, 44),
            "no fixture event has a city folder"
        );
        assert_eq!(
            reasons(&survey),
            [
                ("Loose files".to_string(), 1),
                ("No city folder".to_string(), 42),
                ("In event sub-folders".to_string(), 3),
                ("Not named by their date".to_string(), 39),
                ("Event disagrees with the folder".to_string(), 1),
            ]
        );
        assert_eq!(
            survey.aligned.reasons[1].detail,
            "the city level is left out, in 14 events"
        );

        cache
            .follow_layout(&crate::layout::Layout::read("country/city?/*").unwrap())
            .unwrap();
        let grouped = Survey::take(&cache).unwrap();
        assert!(
            !reasons(&grouped)
                .iter()
                .any(|(title, _)| title == "In event sub-folders"),
            "allowed, a sub-folder only groups"
        );
        assert_eq!(Filter::of(Kind::SubFolder).count(&cache).unwrap(), 0);
        for (number, filter) in grouped.numbers() {
            assert_eq!(filter.paths(&cache).unwrap().len() as i64, number, "{filter}");
        }
    }
}
