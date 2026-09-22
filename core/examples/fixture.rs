//! Writes the stand-in library used for development and tests: `just fixture`.

use std::path::PathBuf;

fn main() {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: fixture <directory>");
        std::process::exit(2);
    };
    match photomanager_core::fixtures::build(&root) {
        Ok(()) => println!(
            "{} photos in {}",
            photomanager_core::fixtures::photo_count(),
            root.display()
        ),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
