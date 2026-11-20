use super::*;
use serde_json::json;

const FRAMES: TiffFrames = TiffFrames {
    thumbnail: (288, 192),
    full: (2496, 1662),
};

fn close(a: &Value, b: f64) {
    let a = a.as_f64().unwrap();
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}

#[test]
fn the_stamp() {
    let mut recipe = json!({"exposure": 1.0});
    assert_eq!(stamped(&recipe), None);
    stamp(&mut recipe, 2496, 1662);
    assert_eq!(stamped(&recipe), Some((2496.0, 1662.0)));
    assert_eq!(stamped(&json!({KEY: {"width": 0, "height": 5}})), None);
}

#[test]
fn a_crop_and_spots_scale_with_the_photo() {
    let (sx, sy) = (2496.0 / 288.0, 1662.0 / 192.0);
    let mut recipe = json!({
        "crop": {"x": 57.6, "y": 28.8, "width": 144.0, "height": 115.2},
        "spots": [{"id": "a", "x": 158.4, "y": 96.0, "radius": 11.52, "sourceX": 72.0, "sourceY": 96.0,
                   "points": [{"x": 10.0, "y": 20.0}]}]
    });
    scale_geometry(&mut recipe, FRAMES.thumbnail, FRAMES.full);
    close(&recipe["crop"]["x"], 57.6 * sx);
    close(&recipe["crop"]["y"], 28.8 * sy);
    close(&recipe["crop"]["width"], 144.0 * sx);
    close(&recipe["crop"]["height"], 115.2 * sy);
    close(&recipe["spots"][0]["x"], 158.4 * sx);
    close(&recipe["spots"][0]["sourceY"], 96.0 * sy);
    close(&recipe["spots"][0]["radius"], 11.52 * (sx + sy) / 2.0);
    close(&recipe["spots"][0]["points"][0]["y"], 20.0 * sy);
}

/// A crop of a turned photo is in the turned frame: its x scales with the
/// photo's height.
#[test]
fn a_turned_crop_scales_in_the_turned_frame() {
    let mut recipe = json!({"orientationSteps": 1, "crop": {"x": 19.2, "y": 72.0, "width": 105.6, "height": 144.0}});
    scale_geometry(&mut recipe, FRAMES.thumbnail, FRAMES.full);
    close(&recipe["crop"]["x"], 19.2 * 1662.0 / 192.0);
    close(&recipe["crop"]["y"], 72.0 * 2496.0 / 288.0);
}

/// A crop of a locked ratio keeps it (the frames' ratios differ slightly)
/// and stays inside the photo.
#[test]
fn a_locked_crop_keeps_its_ratio() {
    let frames = TiffFrames {
        thumbnail: (156, 104),
        full: (3048, 2016),
    };
    let mut recipe =
        json!({"aspectRatio": 1.5, "crop": {"x": 0.0, "y": 0.0, "width": 156.0, "height": 104.0}});
    scale_geometry(&mut recipe, frames.thumbnail, frames.full);
    let c = &recipe["crop"];
    let (x, y, w, h) = (
        c["x"].as_f64().unwrap(),
        c["y"].as_f64().unwrap(),
        c["width"].as_f64().unwrap(),
        c["height"].as_f64().unwrap(),
    );
    assert!((w / h - 1.5).abs() < 1e-9);
    assert!(x >= 0.0 && y >= 0.0 && x + w <= 3048.0 + 1e-9 && y + h <= 2016.0 + 1e-9);
    assert!(w > 3000.0);
}

#[test]
fn drawn_masks_without_a_size_get_the_thumbnail() {
    let mut recipe = json!({"masks": [{"subMasks": [
        {"type": "radial", "parameters": {"centerX": 10.0}},
        {"type": "brush", "parameters": {"lines": [{"points": []}, {"points": [], "frame": {"rotation": 0.0, "width": 2496.0, "height": 1662.0}}]}},
        {"type": "linear", "parameters": {"frame": {"rotation": 0.0}}}
    ]}]});
    scale_geometry(&mut recipe, FRAMES.thumbnail, FRAMES.full);
    let subs = &recipe["masks"][0]["subMasks"];
    assert_eq!(subs[0]["parameters"]["frame"]["width"], json!(288.0));
    assert_eq!(
        subs[1]["parameters"]["lines"][0]["frame"]["height"],
        json!(192.0)
    );
    // A stroke that knows its size keeps it.
    assert_eq!(
        subs[1]["parameters"]["lines"][1]["frame"]["width"],
        json!(2496.0)
    );
    assert_eq!(subs[2]["parameters"]["frame"]["width"], json!(288.0));
}

#[test]
fn the_recipe_tells_on_which_frame_it_was_made() {
    // Everything within the thumbnail: made on it.
    assert!(made_on_thumbnail(
        &json!({"crop": {"x": 57.6, "y": 28.8, "width": 144.0, "height": 115.2}}),
        FRAMES
    ));
    assert!(made_on_thumbnail(&json!({"exposure": 1.0}), FRAMES));
    // A crop beyond it: the full frame.
    assert!(!made_on_thumbnail(
        &json!({"crop": {"x": 500.0, "y": 300.0, "width": 1248.0, "height": 997.0}}),
        FRAMES
    ));
    // A turned photo's crop is in the turned frame (192 wide).
    assert!(!made_on_thumbnail(
        &json!({"orientationSteps": 1, "crop": {"x": 0.0, "y": 0.0, "width": 250.0, "height": 100.0}}),
        FRAMES
    ));
    // A spot beyond it.
    assert!(!made_on_thumbnail(
        &json!({"spots": [{"x": 1000.0, "y": 50.0, "radius": 10.0, "sourceX": 20.0, "sourceY": 20.0}]}),
        FRAMES
    ));
    // A drawn mask of the full frame's size, among thumbnail geometry.
    let full_mask = json!({
        "crop": {"x": 10.0, "y": 10.0, "width": 100.0, "height": 100.0},
        "masks": [{"subMasks": [{"type": "radial", "parameters": {"frame": {"width": 2496.0, "height": 1662.0}}}]}]
    });
    assert!(!made_on_thumbnail(&full_mask, FRAMES));
    let thumb_mask = json!({
        "masks": [{"subMasks": [{"type": "radial", "parameters": {"frame": {"width": 288.0, "height": 192.0}}}]}]
    });
    assert!(made_on_thumbnail(&thumb_mask, FRAMES));
}

#[test]
fn ai_bitmaps_tell_their_size() {
    let png = |w: u32, h: u32| {
        let mut bytes = Vec::new();
        image::GrayImage::new(w, h)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    };
    let recipe = |w, h| json!({"masks": [{"subMasks": [{"type": "ai-foreground", "parameters": {"maskDataBase64": png(w, h)}}]}]});
    assert_eq!(ai_bitmap_sizes(&recipe(288, 192)), vec![(288, 192)]);
    assert!(made_on_thumbnail(&recipe(288, 192), FRAMES));
    assert!(!made_on_thumbnail(&recipe(2496, 1662), FRAMES));
}

/// Files other than camera TIFFs, new recipes and stamped ones are left
/// alone.
#[test]
fn conform_leaves_other_recipes_alone() {
    let crop = json!({"crop": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}});
    for path in ["/offline/IMG_1.CR3", "/offline/scan.tif", "/offline/a.jpg"] {
        let mut recipe = crop.clone();
        assert!(!conform(&mut recipe, Path::new(path)));
        assert_eq!(recipe, crop);
    }
    let mut new = json!({});
    assert!(!conform(&mut new, Path::new("/offline/a.tif")));
}

/// The camera TIFFs of `RAWTONIC_TIFF_SAMPLES` (CC0 raw.pixls.us copies,
/// notes: benchmark-results/tifr2-2026-11-21): their thumbnail and full
/// frames, and a thumbnail crop conformed to the full frame.
#[test]
#[ignore = "requires RAWTONIC_TIFF_SAMPLES with camera RAW TIFFs"]
fn camera_tiff_frames() {
    let root = std::env::var_os("RAWTONIC_TIFF_SAMPLES").expect("sample directory");
    let mut seen = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let Some(frames) = tiff_frames(&path) else {
            println!("{}: no frames", path.display());
            continue;
        };
        println!("{}: {:?}", path.display(), frames);
        let (w, h) = frames.thumbnail;
        let mut recipe = json!({"crop": {"x": 0.2 * w as f64, "y": 0.15 * h as f64,
            "width": 0.5 * w as f64, "height": 0.6 * h as f64}});
        assert!(conform(&mut recipe, &path));
        assert_eq!(
            stamped(&recipe),
            Some((frames.full.0 as f64, frames.full.1 as f64))
        );
        close(&recipe["crop"]["width"], 0.5 * frames.full.0 as f64);
        // Conformed once: stamped, so never again.
        assert!(!conform(&mut recipe, &path));
        seen += 1;
    }
    assert!(seen > 0);
}

/// An edit begun on the RAW (since PR #916) carries the RAW passes'
/// versions, which an ordinary TIFF's edit never has: it is never moved,
/// however small its geometry (a cloned dust speck in the corner).
#[test]
fn an_edit_begun_on_the_raw_is_left_alone() {
    let corner =
        json!({"spots": [{"x": 40.0, "y": 30.0, "radius": 8.0, "sourceX": 60.0, "sourceY": 30.0}]});
    assert!(made_on_thumbnail(&corner, FRAMES));
    for key in ["fringeVersion", "autoCaVersion"] {
        let mut raw = corner.clone();
        raw[key] = json!(6);
        assert!(!made_on_thumbnail(&raw, FRAMES), "{key}");
    }
}

/// An Object selection's box and prompts move with the photo, and its
/// bitmap records the size it was made on so the render stretches it.
#[test]
fn ai_prompts_and_bitmaps_move_with_the_photo() {
    let (sx, sy) = (2496.0 / 288.0, 1662.0 / 192.0);
    let mut recipe = json!({"masks": [{"subMasks": [{"type": "ai-subject", "parameters": {
        "startX": 10.0, "startY": 20.0, "endX": 100.0, "endY": 120.0, "maskDataBase64": "data:x",
        "prompts": [{"type": "point", "x": 50.0, "y": 60.0},
                    {"type": "box", "startX": 1.0, "startY": 2.0, "endX": 3.0, "endY": 4.0, "exclude": true}]
    }}]}]});
    scale_geometry(&mut recipe, FRAMES.thumbnail, FRAMES.full);
    let p = &recipe["masks"][0]["subMasks"][0]["parameters"];
    close(&p["startX"], 10.0 * sx);
    close(&p["endY"], 120.0 * sy);
    close(&p["prompts"][0]["x"], 50.0 * sx);
    close(&p["prompts"][1]["endY"], 4.0 * sy);
    assert_eq!(p["prompts"][1]["exclude"], json!(true));
    assert_eq!(
        p[crate::mask_generation::BITMAP_FRAME],
        json!({"width": 288, "height": 192})
    );
}
