//! Drives the real binary in a private headless GNOME session, the way a person would click
//! through it. Needs `pinchy` plus mutter, at-spi2-core and gstreamer, so it is ignored by
//! default: run it with `just smoke`.

#![cfg(feature = "devtools")]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const SESSION: &str = "photomanager-smoke";

/// The checked-in excerpt, so the smoke test never asks GeoNames for anything.
fn dumps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/src/geo/dumps")
}

struct Ui {
    dir: PathBuf,
}

impl Ui {
    fn start(library: &Path) -> Ui {
        let dir = std::env::temp_dir().join("photomanager-smoke");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the work directory");
        let ui = Ui { dir };
        ui.run(&["up", "--size", "1024x768"], library);
        ui.run(&["launch", "--", env!("CARGO_BIN_EXE_photomanager")], library);
        ui
    }

    fn run(&self, args: &[&str], library: &Path) -> Value {
        let output = Command::new("pinchy")
            .args(["--session", SESSION, "--json"])
            .args(args)
            .env("PHOTOMANAGER_LIBRARY", library)
            .env("PHOTOMANAGER_GEONAMES", dumps())
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .output()
            .expect("run pinchy, is it installed?");
        let json: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        assert!(
            output.status.success(),
            "pinchy {args:?} failed: {json} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        json
    }

    /// Actions are activated over D-Bus and run on the next main loop iteration.
    fn wait_for_file(&self, path: &Path) {
        for _ in 0..50 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{} never appeared", path.display());
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = Command::new("pinchy").args(["--session", SESSION, "down"]).output();
    }
}

#[test]
#[ignore]
fn the_app_can_be_clicked_through_headless() {
    let library = std::env::temp_dir().join("photomanager-smoke-library");
    photomanager_core::fixtures::build(&library).expect("build the stand-in library");
    let ui = Ui::start(&library);
    let lib = library.as_path();

    let tree = ui.run(&["tree"], lib);
    let tabs: Vec<&str> = tree
        .as_array()
        .expect("a tree")
        .iter()
        .filter(|node| node["role"] == "tab")
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert_eq!(tabs, ["Dashboard", "Gallery", "Tools", "Suggestions"]);
    assert!(
        tree.as_array()
            .unwrap()
            .iter()
            .any(|node| node["name"] == "Import" && node["role"] == "button"),
        "the import button must be findable by name"
    );

    let actions = ui.run(&["actions"], lib);
    assert_eq!(actions["app_id"], "org.beijingcode.PhotoManager");

    for (view, tab) in [("gallery", "Gallery"), ("tools", "Tools"), ("dashboard", "Dashboard")] {
        ui.run(&["click", tab, "--role", "tab"], lib);
        assert_eq!(state(&ui, lib)["view"], view, "clicking {tab} shows {view}");
    }

    ui.run(&["click", "Scan the Library", "--role", "button"], lib);
    let scanned = scanned(&ui, lib);
    let photos = photomanager_core::fixtures::photo_count() as u64;
    assert_eq!(scanned["photos"].as_u64(), Some(photos), "the scan found every photo");
    assert_eq!(scanned["events"].as_u64(), Some(14));
    assert!(
        scanned["issues"].as_u64().unwrap() > 0,
        "the stand-in library has issues to report"
    );
    assert_eq!(scanned["scanning"], false);
    assert_eq!(
        scanned["thumbnails"].as_u64(),
        Some(photos),
        "every photo got a thumbnail while it was scanned"
    );

    let survey = surveyed(&ui, lib);
    assert_eq!(survey["coverage"]["no-gps"]["missing"].as_u64(), Some(photos - 22));
    assert_eq!(survey["coverage"]["no-date"]["missing"].as_u64(), Some(4));
    assert!(
        survey["tidy"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["filter"] == "loose" && finding["count"] == 1),
        "the loose file is reported: {survey}"
    );

    ui.run(&["act", "win.show-photos", "'no-date'"], lib);
    let shown = state(&ui, lib);
    assert_eq!(shown["view"], "gallery", "a number opens the gallery");
    assert_eq!(shown["gallery"]["filter"], "no-date");
    assert_eq!(
        shown["gallery"]["count"].as_u64(),
        Some(4),
        "with exactly the photos it counted"
    );
    let gallery = pictured(&ui, lib, 4);
    assert_eq!(gallery["listed"].as_u64(), Some(4), "the grid holds them");
    assert_eq!(gallery["page"], "grid");
    steps_through_one_photo_at_a_time(&ui, lib);
    ui.run(&["act", "win.show-view", "'dashboard'"], lib);

    ui.run(&["click", "Get Place Data", "--role", "button"], lib);
    let places = settled(&ui, lib, "places");
    assert_eq!(places["places"].as_u64(), Some(151), "the excerpt was imported");

    previews_applies_and_takes_it_back(&ui, lib);
    edits_one_photo_and_takes_it_back(&ui, lib);
    a_scope_is_what_the_demo_works_on(&ui, lib);
    takes_back_an_older_pass(&ui, lib);
    gives_places_from_their_tag_and_takes_them_back(&ui, lib);
    gives_places_from_the_event_and_takes_them_back(&ui, lib);
    writes_time_zones_and_takes_them_back(&ui, lib);
    merges_a_tag_and_takes_it_back(&ui, lib);
    gives_people_from_immich_and_takes_them_back(&ui, lib);
    moves_an_event_into_its_city_and_takes_it_back(&ui, lib);

    ui.run(&["act", "win.show-view", "'suggestions'"], lib);
    let state = state(&ui, lib);
    assert_eq!(state["view"], "suggestions");
    assert_eq!(state["library"], library.to_str().unwrap());
    assert_eq!(state["version"], env!("CARGO_PKG_VERSION"));

    let png = ui.dir.join("window.png");
    ui.run(&["act", "app.snapshot", &format!("'{}'", png.display())], lib);
    ui.wait_for_file(&png);
    assert_eq!(&std::fs::read(&png).unwrap()[1..4], b"PNG");

    let shot = ui.run(&["shot", ui.dir.join("screen.png").to_str().unwrap()], lib);
    assert_eq!(
        (shot["width"].as_u64(), shot["height"].as_u64()),
        (Some(1024), Some(768))
    );
}

/// The whole point of 1.6, clicked through: a scope is chosen, a tool is opened from the list,
/// the change set is previewed, what it promised is what is written, and the last one can be
/// taken back.
fn previews_applies_and_takes_it_back(ui: &Ui, library: &Path) {
    const EVENT: &str = "Ireland/2008-10-03 Galway";
    let first = library.join(EVENT).join("IMG_0003.JPG");

    ui.run(&["click", "Tools", "--role", "tab"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{EVENT}'")], library);
    let tools = counted(ui, library, 2);
    assert_eq!(
        tools["counts"]["demo-rating"].as_u64(),
        Some(2),
        "the list says what it would change"
    );
    ui.run(&["act", "win.run-tool", "'demo-rating'"], library);
    let previewed = settled_preview(ui, library);
    assert_eq!(previewed["photos"].as_u64(), Some(2), "the demo is the event's photos");
    assert_eq!(previewed["change"].as_u64(), Some(2), "and the number the list showed");
    assert_eq!(previewed["selected"].as_u64(), Some(2));
    assert!(previewed["traffic"].as_u64().unwrap() > 0, "whole files go up again");
    assert_eq!(state(ui, library)["page"], "preview");
    assert_eq!(rating_of(&first), None, "nothing has been written yet");

    // The first write of all is gated on the user saying their photos are backed up, and saying
    // no to that leaves every photo exactly as it was.
    ui.run(&["click", "Apply", "--role", "button"], library);
    ui.run(&["click", "Cancel", "--role", "button"], library);
    assert!(state(ui, library)["applied"].is_null(), "declining wrote something");
    assert_eq!(rating_of(&first), None, "declining changed a photo");

    ui.run(&["click", "Apply", "--role", "button"], library);
    ui.run(&["click", "My Photos Are Backed Up", "--role", "button"], library);
    let applied = written(ui, library, "write", 0);
    assert_eq!(applied["written"].as_u64(), Some(2));
    assert_eq!(applied["failed"].as_u64(), Some(0));
    assert_eq!(applied["cancelled"], false);
    assert!(
        state(ui, library)["toast"]
            .as_str()
            .unwrap()
            .contains("2 photos changed"),
        "the toast says what happened"
    );
    assert_eq!(rating_of(&first), Some(3), "the photo says what the preview promised");

    ui.run(&["act", "win.undo-last"], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    let undone = written(ui, library, "undo", 0);
    assert_eq!(undone["written"].as_u64(), Some(2));
    assert_eq!(rating_of(&first), None, "the photos say again what they said before");

    // Asked once and never again: this time the apply goes straight through.
    ui.run(&["click", "Select All", "--role", "button"], library);
    ui.run(&["click", "Apply", "--role", "button"], library);
    let again = written(ui, library, "write", applied["batch"].as_i64().unwrap());
    assert_eq!(again["written"].as_u64(), Some(2));
    assert_eq!(rating_of(&first), Some(3));
}

/// One photo edited by hand goes the same way as a change set: reviewed, written, read back,
/// taken back, and its image data is the same throughout.
fn edits_one_photo_and_takes_it_back(ui: &Ui, library: &Path) {
    const PHOTO: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG";
    let file = library.join(PHOTO);
    ui.run(&["act", "win.show-photos", "'all'"], library);
    pictured(ui, library, 1);
    ui.run(&["act", "win.show-photo", &format!("'{PHOTO}'")], library);
    let before = photo_details(ui, library, |details| details["taken_at"].is_string());
    let content = before["content_id"].clone();
    assert!(content.is_string());
    assert_eq!(
        read_back(&file),
        ("2018:10:06 14:03:40".to_string(), String::new(), String::new())
    );

    ui.run(&["act", "win.photo-edit"], library);
    let set = |name: &str, value: &str| {
        ui.run(&["set", name, "--role", "text box", "--value", value], library);
    };
    set("Date Taken", "2018-10-06 15:00:00");
    set("Coordinates", "55.6761, 12.5683");
    set("Add a Tag", "food");
    ui.run(&["click", "Add mixed/food", "--role", "button"], library);
    assert_eq!(
        state(ui, library)["photo"]["pending"],
        serde_json::json!(["date", "location", "tags"])
    );

    ui.run(&["click", "Review Change", "--role", "button"], library);
    for _ in 0..40 {
        if state(ui, library)["photo"]["review"].is_array() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert_eq!(read_back(&file).0, "2018:10:06 14:03:40", "the review wrote nothing");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let applied = photo_applied(ui, library, "write");
    assert_eq!(applied["written"].as_u64(), Some(1));

    let (date, gps, tags) = read_back(&file);
    assert_eq!(date, "2018:10:06 15:00:00");
    assert_eq!(gps, "55.6761 12.5683");
    assert_eq!(tags, "mixed, mixed/food");
    let after = photo_details(ui, library, |details| details["taken_at"] == "2018-10-06 15:00:00");
    assert_eq!(
        after["tags"],
        serde_json::json!(["mixed", "mixed/food"]),
        "the panel shows what the file says"
    );
    assert_eq!(after["content_id"], content, "the picture itself is untouched");

    ui.run(&["act", "win.undo-last"], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    photo_applied(ui, library, "undo");
    assert_eq!(
        read_back(&file),
        ("2018:10:06 14:03:40".to_string(), String::new(), String::new())
    );
    let undone = photo_details(ui, library, |details| details["taken_at"] == "2018-10-06 14:03:40");
    assert_eq!(undone["tags"], serde_json::json!([]));
    assert_eq!(undone["content_id"], content);
    ui.run(&["act", "win.photo-close"], library);
}

/// The open photo's details, once they satisfy `ready`.
fn photo_details(ui: &Ui, library: &Path, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..60 {
        let details = state(ui, library)["photo"]["details"].clone();
        if details.is_object() && ready(&details) {
            return details;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("the details never came to say that");
}

/// Waits for the photo page's pass of this kind, and for the read-back after it.
fn photo_applied(ui: &Ui, library: &Path, kind: &str) -> Value {
    for _ in 0..120 {
        let state = state(ui, library);
        let applied = &state["photo"]["applied"];
        if applied["kind"] == kind && state["writing"] == false && state["scanning"] == false {
            return applied.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the {kind} of one photo never finished");
}

/// The date, the position and the tags, as ExifTool reads them.
fn read_back(photo: &Path) -> (String, String, String) {
    let out = Command::new("exiftool")
        .args([
            "-s3",
            "-n",
            "-f",
            "-DateTimeOriginal",
            "-GPSPosition",
            "-TagsList",
            "-sep",
            ", ",
        ])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<String> = text
        .lines()
        .map(|line| match line.trim() {
            "-" => String::new(),
            line => line.to_string(),
        })
        .collect();
    (lines[0].clone(), lines[1].clone(), lines[2].clone())
}

/// What the gallery shows becomes the scope, the tools count it, and the next change set is about
/// exactly those photos. Previewed only: nothing is applied.
fn a_scope_is_what_the_demo_works_on(ui: &Ui, library: &Path) {
    ui.run(&["act", "win.show-photos", "'no-gps@Germany'"], library);
    pictured(ui, library, 4);
    ui.run(&["act", "win.gallery-select-all"], library);
    assert_eq!(state(ui, library)["gallery"]["selected"].as_u64(), Some(4));
    ui.run(&["click", "Use as Scope", "--role", "button"], library);
    let state_now = state(ui, library);
    assert_eq!(
        state_now["scope"]["filter"], "no-gps@Germany",
        "the whole filter, not its paths"
    );
    assert!(
        state_now["gallery"]["toast"].as_str().unwrap().contains("(4)"),
        "the toast says what the scope is: {}",
        state_now["gallery"]["toast"]
    );

    let tools = counted(ui, library, 4);
    assert_eq!(tools["counts"]["demo-rating"].as_u64(), Some(4));
    ui.run(&["act", "win.run-tool", "'demo-rating'"], library);
    for _ in 0..60 {
        let state = state(ui, library);
        if state["preview"]["photos"].as_u64() == Some(4) && state["preview"]["change"].as_u64() == Some(4) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the demo never previewed the scope");
}

/// Two passes that overlap, and the older one taken back from the history: the photos only it
/// touched get back what they said, the ones the newer pass changed again keep the newer value,
/// and the history says so.
fn takes_back_an_older_pass(ui: &Ui, library: &Path) {
    const COUNTRY: &str = "China";
    const EVENT: &str = "China/2006-09-00 Besuch Ben";
    let apply = |scope: &str, photos: u64, tool: &str, title: &str| {
        let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
        ui.run(&["act", "win.tools-scope", &format!("'{scope}'")], library);
        counted(ui, library, photos);
        ui.run(&["act", "win.run-tool", &format!("'{tool}'")], library);
        let mut previewed = Value::Null;
        for _ in 0..60 {
            previewed = state(ui, library)["preview"].clone();
            if previewed["title"] == title {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        assert_eq!(previewed["change"].as_u64(), Some(photos), "{previewed}");
        ui.run(&["click", "Apply", "--role", "button"], library);
        written(ui, library, "write", before)["batch"].as_i64().unwrap()
    };
    ui.run(&["act", "win.show-view", "'tools'"], library);
    let first = apply(COUNTRY, 7, "demo-rating", "Set a rating of 3");
    let second = apply(EVENT, 2, "demo-rating:4", "Set a rating of 4");

    ui.run(&["act", "win.show-history"], library);
    let history = state(ui, library)["history"].clone();
    assert_eq!(history["passes"][0]["batch"].as_i64(), Some(second), "newest first");
    assert_eq!(history["passes"][1]["batch"].as_i64(), Some(first));
    assert_eq!(history["passes"][1]["title"], "Set a rating of 3");
    assert_eq!(history["passes"][1]["changed_since"].as_u64(), Some(2));

    ui.run(&["act", "win.undo-pass", &format!("int64 {first}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    let mut taken = Value::Null;
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"].is_object() && now["writing"] == false && now["scanning"] == false {
            taken = now["history"].clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(taken["taken"]["written"].as_u64(), Some(5), "{taken}");
    assert_eq!(
        taken["taken"]["refused"].as_u64(),
        Some(2),
        "the newer ones were left alone"
    );
    assert!(
        taken["toast"].as_str().unwrap().contains("2 left as they are"),
        "{taken}"
    );

    for (photo, rating) in [
        ("China/2008-01-00 Holiday SOUTHTOUR/IMG_0001.JPG", None),
        ("China/IMG_3140.JPG", None),
        ("China/2012-04-00 Rail Trip/IMG_5003.JPG", None),
        ("China/2006-09-00 Besuch Ben/P1000001.JPG", Some(4)),
        ("China/2006-09-00 Besuch Ben/2006-08-21/P1000002.JPG", Some(4)),
    ] {
        assert_eq!(rating_of(&library.join(photo)), rating, "{photo}");
    }

    let passes = state(ui, library)["history"]["passes"].clone();
    assert_eq!(passes[0]["kind"], "undo");
    assert_eq!(passes[0]["title"], "Take back: Set a rating of 3");
    assert_eq!(passes[2]["batch"].as_i64(), Some(first));
    assert!(passes[2]["undone_by"].is_i64(), "the first pass shows as taken back");
    assert_eq!(passes[2]["can_take_back"], false, "and offers nothing more");
    assert_eq!(passes[1]["can_take_back"], true);
}

/// GPS from the places tag, clicked through: the tags are answered on the question page, the
/// preview holds the answered photos, the write puts the city centre, the mark that it was
/// derived and the words into the files, and taking the pass back takes all of it away again.
fn gives_places_from_their_tag_and_takes_them_back(ui: &Ui, library: &Path) {
    const TOOL: &str = "gps-from-places-tag";
    const BEIJING: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";
    const WITH_WORDS: &str = "Ireland/2008-10-03 Galway/IMG_0003.JPG";
    let beijing = library.join(BEIJING);
    let with_words = library.join(WITH_WORDS);

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", "'all'"], library);
    let tools = counted(ui, library, photomanager_core::fixtures::photo_count() as u64);
    assert_eq!(tools["waiting"][TOOL].as_u64(), Some(8), "{tools}");
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    let asked = questions(ui, library, |questions| {
        questions["questions"].as_array().unwrap().len() == 10
    });
    assert_eq!(state(ui, library)["page"], "questions");
    assert!(
        asked["questions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|question| question["answer"].is_null() || question["apart"] == true)
    );

    ui.run(&["act", "win.answer-exact", &format!("'{TOOL}'")], library);
    ui.run(
        &[
            "act",
            "win.answer",
            &format!("('{TOOL}', 'places/inGreece/Atens', 'leave')"),
        ],
        library,
    );
    let answered = questions(ui, library, |questions| {
        questions["questions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|question| question["key"] == "places/inGreece/Atens" && question["answer"] == "leave")
    });
    let confirmed: Vec<&str> = answered["questions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|question| question["sure"] == true && question["apart"] == false)
        .map(|question| question["answer"].as_str().unwrap_or("unanswered"))
        .collect();
    assert_eq!(confirmed.len(), 6, "{answered}");
    assert!(!confirmed.contains(&"unanswered"), "{answered}");

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-answers"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Set GPS from the places tag" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(8), "{previewed}");
    assert_eq!(read_place(&beijing), None, "nothing has been written yet");

    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(8), "{written}");
    let batch = written["batch"].as_i64().unwrap();

    let (lat, method, error, city) = read_place(&beijing).expect("a position");
    assert!((lat - 39.9075).abs() < 0.001, "{lat}");
    assert_eq!(method, "photoManager: places tag");
    assert_eq!(error, 5000.0);
    assert_eq!(city.as_deref(), Some("Beijing"));
    let (_, _, _, kept) = read_place(&with_words).expect("a position");
    assert_eq!(kept.as_deref(), Some("Galway"), "its own words were kept");

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    let mut taken = Value::Null;
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            taken = now["history"]["taken"].clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(taken["written"].as_u64(), Some(8), "{taken}");
    assert_eq!(
        read_place(&beijing),
        None,
        "the position, the mark and the words are gone"
    );
    assert_eq!(city_of(&beijing), None);
    assert_eq!(city_of(&with_words).as_deref(), Some("Galway"));
}

/// GPS from the event, clicked through: one event answered with the place where the rest of it
/// is, one with a pin as the map gives it, the preview holds their bare photos, the write puts
/// position, mark, error and words into the files, and taking the pass back takes them away.
fn gives_places_from_the_event_and_takes_them_back(ui: &Ui, library: &Path) {
    const TOOL: &str = "gps-from-the-event";
    const HARBOUR: &str = "Germany/2016-06-00 Harbour Walk";
    const COPENHAGEN: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    const PIN: &str = r#"{"pin":[55.68,12.59],"near":{"id":2618425,"name":"Copenhagen","region":"Capital Region","country":"Denmark","code":"DK","lat":55.67594,"lon":12.56553}}"#;
    let bare = library.join(HARBOUR).join("DSC_0104.JPG");
    let pinned = library.join(COPENHAGEN).join("DSCF0002.JPG");

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", "'all'"], library);
    let tools = counted(ui, library, photomanager_core::fixtures::photo_count() as u64);
    assert_eq!(tools["waiting"][TOOL].as_u64(), Some(6), "{tools}");
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    let asked = questions(ui, library, |questions| {
        questions["tool"] == TOOL && questions["questions"].as_array().unwrap().len() == 6
    });
    let harbour = asked["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|question| question["key"] == HARBOUR)
        .unwrap();
    assert_eq!(harbour["sure"], true, "{harbour}");
    assert_eq!(harbour["best"]["located"].as_u64(), Some(3), "{harbour}");

    ui.run(
        &["act", "win.answer", &format!("('{TOOL}', '{HARBOUR}', 'best')")],
        library,
    );
    ui.run(
        &["act", "win.answer", &format!("('{TOOL}', '{COPENHAGEN}', '{PIN}')")],
        library,
    );
    questions(ui, library, |questions| {
        questions["questions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|question| !question["answer"].is_null())
            .count()
            == 2
    });

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-answers"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Set GPS from the event" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(2), "{previewed}");

    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(2), "{written}");
    let batch = written["batch"].as_i64().unwrap();

    let (lat, method, error, city) = read_place(&bare).expect("a position");
    assert!((lat - 53.55073).abs() < 0.0001, "{lat}");
    assert_eq!(method, "photoManager: event");
    assert_eq!(error, 5000.0);
    assert_eq!(city.as_deref(), Some("Hamburg"));
    let (lat, method, error, city) = read_place(&pinned).expect("a position");
    assert!((lat - 55.68).abs() < 0.0001, "the pin is written at its point: {lat}");
    assert_eq!(method, "photoManager: event");
    assert_eq!(error, 1000.0);
    assert_eq!(city.as_deref(), Some("Copenhagen"));

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    let mut taken = Value::Null;
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            taken = now["history"]["taken"].clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(taken["written"].as_u64(), Some(2), "{taken}");
    assert_eq!(read_place(&bare), None, "the position, the mark and the words are gone");
    assert_eq!(read_place(&pinned), None);
    assert_eq!(city_of(&bare), None);
}

/// The zones tool asks nothing where no country has several zones: its page is empty, Preview
/// still opens, and what is written is the offset of where each photo was, taken back after.
fn writes_time_zones_and_takes_them_back(ui: &Ui, library: &Path) {
    const TOOL: &str = "time-zones";
    const SEASONS: &str = "Germany/2015-00-00 Seasons";
    let summer = library.join(SEASONS).join("IMG_8002.JPG");

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{SEASONS}'")], library);
    let tools = counted(ui, library, 3);
    assert_eq!(tools["counts"][TOOL].as_u64(), Some(2), "{tools}");
    assert_eq!(tools["waiting"][TOOL].as_u64(), Some(0), "{tools}");
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    questions(ui, library, |questions| {
        questions["tool"] == TOOL && questions["questions"].as_array().unwrap().is_empty()
    });

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-answers"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Write time zones and XMP dates" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(2), "{previewed}");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(2), "{written}");
    let batch = written["batch"].as_i64().unwrap();
    assert_eq!(offset_of(&summer).as_deref(), Some("+02:00"));

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(offset_of(&summer), None, "the offset is gone again");
}

/// The tag vocabulary, clicked through: its tree opens, one merge becomes a rule, the preview holds
/// the event's photos, the write puts the merged tag into every field, and taking the pass back
/// puts the old spelling back.
fn merges_a_tag_and_takes_it_back(ui: &Ui, library: &Path) {
    const TOOL: &str = "tag-vocabulary";
    const GALWAY: &str = "Ireland/2008-10-03 Galway";
    let kira = library.join(GALWAY).join("Kira/IMG_0002.JPG");
    assert!(tags_of(&kira).contains(&"People/Kira".to_string()));

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{GALWAY}'")], library);
    counted(ui, library, 2);
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    let looked = vocabulary(ui, library, |tags| tags["tree"].is_object());
    assert_eq!(state(ui, library)["page"], "vocabulary");
    assert_eq!(looked["tree"]["People"].as_u64(), Some(1), "{looked}");

    ui.run(&["act", "win.tag-rule", "'rename People -> people'"], library);
    let merged = vocabulary(ui, library, |tags| {
        tags["rules"].as_array().is_some_and(|rules| rules.len() == 1)
    });
    assert!(merged["tree"]["People"].is_null(), "{merged}");

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-tags"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Tidy the tags with 1 rule" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(2), "{previewed}");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(2), "{written}");
    let batch = written["batch"].as_i64().unwrap();
    let tags = tags_of(&kira);
    assert!(tags.contains(&"people/Kira".to_string()), "{tags:?}");
    assert!(tags.contains(&"people".to_string()), "every level is written: {tags:?}");
    assert!(!tags.contains(&"People/Kira".to_string()), "{tags:?}");

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(
        tags_of(&kira),
        ["People/Kira", "mixed/disgusting", "places/inIreland/Galway"]
    );
}

/// People from Immich against a fake Immich served from this test: the address set in the
/// preferences, the people fetched on the dashboard, a person answered, the preview applied, and
/// taken back.
fn gives_people_from_immich_and_takes_them_back(ui: &Ui, library: &Path) {
    use photomanager_core::immich::fake::{self, Data, FakeImmich};
    const TOOL: &str = "people-from-immich";
    let immich = FakeImmich::serve(Data::over(library));
    let turned = library.join(fake::TURNED);

    ui.run(&["act", "app.preferences"], library);
    ui.run(&["wait-for", "Server Address"], library);
    ui.run(
        &["set", "Server Address", "--role", "text box", "--value", &immich.url],
        library,
    );
    ui.run(&["key", "Return"], library);
    ui.run(&["key", "Escape"], library);
    ui.run(&["act", "win.immich-key", &format!("'{}'", fake::KEY)], library);

    ui.run(&["act", "win.show-view", "'dashboard'"], library);
    ui.run(&["click", "Get People from Immich", "--role", "button"], library);
    let mut fetched = Value::Null;
    for _ in 0..60 {
        fetched = state(ui, library);
        if fetched["scanning"] == false && fetched["people"]["named"].as_u64() == Some(4) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(
        fetched["people"]["named"].as_u64(),
        Some(4),
        "{}",
        fetched["dashboard_toast"]
    );
    assert!(
        fetched["dashboard_toast"]
            .as_str()
            .unwrap_or_default()
            .starts_with("4 named persons"),
        "{}",
        fetched["dashboard_toast"]
    );

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", "'all'"], library);
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    let asked = questions(ui, library, |questions| {
        questions["tool"] == TOOL && questions["questions"].as_array().unwrap().len() == 4
    });
    assert_eq!(asked["questions"][0]["title"], "Ben", "{asked}");
    assert_eq!(asked["questions"][0]["kind"], "person");
    assert!(
        asked["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["title"] == "From Immich"),
        "{asked}"
    );
    ui.run(&["act", "win.answer-exact", &format!("'{TOOL}'")], library);
    ui.run(&["act", "win.answer", &format!("('{TOOL}', 'p-ann', 'best')")], library);
    let answered = questions(ui, library, |questions| {
        questions["questions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|question| !question["answer"].is_null())
            .count()
            == 3
    });
    assert!(
        answered["settings"].as_str().unwrap().contains("people/family/Anna"),
        "{answered}"
    );

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-answers"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Write people from Immich" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(5), "{previewed}");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(5), "{written}");
    let batch = written["batch"].as_i64().unwrap();
    let out = Command::new("exiftool")
        .args(["-j", "-struct", "-XMP-mwg-rs:RegionInfo"])
        .arg(&turned)
        .output()
        .expect("run exiftool");
    let read: Value = serde_json::from_slice(&out.stdout).expect("exiftool json");
    assert_eq!(read[0]["RegionInfo"]["RegionList"][0]["Name"], "Anna", "{read}");
    assert_eq!(read[0]["RegionInfo"]["AppliedToDimensions"]["W"], 24, "{read}");
    assert!(tags_of(&turned).contains(&"people/family/Anna".to_string()));

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(
        !tags_of(&turned).contains(&"people/family/Anna".to_string()),
        "the tag is gone again"
    );
    let out = Command::new("exiftool")
        .args(["-j", "-struct", "-XMP-mwg-rs:RegionInfo"])
        .arg(&turned)
        .output()
        .expect("run exiftool");
    let read: Value = serde_json::from_slice(&out.stdout).expect("exiftool json");
    assert!(read[0]["RegionInfo"].is_null(), "and the region: {read}");
}

/// The TagsList of a photo, as ExifTool reads it.
/// Folder Migration, clicked through: one event is asked about, answered with where its photos
/// are, previewed as one folder that costs no traffic, moved by the apply with every photo as it
/// was, and moved back by taking the pass back.
fn moves_an_event_into_its_city_and_takes_it_back(ui: &Ui, library: &Path) {
    const TOOL: &str = "folder-migration";
    const GARDEN: &str = "Germany/2013-05-18 Garden Party";
    const MOVED: &str = "Germany/Hamburg/2013-05-18 Garden Party";
    let photo = |dir: &str| library.join(dir).join("IMG_6001.JPG");
    let content = photomanager_core::identity::content_id(&std::fs::read(photo(GARDEN)).unwrap()).unwrap();
    let mtime = std::fs::metadata(photo(GARDEN)).unwrap().modified().unwrap();

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{GARDEN}'")], library);
    let tools = counted(ui, library, 5);
    assert_eq!(tools["waiting"][TOOL].as_u64(), Some(1), "{tools}");
    ui.run(&["act", "win.run-tool", &format!("'{TOOL}'")], library);
    let asked = questions(ui, library, |questions| {
        questions["tool"] == TOOL && questions["questions"].as_array().unwrap().len() == 1
    });
    assert_eq!(asked["questions"][0]["kind"], "folder", "{asked}");
    ui.run(
        &["act", "win.answer", &format!("('{TOOL}', '{GARDEN}', 'best')")],
        library,
    );
    questions(ui, library, |questions| {
        questions["questions"][0]["answer"] == format!("Into {MOVED}")
    });

    let before = state(ui, library)["applied"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.preview-answers"], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == "Folder Migration" && previewed["change"].as_u64() == Some(1) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert_eq!(previewed["change"].as_u64(), Some(1), "{previewed}");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library, "write", before);
    assert_eq!(written["written"].as_u64(), Some(1), "{written}");
    let batch = written["batch"].as_i64().unwrap();
    assert!(!library.join(GARDEN).exists(), "the event left its old folder");
    let moved = photo(MOVED);
    assert_eq!(
        photomanager_core::identity::content_id(&std::fs::read(&moved).unwrap()).as_deref(),
        Some(content.as_str())
    );
    assert_eq!(
        std::fs::metadata(&moved).unwrap().modified().unwrap(),
        mtime,
        "a move keeps the mtime"
    );

    let taken_before = state(ui, library)["history"]["taken"]["batch"].as_i64().unwrap_or(0);
    ui.run(&["act", "win.undo-pass", &format!("int64 {batch}")], library);
    ui.run(&["click", "Take It Back", "--role", "button"], library);
    for _ in 0..120 {
        let now = state(ui, library);
        if now["history"]["taken"]["batch"].as_i64().unwrap_or(0) > taken_before
            && now["writing"] == false
            && now["scanning"] == false
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(photo(GARDEN).is_file(), "back in its old folder");
    assert!(!library.join(MOVED).exists());
    assert!(
        library.join("Germany/Hamburg").is_dir(),
        "the city folder holds another event and stays"
    );
}

fn tags_of(photo: &Path) -> Vec<String> {
    let out = Command::new("exiftool")
        .args(["-j", "-XMP-digiKam:TagsList"])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let read: Value = serde_json::from_slice(&out.stdout).expect("exiftool json");
    let mut tags: Vec<String> = match &read[0]["TagsList"] {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(String::from))
            .collect(),
        Value::String(one) => vec![one.clone()],
        _ => Vec::new(),
    };
    tags.sort();
    tags
}

/// The tag page once it has looked and matches what is waited for.
fn vocabulary(ui: &Ui, library: &Path, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..60 {
        let tags = state(ui, library)["vocabulary"].clone();
        if tags["busy"] == false && ready(&tags) {
            return tags;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the tag page never settled");
}

fn offset_of(photo: &Path) -> Option<String> {
    let out = Command::new("exiftool")
        .args(["-s3", "-ExifIFD:OffsetTimeOriginal"])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let offset = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!offset.is_empty()).then_some(offset)
}

/// The questions on the page once they are asked and match what is waited for.
fn questions(ui: &Ui, library: &Path, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..60 {
        let questions = state(ui, library)["questions"].clone();
        if questions["busy"] == false && questions["questions"].is_array() && ready(&questions) {
            return questions;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the questions never arrived");
}

/// Latitude, how it was worked out, how far off it may be, and the city, as ExifTool reads them.
/// `None` when there is no position.
fn read_place(photo: &Path) -> Option<(f64, String, f64, Option<String>)> {
    let out = Command::new("exiftool")
        .args([
            "-j",
            "-n",
            "-GPSLatitude",
            "-GPSProcessingMethod",
            "-GPSHPositioningError",
            "-XMP-photoshop:City",
        ])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let read: Value = serde_json::from_slice(&out.stdout).expect("exiftool json");
    let fields = &read[0];
    let lat = fields["GPSLatitude"].as_f64()?;
    Some((
        lat,
        fields["GPSProcessingMethod"].as_str().unwrap_or_default().to_string(),
        fields["GPSHPositioningError"].as_f64().unwrap_or_default(),
        fields["City"].as_str().map(String::from),
    ))
}

fn city_of(photo: &Path) -> Option<String> {
    let out = Command::new("exiftool")
        .args(["-s3", "-XMP-photoshop:City"])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let city = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!city.is_empty()).then_some(city)
}

/// A photo opens from the gallery and the arrow keys walk the grid's list, in its order.
fn steps_through_one_photo_at_a_time(ui: &Ui, library: &Path) {
    ui.run(
        &["act", "win.show-photos", "'all@Germany/2019-07-13 Sommerfest'"],
        library,
    );
    ui.run(&["act", "win.gallery-sort", "'name'"], library);
    pictured(ui, library, 3);
    ui.run(
        &["act", "win.show-photo", "'Germany/2019-07-13 Sommerfest/IMAG0001.jpg'"],
        library,
    );
    let photo = arrived(ui, library);
    assert_eq!(photo["position"].as_u64(), Some(0));
    assert_eq!(photo["count"].as_u64(), Some(3));
    assert_eq!(photo["picture"], true);

    ui.run(&["key", "Right"], library);
    let photo = state(ui, library)["photo"].clone();
    assert_eq!(photo["path"], "Germany/2019-07-13 Sommerfest/img_0657.jpg");
    assert_eq!(photo["position"].as_u64(), Some(1));
    let photo = arrived(ui, library);
    assert_eq!(
        (photo["full"]["width"].as_u64(), photo["full"]["height"].as_u64()),
        (Some(16), Some(24)),
        "turned by its orientation"
    );

    ui.run(&["key", "End"], library);
    ui.run(&["key", "Right"], library);
    assert_eq!(
        state(ui, library)["photo"]["position"].as_u64(),
        Some(2),
        "the end stays the end"
    );
    ui.run(&["key", "Home"], library);
    assert_eq!(state(ui, library)["photo"]["position"].as_u64(), Some(0));

    ui.run(&["key", "Escape"], library);
    let back = state(ui, library);
    assert!(back["photo"].is_null(), "Escape went back to the grid");
    assert_eq!(back["gallery"]["filter"], "all@Germany/2019-07-13 Sommerfest");
    ui.run(&["act", "win.gallery-sort", "'date'"], library);
}

/// Waits until the photo on screen has its full size.
fn arrived(ui: &Ui, library: &Path) -> Value {
    for _ in 0..60 {
        let photo = state(ui, library)["photo"].clone();
        if photo["full"].is_object() {
            return photo;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("the full size never arrived");
}

/// Waits until the gallery has its photos and at least this many cells show a picture.
fn pictured(ui: &Ui, library: &Path, at_least: u64) -> Value {
    for _ in 0..60 {
        let state = state(ui, library);
        let gallery = &state["gallery"];
        if gallery["loading"] == false && gallery["pictures"].as_u64().unwrap_or(0) >= at_least {
            return gallery.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("the gallery never showed its pictures");
}

fn rating_of(photo: &Path) -> Option<i64> {
    let out = Command::new("exiftool")
        .args(["-s3", "-n", "-XMP-xmp:Rating"])
        .arg(photo)
        .output()
        .expect("run exiftool");
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The change set is built off the main thread, like everything else that takes a moment.
fn settled_preview(ui: &Ui, library: &Path) -> Value {
    for _ in 0..60 {
        let state = state(ui, library);
        if state["preview"]["photos"].as_u64().unwrap_or(0) > 0 {
            return state["preview"].clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the preview never arrived");
}

/// Waits until the tools are counted for a scope of this many photos.
fn counted(ui: &Ui, library: &Path, photos: u64) -> Value {
    let mut tools = Value::Null;
    for _ in 0..60 {
        let state = state(ui, library);
        tools = state["tools"].clone();
        if tools["photos"].as_u64() == Some(photos) {
            return tools;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the tools were never counted for {photos} photos: {tools}");
}

/// Waits for a pass of the given kind, newer than `after`, to be over and read back afterwards.
fn written(ui: &Ui, library: &Path, kind: &str, after: i64) -> Value {
    for _ in 0..120 {
        let state = state(ui, library);
        let applied = &state["applied"];
        if applied["kind"] == kind
            && applied["batch"].as_i64().unwrap_or(0) > after
            && state["writing"] == false
            && state["scanning"] == false
        {
            return applied.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the {kind} pass never finished");
}

/// The scan runs off the main thread, so the counts appear a moment after the click.
fn scanned(ui: &Ui, library: &Path) -> Value {
    settled(ui, library, "photos")
}

/// The survey is taken off the main thread after the scan, a moment after its counts.
fn surveyed(ui: &Ui, library: &Path) -> Value {
    for _ in 0..60 {
        let state = state(ui, library);
        if state["survey"]["photos"].as_u64().unwrap_or(0) > 0 {
            return state["survey"].clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the survey never arrived");
}

/// Waits until nothing is running any more and the count asked about is there.
fn settled(ui: &Ui, library: &Path, count: &str) -> Value {
    for _ in 0..60 {
        let state = state(ui, library);
        if state["scanning"] == false && state[count].as_u64().unwrap_or(0) > 0 {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("{count} never arrived");
}

fn state(ui: &Ui, library: &Path) -> Value {
    let file = ui.dir.join("state.json");
    let _ = std::fs::remove_file(&file);
    ui.run(&["act", "app.dump-state", &format!("'{}'", file.display())], library);
    ui.wait_for_file(&file);
    serde_json::from_slice(&std::fs::read(&file).unwrap()).expect("state as json")
}
