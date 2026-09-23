use adw::prelude::*;
use photomanager::library::Library;
use photomanager::window::{VIEWS, Window};
use photomanager_core::changeset::Wanted;
use photomanager_core::filter::{Filter, Gap};
use photomanager_core::paths::Paths;
use photomanager_core::scope::Scope;
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

    surveys_what_is_missing(&window, &opened);
    browses_the_gallery(&window, &opened);

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

/// The dashboard shows the survey, and a number shows its photos when it is clicked.
fn surveys_what_is_missing(window: &Window, opened: &Rc<Library>) {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    // The one asked for when the window opened, on an empty cache, may come first.
    while opened.survey().is_none_or(|survey| survey.photos == 0) && std::time::Instant::now() < deadline {
        context.iteration(false);
    }
    let survey = opened.survey().expect("the survey arrived after the scan");
    assert_eq!(survey.photos as usize, photomanager_core::fixtures::photo_count());

    let dashboard = window.dashboard();
    assert_eq!(dashboard.field(), Gap::Gps);
    assert_eq!(
        dashboard.listed_places(),
        ["China", "Denmark", "Germany", "Ireland", "Greece"],
        "the most photos without GPS first"
    );
    dashboard.set_field(Gap::People);
    assert_eq!(
        dashboard.listed_places(),
        ["China", "Denmark", "Germany", "Greece", "Ireland"],
        "another field, another order"
    );
    dashboard.set_field(Gap::Gps);

    let shown = labels(dashboard.upcast_ref());
    for wanted in ["Coverage", "Tidy Up", "Loose files", "People and people"] {
        assert!(shown.iter().any(|label| label == wanted), "no {wanted}: {shown:?}");
    }
    assert!(
        !shown.iter().any(|label| label == "Sidecars"),
        "a finding with nothing in it is not shown"
    );

    let row = rows(dashboard.upcast_ref())
        .into_iter()
        .find(|row| row.title() == "GPS")
        .expect("a coverage row for GPS");
    WidgetExt::activate(&row);
    assert_eq!(window.visible_view(), "gallery", "the click went to the gallery");
    let (filter, count) = window.shown().expect("the gallery was handed a set of photos");
    assert_eq!(filter.to_string(), "no-gps");
    assert_eq!(count, Some(survey.photos - 1), "the number clicked is the number shown");

    WidgetExt::activate_action(window, "win.show-photos", Some(&"no-gps@Germany".to_variant())).unwrap();
    assert_eq!(window.shown().unwrap().1, Some(2));
    WidgetExt::activate_action(window, "win.show-photos", Some(&"nonsense".to_variant())).unwrap();
    assert_eq!(
        window.shown().unwrap().0.to_string(),
        "no-gps@Germany",
        "a name that means nothing changes nothing"
    );
    window.show_view("dashboard");
}

/// The gallery shows exactly the photos of its filter, and each control changes only its part.
fn browses_the_gallery(window: &Window, opened: &Rc<Library>) {
    let gallery = window.gallery();
    let act = |name: &str, target: &str| {
        WidgetExt::activate_action(window, name, Some(&target.to_variant())).unwrap();
        settle(window);
    };
    let filter = || window.shown().unwrap().0.to_string();
    let listed = || window.gallery().listed();
    let all = photomanager_core::fixtures::photo_count();

    act("win.show-photos", "no-gps@Germany");
    assert_eq!(gallery.page(), "grid");
    assert_eq!(listed().len(), 2, "the grid holds exactly the photos counted");
    assert_eq!(window.shown().unwrap().1, Some(2));

    act("win.show-photos", "all");
    act("win.gallery-place", "Germany/2019-07-13 Sommerfest");
    assert_eq!(filter(), "all@Germany/2019-07-13 Sommerfest");
    assert_eq!(listed().len(), 3);
    act("win.gallery-tag", "people");
    assert_eq!(filter(), "tag:people@Germany/2019-07-13 Sommerfest");
    assert_eq!(
        listed(),
        ["Germany/2019-07-13 Sommerfest/IMAG0001.jpg"],
        "the event and the tag"
    );
    act("win.gallery-tag", "people");
    assert_eq!(
        filter(),
        "all@Germany/2019-07-13 Sommerfest",
        "the chosen tag again widens back"
    );
    assert_eq!(listed().len(), 3);
    act("win.gallery-place", "Germany/2019-07-13 Sommerfest");
    assert_eq!(filter(), "all");

    let gap = descendants(gallery.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::DropDown>().ok())
        .find(|dropdown| {
            dropdown
                .model()
                .is_some_and(|model| model.n_items() as usize == Gap::ALL.len() + 1)
        })
        .expect("the gap dropdown");
    gap.set_selected(1);
    settle(window);
    let picked = window.shown().unwrap().0;
    WidgetExt::activate_action(window, "win.show-photos", Some(&"no-gps".to_variant())).unwrap();
    settle(window);
    assert_eq!(
        picked,
        window.shown().unwrap().0,
        "the dropdown and the dashboard set the same part"
    );
    assert_eq!(listed().len(), all - 1);
    act("win.gallery-gap", "none");
    assert_eq!(filter(), "all");
    assert_eq!(gap.selected(), 0, "the dropdown follows the filter");

    act("win.gallery-sort", "name");
    let mut paths: Vec<String> = photomanager_core::fixtures::photo_paths()
        .into_iter()
        .map(String::from)
        .collect();
    paths.sort();
    assert_eq!(listed(), paths, "by name is path order");
    act("win.gallery-sort", "date");
    assert_ne!(listed(), paths);
    assert_eq!(listed().len(), all);

    act("win.show-photos", "no-gps+loose");
    assert_eq!(gallery.chips(), ["Loose files"]);
    assert_eq!(listed(), ["China/IMG_3140.JPG"]);
    let chip = descendants(gallery.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.tooltip_text().as_deref() == Some("Show without this"))
        .expect("a chip for the loose part");
    chip.emit_clicked();
    settle(window);
    assert_eq!(filter(), "no-gps", "the chip took its part out and nothing else");
    assert!(gallery.chips().is_empty());

    act("win.show-photos", "issue:not a photo");
    assert_eq!(gallery.page(), "empty", "the fixture has no file that is not a photo");

    selects_and_hands_on_a_scope(window, opened);
}

/// Selecting counts; everything or nothing is the filter itself, a few are their paths.
fn selects_and_hands_on_a_scope(window: &Window, opened: &Rc<Library>) {
    let gallery = window.gallery();
    WidgetExt::activate_action(window, "win.show-photos", Some(&"no-gps".to_variant())).unwrap();
    settle(window);
    let listed = gallery.listed();
    assert_eq!(gallery.selected(), 0);
    assert_eq!(
        gallery.scope(),
        Some(Scope::Filter("no-gps".parse().unwrap())),
        "nothing selected is all of it"
    );

    gallery.select(0, true);
    gallery.select(2, true);
    assert_eq!(gallery.selected(), 2);
    let Some(Scope::Photos { paths, .. }) = gallery.scope() else {
        panic!("a few selected photos are a list of them");
    };
    assert_eq!(paths, [listed[0].clone(), listed[2].clone()], "in grid order");

    WidgetExt::activate_action(window, "win.gallery-select-all", None).unwrap();
    assert_eq!(gallery.selected() as usize, listed.len());
    WidgetExt::activate_action(window, "win.use-as-scope", None).unwrap();
    assert_eq!(
        window.scope(),
        Some(Scope::Filter(Filter::missing(Gap::Gps))),
        "all selected is the filter, not its paths"
    );
    assert!(gallery.toast().contains("without GPS"), "{}", gallery.toast());
    assert_eq!(opened.scope_paths(&window.scope().unwrap()), {
        let mut sorted = listed.clone();
        sorted.sort();
        sorted
    });

    WidgetExt::activate_action(window, "win.gallery-select-none", None).unwrap();
    gallery.select(1, true);
    WidgetExt::activate_action(window, "win.use-as-scope", None).unwrap();
    assert_eq!(opened.scope_paths(&window.scope().unwrap()), [listed[1].clone()]);

    WidgetExt::activate_action(window, "win.gallery-place", Some(&"Germany".to_variant())).unwrap();
    assert_eq!(gallery.selected(), 0, "another filter, no selection");
    settle(window);
    assert_eq!(gallery.selected(), 0);
    window.show_view("dashboard");
}

/// Waits until the gallery has the photos it asked for.
fn settle(window: &Window) {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while window.gallery().is_loading() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }
    assert!(!window.gallery().is_loading(), "the photos never arrived");
}

fn labels(widget: &gtk::Widget) -> Vec<String> {
    descendants(widget)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .map(|label| label.label().to_string())
        .collect()
}

fn rows(widget: &gtk::Widget) -> Vec<adw::ActionRow> {
    descendants(widget)
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .collect()
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        found.push(widget.clone());
        found.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    found
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
