use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Write as _};
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
    let mut consts = String::new();

    let plugins_file = BufReader::new(File::open(plugins_file)?);
    for line in plugins_file.lines().map_while(Result::ok) {
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

        writeln!(consts, "{class_name}RegisterWithRegistrar,")?;
    }

    let mut plugin_registrant = File::create(out_dir.join("plugin_registrant.rs"))?;

    writeln!(plugin_registrant, "{}", externs)?;
    writeln!(
        plugin_registrant,
        "static PLUGINS: &[unsafe extern \"C\" fn(*mut std::ffi::c_void)] = &["
    )?;
    writeln!(plugin_registrant, "{}", consts)?;
    writeln!(plugin_registrant, "];")?;

    Ok(())
}
