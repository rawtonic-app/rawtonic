use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::formats::is_raw_file;
use crate::output_color_space::OutputColorSpace;
use chrono::{DateTime, Local, LocalResult, NaiveDateTime, TimeZone, Utc};
use exif::Exif;
use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::ifd::ExifTagGroup;
use little_exif::metadata::Metadata;
use little_exif::rational::{iR64, uR64};
use rawler::decoders::RawMetadata;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone)]
struct CachedExifEntry {
    mtime_ns: u128,
    size: u64,
    data: HashMap<String, String>,
}

struct ExifCacheState {
    cache: HashMap<PathBuf, HashMap<String, CachedExifEntry>>,
    dirty: HashSet<PathBuf>,
    cache_dir: Option<PathBuf>,
}

impl ExifCacheState {
    fn get_cache_file_path(&self, folder: &Path) -> Option<PathBuf> {
        let base_dir = self.cache_dir.as_ref()?;
        let hash = blake3::hash(folder.to_string_lossy().as_bytes())
            .to_hex()
            .to_string();
        Some(base_dir.join(format!("{}.json", hash)))
    }
}

fn get_exif_cache() -> &'static Mutex<ExifCacheState> {
    static EXIF_CACHE: OnceLock<Mutex<ExifCacheState>> = OnceLock::new();
    EXIF_CACHE.get_or_init(|| {
        std::thread::spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(3));
                flush_all_dirty_caches();
            }
        });

        Mutex::new(ExifCacheState {
            cache: HashMap::new(),
            dirty: HashSet::new(),
            cache_dir: None,
        })
    })
}

pub fn initialize_cache_dir(cache_dir: PathBuf) {
    let dir = cache_dir.join("exif");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut state) = get_exif_cache().lock() {
        state.cache_dir = Some(dir);
    }
}

pub fn flush_all_dirty_caches() {
    let mut state = match get_exif_cache().lock() {
        Ok(s) => s,
        Err(_) => return,
    };

    let dirty_folders: Vec<PathBuf> = state.dirty.drain().collect();
    let mut to_write = Vec::new();

    for folder in &dirty_folders {
        if let Some(folder_map) = state.cache.get(folder)
            && !folder_map.is_empty()
            && let Some(cache_path) = state.get_cache_file_path(folder)
        {
            to_write.push((cache_path, folder_map.clone()));
        }
    }

    drop(state);

    for (path, map) in to_write {
        if let Ok(json) = serde_json::to_string(&map) {
            let tmp_path = path.with_extension("tmp");
            if std::fs::write(&tmp_path, json).is_ok() {
                let _ = std::fs::rename(tmp_path, path);
            }
        }
    }
}

pub fn load_rrcache_for_folder(folder: &Path) {
    let mut cache_path = None;

    if let Ok(state) = get_exif_cache().lock() {
        if state.cache.contains_key(folder) {
            return;
        }
        cache_path = state.get_cache_file_path(folder);
    }

    let Some(path) = cache_path else {
        return;
    };

    let loaded_map = if let Ok(content) = std::fs::read_to_string(&path) {
        serde_json::from_str::<HashMap<String, CachedExifEntry>>(&content).unwrap_or_default()
    } else {
        HashMap::new()
    };

    if let Ok(mut state) = get_exif_cache().lock() {
        state
            .cache
            .entry(folder.to_path_buf())
            .or_insert(loaded_map);
    }
}

pub fn get_file_stamp(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((mtime_ns, meta.len()))
}

pub fn get_exif_from_rrcache(image_path: &Path) -> Option<HashMap<String, String>> {
    let folder = image_path.parent()?;
    let filename = image_path.file_name()?.to_string_lossy().to_string();

    let (current_mtime, current_size) = get_file_stamp(image_path)?;

    load_rrcache_for_folder(folder);

    let mut state = get_exif_cache().lock().ok()?;
    let folder_map = state.cache.get_mut(folder)?;

    if let Some(entry) = folder_map.get(&filename) {
        if entry.mtime_ns == current_mtime && entry.size == current_size {
            return Some(entry.data.clone());
        }

        folder_map.remove(&filename);
        state.dirty.insert(folder.to_path_buf());
    }

    None
}

pub fn save_exif_to_rrcache(image_path: &Path, exif: HashMap<String, String>) {
    let Some(folder) = image_path.parent() else {
        return;
    };
    let Some(filename) = image_path.file_name() else {
        return;
    };

    let Some((mtime_ns, size)) = get_file_stamp(image_path) else {
        return;
    };

    load_rrcache_for_folder(folder);

    let mut state = match get_exif_cache().lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    let folder_map = state.cache.entry(folder.to_path_buf()).or_default();
    folder_map.insert(
        filename.to_string_lossy().to_string(),
        CachedExifEntry {
            mtime_ns,
            size,
            data: exif,
        },
    );
    state.dirty.insert(folder.to_path_buf());
}

pub fn truncate_large_exif(value: &str) -> String {
    if value.len() <= 500 {
        return value.to_string();
    }

    let mut start_idx = 200;
    while !value.is_char_boundary(start_idx) {
        start_idx -= 1;
    }

    let mut end_idx = value.len() - 200;
    while !value.is_char_boundary(end_idx) {
        end_idx += 1;
    }

    if start_idx < end_idx {
        let start_str = &value[..start_idx];
        let end_str = &value[end_idx..];
        return format!("{}...{}", start_str, end_str);
    }

    value.to_string()
}

pub fn to_ur64(val: &exif::Rational) -> uR64 {
    uR64 {
        nominator: val.num,
        denominator: val.denom,
    }
}

pub fn to_ir64(val: &exif::SRational) -> iR64 {
    iR64 {
        nominator: val.num,
        denominator: val.denom,
    }
}

pub fn clean_creation_datetime_str(s: &str) -> &str {
    s.trim().trim_matches('"').trim_matches('\'').trim()
}

pub fn fmt_date_str(s: String) -> String {
    if let Some(dt) = parse_creation_datetime(&s) {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    clean_creation_datetime_str(&s).to_string()
}

/// EXIF date fields need "YYYY:MM:DD HH:MM:SS"; sidecar values use dashes for
/// display, and readers such as ImageIO drop dates in that form.
pub fn exif_datetime(s: &str) -> String {
    match parse_creation_datetime(s) {
        Some(dt) => dt.format("%Y:%m:%d %H:%M:%S").to_string(),
        None => clean_creation_datetime_str(s).to_string(),
    }
}

pub fn normalize_creation_datetime(s: &str) -> Option<String> {
    let normalized = s.replace('T', " ");
    let (date, time) = normalized.split_once(' ')?;
    Some(format!("{} {}", date.replace(':', "-"), time))
}

pub fn parse_creation_datetime(s: &str) -> Option<NaiveDateTime> {
    let clean = clean_creation_datetime_str(s);
    if clean.is_empty() {
        return None;
    }

    let normalized = normalize_creation_datetime(clean);
    for candidate in std::iter::once(clean).chain(normalized.as_deref()) {
        for format in [
            "%Y:%m:%d %H:%M:%S",
            "%Y:%m:%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M:%S%.f",
        ] {
            if let Ok(dt) = NaiveDateTime::parse_from_str(candidate, format) {
                return Some(dt);
            }
        }
    }

    None
}

pub fn creation_datetime_to_utc(dt: NaiveDateTime) -> DateTime<Utc> {
    match Local.from_local_datetime(&dt) {
        LocalResult::Single(local_dt) | LocalResult::Ambiguous(local_dt, _) => {
            local_dt.with_timezone(&Utc)
        }
        LocalResult::None => DateTime::from_naive_utc_and_offset(dt, Utc),
    }
}

pub fn parse_creation_field(field: &exif::Field) -> Option<DateTime<Utc>> {
    parse_creation_datetime(&field.display_value().to_string()).map(creation_datetime_to_utc)
}

pub fn parse_raw_creation_date(date_str: Option<&str>) -> Option<DateTime<Utc>> {
    parse_creation_datetime(date_str?).map(creation_datetime_to_utc)
}

/// The text of an EXIF UserComment (`Undefined`: an 8-byte character code,
/// then the text), or `None` when it holds no text — cameras often fill it
/// with zeros or spaces, which would otherwise show as a hex string.
pub fn user_comment_text(value: &exif::Value, little_endian: bool) -> Option<String> {
    let exif::Value::Undefined(bytes, _) = value else {
        return clean_ascii_value(value);
    };
    let (code, text) = bytes.split_at(bytes.len().min(8));
    let text = if code.starts_with(b"UNICODE") {
        let units: Vec<u16> = text
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| {
                if little_endian {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(text).into_owned()
    };
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!text.is_empty()).then(|| text.to_string())
}

pub fn clean_ascii_value(value: &exif::Value) -> Option<String> {
    let exif::Value::Ascii(ref components) = *value else {
        return None;
    };

    let cleaned: Vec<String> = components
        .iter()
        .map(|c| {
            String::from_utf8_lossy(c)
                .trim_matches(char::from(0))
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.join(" "))
    }
}

pub fn rational_to_f32_checked(r: &exif::Rational) -> Option<f32> {
    if r.denom == 0 {
        None
    } else {
        Some(r.num as f32 / r.denom as f32)
    }
}

pub fn rawler_rational_to_f32_checked(r: &rawler::formats::tiff::Rational) -> Option<f32> {
    if r.d == 0 {
        None
    } else {
        Some(r.n as f32 / r.d as f32)
    }
}

pub fn format_min_max(min: f32, max: f32, tolerance: f32) -> String {
    if (min - max).abs() < tolerance {
        format!("{min}")
    } else {
        format!("{min}-{max}")
    }
}

pub fn format_lens_specification(components: &[exif::Rational]) -> Option<String> {
    if components.len() < 4 {
        return None;
    }

    let focal_min = rational_to_f32_checked(&components[0]);
    let focal_max = rational_to_f32_checked(&components[1]);
    let (focal_min, focal_max) = match (focal_min, focal_max) {
        (Some(min), Some(max)) => (min, max),
        _ => return None,
    };

    let mut spec = format!("{} mm", format_min_max(focal_min, focal_max, 0.01));

    let aperture_min = rational_to_f32_checked(&components[2]);
    let aperture_max = rational_to_f32_checked(&components[3]);
    if let (Some(amin), Some(amax)) = (aperture_min, aperture_max) {
        spec.push_str(&format!(", f/{}", format_min_max(amin, amax, 0.01)));
    }

    Some(spec)
}

pub fn read_exif(file_bytes: &[u8]) -> Option<Exif> {
    let exifreader = exif::Reader::new();
    exifreader
        .read_from_container(&mut Cursor::new(file_bytes))
        .ok()
}

pub fn read_raw_metadata(file_bytes: &[u8]) -> Option<RawMetadata> {
    let loader = rawler::RawLoader::new();
    let raw_source = rawler::rawsource::RawSource::new_from_slice(file_bytes);
    let decoder = loader.get_decoder(&raw_source).ok()?;
    decoder.raw_metadata(&raw_source, &Default::default()).ok()
}

pub fn extract_metadata(file_bytes: &[u8]) -> Option<HashMap<String, String>> {
    let mut map = HashMap::new();

    if let Some(exif_obj) = read_exif(file_bytes) {
        for field in exif_obj.fields() {
            match field.tag {
                exif::Tag::ExposureTime => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let r = &v[0];
                        if r.num == 1 && r.denom > 1 {
                            map.insert("ExposureTime".to_string(), format!("1/{} s", r.denom));
                        } else {
                            let val = r.num as f32 / r.denom as f32;
                            if val < 1.0 && val > 0.0 {
                                map.insert(
                                    "ExposureTime".to_string(),
                                    format!("1/{} s", (1.0 / val).round()),
                                );
                            } else {
                                map.insert("ExposureTime".to_string(), format!("{} s", val));
                            }
                        }
                    }
                }
                exif::Tag::ShutterSpeedValue => {
                    if let exif::Value::SRational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("ShutterSpeedValue".to_string(), val.to_string());
                    }
                }
                exif::Tag::FNumber => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("FNumber".to_string(), format!("f/{}", val));
                    }
                }
                exif::Tag::ApertureValue => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("ApertureValue".to_string(), format!("f/{}", val));
                    }
                }
                exif::Tag::FocalLength => {
                    if let exif::Value::Rational(ref v) = field.value
                        && !v.is_empty()
                    {
                        let val = v[0].num as f32 / v[0].denom as f32;
                        map.insert("FocalLength".to_string(), val.to_string());
                    }
                }
                exif::Tag::PhotographicSensitivity | exif::Tag::ISOSpeed => {
                    map.insert(
                        "PhotographicSensitivity".to_string(),
                        field.display_value().to_string(),
                    );
                    map.insert("ISOSpeed".to_string(), field.display_value().to_string());
                }
                exif::Tag::DateTimeOriginal => {
                    map.insert(
                        "DateTimeOriginal".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                exif::Tag::DateTime => {
                    map.insert(
                        "CreateDate".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                exif::Tag::DateTimeDigitized => {
                    map.insert(
                        "ModifyDate".to_string(),
                        fmt_date_str(field.display_value().to_string()),
                    );
                }
                exif::Tag::LensSpecification => {
                    if let exif::Value::Rational(ref v) = field.value
                        && v.len() >= 4
                        && let (Some(focal_min), Some(focal_max)) = (
                            rational_to_f32_checked(&v[0]),
                            rational_to_f32_checked(&v[1]),
                        )
                    {
                        let mut spec = format!("{} mm", format_min_max(focal_min, focal_max, 0.01));

                        let aperture = match (
                            rational_to_f32_checked(&v[2]),
                            rational_to_f32_checked(&v[3]),
                        ) {
                            (Some(amin), Some(amax)) => Some((amin, amax)),
                            _ => read_raw_metadata(file_bytes).and_then(|meta| {
                                let lens_desc = meta.lens?;
                                let amin =
                                    rawler_rational_to_f32_checked(&lens_desc.aperture_range[0])?;
                                let amax =
                                    rawler_rational_to_f32_checked(&lens_desc.aperture_range[1])?;
                                Some((amin, amax))
                            }),
                        };

                        if let Some((amin, amax)) = aperture
                            && (amin > 0.0 || amax > 0.0)
                        {
                            spec.push_str(&format!(", f/{}", format_min_max(amin, amax, 0.01)));
                        }

                        map.insert("LensSpecification".to_string(), spec);
                    }
                }
                exif::Tag::UserComment => {
                    if let Some(val) = user_comment_text(&field.value, exif_obj.little_endian()) {
                        map.insert(field.tag.to_string(), truncate_large_exif(&val));
                    }
                }
                _ => match &field.value {
                    exif::Value::Ascii(_) => {
                        if let Some(val) = clean_ascii_value(&field.value) {
                            map.insert(field.tag.to_string(), truncate_large_exif(&val));
                        }
                    }
                    _ => {
                        let val = field.display_value().with_unit(&exif_obj).to_string();
                        if !val.trim().is_empty() {
                            map.insert(field.tag.to_string(), truncate_large_exif(&val));
                        }
                    }
                },
            }
        }
    }

    if !map.is_empty() {
        return Some(map);
    }

    let metadata = read_raw_metadata(file_bytes)?;

    let exif = metadata.exif;

    let fmt_rat = |r: &rawler::formats::tiff::Rational| -> f32 {
        if r.d == 0 {
            0.0
        } else {
            r.n as f32 / r.d as f32
        }
    };

    let fmt_srat = |r: &rawler::formats::tiff::SRational| -> f32 {
        if r.d == 0 {
            0.0
        } else {
            r.n as f32 / r.d as f32
        }
    };

    let mut insert_if_present = |key: &str, val: String| {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            map.insert(key.to_string(), truncate_large_exif(trimmed));
        }
    };

    insert_if_present("Make", metadata.make);
    insert_if_present("Model", metadata.model);

    if let Some(v) = exif.artist {
        insert_if_present("Artist", v);
    }
    if let Some(v) = exif.copyright {
        insert_if_present("Copyright", v);
    }
    if let Some(v) = exif.owner_name {
        insert_if_present("OwnerName", v);
    }
    if let Some(v) = exif.serial_number {
        insert_if_present("SerialNumber", v);
    }
    if let Some(v) = exif.image_number {
        insert_if_present("ImageNumber", v.to_string());
    }
    if let Some(v) = exif.user_comment {
        insert_if_present("UserComment", v.trim_matches('\0').to_string());
    }

    if let Some(v) = exif.date_time_original {
        insert_if_present("DateTimeOriginal", fmt_date_str(v));
    }
    if let Some(v) = exif.create_date {
        insert_if_present("CreateDate", fmt_date_str(v));
    }
    if let Some(v) = exif.modify_date {
        insert_if_present("ModifyDate", fmt_date_str(v));
    }

    if let Some(v) = exif.offset_time {
        insert_if_present("OffsetTime", v);
    }
    if let Some(v) = exif.offset_time_original {
        insert_if_present("OffsetTimeOriginal", v);
    }
    if let Some(v) = exif.offset_time_digitized {
        insert_if_present("OffsetTimeDigitized", v);
    }
    if let Some(v) = exif.sub_sec_time {
        insert_if_present("SubSecTime", v);
    }
    if let Some(v) = exif.sub_sec_time_original {
        insert_if_present("SubSecTimeOriginal", v);
    }
    if let Some(v) = exif.sub_sec_time_digitized {
        insert_if_present("SubSecTimeDigitized", v);
    }

    if let Some(v) = exif.lens_model {
        insert_if_present("LensModel", v);
    } else if let Some(lens_desc) = &metadata.lens {
        insert_if_present("LensModel", lens_desc.lens_model.clone());
    }

    if let Some(v) = exif.lens_make {
        insert_if_present("LensMake", v);
    } else if let Some(lens_desc) = &metadata.lens {
        insert_if_present("LensMake", lens_desc.lens_make.clone());
    }

    if let Some(v) = exif.lens_serial_number {
        insert_if_present("LensSerialNumber", v);
    }

    if let Some(lens_desc) = &metadata.lens {
        let focal_min = fmt_rat(&lens_desc.focal_range[0]);
        let focal_max = fmt_rat(&lens_desc.focal_range[1]);
        let mut spec = format!("{} mm", format_min_max(focal_min, focal_max, 0.01));

        let aperture_min = fmt_rat(&lens_desc.aperture_range[0]);
        let aperture_max = fmt_rat(&lens_desc.aperture_range[1]);
        if aperture_min > 0.0 || aperture_max > 0.0 {
            spec.push_str(&format!(
                ", f/{}",
                format_min_max(aperture_min, aperture_max, 0.01)
            ));
        }

        insert_if_present("LensSpecification", spec);
    }

    if let Some(v) = exif.orientation {
        insert_if_present("Orientation", v.to_string());
    }

    if let Some(r) = exif.fnumber {
        let val = fmt_rat(&r);
        insert_if_present("FNumber", format!("f/{}", val));
    }

    if let Some(r) = exif.aperture_value {
        let val = fmt_rat(&r);
        insert_if_present("ApertureValue", format!("f/{}", val));
    }

    if let Some(r) = exif.max_aperture_value {
        insert_if_present("MaxApertureValue", fmt_rat(&r).to_string());
    }

    if let Some(r) = exif.exposure_time {
        if r.n == 1 && r.d > 1 {
            insert_if_present("ExposureTime", format!("1/{} s", r.d));
        } else {
            let val = fmt_rat(&r);
            if val < 1.0 && val > 0.0 {
                insert_if_present("ExposureTime", format!("1/{} s", (1.0 / val).round()));
            } else {
                insert_if_present("ExposureTime", format!("{} s", val));
            }
        }
    }

    if let Some(r) = exif.shutter_speed_value {
        insert_if_present("ShutterSpeedValue", fmt_srat(&r).to_string());
    }

    if let Some(v) = exif.iso_speed {
        insert_if_present("PhotographicSensitivity", v.to_string());
        insert_if_present("ISOSpeed", v.to_string());
    } else if let Some(v) = exif.iso_speed_ratings {
        insert_if_present("PhotographicSensitivity", v.to_string());
        insert_if_present("ISOSpeedRatings", v.to_string());
    }

    if let Some(v) = exif.recommended_exposure_index {
        insert_if_present("RecommendedExposureIndex", v.to_string());
    }
    if let Some(v) = exif.sensitivity_type {
        insert_if_present("SensitivityType", v.to_string());
    }

    if let Some(r) = exif.focal_length {
        let val = fmt_rat(&r);
        insert_if_present("FocalLength", val.to_string());
    }

    if let Some(r) = exif.exposure_bias {
        insert_if_present("ExposureBiasValue", fmt_srat(&r).to_string());
    }

    if let Some(v) = exif.metering_mode {
        insert_if_present("MeteringMode", v.to_string());
    }
    if let Some(v) = exif.light_source {
        insert_if_present("LightSource", v.to_string());
    }
    if let Some(v) = exif.flash {
        insert_if_present("Flash", v.to_string());
    }
    if let Some(v) = exif.white_balance {
        insert_if_present("WhiteBalance", v.to_string());
    }
    if let Some(v) = exif.exposure_program {
        insert_if_present("ExposureProgram", v.to_string());
    }
    if let Some(v) = exif.exposure_mode {
        insert_if_present("ExposureMode", v.to_string());
    }
    if let Some(v) = exif.scene_capture_type {
        insert_if_present("SceneCaptureType", v.to_string());
    }
    if let Some(v) = exif.color_space {
        insert_if_present("ColorSpace", v.to_string());
    }
    if let Some(r) = exif.flash_energy {
        insert_if_present("FlashEnergy", fmt_rat(&r).to_string());
    }
    if let Some(r) = exif.brightness_value {
        insert_if_present("BrightnessValue", fmt_srat(&r).to_string());
    }

    if let Some(r) = exif.subject_distance {
        insert_if_present("SubjectDistance", fmt_rat(&r).to_string());
    }
    if let Some(v) = exif.subject_distance_range {
        insert_if_present("SubjectDistanceRange", v.to_string());
    }

    if let Some(gps) = exif.gps {
        let fmt_gps_coord = |coords: &[rawler::formats::tiff::Rational; 3]| -> String {
            format!(
                "{} deg {} min {} sec",
                fmt_rat(&coords[0]),
                fmt_rat(&coords[1]),
                fmt_rat(&coords[2])
            )
        };

        if let Some(lat) = gps.gps_latitude {
            insert_if_present("GPSLatitude", fmt_gps_coord(&lat));
        }
        if let Some(lat_ref) = gps.gps_latitude_ref {
            insert_if_present("GPSLatitudeRef", lat_ref);
        }
        if let Some(lon) = gps.gps_longitude {
            insert_if_present("GPSLongitude", fmt_gps_coord(&lon));
        }
        if let Some(lon_ref) = gps.gps_longitude_ref {
            insert_if_present("GPSLongitudeRef", lon_ref);
        }
        if let Some(alt) = gps.gps_altitude {
            insert_if_present("GPSAltitude", fmt_rat(&alt).to_string());
        }
        if let Some(alt_ref) = gps.gps_altitude_ref {
            insert_if_present("GPSAltitudeRef", alt_ref.to_string());
        }
        if let Some(v) = gps.gps_img_direction {
            insert_if_present("GPSImgDirection", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_img_direction_ref {
            insert_if_present("GPSImgDirectionRef", v);
        }
        if let Some(v) = gps.gps_speed {
            insert_if_present("GPSSpeed", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_speed_ref {
            insert_if_present("GPSSpeedRef", v);
        }
        if let Some(v) = gps.gps_status {
            insert_if_present("GPSStatus", v);
        }
        if let Some(v) = gps.gps_measure_mode {
            insert_if_present("GPSMeasureMode", v);
        }
        if let Some(v) = gps.gps_dop {
            insert_if_present("GPSDOP", fmt_rat(&v).to_string());
        }
        if let Some(v) = gps.gps_map_datum {
            insert_if_present("GPSMapDatum", v);
        }
    }

    Some(map)
}

/// The capture date from the cached EXIF map, else the file's EXIF, else a
/// RAW decoder's metadata.
pub fn try_capture_date_with(
    path: &Path,
    exif_sidecar: &dyn Fn() -> Option<HashMap<String, String>>,
) -> Option<DateTime<Utc>> {
    if let Some(map) = exif_sidecar()
        && let Some(dt_str) = map.get("DateTimeOriginal").or(map.get("CreateDate"))
        && let Some(dt) = parse_creation_datetime(dt_str)
    {
        return Some(creation_datetime_to_utc(dt));
    }

    if let Ok(file) = std::fs::File::open(path) {
        let mut bufreader = BufReader::new(&file);
        let exifreader = exif::Reader::new();

        if let Ok(exif_obj) = exifreader.read_from_container(&mut bufreader) {
            for tag in [exif::Tag::DateTimeOriginal, exif::Tag::DateTime] {
                if let Some(field) = exif_obj.get_field(tag, exif::In::PRIMARY)
                    && let Some(dt) = parse_creation_field(field)
                {
                    return Some(dt);
                }
            }
        }
    }

    if is_raw_file(path) {
        let loader = rawler::RawLoader::new();
        if let Ok(raw_source) = rawler::rawsource::RawSource::new(path)
            && let Ok(decoder) = loader.get_decoder(&raw_source)
            && let Ok(metadata) = decoder.raw_metadata(&raw_source, &Default::default())
        {
            if let Some(dt) = parse_raw_creation_date(metadata.exif.date_time_original.as_deref()) {
                return Some(dt);
            }
            if let Some(dt) = parse_raw_creation_date(metadata.exif.create_date.as_deref()) {
                return Some(dt);
            }
        }
    }

    None
}

/// The capture date, else the file's creation time, else now.
pub fn capture_date_with(
    path: &Path,
    exif_sidecar: &dyn Fn() -> Option<HashMap<String, String>>,
) -> DateTime<Utc> {
    if let Some(dt) = try_capture_date_with(path, exif_sidecar) {
        return dt;
    }

    fs::metadata(path)
        .ok()
        .and_then(|m| m.created().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now)
}

pub fn copy_full_exif_from_source(
    metadata: &mut Metadata,
    original_path: &Path,
    strip_gps: bool,
) -> bool {
    let Ok(source_metadata) = Metadata::new_from_path(original_path) else {
        return false;
    };

    let mut copied_any = false;
    for ifd in source_metadata.get_ifds() {
        if ifd.get_generic_ifd_nr() != 0 {
            continue;
        }
        if strip_gps && ifd.get_ifd_type() == ExifTagGroup::GPS {
            continue;
        }
        for tag in ifd.get_tags() {
            metadata.set_tag(tag.clone());
            copied_any = true;
        }
    }
    copied_any
}

/// Marks an EXIF map whose capture time the library corrected (Shift
/// Capture Time): its `DateTimeOriginal`, `SubSecTimeOriginal` and
/// `OffsetTimeOriginal` are the corrected ones, an absent fraction or offset
/// meaning none. Set only on the map an export reads, never stored.
pub const CAPTURE_CORRECTED: &str = "RawtonicCaptureCorrected";

/// The fields of a corrected capture time, as the library gives them
/// (`DateTimeOriginal`, `SubSecTimeOriginal`, `OffsetTimeOriginal`; `None`
/// removes one).
pub type CaptureFields = [(&'static str, Option<String>); 3];

/// Sets `fields` in a source's EXIF map and marks it corrected
/// (`CAPTURE_CORRECTED`), for an export to write.
pub fn mark_corrected_capture(map: &mut HashMap<String, String>, fields: &CaptureFields) {
    for (key, value) in fields {
        match value {
            Some(value) => map.insert(key.to_string(), value.clone()),
            None => map.remove(*key),
        };
    }
    map.insert(CAPTURE_CORRECTED.into(), "1".into());
}

/// Writes a corrected capture time (`CAPTURE_CORRECTED`) over whatever date
/// fields the export copied, whichever way it read them; other dates
/// (digitized, modified) stay as the file has them.
pub fn apply_corrected_capture_time(metadata: &mut Metadata, map: &HashMap<String, String>) {
    if !map.contains_key(CAPTURE_CORRECTED) {
        return;
    }
    let Some(original) = map.get("DateTimeOriginal") else {
        return;
    };
    metadata.set_tag(ExifTag::DateTimeOriginal(exif_datetime(original)));
    match map.get("SubSecTimeOriginal") {
        Some(digits) => metadata.set_tag(ExifTag::SubSecTimeOriginal(digits.clone())),
        None => {
            metadata.remove_tag(ExifTag::SubSecTimeOriginal(String::new()));
        }
    }
    match map.get("OffsetTimeOriginal") {
        Some(offset) => metadata.set_tag(ExifTag::OffsetTimeOriginal(offset.clone())),
        None => {
            metadata.remove_tag(ExifTag::OffsetTimeOriginal(String::new()));
        }
    }
}

pub fn apply_sidecar_field_overrides(metadata: &mut Metadata, map: &HashMap<String, String>) {
    let clean_s = |s: &String| s.replace('"', "").trim().to_string();
    let is_user_edit = |s: &str| !s.is_empty() && s != "...";

    match map.get("Artist").map(clean_s) {
        Some(val) => {
            if is_user_edit(&val) {
                metadata.set_tag(ExifTag::Artist(val));
            }
        }
        None => {
            metadata.remove_tag(ExifTag::Artist(String::new()));
        }
    }
    match map.get("Copyright").map(clean_s) {
        Some(val) => {
            if is_user_edit(&val) {
                metadata.set_tag(ExifTag::Copyright(val));
            }
        }
        None => {
            metadata.remove_tag(ExifTag::Copyright(String::new()));
        }
    }
    match map.get("ImageDescription").map(clean_s) {
        Some(val) => {
            if is_user_edit(&val) {
                metadata.set_tag(ExifTag::ImageDescription(val));
            }
        }
        None => {
            metadata.remove_tag(ExifTag::ImageDescription(String::new()));
        }
    }
    match map.get("UserComment").map(clean_s) {
        Some(val) => {
            if is_user_edit(&val) && !val.starts_with("0x") {
                metadata.set_tag(ExifTag::UserComment(val.into_bytes()));
            }
        }
        None => {
            metadata.remove_tag(ExifTag::UserComment(Vec::new()));
        }
    }
}

pub fn apply_gps_from_kamadak(metadata: &mut Metadata, original_path: &Path) {
    let Ok(file) = std::fs::File::open(original_path) else {
        return;
    };
    let mut bufreader = std::io::BufReader::new(&file);
    let exifreader = exif::Reader::new();
    let Ok(exif_obj) = exifreader.read_from_container(&mut bufreader) else {
        return;
    };

    let get_string_val = |field: &exif::Field| -> String {
        match &field.value {
            exif::Value::Ascii(vec) => vec
                .iter()
                .map(|v| {
                    String::from_utf8_lossy(v)
                        .trim_matches(char::from(0))
                        .to_string()
                })
                .collect::<Vec<String>>()
                .join(" "),
            _ => field
                .display_value()
                .to_string()
                .replace("\"", "")
                .trim()
                .to_string(),
        }
    };

    if let Some(f) = exif_obj.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
        && let exif::Value::Rational(v) = &f.value
        && v.len() >= 3
    {
        metadata.set_tag(ExifTag::GPSLatitude(vec![
            to_ur64(&v[0]),
            to_ur64(&v[1]),
            to_ur64(&v[2]),
        ]));
    }
    if let Some(f) = exif_obj.get_field(exif::Tag::GPSLatitudeRef, exif::In::PRIMARY) {
        metadata.set_tag(ExifTag::GPSLatitudeRef(get_string_val(f)));
    }
    if let Some(f) = exif_obj.get_field(exif::Tag::GPSLongitude, exif::In::PRIMARY)
        && let exif::Value::Rational(v) = &f.value
        && v.len() >= 3
    {
        metadata.set_tag(ExifTag::GPSLongitude(vec![
            to_ur64(&v[0]),
            to_ur64(&v[1]),
            to_ur64(&v[2]),
        ]));
    }
    if let Some(f) = exif_obj.get_field(exif::Tag::GPSLongitudeRef, exif::In::PRIMARY) {
        metadata.set_tag(ExifTag::GPSLongitudeRef(get_string_val(f)));
    }
    if let Some(f) = exif_obj.get_field(exif::Tag::GPSAltitude, exif::In::PRIMARY)
        && let exif::Value::Rational(v) = &f.value
        && !v.is_empty()
    {
        metadata.set_tag(ExifTag::GPSAltitude(vec![to_ur64(&v[0])]));
    }
    if let Some(f) = exif_obj.get_field(exif::Tag::GPSAltitudeRef, exif::In::PRIMARY)
        && let Some(val) = f.value.get_uint(0)
    {
        metadata.set_tag(ExifTag::GPSAltitudeRef(vec![val as u8]));
    }
}

pub fn apply_gps_from_rawler(metadata: &mut Metadata, original_path_str: &str) {
    let loader = rawler::RawLoader::new();
    let Ok(raw_source) = rawler::rawsource::RawSource::new(Path::new(original_path_str)) else {
        return;
    };
    let Ok(decoder) = loader.get_decoder(&raw_source) else {
        return;
    };
    let Ok(meta) = decoder.raw_metadata(&raw_source, &Default::default()) else {
        return;
    };
    let Some(gps) = meta.exif.gps else {
        return;
    };
    if let Some(lat) = gps.gps_latitude {
        metadata.set_tag(ExifTag::GPSLatitude(vec![
            uR64 {
                nominator: lat[0].n,
                denominator: lat[0].d,
            },
            uR64 {
                nominator: lat[1].n,
                denominator: lat[1].d,
            },
            uR64 {
                nominator: lat[2].n,
                denominator: lat[2].d,
            },
        ]));
    }
    if let Some(lat_ref) = gps.gps_latitude_ref {
        metadata.set_tag(ExifTag::GPSLatitudeRef(lat_ref));
    }
    if let Some(lon) = gps.gps_longitude {
        metadata.set_tag(ExifTag::GPSLongitude(vec![
            uR64 {
                nominator: lon[0].n,
                denominator: lon[0].d,
            },
            uR64 {
                nominator: lon[1].n,
                denominator: lon[1].d,
            },
            uR64 {
                nominator: lon[2].n,
                denominator: lon[2].d,
            },
        ]));
    }
    if let Some(lon_ref) = gps.gps_longitude_ref {
        metadata.set_tag(ExifTag::GPSLongitudeRef(lon_ref));
    }
    if let Some(alt) = gps.gps_altitude {
        metadata.set_tag(ExifTag::GPSAltitude(vec![uR64 {
            nominator: alt.n,
            denominator: alt.d,
        }]));
    }
    if let Some(alt_ref) = gps.gps_altitude_ref {
        metadata.set_tag(ExifTag::GPSAltitudeRef(vec![alt_ref]));
    }
}

/// Name and version written as EXIF Software and XMP CreatorTool.
pub const SOFTWARE: &str = concat!("Rawtonic ", env!("CARGO_PKG_VERSION"));

/// Metadata embedded in one export: EXIF (with GPS unless stripped) and the
/// XMP packet with the session's descriptive metadata.
pub struct ExportMetadata {
    pub exif: Metadata,
    pub xmp: Option<String>,
    /// The XMP carries the AI marking (IPTC Digital Source Type): an export
    /// must not be written without it.
    pub generated: bool,
}

impl ExportMetadata {
    /// Only the AI marking and the creating tool (EXIF Software and the XMP
    /// marking), for a photo with generated areas.
    pub fn marking_only() -> Result<Self, String> {
        let mut exif = Metadata::new();
        exif.set_tag(ExifTag::Software(SOFTWARE.to_string()));
        let xmp = rawtonic_metadata::export_xmp_with_source(
            &rawtonic_metadata::Metadata::default(),
            SOFTWARE,
            Some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI),
        )
        .map_err(|e| format!("The AI marking could not be made: {e}"))?;
        Ok(Self {
            exif,
            xmp: Some(xmp),
            generated: true,
        })
    }
}

/// A fresh rights-only packet. Do not copy source EXIF or source XMP: these
/// may carry camera identifiers and locations. Catalog rights avoid any source
/// read; otherwise inspect just Artist/Copyright in the cached map or source.
fn stripped_export_metadata(
    source: &str,
    generated: bool,
    keep_creator_contact: bool,
    exif_sidecar: &dyn Fn() -> Option<HashMap<String, String>>,
    catalog: &dyn Fn() -> Option<rawtonic_metadata::Metadata>,
) -> Option<ExportMetadata> {
    let mut kept = catalog().unwrap_or_default();
    if kept.creator.trim().is_empty() || kept.copyright.trim().is_empty() {
        let (artist, copyright) = if let Some(map) = exif_sidecar() {
            let value = |key| {
                map.get(key)
                    .filter(|s| s.as_str() != "...")
                    .map(|s| s.replace('"', "").trim().to_string())
                    .unwrap_or_default()
            };
            (value("Artist"), value("Copyright"))
        } else {
            let mut rights = (String::new(), String::new());
            if let Ok(file) = std::fs::File::open(source)
                && let Ok(exif) =
                    exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file))
            {
                let text = |tag| {
                    exif.get_field(tag, exif::In::PRIMARY)
                        .and_then(|f| match &f.value {
                            exif::Value::Ascii(values) => Some(
                                values
                                    .iter()
                                    .map(|v| {
                                        String::from_utf8_lossy(v).trim_matches('\0').to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            ),
                            _ => None,
                        })
                        .unwrap_or_default()
                };
                rights = (text(exif::Tag::Artist), text(exif::Tag::Copyright));
            } else if is_raw_file(source) {
                let loader = rawler::RawLoader::new();
                if let Ok(raw_source) = rawler::rawsource::RawSource::new(Path::new(source))
                    && let Ok(decoder) = loader.get_decoder(&raw_source)
                    && let Ok(meta) = decoder.raw_metadata(&raw_source, &Default::default())
                {
                    rights = (
                        meta.exif.artist.unwrap_or_default(),
                        meta.exif.copyright.unwrap_or_default(),
                    );
                }
            }
            rights
        };
        if kept.creator.trim().is_empty() {
            kept.creator = artist;
        }
        if kept.copyright.trim().is_empty() {
            kept.copyright = copyright;
        }
    }
    kept = rawtonic_metadata::Metadata {
        creator: kept.creator,
        copyright: kept.copyright,
        iptc: kept
            .iptc
            .into_iter()
            .filter(|(key, _)| {
                key == "usageTerms"
                    || (keep_creator_contact
                        && matches!(
                            rawtonic_metadata::iptc_field(key),
                            Some((_, _, rawtonic_metadata::IptcKind::Contact))
                        ))
            })
            .collect(),
        ..Default::default()
    };
    if kept.creator.trim().is_empty()
        && kept.copyright.trim().is_empty()
        && kept.iptc.values().all(|v| v.trim().is_empty())
    {
        return generated
            .then(ExportMetadata::marking_only)
            .and_then(|r| r.ok());
    }
    let mut exif = Metadata::new();
    if !kept.creator.trim().is_empty() {
        exif.set_tag(ExifTag::Artist(kept.creator.trim().to_string()));
    }
    if !kept.copyright.trim().is_empty() {
        exif.set_tag(ExifTag::Copyright(kept.copyright.trim().to_string()));
    }
    exif.set_tag(ExifTag::Software(SOFTWARE.to_string()));
    let xmp = rawtonic_metadata::export_xmp_with_source(
        &kept,
        SOFTWARE,
        generated.then_some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI),
    )
    .map_err(|e| log::warn!("Export rights XMP not written: {e}"))
    .ok();
    if generated && xmp.is_none() {
        return ExportMetadata::marking_only().ok();
    }
    Some(ExportMetadata {
        exif,
        xmp,
        generated,
    })
}

/// The EXIF and XMP an export carries. `exif_sidecar` returns the source's
/// cached EXIF map (the app's `.rrdata`/EXIF cache), `catalog` the session's
/// metadata for the source. Stripped packets allow only creator, copyright,
/// usage terms and creator contacts only when `keep_creator_contact` is true.
/// `color_space` describes the output pixels and replaces source EXIF colour tags.
/// `generated` marks a photo with areas made by a generative model in its
/// XMP (IPTC Digital Source Type), also when other metadata is stripped.
#[allow(clippy::too_many_arguments)]
pub fn export_metadata_with(
    original_path_str: &str,
    output_format: &str,
    color_space: OutputColorSpace,
    keep_metadata: bool,
    strip_gps: bool,
    keep_creator_contact: bool,
    width: u32,
    height: u32,
    generated: bool,
    exif_sidecar: &dyn Fn() -> Option<HashMap<String, String>>,
    catalog: &dyn Fn() -> Option<rawtonic_metadata::Metadata>,
) -> Option<ExportMetadata> {
    if !matches!(
        output_format.to_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "avif" | "jxl"
    ) {
        return None;
    }
    let digital_source_type = generated.then_some(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI);
    // Without the source's metadata (not kept, or the original is gone) a
    // photo with generated areas still carries the marking.
    let marking_only = || {
        generated
            .then(ExportMetadata::marking_only)
            .and_then(|made| made.map_err(|e| log::warn!("{e}")).ok())
    };
    if !keep_metadata {
        return stripped_export_metadata(
            original_path_str,
            generated,
            keep_creator_contact,
            exif_sidecar,
            catalog,
        );
    }
    let original_path = Path::new(original_path_str);
    if !original_path.exists() {
        return marking_only();
    }

    // A TIFF source's IFD0 describes its own strips and sample layout, which
    // must not be copied into the export; its named fields are read below.
    let original_ext = original_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let tiff_source = original_ext == "tiff" || original_ext == "tif";

    let mut metadata = Metadata::new();

    let full_exif_copied = !is_raw_file(original_path_str)
        && !tiff_source
        && copy_full_exif_from_source(&mut metadata, original_path, strip_gps);
    let mut source_read_success = full_exif_copied;

    if !source_read_success && let Some(map) = exif_sidecar() {
        source_read_success = true;

        let clean_s = |s: &String| s.replace('"', "").trim().to_string();

        let parse_ur64 = |s: &str| -> Option<uR64> {
            let cleaned_string = s
                .replace("f/", "")
                .replace(" s", "")
                .replace(" mm", "")
                .replace("\"", "");

            let val = cleaned_string.trim();

            if val.contains('/') {
                let parts: Vec<&str> = val.split('/').collect();
                if parts.len() == 2
                    && let (Ok(n), Ok(d)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                {
                    return Some(uR64 {
                        nominator: n,
                        denominator: d,
                    });
                }
            } else if let Ok(f) = val.parse::<f32>() {
                return Some(uR64 {
                    nominator: (f * 1000.0) as u32,
                    denominator: 1000,
                });
            }
            None
        };
        if let Some(val) = map.get("Make") {
            metadata.set_tag(ExifTag::Make(clean_s(val)));
        }
        if let Some(val) = map.get("Model") {
            metadata.set_tag(ExifTag::Model(clean_s(val)));
        }
        if let Some(val) = map.get("LensMake") {
            metadata.set_tag(ExifTag::LensMake(clean_s(val)));
        }
        if let Some(val) = map.get("LensModel") {
            metadata.set_tag(ExifTag::LensModel(clean_s(val)));
        }
        if let Some(val) = map.get("Artist") {
            metadata.set_tag(ExifTag::Artist(clean_s(val)));
        }
        if let Some(val) = map.get("Copyright") {
            metadata.set_tag(ExifTag::Copyright(clean_s(val)));
        }
        if let Some(val) = map.get("UserComment") {
            metadata.set_tag(ExifTag::UserComment(clean_s(val).into_bytes()));
        }
        if let Some(val) = map.get("ImageDescription") {
            metadata.set_tag(ExifTag::ImageDescription(clean_s(val)));
        }
        if let Some(val) = map.get("DateTimeOriginal") {
            metadata.set_tag(ExifTag::DateTimeOriginal(exif_datetime(val)));
        }
        if let Some(val) = map.get("CreateDate") {
            metadata.set_tag(ExifTag::CreateDate(exif_datetime(val)));
        }
        if let Some(val) = map.get("FNumber")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::FNumber(vec![ur]));
        }
        if let Some(val) = map.get("ExposureTime")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::ExposureTime(vec![ur]));
        }
        if let Some(val) = map.get("FocalLength")
            && let Some(ur) = parse_ur64(val)
        {
            metadata.set_tag(ExifTag::FocalLength(vec![ur]));
        }
        if let Some(val) = map.get("FocalLengthIn35mmFilm") {
            let cleaned = val.replace(" mm", "").replace("\"", "");
            let trimmed = cleaned.trim();
            if let Ok(f_val) = trimmed.parse::<f32>() {
                metadata.set_tag(ExifTag::FocalLengthIn35mmFormat(vec![f_val.round() as u16]));
            }
        }
        if let Some(val) = map.get("ISOSpeed").or(map.get("PhotographicSensitivity"))
            && let Ok(iso) = val.replace('"', "").trim().parse::<u16>()
        {
            metadata.set_tag(ExifTag::ISO(vec![iso]));
        }
    }

    if !source_read_success && let Ok(file) = std::fs::File::open(original_path) {
        let mut bufreader = std::io::BufReader::new(&file);
        let exifreader = exif::Reader::new();

        if let Ok(exif_obj) = exifreader.read_from_container(&mut bufreader) {
            source_read_success = true;

            let get_string_val = |field: &exif::Field| -> String {
                match &field.value {
                    exif::Value::Ascii(vec) => vec
                        .iter()
                        .map(|v| {
                            String::from_utf8_lossy(v)
                                .trim_matches(char::from(0))
                                .to_string()
                        })
                        .collect::<Vec<String>>()
                        .join(" "),
                    _ => field
                        .display_value()
                        .to_string()
                        .replace("\"", "")
                        .trim()
                        .to_string(),
                }
            };

            if let Some(f) = exif_obj.get_field(exif::Tag::Make, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Make(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Model, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Model(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::LensMake, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::LensMake(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::LensModel, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::LensModel(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Artist, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Artist(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::Copyright, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::Copyright(get_string_val(f)));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::DateTimeOriginal(exif_datetime(&get_string_val(f))));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::DateTime, exif::In::PRIMARY) {
                metadata.set_tag(ExifTag::CreateDate(exif_datetime(&get_string_val(f))));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FNumber, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::FNumber(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::ExposureTime(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FocalLength, exif::In::PRIMARY)
                && let exif::Value::Rational(v) = &f.value
                && !v.is_empty()
            {
                metadata.set_tag(ExifTag::FocalLength(vec![to_ur64(&v[0])]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::ExposureBiasValue, exif::In::PRIMARY) {
                match &f.value {
                    exif::Value::SRational(v) if !v.is_empty() => {
                        metadata.set_tag(ExifTag::ExposureCompensation(vec![to_ir64(&v[0])]));
                    }
                    exif::Value::Rational(v) if !v.is_empty() => {
                        metadata.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
                            nominator: v[0].num as i32,
                            denominator: v[0].denom as i32,
                        }]));
                    }
                    _ => {}
                }
            }
            if let Some(f) =
                exif_obj.get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            {
                if let Some(val) = f.value.get_uint(0) {
                    metadata.set_tag(ExifTag::ISO(vec![val as u16]));
                }
            } else if let Some(f) = exif_obj.get_field(exif::Tag::ISOSpeed, exif::In::PRIMARY)
                && let Some(val) = f.value.get_uint(0)
            {
                metadata.set_tag(ExifTag::ISO(vec![val as u16]));
            }
            if let Some(f) = exif_obj.get_field(exif::Tag::FocalLengthIn35mmFilm, exif::In::PRIMARY)
                && let Some(val) = f.value.get_uint(0)
            {
                metadata.set_tag(ExifTag::FocalLengthIn35mmFormat(vec![val as u16]));
            }
        }
    }

    if !source_read_success && is_raw_file(original_path_str) {
        let loader = rawler::RawLoader::new();
        if let Ok(raw_source) = rawler::rawsource::RawSource::new(Path::new(original_path_str))
            && let Ok(decoder) = loader.get_decoder(&raw_source)
            && let Ok(meta) = decoder.raw_metadata(&raw_source, &Default::default())
        {
            if !meta.make.is_empty() {
                metadata.set_tag(ExifTag::Make(meta.make.clone()));
            }
            if !meta.model.is_empty() {
                metadata.set_tag(ExifTag::Model(meta.model.clone()));
            }
            let exif = meta.exif;
            if let Some(artist) = exif.artist {
                metadata.set_tag(ExifTag::Artist(artist));
            }
            if let Some(copyright) = exif.copyright {
                metadata.set_tag(ExifTag::Copyright(copyright));
            }
            if let Some(dt) = exif.date_time_original {
                metadata.set_tag(ExifTag::DateTimeOriginal(exif_datetime(&dt)));
            }
            if let Some(dt) = exif.create_date {
                metadata.set_tag(ExifTag::CreateDate(exif_datetime(&dt)));
            }
            if let Some(lens_make) = exif.lens_make {
                metadata.set_tag(ExifTag::LensMake(lens_make));
            }
            if let Some(lens_model) = exif.lens_model {
                metadata.set_tag(ExifTag::LensModel(lens_model));
            }
            if let Some(f) = exif.fnumber {
                metadata.set_tag(ExifTag::FNumber(vec![uR64 {
                    nominator: f.n,
                    denominator: f.d,
                }]));
            }
            if let Some(t) = exif.exposure_time {
                metadata.set_tag(ExifTag::ExposureTime(vec![uR64 {
                    nominator: t.n,
                    denominator: t.d,
                }]));
            }
            if let Some(fl) = exif.focal_length {
                metadata.set_tag(ExifTag::FocalLength(vec![uR64 {
                    nominator: fl.n,
                    denominator: fl.d,
                }]));
            }
            if let Some(iso) = exif.iso_speed {
                metadata.set_tag(ExifTag::ISO(vec![iso as u16]));
            } else if let Some(iso) = exif.iso_speed_ratings {
                metadata.set_tag(ExifTag::ISO(vec![iso]));
            }
            if let Some(ev) = exif.exposure_bias {
                metadata.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
                    nominator: ev.n,
                    denominator: ev.d,
                }]));
            }
            if let Some(flash) = exif.flash {
                metadata.set_tag(ExifTag::Flash(vec![flash]));
            }
            if let Some(metering) = exif.metering_mode {
                metadata.set_tag(ExifTag::MeteringMode(vec![metering]));
            }
            if let Some(wb) = exif.white_balance {
                metadata.set_tag(ExifTag::WhiteBalance(vec![wb]));
            }
            if let Some(prog) = exif.exposure_program {
                metadata.set_tag(ExifTag::ExposureProgram(vec![prog]));
            }
        }
    }

    if !strip_gps {
        if is_raw_file(original_path_str) {
            apply_gps_from_rawler(&mut metadata, original_path_str);
        } else {
            apply_gps_from_kamadak(&mut metadata, original_path);
        }
    }

    if full_exif_copied && let Some(map) = exif_sidecar() {
        apply_sidecar_field_overrides(&mut metadata, &map);
    }
    if let Some(map) = exif_sidecar() {
        apply_corrected_capture_time(&mut metadata, &map);
    }
    // Session metadata (the Metadata tool) wins over the camera's and the
    // inherited sidecar's values, as in Lightroom: Creator → Artist,
    // Copyright → Copyright, Description → ImageDescription. Empty fields keep
    // what the file already carries.
    let catalog = catalog().unwrap_or_default();
    for (value, tag) in [
        (&catalog.creator, ExifTag::Artist as fn(String) -> ExifTag),
        (&catalog.copyright, ExifTag::Copyright),
        (&catalog.description, ExifTag::ImageDescription),
    ] {
        if !value.trim().is_empty() {
            metadata.set_tag(tag(value.trim().to_string()));
        }
    }

    metadata.set_tag(ExifTag::Software(SOFTWARE.to_string()));
    metadata.set_tag(ExifTag::Orientation(vec![1u16]));
    metadata.set_tag(ExifTag::ColorSpace(vec![
        if color_space == OutputColorSpace::Srgb {
            1
        } else {
            0xffff
        },
    ]));
    // A copied DCF index describes the source encoding, not the export.
    // Only sRGB and Adobe RGB have a DCF interoperability designation.
    if metadata
        .get_ifds()
        .iter()
        .any(|ifd| ifd.get_ifd_type() == ExifTagGroup::INTEROP)
    {
        metadata.remove_tag(ExifTag::InteroperabilityIndex(String::new()));
        let index = match color_space {
            OutputColorSpace::Srgb => Some("R98"),
            OutputColorSpace::AdobeRgb => Some("R03"),
            OutputColorSpace::DisplayP3 | OutputColorSpace::ProPhoto => None,
        };
        if let Some(index) = index {
            metadata.set_tag(ExifTag::InteroperabilityIndex(index.to_string()));
        }
    }
    if width > 0 && height > 0 {
        metadata.set_tag(ExifTag::ExifImageWidth(vec![width]));
        metadata.set_tag(ExifTag::ExifImageHeight(vec![height]));
    }

    let xmp = rawtonic_metadata::export_xmp_with_source(&catalog, SOFTWARE, digital_source_type)
        .map_err(|e| log::warn!("Export XMP not written: {e}"))
        .ok();
    if generated && xmp.is_none() {
        return marking_only();
    }
    Some(ExportMetadata {
        exif: metadata,
        xmp,
        generated,
    })
}

/// Embeds `metadata` in encoded export bytes. AVIF takes its EXIF at encoding
/// time; its XMP (with the AI marking) is added here as a `mime` item.
pub fn embed_export_metadata(
    image_bytes: &mut Vec<u8>,
    output_format: &str,
    metadata: &ExportMetadata,
) -> Result<(), String> {
    let xmp = metadata.xmp.as_deref().map(str::as_bytes);
    let tiff_exif = || metadata.exif.encode().map_err(|e| e.to_string());
    match output_format.to_lowercase().as_str() {
        "jpg" | "jpeg" => {
            metadata
                .exif
                .write_to_vec(image_bytes, FileExtension::JPEG)
                .map_err(|e| e.to_string())?;
            if let Some(xmp) = xmp {
                crate::xmp_embed::jpeg_insert_xmp(image_bytes, xmp)?;
            }
        }
        "png" => {
            crate::xmp_embed::png_insert_exif(image_bytes, &tiff_exif()?)?;
            if let Some(xmp) = xmp {
                crate::xmp_embed::png_insert_xmp(image_bytes, xmp)?;
            }
        }
        // TIFFs come from the image encoder without EXIF; little_exif would
        // re-encode the whole file, so the directories are appended instead.
        "tif" | "tiff" => crate::tiff_exif::append_exif(image_bytes, &metadata.exif, xmp)?,
        "webp" => {
            *image_bytes =
                crate::webp_container::set_chunks(image_bytes, None, Some(&tiff_exif()?), xmp)?;
        }
        // EXIF is written by the AVIF encoder; XMP becomes a mime item.
        "avif" => {
            if let Some(xmp) = xmp {
                *image_bytes = crate::avif_container::set_metadata(image_bytes, None, Some(xmp))?;
            }
        }
        "jxl" => {
            if !image_bytes.starts_with(&[0xff, 0x0a]) {
                return Err("JPEG XL export is not a bare codestream".to_string());
            }
            *image_bytes = crate::xmp_embed::jxl_container(image_bytes, Some(&tiff_exif()?), xmp);
        }
        _ => {}
    }
    Ok(())
}

pub fn get_primary_sidecar_path(image_path: &Path) -> PathBuf {
    let mut filename = image_path.file_name().unwrap_or_default().to_os_string();
    filename.push(".rrdata");
    image_path.with_file_name(filename)
}

pub fn get_rrexif_path(image_path: &Path) -> PathBuf {
    let mut filename = image_path.file_name().unwrap_or_default().to_os_string();
    filename.push(".rrexif");
    image_path.with_file_name(filename)
}

pub fn read_exif_data_from_bytes(path: &str, file_bytes: &[u8]) -> HashMap<String, String> {
    if is_raw_file(path)
        && let Some(map) = extract_metadata(file_bytes)
    {
        return map;
    }

    let mut exif_data = HashMap::new();
    if let Some(exif) = read_exif(file_bytes) {
        for field in exif.fields() {
            let raw_val = match &field.value {
                _ if field.tag == exif::Tag::UserComment => {
                    match user_comment_text(&field.value, exif.little_endian()) {
                        Some(v) => v,
                        None => continue,
                    }
                }
                exif::Value::Ascii(_) => match clean_ascii_value(&field.value) {
                    Some(v) => v,
                    None => continue,
                },
                exif::Value::Rational(v) if field.tag == exif::Tag::LensSpecification => {
                    match format_lens_specification(v) {
                        Some(s) => s,
                        None => continue,
                    }
                }
                _ => field.display_value().with_unit(&exif).to_string(),
            };
            exif_data.insert(field.tag.to_string(), truncate_large_exif(&raw_val));
        }
    }
    exif_data
}

/// A recipe saved while its photo could not be read gets its lens
/// parameters resolved from the photo before it renders
/// (`rawtonic_engine::lens_recipe::resolve_unresolved`).
pub fn resolve_unresolved_lens(recipe: &mut Value, source: &Path) -> Result<(), String> {
    if !rawtonic_engine::lens_recipe::is_unresolved(recipe) {
        return Ok(());
    }
    // Readable again: facts registered while it was offline are stale (as
    // image loading drops them, `preview_proxy::prepare_offline`). A cloud
    // file is neither read (that downloads it) nor counted as back.
    let available = rawtonic_engine::lens_recipe::readable(source);
    if available {
        rawtonic_engine::source_facts::forget(&source.to_string_lossy());
    }
    let exif = available
        .then(|| std::fs::read(source).ok())
        .flatten()
        .map(|bytes| read_exif_data_from_bytes(&source.to_string_lossy(), &bytes))
        .filter(|exif| !exif.is_empty());
    let lens_db = rawtonic_engine::lens_correction::global_lens_db();
    rawtonic_engine::lens_recipe::resolve_unresolved(recipe, &exif, lens_db.as_deref(), source)
}

/// The recipe as every render uses it: lens parameters saved while the
/// photo could not be read resolved now (`resolve_unresolved_lens`; its
/// error is returned after the defaults are filled in, for the callers that
/// must not render without them), then the camera's defaults for a
/// never-edited photo (`camera_crop::apply_default` for a new recipe,
/// `embedded_lens::apply_default`), and the photo's
/// orientation for Lightroom items imported before imports recorded it
/// (`record_lightroom_orientation`).
pub fn prepare_recipe_lens(recipe: &mut Value, source: &Path) -> Result<(), String> {
    let resolved = resolve_unresolved_lens(recipe, source);
    rawtonic_engine::camera_crop::apply_default(recipe, source);
    rawtonic_engine::embedded_lens::apply_default(recipe, source);
    record_lightroom_orientation(recipe, source);
    resolved
}

/// Records the photo's orientation in Lightroom spots and red eye
/// corrections imported before imports recorded it
/// (`lightroom_retouch::needs_orientation`), so they render.
pub fn record_lightroom_orientation(recipe: &mut Value, source: &Path) {
    if rawtonic_engine::lightroom_retouch::needs_orientation(recipe)
        && rawtonic_engine::lens_recipe::readable(source)
        && let Ok(bytes) = std::fs::read(source)
    {
        let orientation =
            rawtonic_engine::image_loader::source_orientation(&bytes, &source.to_string_lossy());
        rawtonic_engine::lightroom_retouch::set_orientation(recipe, orientation);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_render_resolves_a_recipe_saved_while_its_photo_was_unreadable() {
        use rawtonic_engine::lens_correction::{
            lenses_for_maker, load_lensfun_dir, resolve_lens_params, set_global_lens_db,
        };
        use rawtonic_engine::lens_recipe::{LENS_PROFILE_FOR, LENS_UNRESOLVED, is_unresolved};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lenses.xml"),
            r#"<lensdatabase><lens>
                <maker>Canon</maker><model>Canon RF 24-70mm F2.8L IS USM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration>
                    <distortion model="ptlens" focal="24" a="0.03" b="-0.09" c="0.06"/>
                    <distortion model="ptlens" focal="70" a="0.002" b="0.004" c="-0.003"/>
                </calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(tmp.path());
        let lenses = lenses_for_maker(&db, "Canon");
        let zoom = lenses[0].get_display_name(&lenses);
        let fallback = resolve_lens_params(&db, "Canon", &zoom, 50.0, None, None, None).unwrap();
        set_global_lens_db(std::sync::Arc::new(db));
        // The photo is back (a file without EXIF here); the recipe still
        // carries another lens's parameters from the offline save.
        let photo = tmp.path().join("scan.png");
        std::fs::write(&photo, b"no exif").unwrap();
        // Facts registered while it was offline are stale once it is back.
        let photo_str = photo.to_string_lossy();
        rawtonic_engine::source_facts::register(&photo_str, Default::default());
        let mut recipe = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": "Canon", "lensModel": zoom,
            "lensDistortionParams": {"k1": 0.5},
            LENS_PROFILE_FOR: LENS_UNRESOLVED,
        });
        super::prepare_recipe_lens(&mut recipe, &photo).unwrap();
        assert!(!is_unresolved(&recipe));
        assert!(rawtonic_engine::source_facts::registered(&photo_str).is_none());
        assert_eq!(
            recipe["lensDistortionParams"],
            serde_json::to_value(fallback).unwrap()
        );
        // Still offline: an error for the export, the recipe as it was.
        let mut offline = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": "Canon", "lensModel": "Canon RF 24-70mm F2.8L IS USM",
            LENS_PROFILE_FOR: LENS_UNRESOLVED,
        });
        assert!(
            super::prepare_recipe_lens(&mut offline, std::path::Path::new("/offline/IMG_1.CR3"))
                .is_err()
        );
        assert!(is_unresolved(&offline));
    }

    #[test]
    fn user_comment_shows_its_text_not_hex() {
        use super::user_comment_text;
        let undefined = |bytes: &[u8]| exif::Value::Undefined(bytes.to_vec(), 0);
        assert_eq!(user_comment_text(&undefined(&[0; 72]), true), None);
        assert_eq!(
            user_comment_text(&undefined(b"ASCII\0\0\0Hello  "), true).as_deref(),
            Some("Hello")
        );
        assert_eq!(
            user_comment_text(&undefined(b"\0\0\0\0\0\0\0\0        "), false),
            None
        );
        let mut unicode = b"UNICODE\0".to_vec();
        unicode.extend("Grüße".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            user_comment_text(&undefined(&unicode), true).as_deref(),
            Some("Grüße")
        );
    }

    use super::exif_datetime;

    #[test]
    fn stripped_export_keeps_rights_and_contacts_but_no_camera_or_location() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("photo.jpg");
        std::fs::write(&source, b"no exif").unwrap();
        let sidecar = || {
            Some(std::collections::HashMap::from([
                ("Artist".into(), "Camera creator".into()),
                ("Copyright".into(), "Camera copyright".into()),
                ("Model".into(), "Secret camera".into()),
            ]))
        };
        for generated in [false, true] {
            let catalog = || {
                Some(rawtonic_metadata::Metadata {
                    creator: "Photographer".into(),
                    copyright: "All rights reserved".into(),
                    title: "Private title".into(),
                    keywords: vec!["Private keyword".into()],
                    iptc: [
                        ("creatorEmail".into(), "photo@example.invalid".into()),
                        ("usageTerms".into(), "Licensed use only".into()),
                        ("city".into(), "Private city".into()),
                    ]
                    .into(),
                    ..Default::default()
                })
            };
            let result = super::export_metadata_with(
                source.to_str().unwrap(),
                "jpeg",
                rawtonic_engine::output_color_space::OutputColorSpace::Srgb,
                false,
                false,
                true,
                8,
                8,
                generated,
                &sidecar,
                &catalog,
            )
            .expect("rights survive stripping");
            let xmp = result.xmp.unwrap();
            let (kept, _) = rawtonic_metadata::read_xmp(&xmp).unwrap();
            assert_eq!(kept.creator, "Photographer");
            assert_eq!(kept.copyright, "All rights reserved");
            assert_eq!(kept.iptc.len(), 2);
            assert_eq!(kept.iptc["creatorEmail"], "photo@example.invalid");
            assert_eq!(kept.iptc["usageTerms"], "Licensed use only");
            assert!(kept.title.is_empty() && kept.keywords.is_empty());
            assert_eq!(
                xmp.contains(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI),
                generated
            );
            for ifd in result.exif.get_ifds() {
                for tag in ifd.get_tags() {
                    assert!(
                        matches!(
                            tag,
                            little_exif::exif_tag::ExifTag::Artist(_)
                                | little_exif::exif_tag::ExifTag::Copyright(_)
                                | little_exif::exif_tag::ExifTag::Software(_)
                        ),
                        "unexpected tag: {tag:?}"
                    );
                }
            }
        }
        let result = super::export_metadata_with(
            source.to_str().unwrap(),
            "jpeg",
            rawtonic_engine::output_color_space::OutputColorSpace::Srgb,
            false,
            false,
            false,
            8,
            8,
            false,
            &sidecar,
            &|| None,
        )
        .unwrap();
        let (kept, _) = rawtonic_metadata::read_xmp(&result.xmp.unwrap()).unwrap();
        assert_eq!(kept.creator, "Camera creator");
        assert_eq!(kept.copyright, "Camera copyright");
    }
    #[test]
    fn stripped_packet_builder_itself_requires_contact_opt_in() {
        let catalog = || {
            Some(rawtonic_metadata::Metadata {
                creator: "Creator".into(),
                copyright: "Copyright".into(),
                iptc: [
                    ("creatorEmail".into(), "photo@example.invalid".into()),
                    ("usageTerms".into(), "Licensed use".into()),
                ]
                .into(),
                ..Default::default()
            })
        };
        let metadata = super::export_metadata_with(
            "/nonexistent.jpg",
            "jpeg",
            rawtonic_engine::output_color_space::OutputColorSpace::Srgb,
            false,
            false,
            false,
            4,
            4,
            false,
            &|| None,
            &catalog,
        )
        .unwrap();
        let (kept, _) = rawtonic_metadata::read_xmp(&metadata.xmp.unwrap()).unwrap();
        assert_eq!(kept.creator, "Creator");
        assert_eq!(kept.copyright, "Copyright");
        assert_eq!(kept.iptc["usageTerms"], "Licensed use");
        assert!(!kept.iptc.contains_key("creatorEmail"));
    }

    #[test]
    fn generated_areas_are_marked_even_without_metadata() {
        let none = || None;
        let marked = |keep| {
            super::export_metadata_with(
                "/nonexistent/photo.jpg",
                "jpeg",
                rawtonic_engine::output_color_space::OutputColorSpace::Srgb,
                keep,
                false,
                false,
                8,
                8,
                true,
                &none,
                &|| None,
            )
        };
        let stripped = marked(false).expect("marking without metadata");
        assert!(
            stripped
                .xmp
                .unwrap()
                .contains(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI)
        );
        let plain = super::export_metadata_with(
            "/nonexistent/photo.jpg",
            "jpeg",
            rawtonic_engine::output_color_space::OutputColorSpace::Srgb,
            false,
            false,
            false,
            8,
            8,
            false,
            &none,
            &|| None,
        );
        assert!(
            plain.is_none(),
            "nothing without metadata or generated areas"
        );
    }

    /// A little-endian TIFF whose EXIF holds FocalLength 50 mm and, when
    /// given, FocalLengthIn35mmFilm.
    fn tiff_with_focal_length(equivalent: Option<u16>) -> Vec<u8> {
        let entries = 1 + u16::from(equivalent.is_some());
        let exif_ifd = 26u32;
        let rational_at = exif_ifd + 2 + 12 * u32::from(entries) + 4;
        let mut tiff = b"II\x2a\x00".to_vec();
        tiff.extend(8u32.to_le_bytes());
        tiff.extend(1u16.to_le_bytes());
        tiff.extend(0x8769u16.to_le_bytes());
        tiff.extend(4u16.to_le_bytes());
        tiff.extend(1u32.to_le_bytes());
        tiff.extend(exif_ifd.to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(entries.to_le_bytes());
        tiff.extend(0x920Au16.to_le_bytes());
        tiff.extend(5u16.to_le_bytes());
        tiff.extend(1u32.to_le_bytes());
        tiff.extend(rational_at.to_le_bytes());
        if let Some(equivalent) = equivalent {
            tiff.extend(0xA405u16.to_le_bytes());
            tiff.extend(3u16.to_le_bytes());
            tiff.extend(1u32.to_le_bytes());
            tiff.extend(u32::from(equivalent).to_le_bytes());
        }
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(50u32.to_le_bytes());
        tiff.extend(1u32.to_le_bytes());
        tiff
    }

    /// The 35 mm equivalent is the photo's own, never the focal length again
    /// (which made lens profiles take every body for full frame).
    #[test]
    fn the_35mm_equivalent_is_read_not_copied_from_the_focal_length() {
        let without = super::extract_metadata(&tiff_with_focal_length(None)).unwrap();
        assert_eq!(without.get("FocalLength").map(String::as_str), Some("50"));
        assert_eq!(without.get("FocalLengthIn35mmFilm"), None);
        let with = super::extract_metadata(&tiff_with_focal_length(Some(80))).unwrap();
        assert_eq!(with.get("FocalLength").map(String::as_str), Some("50"));
        let equivalent = with.get("FocalLengthIn35mmFilm").unwrap();
        assert!(equivalent.starts_with("80"), "{equivalent}");
    }

    #[test]
    fn exif_dates_use_colons() {
        assert_eq!(exif_datetime("2020-07-10 07:34:38"), "2020:07:10 07:34:38");
        assert_eq!(
            exif_datetime("\"2020:07:10 07:34:38\""),
            "2020:07:10 07:34:38"
        );
        assert_eq!(
            exif_datetime("2020-07-10T07:34:38.25"),
            "2020:07:10 07:34:38"
        );
        assert_eq!(exif_datetime(" unknown "), "unknown");
    }
}
