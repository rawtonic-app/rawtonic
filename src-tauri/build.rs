use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

fn verify_sha256(path: &Path, expected_hash: &str) -> Result<bool, io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hash_bytes = hasher.finalize();
    let calculated_hash = hex::encode(hash_bytes);
    Ok(calculated_hash == expected_hash)
}

fn download_and_verify(
    url: &str,
    dest_path: &Path,
    expected_hash: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let temp_filename = dest_path.file_name().unwrap();
    let temp_path = out_dir.join(temp_filename);

    println!(
        "cargo:warning=Downloading to temporary path: {:?}",
        temp_path
    );
    let mut response = reqwest::blocking::get(url)?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response
            .text()
            .unwrap_or_else(|_| "Could not read error body".to_string());
        return Err(format!("Download failed with status {}: {}", status, error_body).into());
    }

    let mut temp_file = fs::File::create(&temp_path)?;
    response.copy_to(&mut temp_file)?;
    println!("cargo:warning=Download complete. Verifying file integrity...");

    match verify_sha256(&temp_path, expected_hash) {
        Ok(true) => {
            fs::copy(&temp_path, dest_path)?;
            fs::remove_file(&temp_path)?;
            println!(
                "cargo:warning=Successfully downloaded and verified {:?}.",
                dest_path
            );
            Ok(())
        }
        Ok(false) => {
            fs::remove_file(&temp_path)?;
            Err("Verification failed! The downloaded file is corrupt.".into())
        }
        Err(e) => {
            fs::remove_file(&temp_path).ok();
            Err(format!("Could not verify file after download: {}", e).into())
        }
    }
}

/// Main-thread stack reserve of the executables: `RENDER_STACK_SIZE`
/// (rawtonic-runtime; a test checks they agree). Linux and macOS give the main
/// thread 8 MiB, Windows only 1 MiB unless the linker sets more, and a render
/// or GPU start-up on it would then overflow silently at start (gap G74).
const MAIN_STACK_SIZE: u64 = 8 * 1024 * 1024;

fn link_main_stack() {
    println!("cargo:rustc-env=RAWTONIC_MAIN_STACK_SIZE={MAIN_STACK_SIZE}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_STACK_SIZE}"),
        _ => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_STACK_SIZE}"),
    }
}

fn link_windows_test_resources() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // tauri-build already links this intact resource into binaries.
        // Integration tests need it once, and lib unit tests need a cfg(test)
        // native link: a generic link arg would duplicate the binary resource.
        let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
        let resource = out.join("resource.lib");
        let resource_name = resource.to_str().expect("Build paths must be valid UTF-8");
        let declaration = format!(
            "#[link(name = {resource_name:?}, kind = \"dylib\", modifiers = \"+verbatim\")]\nunsafe extern \"C\" {{}}\n"
        );
        fs::write(out.join("windows-test-resources.rs"), declaration)
            .expect("Cannot generate the Windows unit-test resource link");
        println!("cargo:rustc-link-arg-tests={}", resource.display());
    }
}

/// Tauri's CRT override must travel with its app-only linker arguments.
/// Cargo propagates native search paths to workspace doctests, but not those
/// arguments: an empty msvcrt.lib in OUT_DIR hides the doctests' real CRT.
fn build_tauri() {
    println!("cargo:rerun-if-env-changed=STATIC_VCRUNTIME");
    let msvc = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    let shim = out.join("msvcrt.lib");
    if msvc {
        // A previous build may have enabled static VC. Only a newly generated
        // override proves that Tauri selected it for the current configuration.
        match fs::remove_file(&shim) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => panic!("Cannot clear the generated VC runtime override: {e}"),
        }
    }
    // Tauri's manifest plus the Windows 8+ compatibility entry (layered
    // child windows, src/snap_layout/win32.rs).
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    let windows = tauri_build::WindowsAttributes::new()
        .app_manifest(include_str!("windows-app-manifest.xml"));
    if let Err(error) =
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
    {
        println!("{error:#}");
        std::process::exit(1);
    }
    if msvc && shim.exists() {
        // Move only Tauri's generated override, keeping resource.lib intact.
        // /LIBPATH is app-local, unlike cargo:rustc-link-search. The original
        // static VC / dynamic UCRT selection and Tauri's architecture/config
        // guards remain owned by tauri-build.
        let private = out.join("app-vcruntime");
        fs::create_dir_all(&private).expect("Cannot create the VC runtime link directory");
        fs::copy(&shim, private.join("msvcrt.lib"))
            .expect("Cannot relocate Tauri's VC runtime override");
        fs::remove_file(shim).expect("Cannot remove the public VC runtime override");
        println!("cargo:rustc-link-arg=/LIBPATH:{}", private.display());
    }
}

/// The VC++ runtime libraries ONNX Runtime and its WebGPU plugin import
/// (`dumpbin /dependents`), bundled app-local beside `onnxruntime.dll`
/// (gap G76): Microsoft's DLLs are built with the current toolset and crash
/// in an older system `MSVCP140.dll` (`std::mutex`, VS 2022 17.10). The app
/// loads them before ONNX Runtime (rawtonic-onnx `vc_runtime`). The x64-only
/// `vcruntime140_1.dll` is optional; the CI check (scripts/windows/
/// check_vc_runtime.ps1) proves every import is covered and new enough.
const VC_RUNTIME: &[(&str, bool)] = &[
    ("vcruntime140.dll", true),
    ("vcruntime140_1.dll", false),
    ("msvcp140.dll", true),
    ("msvcp140_1.dll", true),
    ("msvcp140_atomic_wait.dll", true),
];

/// The redistributable CRT folder of the Visual Studio that builds this
/// (`VC\Redist\MSVC\<version>\<arch>\Microsoft.VC14x.CRT`):
/// `RAWTONIC_VC_REDIST_DIR` (that folder itself), else `VCToolsRedistDir`
/// of a developer prompt, else the newest Visual Studio with C++ tools that
/// vswhere finds.
fn vc_redist_dir(target_arch: &str) -> PathBuf {
    println!("cargo:rerun-if-env-changed=RAWTONIC_VC_REDIST_DIR");
    println!("cargo:rerun-if-env-changed=VCToolsRedistDir");
    if let Some(dir) = env::var_os("RAWTONIC_VC_REDIST_DIR") {
        return PathBuf::from(dir);
    }
    let arch = match target_arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("No VC++ runtime for {other}"),
    };
    let redist = env::var_os("VCToolsRedistDir")
        .map(PathBuf::from)
        .or_else(|| {
            let program_files = env::var_os("ProgramFiles(x86)")?;
            let vswhere =
                Path::new(&program_files).join("Microsoft Visual Studio/Installer/vswhere.exe");
            let output = std::process::Command::new(vswhere)
                .args([
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                ])
                .output()
                .ok()?;
            let install = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            let version = fs::read_to_string(
                install.join("VC/Auxiliary/Build/Microsoft.VCRedistVersion.default.txt"),
            )
            .ok()?;
            Some(install.join("VC/Redist/MSVC").join(version.trim()))
        });
    let Some(redist) = redist else {
        panic!(
            "RAWTONIC_BUNDLE_ONNX=1 on Windows bundles the VC++ runtime, but no Visual Studio redistributable folder was found; set RAWTONIC_VC_REDIST_DIR to its Microsoft.VC14x.CRT folder"
        );
    };
    let arch_dir = redist.join(arch);
    fs::read_dir(&arch_dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("Microsoft.VC") && n.ends_with(".CRT"))
                })
                .max()
        })
        .unwrap_or_else(|| panic!("No Microsoft.VC*.CRT folder in {}", arch_dir.display()))
}

/// Copies the VC++ runtime into `dest` (resources\, beside ONNX Runtime).
fn bundle_vc_runtime(dest: &Path, target_arch: &str) {
    let source = vc_redist_dir(target_arch);
    println!("cargo:warning=VC++ runtime from {}", source.display());
    for &(name, required) in VC_RUNTIME {
        let from = source.join(name);
        println!("cargo:rerun-if-changed={}", from.display());
        if !from.is_file() {
            if required {
                panic!("{} is missing", from.display());
            }
            // Not in this architecture's runtime: no stale copy either.
            match fs::remove_file(dest.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => panic!("Cannot remove the stale {name}: {e}"),
            }
            continue;
        }
        fs::copy(&from, dest.join(name))
            .unwrap_or_else(|e| panic!("Cannot bundle {}: {e}", from.display()));
    }
}

/// The DirectX Shader Compiler pins (`windows/dxc.sha256`): the release
/// zip's URL and hash, and each file's hash by its path in the zip.
struct DxcPins {
    url: String,
    zip: (String, String),
    files: Vec<(String, String)>,
}

fn dxc_pins() -> DxcPins {
    let text = include_str!("windows/dxc.sha256");
    let mut url = None;
    let mut zip = None;
    let mut files = Vec::new();
    for line in text.lines() {
        if let Some(u) = line.strip_prefix("# url ") {
            url = Some(u.trim().to_string());
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let (hash, name) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("bad line in windows/dxc.sha256: {line}"));
        let entry = (name.trim().to_string(), hash.to_string());
        if name.ends_with(".zip") {
            zip = Some(entry);
        } else {
            files.push(entry);
        }
    }
    DxcPins {
        url: url.expect("no url in windows/dxc.sha256"),
        zip: zip.expect("no zip in windows/dxc.sha256"),
        files,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One stored or deflated member of a zip archive (no zip64: the DXC
/// release zip is 26 MB). Every result is checked against its pinned hash,
/// so a misread archive fails the build instead of shipping.
fn zip_member(archive: &[u8], wanted: &str) -> Option<Vec<u8>> {
    let u16_at = |at: usize| {
        Some(u16::from_le_bytes(
            archive.get(at..at + 2)?.try_into().ok()?,
        ))
    };
    let u32_at = |at: usize| {
        Some(u32::from_le_bytes(
            archive.get(at..at + 4)?.try_into().ok()?,
        ))
    };
    // End of central directory: the last "PK\5\6" (a comment may follow it).
    let eocd = (0..archive.len().saturating_sub(21))
        .rev()
        .find(|&i| archive[i..].starts_with(b"PK\x05\x06"))?;
    let count = u16_at(eocd + 10)? as usize;
    let mut at = u32_at(eocd + 16)? as usize;
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 {
            return None;
        }
        let method = u16_at(at + 10)?;
        let compressed = u32_at(at + 20)? as usize;
        let size = u32_at(at + 24)? as usize;
        let name_len = u16_at(at + 28)? as usize;
        let skip = name_len + u16_at(at + 30)? as usize + u16_at(at + 32)? as usize;
        let local = u32_at(at + 42)? as usize;
        let name = archive.get(at + 46..at + 46 + name_len)?;
        at += 46 + skip;
        if name != wanted.as_bytes() {
            continue;
        }
        if u32_at(local)? != 0x0403_4b50 {
            return None;
        }
        let start = local + 30 + u16_at(local + 26)? as usize + u16_at(local + 28)? as usize;
        let data = archive.get(start..start + compressed)?;
        let mut out = Vec::with_capacity(size);
        match method {
            0 => out.extend_from_slice(data),
            8 => {
                flate2::read::DeflateDecoder::new(data)
                    .read_to_end(&mut out)
                    .ok()?;
            }
            _ => return None,
        }
        return (out.len() == size).then_some(out);
    }
    None
}

/// Bundles `dxcompiler.dll` and `dxil.dll` into `dest` (resources\, beside
/// ONNX Runtime's WebGPU plugin, where Dawn's Direct3D 12 backend looks for
/// them; rawtonic-onnx `dxc` also loads them by full path): from
/// Microsoft's official release zip, every file checked against
/// `windows/dxc.sha256`. Gap G76: without them the plugin fails on
/// Direct3D 12 and every model runs on the CPU.
fn bundle_dxc(dest: &Path, target_arch: &str) {
    println!("cargo:rerun-if-changed=windows/dxc.sha256");
    let arch = match target_arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("No DirectX Shader Compiler for {other}"),
    };
    let pins = dxc_pins();
    let wanted: Vec<(String, &str, &str)> = pins
        .files
        .iter()
        .filter_map(|(path, hash)| {
            let name = path.strip_prefix(&format!("bin/{arch}/"))?;
            Some((path.clone(), name, hash.as_str()))
        })
        .collect();
    assert_eq!(
        wanted.len(),
        2,
        "windows/dxc.sha256 must pin dxcompiler.dll and dxil.dll for {arch}"
    );
    let current = |name: &str, hash: &str| verify_sha256(&dest.join(name), hash).unwrap_or(false);
    if wanted.iter().all(|(_, name, hash)| current(name, hash)) {
        return;
    }
    println!(
        "cargo:warning=Downloading the DirectX Shader Compiler: {}",
        pins.url
    );
    let mut archive = Vec::new();
    reqwest::blocking::get(&pins.url)
        .and_then(|r| r.error_for_status())
        .and_then(|mut r| r.copy_to(&mut archive))
        .unwrap_or_else(|e| panic!("Cannot download {}: {e}", pins.url));
    let got = sha256_hex(&archive);
    if got != pins.zip.1 {
        panic!("{} has SHA-256 {got}, pinned {}", pins.zip.0, pins.zip.1);
    }
    for (path, name, hash) in wanted {
        let bytes = zip_member(&archive, &path)
            .unwrap_or_else(|| panic!("{path} not readable in {}", pins.zip.0));
        let got = sha256_hex(&bytes);
        if got != hash {
            panic!("{path} has SHA-256 {got}, pinned {hash}");
        }
        // Never a half-written DLL under its real name.
        let partial = dest.join(format!("{name}.partial"));
        fs::write(&partial, &bytes).unwrap_or_else(|e| panic!("Cannot write {name}: {e}"));
        fs::rename(&partial, dest.join(name))
            .unwrap_or_else(|e| panic!("Cannot bundle {name}: {e}"));
    }
}

fn main() {
    link_main_stack();
    println!("cargo:rerun-if-env-changed=RAWTONIC_BUNDLE_ONNX");
    if std::env::var("RAWTONIC_BUNDLE_ONNX").as_deref() != Ok("1") {
        build_tauri();
        link_windows_test_resources();
        return;
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // (file in the model pack, name in resources/, SHA-256). Windows and
    // Linux get ONNX Runtime 1.30.0 and its WebGPU plugin 0.4.0 (GPU AI,
    // docs/decisions/0011-gpu-ai.md; plugins need 1.24.4 or later); Apple
    // silicon uses 1.30.0 too (Core ML stays the fast path), Intel macOS
    // 1.23.2 (1.30 has no Intel build).
    // All are Microsoft's release files, unchanged (docs/MODELS.md).
    let files: &[(&str, &str, &str)] = match (target_os.as_str(), target_arch.as_str()) {
        ("windows", "x86_64") => &[
            (
                "onnxruntime-1.30.0-windows-x86_64.dll",
                "onnxruntime.dll",
                "7e39e2bdbba836d98071ef28620735ba36a47c554cf794585269aecc50fab0da",
            ),
            (
                "onnxruntime_providers_webgpu-0.4.0-windows-x86_64.dll",
                "onnxruntime_providers_webgpu.dll",
                "85d7b2065b1968b846cd00d5d87d5286d3c44e8f6c557cd382d96300e524b6cb",
            ),
        ],
        ("windows", "aarch64") => &[
            (
                "onnxruntime-1.30.0-windows-aarch64.dll",
                "onnxruntime.dll",
                "46b7ec24a6ee2932bb315edb39d26d6b2d51eef21f02c8e1ceafe801fe991a89",
            ),
            (
                "onnxruntime_providers_webgpu-0.4.0-windows-aarch64.dll",
                "onnxruntime_providers_webgpu.dll",
                "e2a72894617c0519cae3080534ca1fc957c4ab607a993c05f326d83b322a581d",
            ),
        ],
        ("linux", "x86_64") => &[
            (
                "libonnxruntime-1.30.0-linux-x86_64.so",
                "libonnxruntime.so",
                "245a6f8c38127551057a1cd1ffd59f0a186a227ade4f3492dea2494eb565542e",
            ),
            (
                "libonnxruntime_providers_webgpu-0.4.0-linux-x86_64.so",
                "libonnxruntime_providers_webgpu.so",
                "9f76e3dc4459421958ad086a9da03bf7e5de9289e9a3a56e28bbb459773dec81",
            ),
        ],
        ("linux", "aarch64") => &[
            (
                "libonnxruntime-1.30.0-linux-aarch64.so",
                "libonnxruntime.so",
                "64e903a43a041240fd6bcffe0ac6d4fea47ef87bf24b9d097801bd00a9612a4b",
            ),
            (
                "libonnxruntime_providers_webgpu-0.4.0-linux-aarch64.so",
                "libonnxruntime_providers_webgpu.so",
                "720e1288e7e465f924db44b88a98a9d1c543eba3fbb2ff801cf2545f5e7f0279",
            ),
        ],
        ("macos", "x86_64") => &[(
            "libonnxruntime-1.23.2-macos-x86_64.dylib",
            "libonnxruntime.dylib",
            "8c9c78de65ea3786f987c0d980e9c1b13a3a5fbc6b3e2965ba05b450e6e4c054",
        )],
        ("macos", "aarch64") => &[(
            "libonnxruntime-1.30.0-macos-aarch64.dylib",
            "libonnxruntime.dylib",
            "bcc9110f9d638a119de2db7afb3ba9a1da8085f0cb3401e1c48ae1caf450b6fa",
        )],
        ("android", "aarch64") => &[(
            "libonnxruntime-android-arm64-v8a.so",
            "libonnxruntime.so",
            "999ecfdb5b5a13e4097487773b6d71ce8a075408a237daab072e8f5e817bd78e",
        )],
        _ => panic!("Unsupported target: {}-{}", target_os, target_arch),
    };

    let dest_dir = if target_os == "android" {
        manifest_dir.join("libs").join("arm64-v8a")
    } else {
        manifest_dir.join("resources")
    };
    fs::create_dir_all(&dest_dir).unwrap();

    for &(download_filename, lib_name, expected_hash) in files {
        let dest_path = dest_dir.join(lib_name);
        let mut is_valid = false;
        if dest_path.exists() {
            match verify_sha256(&dest_path, expected_hash) {
                Ok(true) => {
                    println!(
                        "cargo:warning={lib_name} already exists and is valid. Skipping download."
                    );
                    is_valid = true;
                }
                Ok(false) => {
                    println!(
                        "cargo:warning=File {:?} exists but has incorrect hash. Deleting and re-downloading.",
                        dest_path
                    );
                    fs::remove_file(&dest_path).unwrap();
                }
                Err(e) => {
                    println!(
                        "cargo:warning=Could not verify file {:?}: {}. Re-downloading.",
                        dest_path, e
                    );
                }
            }
        }

        if !is_valid {
            println!(
                "cargo:warning=Downloading {download_filename} for {target_os}-{target_arch}..."
            );
            // Microsoft's files, unchanged, in Rawtonic's model pack
            // (docs/MODELS.md); scripts/models/install_models.sh --runtime
            // also puts them here.
            let base_url =
                "https://github.com/rawtonic-app/models/releases/download/pack-2026-11-12/";
            let download_url = format!("{}{}", base_url, download_filename);
            println!("cargo:warning=URL: {}", download_url);

            if let Err(e) = download_and_verify(&download_url, &dest_path, expected_hash) {
                panic!("Failed to download and verify {download_filename}: {}", e);
            }
        }
    }
    let (lib_name, dest_path) = (files[0].1, dest_dir.join(files[0].1));

    // The runtime's license and third-party notices (Dawn and others for
    // the WebGPU plugin) ship beside it.
    let notices = if files[0].0.contains("1.30.0") {
        "onnxruntime-1.30.0-ThirdPartyNotices.txt"
    } else if files[0].0.contains("1.23.2") {
        "onnxruntime-1.23.2-ThirdPartyNotices.txt"
    } else {
        "onnxruntime-ThirdPartyNotices.txt"
    };
    let source = manifest_dir.join("../licenses/models");
    let licenses = manifest_dir.join("resources/licenses");
    fs::create_dir_all(&licenses).unwrap();
    for (from, to) in [
        ("onnxruntime-MIT.txt", "onnxruntime-LICENSE.txt"),
        (notices, "onnxruntime-ThirdPartyNotices.txt"),
    ] {
        println!("cargo:rerun-if-changed=../licenses/models/{from}");
        fs::copy(source.join(from), licenses.join(to)).unwrap();
    }

    if target_os == "windows" {
        bundle_vc_runtime(&dest_dir, &target_arch);
        bundle_dxc(&dest_dir, &target_arch);
    }

    if target_os == "android" {
        let jni_libs_dir = manifest_dir.join("gen/android/app/src/main/jniLibs/arm64-v8a");
        fs::create_dir_all(&jni_libs_dir).unwrap();
        fs::copy(&dest_path, jni_libs_dir.join(lib_name)).unwrap();

        println!("cargo:rustc-env=ORT_LIB_LOCATION={}", dest_dir.display());
        println!("cargo:rustc-env=ORT_STRATEGY=manual");
        println!("cargo:rustc-link-search=native={}", dest_dir.display());
    }

    println!("cargo:rerun-if-changed=build.rs");

    build_tauri();
    link_windows_test_resources();
}
