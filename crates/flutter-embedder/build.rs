use std::env;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=FLUTTER_EMBEDDER_PATH");

    if let Ok(embedder_path) = env::var("FLUTTER_EMBEDDER_PATH") {
        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();

        if target_os == "windows" {
            println!("cargo::rustc-link-search=native={embedder_path}");
            println!("cargo::rustc-link-lib=dylib=flutter_engine.dll");
        }
    }

    Ok(())
}
