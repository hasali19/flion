use std::env;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=FLUTTER_EMBEDDER_PATH");

    if let Ok(embedder_path) = env::var("FLUTTER_EMBEDDER_PATH") {
        println!("cargo::rustc-link-search=native={embedder_path}");
        println!("cargo::rustc-link-lib=dylib=flutter_engine.dll");
    }

    Ok(())
}
