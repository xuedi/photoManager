//! Serves the stand-in Immich over a stand-in library, to look at the people tool without the
//! real one: `just fake-immich`. Prints its address and key, and serves until it is stopped.

use std::path::PathBuf;

use photomanager_core::immich::fake::{Data, FakeImmich, KEY};

fn main() {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: fake_immich <stand-in library>");
        std::process::exit(2);
    };
    let immich = FakeImmich::serve(Data::over(&root));
    println!("{} {KEY}", immich.url);
    loop {
        std::thread::park();
    }
}
