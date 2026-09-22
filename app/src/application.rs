use adw::prelude::*;
use gtk::{gio, glib};
use photomanager_core::paths::Paths;
use photomanager_core::{APP_ID, VERSION};

use crate::window::Window;

pub fn build(paths: Paths) -> adw::Application {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let window = Window::new(app);
        setup_actions(app, &window);
        window.present();
        tracing::info!(library = %paths.library().display(), "library opened");
    });

    app
}

fn setup_actions(app: &adw::Application, window: &Window) {
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
    app.add_action_entries([about, quit]);
    app.set_accels_for_action("app.quit", &["<primary>q"]);
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
