use rawtonic_runtime::cli_eprintln;

// The app's allocator, so exports free memory as the app does (the export
// crate returns freed pages through it, `allocator::release_freed_memory`).
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rawtonic_catalog::{Library, Mode};
use rawtonic_recipe::geometry;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

mod export_library;
mod fingerprint;
mod search_bench;

#[derive(Parser)]
#[command(
    name = "rawtonic",
    version,
    about = "Rawtonic portable session and verified ingest tools"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create a session (default) or catalog bundle without touching originals.
    Create {
        directory: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        catalog: bool,
    },
    /// Inspect a session/catalog, or fingerprint a source file read-only.
    Inspect { path: PathBuf },
    /// List assets, optionally those whose searchable text holds every
    /// word of `--search` (the Library's search box).
    List {
        library: PathBuf,
        #[arg(long)]
        search: Option<String>,
    },
    /// Paths of the photographs a search rule matches (JSON, as smart
    /// albums store it: `{"field":"iso","op":"ge","value":3200}`, groups
    /// `{"combine":"all"|"any"|"none","rules":[…]}`). Reads the library only.
    Search { library: PathBuf, rule: String },
    /// Read the capture facts (camera, lens, exposure, capture time) of
    /// every photograph not read yet, for the search. Reads the photographs'
    /// headers only.
    Facts { library: PathBuf },
    /// Time the library search on a synthetic library of `--photos`
    /// photographs (tiny files in a temporary folder, removed afterwards).
    SearchBench {
        #[arg(long, default_value_t = 50_000)]
        photos: u32,
        #[arg(long, default_value_t = 20)]
        iterations: u32,
    },
    /// Copy and SHA-256 verify files into Capture. Originals remain untouched.
    Ingest {
        library: PathBuf,
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        #[arg(long, default_value = "{orig}")]
        template: String,
        #[arg(long)]
        backup: Option<PathBuf>,
    },
    /// Show the names an ingest would give files (read-only): the template
    /// filled from each file's EXIF and card sidecar, where its capture
    /// date came from and whether a sidecar would be copied along.
    Name {
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        #[arg(long, default_value = "{orig}")]
        template: String,
        /// The value of `{job}` and `{session}`.
        #[arg(long, default_value = "Project")]
        job: String,
        #[arg(long, default_value_t = 1)]
        sequence_start: u64,
    },
    /// Rate an asset by UUID; queues XMP synchronization.
    Rate {
        library: PathBuf,
        asset: String,
        #[arg(value_parser=clap::value_parser!(u8).range(0..=i64::from(rawtonic_metadata::culling::Rating::MAX)))]
        rating: u8,
    },
    /// Set pick (1), reject (-1), or clear (0).
    Flag {
        library: PathBuf,
        asset: String,
        #[arg(allow_hyphen_values=true,value_parser=clap::value_parser!(i8).range(-1..=1))]
        flag: i8,
    },
    /// Register files where they are (catalogs; session files must be inside
    /// the session), each with its sidecar edits or XMP metadata, all in one
    /// transaction and one import batch. Reads only.
    Add {
        library: PathBuf,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Check every registered file and list the missing ones (with the
    /// disconnected drive they are on). Missing photos stay in the library.
    Missing { library: PathBuf },
    /// Relink a missing photo (asset UUID) to a file found elsewhere; the
    /// file must have the registered size and content. With --nearby the
    /// other missing photos of its old folder are looked for beside it.
    /// Files are only read.
    Relink {
        library: PathBuf,
        asset: String,
        to: PathBuf,
        #[arg(long)]
        nearby: bool,
    },
    /// Relink the missing photos registered in or below FROM to the same
    /// places in or below TO (a moved or renamed folder). Files are only read.
    RelinkFolder {
        library: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    /// Find photographs moved, renamed, added or deleted outside Rawtonic in
    /// FOLDER (default: the whole library) and print what was found. With
    /// --apply, verified moves and new files are applied (each checked
    /// again first); probable moves, copies and removals only with their
    /// flags, contested files never (use the app). Files are only read; a
    /// removal removes the library entry, never a file.
    SyncFolder {
        library: PathBuf,
        folder: Option<PathBuf>,
        #[arg(long)]
        apply: bool,
        /// Also apply moves matched by size and fingerprint only.
        #[arg(long)]
        include_probable: bool,
        /// Also add copies of photographs in the library.
        #[arg(long)]
        include_copies: bool,
        /// Also remove photographs whose file is gone from the library.
        #[arg(long)]
        remove_missing: bool,
    },
    /// Record the content hash (SHA-256) of every photograph that has none,
    /// so Synchronize Folder can verify moves. Reads the photographs only.
    Checksums { library: PathBuf },
    /// Find Duplicates: read the image data hash of every photograph that
    /// has none for its file as it is now, then list the groups of
    /// photographs with identical image data, with the time each step took.
    /// Reads the photographs only; nothing is moved or removed.
    Duplicates {
        library: PathBuf,
        /// Files read at the same time.
        #[arg(long, default_value_t = 4)]
        workers: usize,
    },
    /// Explicitly replace stored metadata with an external XMP sidecar. Keeps our recipe.
    AdoptXmp { library: PathBuf, asset: String },
    /// Sync sidecars, reporting conflicts without replacing external edits.
    Sync { library: PathBuf },
    /// Measure SQLite metadata writes on this machine. No fabricated GPU numbers.
    Bench {
        #[arg(long, default_value_t = 200)]
        iterations: u32,
    },
    /// Decode a RAW file to linear RGB without the desktop app and time it.
    /// Reads the file once; the original is opened read-only.
    Decode {
        path: PathBuf,
        #[arg(long, default_value_t = 3)]
        repeats: u32,
        /// Half-size preview demosaic instead of the full-resolution one.
        #[arg(long)]
        fast: bool,
    },
    /// Export photos at full resolution without the desktop app, as the
    /// app's export renders them with default preferences. Never writes over
    /// a file other than its own previous export (--on-conflict).
    Export(ExportArgs),
    /// Make a proof sheet (contact sheet) of photos as the app's Proof
    /// Sheet does: each photo rendered with its recipe at its cell's size,
    /// laid out in a PDF and/or one JPEG per page. Never overwrites.
    ProofSheet(ProofSheetArgs),
    /// Merge photos into a linear DNG that develops like a RAW. Never
    /// overwrites.
    Merge(MergeArgs),
    /// Time the AI models on the CPU and on a GPU provider and compare
    /// their outputs (mask IoU, largest difference). Needs ONNX Runtime
    /// 1.24.4 or later in `ORT_DYLIB_PATH` and the provider's plugin library
    /// beside it, in --plugins or the model folder. Reads only the models.
    AiBench(AiBenchArgs),
    /// Record what each RAW file decodes to (camera, levels, crop, colour,
    /// lens, raw data and development hashes), keyed by file content, or
    /// compare with a baseline (--check) after a decoder change. Reads only.
    Fingerprint(fingerprint::FingerprintArgs),
}
#[derive(clap::Args)]
struct AiBenchArgs {
    /// The model pack folder (default: the app's model folder).
    #[arg(long)]
    models: Option<PathBuf>,
    /// webgpu or cuda (cpu: the CPU only).
    #[arg(long, default_value = "webgpu")]
    provider: String,
    /// Timed runs after the first.
    #[arg(long, default_value_t = 3)]
    runs: usize,
    /// A photo (JPEG, PNG, TIFF) as the input; default a synthetic picture.
    #[arg(long)]
    image: Option<PathBuf>,
    /// Only files starting with these names (comma-separated), e.g. birefnet,sam2.
    #[arg(long)]
    only: Option<String>,
    /// A folder with the plugin library.
    #[arg(long)]
    plugins: Option<PathBuf>,
    /// Also the 1.1 GB SigLIP 2 text encoder.
    #[arg(long)]
    with_text: bool,
}
#[derive(clap::Args)]
struct MergeArgs {
    /// hdr, pano, hdrpano, focus, or single (one photo through the merge path, a round-trip
    /// check).
    kind: String,
    /// HDR ghost removal: off, low, medium, high.
    #[arg(long, default_value = "off")]
    deghost: String,
    /// HDR: merge without aligning.
    #[arg(long)]
    no_align: bool,
    /// Panorama projection: auto, spherical, cylindrical, perspective, panini.
    #[arg(long, default_value = "auto")]
    projection: String,
    /// Panorama boundary warp, 0–100.
    #[arg(long, default_value_t = 0.0)]
    boundary_warp: f32,
    /// Panorama: keep the irregular outline (no auto crop).
    #[arg(long)]
    no_crop: bool,
    /// Automatic tone settings in the merged DNG's starting recipe.
    #[arg(long)]
    auto_settings: bool,
    #[arg(required = true)]
    sources: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}
#[derive(clap::Args)]
struct ProofSheetArgs {
    /// Photos in the sheet's order; a folder adds its photos and their
    /// virtual copies in the library order, as `export` does.
    #[arg(required = true)]
    sources: Vec<PathBuf>,
    /// The folder the sheet is written into.
    #[arg(long)]
    output: PathBuf,
    /// The layout as JSON, as the app stores it (`ProofSheet`: page size and
    /// margins in mm, columns, rows, caption template, header, footer, ppi,
    /// pdf, jpeg).
    #[arg(long)]
    sheet: PathBuf,
    /// The file name; `{job}` and `{date}` are filled.
    #[arg(long, default_value = "{job} proof sheet")]
    name: String,
    /// The job name `{job}` fills.
    #[arg(long, default_value = "")]
    job: String,
    /// The date `{date}` fills (default: today, yyyy-mm-dd).
    #[arg(long)]
    date: Option<String>,
    /// Output sharpening as AMOUNT:RADIUS[:THRESHOLD] for the photos.
    #[arg(long, value_parser = parse_output_sharpening)]
    output_sharpening: Option<rawtonic_export::output_sharpening::OutputSharpening>,
    #[arg(long)]
    lensfun_db: Option<PathBuf>,
    #[arg(long)]
    models: Option<PathBuf>,
}

#[derive(clap::Args)]
struct ExportArgs {
    /// Select the library managing these photos instead of automatic discovery.
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// A photo, or a folder exported with its subfolders.
    source: PathBuf,
    /// For a photo the output file (its extension chooses the format); for a
    /// folder the output folder.
    #[arg(long)]
    output: PathBuf,
    /// If a file of an output's name exists: keep-both (a numbered name;
    /// for a single output file: fail), skip (decided before rendering), or
    /// replace-if-changed (only a previous export of the same photo recorded
    /// in --export-records and unchanged since, and only when its recipe,
    /// metadata or settings changed; unchanged it is skipped; any other
    /// file is kept as keep-both does).
    #[arg(long, default_value = "keep-both", value_parser = rawtonic_export::existing_files::ExistingFiles::parse)]
    on_conflict: rawtonic_export::existing_files::ExistingFiles,
    /// The record of the files exports wrote (an SQLite file; created when
    /// missing): every written file is recorded there. Needed by
    /// --on-conflict replace-if-changed.
    #[arg(long, value_name = "FILE")]
    export_records: Option<PathBuf>,
    /// File format and extension for folder exports (jpeg, jpg, png, tiff, tif, webp, jxl, avif).
    #[arg(long, default_value = "jpeg")]
    format: String,
    /// The editor's adjustments JSON (as for the app's `export --adjustments`).
    #[arg(long)]
    adjustments: Option<PathBuf>,
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
    /// JPEG only: limit the whole file (metadata and ICC profile included)
    /// to this many kilobytes (1000 bytes). The quality is then searched for
    /// (a high one that fits; --quality is not used); a limit even the lowest quality
    /// cannot meet writes the smallest file and says so.
    #[arg(long, value_name = "KB", value_parser = clap::value_parser!(u32).range(1..))]
    max_file_kb: Option<u32>,
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u8).range(8..=16))]
    tiff_bit_depth: u8,
    /// Copy all source EXIF and session metadata. Creator, copyright
    /// and usage terms are retained even without this flag; contacts require
    /// --keep-creator-contact when other metadata is stripped.
    #[arg(long)]
    keep_metadata: bool,
    /// With --keep-metadata: leave out GPS positions.
    #[arg(long)]
    strip_gps: bool,
    /// With other metadata stripped, explicitly include creator contact details.
    #[arg(long)]
    keep_creator_contact: bool,
    /// Output colour space: srgb, display-p3, adobe-rgb or prophoto (the
    /// file carries its ICC profile or JPEG XL colour encoding).
    #[arg(long, default_value = "srgb", value_parser = parse_color_space)]
    color_space: rawtonic_engine::output_color_space::OutputColorSpace,
    /// The app's Lensfun folder; default: $RAWTONIC_LENSFUN_DB, then
    /// `lensfun_db` beside this binary or in a macOS app bundle's Resources.
    #[arg(long)]
    lensfun_db: Option<PathBuf>,
    /// The app's AI model folder, including models for AI Denoise, Super
    /// Resolution and Sky and Subject selections; default:
    /// $RAWTONIC_MODELS, then the installed app's (cc.rawtonic.editor).
    /// Denoised mosaics are cached beside it in `ai-denoise-cache`.
    #[arg(long)]
    models: Option<PathBuf>,
    /// Output sharpening as AMOUNT:RADIUS[:THRESHOLD] (percent, output
    /// pixels, levels of 255), after any resize.
    #[arg(long, value_parser = parse_output_sharpening)]
    output_sharpening: Option<rawtonic_export::output_sharpening::OutputSharpening>,
    /// Super Resolution: enlarge each photo 2× with the AI model in --models
    /// before any resize and output sharpening, as the app's export option.
    #[arg(long)]
    super_resolution: bool,
    /// Enable Super Resolution at this quality: `full` averages each tile's four
    /// rotations, `fast` enlarges it once (a quarter of the work), as the
    /// app's export recipe.
    #[arg(long, value_enum)]
    super_resolution_quality: Option<SuperResolutionQualityArg>,
    /// Sign a provenance manifest (C2PA) into each JPEG, PNG, TIFF, WebP, AVIF
    /// or JPEG XL with the signing identity in DIR, as the app keeps it (its data folder's
    /// `provenance/`, holding `signing-identity.json`).
    #[arg(long, value_name = "DIR")]
    provenance_identity: Option<PathBuf>,
    /// With --provenance-identity: name the photo's creator as author.
    #[arg(long, requires = "provenance_identity")]
    provenance_author: bool,
    /// Face retouch for every photo: the sliders for all faces as JSON
    /// (`{"blemishes":60,"evenSkin":50}`, see docs/RECIPE_SCHEMA.md). The
    /// faces are found with the face models in --models (ONNX Runtime from
    /// `ORT_DYLIB_PATH`) and replace the recipe's own face retouch.
    #[arg(long)]
    face_retouch: Option<String>,
    /// QA: also write every input the export's GPU render consumes (uniform
    /// blocks, flags, resource, mask and source hashes, decode and geometry
    /// decisions) to this new JSON file, recorded on the export's own path
    /// (docs/DEVELOPMENT.md "Render-input snapshots"). A single photo only.
    #[arg(long)]
    render_inputs: Option<PathBuf>,
    /// Upper limit for photos exported at the same time (as the app's
    /// headless `--jobs`); default: the export queue's rule on cores and
    /// free memory.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    jobs: Option<u16>,
}

fn parse_color_space(
    text: &str,
) -> std::result::Result<rawtonic_engine::output_color_space::OutputColorSpace, String> {
    rawtonic_engine::output_color_space::OutputColorSpace::parse(text)
        .ok_or_else(|| "expected srgb, display-p3, adobe-rgb or prophoto".to_string())
}

fn parse_output_sharpening(
    text: &str,
) -> std::result::Result<rawtonic_export::output_sharpening::OutputSharpening, String> {
    let parts: Vec<f32> = text
        .split(':')
        .map(|p| p.trim().parse::<f32>().map_err(|e| format!("{p}: {e}")))
        .collect::<std::result::Result<_, _>>()?;
    match parts[..] {
        [amount, radius] | [amount, radius, _] if amount >= 0.0 && radius > 0.0 => {
            Ok(rawtonic_export::output_sharpening::OutputSharpening {
                amount,
                radius,
                threshold: parts.get(2).copied().unwrap_or(0.0).max(0.0),
            })
        }
        _ => Err("expected AMOUNT:RADIUS[:THRESHOLD]".to_string()),
    }
}
/// `recipe` with a face retouch of `settings` on the faces of `source`,
/// found as the app's Retouch tab finds them: in the mask frame (the base
/// image warped by the recipe's geometry, oriented and straightened, not
/// cropped).
fn with_face_retouch(
    source: &str,
    mut recipe: serde_json::Value,
    settings: &serde_json::Value,
    models: &rawtonic_faces::FaceModels,
    portrait: Option<&rawtonic_faces::portrait::PortraitModel>,
) -> Result<serde_json::Value> {
    let bytes = std::fs::read(source).with_context(|| format!("Read {source}"))?;
    let is_raw = rawtonic_engine::formats::is_raw_file(source);
    if !recipe.is_object() {
        recipe = serde_json::json!({});
    }
    // The base the export develops (its colour noise filter decides the
    // decode-time chroma clean-up), so the faces are found on the same pixels.
    let base = rawtonic_engine::image_loader::load_base_image(
        &bytes,
        source,
        false,
        &rawtonic_engine::image_loader::BaseImageOptions::default().for_recipe(&recipe, source),
        None,
    )?;
    // The geometry as the export renders it (the camera's lens correction
    // filled in), so the faces' frame matches the render's.
    rawtonic_export::normalize::normalize(
        &mut recipe,
        &rawtonic_export::normalize::RecipeSource::file(std::path::Path::new(source)),
        rawtonic_export::normalize::Bitmaps::None,
        rawtonic_export::normalize::Defaults::Render,
    )
    .lens
    .map_err(anyhow::Error::msg)?;
    let warped = rawtonic_engine::mask_generation::prepare_shared_warped_mask_image(
        &std::sync::Arc::new(base),
        is_raw,
        &recipe,
    )
    .map_err(anyhow::Error::msg)?;
    let mut uncropped = recipe.clone();
    uncropped[geometry::CROP.name()] = serde_json::Value::Null;
    let (framed, _) = rawtonic_engine::adjustment_utils::apply_spatial_transformations(
        warped.as_ref(),
        &uncropped,
    );
    let image = rawtonic_faces::recipe::display_rgb8(&framed, is_raw);
    let analysis = rawtonic_faces::analyze(&image, models)?;
    let faces = rawtonic_faces::recipe::retouch_faces(&image, &analysis, portrait)
        .map_err(anyhow::Error::msg)?;
    cli_eprintln!("{source}: {} face(s)", faces.len());
    recipe["faceRetouch"] = serde_json::json!({
        "settings": settings,
        "faces": faces,
        "frame": rawtonic_engine::face_retouch::frame_key(&recipe),
        "geometry": "",
    });
    if let Some(disk) = rawtonic_faces::recipe::iris_disk(portrait) {
        recipe["faceRetouch"]["irisDisk"] = serde_json::json!(disk);
    }
    Ok(recipe)
}

/// The app's file of GPU check verdicts for a model folder: beside it in
/// the app's data folder (`ai_processing::configure_acceleration`).
fn gpu_checks_file(models: &Path) -> Option<PathBuf> {
    Some(models.parent()?.join("ai-gpu-checks.tsv"))
}

/// The app's model folder: --models, $RAWTONIC_MODELS, then the installed
/// app's (cc.rawtonic.editor). Models loaded from it use the app's GPU
/// check verdicts too, so the CLI picks the provider the app picks and its
/// exports stay the app's (a device accepted before speed was compared
/// stays accepted, rawtonic-onnx `check`).
fn models_dir(models: Option<PathBuf>) -> Result<PathBuf> {
    let models = resolve_models_dir(models)?;
    rawtonic_onnx::configure(rawtonic_onnx::Settings {
        checks_file: gpu_checks_file(&models),
        ..rawtonic_onnx::settings()
    });
    Ok(models)
}

fn app_data_dir() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        let base = PathBuf::from(std::env::var_os("APPDATA").context("No APPDATA; use --catalog")?);
        return Ok(base.join("cc.rawtonic.editor"));
    }
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("No home directory; use --catalog")?,
    );
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
    };
    Ok(base.join("cc.rawtonic.editor"))
}

fn resolve_models_dir(models: Option<PathBuf>) -> Result<PathBuf> {
    models
        .or_else(|| std::env::var_os("RAWTONIC_MODELS").map(PathBuf::from))
        .or_else(|| app_data_dir().ok().map(|dir| dir.join("models")))
        .context("no model folder; pass --models")
}

/// The denoiser for AI Denoise `model` (`AiDenoiseSettings::models_for_source`)
/// from the app's model folder: MatchFilter on Core ML on macOS (1 the
/// mixed-precision package) and on ONNX Runtime elsewhere (one model for
/// every version); the Fast and the X-Trans network on ONNX Runtime
/// everywhere. ONNX Runtime needs `ORT_DYLIB_PATH`.
fn ai_denoiser(
    models: Option<PathBuf>,
    model: usize,
) -> Result<std::sync::Arc<dyn rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser>> {
    let models = models_dir(models)?;
    let cache = models.parent().map(|dir| dir.join("ai-denoise-cache"));
    if model == rawtonic_engine::image_loader::FAST_AI_DENOISE_MODEL {
        return Ok(std::sync::Arc::new(
            rawtonic_denoise::MatchFilter::fast_onnx(
                &models.join(rawtonic_denoise::FAST_ONNX_MODEL),
                cache,
            )?,
        ));
    }
    if model == rawtonic_engine::image_loader::XTRANS_AI_DENOISE_MODEL {
        return Ok(std::sync::Arc::new(rawtonic_denoise::XTransNet::onnx(
            &models.join(rawtonic_denoise::XTRANS_ONNX_MODEL),
            cache,
        )?));
    }
    #[cfg(target_os = "macos")]
    let filter = if model == 1 {
        rawtonic_denoise::MatchFilter::core_ml_mixed(
            &models.join(rawtonic_denoise::CORE_ML_MIXED_PACKAGE),
            cache,
        )?
    } else {
        rawtonic_denoise::MatchFilter::core_ml(
            &models.join(rawtonic_denoise::CORE_ML_PACKAGE),
            cache,
        )?
    };
    #[cfg(not(target_os = "macos"))]
    let filter = {
        let _ = model;
        rawtonic_denoise::MatchFilter::onnx(&models.join(rawtonic_denoise::ONNX_MODEL), cache)?
    };
    Ok(std::sync::Arc::new(filter))
}

/// `--super-resolution-quality`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SuperResolutionQualityArg {
    Full,
    Fast,
}

impl From<SuperResolutionQualityArg>
    for rawtonic_export::export_processing::SuperResolutionQuality
{
    fn from(quality: SuperResolutionQualityArg) -> Self {
        match quality {
            SuperResolutionQualityArg::Full => Self::Full,
            SuperResolutionQualityArg::Fast => Self::Fast,
        }
    }
}

/// Super Resolution's model of the app's model folder (`--super-resolution`).
struct SuperResolution(rawtonic_superres::Model);

impl rawtonic_export::export_processing::Enlarger for SuperResolution {
    fn enlarge_2x(
        &self,
        image: image::DynamicImage,
        quality: rawtonic_export::export_processing::SuperResolutionQuality,
        cancelled: &dyn Fn() -> Result<(), String>,
    ) -> Result<image::DynamicImage, String> {
        let started = std::time::Instant::now();
        let (width, height) = (image.width(), image.height());
        let quality = superres_quality(quality);
        let enlarged = self.0.enlarge(image, quality, cancelled)?;
        cli_eprintln!(
            "Super Resolution ({quality:?}): {width}x{height} enlarged on {} in {:.2} s",
            self.0.backend_name(),
            started.elapsed().as_secs_f64()
        );
        Ok(enlarged)
    }
}

/// The export recipe's quality as the model's.
fn superres_quality(
    quality: rawtonic_export::export_processing::SuperResolutionQuality,
) -> rawtonic_superres::Quality {
    match quality {
        rawtonic_export::export_processing::SuperResolutionQuality::Full => {
            rawtonic_superres::Quality::Full
        }
        rawtonic_export::export_processing::SuperResolutionQuality::Fast => {
            rawtonic_superres::Quality::Fast
        }
    }
}

/// The Sky, Subject, People and landscape networks of the app's model folder, for
/// selections an export computes (`ai_selections::detect_missing`). Depth
/// ranges are computed by the app only (its depth network is not in the
/// CLI); a photo needing one fails, naming the layer.
struct SelectionModels {
    segment: rawtonic_segment::Models,
    dir: PathBuf,
    faces: std::sync::OnceLock<Result<rawtonic_faces::FaceModels, String>>,
}

impl SelectionModels {
    fn new(dir: PathBuf) -> Self {
        Self {
            segment: rawtonic_segment::Models::new(&dir).with_total_memory(total_memory()),
            dir,
            faces: std::sync::OnceLock::new(),
        }
    }
}

impl rawtonic_engine::ai_selections::SelectionDetector for SelectionModels {
    fn detect(
        &self,
        selection: rawtonic_engine::ai_selections::AutoSelection,
        parameters: &serde_json::Value,
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<String, String> {
        use rawtonic_engine::ai_selections::{self as sel, AutoSelection};
        let mask = match selection {
            AutoSelection::Sky => self.segment.sky().and_then(|model| {
                rawtonic_segment::run_sky_seg_model(warped, &model, linear, None)
            }),
            AutoSelection::Subject => self.segment.subject().and_then(|model| {
                rawtonic_segment::subject_selection(
                    warped,
                    &model,
                    linear,
                    None,
                    sel::subject_version(parameters),
                )
            }),
            AutoSelection::People => {
                let models = self
                    .faces
                    .get_or_init(|| {
                        rawtonic_faces::FaceModels::load(&self.dir).map_err(|e| format!("{e:#}"))
                    })
                    .as_ref()
                    .map_err(|e| format!("the face models are not available: {e}"))?;
                let mask = rawtonic_faces::recipe::people_selection(
                    warped,
                    linear,
                    models,
                    &sel::people_parts(parameters),
                    sel::people_person(parameters),
                )?;
                return rawtonic_segment::mask_data_url(&mask);
            }
            AutoSelection::Depth => {
                return Err(
                    "depth ranges are computed by the app (export the photo from the app)".into(),
                );
            }
            AutoSelection::Landscape => {
                let parts = sel::landscape_parts(parameters)
                    .ok_or_else(|| "a Landscape selection without known parts".to_string())?;
                return self
                    .detect_landscape(&[parts], warped, linear)
                    .map(|mut masks| masks.remove(0));
            }
        }
        .map_err(|e| format!("{e:#}"))?;
        rawtonic_segment::mask_data_url(&mask)
    }

    fn detect_landscape(
        &self,
        sets: &[rawtonic_engine::ai_selections::LandscapeParts],
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<Vec<String>, String> {
        let model = self.segment.landscape().map_err(|e| format!("{e:#}"))?;
        let probabilities = rawtonic_segment::landscape_probabilities(warped, &model, linear, None)
            .map_err(|e| format!("{e:#}"))?;
        sets.iter()
            .map(|set| {
                let parts =
                    rawtonic_segment::landscape_parts(&set.keys()).map_err(|e| format!("{e:#}"))?;
                rawtonic_segment::mask_data_url(&rawtonic_segment::landscape_mask_grid(
                    &probabilities,
                    &parts,
                    set.version,
                ))
            })
            .collect()
    }
}

pub(crate) fn print(value: impl serde::Serialize) -> Result<()> {
    rawtonic_runtime::cli_output::write_result_line(
        std::io::stdout(),
        &serde_json::to_string_pretty(&value)?,
    )?;
    Ok(())
}
fn run() -> Result<()> {
    match Args::parse().command {
        Command::Create {
            directory,
            name,
            catalog,
        } => print(
            Library::create(
                &directory,
                &name,
                if catalog {
                    Mode::Catalog
                } else {
                    Mode::Session
                },
            )?
            .summary()?,
        ),
        Command::Inspect { path } => {
            if path.is_dir() {
                print(Library::open(&path)?.summary()?)
            } else {
                print(
                    serde_json::json!({"path":path,"bytes":std::fs::metadata(&path)?.len(),"fingerprint":rawtonic_catalog::fingerprint(&path)?,"sha256":rawtonic_ingest::sha256(&path)?}),
                )
            }
        }
        Command::List { library, search } => {
            print(Library::open(&library)?.assets(search.as_deref())?)
        }
        Command::Ingest {
            library,
            sources,
            template,
            backup,
        } => {
            let lib = Library::open(&library)?;
            let _batch = lib.import_batch(rawtonic_catalog::ImportKind::Ingest);
            let cancel = AtomicBool::new(false);
            let mut failed = 0;
            for (i, path) in sources.iter().enumerate() {
                match rawtonic_ingest::import_one(
                    &lib,
                    path,
                    &template,
                    i as u64 + 1,
                    backup.as_deref(),
                    &cancel,
                ) {
                    Ok(r) => {
                        if r.error.is_some() {
                            failed += 1;
                        }
                        print(r)?;
                    }
                    Err(e) => {
                        failed += 1;
                        cli_eprintln!("{}: {e:#}", path.display());
                    }
                }
            }
            anyhow::ensure!(
                failed == 0,
                "{failed} import(s) failed; completed files were retained"
            );
            Ok(())
        }
        Command::Name {
            sources,
            template,
            job,
            sequence_start,
        } => {
            let mut failed = 0;
            for (i, source) in sources.iter().enumerate() {
                match rawtonic_ingest::preview_name(
                    &job,
                    source,
                    &template,
                    sequence_start + i as u64,
                    sequence_start,
                ) {
                    Ok(preview) => print(serde_json::json!({
                        "source": source,
                        "name": preview.name,
                        "dateSource": preview.date_source,
                        "sidecar": preview.sidecar,
                    }))?,
                    Err(e) => {
                        failed += 1;
                        cli_eprintln!("{}: {e:#}", source.display());
                    }
                }
            }
            anyhow::ensure!(failed == 0, "{failed} name(s) could not be made");
            Ok(())
        }
        Command::Rate {
            library,
            asset,
            rating,
        } => {
            let l = Library::open(&library)?;
            l.adopt_editor_sidecar(&asset)?;
            let mut m = l.metadata(&asset)?;
            m.rating = rating;
            l.set_metadata(&asset, &m)?;
            print(l.asset(&asset)?)
        }
        Command::Flag {
            library,
            asset,
            flag,
        } => {
            let l = Library::open(&library)?;
            l.adopt_editor_sidecar(&asset)?;
            let mut m = l.metadata(&asset)?;
            m.flag = flag;
            l.set_metadata(&asset, &m)?;
            print(l.asset(&asset)?)
        }
        Command::Add { library, paths } => {
            let l = Library::open(&library)?;
            let _batch = l.import_batch(rawtonic_catalog::ImportKind::Add);
            let report = l.add_files(&paths)?;
            print(&report)?;
            anyhow::ensure!(
                report.failed.is_empty(),
                "{} file(s) not added; the others are in the library",
                report.failed.len()
            );
            Ok(())
        }
        Command::Missing { library } => {
            let l = Library::open(&library)?;
            l.verify_paths()?;
            print(l.offline_assets()?)
        }
        Command::Relink {
            library,
            asset,
            to,
            nearby,
        } => print(Library::open(&library)?.relink(&asset, &to, nearby)?),
        Command::RelinkFolder { library, from, to } => {
            print(Library::open(&library)?.relink_folder(&from, &to)?)
        }
        Command::SyncFolder {
            library,
            folder,
            apply,
            include_probable,
            include_copies,
            remove_missing,
        } => {
            use rawtonic_catalog::folder_sync::{SyncAction, SyncOptions, is_photo};
            let l = Library::open(&library)?;
            let cancel = AtomicBool::new(false);
            let plan = l.scan_folder_sync(
                folder.as_deref(),
                &SyncOptions {
                    accept: &is_photo,
                    cancel: &cancel,
                    progress: &|_| {},
                },
            )?;
            if !apply {
                return print(&plan);
            }
            let add = |f: &rawtonic_catalog::folder_sync::SyncNewFile| SyncAction::Add {
                path: f.path.clone(),
                identity: f.identity.clone(),
            };
            let mut actions: Vec<SyncAction> =
                plan.verified_moves.iter().map(SyncAction::from).collect();
            if include_probable {
                actions.extend(plan.probable_moves.iter().map(SyncAction::from));
            }
            actions.extend(plan.new_files.iter().map(add));
            if include_copies {
                actions.extend(plan.copies.iter().map(add));
            }
            if remove_missing {
                actions.extend(plan.removed.iter().map(|m| SyncAction::Remove {
                    asset_id: m.asset_id.clone(),
                    path: m.path.clone(),
                }));
            }
            let applied = l.apply_folder_sync(&actions)?;
            print(serde_json::json!({ "plan": plan, "applied": applied }))
        }
        Command::Search { library, rule } => {
            let rule: rawtonic_catalog::Rule =
                serde_json::from_str(&rule).context("The rule is not valid JSON")?;
            print(Library::open(&library)?.search(&rule)?)
        }
        Command::Facts { library } => {
            let l = Library::open(&library)?;
            let recorded = l.fill_all_facts(
                &AtomicBool::new(false),
                &rawtonic_ingest::capture::capture_facts,
            )?;
            print(serde_json::json!({ "recorded": recorded, "unread": l.facts_backlog()? }))
        }
        Command::SearchBench { photos, iterations } => search_bench::run(photos, iterations),
        Command::Checksums { library } => {
            let l = Library::open(&library)?;
            let recorded = l.fill_all_checksums(&AtomicBool::new(false))?;
            print(serde_json::json!({ "recorded": recorded, "without": l.checksum_backlog()? }))
        }
        Command::Duplicates { library, workers } => {
            let l = Library::open(&library)?;
            let started = std::time::Instant::now();
            let pass = l.fill_image_hashes(workers, &AtomicBool::new(false), &mut |_, _| {})?;
            let hashed_in = started.elapsed();
            let started = std::time::Instant::now();
            let groups = l.duplicate_groups()?;
            print(serde_json::json!({
                "photos": pass.photos,
                "hashed": pass.hashed,
                "unreadable": pass.unreadable,
                "hashSeconds": hashed_in.as_secs_f64(),
                "groupSeconds": started.elapsed().as_secs_f64(),
                "groups": groups,
            }))
        }
        Command::AdoptXmp { library, asset } => print(Library::open(&library)?.adopt_xmp(&asset)?),
        Command::Sync { library } => {
            let l = Library::open(&library)?;
            let mut failed = 0;
            for a in l.assets(None)? {
                if let Err(e) = l.sync_xmp(&a.id) {
                    failed += 1;
                    cli_eprintln!("{}: {e:#}", a.rel_path);
                }
            }
            print(l.summary()?)?;
            anyhow::ensure!(
                failed == 0,
                "{failed} sidecars are not in sync (offline, not writable or in conflict)"
            );
            Ok(())
        }
        Command::Bench { iterations } => {
            anyhow::ensure!(
                (10..=100_000).contains(&iterations),
                "Use 10–100000 iterations"
            );
            let d = tempfile::tempdir()?;
            let l = Library::create(d.path(), "Benchmark", Mode::Session)?;
            let p = d.path().join("Capture/synthetic.bin");
            std::fs::write(&p, b"synthetic storage fixture")?;
            let a = l.add_asset(&p)?;
            let mut meta = l.metadata(&a.id)?;
            let mut timings = Vec::new();
            for i in 0..iterations {
                meta.rating = (i % 6) as u8;
                let now = Instant::now();
                l.set_metadata(&a.id, &meta)?;
                timings.push(now.elapsed().as_secs_f64() * 1000.0);
            }
            timings.sort_by(f64::total_cmp);
            let p95 = timings[((timings.len() - 1) as f64 * 0.95).ceil() as usize];
            print(
                serde_json::json!({"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"fixture":"synthetic single-asset SQLite WAL FULL","iterations":iterations,"metadata_ms":{"median":timings[timings.len()/2],"p95":p95,"max":timings.last().context("No samples")?},"budget_ms":5,"p95_within_budget":p95<5.0,"gpu_render":"unmeasured","raw_decode":"unmeasured","grid_scroll":"unmeasured","tether":"hardware absent","target_hardware_qualified":false}),
            )
        }
        Command::Decode {
            path,
            repeats,
            fast,
        } => decode(&path, repeats, fast),
        Command::Export(args) => export(args),
        Command::ProofSheet(args) => proof_sheet(args),
        Command::Merge(args) => merge(args),
        Command::AiBench(args) => ai_bench(args),
        Command::Fingerprint(args) => fingerprint::run(args),
    }
}
fn ai_bench(args: AiBenchArgs) -> Result<()> {
    use rawtonic_onnx::Provider;
    use rawtonic_onnx::bench::{Picture, Timing, bench_file, model_files};
    let provider = Provider::parse(&args.provider).context("--provider: webgpu, cuda or cpu")?;
    let models = models_dir(args.models)?;
    let mut dirs: Vec<PathBuf> = args.plugins.into_iter().collect();
    dirs.extend(rawtonic_onnx::default_plugin_dirs());
    dirs.push(models.clone());
    let picture = match &args.image {
        Some(path) => {
            let rgb = image::open(path)
                .with_context(|| format!("reading {}", path.display()))?
                .to_rgb8();
            Picture {
                width: rgb.width() as usize,
                height: rgb.height() as usize,
                rgb: rgb.into_raw(),
            }
        }
        None => Picture::synthetic(1536, 1024),
    };
    let only: Vec<String> = args
        .only
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let timing = |t: &Timing| serde_json::json!({"load_ms": t.load_ms, "first_run_ms": t.first_run_ms, "median_ms": t.median_ms(), "runs_ms": t.runs_ms});
    let mut reports = Vec::new();
    for path in model_files(&models, args.with_text)? {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !only.is_empty() && !only.iter().any(|o| name.starts_with(o.as_str())) {
            continue;
        }
        cli_eprintln!("{name}");
        let r = bench_file(&path, provider, args.runs, &dirs, &picture)?;
        reports.push(serde_json::json!({
            "file": r.file,
            "model": r.kind.name(),
            "cpu": timing(&r.cpu),
            "cpu_digests": r.cpu_digests.iter().map(|(o, d)| serde_json::json!({"output": o, "blake3": d})).collect::<Vec<_>>(),
            "provider": r.gpu_provider.name(),
            "device": r.gpu_device,
            "gpu": r.gpu.as_ref().map(timing),
            "gpu_error": r.gpu_error,
            "app_uses_gpu": r.kind.gpu_exception(provider).is_none(),
            "agreement": r.agreement.iter().map(|a| serde_json::json!({
                "output": a.output, "max_abs": a.max_abs, "mean_abs": a.mean_abs,
                "max_rel_range": a.max_rel_range, "iou": a.iou, "finite": a.finite,
            })).collect::<Vec<_>>(),
        }));
    }
    print(serde_json::json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "input": args.image.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "synthetic 1536x1024".into()),
        "runs": args.runs,
        "note": "Model inference in this process, one model at a time; the first run includes shader compilation. Not the app's click-to-mask time (no pre- or post-processing).",
        "models": reports,
    }))
}

fn merge(args: MergeArgs) -> Result<()> {
    use rawtonic_merge::{dng, frame};
    let options = rawtonic_engine::image_loader::BaseImageOptions::default();
    let started = Instant::now();
    let progress = |msg: &str| cli_eprintln!("{msg}");
    let mut lens_applied = false;
    let (image, reference, color, metadata, report) = match args.kind.as_str() {
        "single" => {
            let frame = frame::load_frame(&args.sources[0], &options)?;
            (
                frame.image,
                frame.path,
                frame.color,
                frame.metadata,
                serde_json::Value::Null,
            )
        }
        "hdr" => {
            let deghost = serde_json::from_value(serde_json::json!(args.deghost))
                .context("--deghost: off, low, medium or high")?;
            let config = rawtonic_merge::hdr::HdrConfig {
                align: !args.no_align,
                deghost,
            };
            let result =
                rawtonic_merge::hdr::merge_hdr(&args.sources, &options, &config, &progress)?;
            let report = serde_json::to_value(&result.frames)?;
            (
                result.image,
                result.reference_path,
                result.color,
                result.metadata,
                report,
            )
        }
        "focus" => {
            let config = rawtonic_merge::focus_stack::FocusConfig {
                align: !args.no_align,
            };
            let result =
                rawtonic_merge::focus_stack::stack(&args.sources, &options, &config, &progress)
                    .map_err(anyhow::Error::msg)?;
            lens_applied = true;
            let report = serde_json::json!({
                "reference": result.stack.reference,
                "size": [result.image.width(), result.image.height()],
            });
            (
                result.image,
                result.reference_path,
                result.color,
                result.metadata,
                report,
            )
        }
        "hdrpano" => {
            let deghost = serde_json::from_value(serde_json::json!(args.deghost))
                .context("--deghost: off, low, medium or high")?;
            let hdr = rawtonic_merge::hdr::HdrConfig {
                align: !args.no_align,
                deghost,
            };
            let projection = serde_json::from_value(serde_json::json!(args.projection))
                .context("--projection: auto, spherical, cylindrical, perspective or panini")?;
            let config = rawtonic_merge::panorama::PanoramaConfig {
                projection,
                boundary_warp: args.boundary_warp,
                auto_crop: !args.no_crop,
            };
            let result = rawtonic_merge::hdr_panorama::hdr_panorama(
                &args.sources,
                &options,
                &hdr,
                &config,
                &progress,
            )?;
            lens_applied = true;
            let report = serde_json::json!({
                "projection": result.projection,
                "rmsPx": result.rms_px,
                "fovDeg": result.fov_deg,
                "frames": result.frames,
            });
            (
                rawtonic_merge::panorama::finish(&result.image, &result.covered, &config).0,
                result.reference_path,
                result.color,
                result.metadata,
                report,
            )
        }
        "pano" => {
            let projection = serde_json::from_value(serde_json::json!(args.projection))
                .context("--projection: auto, spherical, cylindrical, perspective or panini")?;
            let config = rawtonic_merge::panorama::PanoramaConfig {
                projection,
                boundary_warp: args.boundary_warp,
                auto_crop: !args.no_crop,
            };
            let result =
                rawtonic_merge::panorama::stitch(&args.sources, &options, &config, &progress)?;
            lens_applied = true;
            let report = serde_json::json!({
                "projection": result.projection,
                "rmsPx": result.rms_px,
                "fovDeg": result.fov_deg,
                "frames": result.frames,
            });
            (
                rawtonic_merge::panorama::finish(&result.image, &result.covered, &config).0,
                result.reference_path,
                result.color,
                result.metadata,
                report,
            )
        }
        other => anyhow::bail!("unknown merge kind {other}"),
    };
    let loaded = started.elapsed();
    let bytes = dng::encode_linear_dng(
        &image,
        &dng::DngMeta {
            color: color.as_ref(),
            metadata: metadata.as_ref(),
            keep_lens: !lens_applied,
            baseline_exposure: Some(rawtonic_engine::baseline_exposure::dng_baseline_equivalent(
                &reference.to_string_lossy(),
            ) as f64),
            preview: None,
            software: "Rawtonic",
        },
    )?;
    rawtonic_export::output::write_new_output(&args.output, &bytes)
        .with_context(|| format!("Write {}", args.output.display()))?;
    let sidecar = sidecar_path_of(&args.output);
    let mut adjustments = rawtonic_merge::initial_adjustments(&reference, lens_applied);
    if args.auto_settings {
        rawtonic_merge::apply_auto_settings(&mut adjustments, &image, &args.output);
    }
    rawtonic_export::output::write_new_output(
        &sidecar,
        rawtonic_merge::sidecar_json(adjustments)?,
    )?;
    print(serde_json::json!({
        "output": args.output,
        "bytes": bytes.len(),
        "merge_ms": loaded.as_millis(),
        "frames": report,
        "total_ms": started.elapsed().as_millis(),
    }))
}
fn decode(path: &std::path::Path, repeats: u32, fast: bool) -> Result<()> {
    anyhow::ensure!((1..=100).contains(&repeats), "Use 1–100 repeats");
    let bytes = std::fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    let mut timings = Vec::new();
    let mut size = (0, 0);
    let mut measures = None;
    let mut measure_timings = Vec::new();
    for _ in 0..repeats {
        let now = Instant::now();
        let image = rawtonic_rawdecode::raw_processing::develop_raw_image(
            &bytes,
            &rawtonic_rawdecode::raw_processing::DecodeOptions {
                fast_demosaic: fast,
                ..Default::default()
            },
        )?;
        timings.push(now.elapsed().as_secs_f64() * 1000.0);
        size = (image.width(), image.height());
        if !fast {
            let now = Instant::now();
            measures = Some(rawtonic_engine::image_loader::DecodeMeasures::of(&image));
            measure_timings.push(now.elapsed().as_secs_f64() * 1000.0);
        }
    }
    timings.sort_by(f64::total_cmp);
    measure_timings.sort_by(f64::total_cmp);
    let measures_ms = (!measure_timings.is_empty()).then(|| {
        let n = measure_timings.len();
        serde_json::json!({"median": measure_timings[n / 2], "min": measure_timings[0], "max": measure_timings[n - 1]})
    });
    print(serde_json::json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "width": size.0,
        "height": size.1,
        "repeats": repeats,
        "fast_demosaic": fast,
        "decode_ms": {"median": timings[timings.len() / 2], "min": timings[0], "max": timings[timings.len() - 1]},
        // What renders read from the decode (image_loader::DecodeMeasures),
        // timed apart from the decode.
        "measures_ms": measures_ms,
        "noise_level": measures.and_then(|m| m.noise_level),
        "image_key": measures.and_then(|m| m.image_key),
        "airlight": measures.and_then(|m| m.airlight),
        "measures": "rawdecode only: file bytes in memory to oriented linear RGB (CPU demosaic, system allocator); excludes file read, GPU develop, encoding and the app's mimalloc tuning",
    }))
}
fn export(args: ExportArgs) -> Result<()> {
    use rawtonic_export::export_processing::{
        ExportSettings, TiffBitDepth, relative_export_dir_for_preserved_folders,
    };
    let ExportArgs {
        catalog,
        source,
        output,
        on_conflict,
        export_records,
        adjustments,
        format,
        quality,
        max_file_kb,
        tiff_bit_depth,
        keep_metadata,
        strip_gps,
        keep_creator_contact,
        color_space,
        lensfun_db,
        models,
        output_sharpening,
        super_resolution,
        super_resolution_quality,
        provenance_identity,
        provenance_author,
        face_retouch,
        render_inputs,
        jobs: max_jobs,
    } = args;
    if let Some(path) = &render_inputs {
        anyhow::ensure!(
            source.is_file(),
            "--render-inputs records one export: give a single photo"
        );
        anyhow::ensure!(
            !path.exists(),
            "Render-input file exists; Rawtonic never overwrites: {}",
            path.display()
        );
    }
    use rawtonic_export::existing_files::{
        self, Decision, ExistingFiles, ExportRecords, History, Outcome,
    };
    anyhow::ensure!(
        on_conflict != ExistingFiles::ReplaceIfChanged || export_records.is_some(),
        "--on-conflict replace-if-changed needs --export-records FILE (the record of earlier exports)"
    );
    let records_path = export_records;
    let started_at = Instant::now();
    let lensfun_db = lensfun_folder(lensfun_db)?;
    let recipe = adjustments
        .as_deref()
        .map(|path| -> Result<serde_json::Value> {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("Read {}", path.display()))?;
            Ok(serde_json::from_str(&text)?)
        })
        .transpose()?;
    // As the app's headless export: capture-date file times, folders kept.
    let settings = ExportSettings {
        jpeg_quality: quality,
        tiff_bit_depth: TiffBitDepth::try_from(tiff_bit_depth).map_err(anyhow::Error::msg)?,
        resize: None,
        border: None,
        pad: None,
        keep_metadata,
        color_space,
        preserve_timestamps: true,
        strip_gps,
        keep_creator_contact,
        filename_template: None,
        name_template: None,
        job_name: None,
        recipe_name: None,
        sequence_start: None,
        watermark: None,
        export_masks: false,
        preserve_folders: true,
        destination_type: None,
        subfolder: None,
        output_sharpening,
        super_resolution: super_resolution || super_resolution_quality.is_some(),
        super_resolution_quality: super_resolution_quality.map(Into::into).unwrap_or_default(),
        provenance: provenance_identity.as_ref().map(|_| {
            rawtonic_export::export_processing::ProvenanceSettings {
                include_author: provenance_author,
                signer: None,
            }
        }),
        existing_files: Default::default(),
        max_file_kb,
    };

    // (photo path, output) pairs. A folder exports every supported image below
    // it and each virtual copy, in the app's library order, named like the
    // app's folder export ("<name>_edited.<format>", "_VC01"… for copies,
    // source subfolders kept, "_1", "_2"… instead of overwriting).
    use rawtonic_export::sidecars::{
        export_stem, parse_virtual_path, plan_export_items, sources_with_copies, virtual_path,
    };
    // Each photo with its output's own name and whether another name may
    // be chosen for it (a folder export) or not (a file the user named).
    let named: Vec<(String, PathBuf, bool)> = if source.is_dir() {
        let photos: Vec<String> = sources_with_copies(&source)
            .into_iter()
            .flat_map(|(image, copies)| {
                copies
                    .into_iter()
                    .map(move |copy| virtual_path(&image, copy.as_deref()))
            })
            .collect();
        anyhow::ensure!(
            !photos.is_empty(),
            "No supported images in {}",
            source.display()
        );
        let origin = vec![source.to_string_lossy().into_owned()];
        let total = photos.len();
        plan_export_items(photos)
            .into_iter()
            .map(
                |(index, photo, appearance, explicit_vc)| -> Result<(String, PathBuf, bool)> {
                    let (path, _) = parse_virtual_path(&photo);
                    // Naming must not fetch a TIFF that its export will skip.
                    let capture = if rawtonic_export::file_export::require_local_tiff(&path).is_ok()
                    {
                        rawtonic_export::exif_processing::capture_date_with(&path, &|| None)
                    } else {
                        jiff::Timestamp::now()
                    };
                    let naming =
                        settings.naming_context(&path, index + 1, total, &capture, &format, None);
                    let stem = export_stem(
                        &settings.naming_template(),
                        &naming,
                        appearance,
                        explicit_vc,
                    )
                    .map_err(anyhow::Error::msg)?;
                    let dir = relative_export_dir_for_preserved_folders(&path, &origin)
                        .map_or_else(|| output.clone(), |rel| output.join(rel));
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("Create {}", dir.display()))?;
                    Ok((photo, dir.join(format!("{stem}.{format}")), true))
                },
            )
            .collect::<Result<_>>()?
    } else {
        anyhow::ensure!(source.is_file(), "Source not found: {}", source.display());
        anyhow::ensure!(
            output.extension().is_some(),
            "The output file needs an extension (jpg, png, tif, webp, jxl, avif)"
        );
        anyhow::ensure!(
            on_conflict != ExistingFiles::KeepBoth || !output.exists(),
            "Output exists; Rawtonic never overwrites: {}",
            output.display()
        );
        vec![(source.to_string_lossy().into_owned(), output.clone(), false)]
    };
    // Skip needs no recipe: the photos whose file exists are passed by
    // before anything is decoded or rendered (no graphics card is even
    // opened when every photo is skipped); only naming read their dates.
    let mut skipped = Vec::new();
    let named: Vec<(String, PathBuf, bool)> = named
        .into_iter()
        .filter(|(photo, own, _)| {
            let skip = on_conflict == ExistingFiles::Skip && own.exists();
            if skip {
                skipped.push(serde_json::json!({
                    "source": photo, "output": own, "reason": Outcome::SkippedExists.as_str()
                }));
            }
            !skip
        })
        .collect();
    if named.is_empty() {
        return print(serde_json::json!({
            "written": [], "skipped": skipped, "replaced": [], "keptBoth": [], "keptOriginal": [],
            "failed": [],
            "seconds": started_at.elapsed().as_secs_f64(),
        }));
    }

    let mut known = if catalog.is_some() {
        Vec::new()
    } else {
        export_library::registered(&app_data_dir()?.join("settings.json"))?
    };
    let owners: std::collections::HashMap<PathBuf, Option<PathBuf>> = named
        .iter()
        .map(|(photo, _, _)| {
            let source = parse_virtual_path(photo).0;
            export_library::owner(&source, catalog.as_deref(), &known).map(|owner| (source, owner))
        })
        .collect::<Result<_>>()?;
    // Pin the catalog inputs before rendering, as the app does. Read failures
    // cannot silently discard metadata or a capture correction.
    let mut managed = std::collections::HashMap::new();
    for (source, owner) in &owners {
        if let Some(owner) = owner {
            let lib = Library::open(owner)?;
            let asset = lib
                .asset_at(source)?
                .context("Managed photo disappeared from its library")?;
            managed.insert(
                source.clone(),
                (
                    lib.metadata(&asset.id)?,
                    lib.capture_override(&asset.id)?
                        .map(|time| time.exif_fields()),
                    lib.editor_state(&asset.id)?,
                    lib.has_generated_provenance(&asset.id)?,
                ),
            );
        }
    }
    if let Some(selected) = &catalog {
        known.push(selected.clone());
    }
    let is_library_photo = |path: &Path| {
        is_library_photo(path)
            || export_library::owner(path, None, &known).map_or(true, |owner| owner.is_some())
    };
    let library_of = |source: &Path| owners.get(source).cloned().flatten();
    let photo_metadata = |photo: &str, source: &Path, sidecar: &Path| {
        let metadata = managed.get(source)?.0.clone();
        Some(if rawtonic_metadata::photo_ref::is_copy(photo) {
            let copy = rawtonic_export::sidecars::read_sidecar_file(sidecar, None).0;
            rawtonic_catalog::copy_metadata(metadata, copy.rating, copy.tags.as_deref())
        } else {
            metadata
        })
    };
    let corrected_capture = |source: &Path| match managed.get(source) {
        Some((_, capture, _, _)) => capture.clone(),
        None => rawtonic_export::file_export::sidecar_capture(source),
    };

    let generated_provenance = |source: &Path| managed.get(source).is_some_and(|entry| entry.3);

    let started = Instant::now();
    // Missing models are reported before the GPU starts: that is cheaper,
    // and the user learns what to install even where no GPU can start.
    let enlarger = if settings.super_resolution {
        let dir = models_dir(models.clone()).context("Super Resolution")?;
        let model = rawtonic_superres::Model::from_dir(&dir).context("Super Resolution")?;
        if model.uses_onnx() {
            ONNX_RUNTIME_USED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        anyhow::ensure!(
            settings.super_resolution_quality.is_full() || model.fast_installed(),
            "the Super Resolution Fast model is not installed in {} (tick Super Resolution in the app's Export dialog, or run scripts/models/install_models.sh)",
            dir.display()
        );
        Some(model)
    } else {
        None
    };
    // First: it loads the Lensfun database, which preparing the recipes
    // (face retouch: lens defaults and lens parameters of recipes saved
    // while the photo could not be read) already reads.
    let exporter =
        rawtonic_export::file_export::FileExporter::new(&lensfun_db).map_err(anyhow::Error::msg)?;
    // --adjustments overrides every photo; otherwise each photo's own
    // recipe, as the app's export reads it.
    let recipes: Vec<Option<serde_json::Value>> = named
        .iter()
        .map(|(photo, _, _)| match &recipe {
            Some(recipe) => Some(recipe.clone()),
            None => {
                let (source, sidecar) = parse_virtual_path(photo);
                let state = (!rawtonic_metadata::photo_ref::is_copy(photo))
                    .then(|| managed.get(&source))
                    .flatten()
                    .and_then(|(_, _, state, _)| state.clone())
                    .and_then(|state| {
                        serde_json::from_value::<rawtonic_engine::image_processing::ImageMetadata>(
                            state,
                        )
                        .ok()
                    });
                Some(state.map(|state| state.adjustments).unwrap_or_else(|| {
                    rawtonic_export::sidecars::read_sidecar_file(
                        &sidecar,
                        library_of(&source).as_deref(),
                    )
                    .0
                    .adjustments
                }))
            }
        })
        .collect();
    let recipes = match &face_retouch {
        Some(settings) => {
            let settings: serde_json::Value =
                serde_json::from_str(settings).context("--face-retouch: JSON expected")?;
            ONNX_RUNTIME_USED.store(true, std::sync::atomic::Ordering::Relaxed);
            let face_models = rawtonic_faces::FaceModels::load(&models_dir(models.clone())?)?;
            // New Eyes and Teeth use the eye and teeth parser, as in the app.
            let portrait = rawtonic_engine::face_retouch::Settings::uses_eyes_teeth(&settings)
                .then(|| {
                    rawtonic_faces::portrait::PortraitModel::from_dir(&models_dir(models.clone())?)
                        .context("Eyes and Teeth")
                })
                .transpose()?;
            let recipes = named
                .iter()
                .zip(recipes)
                .map(|((photo, _, _), recipe)| {
                    let source = parse_virtual_path(photo).0;
                    rawtonic_export::file_export::require_local_tiff(&source)
                        .map_err(anyhow::Error::msg)?;
                    with_face_retouch(
                        &source.to_string_lossy(),
                        recipe.unwrap_or_default(),
                        &settings,
                        &face_models,
                        portrait.as_ref(),
                    )
                    .map(Some)
                })
                .collect::<Result<Vec<_>>>()?;
            // ONNX Runtime aborts when its environment is torn down at exit.
            std::mem::forget(face_models);
            std::mem::forget(portrait);
            recipes
        }
        None => recipes,
    };
    let exporter = match enlarger {
        Some(model) => exporter.with_enlarger(std::sync::Arc::new(SuperResolution(model))),
        None => exporter,
    };
    let exporter = match &provenance_identity {
        Some(dir) => {
            let identity = rawtonic_export::c2pa::identity::load(dir)
                .map_err(anyhow::Error::msg)?
                .with_context(|| format!("No signing identity in {}", dir.display()))?;
            exporter.with_provenance(std::sync::Arc::new(identity))
        }
        None => exporter,
    };
    let sources: Vec<PathBuf> = named
        .iter()
        .map(|(photo, _, _)| rawtonic_export::sidecars::parse_virtual_path(photo).0)
        .collect();
    // What decides each photo's file (for --on-conflict replace-if-changed):
    // its recipe, metadata, corrected capture time and source file, with
    // the export's settings and format.
    let export_inputs: Vec<String> = named
        .iter()
        .zip(&recipes)
        .map(|((photo, _, _), recipe)| {
            let (path, sidecar) = parse_virtual_path(photo);
            let lut_file = recipe
                .as_ref()
                .and_then(|recipe| recipe["lutPath"].as_str())
                .filter(|lut| !lut.is_empty())
                .map(|lut| existing_files::file_fingerprint(std::path::Path::new(lut)));
            let mut parts = serde_json::json!({
                "catalog": photo_metadata(photo, &path, &sidecar),
                "sourceGenerated": generated_provenance(&path),
                "capture": corrected_capture(&path),
                "source": rawtonic_catalog::fingerprint(&path).ok(),
                "lutFile": lut_file,
                "format": format.to_lowercase(),
                "settings": existing_files::settings_for_hash(&settings),
            });
            parts["recipe"] = recipe.as_ref().map_or(serde_json::Value::Null, |recipe| {
                existing_files::adjustments_for_hash(recipe, &path)
            });
            existing_files::inputs_hash(env!("RAWTONIC_APP_VERSION"), &parts)
        })
        .collect();
    // Each photo as its exports are recorded: its path and its original's
    // fingerprint, so another photo put at the same path is never taken for
    // it.
    let photo_keys: Vec<Option<String>> = named
        .iter()
        .map(|(photo, _, _)| {
            let original = parse_virtual_path(photo).0;
            existing_files::photo_key(
                photo,
                rawtonic_catalog::fingerprint(&original).ok().as_deref(),
            )
        })
        .collect();
    // Decided before anything renders: written (its own or a numbered
    // name), replaced, or skipped.
    let records = records_path
        .as_deref()
        .map(ExportRecords::open)
        .transpose()?;
    let mut used = std::collections::HashSet::new();
    let mut jobs: Vec<(
        String,
        PathBuf,
        Option<existing_files::Recorded>,
        Outcome,
        usize,
    )> = Vec::new();
    let history = History {
        records: &|| records.as_ref(),
        is_original: &is_library_photo,
    };
    for (named_index, ((photo, own, may_number), inputs)) in
        named.iter().zip(&export_inputs).enumerate()
    {
        let photo_key = photo_keys[named_index].as_deref();
        let (path, decision) = history.decide(
            on_conflict,
            own,
            own.exists(),
            used.contains(own),
            false,
            photo_key.unwrap_or_default(),
            || inputs.as_str(),
            |path| used.contains(path),
        );
        let (path, replace, outcome) = match decision {
            Decision::Skip(outcome) => {
                if outcome == Outcome::SkippedUnchanged && path != *own {
                    used.insert(path.clone());
                }
                skipped.push(serde_json::json!({
                    "source": photo, "output": path, "reason": outcome.as_str()
                }));
                continue;
            }
            Decision::Write => (path, None, Outcome::Written),
            Decision::Replace(previous) => (path, Some(previous), Outcome::Replaced),
            Decision::Number { reported } => {
                // A file the user named is numbered only as Replace if
                // changed's fallback; Keep both refuses it, as before.
                anyhow::ensure!(
                    *may_number || reported.is_some(),
                    "Output exists; Rawtonic never overwrites: {}",
                    own.display()
                );
                let stem = own.file_stem().unwrap_or_default().to_string_lossy();
                let extension = own
                    .extension()
                    .map_or_else(|| format.clone(), |ext| ext.to_string_lossy().into_owned());
                let mut target = own.clone();
                let mut counter = 1;
                while target.exists() || used.contains(&target) {
                    target = own.with_file_name(format!("{stem}_{counter}.{extension}"));
                    counter += 1;
                }
                (target, None, reported.unwrap_or(Outcome::Written))
            }
        };
        used.insert(path.clone());
        jobs.push((photo.clone(), path, replace, outcome, named_index));
    }
    let exporter = with_recipe_models(exporter, models, &recipes, &sources);
    let inputs = render_inputs
        .as_ref()
        .map(|_| rawtonic_export::render_inputs::RenderInputs::new());
    let exporter = match &inputs {
        Some(inputs) => exporter.with_render_inputs(inputs.clone()),
        None => exporter,
    };
    // Several photos at once, as the app's export queue renders them: its
    // number of workers, each in a thread pool of its own
    // (`rawtonic_export::workers`). One at a time while the render inputs
    // are recorded, which each export replaces.
    let workers = if inputs.is_some() {
        1
    } else {
        rawtonic_export::workers::export_workers(
            jobs.len(),
            rawtonic_export::workers::available_cores(),
            available_memory(),
        )
        .min(max_jobs.map_or(usize::MAX, usize::from))
    };
    let pools = rawtonic_export::workers::lane_pools(
        workers,
        rawtonic_export::workers::available_cores(),
        "export",
        || {},
    );
    let exporter = &exporter;
    // Files that stayed above --max-file-kb at the lowest JPEG quality.
    let size_limit_missed = std::sync::Mutex::new(Vec::new());
    let outcomes = rawtonic_export::workers::run_in_lanes(&pools, jobs.len(), |i| {
        let (photo, output, replace, _, named_index) = &jobs[i];
        let named_index = *named_index;
        let recipe = &recipes[named_index];
        let (source, sidecar) = parse_virtual_path(photo);
        if let Err(error) = rawtonic_export::file_export::require_local_tiff(&source) {
            return Err(serde_json::json!({"source": photo, "error": error}));
        }
        let library = library_of(&source);
        if let Some(resource) = recipe
            .as_ref()
            .and_then(|r| unresolved_resource(r, library.as_deref()))
        {
            return Err(serde_json::json!({"source": photo, "error": format!(
                "The recipe names the library resource {resource}, but the photo is in no session or catalog"
            )}));
        }
        // A render out of graphics memory (another program holding it) is
        // exported again after a wait, into the same file (app: the same).
        let catalog = photo_metadata(photo, &source, &sidecar);
        let capture = corrected_capture(&source);
        // A replacement renders only the photo it was planned for: a sync
        // tool that put another file at the photo's path since gets no
        // export over the first photo's file.
        if replace.is_some()
            && existing_files::photo_key(
                photo,
                rawtonic_catalog::fingerprint(&source).ok().as_deref(),
            ) != photo_keys[named_index]
        {
            return Err(serde_json::json!({"source": photo, "error":
                "The photo's file changed since the export was planned; its earlier export is kept"}));
        }
        let written_file = std::sync::Mutex::new(None);
        let mut results = rawtonic_runtime::gpu_memory::retry_out_of_memory(
            &[()],
            |_| {
                let result = exporter.export_written(&rawtonic_export::file_export::FileExport {
                    source: &source,
                    output,
                    format: &format,
                    recipe: recipe.clone(),
                    settings: settings.clone(),
                    base: rawtonic_engine::image_loader::BaseImageOptions::default(),
                    catalog: catalog.clone(),
                    capture: capture.clone(),
                    library: library.as_deref(),
                    source_generated: generated_provenance(&source),
                    publish: match (replace, &photo_keys[named_index]) {
                        (Some(previous), Some(photo_key)) => {
                            rawtonic_export::output::Publish::Replace {
                                previous,
                                source: photo_key,
                            }
                        }
                        _ => rawtonic_export::output::Publish::New,
                    },
                });
                let result = result.map(|file| {
                    *written_file.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
                });
                vec![((), result)]
            },
            // Before a replace the file is there: whether it is still the
            // previous export (the failed attempt published nothing).
            |()| match (replace, &photo_keys[named_index]) {
                (Some(previous), Some(photo_key)) => previous.is_export_of(output, photo_key),
                _ => !output.exists(),
            },
            |_, wait| {
                cli_eprintln!(
                    "Rawtonic: the graphics card ran out of memory for {photo}; again in {wait:?}"
                );
                std::thread::sleep(wait);
                true
            },
        );
        match results.remove(0).1 {
            Ok(()) => {
                let written_file = written_file.into_inner().unwrap_or_else(|e| e.into_inner());
                if let (Some(records), Some(photo_key), Some(file)) =
                    (&records_path, &photo_keys[named_index], &written_file)
                {
                    // One connection per thread (SQLite). What was written,
                    // not what the path holds by now.
                    let recorded = ExportRecords::open(records).and_then(|records| {
                        records.record_written(
                            output,
                            &named[named_index].1,
                            photo_key,
                            &export_inputs[named_index],
                            file,
                        )
                    });
                    if let Err(e) = recorded {
                        cli_eprintln!(
                            "Rawtonic: recording {} in the export records failed: {e:#}",
                            output.display()
                        );
                    }
                }
                if let (Some(limit), Some(file)) = (
                    rawtonic_export::size_limit::limit_bytes(settings.max_file_kb),
                    &written_file,
                ) && file.size > limit
                    && matches!(
                        output
                            .extension()
                            .map_or(format.as_str(), |ext| ext.to_str().unwrap_or_default())
                            .to_lowercase()
                            .as_str(),
                        "jpg" | "jpeg"
                    )
                {
                    let notice = rawtonic_export::size_limit::SizeLimitMiss {
                        limit_bytes: limit,
                        bytes: file.size,
                    }
                    .for_file(output);
                    let message = notice.describe();
                    cli_eprintln!("Rawtonic: {message}");
                    size_limit_missed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(serde_json::json!({
                            "output": notice.file,
                            "limitBytes": notice.limit_bytes,
                            "bytes": notice.bytes,
                            "message": message,
                        }));
                }
                Ok(output.clone())
            }
            Err(error) => Err(serde_json::json!({"source": photo, "error": error})),
        }
    });
    let mut written = Vec::new();
    let mut replaced = Vec::new();
    let mut kept_both = Vec::new();
    let mut kept_original = Vec::new();
    let mut failed = Vec::new();
    for (outcome, (_, _, _, planned, _)) in outcomes.into_iter().zip(&jobs) {
        match outcome {
            Ok(path) => {
                match planned {
                    Outcome::Replaced => replaced.push(path.clone()),
                    Outcome::KeptBoth => kept_both.push(path.clone()),
                    Outcome::KeptOriginal => kept_original.push(path.clone()),
                    _ => {}
                }
                written.push(path);
            }
            Err(failure) => failed.push(failure),
        }
    }
    if let (Some(path), Some(inputs), true) = (&render_inputs, &inputs, failed.is_empty()) {
        let snapshot = inputs.snapshot();
        let mut text = serde_json::to_string_pretty(&snapshot)?;
        text.push('\n');
        rawtonic_export::output::write_new_output(path, text.as_bytes())
            .with_context(|| format!("Write {}", path.display()))?;
        written.push(path.clone());
    }
    let size_limit_missed = size_limit_missed
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    let mut result = serde_json::json!({
        "written": written,
        "skipped": skipped,
        "replaced": replaced,
        "keptBoth": kept_both,
        "keptOriginal": kept_original,
        "failed": failed,
        "seconds": started.elapsed().as_secs_f64(),
        "metadata": if keep_metadata { "source EXIF and session catalog metadata" } else if keep_creator_contact { "creator, copyright, creator contact details and usage terms" } else { "creator, copyright and usage terms" },
    });
    // Only an export that missed a --max-file-kb names them: other runs
    // print what they always did.
    if !size_limit_missed.is_empty() {
        result["sizeLimitMissed"] = size_limit_missed.into();
    }
    print(result)?;
    anyhow::ensure!(
        failed.is_empty(),
        "{} of {} exports failed",
        failed.len(),
        jobs.len()
    );
    Ok(())
}
/// The machine's physical memory in bytes, as the app reads it.
fn total_memory() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.total_memory()
}

/// Free physical memory in bytes, as the app's export reads it.
fn available_memory() -> u64 {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    system.available_memory()
}

fn proof_sheet(args: ProofSheetArgs) -> Result<()> {
    use rawtonic_export::proof_sheet::{
        CellImage, PageValues, ProofSheet, caption, cell_settings, file_stem, page_text,
        write_files, write_sheet,
    };
    use rawtonic_export::sidecars::{parse_virtual_path, sources_with_copies, virtual_path};
    let started = Instant::now();
    let sheet: ProofSheet = serde_json::from_str(
        &std::fs::read_to_string(&args.sheet)
            .with_context(|| format!("Read {}", args.sheet.display()))?,
    )
    .context("--sheet: proof sheet JSON expected")?;
    let mut photos = Vec::new();
    for source in &args.sources {
        if source.is_dir() {
            photos.extend(
                sources_with_copies(source)
                    .into_iter()
                    .flat_map(|(image, copies)| {
                        copies
                            .into_iter()
                            .map(move |copy| virtual_path(&image, copy.as_deref()))
                    }),
            );
        } else {
            anyhow::ensure!(source.is_file(), "Source not found: {}", source.display());
            photos.push(source.to_string_lossy().into_owned());
        }
    }
    anyhow::ensure!(!photos.is_empty(), "No photos for the proof sheet");
    anyhow::ensure!(
        args.output.is_dir(),
        "Output folder not found: {}",
        args.output.display()
    );
    let layout = sheet.layout(photos.len()).map_err(anyhow::Error::msg)?;
    let settings = rawtonic_export::export_processing::ExportSettings {
        // `{job}` in captions, as in the app.
        job_name: Some(args.job.clone()),
        ..cell_settings(&layout, None, args.output_sharpening)
    };
    let recipes: Vec<Option<serde_json::Value>> = photos
        .iter()
        .map(|photo| Some(editor_recipe(&parse_virtual_path(photo).1)))
        .collect();
    let exporter =
        rawtonic_export::file_export::FileExporter::new(&lensfun_folder(args.lensfun_db)?)
            .map_err(anyhow::Error::msg)?;
    let sources: Vec<PathBuf> = photos
        .iter()
        .map(|photo| parse_virtual_path(photo).0)
        .collect();
    let exporter = with_recipe_models(exporter, args.models, &recipes, &sources);
    let cells_dir = tempfile::tempdir()?;
    let total = photos.len();
    let mut cells = Vec::with_capacity(total);
    let mut captions = Vec::with_capacity(total);
    let mut failed = Vec::new();
    let mut generated = false;
    for (index, (photo, recipe)) in photos.iter().zip(recipes).enumerate() {
        let (source, sidecar) = parse_virtual_path(photo);
        if let Err(error) = rawtonic_export::file_export::require_local_tiff(&source) {
            failed.push(serde_json::json!({"source": photo, "error": error}));
            cells.push(None);
            captions.push(String::new());
            continue;
        }
        let metadata = photo_metadata(photo, &source, &sidecar);
        let date = rawtonic_export::exif_processing::capture_date_with(&source, &|| None);
        let (sequence, last) = settings.filename_counter(index, total);
        let capture = rawtonic_ingest::naming_capture(&source, metadata.as_ref());
        let naming = settings.naming_context(&source, sequence, last, &date, "jpg", Some(&capture));
        captions.push(
            sheet
                .caption
                .as_deref()
                .map(|t| caption(t, &naming))
                .unwrap_or_default(),
        );
        generated |= recipe
            .as_ref()
            .is_some_and(rawtonic_engine::image_loader::has_generated_areas)
            || generated_provenance(&source);
        let library = library_of(&source);
        if let Some(resource) = recipe
            .as_ref()
            .and_then(|r| unresolved_resource(r, library.as_deref()))
        {
            failed.push(serde_json::json!({"source": photo, "error": format!(
                "The recipe names the library resource {resource}, but the photo is in no session or catalog"
            )}));
            cells.push(None);
            continue;
        }
        let output = cells_dir.path().join(format!("cell-{index}.jpg"));
        let result = exporter.export(&rawtonic_export::file_export::FileExport {
            source: &source,
            output: &output,
            format: "jpg",
            recipe,
            settings: settings.clone(),
            base: rawtonic_engine::image_loader::BaseImageOptions::default(),
            catalog: metadata,
            capture: corrected_capture(&source),
            source_generated: generated_provenance(&source),
            library: library.as_deref(),
            publish: rawtonic_export::output::Publish::New,
        });
        match result.and_then(|()| std::fs::read(&output).map_err(|e| e.to_string())) {
            Ok(jpeg) => cells.push(Some(CellImage { jpeg })),
            Err(error) => {
                failed.push(serde_json::json!({"source": photo, "error": error}));
                cells.push(None);
            }
        }
    }
    // As the app: a sheet is written only with every photo on it.
    if !failed.is_empty() {
        print(serde_json::json!({ "written": [], "failed": failed }))?;
        anyhow::bail!(
            "{} of {} photos failed; no proof sheet written",
            failed.len(),
            total
        );
    }
    let values = PageValues {
        job: args.job,
        date: args.date.unwrap_or_else(|| {
            rawtonic_runtime::local_time::now()
                .strftime("%Y-%m-%d")
                .to_string()
        }),
        count: total,
    };
    let written =
        write_sheet(&sheet, &cells, &captions, &values, generated).map_err(anyhow::Error::msg)?;
    let stem = file_stem(&page_text(&args.name, &values, 1, layout.pages.len()));
    let files = write_files(&args.output, &stem, &written).map_err(anyhow::Error::msg)?;
    print(serde_json::json!({
        "written": files,
        "pages": layout.pages.len(),
        "photos": total,
        "cellPixels": [layout.box_px.0, layout.box_px.1],
        "seconds": started.elapsed().as_secs_f64(),
    }))
}

/// `exporter` with what `recipes` need of the app's models: AI Denoise
/// and the Sky and Subject selections of never-opened photos.
fn with_recipe_models(
    mut exporter: rawtonic_export::file_export::FileExporter,
    models: Option<PathBuf>,
    recipes: &[Option<serde_json::Value>],
    sources: &[PathBuf],
) -> rawtonic_export::file_export::FileExporter {
    // The model each photo's AI Denoise runs (X-Trans photos read from
    // their tags where the version denoises them).
    let ai_models: std::collections::BTreeSet<usize> = recipes
        .iter()
        .zip(sources)
        .filter_map(|(recipe, source)| {
            let ai = rawtonic_engine::image_loader::ai_denoise_settings(recipe.as_ref()?)?;
            Some(ai.models_for_source(source))
        })
        .flatten()
        .collect();
    // A model that cannot be loaded fails only the photos that need it.
    for model in ai_models {
        match ai_denoiser(models.clone(), model) {
            Ok(denoiser) => exporter = exporter.with_mosaic_denoiser(model, denoiser),
            Err(e) => cli_eprintln!("AI Denoise model {model} unavailable: {e:#}"),
        }
    }
    // Automatic selections a never-opened photo holds without their bitmap
    // are computed with the app's models, as the app's export does; without
    // the models those photos fail. So are those of a camera TIFF's edit made
    // on its thumbnail, once moved to the full frame (`recipe_frame`).
    if recipes.iter().zip(sources).any(|(recipe, source)| {
        recipe.as_ref().is_some_and(|recipe| {
            let mut recipe = recipe.clone();
            rawtonic_engine::recipe_frame::conform(&mut recipe, source);
            !rawtonic_engine::ai_selections::undetected(&recipe).is_empty()
        })
    }) {
        match models_dir(models.clone()) {
            Ok(dir) => {
                ONNX_RUNTIME_USED.store(true, std::sync::atomic::Ordering::Relaxed);
                exporter = exporter
                    .with_selection_detector(std::sync::Arc::new(SelectionModels::new(dir)));
            }
            Err(e) => cli_eprintln!("AI selections unavailable: {e:#}"),
        }
    }
    exporter
}

/// The app's Lensfun folder: `explicit`, `$RAWTONIC_LENSFUN_DB`, else
/// `lensfun_db` beside this binary or in a macOS app bundle's Resources.
fn lensfun_folder(explicit: Option<PathBuf>) -> Result<PathBuf> {
    explicit
        .or_else(|| std::env::var_os("RAWTONIC_LENSFUN_DB").map(PathBuf::from))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            [dir.join("lensfun_db"), dir.join("../Resources/lensfun_db")]
                .into_iter()
                .find(|candidate| candidate.is_dir())
        })
        .context("Lensfun database not found; pass --lensfun-db (the app's lensfun_db folder)")
}

/// The editor sidecar of an output written at `path` (`<file>.rrdata`),
/// built from the path itself so a non-UTF-8 file name is kept as it is.
fn sidecar_path_of(path: &std::path::Path) -> PathBuf {
    path.with_file_name(rawtonic_metadata::photo_ref::sidecar_name(path, None))
}

/// A photo's recipe as the app's export reads it: the session database's
/// editor state for a primary photo in a session, else its `.rrdata`
/// sidecar (a virtual copy always reads its sidecar). Read-only.
fn editor_recipe(sidecar: &std::path::Path) -> serde_json::Value {
    use rawtonic_engine::image_processing::ImageMetadata;
    let from_session = || -> Option<serde_json::Value> {
        // Only a primary photo's recipe lives in the session: a virtual
        // copy's sidecar (`<file>.<copy id>.rrdata`) is always read itself.
        let name = sidecar.file_name()?.to_str()?;
        let (_, None) = rawtonic_metadata::photo_ref::sidecar_name_parts(name)? else {
            return None;
        };
        let source = rawtonic_metadata::photo_ref::source_path_of_sidecar(sidecar)?;
        if !source.is_file() {
            return None;
        }
        let lib = Library::open(&rawtonic_catalog::session_for(&source)?).ok()?;
        let asset = lib.asset_at(&source).ok()??;
        lib.editor_state(&asset.id).ok().flatten()
    };
    if let Some(state) = from_session()
        && let Ok(meta) = serde_json::from_value::<ImageMetadata>(state)
    {
        return meta.adjustments;
    }
    rawtonic_export::sidecars::read_sidecar_file(sidecar, library_of(sidecar).as_deref())
        .0
        .adjustments
}
/// The library (session or catalog) folder holding `path`, whose files a
/// recipe of the photo names.
fn library_of(path: &std::path::Path) -> Option<std::path::PathBuf> {
    rawtonic_catalog::layout::LibraryLayout::holding(path)
        .map(rawtonic_catalog::layout::LibraryLayout::into_directory)
}
/// Whether the file at `path` is a photograph by now (an asset of its
/// session, or edited as a photo: an editor sidecar beside it): an export
/// there is never replaced.
fn is_library_photo(path: &std::path::Path) -> bool {
    if rawtonic_export::exif_processing::get_primary_sidecar_path(path).exists() {
        return true;
    }
    rawtonic_catalog::session_for(path).is_some_and(|session| match Library::open(&session) {
        Ok(lib) => !matches!(lib.asset_at(path), Ok(None)),
        Err(_) => true,
    })
}
/// The metadata an export of `photo` embeds, as the app's: its session's
/// for the source, with a virtual copy's own rating, label and keywords
/// from its sidecar.
fn photo_metadata(
    photo: &str,
    source: &std::path::Path,
    sidecar: &std::path::Path,
) -> Option<rawtonic_metadata::Metadata> {
    let metadata = catalog_metadata(source)?;
    if !rawtonic_metadata::photo_ref::is_copy(photo) {
        return Some(metadata);
    }
    // Only its rating and keywords are read: no library resources to resolve.
    let copy = rawtonic_export::sidecars::read_sidecar_file(sidecar, None).0;
    Some(rawtonic_catalog::copy_metadata(
        metadata,
        copy.rating,
        copy.tags.as_deref(),
    ))
}
/// The capture time a session corrected for a source (Shift Capture Time),
/// as the app's export writes it.
fn corrected_capture(
    source: &std::path::Path,
) -> Option<rawtonic_export::exif_processing::CaptureFields> {
    let Some(session) = rawtonic_catalog::session_for(source) else {
        // A catalog's photograph anywhere: its `.rrdata` carries the correction.
        return rawtonic_export::file_export::sidecar_capture(source);
    };
    let lib = Library::open(&session).ok()?;
    let asset = lib.asset_at(source).ok()??;
    Some(lib.capture_override(&asset.id).ok()??.exif_fields())
}
/// Whether the session of `source` marked it as a file Rawtonic rendered
/// with generated areas (an Edit With render), as the app reads it for its
/// exports. A catalog's photograph anywhere is not found from its path.
fn generated_provenance(source: &std::path::Path) -> bool {
    rawtonic_catalog::session_for(source)
        .and_then(|session| Library::open(&session).ok())
        .and_then(|lib| {
            let asset = lib.asset_at(source).ok()??;
            lib.has_generated_provenance(&asset.id).ok()
        })
        .unwrap_or(false)
}
/// The catalog metadata of a source inside a session, as the app writes it.
fn catalog_metadata(source: &std::path::Path) -> Option<rawtonic_metadata::Metadata> {
    let lib = Library::open(&rawtonic_catalog::session_for(source)?).ok()?;
    let asset = lib.asset_at(source).ok()??;
    lib.metadata(&asset.id).ok()
}
/// A file a recipe names inside a library (a camera profile by its path in
/// the session, `profiles/…dcp`; a `rawtonic-resource:` LUT, profile or
/// mask) that cannot be found because the photo belongs to no library
/// (`library` None): exporting without it would silently change the photo.
/// Other relative paths (a LUT beside the recipe) are the caller's own.
fn unresolved_resource(
    recipe: &serde_json::Value,
    library: Option<&std::path::Path>,
) -> Option<String> {
    if library.is_some() {
        return None;
    }
    let mut found = None;
    rawtonic_metadata::resources::visit(&mut recipe.clone(), &mut |path| {
        if found.is_none() && path.starts_with(rawtonic_metadata::resources::PREFIX) {
            found = Some(path.clone());
        }
    });
    // `cameraProfile` names a path in the photo's library unless absolute
    // (camera_profile::resolve).
    fn profiles(value: &serde_json::Value, found: &mut Option<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    match (key.as_str(), child) {
                        ("cameraProfile", serde_json::Value::String(name))
                            if found.is_none()
                                && !name.is_empty()
                                && !std::path::Path::new(name).is_absolute() =>
                        {
                            *found = Some(name.clone())
                        }
                        _ => profiles(child, found),
                    }
                }
            }
            serde_json::Value::Array(values) => values.iter().for_each(|v| profiles(v, found)),
            _ => {}
        }
    }
    if found.is_none() {
        profiles(recipe, &mut found);
    }
    found
}
/// Set once ONNX Runtime ran (face models): its environment aborts when
/// static destructors tear it down at exit, so the process ends without
/// them.
static ONNX_RUNTIME_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stack of the thread that runs the command: the render stack of the
/// app's export and preview threads (`RENDER_STACK_SIZE`). Before 2026-11-06
/// the app's export lanes had Rust's 2 MiB default and overflowed it on AMD's
/// Linux driver while the CLI, on this thread, never did (gap G74). Windows
/// gives the main thread only 1 MiB, so the command never runs on it.
const COMMAND_STACK_SIZE: usize = rawtonic_runtime::RENDER_STACK_SIZE;

fn main() {
    // Before any thread starts (ONNX Runtime's usage uploader, PRIVACY.md).
    rawtonic_onnx::opt_out_of_runtime_telemetry();
    let result = std::thread::Builder::new()
        .name("rawtonic-command".into())
        .stack_size(COMMAND_STACK_SIZE)
        .spawn(run)
        .expect("start the command thread")
        .join();
    let code = match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            cli_eprintln!("Rawtonic: {e:#}");
            1
        }
        // The panic message has already been printed by the panic hook.
        Err(_) => 101,
    };
    // Linux GPU driver exit handlers can also race background resources.
    // Command-owned state has already been dropped when run() returns.
    if cfg!(target_os = "linux") || ONNX_RUNTIME_USED.load(std::sync::atomic::Ordering::Relaxed) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        #[cfg(unix)]
        unsafe {
            libc::_exit(code)
        }
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI reads the app's GPU verdicts, so it loads models on the
    /// provider the app uses (and keeps devices accepted before).
    #[test]
    fn the_cli_uses_the_apps_gpu_check_verdicts() {
        let data = Path::new("/data/cc.rawtonic.editor");
        assert_eq!(
            gpu_checks_file(&data.join("models")),
            Some(data.join("ai-gpu-checks.tsv"))
        );
        let models = models_dir(Some(data.join("models"))).unwrap();
        assert_eq!(models, data.join("models"));
        assert_eq!(
            rawtonic_onnx::settings().checks_file,
            Some(data.join("ai-gpu-checks.tsv"))
        );
    }

    /// The build script's Windows main-thread stack is the render stack.
    #[test]
    fn the_main_thread_stack_is_the_render_stack() {
        assert_eq!(
            env!("RAWTONIC_MAIN_STACK_SIZE").parse::<usize>().unwrap(),
            rawtonic_runtime::RENDER_STACK_SIZE
        );
    }

    #[test]
    fn library_resources_of_photos_outside_any_library_are_named_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        std::fs::create_dir_all(session.join("Capture")).unwrap();
        // Its manifest makes it a library, also while its database is missing.
        std::fs::write(
            session.join(rawtonic_catalog::layout::SESSION_MANIFEST),
            "{}",
        )
        .unwrap();
        let loose = dir.path().join("a.NEF");
        let profile = serde_json::json!({"cameraProfile": "profiles/x.dcp"});
        let lut = serde_json::json!({"lutPath": "rawtonic-resource:resources/l.cube"});
        let absolute = serde_json::json!({"cameraProfile": dir.path().join("x.dcp")});
        // A LUT beside the recipe, as `--adjustments` has always read it.
        let own_lut = serde_json::json!({"lutPath": "look.cube"});
        let none = library_of(&loose);
        assert_eq!(none, None);
        assert_eq!(
            unresolved_resource(&profile, none.as_deref()).as_deref(),
            Some("profiles/x.dcp")
        );
        assert!(unresolved_resource(&lut, None).is_some());
        assert_eq!(unresolved_resource(&absolute, None), None);
        assert_eq!(unresolved_resource(&own_lut, None), None);
        assert_eq!(unresolved_resource(&serde_json::json!({}), None), None);
        let inside = library_of(&session.join("Capture/a.NEF"));
        assert_eq!(inside.as_deref(), Some(session.as_path()));
        assert_eq!(unresolved_resource(&profile, inside.as_deref()), None);
    }

    /// The sidecar of `merge`'s output is the output's path with
    /// `.rrdata` pushed onto its file name: a name that is not UTF-8 stays
    /// byte for byte (a `PhotoRef` would make it lossy).
    #[cfg(unix)]
    #[test]
    fn a_merges_sidecar_keeps_a_non_utf8_output_name() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;
        let output = Path::new("/out").join(OsStr::from_bytes(b"pano\xfe.dng"));
        let sidecar = sidecar_path_of(&output);
        assert_eq!(sidecar.parent(), Some(Path::new("/out")));
        assert_eq!(
            sidecar.file_name().unwrap().as_encoded_bytes(),
            b"pano\xfe.dng.rrdata"
        );
        // The UTF-8 names are the ones the written-out form gave.
        assert_eq!(
            sidecar_path_of(Path::new("/out/pano.dng")),
            PathBuf::from("/out/pano.dng.rrdata")
        );
    }
}
