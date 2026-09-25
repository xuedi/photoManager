//! A small Immich for tests: the four read endpoints the fetch uses, served from this process on
//! a free local port, with persons and faces over the stand-in library. Never the real one.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Face, Person};
use crate::metadata::{Exiv2, Reader};

/// The key the fake takes. Any other is refused.
pub const KEY: &str = "fake-immich-key-5d1e";
/// Where the stand-in library lies inside the fake.
pub const IMPORT: &str = "/gallery/test";

/// A turned photo, a photo already tagged with the person, and one with two people.
pub const TURNED: &str = "Germany/2019-07-13 Sommerfest/img_0657.jpg";
pub const TWO: &str = "Germany/2019-07-13 Sommerfest/IMAG0001.jpg";
pub const BEN_TAGGED: &str = "China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG";
pub const BEN_UNTAGGED: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";
pub const KIRA: &str = "Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG";
pub const LENA: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG";
pub const OFFLINE: &str = "Greece/0000-00-00 Aeron ilands/IMG_0005.JPG";
pub const RESIZED: &str = "Germany/2016-06-00 Harbour Walk/DSC_0101.JPG";
pub const GONE: &str = "Germany/2016-06-00 Harbour Walk/DSC_0404.JPG";
/// Faces only of a hidden person, and only of nobody.
pub const HIDDEN_ONLY: &str = "Germany/2014-03-22 Museum/IMG_9001.JPG";
pub const UNNAMED_ONLY: &str = "Germany/2014-03-22 Museum/IMG_9002.JPG";

#[derive(Debug, Clone)]
pub struct FakeAsset {
    pub id: String,
    pub path: String,
    pub offline: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub faces: Vec<Face>,
}

#[derive(Debug, Clone, Default)]
pub struct Data {
    pub import_paths: Vec<String>,
    pub people: Vec<Person>,
    pub assets: Vec<FakeAsset>,
}

impl Data {
    pub fn asset(&mut self, rel_path: &str) -> &mut FakeAsset {
        let path = format!("{IMPORT}/{rel_path}");
        self.assets
            .iter_mut()
            .find(|asset| asset.path == path)
            .unwrap_or_else(|| panic!("the fake has no {rel_path}"))
    }

    /// A face of this person on the asset, at a box given as fractions of the preview.
    pub fn add_face(&mut self, rel_path: &str, person: Option<&str>, left: f64, top: f64, right: f64, bottom: f64) {
        let count = self.assets.iter().map(|asset| asset.faces.len()).sum::<usize>();
        let asset = self.asset(rel_path);
        let (width, height) = preview(asset.width, asset.height, asset.orientation);
        let pixel = |fraction: f64, of: i64| (fraction * of as f64).round() as i64;
        asset.faces.push(Face {
            id: format!("face-{count}"),
            asset_id: asset.id.clone(),
            person_id: person.map(String::from),
            image_width: width,
            image_height: height,
            x1: pixel(left, width),
            y1: pixel(top, height),
            x2: pixel(right, width),
            y2: pixel(bottom, height),
        });
    }

    pub fn rename(&mut self, person: &str, name: &str) {
        for one in &mut self.people {
            if one.id == person {
                one.name = name.to_string();
            }
        }
    }
}

/// The preview Immich measures faces on: the photo turned the way it is shown, ten times the size.
fn preview(width: Option<i64>, height: Option<i64>, orientation: Option<i64>) -> (i64, i64) {
    let (width, height) = (width.unwrap_or(100) * 10, height.unwrap_or(100) * 10);
    match super::boxes::sideways(orientation) {
        true => (height, width),
        false => (width, height),
    }
}

fn person(id: &str, name: &str, hidden: bool) -> Person {
    Person {
        id: id.to_string(),
        name: name.to_string(),
        hidden,
    }
}

impl Data {
    /// Persons and faces over the stand-in library at `root`, each photo's size and orientation
    /// read from the file the way Immich reads it.
    pub fn over(root: &Path) -> Data {
        let mut data = Data {
            import_paths: vec![IMPORT.to_string()],
            people: vec![
                person("p-ben", "Ben", false),
                person("p-ann", "Ann", false),
                person("p-kira", "Kira", false),
                person("p-lena", "Lena Park", false),
                person("p-hidden", "Hidden Person", true),
                person("p-unnamed", "", false),
            ],
            assets: Vec::new(),
        };
        for (index, rel_path) in crate::fixtures::photo_paths().into_iter().enumerate() {
            let file = root.join(rel_path);
            let read = std::fs::read(&file)
                .ok()
                .and_then(|bytes| Exiv2.read(&file, &bytes).ok())
                .unwrap_or_default();
            data.assets.push(FakeAsset {
                id: format!("asset-{index}"),
                path: format!("{IMPORT}/{rel_path}"),
                offline: false,
                width: read.width,
                height: read.height,
                orientation: read.orientation,
                faces: Vec::new(),
            });
        }
        data.assets.push(FakeAsset {
            id: "asset-gone".to_string(),
            path: format!("{IMPORT}/{GONE}"),
            offline: false,
            width: Some(16),
            height: Some(16),
            orientation: None,
            faces: Vec::new(),
        });
        data.assets.push(FakeAsset {
            id: "asset-elsewhere".to_string(),
            path: "/upload/library/elsewhere.jpg".to_string(),
            offline: false,
            width: Some(16),
            height: Some(16),
            orientation: None,
            faces: Vec::new(),
        });

        data.add_face(TURNED, Some("p-ann"), 0.0625, 0.083333, 0.3125, 0.333333);
        data.add_face(TWO, Some("p-ann"), 0.1, 0.1, 0.4, 0.5);
        data.add_face(TWO, Some("p-ben"), 0.6, 0.2, 0.9, 0.6);
        data.add_face(BEN_UNTAGGED, Some("p-ben"), 0.2, 0.2, 0.5, 0.6);
        data.add_face(BEN_UNTAGGED, None, 0.6, 0.2, 0.8, 0.4);
        data.add_face(BEN_TAGGED, Some("p-ben"), 0.3, 0.3, 0.7, 0.8);
        data.add_face(KIRA, Some("p-kira"), 0.4, 0.1, 0.7, 0.5);
        data.add_face(KIRA, Some("p-hidden"), 0.05, 0.05, 0.2, 0.2);
        data.add_face(LENA, Some("p-lena"), 0.25, 0.25, 0.75, 0.75);
        data.add_face(OFFLINE, Some("p-ben"), 0.2, 0.2, 0.4, 0.4);
        data.asset(OFFLINE).offline = true;
        data.add_face(RESIZED, Some("p-ben"), 0.2, 0.2, 0.4, 0.4);
        data.asset(RESIZED).width = Some(999);
        data.add_face(GONE, Some("p-ben"), 0.2, 0.2, 0.4, 0.4);
        data.add_face(HIDDEN_ONLY, Some("p-hidden"), 0.2, 0.2, 0.4, 0.4);
        data.add_face(UNNAMED_ONLY, Some("p-unnamed"), 0.2, 0.2, 0.4, 0.4);
        data
    }
}

/// The fake, serving until it is dropped.
pub struct FakeImmich {
    pub url: String,
    pub data: Arc<Mutex<Data>>,
    /// Every request as `GET /api/people?...`, in order.
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    port: u16,
}

impl FakeImmich {
    pub fn serve(data: Data) -> FakeImmich {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free local port");
        let port = listener.local_addr().expect("a local address").port();
        let data = Arc::new(Mutex::new(data));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (served, asked, stopped) = (data.clone(), requests.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(stream) = stream {
                    answer(stream, &served, &asked);
                }
            }
        });
        FakeImmich {
            url: format!("http://127.0.0.1:{port}"),
            data,
            requests,
            stop,
            port,
        }
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn change(&self, change: impl FnOnce(&mut Data)) {
        change(&mut self.data.lock().unwrap());
    }
}

impl Drop for FakeImmich {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn answer(stream: TcpStream, data: &Mutex<Data>, requests: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream"));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let mut length = 0;
    let mut key = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "x-api-key" => key = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    requests.lock().unwrap().push(format!("{method} {target}"));

    let (status, reply) = match key.as_deref() == Some(KEY) {
        false => (401, json!({"message": "Invalid API key"})),
        true => route(method, target, &body, &data.lock().unwrap()),
    };
    let text = reply.to_string();
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.flush();
}

fn query(target: &str, name: &str) -> Option<String> {
    let (_, query) = target.split_once('?')?;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

fn route(method: &str, target: &str, body: &[u8], data: &Data) -> (u16, Value) {
    let path = target.split('?').next().unwrap_or_default();
    match (method, path) {
        ("GET", "/api/libraries") => (
            200,
            json!([{"id": "library", "name": "Photos", "importPaths": data.import_paths}]),
        ),
        ("GET", "/api/people") => {
            let page: usize = query(target, "page").and_then(|page| page.parse().ok()).unwrap_or(1);
            let size: usize = query(target, "size").and_then(|size| size.parse().ok()).unwrap_or(500);
            let listed: Vec<Value> = data
                .people
                .iter()
                .skip((page - 1) * size)
                .take(size)
                .map(|person| json!({"id": person.id, "name": person.name, "isHidden": person.hidden}))
                .collect();
            let more = page * size < data.people.len();
            (
                200,
                json!({"people": listed, "total": data.people.len(), "hasNextPage": more}),
            )
        }
        ("POST", "/api/search/metadata") => {
            let asked: Value = serde_json::from_slice(body).unwrap_or_default();
            let page = match &asked["page"] {
                Value::String(text) => text.parse().unwrap_or(1),
                other => other.as_u64().unwrap_or(1) as usize,
            };
            let size = asked["size"].as_u64().unwrap_or(250) as usize;
            let items: Vec<Value> = data
                .assets
                .iter()
                .skip((page - 1) * size)
                .take(size)
                .map(|asset| {
                    let mut people: Vec<&str> = asset
                        .faces
                        .iter()
                        .filter_map(|face| face.person_id.as_deref())
                        .collect();
                    people.dedup();
                    json!({
                        "id": asset.id,
                        "originalPath": asset.path,
                        "isOffline": asset.offline,
                        "exifInfo": {
                            "exifImageWidth": asset.width,
                            "exifImageHeight": asset.height,
                            "orientation": asset.orientation.map(|orientation| orientation.to_string()),
                        },
                        "people": people.iter().map(|id| json!({"id": id})).collect::<Vec<Value>>(),
                    })
                })
                .collect();
            let next = (page * size < data.assets.len()).then(|| (page + 1).to_string());
            (
                200,
                json!({"assets": {"items": items, "nextPage": next, "total": data.assets.len()}}),
            )
        }
        ("GET", "/api/faces") => {
            let id = query(target, "id").unwrap_or_default();
            let Some(asset) = data.assets.iter().find(|asset| asset.id == id) else {
                return (400, json!({"message": "Not found or no asset.read access"}));
            };
            let faces: Vec<Value> = asset
                .faces
                .iter()
                .map(|face| {
                    let person = face
                        .person_id
                        .as_ref()
                        .and_then(|id| data.people.iter().find(|one| &one.id == id));
                    json!({
                        "id": face.id,
                        "imageWidth": face.image_width,
                        "imageHeight": face.image_height,
                        "boundingBoxX1": face.x1,
                        "boundingBoxY1": face.y1,
                        "boundingBoxX2": face.x2,
                        "boundingBoxY2": face.y2,
                        "sourceType": "machine-learning",
                        "person": person.map(|one| json!({"id": one.id, "name": one.name, "isHidden": one.hidden})),
                    })
                })
                .collect();
            (200, Value::from(faces))
        }
        _ => (404, json!({"message": "Not Found"})),
    }
}
