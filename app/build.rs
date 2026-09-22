use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/ui");
    println!("cargo:rerun-if-changed=src/photomanager.gresource.xml");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("resources");
    std::fs::create_dir_all(&out).expect("create the resource directory");
    compile_blueprints(Path::new("src/ui"), &out);

    let manifest = out.join("photomanager.gresource.xml");
    std::fs::copy("src/photomanager.gresource.xml", &manifest).expect("copy the resource manifest");
    glib_build_tools::compile_resources(&[&out], manifest.to_str().unwrap(), "photomanager.gresource");
}

fn compile_blueprints(src: &Path, out: &Path) {
    let files: Vec<PathBuf> = std::fs::read_dir(src)
        .expect("read src/ui")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "blp"))
        .collect();

    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(out)
        .arg(src)
        .args(&files)
        .status()
        .expect("run blueprint-compiler, is it installed?");
    assert!(status.success(), "blueprint-compiler failed");
}
