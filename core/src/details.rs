//! Everything the cache knows about one photo, for looking at it closely, and the form a person
//! edits it in. Nothing here opens the file: the details are as fresh as the last scan.
//!
//! The form holds text, the way a person typed it. `Details::change_to` compares it with what the
//! photo says, parses only what differs, and hands over a `Change` of exactly those fields, so
//! a form nobody touched is an empty change and a field emptied is a field taken away.

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::cache::{Cache, Result};
use crate::clock::stamp;
use crate::filter::Gap;
use crate::write::{Change, Field, Gps, Place, Taken};

/// One photo, as the last scan read it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    pub rel_path: String,
    pub size: u64,
    /// When the file last changed, as the scan saw it, in UTC.
    pub changed: String,
    pub content_id: Option<String>,
    pub taken_at: Option<String>,
    pub taken_offset: Option<String>,
    pub xmp_taken_at: Option<String>,
    /// The date the folder states, as far as it states one: `2019-07-13`, `2018-10` or `2008`.
    pub folder_date: Option<String>,
    /// Whether the photo's date agrees with its folder's, by the rule the dashboard counts with.
    /// `None` when there is nothing to compare.
    pub agrees: Option<bool>,
    /// The country, city and event name the folders say.
    pub country: Option<String>,
    pub city: Option<String>,
    pub event: Option<String>,
    pub gps: Option<(f64, f64)>,
    pub altitude: Option<f64>,
    /// The place in words, as the photo says it: XMP first, IPTC where XMP says nothing.
    pub place: Place,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub orientation: Option<i64>,
    pub rating: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub tags: Vec<String>,
    /// Names from `people` tags, either spelling of the root, and from face regions.
    pub people: Vec<String>,
    /// Each issue the scan found, and what it said about it.
    pub issues: Vec<(String, Option<String>)>,
    /// Every field the scan read, by name.
    pub raw: Vec<(String, String)>,
}

/// What the edit form holds: text as it was typed, and the tags and rating as they were picked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Edited {
    /// `YYYY-MM-DD HH:MM:SS`, or empty for no date.
    pub taken_at: String,
    /// `+HH:MM` or `-HH:MM`, or empty.
    pub offset: String,
    /// `latitude, longitude` in decimal degrees, or empty.
    pub position: String,
    pub place: Place,
    pub tags: Vec<String>,
    pub rating: Option<i64>,
}

/// Where each part of the place is read from: XMP, then IPTC.
const PLACE_FIELDS: [[&str; 2]; 5] = [
    ["Xmp.photoshop.City", "Iptc.Application2.City"],
    ["Xmp.photoshop.State", "Iptc.Application2.ProvinceState"],
    ["Xmp.photoshop.Country", "Iptc.Application2.CountryName"],
    ["Xmp.iptcCore.CountryCode", "Iptc.Application2.CountryCode"],
    ["Xmp.iptcCore.Location", "Iptc.Application2.SubLocation"],
];

const REGIONS: &str = "Xmp.mwg-rs.Regions/";
const REGION_NAME: &str = "/mwg-rs:Name";
const PERSON_IN_IMAGE: &str = "Xmp.iptcExt.PersonInImage";
const ALTITUDE: &str = "Exif.GPSInfo.GPSAltitude";
const ALTITUDE_REF: &str = "Exif.GPSInfo.GPSAltitudeRef";

impl Details {
    /// The photo at this path, or `None` when the cache does not know it.
    pub fn of(cache: &Cache, rel_path: &str) -> Result<Option<Details>> {
        let connection = cache.connection();
        let gap = Gap::DateOffFolder;
        let sql = format!(
            "SELECT id, size, mtime_ns, content_id, country, city, event_name, event_year, event_month,
                event_day, taken_at, taken_offset, xmp_taken_at, gps_lat, gps_lon, camera_make,
                camera_model, orientation, rating, width, height, raw,
                CASE WHEN {} THEN ({}) END
             FROM photo p WHERE rel_path = ?1",
            gap.measured(),
            gap.missing()
        );
        let found = connection
            .query_row(&sql, params![rel_path], |row| {
                let year: Option<i64> = row.get(7)?;
                let month: Option<i64> = row.get(8)?;
                let day: Option<i64> = row.get(9)?;
                let off: Option<bool> = row.get(22)?;
                let details = Details {
                    rel_path: rel_path.to_string(),
                    size: row.get::<_, i64>(1)? as u64,
                    changed: changed(row.get(2)?),
                    content_id: row.get(3)?,
                    country: row.get(4)?,
                    city: row.get(5)?,
                    event: row.get(6)?,
                    folder_date: folder_date(year, month, day),
                    taken_at: row.get(10)?,
                    taken_offset: row.get(11)?,
                    xmp_taken_at: row.get(12)?,
                    gps: row.get::<_, Option<f64>>(13)?.zip(row.get::<_, Option<f64>>(14)?),
                    camera_make: row.get(15)?,
                    camera_model: row.get(16)?,
                    orientation: row.get(17)?,
                    rating: row.get(18)?,
                    width: row.get(19)?,
                    height: row.get(20)?,
                    agrees: off.map(|off| !off),
                    ..Details::default()
                };
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(21)?, details))
            })
            .optional()?;
        let Some((id, raw, mut details)) = found else {
            return Ok(None);
        };

        let mut statement = connection.prepare("SELECT path FROM tag WHERE photo_id = ?1 ORDER BY path")?;
        details.tags = statement
            .query_map(params![id], |row| row.get(0))?
            .collect::<Result<Vec<String>>>()?;
        let mut statement = connection.prepare("SELECT kind, detail FROM issue WHERE rel_path = ?1 ORDER BY kind")?;
        details.issues = statement
            .query_map(params![rel_path], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>>>()?;

        details.raw = raw_fields(&raw);
        details.place = place(&details.raw);
        details.altitude = altitude(&details.raw);
        details.people = people(&details.tags, &details.raw);
        Ok(Some(details))
    }

    /// The tags below `places`, in either spelling of the root.
    pub fn place_tags(&self) -> Vec<&str> {
        self.tags
            .iter()
            .filter(|tag| root(tag).eq_ignore_ascii_case("places"))
            .map(String::as_str)
            .collect()
    }

    /// What a raw field says, by its exact name.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.raw
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The form as it starts: what the photo says now.
    pub fn edited(&self) -> Edited {
        Edited {
            taken_at: self.taken_at.clone().unwrap_or_default(),
            offset: self.taken_offset.clone().unwrap_or_default(),
            position: self.gps.map(|(lat, lon)| position(lat, lon)).unwrap_or_default(),
            place: self.place.clone(),
            tags: self.tags.clone(),
            rating: self.rating,
        }
    }

    /// Only the fields the form changed, parsed and checked. `Err` says what in the form is wrong.
    pub fn change_to(&self, edited: &Edited) -> std::result::Result<Change, String> {
        let before = self.edited();
        let mut fields = Vec::new();

        if edited.taken_at.trim() != before.taken_at || edited.offset.trim() != before.offset {
            let at = parse_date(&edited.taken_at)?;
            let offset = parse_offset(&edited.offset)?;
            fields.push(Field::Taken(match (at, offset) {
                (None, Some(_)) => return Err("an offset needs a date to belong to".to_string()),
                (None, None) => None,
                (Some(at), offset) => Some(Taken { at, offset }),
            }));
        }

        if edited.position.trim() != before.position {
            fields.push(Field::Gps(parse_position(&edited.position)?.map(|(lat, lon)| Gps {
                lat,
                lon,
                altitude: self.altitude,
            })));
        }

        let place = tidy_place(&edited.place);
        if place != tidy_place(&before.place) {
            let empty = place == Place::default();
            fields.push(Field::Place((!empty).then_some(place)));
        }

        let tags = tidy_tags(&edited.tags);
        if tags != tidy_tags(&before.tags) {
            if let Some(bad) = tags
                .iter()
                .find(|tag| tag.split('/').any(|level| level.trim().is_empty()))
            {
                return Err(format!("the tag {bad:?} has an empty level"));
            }
            fields.push(Field::Tags(tags));
        }

        if edited.rating != before.rating {
            if let Some(stars) = edited.rating
                && !(0..=5).contains(&stars)
            {
                return Err(format!("a rating of {stars} is not between 0 and 5"));
            }
            fields.push(Field::Rating(edited.rating));
        }
        Ok(Change::of(fields))
    }
}

/// Coordinates the way the form shows them and takes them back.
pub fn position(lat: f64, lon: f64) -> String {
    format!("{lat:.6}, {lon:.6}")
}

/// The one date format, checked down to the day of the month. Empty is no date.
pub fn parse_date(text: &str) -> std::result::Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let wrong = || format!("{text:?} is not a date like 2019-07-13 18:20:00");
    let (date, time) = text.split_once(' ').ok_or_else(wrong)?;
    let numbers = |part: &str, separator: char, widths: [usize; 3]| -> Option<[u32; 3]> {
        let found: Vec<&str> = part.split(separator).collect();
        if found.len() != 3 {
            return None;
        }
        let mut numbers = [0; 3];
        for ((number, text), width) in numbers.iter_mut().zip(&found).zip(widths) {
            if text.len() != width || !text.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            *number = text.parse().ok()?;
        }
        Some(numbers)
    };
    let [year, month, day] = numbers(date, '-', [4, 2, 2]).ok_or_else(wrong)?;
    let [hour, minute, second] = numbers(time.trim(), ':', [2, 2, 2]).ok_or_else(wrong)?;
    if !(1..=12).contains(&month) {
        return Err(format!("{text:?} has no month {month}"));
    }
    if day == 0 || day > days_in(year, month) {
        return Err(format!("{text:?} has no day {day} in its month"));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("{text:?} is not a time of day"));
    }
    Ok(Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    )))
}

/// `+HH:MM` or `-HH:MM`, as far as clocks on earth go. Empty is no offset.
pub fn parse_offset(text: &str) -> std::result::Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let wrong = || format!("{text:?} is not an offset like +02:00");
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 6
        && matches!(bytes[0], b'+' | b'-')
        && bytes[3] == b':'
        && [1, 2, 4, 5].iter().all(|at| bytes[*at].is_ascii_digit());
    if !shaped {
        return Err(wrong());
    }
    let hours: u32 = text[1..3].parse().map_err(|_| wrong())?;
    let minutes: u32 = text[4..6].parse().map_err(|_| wrong())?;
    if hours > 14 || minutes > 59 {
        return Err(format!("{text:?} is further from UTC than any clock"));
    }
    Ok(Some(text.to_string()))
}

/// `latitude, longitude` in decimal degrees; a comma, spaces or both between them. Empty is no
/// position. A pair that only makes sense the other way round is refused, not turned.
pub fn parse_position(text: &str) -> std::result::Result<Option<(f64, f64)>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let wrong = || format!("{text:?} is not a position like 53.551100, 9.993700");
    let parts: Vec<&str> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let [lat, lon] = parts.as_slice() else {
        return Err(wrong());
    };
    let (lat, lon): (f64, f64) = match (lat.parse(), lon.parse()) {
        (Ok(lat), Ok(lon)) => (lat, lon),
        _ => return Err(wrong()),
    };
    if !lat.is_finite() || !lon.is_finite() {
        return Err(wrong());
    }
    let is_lat = |value: f64| (-90.0..=90.0).contains(&value);
    let is_lon = |value: f64| (-180.0..=180.0).contains(&value);
    if !is_lat(lat) {
        let swapped = match is_lon(lat) && is_lat(lon) {
            true => ", so the longitude came first: the latitude goes first",
            false => "",
        };
        return Err(format!("a latitude of {lat} is not between -90 and 90{swapped}"));
    }
    if !is_lon(lon) {
        return Err(format!("a longitude of {lon} is not between -180 and 180"));
    }
    Ok(Some((lat, lon)))
}

fn days_in(year: u32, month: u32) -> u32 {
    match month {
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn changed(mtime_ns: i64) -> String {
    let since = std::time::Duration::from_nanos(mtime_ns.max(0) as u64);
    stamp(std::time::UNIX_EPOCH + since)
}

fn folder_date(year: Option<i64>, month: Option<i64>, day: Option<i64>) -> Option<String> {
    Some(match (year?, month, day) {
        (year, Some(month), Some(day)) => format!("{year:04}-{month:02}-{day:02}"),
        (year, Some(month), None) => format!("{year:04}-{month:02}"),
        (year, ..) => format!("{year:04}"),
    })
}

fn root(tag: &str) -> &str {
    tag.split('/').next().unwrap_or(tag)
}

fn raw_fields(raw: &str) -> Vec<(String, String)> {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut all: Vec<(String, String)> = fields
        .into_iter()
        .map(|(name, value)| {
            let text = match value {
                Value::String(text) => text,
                // A cache read before the scanner learnt better still holds a text repeated.
                Value::Array(items) if items.windows(2).all(|pair| pair[0] == pair[1]) => {
                    items.first().and_then(Value::as_str).unwrap_or_default().to_string()
                }
                Value::Array(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<String>>()
                    .join(", "),
                other => other.to_string(),
            };
            (name, text)
        })
        .collect();
    all.sort();
    all
}

fn lookup<'a>(raw: &'a [(String, String)], name: &str) -> Option<&'a str> {
    raw.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn place(raw: &[(String, String)]) -> Place {
    let part = |names: [&str; 2]| names.iter().find_map(|name| lookup(raw, name)).map(String::from);
    Place {
        city: part(PLACE_FIELDS[0]),
        state: part(PLACE_FIELDS[1]),
        country: part(PLACE_FIELDS[2]),
        country_code: part(PLACE_FIELDS[3]),
        location: part(PLACE_FIELDS[4]),
    }
}

/// exiv2 hands a rational over as `1234/10`.
fn altitude(raw: &[(String, String)]) -> Option<f64> {
    let text = lookup(raw, ALTITUDE)?;
    let metres = match text.split_once('/') {
        Some((top, bottom)) => {
            let bottom: f64 = bottom.trim().parse().ok()?;
            (bottom != 0.0).then_some(top.trim().parse::<f64>().ok()? / bottom)?
        }
        None => text.trim_end_matches('m').trim().parse().ok()?,
    };
    let below = lookup(raw, ALTITUDE_REF) == Some("1");
    Some(if below { -metres } else { metres })
}

/// The leaves of the `people` tags - a group like `people/family` is not a person - and every
/// name a face region or the IPTC person field gives.
fn people(tags: &[String], raw: &[(String, String)]) -> Vec<String> {
    let below: Vec<&String> = tags
        .iter()
        .filter(|tag| root(tag).eq_ignore_ascii_case("people") && tag.contains('/'))
        .collect();
    let mut names: Vec<String> = below
        .iter()
        .filter(|tag| !below.iter().any(|other| other.starts_with(&format!("{tag}/"))))
        .filter_map(|tag| tag.rsplit('/').next())
        .map(String::from)
        .collect();
    for (key, value) in raw {
        let named = (key.starts_with(REGIONS) && key.ends_with(REGION_NAME)) || key == PERSON_IN_IMAGE;
        if named {
            names.extend(
                value
                    .split(", ")
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(String::from),
            );
        }
    }
    names.sort();
    names.dedup();
    names
}

fn tidy_text(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

fn tidy_place(place: &Place) -> Place {
    Place {
        city: tidy_text(&place.city),
        state: tidy_text(&place.state),
        country: tidy_text(&place.country),
        country_code: tidy_text(&place.country_code),
        location: tidy_text(&place.location),
    }
}

fn tidy_tags(tags: &[String]) -> Vec<String> {
    let mut all: Vec<String> = tags
        .iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    all.sort();
    all.dedup();
    all
}

#[cfg(test)]
mod tests;
