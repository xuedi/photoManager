pub mod browse;
pub mod cache;
pub mod changeset;
pub mod clock;
pub mod filter;
pub mod geo;
pub mod identity;
pub mod journal;
pub mod layout;
pub mod metadata;
pub mod paths;
pub mod scan;
pub mod scope;
pub mod settings;
pub mod survey;
pub mod thumbs;
pub mod write;

#[cfg(feature = "fixtures")]
pub mod fixtures;

pub const APP_ID: &str = "org.beijingcode.PhotoManager";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
