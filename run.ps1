Set-Location .\example
flutter build bundle
Set-Location ..

New-Item -ItemType Directory -ErrorAction SilentlyContinue .\build
New-Item -ItemType Directory -ErrorAction SilentlyContinue .\target
New-Item -ItemType Directory -ErrorAction SilentlyContinue .\target\debug

$flutter_engine_commit = "9aa7816315095c86410527932918c718cb35e7d6"

if (!(Test-Path ".\build\windows-x64-embedder.zip")) {
    Invoke-WebRequest "https://storage.googleapis.com/flutter_infra_release/flutter/$flutter_engine_commit/windows-x64/windows-x64-embedder.zip" -OutFile ".\build\windows-x64-embedder.zip"
}

if (!(Test-Path ".\build\windows-x64-embedder")) {
    Expand-Archive ".\build\windows-x64-embedder.zip" -DestinationPath ".\build\windows-x64-embedder"
}

if (!(Test-Path ".\target\debug\flutter_engine.dll")) {
    Copy-Item ".\build\windows-x64-embedder\flutter_engine.dll" ".\target\debug\flutter_engine.dll"
}

$angle_version = "2023-04-01-23-12"
$extract_angle = $false

if (!(Test-Path ".\build\angle-win64-$angle_version.tar.gz")) {
    Invoke-WebRequest "https://github.com/hasali19/angle-build/releases/download/build-$angle_version/angle-win64.tar.gz" -OutFile ".\build\angle-win64-$angle_version.tar.gz"
    $extract_angle = $true
}

if ($extract_angle -or !(Test-Path ".\build\angle-win64")) {
    Remove-Item -Recurse ".\build\angle-win64" -ErrorAction SilentlyContinue
    tar -xvzf ".\build\angle-win64-$angle_version.tar.gz" -C ".\build"
    Copy-Item ".\build\angle-win64\bin\libEGL.dll" ".\target\debug\libEGL.dll"
    Copy-Item ".\build\angle-win64\bin\libGLESv2.dll" ".\target\debug\libGLESv2.dll"
}

if (!(Test-Path .\target\debug\icudtl.dat)) {
    $flutter_exe = (Get-Command flutter).Path
    $flutter_bin = Split-Path $flutter_exe

    Copy-Item (Join-Path $flutter_bin "cache\artifacts\engine\windows-x64\icudtl.dat") .\target\debug -ErrorAction Stop
}

cargo run
