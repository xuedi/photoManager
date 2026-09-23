//! Actions that let a test drive the window from outside the process. Compiled only with the
//! `devtools` feature.

use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::changeset::Wanted;
use photomanager_core::paths::Paths;
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
/// tools are still to come.
fn demo_change_set(window: &Window) {
    let tools = window.tools();
    let Some(library) = window.library() else {
        return;
    };
    let wanted: Vec<Wanted> = library
        .photo_paths(DEMO_PHOTOS)
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
    serde_json::json!({
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
