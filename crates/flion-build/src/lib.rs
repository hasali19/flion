use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Write as _};
use std::path::{Path, PathBuf};

pub fn generate_plugins_registrant(project_dir: &Path) -> Result<(), Box<dyn Error>> {
    let build_dir = project_dir.join("build/flion").canonicalize()?;
    let plugins_dir = build_dir.join("plugins");
    let plugins_lib_dir = plugins_dir.join("lib");

    println!("cargo::rustc-link-search=native={}", plugins_dir.display());
    println!(
        "cargo::rustc-link-search=native={}",
        plugins_lib_dir.display()
    );

    let plugins_file = plugins_dir.join("plugins.txt");
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    println!("cargo::rerun-if-changed={}", plugins_file.display());

    let mut externs = String::new();
    let mut statics = String::new();

    for line in read_plugins(&plugins_file)? {
        let Some((name, class_name)) = line.split_once(',') else {
            continue;
        };

        writeln!(
            externs,
            "\
#[link(name = \"{name}_plugin\")]
unsafe extern \"C\" {{
    fn {class_name}RegisterWithRegistrar(registrar: *mut std::ffi::c_void);
}}"
        )?;

        writeln!(statics, "#[allow(non_upper_case_globals)]")?;
        writeln!(
            statics,
            "#[flion::linkme::distributed_slice(flion::PLUGINS)] #[linkme(crate = flion::linkme)] static {class_name}RegisterWithRegistrarPtr: unsafe extern \"C\" fn(*mut std::ffi::c_void) = {class_name}RegisterWithRegistrar;"
        )?;
    }

    let mut plugin_registrant = File::create(out_dir.join("plugin_registrant.rs"))?;

    writeln!(plugin_registrant, "const _: () = {{")?;
    writeln!(plugin_registrant, "{}", externs)?;
    writeln!(plugin_registrant, "{}", statics)?;
    writeln!(plugin_registrant, "}};")?;

    Ok(())
}

fn read_plugins(filename: &Path) -> Result<Box<dyn Iterator<Item = String>>, Box<dyn Error>> {
    let plugins_file = match File::open(filename) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Box::new(std::iter::empty())),
        Err(e) => return Err(Box::new(e)),
    };

    let plugins_file = BufReader::new(plugins_file);

    Ok(Box::new(plugins_file.lines().map_while(Result::ok)))
}
