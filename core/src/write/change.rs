//! What a write means, and the ExifTool arguments it becomes.
//!
//! A `Change` is an intent: these tags, this position, this date, this rating, these faces. It
//! turns into a flat list of `Assign`s, one per tag ExifTool is asked to set, and everything the
//! engine does afterwards - deciding there is nothing left to do, writing the journal, proving the
//! result - works on that one list.
//!
//! Every assignment carries two names: the one ExifTool is *written* with (`EXIF:GPSLatitude`) and
//! the one the same value comes *back* under when the file is read with `-G1` (`GPS:GPSLatitude`).
//! They differ often enough that guessing is not an option.

use serde_json::{Map, Value};

/// A tag, what it should say, and the name it reads back under. `Null` means: take it away.
#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub tag: String,
    pub key: String,
    pub value: Value,
}

/// One tag a write would set, with what that tag says now: what a dry run is made of. `None` on
/// either side is a tag that is not there, or is to be taken away.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub tag: String,
    pub key: String,
    pub now: Option<Value>,
    pub then: Option<Value>,
}

impl Assignment {
    pub(crate) fn of(assign: &Assign, fields: &Map<String, Value>) -> Assignment {
        let held = |value: Option<&Value>| match value {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.clone()),
        };
        Assignment {
            tag: assign.tag.clone(),
            key: assign.key.clone(),
            now: held(fields.get(&assign.key)),
            then: held(Some(&assign.value)),
        }
    }

    pub fn tells(&self) -> String {
        format!("{} -> {}", shown(self.now.as_ref()), shown(self.then.as_ref()))
    }
}

/// A tag value the way a person reads it, rather than as the JSON it is kept in.
pub fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "none".to_string(),
        Some(Value::Array(items)) => items.iter().map(plain).collect::<Vec<String>>().join(", "),
        Some(object @ Value::Object(_)) => structured(object),
        Some(other) => plain(other),
    }
}

/// A position. Latitude and longitude are signed; ExifTool stores the size and the hemisphere
/// apart, so that is how they are assigned and proved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gps {
    pub lat: f64,
    pub lon: f64,
    pub altitude: Option<f64>,
    /// `None` is a position someone measured, and takes away any mark a derived one left.
    pub derived: Option<Derived>,
}

/// A position worked out rather than measured, said in the file itself: how it was worked out, in
/// `GPSProcessingMethod`, and how far off it may be, in `GPSHPositioningError`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Derived {
    pub method: &'static str,
    pub metres: f64,
}

/// What every method this application writes starts with, so a derived position is told apart
/// from one a camera or a phone measured.
pub const DERIVED_BY: &str = "photoManager: ";

/// Whether a `GPSProcessingMethod` says the position was derived here.
pub fn is_derived(method: &str) -> bool {
    method.trim().starts_with(DERIVED_BY)
}

/// Where a photo was, in words. Each part that is `None` is taken away.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub location: Option<String>,
}

/// When a photo was taken. `at` is the one date format; `offset` is `+HH:MM` or `-HH:MM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    pub at: String,
    pub offset: Option<String>,
}

/// Faces as MWG wants them: the size the boxes were measured against, and the boxes.
#[derive(Debug, Clone, PartialEq)]
pub struct Faces {
    pub width: i64,
    pub height: i64,
    pub faces: Vec<Face>,
}

/// One box, centre-based and normalised to the applied-to dimensions.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One thing a write says about a photo. An `Option` that is `None` means: take it away.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    /// The whole tag set of the photo, as paths with `/` between the levels.
    Tags(Vec<String>),
    Rating(Option<i64>),
    Gps(Option<Gps>),
    Place(Option<Place>),
    Taken(Option<Taken>),
    Faces(Option<Faces>),
    /// Old Shotwell leaked a keyword into the label field; this takes it back out.
    DropLabel,
    /// An iView leftover nothing in this library reads.
    DropCatalogSets,
}

/// An intent, as a tool hands it to the engine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Change {
    pub fields: Vec<Field>,
}

impl Change {
    pub fn of(fields: impl IntoIterator<Item = Field>) -> Change {
        Change {
            fields: fields.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Every tag this change touches. `Err` is a refusal: the intent itself is not writable.
    pub fn assigns(&self) -> Result<Vec<Assign>, String> {
        let mut all = Vec::new();
        for field in &self.fields {
            all.extend(field.assigns()?);
        }
        let mut seen = std::collections::HashSet::new();
        for assign in &all {
            if !seen.insert(assign.tag.clone()) {
                return Err(format!("{} is set twice by one change", assign.tag));
            }
        }
        Ok(all)
    }
}

fn set(tag: &str, key: &str, value: Value) -> Assign {
    Assign {
        tag: tag.to_string(),
        key: key.to_string(),
        value,
    }
}

fn gone(tag: &str, key: &str) -> Assign {
    set(tag, key, Value::Null)
}

/// ExifTool writes an EXIF tag under a group we do not read it back under.
const DATE_TAGS: [(&str, &str); 5] = [
    ("EXIF:DateTimeOriginal", "ExifIFD:DateTimeOriginal"),
    ("EXIF:CreateDate", "ExifIFD:CreateDate"),
    ("EXIF:OffsetTimeOriginal", "ExifIFD:OffsetTimeOriginal"),
    ("EXIF:OffsetTimeDigitized", "ExifIFD:OffsetTimeDigitized"),
    ("EXIF:OffsetTime", "ExifIFD:OffsetTime"),
];

const GPS_TAGS: [(&str, &str); 9] = [
    ("EXIF:GPSLatitude", "GPS:GPSLatitude"),
    ("EXIF:GPSLatitudeRef", "GPS:GPSLatitudeRef"),
    ("EXIF:GPSLongitude", "GPS:GPSLongitude"),
    ("EXIF:GPSLongitudeRef", "GPS:GPSLongitudeRef"),
    ("EXIF:GPSAltitude", "GPS:GPSAltitude"),
    ("EXIF:GPSAltitudeRef", "GPS:GPSAltitudeRef"),
    ("EXIF:GPSMapDatum", "GPS:GPSMapDatum"),
    (METHOD.0, METHOD.1),
    (ERROR.0, ERROR.1),
];

const METHOD: (&str, &str) = ("EXIF:GPSProcessingMethod", "GPS:GPSProcessingMethod");
const ERROR: (&str, &str) = ("EXIF:GPSHPositioningError", "GPS:GPSHPositioningError");

/// City, state, country, country code and location, in both the XMP and the IPTC spelling.
const PLACE_TAGS: [(&str, &str); 10] = [
    ("XMP-photoshop:City", "XMP-photoshop:City"),
    ("IPTC:City", "IPTC:City"),
    ("XMP-photoshop:State", "XMP-photoshop:State"),
    ("IPTC:Province-State", "IPTC:Province-State"),
    ("XMP-photoshop:Country", "XMP-photoshop:Country"),
    ("IPTC:Country-PrimaryLocationName", "IPTC:Country-PrimaryLocationName"),
    ("XMP-iptcCore:CountryCode", "XMP-iptcCore:CountryCode"),
    ("IPTC:Country-PrimaryLocationCode", "IPTC:Country-PrimaryLocationCode"),
    ("XMP-iptcCore:Location", "XMP-iptcCore:Location"),
    ("IPTC:Sub-location", "IPTC:Sub-location"),
];

const REGION_INFO: &str = "XMP-mwg-rs:RegionInfo";
const PERSON_IN_IMAGE: &str = "XMP-iptcExt:PersonInImage";
const LABEL: &str = "XMP-xmp:Label";
const CATALOG_SETS: &str = "XMP-mediapro:CatalogSets";

impl Field {
    fn assigns(&self) -> Result<Vec<Assign>, String> {
        match self {
            Field::Tags(paths) => tag_assigns(paths),
            Field::Rating(rating) => rating_assigns(*rating),
            Field::Gps(gps) => gps_assigns(*gps),
            Field::Place(place) => Ok(place_assigns(place.as_ref())),
            Field::Taken(taken) => taken_assigns(taken.as_ref()),
            Field::Faces(faces) => face_assigns(faces.as_ref()),
            Field::DropLabel => Ok(vec![gone(LABEL, LABEL)]),
            Field::DropCatalogSets => Ok(vec![gone(CATALOG_SETS, CATALOG_SETS)]),
        }
    }
}

/// The five tag fields, all saying the same thing in their own way. Immich reads the first one it
/// finds, digiKam and Shotwell each read another, so they have to agree.
fn tag_assigns(paths: &[String]) -> Result<Vec<Assign>, String> {
    let all = expand(paths)?;
    let names: Vec<String> = {
        let mut names: Vec<String> = all.iter().map(|path| leaf(path).to_string()).collect();
        names.sort();
        names.dedup();
        names
    };
    let list = |items: &[String]| Value::Array(items.iter().map(|item| Value::String(item.clone())).collect());
    let piped: Vec<String> = all.iter().map(|path| path.replace('/', "|")).collect();

    Ok(vec![
        set("XMP-digiKam:TagsList", "XMP-digiKam:TagsList", list(&all)),
        set("XMP-lr:HierarchicalSubject", "XMP-lr:HierarchicalSubject", list(&piped)),
        set(
            "XMP-microsoft:LastKeywordXMP",
            "XMP-microsoft:LastKeywordXMP",
            list(&all),
        ),
        set("XMP-dc:Subject", "XMP-dc:Subject", list(&names)),
        set("IPTC:Keywords", "IPTC:Keywords", list(&names)),
    ])
}

/// Every level of every path, so a reader that knows only the flat fields still sees the whole
/// tree. `places/inChina/Beijing` also means `places` and `places/inChina`.
pub fn expand(paths: &[String]) -> Result<Vec<String>, String> {
    let mut all = std::collections::BTreeSet::new();
    for path in paths {
        let levels: Vec<&str> = path.split('/').map(str::trim).collect();
        if levels.iter().any(|level| level.is_empty()) {
            return Err(format!("{path:?} has an empty level"));
        }
        if let Some(bad) = levels.iter().find(|level| level.contains('|')) {
            return Err(format!("{bad:?} has a | in it, which separates the levels of a tag"));
        }
        for depth in 1..=levels.len() {
            all.insert(levels[..depth].join("/"));
        }
    }
    Ok(all.into_iter().collect())
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Only `xmp:Rating`. The notes never said where a rating belongs, and one place is enough.
fn rating_assigns(rating: Option<i64>) -> Result<Vec<Assign>, String> {
    let value = match rating {
        None => Value::Null,
        Some(stars) if (0..=5).contains(&stars) => Value::from(stars),
        Some(stars) => return Err(format!("a rating of {stars} is not between 0 and 5")),
    };
    Ok(vec![set("XMP-xmp:Rating", "XMP-xmp:Rating", value)])
}

fn gps_assigns(gps: Option<Gps>) -> Result<Vec<Assign>, String> {
    let Some(gps) = gps else {
        return Ok(GPS_TAGS.iter().map(|(tag, key)| gone(tag, key)).collect());
    };
    if !(-90.0..=90.0).contains(&gps.lat) || !(-180.0..=180.0).contains(&gps.lon) {
        return Err(format!("{}, {} is not a position on earth", gps.lat, gps.lon));
    }
    if gps.altitude.is_some_and(|metres| !metres.is_finite()) {
        return Err("that altitude is not a number".to_string());
    }
    if let Some(derived) = gps.derived {
        if !is_derived(derived.method) || derived.method.trim().len() == DERIVED_BY.trim().len() {
            return Err(format!(
                "{:?} does not say how the position was derived",
                derived.method
            ));
        }
        if !derived.metres.is_finite() || derived.metres <= 0.0 {
            return Err(format!(
                "{} metres is not how far off a position may be",
                derived.metres
            ));
        }
    }

    let mut assigns = vec![
        set("EXIF:GPSLatitude", "GPS:GPSLatitude", Value::from(gps.lat.abs())),
        set(
            "EXIF:GPSLatitudeRef",
            "GPS:GPSLatitudeRef",
            Value::from(if gps.lat < 0.0 { "S" } else { "N" }),
        ),
        set("EXIF:GPSLongitude", "GPS:GPSLongitude", Value::from(gps.lon.abs())),
        set(
            "EXIF:GPSLongitudeRef",
            "GPS:GPSLongitudeRef",
            Value::from(if gps.lon < 0.0 { "W" } else { "E" }),
        ),
        set("EXIF:GPSMapDatum", "GPS:GPSMapDatum", Value::from("WGS-84")),
    ];
    match gps.altitude {
        Some(metres) => assigns.extend([
            set("EXIF:GPSAltitude", "GPS:GPSAltitude", Value::from(metres.abs())),
            set(
                "EXIF:GPSAltitudeRef",
                "GPS:GPSAltitudeRef",
                Value::from(i64::from(metres < 0.0)),
            ),
        ]),
        None => assigns.extend([
            gone("EXIF:GPSAltitude", "GPS:GPSAltitude"),
            gone("EXIF:GPSAltitudeRef", "GPS:GPSAltitudeRef"),
        ]),
    }
    match gps.derived {
        Some(derived) => assigns.extend([
            set(METHOD.0, METHOD.1, Value::from(derived.method)),
            set(ERROR.0, ERROR.1, Value::from(derived.metres)),
        ]),
        None => assigns.extend([gone(METHOD.0, METHOD.1), gone(ERROR.0, ERROR.1)]),
    }
    Ok(assigns)
}

fn place_assigns(place: Option<&Place>) -> Vec<Assign> {
    let parts: [Option<&String>; 5] = match place {
        Some(place) => [
            place.city.as_ref(),
            place.state.as_ref(),
            place.country.as_ref(),
            place.country_code.as_ref(),
            place.location.as_ref(),
        ],
        None => [None; 5],
    };
    PLACE_TAGS
        .iter()
        .enumerate()
        .map(|(index, (tag, key))| match parts[index / 2] {
            Some(text) if !text.trim().is_empty() => set(tag, key, Value::from(text.trim())),
            _ => gone(tag, key),
        })
        .collect()
}

/// EXIF is what every reader believes, so it leads; the XMP and IPTC dates are made to agree with
/// it, and the EXIF-shaped XMP dates old Shotwell wrote are taken away.
fn taken_assigns(taken: Option<&Taken>) -> Result<Vec<Assign>, String> {
    let bare = || {
        DATE_TAGS
            .iter()
            .map(|(tag, key)| gone(tag, key))
            .chain([
                gone("XMP-xmp:CreateDate", "XMP-xmp:CreateDate"),
                gone("XMP-photoshop:DateCreated", "XMP-photoshop:DateCreated"),
                gone("XMP-exif:DateTimeOriginal", "XMP-exif:DateTimeOriginal"),
                gone("XMP-exif:DateTimeDigitized", "XMP-exif:DateTimeDigitized"),
                gone("IPTC:DateCreated", "IPTC:DateCreated"),
                gone("IPTC:TimeCreated", "IPTC:TimeCreated"),
            ])
            .collect()
    };
    let Some(taken) = taken else { return Ok(bare()) };

    let (date, time) = split_timestamp(&taken.at)?;
    let offset = match &taken.offset {
        Some(offset) => Some(checked_offset(offset)?),
        None => None,
    };
    let exif_date = format!("{} {time}", date.replace('-', ":"));
    let suffix = offset.clone().unwrap_or_default();

    let mut assigns = vec![
        set(DATE_TAGS[0].0, DATE_TAGS[0].1, Value::from(exif_date.clone())),
        set(DATE_TAGS[1].0, DATE_TAGS[1].1, Value::from(exif_date.clone())),
    ];
    for (tag, key) in &DATE_TAGS[2..] {
        assigns.push(match &offset {
            Some(offset) => set(tag, key, Value::from(offset.clone())),
            None => gone(tag, key),
        });
    }
    assigns.extend([
        set(
            "XMP-xmp:CreateDate",
            "XMP-xmp:CreateDate",
            Value::from(format!("{exif_date}{suffix}")),
        ),
        set(
            "XMP-photoshop:DateCreated",
            "XMP-photoshop:DateCreated",
            Value::from(format!("{exif_date}{suffix}")),
        ),
        gone("XMP-exif:DateTimeOriginal", "XMP-exif:DateTimeOriginal"),
        gone("XMP-exif:DateTimeDigitized", "XMP-exif:DateTimeDigitized"),
        set(
            "IPTC:DateCreated",
            "IPTC:DateCreated",
            Value::from(date.replace('-', ":")),
        ),
        // IPTC's time always carries a zone, and ExifTool fills in the computer's own when it is
        // given none: without an offset the field is left out rather than made up.
        match &offset {
            Some(offset) => set(
                "IPTC:TimeCreated",
                "IPTC:TimeCreated",
                Value::from(format!("{time}{offset}")),
            ),
            None => gone("IPTC:TimeCreated", "IPTC:TimeCreated"),
        },
    ]);
    Ok(assigns)
}

/// The one date format in, `YYYY-MM-DD` and `HH:MM:SS` out.
fn split_timestamp(at: &str) -> Result<(String, String), String> {
    let wrong = || format!("{at:?} is not YYYY-MM-DD HH:MM:SS");
    let (date, time) = at.trim().split_once(' ').ok_or_else(wrong)?;
    let digits = |text: &str, separator: char, parts: [usize; 3]| {
        let found: Vec<&str> = text.split(separator).collect();
        found.len() == 3
            && found
                .iter()
                .zip(parts)
                .all(|(part, width)| part.len() == width && part.chars().all(|c| c.is_ascii_digit()))
    };
    if !digits(date, '-', [4, 2, 2]) || !digits(time, ':', [2, 2, 2]) {
        return Err(wrong());
    }
    Ok((date.to_string(), time.to_string()))
}

fn checked_offset(offset: &str) -> Result<String, String> {
    let offset = offset.trim();
    let shape = offset.len() == 6
        && matches!(offset.as_bytes()[0], b'+' | b'-')
        && offset.as_bytes()[3] == b':'
        && offset[1..3]
            .chars()
            .chain(offset[4..6].chars())
            .all(|c| c.is_ascii_digit());
    match shape {
        true => Ok(offset.to_string()),
        false => Err(format!("{offset:?} is not an offset like +02:00")),
    }
}

/// The shape ExifTool reads an MWG region back in, so what we ask for and what we prove match.
fn face_assigns(faces: Option<&Faces>) -> Result<Vec<Assign>, String> {
    let Some(faces) = faces else {
        return Ok(vec![
            gone(REGION_INFO, REGION_INFO),
            gone(PERSON_IN_IMAGE, PERSON_IN_IMAGE),
        ]);
    };
    if faces.width <= 0 || faces.height <= 0 {
        return Err(format!("{} by {} is not a size", faces.width, faces.height));
    }
    if faces.faces.is_empty() {
        return Err("a region list without a single region is ignored by every reader".to_string());
    }

    let mut regions = Vec::new();
    let mut names = Vec::new();
    for face in &faces.faces {
        let name = face.name.trim();
        if name.is_empty() {
            return Err("an unnamed face is skipped by every reader".to_string());
        }
        for (value, what) in [
            (face.x, "x"),
            (face.y, "y"),
            (face.width, "width"),
            (face.height, "height"),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!("{name}: a {what} of {value} is not inside the picture"));
            }
        }
        if face.width == 0.0 || face.height == 0.0 {
            return Err(format!("{name}: a box with no size"));
        }
        let mut area = Map::new();
        area.insert("X".to_string(), Value::from(face.x));
        area.insert("Y".to_string(), Value::from(face.y));
        area.insert("W".to_string(), Value::from(face.width));
        area.insert("H".to_string(), Value::from(face.height));
        area.insert("Unit".to_string(), Value::from("normalized"));

        let mut region = Map::new();
        region.insert("Area".to_string(), Value::Object(area));
        region.insert("Name".to_string(), Value::from(name));
        region.insert("Type".to_string(), Value::from("Face"));
        regions.push(Value::Object(region));
        names.push(Value::from(name));
    }

    let mut applied = Map::new();
    applied.insert("W".to_string(), Value::from(faces.width));
    applied.insert("H".to_string(), Value::from(faces.height));
    applied.insert("Unit".to_string(), Value::from("pixel"));

    let mut info = Map::new();
    info.insert("AppliedToDimensions".to_string(), Value::Object(applied));
    info.insert("RegionList".to_string(), Value::Array(regions));

    Ok(vec![
        set(REGION_INFO, REGION_INFO, Value::Object(info)),
        set(PERSON_IN_IMAGE, PERSON_IN_IMAGE, Value::Array(names)),
    ])
}

/// The arguments for one photo. Every tag is emptied before it is filled: ExifTool *adds* to a list
/// it is given a value for, so setting one without clearing it first would append instead of
/// replace - and a scalar does not mind being deleted a moment before it is written.
pub fn arguments(assigns: &[Assign]) -> Vec<String> {
    let mut args = Vec::new();
    for assign in assigns {
        args.push(format!("-{}=", assign.tag));
        match &assign.value {
            Value::Null => {}
            Value::Array(items) => {
                for item in items {
                    args.push(format!("-{}={}", assign.tag, plain(item)));
                }
            }
            Value::Object(_) => args.push(format!("-{}={}", assign.tag, structured(&assign.value))),
            value => args.push(format!("-{}={}", assign.tag, plain(value))),
        }
    }
    args
}

fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// ExifTool's own syntax for a structure, with `|` escaping the characters that hold it together.
fn structured(value: &Value) -> String {
    match value {
        Value::Object(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("{name}={}", structured(value)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(structured).collect();
            format!("[{}]", inner.join(","))
        }
        other => escape(&plain(other)),
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '{' | '}' | '[' | ']' | ',' | '|') {
            out.push('|');
        }
        out.push(character);
    }
    out
}

/// Whether the file already says what an assignment wants, so it does not need writing again.
pub fn settled(assign: &Assign, fields: &Map<String, Value>) -> bool {
    same(&assign.value, fields.get(&assign.key))
}

fn same(wanted: &Value, found: Option<&Value>) -> bool {
    let found = match found {
        Some(Value::String(text)) if text.trim().is_empty() => None,
        other => other,
    };
    match wanted {
        Value::Null => found.is_none(),
        Value::Array(items) if items.is_empty() => found.is_none(),
        Value::Array(items) => match found {
            Some(found) => texts(items) == texts(&as_list(found)),
            None => false,
        },
        Value::Object(_) => found.is_some_and(|found| alike(wanted, found)),
        wanted => found.is_some_and(|found| alike(wanted, found)),
    }
}

/// A list field with one item reads back as that item, not as a list of one.
fn as_list(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.clone(),
        single => vec![single.clone()],
    }
}

/// A tag list is a set: the same tags in another order say the same thing.
fn texts(items: &[Value]) -> Vec<String> {
    let mut all: Vec<String> = items.iter().map(|item| plain(item).trim().to_string()).collect();
    all.sort();
    all.dedup();
    all
}

/// Inside a structure order matters, unlike a tag list, and a number that came back through a
/// rational is never bit-for-bit what went in.
fn alike(wanted: &Value, found: &Value) -> bool {
    match (wanted, found) {
        (Value::Object(wanted), Value::Object(found)) => wanted
            .iter()
            .all(|(name, value)| found.get(name).is_some_and(|found| alike(value, found))),
        (Value::Array(wanted), Value::Array(found)) => {
            wanted.len() == found.len() && wanted.iter().zip(found).all(|(one, other)| alike(one, other))
        }
        (Value::Number(wanted), found) => match (wanted.as_f64(), number(found)) {
            (Some(wanted), Some(found)) => (wanted - found).abs() < 1e-6,
            _ => false,
        },
        (wanted, found) => plain(wanted).trim() == plain(found).trim(),
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn by_tag(assigns: &[Assign], tag: &str) -> Value {
        assigns
            .iter()
            .find(|assign| assign.tag == tag)
            .unwrap_or_else(|| panic!("{tag} is not assigned"))
            .value
            .clone()
    }

    #[test]
    fn one_tag_reaches_all_five_fields_with_every_ancestor() {
        let assigns = tag_assigns(&["places/inChina/Beijing".to_string()]).unwrap();
        assert_eq!(assigns.len(), 5);

        let paths = vec!["places", "places/inChina", "places/inChina/Beijing"];
        assert_eq!(by_tag(&assigns, "XMP-digiKam:TagsList"), Value::from(paths.clone()));
        assert_eq!(by_tag(&assigns, "XMP-microsoft:LastKeywordXMP"), Value::from(paths));
        assert_eq!(
            by_tag(&assigns, "XMP-lr:HierarchicalSubject"),
            Value::from(vec!["places", "places|inChina", "places|inChina|Beijing"]),
            "lightroom separates the levels with a pipe"
        );

        let names = Value::from(vec!["Beijing", "inChina", "places"]);
        assert_eq!(by_tag(&assigns, "XMP-dc:Subject"), names);
        assert_eq!(by_tag(&assigns, "IPTC:Keywords"), names);
    }

    #[test]
    fn a_tag_with_a_separator_in_a_level_is_refused() {
        assert!(tag_assigns(&["places/in|China".to_string()]).is_err());
        assert!(tag_assigns(&["places//Beijing".to_string()]).is_err());
        assert!(tag_assigns(&["/places".to_string()]).is_err());
        assert!(tag_assigns(&["places/".to_string()]).is_err());
    }

    #[test]
    fn no_tags_at_all_empties_every_field() {
        let assigns = tag_assigns(&[]).unwrap();
        assert_eq!(assigns.len(), 5);
        for assign in &assigns {
            assert_eq!(assign.value, Value::Array(vec![]), "{}", assign.tag);
        }
        assert_eq!(arguments(&assigns[..1]), vec!["-XMP-digiKam:TagsList="]);
    }

    #[test]
    fn a_position_is_a_size_and_a_hemisphere() {
        let gps = gps_assigns(Some(Gps {
            lat: -33.8568,
            lon: -70.6693,
            altitude: Some(-12.5),
            derived: None,
        }))
        .unwrap();
        assert_eq!(by_tag(&gps, "EXIF:GPSLatitude"), Value::from(33.8568));
        assert_eq!(by_tag(&gps, "EXIF:GPSLatitudeRef"), Value::from("S"));
        assert_eq!(by_tag(&gps, "EXIF:GPSLongitudeRef"), Value::from("W"));
        assert_eq!(by_tag(&gps, "EXIF:GPSAltitude"), Value::from(12.5));
        assert_eq!(by_tag(&gps, "EXIF:GPSAltitudeRef"), Value::from(1));
        assert_eq!(by_tag(&gps, "EXIF:GPSMapDatum"), Value::from("WGS-84"));

        assert_eq!(
            by_tag(&gps, "EXIF:GPSProcessingMethod"),
            Value::Null,
            "measured, so no mark"
        );
        assert_eq!(by_tag(&gps, "EXIF:GPSHPositioningError"), Value::Null);

        let cleared = gps_assigns(None).unwrap();
        assert_eq!(cleared.len(), 9);
        assert!(cleared.iter().all(|assign| assign.value == Value::Null));
        assert!(
            gps_assigns(Some(Gps {
                lat: 100.0,
                lon: 0.0,
                altitude: None,
                derived: None,
            }))
            .is_err()
        );
    }

    #[test]
    fn a_position_without_an_altitude_takes_the_old_one_away() {
        let gps = gps_assigns(Some(Gps {
            lat: 39.9,
            lon: 116.4,
            altitude: None,
            derived: None,
        }))
        .unwrap();
        assert_eq!(by_tag(&gps, "EXIF:GPSAltitude"), Value::Null);
    }

    #[test]
    fn a_derived_position_says_so_and_how_far_off_it_may_be() {
        let derived = |method: &'static str, metres: f64| {
            gps_assigns(Some(Gps {
                lat: 39.9042,
                lon: 116.4074,
                altitude: None,
                derived: Some(Derived { method, metres }),
            }))
        };
        let gps = derived("photoManager: places tag", 5000.0).unwrap();
        assert_eq!(
            by_tag(&gps, "EXIF:GPSProcessingMethod"),
            Value::from("photoManager: places tag")
        );
        assert_eq!(by_tag(&gps, "EXIF:GPSHPositioningError"), Value::from(5000.0));
        assert!(is_derived("photoManager: places tag"));
        assert!(!is_derived("GPS"));

        assert!(derived("GPS", 5000.0).is_err(), "only a mark of our own is written");
        assert!(derived("photoManager: ", 5000.0).is_err(), "it says how");
        assert!(derived("photoManager: places tag", 0.0).is_err());
        assert!(derived("photoManager: places tag", f64::NAN).is_err());
    }

    #[test]
    fn a_date_carries_its_offset_into_every_field() {
        let assigns = taken_assigns(Some(&Taken {
            at: "2006-09-14 10:12:00".to_string(),
            offset: Some("+02:00".to_string()),
        }))
        .unwrap();
        assert_eq!(
            by_tag(&assigns, "EXIF:DateTimeOriginal"),
            Value::from("2006:09:14 10:12:00")
        );
        assert_eq!(by_tag(&assigns, "EXIF:OffsetTimeOriginal"), Value::from("+02:00"));
        assert_eq!(
            by_tag(&assigns, "XMP-xmp:CreateDate"),
            Value::from("2006:09:14 10:12:00+02:00")
        );
        assert_eq!(by_tag(&assigns, "IPTC:DateCreated"), Value::from("2006:09:14"));
        assert_eq!(by_tag(&assigns, "IPTC:TimeCreated"), Value::from("10:12:00+02:00"));
        assert_eq!(
            by_tag(&assigns, "XMP-exif:DateTimeOriginal"),
            Value::Null,
            "the date old Shotwell wrote in the wrong format goes"
        );
    }

    #[test]
    fn a_date_without_an_offset_writes_no_offset() {
        let assigns = taken_assigns(Some(&Taken {
            at: "2006-09-14 10:12:00".to_string(),
            offset: None,
        }))
        .unwrap();
        assert_eq!(by_tag(&assigns, "EXIF:OffsetTimeOriginal"), Value::Null);
        assert_eq!(
            by_tag(&assigns, "XMP-xmp:CreateDate"),
            Value::from("2006:09:14 10:12:00")
        );
    }

    #[test]
    fn a_date_that_is_not_the_one_format_is_refused() {
        for bad in ["2006:09:14 10:12:00", "2006-9-14 10:12:00", "2006-09-14", "nonsense"] {
            assert!(
                taken_assigns(Some(&Taken {
                    at: bad.to_string(),
                    offset: None
                }))
                .is_err(),
                "{bad} was accepted"
            );
        }
        assert!(
            taken_assigns(Some(&Taken {
                at: "2006-09-14 10:12:00".to_string(),
                offset: Some("+2".to_string())
            }))
            .is_err()
        );
    }

    #[test]
    fn a_face_becomes_a_region_in_the_shape_it_reads_back_in() {
        let assigns = face_assigns(Some(&Faces {
            width: 640,
            height: 480,
            faces: vec![Face {
                name: "Koch, Daniel".to_string(),
                x: 0.5,
                y: 0.4,
                width: 0.2,
                height: 0.3,
            }],
        }))
        .unwrap();
        let text = arguments(&assigns[..1]).remove(1);
        assert!(text.contains("AppliedToDimensions={"), "{text}");
        assert!(text.contains("Unit=pixel"), "{text}");
        assert!(text.contains("Name=Koch|, Daniel"), "a comma is escaped: {text}");
        assert!(text.contains("Type=Face"), "{text}");
        assert_eq!(by_tag(&assigns, PERSON_IN_IMAGE), Value::from(vec!["Koch, Daniel"]));
    }

    #[test]
    fn a_face_outside_the_picture_or_without_a_name_is_refused() {
        let one = |face: Face| {
            face_assigns(Some(&Faces {
                width: 640,
                height: 480,
                faces: vec![face],
            }))
        };
        let good = Face {
            name: "Ben".to_string(),
            x: 0.5,
            y: 0.5,
            width: 0.2,
            height: 0.2,
        };
        assert!(one(good.clone()).is_ok());
        assert!(one(Face { x: 1.4, ..good.clone() }).is_err());
        assert!(
            one(Face {
                name: "  ".to_string(),
                ..good.clone()
            })
            .is_err()
        );
        assert!(one(Face { width: 0.0, ..good }).is_err());
        assert!(
            face_assigns(Some(&Faces {
                width: 640,
                height: 480,
                faces: vec![]
            }))
            .is_err()
        );
    }

    #[test]
    fn a_place_writes_both_spellings_and_empties_what_it_is_not_given() {
        let assigns = place_assigns(Some(&Place {
            city: Some("Beijing".to_string()),
            country: Some("China".to_string()),
            ..Place::default()
        }));
        assert_eq!(by_tag(&assigns, "XMP-photoshop:City"), Value::from("Beijing"));
        assert_eq!(by_tag(&assigns, "IPTC:City"), Value::from("Beijing"));
        assert_eq!(
            by_tag(&assigns, "IPTC:Country-PrimaryLocationName"),
            Value::from("China")
        );
        assert_eq!(by_tag(&assigns, "XMP-photoshop:State"), Value::Null);
        assert_eq!(by_tag(&assigns, "XMP-iptcCore:CountryCode"), Value::Null);
        assert_eq!(place_assigns(None).len(), 10);
    }

    #[test]
    fn a_change_that_sets_one_tag_twice_is_refused() {
        let twice = Change::of([Field::Rating(Some(1)), Field::Rating(Some(2))]);
        assert!(twice.assigns().is_err());
    }

    #[test]
    fn what_is_already_right_is_settled() {
        let rating = &rating_assigns(Some(3)).unwrap()[0];
        assert!(settled(rating, &found(&[("XMP-xmp:Rating", Value::from(3))])));
        assert!(
            settled(rating, &found(&[("XMP-xmp:Rating", Value::from("3"))])),
            "a number that came back as text"
        );
        assert!(!settled(rating, &found(&[("XMP-xmp:Rating", Value::from(4))])));
        assert!(!settled(rating, &found(&[])));

        let cleared = &rating_assigns(None).unwrap()[0];
        assert!(settled(cleared, &found(&[])));
        assert!(settled(cleared, &found(&[("XMP-xmp:Rating", Value::from(""))])));
        assert!(!settled(cleared, &found(&[("XMP-xmp:Rating", Value::from(3))])));
    }

    #[test]
    fn a_tag_list_is_the_same_set_in_any_order() {
        let assign = &tag_assigns(&["a/b".to_string()]).unwrap()[3];
        assert_eq!(assign.tag, "XMP-dc:Subject");
        assert!(settled(
            assign,
            &found(&[("XMP-dc:Subject", Value::from(vec!["b", "a"]))])
        ));
        assert!(!settled(assign, &found(&[("XMP-dc:Subject", Value::from(vec!["a"]))])));
    }

    #[test]
    fn a_single_item_that_came_back_bare_is_still_a_list() {
        let assign = &tag_assigns(&["only".to_string()]).unwrap()[0];
        assert!(settled(
            assign,
            &found(&[("XMP-digiKam:TagsList", Value::from("only"))])
        ));
    }

    #[test]
    fn a_position_is_settled_within_a_rounding() {
        let assigns = gps_assigns(Some(Gps {
            lat: 39.9,
            lon: 116.4,
            altitude: None,
            derived: None,
        }))
        .unwrap();
        let latitude = assigns.iter().find(|a| a.tag == "EXIF:GPSLatitude").unwrap();
        assert!(settled(latitude, &found(&[("GPS:GPSLatitude", Value::from(39.9))])));
        assert!(settled(
            latitude,
            &found(&[("GPS:GPSLatitude", Value::from(39.90000001))])
        ));
        assert!(!settled(latitude, &found(&[("GPS:GPSLatitude", Value::from(39.91))])));
        assert!(
            !settled(latitude, &found(&[("Composite:GPSLatitude", Value::from(39.9))])),
            "the key it reads back under is the one that counts"
        );
    }

    #[test]
    fn a_region_is_settled_by_what_it_says_not_by_what_else_is_there() {
        let assigns = face_assigns(Some(&Faces {
            width: 640,
            height: 480,
            faces: vec![Face {
                name: "Ben".to_string(),
                x: 0.5,
                y: 0.4,
                width: 0.2,
                height: 0.3,
            }],
        }))
        .unwrap();
        let read: Value = serde_json::from_str(
            r#"{"AppliedToDimensions":{"H":480,"Unit":"pixel","W":640},
                "RegionList":[{"Area":{"H":0.3,"Unit":"normalized","W":0.2,"X":0.5,"Y":0.4},
                "Name":"Ben","Type":"Face"}]}"#,
        )
        .unwrap();
        assert!(settled(&assigns[0], &found(&[(REGION_INFO, read.clone())])));

        let moved: Value = serde_json::from_str(
            r#"{"AppliedToDimensions":{"H":480,"Unit":"pixel","W":640},
                "RegionList":[{"Area":{"H":0.3,"Unit":"normalized","W":0.2,"X":0.9,"Y":0.4},
                "Name":"Ben","Type":"Face"}]}"#,
        )
        .unwrap();
        assert!(!settled(&assigns[0], &found(&[(REGION_INFO, moved)])));
    }

    #[test]
    fn every_tag_is_emptied_before_it_is_filled() {
        let assigns = tag_assigns(&["a/b".to_string()]).unwrap();
        assert_eq!(
            arguments(&assigns[..1]),
            vec![
                "-XMP-digiKam:TagsList=",
                "-XMP-digiKam:TagsList=a",
                "-XMP-digiKam:TagsList=a/b",
            ]
        );
        assert_eq!(
            arguments(&rating_assigns(Some(3)).unwrap()),
            vec!["-XMP-xmp:Rating=", "-XMP-xmp:Rating=3"],
            "a scalar is replaced, not added to"
        );
        assert_eq!(arguments(&rating_assigns(None).unwrap()), vec!["-XMP-xmp:Rating="]);
    }
}
