use std::fs;
use std::path::{Path, PathBuf};

use crate::cargo::{BuildOptions, Cargo};

static PLUGINS_SHIM_MANIFEST: &str = include_str!("plugins_shim.manifest.toml");
static PLUGINS_SHIM_SOURCE: &str = include_str!("../../plugins-compat/src/lib.rs");

pub struct PluginsShimCompiler<'a> {
    cargo: &'a Cargo,
    build_dir: PathBuf,
}

impl<'a> PluginsShimCompiler<'a> {
    pub fn new(cargo: &Cargo, build_dir: PathBuf) -> PluginsShimCompiler<'_> {
        PluginsShimCompiler { cargo, build_dir }
    }

    pub fn compile(&self) -> eyre::Result<()> {
        let manifest_path = self.build_dir.join("Cargo.toml");
        if fs::read_to_string(&manifest_path).unwrap_or_default() != PLUGINS_SHIM_MANIFEST {
            fs::write(manifest_path, PLUGINS_SHIM_MANIFEST)?;
        }

        let src_path = self.build_dir.join("src").join("lib.rs");
        if fs::read_to_string(&src_path).unwrap_or_default() != PLUGINS_SHIM_SOURCE {
            fs::create_dir_all(src_path.parent().unwrap())?;
            fs::write(src_path, PLUGINS_SHIM_SOURCE)?;
        }

        self.cargo
            .build(
                &self.build_dir,
                Path::new("build"),
                "build",
                BuildOptions {
                    rustflags: "--cfg cdylib",
                },
            )
            .stdin_bytes(PLUGINS_SHIM_SOURCE)
            .run()?;

        Ok(())
    }
}
