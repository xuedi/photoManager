pub mod application;
pub mod confirm;
pub mod dashboard;
#[cfg(feature = "devtools")]
pub mod devtools;
pub mod edit;
pub mod gallery;
pub mod history;
pub mod library;
pub mod panel;
pub mod photo;
pub mod preferences;
pub mod preview;
pub mod questions;
pub mod secrets;
pub mod thumbnails;
pub mod tools;
pub mod vocabulary;
pub mod window;

use gtk::gio;

pub fn register_resources() {
    gio::resources_register_include!("photomanager.gresource").expect("register the resources");
}
