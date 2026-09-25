use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use super::fake::{self, Data, FakeImmich};
use super::*;
use crate::tools::testing::Library;

fn fetched(immich: &FakeImmich, file: &Path, page: usize) -> Result<Fetched> {
    let mut client = Client::new(&immich.url, fake::KEY).unwrap().with_page(page);
    fetch(&mut client, file, None, &|_| {}, &AtomicBool::new(false))
}

fn faces_asked(immich: &FakeImmich) -> BTreeSet<String> {
    immich
        .requests()
        .iter()
        .filter_map(|request| request.strip_prefix("GET /api/faces?id="))
        .map(String::from)
        .collect()
}

#[test]
fn the_fetch_pages_to_the_end_and_keeps_only_what_it_needs() {
    let library = Library::new("immich-fetch");
    let immich = FakeImmich::serve(Data::over(&library.root));
    let file = beside(library.cache.file());

    let found = fetched(&immich, &file, 2).unwrap();
    let data = immich.data.lock().unwrap().clone();
    assert_eq!(found.persons, 6, "every page of the persons");
    assert_eq!(found.named, 4, "neither the hidden nor the unnamed person");
    assert_eq!(found.assets, data.assets.len(), "every page of the assets");
    assert_eq!(found.offline, 1);
    assert_eq!(found.outside, 1);
    let people_pages = immich
        .requests()
        .iter()
        .filter(|request| request.starts_with("GET /api/people"))
        .count();
    assert_eq!(people_pages, 3, "six persons in pages of two");

    let with_named: BTreeSet<String> = data
        .assets
        .iter()
        .filter(|asset| {
            asset
                .faces
                .iter()
                .any(|face| matches!(face.person_id.as_deref(), Some("p-ben" | "p-ann" | "p-kira" | "p-lena")))
        })
        .map(|asset| asset.id.clone())
        .collect();
    assert_eq!(faces_asked(&immich), with_named, "faces only where a named person is");
    assert_eq!(found.with_named, with_named.len());
    for only in [fake::HIDDEN_ONLY, fake::UNNAMED_ONLY] {
        let id = data
            .assets
            .iter()
            .find(|asset| asset.path.ends_with(only))
            .map(|asset| asset.id.clone())
            .unwrap();
        assert!(!faces_asked(&immich).contains(&id), "{only} has no named person");
    }

    let snapshot = Snapshot::open(&file).unwrap().expect("a snapshot");
    assert_eq!(snapshot.people().unwrap().len(), 6);
    let assets = snapshot.assets().unwrap();
    let offline: Vec<&str> = assets
        .iter()
        .filter(|asset| asset.offline)
        .filter_map(|asset| asset.rel_path.as_deref())
        .collect();
    assert_eq!(offline, [fake::OFFLINE], "an offline asset is kept, and marked");
    let turned = assets
        .iter()
        .find(|asset| asset.rel_path.as_deref() == Some(fake::TURNED))
        .unwrap();
    assert_eq!(turned.orientation, Some(6));
    assert_eq!((turned.width, turned.height), (Some(24), Some(16)));
    assert!(
        snapshot
            .faces()
            .unwrap()
            .iter()
            .any(|face| face.asset_id == turned.id && face.person_id.as_deref() == Some("p-ann"))
    );
    assert_eq!(snapshot.about("url").unwrap().as_deref(), Some(immich.url.as_str()));
}

#[test]
fn the_paths_are_the_photos_of_the_library() {
    let library = Library::new("immich-paths");
    let immich = FakeImmich::serve(Data::over(&library.root));
    let file = beside(library.cache.file());
    fetched(&immich, &file, 1000).unwrap();

    let known: BTreeSet<String> = library.cache.paths().unwrap().into_iter().collect();
    let snapshot = Snapshot::open(&file).unwrap().unwrap();
    for asset in snapshot.assets().unwrap() {
        match asset.rel_path.as_deref() {
            None => assert_eq!(asset.original_path, "/upload/library/elsewhere.jpg"),
            Some(fake::GONE) => assert!(!known.contains(fake::GONE)),
            Some(rel_path) => assert!(known.contains(rel_path), "{rel_path} is no photo of the library"),
        }
    }
    assert_eq!(
        relative(
            "/gallery/test/a/b.jpg",
            &["/gallery".to_string(), "/gallery/test/".to_string()]
        ),
        Some("a/b.jpg".to_string()),
        "the longest import path wins"
    );
    assert_eq!(relative("/gallery/testing/b.jpg", &["/gallery/test".to_string()]), None);
}

#[test]
fn a_wrong_key_says_so_and_keeps_nothing() {
    let library = Library::new("immich-key");
    let immich = FakeImmich::serve(Data::over(&library.root));
    let file = beside(library.cache.file());
    let mut client = Client::new(&immich.url, "not-the-key").unwrap();
    let error = fetch(&mut client, &file, None, &|_| {}, &AtomicBool::new(false)).unwrap_err();
    assert_eq!(error, Error::KeyRefused);
    assert_eq!(error.to_string(), "Immich refused the API key");
    assert!(!file.exists());
    assert!(Client::new(&immich.url, "  ").is_err(), "no key, no client");

    let unreachable = Client::new("http://127.0.0.1:9", fake::KEY)
        .unwrap()
        .check()
        .unwrap_err();
    assert!(matches!(unreachable, Error::Unreachable(_)), "{unreachable}");
    assert!(
        Client::new(&immich.url, fake::KEY)
            .unwrap()
            .check()
            .unwrap()
            .contains("6 persons"),
        "the check says what it found"
    );
}

#[test]
fn a_typed_address_is_made_plain() {
    assert_eq!(
        address(" https://gallery.example.org/api/ ").unwrap(),
        "https://gallery.example.org"
    );
    assert_eq!(address("http://127.0.0.1:2283/").unwrap(), "http://127.0.0.1:2283");
    assert!(address("gallery.example.org").is_err());
    assert!(address("https://").is_err());
}

#[test]
fn a_stopped_fetch_keeps_the_old_snapshot_whole() {
    let library = Library::new("immich-cancel");
    let immich = FakeImmich::serve(Data::over(&library.root));
    let file = beside(library.cache.file());
    fetched(&immich, &file, 1000).unwrap();
    let before = std::fs::read(&file).unwrap();

    immich.change(|data| data.rename("p-ben", "Benjamin"));
    let cancel = AtomicBool::new(false);
    let mut client = Client::new(&immich.url, fake::KEY).unwrap();
    let stopped = fetch(
        &mut client,
        &file,
        None,
        &|step| {
            if let Step::Faces(3, _) = step {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert_eq!(stopped.unwrap_err(), Error::Cancelled);
    assert_eq!(std::fs::read(&file).unwrap(), before, "the old snapshot is untouched");
    let dir = file.parent().unwrap();
    let leftovers: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".part"))
        .collect();
    assert!(leftovers.is_empty(), "nothing half: {leftovers:?}");
}

#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl Write for Written {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_key_is_never_logged_nor_kept() {
    let library = Library::new("immich-secret");
    let immich = FakeImmich::serve(Data::over(&library.root));
    let file = beside(library.cache.file());
    let written = Written::default();
    let sink = written.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut client = Client::new(&immich.url, fake::KEY).unwrap();
        tracing::debug!(?client, "a client");
        fetch(&mut client, &file, None, &|_| {}, &AtomicBool::new(false)).unwrap();
        let mut wrong = Client::new(&immich.url, &format!("{}x", fake::KEY)).unwrap();
        let refused = fetch(&mut wrong, &file, None, &|_| {}, &AtomicBool::new(false)).unwrap_err();
        tracing::error!(%refused, "refused");
    });
    let log = String::from_utf8(written.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("people fetched from Immich"),
        "the log was captured: {log}"
    );
    assert!(!log.contains(fake::KEY), "the key is in the log: {log}");

    let kept = std::fs::read(&file).unwrap();
    assert!(
        !kept
            .windows(fake::KEY.len())
            .any(|window| window == fake::KEY.as_bytes()),
        "the key is in the snapshot"
    );
    let client = Client::new(&immich.url, fake::KEY).unwrap();
    assert!(!format!("{client:?}").contains(fake::KEY));
}
