use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::paths::Paths;
use photomanager_core::scan::Mode;
use photomanager_core::{APP_ID, VERSION};

use crate::library::Library;
use crate::window::Window;

pub fn build(paths: Paths) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let window = Window::new(app);
        let library = match Library::open(paths.clone()) {
            Ok(library) => Some(library),
            Err(error) => {
                tracing::error!(error, "the cache cannot be opened");
                None
            }
        };
        window.set_library(library.clone());
        setup_actions(app, &window, library.clone());
        #[cfg(feature = "devtools")]
        crate::devtools::install(app, &window, &paths, library.clone());
        window.present();
        tracing::info!(library = %paths.library().display(), "library opened");
    });

    app
}

fn setup_actions(app: &adw::Application, window: &Window, library: Option<Rc<Library>>) {
    let about = gio::ActionEntry::builder("about")
        .activate(glib::clone!(
            #[weak]
            window,
            move |_: &adw::Application, _, _| show_about(&window)
        ))
        .build();
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build();
    let rebuild = gio::ActionEntry::builder("rebuild-cache")
        .activate(glib::clone!(
            #[weak]
            window,
            move |_: &adw::Application, _, _| ask_to_rebuild(&window, library.clone())
        ))
        .build();
    app.add_action_entries([about, quit, rebuild]);
    app.set_accels_for_action("app.quit", &["<primary>q"]);
}

/// The cache is thrown away and filled again. The photos are never touched.
fn ask_to_rebuild(window: &Window, library: Option<Rc<Library>>) {
    let Some(library) = library else {
        return;
    };
    let dialog = adw::AlertDialog::new(
        Some("Rebuild the Cache?"),
        Some("Every photo is read again. The photos themselves are not changed."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("rebuild", "Rebuild")]);
    dialog.set_response_appearance("rebuild", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.connect_response(
        None,
        glib::clone!(
            #[weak]
            window,
            move |_: &adw::AlertDialog, response: &str| {
                if response != "rebuild" {
                    return;
                }
                match library.rebuild_cache() {
                    Ok(()) => {
                        window.set_library(Some(library.clone()));
                        window.scan(Mode::Reread);
                    }
                    Err(error) => tracing::error!(error, "the cache cannot be rebuilt"),
                }
            }
        ),
    );
    dialog.present(Some(window));
}

fn show_about(window: &Window) {
    adw::AboutDialog::builder()
        .application_name("Photo Manager")
        .application_icon(APP_ID)
        .version(VERSION)
        .developer_name("Daniel Koch")
        .license_type(gtk::License::Unknown)
        .comments("Manage the data of a personal photo library.")
        .build()
        .present(Some(window));
}
