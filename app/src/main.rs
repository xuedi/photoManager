use photomanager_core::paths::Paths;

fn main() {
    let paths = Paths::from_env().unwrap();
    println!("{}", paths.library().display());
}
