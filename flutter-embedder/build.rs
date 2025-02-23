use std::env;
use std::error::Error;
use std::path::PathBuf;

use bindgen::CargoCallbacks;

fn main() -> Result<(), Box<dyn Error>> {
    let embedder_path = match env::var("FLUTTER_EMBEDDER_PATH") {
        Ok(embedder_path) => embedder_path,
        Err(e) => {
            println!("cargo::error=FLUTTER_EMBEDDER_PATH must be set");
            return Err(Box::new(e));
        }
    };

    let embedder_path = PathBuf::from(embedder_path);
    let embedder_header = embedder_path.join("flutter_embedder.h");

    bindgen::builder()
        .header(embedder_header.to_str().unwrap())
        .parse_callbacks(Box::new(CargoCallbacks))
        .derive_default(true)
        .generate()
        .unwrap()
        .write_to_file("src/bindings.rs")
        .unwrap();

    println!("cargo::rerun-if-env-changed=FLUTTER_EMBEDDER_PATH");
    println!(
        "cargo::rustc-link-search=native={}",
        embedder_path.display()
    );
    println!("cargo::rustc-link-lib=dylib=flutter_engine.dll");

    Ok(())
}
