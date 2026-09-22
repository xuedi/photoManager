pub mod identity;
pub mod layout;
pub mod metadata;
pub mod paths;

#[cfg(feature = "fixtures")]
pub mod fixtures;

pub const APP_ID: &str = "org.beijingcode.PhotoManager";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
