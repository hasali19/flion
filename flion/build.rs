use std::env;
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=ANGLE_PATH");

    let target = env::var("TARGET")?;

    if let Ok(angle_path) = env::var("ANGLE_PATH") {
        let angle_lib_dir = PathBuf::from(angle_path).join("lib");

        println!("cargo:rustc-link-search=native={}", angle_lib_dir.display());

        if target.contains("windows") {
            println!("cargo:rustc-link-lib=dylib=libEGL.dll");
            println!("cargo:rustc-link-lib=dylib=libGLESv2.dll");
        }
    }

    Ok(())
}
