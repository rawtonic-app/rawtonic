//! Open With / Edit With on Linux: the programs the freedesktop desktop
//! offers (`desktop.rs`) bound to the session's environment, and their
//! launch. A program is the path of its `.desktop` file; a program the user
//! picked with "Browse for a program…" is such a file or an executable.
use super::{
    AppChoice, Launched,
    desktop::{self, Dirs, Entry, Fields},
};
use std::{
    collections::HashSet,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const SUPPORTED: bool = true;
/// Copies of a program in several places show once.
pub const ONE_PER_NAME: bool = true;

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn languages() -> Vec<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .map(|l| desktop::languages(&l))
        .unwrap_or_default()
}

/// Every text file `name` of the shared MIME database under `dirs.data`.
fn mime_db(dirs: &Dirs, name: &str) -> Vec<String> {
    dirs.data
        .iter()
        .filter_map(|d| fs::read_to_string(d.join("mime").join(name)).ok())
        .collect()
}

fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

fn entries(dirs: &Dirs) -> Vec<Entry> {
    let own = own_exe();
    desktop::all_entries(dirs, &languages(), &path_dirs())
        .into_iter()
        .filter(|e| !desktop::is_own(e, own.as_deref()))
        .collect()
}

/// The programs that open every one of `files`, and the default program of
/// the first.
pub fn apps_for(files: &[&Path]) -> (Vec<AppChoice>, Option<String>) {
    apps_in(&Dirs::from_env(), files, false)
}

pub fn edit_apps_for(file: &Path) -> (Vec<AppChoice>, Option<String>) {
    apps_in(&Dirs::from_env(), &[file], true)
}

fn apps_in(dirs: &Dirs, files: &[&Path], edit_with: bool) -> (Vec<AppChoice>, Option<String>) {
    let entries = entries(dirs);
    let assoc = desktop::Associations::read(dirs);
    let (globs, subclasses) = (mime_db(dirs, "globs2"), mime_db(dirs, "subclasses"));
    let aliases = mime_db(dirs, "aliases");
    let mut common: Option<Vec<(&Entry, bool)>> = None;
    let mut default = None;
    for (i, file) in files.iter().enumerate() {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
        let types = name
            .map(|n| desktop::mime_types_of(&n, &globs))
            .unwrap_or_default();
        let family = types
            .first()
            .map(|first| desktop::mime_family(first, &subclasses, &aliases))
            .unwrap_or_default();
        let (apps, def) = desktop::apps_for_family(&family, &entries, &assoc);
        let mut choices: Vec<_> = apps.iter().map(|entry| (*entry, true)).collect();
        if edit_with {
            let mime_ids: HashSet<_> = apps.iter().map(|entry| entry.id.as_str()).collect();
            choices.extend(
                entries
                    .iter()
                    .filter(|entry| {
                        !mime_ids.contains(entry.id.as_str())
                            && entry.offers_raster_editing(&dirs.desktops)
                            && !family.iter().any(|mime| assoc.is_removed(mime, &entry.id))
                    })
                    .map(|entry| (entry, false)),
            );
        }
        if i == 0 {
            default = def;
        }
        common = Some(match common {
            None => choices,
            Some(before) => {
                let these: HashSet<_> = choices.iter().map(|(entry, _)| &entry.id).collect();
                before
                    .into_iter()
                    .filter(|(entry, _)| these.contains(&entry.id))
                    .collect()
            }
        });
    }
    let mut out = Vec::new();
    let mut default_path = None;
    for (entry, recommended) in common.unwrap_or_default() {
        let icon = entry
            .icon
            .as_deref()
            .and_then(|i| desktop::icon_data_url(i, dirs));
        let mut choice = desktop::choice(entry, icon);
        choice.recommended = recommended;
        if recommended && default.as_deref() == Some(entry.id.as_str()) {
            default_path = Some(choice.path.clone());
        }
        out.push(choice);
    }
    (out, default_path)
}

fn is_desktop_file(program: &str) -> bool {
    Path::new(program)
        .extension()
        .is_some_and(|e| e == "desktop")
}

fn read_entry(program: &str) -> Option<Entry> {
    let path = Path::new(program);
    let text = fs::read_to_string(path).ok()?;
    let id = path.file_name()?.to_string_lossy().into_owned();
    desktop::parse_entry(&text, path, &id, &languages())
}

/// A picked program: its `.desktop` file or an executable, shown by name.
pub fn describe(program: &str) -> Option<AppChoice> {
    if is_desktop_file(program) {
        let entry = read_entry(program)?;
        let icon = entry
            .icon
            .as_deref()
            .and_then(|i| desktop::icon_data_url(i, &Dirs::from_env()));
        return Some(desktop::choice(&entry, icon));
    }
    let path = Path::new(program);
    if !path.is_absolute() || !desktop::is_executable(path) {
        return None;
    }
    Some(AppChoice {
        path: program.to_string(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        is_default: false,
        is_recent: false,
        recommended: false,
        icon: None,
    })
}

/// A desktop entry for Application files or an executable program file.
pub fn check(program: &str) -> Result<(), String> {
    let path = Path::new(program);
    if !path.is_absolute() {
        return Err(format!("{program} is not a program."));
    }
    if is_desktop_file(program) {
        return read_entry(program)
            .map(|_| ())
            .ok_or_else(|| format!("{program} is not an application entry."));
    }
    if desktop::is_executable(path) {
        Ok(())
    } else {
        Err(format!("{program} is not a program."))
    }
}

/// Starts `program` with `files`, without a shell and detached from this
/// process (its own session), and does not wait for it to end.
fn spawn(command: &[String]) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("Nothing to start.")?;
    let mut command = Command::new(program);
    // The portal dialogs are this app's choice, not the started program's.
    if crate::startup::portal_dialogs_set() {
        command.env_remove("GTK_USE_PORTAL");
    }
    // Started from an AppImage the program must not see its libraries.
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        for (name, value) in desktop::desktop_environment(&appdir.to_string_lossy(), &vars) {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{program} could not be started: {e}"))?;
    // Reap it when it ends, whenever that is.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens `files` in `program` in one launch (an entry that takes one file at
/// a time: one launch per file). An executable the user picked is given the
/// files as arguments.
pub fn open(program: &str, files: &[PathBuf], edit_with: bool) -> Result<Launched, String> {
    let first = files.first().ok_or("No files to open.")?;
    if !is_desktop_file(program) {
        let mut command = vec![program.to_string()];
        command.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        spawn(&command)?;
        return Ok(Launched::Picked);
    }
    let entry =
        read_entry(program).ok_or_else(|| format!("{program} is not an application entry."))?;
    let choices = apps_in(&Dirs::from_env(), &[first.as_path()], edit_with);
    let listed = choices.0.iter().any(|a| a.path == program);
    let desktop_file = PathBuf::from(program);
    let fields = Fields {
        name: &entry.name,
        icon: entry.icon.as_deref(),
        desktop_file: &desktop_file,
    };
    for command in desktop::commands(&entry.exec, files, &fields)? {
        spawn(&command)?;
    }
    Ok(if listed {
        Launched::Listed
    } else {
        Launched::Picked
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake desktop: MIME database, two applications and a user default.
    fn desktop_tree(root: &Path) -> Dirs {
        let (user, system) = (root.join("user"), root.join("sys"));
        for dir in [&user, &system] {
            fs::create_dir_all(dir.join("applications")).unwrap();
        }
        fs::create_dir_all(system.join("mime")).unwrap();
        fs::write(
            system.join("mime/globs2"),
            "50:image/x-nikon-nef:*.nef\n50:image/tiff:*.tif\n50:image/tiff:*.tiff\n50:image/jpeg:*.jpg\n",
        )
        .unwrap();
        fs::write(
            system.join("mime/subclasses"),
            "image/x-nikon-nef image/x-dcraw\n",
        )
        .unwrap();
        let app = |name: &str, exec: &str, mime: &str| {
            format!(
                "[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType={mime}\n"
            )
        };
        fs::write(
            system.join("applications/raw.desktop"),
            app("Zed Raw", "zraw %F", "image/x-dcraw;"),
        )
        .unwrap();
        fs::write(
            system.join("applications/paint.desktop"),
            app("Paint", "paint %f", "image/tiff;image/x-dcraw;"),
        )
        .unwrap();
        fs::write(
            system.join("applications/notes.desktop"),
            app("Notes", "notes %f", "text/plain;"),
        )
        .unwrap();
        fs::write(
            user.join("applications/mimeapps.list"),
            "[Default Applications]\nimage/x-dcraw=raw.desktop\n",
        )
        .unwrap();
        Dirs {
            data: vec![user, system],
            ..Dirs::default()
        }
    }

    #[test]
    fn lists_the_programs_for_a_raw_file_with_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = desktop_tree(tmp.path());
        let nef = Path::new("/p/A.NEF");
        let (apps, default) = apps_in(&dirs, &[nef], false);
        // Nearest type's programs first (the menu sorts them later).
        let names: Vec<_> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Paint", "Zed Raw"]);
        assert_eq!(
            default.as_deref(),
            Some(
                tmp.path()
                    .join("sys/applications/raw.desktop")
                    .to_str()
                    .unwrap()
            )
        );
        // Several types: only what opens all of them.
        let tif = Path::new("/p/B.tif");
        let (apps, _) = apps_in(&dirs, &[nef, tif], false);
        assert_eq!(
            apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Paint"]
        );
        // An unknown type offers nothing.
        assert!(
            apps_in(&dirs, &[Path::new("/p/x.unknown")], false)
                .0
                .is_empty()
        );
    }

    #[test]
    fn edit_with_appends_visible_raster_editors_after_mime_handlers() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dirs = desktop_tree(tmp.path());
        dirs.desktops = vec!["kde".into()];
        let system = dirs.data[1].join("applications");
        let write = |id: &str, fields: &str| {
            fs::write(
                system.join(format!("{id}.desktop")),
                format!("[Desktop Entry]\nType=Application\nName={id}\n{fields}"),
            )
            .unwrap();
        };
        write(
            "a-paint",
            "Exec=paint %U\nCategories=Qt;Graphics;2DGraphics;RasterGraphics;\nOnlyShowIn=KDE;\n",
        );
        write(
            "b-edit",
            "Exec=edit %u\nCategories=Graphics;2DGraphics;\nKeywords=Image Editor;\n",
        );
        write(
            "c-handler",
            "Exec=handler %f\nMimeType=image/tiff;image/jpeg;\nCategories=Graphics;RasterGraphics;\n",
        );
        write("d-viewer", "Exec=viewer\nCategories=Graphics;\n");
        write(
            "e-no-file",
            "Exec=paint %%F\nCategories=Graphics;RasterGraphics;\n",
        );
        write(
            "f-no-display",
            "Exec=paint %F\nCategories=Graphics;RasterGraphics;\nNoDisplay=true\n",
        );
        write(
            "g-other-desktop",
            "Exec=paint %F\nCategories=Graphics;RasterGraphics;\nOnlyShowIn=GNOME;\n",
        );
        write(
            "h-not-here",
            "Exec=paint %F\nCategories=Graphics;RasterGraphics;\nNotShowIn=KDE;\n",
        );
        write(
            "i-hidden",
            "Exec=paint %F\nCategories=Graphics;RasterGraphics;\nHidden=true\n",
        );
        write(
            "j-viewer",
            "Exec=viewer %F\nCategories=Graphics;2DGraphics;\nKeywords=Image Viewer;\n",
        );
        write("k-removed", "Exec=paint %F\nCategories=RasterGraphics;\n");
        write("l-shadowed", "Exec=paint %F\nCategories=RasterGraphics;\n");
        fs::write(
            dirs.data[0].join("applications/l-shadowed.desktop"),
            "[Desktop Entry]\nHidden=true\n",
        )
        .unwrap();
        fs::write(
            dirs.data[0].join("applications/mimeapps.list"),
            "[Default Applications]\nimage/jpeg=c-handler.desktop;\nimage/tiff=c-handler.desktop;\n\
             [Removed Associations]\nimage/jpeg=k-removed.desktop;\nimage/tiff=k-removed.desktop;\n",
        )
        .unwrap();

        let jpeg = Path::new("/p/photo.jpg");
        let (apps, default) = apps_in(&dirs, &[jpeg], true);
        let choices: Vec<_> = apps
            .iter()
            .map(|a| (a.name.as_str(), a.recommended))
            .collect();
        assert_eq!(
            choices,
            [("c-handler", true), ("a-paint", false), ("b-edit", false)]
        );
        let handler = system.join("c-handler.desktop");
        assert_eq!(default.as_deref().map(Path::new), Some(handler.as_path()));
        let (apps, _) = apps_in(&dirs, &[Path::new("/p/photo.tiff")], true);
        assert_eq!(
            apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["c-handler", "Paint", "a-paint", "b-edit"]
        );

        let (apps, _) = apps_in(&dirs, &[jpeg], false);
        assert_eq!(
            apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["c-handler"]
        );
    }

    #[test]
    fn a_picked_executable_is_described_checked_and_started_with_the_files() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("my editor");
        let log = tmp.path().join("args.txt");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let program = script.to_str().unwrap();
        check(program).unwrap();
        assert_eq!(describe(program).unwrap().name, "my editor");
        assert!(check("relative/tool").is_err());
        assert!(check("/no/such/tool").is_err());
        let file = tmp.path().join("a b;touch pwned.tiff");
        assert_eq!(
            open(program, std::slice::from_ref(&file), false).unwrap(),
            Launched::Picked
        );
        for _ in 0..100 {
            if log.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            format!("{}\n", file.display())
        );
        assert!(!tmp.path().join("pwned.tiff").exists());
    }
}
