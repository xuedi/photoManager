//! Actions that let a test drive the window from outside the process. Compiled only with the
//! `devtools` feature.

use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::changeset::Wanted;
use photomanager_core::paths::Paths;
use photomanager_core::scope::Scope;
use photomanager_core::write::{Change, Field};
use std::path::Path;
use std::rc::Rc;

use crate::library::Library;
use crate::window::Window;

/// How many photos the demo change set is about.
const DEMO_PHOTOS: usize = 5;
/// What it would set on them. A rating is the smallest real change there is.
const DEMO_RATING: i64 = 3;

pub fn install(app: &adw::Application, window: &Window, paths: &Paths, library: Option<Rc<Library>>) {
    let dump_state = gio::ActionEntry::builder("dump-state")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(glib::clone!(
            #[weak]
            window,
            #[strong]
            paths,
            move |_: &adw::Application, _, parameter| {
                let target = parameter.and_then(|value| value.str()).unwrap_or_default();
                write_out(target, &state(&window, &paths, library.as_deref()));
            }
        ))
        .build();

    let snapshot = gio::ActionEntry::builder("snapshot")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(glib::clone!(
            #[weak]
            window,
            move |_: &adw::Application, _, parameter| {
                let Some(target) = parameter.and_then(|value| value.str()) else {
                    return;
                };
                if let Err(error) = snapshot_to_png(&window, Path::new(target)) {
                    tracing::error!(%error, "snapshot failed");
                }
            }
        ))
        .build();

    app.add_action_entries([dump_state, snapshot]);

    let demo = gio::ActionEntry::builder("preview-demo")
        .activate(|window: &Window, _, _| demo_change_set(window))
        .build();
    window.add_action_entries([demo]);
}

/// A change set without a tool behind it, so the preview and the apply can be driven while the
/// tools are still to come. It works on the scope when there is one, else on the first photos.
fn demo_change_set(window: &Window) {
    let tools = window.tools();
    let Some(library) = window.library() else {
        return;
    };
    let paths = match window.scope() {
        Some(scope) => library.scope_paths(&scope),
        None => library.photo_paths(DEMO_PHOTOS),
    };
    let wanted: Vec<Wanted> = paths
        .into_iter()
        .map(|rel_path| Wanted::new(rel_path, Change::of([Field::Rating(Some(DEMO_RATING))])))
        .collect();
    tools.preview_change_set(&format!("Set a rating of {DEMO_RATING}"), wanted);
}

fn state(window: &Window, paths: &Paths, library: Option<&Library>) -> String {
    let counts = library.map(|library| library.counts()).unwrap_or_default();
    let preview = window.preview();
    let previewed = preview.counts().map(|counts| {
        serde_json::json!({
            "photos": counts.photos,
            "change": counts.change,
            "nothing": counts.nothing,
            "refused": counts.refused,
            "written": counts.written,
            "failed": counts.failed,
            "selected": counts.selected,
            "traffic": counts.traffic,
            "asked": preview.asked(),
        })
    });
    let applied = preview.applied().map(|(kind, summary)| {
        serde_json::json!({
            "kind": kind.as_str(),
            "batch": summary.batch,
            "written": summary.written,
            "skipped": summary.skipped,
            "refused": summary.refused,
            "failed": summary.failed,
            "cancelled": summary.cancelled,
        })
    });
    let survey = library.and_then(|library| library.survey()).map(|survey| {
        let coverage: serde_json::Map<String, serde_json::Value> = survey
            .coverage
            .iter()
            .map(|(gap, measure)| {
                (
                    gap.key().to_string(),
                    serde_json::json!({ "of": measure.of, "missing": measure.missing }),
                )
            })
            .collect();
        serde_json::json!({
            "photos": survey.photos,
            "events": survey.events,
            "bytes": survey.bytes,
            "first": survey.first,
            "last": survey.last,
            "cameras": survey.camera_count(),
            "coverage": coverage,
            "countries": survey.countries.iter().map(|place| place.name.clone()).collect::<Vec<_>>(),
            "tidy": survey.tidy.iter().map(|finding| serde_json::json!({
                "title": finding.title,
                "count": finding.count,
                "filter": finding.filter.to_string(),
            })).collect::<Vec<_>>(),
        })
    });
    let dashboard = window.dashboard();
    let shown = window.gallery();
    let gallery = window.shown().map(|(filter, count)| {
        serde_json::json!({
            "filter": filter.to_string(),
            "title": filter.title(),
            "count": count,
            "sort": shown.order().key(),
            "page": shown.page(),
            "loading": shown.is_loading(),
            "listed": shown.listed().len(),
            "pictures": shown.pictures(),
            "kept": shown.kept(),
            "selected": shown.selected(),
            "chips": shown.chips(),
            "toast": shown.toast(),
        })
    });
    let page = shown.photo();
    let photo = shown.photo_open().then(|| {
        serde_json::json!({
            "path": page.path(),
            "position": page.position(),
            "count": page.count(),
            "full": page.full_size().map(|(width, height)| serde_json::json!({ "width": width, "height": height })),
            "failed": page.full_failed(),
            "held": page.held(),
            "picture": page.shows_picture(),
            "panel": page.shows_panel(),
            "map": page.shows_map(),
            "toast": page.toast(),
            "editing": page.editing(),
            "pending": page.pending().map(|pending| match pending {
                Ok(fields) => serde_json::json!(fields),
                Err(why) => serde_json::json!({ "refused": why }),
            }),
            "review": page.review_lines(),
            "applied": page.applied().map(|(kind, summary)| serde_json::json!({
                "kind": kind.as_str(),
                "batch": summary.batch,
                "written": summary.written,
            })),
            "details": page.details().map(|details| serde_json::json!({
                "taken_at": details.taken_at,
                "offset": details.taken_offset,
                "gps": details.gps.map(|(lat, lon)| [lat, lon]),
                "tags": details.tags,
                "rating": details.rating,
                "content_id": details.content_id,
            })),
        })
    });
    let scope = window.scope().map(|scope| match scope {
        Scope::Filter(filter) => serde_json::json!({ "filter": filter.to_string(), "title": filter.title() }),
        Scope::Photos { title, paths } => serde_json::json!({ "title": title, "paths": paths }),
    });
    serde_json::json!({
        "survey": survey,
        "field": dashboard.field().key(),
        "listed": dashboard.listed_places(),
        "gallery": gallery,
        "photo": photo,
        "scope": scope,
        "page": window.tools().showing(),
        "preview": previewed,
        "applied": applied,
        "toast": preview.toast(),
        "writing": library.map(|library| library.is_busy()).unwrap_or(false),
        "version": photomanager_core::VERSION,
        "library": paths.library().display().to_string(),
        "view": window.visible_view(),
        "width": window.width(),
        "height": window.height(),
        "photos": counts.photos,
        "events": counts.events,
        "issues": counts.issues,
        "thumbnails": counts.thumbnails,
        "places": counts.places,
        "scanning": library.map(|library| library.is_scanning()).unwrap_or(false),
    })
    .to_string()
}

fn write_out(target: &str, text: &str) {
    if target.is_empty() {
        println!("{text}");
        return;
    }
    if let Err(error) = std::fs::write(target, text) {
        tracing::error!(%error, target, "cannot write the state");
    }
}

fn snapshot_to_png(window: &Window, target: &Path) -> Result<(), glib::BoolError> {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let width = window.width().max(1);
    let height = window.height().max(1);

    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let Some(node) = snapshot.to_node() else {
        return Err(glib::bool_error!("the window rendered nothing"));
    };
    let renderer = window
        .native()
        .and_then(|native| native.renderer())
        .ok_or_else(|| glib::bool_error!("the window has no renderer"))?;

    renderer.render_texture(&node, None).save_to_png(target)
}
