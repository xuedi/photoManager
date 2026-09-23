pub mod application;
pub mod confirm;
pub mod dashboard;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod edit;
pub mod gallery;
pub mod library;
pub mod panel;
pub mod photo;
pub mod preview;
pub mod thumbnails;
pub mod tools;
pub mod window;

use gtk::gio;

pub fn register_resources() {
    gio::resources_register_include!("photomanager.gresource").expect("register the resources");
}
