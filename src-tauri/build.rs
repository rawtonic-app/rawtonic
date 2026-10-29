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
    tauri_build::build();
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

fn main() {
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
    // docs/decisions/0011-gpu-ai.md; plugins need 1.24.4 or later); macOS
    // uses 1.23.2 (Core ML is its fast path; 1.30 has no Intel build).
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
            "libonnxruntime-1.23.2-macos-aarch64.dylib",
            "libonnxruntime.dylib",
            "d306d2bc768540766c7ed8a1e0ff05d2870c77a934ebeee4a7bafa1b732ef299",
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
                "https://github.com/rawtonic-app/models/releases/download/pack-2026-10-27b/";
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
