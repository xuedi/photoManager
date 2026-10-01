use adw::prelude::*;
use photomanager::library::Library;
use photomanager::window::{VIEWS, Window};
use photomanager_core::changeset::Wanted;
use photomanager_core::filter::{Filter, Gap};
use photomanager_core::layout::Component;
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
    assert_eq!(
        window.scope(),
        Scope::Filter(Filter::all()),
        "the tools work on the whole library until told otherwise"
    );
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
    looks_at_one_photo(&window, &library);

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
    assert_eq!(opened.counts().places, 151, "the excerpt was imported");
    assert_eq!(opened.dump_date().map(|date| date.len()), Some(19));

    reads_the_panel(&window);
    edits_one_photo(&window);
    suggests_fixes_to_tick(&window, &opened);
    leads_a_finding_to_its_fix(&window);

    previews_what_a_tool_would_change(&window, &opened);
    lists_the_tools_for_a_scope(&window, &opened, &library);
    fills_in_the_forms_of_the_edits(&window, &opened);
    gives_positions_from_a_neighbour(&window, &opened, &library);
    chooses_a_folder_layout(&opened);
}

/// The Suggestions view lists every fix the app is sure about, grouped by finder in the order
/// they are applied, each with a check, and the dashboard says how many. A tick is one click and
/// Select All ticks a group; the checks live only in the window, so another window on the same
/// library starts with none. Nothing is written here: the apply is the smoke test's.
fn suggests_fixes_to_tick(window: &Window, opened: &Rc<Library>) {
    const BEIJING: &str = "places-from-tags:places/inChina/Beijing";
    const FOLDER: &str = "folders:China/2006-09-00 Besuch Ben";
    let page = window.suggestions();
    let act = |name: &str, target: Option<gtk::glib::Variant>| {
        WidgetExt::activate_action(window, name, target.as_ref()).unwrap()
    };

    window.show_view("suggestions");
    until(|| !page.is_busy() && !page.found().is_empty(), "the fixes were found");
    let found = page.found();
    let keys: Vec<&str> = found.iter().map(|fix| fix.key.as_str()).collect();
    for wanted in [
        "tags:rename People -> people",
        "tags:rename inChina -> places/inChina",
        "tags:tidy",
        BEIJING,
        "places-from-events:Germany/2016-06-00 Harbour Walk",
        FOLDER,
        "file-names:Denmark/2018-10-00 Wedding Trip to Copenhagen",
    ] {
        assert!(keys.contains(&wanted), "{wanted} is not in {keys:?}");
    }
    let beijing = found.iter().find(|fix| fix.key == BEIJING).unwrap();
    assert_eq!(
        (beijing.photos, beijing.detail.as_str()),
        (2, "Beijing, Beijing, China")
    );
    let shown = labels(page.upcast_ref());
    for group in [
        "Tag Tree",
        "Places from Tags",
        "Places from Events",
        "Folders",
        "File Names",
    ] {
        assert!(shown.iter().any(|label| label == group), "no group {group}");
    }
    assert!(shown.iter().position(|label| label == "Tag Tree") < shown.iter().position(|label| label == "Folders"));
    assert!(shown.iter().position(|label| label == "Folders") < shown.iter().position(|label| label == "File Names"));
    assert_eq!(
        window.dashboard().suggestions_line(),
        Some(format!("{} Suggestions", found.len()))
    );
    assert!(page.ticked().is_empty(), "nothing starts ticked");
    assert!(shown.iter().any(|label| label == "Tick the fixes to apply"));

    act("win.tick-fix", Some((BEIJING, true).to_variant()));
    assert_eq!(page.ticked(), [BEIJING]);
    assert!(labels(page.upcast_ref()).iter().any(|label| label == "1 fix selected"));
    assert!(!labels(page.upcast_ref()).iter().any(|label| label == "Unselect All"));
    act("win.fixes-select-all", Some("folders".to_variant()));
    assert_eq!(page.ticked().len(), 2);
    assert!(
        labels(page.upcast_ref()).iter().any(|label| label == "Unselect All"),
        "a group with every fix ticked offers to take the ticks away"
    );
    act("win.fixes-select-none", Some("all".to_variant()));
    assert!(page.ticked().is_empty());
    act("win.tick-fix", Some((FOLDER, true).to_variant()));

    let again = Library::open(opened.paths().clone()).expect("open the library again");
    let other: Window = gtk::glib::Object::builder().build();
    other.set_library(Some(again));
    let fresh = other.suggestions();
    until(
        || !fresh.is_busy() && !fresh.found().is_empty(),
        "found in the other window",
    );
    assert!(fresh.ticked().is_empty(), "a tick is not kept anywhere");
    other.close();

    act("win.tick-fix", Some((FOLDER, false).to_variant()));
    assert!(page.applied().is_none(), "nothing was written");
    window.show_view("dashboard");
}

/// The dashboard's Fix opens where a finding is fixed: the tool on exactly those photos, or the
/// finder's group on the Suggestions tab. Nothing is written.
fn leads_a_finding_to_its_fix(window: &Window) {
    const EVENT: &str = "no-gps@Germany/2016-06-00 Harbour Walk";
    let act = |target: &str| WidgetExt::activate_action(window, "win.fix", Some(&target.to_variant())).unwrap();
    let scoped = || match window.scope() {
        Scope::Filter(filter) => filter.to_string(),
        Scope::Photos { title, .. } => title,
    };
    let form = || window.visible_dialog().map(|dialog| dialog.title().to_string());

    window.show_view("dashboard");
    let shown = labels(window.dashboard().upcast_ref());
    assert!(
        shown.iter().any(|label| label == "Not named by their date"),
        "{shown:?}"
    );
    assert!(shown.iter().any(|label| label == "Fix"), "the findings offer their fix");

    act("no-gps");
    assert_eq!(window.visible_view(), "tools");
    assert_eq!(scoped(), "no-gps");
    assert_eq!(form().as_deref(), Some("Set Place"));
    window.visible_dialog().unwrap().force_close();

    act(EVENT);
    assert_eq!(scoped(), EVENT, "a gap of one event stays that event's");
    assert_eq!(form().as_deref(), Some("Set Place"));
    window.visible_dialog().unwrap().force_close();

    act("date-off-folder");
    assert_eq!(form().as_deref(), Some("Shift Dates"));
    window.visible_dialog().unwrap().force_close();

    let page = window.suggestions();
    act("off-name");
    assert_eq!(window.visible_view(), "suggestions");
    until(
        || page.revealed().as_deref() == Some("file-names"),
        "the File Names group was shown",
    );
    act("sub-folder");
    until(
        || page.revealed().as_deref() == Some("folders"),
        "the Folders group was shown",
    );

    WidgetExt::activate_action(window, "win.tools-scope", Some(&"all".to_variant())).unwrap();
    window.show_view("dashboard");
}

/// The layout is picked from the presets, put together level by level, refused when it could be
/// read two ways, and saved: the photos are placed in it again and nothing on disk moves.
fn chooses_a_folder_layout(opened: &Rc<Library>) {
    use photomanager::layout_editor::LayoutEditor;
    use photomanager_core::filter::Kind;

    let editor = LayoutEditor::new(opened.clone());
    assert_eq!(editor.titles(), ["Country", "City"]);
    assert_eq!(editor.preset().as_deref(), Some("Country / City / Event"));
    assert!(!editor.can_save(), "nothing changed yet");
    assert_eq!(editor.example(), "Germany/Hamburg/2014-08-00 Wedding");

    editor.choose_preset("Year / Country / Event");
    assert_eq!(editor.titles(), ["Year", "Country"]);
    assert!(editor.can_save());
    assert_eq!(editor.example(), "2014/Germany/2014-08-00 Wedding");

    editor.move_level(1, 0);
    assert_eq!(editor.titles(), ["Country", "Year"]);
    assert_eq!(editor.preset().as_deref(), Some("Country / Year / Event"));
    editor.add(Component::Region);
    assert_eq!(editor.titles(), ["Country", "Year", "Region"]);
    assert_eq!(editor.preset(), None, "its own now");
    assert_eq!(editor.example(), "Germany/2014/<Region>/2014-08-00 Wedding");
    editor.remove(1);
    editor.toggle_optional(0);
    editor.toggle_optional(1);
    assert!(
        editor.problem().is_some(),
        "an optional country next to an optional region"
    );
    assert!(!editor.can_save());
    assert_eq!(editor.example(), "");

    editor.choose_preset("Year / Country / Event");
    let before = walk(opened.paths().library());
    editor.save_now().unwrap();
    assert_eq!(opened.layout().to_string(), "year/country");
    assert!(!editor.can_save(), "saved");
    assert_eq!(
        opened.count(&Filter::of(Kind::OffLayout)),
        Some(photomanager_core::fixtures::photo_count() as i64),
        "every photo is off a layout by year"
    );
    assert_eq!(walk(opened.paths().library()), before, "nothing moved");

    let again = LayoutEditor::new(opened.clone());
    assert_eq!(
        again.titles(),
        ["Year", "Country"],
        "the kept layout is where it starts"
    );
    again.choose_preset("Country / City / Event");
    again.save_now().unwrap();
    assert_eq!(opened.count(&Filter::of(Kind::OffLayout)), Some(0));
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut all = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            all.extend(walk(&path));
        }
        all.push(path);
    }
    all.sort();
    all
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

/// The tools work on the scope the dialog or the gallery gives, the scope row says how many
/// photos it names, and an edit given its value previews exactly those photos. The count follows
/// the library after a scan.
fn lists_the_tools_for_a_scope(window: &Window, opened: &Rc<Library>, library: &std::path::Path) {
    const EVENT: &str = "Germany/2019-07-13 Sommerfest";
    let tools = window.tools();
    let act = |name: &str, target: &str| WidgetExt::activate_action(window, name, Some(&target.to_variant())).unwrap();
    let counted = |photos: usize| until(|| tools.scope_photos() == Some(photos), "the scope was counted");
    window.show_view("tools");
    let listed: Vec<String> = rows(tools.upcast_ref())
        .iter()
        .map(|row| row.title().to_string())
        .collect();
    for edit in [
        "Set Place",
        "Shift Dates",
        "Set Date",
        "Set Time Zone",
        "Add Tag",
        "Remove Tag",
        "Rename Tag",
        "Tidy Tags",
        "Move Event",
        "Position from a Neighbour",
    ] {
        assert!(listed.iter().any(|title| title == edit), "{edit} is not in {listed:?}");
    }

    act("win.tools-scope", "all");
    counted(photomanager_core::fixtures::photo_count());
    act("win.tools-scope", EVENT);
    assert_eq!(window.scope(), Scope::Filter(Filter::all().within(EVENT)));
    counted(3);

    act("win.tools-scope", "picked");
    assert_eq!(
        window.scope(),
        tools.picked().unwrap(),
        "what the gallery handed over is a scope again"
    );
    act("win.tools-scope", EVENT);

    let preview = window.preview();
    let previewed = |value: &str| {
        act("win.run-edit", &format!("demo-rating:{value}"));
        until(
            || !opened.is_busy() && preview.title().as_deref() == Some(&format!("Set a rating of {value}")),
            "the demo was built",
        );
        preview.counts().expect("the demo was previewed")
    };
    let counts = previewed("3");
    assert_eq!((counts.photos, counts.change), (3, 3), "the scope's photos");
    assert_eq!(tools.showing(), "preview");

    // The header narrows at a breakpoint once the window is that narrow; the tabs cannot.
    let stack = descendants(window.upcast_ref())
        .into_iter()
        .find(|widget| widget.is::<adw::ViewStack>())
        .expect("the tabs");
    let (width, _, _, _) = stack.measure(gtk::Orientation::Horizontal, -1);
    assert!(width < 400, "the tabs need {width} px, more than a phone has");

    // Another program rates one photo of the event; after a scan the preview knows it.
    let photo = library.join(EVENT).join("IMAG0001.jpg");
    let status = std::process::Command::new("exiftool")
        .args(["-q", "-overwrite_original", "-XMP-xmp:Rating=3"])
        .arg(&photo)
        .status()
        .unwrap();
    assert!(status.success());
    let before = opened.version();
    WidgetExt::activate_action(window, "win.scan", None).unwrap();
    until(
        || opened.version() > before && !opened.is_scanning(),
        "the scan finished",
    );
    counted(3);
    let counts = previewed("3");
    assert_eq!(counts.change, 2, "the preview after a scan is fresh");
    window.show_view("dashboard");
}

/// Each edit asks its value in a form of its own, and the value given makes the preview. A value
/// that does not read says why and previews nothing. Nothing is written here.
fn fills_in_the_forms_of_the_edits(window: &Window, opened: &Rc<Library>) {
    use photomanager_core::tools::{Answer, Located};
    let tools = window.tools();
    let preview = window.preview();
    let act = |name: &str, target: &str| WidgetExt::activate_action(window, name, Some(&target.to_variant())).unwrap();
    let previewed = |asked: &str, title: &str| {
        act("win.run-edit", asked);
        until(
            || !opened.is_busy() && preview.title().as_deref() == Some(title),
            "the edit was previewed",
        );
        preview.counts().unwrap()
    };
    window.show_view("tools");

    let form = || window.visible_dialog().map(|dialog| dialog.title().to_string());
    for (edit, title) in [
        ("add-tag", "Add Tag"),
        ("set-place", "Set Place"),
        ("set-date", "Set Date"),
        ("set-time-zone", "Set Time Zone"),
        ("tidy-tags", "Tidy Tags"),
        ("shift-dates", "Shift Dates"),
    ] {
        act("win.run-edit", edit);
        assert_eq!(form().as_deref(), Some(title), "{edit} shows its form");
        window.visible_dialog().unwrap().force_close();
    }
    act("win.tools-scope", "all");
    act("win.run-edit", "move-event");
    assert_ne!(form().as_deref(), Some("Move Event"), "no form without one event");
    assert!(
        tools.toast().contains("choose one event as the scope"),
        "{}",
        tools.toast()
    );

    act("win.run-edit", "add-tag:people//Anna");
    assert!(tools.toast().starts_with("Add Tag: "), "{}", tools.toast());

    let counts = previewed("rename-tag:People -> people", "Rename People to people");
    assert!(counts.change > 0);
    let counts = previewed("set-time-zone:", "Set the time zone of where each photo was taken");
    assert!(counts.change > 0);
    let beijing = opened.find_place("Beijing")[0].place.clone();
    let place = Answer::Place(Located::of(&beijing)).written();
    let counts = previewed(&format!("set-place:{place}"), "Set the place to Beijing");
    assert!(
        counts.change > 0 && counts.refused > 0,
        "a photo with a position of its own is refused"
    );

    act("win.tools-scope", "China/2006-09-00 Besuch Ben");
    act("win.run-edit", "move-event");
    assert_eq!(form().as_deref(), Some("Move Event"));
    let dialog = window.visible_dialog().expect("the folder form");
    assert!(
        headed(dialog.upcast_ref())
            .iter()
            .any(|(title, said)| title == "After" && said == "China/Beijing/2006-09-00 Besuch Ben"),
        "it starts from where Folder Migration would put it"
    );
    dialog.force_close();
    let counts = previewed(
        "move-event:China/Beijing/2006-09-00 Besuch Ben",
        "Move China/2006-09-00 Besuch Ben to China/Beijing/2006-09-00 Besuch Ben",
    );
    assert_eq!(counts.change, 1, "the event moves as one folder");
    window.show_view("dashboard");
}

/// The dashboard lists the events where a photo measured its position and others did not, and
/// opens the timeline of one: a lane per camera, the undated apart. A lane moved for the eye
/// writes nothing, a measured photo is never selected, each group names its source in the
/// preview, a group taken back is gone from it, and the apply writes exactly the targets.
fn gives_positions_from_a_neighbour(window: &Window, opened: &Rc<Library>, library: &std::path::Path) {
    const EVENT: &str = "Germany/2018-05-12 Canal Tour";
    const PHONE: &str = "Germany/2018-05-12 Canal Tour/PXL_0001.jpg";
    const EVENING: &str = "Germany/2018-05-12 Canal Tour/Evening/PXL_0002.jpg";
    const CENTRE: &str = "Germany/2018-05-12 Canal Tour/DSCF0201.JPG";
    const LATER: &str = "Germany/2018-05-12 Canal Tour/DSCF0202.JPG";
    const UNDATED: &str = "Germany/2018-05-12 Canal Tour/DSCF0203.JPG";
    let tools = window.tools();
    let page = tools.neighbour();
    let preview = window.preview();
    let act = |name: &str, target: &str| WidgetExt::activate_action(&page, name, Some(&target.to_variant())).unwrap();
    let plain = |name: &str| WidgetExt::activate_action(&page, name, None).unwrap();
    let bytes = |rel_path: &str| std::fs::read(library.join(rel_path)).unwrap();

    window.show_view("dashboard");
    until(
        || window.dashboard().neighbours_line().is_some(),
        "the dashboard lists the events",
    );
    let (line, events) = window.dashboard().neighbours_line().unwrap();
    assert_eq!(line, "Events where a neighbour knows the position");
    assert_eq!(
        events,
        [
            "Germany/2016-06-00 Harbour Walk",
            EVENT,
            "Germany/2019-07-13 Sommerfest"
        ]
    );

    WidgetExt::activate_action(window, "win.neighbour-event", Some(&EVENT.to_variant())).unwrap();
    assert_eq!(window.visible_view(), "tools");
    assert_eq!(tools.showing(), "neighbour");
    assert_eq!(window.scope(), Scope::Filter(Filter::all().within(EVENT)));
    until(|| page.is_loaded(), "the timeline was read");
    assert_eq!(
        page.lanes(),
        [("Google Pixel 3".to_string(), 2), ("FUJIFILM X100S".to_string(), 2)]
    );
    assert_eq!(page.undated(), [UNDATED]);

    let before = bytes(CENTRE);
    let x = page.x_of(CENTRE).unwrap();
    act("neighbour.shift", "1:60");
    assert!(page.x_of(CENTRE).unwrap() > x, "the lane moved along the axis");
    assert_eq!(bytes(CENTRE), before, "a lane moved for the eye writes nothing");
    act("neighbour.shift", "1:0");
    assert_eq!(page.x_of(CENTRE), Some(x));

    act("neighbour.select", EVENING);
    assert!(page.selected().is_empty(), "a measured photo is never a target");
    act("neighbour.select-many", &format!("{EVENING}\n{CENTRE}"));
    assert_eq!(page.selected(), [CENTRE]);
    plain("neighbour.give");
    assert!(page.groups().is_empty(), "no source yet");
    act("neighbour.source", PHONE);
    assert_eq!(page.source().as_deref(), Some(PHONE));
    act("neighbour.select", UNDATED);
    plain("neighbour.give");
    act("neighbour.source", EVENING);
    act("neighbour.select", LATER);
    act("neighbour.reach", "area");
    plain("neighbour.give");
    assert_eq!(page.groups().len(), 2);
    assert!(page.selected().is_empty(), "the selection became a group");

    let previewed = |title: &str| {
        plain("neighbour.preview");
        until(
            || !opened.is_busy() && preview.title().as_deref() == Some(title) && tools.showing() == "preview",
            "the groups were previewed",
        );
        preview.told()
    };
    let told = previewed("Positions from a neighbour in 2018-05-12 Canal Tour");
    let of = |told: &[(String, String)], rel_path: &str| {
        told.iter()
            .find(|(path, _)| path == rel_path)
            .map(|(_, change)| change.clone())
            .unwrap_or_default()
    };
    assert_eq!(told.len(), 3);
    assert!(of(&told, CENTRE).ends_with("from PXL_0001.jpg, 200 m"), "{told:?}");
    assert!(of(&told, LATER).ends_with("from PXL_0002.jpg, 1 km"), "{told:?}");

    tools.back_to("neighbour");
    assert_eq!(tools.showing(), "neighbour", "back from the preview is the timeline");
    WidgetExt::activate_action(&page, "neighbour.take-back", Some(&1i32.to_variant())).unwrap();
    assert_eq!(page.groups().len(), 1);
    plain("neighbour.preview");
    until(
        || !opened.is_busy() && tools.showing() == "preview" && preview.told().len() == 2,
        "the group taken back is gone from the preview",
    );
    let told = preview.told();
    assert!(of(&told, LATER).is_empty());

    let evening = bytes(EVENING);
    opened.acknowledge(None);
    WidgetExt::activate_action(window, "win.apply-change-set", None).unwrap();
    until(
        || preview.applied().is_some() && !opened.is_busy(),
        "the groups were written",
    );
    assert_eq!(preview.applied().unwrap().written, 2);
    assert_eq!(bytes(EVENING), evening, "a measured photo is untouched");
    let read = std::process::Command::new("exiftool")
        .args([
            "-s3",
            "-n",
            "-GPSLatitude",
            "-GPSProcessingMethod",
            "-GPSHPositioningError",
        ])
        .arg(library.join(CENTRE))
        .output()
        .unwrap();
    let read = String::from_utf8_lossy(&read.stdout);
    assert_eq!(
        read.lines().collect::<Vec<&str>>(),
        ["53.5485", "photoManager: neighbour", "200"]
    );
    window.show_view("dashboard");
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
        ["China", "Germany", "Greece", "Denmark", "Ireland"],
        "the most photos without GPS first"
    );
    dashboard.set_field(Gap::People);
    assert_eq!(
        dashboard.listed_places(),
        ["Germany", "China", "Greece", "Denmark", "Ireland"],
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
    assert_eq!(
        count,
        Some(survey.photos - 26),
        "the number clicked is the number shown"
    );

    WidgetExt::activate_action(window, "win.show-photos", Some(&"no-gps@Germany".to_variant())).unwrap();
    assert_eq!(window.shown().unwrap().1, Some(5));
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
    assert_eq!(listed().len(), 5, "the grid holds exactly the photos counted");
    assert_eq!(window.shown().unwrap().1, Some(5));

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
    assert_eq!(listed().len(), all - 26);
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
    assert_eq!(gallery.chips(), ["without GPS", "Loose files"]);
    assert_eq!(listed(), ["China/IMG_3140.JPG"]);
    assert!(gallery.close_chip("Loose files"));
    settle(window);
    assert_eq!(filter(), "no-gps", "the chip took its part out and nothing else");
    assert_eq!(gallery.chips(), ["without GPS"]);
    act("win.gallery-gap", "none");
    assert!(gallery.chips().is_empty(), "no chips for the whole library");

    browses_by_person(window);

    act("win.show-photos", "issue:not a photo");
    assert_eq!(gallery.page(), "empty", "the fixture has no file that is not a photo");

    selects_and_hands_on_a_scope(window, opened);
}

/// The People sidebar lists who the photos name and narrows to them, several together; a
/// sidebar lists only what would still show photos; every part of the filter is a chip above the
/// grid whichever sidebar is open, and each tab that narrows the grid carries a dot.
fn browses_by_person(window: &Window) {
    let gallery = window.gallery();
    let act = |name: &str, target: &str| {
        WidgetExt::activate_action(window, name, Some(&target.to_variant())).unwrap();
        settle(window);
    };
    let filter = || window.shown().unwrap().0.to_string();

    act("win.show-photos", "all");
    gallery.browse_by("people");
    settle(window);
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(
        gallery.people_listed(),
        ["Anna", "Mia", "Tom"],
        "a person without a box too"
    );
    gallery.search_people("to");
    settle(window);
    assert_eq!(gallery.people_listed(), ["Tom"], "the search narrows the list");
    gallery.search_people("");
    settle(window);

    act("win.gallery-person", "Mia");
    assert_eq!(filter(), "person:Mia");
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(
        gallery.count_shown("places", "Germany"),
        Some((0, false)),
        "a country Mia is not in is not listed"
    );
    assert_eq!(gallery.count_shown("places", "Denmark"), Some((1, true)));
    assert_eq!(
        gallery.count_shown("people", "Tom"),
        Some((0, false)),
        "a person never with Mia is not listed"
    );
    assert_eq!(gallery.people_listed(), ["Mia"]);
    assert_eq!(
        window.gallery().listed(),
        ["Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG"]
    );
    assert_eq!(gallery.dots(), ["people"]);
    act("win.gallery-person", "Mia");
    assert_eq!(filter(), "all", "the chosen person again widens back");
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(
        gallery.count_shown("places", "Germany"),
        Some((25, true)),
        "with nothing chosen, the library's count"
    );
    assert_eq!(gallery.people_listed(), ["Anna", "Mia", "Tom"], "everyone again");
    for quick in ["person:Mia", "person:Tom"] {
        WidgetExt::activate_action(window, "win.show-photos", Some(&quick.to_variant())).unwrap();
    }
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(
        gallery.count_shown("places", "Germany"),
        Some((1, true)),
        "only the newest recount is shown"
    );
    act("win.show-photos", "all");

    act("win.gallery-person", "Tom");
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(
        gallery.people_listed(),
        ["Tom", "Anna"],
        "the chosen first, then those in photos with them"
    );
    act("win.gallery-person", "Anna");
    assert_eq!(filter(), "person:Tom&Anna");
    assert_eq!(
        window.gallery().listed(),
        ["Germany/2019-07-13 Sommerfest/IMAG0001.jpg"],
        "the photos they are in together"
    );
    assert_eq!(gallery.chips(), ["Tom", "Anna"], "a chip for each person");
    until(|| !gallery.is_recounting(), "the sidebars counted again");
    assert_eq!(gallery.count_shown("tags", "people"), Some((1, true)));
    assert_eq!(gallery.count_shown("tags", "mixed"), Some((0, false)));
    assert!(gallery.close_chip("Tom"));
    settle(window);
    assert_eq!(filter(), "person:Anna", "the chip took out one person");
    act("win.show-photos", "all");

    act("win.gallery-tag", "people");
    act("win.gallery-tag", "places/inGermany");
    assert_eq!(filter(), "tag:people&places/inGermany");
    assert_eq!(gallery.chips(), ["tag: people", "tag: places/inGermany"]);
    assert_eq!(
        window.gallery().listed(),
        ["Germany/2019-07-13 Sommerfest/IMAG0001.jpg"],
        "the photos carrying both"
    );
    act("win.gallery-tag", "places");
    assert_eq!(
        filter(),
        "tag:people&places",
        "a tag above a chosen one takes its place"
    );
    act("win.show-photos", "all");

    act("win.gallery-place", "Germany/2019-07-13 Sommerfest");
    act("win.gallery-tag", "people");
    act("win.gallery-person", "Tom");
    act("win.gallery-gap", "no-gps");
    assert_eq!(filter(), "no-gps+tag:people+person:Tom@Germany/2019-07-13 Sommerfest");
    assert_eq!(
        gallery.chips(),
        ["without GPS", "tag: people", "Tom", "2019-07-13 Sommerfest"],
        "a chip for every part"
    );
    assert_eq!(gallery.dots(), ["places", "tags", "people"]);
    assert!(gallery.close_chip("tag: people"));
    settle(window);
    assert_eq!(filter(), "no-gps+person:Tom@Germany/2019-07-13 Sommerfest");
    assert_eq!(gallery.dots(), ["places", "people"], "the dots follow the parts");
    assert!(gallery.close_chip("2019-07-13 Sommerfest"));
    settle(window);
    assert_eq!(filter(), "no-gps+person:Tom");
    let clear = descendants(gallery.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Clear All"))
        .expect("Clear All");
    clear.emit_clicked();
    settle(window);
    assert_eq!(filter(), "all");
    assert!(gallery.chips().is_empty());
    assert!(gallery.dots().is_empty());
    gallery.browse_by("places");
    settle(window);
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
        Scope::Filter(Filter::missing(Gap::Gps)),
        "all selected is the filter, not its paths"
    );
    assert_eq!(
        window.tools().picked(),
        Some(window.scope()),
        "and it is on offer as the gallery's"
    );
    assert!(gallery.toast().contains("without GPS"), "{}", gallery.toast());
    assert_eq!(opened.scope_paths(&window.scope()), {
        let mut sorted = listed.clone();
        sorted.sort();
        sorted
    });

    WidgetExt::activate_action(window, "win.gallery-select-none", None).unwrap();
    gallery.select(1, true);
    WidgetExt::activate_action(window, "win.use-as-scope", None).unwrap();
    assert_eq!(opened.scope_paths(&window.scope()), [listed[1].clone()]);

    WidgetExt::activate_action(window, "win.gallery-place", Some(&"Germany".to_variant())).unwrap();
    assert_eq!(gallery.selected(), 0, "another filter, no selection");
    settle(window);
    assert_eq!(gallery.selected(), 0);
    window.show_view("dashboard");
}

/// A photo opens over the grid, arrives at its own size the right way up, steps through the
/// grid's list in the grid's order, and Back leaves the grid as it was.
fn looks_at_one_photo(window: &Window, library: &std::path::Path) {
    let gallery = window.gallery();
    let page = gallery.photo();
    let act = |name: &str, target: Option<&str>| {
        let target = target.map(|target| target.to_variant());
        WidgetExt::activate_action(window, name, target.as_ref()).unwrap();
    };

    act("win.show-photos", Some("all@Germany/2019-07-13 Sommerfest"));
    settle(window);
    act("win.gallery-sort", Some("name"));
    settle(window);
    let listed = gallery.listed();
    assert_eq!(listed.len(), 3);
    gallery.select(1, true);

    act("win.show-photo", Some(LOCATED));
    assert!(gallery.photo_open(), "the page was pushed");
    assert_eq!(page.path().as_deref(), Some(LOCATED));
    assert_eq!(page.count(), 3, "the page walks the grid's list");
    until(|| page.full_size().is_some(), "the full size arrived");
    assert_eq!(
        page.full_size(),
        Some((16, 24)),
        "stored 24x16 with orientation 6, so it stands taller than wide"
    );
    assert!(page.held() <= 3, "the photo and one on each side, no more");

    act("win.photo-first", None);
    assert_eq!(page.path().as_deref(), Some(listed[0].as_str()));
    act("win.photo-previous", None);
    assert_eq!(page.position(), 0, "the first photo stays the first");
    act("win.photo-next", None);
    assert_eq!(
        page.path().as_deref(),
        Some(listed[1].as_str()),
        "by name, as the grid shows it"
    );
    act("win.photo-last", None);
    act("win.photo-next", None);
    assert_eq!(
        page.path().as_deref(),
        Some(listed[2].as_str()),
        "the last photo stays the last"
    );

    act("win.photo-close", None);
    assert!(!gallery.photo_open(), "back to the grid");
    assert!(page.path().is_none(), "the pictures were let go");
    assert_eq!(page.held(), 0);
    assert_eq!(
        window.shown().unwrap().0.to_string(),
        "all@Germany/2019-07-13 Sommerfest"
    );
    assert_eq!(gallery.order().key(), "name");
    assert_eq!(gallery.selected(), 1, "the selection is as it was");

    let grid = descendants(gallery.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::GridView>().ok())
        .expect("the grid");
    grid.emit_by_name::<()>("activate", &[&2u32]);
    assert!(gallery.photo_open(), "Enter or a double-click opens the photo");
    assert_eq!(page.path().as_deref(), Some(listed[2].as_str()));
    act("win.photo-close", None);

    // A photo whose file went bad since the scan keeps its thumbnail and says so.
    let broken = library.join(&listed[0]);
    let bytes = std::fs::read(&broken).unwrap();
    std::fs::write(&broken, &bytes[..40]).unwrap();
    act("win.show-photo", Some(listed[0].as_str()));
    until(|| page.full_failed().is_some(), "the full size was refused");
    until(|| page.shows_picture(), "the thumbnail is on screen");
    assert!(page.full_size().is_none());
    std::fs::write(&broken, &bytes).unwrap();
    act("win.photo-close", None);

    act("win.gallery-sort", Some("date"));
    settle(window);
    window.show_view("dashboard");
}

const LOCATED: &str = "Germany/2019-07-13 Sommerfest/img_0657.jpg";

/// The panel says what the cache knows, the nearest place comes from the local place data, and
/// the map, the one thing that would ask a server, is not made until it is asked for.
fn reads_the_panel(window: &Window) {
    let gallery = window.gallery();
    let page = gallery.photo();
    let open = |path: &str| {
        WidgetExt::activate_action(window, "win.show-photos", Some(&"all".to_variant())).unwrap();
        settle(window);
        WidgetExt::activate_action(window, "win.show-photo", Some(&path.to_variant())).unwrap();
        until(
            || page.details().is_some_and(|details| details.rel_path == path),
            "the details arrived",
        );
        page.panel_texts()
    };
    let has = |texts: &[String], wanted: &str| {
        assert!(texts.iter().any(|text| text == wanted), "no {wanted:?} in {texts:#?}");
    };

    let texts = open("Germany/2019-07-13 Sommerfest/IMAG0001.jpg");
    has(&texts, "Taken: 2019-07-13 20:41:00");
    has(&texts, "Folder date: 2019-07-13");
    has(&texts, "Folder date agrees");
    has(&texts, "Tag: people/family/Anna");
    has(&texts, "Tag: places/inGermany");
    has(&texts, "Person: Anna");
    has(&texts, "Person: Tom");
    assert!(
        !texts.iter().any(|text| text == "Person: me"),
        "a people tag is a tag, not a person"
    );
    let frames = || page.frame_rects(400.0, 300.0);
    assert!(frames().is_empty(), "no face framed until a person is pointed at");
    let rows = page.boxed_people();
    let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Anna", "Tom"]);
    rows[0].1.grab_focus();
    until(
        || page.pointed().as_deref() == Some("Anna"),
        "the focused row frames Anna",
    );
    until(|| page.shows_picture(), "the picture is there to frame on");
    let near = |one: &[(f64, f64, f64, f64)], other: &[(f64, f64, f64, f64)]| {
        one.len() == other.len()
            && one.iter().zip(other).all(|(a, b)| {
                [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)]
                    .iter()
                    .all(|(a, b)| (a - b).abs() < 0.01)
            })
    };
    assert!(
        near(&frames(), &[(86.0, 33.0, 84.0, 114.0)]),
        "a square picture fitted into 400 by 300 is 300 wide, 50 in: {:?}",
        frames()
    );
    page.point_at(Some("Tom"));
    assert!(near(&frames(), &[(185.0, 240.0, 30.0, 30.0)]), "{:?}", frames());
    page.point_at(None);
    assert!(frames().is_empty());

    let texts = open("Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0002.JPG");
    assert!(page.pointed().is_none(), "another photo frames nobody");
    assert!(page.boxed_people().is_empty(), "Mia has no box to frame");
    has(&texts, "Person: Mia, no face box");
    has(&texts, "No coordinates");
    assert!(!page.shows_map(), "no coordinates, no map");

    let texts = open("Denmark/2018-10-00 Wedding Trip to Copenhagen/DSCF0001.JPG");
    has(&texts, "Camera: FUJIFILM X100S");
    has(&texts, "Folder date: 2018-10");
    has(&texts, "No people");

    let texts = open(LOCATED);
    has(&texts, "Coordinates: 53.551100, 9.993700");
    has(&texts, "Nearest place: Hamburg, Germany");
    has(&texts, "Orientation: 6, turned right");
    assert!(!page.shows_map(), "no map until it is asked for");

    assert!(!page.raw_open(), "All Fields starts closed");
    page.set_raw_open(true);
    assert!(page.raw_open());
    let (there, back) = match page.position() + 1 < page.count() {
        true => ("win.photo-next", "win.photo-previous"),
        false => ("win.photo-previous", "win.photo-next"),
    };
    WidgetExt::activate_action(window, there, None).unwrap();
    until(
        || page.details().is_some_and(|details| details.rel_path != LOCATED),
        "the next photo",
    );
    assert!(page.raw_open(), "open, it stays open while stepping");
    WidgetExt::activate_action(window, back, None).unwrap();
    until(
        || page.details().is_some_and(|details| details.rel_path == LOCATED),
        "back again",
    );
    let all = page.raw_shown();
    assert!(all > 5, "every raw field is listed: {all}");
    page.filter_raw("gpslat");
    let narrowed = page.raw_shown();
    assert!(narrowed > 0 && narrowed < all, "{narrowed} of {all}");
    page.filter_raw("");
    assert_eq!(page.raw_shown(), all);

    WidgetExt::activate_action(window, "win.photo-close", None).unwrap();
    open(LOCATED);
    assert!(!page.raw_open(), "a photo opened again starts with All Fields closed");

    WidgetExt::activate_action(window, "win.photo-panel", None).unwrap();
    assert!(!page.shows_panel(), "F9 hides the panel");
    WidgetExt::activate_action(window, "win.photo-panel", None).unwrap();
    assert!(page.shows_panel());

    WidgetExt::activate_action(window, "win.photo-close", None).unwrap();
    window.show_view("dashboard");
}

/// The form's change is exactly what was edited, the review lists what the engine would write
/// for it, and nothing is written here: the apply is the smoke test's.
fn edits_one_photo(window: &Window) {
    let gallery = window.gallery();
    let page = gallery.photo();
    let act = |name: &str| WidgetExt::activate_action(window, name, None).unwrap();
    WidgetExt::activate_action(window, "win.show-photos", Some(&"all".to_variant())).unwrap();
    settle(window);
    WidgetExt::activate_action(window, "win.show-photo", Some(&LOCATED.to_variant())).unwrap();
    until(
        || page.details().is_some_and(|details| details.rel_path == LOCATED),
        "the details arrived",
    );
    let file = window.library().unwrap().paths().library().join(LOCATED);
    let before = std::fs::read(&file).unwrap();

    act("win.photo-edit");
    assert!(page.editing());
    assert_eq!(
        page.pending(),
        Some(Ok(Vec::new())),
        "nothing edited, nothing to change"
    );
    assert!(!page.can_review(), "and nothing to review");

    assert!(page.set_form("Date Taken", "2019-07-13 18:25:00"));
    page.form_add_tag("mixed/food");
    assert_eq!(page.pending(), Some(Ok(vec!["date", "tags"])), "exactly the two fields");
    assert!(page.can_review());

    page.set_form("Date Taken", "2019-13-13 18:25:00");
    assert!(matches!(page.pending(), Some(Err(why)) if why.contains("month 13")));
    assert!(!page.can_review(), "a form that does not parse cannot be reviewed");
    page.set_form("Date Taken", "2019-07-13 18:25:00");

    act("win.photo-review");
    until(|| page.review_lines().is_some(), "the review arrived");
    let lines = page.review_lines().unwrap();
    let has = |tag: &str| lines.iter().any(|line| line.starts_with(&format!("{tag}: ")));
    assert!(has("EXIF:DateTimeOriginal"), "{lines:#?}");
    assert!(has("XMP-digiKam:TagsList"), "{lines:#?}");
    assert!(!has("EXIF:GPSLatitude"), "the position was not touched: {lines:#?}");
    page.cancel_review();
    assert!(page.review_lines().is_none());

    // Stepping away from a change asks first, and nothing moves until it is answered.
    act("win.photo-next");
    assert_eq!(
        page.path().as_deref(),
        Some(LOCATED),
        "an unfinished edit is not dropped"
    );
    assert!(page.editing());
    let dialog = window
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
        .expect("the question");
    dialog.emit_by_name::<()>("response", &[&"discard"]);
    assert!(!page.editing(), "discarded");
    assert_ne!(page.path().as_deref(), Some(LOCATED), "and then it stepped on");

    assert_eq!(before, std::fs::read(&file).unwrap(), "a review writes nothing");
    act("win.photo-close");
    window.show_view("dashboard");
}

/// Runs the main loop until `done`, or fails after a while.
fn until(done: impl Fn() -> bool, what: &str) {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done() && std::time::Instant::now() < deadline {
        context.iteration(false);
    }
    assert!(done(), "never happened: {what}");
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

/// The title and subtitle of every row, the ones that open to more rows too.
fn headed(widget: &gtk::Widget) -> Vec<(String, String)> {
    descendants(widget)
        .into_iter()
        .filter_map(|widget| match widget.downcast::<adw::ActionRow>() {
            Ok(row) => Some((row.title().to_string(), row.subtitle().unwrap_or_default().to_string())),
            Err(widget) => widget
                .downcast::<adw::ExpanderRow>()
                .ok()
                .map(|row| (row.title().to_string(), row.subtitle().to_string())),
        })
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
