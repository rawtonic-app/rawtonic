//! Opt-in, headless GPU/export regression runner. Missing inputs are explicit SKIPs.
use serde::Deserialize;
use std::{env, fs, path::Path, process::Command};

#[derive(Deserialize)]
struct Fixture {
    source: String,
    golden: String,
    max_rgb_rmse: f64,
    adjustments: Option<serde_json::Value>,
}

fn inside(root: &Path, relative: &str) -> std::path::PathBuf {
    let path = root
        .join(relative)
        .canonicalize()
        .expect("Fixture file missing");
    assert!(
        path.starts_with(root),
        "Fixture paths must stay inside APP_FIXTURES"
    );
    path
}

fn main() {
    let Some(root) = env::var_os("APP_FIXTURES") else {
        println!("SKIP fixture_render: APP_FIXTURES is not set; no RAW/golden claim");
        return;
    };
    let root = Path::new(&root)
        .canonicalize()
        .expect("Invalid APP_FIXTURES");
    let fixtures: Vec<Fixture> = serde_json::from_slice(
        &fs::read(root.join("fixtures.json")).expect("APP_FIXTURES needs fixtures.json"),
    )
    .expect("Invalid fixture manifest");
    assert!(
        !fixtures.is_empty(),
        "Manifest must contain at least one fixture"
    );
    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        println!("SKIP fixture_render: no wgpu adapter available");
        return;
    };
    println!("Adapter: {:?}", adapter.get_info());
    for fixture in fixtures {
        assert!((0.0..=1.0).contains(&fixture.max_rgb_rmse));
        let original = inside(&root, &fixture.source);
        let golden = image::open(inside(&root, &fixture.golden))
            .expect("Cannot decode golden")
            .to_rgb8();
        let hash = rawtonic_ingest::sha256(&original).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join(original.file_name().unwrap());
        fs::copy(&original, &input).unwrap();
        let output = temp.path().join("render.png");
        let mut command = Command::new(env!("CARGO_BIN_EXE_rawtonic-app"));
        command
            .arg("export")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .args(["--format", "png"]);
        if let Some(adjustments) = fixture.adjustments {
            command
                .arg("--adjustments")
                .arg(serde_json::to_string(&adjustments).unwrap());
        }
        let result = command.output().expect("Renderer failed to start");
        assert!(
            result.status.success(),
            "Export failed: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let actual = image::open(&output)
            .expect("Renderer did not produce an image")
            .to_rgb8();
        assert_eq!(
            actual.dimensions(),
            golden.dimensions(),
            "Golden dimensions differ"
        );
        let squared: f64 = actual
            .as_raw()
            .iter()
            .zip(golden.as_raw())
            .map(|(a, b)| ((f64::from(*a) - f64::from(*b)) / 255.0).powi(2))
            .sum();
        let rmse = (squared / actual.as_raw().len() as f64).sqrt();
        assert!(
            rmse <= fixture.max_rgb_rmse,
            "{} RMSE {rmse} exceeds {}",
            fixture.source,
            fixture.max_rgb_rmse
        );
        assert_eq!(
            hash,
            rawtonic_ingest::sha256(&original).unwrap(),
            "Original changed"
        );
        assert_eq!(
            hash,
            rawtonic_ingest::sha256(&input).unwrap(),
            "Working original changed"
        );
        println!("PASS {}: normalized RGB RMSE {rmse:.6}", fixture.source);
    }
}
