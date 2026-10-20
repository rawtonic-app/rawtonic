//! Ingest and export onto a real exFAT volume, which has neither an
//! exclusive rename nor hard links (macOS answers both with ENOTSUP): a
//! disk image made with hdiutil. Opt-in, as it mounts a volume:
//!
//! RAWTONIC_EXFAT_RAW=/path/photo.NEF cargo test -p rawtonic-cli --test exfat -- --ignored
//!
//! The RAW is any one the decoder reads (e.g. `bayer-nef.NEF` from
//! `scripts/bench/fetch_fixtures.sh`); the lensfun database is the
//! checkout's `src-tauri/lensfun_db` unless RAWTONIC_LENSFUN_DB names one.
#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A mounted exFAT disk image, detached when dropped.
struct ExfatVolume {
    mount: PathBuf,
}

impl ExfatVolume {
    fn create(dir: &Path) -> Self {
        let image = dir.join("exfat.dmg");
        let mount = dir.join("volume");
        fs::create_dir(&mount).unwrap();
        let ok = |command: &mut Command| assert!(command.status().unwrap().success());
        ok(Command::new("hdiutil")
            .args([
                "create", "-quiet", "-size", "400m", "-fs", "ExFAT", "-volname", "RTEXFAT",
            ])
            .arg(&image));
        ok(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image));
        Self { mount }
    }
}

impl Drop for ExfatVolume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mount)
            .status();
    }
}

fn rawtonic(args: &[&Path]) -> std::process::Output {
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .args(args)
        .env("RAWTONIC_LENSFUN_DB", lensfun)
        .output()
        .unwrap()
}

fn succeeds(args: &[&Path]) {
    let output = rawtonic(args);
    assert!(
        output.status.success(),
        "rawtonic {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every file under `dir` named `name`.
fn find(dir: &Path, name: &std::ffi::OsStr) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(find(&path, name));
        } else if path.file_name() == Some(name) {
            found.push(path);
        }
    }
    found
}

/// Names a no-clobber persist leaves behind when it fails half-way.
fn leftovers(dir: &Path) -> Vec<PathBuf> {
    let mut left = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            left.extend(leftovers(&path));
        } else if name.starts_with(".tmp") {
            left.push(path);
        }
    }
    left
}

#[test]
#[ignore = "mounts an exFAT disk image; needs RAWTONIC_EXFAT_RAW"]
fn ingest_and_export_onto_exfat_never_replace_a_file() {
    let raw = PathBuf::from(
        std::env::var_os("RAWTONIC_EXFAT_RAW").expect("RAWTONIC_EXFAT_RAW names a RAW file"),
    );
    let work = tempfile::tempdir().unwrap();
    let volume = ExfatVolume::create(work.path());
    let mount = volume.mount.as_path();

    // The volume really lacks an exclusive rename (else this tests nothing).
    let (from, to) = (mount.join("probe"), mount.join("probe 2"));
    fs::write(&from, b"").unwrap();
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).unwrap();
    // SAFETY: both are valid NUL-terminated paths for the call's duration.
    let renamed =
        unsafe { libc::renamex_np(c(&from).as_ptr(), c(&to).as_ptr(), libc::RENAME_EXCL) };
    assert_eq!(renamed, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENOTSUP)
    );
    fs::remove_file(&from).unwrap();

    // Ingest (with a backup) onto the volume: verified copies.
    let job = mount.join("Job");
    succeeds(&[
        Path::new("create"),
        &job,
        Path::new("--name"),
        Path::new("Job"),
    ]);
    let backup = mount.join("Backup");
    fs::create_dir(&backup).unwrap();
    succeeds(&[
        Path::new("ingest"),
        &job,
        &raw,
        Path::new("--backup"),
        &backup,
    ]);
    let original = fs::read(&raw).unwrap();
    let name = raw.file_name().unwrap();
    let copies: Vec<_> = find(&job, name)
        .into_iter()
        .chain(find(&backup, name))
        .collect();
    assert_eq!(copies.len(), 2, "{copies:?}");
    for copy in &copies {
        assert_eq!(fs::read(copy).unwrap(), original, "{}", copy.display());
    }

    // Export onto the volume: the same bytes as an export to the system
    // volume (which renames exclusively).
    let output = mount.join("photo.jpg");
    let reference = work.path().join("photo.jpg");
    succeeds(&[Path::new("export"), &raw, Path::new("--output"), &output]);
    succeeds(&[Path::new("export"), &raw, Path::new("--output"), &reference]);
    let exported = fs::read(&output).unwrap();
    assert_eq!(exported, fs::read(&reference).unwrap());

    // An existing file is refused, not replaced.
    let again = rawtonic(&[Path::new("export"), &raw, Path::new("--output"), &output]);
    assert!(!again.status.success());
    assert_eq!(fs::read(&output).unwrap(), exported);
    let clash = mount.join("taken.jpg");
    fs::write(&clash, b"the user's").unwrap();
    let refused = rawtonic(&[Path::new("export"), &raw, Path::new("--output"), &clash]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&clash).unwrap(), b"the user's");
    assert_eq!(leftovers(mount), Vec::<PathBuf>::new());
}
