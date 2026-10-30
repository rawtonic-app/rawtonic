//! Preview proxies: the decoded, enhanced and still unedited photograph at
//! about screen size, kept in the session's `.cache/previews/` (Capture
//! One's preview files, Lightroom's smart previews), for photographs outside
//! sessions in the app's cache folder. Develop renders from a proxy at once
//! while the RAW decodes in the background; exports, 100% zoom and every
//! full-resolution step keep using the full decode.
//!
//! Two kinds: the Standard proxy (about screen size, made whenever a
//! photograph is opened or its previews are built) and the 1:1 proxy (the
//! whole decode, built only on request). Both are unedited linear data, so an
//! edit never makes them stale: the recipe is rendered over them. When the
//! RAW cannot be read (offline: missing, or a cloud file not downloaded) the
//! photograph opens on them: on the 1:1 proxy at full resolution, else on the
//! Standard one.
//!
//! File: `RTPX`, format version (u32 LE), JSON header length (u32 LE), JSON
//! header, then the image in bands of rows: per band its row count and
//! compressed length (u32 LE) and the band's bytes, in the header's
//! `encoding`:
//!
//! - `jxl` with a `transfer` (format 3): lossy JPEG XL of the linear float
//!   pixels after a shadow-weighted transfer that decoding inverts
//!   (rawtonic_engine::proxy_codec), so edits that lift the shadows keep
//!   their grain.
//! - `jxl` without a transfer (format 2): the same of the linear values.
//! - `f16` (format 1): RGB half floats, per channel the difference to the
//!   left neighbour (wrapping u16) as a low-byte plane followed by a
//!   high-byte plane, deflated.
//!
//! Format 1 and 2 files are still read (offline photographs open on them);
//! they are replaced by format 3 ones the next time the photograph is
//! decoded or its previews are built.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::DeflateDecoder;
use half::f16;
use image::{DynamicImage, GenericImageView, Rgb32FImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::app_settings::AppSettings;
use crate::image_loader::{DeadTest, DecodeMeasures, DecodeVariant};
use rawtonic_engine::proxy_codec::{self, Transfer};
use rawtonic_engine::source_facts::{self, SourceFacts};
use std::collections::HashMap;

const MAGIC: &[u8; 4] = b"RTPX";
const FORMAT_VERSION: u32 = 3;
/// Format versions still read (1: half floats only; 2: linear JPEG XL).
const READ_VERSIONS: [u32; 3] = [1, 2, 3];
/// Raise when decoding or enhancement changes what a decode produces, so old
/// proxies (and the thumbnails developed from them) are re-made.
/// 2: clipped sensor channels rebuilt before demosaicing (2026-10-04).
/// 3: own Bayer/X-Trans demosaicing, colour-pattern fixes (2026-10-05).
/// 4: clipped colours hidden by stray samples are rebuilt (2026-10-05).
/// 5: Panasonic/Olympus aspect-ratio crop at decode (2026-10-05).
/// 6: white/black levels and as-shot white balance fixed in rawler for
///    Sony, Samsung, Canon CRW/12-bit CR2, Kodak, Panasonic (2026-10-05).
/// 7: DNG camera calibration, analog balance and forward matrices (2026-10-05).
/// 8: DNG SDK white adaptation for warm and cool light (2026-10-06).
/// 9: as-shot white balance Camera Raw ignores (Nikon D1, PowerShot CRW,
///    matrix fallback white) (2026-10-06).
/// 10: Hasselblad H4D signed 3FR/FFF samples and file colour matrix (2026-10-06).
/// 11: Canon sRAW/mRAW levels and balance record (2026-10-06).
/// 12: Samsung GX20 DNGs with Adobe's matrices (2026-10-06).
/// 13: EOS 300D ISO 100 linearization (2026-10-06).
/// 14: Olympus E-10 levels and CFA, Phase One P20+ CFA (2026-10-06).
/// 15: white balance with the smallest multiplier at 1 (2026-10-06).
/// 16: X-Trans fine-grain smoothing after demosaicing (2026-10-06).
/// 17: grey-world white balance for the EOS D30 (2026-10-06).
/// 18: as-shot white balance for Mamiya ZD, Kodak DCS Pro 14N/14nx/SLR/c,
///     PowerShot G9 (2026-10-06).
/// 19: COOLPIX A1000/B700 built-in distortion (2026-10-06).
/// 20: Fujifilm stored corrections skipped where Camera Raw ignores them (2026-10-06).
/// 21: Pixel DNGs with Adobe's matrices; gain table maps beyond ProRAW (2026-10-06).
/// 22: gain table maps after BaselineExposure, DefaultBlackRender, unconverged
///     white search, embedded curves for cameras without Adobe profile (2026-10-06).
/// 23: four-colour sensors balanced with the largest multiplier at 1 (2026-10-06).
/// 24: FinePix F700/S2Pro/S5000/S7000 white balance like Adobe (2026-10-06).
/// 25: Epson R-D1 and Olympus SP350/SP500UZ/SP510UZ/SP550UZ white balance like Adobe (2026-10-06).
/// 26: Samsung GX-1L, Hasselblad Lunar/CF132/CFV-50c 3FR/CFV 100C, Phase One P40+ (2026-10-06).
/// 27: Nikon small NEF levels (2026-10-06).
/// 28: malformed DNG BaselineExposure ignored (2026-10-06).
/// 29: Panasonic superzoom distortion data no longer dropped (2026-10-06).
/// 30: lensfun default for five PowerShot compacts (2026-10-06).
/// 31: colour matrices normalised before blending (2026-10-06).
/// 32: Phase One IQ150/IQ3 100MP/Trichromatic and Credo 50 colour matrices (2026-10-06).
/// 33: Adobe matrices for eight more cameras (2026-10-06).
/// 34: DNG black level from masked areas (2026-10-06).
/// 35: Sigma lenses on Canon EOS R bodies match their lensfun profiles (2026-10-06).
/// 36: DNG vignette opcodes from OpcodeList2; no second 300D linearization for DNGs (2026-10-06).
/// 37: lensfun default for DNGs of Canon EOS R bodies (2026-10-06).
/// 38: Sony vignetting for Adobe DNG Converter files of Sony bodies (2026-10-06).
/// 39: vignetting from the lens profile embedded in Adobe DNG conversions (2026-10-06).
/// 40: distortion and framing from that profile; DNG DefaultUserCrop (2026-10-06).
/// 41: Sony aspect-ratio crops; Sony DNG distortion (2026-10-07).
/// 42: X-Trans demosaic without tile seams (2026-10-07).
/// 43: Adobe lens profiles (reverted).
/// 44: back to the camera's data and lensfun (2026-10-07).
/// 45: noise reduction against Capture One (Wiener bands, NR-aware
/// sharpening, colour on the denoised luminance) (2026-10-07).
/// 46: PowerShot Pro1 stored white balance (2026-10-07).
/// 47: saturation/lightness-dependent look residual (2026-10-07).
/// 48: per-make look tables (2026-10-07).
/// 49: X-Trans demosaic with one refinement pass (2026-10-07).
/// 50: DJI drone DNGs on the regular DNG path (2026-10-07).
/// 51: Camera Raw phone defaults (Whites/Blacks/Clarity) for phone DNGs (2026-10-07).
/// 52: Galaxy Expert RAW without the black ramp (2026-10-07).
/// 53: Canon CR3 lens data from CTMD record 14 (R5 Mark II, R6 Mark III) (2026-10-07).
/// 54: lensfun profiles at their converted distortion strength (2026-10-07).
/// 55: Sony ARW 6 compressed decoding (2026-10-07).
/// 56: X-Trans demosaic on the GPU (2026-10-07).
/// 57: Rawtonic's own tone curve and colour rendition; own curve choices (2026-10-11).
/// 58: highlight reconstruction without pink or green casts: two clipped
/// colours solved from the recorded one, fully clipped areas neutral at the
/// brightness around them (2026-10-16).
/// 59: Panasonic RW2 (v4) samples where a split block wraps, as LibRaw reads
/// them (2026-10-28).
pub const ENGINE_VERSION: u32 = 59;
/// Rows per band of half floats (format 1; written by tests only).
#[cfg(test)]
const F16_ROWS_PER_BAND: u32 = 64;
/// Rows per band of JPEG XL (bands encode and decode in parallel; larger
/// ones keep the per-band overhead small).
const JXL_ROWS_PER_BAND: u32 = 256;
/// Long-edge sizes a proxy is made at: the smallest that covers the viewer's
/// fit preview, so resizing the window does not invalidate them.
pub const PROXY_SIZES: [u32; 4] = [2048, 2560, 3072, 3840];

/// Standard (about screen size) or 1:1 (the whole decode).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProxyKind {
    Standard,
    Full,
}

/// What a proxy was made from; any difference makes it stale.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProxyKey {
    pub file_size: u64,
    pub modified_ns: u128,
    /// Decoder settings that change the decoded pixels.
    pub decoder: String,
    pub engine: u32,
}

/// How the bands of a proxy file are stored.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
enum Encoding {
    /// Lossless half floats (format 1 has nothing else).
    #[default]
    F16,
    /// Lossy JPEG XL of the linear pixels after the header's `transfer`
    /// (proxy_codec).
    Jxl,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct Header {
    key: ProxyKey,
    full_width: u32,
    full_height: u32,
    width: u32,
    height: u32,
    rows_per_band: u32,
    /// Hash of the source's first and last bytes (`content_sample`): a copy
    /// of the photograph with another modification time (`cp` without `-p`,
    /// some sync tools) keeps its proxies. Absent in older proxies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample: Option<String>,
    /// Absent in format 1 (half floats).
    #[serde(default)]
    encoding: Encoding,
    /// The photograph: its path inside the session, or its absolute path
    /// outside sessions (`source_label`). Absent in format 1. The size
    /// limit for Standard previews keeps those of offline photographs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// What JPEG XL bands encode (absent in format 2: the linear values).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transfer: Option<Transfer>,
    /// What the rendering reads from the RAW besides its pixels (base look,
    /// colour, lens defaults...), so that the photograph renders offline as
    /// online (rawtonic_engine::source_facts). Absent before 2026-10-10:
    /// such previews render offline without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    facts: Option<SourceFacts>,
    /// The RAW's EXIF as the editor shows it (capture details offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exif: Option<HashMap<String, String>>,
}

impl Header {
    /// Whether this file is in the current encoding. Others are still read,
    /// and replaced whenever the RAW is decoded.
    fn is_current_encoding(&self) -> bool {
        self.encoding == Encoding::Jxl
            && self.transfer == Some(proxy_codec::TRANSFER)
            && self
                .facts
                .as_ref()
                .is_some_and(|facts| facts.version == source_facts::VERSION)
    }
}

/// A proxy read from disk: the reduced image and the size of the full decode.
pub struct Proxy {
    pub image: DynamicImage,
    pub full_width: u32,
    pub full_height: u32,
    /// What it was made from (its header's key).
    pub key: ProxyKey,
    /// The measures of the decode it was made from (its facts).
    pub measures: Option<DecodeMeasures>,
}

/// The measures a proxy's facts keep of the decode it was made from.
fn measures_of(facts: Option<&SourceFacts>) -> Option<DecodeMeasures> {
    facts.map(|facts| DecodeMeasures {
        noise_level: facts.noise_level,
        image_key: facts.measured_image_key(),
    })
}

pub use rawtonic_engine::session_layout::session_root;

/// Where previews of photographs outside sessions live (the app's cache
/// folder, as Lightroom keeps previews beside its catalog); set at start-up.
/// Unset (tests, headless tools), such photographs have no proxies.
static OUTSIDE_SESSIONS: OnceLock<PathBuf> = OnceLock::new();

pub fn set_outside_sessions_dir(dir: PathBuf) {
    let _ = OUTSIDE_SESSIONS.set(dir);
}

/// The previews folder for `source` and the file stem of its previews:
/// inside a session named after its path in the session, so a moved session
/// keeps its proxies; elsewhere after its absolute path.
fn location(source: &Path) -> Option<(PathBuf, String)> {
    location_with(source, OUTSIDE_SESSIONS.get().map(PathBuf::as_path))
}

fn location_with(source: &Path, outside: Option<&Path>) -> Option<(PathBuf, String)> {
    if let Some(root) = session_root(source) {
        let relative = source.strip_prefix(&root).ok()?;
        return Some((
            root.join(".cache").join("previews"),
            file_stem_for(&relative.to_string_lossy()),
        ));
    }
    let dir = outside?;
    Some((dir.to_path_buf(), file_stem_for(&source.to_string_lossy())))
}

/// The file stem of the previews of the photograph `source_label` names
/// (its path in the session, or its absolute path outside sessions).
fn file_stem_for(label: &str) -> String {
    blake3::hash(label.as_bytes()).to_hex()[..32].to_string()
}

/// How a proxy header names `source`: its path inside its session (so a
/// moved session still finds it), else its absolute path.
fn source_label(source: &Path) -> String {
    session_root(source)
        .and_then(|root| source.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| source.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The photograph a proxy in `dir` was made from, by its header's label: a
/// relative label lies in the session two levels above `dir`
/// (`<session>/.cache/previews`).
fn labelled_source(dir: &Path, label: &str) -> Option<PathBuf> {
    let path = Path::new(label);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    Some(dir.parent()?.parent()?.join(path))
}

/// Whether a header label names the proxy file at `path` (its file stem is
/// the label's hash, as `location` names it).
fn labels_file(label: &str, path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.split('.').next() == Some(file_stem_for(label).as_str()))
}

/// The previews folder of photographs outside sessions, once set.
pub fn outside_sessions_dir() -> Option<PathBuf> {
    OUTSIDE_SESSIONS.get().cloned()
}

/// The previews folder holding the previews of the photographs in `folder`.
pub fn previews_dir_for_folder(folder: &Path) -> Option<PathBuf> {
    location(&folder.join("_")).map(|(dir, _)| dir)
}

/// Where the Standard proxy of `source` lives. None without a previews
/// folder (outside sessions before start-up named one).
pub fn proxy_path(source: &Path) -> Option<PathBuf> {
    proxy_path_of(source, ProxyKind::Standard)
}

const FULL_SUFFIX: &str = ".full.rtpx";

/// Where the proxy of `kind` of `source` lives.
pub fn proxy_path_of(source: &Path, kind: ProxyKind) -> Option<PathBuf> {
    let (dir, name) = location(source)?;
    Some(dir.join(match kind {
        ProxyKind::Standard => format!("{name}.rtpx"),
        ProxyKind::Full => format!("{name}{FULL_SUFFIX}"),
    }))
}

/// Whether the RAW itself can be read now: it can be opened and is not a
/// cloud file waiting to be downloaded (reading that starts the download).
/// Otherwise the photograph is offline and opens on its proxies.
pub fn source_available(source: &Path) -> bool {
    source_readable(source) && !crate::file_management::is_cloud_placeholder(source)
}

/// Whether the RAW exists and can be opened (a cloud file not downloaded
/// yet can: reading it downloads it).
pub fn source_readable(source: &Path) -> bool {
    source.is_file() && fs::File::open(source).is_ok()
}

/// A hash of the size and the first and last 256 KiB of `source`.
fn content_sample(source: &Path) -> Option<String> {
    use std::io::{Seek, SeekFrom};
    const PART: u64 = 256 * 1024;
    let mut file = fs::File::open(source).ok()?;
    let len = file.metadata().ok()?.len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());
    let mut buffer = vec![0u8; PART.min(len) as usize];
    file.read_exact(&mut buffer).ok()?;
    hasher.update(&buffer);
    if len > PART {
        let tail = PART.min(len - PART);
        file.seek(SeekFrom::Start(len - tail)).ok()?;
        buffer.resize(tail as usize, 0);
        file.read_exact(&mut buffer).ok()?;
        hasher.update(&buffer);
    }
    Some(hasher.finalize().to_hex()[..32].to_string())
}

/// Whether the proxies of the RAW `source` (made for its master recipe's
/// decode variant, `header_is_current`) stand for a photograph of it shown
/// with `recipe`: a virtual copy whose recipe asks for another variant
/// (colour noise filter, dead-photosite test) must not be shown from them.
pub fn stands_for(source: &Path, recipe: &serde_json::Value) -> bool {
    // Offline, the frames the file's versions develop are the ones its
    // previews keep (`source_frame::frames` reads the registered facts).
    if source_facts::registered(&source.to_string_lossy()).is_none() {
        prepare_offline(source);
    }
    let source = source.to_string_lossy();
    crate::image_loader::decode_variant(recipe, &source)
        == crate::image_loader::stored_decode_variant(&source)
}

/// The key a proxy of `source` made now from a decode for `variant` would
/// carry.
pub fn current_key(
    source: &Path,
    settings: &AppSettings,
    variant: DecodeVariant,
) -> Option<ProxyKey> {
    let meta = fs::metadata(source).ok()?;
    let modified_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(ProxyKey {
        file_size: meta.len(),
        modified_ns,
        decoder: decoder_key(settings, variant),
        engine: ENGINE_VERSION,
    })
}

/// The decoder settings that change the decoded pixels, and the decode
/// variant (`image_loader::load_base_image_from_bytes_for`): the multi-scale
/// colour noise filter (no decode-time chroma clean-up) and, appended only
/// when set so earlier proxies keep their key, the second dead-photosite
/// test and a frame other than version 0's (`source_frame`).
fn decoder_key(settings: &AppSettings, variant: DecodeVariant) -> String {
    let key = format!(
        "hl={:?};linear={};cnr={:?};sharp={:?};nonraw={:?};raw9={:?};cnrms={}",
        settings.raw_highlight_compression.unwrap_or(2.5),
        settings.linear_raw_mode,
        settings.raw_preprocessing_color_nr.unwrap_or(0.5),
        settings.raw_preprocessing_sharpening.unwrap_or(0.35),
        settings.apply_preprocessing_to_non_raws.unwrap_or(false),
        settings.use_apple_raw9.unwrap_or(false),
        u8::from(variant.multiscale),
    );
    let key = if variant.dead_test == DeadTest::Adjacent {
        key + ";dead=adjacent"
    } else {
        key
    };
    if variant.frame > 0 {
        format!("{key};frame={}", variant.frame)
    } else {
        key
    }
}

/// The decoder settings part of a proxy key's `decoder` (without the decode
/// variant `decoder_key` appends).
fn decoder_settings(decoder: &str) -> &str {
    decoder.split(";cnrms=").next().unwrap_or(decoder)
}

/// The decode variant a proxy key's `decoder` part was made for (the
/// inverse of `decoder_key`'s variant fields).
pub(crate) fn variant_of_key(decoder: &str) -> DecodeVariant {
    DecodeVariant {
        multiscale: decoder.contains(";cnrms=1"),
        dead_test: if decoder.contains(";dead=adjacent") {
            DeadTest::Adjacent
        } else {
            DeadTest::SameColour
        },
        frame: decoder
            .split_once(";frame=")
            .and_then(|(_, frame)| frame.split(';').next()?.parse().ok())
            .unwrap_or(0),
    }
}

/// Whether previews made for frame `proxy_frame` (`DecodeVariant::frame`,
/// ADR 0020) can show the RAW at `source` with its stored recipe `recipe`:
/// any frame for a never-edited photograph (no coordinates to misplace; its
/// first edit takes the frame shown, src/utils/sourceFrame.ts), else the
/// recipe's own (a Reset's recipe states it: `source_frame::reset_recipe`). Which frame the recipe's version develops of the file is
/// read from the previews' own `facts` when they keep it (offline nothing
/// may be registered yet), else from the file or the registered facts;
/// where nothing tells it, the version itself, so an edit waits for the RAW
/// rather than show its coordinates on another frame. The one frame check
/// of every preview read from disk (`header_is_current`: offline any other
/// decode variant is read).
fn frame_fits(
    proxy_frame: u32,
    recipe: &serde_json::Value,
    facts: Option<&SourceFacts>,
    source: &str,
) -> bool {
    use rawtonic_engine::source_frame;
    if rawtonic_engine::image_processing::is_new_recipe(recipe)
        && recipe.get(source_frame::KEY).is_none()
    {
        return true;
    }
    let requested = source_frame::requested(recipe);
    let wanted = match facts.and_then(|facts| facts.frames.as_deref()) {
        Some(frames) if requested > 0 => source_frame::effective_in(frames, requested),
        _ => source_frame::effective(source, requested),
    };
    proxy_frame == wanted
}

/// Whether a proxy with `header` stands for `source` as it is now. Its key
/// must match; a copy with another modification time still matches when its
/// size and content sample do. A source that cannot be read (offline) is
/// taken as unchanged: a proxy made with the current decoder is the last
/// known state of the photograph, whatever decode variant it was made for
/// (no decode can follow to replace it: previews made before a recipe reset
/// or before never-edited photographs moved to a newer variant must still
/// open), but of the stored recipe's frame (`frame_fits`). Online, the
/// decode must also have been made for the decode variant of the
/// photograph's stored (master) recipe.
fn header_is_current(header: &Header, source: &Path, settings: &AppSettings) -> bool {
    let source_str = source.to_string_lossy();
    let recipe = crate::exif_processing::load_sidecar(
        &crate::file_management::parse_virtual_path(&source_str).1,
    )
    .adjustments;
    let variant = crate::image_loader::decode_variant(&recipe, &source_str);
    let decoder_matches =
        header.key.engine == ENGINE_VERSION && header.key.decoder == decoder_key(settings, variant);
    let settings_match = || {
        header.key.engine == ENGINE_VERSION
            && decoder_settings(&header.key.decoder)
                == decoder_settings(&decoder_key(settings, variant))
            && frame_fits(
                variant_of_key(&header.key.decoder).frame,
                &recipe,
                header.facts.as_ref(),
                &source_str,
            )
    };
    let Some(key) = current_key(source, settings, variant) else {
        return settings_match();
    };
    if header.key == key {
        return true;
    }
    // Offline with its metadata still there (a cloud placeholder, a file
    // that cannot be opened): the same file's preview, any variant.
    if !source_available(source) {
        return settings_match()
            && header.key.file_size == key.file_size
            && header.key.modified_ns == key.modified_ns;
    }
    decoder_matches
        && header.key.file_size == key.file_size
        && header.sample.is_some()
        && !crate::file_management::is_cloud_placeholder(source)
        && header.sample == content_sample(source)
}

/// The proxy size for a viewer preview of `preview_dim` pixels.
pub fn proxy_size_for(preview_dim: u32) -> u32 {
    PROXY_SIZES
        .into_iter()
        .find(|&size| size >= preview_dim)
        .unwrap_or(PROXY_SIZES[PROXY_SIZES.len() - 1])
}

/// The Standard proxy size for a viewer preview of `preview_dim` pixels (0:
/// not known yet), at least the size chosen in the preferences.
pub fn standard_size(preview_dim: u32, settings: &AppSettings) -> u32 {
    proxy_size_for(
        preview_dim
            .max(settings.standard_preview_size.unwrap_or(0))
            .max(PROXY_SIZES[0]),
    )
}

/// How long a 1:1 proxy may go unused before it is discarded (None: never).
pub fn full_expiry(settings: &AppSettings) -> Option<Duration> {
    match settings.discard_full_previews_days.unwrap_or(30) {
        0 => None,
        days => Some(Duration::from_secs(u64::from(days) * 24 * 3600)),
    }
}

/// A band of format 1 (half floats); written by tests only, format 1 files
/// are only read.
#[cfg(test)]
fn encode_f16_band(pixels: &[f32], width: usize) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;
    let rows = pixels.len() / (width * 3);
    let count = pixels.len();
    let mut low = Vec::with_capacity(count);
    let mut high = Vec::with_capacity(count);
    for row in 0..rows {
        let start = row * width * 3;
        let mut previous = [0u16; 3];
        for x in 0..width {
            for (c, last) in previous.iter_mut().enumerate() {
                let bits = f16::from_f32(pixels[start + x * 3 + c]).to_bits();
                let delta = bits.wrapping_sub(*last);
                *last = bits;
                low.push(delta as u8);
                high.push((delta >> 8) as u8);
            }
        }
    }
    low.extend_from_slice(&high);
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&low).expect("writing to memory");
    encoder.finish().expect("writing to memory")
}

fn decode_f16_band(bytes: &[u8], width: usize, rows: usize) -> Option<Vec<f32>> {
    let count = width * rows * 3;
    let mut planes = Vec::with_capacity(count * 2);
    DeflateDecoder::new(bytes).read_to_end(&mut planes).ok()?;
    if planes.len() != count * 2 {
        return None;
    }
    let (low, high) = planes.split_at(count);
    let mut out = Vec::with_capacity(count);
    for row in 0..rows {
        let mut previous = [0u16; 3];
        for x in 0..width {
            let i = (row * width + x) * 3;
            for (c, last) in previous.iter_mut().enumerate() {
                let delta = low[i + c] as u16 | (high[i + c] as u16) << 8;
                *last = last.wrapping_add(delta);
                out.push(f16::from_bits(*last).to_f32());
            }
        }
    }
    Some(out)
}

/// The proxy file's bytes for `image` (already at proxy size), without a
/// content sample.
#[cfg(test)]
pub fn encode(image: &DynamicImage, full_width: u32, full_height: u32, key: ProxyKey) -> Vec<u8> {
    encode_with_sample(
        image,
        full_width,
        full_height,
        key,
        None,
        None,
        SourceData::default(),
        &|| {},
    )
}

/// What a proxy keeps of its RAW besides the pixels (see `Header`).
#[derive(Default)]
struct SourceData {
    facts: Option<SourceFacts>,
    exif: Option<HashMap<String, String>>,
}

impl SourceData {
    /// Read from the RAW at `source` (it must be readable); `measures`:
    /// those of the decode the proxy is made from (the facts would otherwise
    /// take the last full decode's, which may be of another recipe).
    fn read(source: &Path, measures: Option<DecodeMeasures>) -> SourceData {
        let path = source.to_string_lossy();
        let exif = fs::read(source)
            .ok()
            .map(|bytes| crate::exif_processing::read_exif_data(&path, &bytes))
            .filter(|exif| !exif.is_empty());
        let mut facts = source_facts::collect(&path);
        if let Some(measures) = measures {
            facts.noise_level = measures.noise_level;
            facts.set_image_key(measures.image_key);
        }
        SourceData {
            facts: Some(facts),
            exif,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_with_sample(
    image: &DynamicImage,
    full_width: u32,
    full_height: u32,
    key: ProxyKey,
    sample: Option<String>,
    source: Option<String>,
    data: SourceData,
    before_band: &(dyn Fn() + Sync),
) -> Vec<u8> {
    // Decodes are float RGB already: no copy of the whole frame (half a
    // gigabyte at 45 MP) for a 1:1 preview.
    let converted;
    let rgb = match image.as_rgb32f() {
        Some(rgb) => rgb,
        None => {
            converted = image.to_rgb32f();
            &converted
        }
    };
    let (width, height) = rgb.dimensions();
    let encoding = Encoding::Jxl;
    let rows_per_band = JXL_ROWS_PER_BAND;
    let band_len = (width * rows_per_band * 3) as usize;
    let transfer = proxy_codec::TRANSFER;
    let bands: Vec<Vec<u8>> = rgb
        .as_raw()
        .par_chunks(band_len)
        .map(|band| {
            before_band();
            proxy_codec::encode(band, width as usize, proxy_codec::DISTANCE, transfer)
                .expect("a whole number of rows")
        })
        .collect();
    let header = serde_json::to_vec(&Header {
        key,
        full_width,
        full_height,
        width,
        height,
        rows_per_band,
        sample,
        encoding,
        source,
        transfer: Some(transfer),
        facts: data.facts,
        exif: data.exif,
    })
    .expect("serializable header");
    let mut out =
        Vec::with_capacity(16 + header.len() + bands.iter().map(|b| b.len() + 8).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    for (i, band) in bands.iter().enumerate() {
        let rows = rows_per_band.min(height - i as u32 * rows_per_band);
        out.extend_from_slice(&rows.to_le_bytes());
        out.extend_from_slice(&(band.len() as u32).to_le_bytes());
        out.extend_from_slice(band);
    }
    out
}

fn read_u32(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let value = u32::from_le_bytes(bytes.get(*at..*at + 4)?.try_into().ok()?);
    *at += 4;
    Some(value)
}

/// The proxy in `bytes` when it was made with `key` (else None: stale, or
/// another format version).
#[cfg(test)]
pub fn decode(bytes: &[u8], key: &ProxyKey) -> Option<Proxy> {
    decode_checked(bytes, |header| &header.key == key)
}

/// The header of the proxy in `bytes` and where its image starts.
fn parse_header(bytes: &[u8]) -> Option<(Header, usize)> {
    if bytes.get(..4)? != MAGIC {
        return None;
    }
    let mut at = 4;
    if !READ_VERSIONS.contains(&read_u32(bytes, &mut at)?) {
        return None;
    }
    let header_len = read_u32(bytes, &mut at)? as usize;
    let header: Header = serde_json::from_slice(bytes.get(at..at + header_len)?).ok()?;
    Some((header, at + header_len))
}

fn decode_checked(bytes: &[u8], accept: impl FnOnce(&Header) -> bool) -> Option<Proxy> {
    let (header, mut at) = parse_header(bytes)?;
    if !accept(&header) || header.width == 0 || header.height == 0 {
        return None;
    }
    let mut bands = Vec::new();
    let mut rows_total = 0u32;
    while rows_total < header.height {
        let rows = read_u32(bytes, &mut at)?;
        let len = read_u32(bytes, &mut at)? as usize;
        bands.push((rows, bytes.get(at..at + len)?));
        at += len;
        rows_total += rows;
    }
    if rows_total != header.height {
        return None;
    }
    let width = header.width as usize;
    let decoded: Option<Vec<Vec<f32>>> = bands
        .par_iter()
        .map(|&(rows, data)| match header.encoding {
            Encoding::Jxl => proxy_codec::decode(
                data,
                width,
                rows as usize,
                header.transfer.unwrap_or(Transfer::Linear),
            )
            .ok(),
            Encoding::F16 => decode_f16_band(data, width, rows as usize),
        })
        .collect();
    let pixels: Vec<f32> = decoded?.concat();
    let image = Rgb32FImage::from_raw(header.width, header.height, pixels)?;
    Some(Proxy {
        image: DynamicImage::ImageRgb32F(image),
        full_width: header.full_width,
        full_height: header.full_height,
        measures: measures_of(header.facts.as_ref()),
        key: header.key,
    })
}

/// Reads the Standard proxy of `source` if it exists and is current (or the
/// source is offline and it was made with the current decoder).
pub fn read(source: &Path, settings: &AppSettings) -> Option<Proxy> {
    read_kind(source, ProxyKind::Standard, settings)
}

/// Reads the proxy of `kind` of `source`, as `read`. Reading counts as a
/// use (automatic discarding goes by the last use).
pub fn read_kind(source: &Path, kind: ProxyKind, settings: &AppSettings) -> Option<Proxy> {
    let path = proxy_path_of(source, kind)?;
    let _file_use = crate::storage_fs::pin_file(&path);
    let bytes = fs::read(&path).ok()?;
    let proxy = decode_checked(&bytes, |header| {
        let current = header_is_current(header, source, settings);
        if current && !source_available(source) {
            register_facts(source, header);
        }
        current
    })?;
    mark_used(&path);
    Some(proxy)
}

/// The size of the current 1:1 proxy of `source` (its full decode's), read
/// from its header.
pub fn full_size(source: &Path, settings: &AppSettings) -> Option<(u32, u32)> {
    let header = read_header(&proxy_path_of(source, ProxyKind::Full)?)?;
    (header.width == header.full_width
        && header.height == header.full_height
        && header_is_current(&header, source, settings))
    .then_some((header.width, header.height))
}

/// Part of a 1:1 proxy: columns `col0..col0 + cols` of RGB rows `row0..`
/// of the full-resolution frame `width` × `height`.
pub struct ProxyRows {
    /// What the proxy read was made from (its header's key).
    pub key: ProxyKey,
    /// The measures of the decode it was made from (its facts).
    pub measures: Option<DecodeMeasures>,
    pub pixels: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub row0: u32,
    pub col0: u32,
    pub cols: u32,
}

/// Columns `cols` of rows `rows` (start, end exclusive each) of the 1:1
/// proxy of `source` when it is current, decoding only that part of the
/// bands that hold them (a zoomed multi view cell needs a few hundred rows
/// and columns, not the whole photograph). The rows returned may reach
/// beyond `rows` to the bands' edges. Reading counts as a use, as
/// `read_kind`.
pub fn read_full_rows(
    source: &Path,
    settings: &AppSettings,
    rows: (u32, u32),
    cols: (u32, u32),
) -> Option<ProxyRows> {
    let path = proxy_path_of(source, ProxyKind::Full)?;
    let _file_use = crate::storage_fs::pin_file(&path);
    let bytes = fs::read(&path).ok()?;
    let proxy = decode_rows(&bytes, rows, cols, |header| {
        let current = header_is_current(header, source, settings);
        if current && !source_available(source) {
            register_facts(source, header);
        }
        current
    })?;
    mark_used(&path);
    Some(proxy)
}

/// Columns `cols` of the bands of the full-resolution proxy in `bytes`
/// that hold `rows`.
fn decode_rows(
    bytes: &[u8],
    (start, end): (u32, u32),
    (left, right): (u32, u32),
    accept: impl FnOnce(&Header) -> bool,
) -> Option<ProxyRows> {
    let (header, mut at) = parse_header(bytes)?;
    // Only a whole-resolution proxy has the frame's rows.
    if !accept(&header)
        || header.width == 0
        || header.height == 0
        || header.width != header.full_width
        || header.height != header.full_height
        || start >= end
        || end > header.height
        || left >= right
        || right > header.width
    {
        return None;
    }
    let mut wanted = Vec::new();
    let mut row = 0u32;
    let mut row0 = None;
    while row < header.height {
        let rows = read_u32(bytes, &mut at)?;
        let len = read_u32(bytes, &mut at)? as usize;
        let data = bytes.get(at..at + len)?;
        at += len;
        if rows == 0 {
            return None;
        }
        if row + rows > start && row < end {
            row0.get_or_insert(row);
            wanted.push((rows, data));
        }
        row += rows;
        if row >= end {
            break;
        }
    }
    let width = header.width as usize;
    let columns = (left as usize, (right - left) as usize);
    let decoded: Option<Vec<Vec<f32>>> = wanted
        .par_iter()
        .map(|&(rows, data)| match header.encoding {
            Encoding::Jxl => proxy_codec::decode_columns(
                data,
                width,
                rows as usize,
                header.transfer.unwrap_or(Transfer::Linear),
                columns,
            )
            .ok(),
            Encoding::F16 => {
                let band = decode_f16_band(data, width, rows as usize)?;
                Some(
                    band.chunks_exact(width * 3)
                        .flat_map(|row| &row[columns.0 * 3..(columns.0 + columns.1) * 3])
                        .copied()
                        .collect(),
                )
            }
        })
        .collect();
    Some(ProxyRows {
        measures: measures_of(header.facts.as_ref()),
        key: header.key.clone(),
        pixels: decoded?.concat(),
        width: header.width,
        height: header.height,
        row0: row0?,
        col0: left,
        cols: right - left,
    })
}

/// Lets the rendering of the offline `source` use the facts its preview
/// keeps of the RAW (source_facts), so it renders as it did online.
/// Previews made before they were kept render without them.
fn register_facts(source: &Path, header: &Header) {
    let path = source.to_string_lossy();
    match &header.facts {
        Some(facts) => source_facts::register(&path, facts.clone()),
        None => log::info!(
            "The preview of {path} keeps no camera data (made before 2026-10-10): it renders offline without its base look and lens defaults until it is rebuilt"
        ),
    }
}

/// For an offline `source` (its RAW cannot be read): registers the facts
/// its previews keep (1:1 first, else Standard) for the rendering and
/// returns the EXIF they keep. A source that can be read uses its file:
/// anything registered for it is dropped.
pub fn prepare_offline(source: &Path) -> Option<HashMap<String, String>> {
    if source_available(source) {
        source_facts::forget(&source.to_string_lossy());
        return None;
    }
    let header = [ProxyKind::Full, ProxyKind::Standard]
        .into_iter()
        .filter_map(|kind| read_header(&proxy_path_of(source, kind)?))
        .find(|header| header.facts.is_some())?;
    register_facts(source, &header);
    header.exif
}

/// The header of the proxy file at `path`, read without its image.
fn read_header(path: &Path) -> Option<Header> {
    let mut file = fs::File::open(path).ok()?;
    let mut head = vec![0u8; 12];
    file.read_exact(&mut head).ok()?;
    if &head[..4] != MAGIC
        || !READ_VERSIONS.contains(&u32::from_le_bytes(head[4..8].try_into().ok()?))
    {
        return None;
    }
    let header_len = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
    head.resize(12 + header_len, 0);
    file.read_exact(&mut head[12..]).ok()?;
    parse_header(&head).map(|(header, _)| header)
}

/// The long edge of the Standard proxy stored for `source`, if current
/// (made from the photograph as it is now, in the current encoding).
pub fn stored_size(source: &Path, settings: &AppSettings) -> Option<u32> {
    let header = read_header(&proxy_path(source)?)?;
    (header.is_current_encoding() && header_is_current(&header, source, settings))
        .then_some(header.width.max(header.height))
}

/// Whether a current 1:1 proxy of `source` is stored (in the current
/// encoding: building replaces a format 1 file).
/// What the current 1:1 proxy of `source` was made from (its header's
/// key), as reading it validates it; None without a current one.
pub fn full_key(source: &Path, settings: &AppSettings) -> Option<ProxyKey> {
    let header = read_header(&proxy_path_of(source, ProxyKind::Full)?)?;
    (header.is_current_encoding() && header_is_current(&header, source, settings))
        .then_some(header.key)
}

pub fn has_full(source: &Path, settings: &AppSettings) -> bool {
    proxy_path_of(source, ProxyKind::Full)
        .and_then(|path| read_header(&path))
        .is_some_and(|header| {
            header.is_current_encoding() && header_is_current(&header, source, settings)
        })
}

/// Writes the Standard proxy of `source` from its full decode, at `size` on
/// the long edge, atomically (temporary file in the same folder, then rename).
/// `variant`: the decode variant the decode was made for.
/// `measures`: the decode's (`image_loader::DecodeMeasures`).
pub fn write(
    source: &Path,
    full: &DynamicImage,
    size: u32,
    settings: &AppSettings,
    variant: DecodeVariant,
    measures: Option<DecodeMeasures>,
) -> Result<Proxy, String> {
    let (full_width, full_height) = full.dimensions();
    let reduced = if full_width.max(full_height) > size {
        crate::image_processing::downscale_f32_image(full, size, size)
    } else {
        full.clone()
    };
    let key = store(
        source,
        ProxyKind::Standard,
        &reduced,
        (full_width, full_height),
        settings,
        variant,
        measures,
        &|| {},
    )?;
    Ok(Proxy {
        image: reduced,
        full_width,
        full_height,
        key,
        measures,
    })
}

/// Writes the 1:1 proxy of `source`: its whole decode, atomically (made for
/// the decode variant `variant`, as `write`).
pub fn write_full(
    source: &Path,
    full: &DynamicImage,
    settings: &AppSettings,
    variant: DecodeVariant,
    measures: Option<DecodeMeasures>,
) -> Result<(), String> {
    write_full_with(source, full, settings, variant, measures, &|| {})
}

/// `write_full`, calling `before_band` before each band is encoded (from
/// the encoding threads): it may wait, holding the encode back while more
/// urgent work runs.
pub fn write_full_with(
    source: &Path,
    full: &DynamicImage,
    settings: &AppSettings,
    variant: DecodeVariant,
    measures: Option<DecodeMeasures>,
    before_band: &(dyn Fn() + Sync),
) -> Result<(), String> {
    store(
        source,
        ProxyKind::Full,
        full,
        full.dimensions(),
        settings,
        variant,
        measures,
        before_band,
    )
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn store(
    source: &Path,
    kind: ProxyKind,
    image: &DynamicImage,
    (full_width, full_height): (u32, u32),
    settings: &AppSettings,
    variant: DecodeVariant,
    measures: Option<DecodeMeasures>,
    before_band: &(dyn Fn() + Sync),
) -> Result<ProxyKey, String> {
    let path = proxy_path_of(source, kind).ok_or("No previews folder")?;
    let _file_use = crate::storage_fs::pin_file(&path);
    let key = current_key(source, settings, variant).ok_or("Source file unavailable")?;
    let sample = content_sample(source);
    let bytes = encode_with_sample(
        image,
        full_width,
        full_height,
        key.clone(),
        sample,
        Some(source_label(source)),
        SourceData::read(source, measures),
        before_band,
    );
    fs::create_dir_all(path.parent().ok_or("No parent")?).map_err(|e| e.to_string())?;
    rawtonic_metadata::atomic_write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(key)
}

/// Records a use of the proxy file at `path` in its modification time (its
/// content is not touched).
fn mark_used(path: &Path) {
    mark_used_at(path, SystemTime::now());
}

fn mark_used_at(path: &Path, time: SystemTime) {
    if let Ok(file) = fs::File::options().append(true).open(path) {
        let _ = file.set_modified(time);
    }
}

/// Test-only disk counters for preview retention policy checks.
#[cfg(test)]
#[derive(Debug, Default, Clone, PartialEq)]
struct PreviewUsage {
    pub standard_count: u64,
    pub standard_bytes: u64,
    pub full_count: u64,
    pub full_bytes: u64,
}

fn proxy_files(dir: &Path) -> impl Iterator<Item = (PathBuf, ProxyKind, fs::Metadata)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = if name.ends_with(FULL_SUFFIX) {
                ProxyKind::Full
            } else if name.ends_with(".rtpx") {
                ProxyKind::Standard
            } else {
                return None;
            };
            let meta = fs::symlink_metadata(entry.path())
                .ok()
                .filter(fs::Metadata::is_file)?;
            Some((entry.path(), kind, meta))
        })
}

/// Test-only counters for preview retention policy checks.
#[cfg(test)]
fn usage(dir: &Path) -> PreviewUsage {
    let mut usage = PreviewUsage::default();
    for (_, kind, meta) in proxy_files(dir) {
        match kind {
            ProxyKind::Standard => {
                usage.standard_count += 1;
                usage.standard_bytes += meta.len();
            }
            ProxyKind::Full => {
                usage.full_count += 1;
                usage.full_bytes += meta.len();
            }
        }
    }
    usage
}

/// Removes the previews of the kinds in `kinds` from `dir`; with
/// `unused_for`, only those not used for that long. Returns how many went.
pub fn discard(dir: &Path, kinds: &[ProxyKind], unused_for: Option<Duration>) -> usize {
    let now = SystemTime::now();
    proxy_files(dir)
        .filter(|(_, kind, meta)| {
            kinds.contains(kind)
                && unused_for.is_none_or(|age| {
                    meta.modified()
                        .ok()
                        .and_then(|used| now.duration_since(used).ok())
                        .is_some_and(|idle| idle >= age)
                })
        })
        .filter(|(path, _, _)| crate::storage_fs::remove_if_unpinned(path))
        .count()
}

/// Removes the previews of the kinds in `kinds` of `source`.
pub fn discard_source(source: &Path, kinds: &[ProxyKind]) -> usize {
    kinds
        .iter()
        .filter_map(|&kind| proxy_path_of(source, kind))
        .filter(|path| crate::storage_fs::remove_if_unpinned(path))
        .count()
}

/// Moves the previews of `from` to where those of `to` live (a relinked
/// photograph keeps its previews). Only preview files move, each without
/// replacing one already there; a moved file's header then names `to`
/// (the size limit tells offline photographs by it). Returns how many moved.
pub fn carry_over(from: &Path, to: &Path) -> usize {
    [ProxyKind::Standard, ProxyKind::Full]
        .into_iter()
        .filter_map(|kind| Some((proxy_path_of(from, kind)?, proxy_path_of(to, kind)?)))
        .filter(|(old, new)| old != new && old.is_file())
        .filter(|(old, new)| {
            new.parent()
                .is_some_and(|dir| fs::create_dir_all(dir).is_ok())
                && rawtonic_fileops::rename_no_clobber(old, new).is_ok()
        })
        .inspect(|(_, new)| {
            if let Err(e) = relabel(new, &source_label(to)) {
                log::warn!("Could not relabel the preview {}: {e}", new.display());
            }
        })
        .count()
}

/// Rewrites the source label in the header of the proxy file at `path`,
/// atomically and without touching its image bytes or its format version;
/// its last-use time is kept.
fn relabel(path: &Path, label: &str) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let (mut header, image_at) = parse_header(&bytes).ok_or("Not a preview file")?;
    if header.source.as_deref() == Some(label) {
        return Ok(());
    }
    header.source = Some(label.to_string());
    let header = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(bytes.len() + header.len());
    out.extend_from_slice(&bytes[..8]);
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&bytes[image_at..]);
    let used = fs::metadata(path).and_then(|m| m.modified()).ok();
    rawtonic_metadata::atomic_write(path, out).map_err(|e| e.to_string())?;
    if let Some(used) = used {
        mark_used_at(path, used);
    }
    Ok(())
}

/// The most the Standard previews of one previews folder may take (None:
/// no limit), from the preferences (GB; unset 10, 0 no limit).
pub fn standard_limit(settings: &AppSettings) -> Option<u64> {
    match settings.standard_previews_limit_gb.unwrap_or(10) {
        0 => None,
        gb => Some(u64::from(gb) * 1_000_000_000),
    }
}

/// Keeps the Standard previews in `dir` within `limit` bytes: when they take
/// more, those used longest ago go until they take at most 90 % of it (so
/// the next writes do not each discard one). Previews of photographs whose
/// RAW cannot be read now are kept (offline, they are all there is), as
/// are those whose photograph is not known (format 1 files). A header
/// whose label does not name its file (carried over by a relink before
/// `carry_over` relabelled) says nothing about the photograph: such files
/// may go. They are made again when their photograph is opened. Returns how
/// many went.
pub fn enforce_standard_limit(dir: &Path, limit: u64) -> usize {
    let mut files: Vec<(PathBuf, u64, SystemTime)> = proxy_files(dir)
        .filter(|(_, kind, _)| *kind == ProxyKind::Standard)
        .map(|(path, _, meta)| {
            let used = meta.modified().unwrap_or(UNIX_EPOCH);
            (path, meta.len(), used)
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= limit {
        return 0;
    }
    let target = limit / 10 * 9;
    files.sort_by_key(|(_, _, used)| *used);
    let mut removed = 0;
    for (path, len, _) in files {
        if total <= target {
            break;
        }
        let evictable = read_header(&path)
            .and_then(|header| header.source)
            .is_some_and(|label| {
                !labels_file(&label, &path)
                    || labelled_source(dir, &label).is_some_and(|source| source_available(&source))
            });
        if evictable && crate::storage_fs::remove_if_unpinned(&path) {
            total -= len;
            removed += 1;
        }
    }
    removed
}

/// Proxies kept in memory: about 38 MB each at 2048 px (f32).
const PROXIES_IN_MEMORY: usize = 6;

/// Recently used proxies kept decoded in memory (the open photograph and its
/// filmstrip neighbours), so activating one needs no disk read.
#[derive(Default)]
pub struct ProxyMemory {
    entries: std::collections::VecDeque<crate::app_state::LoadedProxy>,
}

impl ProxyMemory {
    /// Keeps `proxy` (of the photograph at `path`, a virtual path) in memory.
    pub fn remember(&mut self, path: &str, proxy: Proxy) -> crate::app_state::LoadedProxy {
        let loaded = crate::app_state::LoadedProxy {
            path: path.to_string(),
            image: std::sync::Arc::new(proxy.image),
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw: true,
            made_for: Some(variant_of_key(&proxy.key.decoder)),
            measures: proxy.measures,
        };
        self.insert(loaded.clone());
        loaded
    }

    /// The proxy kept for `path` when it was made for `wanted` (a recipe
    /// reset or undone since may ask for another decode variant).
    pub fn get(
        &mut self,
        path: &str,
        wanted: DecodeVariant,
    ) -> Option<crate::app_state::LoadedProxy> {
        let at = self
            .entries
            .iter()
            .position(|p| p.path == path && (!p.is_raw || p.made_for == Some(wanted)))?;
        let entry = self.entries.remove(at)?;
        self.entries.push_front(entry.clone());
        Some(entry)
    }

    pub fn insert(&mut self, proxy: crate::app_state::LoadedProxy) {
        self.entries.retain(|p| p.path != proxy.path);
        self.entries.push_front(proxy);
        self.entries.truncate(PROXIES_IN_MEMORY);
    }

    /// Forgets the proxies of the photograph at `source` and its virtual
    /// copies (their previews were discarded).
    pub fn forget_source(&mut self, source: &str) {
        self.entries
            .retain(|p| p.path != source && !p.path.starts_with(&format!("{source}?vc=")));
    }
}

/// Offline photographs whose previews show one frame of the RAW, for the
/// preview consumers' frame tests.
#[cfg(test)]
pub(crate) mod frame_fixture {
    use super::*;

    /// `A.CR3` of a session under `dir` with the stored recipe `recipe`
    /// and Standard and 1:1 previews made for frame `frame`, then taken
    /// offline (moved away).
    pub(crate) fn offline_cr3(dir: &Path, recipe: serde_json::Value, frame: u32) -> PathBuf {
        let session = dir.join("Job");
        fs::create_dir_all(session.join("Capture")).unwrap();
        fs::write(session.join("session.json"), "{}").unwrap();
        fs::write(session.join("session.sqlite"), "").unwrap();
        let photo = session.join("Capture").join("A.CR3");
        fs::write(&photo, b"raw bytes").unwrap();
        let metadata = serde_json::json!({ "version": 1, "rating": 0, "adjustments": recipe });
        fs::write(photo.with_file_name("A.CR3.rrdata"), metadata.to_string()).unwrap();
        let settings = AppSettings::default();
        let variant = DecodeVariant {
            multiscale: true,
            dead_test: DeadTest::Adjacent,
            frame,
        };
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(80, 60, |x, y| {
            image::Rgb([x as f32 / 80.0, y as f32 / 60.0, 0.25])
        }));
        write(&photo, &image, 40, &settings, variant, None).unwrap();
        write_full(&photo, &image, &settings, variant, None).unwrap();
        fs::rename(&photo, dir.join("elsewhere.CR3")).unwrap();
        assert!(!source_available(&photo));
        photo
    }

    /// An edit of frame 1 (ADR 0020: a new edit of a CR3 whose frame
    /// version 1 changes).
    pub(crate) fn frame_one_edit() -> serde_json::Value {
        serde_json::json!({"exposure": 1.0, "renderVersion": 4, "sourceFrameVersion": 1})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a never-edited RAW's decode is made for.
    const NEW_EDIT: DecodeVariant = DecodeVariant {
        multiscale: true,
        dead_test: DeadTest::Adjacent,
        frame: 0,
    };
    /// A decode for an edit with the old colour noise filter.
    const LEGACY: DecodeVariant = DecodeVariant {
        multiscale: false,
        dead_test: DeadTest::SameColour,
        frame: 0,
    };

    fn key() -> ProxyKey {
        ProxyKey {
            file_size: 10,
            modified_ns: 20,
            decoder: "d".into(),
            engine: ENGINE_VERSION,
        }
    }

    #[test]
    fn proxies_round_trip_close_to_their_values() {
        // Ramps up to above white, steep in the shadows of the second
        // channel: within 4 % of the pixel's largest channel (the transfer
        // gives bright values less precision than the shadows; accuracy on
        // real photographs: crates/export/tests/proxy_codec_eval.rs).
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(301, 150, |x, y| {
            image::Rgb([
                0.01 + x as f32 / 300.0,
                0.01 + y as f32 / 37.0,
                2.5 - x as f32 / 120.0,
            ])
        }));
        let bytes = encode(&image, 3010, 1500, key());
        let proxy = decode(&bytes, &key()).expect("proxy");
        assert_eq!(proxy.image.dimensions(), (301, 150));
        assert_eq!((proxy.full_width, proxy.full_height), (3010, 1500));
        let (a, b) = (image.to_rgb32f(), proxy.image.to_rgb32f());
        for (p, q) in a.pixels().zip(b.pixels()) {
            let largest = p[0].max(p[1]).max(p[2]);
            for c in 0..3 {
                assert!(
                    (p[c] - q[c]).abs() <= largest * 0.04 + 1e-3,
                    "{p:?} vs {q:?}"
                );
            }
        }
        // Far below half floats (6 bytes per pixel).
        assert!(bytes.len() < 301 * 150 * 6 / 8, "{} bytes", bytes.len());
    }

    /// A proxy file as format 1 wrote it (half floats, no encoding field).
    fn format_1(image: &DynamicImage, key: ProxyKey, sample: Option<String>) -> Vec<u8> {
        let rgb = image.to_rgb32f();
        let (width, height) = rgb.dimensions();
        let mut header = serde_json::json!({
            "key": key, "fullWidth": width, "fullHeight": height,
            "width": width, "height": height, "rowsPerBand": F16_ROWS_PER_BAND,
        });
        if let Some(sample) = sample {
            header["sample"] = sample.into();
        }
        let header = serde_json::to_vec(&header).unwrap();
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        for (i, band) in rgb
            .as_raw()
            .chunks((width * F16_ROWS_PER_BAND * 3) as usize)
            .enumerate()
        {
            let rows = F16_ROWS_PER_BAND.min(height - i as u32 * F16_ROWS_PER_BAND);
            let bytes = encode_f16_band(band, width as usize);
            out.extend_from_slice(&rows.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&bytes);
        }
        out
    }

    #[test]
    fn format_1_files_are_read_and_replaced_when_the_raw_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let image = gradient(80, 60);
        let key = current_key(&photo, &settings, NEW_EDIT).unwrap();
        for kind in [ProxyKind::Standard, ProxyKind::Full] {
            let path = proxy_path_of(&photo, kind).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format_1(&image, key.clone(), None)).unwrap();
        }
        // Read at half-float precision (offline photographs open on them)...
        let read = read(&photo, &settings).unwrap().image.to_rgb32f();
        for (p, q) in image.to_rgb32f().as_raw().iter().zip(read.as_raw()) {
            assert!((p - q).abs() <= p.abs() * 1e-3 + 1e-4);
        }
        assert!(read_kind(&photo, ProxyKind::Full, &settings).is_some());
        // ...but not current: the next decode or build writes format 3.
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
        write(&photo, &image, 80, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &image, &settings, NEW_EDIT, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), Some(80));
        assert!(has_full(&photo, &settings));
        let path = proxy_path(&photo).unwrap();
        assert_eq!(fs::read(&path).unwrap()[4..8], FORMAT_VERSION.to_le_bytes());
        let header = read_header(&path).unwrap();
        assert_eq!(header.encoding, Encoding::Jxl);
        assert_eq!(header.transfer, Some(proxy_codec::TRANSFER));
        assert_eq!(
            header.source,
            Some(
                Path::new("Capture")
                    .join("A.RAF")
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    /// A proxy file as format 2 wrote it (linear JPEG XL at distance 0.25,
    /// no transfer field).
    fn format_2(image: &DynamicImage, key: ProxyKey, source: &str) -> Vec<u8> {
        let rgb = image.to_rgb32f();
        let (width, height) = rgb.dimensions();
        let header = serde_json::to_vec(&serde_json::json!({
            "key": key, "fullWidth": width, "fullHeight": height,
            "width": width, "height": height, "rowsPerBand": JXL_ROWS_PER_BAND,
            "encoding": "jxl", "source": source,
        }))
        .unwrap();
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        for (i, band) in rgb
            .as_raw()
            .chunks((width * JXL_ROWS_PER_BAND * 3) as usize)
            .enumerate()
        {
            let rows = JXL_ROWS_PER_BAND.min(height - i as u32 * JXL_ROWS_PER_BAND);
            let bytes = proxy_codec::encode(band, width as usize, 0.25, Transfer::Linear).unwrap();
            out.extend_from_slice(&rows.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&bytes);
        }
        out
    }

    #[test]
    fn format_2_files_are_read_and_replaced_when_the_raw_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let image = gradient(300, 280);
        let key = current_key(&photo, &settings, NEW_EDIT).unwrap();
        for kind in [ProxyKind::Standard, ProxyKind::Full] {
            let path = proxy_path_of(&photo, kind).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format_2(&image, key.clone(), "Capture/A.RAF")).unwrap();
        }
        // Read as linear values (no transfer to invert)...
        let read = read(&photo, &settings).unwrap().image.to_rgb32f();
        assert_eq!(read.dimensions(), (300, 280));
        for (p, q) in image.to_rgb32f().pixels().zip(read.pixels()) {
            let largest = p[0].max(p[1]).max(p[2]);
            for c in 0..3 {
                assert!((p[c] - q[c]).abs() <= largest * 0.03 + 1e-3, "{p:?} {q:?}");
            }
        }
        assert!(read_kind(&photo, ProxyKind::Full, &settings).is_some());
        // ...but not current: replaced by format 3 at the next decode.
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
        write(&photo, &image, 300, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &image, &settings, NEW_EDIT, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), Some(300));
        assert!(has_full(&photo, &settings));
        // A format 2 file carried over by a relink keeps its version and
        // pixels; only its label changes.
        let other = photo.with_file_name("B.RAF");
        fs::write(&other, b"other raw").unwrap();
        let old = proxy_path(&photo).unwrap();
        fs::write(&old, format_2(&image, key.clone(), "Capture/A.RAF")).unwrap();
        let pixels = image_bytes(&old);
        fs::remove_file(proxy_path_of(&photo, ProxyKind::Full).unwrap()).unwrap();
        assert_eq!(carry_over(&photo, &other), 1);
        let moved = proxy_path(&other).unwrap();
        assert_eq!(fs::read(&moved).unwrap()[4..8], 2u32.to_le_bytes());
        assert_eq!(image_bytes(&moved), pixels);
        let header = read_header(&moved).unwrap();
        assert_eq!(
            header.source,
            Some(
                Path::new("Capture")
                    .join("B.RAF")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(header.transfer, None);
    }

    #[test]
    fn previews_of_the_square_root_transfer_are_read_and_replaced() {
        // Format 3 as #52 wrote it: `Half`, no knee, no file facts.
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let image = gradient(300, 280);
        write(&photo, &image, 300, &settings, NEW_EDIT, None).unwrap();
        let path = proxy_path(&photo).unwrap();
        let bytes = fs::read(&path).unwrap();
        let (mut header, _) = parse_header(&bytes).unwrap();
        let half = Transfer::Half { k: 0.1 };
        header.transfer = Some(half);
        header.facts = None;
        let json = serde_json::to_vec(&header).unwrap();
        let mut out = bytes[..8].to_vec();
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        let rgb = image.to_rgb32f();
        for (i, band) in rgb
            .as_raw()
            .chunks((300 * JXL_ROWS_PER_BAND * 3) as usize)
            .enumerate()
        {
            let rows = JXL_ROWS_PER_BAND.min(280 - i as u32 * JXL_ROWS_PER_BAND);
            let encoded = proxy_codec::encode(band, 300, 0.25, half).unwrap();
            out.extend_from_slice(&rows.to_le_bytes());
            out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            out.extend_from_slice(&encoded);
        }
        fs::write(&path, out).unwrap();
        let read = read(&photo, &settings).unwrap().image.to_rgb32f();
        for (p, q) in rgb.pixels().zip(read.pixels()) {
            let largest = p[0].max(p[1]).max(p[2]);
            for c in 0..3 {
                assert!((p[c] - q[c]).abs() <= largest * 0.04 + 1e-3, "{p:?} {q:?}");
            }
        }
        assert_eq!(stored_size(&photo, &settings), None);
        write(&photo, &image, 300, &settings, NEW_EDIT, None).unwrap();
        let header = read_header(&path).unwrap();
        assert_eq!(header.transfer, Some(proxy_codec::TRANSFER));
        assert_eq!(stored_size(&photo, &settings), Some(300));
    }

    #[test]
    fn stale_or_damaged_proxies_are_ignored() {
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::new(70, 70));
        let bytes = encode(&image, 700, 700, key());
        let mut other = key();
        other.modified_ns = 21;
        assert!(decode(&bytes, &other).is_none());
        assert!(decode(&bytes[..bytes.len() - 3], &key()).is_none());
        assert!(decode(b"RTPX", &key()).is_none());
    }

    #[test]
    fn proxies_live_in_the_session_and_follow_it() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        fs::create_dir_all(session.join("Capture")).unwrap();
        fs::write(session.join("session.json"), "{}").unwrap();
        fs::write(session.join("session.sqlite"), "").unwrap();
        let photo = session.join("Capture").join("A.RAF");
        let path = proxy_path(&photo).unwrap();
        assert!(path.starts_with(session.join(".cache").join("previews")));
        // The same relative path in a moved session gives the same name.
        let moved = dir.path().join("Moved").join("Capture").join("A.RAF");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        fs::write(dir.path().join("Moved").join("session.json"), "{}").unwrap();
        fs::write(dir.path().join("Moved").join("session.sqlite"), "").unwrap();
        assert_eq!(path.file_name(), proxy_path(&moved).unwrap().file_name());
        assert!(proxy_path(&dir.path().join("loose.RAF")).is_none());
        assert_eq!(proxy_size_for(1800), 2048);
        assert_eq!(proxy_size_for(2304), 2560);
        assert_eq!(proxy_size_for(9000), 3840);
    }

    /// A session with one fake RAW (`Capture/A.RAF`, bytes `content`).
    fn session_with_photo(dir: &Path, content: &[u8]) -> PathBuf {
        let session = dir.join("Job");
        fs::create_dir_all(session.join("Capture")).unwrap();
        fs::write(session.join("session.json"), "{}").unwrap();
        fs::write(session.join("session.sqlite"), "").unwrap();
        let photo = session.join("Capture").join("A.RAF");
        fs::write(&photo, content).unwrap();
        photo
    }

    fn gradient(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(width, height, |x, y| {
            image::Rgb([x as f32 / width as f32, y as f32 / height as f32, 0.25])
        }))
    }

    #[test]
    fn standard_and_full_proxies_are_separate_files() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let full = gradient(400, 300);
        write(&photo, &full, 200, &settings, NEW_EDIT, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), Some(200));
        assert!(!has_full(&photo, &settings));
        write_full(&photo, &full, &settings, NEW_EDIT, None).unwrap();
        assert!(has_full(&photo, &settings));
        let standard = proxy_path_of(&photo, ProxyKind::Standard).unwrap();
        let one_to_one = proxy_path_of(&photo, ProxyKind::Full).unwrap();
        assert_ne!(standard, one_to_one);
        assert_eq!(standard.parent(), one_to_one.parent());
        let read_full = read_kind(&photo, ProxyKind::Full, &settings).unwrap();
        assert_eq!(read_full.image.dimensions(), (400, 300));
        assert_eq!((read_full.full_width, read_full.full_height), (400, 300));
        assert_eq!(
            read(&photo, &settings).unwrap().image.dimensions(),
            (200, 150)
        );
    }

    /// The multi view's background work holds a 1:1 preview's encode back
    /// band by band while a zoom runs: the hook runs before every band and
    /// the file is the one written without it.
    #[test]
    fn a_full_preview_waits_band_by_band_and_is_written_alike() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let full = gradient(300, 2 * JXL_ROWS_PER_BAND + 10);
        write_full(&photo, &full, &settings, NEW_EDIT, None).unwrap();
        let path = proxy_path_of(&photo, ProxyKind::Full).unwrap();
        let plain = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let bands = std::sync::atomic::AtomicUsize::new(0);
        let before_band = || {
            bands.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        };
        write_full_with(&photo, &full, &settings, NEW_EDIT, None, &before_band).unwrap();
        assert_eq!(bands.into_inner(), 3);
        assert_eq!(fs::read(&path).unwrap(), plain);
    }

    /// A proxy stands only for a decode made for the colour noise filter of
    /// the photograph's stored recipe: the multi-scale filter's base has no
    /// decode-time chroma clean-up, the old filter's has.
    #[test]
    fn proxies_in_memory_serve_only_the_decode_variant_they_were_made_for() {
        // The key's variant fields read back as written.
        let settings = AppSettings::default();
        for multiscale in [false, true] {
            for dead_test in [DeadTest::SameColour, DeadTest::Adjacent] {
                for frame in [0, 1] {
                    let variant = DecodeVariant {
                        multiscale,
                        dead_test,
                        frame,
                    };
                    assert_eq!(variant_of_key(&decoder_key(&settings, variant)), variant);
                }
            }
        }
        // Version 0's frame keeps the keys proxies were made with.
        for variant in [NEW_EDIT, LEGACY] {
            assert!(!decoder_key(&settings, variant).contains("frame"));
        }
        // A proxy kept for a photograph whose recipe was reset since (render
        // version 2 → 3) is not handed out for the new variant.
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let path = photo.to_string_lossy().to_string();
        let old = DecodeVariant {
            multiscale: true,
            dead_test: DeadTest::SameColour,
            frame: 0,
        };
        let proxy = write(&photo, &gradient(80, 60), 40, &settings, old, None).unwrap();
        let mut memory = ProxyMemory::default();
        memory.remember(&path, proxy);
        assert!(memory.get(&path, old).is_some());
        assert!(memory.get(&path, NEW_EDIT).is_none());
    }

    #[test]
    fn proxies_follow_the_colour_noise_filter_of_the_stored_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let full = gradient(80, 60);
        // Never edited: a RAW develops with the multi-scale filter.
        write(&photo, &full, 40, &settings, LEGACY, None).unwrap();
        write_full(&photo, &full, &settings, LEGACY, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
        write(&photo, &full, 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &full, &settings, NEW_EDIT, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), Some(40));
        assert!(has_full(&photo, &settings));
        // An edit with the old filter makes them stale.
        let sidecar = crate::exif_processing::get_primary_sidecar_path(&photo);
        let recipe = serde_json::json!({
            "version": 1, "rating": 0,
            "adjustments": { "colorNoiseReduction": 25, "colorNoiseVersion": 1 },
        });
        fs::write(&sidecar, recipe.to_string()).unwrap();
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
        write(&photo, &full, 40, &settings, LEGACY, None).unwrap();
        assert_eq!(stored_size(&photo, &settings), Some(40));
    }

    #[test]
    fn rows_of_a_full_proxy_decode_only_their_bands() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        // Three bands of 256 rows and a short fourth.
        let full = gradient(64, 900);
        write_full(&photo, &full, &settings, NEW_EDIT, None).unwrap();
        let whole = read_kind(&photo, ProxyKind::Full, &settings)
            .unwrap()
            .image
            .to_rgb32f();
        let rows = read_full_rows(&photo, &settings, (300, 520), (0, 64)).unwrap();
        // The second and third bands only, bit-identical to the whole decode.
        assert_eq!((rows.width, rows.height, rows.row0), (64, 900, 256));
        assert_eq!((rows.col0, rows.cols), (0, 64));
        assert_eq!(rows.pixels.len(), 64 * 512 * 3);
        let stride = 64 * 3;
        assert_eq!(
            rows.pixels.as_slice(),
            &whole.as_raw()[256 * stride..768 * stride]
        );
        // Columns 10..30 of them.
        let part = read_full_rows(&photo, &settings, (300, 520), (10, 30)).unwrap();
        assert_eq!((part.row0, part.col0, part.cols), (256, 10, 20));
        let expected: Vec<f32> = (256..768)
            .flat_map(|y| whole.as_raw()[y * stride + 30..y * stride + 90].to_vec())
            .collect();
        assert_eq!(part.pixels, expected);
        assert_eq!(full_size(&photo, &settings), Some((64, 900)));
        let last = read_full_rows(&photo, &settings, (899, 900), (0, 64)).unwrap();
        assert_eq!((last.row0, last.pixels.len()), (768, 132 * stride));
        assert!(read_full_rows(&photo, &settings, (10, 901), (0, 64)).is_none());
        assert!(read_full_rows(&photo, &settings, (10, 10), (0, 64)).is_none());
        assert!(read_full_rows(&photo, &settings, (10, 20), (5, 65)).is_none());
        // A Standard proxy has no full-resolution rows.
        write(&photo, &full, 300, &settings, NEW_EDIT, None).unwrap();
        discard_source(&photo, &[ProxyKind::Full]);
        assert!(read_full_rows(&photo, &settings, (0, 10), (0, 64)).is_none());
        assert_eq!(full_size(&photo, &settings), None);
    }

    #[test]
    fn a_copy_with_a_new_date_keeps_its_proxies_but_new_content_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), &vec![7u8; 600 * 1024]);
        let settings = AppSettings::default();
        write(&photo, &gradient(64, 48), 64, &settings, NEW_EDIT, None).unwrap();
        // Copied without its dates (cp without -p): same bytes, newer time.
        let later = SystemTime::now() + Duration::from_secs(3600);
        fs::File::options()
            .append(true)
            .open(&photo)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(read(&photo, &settings).is_some());
        assert_eq!(stored_size(&photo, &settings), Some(64));
        // Rewritten with other content of the same size: stale.
        let mut other = vec![7u8; 600 * 1024];
        other[10] = 8;
        fs::write(&photo, &other).unwrap();
        assert!(read(&photo, &settings).is_none());
        assert!(stored_size(&photo, &settings).is_none());
    }

    /// A proxy file whose header lost its `facts` and `exif` (as previews
    /// made before they were kept).
    fn without_facts(path: &Path) {
        edit_header(path, |header| {
            header.facts = None;
            header.exif = None;
        });
    }

    /// Rewrites the header of the proxy file at `path` with `edit`.
    fn edit_header(path: &Path, edit: impl FnOnce(&mut Header)) {
        let bytes = fs::read(path).unwrap();
        let (mut header, at) = parse_header(&bytes).unwrap();
        edit(&mut header);
        let json = serde_json::to_vec(&header).unwrap();
        let mut out = bytes[..8].to_vec();
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        out.extend_from_slice(&bytes[at..]);
        fs::write(path, out).unwrap();
    }

    #[test]
    fn previews_with_facts_of_an_older_version_are_remade() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        assert!(stored_size(&photo, &settings).is_some());
        assert!(has_full(&photo, &settings));
        // Facts of an older version lack what newer readers use (version 3:
        // diffraction_sigma from the newer database; 4: lensfun defaults at
        // the body's crop factor with their whole lateral CA).
        for kind in [ProxyKind::Standard, ProxyKind::Full] {
            let path = proxy_path_of(&photo, kind).unwrap();
            let bytes = fs::read(&path).unwrap();
            let (mut header, at) = parse_header(&bytes).unwrap();
            header.facts.as_mut().unwrap().version = source_facts::VERSION - 1;
            let json = serde_json::to_vec(&header).unwrap();
            let mut out = bytes[..8].to_vec();
            out.extend_from_slice(&(json.len() as u32).to_le_bytes());
            out.extend_from_slice(&json);
            out.extend_from_slice(&bytes[at..]);
            fs::write(&path, out).unwrap();
        }
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
    }

    #[test]
    fn offline_photographs_render_with_the_file_facts_their_previews_keep() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        let key = photo.to_string_lossy().into_owned();
        let header = read_header(&proxy_path(&photo).unwrap()).unwrap();
        let facts = header.facts.clone().expect("facts kept with the preview");
        assert_eq!(facts.version, source_facts::VERSION);
        assert!(facts.base_look.is_some());
        // Online: nothing registered, the readers use the file.
        assert_eq!(prepare_offline(&photo), None);
        assert!(source_facts::registered(&key).is_none());
        read(&photo, &settings).unwrap();
        assert!(source_facts::registered(&key).is_none());
        // Offline: opening registers what the preview keeps.
        let away = dir.path().join("away.RAF");
        fs::rename(&photo, &away).unwrap();
        prepare_offline(&photo);
        assert_eq!(source_facts::registered(&key).as_deref(), Some(&facts));
        source_facts::forget(&key);
        read_kind(&photo, ProxyKind::Full, &settings).unwrap();
        assert_eq!(source_facts::registered(&key).as_deref(), Some(&facts));
        // Back online: the file again.
        fs::rename(&away, &photo).unwrap();
        prepare_offline(&photo);
        assert!(source_facts::registered(&key).is_none());

        // Previews made before facts were kept still open offline, without
        // them, and are not current (the next decode or build replaces them).
        for kind in [ProxyKind::Standard, ProxyKind::Full] {
            without_facts(&proxy_path_of(&photo, kind).unwrap());
        }
        assert_eq!(stored_size(&photo, &settings), None);
        assert!(!has_full(&photo, &settings));
        fs::rename(&photo, &away).unwrap();
        assert_eq!(prepare_offline(&photo), None);
        assert!(read(&photo, &settings).is_some());
        assert!(read_kind(&photo, ProxyKind::Full, &settings).is_some());
        assert!(source_facts::registered(&key).is_none());
    }

    #[test]
    fn an_offline_photograph_opens_on_its_proxies() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        assert!(source_available(&photo));
        // The volume went away (here: the file was moved out).
        fs::rename(&photo, dir.path().join("elsewhere.RAF")).unwrap();
        assert!(!source_available(&photo));
        assert_eq!(read(&photo, &settings).unwrap().full_width, 80);
        assert!(has_full(&photo, &settings));
        // Made with other decoder settings: not the photograph as it is shown.
        let other = AppSettings {
            raw_highlight_compression: Some(1.0),
            ..AppSettings::default()
        };
        assert!(read(&photo, &other).is_none());
        // Previews made for another decode variant (before the never-edited
        // default moved on, or before a recipe reset) still open offline:
        // no decode can replace them.
        fs::rename(dir.path().join("elsewhere.RAF"), &photo).unwrap();
        let before_upgrade = DecodeVariant {
            multiscale: true,
            dead_test: DeadTest::SameColour,
            frame: 0,
        };
        write(
            &photo,
            &gradient(80, 60),
            40,
            &settings,
            before_upgrade,
            None,
        )
        .unwrap();
        write_full(&photo, &gradient(80, 60), &settings, before_upgrade, None).unwrap();
        assert!(read(&photo, &settings).is_none(), "online: decoded anew");
        fs::rename(&photo, dir.path().join("elsewhere.RAF")).unwrap();
        assert_eq!(read(&photo, &settings).unwrap().full_width, 80);
        assert!(has_full(&photo, &settings));
        assert!(read(&photo, &other).is_none());
        // The same with the file there but unreadable (its metadata known;
        // root reads it anyway, so the check only runs where it is not).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::rename(dir.path().join("elsewhere.RAF"), &photo).unwrap();
            fs::set_permissions(&photo, fs::Permissions::from_mode(0o000)).unwrap();
            if !source_available(&photo) {
                assert_eq!(read(&photo, &settings).unwrap().full_width, 80);
                assert!(has_full(&photo, &settings));
                assert!(read(&photo, &other).is_none());
            }
            fs::set_permissions(&photo, fs::Permissions::from_mode(0o644)).unwrap();
            fs::rename(&photo, dir.path().join("elsewhere.RAF")).unwrap();
        }
        // Nothing can be written for it.
        assert!(write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).is_err());
    }

    /// Offline, previews of any decode variant open, but never those of
    /// another frame of the RAW than the stored recipe's (ADR 0020): its
    /// coordinates belong to its own frame. A never-edited photograph takes
    /// the previews' frame. Every consumer reads through this check (the
    /// editor's proxy, multi view, thumbnails, the offline open).
    #[test]
    fn offline_previews_of_another_frame_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let first = session_with_photo(dir.path(), b"raw bytes");
        let photo = first.with_file_name("A.CR3");
        fs::rename(&first, &photo).unwrap();
        let settings = AppSettings::default();
        let old_frame = NEW_EDIT;
        let new_frame = DecodeVariant {
            frame: 1,
            ..NEW_EDIT
        };
        let sidecar = photo.with_file_name("A.CR3.rrdata");
        let save = |adjustments: serde_json::Value| {
            let metadata =
                serde_json::json!({ "version": 1, "rating": 0, "adjustments": adjustments });
            fs::write(&sidecar, metadata.to_string()).unwrap();
        };
        let away = dir.path().join("elsewhere.CR3");
        let previews_of = |variant: DecodeVariant| {
            fs::rename(&away, &photo).ok();
            write(&photo, &gradient(80, 60), 40, &settings, variant, None).unwrap();
            write_full(&photo, &gradient(80, 60), &settings, variant, None).unwrap();
            fs::rename(&photo, &away).unwrap();
            assert!(!source_available(&photo));
        };
        let readable = || {
            [
                read(&photo, &settings).is_some(),
                read_kind(&photo, ProxyKind::Full, &settings).is_some(),
                read_full_rows(&photo, &settings, (0, 4), (0, 4)).is_some(),
                has_full(&photo, &settings),
                full_key(&photo, &settings).is_some(),
            ]
        };
        // An edit of frame 1 on previews of frame 0: none is read.
        save(serde_json::json!({"exposure": 1.0, "renderVersion": 4, "sourceFrameVersion": 1}));
        previews_of(old_frame);
        assert_eq!(readable(), [false; 5]);
        // Its own frame's previews are.
        previews_of(new_frame);
        assert_eq!(readable(), [true; 5]);
        // An edit of frame 0 on previews of frame 1: not read either.
        save(serde_json::json!({"exposure": 1.0, "renderVersion": 4}));
        assert_eq!(readable(), [false; 5]);
        // A never-edited photograph opens on either; a Reset's recipe of
        // frame 1 (its key only) on its own frame only.
        save(serde_json::json!({}));
        assert_eq!(readable(), [true; 5]);
        save(serde_json::json!({"sourceFrameVersion": 1}));
        assert_eq!(readable(), [true; 5]);
        previews_of(old_frame);
        assert_eq!(readable(), [false; 5]);
        save(serde_json::json!({}));
        assert_eq!(readable(), [true; 5]);
        // A new edit of a file whose newest frame is version 0's (an
        // unchanged CR3): its frame-0 previews open cold, nothing registered
        // yet, from the frames their own facts keep; not where those say
        // the file's frame changed.
        save(serde_json::json!({"exposure": 1.0, "renderVersion": 4, "sourceFrameVersion": 1}));
        for (frames, shown) in [(vec![0, 0], true), (vec![0, 1], false)] {
            for kind in [ProxyKind::Standard, ProxyKind::Full] {
                edit_header(&proxy_path_of(&photo, kind).unwrap(), |header| {
                    header.facts.as_mut().unwrap().frames = Some(frames.clone());
                });
            }
            source_facts::forget(&photo.to_string_lossy());
            assert_eq!(readable(), [shown; 5], "{frames:?}");
        }
    }

    /// Offline and cold (nothing registered), a virtual copy of an edit from
    /// before the key stands for its master's previews of a new edit where
    /// the previews' facts say both frame versions develop the file alike.
    #[test]
    fn an_offline_copy_resolves_frames_from_the_previews() {
        use frame_fixture::{frame_one_edit, offline_cr3};
        let legacy = serde_json::json!({"exposure": 0.5, "renderVersion": 4});
        for (frames, shared) in [(vec![0, 0], true), (vec![0, 1], false)] {
            let dir = tempfile::tempdir().unwrap();
            let photo = offline_cr3(dir.path(), frame_one_edit(), frames[1]);
            for kind in [ProxyKind::Standard, ProxyKind::Full] {
                edit_header(&proxy_path_of(&photo, kind).unwrap(), |header| {
                    header.facts.as_mut().unwrap().frames = Some(frames.clone());
                });
            }
            source_facts::forget(&photo.to_string_lossy());
            assert_eq!(stands_for(&photo, &legacy), shared, "{frames:?}");
            assert!(stands_for(&photo, &frame_one_edit()));
            source_facts::forget(&photo.to_string_lossy());
        }
    }

    #[test]
    fn a_relinked_photograph_keeps_its_previews() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        let moved = photo.with_file_name("Renamed.RAF");
        fs::rename(&photo, &moved).unwrap();
        assert!(read(&moved, &settings).is_none());
        assert_eq!(carry_over(&photo, &moved), 2);
        assert_eq!(read(&moved, &settings).unwrap().full_width, 80);
        assert!(has_full(&moved, &settings));
        assert!(read(&photo, &settings).is_none());
        // Never over previews already there.
        let other = photo.with_file_name("Other.RAF");
        fs::write(&other, b"other raw").unwrap();
        write(&other, &gradient(8, 6), 4, &settings, NEW_EDIT, None).unwrap();
        assert_eq!(
            carry_over(&moved, &other),
            1,
            "only the 1:1 preview is free"
        );
        assert_eq!(read(&other, &settings).unwrap().full_width, 8);
        assert!(read(&moved, &settings).is_some(), "kept where it was");
    }

    /// A preview keeps the measures of the decode it was made from, not the
    /// last full decode of the file (another recipe's decode may be newer).
    #[test]
    fn previews_keep_the_measures_of_their_own_decode() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        let path = photo.to_string_lossy().to_string();
        rawtonic_rawdecode::noise_level::record(&path, 0.03);
        rawtonic_rawdecode::image_key::record(&path, [0.2, 0.7, 0.4]);
        let own = DecodeMeasures {
            noise_level: Some(0.01),
            image_key: Some([0.1, 0.6, 0.3]),
        };
        write(
            &photo,
            &gradient(80, 60),
            40,
            &settings,
            NEW_EDIT,
            Some(own),
        )
        .unwrap();
        let facts = read_header(&proxy_path(&photo).unwrap())
            .unwrap()
            .facts
            .unwrap();
        assert_eq!(
            (facts.noise_level, facts.image_key, facts.image_key_median),
            (Some(0.01), Some([0.1, 0.6]), Some(0.3))
        );
        // And renders from it read them back (not the newer decode's).
        assert_eq!(read(&photo, &settings).unwrap().measures, Some(own));
        let mut memory = ProxyMemory::default();
        let kept = memory.remember(&path, read(&photo, &settings).unwrap());
        assert_eq!(kept.measures, Some(own));
    }

    /// The image part of a proxy file (everything after its header).
    fn image_bytes(path: &Path) -> Vec<u8> {
        let bytes = fs::read(path).unwrap();
        let (_, at) = parse_header(&bytes).unwrap();
        bytes[at..].to_vec()
    }

    #[test]
    fn carried_over_previews_name_their_new_photograph_and_can_be_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        let standard = proxy_path(&photo).unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(7200);
        mark_used_at(&standard, long_ago);
        let (pixels, full_pixels) = (
            image_bytes(&standard),
            image_bytes(&proxy_path_of(&photo, ProxyKind::Full).unwrap()),
        );
        let moved = photo.with_file_name("Renamed.RAF");
        fs::rename(&photo, &moved).unwrap();
        assert_eq!(carry_over(&photo, &moved), 2);
        for (kind, before) in [
            (ProxyKind::Standard, pixels),
            (ProxyKind::Full, full_pixels),
        ] {
            let path = proxy_path_of(&moved, kind).unwrap();
            let header = read_header(&path).unwrap();
            assert_eq!(
                header.source,
                Some(
                    Path::new("Capture")
                        .join("Renamed.RAF")
                        .to_string_lossy()
                        .into_owned()
                )
            );
            assert!(labels_file(header.source.as_deref().unwrap(), &path));
            // Same pixels, not re-encoded.
            assert_eq!(image_bytes(&path), before);
        }
        // The last use is kept (relinking is not a use).
        let new_standard = proxy_path(&moved).unwrap();
        let used = fs::metadata(&new_standard).unwrap().modified().unwrap();
        assert!(used < long_ago + Duration::from_secs(2), "{used:?}");
        assert!(has_full(&moved, &settings));
        // The relinked photograph is online: its Standard preview may go.
        let previews = new_standard.parent().unwrap().to_path_buf();
        assert_eq!(enforce_standard_limit(&previews, 1), 1);
        assert!(!new_standard.exists());
    }

    #[test]
    fn previews_whose_label_does_not_name_them_are_not_kept_as_offline() {
        // Carried over before carry_over relabelled: the header still names
        // the photograph's old path, which is gone.
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        let moved = photo.with_file_name("Renamed.RAF");
        fs::rename(&photo, &moved).unwrap();
        fs::rename(proxy_path(&photo).unwrap(), proxy_path(&moved).unwrap()).unwrap();
        let header = read_header(&proxy_path(&moved).unwrap()).unwrap();
        assert_eq!(
            header.source,
            Some(
                Path::new("Capture")
                    .join("A.RAF")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        // A photograph really offline keeps its preview.
        let offline = moved.with_file_name("Offline.RAF");
        fs::write(&offline, b"offline raw").unwrap();
        write(&offline, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        fs::remove_file(&offline).unwrap();
        let previews = previews_dir_for_folder(moved.parent().unwrap()).unwrap();
        assert_eq!(enforce_standard_limit(&previews, 1), 1);
        assert!(!proxy_path(&moved).unwrap().exists());
        assert!(read(&offline, &settings).is_some());
    }

    #[test]
    fn usage_and_discarding_by_kind_and_last_use() {
        let dir = tempfile::tempdir().unwrap();
        let photo = session_with_photo(dir.path(), b"raw bytes");
        let settings = AppSettings::default();
        write(&photo, &gradient(80, 60), 40, &settings, NEW_EDIT, None).unwrap();
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        let previews = previews_dir_for_folder(photo.parent().unwrap()).unwrap();
        assert_eq!(
            Some(previews.clone()),
            proxy_path(&photo).map(|p| p.parent().unwrap().to_path_buf())
        );
        // The session folder itself maps to the same previews folder.
        assert_eq!(
            previews_dir_for_folder(&dir.path().join("Job")),
            Some(previews.clone())
        );
        let used = usage(&previews);
        assert_eq!((used.standard_count, used.full_count), (1, 1));
        assert!(used.standard_bytes > 0 && used.full_bytes > used.standard_bytes);
        // A 1:1 preview used recently stays; one unused for long goes.
        assert_eq!(
            discard(
                &previews,
                &[ProxyKind::Full],
                Some(Duration::from_secs(3600))
            ),
            0
        );
        let full = proxy_path_of(&photo, ProxyKind::Full).unwrap();
        fs::File::options()
            .append(true)
            .open(&full)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        assert_eq!(
            discard(
                &previews,
                &[ProxyKind::Full],
                Some(Duration::from_secs(3600))
            ),
            1
        );
        assert!(!has_full(&photo, &settings));
        assert!(read(&photo, &settings).is_some());
        // Reading a 1:1 preview counts as a use.
        write_full(&photo, &gradient(80, 60), &settings, NEW_EDIT, None).unwrap();
        fs::File::options()
            .append(true)
            .open(&full)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        read_kind(&photo, ProxyKind::Full, &settings).unwrap();
        assert_eq!(
            discard(
                &previews,
                &[ProxyKind::Full],
                Some(Duration::from_secs(3600))
            ),
            0
        );
        assert_eq!(
            discard_source(&photo, &[ProxyKind::Standard, ProxyKind::Full]),
            2
        );
        assert_eq!(usage(&previews), PreviewUsage::default());
    }

    #[test]
    fn the_standard_limit_discards_the_least_recently_used_online_previews() {
        let dir = tempfile::tempdir().unwrap();
        let session = session_with_photo(dir.path(), b"raw a")
            .parent()
            .unwrap()
            .to_path_buf();
        let settings = AppSettings::default();
        let photos: Vec<PathBuf> = ["A.RAF", "B.RAF", "C.RAF", "D.RAF"]
            .iter()
            .map(|name| {
                let photo = session.join(name);
                fs::write(&photo, name.as_bytes()).unwrap();
                write(&photo, &gradient(64, 48), 64, &settings, NEW_EDIT, None).unwrap();
                photo
            })
            .collect();
        let previews = previews_dir_for_folder(&session).unwrap();
        // Last used: A long ago (offline now), B, C, D most recently.
        for (i, photo) in photos.iter().enumerate() {
            fs::File::options()
                .append(true)
                .open(proxy_path(photo).unwrap())
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(3600 * (10 - i as u64)))
                .unwrap();
        }
        fs::rename(&photos[0], dir.path().join("away.RAF")).unwrap();
        let each = usage(&previews).standard_bytes / 4;
        // Within the limit: nothing goes.
        assert_eq!(enforce_standard_limit(&previews, each * 4), 0);
        // Room for about two: the offline A stays, B and C (oldest online) go.
        assert_eq!(enforce_standard_limit(&previews, each * 2 + each / 2), 2);
        assert!(read(&photos[0], &settings).is_some());
        assert!(stored_size(&photos[1], &settings).is_none());
        assert!(stored_size(&photos[2], &settings).is_none());
        assert_eq!(stored_size(&photos[3], &settings), Some(64));
        // Reading counts as a use.
        read(&photos[3], &settings).unwrap();
        let used = fs::metadata(proxy_path(&photos[3]).unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert!(SystemTime::now().duration_since(used).unwrap() < Duration::from_secs(60));
        assert_eq!(standard_limit(&settings), Some(10_000_000_000));
        let unlimited = AppSettings {
            standard_previews_limit_gb: Some(0),
            ..AppSettings::default()
        };
        assert_eq!(standard_limit(&unlimited), None);
    }

    #[test]
    fn photographs_outside_sessions_use_the_given_folder() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("cache").join("previews");
        let photo = dir.path().join("Pictures").join("B.NEF");
        let (folder, name) = location_with(&photo, Some(&outside)).unwrap();
        assert_eq!(folder, outside);
        let (_, other) =
            location_with(&dir.path().join("Other").join("B.NEF"), Some(&outside)).unwrap();
        assert_ne!(name, other);
        assert!(location_with(&photo, None).is_none());
    }
}
