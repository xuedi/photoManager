pub mod application;
pub mod dashboard;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod library;
pub mod window;

use gtk::gio;

pub fn register_resources() {
    gio::resources_register_include!("photomanager.gresource").expect("register the resources");
}
