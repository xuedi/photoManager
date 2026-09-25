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
use crate::metadata::Regions;
use crate::write::{self, Assignment, Change, Engine, Field, Move, Outcome, Summary, Target, change};

/// How far back the undo looks for the pass it can take back.
const RECENT: i64 = 50;

const NONE: &str = "none";
/// What a photo's tags say when its tag fields disagree or hold leftovers.
const UNTIDY: &str = "the tag fields disagree";
/// What the cache cannot answer. The exact diff of the row is the only honest answer there.
pub const UNKNOWN: &str = "unknown";

/// What a tool decided for one photo.
#[derive(Debug, Clone)]
pub struct Wanted {
    pub rel_path: String,
    pub change: Change,
    /// The tool will not decide for this photo, and says why: it is listed, and never written.
    pub refused: Option<String>,
    /// A folder or a photo to take elsewhere instead of anything to write: `rel_path` is where it
    /// is now.
    pub moved: Option<Move>,
}

impl Wanted {
    pub fn new(rel_path: impl Into<String>, change: Change) -> Wanted {
        Wanted {
            rel_path: rel_path.into(),
            change,
            refused: None,
            moved: None,
        }
    }

    pub fn refused(rel_path: impl Into<String>, why: impl Into<String>) -> Wanted {
        Wanted {
            rel_path: rel_path.into(),
            change: Change::default(),
            refused: Some(why.into()),
            moved: None,
        }
    }

    /// A move, and why it will not be made when it will not.
    pub fn moving(moved: Move, refused: Option<String>) -> Wanted {
        Wanted {
            rel_path: moved.from.clone(),
            change: Change::default(),
            refused,
            moved: Some(moved),
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
    /// Where it goes and what goes with it, for a row that moves a folder or a photo.
    pub moved: Option<Move>,
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
        if let Some(moved) = &self.moved {
            return match moved.photos.len() {
                1 => format!("to {}, 1 photo", moved.to),
                count => format!("to {}, {count} photos", moved.to),
            };
        }
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
    /// The key of the tool that built it. A change set made by hand has none.
    pub tool: Option<String>,
    pub rows: Vec<Row>,
}

impl ChangeSet {
    /// Reads the cache, never a photo, and writes nothing at all.
    pub fn build(cache: &Cache, title: &str, wanted: &[Wanted]) -> crate::cache::Result<ChangeSet> {
        let paths: Vec<String> = wanted
            .iter()
            .filter(|one| one.moved.is_none())
            .map(|one| one.rel_path.clone())
            .collect();
        let known = cache.stated(&paths)?;
        Ok(ChangeSet {
            title: title.to_string(),
            tool: None,
            rows: wanted
                .iter()
                .map(|one| match &one.moved {
                    Some(moved) => moving_row(one, moved),
                    None => row(one, known.get(&one.rel_path)),
                })
                .collect(),
        })
    }

    /// Whether its rows move folders and photos rather than write them.
    pub fn moves(&self) -> bool {
        self.rows.iter().any(|row| row.moved.is_some())
    }

    /// Looks at the folders a move would take, never at a photo: a row whose target is there
    /// already, or whose folder holds a file the move was not built with or lacks one it was, is
    /// refused, as the engine would refuse it.
    pub fn look(&mut self, library: &Path) {
        for row in &mut self.rows {
            let Some(moved) = &row.moved else { continue };
            if row.verdict != Verdict::Change {
                continue;
            }
            if let Some(why) = looked_at(library, moved) {
                row.verdict = Verdict::Refused(why);
                row.selected = false;
            }
        }
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
            .filter(|row| row.selected && row.would_change() && row.moved.is_none())
            .map(|row| Target {
                path: library.join(&row.rel_path),
                content_id: row.content_id.clone(),
                change: row.change.clone(),
            })
            .collect()
    }

    /// The folders and photos an apply would move: the selected rows and nothing else.
    pub fn relocations(&self) -> Vec<Move> {
        self.rows
            .iter()
            .filter(|row| row.selected && row.would_change())
            .filter_map(|row| row.moved.clone())
            .collect()
    }

    /// The exact tag-level diff of one row: one ExifTool read, the same code path a write takes,
    /// and nothing written or journaled. A refusal comes back as its reason.
    pub fn exact(&self, index: usize, engine: &mut Engine) -> Result<Vec<Assignment>, String> {
        let row = self.rows.get(index).ok_or("there is no such row")?;
        if row.moved.is_some() {
            return Err("A move changes no field of any photo, only the folder it is in".to_string());
        }
        let target = Target {
            path: engine.library().join(&row.rel_path),
            content_id: row.content_id.clone(),
            change: row.change.clone(),
        };
        engine.dry_run(&target).map_err(|error| error.to_string())
    }

    /// An undo puts the photos back, so the rows it touched are open for applying again. They are
    /// left deselected: taking a change back and putting it straight back on is never accidental.
    pub fn unsettle(&mut self, summary: &Summary) {
        let put_back: HashMap<&str, &Outcome> = summary
            .outcomes
            .iter()
            .map(|(rel_path, outcome)| (rel_path.as_str(), outcome))
            .collect();
        for row in &mut self.rows {
            if put_back.get(row.rel_path.as_str()) == Some(&&Outcome::Written)
                && row.verdict == Verdict::Done(Outcome::Written)
            {
                row.verdict = Verdict::Change;
                row.selected = false;
            }
        }
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

/// Applies the rows the user kept, as one journal batch named after the change set. Every write
/// is the engine's; all this does is choose which photos it is handed.
pub fn apply(
    set: &ChangeSet,
    engine: &mut Engine,
    journal: &mut Journal,
    cache: &mut Cache,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
) -> write::Result<Summary> {
    if set.moves() {
        let moves = set.relocations();
        if moves.is_empty() {
            return Err(write::Error::Refusing("no folder is selected".to_string()));
        }
        return engine.relocate(
            journal,
            cache,
            &set.title,
            set.tool.as_deref(),
            &moves,
            progress,
            cancel,
        );
    }
    let targets = set.targets(engine.library());
    if targets.is_empty() {
        return Err(write::Error::Refusing("no photo is selected".to_string()));
    }
    engine.write(
        journal,
        cache,
        &set.title,
        set.tool.as_deref(),
        &targets,
        progress,
        cancel,
    )
}

/// The pass the last applied change set left behind, if it can still be taken back: what the
/// toast's Undo means. Any other pass is taken back from the history with [`take_back`].
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

/// Takes back any pass that can still be taken back, not only the last. A photo a later pass
/// changed again is refused by the engine and keeps what it says now.
pub fn take_back(
    engine: &mut Engine,
    journal: &mut Journal,
    cache: &mut Cache,
    batch: i64,
    progress: &(dyn Fn(usize, usize) + Sync),
    cancel: &AtomicBool,
) -> write::Result<Summary> {
    if !crate::history::pass(journal, batch)?.can_take_back() {
        return Err(write::Error::Refusing(format!("pass {batch} cannot be taken back")));
    }
    engine.undo(journal, cache, batch, progress, cancel)
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
        moved: None,
        selected: verdict == Verdict::Change,
        verdict,
    }
}

/// A move uploads nothing: Nextcloud takes a renamed folder as a move, so it costs no traffic.
fn moving_row(wanted: &Wanted, moved: &Move) -> Row {
    let verdict = match &wanted.refused {
        Some(why) => Verdict::Refused(why.clone()),
        None if moved.from == moved.to => Verdict::Nothing,
        None => Verdict::Change,
    };
    Row {
        rel_path: wanted.rel_path.clone(),
        content_id: String::new(),
        size: 0,
        change: Change::default(),
        differences: vec![Difference {
            what: "folder",
            before: Some(moved.from.clone()),
            after: moved.to.clone(),
        }],
        moved: Some(moved.clone()),
        selected: verdict == Verdict::Change,
        verdict,
    }
}

/// Why the folders on disk would refuse a move, if they would.
fn looked_at(library: &Path, moved: &Move) -> Option<String> {
    if std::fs::symlink_metadata(library.join(&moved.to)).is_ok() {
        return Some(format!("{} is there already", moved.to));
    }
    let from = library.join(&moved.from);
    if std::fs::symlink_metadata(&from).is_err() {
        return Some(format!("{} is not there, so scan the library first", moved.from));
    }
    let known: std::collections::HashSet<&str> = moved.photos.iter().map(|(rel_path, _)| rel_path.as_str()).collect();
    let mut found = std::collections::HashSet::new();
    for entry in walkdir::WalkDir::new(&from).sort_by_file_name() {
        let Ok(entry) = entry else {
            return Some(format!("{} cannot be read", moved.from));
        };
        if entry.file_type().is_dir() {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(library)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default();
        if !known.contains(rel_path.as_str()) {
            return Some(format!(
                "{rel_path} is not a photo the scan knows, so scan the library first"
            ));
        }
        found.insert(rel_path);
    }
    let mut missing: Vec<&&str> = known.iter().filter(|rel_path| !found.contains(**rel_path)).collect();
    missing.sort();
    missing
        .first()
        .map(|rel_path| format!("{rel_path} is no longer there, so scan the library first"))
}

fn verdict(wanted: &Wanted, known: Option<&Stated>, differences: &[Difference]) -> Verdict {
    let Some(known) = known else {
        return Verdict::Refused("the cache does not know it, so scan the library first".to_string());
    };
    if known.content_id.is_none() {
        return Verdict::Refused("the scan could not read its image data".to_string());
    }
    if let Some(why) = &wanted.refused {
        return Verdict::Refused(why.clone());
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
            before: said.map(|said| {
                let derived = said.gps_method.as_deref().is_some_and(change::is_derived);
                shown_position(said.gps_lat.zip(said.gps_lon), derived)
            }),
            after: shown_position(
                gps.map(|gps| (gps.lat, gps.lon)),
                gps.is_some_and(|gps| gps.derived.is_some()),
            ),
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
            before: said.map(|said| match said.tags_untidy {
                true => format!("{} ({UNTIDY})", shown_tags(&said.tags)),
                false => shown_tags(&said.tags),
            }),
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
        Field::Faces(faces) => {
            let after = shown_faces(faces.as_ref().map(|faces| faces.faces.as_slice()).unwrap_or_default());
            Difference {
                what: "people",
                before: said.map(|said| shown_regions(said.regions.as_ref(), faces.as_ref(), &after)),
                after,
            }
        }
        Field::DropLabel => Difference {
            what: "label",
            before: said.filter(|said| !said.tags_untidy).map(|_| NONE.to_string()),
            after: NONE.to_string(),
        },
        Field::DropCatalogSets => Difference {
            what: "catalog sets",
            before: said.filter(|said| !said.tags_untidy).map(|_| NONE.to_string()),
            after: NONE.to_string(),
        },
    }
}

/// Whether a photo's regions already say what these faces would, as far as the cache knows.
pub fn regions_say(said: Option<&Regions>, wanted: &change::Faces) -> bool {
    let after = shown_faces(&wanted.faces);
    shown_regions(said, Some(wanted), &after) == after
}

/// The names of the regions, in the order the file lists them.
fn shown_faces(faces: &[change::Face]) -> String {
    match faces.is_empty() {
        true => NONE.to_string(),
        false => faces
            .iter()
            .map(|face| face.name.trim())
            .collect::<Vec<&str>>()
            .join(", "),
    }
}

/// What the photo's regions say, and when they name the same people as the change but differ in
/// anything else, what: so a row never reads as settled when a box would still move.
fn shown_regions(said: Option<&Regions>, wanted: Option<&change::Faces>, after: &str) -> String {
    let names = shown_faces(said.map(|said| said.faces.as_slice()).unwrap_or_default());
    let persons_differ = || {
        let mut named: Vec<String> = said.map(|said| said.persons.clone()).unwrap_or_default();
        let mut wanted: Vec<String> = wanted
            .map(|wanted| wanted.faces.iter().map(|face| face.name.trim().to_string()).collect())
            .unwrap_or_default();
        for list in [&mut named, &mut wanted] {
            list.sort();
            list.dedup();
        }
        named != wanted
    };
    if names != after {
        return names;
    }
    match (said, wanted) {
        (Some(said), Some(wanted)) if said.width != Some(wanted.width) || said.height != Some(wanted.height) => {
            format!("{names} (measured on another size)")
        }
        (Some(said), Some(wanted)) if !same_boxes(&said.faces, &wanted.faces) => format!("{names} (other boxes)"),
        _ if persons_differ() => format!("{names} (other persons named)"),
        _ => names,
    }
}

/// The engine's own tolerance for a number that went through a file.
fn same_boxes(one: &[change::Face], other: &[change::Face]) -> bool {
    let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
    one.len() == other.len()
        && one.iter().zip(other).all(|(one, other)| {
            one.name.trim() == other.name.trim()
                && near(one.x, other.x)
                && near(one.y, other.y)
                && near(one.width, other.width)
                && near(one.height, other.height)
        })
}

fn shown_rating(rating: Option<i64>) -> String {
    rating.map(|stars| stars.to_string()).unwrap_or(NONE.to_string())
}

/// A derived position says so, so a person never takes a city centre for where they stood.
fn shown_position(position: Option<(f64, f64)>, derived: bool) -> String {
    match position {
        Some((lat, lon)) if derived => format!("{lat:.5}, {lon:.5} (derived)"),
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
