//! Position from a neighbour: inside one event, photos taken where a photo beside them measured its
//! position are given that position, marked as borrowed and how far off it may be. The person
//! picks the measured photo and the photos by eye on the event's timeline; nothing is chosen by
//! time here. A photo that measured its own position is never written.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use super::Located;
use crate::cache::{self, Cache, Timed};
use crate::changeset::Wanted;
use crate::dates;
use crate::geo::Geo;
use crate::write::change::{DERIVED_BY, Derived, is_derived};
use crate::write::{Change, Field, Gps};

/// What the file is told about where its position came from.
pub const METHOD: &str = "photoManager: neighbour";

/// What a lane of photos without a camera name is called.
pub const UNKNOWN_CAMERA: &str = "Unknown camera";

/// How far from its source a borrowed position may be off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Reach {
    Spot,
    #[default]
    Street,
    Area,
}

impl Reach {
    pub const ALL: [Reach; 3] = [Reach::Spot, Reach::Street, Reach::Area];

    pub fn key(self) -> &'static str {
        match self {
            Reach::Spot => "spot",
            Reach::Street => "street",
            Reach::Area => "area",
        }
    }

    pub fn named(key: &str) -> Option<Reach> {
        Reach::ALL.into_iter().find(|reach| reach.key() == key.trim())
    }

    pub fn metres(self) -> f64 {
        match self {
            Reach::Spot => 50.0,
            Reach::Street => 200.0,
            Reach::Area => 1000.0,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Reach::Spot => "The Same Spot",
            Reach::Street => "The Same Street",
            Reach::Area => "The Same Area",
        }
    }

    /// `200 m`, `1 km`.
    pub fn tells(self) -> String {
        metres(self.metres())
    }
}

/// `200 m`, `1 km`, `5 km`: how far off a position may be, the way a person says it.
pub fn metres(metres: f64) -> String {
    match metres >= 1000.0 {
        true => format!("{} km", (metres / 1000.0 * 10.0).round() / 10.0),
        false => format!("{} m", metres.round()),
    }
}

/// A measured photo and the photos that borrow its position.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub source: String,
    pub reach: Reach,
    pub targets: Vec<String>,
}

/// The groups given in one event, in the order they were given: a photo in two belongs to the last.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Neighbours {
    pub event: String,
    pub groups: Vec<Group>,
}

impl Neighbours {
    /// `{"event": "...", "groups": [{"source": "...", "reach": "street", "targets": [...]}]}`.
    pub fn read(text: &str) -> Result<Neighbours, String> {
        let wrong = || format!("{text} is not an event with groups of photos");
        let value: Value = serde_json::from_str(text).map_err(|_| wrong())?;
        let event = value.get("event").and_then(Value::as_str).ok_or_else(wrong)?;
        let event = event.trim().trim_matches('/').to_string();
        if event.is_empty() {
            return Err(wrong());
        }
        let mut groups = Vec::new();
        for group in value.get("groups").and_then(Value::as_array).ok_or_else(wrong)? {
            let source = group.get("source").and_then(Value::as_str).ok_or_else(wrong)?;
            let reach = match group.get("reach").and_then(Value::as_str) {
                None => Reach::default(),
                Some(key) => Reach::named(key).ok_or_else(|| format!("{key} is not spot, street or area"))?,
            };
            let targets = group
                .get("targets")
                .and_then(Value::as_array)
                .ok_or_else(wrong)?
                .iter()
                .map(|target| target.as_str().map(String::from).ok_or_else(wrong))
                .collect::<Result<Vec<String>, String>>()?;
            groups.push(Group {
                source: source.to_string(),
                reach,
                targets,
            });
        }
        Ok(Neighbours { event, groups })
    }

    pub fn written(&self) -> String {
        let groups: Vec<Value> = self
            .groups
            .iter()
            .map(|group| {
                let mut fields = Map::new();
                fields.insert("source".to_string(), Value::from(group.source.as_str()));
                fields.insert("reach".to_string(), Value::from(group.reach.key()));
                fields.insert("targets".to_string(), Value::from(group.targets.clone()));
                Value::Object(fields)
            })
            .collect();
        let mut fields = Map::new();
        fields.insert("event".to_string(), Value::from(self.event.as_str()));
        fields.insert("groups".to_string(), Value::from(groups));
        Value::Object(fields).to_string()
    }

    /// The group each photo belongs to: the last that names it.
    pub fn owners(&self) -> BTreeMap<&str, usize> {
        let mut owners = BTreeMap::new();
        for (index, group) in self.groups.iter().enumerate() {
            for target in &group.targets {
                owners.insert(target.as_str(), index);
            }
        }
        owners
    }
}

/// What a photo's position is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    Measured,
    /// Worked out here, and how: `places tag`, `event`, `set by hand`, `neighbour`.
    Derived(String),
    None,
}

impl Position {
    pub fn of(gps: Option<(f64, f64)>, method: Option<&str>) -> Position {
        match (gps, method) {
            (None, _) => Position::None,
            (Some(_), Some(method)) if is_derived(method) => {
                Position::Derived(method.trim().trim_start_matches(DERIVED_BY.trim()).trim().to_string())
            }
            (Some(_), _) => Position::Measured,
        }
    }

    pub fn is_measured(&self) -> bool {
        *self == Position::Measured
    }

    /// `measured`, `derived from the places tag`, `no position`.
    pub fn tells(&self) -> String {
        match self {
            Position::Measured => "measured".to_string(),
            Position::Derived(how) if how == "set by hand" => "set by hand".to_string(),
            Position::Derived(how) if how == "neighbour" => "from a neighbour".to_string(),
            Position::Derived(how) => format!("derived from the {how}"),
            Position::None => "no position".to_string(),
        }
    }
}

/// One photo on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub rel_path: String,
    pub content_id: Option<String>,
    pub orientation: Option<i64>,
    pub taken_at: Option<String>,
    /// The date as seconds, for the axis.
    pub seconds: Option<i64>,
    pub camera: String,
    pub gps: Option<(f64, f64)>,
    pub position: Position,
}

impl Moment {
    fn of(timed: Timed) -> Moment {
        let seconds = timed
            .taken_at
            .as_deref()
            .and_then(|at| dates::parse(at).ok())
            .map(dates::seconds);
        Moment {
            camera: camera(timed.camera_make.as_deref(), timed.camera_model.as_deref()),
            position: Position::of(timed.gps, timed.gps_method.as_deref()),
            rel_path: timed.rel_path,
            content_id: timed.content_id,
            orientation: timed.orientation,
            taken_at: timed.taken_at,
            seconds,
            gps: timed.gps,
        }
    }

    pub fn name(&self) -> &str {
        self.rel_path.rsplit('/').next().unwrap_or(&self.rel_path)
    }

    /// `DSCF0102.JPG, 14:03, measured`: what a screen reader says for it.
    pub fn label(&self) -> String {
        let at = self
            .taken_at
            .as_deref()
            .and_then(|at| at.get(11..16))
            .unwrap_or("no date");
        format!("{}, {at}, {}", self.name(), self.position.tells())
    }
}

/// One camera's photos, in the order they were taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub camera: String,
    pub photos: Vec<Moment>,
}

/// An event as its timeline shows it: a lane per camera, in the order of their first photo, and
/// the photos without a date apart, in name order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub event: String,
    pub lanes: Vec<Lane>,
    pub undated: Vec<Moment>,
}

impl Timeline {
    pub fn photos(&self) -> impl Iterator<Item = &Moment> {
        self.lanes.iter().flat_map(|lane| &lane.photos).chain(&self.undated)
    }

    pub fn find(&self, rel_path: &str) -> Option<&Moment> {
        self.photos().find(|photo| photo.rel_path == rel_path)
    }

    /// The first and the last date on the axis, as seconds.
    pub fn span(&self) -> Option<(i64, i64)> {
        let seconds = self
            .lanes
            .iter()
            .flat_map(|lane| &lane.photos)
            .filter_map(|photo| photo.seconds);
        seconds.fold(None, |span, at| match span {
            None => Some((at, at)),
            Some((first, last)) => Some((first.min(at), last.max(at))),
        })
    }
}

/// `FUJIFILM X100S`, or the model alone when it names its maker already.
fn camera(make: Option<&str>, model: Option<&str>) -> String {
    match (make, model) {
        (_, None) => make.map(String::from).unwrap_or_else(|| UNKNOWN_CAMERA.to_string()),
        (None, Some(model)) => model.to_string(),
        (Some(make), Some(model)) => {
            let brand = make.split_whitespace().next().unwrap_or(make).to_lowercase();
            match model.to_lowercase().starts_with(&brand) {
                true => model.to_string(),
                false => format!("{make} {model}"),
            }
        }
    }
}

/// The timeline of an event folder, its sub-folders included.
pub fn timeline(cache: &Cache, event: &str) -> cache::Result<Timeline> {
    let mut lanes: Vec<Lane> = Vec::new();
    let mut undated = Vec::new();
    for moment in cache.timed_in(event)?.into_iter().map(Moment::of) {
        if moment.seconds.is_none() {
            undated.push(moment);
            continue;
        }
        match lanes.iter_mut().find(|lane| lane.camera == moment.camera) {
            Some(lane) => lane.photos.push(moment),
            None => lanes.push(Lane {
                camera: moment.camera.clone(),
                photos: vec![moment],
            }),
        }
    }
    for lane in &mut lanes {
        lane.photos
            .sort_by(|one, other| one.seconds.cmp(&other.seconds).then(one.rel_path.cmp(&other.rel_path)));
    }
    lanes.sort_by(|one, other| {
        one.photos[0]
            .seconds
            .cmp(&other.photos[0].seconds)
            .then(one.camera.cmp(&other.camera))
    });
    undated.sort_by(|one, other| one.name().cmp(other.name()).then(one.rel_path.cmp(&other.rel_path)));
    Ok(Timeline {
        event: event.to_string(),
        lanes,
        undated,
    })
}

/// An event where a photo measured its position and others did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Neighboured {
    pub event: String,
    pub photos: usize,
    pub measured: usize,
    pub derived: usize,
    pub none: usize,
}

/// Every event with a measured photo and photos whose position is derived or missing, the most
/// measured first.
pub fn events(cache: &Cache) -> cache::Result<Vec<Neighboured>> {
    let mut found: BTreeMap<String, Neighboured> = BTreeMap::new();
    for (event, positioned, method) in cache.event_positions()? {
        let counted = found.entry(event.clone()).or_insert_with(|| Neighboured {
            event,
            ..Neighboured::default()
        });
        counted.photos += 1;
        match (positioned, method.as_deref().is_some_and(is_derived)) {
            (false, _) => counted.none += 1,
            (true, true) => counted.derived += 1,
            (true, false) => counted.measured += 1,
        }
    }
    let mut events: Vec<Neighboured> = found
        .into_values()
        .filter(|event| event.measured > 0 && event.derived + event.none > 0)
        .collect();
    events.sort_by(|one, other| other.measured.cmp(&one.measured).then(one.event.cmp(&other.event)));
    Ok(events)
}

/// Each photo of the groups given its group's source position, the neighbour's mark with the
/// reach, and the place words of that position. A photo that measured its own, one outside the
/// event, and a group whose source did not measure its position are refused.
pub fn wanted(cache: &Cache, geo: Option<&Geo>, neighbours: &Neighbours) -> Result<Vec<Wanted>, String> {
    let failed = |error: rusqlite::Error| error.to_string();
    let photos: HashMap<String, Timed> = cache
        .timed_in(&neighbours.event)
        .map_err(failed)?
        .into_iter()
        .map(|timed| (timed.rel_path.clone(), timed))
        .collect();
    let owners = neighbours.owners();
    let mut words: HashMap<(u64, u64), Option<Located>> = HashMap::new();

    let mut wanted = Vec::new();
    for (target, index) in owners {
        let group = &neighbours.groups[index];
        let source_name = group.source.rsplit('/').next().unwrap_or(&group.source);
        let refused = |why: String| Wanted::refused(target, why);
        let Some(source) = photos.get(&group.source) else {
            wanted.push(refused(format!("its source {source_name} is not in the event")));
            continue;
        };
        let (Some((lat, lon)), Position::Measured) =
            (source.gps, Position::of(source.gps, source.gps_method.as_deref()))
        else {
            wanted.push(refused(format!(
                "its source {source_name} did not measure its position"
            )));
            continue;
        };
        let Some(photo) = photos.get(target) else {
            wanted.push(refused("it is not in the event".to_string()));
            continue;
        };
        if Position::of(photo.gps, photo.gps_method.as_deref()).is_measured() {
            wanted.push(refused("it measured its own position".to_string()));
            continue;
        }
        let Some(geo) = geo else {
            wanted.push(refused(
                "there is no place data to name where it was: get it on the dashboard".to_string(),
            ));
            continue;
        };
        let key = (lat.to_bits(), lon.to_bits());
        if let std::collections::hash_map::Entry::Vacant(e) = words.entry(key) {
            let near = Located::near(&geo.at(lat, lon).map_err(|error| error.to_string())?);
            e.insert(near);
        }
        let Some(near) = &words[&key] else {
            wanted.push(refused(format!("the place data knows no town near {source_name}")));
            continue;
        };
        let mut fields = vec![Field::Gps(Some(Gps {
            lat,
            lon,
            altitude: None,
            derived: Some(Derived {
                method: METHOD,
                metres: group.reach.metres(),
            }),
        }))];
        // Borrowed from the same photo already: the words came with it, so nothing is left to do.
        let borrowed = photo.gps == Some((lat, lon)) && photo.gps_method.as_deref().map(str::trim) == Some(METHOD);
        if !borrowed {
            fields.push(Field::Place(Some(near.place())));
        }
        wanted.push(
            Wanted::new(target, Change::of(fields)).with_note(format!("from {source_name}, {}", group.reach.tells())),
        );
    }
    Ok(wanted)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use crate::changeset::{ChangeSet, Verdict};
    use crate::tools::testing::{Library, geo};

    const EVENT: &str = "Germany/2018-05-12 Canal Tour";
    const PHONE: &str = "Germany/2018-05-12 Canal Tour/PXL_0001.jpg";
    const EVENING: &str = "Germany/2018-05-12 Canal Tour/Evening/PXL_0002.jpg";
    const CENTRE: &str = "Germany/2018-05-12 Canal Tour/DSCF0201.JPG";
    const LATER: &str = "Germany/2018-05-12 Canal Tour/DSCF0202.JPG";
    const UNDATED: &str = "Germany/2018-05-12 Canal Tour/DSCF0203.JPG";

    fn group(source: &str, reach: Reach, targets: &[&str]) -> Group {
        Group {
            source: source.to_string(),
            reach,
            targets: targets.iter().map(|target| target.to_string()).collect(),
        }
    }

    fn built(library: &Library, groups: Vec<Group>) -> ChangeSet {
        let neighbours = Neighbours {
            event: EVENT.to_string(),
            groups,
        };
        let wanted = wanted(&library.cache, Some(&geo()), &neighbours).unwrap();
        ChangeSet::build(&library.cache, "", &wanted).unwrap()
    }

    fn row<'a>(set: &'a ChangeSet, rel_path: &str) -> &'a crate::changeset::Row {
        set.rows.iter().find(|row| row.rel_path == rel_path).unwrap()
    }

    fn gps(set: &ChangeSet, rel_path: &str) -> Gps {
        row(set, rel_path)
            .change
            .fields
            .iter()
            .find_map(|field| match field {
                Field::Gps(Some(gps)) => Some(*gps),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn groups_round_trip_through_text() {
        let neighbours = Neighbours {
            event: EVENT.to_string(),
            groups: vec![group(PHONE, Reach::Spot, &[CENTRE, UNDATED])],
        };
        assert_eq!(Neighbours::read(&neighbours.written()), Ok(neighbours));
        assert!(Neighbours::read(r#"{"event": "", "groups": []}"#).is_err());
        assert!(
            Neighbours::read(r#"{"event": "a", "groups": [{"source": "x", "reach": "far", "targets": []}]}"#).is_err()
        );
        assert_eq!(Reach::Street.tells(), "200 m");
        assert_eq!(Reach::Area.tells(), "1 km");
    }

    #[test]
    fn the_timeline_has_a_lane_per_camera_and_the_undated_apart() {
        let library = Library::new("neighbour-timeline");
        let timeline = timeline(&library.cache, EVENT).unwrap();
        let lanes: Vec<(&str, usize)> = timeline
            .lanes
            .iter()
            .map(|lane| (lane.camera.as_str(), lane.photos.len()))
            .collect();
        assert_eq!(
            lanes,
            [("Google Pixel 3", 2), ("FUJIFILM X100S", 2)],
            "in the order of their first photo"
        );
        assert_eq!(
            timeline.lanes[0].photos[1].rel_path, EVENING,
            "a sub-folder is in its event"
        );
        let undated: Vec<&str> = timeline.undated.iter().map(|photo| photo.rel_path.as_str()).collect();
        assert_eq!(undated, [UNDATED]);
        assert_eq!(timeline.find(PHONE).unwrap().position, Position::Measured);
        assert_eq!(
            timeline.find(CENTRE).unwrap().position,
            Position::Derived("places tag".to_string())
        );
        assert_eq!(timeline.find(UNDATED).unwrap().position, Position::None);
        assert_eq!(
            timeline.find(CENTRE).unwrap().label(),
            "DSCF0201.JPG, 14:03, derived from the places tag"
        );
        assert_eq!(
            timeline.span().map(|(first, last)| last - first),
            Some(5 * 3600 - 2 * 60)
        );
    }

    #[test]
    fn the_events_a_neighbour_knows_are_found() {
        let library = Library::new("neighbour-events");
        let found = events(&library.cache).unwrap();
        let canal = found.iter().find(|event| event.event == EVENT).unwrap();
        assert_eq!((canal.photos, canal.measured, canal.derived, canal.none), (5, 2, 2, 1));
        assert!(
            found.iter().all(|event| event.event != "Germany/2014-03-22 Museum"),
            "an event that measured everything is not one"
        );
        assert!(
            found
                .iter()
                .all(|event| event.measured > 0 && event.derived + event.none > 0)
        );
    }

    #[test]
    fn a_target_gets_the_source_position_its_mark_and_its_words() {
        let mut library = Library::new("neighbour-give");
        let set = built(&library, vec![group(PHONE, Reach::Street, &[CENTRE, UNDATED, EVENING])]);
        let given = gps(&set, CENTRE);
        assert_eq!((given.lat, given.lon), (53.5485, 9.978));
        assert_eq!(
            given.derived,
            Some(Derived {
                method: METHOD,
                metres: 200.0
            })
        );
        assert!(
            row(&set, CENTRE)
                .change
                .fields
                .iter()
                .any(|field| matches!(field, Field::Place(Some(place)) if place.city.is_some())),
            "the place words come with it"
        );
        assert!(
            row(&set, CENTRE).tells().ends_with("from PXL_0001.jpg, 200 m"),
            "{}",
            row(&set, CENTRE).tells()
        );
        assert_eq!(
            row(&set, UNDATED).verdict,
            Verdict::Change,
            "the eye decides, not the clock"
        );
        assert_eq!(
            row(&set, EVENING).verdict,
            Verdict::Refused("it measured its own position".to_string())
        );

        let summary = library.apply(&set);
        assert_eq!(summary.written, 2);
        assert!(
            summary.outcomes.iter().all(|(rel_path, _)| rel_path != EVENING),
            "a measured photo is never handed to the engine"
        );
        library.rescan();
        let timeline = timeline(&library.cache, EVENT).unwrap();
        assert_eq!(
            timeline.find(CENTRE).unwrap().position,
            Position::Derived("neighbour".to_string())
        );
        assert_eq!(timeline.find(EVENING).unwrap().position, Position::Measured);
        assert_eq!(timeline.find(EVENING).unwrap().gps, Some((53.543, 9.969)), "untouched");

        let again = built(&library, vec![group(PHONE, Reach::Street, &[CENTRE, UNDATED])]);
        assert_eq!(again.counts().change, 0, "a second run is nothing to do");
    }

    #[test]
    fn each_group_names_its_source_and_the_last_one_wins() {
        let library = Library::new("neighbour-groups");
        let set = built(
            &library,
            vec![
                group(PHONE, Reach::Spot, &[CENTRE, LATER]),
                group(EVENING, Reach::Area, &[LATER, UNDATED]),
            ],
        );
        assert!(row(&set, CENTRE).tells().ends_with("from PXL_0001.jpg, 50 m"));
        assert!(
            row(&set, LATER).tells().ends_with("from PXL_0002.jpg, 1 km"),
            "the last group wins"
        );
        assert!(row(&set, UNDATED).tells().ends_with("from PXL_0002.jpg, 1 km"));
        assert_eq!((gps(&set, LATER).lat, gps(&set, LATER).lon), (53.543, 9.969));
        assert_eq!(set.rows.len(), 3, "one row per photo");
    }

    #[test]
    fn a_source_that_did_not_measure_or_a_photo_outside_is_refused() {
        let library = Library::new("neighbour-refused");
        let set = built(
            &library,
            vec![
                group(CENTRE, Reach::Street, &[LATER]),
                group(PHONE, Reach::Street, &["Germany/2019-07-13 Sommerfest/IMAG0001.jpg"]),
            ],
        );
        assert_eq!(
            row(&set, LATER).verdict,
            Verdict::Refused("its source DSCF0201.JPG did not measure its position".to_string())
        );
        assert_eq!(
            row(&set, "Germany/2019-07-13 Sommerfest/IMAG0001.jpg").verdict,
            Verdict::Refused("it is not in the event".to_string())
        );
        let outside = built(
            &library,
            vec![group(
                "Germany/2019-07-13 Sommerfest/img_0657.jpg",
                Reach::Street,
                &[LATER],
            )],
        );
        assert_eq!(
            row(&outside, LATER).verdict,
            Verdict::Refused("its source img_0657.jpg is not in the event".to_string())
        );
        let without = wanted(
            &library.cache,
            None,
            &Neighbours {
                event: EVENT.to_string(),
                groups: vec![group(PHONE, Reach::Street, &[LATER])],
            },
        )
        .unwrap();
        assert!(
            without[0]
                .refused
                .as_deref()
                .unwrap()
                .starts_with("there is no place data")
        );
    }
}
