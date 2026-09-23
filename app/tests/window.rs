use adw::prelude::*;
use photomanager::library::Library;
use photomanager::window::{VIEWS, Window};
use photomanager_core::changeset::Wanted;
use photomanager_core::paths::Paths;
use photomanager_core::write::{Change, Field};
use std::rc::Rc;

/// One test function on purpose: GTK objects belong to the thread that created them, and the
/// test harness runs test functions in parallel.
#[test]
fn the_window_carries_every_view_and_switches_between_them() {
    if std::env::var("PHOTOMANAGER_UI_TESTS").unwrap_or_default() != "1" {
        eprintln!("widget tests want a display of their own: run them with `just test-ui`");
        return;
    }
    photomanager::register_resources();
    adw::init().expect("initialise libadwaita");

    let window: Window = gtk::glib::Object::builder().build();

    assert_eq!(window.visible_view(), "dashboard");

    for view in VIEWS {
        WidgetExt::activate_action(&window, "win.show-view", Some(&view.to_variant())).unwrap();
        assert_eq!(window.visible_view(), view);
    }

    WidgetExt::activate_action(&window, "win.show-view", Some(&"nonsense".to_variant())).unwrap();
    assert_eq!(
        window.visible_view(),
        VIEWS[VIEWS.len() - 1],
        "an unknown view changes nothing"
    );

    let named: Vec<String> = buttons(window.upcast_ref::<gtk::Widget>())
        .iter()
        .flat_map(|button| {
            let label: Option<gtk::glib::GString> = button.property("label");
            [
                label.map(|l| l.to_string()),
                button.tooltip_text().map(|t| t.to_string()),
            ]
        })
        .flatten()
        .collect();
    for wanted in ["Import", "Main Menu", "Scan the Library"] {
        assert!(
            named.iter().any(|name| name == wanted),
            "no button called {wanted}: {named:?}"
        );
    }

    scans_into_its_cache();
}

/// The window scans the library it was given, and nothing else.
fn scans_into_its_cache() {
    let base = std::env::temp_dir().join("photomanager-widget-scan");
    let _ = std::fs::remove_dir_all(&base);
    let library = base.join("library");
    photomanager_core::fixtures::build(&library).expect("build the stand-in library");

    let paths = Paths::resolve(
        |key| match key {
            "HOME" => Some(base.to_string_lossy().to_string()),
            "XDG_CACHE_HOME" => Some(base.join("cache").to_string_lossy().to_string()),
            "XDG_DATA_HOME" => Some(base.join("data").to_string_lossy().to_string()),
            "PHOTOMANAGER_LIBRARY" => Some(library.to_string_lossy().to_string()),
            // The checked-in excerpt stands in for the download: the same path, no network.
            "PHOTOMANAGER_GEONAMES" => Some(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../core/src/geo/dumps")
                    .to_string_lossy()
                    .to_string(),
            ),
            _ => None,
        },
        |path| path.is_dir(),
    )
    .expect("resolve the paths");
    let cache_db = paths.cache_db();
    let opened = Library::open(paths).expect("open the cache");

    let window: Window = gtk::glib::Object::builder().build();
    window.set_library(Some(opened.clone()));
    WidgetExt::activate_action(&window, "win.scan", None).unwrap();

    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while opened.last_summary().is_none() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }

    let summary = opened.last_summary().expect("the scan finished");
    assert_eq!(summary.photos, photomanager_core::fixtures::photo_count());
    assert_eq!(
        opened.counts().photos as usize,
        photomanager_core::fixtures::photo_count()
    );
    assert!(opened.counts().issues > 0, "the fixture has photos worth looking at");
    assert!(!opened.is_scanning());
    assert!(cache_db.starts_with(&base), "the cache stayed in the test home");

    assert_eq!(
        opened.counts().thumbnails as usize,
        photomanager_core::fixtures::photo_count(),
        "the scan made a thumbnail per photo"
    );
    assert!(
        paths_of(&opened).iter().all(|path| path.starts_with(&base)),
        "the thumbnails stayed in the test home"
    );

    // One is thrown away; the fill-in pass makes exactly that one again.
    let picture = library.join("Germany/2019-07-13 Sommerfest/img_0657.jpg");
    let content = photomanager_core::identity::content_id(&std::fs::read(&picture).unwrap()).unwrap();
    opened.thumbs().forget(&content).unwrap();
    assert_eq!(
        opened.counts().thumbnails as usize,
        photomanager_core::fixtures::photo_count() - 1
    );

    WidgetExt::activate_action(&window, "win.fill-thumbnails", None).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while opened.is_scanning() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }
    assert_eq!(
        opened.counts().thumbnails as usize,
        photomanager_core::fixtures::photo_count(),
        "the fill-in pass made the missing one"
    );

    assert_eq!(opened.counts().places, 0, "no place data before it is asked for");
    WidgetExt::activate_action(&window, "win.get-places", None).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while opened.is_scanning() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }
    assert_eq!(opened.counts().places, 149, "the excerpt was imported");
    assert_eq!(opened.dump_date().map(|date| date.len()), Some(19));

    previews_what_a_tool_would_change(&window, &opened);
}

/// The preview knows only a change set: it counts it, it lets rows be dropped from it, and it
/// asks the engine for the exact diff of one photo when that photo is asked about. Nothing here
/// writes; the apply is the smoke test's.
fn previews_what_a_tool_would_change(window: &Window, opened: &Rc<Library>) {
    let wanted: Vec<Wanted> = opened
        .photo_paths(3)
        .into_iter()
        .map(|rel_path| Wanted::new(rel_path, Change::of([Field::Rating(Some(4))])))
        .collect();
    assert_eq!(wanted.len(), 3);
    window.tools().preview_change_set("Rate three", wanted);

    let preview = window.preview();
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while preview.counts().is_none() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }

    let counts = preview.counts().expect("the change set was built");
    assert_eq!((counts.photos, counts.change, counts.refused), (3, 3, 0));
    assert_eq!(counts.selected, 3, "what would change starts selected");
    assert!(counts.traffic > 0, "three whole files would go up again");
    assert_eq!(window.tools().showing(), "preview", "the preview page was pushed");

    preview.select(0, false);
    let fewer = preview.counts().unwrap();
    assert_eq!(fewer.selected, 2);
    assert_eq!(fewer.change, 3, "dropping a row does not change what it would do");
    assert!(fewer.traffic < counts.traffic, "the photo that is out costs nothing");

    preview.select_none();
    assert_eq!(preview.counts().unwrap().traffic, 0);
    preview.select_all();
    assert_eq!(preview.counts().unwrap().selected, 3);

    assert_eq!(
        preview.asked(),
        0,
        "nothing is read from a photo until it is asked about"
    );
    preview.details(0);
    preview.details(0);
    assert_eq!(preview.asked(), 1, "one row, one read");
    preview.details(1);
    assert_eq!(preview.asked(), 2);

    assert!(preview.applied().is_none(), "a preview writes nothing");
}

fn paths_of(library: &Library) -> Vec<std::path::PathBuf> {
    vec![library.thumbs().root().to_path_buf()]
}

fn buttons(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if widget.is::<gtk::Button>() || widget.is::<gtk::MenuButton>() {
            found.push(widget.clone());
        }
        found.extend(buttons(&widget));
        child = widget.next_sibling();
    }
    found
}
