//! The jobs that keep what the application knows next to the photos in step: the scan, the
//! thumbnails, the place data and the people from Immich. For each, when it last ran and whether
//! it is worth running again. Nothing here runs a job.

use std::path::Path;

use crate::cache::{Cache, Ran};
use crate::dates;
use crate::scan::{Summary, Thumbnails};

/// The keys the cache keeps the runs under.
pub const SCAN: &str = "scan";
pub const THUMBNAILS: &str = "thumbnails";
/// Photos were written: their rows are forgotten until a scan reads them again.
pub const WRITTEN: &str = "written";
/// A scan found photos it did not know.
pub const ADDED: &str = "added";

const PLACES_DAYS: i64 = 365;
const PEOPLE_DAYS: i64 = 30;
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    Scan,
    Thumbnails,
    Places,
    People,
}

impl Job {
    pub const ALL: [Job; 4] = [Job::Scan, Job::Thumbnails, Job::Places, Job::People];

    pub fn title(self) -> &'static str {
        match self {
            Job::Scan => "Scan",
            Job::Thumbnails => "Thumbnails",
            Job::Places => "Places",
            Job::People => "People",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Never,
    Fine,
    Worth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub job: Job,
    pub state: State,
    /// One short line under the name: when it ran, or what is missing.
    pub caption: String,
    /// Why it is worth running, when it is.
    pub reason: Option<String>,
    /// What the last run did, a label and a value each.
    pub facts: Vec<(&'static str, String)>,
}

/// Everything the states are worked out from. The times are UTC stamps in the one format.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub now: String,
    pub scan: Option<Ran>,
    pub written: Option<String>,
    pub added: Option<String>,
    /// A folder that changed after the last scan began, `""` for the library's own.
    pub changed: Option<String>,
    pub photos: i64,
    /// The different pictures among the photos, one thumbnail each.
    pub images: i64,
    pub thumbnails: i64,
    pub filled: Option<Ran>,
    pub places: i64,
    pub dump_date: Option<String>,
    pub imported_at: Option<String>,
    /// How many named persons the snapshot holds, and when it was fetched.
    pub people: Option<(usize, String)>,
}

impl Facts {
    /// What the cache remembers; the rest is filled in by whoever holds the other stores.
    pub fn read(cache: &Cache) -> crate::cache::Result<Facts> {
        Ok(Facts {
            now: crate::clock::now(),
            scan: cache.ran(SCAN)?,
            written: cache.ran(WRITTEN)?.map(|ran| ran.at),
            added: cache.ran(ADDED)?.map(|ran| ran.at),
            filled: cache.ran(THUMBNAILS)?,
            photos: cache.photo_count()?,
            images: cache.image_count()?,
            ..Facts::default()
        })
    }
}

/// What a scan keeps about itself.
pub fn scanned(summary: &Summary, begun: &str) -> String {
    serde_json::json!({
        "begun": begun,
        "photos": summary.photos,
        "read": summary.read,
        "added": summary.added,
        "changed": summary.changed,
        "moved": summary.moved,
        "gone": summary.gone,
        "seconds": summary.seconds,
        "cancelled": summary.cancelled,
    })
    .to_string()
}

/// What a fill-in of the thumbnails keeps about itself.
pub fn filled(done: &Thumbnails) -> String {
    serde_json::json!({ "made": done.made, "failed": done.failed, "cancelled": done.cancelled }).to_string()
}

/// When the last scan began, which is what a later change is measured against.
pub fn scan_begun(scan: &Ran) -> Option<String> {
    let said: serde_json::Value = serde_json::from_str(&scan.what).ok()?;
    said["begun"].as_str().map(str::to_string)
}

pub fn statuses(facts: &Facts) -> Vec<Status> {
    Job::ALL.iter().map(|job| status(*job, facts)).collect()
}

pub fn status(job: Job, facts: &Facts) -> Status {
    match job {
        Job::Scan => scan(facts),
        Job::Thumbnails => thumbnails(facts),
        Job::Places => places(facts),
        Job::People => people(facts),
    }
}

fn scan(facts: &Facts) -> Status {
    let Some(ran) = &facts.scan else {
        return never(Job::Scan);
    };
    let said: serde_json::Value = serde_json::from_str(&ran.what).unwrap_or_default();
    let number = |key: &str| said[key].as_u64().unwrap_or_default();
    let mut facts_of = vec![
        (
            "Last run",
            format!("{}, took {} s", dates::local(&ran.at), number("seconds")),
        ),
        ("Photos", facts.photos.to_string()),
        (
            "Read",
            format!(
                "{} ({} new, {} changed, {} moved, {} gone)",
                number("read"),
                number("added"),
                number("changed"),
                number("moved"),
                number("gone")
            ),
        ),
    ];
    let reason = if said["cancelled"].as_bool().unwrap_or(false) {
        Some("The last scan was stopped before the end".to_string())
    } else if facts
        .written
        .as_deref()
        .is_some_and(|written| written > ran.at.as_str())
    {
        Some("Photos were written since".to_string())
    } else {
        facts.changed.as_ref().map(|dir| match dir.is_empty() {
            true => "The library folder changed since".to_string(),
            false => format!("{dir} changed since"),
        })
    };
    if let Some(reason) = &reason {
        facts_of.push(("Status", reason.clone()));
    }
    Status {
        job: Job::Scan,
        state: worth_if(reason.is_some()),
        caption: dates::local(&ran.at),
        reason,
        facts: facts_of,
    }
}

fn thumbnails(facts: &Facts) -> Status {
    if facts.photos == 0 {
        return Status {
            caption: "nothing to make yet".to_string(),
            ..never(Job::Thumbnails)
        };
    }
    let missing = (facts.images - facts.thumbnails).max(0);
    let mut facts_of = vec![(
        "Made",
        format!("{} of {}", facts.thumbnails.min(facts.images), facts.images),
    )];
    if let Some(filled) = &facts.filled {
        facts_of.push(("Last filled in", dates::local(&filled.at)));
    }
    let reason = (missing > 0).then(|| match missing {
        1 => "1 photo has no thumbnail".to_string(),
        missing => format!("{missing} photos have no thumbnail"),
    });
    Status {
        job: Job::Thumbnails,
        state: worth_if(reason.is_some()),
        caption: match missing {
            0 => "all made".to_string(),
            missing => format!("{missing} missing"),
        },
        reason,
        facts: facts_of,
    }
}

fn places(facts: &Facts) -> Status {
    let Some(imported) = facts.imported_at.as_ref().filter(|_| facts.places > 0) else {
        return never(Job::Places);
    };
    let mut facts_of = vec![("Places", facts.places.to_string())];
    if let Some(dump) = &facts.dump_date {
        facts_of.push(("Dumps of", dump.clone()));
    }
    facts_of.push(("Imported", dates::local(imported)));
    let reason =
        (days(imported, &facts.now) > PLACES_DAYS).then(|| "The place data is more than a year old".to_string());
    Status {
        job: Job::Places,
        state: worth_if(reason.is_some()),
        caption: dates::local(imported),
        reason,
        facts: facts_of,
    }
}

fn people(facts: &Facts) -> Status {
    let Some((named, fetched)) = facts.people.as_ref().filter(|(_, fetched)| !fetched.is_empty()) else {
        return never(Job::People);
    };
    let facts_of = vec![("Named persons", named.to_string()), ("Fetched", dates::local(fetched))];
    let reason = if days(fetched, &facts.now) > PEOPLE_DAYS {
        Some(format!("Fetched more than {PEOPLE_DAYS} days ago"))
    } else if facts.added.as_deref().is_some_and(|added| added > fetched.as_str()) {
        Some("The scan found new photos since".to_string())
    } else {
        None
    };
    Status {
        job: Job::People,
        state: worth_if(reason.is_some()),
        caption: dates::local(fetched),
        reason,
        facts: facts_of,
    }
}

fn never(job: Job) -> Status {
    Status {
        job,
        state: State::Never,
        caption: "never run".to_string(),
        reason: None,
        facts: Vec::new(),
    }
}

fn worth_if(worth: bool) -> State {
    match worth {
        true => State::Worth,
        false => State::Fine,
    }
}

/// Whole days from one stamp to another, 0 when either does not read.
fn days(from: &str, to: &str) -> i64 {
    match (dates::parse(from), dates::parse(to)) {
        (Ok(from), Ok(to)) => (dates::seconds(to) - dates::seconds(from)) / DAY,
        _ => 0,
    }
}

/// The first folder of the library, hidden ones left out as the scan leaves them, that changed
/// after `since` (UTC): something was added to it, taken out or renamed. Only the folders are
/// looked at, no file is opened. `""` stands for the library's own folder.
pub fn changed_since(library: &Path, since: &str) -> Option<String> {
    let since = dates::seconds(dates::parse(since).ok()?);
    walkdir::WalkDir::new(library)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_dir())
        .find(|entry| {
            entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|modified| modified.as_secs() as i64 > since)
        })
        .map(|entry| {
            entry
                .path()
                .strip_prefix(library)
                .map(|relative| relative.to_string_lossy().to_string())
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-09-28 12:00:00";

    fn facts() -> Facts {
        Facts {
            now: NOW.to_string(),
            ..Facts::default()
        }
    }

    fn ran(at: &str, summary: Summary) -> Option<Ran> {
        Some(Ran {
            at: at.to_string(),
            what: scanned(&summary, at),
        })
    }

    fn state(job: Job, facts: &Facts) -> State {
        status(job, facts).state
    }

    #[test]
    fn nothing_run_is_never_run() {
        for job in Job::ALL {
            let status = status(job, &facts());
            assert_eq!(status.state, State::Never, "{job:?}");
            assert_eq!(status.reason, None);
        }
    }

    #[test]
    fn a_finished_scan_is_fine_and_says_what_it_read() {
        let facts = Facts {
            scan: ran(
                "2026-09-28 10:00:00",
                Summary {
                    read: 12,
                    added: 3,
                    seconds: 41,
                    ..Summary::default()
                },
            ),
            photos: 120,
            ..facts()
        };
        let status = status(Job::Scan, &facts);
        assert_eq!(status.state, State::Fine);
        assert_eq!(status.caption, dates::local("2026-09-28 10:00:00"));
        assert!(
            status
                .facts
                .iter()
                .any(|(label, value)| *label == "Read" && value.starts_with("12 (3 new"))
        );
        assert!(status.facts.iter().any(|(_, value)| value.ends_with("took 41 s")));
    }

    #[test]
    fn a_stopped_scan_is_worth_running() {
        let facts = Facts {
            scan: ran(
                "2026-09-28 10:00:00",
                Summary {
                    cancelled: true,
                    ..Summary::default()
                },
            ),
            ..facts()
        };
        assert_eq!(state(Job::Scan, &facts), State::Worth);
    }

    #[test]
    fn a_write_after_the_scan_makes_it_worth_running_and_the_next_scan_clears_it() {
        let mut facts = Facts {
            scan: ran("2026-09-28 10:00:00", Summary::default()),
            written: Some("2026-09-28 10:05:00".to_string()),
            ..facts()
        };
        assert_eq!(state(Job::Scan, &facts), State::Worth);
        assert_eq!(
            status(Job::Scan, &facts).reason.as_deref(),
            Some("Photos were written since")
        );
        facts.scan = ran("2026-09-28 10:06:00", Summary::default());
        assert_eq!(state(Job::Scan, &facts), State::Fine);
    }

    #[test]
    fn a_changed_folder_makes_the_scan_worth_running() {
        let facts = Facts {
            scan: ran("2026-09-28 10:00:00", Summary::default()),
            changed: Some("Atlantis/2024-05-01 Picnic".to_string()),
            ..facts()
        };
        assert_eq!(
            status(Job::Scan, &facts).reason.as_deref(),
            Some("Atlantis/2024-05-01 Picnic changed since")
        );
    }

    #[test]
    fn missing_thumbnails_are_worth_making() {
        let mut facts = Facts {
            photos: 11,
            images: 10,
            thumbnails: 7,
            ..facts()
        };
        let status = status(Job::Thumbnails, &facts);
        assert_eq!((status.state, status.caption.as_str()), (State::Worth, "3 missing"));
        facts.thumbnails = 10;
        let status = super::status(Job::Thumbnails, &facts);
        assert_eq!(
            (status.state, status.caption.as_str()),
            (State::Fine, "all made"),
            "two photos of one picture share a thumbnail"
        );
        facts.thumbnails = 12;
        assert_eq!(
            state(Job::Thumbnails, &facts),
            State::Fine,
            "stale thumbnails are no gap"
        );
    }

    #[test]
    fn place_data_older_than_a_year_is_worth_fetching_again() {
        let mut facts = Facts {
            places: 1000,
            imported_at: Some("2026-01-01 00:00:00".to_string()),
            dump_date: Some("2025-12-31 00:00:00".to_string()),
            ..facts()
        };
        assert_eq!(state(Job::Places, &facts), State::Fine);
        facts.imported_at = Some("2025-09-01 00:00:00".to_string());
        assert_eq!(state(Job::Places, &facts), State::Worth);
        facts.places = 0;
        assert_eq!(state(Job::Places, &facts), State::Never, "an empty import is none");
    }

    #[test]
    fn people_are_worth_fetching_when_old_or_when_photos_came_in_since() {
        let mut facts = Facts {
            people: Some((5, "2026-09-20 00:00:00".to_string())),
            ..facts()
        };
        assert_eq!(state(Job::People, &facts), State::Fine);
        facts.added = Some("2026-09-19 00:00:00".to_string());
        assert_eq!(state(Job::People, &facts), State::Fine, "added before the fetch");
        facts.added = Some("2026-09-21 00:00:00".to_string());
        assert_eq!(state(Job::People, &facts), State::Worth);
        facts.added = None;
        facts.people = Some((5, "2026-08-01 00:00:00".to_string()));
        assert_eq!(state(Job::People, &facts), State::Worth);
    }

    #[test]
    fn the_scan_keeps_when_it_began() {
        let ran = ran("2026-09-28 10:00:00", Summary::default()).unwrap();
        assert_eq!(scan_begun(&ran).as_deref(), Some("2026-09-28 10:00:00"));
    }

    #[test]
    fn a_folder_touched_after_the_scan_is_found() {
        let root = std::env::temp_dir().join("photomanager-upkeep-changed");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Atlantis/2024-05-01 Picnic")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        let later = crate::clock::stamp(std::time::SystemTime::now() + std::time::Duration::from_secs(60));
        assert_eq!(changed_since(&root, &later), None, "nothing is newer than the future");
        let earlier = crate::clock::stamp(std::time::SystemTime::now() - std::time::Duration::from_secs(60));
        assert_eq!(
            changed_since(&root, &earlier).as_deref(),
            Some(""),
            "the library folder itself"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
