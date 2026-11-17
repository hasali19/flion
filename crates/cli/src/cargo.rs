use std::env;
use std::path::{Path, PathBuf};

use duct::cmd;
use tracing::info;

use crate::BuildMode;

pub struct Cargo {
    build_mode: BuildMode,
    target_dir: PathBuf,
}

#[derive(Default)]
pub struct BuildOptions<'a> {
    pub rustflags: &'a str,
}

impl Cargo {
    pub fn new(build_mode: BuildMode, target_dir: PathBuf) -> Cargo {
        Cargo {
            build_mode,
            target_dir,
        }
    }

    pub fn build(
        &self,
        dir: &Path,
        build_dir: &Path,
        cmd: &str,
        options: BuildOptions,
    ) -> duct::Expression {
        let mut cmd = cmd!(
            "cargo",
            cmd,
            "--profile",
            self.build_mode.cargo_profile(),
            "--target",
            "x86_64-pc-windows-msvc",
        )
        .dir(dir)
        .env("CARGO_BUILD_BUILD_DIR", build_dir)
        .env("CARGO_TARGET_DIR", &self.target_dir);

        if !cfg!(windows) {
            let mut linker_search_paths = vec![
                self.target_dir
                    .join("x86_64-pc-windows-msvc")
                    .join(self.build_mode.name()),
            ];

            if let Ok(win_sdk) = env::var("FLION_WINDOWS_SDK") {
                let win_sdk = Path::new(&win_sdk);
                linker_search_paths.extend([
                    win_sdk.join("crt/lib/x86_64"),
                    win_sdk.join("sdk/lib/um/x86_64"),
                    win_sdk.join("sdk/lib/ucrt/x86_64"),
                ]);
            }

            let mut rustflags = String::new();
            for path in linker_search_paths {
                rustflags += &format!("-Lnative={} ", path.display());
            }

            rustflags += options.rustflags;

            info!("RUSTFLAGS={rustflags}");

            cmd = cmd
                .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER", "lld-link")
                .env("RUSTFLAGS", rustflags);
        }

        cmd
    }
}
