//! What a tool would change, before anything is written.
//!
//! A tool decides what a set of photos should say and hands the decision over as a `ChangeSet`.
//! Nothing else about a tool reaches the write engine: the preview knows only this type, and apply
//! feeds the rows the user kept back to the engine. So every tool inherits the preview, the
//! traffic estimate, the confirmation and the undo without asking for them.
//!
//! A change set is built from the [cache](crate::cache) alone, which is what makes a preview over
//! a whole library a database query instead of thousands of ExifTool runs. The exact tag-level
//! diff of one photo is fetched per row on demand, and a write always re-reads the file anyway, so
//! a row that has gone stale becomes a harmless `skipped` or `refused` rather than a wrong write.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::cache::{Cache, Said, Stated};
use crate::journal::{self, Journal, Kind};
use crate::write::{self, Assignment, Change, Engine, Field, Outcome, Summary, Target, change};

/// How far back the undo looks for the pass it can take back.
const RECENT: i64 = 50;

const NONE: &str = "none";
/// What the cache cannot answer. The exact diff of the row is the only honest answer there.
pub const UNKNOWN: &str = "unknown";

/// What a tool decided for one photo.
#[derive(Debug, Clone)]
pub struct Wanted {
    pub rel_path: String,
    pub change: Change,
}

impl Wanted {
    pub fn new(rel_path: impl Into<String>, change: Change) -> Wanted {
        Wanted {
            rel_path: rel_path.into(),
            change,
        }
    }
}

/// What would become of one photo, and once it has been applied, what did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It would be written.
    Change,
    /// It already says what the change wants, as far as the cache knows.
    Nothing,
    /// It will not be touched, and why.
    Refused(String),
    /// What became of it when the change set was applied.
    Done(Outcome),
}

impl Verdict {
    /// One word for a table, and the reason where there is one.
    pub fn tells(&self) -> String {
        match self {
            Verdict::Change => "would change".to_string(),
            Verdict::Nothing => "nothing to do".to_string(),
            Verdict::Refused(why) => format!("refused: {why}"),
            Verdict::Done(Outcome::Written) => "written".to_string(),
            Verdict::Done(Outcome::Skipped) => "nothing to do".to_string(),
            Verdict::Done(Outcome::Refused(why)) => format!("refused: {why}"),
            Verdict::Done(Outcome::Failed(why)) => format!("failed: {why}"),
        }
    }
}

/// One field of one photo, the way a person thinks about it: `location: none -> 39.90420, …`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub what: &'static str,
    /// What the photo says now. `None` where the cache does not keep that field.
    pub before: Option<String>,
    pub after: String,
}

impl Difference {
    /// Whether the cache is sure the photo already says this.
    pub fn settled(&self) -> bool {
        self.before.as_deref() == Some(self.after.as_str())
    }

    pub fn tells(&self) -> String {
        format!(
            "{}: {} -> {}",
            self.what,
            self.before.as_deref().unwrap_or(UNKNOWN),
            self.after
        )
    }
}

/// One photo in a change set.
#[derive(Debug, Clone)]
pub struct Row {
    pub rel_path: String,
    /// What the change was built against. A photo whose image data moved since is refused.
    pub content_id: String,
    pub size: u64,
    pub change: Change,
    pub differences: Vec<Difference>,
    pub verdict: Verdict,
    selected: bool,
}

impl Row {
    pub fn selected(&self) -> bool {
        self.selected
    }

    pub fn would_change(&self) -> bool {
        self.verdict == Verdict::Change
    }

    /// The whole change in one line, the way a person reads it.
    pub fn tells(&self) -> String {
        match self.differences.is_empty() {
            true => "nothing".to_string(),
            false => self
                .differences
                .iter()
                .map(Difference::tells)
                .collect::<Vec<String>>()
                .join("; "),
        }
    }
}

/// What a change set adds up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub photos: usize,
    pub change: usize,
    pub nothing: usize,
    pub refused: usize,
    pub written: usize,
    pub failed: usize,
    pub selected: usize,
    /// Nextcloud re-uploads the whole file for every edit, so this is the sum of the file sizes of
    /// the selected rows that would actually change, not a guess at the size of the difference.
    pub traffic: u64,
}

#[derive(Debug, Clone)]
pub struct ChangeSet {
    pub title: String,
    pub rows: Vec<Row>,
}

impl ChangeSet {
    /// Reads the cache, never a photo, and writes nothing at all.
    pub fn build(cache: &Cache, title: &str, wanted: &[Wanted]) -> crate::cache::Result<ChangeSet> {
        let paths: Vec<String> = wanted.iter().map(|one| one.rel_path.clone()).collect();
        let known = cache.stated(&paths)?;
        Ok(ChangeSet {
            title: title.to_string(),
            rows: wanted.iter().map(|one| row(one, known.get(&one.rel_path))).collect(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts {
            photos: self.rows.len(),
            ..Counts::default()
        };
        for row in &self.rows {
            match &row.verdict {
                Verdict::Change => counts.change += 1,
                Verdict::Nothing | Verdict::Done(Outcome::Skipped) => counts.nothing += 1,
                Verdict::Refused(_) | Verdict::Done(Outcome::Refused(_)) => counts.refused += 1,
                Verdict::Done(Outcome::Written) => counts.written += 1,
                Verdict::Done(Outcome::Failed(_)) => counts.failed += 1,
            }
            if row.selected && row.would_change() {
                counts.selected += 1;
                counts.traffic += row.size;
            }
        }
        counts
    }

    /// A row that would not change anything cannot be selected, whatever is asked.
    pub fn select(&mut self, index: usize, selected: bool) -> bool {
        match self.rows.get_mut(index) {
            Some(row) if row.would_change() => {
                row.selected = selected;
                true
            }
            _ => false,
        }
    }

    pub fn select_all(&mut self) {
        for row in &mut self.rows {
            row.selected = row.verdict == Verdict::Change;
        }
    }

    pub fn select_none(&mut self) {
        for row in &mut self.rows {
            row.selected = false;
        }
    }

    /// The photos an apply would be handed: the selected rows and nothing else.
    pub fn targets(&self, library: &Path) -> Vec<Target> {
        self.rows
            .iter()
            .filter(|row| row.selected && row.would_change())
            .map(|row| Target {
                path: library.join(&row.rel_path),
                content_id: row.content_id.clone(),
                change: row.change.clone(),
            })
            .collect()
    }

    /// The exact tag-level diff of one row: one ExifTool read, the same code path a write takes,
    /// and nothing written or journaled. A refusal comes back as its reason.
    pub fn exact(&self, index: usize, engine: &mut Engine) -> Result<Vec<Assignment>, String> {
        let row = self.rows.get(index).ok_or("there is no such row")?;
        let target = Target {
            path: engine.library().join(&row.rel_path),
            content_id: row.content_id.clone(),
            change: row.change.clone(),
        };
        engine.dry_run(&target).map_err(|error| error.to_string())
    }

    /// Carries what became of every photo back into the rows it came from.
    pub fn settle(&mut self, summary: &Summary) {
        let outcomes: HashMap<&str, &Outcome> = summary
            .outcomes
            .iter()
            .map(|(rel_path, outcome)| (rel_path.as_str(), outcome))
            .collect();
        for row in &mut self.rows {
            if let Some(outcome) = outcomes.get(row.rel_path.as_str()) {
                row.verdict = Verdict::Done((*outcome).clone());
                row.selected = false;
            }
        }
    }
}

/// Applies the rows the user kept, as one journal batch. Every write is the engine's; all this
/// does is choose which photos it is handed.
pub fn apply(
    set: &ChangeSet,
    engine: &mut Engine,
    journal: &mut Journal,
    cache: &mut Cache,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
) -> write::Result<Summary> {
    let targets = set.targets(engine.library());
    if targets.is_empty() {
        return Err(write::Error::Refusing("no photo is selected".to_string()));
    }
    engine.write(journal, cache, &targets, progress, cancel)
}

/// The pass the last applied change set left behind, if it can still be taken back. The whole
/// history with per-batch undo is another day's work; this is the last one only.
pub fn undoable(journal: &Journal) -> journal::Result<Option<journal::Pass>> {
    let last = journal
        .passes(RECENT)?
        .into_iter()
        .find(|pass| pass.kind == Kind::Write.as_str() && pass.written > 0);
    let Some(pass) = last else { return Ok(None) };
    Ok(journal.undo_of(pass.id)?.is_none().then_some(pass))
}

/// Puts the last applied change set back, through the engine that already knows how.
pub fn undo_last(
    engine: &mut Engine,
    journal: &mut Journal,
    cache: &mut Cache,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
) -> write::Result<Summary> {
    let pass = undoable(journal)?.ok_or_else(|| write::Error::Refusing("there is nothing to take back".to_string()))?;
    engine.undo(journal, cache, pass.id, progress, cancel)
}

fn row(wanted: &Wanted, known: Option<&Stated>) -> Row {
    let differences = differences(&wanted.change, known.map(|known| &known.said));
    let verdict = verdict(wanted, known, &differences);
    Row {
        rel_path: wanted.rel_path.clone(),
        content_id: known.and_then(|known| known.content_id.clone()).unwrap_or_default(),
        size: known.map(|known| known.size).unwrap_or_default(),
        change: wanted.change.clone(),
        differences,
        selected: verdict == Verdict::Change,
        verdict,
    }
}

fn verdict(wanted: &Wanted, known: Option<&Stated>, differences: &[Difference]) -> Verdict {
    let Some(known) = known else {
        return Verdict::Refused("the cache does not know it, so scan the library first".to_string());
    };
    if known.content_id.is_none() {
        return Verdict::Refused("the scan could not read its image data".to_string());
    }
    if let Err(why) = wanted.change.assigns() {
        return Verdict::Refused(why);
    }
    match differences.is_empty() || differences.iter().all(Difference::settled) {
        true => Verdict::Nothing,
        false => Verdict::Change,
    }
}

fn differences(change: &Change, said: Option<&Said>) -> Vec<Difference> {
    change.fields.iter().map(|field| difference(field, said)).collect()
}

fn difference(field: &Field, said: Option<&Said>) -> Difference {
    match field {
        Field::Rating(rating) => Difference {
            what: "rating",
            before: said.map(|said| shown_rating(said.rating)),
            after: shown_rating(*rating),
        },
        Field::Gps(gps) => Difference {
            what: "location",
            before: said.map(|said| shown_position(said.gps_lat.zip(said.gps_lon))),
            after: shown_position(gps.map(|gps| (gps.lat, gps.lon))),
        },
        Field::Taken(taken) => Difference {
            what: "date",
            before: said.map(|said| shown_date(said.taken_at.as_deref(), said.taken_offset.as_deref())),
            after: shown_date(
                taken.as_ref().map(|taken| taken.at.as_str()),
                taken.as_ref().and_then(|taken| taken.offset.as_deref()),
            ),
        },
        Field::Tags(paths) => Difference {
            what: "tags",
            before: said.map(|said| shown_tags(&said.tags)),
            after: shown_tags(&change::expand(paths).unwrap_or_else(|_| paths.to_vec())),
        },
        Field::Place(place) => Difference {
            what: "place",
            before: None,
            after: match place {
                None => NONE.to_string(),
                Some(place) => {
                    let parts: Vec<&str> = [
                        place.location.as_deref(),
                        place.city.as_deref(),
                        place.state.as_deref(),
                        place.country.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|part| !part.trim().is_empty())
                    .collect();
                    match parts.is_empty() {
                        true => NONE.to_string(),
                        false => parts.join(", "),
                    }
                }
            },
        },
        Field::Faces(faces) => Difference {
            what: "people",
            before: None,
            after: match faces {
                None => NONE.to_string(),
                Some(faces) => faces
                    .faces
                    .iter()
                    .map(|face| face.name.trim())
                    .collect::<Vec<&str>>()
                    .join(", "),
            },
        },
        Field::DropLabel => Difference {
            what: "label",
            before: None,
            after: NONE.to_string(),
        },
        Field::DropCatalogSets => Difference {
            what: "catalog sets",
            before: None,
            after: NONE.to_string(),
        },
    }
}

fn shown_rating(rating: Option<i64>) -> String {
    rating.map(|stars| stars.to_string()).unwrap_or(NONE.to_string())
}

fn shown_position(position: Option<(f64, f64)>) -> String {
    match position {
        Some((lat, lon)) => format!("{lat:.5}, {lon:.5}"),
        None => NONE.to_string(),
    }
}

fn shown_date(at: Option<&str>, offset: Option<&str>) -> String {
    match at {
        None => NONE.to_string(),
        Some(at) => match offset {
            Some(offset) => format!("{at} {offset}"),
            None => at.to_string(),
        },
    }
}

/// A tag set, in one order so that two of them compare. The photo's side is what the scan read,
/// and a write spells out every level of every path, so a photo whose list is missing a level
/// reads as "would change" - and the write then skips it if it turns out to say it after all.
fn shown_tags(paths: &[String]) -> String {
    let mut all = paths.to_vec();
    all.sort();
    all.dedup();
    match all.is_empty() {
        true => NONE.to_string(),
        false => all.join(", "),
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
