//! The tools: edits the person drives. Each takes one value - a place, a shift, a date, a zone, a
//! tag, a folder - and says what every photo of the scope should say with it. Nothing is guessed
//! and nothing is kept: the value is given, the change set shown, and the preview applied.
//!
//! What the app is sure about on its own is not here but among the fixes ([`crate::fixes`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::cache::{self, Cache};
use crate::changeset::{ChangeSet, Wanted};
use crate::dates::{self, Shift};
use crate::geo::Geo;
use crate::scope::Scope;
use crate::tags::{self, Rule, Rules};
use crate::tools::neighbour::{self, Neighbours};
use crate::tools::offsets::Offsets;
use crate::tools::tag_vocabulary::{self, Generated};
use crate::tools::{Answer, Question, Tool, folders, time_zones};
use crate::tools::{people_from_tags, place_words};
use crate::write::change::is_derived;
use crate::write::{Change, Field, Gps, Taken};

/// What the photos without a camera name are grouped as.
pub const NO_CAMERA: &str = "no camera";

/// What the file is told about where a position given by hand came from.
pub const METHOD: &str = "photoManager: set by hand";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    SetPlace,
    PlacesTagToSublocation,
    ShiftDates,
    SetDate,
    SetTimeZone,
    AddTag,
    RemoveTag,
    RenameTag,
    TagToPerson,
    TidyTags,
    MoveEvent,
    PositionFromNeighbour,
    /// A rating over the whole scope, so the way from an edit to a photo can be driven.
    /// Development builds only.
    #[cfg(feature = "demo")]
    Rating,
}

/// Every edit, in the order they are listed.
pub const ALL: &[Edit] = &[
    Edit::SetPlace,
    Edit::PlacesTagToSublocation,
    Edit::ShiftDates,
    Edit::SetDate,
    Edit::SetTimeZone,
    Edit::AddTag,
    Edit::RemoveTag,
    Edit::RenameTag,
    Edit::TagToPerson,
    Edit::TidyTags,
    Edit::MoveEvent,
    Edit::PositionFromNeighbour,
    #[cfg(feature = "demo")]
    Edit::Rating,
];

/// What an edit is given.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A place or a pin.
    Place(Answer),
    /// Each named camera's clock was off by this much.
    Shift(Vec<(String, Shift)>),
    Date(String),
    /// A time zone, or where each photo was taken.
    Zone(Option<String>),
    Tag(String),
    Rename {
        from: String,
        to: String,
    },
    Generated(Generated),
    /// The photos of a tag and the name they are given: a person, or a sublocation.
    Named {
        tag: String,
        name: String,
    },
    Folder(String),
    /// The photos of one event and the measured photo each borrows its position from.
    Neighbours(Neighbours),
    Rating(i64),
}

impl Edit {
    pub fn find(key: &str) -> Option<Edit> {
        ALL.iter().copied().find(|edit| edit.key() == key)
    }

    /// Stays the same for as long as the edit exists: actions and tests name an edit by it.
    pub fn key(self) -> &'static str {
        match self {
            Edit::SetPlace => "set-place",
            Edit::PlacesTagToSublocation => "places-tag-to-sublocation",
            Edit::ShiftDates => "shift-dates",
            Edit::SetDate => "set-date",
            Edit::SetTimeZone => "set-time-zone",
            Edit::AddTag => "add-tag",
            Edit::RemoveTag => "remove-tag",
            Edit::RenameTag => "rename-tag",
            Edit::TagToPerson => "tag-to-person",
            Edit::TidyTags => "tidy-tags",
            Edit::MoveEvent => "move-event",
            Edit::PositionFromNeighbour => "position-from-a-neighbour",
            #[cfg(feature = "demo")]
            Edit::Rating => "demo-rating",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Edit::SetPlace => "Set Place",
            Edit::PlacesTagToSublocation => "Places Tag to Sublocation",
            Edit::ShiftDates => "Shift Dates",
            Edit::SetDate => "Set Date",
            Edit::SetTimeZone => "Set Time Zone",
            Edit::AddTag => "Add Tag",
            Edit::RemoveTag => "Remove Tag",
            Edit::RenameTag => "Rename Tag",
            Edit::TagToPerson => "Tag to Person",
            Edit::TidyTags => "Tidy Tags",
            Edit::MoveEvent => "Move Event",
            Edit::PositionFromNeighbour => "Position from a Neighbour",
            #[cfg(feature = "demo")]
            Edit::Rating => "Demo Rating",
        }
    }

    /// What it does, in one line.
    pub fn does(self) -> &'static str {
        match self {
            Edit::SetPlace => "Gives the photos without a position of their own a place or a point on the map",
            Edit::PlacesTagToSublocation => "Keeps a place finer than the town, such as a district, as the sublocation",
            Edit::ShiftDates => "Moves the dates of a camera whose clock was off",
            Edit::SetDate => "Gives the photos one date, a second more for each after the first",
            Edit::SetTimeZone => {
                "Writes the offset of a zone, or of where each photo was taken, and XMP dates that agree"
            }
            Edit::AddTag => "Adds a tag to every photo",
            Edit::RemoveTag => "Takes a tag and everything below it off every photo",
            Edit::RenameTag => "Renames, moves or merges a tag and everything below it",
            Edit::TagToPerson => "Names a person in every photo of a people tag, without a face box",
            Edit::TidyTags => "Writes every tag field the same, the generated tags made, dropped or kept",
            Edit::MoveEvent => "Moves one event into another folder",
            Edit::PositionFromNeighbour => "Gives photos of one event the position a photo taken beside them measured",
            #[cfg(feature = "demo")]
            Edit::Rating => "Sets one rating on every photo",
        }
    }

    /// A value as the `win.run-edit` action writes it: a place as an answer, a shift as a JSON
    /// object of cameras, the neighbours as a JSON object of an event and its groups, a rename, a
    /// person or a sublocation of a tag as `tag -> name`, the rest as plain text.
    pub fn read(self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        match self {
            Edit::SetPlace => match Answer::read(text)? {
                answer @ (Answer::Place(_) | Answer::Pin { .. }) => Ok(Value::Place(answer)),
                _ => Err(format!("{text} is not a place")),
            },
            Edit::ShiftDates => {
                let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(text) else {
                    return Err(format!("{text} is not a shift per camera"));
                };
                let mut shifts = Vec::new();
                for (camera, by) in fields {
                    let by = by.as_str().ok_or_else(|| format!("{by} is not a shift"))?;
                    shifts.push((camera, Shift::read(by)?));
                }
                Ok(Value::Shift(shifts))
            }
            Edit::SetDate => Ok(Value::Date(dates::format(dates::parse(text)?))),
            Edit::SetTimeZone => match text.is_empty() {
                true => Ok(Value::Zone(None)),
                false => {
                    dates::offset_in(text, "2000-01-01 12:00:00")?;
                    Ok(Value::Zone(Some(text.to_string())))
                }
            },
            Edit::AddTag | Edit::RemoveTag => Ok(Value::Tag(tags::path(text)?)),
            Edit::RenameTag => {
                let (from, to) = text
                    .split_once("->")
                    .ok_or_else(|| format!("{text} does not say what to rename it to"))?;
                let rule = Rule::rename(from, to)?;
                Ok(Value::Rename {
                    from: rule.from().to_string(),
                    to: tags::path(to)?,
                })
            }
            Edit::TagToPerson | Edit::PlacesTagToSublocation => {
                let (tag, name) = text
                    .split_once("->")
                    .ok_or_else(|| format!("{text} does not say which tag and which name"))?;
                let tag = tags::path(tag)?;
                let name = name.trim();
                if name.is_empty() {
                    return Err(match self {
                        Edit::TagToPerson => "type the person's name".to_string(),
                        _ => "type the name of the place".to_string(),
                    });
                }
                match self {
                    Edit::TagToPerson if !people_from_tags::is_people(&tag) => {
                        Err(format!("{tag} is not a tag below people"))
                    }
                    Edit::PlacesTagToSublocation if !place_words::is_below_a_country(&tag) => {
                        Err(format!("{tag} is not a place below a country's places tag"))
                    }
                    _ => Ok(Value::Named {
                        tag,
                        name: name.to_string(),
                    }),
                }
            }
            Edit::TidyTags => Generated::named(text)
                .map(Value::Generated)
                .ok_or_else(|| format!("{text} is not derived, dropped or kept")),
            Edit::MoveEvent => Ok(Value::Folder(folders::event_folder(text)?)),
            Edit::PositionFromNeighbour => Ok(Value::Neighbours(Neighbours::read(text)?)),
            #[cfg(feature = "demo")]
            Edit::Rating => match text.parse::<i64>() {
                Ok(stars) if (0..=5).contains(&stars) => Ok(Value::Rating(stars)),
                _ => Err(format!("{text} is not a rating from 0 to 5")),
            },
        }
    }

    /// What every photo of the scope should say with this value, as a change set. Reads the
    /// cache and the place data, never a photo, and writes nothing.
    pub fn change_set(
        self,
        value: &Value,
        cache: &Cache,
        geo: Option<&Geo>,
        scope: &Scope,
    ) -> Result<ChangeSet, String> {
        let failed = |error: rusqlite::Error| error.to_string();
        let (title, wanted) = match (self, value) {
            (Edit::SetPlace, Value::Place(answer)) => (
                format!("Set the place to {}", answer.names()),
                set_place(cache, scope, answer).map_err(failed)?,
            ),
            (Edit::ShiftDates, Value::Shift(shifts)) => (
                "Shift dates".to_string(),
                shift_dates(cache, geo, scope, shifts).map_err(failed)?,
            ),
            (Edit::SetDate, Value::Date(at)) => (
                format!("Set the date to {at}"),
                set_date(cache, geo, scope, at).map_err(failed)?,
            ),
            (Edit::SetTimeZone, Value::Zone(zone)) => (
                match zone {
                    Some(zone) => format!("Set the time zone to {zone}"),
                    None => "Set the time zone of where each photo was taken".to_string(),
                },
                time_zones::wanted(cache, geo, scope, zone.as_deref()).map_err(failed)?,
            ),
            (Edit::AddTag, Value::Tag(tag)) => (
                format!("Add the tag {tag}"),
                add_tag(cache, scope, tag).map_err(failed)?,
            ),
            (Edit::RemoveTag, Value::Tag(tag)) => (
                format!("Remove the tag {tag}"),
                tag_vocabulary::renamed(cache, scope, &Rules(vec![Rule::delete(tag)?]), false).map_err(failed)?,
            ),
            (Edit::RenameTag, Value::Rename { from, to }) => (
                format!("Rename {from} to {to}"),
                tag_vocabulary::renamed(cache, scope, &Rules(vec![Rule::rename(from, to)?]), false).map_err(failed)?,
            ),
            (Edit::TagToPerson, Value::Named { tag, name }) => {
                if people_from_tags::is_group(cache, tag).map_err(failed)? {
                    return Err(format!("{tag} has tags below it: a group, not a person"));
                }
                let named = BTreeMap::from([(tag.clone(), name.clone())]);
                (
                    format!("Name {name} in the photos tagged {tag}"),
                    people_from_tags::wanted(cache, &named, scope).map_err(failed)?,
                )
            }
            (Edit::PlacesTagToSublocation, Value::Named { tag, name }) => (
                format!("Keep {name} as the sublocation of {tag}"),
                place_words::sublocation(cache, scope, tag, name).map_err(failed)?,
            ),
            (Edit::TidyTags, Value::Generated(generated)) => (
                "Tidy the tags".to_string(),
                tag_vocabulary::tidied(cache, scope, *generated).map_err(failed)?,
            ),
            (Edit::MoveEvent, Value::Folder(folder)) => {
                let event = event_of(cache, scope)?;
                (
                    format!("Move {event} to {folder}"),
                    folders::moves(cache, &[(event, folder.clone())]).map_err(failed)?,
                )
            }
            (Edit::PositionFromNeighbour, Value::Neighbours(neighbours)) => (
                format!(
                    "Positions from a neighbour in {}",
                    neighbours.event.rsplit('/').next().unwrap_or(&neighbours.event)
                ),
                neighbour::wanted(cache, geo, neighbours)?,
            ),
            #[cfg(feature = "demo")]
            (Edit::Rating, Value::Rating(stars)) => (
                format!("Set a rating of {stars}"),
                scope
                    .paths(cache)
                    .map_err(failed)?
                    .into_iter()
                    .map(|rel_path| Wanted::new(rel_path, Change::of([Field::Rating(Some(*stars))])))
                    .collect(),
            ),
            (edit, value) => return Err(format!("{} does not take {value:?}", edit.title())),
        };
        ChangeSet::build(cache, &title, &wanted).map_err(failed)
    }
}

/// The photos without a position of their own get the place: those with none, and those whose
/// position was worked out rather than taken. A photo whose camera knew where it was is refused.
fn set_place(cache: &Cache, scope: &Scope, answer: &Answer) -> cache::Result<Vec<Wanted>> {
    let Some(spot) = answer.spot() else {
        return Ok(Vec::new());
    };
    let paths = scope.paths(cache)?;
    let stated = cache.stated(&paths)?;
    let mut wanted = Vec::new();
    for rel_path in paths {
        let Some(photo) = stated.get(&rel_path) else { continue };
        let said = &photo.said;
        let own = said.gps_lat.is_some() && !said.gps_method.as_deref().is_some_and(is_derived);
        if own {
            wanted.push(Wanted::refused(rel_path, "it has a position of its own"));
            continue;
        }
        wanted.push(Wanted::new(
            rel_path,
            Change::of([
                Field::Gps(Some(Gps {
                    lat: spot.lat,
                    lon: spot.lon,
                    altitude: None,
                    derived: Some(crate::write::change::Derived {
                        method: METHOD,
                        metres: spot.metres,
                    }),
                })),
                Field::Place(Some(spot.near.place())),
            ]),
        ));
    }
    Ok(wanted)
}

/// One camera of the scope, as the Shift Dates form shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    pub name: String,
    pub photos: usize,
    pub first: String,
    pub last: String,
}

impl Camera {
    /// `49 photos, 2009-12-31 10:00:00 to 2009-12-31 18:00:00`.
    pub fn facts(&self) -> String {
        let photos = match self.photos {
            1 => "1 photo".to_string(),
            count => format!("{count} photos"),
        };
        match self.first == self.last {
            true => format!("{photos}, {}", self.first),
            false => format!("{photos}, {} to {}", self.first, self.last),
        }
    }
}

/// The cameras of the scope's dated photos, with when each took its first and last.
pub fn cameras(cache: &Cache, scope: &Scope) -> cache::Result<Vec<Camera>> {
    let mut found: BTreeMap<String, Camera> = BTreeMap::new();
    for photo in cache.dated(&scope.paths(cache)?)? {
        let Some(at) = photo.taken_at else { continue };
        let name = photo.camera.unwrap_or_else(|| NO_CAMERA.to_string());
        let camera = found.entry(name.clone()).or_insert_with(|| Camera {
            name,
            photos: 0,
            first: at.clone(),
            last: at.clone(),
        });
        camera.photos += 1;
        if at < camera.first {
            camera.first = at.clone();
        }
        if at > camera.last {
            camera.last = at;
        }
    }
    Ok(found.into_values().collect())
}

/// Each dated photo of a shifted camera moved by its shift, keeping the offset it states, else
/// given the one of where it was, where that is known.
fn shift_dates(
    cache: &Cache,
    geo: Option<&Geo>,
    scope: &Scope,
    shifts: &[(String, Shift)],
) -> cache::Result<Vec<Wanted>> {
    let mut offsets = Offsets::new(geo);
    let mut wanted = Vec::new();
    for photo in cache.dated(&scope.paths(cache)?)? {
        let (Some(at), camera) = (&photo.taken_at, photo.camera.as_deref().unwrap_or(NO_CAMERA)) else {
            continue;
        };
        let Some((_, by)) = shifts.iter().find(|(name, _)| name == camera) else {
            continue;
        };
        wanted.push(match by.apply(at) {
            Ok(at) => {
                let offset = photo
                    .taken_offset
                    .clone()
                    .or_else(|| offsets.offset(&photo, &at, None).ok());
                Wanted::new(photo.rel_path, Change::of([Field::Taken(Some(Taken { at, offset }))]))
            }
            Err(why) => Wanted::refused(photo.rel_path, why),
        });
    }
    Ok(wanted)
}

/// The first photo of the scope by name gets the date, each after it a second more, with the
/// offset it states or the one of where it was, where that is known.
fn set_date(cache: &Cache, geo: Option<&Geo>, scope: &Scope, at: &str) -> cache::Result<Vec<Wanted>> {
    let Ok(start) = dates::parse(at) else {
        return Ok(Vec::new());
    };
    let start = dates::seconds(start);
    let mut photos = cache.dated(&scope.paths(cache)?)?;
    photos.sort_by(|one, other| one.rel_path.cmp(&other.rel_path));
    let mut offsets = Offsets::new(geo);
    Ok(photos
        .into_iter()
        .enumerate()
        .map(
            |(step, photo)| match dates::from_seconds(start + step as i64).map(dates::format) {
                Some(at) => {
                    let offset = photo
                        .taken_offset
                        .clone()
                        .or_else(|| offsets.offset(&photo, &at, None).ok());
                    Wanted::new(photo.rel_path, Change::of([Field::Taken(Some(Taken { at, offset }))]))
                }
                None => Wanted::refused(photo.rel_path, format!("{at} is off the calendar")),
            },
        )
        .collect())
}

/// The tag added to every photo of the scope. One that carries it already in every field changes
/// nothing, and the change set says so.
fn add_tag(cache: &Cache, scope: &Scope, tag: &str) -> cache::Result<Vec<Wanted>> {
    Ok(cache
        .tagged(&scope.paths(cache)?)?
        .into_iter()
        .map(|photo| {
            let mut then = photo.tags.clone();
            then.push(tag.to_string());
            Wanted::new(photo.rel_path, Change::of([Field::Tags(tags::deepest(&then))]))
        })
        .collect())
}

/// The one event the scope is in, as Move Event needs it.
pub fn event_of(cache: &Cache, scope: &Scope) -> Result<String, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let paths = scope.paths(cache).map_err(failed)?;
    let events: BTreeSet<String> = cache.event_dirs(&paths).map_err(failed)?.into_values().collect();
    let mut events = events.into_iter();
    match (events.next(), events.next()) {
        (Some(event), None) => Ok(event),
        (None, _) => Err("the scope is in no event: choose one event as the scope".to_string()),
        (Some(_), Some(_)) => Err("the scope is in several events: choose one event as the scope".to_string()),
    }
}

/// What Move Event starts from for the scope's event: the folder Folder Migration would offer it,
/// or where it is now.
pub fn proposal(cache: &Cache, geo: Option<&Geo>, scope: &Scope) -> Result<Question, String> {
    let event = event_of(cache, scope)?;
    let photos = cache.under(&event).map_err(|error| error.to_string())?.len();
    let asked = folders::FolderMigration.questions(
        cache,
        geo,
        &Scope::Filter(crate::filter::Filter::all().within(&event)),
        &Default::default(),
    )?;
    Ok(asked
        .into_iter()
        .find(|question| question.key == event)
        .unwrap_or_else(|| Question::place(event.clone(), event, photos, Vec::new())))
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::Verdict;
    use crate::filter::Filter;
    use crate::tools::testing::{Library, geo};

    const PICKED: [&str; 2] = [
        "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG",
        "Germany/2019-07-13 Sommerfest/IMAG0001.jpg",
    ];

    fn picked() -> Scope {
        Scope::Photos {
            title: "2 photos".to_string(),
            paths: PICKED.iter().map(|path| path.to_string()).collect(),
        }
    }

    fn tags_of(library: &Library, rel_path: &str) -> Vec<String> {
        library.cache.stated(&[rel_path.to_string()]).unwrap()[rel_path]
            .said
            .tags
            .clone()
    }

    #[test]
    fn every_edit_reads_its_value_and_says_why_not() {
        assert!(Edit::AddTag.read("people//Anna").is_err());
        assert_eq!(
            Edit::RenameTag.read("People -> people"),
            Ok(Value::Rename {
                from: "People".to_string(),
                to: "people".to_string()
            })
        );
        assert!(Edit::RenameTag.read("People").is_err());
        assert!(Edit::SetDate.read("yesterday").is_err());
        assert!(Edit::SetTimeZone.read("Nowhere/Atlantis").is_err());
        assert_eq!(Edit::SetTimeZone.read(""), Ok(Value::Zone(None)));
        assert!(Edit::ShiftDates.read(r#"{"X100S": "soon"}"#).is_err());
        assert!(Edit::MoveEvent.read("Germany/nowhere").is_err());
        for edit in ALL {
            assert_eq!(Edit::find(edit.key()), Some(*edit));
        }
    }

    #[test]
    fn a_tag_over_a_selection_writes_exactly_those_photos_and_comes_off_again() {
        let mut library = Library::new("edit-tags");
        let add = Value::Tag("people/family/Anna".to_string());
        let set = Edit::AddTag.change_set(&add, &library.cache, None, &picked()).unwrap();
        assert_eq!(set.title, "Add the tag people/family/Anna");
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rows, PICKED);
        assert!(set.rows.iter().all(|row| row.verdict == Verdict::Change));
        assert_eq!(library.apply(&set).written, 2);

        library.rescan();
        assert_eq!(
            tags_of(&library, PICKED[0]),
            ["people", "people/family", "people/family/Anna"]
        );
        assert!(
            tags_of(&library, PICKED[1]).contains(&"people/me".to_string()),
            "what it had stays"
        );
        let again = Edit::AddTag.change_set(&add, &library.cache, None, &picked()).unwrap();
        assert_eq!(again.counts().change, 0, "already carried, nothing to do");

        let renamed = Value::Rename {
            from: "people/family/Anna".to_string(),
            to: "people/friends/Anna".to_string(),
        };
        let whole = Scope::Filter(Filter::all());
        let set = Edit::RenameTag
            .change_set(&renamed, &library.cache, None, &whole)
            .unwrap();
        assert_eq!(
            set.counts().change,
            3,
            "only the photos that carry it: the two it was added to and the one tagged before"
        );
        library.apply(&set);
        library.rescan();
        assert!(tags_of(&library, PICKED[0]).contains(&"people/friends/Anna".to_string()));

        let removed = Value::Tag("people/friends".to_string());
        let set = Edit::RemoveTag
            .change_set(&removed, &library.cache, None, &whole)
            .unwrap();
        assert_eq!(set.counts().change, 3);
        library.apply(&set);
        library.rescan();
        assert!(
            tags_of(&library, PICKED[0]).is_empty(),
            "{:?}",
            tags_of(&library, PICKED[0])
        );
    }

    #[test]
    fn a_people_tag_names_its_person_in_exactly_the_photos_that_do_not_yet() {
        let mut library = Library::new("edit-tag-to-person");
        const TAGGED: &str = "Germany/2019-07-13 Sommerfest/IMAG0001.jpg";
        let value = Edit::TagToPerson.read(" people/me -> Sam ").unwrap();
        assert_eq!(
            value,
            Value::Named {
                tag: "people/me".to_string(),
                name: "Sam".to_string()
            }
        );
        assert!(Edit::TagToPerson.read("places/inChina -> Sam").is_err());
        assert!(Edit::TagToPerson.read("people/me -> ").is_err());
        let group = Edit::TagToPerson.read("people/family -> Sam").unwrap();
        assert!(
            Edit::TagToPerson
                .change_set(&group, &library.cache, None, &Scope::Filter(Filter::all()))
                .is_err(),
            "a group is not a person"
        );

        let elsewhere = Scope::Filter(Filter::all().within("Denmark"));
        let set = Edit::TagToPerson
            .change_set(&value, &library.cache, None, &elsewhere)
            .unwrap();
        assert!(set.rows.is_empty(), "the scope narrows it");

        let whole = Scope::Filter(Filter::all());
        let set = Edit::TagToPerson
            .change_set(&value, &library.cache, None, &whole)
            .unwrap();
        assert_eq!(set.title, "Name Sam in the photos tagged people/me");
        let rows: Vec<&str> = set.rows.iter().map(|row| row.rel_path.as_str()).collect();
        assert_eq!(rows, [TAGGED]);
        assert_eq!(set.rows[0].tells(), "people: Anna, Tom -> Anna, Tom, Sam (no box)");

        let before = library.cache.stated(&[TAGGED.to_string()]).unwrap()[TAGGED]
            .said
            .clone();
        assert_eq!(library.apply(&set).written, 1);
        library.rescan();
        let after = library.cache.stated(&[TAGGED.to_string()]).unwrap()[TAGGED]
            .said
            .clone();
        let faces = |said: &crate::cache::Said| said.regions.as_ref().unwrap().faces.clone();
        assert_eq!(faces(&after), faces(&before), "the boxes are kept");
        assert_eq!(after.tags, before.tags, "the tag stays");
        let people: Vec<String> = crate::browse::people(&library.cache)
            .unwrap()
            .into_iter()
            .map(|person| person.name)
            .collect();
        assert!(people.contains(&"Sam".to_string()), "{people:?}");
        let again = Edit::TagToPerson
            .change_set(&value, &library.cache, None, &whole)
            .unwrap();
        assert!(again.rows.is_empty());
    }

    #[test]
    fn a_sublocation_is_read_below_a_country_only() {
        assert_eq!(
            Edit::PlacesTagToSublocation.read("places/inGermany/Harbourside -> Harbour Side"),
            Ok(Value::Named {
                tag: "places/inGermany/Harbourside".to_string(),
                name: "Harbour Side".to_string()
            })
        );
        assert!(
            Edit::PlacesTagToSublocation
                .read("places/inGermany -> Harbour Side")
                .is_err()
        );
        assert!(Edit::PlacesTagToSublocation.read("people/me -> Harbour Side").is_err());
    }

    #[test]
    fn a_place_goes_to_the_photos_without_a_position_of_their_own() {
        let library = Library::new("edit-place");
        let geo = geo();
        let beijing = geo.find("Beijing", Some("China")).unwrap().candidates[0].place.clone();
        let value = Value::Place(Answer::Place(crate::tools::Located::of(&beijing)));
        let scope = Scope::Filter(Filter::all().within("Germany/2019-07-13 Sommerfest"));
        let set = Edit::SetPlace
            .change_set(&value, &library.cache, Some(&geo), &scope)
            .unwrap();
        assert_eq!(set.title, "Set the place to Beijing");
        let located = set
            .rows
            .iter()
            .find(|row| row.rel_path == "Germany/2019-07-13 Sommerfest/img_0657.jpg")
            .unwrap();
        assert_eq!(
            located.verdict,
            Verdict::Refused("it has a position of its own".to_string())
        );
        assert!(set.counts().change > 0);
    }

    #[test]
    fn a_shift_moves_one_camera_and_a_date_steps_a_second_a_photo() {
        let library = Library::new("edit-dates");
        let scope = Scope::Filter(Filter::all().within("Germany/2019-07-13 Sommerfest"));
        let cameras = cameras(&library.cache, &scope).unwrap();
        assert!(!cameras.is_empty());
        let camera = cameras[0].clone();
        let shift = Value::Shift(vec![(camera.name.clone(), Shift::days(1))]);
        let set = Edit::ShiftDates
            .change_set(&shift, &library.cache, None, &scope)
            .unwrap();
        assert_eq!(set.counts().change, camera.photos, "{camera:?}");

        let date = Value::Date("2019-07-13 12:00:00".to_string());
        let set = Edit::SetDate.change_set(&date, &library.cache, None, &scope).unwrap();
        let taken: Vec<String> = set
            .rows
            .iter()
            .filter_map(|row| {
                row.change.fields.iter().find_map(|field| match field {
                    Field::Taken(Some(taken)) => Some(taken.at.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(taken[0], "2019-07-13 12:00:00");
        assert_eq!(taken[1], "2019-07-13 12:00:01");
    }

    #[test]
    fn move_event_takes_one_event_and_starts_from_its_proposal() {
        let library = Library::new("edit-move");
        assert!(event_of(&library.cache, &Scope::Filter(Filter::all())).is_err());
        let scope = Scope::Filter(Filter::all().within("China/2006-09-00 Besuch Ben"));
        let proposal = proposal(&library.cache, Some(&geo()), &scope).unwrap();
        assert_eq!(proposal.key, "China/2006-09-00 Besuch Ben");
        let Some(Answer::Folder(folder)) = proposal.sure().map(|offer| offer.answer.clone()) else {
            panic!("a sure folder: {proposal:?}");
        };
        let set = Edit::MoveEvent
            .change_set(&Value::Folder(folder.clone()), &library.cache, None, &scope)
            .unwrap();
        assert!(set.moves());
        assert_eq!(set.title, format!("Move China/2006-09-00 Besuch Ben to {folder}"));
    }
}
