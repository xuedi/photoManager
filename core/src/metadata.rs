//! What a photo says about itself. Read in process while scanning; ExifTool is the reference
//! the fast reader is measured against, and the fallback if the system library moves on.

use std::path::Path;
use std::process::Command;
use std::sync::Once;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub taken_at: Option<String>,
    pub taken_offset: Option<String>,
    pub xmp_taken_at: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// `GPSProcessingMethod`: how the position was worked out, as the photo says it.
    pub gps_method: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub orientation: Option<i64>,
    pub rating: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub tags: Vec<String>,
    /// The city the location text names, from the XMP or the IPTC field.
    pub location_city: Option<String>,
    pub raw: String,
}

#[derive(Debug)]
pub enum Error {
    Unreadable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreadable(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub trait Reader: Send + Sync {
    fn read(&self, path: &Path, bytes: &[u8]) -> Result<Metadata>;
}

impl Metadata {
    pub fn empty() -> Metadata {
        Metadata {
            raw: "{}".to_string(),
            ..Metadata::default()
        }
    }
}

/// exiv2 wants one initialisation per process, before the first file.
pub(crate) fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        rexiv2::initialize().expect("initialise the metadata library");
    });
}

/// The in-process reader: exiv2 through gexiv2, a few milliseconds per photo.
#[derive(Debug, Default, Clone, Copy)]
pub struct Exiv2;

const TAG_FIELDS: [&str; 5] = [
    "Xmp.digiKam.TagsList",
    "Xmp.lr.hierarchicalSubject",
    "Xmp.MicrosoftPhoto.LastKeywordXMP",
    "Xmp.dc.subject",
    "Iptc.Application2.Keywords",
];

impl Reader for Exiv2 {
    fn read(&self, _path: &Path, bytes: &[u8]) -> Result<Metadata> {
        start();
        let source = rexiv2::Metadata::new_from_buffer(bytes).map_err(|error| Error::Unreadable(error.to_string()))?;
        let string = |tag: &str| source.get_tag_string(tag).ok().filter(|value| !value.is_empty());
        let number = |tag: &str| source.has_tag(tag).then(|| i64::from(source.get_tag_numeric(tag)));

        let gps = source.get_gps_info();
        let tags = TAG_FIELDS
            .iter()
            .find_map(|field| {
                let values = source.get_tag_multiple_strings(field).ok()?;
                (!values.is_empty()).then(|| values.iter().map(|value| normalise_tag(value)).collect::<Vec<_>>())
            })
            .unwrap_or_default();

        Ok(Metadata {
            taken_at: string("Exif.Photo.DateTimeOriginal").map(|value| as_timestamp(&value)),
            taken_offset: string("Exif.Photo.OffsetTimeOriginal"),
            xmp_taken_at: string("Xmp.xmp.CreateDate")
                .or_else(|| string("Xmp.exif.DateTimeOriginal"))
                .map(|value| as_timestamp(&value)),
            gps_lat: gps.map(|gps| gps.latitude),
            gps_lon: gps.map(|gps| gps.longitude),
            gps_method: string("Exif.GPSInfo.GPSProcessingMethod").and_then(|value| method(&value)),
            camera_make: string("Exif.Image.Make"),
            camera_model: string("Exif.Image.Model"),
            orientation: number("Exif.Image.Orientation"),
            rating: number("Xmp.xmp.Rating").or_else(|| number("Exif.Image.Rating")),
            width: (source.get_pixel_width() > 0).then(|| i64::from(source.get_pixel_width())),
            height: (source.get_pixel_height() > 0).then(|| i64::from(source.get_pixel_height())),
            tags,
            location_city: string("Xmp.photoshop.City").or_else(|| string("Iptc.Application2.City")),
            raw: raw_json(&source),
        })
    }
}

fn raw_json(source: &rexiv2::Metadata) -> String {
    let mut fields = serde_json::Map::new();
    let groups = [source.get_exif_tags(), source.get_iptc_tags(), source.get_xmp_tags()];
    for group in groups.into_iter().flatten() {
        for tag in group {
            // Asked for a list, gexiv2 repeats a plain XMP text once per character of it.
            let values = match rexiv2::get_tag_type(&tag) {
                Ok(rexiv2::TagType::XmpText) => Vec::new(),
                _ => source.get_tag_multiple_strings(&tag).unwrap_or_default(),
            };
            let value = match values.len() {
                0 => match source.get_tag_string(&tag) {
                    Ok(single) => serde_json::Value::String(single),
                    Err(_) => continue,
                },
                1 => serde_json::Value::String(values[0].clone()),
                _ => serde_json::Value::from(values),
            };
            fields.insert(tag, value);
        }
    }
    serde_json::Value::Object(fields).to_string()
}

/// The reference reader. Slower, but it is the tool that also writes.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExifTool;

impl Reader for ExifTool {
    fn read(&self, path: &Path, _bytes: &[u8]) -> Result<Metadata> {
        let output = Command::new("exiftool")
            .args(["-j", "-n", "-q", "-struct"])
            .arg(path)
            .output()
            .map_err(|error| Error::Unreadable(format!("run exiftool: {error}")))?;
        let parsed: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| Error::Unreadable(error.to_string()))?;
        let fields = parsed
            .get(0)
            .and_then(|entry| entry.as_object())
            .ok_or_else(|| Error::Unreadable("exiftool returned nothing".to_string()))?;

        let string = |key: &str| fields.get(key).and_then(|value| value.as_str()).map(String::from);
        let number = |key: &str| fields.get(key).and_then(|value| value.as_i64());
        let float = |key: &str| fields.get(key).and_then(|value| value.as_f64());
        let list = |key: &str| match fields.get(key) {
            Some(serde_json::Value::Array(values)) => Some(
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .map(normalise_tag)
                    .collect::<Vec<_>>(),
            ),
            Some(serde_json::Value::String(value)) => Some(vec![normalise_tag(value)]),
            _ => None,
        };

        Ok(Metadata {
            taken_at: string("DateTimeOriginal").map(|value| as_timestamp(&value)),
            taken_offset: string("OffsetTimeOriginal"),
            xmp_taken_at: string("CreateDate").map(|value| as_timestamp(&value)),
            gps_lat: float("GPSLatitude"),
            gps_lon: float("GPSLongitude"),
            gps_method: string("GPSProcessingMethod").and_then(|value| method(&value)),
            camera_make: string("Make"),
            camera_model: string("Model"),
            orientation: number("Orientation"),
            rating: number("Rating"),
            width: number("ImageWidth"),
            height: number("ImageHeight"),
            tags: [
                "TagsList",
                "HierarchicalSubject",
                "LastKeywordXMP",
                "Subject",
                "Keywords",
            ]
            .iter()
            .find_map(|key| list(key))
            .unwrap_or_default(),
            location_city: string("City").filter(|city| !city.is_empty()),
            raw: serde_json::Value::Object(fields.clone()).to_string(),
        })
    }
}

/// exiv2 puts the character set in front of a comment-like text: `charset=Ascii GPS`.
fn method(value: &str) -> Option<String> {
    let value = value.trim();
    let text = match value.strip_prefix("charset=") {
        Some(rest) => rest.split_once(' ').map(|(_, text)| text).unwrap_or_default(),
        None => value,
    };
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!text.is_empty()).then(|| text.to_string())
}

/// Lightroom separates the levels with a pipe, everyone else with a slash.
fn normalise_tag(tag: &str) -> String {
    tag.replace('|', "/").trim().to_string()
}

/// EXIF writes `2006:09:14 10:12:00`, XMP writes ISO 8601; we keep one format everywhere.
fn as_timestamp(value: &str) -> String {
    let value = value.trim();
    let (date, rest) = match value.split_once([' ', 'T']) {
        Some((date, rest)) => (date, rest),
        None => (value, ""),
    };
    let date = date.replace(':', "-");
    let time: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == ':').collect();
    match time.is_empty() {
        true => date,
        false => format!("{date} {time}"),
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn library(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("photomanager-metadata-{name}"));
        crate::fixtures::build(&root).expect("build the fixture library");
        root
    }

    fn read<R: Reader>(reader: &R, file: &Path) -> Metadata {
        reader.read(file, &std::fs::read(file).unwrap()).unwrap()
    }

    #[test]
    fn reads_back_what_the_fixture_wrote() {
        let root = library("reads-wrote");
        let photo = root.join("Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG");
        let found = read(&Exiv2, &photo);

        assert_eq!(found.taken_at.as_deref(), Some("2018-10-06 14:02:11"));
        assert_eq!(found.camera_make.as_deref(), Some("FUJIFILM"));
        assert_eq!(found.camera_model.as_deref(), Some("X100S"));
        assert!(found.tags.contains(&"places/inDenmark/Copenhagen".to_string()));
        assert!(found.tags.contains(&"events/2018 Wedding Trip".to_string()));
        assert!(found.raw.contains("Xmp.digiKam.TagsList"), "raw fields are kept");

        let bare = read(
            &Exiv2,
            &root.join("Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG"),
        );
        assert!(bare.tags.is_empty());
        assert_eq!(bare.gps_lat, None);
    }

    #[test]
    fn reads_gps_orientation_and_the_xmp_date() {
        let root = library("reads-date");
        let located = read(&Exiv2, &root.join("Germany/2019-07-13 Sommerfest/img_0657.jpg"));
        assert_eq!(located.orientation, Some(6));
        assert!((located.gps_lat.unwrap() - 53.5511).abs() < 0.0001);
        assert!((located.gps_lon.unwrap() - 9.9937).abs() < 0.0001);

        let shifted = read(&Exiv2, &root.join("Germany/2019-07-13 Sommerfest/p1000003.jpg"));
        assert_eq!(shifted.taken_at.as_deref(), Some("2019-07-13 19:05:00"));
        assert_eq!(shifted.xmp_taken_at.as_deref(), Some("2019-07-13 17:05:00"));

        let undated = read(&Exiv2, &root.join("China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG"));
        assert_eq!(undated.taken_at, None);
    }

    #[test]
    fn both_readers_agree() {
        let root = library("both-agree");
        for path in crate::fixtures::photo_paths() {
            let file = root.join(path);
            let fast = read(&Exiv2, &file);
            let reference = read(&ExifTool, &file);

            assert_eq!(fast.taken_at, reference.taken_at, "{path}: date");
            assert_eq!(fast.camera_model, reference.camera_model, "{path}: model");
            assert_eq!(fast.orientation, reference.orientation, "{path}: orientation");
            assert_eq!(fast.tags, reference.tags, "{path}: tags");
            assert_eq!(fast.location_city, reference.location_city, "{path}: city");
            match (fast.gps_lat, reference.gps_lat) {
                (Some(one), Some(other)) => assert!((one - other).abs() < 0.0001, "{path}: latitude"),
                (one, other) => assert_eq!(one.is_some(), other.is_some(), "{path}: latitude"),
            }
        }
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join("photomanager-metadata-broken");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("truncated.jpg");
        std::fs::write(&file, &include_bytes!("fixtures/p01.jpg")[..40]).unwrap();

        assert!(Exiv2.read(&file, &std::fs::read(&file).unwrap()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
