//! Who is in the photos, as Immich knows it: read once over its API into a snapshot, never
//! written back. Immich stays read only, so nothing here sends anything but a read.
//!
//! The snapshot is Immich's data, not ours: the named persons, every asset with where it lies in
//! the library, and the faces of the assets a named person is in. It lives in the cache
//! directory beside the cache and is thrown away and fetched again at will. The people tool
//! reads it to decide what each photo should say about who is in it; the files stay the truth.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub mod boxes;
#[cfg(feature = "fixtures")]
pub mod fake;
mod snapshot;

pub use snapshot::{Asset, Face, Person, Snapshot};

/// The snapshot's file name, beside the cache.
pub const FILE: &str = "immich.db";

/// Nothing Immich answers is anywhere near this large; a page of a thousand assets is a few MB.
const LIMIT: u64 = 128 * 1024 * 1024;
const PAGE: usize = 1000;
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No answer from the address, or no Immich at it.
    Unreachable(String),
    KeyRefused,
    /// The key works but may not read this.
    Forbidden(String),
    /// Immich answered something this does not understand.
    Answer(String),
    Cancelled,
    /// The snapshot could not be written.
    Store(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(why) => write!(f, "Immich cannot be reached: {why}"),
            Error::KeyRefused => write!(f, "Immich refused the API key"),
            Error::Forbidden(what) => write!(f, "the API key is missing a permission: {what}"),
            Error::Answer(why) => write!(f, "Immich answered something unexpected: {why}"),
            Error::Cancelled => write!(f, "stopped before it was done, nothing was kept"),
            Error::Store(why) => write!(f, "the snapshot could not be kept: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Store(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where Immich is and the key to read it with. The key is never printed, not even in a debug
/// line.
pub struct Client {
    url: String,
    key: String,
    agent: ureq::Agent,
    page: usize,
    requests: usize,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("url", &self.url)
            .field("requests", &self.requests)
            .finish_non_exhaustive()
    }
}

/// `https://gallery.example.org`, from what a person typed: without a trailing slash or `/api`.
pub fn address(typed: &str) -> std::result::Result<String, String> {
    let trimmed = typed.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/api").unwrap_or(trimmed).trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| format!("{:?} is not an address starting with https://", typed.trim()))?;
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return Err(format!("{:?} is not an address", typed.trim()));
    }
    Ok(trimmed.to_string())
}

impl Client {
    pub fn new(url: &str, key: &str) -> std::result::Result<Client, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("there is no API key".to_string());
        }
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(TIMEOUT))
            .build()
            .new_agent();
        Ok(Client {
            url: address(url)?,
            key: key.to_string(),
            agent,
            page: PAGE,
            requests: 0,
        })
    }

    /// Smaller pages, so a test sees the paging.
    pub fn with_page(self, page: usize) -> Client {
        Client {
            page: page.max(1),
            ..self
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// How many requests it has sent.
    pub fn requests(&self) -> usize {
        self.requests
    }

    fn get(&mut self, path: &str) -> Result<Value> {
        self.requests += 1;
        let response = self
            .agent
            .get(format!("{}{path}", self.url))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .call();
        read(path, response)
    }

    fn post(&mut self, path: &str, body: &Value) -> Result<Value> {
        self.requests += 1;
        let response = self
            .agent
            .post(format!("{}{path}", self.url))
            .header("x-api-key", &self.key)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .send(body.to_string());
        read(path, response)
    }

    /// The import paths of the libraries the key can see.
    pub fn import_paths(&mut self) -> Result<Vec<String>> {
        let libraries = self.get("/api/libraries")?;
        let libraries = libraries
            .as_array()
            .ok_or_else(|| Error::Answer("the libraries are not a list".to_string()))?;
        let mut paths: Vec<String> = libraries
            .iter()
            .filter_map(|library| library.get("importPaths").and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str)
            .map(|path| path.trim_end_matches('/').to_string())
            .filter(|path| !path.is_empty())
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// Every person, named or not, hidden or not.
    pub fn people(&mut self, cancel: &AtomicBool) -> Result<Vec<Person>> {
        let mut people = Vec::new();
        for page in 1.. {
            stop_if(cancel)?;
            let answer = self.get(&format!("/api/people?withHidden=true&page={page}&size={}", self.page))?;
            let listed = answer
                .get("people")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::Answer("the people are not a list".to_string()))?;
            for person in listed {
                people.push(Person {
                    id: text(person, "id")?,
                    name: person
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    hidden: person.get("isHidden").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            let more = answer.get("hasNextPage").and_then(Value::as_bool).unwrap_or(false);
            if !more || listed.is_empty() {
                break;
            }
        }
        Ok(people)
    }

    /// Every asset of the key's user, with the persons Immich found in it, a page at a time.
    pub fn assets(&mut self, progress: &dyn Fn(usize), cancel: &AtomicBool) -> Result<Vec<Listed>> {
        let mut assets = Vec::new();
        let mut page = Value::from(1);
        loop {
            stop_if(cancel)?;
            let body = json!({
                "page": page,
                "size": self.page,
                "withPeople": true,
                "withExif": true,
            });
            let answer = self.post("/api/search/metadata", &body)?;
            let found = answer
                .pointer("/assets/items")
                .and_then(Value::as_array)
                .ok_or_else(|| Error::Answer("the assets are not a list".to_string()))?;
            for item in found {
                assets.push(Listed::of(item)?);
            }
            progress(assets.len());
            match answer.pointer("/assets/nextPage") {
                Some(Value::String(next)) if !next.is_empty() && !found.is_empty() => {
                    page = next
                        .parse::<i64>()
                        .map(Value::from)
                        .unwrap_or(Value::from(next.as_str()))
                }
                Some(Value::Number(next)) if !found.is_empty() => page = Value::Number(next.clone()),
                _ => break,
            }
        }
        Ok(assets)
    }

    /// The faces Immich found in one asset, in the pixels of its preview.
    pub fn faces(&mut self, asset: &str) -> Result<Vec<Face>> {
        let answer = self.get(&format!("/api/faces?id={asset}"))?;
        let listed = answer
            .as_array()
            .ok_or_else(|| Error::Answer("the faces are not a list".to_string()))?;
        listed
            .iter()
            .map(|face| {
                let number = |name: &str| {
                    face.get(name)
                        .and_then(Value::as_i64)
                        .ok_or_else(|| Error::Answer(format!("a face without {name}")))
                };
                Ok(Face {
                    id: text(face, "id")?,
                    asset_id: asset.to_string(),
                    person_id: face.pointer("/person/id").and_then(Value::as_str).map(String::from),
                    image_width: number("imageWidth")?,
                    image_height: number("imageHeight")?,
                    x1: number("boundingBoxX1")?,
                    y1: number("boundingBoxY1")?,
                    x2: number("boundingBoxX2")?,
                    y2: number("boundingBoxY2")?,
                })
            })
            .collect()
    }

    /// Whether the address and the key work, in words: how many persons are named and where the
    /// libraries are. Two requests, nothing kept.
    pub fn check(&mut self) -> Result<String> {
        let paths = self.import_paths()?;
        let answer = self.get("/api/people?withHidden=true&page=1&size=1")?;
        let total = answer.get("total").and_then(Value::as_i64).unwrap_or_default();
        let libraries = match paths.as_slice() {
            [] => "no external library".to_string(),
            [one] => format!("the library at {one}"),
            many => format!("libraries at {}", many.join(", ")),
        };
        Ok(format!("Immich answers: {total} persons, {libraries}"))
    }
}

fn read(path: &str, response: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value> {
    let mut response = response.map_err(|error| Error::Unreachable(error.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(LIMIT)
        .read_to_string()
        .map_err(|error| Error::Unreachable(error.to_string()))?;
    let said = || {
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("message")
                    .map(|message| crate::write::change::shown(Some(message)))
            })
            .unwrap_or_else(|| path.to_string())
    };
    match status {
        200..=299 => serde_json::from_str(&body).map_err(|_| Error::Answer(format!("{path} did not answer JSON"))),
        401 => Err(Error::KeyRefused),
        403 => Err(Error::Forbidden(said())),
        404 => Err(Error::Unreachable(format!("{path} is not there, is this Immich?"))),
        status => Err(Error::Answer(format!("{path} answered {status}: {}", said()))),
    }
}

fn text(value: &Value, name: &str) -> Result<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error::Answer(format!("something without {name}")))
}

fn stop_if(cancel: &AtomicBool) -> Result<()> {
    match cancel.load(Ordering::Relaxed) {
        true => Err(Error::Cancelled),
        false => Ok(()),
    }
}

/// An asset as the search lists it: where it is, whether it is there, how Immich saw its image,
/// and the persons in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub id: String,
    pub original_path: String,
    pub offline: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub people: Vec<String>,
}

impl Listed {
    fn of(item: &Value) -> Result<Listed> {
        let exif = |name: &str| item.pointer(&format!("/exifInfo/{name}"));
        let number = |value: Option<&Value>| match value {
            Some(Value::Number(number)) => number.as_i64(),
            Some(Value::String(text)) => text.trim().parse().ok(),
            _ => None,
        };
        Ok(Listed {
            id: text(item, "id")?,
            original_path: text(item, "originalPath")?,
            offline: item.get("isOffline").and_then(Value::as_bool).unwrap_or(false),
            width: number(exif("exifImageWidth")),
            height: number(exif("exifImageHeight")),
            orientation: number(exif("orientation")),
            people: item
                .get("people")
                .and_then(Value::as_array)
                .map(|people| {
                    people
                        .iter()
                        .filter_map(|person| person.get("id").and_then(Value::as_str))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// Where a fetch is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    People,
    /// How many assets are listed so far.
    Assets(usize),
    /// Faces of this many assets read, of how many.
    Faces(usize, usize),
    Keeping,
}

impl Step {
    pub fn tells(&self) -> String {
        match self {
            Step::People => "Reading the persons".to_string(),
            Step::Assets(listed) => format!("Listing the photos: {listed}"),
            Step::Faces(done, of) => format!("Reading the faces: {done} of {of}"),
            Step::Keeping => "Keeping the snapshot".to_string(),
        }
    }
}

/// What a fetch found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fetched {
    pub persons: usize,
    /// Named and not hidden: the ones that are asked about.
    pub named: usize,
    pub assets: usize,
    /// Assets with a face of a named person.
    pub with_named: usize,
    pub faces: usize,
    pub offline: usize,
    /// Assets outside every import path, so no photo of the library.
    pub outside: usize,
    pub requests: usize,
    pub seconds: u64,
}

/// Where each asset lies inside the library: its path with the import path taken off. The
/// longest import path that holds it wins.
pub fn relative(original: &str, prefixes: &[String]) -> Option<String> {
    prefixes
        .iter()
        .filter_map(|prefix| {
            let rest = original.strip_prefix(prefix.trim_end_matches('/'))?.strip_prefix('/')?;
            (!rest.is_empty()).then(|| (prefix.len(), rest.to_string()))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, rest)| rest)
}

/// Reads the persons, the assets and the faces of every asset a named person is in, and keeps
/// them as the snapshot at `file`, replacing the one there only once the new one is whole. A
/// fetch that is stopped or fails keeps nothing of itself. `prefix` is where the library lies
/// inside Immich; without one the libraries' own import paths are used.
pub fn fetch(
    client: &mut Client,
    file: &Path,
    prefix: Option<&str>,
    progress: &dyn Fn(Step),
    cancel: &AtomicBool,
) -> Result<Fetched> {
    let started = Instant::now();
    let prefixes = match prefix.map(str::trim).filter(|prefix| !prefix.is_empty()) {
        Some(prefix) => vec![prefix.trim_end_matches('/').to_string()],
        None => client.import_paths()?,
    };
    progress(Step::People);
    let people = client.people(cancel)?;
    let named: HashMap<&str, &Person> = people
        .iter()
        .filter(|person| person.named())
        .map(|person| (person.id.as_str(), person))
        .collect();

    let listed = client.assets(&|count| progress(Step::Assets(count)), cancel)?;
    let wanted: Vec<&Listed> = listed
        .iter()
        .filter(|asset| asset.people.iter().any(|id| named.contains_key(id.as_str())))
        .collect();
    let mut faces = Vec::new();
    for (done, asset) in wanted.iter().enumerate() {
        stop_if(cancel)?;
        progress(Step::Faces(done, wanted.len()));
        faces.extend(client.faces(&asset.id)?);
    }
    stop_if(cancel)?;
    progress(Step::Keeping);

    let assets: Vec<Asset> = listed
        .iter()
        .map(|asset| Asset {
            id: asset.id.clone(),
            original_path: asset.original_path.clone(),
            rel_path: relative(&asset.original_path, &prefixes),
            offline: asset.offline,
            width: asset.width,
            height: asset.height,
            orientation: asset.orientation,
        })
        .collect();
    let fetched = Fetched {
        persons: people.len(),
        named: named.len(),
        assets: assets.len(),
        with_named: wanted.len(),
        faces: faces.len(),
        offline: assets.iter().filter(|asset| asset.offline).count(),
        outside: assets.iter().filter(|asset| asset.rel_path.is_none()).count(),
        requests: client.requests(),
        seconds: started.elapsed().as_secs(),
    };
    let mut about = BTreeMap::new();
    about.insert("url", client.url().to_string());
    about.insert("prefixes", prefixes.join("\n"));
    about.insert("fetched-at", crate::clock::now());
    snapshot::keep(file, &people, &assets, &faces, &about)?;
    tracing::info!(
        persons = fetched.persons,
        named = fetched.named,
        assets = fetched.assets,
        faces = fetched.faces,
        requests = fetched.requests,
        seconds = fetched.seconds,
        "people fetched from Immich"
    );
    Ok(fetched)
}

/// The snapshot beside a cache file.
pub fn beside(cache_file: &Path) -> PathBuf {
    cache_file.with_file_name(FILE)
}

#[cfg(all(test, feature = "fixtures"))]
mod tests;
