// The app's allocator, so exports free memory as the app does (the export
// crate returns freed pages through it, `allocator::release_freed_memory`).
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rawtonic_catalog::{Library, Mode};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};

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
        #[arg(value_parser=clap::value_parser!(u8).range(0..=5))]
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
    /// app's export renders them with default preferences. Never overwrites.
    Export(ExportArgs),
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
struct ExportArgs {
    /// A photo, or a folder exported with its subfolders.
    source: PathBuf,
    /// For a photo the output file (its extension chooses the format); for a
    /// folder the output folder.
    #[arg(long)]
    output: PathBuf,
    /// File format and extension for folder exports (jpeg, jpg, png, tiff, tif, webp, jxl, avif).
    #[arg(long, default_value = "jpeg")]
    format: String,
    /// The editor's adjustments JSON (as for the app's `export --adjustments`).
    #[arg(long)]
    adjustments: Option<PathBuf>,
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,
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
    /// The app's AI model folder, for recipes with AI Denoise; default:
    /// $RAWTONIC_MODELS, then the installed app's (cc.rawtonic.editor).
    /// Denoised mosaics are cached beside it in `ai-denoise-cache`.
    #[arg(long)]
    models: Option<PathBuf>,
    /// Output sharpening as AMOUNT:RADIUS[:THRESHOLD] (percent, output
    /// pixels, levels of 255), after any resize.
    #[arg(long, value_parser = parse_output_sharpening)]
    output_sharpening: Option<rawtonic_export::output_sharpening::OutputSharpening>,
    /// Face retouch for every photo: the sliders for all faces as JSON
    /// (`{"blemishes":60,"evenSkin":50}`, see docs/RECIPE_SCHEMA.md). The
    /// faces are found with the face models in --models (ONNX Runtime from
    /// `ORT_DYLIB_PATH`) and replace the recipe's own face retouch.
    #[arg(long)]
    face_retouch: Option<String>,
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
        &rawtonic_engine::image_loader::BaseImageOptions::default().for_recipe(&recipe, is_raw),
        None,
    )?;
    // The geometry as the export renders it (the camera's lens correction
    // filled in), so the faces' frame matches the render's.
    rawtonic_export::exif_processing::prepare_recipe_lens(
        &mut recipe,
        std::path::Path::new(source),
    )
    .map_err(anyhow::Error::msg)?;
    let warped = rawtonic_engine::mask_generation::prepare_shared_warped_mask_image(
        &std::sync::Arc::new(base),
        is_raw,
        &recipe,
    )
    .map_err(anyhow::Error::msg)?;
    let mut uncropped = recipe.clone();
    uncropped["crop"] = serde_json::Value::Null;
    let (framed, _) = rawtonic_engine::adjustment_utils::apply_spatial_transformations(
        warped.as_ref(),
        &uncropped,
    );
    let image = rawtonic_faces::recipe::display_rgb8(&framed, is_raw);
    let analysis = rawtonic_faces::analyze(&image, models)?;
    let faces =
        rawtonic_faces::recipe::retouch_faces(&image, &analysis).map_err(anyhow::Error::msg)?;
    eprintln!("{source}: {} face(s)", faces.len());
    recipe["faceRetouch"] = serde_json::json!({
        "settings": settings,
        "faces": faces,
        "frame": rawtonic_engine::face_retouch::frame_key(&recipe),
        "geometry": "",
    });
    Ok(recipe)
}

/// The app's model folder: --models, $RAWTONIC_MODELS, then the installed
/// app's (cc.rawtonic.editor).
fn models_dir(models: Option<PathBuf>) -> Result<PathBuf> {
    models
        .or_else(|| std::env::var_os("RAWTONIC_MODELS").map(PathBuf::from))
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            let base = if cfg!(target_os = "macos") {
                home.join("Library/Application Support")
            } else {
                std::env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local/share"))
            };
            Some(base.join("cc.rawtonic.editor/models"))
        })
        .context("no model folder; pass --models")
}

/// The MatchFilter denoiser for AI Denoise `model`
/// (`AiDenoiseSettings::model`) from the app's model folder (Core ML on
/// macOS: 1 the mixed-precision package; ONNX Runtime elsewhere, which needs
/// `ORT_DYLIB_PATH`, with one model for every version).
fn ai_denoiser(
    models: Option<PathBuf>,
    model: usize,
) -> Result<std::sync::Arc<dyn rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser>> {
    let models = models_dir(models)?;
    let cache = models.parent().map(|dir| dir.join("ai-denoise-cache"));
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

pub(crate) fn print(value: impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
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
                        eprintln!("{}: {e:#}", path.display());
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
                        eprintln!("{}: {e:#}", source.display());
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
        Command::AdoptXmp { library, asset } => print(Library::open(&library)?.adopt_xmp(&asset)?),
        Command::Sync { library } => {
            let l = Library::open(&library)?;
            let mut failed = 0;
            for a in l.assets(None)? {
                if let Err(e) = l.sync_xmp(&a.id) {
                    failed += 1;
                    eprintln!("{}: {e:#}", a.rel_path);
                }
            }
            print(l.summary()?)?;
            anyhow::ensure!(failed == 0, "{failed} sidecars need conflict resolution");
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
        eprintln!("{name}");
        let r = bench_file(&path, provider, args.runs, &dirs, &picture)?;
        reports.push(serde_json::json!({
            "file": r.file,
            "model": r.kind.name(),
            "cpu": timing(&r.cpu),
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
    let progress = |msg: &str| eprintln!("{msg}");
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
    let mut sidecar = args.output.clone().into_os_string();
    sidecar.push(".rrdata");
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
    for _ in 0..repeats {
        let now = Instant::now();
        let image = rawtonic_rawdecode::raw_processing::develop_raw_image(
            &bytes,
            fast,
            2.5,
            "auto".into(),
            None,
        )?;
        timings.push(now.elapsed().as_secs_f64() * 1000.0);
        size = (image.width(), image.height());
    }
    timings.sort_by(f64::total_cmp);
    print(serde_json::json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "width": size.0,
        "height": size.1,
        "repeats": repeats,
        "fast_demosaic": fast,
        "decode_ms": {"median": timings[timings.len() / 2], "min": timings[0], "max": timings[timings.len() - 1]},
        "measures": "rawdecode only: file bytes in memory to oriented linear RGB (CPU demosaic, system allocator); excludes file read, GPU develop, encoding and the app's mimalloc tuning",
    }))
}
fn export(args: ExportArgs) -> Result<()> {
    use rawtonic_export::export_processing::{
        ExportSettings, TiffBitDepth, relative_export_dir_for_preserved_folders,
    };
    let ExportArgs {
        source,
        output,
        adjustments,
        format,
        quality,
        tiff_bit_depth,
        keep_metadata,
        strip_gps,
        keep_creator_contact,
        color_space,
        lensfun_db,
        models,
        output_sharpening,
        face_retouch,
    } = args;
    let lensfun_db = lensfun_db
        .or_else(|| std::env::var_os("RAWTONIC_LENSFUN_DB").map(PathBuf::from))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            [dir.join("lensfun_db"), dir.join("../Resources/lensfun_db")]
                .into_iter()
                .find(|candidate| candidate.is_dir())
        })
        .context("Lensfun database not found; pass --lensfun-db (the app's lensfun_db folder)")?;
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
        sequence_start: None,
        watermark: None,
        export_masks: false,
        preserve_folders: true,
        destination_type: None,
        subfolder: None,
        output_sharpening,
    };

    // (photo path, output) pairs. A folder exports every supported image below
    // it and each virtual copy, in the app's library order, named like the
    // app's folder export ("<name>_edited.<format>", "_VC01"… for copies,
    // source subfolders kept, "_1", "_2"… instead of overwriting).
    use rawtonic_export::sidecars::{
        export_stem, parse_virtual_path, plan_export_items, sources_with_copies, virtual_path,
    };
    let jobs: Vec<(String, PathBuf)> = if source.is_dir() {
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
        let mut planned = std::collections::HashSet::new();
        plan_export_items(photos)
            .into_iter()
            .map(
                |(index, photo, appearance, explicit_vc)| -> Result<(String, PathBuf)> {
                    let (path, _) = parse_virtual_path(&photo);
                    let capture =
                        rawtonic_export::exif_processing::capture_date_with(&path, &|| None);
                    let stem = export_stem(
                        "{original_filename}_edited",
                        &path,
                        index + 1,
                        total,
                        &capture,
                        appearance,
                        explicit_vc,
                    );
                    let dir = relative_export_dir_for_preserved_folders(&path, &origin)
                        .map_or_else(|| output.clone(), |rel| output.join(rel));
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("Create {}", dir.display()))?;
                    let mut target = dir.join(format!("{stem}.{format}"));
                    let mut counter = 1;
                    while target.exists() || planned.contains(&target) {
                        target = dir.join(format!("{stem}_{counter}.{format}"));
                        counter += 1;
                    }
                    planned.insert(target.clone());
                    Ok((photo, target))
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
            !output.exists(),
            "Output exists; Rawtonic never overwrites: {}",
            output.display()
        );
        vec![(source.to_string_lossy().into_owned(), output.clone())]
    };

    let started = Instant::now();
    // First: it loads the Lensfun database, which preparing the recipes
    // (face retouch: lens defaults and lens parameters of recipes saved
    // while the photo could not be read) already reads.
    let mut exporter =
        rawtonic_export::file_export::FileExporter::new(&lensfun_db).map_err(anyhow::Error::msg)?;
    // --adjustments overrides every photo; otherwise each photo's own
    // recipe, as the app's export reads it.
    let recipes: Vec<Option<serde_json::Value>> = jobs
        .iter()
        .map(|(photo, _)| match &recipe {
            Some(recipe) => Some(recipe.clone()),
            None => Some(editor_recipe(&parse_virtual_path(photo).1)),
        })
        .collect();
    let recipes = match &face_retouch {
        Some(settings) => {
            let settings: serde_json::Value =
                serde_json::from_str(settings).context("--face-retouch: JSON expected")?;
            ONNX_RUNTIME_USED.store(true, std::sync::atomic::Ordering::Relaxed);
            let face_models = rawtonic_faces::FaceModels::load(&models_dir(models.clone())?)?;
            let recipes = jobs
                .iter()
                .zip(recipes)
                .map(|((photo, _), recipe)| {
                    let source = parse_virtual_path(photo).0;
                    with_face_retouch(
                        &source.to_string_lossy(),
                        recipe.unwrap_or_default(),
                        &settings,
                        &face_models,
                    )
                    .map(Some)
                })
                .collect::<Result<Vec<_>>>()?;
            // ONNX Runtime aborts when its environment is torn down at exit.
            std::mem::forget(face_models);
            recipes
        }
        None => recipes,
    };
    let ai_models: std::collections::BTreeSet<usize> = recipes
        .iter()
        .flatten()
        .filter_map(|r| rawtonic_engine::image_loader::ai_denoise_settings(r).map(|ai| ai.model()))
        .collect();
    // A model that cannot be loaded fails only the photos that need it.
    for model in ai_models {
        match ai_denoiser(models.clone(), model) {
            Ok(denoiser) => exporter = exporter.with_mosaic_denoiser(model, denoiser),
            Err(e) => eprintln!("AI Denoise model {model} unavailable: {e:#}"),
        }
    }
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for ((photo, output), recipe) in jobs.iter().zip(recipes) {
        let (source, sidecar) = parse_virtual_path(photo);
        if let Some(resource) = recipe
            .as_ref()
            .and_then(|r| unresolved_resource(r, &source))
        {
            failed.push(serde_json::json!({"source": photo, "error": format!(
                "The recipe names the library resource {resource}, but the photo is in no session or catalog"
            )}));
            continue;
        }
        let result = exporter.export(&rawtonic_export::file_export::FileExport {
            source: &source,
            output,
            format: &format,
            recipe,
            settings: settings.clone(),
            base: rawtonic_engine::image_loader::BaseImageOptions::default(),
            catalog: photo_metadata(photo, &source, &sidecar),
        });
        match result {
            Ok(()) => written.push(output.clone()),
            Err(error) => failed.push(serde_json::json!({"source": photo, "error": error})),
        }
    }
    print(serde_json::json!({
        "written": written,
        "failed": failed,
        "seconds": started.elapsed().as_secs_f64(),
        "metadata": if keep_metadata { "source EXIF and session catalog metadata" } else if keep_creator_contact { "creator, copyright, creator contact details and usage terms" } else { "creator, copyright and usage terms" },
    }))?;
    anyhow::ensure!(
        failed.is_empty(),
        "{} of {} exports failed",
        failed.len(),
        jobs.len()
    );
    Ok(())
}
/// A photo's recipe as the app's export reads it: the session database's
/// editor state for a primary photo in a session, else its `.rrdata`
/// sidecar (a virtual copy always reads its sidecar). Read-only.
fn editor_recipe(sidecar: &std::path::Path) -> serde_json::Value {
    use rawtonic_engine::image_processing::ImageMetadata;
    let from_session = || -> Option<serde_json::Value> {
        let name = sidecar.file_name()?.to_str()?.strip_suffix(".rrdata")?;
        let source = sidecar.with_file_name(name);
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
    rawtonic_export::sidecars::read_sidecar_file(sidecar)
        .0
        .adjustments
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
    if !photo.contains("?vc=") {
        return Some(metadata);
    }
    let copy = rawtonic_export::sidecars::read_sidecar_file(sidecar).0;
    Some(rawtonic_catalog::copy_metadata(
        metadata,
        copy.rating,
        copy.tags.as_deref(),
    ))
}
/// The catalog metadata of a source inside a session, as the app writes it.
fn catalog_metadata(source: &std::path::Path) -> Option<rawtonic_metadata::Metadata> {
    let lib = Library::open(&rawtonic_catalog::session_for(source)?).ok()?;
    let asset = lib.asset_at(source).ok()??;
    lib.metadata(&asset.id).ok()
}
/// A file a recipe names inside a library (a camera profile by its path in
/// the session, `profiles/…dcp`; a `rawtonic-resource:` LUT, profile or
/// mask) that cannot be found because `source` belongs to no library:
/// exporting without it would silently change the photo. Other relative
/// paths (a LUT beside the recipe) are the caller's own.
fn unresolved_resource(recipe: &serde_json::Value, source: &std::path::Path) -> Option<String> {
    if rawtonic_engine::session_layout::library_root(source).is_some() {
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

/// Stack of the thread that runs the command. The export render keeps
/// several large frames on the stack at once (well over 1 MiB in release
/// builds), more than Windows gives the main thread (1 MiB); this is the
/// main-thread default of Linux and macOS and the app's worker stack.
const COMMAND_STACK_SIZE: usize = 8 * 1024 * 1024;

fn main() {
    let result = std::thread::Builder::new()
        .name("rawtonic-command".into())
        .stack_size(COMMAND_STACK_SIZE)
        .spawn(run)
        .expect("start the command thread")
        .join();
    let code = match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            eprintln!("Rawtonic: {e:#}");
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

    #[test]
    fn library_resources_of_photos_outside_any_library_are_named_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        std::fs::create_dir_all(session.join("Capture")).unwrap();
        std::fs::write(session.join("session.json"), "{}").unwrap();
        std::fs::write(session.join("session.sqlite"), "").unwrap();
        let loose = dir.path().join("a.NEF");
        let profile = serde_json::json!({"cameraProfile": "profiles/x.dcp"});
        let lut = serde_json::json!({"lutPath": "rawtonic-resource:resources/l.cube"});
        let absolute = serde_json::json!({"cameraProfile": dir.path().join("x.dcp")});
        // A LUT beside the recipe, as `--adjustments` has always read it.
        let own_lut = serde_json::json!({"lutPath": "look.cube"});
        assert_eq!(
            unresolved_resource(&profile, &loose).as_deref(),
            Some("profiles/x.dcp")
        );
        assert!(unresolved_resource(&lut, &loose).is_some());
        assert_eq!(unresolved_resource(&absolute, &loose), None);
        assert_eq!(unresolved_resource(&own_lut, &loose), None);
        assert_eq!(unresolved_resource(&serde_json::json!({}), &loose), None);
        assert_eq!(
            unresolved_resource(&profile, &session.join("Capture/a.NEF")),
            None
        );
        // Kept by the session outside its folder: resolved.
        rawtonic_engine::session_layout::set_outside_photos(
            &session,
            std::collections::HashSet::from([loose.clone()]),
        );
        assert_eq!(unresolved_resource(&profile, &loose), None);
        rawtonic_engine::session_layout::set_outside_photos(&session, Default::default());
    }
}
