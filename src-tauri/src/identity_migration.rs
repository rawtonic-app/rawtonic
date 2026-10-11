//! One-time carry-over from the app's former identity, Re-touch
//! (`io.retouch.editor`), to Rawtonic (`cc.rawtonic.editor`).
//!
//! Settings, presets, LUTs, albums, the remembered library and downloaded
//! models live in directories named after the bundle identifier, so the new
//! identifier starts empty. On the first start the former directories are
//! copied file by file with the verified, no-clobber ingest copy. The former
//! directories are left untouched, an existing file is never replaced, and a
//! marker records the carry-over so it runs once. Caches, logs and the
//! webview's own storage are not carried over; they are rebuilt.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use tauri::Manager;

pub const FORMER_IDENTIFIER: &str = "io.retouch.editor";
const MARKER: &str = ".carried-over-from-io.retouch.editor";
/// Written while the app runs; never meaningful in another installation.
const SKIPPED: &[&str] = &[".gpu_init_crash_flag"];

/// What the carry-over did, for the log (logging is not set up yet when it runs).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub copied: usize,
    pub kept_existing: usize,
    pub messages: Vec<String>,
}

/// Carry the former identity's config and data directories over, if this
/// installation has none of its own yet. Errors are reported, never fatal.
pub fn carry_over(app: &tauri::AppHandle) -> Report {
    let mut report = Report::default();
    let path = app.path();
    let mut directories: Vec<PathBuf> = [path.app_config_dir(), path.app_data_dir()]
        .into_iter()
        .flatten()
        .collect();
    directories.dedup();
    for current in directories {
        let Some(former) = former_directory(&current) else {
            continue;
        };
        match carry_over_directory(&former, &current) {
            Ok(Some((copied, kept))) => {
                report.copied += copied;
                report.kept_existing += kept;
                report.messages.push(format!(
                    "Carried over {copied} file(s) from {} to {} ({kept} already present, kept)",
                    former.display(),
                    current.display()
                ));
            }
            Ok(None) => {}
            Err(error) => report.messages.push(format!(
                "Could not carry over {} to {}: {error:#}",
                former.display(),
                current.display()
            )),
        }
    }
    report
}

/// The sibling directory named after the former identifier, when `current`
/// is named after an identifier at all.
fn former_directory(current: &Path) -> Option<PathBuf> {
    let name = current.file_name()?.to_str()?;
    (name != FORMER_IDENTIFIER && name.contains('.'))
        .then(|| current.with_file_name(FORMER_IDENTIFIER))
}

/// `Ok(None)`: nothing to do (no former directory, already carried over, or
/// this installation already has its own files).
fn carry_over_directory(former: &Path, current: &Path) -> Result<Option<(usize, usize)>> {
    if !former.is_dir() || current.join(MARKER).exists() {
        return Ok(None);
    }
    if current.is_dir()
        && fs::read_dir(current)?
            .flatten()
            .any(|entry| entry.file_name() != MARKER)
    {
        return Ok(None);
    }
    fs::create_dir_all(current)?;
    let (mut copied, mut kept) = (0, 0);
    copy_tree(former, current, &mut copied, &mut kept)?;
    rawtonic_metadata::atomic_write(&current.join(MARKER), format!("{}\n", former.display()))?;
    Ok(Some((copied, kept)))
}

fn copy_tree(from: &Path, to: &Path, copied: &mut usize, kept: &mut usize) -> Result<()> {
    let never_cancelled = AtomicBool::new(false);
    for entry in fs::read_dir(from).with_context(|| format!("Cannot list {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIPPED.iter().any(|skipped| name == *skipped) {
            continue;
        }
        let source = entry.path();
        let destination = to.join(&name);
        // Symbolic links are not followed or recreated.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir_all(&destination)?;
            copy_tree(&source, &destination, copied, kept)?;
        } else if kind.is_file() {
            if destination.exists() {
                *kept += 1;
                continue;
            }
            rawtonic_ingest::verified_copy(&source, &destination, &never_cancelled, |_| {})
                .with_context(|| format!("Cannot copy {}", source.display()))?;
            *copied += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push((
                        path.strip_prefix(root).unwrap().into(),
                        fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        files.sort();
        files
    }

    #[test]
    fn carries_over_once_without_touching_the_former_directory() {
        let base = tempfile::tempdir().unwrap();
        let former = base.path().join(FORMER_IDENTIFIER);
        let current = base.path().join("cc.rawtonic.editor");
        write(
            &former.join("settings.json"),
            r##"{"maskColor":"#00ff00"}"##,
        );
        write(&former.join("last-library.json"), r#""/Jobs/Session""#);
        write(&former.join("presets/Portrait.json"), "{}");
        write(&former.join("luts/film.cube"), "LUT_3D_SIZE 2");
        write(&former.join(".gpu_init_crash_flag"), "");
        let before = snapshot(&former);

        assert_eq!(former_directory(&current), Some(former.clone()));
        assert_eq!(
            carry_over_directory(&former, &current).unwrap(),
            Some((4, 0))
        );
        assert_eq!(
            snapshot(&former),
            before,
            "the former directory stays as it was"
        );
        assert_eq!(
            fs::read_to_string(current.join("settings.json")).unwrap(),
            r##"{"maskColor":"#00ff00"}"##
        );
        assert!(current.join("presets/Portrait.json").is_file());
        assert!(current.join("luts/film.cube").is_file());
        assert!(!current.join(".gpu_init_crash_flag").exists());

        // Later starts change nothing, even after the user edits settings.
        write(
            &current.join("settings.json"),
            r##"{"maskColor":"#ff0000"}"##,
        );
        assert_eq!(carry_over_directory(&former, &current).unwrap(), None);
        assert_eq!(
            fs::read_to_string(current.join("settings.json")).unwrap(),
            r##"{"maskColor":"#ff0000"}"##
        );
    }

    #[test]
    fn an_installation_with_its_own_files_is_left_alone() {
        let base = tempfile::tempdir().unwrap();
        let former = base.path().join(FORMER_IDENTIFIER);
        let current = base.path().join("cc.rawtonic.editor");
        write(&former.join("settings.json"), "old");
        write(&current.join("settings.json"), "new");
        assert_eq!(carry_over_directory(&former, &current).unwrap(), None);
        assert_eq!(
            fs::read_to_string(current.join("settings.json")).unwrap(),
            "new"
        );
        assert!(!current.join(MARKER).exists());
    }

    #[test]
    fn nothing_happens_without_a_former_directory() {
        let base = tempfile::tempdir().unwrap();
        let current = base.path().join("cc.rawtonic.editor");
        assert_eq!(
            carry_over_directory(&base.path().join(FORMER_IDENTIFIER), &current).unwrap(),
            None
        );
        assert!(!current.exists());
        // The former identity itself, or a directory not named after an
        // identifier, has nothing to carry over from.
        assert_eq!(former_directory(&base.path().join(FORMER_IDENTIFIER)), None);
        assert_eq!(former_directory(&base.path().join("config")), None);
    }
}
