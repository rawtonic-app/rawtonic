// SPDX-License-Identifier: AGPL-3.0-or-later

use rawtonic_fs::anchored::Directory;
use semver::Version;
use std::{ffi::OsStr, fs, io, path::Path};

/// The updater has always used tempfile's six alphanumeric characters.
pub(super) const SUFFIX_LEN: usize = 6;

pub(super) fn setup_name(version: &str) -> String {
    format!("Rawtonic_{version}_x64-setup.exe")
}

#[cfg(windows)]
pub(super) fn updates_dir(app: &tauri::AppHandle) -> tauri::Result<std::path::PathBuf> {
    use tauri::Manager;
    Ok(app.path().app_local_data_dir()?.join("updates"))
}

#[cfg(windows)]
pub(crate) fn cleanup_at_start(app: &tauri::AppHandle) {
    match updates_dir(app) {
        Ok(root) => cleanup(&root, &app.package_info().version),
        Err(error) => log::warn!("Could not locate staged updates: {error}"),
    }
}

fn cleanup(root: &Path, current: &Version) {
    let entries = || {
        let directory = Directory::open(root)?;
        Ok::<_, io::Error>((directory, fs::read_dir(root)?))
    };
    let (directory, entries) = match entries() {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            log::warn!("Could not scan staged updates: {error}");
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::warn!("Could not read staged update entry: {error}");
                continue;
            }
        };
        let name = entry.file_name();
        let Some((version, random)) = name.to_str().and_then(|name| name.rsplit_once('-')) else {
            continue;
        };
        if random.len() != SUFFIX_LEN || !random.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(parsed) = Version::parse(version) else {
            continue;
        };
        if parsed.cmp_precedence(current).is_gt() {
            continue;
        }

        // Resolve everything to be deleted under the opened updates directory.
        // Only the installer's own filename is eligible. A nonempty folder
        // containing anything else is kept.
        let remove = || {
            let staged = directory.open_child(&name)?;
            remove_setup(&staged, OsStr::new(&setup_name(version)))?;
            drop(staged);
            directory.remove_empty_child(&name)
        };
        if let Err(error) = remove() {
            log::warn!("Could not clean staged update {version}: {error}");
        }
    }
}

fn remove_setup(directory: &Directory, name: &OsStr) -> io::Result<()> {
    #[cfg(windows)]
    {
        // The installer may still be exiting after starting the new app.
        use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
        for delay in [100, 250, 500, 1000] {
            match directory.remove_file(name, None) {
                Err(ref error)
                    if error.raw_os_error().is_some_and(|code| {
                        code == ERROR_ACCESS_DENIED.0 as i32
                            || code == ERROR_SHARING_VIOLATION.0 as i32
                    }) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                }
                result => return result,
            }
        }
    }
    directory.remove_file(name, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(root: &Path, version: &str, suffix: &str) -> std::path::PathBuf {
        let folder = root.join(format!("{version}-{suffix}"));
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join(setup_name(version)), b"setup").unwrap();
        folder
    }

    #[test]
    fn removes_only_staged_setups_at_or_before_the_running_version() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("updates");
        fs::create_dir(&root).unwrap();

        let old = stage(&root, "0.1.0-alpha.9", "AA1234");
        let current = stage(&root, "0.1.0-alpha.10", "Bb0234");
        let newer = stage(&root, "0.1.0-alpha.11", "CC1234");
        let wrong_suffix = stage(&root, "0.1.0-alpha.9", "backup-folder");
        let unrelated_file = root.join("0.1.0-alpha.9-DD1234");
        fs::write(&unrelated_file, b"keep").unwrap();
        let unrelated_folder = root.join("0.1.0-alpha.9-EE1234");
        fs::create_dir(&unrelated_folder).unwrap();
        fs::write(unrelated_folder.join("user.txt"), b"keep").unwrap();

        cleanup(&root, &Version::parse("0.1.0-alpha.10").unwrap());
        assert!(!old.exists());
        assert!(!current.exists());
        assert!(newer.join(setup_name("0.1.0-alpha.11")).is_file());
        assert!(wrong_suffix.join(setup_name("0.1.0-alpha.9")).is_file());
        assert_eq!(fs::read(unrelated_file).unwrap(), b"keep");
        assert_eq!(
            fs::read(unrelated_folder.join("user.txt")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn leaves_symlinks_and_their_targets_in_place() {
        #[cfg(unix)]
        use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
        #[cfg(windows)]
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("updates");
        fs::create_dir(&root).unwrap();
        let outside = stage(temp.path(), "0.1.0", "AA1234");
        let outside_setup = outside.join(setup_name("0.1.0"));
        let linked_dir = root.join("0.1.0-BB1234");
        let made = symlink_dir(&outside, &linked_dir);
        #[cfg(windows)]
        if made
            .as_ref()
            .is_err_and(|error| error.raw_os_error() == Some(1314))
        {
            eprintln!("SKIP: no right to make symbolic links");
            return;
        }
        made.unwrap();

        let file_dir = root.join("0.1.0-CC1234");
        fs::create_dir(&file_dir).unwrap();
        let linked_setup = file_dir.join(setup_name("0.1.0"));
        symlink_file(&outside_setup, &linked_setup).unwrap();

        let linked_root = temp.path().join("linked-updates");
        symlink_dir(temp.path(), &linked_root).unwrap();
        let running = Version::new(0, 1, 0);
        cleanup(&linked_root, &running);
        cleanup(&root, &running);

        assert!(fs::symlink_metadata(linked_dir).unwrap().is_symlink());
        assert!(fs::symlink_metadata(linked_setup).unwrap().is_symlink());
        assert_eq!(fs::read(outside_setup).unwrap(), b"setup");
    }
}
