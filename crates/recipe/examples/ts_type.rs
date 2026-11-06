//! Writes the frontend's recipe type (`rawtonic_recipe::ts`) to
//! `src/generated/recipeKeys.ts`; with `--check`, fails instead when the
//! checked-in file differs (scripts/check.sh). Run from anywhere in the
//! repository: the path is resolved from this crate's directory.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rawtonic_recipe::ts::PATH);
    let text = rawtonic_recipe::ts::render();
    let check = std::env::args().skip(1).any(|a| a == "--check");
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref() == Some(text.as_str()) {
        return ExitCode::SUCCESS;
    }
    if check {
        eprintln!(
            "{} is stale: run `cargo run -p rawtonic-recipe --example ts_type`",
            rawtonic_recipe::ts::PATH
        );
        return ExitCode::FAILURE;
    }
    if let Err(err) = std::fs::create_dir_all(path.parent().expect("a file path"))
        .and_then(|()| std::fs::write(&path, text))
    {
        eprintln!("writing {}: {err}", path.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", rawtonic_recipe::ts::PATH);
    ExitCode::SUCCESS
}
