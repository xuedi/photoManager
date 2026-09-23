//! Every pass the journal holds, the way a person looks back at them: what ran, when, how many
//! photos it changed, whether it was taken back, and whether it still can be.
//!
//! Taking back an older pass is the engine's existing undo. It refuses any photo that no longer
//! says what the pass wrote, so a later change is never overwritten; what this adds is knowing
//! beforehand how many such photos there are, so the question can say so.

use rusqlite::params;

use crate::journal::{self, Journal, Kind, Recorded, Result, Swap, WRITTEN};
use crate::write::change::shown;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    pub id: i64,
    pub kind: Kind,
    pub title: String,
    pub tool: Option<String>,
    pub started_at: String,
    pub written: i64,
    /// The pass this one took back.
    pub undoes: Option<i64>,
    /// The pass that took this one back.
    pub undone_by: Option<i64>,
    /// How many of the photos it wrote a later pass changed again. Those are left as they are
    /// when it is taken back.
    pub changed_since: i64,
}

impl Pass {
    /// A write that changed something and is not taken back yet. An undo is not offered: running
    /// the tool again is how a change is put back on.
    pub fn can_take_back(&self) -> bool {
        self.kind == Kind::Write && self.written > 0 && self.undone_by.is_none()
    }
}

/// Newest first, `limit` of them after the `skip` newest.
pub fn passes(journal: &Journal, skip: i64, limit: i64) -> Result<Vec<Pass>> {
    journal
        .passes_from(skip, limit)?
        .into_iter()
        .map(|pass| read(journal, pass))
        .collect()
}

pub fn pass(journal: &Journal, batch: i64) -> Result<Pass> {
    read(journal, journal.pass(batch)?)
}

/// Every photo of one pass and what it got, in the order they were done.
pub fn photos(journal: &Journal, batch: i64) -> Result<Vec<Recorded>> {
    journal.entries(batch)
}

/// What one photo of a pass got, the way a person reads it: every tag with its two sides, or why
/// it was left alone.
pub fn told(photo: &Recorded) -> String {
    match photo.outcome.as_deref() {
        Some(WRITTEN) => photo.swaps.iter().map(swapped).collect::<Vec<String>>().join("; "),
        Some(outcome) => match &photo.detail {
            Some(detail) => format!("{outcome}: {detail}"),
            None => outcome.to_string(),
        },
        None => "never finished: the pass was interrupted".to_string(),
    }
}

fn swapped(swap: &Swap) -> String {
    let value = |text: Option<&str>| text.and_then(|text| serde_json::from_str(text).ok());
    format!(
        "{}: {} -> {}",
        swap.tag,
        shown(value(swap.old.as_deref()).as_ref()),
        shown(value(swap.new.as_deref()).as_ref())
    )
}

fn read(journal: &Journal, pass: journal::Pass) -> Result<Pass> {
    let kind = match pass.kind.as_str() {
        "undo" => Kind::Undo,
        _ => Kind::Write,
    };
    let undone_by = match kind {
        Kind::Write => journal.undo_of(pass.id)?,
        Kind::Undo => None,
    };
    let mut read = Pass {
        id: pass.id,
        kind,
        title: pass.title().to_string(),
        tool: pass.tool,
        started_at: pass.started_at,
        written: pass.written,
        undoes: pass.undoes,
        undone_by,
        changed_since: 0,
    };
    if read.can_take_back() {
        read.changed_since = changed_since(journal, read.id)?;
    }
    Ok(read)
}

/// The photos of a pass that a later write wrote again and that still say what that write left:
/// a later write that was itself taken back for that photo does not count.
fn changed_since(journal: &Journal, batch: i64) -> Result<i64> {
    Ok(journal.connection().query_row(
        "SELECT count(DISTINCT e.rel_path) FROM entry e
         WHERE e.batch_id = ?1 AND e.outcome = ?2 AND EXISTS (
            SELECT 1 FROM entry later JOIN batch b ON b.id = later.batch_id
            WHERE later.rel_path = e.rel_path AND later.outcome = ?2
              AND b.id > ?1 AND b.kind = ?3
              AND NOT EXISTS (
                SELECT 1 FROM batch u JOIN entry back ON back.batch_id = u.id
                WHERE u.undoes = b.id AND back.rel_path = e.rel_path AND back.outcome = ?2))",
        params![batch, WRITTEN, Kind::Write.as_str()],
        |row| row.get(0),
    )?)
}
