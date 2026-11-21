//! A recipe group's TIFF written while the group renders
//! (`save_group_rendering`, notes: benchmark-results/mem14-2026-11-17 and
//! mem15-2026-11-17), end to end on a prepared photo: the member the group
//! renders for streams its file in its own colour space and depth, also when
//! the group's first recipe was skipped, and a file that cannot be created
//! fails alone while the other recipes get their renders. Every file has the
//! bytes of the same recipe saved from its render held whole. Needs a GPU:
//!
//! cargo test -p rawtonic-export --test group_stream -- --ignored

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{
    EncodedExport, ExportOutput, ExportSettings, ExportSource, GroupRender, PreparedExport,
    RenderEnv, TiffBitDepth, prepare_export_render, render_output_precision,
    render_prepared_exports, save_group_rendering, save_rendered_export,
};
use rawtonic_export::output::write_new_export;
use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessorSlot};

const SOURCE: &str = "group-stream-synthetic.tif";

/// A recipe of the group: its format and settings.
struct Recipe {
    format: &'static str,
    settings: ExportSettings,
}

fn recipe(format: &'static str, space: OutputColorSpace, bit_depth: TiffBitDepth) -> Recipe {
    let settings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 90, "keepMetadata": true, "stripGps": false,
        "colorSpace": space, "tiffBitDepth": bit_depth,
    }))
    .unwrap();
    Recipe { format, settings }
}

fn write(path: &Path, _extension: &str, encoded: EncodedExport<'_>) -> Result<(), String> {
    write_new_export(path, encoded)
}

fn no_exif() -> Option<rawtonic_export::export_processing::ExifMap> {
    None
}

fn no_catalog() -> Option<rawtonic_metadata::Metadata> {
    None
}

fn output<'a>(path: &'a Path, format: &'a str) -> ExportOutput<'a> {
    ExportOutput {
        path,
        format,
        exif_sidecar: &no_exif,
        catalog: &no_catalog,
        enlarger: None,
        write: &write,
        provenance: None,
    }
}

fn going() -> Result<(), String> {
    Ok(())
}

/// A smooth photo whose rows span two render bands.
fn photo() -> Arc<DynamicImage> {
    let (width, height) = (900u32, 1300u32);
    let image = Rgb32FImage::from_fn(width, height, |x, y| {
        let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
        image::Rgb([0.05 + 0.8 * u, 0.1 + 0.7 * v, 0.6 * (1.0 - u) * v + 0.02])
    });
    Arc::new(DynamicImage::ImageRgb32F(image))
}

struct Fixture {
    prepared: PreparedExport,
    recipe: serde_json::Value,
}

fn outputs(
    recipes: &[Recipe],
) -> Vec<(
    rawtonic_gpu::gpu_processing::RenderOutputPrecision,
    OutputColorSpace,
)> {
    recipes
        .iter()
        .map(|r| {
            (
                render_output_precision(r.format, &r.settings),
                r.settings.color_space,
            )
        })
        .collect()
}

/// Each recipe's file saved from the group's renders held whole, in `dir`.
fn reference(
    env: &RenderEnv,
    fixture: &Fixture,
    recipes: &[Recipe],
    dir: &Path,
) -> (Vec<Vec<u8>>, Vec<DynamicImage>) {
    let images = render_prepared_exports(
        &fixture.prepared,
        env,
        "group-stream",
        &outputs(recipes),
        &mut [],
    )
    .unwrap();
    let files = recipes
        .iter()
        .zip(&images)
        .enumerate()
        .map(|(index, (recipe, image))| {
            let path = dir.join(format!("{index}.{}", recipe.format));
            save_rendered_export(
                image.clone(),
                SOURCE,
                &fixture.recipe,
                &recipe.settings,
                &output(&path, recipe.format),
                &going,
            )
            .unwrap();
            std::fs::read(&path).unwrap()
        })
        .collect();
    (files, images)
}

/// `save_group_rendering` of `recipes` for member `own`, whose file is at
/// `paths[own]`.
fn group(
    env: &RenderEnv,
    fixture: &Fixture,
    recipes: &[Recipe],
    paths: &[PathBuf],
    own: usize,
) -> Vec<GroupRender> {
    save_group_rendering(
        SOURCE,
        &fixture.prepared,
        &fixture.recipe,
        &recipes[own].settings,
        env,
        &output(&paths[own], recipes[own].format),
        &going,
        "group-stream",
        &outputs(recipes),
        own,
    )
    .unwrap()
}

fn kind(render: &GroupRender) -> &'static str {
    match render {
        GroupRender::Image(_) => "image",
        GroupRender::Saved => "saved",
        GroupRender::Failed(_) => "failed",
    }
}

/// The file of a member from its group render: written while it rendered,
/// or saved from its render at its turn.
fn file_of(render: GroupRender, fixture: &Fixture, recipe: &Recipe, path: &Path) -> Vec<u8> {
    match render {
        GroupRender::Saved => {}
        GroupRender::Image(image) => save_rendered_export(
            image,
            SOURCE,
            &fixture.recipe,
            &recipe.settings,
            &output(path, recipe.format),
            &going,
        )
        .unwrap(),
        GroupRender::Failed(e) => panic!("{}: {e}", path.display()),
    }
    std::fs::read(path).unwrap()
}

#[test]
#[ignore = "needs a GPU"]
fn a_group_writes_each_tiff_while_it_renders_with_the_bytes_of_its_render() {
    let context = GpuContext::headless().unwrap();
    let processor = GpuProcessorSlot::new(None);
    let cache = Mutex::new(None);
    let tonemapper = |_raw: bool| None;
    let lut = |_path: &str| None;
    let env = RenderEnv {
        context: &context,
        processor: &processor,
        image_cache: &cache,
        tonemapper_override: &tonemapper,
        load_lut: &lut,
        render_inputs: None,
    };
    let adjustments = serde_json::json!({"exposure": 0.4, "contrast": 12, "saturation": 15});
    let versions = rawtonic_engine::recipe_versions::RecipeVersions::of(&adjustments, false);
    let source = ExportSource {
        pristine: photo(),
        patched: None,
        measures: None,
        library: None,
    };
    let prepared =
        prepare_export_render(SOURCE, source, &adjustments, &versions, &env, false).unwrap();
    let fixture = Fixture {
        prepared,
        recipe: adjustments,
    };
    let dir = tempfile::tempdir().unwrap();
    let folder = |name: &str| {
        let folder = dir.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        folder
    };
    let paths = |folder: &Path, recipes: &[Recipe]| -> Vec<PathBuf> {
        recipes
            .iter()
            .enumerate()
            .map(|(index, recipe)| folder.join(format!("{index}.{}", recipe.format)))
            .collect()
    };
    let files = |folder: &Path| std::fs::read_dir(folder).unwrap().count();
    use OutputColorSpace::{AdobeRgb, ProPhoto, Srgb};
    use TiffBitDepth::{Eight, Sixteen};

    // A TIFF first (the app saves full-size TIFFs first), then a JPEG and
    // a TIFF in other colour spaces and depths.
    let recipes = [
        recipe("tif", AdobeRgb, Sixteen),
        recipe("jpg", Srgb, Eight),
        recipe("tiff", ProPhoto, Eight),
    ];
    let (expected, _) = reference(&env, &fixture, &recipes, &folder("tiff-first-ref"));
    let out = folder("tiff-first");
    let paths_of = paths(&out, &recipes);
    let renders = group(&env, &fixture, &recipes, &paths_of, 0);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["saved", "image", "image"]
    );
    for (index, render) in renders.into_iter().enumerate() {
        let bytes = file_of(render, &fixture, &recipes[index], &paths_of[index]);
        assert!(bytes == expected[index], "tiff first, recipe {index}");
    }
    assert_eq!(files(&out), 3, "no temporary file left");

    // The group's first recipe was skipped: the group renders for the
    // second, which streams in its own colour space, not the first's.
    let recipes = [
        recipe("tif", Srgb, Sixteen),
        recipe("tif", ProPhoto, Sixteen),
        recipe("tif", AdobeRgb, Eight),
    ];
    let (expected, images) = reference(&env, &fixture, &recipes, &folder("skipped-ref"));
    let out = folder("skipped");
    let paths_of = paths(&out, &recipes);
    let renders = group(&env, &fixture, &recipes, &paths_of, 1);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["image", "saved", "image"]
    );
    let mut renders = renders.into_iter();
    match renders.next().unwrap() {
        GroupRender::Image(image) => assert!(image.as_bytes() == images[0].as_bytes()),
        _ => unreachable!(),
    }
    for (index, render) in (1..).zip(renders) {
        let bytes = file_of(render, &fixture, &recipes[index], &paths_of[index]);
        assert!(bytes == expected[index], "skipped first, recipe {index}");
    }
    assert!(expected[1] != expected[0], "the colour spaces differ");

    // The streaming member's file cannot be created (its folder is gone):
    // it fails alone, the later recipes get their renders and are saved at
    // their turn, writable as ever.
    let recipes = [
        recipe("tif", Srgb, Sixteen),
        recipe("tif", AdobeRgb, Sixteen),
        recipe("jpg", Srgb, Eight),
    ];
    let (expected, _) = reference(&env, &fixture, &recipes, &folder("failed-ref"));
    let out = folder("failed");
    let mut paths_of = paths(&out, &recipes);
    paths_of[0] = dir.path().join("missing").join("0.tif");
    let renders = group(&env, &fixture, &recipes, &paths_of, 0);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["failed", "image", "image"]
    );
    for (index, render) in renders.into_iter().enumerate().skip(1) {
        let bytes = file_of(render, &fixture, &recipes[index], &paths_of[index]);
        assert!(bytes == expected[index], "failed first, recipe {index}");
    }
    assert!(!paths_of[0].exists());
    assert_eq!(files(&out), 2);
}
