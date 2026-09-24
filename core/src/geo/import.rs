//! Turning the GeoNames dumps into the place database. Reads a directory, writes `geo.db`, and
//! touches nothing else.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::params;

use super::{Error, Geo, Result, fold};
use crate::clock::{now, stamp};

/// The four files an import needs, in the order it reads them.
pub const DUMPS: [&str; 4] = [
    "countryInfo.txt",
    "admin1CodesASCII.txt",
    "cities1000.txt",
    "shapes_simplified_low.json",
];

/// The same files where they come from.
pub const SOURCE: &str = "https://download.geonames.org/export/dump/";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Imported {
    pub countries: usize,
    pub areas: usize,
    pub places: usize,
    pub names: usize,
    pub rings: usize,
    pub seconds: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum Step {
    Reading(&'static str),
    Places(usize, usize),
}

/// Reads every dump from `dir` and replaces what is in the database.
pub fn run(geo: &mut Geo, dir: &Path, progress: &dyn Fn(Step)) -> Result<Imported> {
    let started = Instant::now();
    let mut sources = Vec::new();
    for name in DUMPS {
        sources.push(find(dir, name)?);
    }

    let transaction = geo.connection.transaction()?;
    for table in [
        "meta", "country", "area", "place", "name", "place_at", "ring", "ring_at",
    ] {
        transaction.execute(&format!("DELETE FROM {table}"), [])?;
    }
    transaction.execute("INSERT INTO name_search (name_search) VALUES ('delete-all')", [])?;

    progress(Step::Reading(DUMPS[0]));
    let countries = countries(&transaction, &read(&sources[0])?)?;
    progress(Step::Reading(DUMPS[1]));
    let areas = areas(&transaction, &read(&sources[1])?)?;
    progress(Step::Reading(DUMPS[2]));
    let (places, names) = places(&transaction, &read(&sources[2])?, progress)?;
    progress(Step::Reading(DUMPS[3]));
    let rings = rings(&transaction, &read(&sources[3])?, &countries.1)?;

    let note = |key: &str, value: &str| -> Result<()> {
        transaction.execute("INSERT INTO meta (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    };
    note("dump date", &newest(&sources))?;
    note("imported at", &now())?;
    note("source", SOURCE)?;
    transaction.commit()?;
    geo.connection.execute_batch("ANALYZE; VACUUM")?;
    // Without this the write-ahead log keeps a second copy of the whole database on disk.
    geo.connection.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;

    Ok(Imported {
        countries: countries.0,
        areas,
        places,
        names,
        rings,
        seconds: started.elapsed().as_secs(),
    })
}

/// Whether every dump an import needs is already in the directory, so it can be imported again
/// without a download.
pub fn present(dir: &Path) -> bool {
    DUMPS.iter().all(|name| find(dir, name).is_ok())
}

/// The dump itself, or the zip it arrives in. GeoNames ships `cities1000.txt` as
/// `cities1000.zip` but `shapes_simplified_low.json` as `shapes_simplified_low.json.zip`, so
/// both spellings are looked for.
fn find(dir: &Path, name: &str) -> Result<PathBuf> {
    let plain = dir.join(name);
    if plain.is_file() {
        return Ok(plain);
    }
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    for zipped in [format!("{name}.zip"), format!("{stem}.zip")] {
        let zipped = dir.join(zipped);
        if zipped.is_file() {
            return Ok(zipped);
        }
    }
    Err(Error::Missing(plain))
}

/// The one file inside the zip. `cities1000.zip` holds `cities1000.txt`, so the name of the
/// zip is not always the name of what is in it.
fn entry_of(source: &Path, archive: &mut zip::ZipArchive<std::fs::File>) -> Result<String> {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    if archive.by_name(&stem).is_ok() {
        return Ok(stem);
    }
    archive
        .file_names()
        .find(|name| name.starts_with(&stem))
        .map(|name| name.to_string())
        .ok_or_else(|| Error::Malformed(format!("{} holds no {stem}", source.display())))
}

fn read(source: &Path) -> Result<String> {
    if source.extension().is_some_and(|extension| extension == "zip") {
        let file = std::fs::File::open(source)?;
        let mut archive =
            zip::ZipArchive::new(file).map_err(|error| Error::Malformed(format!("{}: {error}", source.display())))?;
        let wanted = entry_of(source, &mut archive)?;
        let mut entry = archive
            .by_name(&wanted)
            .map_err(|error| Error::Malformed(format!("{} holds no {wanted}: {error}", source.display())))?;
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        return Ok(text);
    }
    Ok(std::fs::read_to_string(source)?)
}

/// `countryInfo.txt`: comment lines, then one line per country.
fn countries(
    transaction: &rusqlite::Transaction<'_>,
    text: &str,
) -> Result<(usize, std::collections::HashMap<String, String>)> {
    let mut statement =
        transaction.prepare("INSERT INTO country (code, name, continent, geoname_id) VALUES (?1, ?2, ?3, ?4)")?;
    let mut by_geoname = std::collections::HashMap::new();
    let mut count = 0;

    for line in text.lines().filter(|line| !line.starts_with('#') && !line.is_empty()) {
        let field: Vec<&str> = line.split('\t').collect();
        if field.len() < 17 || field[0].len() != 2 {
            continue;
        }
        let geoname_id: Option<i64> = field[16].parse().ok();
        statement.execute(params![field[0], field[4], field[8], geoname_id])?;
        if let Some(id) = geoname_id {
            by_geoname.insert(id.to_string(), field[0].to_string());
        }
        count += 1;
    }
    if count == 0 {
        return Err(Error::Malformed("countryInfo.txt holds no countries".to_string()));
    }
    Ok((count, by_geoname))
}

/// `admin1CodesASCII.txt`: `CC.admin1`, name, ascii name, geonameid.
fn areas(transaction: &rusqlite::Transaction<'_>, text: &str) -> Result<usize> {
    let mut statement = transaction.prepare("INSERT INTO area (code, country, name) VALUES (?1, ?2, ?3)")?;
    let mut count = 0;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let field: Vec<&str> = line.split('\t').collect();
        let Some((country, _)) = field[0].split_once('.') else {
            continue;
        };
        if field.len() < 2 {
            continue;
        }
        statement.execute(params![field[0], country, field[1]])?;
        count += 1;
    }
    Ok(count)
}

/// `cities1000.txt`: one line per place, with its alternate names in the fourth field.
fn places(transaction: &rusqlite::Transaction<'_>, text: &str, progress: &dyn Fn(Step)) -> Result<(usize, usize)> {
    let total = text.lines().count();
    let mut place = transaction.prepare(
        "INSERT INTO place (id, name, country, area, feature, population, lat, lon, zone)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut at = transaction.prepare("INSERT INTO place_at VALUES (?1, ?2, ?3, ?4, ?5)")?;
    let mut name = transaction.prepare("INSERT INTO name (place_id, folded, own) VALUES (?1, ?2, ?3)")?;
    let mut search = transaction.prepare("INSERT INTO name_search (rowid, folded) VALUES (?1, ?2)")?;

    let mut places = 0;
    let mut names = 0;
    let mut folded = HashSet::new();

    for (index, line) in text.lines().enumerate() {
        let field: Vec<&str> = line.split('\t').collect();
        if field.len() < 15 {
            continue;
        }
        let (Ok(id), Ok(lat), Ok(lon)) = (
            field[0].parse::<i64>(),
            field[4].parse::<f64>(),
            field[5].parse::<f64>(),
        ) else {
            continue;
        };
        let population: i64 = field[14].parse().unwrap_or(0);
        let area = (!field[10].is_empty()).then(|| format!("{}.{}", field[8], field[10]));
        let zone = field.get(17).filter(|zone| !zone.is_empty());
        place.execute(params![
            id, field[1], field[8], area, field[7], population, lat, lon, zone
        ])?;
        at.execute(params![id, lat, lat, lon, lon])?;
        places += 1;

        folded.clear();
        let own = [field[1], field[2]];
        let alternates = field[3].split(',').filter(|value| !value.is_empty());
        for (spelling, is_own) in own
            .iter()
            .map(|value| (*value, true))
            .chain(alternates.map(|value| (value, false)))
        {
            let key = fold(spelling);
            if key.is_empty() || !folded.insert(key.clone()) {
                continue;
            }
            name.execute(params![id, key, is_own as i64])?;
            search.execute(params![transaction.last_insert_rowid(), key])?;
            names += 1;
        }

        if index % 20_000 == 0 {
            progress(Step::Places(index, total));
        }
    }
    progress(Step::Places(total, total));
    if places == 0 {
        return Err(Error::Malformed("cities1000.txt holds no places".to_string()));
    }
    Ok((places, names))
}

/// `shapes_simplified_low.json`: a GeoJSON collection, one feature per country, keyed by the
/// country's own geoname id. Rings are stored as plain coordinate pairs with a box around them.
fn rings(
    transaction: &rusqlite::Transaction<'_>,
    text: &str,
    by_geoname: &std::collections::HashMap<String, String>,
) -> Result<usize> {
    let collection: serde_json::Value =
        serde_json::from_str(text).map_err(|error| Error::Malformed(format!("the shapes are not json: {error}")))?;
    let features = collection["features"]
        .as_array()
        .ok_or_else(|| Error::Malformed("the shapes hold no features".to_string()))?;

    let mut ring = transaction.prepare("INSERT INTO ring (country, shape, hole, points) VALUES (?1, ?2, ?3, ?4)")?;
    let mut at = transaction.prepare("INSERT INTO ring_at VALUES (?1, ?2, ?3, ?4, ?5)")?;
    let mut count = 0;

    for feature in features {
        let geoname_id = feature["properties"]["geoNameId"].as_str().unwrap_or_default();
        let Some(country) = by_geoname.get(geoname_id) else {
            continue;
        };
        let geometry = &feature["geometry"];
        let shapes: Vec<&serde_json::Value> = match geometry["type"].as_str() {
            Some("Polygon") => Some(vec![&geometry["coordinates"]]),
            Some("MultiPolygon") => geometry["coordinates"].as_array().map(|all| all.iter().collect()),
            _ => None,
        }
        .unwrap_or_default();

        for (shape, polygon) in shapes.iter().enumerate() {
            let Some(loops) = polygon.as_array() else { continue };
            for (hole, points) in loops.iter().enumerate() {
                let Some((blob, box_of)) = coordinates(points) else {
                    continue;
                };
                ring.execute(params![country, shape as i64, hole as i64, blob])?;
                let id = transaction.last_insert_rowid();
                at.execute(params![id, box_of.0, box_of.1, box_of.2, box_of.3])?;
                count += 1;
            }
        }
    }
    Ok(count)
}

type Box = (f64, f64, f64, f64);

/// A ring as pairs of little-endian f32, plus the box around it.
fn coordinates(points: &serde_json::Value) -> Option<(Vec<u8>, Box)> {
    let points = points.as_array()?;
    if points.len() < 3 {
        return None;
    }
    let mut blob = Vec::with_capacity(points.len() * 8);
    let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
    let (mut min_lon, mut max_lon) = (f64::MAX, f64::MIN);

    for point in points {
        let pair = point.as_array()?;
        let lon = pair.first()?.as_f64()?;
        let lat = pair.get(1)?.as_f64()?;
        blob.extend_from_slice(&(lon as f32).to_le_bytes());
        blob.extend_from_slice(&(lat as f32).to_le_bytes());
        min_lat = min_lat.min(lat);
        max_lat = max_lat.max(lat);
        min_lon = min_lon.min(lon);
        max_lon = max_lon.max(lon);
    }
    Some((blob, (min_lat, max_lat, min_lon, max_lon)))
}

/// The newest modification time of the dumps, which is what "how old is this data" means.
fn newest(sources: &[PathBuf]) -> String {
    sources
        .iter()
        .filter_map(|source| source.metadata().ok()?.modified().ok())
        .max()
        .map(stamp)
        .unwrap_or_else(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::Counts;

    /// The checked-in excerpt of the dumps, so the tests never need the network.
    pub fn dumps() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/geo/dumps")
    }

    fn geo(name: &str) -> Geo {
        let dir = std::env::temp_dir().join(format!("photomanager-import-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        Geo::open(&dir.join("geo.db")).unwrap()
    }

    #[test]
    fn the_excerpt_imports_into_the_counts_it_should() {
        let mut geo = geo("excerpt");
        let imported = run(&mut geo, &dumps(), &|_| {}).unwrap();

        assert_eq!(imported.places, 151);
        assert_eq!(imported.countries, 18);
        assert_eq!(imported.areas, 63);
        assert!(imported.names > imported.places, "every place has more than one name");
        assert!(imported.rings > 7, "seven countries, more than seven rings");

        let counts = geo.counts().unwrap();
        assert_eq!(
            counts,
            Counts {
                countries: imported.countries as i64,
                areas: imported.areas as i64,
                places: imported.places as i64,
                names: imported.names as i64,
                rings: imported.rings as i64,
            }
        );
        assert!(geo.is_filled());
        assert_eq!(geo.dump_date().map(|date| date.len()), Some(19));
        assert_eq!(geo.imported_at().map(|date| date.len()), Some(19));
    }

    #[test]
    fn importing_twice_replaces_rather_than_duplicates() {
        let mut geo = geo("twice");
        let once = run(&mut geo, &dumps(), &|_| {}).unwrap();
        let twice = run(&mut geo, &dumps(), &|_| {}).unwrap();
        assert_eq!(once.places, twice.places);
        assert_eq!(once.names, twice.names);
        assert_eq!(geo.counts().unwrap().places, once.places as i64);
        assert_eq!(geo.counts().unwrap().names, once.names as i64);
    }

    #[test]
    fn the_dumps_are_read_the_way_geonames_ships_them() {
        let dir = std::env::temp_dir().join("photomanager-dumps-zipped");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["countryInfo.txt", "admin1CodesASCII.txt"] {
            std::fs::copy(dumps().join(name), dir.join(name)).unwrap();
        }
        // cities1000.txt arrives as cities1000.zip, the shapes as shapes_simplified_low.json.zip.
        for (name, archive) in [
            ("cities1000.txt", "cities1000.zip"),
            ("shapes_simplified_low.json", "shapes_simplified_low.json.zip"),
        ] {
            let file = std::fs::File::create(dir.join(archive)).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut writer, &std::fs::read(dumps().join(name)).unwrap()).unwrap();
            writer.finish().unwrap();
        }

        let mut geo = geo("zipped");
        let imported = run(&mut geo, &dir, &|_| {}).unwrap();
        assert_eq!(imported.places, 151, "the same import, still in its zips");
        assert!(imported.rings > 7);
    }

    #[test]
    fn a_missing_or_malformed_dump_says_so() {
        let mut geo = geo("missing");
        let empty = std::env::temp_dir().join("photomanager-import-empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(matches!(run(&mut geo, &empty, &|_| {}), Err(Error::Missing(_))));

        let broken = std::env::temp_dir().join("photomanager-import-broken");
        let _ = std::fs::remove_dir_all(&broken);
        std::fs::create_dir_all(&broken).unwrap();
        for name in DUMPS {
            std::fs::copy(dumps().join(name), broken.join(name)).unwrap();
        }
        std::fs::write(broken.join("shapes_simplified_low.json"), "{ not json").unwrap();
        assert!(matches!(run(&mut geo, &broken, &|_| {}), Err(Error::Malformed(_))));
        assert_eq!(
            geo.counts().unwrap(),
            Counts::default(),
            "a failed import leaves nothing"
        );
    }
}
