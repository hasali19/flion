use bindgen::CargoCallbacks;

fn main() {
    let build = dunce::canonicalize("../build").unwrap();

    let embedder = build.join("windows-x64-embedder");
    let embedder_header = embedder.join("flutter_embedder.h");

    bindgen::builder()
        .header(embedder_header.to_str().unwrap())
        .parse_callbacks(Box::new(CargoCallbacks))
        .derive_default(true)
        .generate()
        .unwrap()
        .write_to_file("src/bindings.rs")
        .unwrap();

    println!("cargo:rustc-link-search=native={}", embedder.display());
    println!("cargo:rustc-link-lib=dylib=flutter_engine.dll");
}
