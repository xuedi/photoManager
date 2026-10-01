pub mod aside;
pub mod browse;
pub mod cache;
pub mod changeset;
pub mod checks;
pub mod clock;
pub mod dates;
pub mod details;
pub mod edits;
pub mod filter;
pub mod fixes;
pub mod geo;
pub mod identity;
pub mod immich;
pub mod layout;
pub mod metadata;
pub mod names;
pub mod paths;
pub mod redundant;
pub mod remedy;
pub mod roles;
pub mod scan;
pub mod scope;
pub mod settings;
pub mod survey;
pub mod tags;
pub mod thumbs;
pub mod tools;
pub mod upkeep;
pub mod write;

#[cfg(feature = "fixtures")]
pub mod fixtures;

pub const APP_ID: &str = "org.beijingcode.PhotoManager";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
