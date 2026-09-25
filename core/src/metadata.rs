//! What a photo says about itself. Read in process while scanning; ExifTool is the reference
//! the fast reader is measured against, and the fallback if the system library moves on.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Once;

use crate::write::Face;

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
    /// Every tag path any tag field holds, and each flat keyword that is no level of one.
    pub tags: Vec<String>,
    /// The five tag fields do not say the same thing with every level, or a keyword is left in
    /// the label or the catalog sets.
    pub tags_untidy: bool,
    /// The city the location text names, from the XMP or the IPTC field.
    pub location_city: Option<String>,
    /// The face regions and the persons it names, when it says anything about either.
    pub regions: Option<Regions>,
    pub raw: String,
}

/// Who a photo says is in it: the MWG face regions with the size they were measured against, and
/// the IPTC persons. The same shape a write gives them, so the two compare.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Regions {
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// In the order the file lists them. A region without a name has an empty one.
    pub faces: Vec<Face>,
    pub persons: Vec<String>,
}

impl Regions {
    /// `None` for a photo that says nothing about who is in it.
    fn said(self) -> Option<Regions> {
        (!self.faces.is_empty() || !self.persons.is_empty()).then_some(self)
    }

    /// As one line of JSON, the way the cache keeps it.
    pub fn written(&self) -> String {
        serde_json::json!({
            "w": self.width,
            "h": self.height,
            "faces": self.faces.iter().map(|face| serde_json::json!({
                "name": face.name, "x": face.x, "y": face.y, "w": face.width, "h": face.height,
            })).collect::<Vec<serde_json::Value>>(),
            "persons": self.persons,
        })
        .to_string()
    }

    pub fn read(text: &str) -> Option<Regions> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let number = |value: &serde_json::Value, name: &str| value.get(name).and_then(serde_json::Value::as_f64);
        Some(Regions {
            width: value.get("w").and_then(serde_json::Value::as_i64),
            height: value.get("h").and_then(serde_json::Value::as_i64),
            faces: value
                .get("faces")?
                .as_array()?
                .iter()
                .map(|face| Face {
                    name: face
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    x: number(face, "x").unwrap_or_default(),
                    y: number(face, "y").unwrap_or_default(),
                    width: number(face, "w").unwrap_or_default(),
                    height: number(face, "h").unwrap_or_default(),
                })
                .collect(),
            persons: value
                .get("persons")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(String::from)
                .collect(),
        })
    }
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

/// The three fields that hold whole paths, then the two that hold only the names.
const TAG_FIELDS: [&str; 5] = [
    "Xmp.digiKam.TagsList",
    "Xmp.lr.hierarchicalSubject",
    "Xmp.MicrosoftPhoto.LastKeywordXMP",
    "Xmp.dc.subject",
    "Iptc.Application2.Keywords",
];
const LEFTOVERS: [&str; 2] = ["Xmp.xmp.Label", "Xmp.mediapro.CatalogSets"];

impl Reader for Exiv2 {
    fn read(&self, _path: &Path, bytes: &[u8]) -> Result<Metadata> {
        start();
        let source = rexiv2::Metadata::new_from_buffer(bytes).map_err(|error| Error::Unreadable(error.to_string()))?;
        let string = |tag: &str| source.get_tag_string(tag).ok().filter(|value| !value.is_empty());
        let number = |tag: &str| source.has_tag(tag).then(|| i64::from(source.get_tag_numeric(tag)));

        let gps = source.get_gps_info();
        let fields = TAG_FIELDS.map(|field| {
            source
                .get_tag_multiple_strings(field)
                .unwrap_or_default()
                .iter()
                .map(|value| normalise_tag(value))
                .collect::<Vec<String>>()
        });
        let (tags, tags_untidy) = tags_of(&fields, LEFTOVERS.iter().any(|field| source.has_tag(field)));

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
            tags_untidy,
            location_city: string("Xmp.photoshop.City").or_else(|| string("Iptc.Application2.City")),
            regions: exiv2_regions(&source),
            raw: raw_json(&source),
        })
    }
}

const REGIONS: &str = "Xmp.mwg-rs.Regions/";
const REGION_LIST: &str = "Xmp.mwg-rs.Regions/mwg-rs:RegionList[";

/// exiv2 flattens the structure into one tag per value:
/// `Xmp.mwg-rs.Regions/mwg-rs:RegionList[2]/mwg-rs:Area/stArea:x`.
fn exiv2_regions(source: &rexiv2::Metadata) -> Option<Regions> {
    let text = |tag: &str| source.get_tag_string(tag).ok().map(|value| value.trim().to_string());
    let number = |tag: &str| text(tag).and_then(|value| value.parse::<f64>().ok());
    let mut count = 0;
    for tag in source.get_xmp_tags().unwrap_or_default() {
        if let Some(rest) = tag.strip_prefix(REGION_LIST)
            && let Some((index, _)) = rest.split_once(']')
            && let Ok(index) = index.parse::<usize>()
        {
            count = count.max(index);
        }
    }
    let faces = (1..=count)
        .map(|index| {
            let field = |name: &str| format!("{REGION_LIST}{index}]/{name}");
            Face {
                name: text(&field("mwg-rs:Name")).unwrap_or_default(),
                x: number(&field("mwg-rs:Area/stArea:x")).unwrap_or_default(),
                y: number(&field("mwg-rs:Area/stArea:y")).unwrap_or_default(),
                width: number(&field("mwg-rs:Area/stArea:w")).unwrap_or_default(),
                height: number(&field("mwg-rs:Area/stArea:h")).unwrap_or_default(),
            }
        })
        .collect();
    let dimension =
        |name: &str| number(&format!("{REGIONS}mwg-rs:AppliedToDimensions/stDim:{name}")).map(|value| value as i64);
    Regions {
        width: dimension("w"),
        height: dimension("h"),
        faces,
        persons: source
            .get_tag_multiple_strings("Xmp.iptcExt.PersonInImage")
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    }
    .said()
}

/// ExifTool reads the structure whole with `-struct`.
fn exiftool_regions(fields: &serde_json::Map<String, serde_json::Value>) -> Option<Regions> {
    let info = fields.get("RegionInfo");
    let number = |value: Option<&serde_json::Value>| match value {
        Some(serde_json::Value::Number(number)) => number.as_f64(),
        Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    };
    let faces = info
        .and_then(|info| info.get("RegionList"))
        .map(|list| match list {
            serde_json::Value::Array(items) => items.clone(),
            one => vec![one.clone()],
        })
        .unwrap_or_default()
        .iter()
        .map(|region| Face {
            name: region
                .get("Name")
                .map(|name| crate::write::change::shown(Some(name)))
                .unwrap_or_default()
                .trim()
                .to_string(),
            x: number(region.pointer("/Area/X")).unwrap_or_default(),
            y: number(region.pointer("/Area/Y")).unwrap_or_default(),
            width: number(region.pointer("/Area/W")).unwrap_or_default(),
            height: number(region.pointer("/Area/H")).unwrap_or_default(),
        })
        .collect();
    let persons = match fields.get("PersonInImage") {
        Some(serde_json::Value::Array(names)) => names
            .iter()
            .map(|name| crate::write::change::shown(Some(name)))
            .collect(),
        Some(name) => vec![crate::write::change::shown(Some(name))],
        None => Vec::new(),
    };
    Regions {
        width: number(info.and_then(|info| info.pointer("/AppliedToDimensions/W"))).map(|value| value as i64),
        height: number(info.and_then(|info| info.pointer("/AppliedToDimensions/H"))).map(|value| value as i64),
        faces,
        persons: persons
            .into_iter()
            .map(|name: String| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect(),
    }
    .said()
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
        let tag_fields = [
            "TagsList",
            "HierarchicalSubject",
            "LastKeywordXMP",
            "Subject",
            "Keywords",
        ]
        .map(|key| list(key).unwrap_or_default());
        let leftovers = ["Label", "CatalogSets"].iter().any(|key| fields.contains_key(*key));
        let (tags, tags_untidy) = tags_of(&tag_fields, leftovers);

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
            tags,
            tags_untidy,
            location_city: string("City").filter(|city| !city.is_empty()),
            regions: exiftool_regions(fields),
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

/// The tags of a photo from its five tag fields, and whether they disagree. Every path of the
/// three hierarchical fields counts, so a tag written to only one of them is not lost, and so
/// does a flat keyword that names no level of any path. Tidy is what a write leaves: every path
/// with all its levels in the hierarchical fields, every level's name in the flat ones, and
/// nothing in the label or the catalog sets.
fn tags_of(fields: &[Vec<String>; 5], leftovers: bool) -> (Vec<String>, bool) {
    let set =
        |field: &[String]| -> BTreeSet<String> { field.iter().filter(|value| !value.is_empty()).cloned().collect() };
    let mut paths: BTreeSet<String> = fields[..3].iter().flat_map(|field| set(field)).collect();
    let orphans: Vec<String> = {
        let names: BTreeSet<&str> = paths.iter().flat_map(|path| path.split('/')).collect();
        fields[3..]
            .iter()
            .flat_map(|field| set(field))
            .filter(|keyword| !names.contains(keyword.as_str()))
            .collect()
    };
    paths.extend(orphans);

    let every_level: BTreeSet<String> = paths
        .iter()
        .flat_map(|path| {
            path.match_indices('/')
                .map(|(at, _)| path[..at].to_string())
                .chain(std::iter::once(path.clone()))
        })
        .collect();
    let names: BTreeSet<String> = every_level
        .iter()
        .map(|path| path.rsplit('/').next().unwrap_or(path).to_string())
        .collect();
    let tidy = !leftovers
        && fields[..3].iter().all(|field| set(field) == every_level)
        && fields[3..].iter().all(|field| set(field) == names);
    (paths.into_iter().collect(), !tidy)
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
            assert_eq!(fast.tags_untidy, reference.tags_untidy, "{path}: tidy");
            assert_eq!(fast.location_city, reference.location_city, "{path}: city");
            match (fast.gps_lat, reference.gps_lat) {
                (Some(one), Some(other)) => assert!((one - other).abs() < 0.0001, "{path}: latitude"),
                (one, other) => assert_eq!(one.is_some(), other.is_some(), "{path}: latitude"),
            }
        }
    }

    #[test]
    fn both_readers_read_the_same_regions() {
        let root = library("regions");
        let file = root.join("Germany/2019-07-13 Sommerfest/img_0657.jpg");
        let status = Command::new("exiftool")
            .args([
                "-q",
                "-overwrite_original",
                "-XMP-mwg-rs:RegionInfo={AppliedToDimensions={W=24,H=16,Unit=pixel},RegionList=[\
                 {Area={X=0.5,Y=0.4,W=0.2,H=0.3,Unit=normalized},Name=Ben,Type=Face},\
                 {Area={X=0.123456,Y=0.4,W=0.2,H=0.3,Unit=normalized},Name=Anna Maria,Type=Face}]}",
                "-XMP-iptcExt:PersonInImage=Ben",
                "-XMP-iptcExt:PersonInImage=Anna Maria",
            ])
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
        let fast = read(&Exiv2, &file).regions.expect("regions");
        assert_eq!(fast, read(&ExifTool, &file).regions.expect("regions"));
        assert_eq!((fast.width, fast.height), (Some(24), Some(16)));
        assert_eq!(fast.persons, ["Ben", "Anna Maria"]);
        assert_eq!(fast.faces[1].name, "Anna Maria");
        assert_eq!(fast.faces[1].x, 0.123456);
        assert_eq!(Regions::read(&fast.written()), Some(fast));
        assert_eq!(read(&Exiv2, &root.join("China/IMG_3140.JPG")).regions, None);
    }

    #[test]
    fn tidy_is_every_level_in_every_field_and_no_leftovers() {
        let paths = |items: &[&str]| items.iter().map(|item| item.to_string()).collect::<Vec<String>>();
        let full = paths(&["places", "places/inChina", "places/inChina/Beijing"]);
        let names = paths(&["Beijing", "inChina", "places"]);
        let written = [full.clone(), full.clone(), full.clone(), names.clone(), names.clone()];
        assert_eq!(tags_of(&written, false), (full.clone(), false));
        assert!(tags_of(&written, true).1, "a keyword in the label is untidy");

        let shotwell = [
            paths(&["places/inChina/Beijing"]),
            vec![],
            paths(&["places/inChina/Beijing"]),
            paths(&["Beijing"]),
            paths(&["Beijing"]),
        ];
        assert_eq!(tags_of(&shotwell, false), (paths(&["places/inChina/Beijing"]), true));

        let apart = [
            paths(&["people/Anna"]),
            vec![],
            paths(&["people/Anna", "people/Tom"]),
            paths(&["Anna", "Kira"]),
            vec![],
        ];
        assert_eq!(
            tags_of(&apart, false).0,
            paths(&["Kira", "people/Anna", "people/Tom"]),
            "a tag in one field only is kept, and so is a keyword that is no level"
        );
        assert_eq!(
            tags_of(&Default::default(), false),
            (vec![], false),
            "no tags at all is tidy"
        );
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
