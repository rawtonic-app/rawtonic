// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::OsString;
use std::path::PathBuf;

/// The folder the command was run in, which relative path arguments refer to.
/// An AppImage's AppRun changes into the AppImage's own read-only folder
/// (`$APPDIR/usr`) before it starts the app; the AppImage runtime keeps the
/// user's folder in `OWD`, and `--appimage-extract-and-run`, which sets no
/// `OWD`, keeps the shell's `PWD`. A plain binary and a macOS bundle keep the
/// working directory. `var` reads the environment.
pub fn invocation_dir(
    var: impl Fn(&str) -> Option<OsString>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    if var("APPIMAGE").is_none() {
        return cwd;
    }
    ["OWD", "PWD"]
        .into_iter()
        .filter_map(|name| var(name).map(PathBuf::from))
        .find(|dir| dir.is_absolute())
        .or(cwd)
}
