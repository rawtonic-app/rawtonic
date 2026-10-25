use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tauri::Emitter;

use crate::export_processing::TiffBitDepth;
use rawtonic_engine::output_color_space::OutputColorSpace;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalEditSession {
    pub source: String,
    pub output: String,
    pub format: String,
    pub jpeg_quality: u8,
}

#[derive(Clone, Debug)]
pub struct HeadlessExportSession {
    pub source: String,
    pub output: String,
    pub format: String,
    pub quality: u8,
    pub tiff_bit_depth: TiffBitDepth,
    pub color_space: OutputColorSpace,
    pub keep_metadata: bool,
    pub keep_creator_contact: bool,
    pub adjustments_override: Option<String>,
    /// Upper limit for files exported at the same time (`--jobs`).
    pub max_jobs: Option<usize>,
    /// A JSON file of recipes (`--recipes`) exported together instead of
    /// the format and settings above, as ticked recipes in the app.
    pub recipes: Option<String>,
}

#[derive(Clone, Debug)]
pub enum LaunchRequest {
    None,
    OpenFile(String),
    EditSession(ExternalEditSession),
    HeadlessExport(HeadlessExportSession),
    InvalidHeadless(String),
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPayload {
    pub open_with_file: Option<String>,
    pub edit_session: Option<ExternalEditSession>,
}

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

/// Path arguments as given, relative ones joined to `base` (the folder the
/// command was run in), so they never resolve against the app's own folder.
pub fn parse_launch_args(args: &[String], base: Option<&Path>) -> LaunchRequest {
    let absolute = |path: String| -> String {
        match base {
            Some(base) if !path.is_empty() && Path::new(&path).is_relative() => {
                base.join(&path).to_string_lossy().into_owned()
            }
            _ => path,
        }
    };
    if args.first().map(|s| s.as_str()) == Some("export") {
        let mut iter = args.iter().skip(1);

        let mut source = String::new();
        let mut output = String::new();
        let mut format = String::from("jpeg");
        let mut quality = 90;
        let mut tiff_bit_depth = TiffBitDepth::default();
        let mut color_space = OutputColorSpace::Srgb;
        let mut keep_metadata = false;
        let mut keep_creator_contact = false;
        let mut adjustments_override = None;
        let mut max_jobs = None;
        let mut recipes = None;

        if let Some(src) = iter.next()
            && !src.starts_with('-')
        {
            source = absolute(src.clone());
        }

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output" => {
                    if let Some(out) = iter.next() {
                        output = absolute(out.clone());
                    }
                }
                "--format" => {
                    if let Some(fmt) = iter.next() {
                        format = fmt.clone();
                    }
                }
                "--quality" => {
                    if let Some(q) = iter.next() {
                        quality = q.parse().unwrap_or(90);
                    }
                }
                "--tiff-bit-depth" => {
                    let Some(value) = iter.next() else {
                        return LaunchRequest::InvalidHeadless(
                            "Missing value for --tiff-bit-depth; expected 8 or 16.".to_string(),
                        );
                    };
                    let Ok(value) = value.parse::<u8>() else {
                        return LaunchRequest::InvalidHeadless(format!(
                            "Invalid TIFF bit depth '{}'; expected 8 or 16.",
                            value
                        ));
                    };
                    let Ok(value) = TiffBitDepth::try_from(value) else {
                        return LaunchRequest::InvalidHeadless(format!(
                            "Invalid TIFF bit depth '{}'; expected 8 or 16.",
                            value
                        ));
                    };
                    tiff_bit_depth = value;
                }
                "--color-space" => {
                    let Some(value) = iter.next() else {
                        return LaunchRequest::InvalidHeadless(
                            "Missing value for --color-space; expected srgb, display-p3, adobe-rgb or prophoto.".to_string(),
                        );
                    };
                    let Some(value) = OutputColorSpace::parse(value) else {
                        return LaunchRequest::InvalidHeadless(format!(
                            "Invalid colour space '{}'; expected srgb, display-p3, adobe-rgb or prophoto.",
                            value
                        ));
                    };
                    color_space = value;
                }
                "--keep-metadata" => keep_metadata = true,
                "--keep-creator-contact" => keep_creator_contact = true,
                "--jobs" => {
                    let Some(value) = iter.next() else {
                        return LaunchRequest::InvalidHeadless(
                            "Missing value for --jobs; expected a positive number.".to_string(),
                        );
                    };
                    match value.parse::<usize>() {
                        Ok(jobs) if jobs > 0 => max_jobs = Some(jobs),
                        _ => {
                            return LaunchRequest::InvalidHeadless(format!(
                                "Invalid --jobs value '{}'; expected a positive number.",
                                value
                            ));
                        }
                    }
                }
                "--adjustments" => {
                    if let Some(adj) = iter.next() {
                        adjustments_override = Some(absolute(adj.clone()));
                    }
                }
                "--recipes" => {
                    let Some(file) = iter.next() else {
                        return LaunchRequest::InvalidHeadless(
                            "Missing value for --recipes; expected a JSON file.".to_string(),
                        );
                    };
                    recipes = Some(absolute(file.clone()));
                }
                _ => {}
            }
        }

        return LaunchRequest::HeadlessExport(HeadlessExportSession {
            source,
            output,
            format,
            quality,
            tiff_bit_depth,
            color_space,
            keep_metadata,
            keep_creator_contact,
            adjustments_override,
            max_jobs,
            recipes,
        });
    }

    let mut edit: Option<String> = None;
    let mut output: Option<String> = None;
    let mut format: Option<String> = None;
    let mut quality: Option<u8> = None;
    let mut plain: Option<String> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--edit" => edit = iter.next().cloned().map(absolute),
            "--output" => output = iter.next().cloned().map(absolute),
            "--format" => format = iter.next().cloned(),
            "--quality" => quality = iter.next().and_then(|q| q.parse().ok()),
            s if !s.starts_with('-') && plain.is_none() => plain = Some(absolute(s.to_string())),
            _ => {}
        }
    }

    match (edit, output) {
        (Some(source), Some(output)) => {
            let format = format.unwrap_or_else(|| {
                std::path::Path::new(&output)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_else(|| "jpg".to_string())
            });
            let format = match format.as_str() {
                "tif" => "tiff".to_string(),
                _ => format,
            };
            LaunchRequest::EditSession(ExternalEditSession {
                source,
                output,
                format,
                jpeg_quality: quality.unwrap_or(90),
            })
        }
        (Some(source), None) => LaunchRequest::OpenFile(source),
        _ => match plain {
            Some(path) => LaunchRequest::OpenFile(path),
            None => LaunchRequest::None,
        },
    }
}

fn handle_file_open(app_handle: &tauri::AppHandle, path: PathBuf) {
    if let Some(path_str) = path.to_str()
        && let Err(e) = app_handle.emit("open-with-file", path_str)
    {
        log::error!("Failed to emit open-with-file event: {}", e);
    }
}

pub fn emit_launch_request(app_handle: &tauri::AppHandle, request: LaunchRequest) {
    match request {
        LaunchRequest::EditSession(session) => {
            if let Err(e) = app_handle.emit("external-edit-session", &session) {
                log::error!("Failed to emit external-edit-session event: {}", e);
            }
        }
        LaunchRequest::OpenFile(path) => {
            handle_file_open(app_handle, PathBuf::from(path));
        }
        LaunchRequest::HeadlessExport(_) => {
            println!(
                "Error: Headless export cannot be attached to an already running GUI instance."
            );
        }
        LaunchRequest::InvalidHeadless(error) => {
            log::error!("Invalid headless export request: {}", error);
        }
        LaunchRequest::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_exports_require_explicit_contact_retention() {
        for keep in [false, true] {
            let mut args = vec!["export", "/qa/source.jpg", "--output", "/qa/output.jpg"];
            if keep {
                args.push("--keep-creator-contact");
            }
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            let LaunchRequest::HeadlessExport(session) = parse_launch_args(&args, None) else {
                panic!("headless export");
            };
            assert_eq!(session.keep_creator_contact, keep);
        }
    }

    #[test]
    fn headless_exports_take_a_recipes_file() {
        let args = |extra: &[&str]| -> Vec<String> {
            ["export", "/qa/Capture", "--output", "/qa/Output"]
                .iter()
                .chain(extra)
                .map(|s| s.to_string())
                .collect()
        };
        let LaunchRequest::HeadlessExport(session) =
            parse_launch_args(&args(&["--recipes", "/qa/recipes.json"]), None)
        else {
            panic!("headless export");
        };
        assert_eq!(session.recipes.as_deref(), Some("/qa/recipes.json"));
        let LaunchRequest::HeadlessExport(session) = parse_launch_args(&args(&[]), None) else {
            panic!("headless export");
        };
        assert_eq!(session.recipes, None);
        assert!(matches!(
            parse_launch_args(&args(&["--recipes"]), None),
            LaunchRequest::InvalidHeadless(_)
        ));
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_appimage_resolves_against_the_folder_it_was_run_in() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let mount = Some(PathBuf::from("/tmp/.mount_Rawton/usr"));
        let photos = Some(PathBuf::from("/home/u/photos"));
        const APPIMAGE: (&str, &str) = ("APPIMAGE", "/home/u/Rawtonic.AppImage");
        let owd = env(&[
            APPIMAGE,
            ("OWD", "/home/u/photos"),
            ("PWD", "/home/u/other"),
        ]);
        assert_eq!(invocation_dir(owd, mount.clone()), photos);
        // --appimage-extract-and-run: no OWD, the shell's PWD.
        let extracted = env(&[APPIMAGE, ("PWD", "/home/u/photos")]);
        assert_eq!(invocation_dir(extracted, mount.clone()), photos);
        // Neither, or not absolute: nothing better than the working directory.
        assert_eq!(invocation_dir(env(&[APPIMAGE]), mount.clone()), mount);
        let relative = env(&[APPIMAGE, ("OWD", "photos"), ("PWD", "photos")]);
        assert_eq!(invocation_dir(relative, mount.clone()), mount);
        // A plain binary or a macOS bundle: the working directory, even with
        // OWD or PWD in the environment.
        let plain = env(&[("OWD", "/elsewhere"), ("PWD", "/elsewhere")]);
        assert_eq!(invocation_dir(plain, mount.clone()), mount);
        assert_eq!(invocation_dir(env(&[]), None), None);
    }

    #[test]
    fn relative_path_arguments_join_the_invocation_folder() {
        let base = Path::new("/home/u/photos");
        let args = strings(&[
            "export",
            "a.ARW",
            "--output",
            "out/a.jpg",
            "--adjustments",
            "../look.json",
            "--recipes",
            "recipes.json",
        ]);
        let LaunchRequest::HeadlessExport(session) = parse_launch_args(&args, Some(base)) else {
            panic!("headless export");
        };
        assert_eq!(session.source, "/home/u/photos/a.ARW");
        assert_eq!(session.output, "/home/u/photos/out/a.jpg");
        assert_eq!(
            session.adjustments_override.as_deref(),
            Some("/home/u/photos/../look.json")
        );
        assert_eq!(
            session.recipes.as_deref(),
            Some("/home/u/photos/recipes.json")
        );

        let args = strings(&["export", "/abs/a.ARW", "--output", "/abs/out"]);
        let LaunchRequest::HeadlessExport(session) = parse_launch_args(&args, Some(base)) else {
            panic!("headless export");
        };
        assert_eq!(
            (session.source.as_str(), session.output.as_str()),
            ("/abs/a.ARW", "/abs/out")
        );

        // A missing --output stays empty, not the folder itself.
        let LaunchRequest::HeadlessExport(session) =
            parse_launch_args(&strings(&["export", "a.ARW"]), Some(base))
        else {
            panic!("headless export");
        };
        assert_eq!(session.output, "");

        let args = strings(&["--edit", "a.tif", "--output", "b.tif"]);
        let LaunchRequest::EditSession(edit) = parse_launch_args(&args, Some(base)) else {
            panic!("edit session");
        };
        assert_eq!(
            (edit.source.as_str(), edit.output.as_str()),
            ("/home/u/photos/a.tif", "/home/u/photos/b.tif")
        );
        assert_eq!(edit.format, "tiff");

        let LaunchRequest::OpenFile(path) = parse_launch_args(&strings(&["a.jpg"]), Some(base))
        else {
            panic!("open file");
        };
        assert_eq!(path, "/home/u/photos/a.jpg");
    }
}
