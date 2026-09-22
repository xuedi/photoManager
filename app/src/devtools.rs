//! Actions that let a test drive the window from outside the process. Compiled only with the
//! `devtools` feature.

use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::paths::Paths;
use std::path::Path;
use std::rc::Rc;

use crate::library::Library;
use crate::window::Window;

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
}

fn state(window: &Window, paths: &Paths, library: Option<&Library>) -> String {
    let counts = library.map(|library| library.counts()).unwrap_or_default();
    serde_json::json!({
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
