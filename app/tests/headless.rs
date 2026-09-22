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
    assert_eq!(scanned["events"].as_u64(), Some(6));
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

    ui.run(&["click", "Get Place Data", "--role", "button"], lib);
    let places = settled(&ui, lib, "places");
    assert_eq!(places["places"].as_u64(), Some(149), "the excerpt was imported");

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

/// The scan runs off the main thread, so the counts appear a moment after the click.
fn scanned(ui: &Ui, library: &Path) -> Value {
    settled(ui, library, "photos")
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
