fn main() {
    let build = dunce::canonicalize("build").unwrap();
    let angle_lib = build.join("angle-win64/lib");

    println!("cargo:rustc-link-search=native={}", angle_lib.display());
    println!("cargo:rustc-link-lib=dylib=libEGL.dll");
    println!("cargo:rustc-link-lib=dylib=libGLESv2.dll");
}
