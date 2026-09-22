use adw::prelude::*;
use gtk::glib;
use photomanager::{application, register_resources};
use photomanager_core::paths::Paths;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(error) => {
            tracing::error!(%error, "cannot start");
            return glib::ExitCode::FAILURE;
        }
    };

    register_resources();
    application::build(paths).run()
}
