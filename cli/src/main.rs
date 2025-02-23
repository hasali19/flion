use std::path::{Path, PathBuf};
use std::{env, fs, process};

use clap::Parser;
use duct::cmd;
use eyre::{Context, OptionExt, bail, eyre};
use which::which;

#[derive(clap::Parser)]
enum Command {
    /// Run a fluyt application
    Run,
}

fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let command = Command::parse();

    match command {
        Command::Run => {
            #[cfg(target_os = "windows")]
            let flutter_program = "flutter.bat";

            #[cfg(not(target_os = "windows"))]
            let flutter_program = "flutter";

            let flutter_program = which(flutter_program)?;
            let cargo_manifest = find_manifest_path()?;
            let pubspec = find_pubspec_path()?;
            let flutter_project_dir = pubspec.parent().unwrap();

            let cargo_metadata = get_cargo_metadata(&cargo_manifest)?;

            let out = cmd!(&flutter_program, "build", "bundle").run()?;
            if !out.status.success() {
                bail!("flutter build failed with status {}", out.status);
            }

            copy_native_libraries(
                &flutter_program,
                flutter_project_dir,
                &cargo_metadata.target_directory.as_std_path().join("debug"),
            )?;

            let out = cmd!("cargo", "run").run()?;

            if let Some(code) = out.status.code() {
                process::exit(code);
            }
        }
    }

    Ok(())
}

fn find_manifest_path() -> eyre::Result<PathBuf> {
    let dir = env::current_dir()?;
    let mut dir = dir.as_path();
    loop {
        let path = dir.join("Cargo.toml");
        if path.exists() {
            return Ok(path);
        }
        dir = dir.parent().ok_or_eyre("cargo manifest not found")?;
    }
}

fn find_pubspec_path() -> eyre::Result<PathBuf> {
    let dir = env::current_dir()?;
    let mut dir = dir.as_path();
    loop {
        let path = dir.join("pubspec.yaml");
        if path.exists() {
            return Ok(path);
        }
        dir = dir.parent().ok_or_eyre("pubspec not found")?;
    }
}

fn get_cargo_metadata(manifest: &Path) -> eyre::Result<cargo_metadata::Metadata> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(manifest)
        .exec()?;

    Ok(metadata)
}

#[cfg(target_os = "windows")]
fn copy_native_libraries(
    flutter_path: &Path,
    flutter_project_dir: &Path,
    out_dir: &Path,
) -> eyre::Result<()> {
    use std::fs::{self, File};
    use std::io::{self, BufReader, BufWriter};

    use eyre::{Context, eyre};
    use zip::ZipArchive;

    let build_dir = flutter_project_dir.join("build").join("fluyt");
    if !build_dir.is_dir() {
        fs::create_dir_all(&build_dir)?;
    }

    let flutter_bin_dir = flutter_path.parent().unwrap();
    let engine_version_path = flutter_bin_dir.join("internal").join("engine.version");

    let engine_commit = fs::read_to_string(&engine_version_path).wrap_err_with(|| {
        eyre!("failed to read flutter engine version from {engine_version_path:?}")
    })?;

    let engine_commit = engine_commit.trim();

    let embedder_archive_path = build_dir
        .join(engine_commit)
        .join("windows-x64-embedder.zip");

    let embedder_extract_path = build_dir.join(engine_commit).join("windows-x64-embedder");

    if !embedder_archive_path.exists() {
        let url = format!(
            "https://storage.googleapis.com/flutter_infra_release/flutter/{engine_commit}/windows-x64/windows-x64-embedder.zip"
        );

        tracing::info!("downloading flutter engine from {url}");

        let res = ureq::get(&url).call()?;
        if !res.status().is_success() {
            bail!(
                "downloading flutter engine failed with status {}",
                res.status()
            );
        }

        fs::create_dir_all(embedder_archive_path.parent().unwrap())?;

        let body = res.into_body();
        let out_file = File::create(&embedder_archive_path)?;

        io::copy(&mut body.into_reader(), &mut BufWriter::new(out_file))?;

        if embedder_extract_path.exists() {
            fs::remove_dir_all(&embedder_extract_path)?
        }

        tracing::info!("unpacking flutter engine to {embedder_extract_path:?}");

        ZipArchive::new(BufReader::new(File::open(&embedder_archive_path)?))?
            .extract(&embedder_extract_path)?;
    }

    copy_if_newer(
        &embedder_extract_path.join("flutter_engine.dll"),
        &out_dir.join("flutter_engine.dll"),
    )?;

    copy_if_newer(
        &flutter_bin_dir
            .join("cache")
            .join("artifacts")
            .join("engine")
            .join("windows-x64")
            .join("icudtl.dat"),
        &out_dir.join("icudtl.dat"),
    )?;

    let angle_version = "2024-10-05-23-15";
    let angle_archive_name = format!("angle-win64-{angle_version}.tar.gz");
    let angle_archive_path = build_dir.join(angle_archive_name);
    let angle_extract_path = build_dir.join("angle-win64");

    if !angle_archive_path.exists() {
        let url = format!(
            "https://github.com/hasali19/angle-build/releases/download/build-{angle_version}/angle-win64.tar.gz"
        );

        tracing::info!("downloading angle from {url}");

        let res = ureq::get(&url).call()?;
        if !res.status().is_success() {
            bail!("downloading angle failed with status {}", res.status());
        }

        let body = res.into_body();
        let out_file = File::create(&angle_archive_path)?;

        io::copy(&mut body.into_reader(), &mut BufWriter::new(out_file))?;

        if angle_extract_path.exists() {
            fs::remove_dir_all(&angle_extract_path)?
        }

        tracing::info!("unpacking angle to {angle_extract_path:?}");

        cmd!("tar", "xf", &angle_archive_path, "-C", &build_dir).run()?;
    }

    for lib in ["libEGL.dll", "libGLESv2.dll"] {
        let src_path = angle_extract_path.join("bin").join(lib);
        let dst_path = out_dir.join(lib);
        copy_if_newer(&src_path, &dst_path)?;
    }

    Ok(())
}

fn copy_if_newer(src: &Path, dst: &Path) -> eyre::Result<()> {
    if dst.exists() {
        let src_metadata = src.metadata()?;
        let dst_metadata = dst.metadata()?;
        if src_metadata.modified()? <= dst_metadata.modified()? {
            return Ok(());
        }
    }

    tracing::info!("copying {} to {}", src.display(), dst.display());

    fs::copy(src, dst)
        .wrap_err_with(|| eyre!("failed to copy {} to {}", src.display(), dst.display()))?;

    Ok(())
}
