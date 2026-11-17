//! A recipe group's files written while the group renders
//! (`save_group_rendering`, notes: benchmark-results/mem15-2026-11-17): the
//! member it renders for publishes its streamed TIFF at once, later TIFFs
//! are staged and published at their own turn, and every file has the bytes
//! of the same recipe saved from its render held whole. Also when the
//! group's first recipe was skipped (the member renders in its own colour
//! space) and when a file cannot be created (the other recipes are
//! unaffected). Needs a GPU:
//!
//! cargo test -p rawtonic-export --test group_stream -- --ignored

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{
    EncodedExport, ExportOutput, ExportSettings, ExportSource, GroupFile, GroupRender,
    PreparedExport, RenderEnv, TiffBitDepth, prepare_export_render, render_output_precision,
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

/// `save_group_rendering` of `recipes` for member `own` with files at
/// `paths` (index → path) and the members after `own` in `later` staged.
fn group(
    env: &RenderEnv,
    fixture: &Fixture,
    recipes: &[Recipe],
    paths: &[PathBuf],
    own: usize,
    later: &[usize],
) -> Vec<GroupRender> {
    let outputs_of: Vec<ExportOutput> = recipes
        .iter()
        .zip(paths)
        .map(|(recipe, path)| output(path, recipe.format))
        .collect();
    let files: Vec<GroupFile> = later
        .iter()
        .map(|&index| GroupFile {
            index,
            export_settings: &recipes[index].settings,
            output: &outputs_of[index],
        })
        .collect();
    save_group_rendering(
        SOURCE,
        &fixture.prepared,
        &fixture.recipe,
        &recipes[own].settings,
        env,
        &outputs_of[own],
        &going,
        "group-stream",
        &outputs(recipes),
        own,
        &files,
    )
    .unwrap()
}

fn kind(render: &GroupRender) -> &'static str {
    match render {
        GroupRender::Image(_) => "image",
        GroupRender::Saved => "saved",
        GroupRender::Staged(_) => "staged",
        GroupRender::Failed(_) => "failed",
    }
}

/// The file of member `index` from its group render: published (staged or
/// saved) or saved from its render at its turn.
fn file_of(render: GroupRender, fixture: &Fixture, recipe: &Recipe, path: &Path) -> Vec<u8> {
    match render {
        GroupRender::Saved => {}
        GroupRender::Staged(staged) => {
            assert_eq!(staged.path(), path);
            assert!(!path.exists(), "published only at its turn");
            staged.publish().unwrap();
        }
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

    // A JPEG first, then two TIFFs in other colour spaces and depths: the
    // TIFFs are written while the group renders, published at their turn.
    let recipes = [
        recipe("jpg", Srgb, Eight),
        recipe("tif", AdobeRgb, Sixteen),
        recipe("tiff", ProPhoto, Eight),
    ];
    let (expected, _) = reference(&env, &fixture, &recipes, &folder("jpeg-first-ref"));
    let out = folder("jpeg-first");
    let paths_of = paths(&out, &recipes);
    let renders = group(&env, &fixture, &recipes, &paths_of, 0, &[1, 2]);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["image", "staged", "staged"]
    );
    assert_eq!(files(&out), 2, "the staged temporary files only");
    for (index, render) in renders.into_iter().enumerate() {
        let bytes = file_of(render, &fixture, &recipes[index], &paths_of[index]);
        assert!(bytes == expected[index], "jpeg first, recipe {index}");
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
    let renders = group(&env, &fixture, &recipes, &paths_of, 1, &[2]);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["image", "saved", "staged"]
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

    // The rendering member's file cannot be created (its folder is gone):
    // it fails alone; the later TIFF still streams, the JPEG keeps its
    // render.
    let recipes = [
        recipe("tif", Srgb, Sixteen),
        recipe("tif", AdobeRgb, Sixteen),
        recipe("jpg", Srgb, Eight),
    ];
    let (expected, _) = reference(&env, &fixture, &recipes, &folder("failed-ref"));
    let out = folder("failed");
    let mut paths_of = paths(&out, &recipes);
    paths_of[0] = dir.path().join("missing").join("0.tif");
    let renders = group(&env, &fixture, &recipes, &paths_of, 0, &[1]);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["failed", "staged", "image"]
    );
    for (index, render) in renders.into_iter().enumerate().skip(1) {
        let bytes = file_of(render, &fixture, &recipes[index], &paths_of[index]);
        assert!(bytes == expected[index], "failed first, recipe {index}");
    }
    assert!(!paths_of[0].exists());

    // A later TIFF whose file cannot be created comes back as its render,
    // saved at its turn as any other.
    let out = folder("failed-later");
    let mut paths_of = paths(&out, &recipes);
    paths_of[1] = dir.path().join("missing").join("1.tif");
    let renders = group(&env, &fixture, &recipes, &paths_of, 0, &[1]);
    assert_eq!(
        renders.iter().map(kind).collect::<Vec<_>>(),
        ["saved", "image", "image"]
    );
    let mut renders = renders.into_iter();
    let first = file_of(renders.next().unwrap(), &fixture, &recipes[0], &paths_of[0]);
    assert!(first == expected[0]);
    let later_path = out.join("1.tif");
    let later = file_of(renders.next().unwrap(), &fixture, &recipes[1], &later_path);
    assert!(later == expected[1]);
    assert_eq!(files(&out), 2);
}
