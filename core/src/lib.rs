pub mod cache;
pub mod geo;
pub mod identity;
pub mod layout;
pub mod metadata;
pub mod paths;
pub mod scan;
pub mod thumbs;

#[cfg(feature = "fixtures")]
pub mod fixtures;

pub const APP_ID: &str = "org.beijingcode.PhotoManager";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
