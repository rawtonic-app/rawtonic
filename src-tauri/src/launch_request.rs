use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
    pub strip_creator_contact: bool,
    pub adjustments_override: Option<String>,
    /// Upper limit for files exported at the same time (`--jobs`).
    pub max_jobs: Option<usize>,
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

pub fn parse_launch_args(args: &[String]) -> LaunchRequest {
    if args.first().map(|s| s.as_str()) == Some("export") {
        let mut iter = args.iter().skip(1);

        let mut source = String::new();
        let mut output = String::new();
        let mut format = String::from("jpeg");
        let mut quality = 90;
        let mut tiff_bit_depth = TiffBitDepth::default();
        let mut color_space = OutputColorSpace::Srgb;
        let mut keep_metadata = false;
        let mut strip_creator_contact = false;
        let mut adjustments_override = None;
        let mut max_jobs = None;

        if let Some(src) = iter.next()
            && !src.starts_with('-')
        {
            source = src.clone();
        }

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--output" => {
                    if let Some(out) = iter.next() {
                        output = out.clone();
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
                "--strip-creator-contact" => strip_creator_contact = true,
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
                        adjustments_override = Some(adj.clone());
                    }
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
            strip_creator_contact,
            adjustments_override,
            max_jobs,
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
            "--edit" => edit = iter.next().cloned(),
            "--output" => output = iter.next().cloned(),
            "--format" => format = iter.next().cloned(),
            "--quality" => quality = iter.next().and_then(|q| q.parse().ok()),
            s if !s.starts_with('-') && plain.is_none() => plain = Some(s.to_string()),
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
    fn headless_exports_offer_explicit_contact_stripping() {
        for strip in [false, true] {
            let mut args = vec!["export", "/qa/source.jpg", "--output", "/qa/output.jpg"];
            if strip {
                args.push("--strip-creator-contact");
            }
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            let LaunchRequest::HeadlessExport(session) = parse_launch_args(&args) else {
                panic!("headless export");
            };
            assert_eq!(session.strip_creator_contact, strip);
        }
    }
}
