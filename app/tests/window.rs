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

    previews_what_a_tool_would_change(&window, &opened);
    lists_the_tools_for_a_scope(&window, &opened, &library);
    answers_the_questions_of_a_tool(&window, &opened);
    answers_by_event_and_on_the_map(&window);
    answers_a_question_about_a_camera(&window);
    reads_the_history(&window, &opened);
    keeps_a_tag_vocabulary(&window, &opened);
    runs_tools_together(&window, &opened);
}

/// The tag tool opens its tree instead of questions. A rename merges two nodes and the merged
/// one counts both; taking the rule out brings the other back; a rule that would undo an earlier
/// one says why; a suggestion is a click; and the preview holds exactly the photos the rules
/// touch plus the untidy ones. Nothing is written here.
fn keeps_a_tag_vocabulary(window: &Window, opened: &Rc<Library>) {
    use photomanager_core::cache::Cache;
    use photomanager_core::tools::Settings;
    use photomanager_core::tools::tag_vocabulary::Vocabulary;
    const TOOL: &str = "tag-vocabulary";
    let tools = window.tools();
    let page = tools.vocabulary();
    let act = |name: &str, target: gtk::glib::Variant| WidgetExt::activate_action(window, name, Some(&target)).unwrap();
    let looked = || {
        until(
            || !page.is_busy() && page.overview().is_some(),
            "the tags were looked at",
        );
        page.overview().unwrap()
    };
    window.show_view("tools");
    act("win.tools-scope", "all".to_variant());
    act("win.run-tool", TOOL.to_variant());
    assert_eq!(tools.showing(), "vocabulary", "a tree, not questions");
    let before = looked();
    let people = before.tree.count("people").unwrap();
    let other = before.tree.count("People").unwrap();
    assert!(
        labels(page.upcast_ref()).iter().any(|label| label == "Suggestions"),
        "the suggestions are on top"
    );

    act("win.tag-rule", "rename People -> people".to_variant());
    let merged = looked();
    assert_eq!(merged.tree.count("People"), None);
    assert_eq!(
        merged.tree.count("people"),
        Some(people + other),
        "the merged node counts both"
    );
    assert!(
        rows(page.upcast_ref())
            .iter()
            .any(|row| row.title() == "Rename People to people"),
        "the rule is listed"
    );

    act("win.tag-rule", "rename people -> People".to_variant());
    let why = page.refused().expect("the rule was refused");
    assert!(why.contains("back to where it was"), "{why}");
    assert_eq!(page.vocabulary().rules.0.len(), 1, "a refused rule is not kept");

    act("win.tag-forget-rule", 0.to_variant());
    let back = looked();
    assert_eq!(back.tree.count("People"), Some(other), "without the rule it is back");

    let twin = back
        .suggestions
        .iter()
        .find(|suggestion| suggestion.key == "twin:people")
        .expect("the twins are suggested");
    assert_eq!(twin.offer, "Merge Into people");
    act("win.tag-suggestion", ("twin:people", "confirm").to_variant());
    let confirmed = looked();
    assert_eq!(confirmed.tree.count("People"), None, "confirm makes it a rule");
    act("win.tag-suggestion", ("mixed:food", "leave").to_variant());
    let left = looked();
    assert!(left.suggestions.iter().all(|suggestion| suggestion.key != "mixed:food"));

    act("win.tag-generated", "kept".to_variant());
    looked();
    let settings = page.settings().unwrap();
    assert_eq!(Vocabulary::read(&settings).unwrap().generated.key(), "kept");
    assert_eq!(opened.tool_settings(TOOL), Some(settings.clone()), "kept at once");

    act("win.tag-rename", "mixed/wired".to_variant());
    assert_eq!(
        page.editing().as_deref(),
        Some("mixed/wired"),
        "the rename dialog is open"
    );

    let cache = Cache::read_only(&opened.paths().cache_db()).unwrap().unwrap();
    let expected = photomanager_core::tools::find(TOOL)
        .unwrap()
        .change_set(&cache, None, &window.scope(), Some(&settings))
        .unwrap();
    WidgetExt::activate_action(window, "win.preview-tags", None).unwrap();
    let preview = window.preview();
    until(
        || preview.title().as_deref() == Some("Tidy the tags with 1 rule") && !opened.is_busy(),
        "the rules were previewed",
    );
    let counts = preview.counts().unwrap();
    assert_eq!(counts.change, expected.counts().change);
    assert!(counts.change > 0);
    assert_eq!(tools.showing(), "preview");
    window.show_view("dashboard");
}

/// Two tools chosen together are one preview, named after both, with one row per photo.
fn runs_tools_together(window: &Window, opened: &Rc<Library>) {
    window.show_view("tools");
    WidgetExt::activate_action(
        window,
        "win.tools-scope",
        Some(&"Germany/2015-00-00 Seasons".to_variant()),
    )
    .unwrap();
    WidgetExt::activate_action(
        window,
        "win.run-together",
        Some(&"time-zones,tag-vocabulary".to_variant()),
    )
    .unwrap();
    let preview = window.preview();
    until(
        || {
            preview
                .title()
                .is_some_and(|title| title.starts_with("Write time zones and XMP dates and tidy the tags"))
                && !opened.is_busy()
        },
        "the two tools were previewed as one",
    );
    let counts = preview.counts().unwrap();
    assert_eq!(
        (counts.photos, counts.change),
        (2, 2),
        "one row per photo with a date to zone, the third states its offset and its tags are tidy"
    );
    window.show_view("dashboard");
}

/// A tool that asks shows one row per tag with its photos, the row on the list says what waits,
/// Confirm Exact Matches answers exactly the exact ones, an answer outlives the window, and the
/// preview holds the answered photos and no other. Nothing is written here.
fn answers_the_questions_of_a_tool(window: &Window, opened: &Rc<Library>) {
    use photomanager_core::tools::Answer;
    const TOOL: &str = "gps-from-places-tag";
    let tools = window.tools();
    let questions = tools.questions();
    let act = |name: &str, target: gtk::glib::Variant| WidgetExt::activate_action(window, name, Some(&target)).unwrap();
    let count_of = |tools: &photomanager::tools::Tools| {
        until(|| tools.counted().is_some(), "the tools were counted");
        let counted = tools.counted().unwrap();
        let count = counted
            .tools
            .iter()
            .find(|(key, _)| key == TOOL)
            .unwrap()
            .1
            .clone()
            .unwrap();
        let waiting = counted.waiting.iter().find(|(key, _)| key == TOOL).unwrap().1;
        (count, waiting)
    };
    window.show_view("tools");
    act("win.tools-scope", "all".to_variant());
    assert_eq!(count_of(&tools), (0, 8), "nothing is answered yet");
    assert!(
        labels(tools.upcast_ref())
            .iter()
            .any(|label| label == "8 tags wait for an answer"),
        "the row says what waits instead of a count of nothing"
    );

    act("win.run-tool", TOOL.to_variant());
    assert_eq!(tools.showing(), "questions");
    until(|| !questions.is_busy(), "the questions were asked");
    let asked = questions.questions();
    assert_eq!(asked.len(), 10, "one question per tag");
    let shown: Vec<String> = rows(questions.upcast_ref())
        .iter()
        .map(|row| row.title().to_string())
        .collect();
    for question in &asked {
        assert_eq!(
            shown.iter().filter(|title| **title == question.title).count(),
            1,
            "one row for {}",
            question.title
        );
    }
    let beijing = rows(questions.upcast_ref())
        .into_iter()
        .find(|row| row.title() == "inChina/Beijing")
        .expect("a row for Beijing");
    assert!(beijing.subtitle().unwrap().starts_with("2 photos - Best match Beijing"));

    act("win.answer-exact", TOOL.to_variant());
    let answered = questions.questions();
    for question in &answered {
        let exact = !question.apart && question.sure().is_some();
        assert_eq!(
            matches!(question.answer, Some(Answer::Place(_))),
            exact,
            "{} is answered only if it matched exactly",
            question.key
        );
    }
    let confirmed = answered
        .iter()
        .filter(|question| matches!(question.answer, Some(Answer::Place(_))))
        .count();
    assert_eq!(confirmed, 6);
    until(|| count_of(&tools) == (7, 2), "the row counts what the answers give");

    // Another window on another opening of the same library finds the answers where they were.
    let again = Library::open(opened.paths().clone()).expect("open the library again");
    let other: Window = gtk::glib::Object::builder().build();
    other.set_library(Some(again));
    WidgetExt::activate_action(&other, "win.run-tool", Some(&TOOL.to_variant())).unwrap();
    let remembered = other.tools().questions();
    until(
        || !remembered.is_busy() && !remembered.questions().is_empty(),
        "asked again",
    );
    assert_eq!(
        remembered.settings(),
        questions.settings(),
        "the answers are remembered"
    );
    assert_eq!(
        remembered
            .questions()
            .iter()
            .map(|question| question.answer.clone())
            .collect::<Vec<_>>(),
        answered
            .iter()
            .map(|question| question.answer.clone())
            .collect::<Vec<_>>()
    );
    other.close();

    WidgetExt::activate_action(window, "win.preview-answers", None).unwrap();
    let preview = window.preview();
    until(
        || preview.title().as_deref() == Some("Set GPS from the places tag") && !opened.is_busy(),
        "the answers were previewed",
    );
    let counts = preview.counts().unwrap();
    assert_eq!(
        (counts.photos, counts.change),
        (7, 7),
        "the answered photos and no other"
    );
    assert_eq!(tools.showing(), "preview");

    act("win.answer", (TOOL, "places/inGreece/Atens", "leave").to_variant());
    until(
        || count_of(&tools) == (8, 1),
        "left alone, the photo with both tags is free",
    );
    act("win.answer", (TOOL, "places/inGreece/Atens", "forget").to_variant());
    until(|| count_of(&tools) == (7, 2), "asked again, it waits again");
    window.show_view("dashboard");
}

/// The event tool asks one question per event on the same page, with its own words; its bulk
/// button confirms only the events whose located photos agree, and a pin dropped on the map
/// answers a question. Nothing is written here.
fn answers_by_event_and_on_the_map(window: &Window) {
    use photomanager_core::tools::Answer;
    const TOOL: &str = "gps-from-the-event";
    const HARBOUR: &str = "Germany/2016-06-00 Harbour Walk";
    const COPENHAGEN: &str = "Denmark/2018-10-00 Wedding Trip to Copenhagen";
    let tools = window.tools();
    let questions = tools.questions();
    let act = |name: &str, target: gtk::glib::Variant| WidgetExt::activate_action(window, name, Some(&target)).unwrap();
    let count_of = |tools: &photomanager::tools::Tools| {
        until(|| tools.counted().is_some(), "the tools were counted");
        let counted = tools.counted().unwrap();
        let count = counted
            .tools
            .iter()
            .find(|(key, _)| key == TOOL)
            .unwrap()
            .1
            .clone()
            .unwrap();
        let waiting = counted.waiting.iter().find(|(key, _)| key == TOOL).unwrap().1;
        (count, waiting)
    };
    window.show_view("tools");
    act("win.tools-scope", "all".to_variant());
    assert_eq!(count_of(&tools), (0, 6));
    assert!(
        labels(tools.upcast_ref())
            .iter()
            .any(|label| label == "6 events wait for an answer"),
        "the row says what waits"
    );

    act("win.run-tool", TOOL.to_variant());
    until(
        || questions.key().as_deref() == Some(TOOL) && !questions.is_busy(),
        "the events were asked about",
    );
    let asked = questions.questions();
    assert_eq!(asked.len(), 6, "one question per event");
    let shown = rows(questions.upcast_ref());
    for question in &asked {
        assert_eq!(
            shown.iter().filter(|row| row.title() == question.title).count(),
            1,
            "one row for {}",
            question.title
        );
    }
    let harbour = shown
        .iter()
        .find(|row| row.title() == "2016-06-00 Harbour Walk")
        .expect("a row for the walk");
    assert_eq!(
        harbour.subtitle().unwrap(),
        "1 photo - Best match Hamburg, Hamburg, Germany, where 3 of its photos are"
    );
    let menu = descendants(harbour.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
        .and_then(|button| button.menu_model())
        .expect("a menu of answers");
    let items: Vec<String> = (0..menu.n_items())
        .filter_map(|index| {
            menu.item_attribute_value(index, "label", None)
                .and_then(|label| label.get::<String>())
        })
        .collect();
    assert!(items.iter().any(|item| item == "Pick on Map…"), "{items:?}");
    assert!(
        labels(questions.upcast_ref())
            .iter()
            .any(|label| label == "Confirm Where the Rest Is"),
        "the bulk button is worded by the tool"
    );

    act("win.answer-exact", TOOL.to_variant());
    let answered: Vec<String> = questions
        .questions()
        .into_iter()
        .filter(|question| question.answer.is_some())
        .map(|question| question.key)
        .collect();
    assert_eq!(answered, [HARBOUR], "only where the rest agrees");

    act("win.answer", (TOOL, COPENHAGEN, "map").to_variant());
    assert_eq!(
        questions.picking(),
        Some((COPENHAGEN.to_string(), None)),
        "the map is open"
    );
    questions.pick_point(55.6800, 12.5900);
    let (_, pin) = questions.picking().unwrap();
    let Some(Answer::Pin { near, .. }) = pin else {
        panic!("the click made no pin: {pin:?}");
    };
    assert_eq!(near.name, "Copenhagen");
    assert!(questions.use_point());
    assert_eq!(questions.picking(), None, "the map closed");
    let copenhagen = questions
        .questions()
        .into_iter()
        .find(|question| question.key == COPENHAGEN)
        .unwrap();
    assert!(
        matches!(copenhagen.answer, Some(Answer::Pin { lat, lon, .. }) if (lat, lon) == (55.68, 12.59)),
        "{:?}",
        copenhagen.answer
    );
    until(|| count_of(&tools) == (2, 4), "the row counts what the answers give");
    window.show_view("dashboard");
}

/// A question about a camera's clock has Enter a Shift where a place has the map, Confirm puts
/// the offer's own answer in, and a typed shift is checked before it is kept. Nothing is written
/// here.
fn answers_a_question_about_a_camera(window: &Window) {
    use photomanager_core::tools::{Answer, Kind};
    const TOOL: &str = "dates-against-the-folder";
    const PARTY: &str = "Germany/2013-05-18 Garden Party";
    let tools = window.tools();
    let questions = tools.questions();
    let act = |name: &str, target: gtk::glib::Variant| WidgetExt::activate_action(window, name, Some(&target)).unwrap();
    let asked = |key: &str| {
        questions
            .questions()
            .into_iter()
            .find(|question| question.key == key)
            .unwrap_or_else(|| panic!("nothing asks about {key}"))
    };
    window.show_view("tools");
    act("win.tools-scope", "all".to_variant());
    act("win.run-tool", TOOL.to_variant());
    until(
        || questions.key().as_deref() == Some(TOOL) && !questions.is_busy(),
        "the events were asked about",
    );
    let party = asked(PARTY);
    assert_eq!(party.kind, Kind::Shift);
    let row = rows(questions.upcast_ref())
        .into_iter()
        .find(|row| row.title() == "2013-05-18 Garden Party")
        .expect("a row for the party");
    let subtitle = row.subtitle().unwrap();
    assert!(
        subtitle.contains("Offered: Shift DMC-TZ7 by +623d 23:45 (more than a year)"),
        "{subtitle}"
    );
    assert!(
        subtitle.contains("DMC-TZ7: 2 photos"),
        "the evidence per camera: {subtitle}"
    );
    let menu = descendants(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
        .and_then(|button| button.menu_model())
        .expect("a menu of answers");
    let items: Vec<String> = (0..menu.n_items())
        .filter_map(|index| {
            menu.item_attribute_value(index, "label", None)
                .and_then(|label| label.get::<String>())
        })
        .collect();
    assert!(items.iter().any(|item| item == "Enter a Shift…"), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|item| item == "Pick on Map…" || item == "Choose Another…"),
        "{items:?}"
    );
    assert!(
        !labels(questions.upcast_ref())
            .iter()
            .any(|label| label == "Confirm Exact Matches"),
        "no bulk button for dates"
    );

    act("win.answer", (TOOL, PARTY, "best").to_variant());
    assert_eq!(
        asked(PARTY).answer,
        Some(party.offers[0].answer.clone()),
        "Confirm puts the offer in"
    );

    act("win.answer", (TOOL, PARTY, "shift").to_variant());
    assert_eq!(questions.typing().as_deref(), Some(PARTY), "the shift dialog is open");
    let typed = |text: &str| questions.type_shift(PARTY, &[("DMC-TZ7".to_string(), text.to_string())]);
    assert!(typed("soon").unwrap_err().starts_with("DMC-TZ7: "));
    assert_eq!(
        questions.typing().as_deref(),
        Some(PARTY),
        "a bad shift keeps the dialog open"
    );
    typed("-2d").unwrap();
    assert_eq!(questions.typing(), None, "the dialog closed");
    let Some(Answer::Shift(moved)) = asked(PARTY).answer else {
        panic!("not a shift: {:?}", asked(PARTY).answer);
    };
    assert_eq!(moved[0].by.written(), "-2d");
    window.show_view("dashboard");
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

/// Each tool says what it would change for the scope, and that is the number the preview shows
/// when it is opened. The count follows the scope, and the library after a scan.
fn lists_the_tools_for_a_scope(window: &Window, opened: &Rc<Library>, library: &std::path::Path) {
    const EVENT: &str = "Germany/2019-07-13 Sommerfest";
    const DEMO: &str = "demo-rating";
    let tools = window.tools();
    let act = |name: &str, target: &str| WidgetExt::activate_action(window, name, Some(&target.to_variant())).unwrap();
    let demo = || {
        until(|| tools.counted().is_some(), "the tools were counted");
        let counted = tools.counted().unwrap();
        let (_, count) = counted
            .tools
            .iter()
            .find(|(key, _)| key == DEMO)
            .expect("the demo is listed");
        (counted.photos, *count.as_ref().unwrap())
    };
    window.show_view("tools");

    act("win.tools-scope", "all");
    assert_eq!(
        demo(),
        (
            photomanager_core::fixtures::photo_count(),
            photomanager_core::fixtures::photo_count()
        )
    );
    act("win.tools-scope", EVENT);
    assert_eq!(window.scope(), Scope::Filter(Filter::all().within(EVENT)));
    assert_eq!(demo(), (3, 3), "the event narrows the count to its photos");

    act("win.tools-scope", "picked");
    assert_eq!(
        window.scope(),
        tools.picked().unwrap(),
        "what the gallery handed over is a scope again"
    );
    act("win.tools-scope", EVENT);

    act("win.run-tool", DEMO);
    until(|| !opened.is_busy(), "the demo was built");
    let preview = window.preview();
    let counts = preview.counts().expect("the demo was previewed");
    assert_eq!(
        (counts.photos, counts.change),
        (3, demo().1),
        "the row said what the preview shows"
    );
    assert_eq!(tools.showing(), "preview");

    // The header narrows at a breakpoint once the window is that narrow; the tabs cannot.
    let stack = descendants(window.upcast_ref())
        .into_iter()
        .find(|widget| widget.is::<adw::ViewStack>())
        .expect("the tabs");
    let (width, _, _, _) = stack.measure(gtk::Orientation::Horizontal, -1);
    assert!(width < 400, "the tabs need {width} px, more than a phone has");

    // Another program rates one photo of the event; after a scan the count knows it.
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
    assert_eq!(demo(), (3, 2), "the count after a scan is fresh");
    window.show_view("dashboard");
}

/// The history lists every pass newest first, by the name of what ran it, and a pass opens to
/// its photos. The passes are written into the journal by hand: no photo is touched here.
fn reads_the_history(window: &Window, opened: &Rc<Library>) {
    use photomanager_core::journal::{Entry, Journal, Kind, Swap, WRITTEN};
    let mut journal = Journal::open(&opened.paths().app_db()).unwrap();
    let mut pass = |title: &str, paths: &[&str]| {
        let batch = journal.start(Kind::Write, None, title, Some("demo-rating")).unwrap();
        for path in paths {
            let entry = Entry {
                rel_path: path.to_string(),
                content_id: "0123456789abcdef0123456789abcdef".to_string(),
                before: "{}".to_string(),
                image_hash: None,
                swaps: vec![Swap {
                    tag: "XMP-xmp:Rating".to_string(),
                    key: "XMP-xmp:Rating".to_string(),
                    old: None,
                    new: Some("3".to_string()),
                }],
            };
            let id = journal.record(batch, &entry).unwrap();
            journal.settle(id, WRITTEN, None).unwrap();
        }
        journal.finish(batch).unwrap();
        batch
    };
    let first = pass("First pass", &["a.jpg", "b.jpg"]);
    let second = pass("Second pass", &["b.jpg"]);

    window.show_view("tools");
    WidgetExt::activate_action(window, "win.show-history", None).unwrap();
    let tools = window.tools();
    assert_eq!(tools.showing(), "history");
    let passes = tools.history().passes();
    let titles: Vec<&str> = passes.iter().take(2).map(|pass| pass.title.as_str()).collect();
    assert_eq!(titles, ["Second pass", "First pass"], "newest first");
    assert_eq!(passes[0].id, second);
    assert_eq!(passes[1].changed_since, 1, "b.jpg was written again by the second pass");
    assert!(labels(tools.upcast_ref()).iter().any(|label| label == "First pass"));

    WidgetExt::activate_action(window, "win.history-details", Some(&first.to_variant())).unwrap();
    assert_eq!(tools.showing(), "pass");
    let (batch, photos) = tools.history().detail().expect("the pass was opened");
    assert_eq!(batch, first);
    assert_eq!(photos, ["XMP-xmp:Rating: none -> 3", "XMP-xmp:Rating: none -> 3"]);
    WidgetExt::activate_action(window, "win.show-history", None).unwrap();
    assert_eq!(tools.showing(), "history", "back from a pass to the list");
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
        Some(survey.photos - 22),
        "the number clicked is the number shown"
    );

    WidgetExt::activate_action(window, "win.show-photos", Some(&"no-gps@Germany".to_variant())).unwrap();
    assert_eq!(window.shown().unwrap().1, Some(4));
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
    assert_eq!(listed().len(), 4, "the grid holds exactly the photos counted");
    assert_eq!(window.shown().unwrap().1, Some(4));

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
    assert_eq!(listed().len(), all - 22);
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
    has(&texts, "Person: me");
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

    let all = page.raw_shown();
    assert!(all > 5, "every raw field is listed: {all}");
    page.filter_raw("gpslat");
    let narrowed = page.raw_shown();
    assert!(narrowed > 0 && narrowed < all, "{narrowed} of {all}");
    page.filter_raw("");
    assert_eq!(page.raw_shown(), all);

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
