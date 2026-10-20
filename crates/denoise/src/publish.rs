// SPDX-License-Identifier: AGPL-3.0-or-later
//! Putting a built folder (a compiled Core ML model) in place when the app
//! and the CLI may build it at the same time.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Builds the folder `target` with `build` unless `complete(target)`. One
/// process at a time per target (a lock file beside it): a second one waits
/// and uses the first one's result. `build` fills a staging folder of its
/// own; a complete `target` is never deleted, an incomplete one (a crash
/// before an earlier rename) is replaced.
pub(crate) fn publish_dir(
    target: &Path,
    complete: impl Fn(&Path) -> bool,
    build: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = target.parent().context("target without a folder")?;
    let name = target
        .file_name()
        .context("target without a name")?
        .to_string_lossy();
    let lock = fs::File::create(parent.join(format!(".{name}.lock")))?;
    lock.lock()?;
    if complete(target) {
        return Ok(());
    }
    let staging_dir = tempfile::Builder::new()
        .prefix(&format!(".{name}-"))
        .tempdir_in(parent)?;
    let staging = staging_dir.path().join(&*name);
    build(&staging)?;
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::rename(&staging, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn complete(dir: &Path) -> bool {
        dir.join("done").exists()
    }

    fn build_into(staging: &Path, by: &str) -> Result<()> {
        fs::create_dir_all(staging)?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(staging.join("done"), by)?;
        Ok(())
    }

    #[test]
    fn concurrent_builds_publish_once_and_reuse_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.mlmodelc");
        let builds = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for i in 0..4 {
                let (target, builds) = (&target, &builds);
                scope.spawn(move || {
                    publish_dir(target, complete, |staging| {
                        builds.fetch_add(1, Ordering::SeqCst);
                        build_into(staging, &i.to_string())
                    })
                    .unwrap();
                    // Each caller finds a complete model when it returns.
                    assert!(complete(target));
                });
            }
        });
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_incomplete_target_is_replaced_and_a_complete_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("model.mlmodelc");
        fs::create_dir_all(target.join("weights")).unwrap();
        publish_dir(&target, complete, |s| build_into(s, "new")).unwrap();
        assert_eq!(fs::read(target.join("done")).unwrap(), b"new");
        assert!(!target.join("weights").exists());
        publish_dir(&target, complete, |_| panic!("built again")).unwrap();
        // Only the lock file and the model are left.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
