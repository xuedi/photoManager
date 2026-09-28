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

    assert_eq!(upkeep(&state(&ui, lib), "Scan"), "never run");
    // A popover does not open in a headless session, so the bar's jobs are run by their actions.
    ui.run(&["act", "win.scan"], lib);
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
    let bar = settled_bar(&ui, lib, "Scan", "fine");
    assert_eq!(upkeep(&bar, "Thumbnails"), "fine");
    assert_eq!(upkeep(&bar, "Places"), "never run");

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

    ui.run(&["act", "win.get-places"], lib);
    let places = settled(&ui, lib, "places");
    assert_eq!(places["places"].as_u64(), Some(151), "the excerpt was imported");
    settled_bar(&ui, lib, "Places", "fine");

    previews_and_applies(&ui, lib);
    edits_one_photo(&ui, lib);
    a_scope_is_what_the_demo_works_on(&ui, lib);
    applies_suggestions(&ui, lib);
    sets_a_place(&ui, lib);
    writes_time_zones(&ui, lib);
    merges_a_tag(&ui, lib);
    gives_people_from_immich(&ui, lib);
    moves_an_event_into_its_city(&ui, lib);
    names_photos_by_their_date(&ui, lib);

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
/// the change set is previewed, and what it promised is what is written.
fn previews_and_applies(ui: &Ui, library: &Path) {
    const EVENT: &str = "Ireland/2008-10-03 Galway";
    let first = library.join(EVENT).join("IMG_0003.JPG");

    ui.run(&["click", "Tools", "--role", "tab"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{EVENT}'")], library);
    counted(ui, library, 2);
    ui.run(&["act", "win.run-edit", "'demo-rating:3'"], library);
    let previewed = settled_preview(ui, library);
    assert_eq!(previewed["photos"].as_u64(), Some(2), "the demo is the event's photos");
    assert_eq!(previewed["change"].as_u64(), Some(2), "and the number the scope said");
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
    let applied = written(ui, library);
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

    // Asked once and never again: this time the apply goes straight through.
    let previewed = previewed_edit(ui, library, "demo-rating:4", "Set a rating of 4");
    assert_eq!(previewed["change"].as_u64(), Some(2));
    ui.run(&["click", "Apply", "--role", "button"], library);
    let again = written(ui, library);
    assert_eq!(again["written"].as_u64(), Some(2));
    assert_eq!(rating_of(&first), Some(4));
    start_over(ui, library);
}

/// One photo edited by hand goes the same way as a change set: reviewed, written, read back, and
/// its image data is the same throughout.
fn edits_one_photo(ui: &Ui, library: &Path) {
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
    let applied = photo_applied(ui, library);
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
    ui.run(&["act", "win.photo-close"], library);
    start_over(ui, library);
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

/// Waits for the photo page's write, and for the read-back after it.
fn photo_applied(ui: &Ui, library: &Path) -> Value {
    for _ in 0..120 {
        let state = state(ui, library);
        let applied = &state["photo"]["applied"];
        if applied.is_object() && state["writing"] == false && state["scanning"] == false {
            return applied.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the write of one photo never finished");
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

    counted(ui, library, 4);
    ui.run(&["act", "win.run-edit", "'demo-rating:3'"], library);
    for _ in 0..60 {
        let state = state(ui, library);
        if state["preview"]["photos"].as_u64() == Some(4) && state["preview"]["change"].as_u64() == Some(4) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the demo never previewed the scope");
}

/// The Suggestions tab, clicked through: two finders' fixes ticked, Apply Selected writes them as
/// two passes in their order - the tags before the places - the photos say what the fixes said,
/// and the applied fixes are gone from the list and the rest stays.
fn applies_suggestions(ui: &Ui, library: &Path) {
    const MERGE: &str = "tags:rename People -> people";
    const BEIJING: &str = "China/2006-09-00 Besuch Ben/P1000001.JPG";
    const WITH_WORDS: &str = "Ireland/2008-10-03 Galway/IMG_0003.JPG";
    let beijing = library.join(BEIJING);
    let with_words = library.join(WITH_WORDS);
    let kira = library.join("Ireland/2008-10-03 Galway/Kira/IMG_0002.JPG");
    assert!(tags_of(&kira).contains(&"People/Kira".to_string()));

    let listed = suggestions(ui, library, |listed| !listed["fixes"].as_array().unwrap().is_empty());
    let fixes = listed["fixes"].as_array().unwrap().clone();
    let places: Vec<&Value> = fixes.iter().filter(|fix| fix["finder"] == "places-from-tags").collect();
    assert_eq!(places.len(), 6, "{listed}");
    assert_eq!(listed["dashboard"], format!("{} Suggestions", fixes.len()), "{listed}");
    assert!(fixes.iter().all(|fix| fix["ticked"] == false), "nothing starts ticked");

    ui.run(&["click", "Suggestions", "--role", "tab"], library);
    ui.run(&["act", "win.fixes-select-all", "'places-from-tags'"], library);
    // A check has no action a click from outside can use; ticking is the same GAction.
    ui.run(&["act", "win.tick-fix", &format!("('{MERGE}', true)")], library);
    let ticked = state(ui, library)["suggestions"]["fixes"].clone();
    assert_eq!(
        ticked
            .as_array()
            .unwrap()
            .iter()
            .filter(|fix| fix["ticked"] == true)
            .count(),
        7,
        "{ticked}"
    );
    assert_eq!(read_place(&beijing), None, "nothing has been written yet");

    ui.run(&["click", "Apply Selected", "--role", "button"], library);
    let applied = suggestions(ui, library, |listed| {
        listed["applied"].is_array() && listed["busy"] == false
    });
    let passes = applied["applied"].as_array().unwrap().clone();
    let finders: Vec<&str> = passes.iter().map(|pass| pass["finder"].as_str().unwrap()).collect();
    assert_eq!(finders, ["tags", "places-from-tags"], "the tags first: {applied}");
    assert_eq!(passes[0]["written"].as_u64(), Some(1), "{applied}");
    assert_eq!(passes[1]["written"].as_u64(), Some(7), "{applied}");
    assert!(
        applied["toast"]
            .as_str()
            .unwrap()
            .starts_with("Written 8 photos in 2 passes"),
        "{applied}"
    );
    let left: Vec<String> = applied["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fix| fix["key"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !left
            .iter()
            .any(|key| key == MERGE || key.starts_with("places-from-tags:")),
        "{left:?}"
    );
    assert!(
        left.iter().any(|key| key == "tags:tidy"),
        "what was not ticked stays: {left:?}"
    );

    let (lat, method, error, city) = read_place(&beijing).expect("a position");
    assert!((lat - 39.9075).abs() < 0.001, "{lat}");
    assert_eq!(method, "photoManager: places tag");
    assert_eq!(error, 5000.0);
    assert_eq!(city.as_deref(), Some("Beijing"));
    let (_, _, _, kept) = read_place(&with_words).expect("a position");
    assert_eq!(kept.as_deref(), Some("Galway"), "its own words were kept");
    assert!(tags_of(&kira).contains(&"people/Kira".to_string()));
    start_over(ui, library);
}

/// The suggestions once found and matching what is waited for.
fn suggestions(ui: &Ui, library: &Path, ready: impl Fn(&Value) -> bool) -> Value {
    let mut listed = Value::Null;
    for _ in 0..120 {
        let now = state(ui, library);
        listed = now["suggestions"].clone();
        if listed["busy"] == false && now["writing"] == false && now["scanning"] == false && ready(&listed) {
            return listed;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the suggestions never came to that: {listed}");
}

/// Waits for an edit's preview by its title.
fn previewed_edit(ui: &Ui, library: &Path, asked: &str, title: &str) -> Value {
    ui.run(&["act", "win.run-edit", &format!("'{asked}'")], library);
    let mut previewed = Value::Null;
    for _ in 0..60 {
        previewed = state(ui, library)["preview"].clone();
        if previewed["title"] == title {
            return previewed;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("{asked} never previewed: {previewed}");
}

/// Set Place, clicked through: one event given a pin as the map gives it, the preview holds its
/// photos without a position of their own, the write puts position, mark, error and words into
/// the files.
fn sets_a_place(ui: &Ui, library: &Path) {
    const COPENHAGEN: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    const PIN: &str = r#"{"pin":[55.68,12.59],"near":{"id":2618425,"name":"Copenhagen","region":"Capital Region","country":"Denmark","code":"DK","lat":55.67594,"lon":12.56553}}"#;
    let pinned = library.join(COPENHAGEN).join("DSCF0002.JPG");

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{COPENHAGEN}'")], library);
    counted(ui, library, 2);
    let previewed = previewed_edit(
        ui,
        library,
        &format!("set-place:{PIN}"),
        "Set the place to a point near Copenhagen",
    );
    assert_eq!(previewed["change"].as_u64(), Some(2), "{previewed}");

    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library);
    assert_eq!(written["written"].as_u64(), Some(2), "{written}");
    let (lat, method, error, city) = read_place(&pinned).expect("a position");
    assert!((lat - 55.68).abs() < 0.0001, "the pin is written at its point: {lat}");
    assert_eq!(method, "photoManager: set by hand");
    assert_eq!(error, 1000.0);
    assert_eq!(city.as_deref(), Some("Copenhagen"));
    start_over(ui, library);
}

/// Set Time Zone where each photo was taken: the offset of where each photo was is written, a
/// photo that states its own keeps it.
fn writes_time_zones(ui: &Ui, library: &Path) {
    const SEASONS: &str = "Germany/2015-00-00 Seasons";
    let summer = library.join(SEASONS).join("IMG_8002.JPG");

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{SEASONS}'")], library);
    counted(ui, library, 3);
    let previewed = previewed_edit(
        ui,
        library,
        "set-time-zone:",
        "Set the time zone of where each photo was taken",
    );
    assert_eq!(previewed["change"].as_u64(), Some(2), "{previewed}");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library);
    assert_eq!(written["written"].as_u64(), Some(2), "{written}");
    assert_eq!(offset_of(&summer).as_deref(), Some("+02:00"));
    start_over(ui, library);
}

/// Rename Tag, clicked through: the preview holds the event's photo that carries the tag, the
/// write puts the merged tag into every field.
fn merges_a_tag(ui: &Ui, library: &Path) {
    const GALWAY: &str = "Ireland/2008-10-03 Galway";
    let kira = library.join(GALWAY).join("Kira/IMG_0002.JPG");
    assert!(tags_of(&kira).contains(&"People/Kira".to_string()));

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{GALWAY}'")], library);
    counted(ui, library, 2);
    let previewed = previewed_edit(ui, library, "rename-tag:People -> people", "Rename People to people");
    assert_eq!(
        previewed["change"].as_u64(),
        Some(1),
        "only the photo that carries it: {previewed}"
    );
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library);
    assert_eq!(written["written"].as_u64(), Some(1), "{written}");
    let tags = tags_of(&kira);
    assert!(tags.contains(&"people/Kira".to_string()), "{tags:?}");
    assert!(tags.contains(&"people".to_string()), "every level is written: {tags:?}");
    assert!(!tags.contains(&"People/Kira".to_string()), "{tags:?}");
    start_over(ui, library);
}

/// People from Immich against a fake Immich served from this test: the address set in the
/// preferences, the people fetched on the dashboard, a person answered, and the preview applied.
fn gives_people_from_immich(ui: &Ui, library: &Path) {
    use photomanager_core::immich::fake::{self, Data, FakeImmich};
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
    ui.run(&["act", "win.get-people"], library);
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

    let listed = suggestions(ui, library, |listed| {
        listed["fixes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|fix| fix["finder"] == "people")
    });
    let people: Vec<&str> = listed["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fix| fix["finder"] == "people")
        .map(|fix| fix["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        people,
        ["Ben", "Ann", "Kira", "Lena Park"],
        "every named person, whatever their tag is called: {listed}"
    );

    ui.run(&["act", "win.fixes-select-all", "'people'"], library);
    ui.run(&["act", "win.apply-fixes"], library);
    let applied = suggestions(ui, library, |listed| listed["applied"].is_array());
    let pass = applied["applied"][0].clone();
    assert_eq!(pass["finder"], "people", "{applied}");
    assert!(pass["written"].as_u64().unwrap() > 0, "{applied}");
    assert!(
        applied["fixes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fix| fix["finder"] != "people"),
        "written, the people are no fix any more: {applied}"
    );
    let untagged = library.join(fake::BEN_UNTAGGED);
    assert!(
        !tags_of(&untagged).iter().any(|tag| tag.starts_with("people")),
        "a person is not a tag"
    );
    let regions = |photo: &Path| {
        let out = Command::new("exiftool")
            .args(["-j", "-struct", "-XMP-mwg-rs:RegionInfo"])
            .arg(photo)
            .output()
            .expect("run exiftool");
        let read: Value = serde_json::from_slice(&out.stdout).expect("exiftool json");
        read[0]["RegionInfo"].clone()
    };
    assert_eq!(regions(&untagged)["RegionList"][0]["Name"], "Ben");
    assert_eq!(
        regions(&turned)["RegionList"][0]["Name"],
        "Ann",
        "written under Immich's name, though her tag says Anna"
    );
    start_over(ui, library);
}

/// Folder Migration, clicked through: one event is asked about, answered with where its photos
/// are, previewed as one folder that costs no traffic, and moved by the apply with every photo as
/// it was.
fn moves_an_event_into_its_city(ui: &Ui, library: &Path) {
    const GARDEN: &str = "Germany/2013-05-18 Garden Party";
    const MOVED: &str = "Germany/Hamburg/2013-05-18 Garden Party";
    let photo = |dir: &str| library.join(dir).join("IMG_6001.JPG");
    let content = photomanager_core::identity::content_id(&std::fs::read(photo(GARDEN)).unwrap()).unwrap();
    let mtime = std::fs::metadata(photo(GARDEN)).unwrap().modified().unwrap();

    ui.run(&["act", "win.show-view", "'tools'"], library);
    ui.run(&["act", "win.tools-scope", &format!("'{GARDEN}'")], library);
    counted(ui, library, 5);
    let previewed = previewed_edit(
        ui,
        library,
        &format!("move-event:{MOVED}"),
        &format!("Move {GARDEN} to {MOVED}"),
    );
    assert_eq!(previewed["change"].as_u64(), Some(1), "{previewed}");
    assert_eq!(previewed["traffic"].as_u64(), Some(0), "a move sends nothing up again");
    ui.run(&["click", "Apply", "--role", "button"], library);
    let written = written(ui, library);
    assert_eq!(written["written"].as_u64(), Some(1), "{written}");
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
    assert!(
        library.join("Germany/Hamburg").is_dir(),
        "the city folder holds another event and stays"
    );
    start_over(ui, library);
}

/// File Names, clicked through: one folder ticked, its photos named by the date they were taken,
/// every one the same file as before, and the fix gone.
fn names_photos_by_their_date(ui: &Ui, library: &Path) {
    const WALK: &str = "Denmark/2017-09-00 Autumn Walk";
    const FIX: &str = "file-names:Denmark/2017-09-00 Autumn Walk";
    let dir = library.join(WALK);
    let before: Vec<(String, std::time::SystemTime)> = ["DSCF0101.JPG", "DSCF0102.JPG"]
        .iter()
        .map(|name| {
            let file = dir.join(name);
            (
                photomanager_core::identity::content_id(&std::fs::read(&file).unwrap()).unwrap(),
                std::fs::metadata(&file).unwrap().modified().unwrap(),
            )
        })
        .collect();

    let listed = suggestions(ui, library, |listed| {
        listed["fixes"].as_array().unwrap().iter().any(|fix| fix["key"] == FIX)
    });
    let fix = listed["fixes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fix| fix["key"] == FIX)
        .unwrap()
        .clone();
    assert_eq!(fix["photos"].as_u64(), Some(2), "{fix}");
    ui.run(&["click", "Suggestions", "--role", "tab"], library);
    ui.run(&["act", "win.tick-fix", &format!("('{FIX}', true)")], library);
    ui.run(&["click", "Apply Selected", "--role", "button"], library);
    let applied = suggestions(ui, library, |listed| {
        listed["applied"].is_array() && listed["busy"] == false
    });
    let passes = applied["applied"].as_array().unwrap();
    assert_eq!(passes.len(), 1, "{applied}");
    assert_eq!(passes[0]["finder"], "file-names", "{applied}");
    assert_eq!(passes[0]["written"].as_u64(), Some(2), "{applied}");
    assert!(
        !applied["fixes"].as_array().unwrap().iter().any(|fix| fix["key"] == FIX),
        "{applied}"
    );

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["2017-08-26_100000.jpg", "2017-08-26_113000.jpg"]);
    for (name, (content, mtime)) in names.iter().zip(&before) {
        let file = dir.join(name);
        assert_eq!(
            photomanager_core::identity::content_id(&std::fs::read(&file).unwrap()).as_deref(),
            Some(content.as_str()),
            "{name}"
        );
        assert_eq!(
            &std::fs::metadata(&file).unwrap().modified().unwrap(),
            mtime,
            "{name} keeps its mtime"
        );
    }
    start_over(ui, library);
}

/// The TagsList of a photo, as ExifTool reads it.
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

fn offset_of(photo: &Path) -> Option<String> {
    let out = Command::new("exiftool")
        .args(["-s3", "-ExifIFD:OffsetTimeOriginal"])
        .arg(photo)
        .output()
        .expect("run exiftool");
    let offset = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!offset.is_empty()).then_some(offset)
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

/// Waits until the scope is counted at this many photos.
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

/// Waits for the preview's change set to be written and read back afterwards. A new preview
/// forgets what the last one came to, so this is always the newest.
fn written(ui: &Ui, library: &Path) -> Value {
    for _ in 0..120 {
        let state = state(ui, library);
        let applied = &state["applied"];
        if applied.is_object() && state["writing"] == false && state["scanning"] == false {
            return applied.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("the change set was never written");
}

/// The way back is a backup, so the next step starts from the stand-in library as it was built,
/// read again.
fn start_over(ui: &Ui, library: &Path) {
    photomanager_core::fixtures::build(library).expect("build the stand-in library again");
    ui.run(&["act", "win.scan"], library);
    scanned(ui, library);
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

/// A job's state on the dashboard's bar, in a word.
fn upkeep(state: &Value, job: &str) -> String {
    state["upkeep"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|each| each["job"] == job))
        .and_then(|each| each["state"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Waits until the bar shows a job in a state; the bar is read off the main thread.
fn settled_bar(ui: &Ui, library: &Path, job: &str, wanted: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..40 {
        last = state(ui, library);
        if upkeep(&last, job) == wanted {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("{job} never became {wanted}: {}", last["upkeep"]);
}
