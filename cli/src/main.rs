#![feature(let_chains)]

use std::fmt::Write;
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

use clap::Parser;
use duct::cmd;
use eyre::{Context, OptionExt, bail, eyre};
use saphyr::Yaml;
use which::which;
use zip::ZipArchive;

static PLUGINS_SHIM_SOURCE: &str = include_str!("../../plugins-compat/src/lib.rs");

#[derive(clap::Parser)]
enum Command {
    /// Run a flion application
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

            download_engine_artifacts(
                &flutter_program,
                &flutter_project_dir.join("build").join("flion"),
            )?;

            build_flutter_assets(&flutter_program)?;

            let flion_build_dir = flutter_project_dir.join("build").join("flion");
            let target_dir = cargo_metadata.target_directory.as_std_path().join("debug");

            if !target_dir.exists() {
                fs::create_dir_all(&target_dir)?;
            }

            copy_native_libraries(&flutter_program, flutter_project_dir, &target_dir)?;

            compile_plugins_shim(&flion_build_dir.join("plugins"), &target_dir)?;

            process_plugins(&flutter_program, flutter_project_dir, &target_dir)?;

            let embedder_path = get_engine_artifacts_dir(&flutter_program, &flion_build_dir)?
                .join("windows-x64-embedder");
            let angle_path = flion_build_dir.join("angle-win64");

            let out = cmd!("cargo", "run")
                .env("FLUTTER_EMBEDDER_PATH", embedder_path)
                .env("ANGLE_PATH", angle_path)
                .run()?;

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

fn get_engine_artifacts_dir(flutter_path: &Path, build_dir: &Path) -> eyre::Result<PathBuf> {
    let flutter_bin_dir = flutter_path.parent().unwrap();
    let engine_version_path = flutter_bin_dir.join("internal").join("engine.version");

    let engine_commit = fs::read_to_string(&engine_version_path).wrap_err_with(|| {
        eyre!("failed to read flutter engine version from {engine_version_path:?}")
    })?;

    let engine_commit = engine_commit.trim();
    let engine_artifacts_dir = build_dir.join(engine_commit);

    Ok(engine_artifacts_dir)
}

const FLUTTER_ENGINE_ARTIFACTS: &[(&str, &str)] = &[
    ("artifacts", "windows-x64/artifacts.zip"),
    (
        "windows-x64-embedder",
        "windows-x64/windows-x64-embedder.zip",
    ),
    (
        "windows-x64-flutter",
        // TODO: Should this use windows-x64-release instead? Does it matter?
        "windows-x64-debug/windows-x64-flutter.zip",
    ),
    (
        "flutter-cpp-client-wrapper",
        "windows-x64/flutter-cpp-client-wrapper.zip",
    ),
];

fn download_engine_artifacts(flutter_path: &Path, build_dir: &Path) -> eyre::Result<()> {
    let flutter_bin_dir = flutter_path.parent().unwrap();
    let engine_version_path = flutter_bin_dir.join("internal").join("engine.version");

    let engine_commit = fs::read_to_string(&engine_version_path).wrap_err_with(|| {
        eyre!("failed to read flutter engine version from {engine_version_path:?}")
    })?;

    let engine_commit = engine_commit.trim();
    let out_dir = build_dir.join(engine_commit);

    for (name, archive_name) in FLUTTER_ENGINE_ARTIFACTS {
        let path = out_dir.join(name);
        if !path.exists() {
            download_flutter_engine_artifact(engine_commit, name, archive_name, &out_dir)?;
        }
    }

    Ok(())
}

fn download_flutter_engine_artifact(
    engine_commit: &str,
    name: &str,
    archive_name: &str,
    out_dir: &Path,
) -> eyre::Result<()> {
    let url = format!(
        "https://storage.googleapis.com/flutter_infra_release/flutter/{engine_commit}/{archive_name}"
    );

    tracing::info!("downloading {name} from {url}");

    let res = ureq::get(&url).call()?;
    if !res.status().is_success() {
        bail!("downloading {name} failed with: {}", res.status());
    }

    fs::create_dir_all(out_dir)?;

    let extract_path = out_dir.join(name);
    let archive_path = extract_path.with_extension("zip");

    {
        let body = res.into_body();
        let out_file = File::create(&archive_path)
            .wrap_err_with(|| eyre!("failed to create file: {}", archive_path.display()))?;

        io::copy(&mut body.into_reader(), &mut BufWriter::new(out_file))?;
    }

    let archive = File::open(&archive_path)
        .wrap_err_with(|| eyre!("failed to open file: {}", archive_path.display()))?;

    tracing::info!("unpacking {name} to {}", extract_path.display());

    ZipArchive::new(BufReader::new(archive))?.extract(extract_path)?;

    Ok(())
}

fn build_flutter_assets(flutter_path: &Path) -> eyre::Result<()> {
    tracing::info!("running flutter build");

    let out = cmd!(flutter_path, "build", "bundle").run()?;

    if !out.status.success() {
        bail!("flutter build failed with status {}", out.status);
    }

    Ok(())
}

fn copy_native_libraries(
    flutter_path: &Path,
    flutter_project_dir: &Path,
    out_dir: &Path,
) -> eyre::Result<()> {
    let build_dir = flutter_project_dir.join("build").join("flion");
    if !build_dir.is_dir() {
        fs::create_dir_all(&build_dir)?;
    }

    let engine_artifacts_dir = get_engine_artifacts_dir(flutter_path, &build_dir)?;

    copy_if_newer(
        &engine_artifacts_dir
            .join("windows-x64-embedder")
            .join("flutter_engine.dll"),
        &out_dir.join("flutter_engine.dll"),
    )?;

    copy_if_newer(
        &engine_artifacts_dir.join("artifacts").join("icudtl.dat"),
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

fn compile_plugins_shim(build_dir: &Path, out_dir: &Path) -> eyre::Result<()> {
    fs::create_dir_all(build_dir)?;

    let lib_path = build_dir.join("flion_plugins_shim.dll");

    if !lib_path.exists() {
        cmd!(
            "rustc",
            "-",
            "--crate-type",
            "cdylib",
            "--crate-name",
            "plugins_shim",
            "-o",
            &lib_path,
        )
        .stdin_bytes(PLUGINS_SHIM_SOURCE)
        .run()?;
    }

    copy_if_newer(&lib_path, &out_dir.join("flion_plugins_shim.dll"))?;

    Ok(())
}

fn process_plugins(
    flutter_path: &Path,
    flutter_project_dir: &Path,
    out_dir: &Path,
) -> eyre::Result<()> {
    let plugins_path = flutter_project_dir.join(".flutter-plugins-dependencies");
    if !plugins_path.exists() {
        return Ok(());
    }

    let plugins: serde_json::Value = serde_json::from_str(&fs::read_to_string(&plugins_path)?)?;
    let plugins = plugins["plugins"]["windows"]
        .as_array()
        .into_iter()
        .flatten();

    let plugins_build_dir = flutter_project_dir
        .join("build")
        .join("flion")
        .join("plugins");

    if !plugins_build_dir.is_dir() {
        fs::create_dir_all(&plugins_build_dir)?;
    }

    let flutter_bin_dir = flutter_path.parent().unwrap();
    let engine_version_path = flutter_bin_dir.join("internal").join("engine.version");

    let engine_commit = fs::read_to_string(&engine_version_path).wrap_err_with(|| {
        eyre!("failed to read flutter engine version from {engine_version_path:?}")
    })?;

    let engine_commit = engine_commit.trim();
    let engine_artifacts_dir = flutter_project_dir
        .join("build")
        .join("flion")
        .join(engine_commit);

    let flutter_engine_artifacts_link = plugins_build_dir.join("flutter");
    if !flutter_engine_artifacts_link.exists() {
        std::os::windows::fs::symlink_dir(&engine_artifacts_dir, &flutter_engine_artifacts_link)?;
    }

    let mut plugin_names = vec![];
    let mut plugins_list = String::new();

    for plugin in plugins {
        let name = plugin["name"].as_str().unwrap();
        let path = plugin["path"].as_str().unwrap();

        let plugin_pubspec = fs::read_to_string(Path::new(path).join("pubspec.yaml"))?;
        let plugin_pubspec = Yaml::load_from_str(&plugin_pubspec)?;
        let plugin_pubspec = &plugin_pubspec[0];

        let platforms = &plugin_pubspec["flutter"]["plugin"]["platforms"];

        if let Some(platforms) = platforms.as_hash()
            && let Some(platform) = platforms.get(&Yaml::from_str("windows"))
        {
            if platforms.contains_key(&Yaml::from_str("flion")) {
                // TODO: Figure out flion plugins
                continue;
            }

            let plugin_class = platform["pluginClass"].as_str();
            let ffi_plugin = platform["ffiPlugin"].as_bool().unwrap_or(false);

            if plugin_class.is_some() || ffi_plugin {
                tracing::info!("processing plugin: {name} {path}");

                let link_path = plugins_build_dir.join(name);
                if !link_path.exists() {
                    std::os::windows::fs::symlink_dir(path, &link_path)?;
                }

                plugin_names.push(name);

                // writeln!(cmake, "add_subdirectory(\"{name}/windows\")")?;

                if let Some(plugin_class) = plugin_class {
                    writeln!(plugins_list, "{name},{plugin_class}")?;
                }
            }
        }
    }

    fs::write(
        plugins_build_dir.join("CMakeLists.txt"),
        include_str!("CMakeLists.txt"),
    )?;

    fs::write(plugins_build_dir.join("plugins.txt"), plugins_list)?;

    cmake::Config::new(&plugins_build_dir)
        .host("x86_64-pc-windows-msvc")
        .target("x86_64-pc-windows-msvc")
        .profile("Debug") // TODO: Release mode
        .no_build_target(true)
        .out_dir(&plugins_build_dir)
        .define("FLUTTER_PLUGINS", plugin_names.join(";"))
        .build();

    // TODO: Release mode
    cmd!("cmake", "--install", ".", "--config", "Debug")
        .dir(plugins_build_dir.join("build"))
        .run()?;

    for lib in std::fs::read_dir(plugins_build_dir.join("bin"))? {
        let lib = lib?;
        let src = lib.path();
        let dest = out_dir.join(lib.file_name());
        copy_if_newer(&src, &dest)?;
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
