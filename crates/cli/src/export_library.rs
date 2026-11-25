//! Resolve export ownership before rendering; never choose between two catalogs' edits.
use anyhow::{Context, Result};
use rawtonic_catalog::Library;
use rawtonic_fs::Canonical;
use std::path::{Path, PathBuf};

pub fn registered(settings: &Path) -> Result<Vec<PathBuf>> {
    let text = match std::fs::read_to_string(settings) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("Reading the app's library registry"),
    };
    #[derive(serde::Deserialize)]
    struct Registry {
        #[serde(rename = "recentLibraries")]
        recent_libraries: Option<Vec<PathBuf>>,
    }
    let registry: Registry = serde_json::from_str(&text)
        .context("Reading the app’s library registry; use --catalog to select a library")?;
    Ok(registry.recent_libraries.unwrap_or_default())
}

pub fn owner(source: &Path, selected: Option<&Path>, known: &[PathBuf]) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    let containing = rawtonic_catalog::layout::LibraryLayout::holding(source)
        .map(rawtonic_catalog::layout::LibraryLayout::into_directory);
    let candidates = containing
        .filter(|_| selected.is_none())
        .into_iter()
        .chain(selected.map(PathBuf::from))
        .chain(if selected.is_none() { known } else { &[] }.iter().cloned());
    for directory in candidates {
        // Offline recent libraries are normal. An explicitly selected library must be readable.
        if !directory.exists() && selected != Some(directory.as_path()) {
            continue;
        }
        let directory = directory
            .canonical()
            .with_context(|| format!("Library {} cannot be read", directory.display()))?;
        if matches.contains(&directory) {
            continue;
        }
        let lib = Library::open(&directory)
            .with_context(|| format!("Library {} cannot be read", directory.display()))?;
        if lib.asset_at(source)?.is_some() {
            matches.push(directory);
        } else {
            anyhow::ensure!(
                selected.is_none(),
                "The selected library does not manage {}",
                source.display()
            );
        }
    }
    anyhow::ensure!(
        matches.len() <= 1,
        "Several libraries manage {}; select one with --catalog",
        source.display()
    );
    Ok(matches.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_photos_resolve_and_ambiguity_requires_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("external.tif");
        std::fs::write(&source, b"synthetic").unwrap();
        let a =
            Library::create(&tmp.path().join("a"), "a", rawtonic_catalog::Mode::Catalog).unwrap();
        a.add_asset(&source).unwrap();
        let known = vec![a.directory.clone()];
        assert_eq!(
            owner(&source, None, &known).unwrap(),
            Some(a.directory.canonical().unwrap())
        );
        let b =
            Library::create(&tmp.path().join("b"), "b", rawtonic_catalog::Mode::Catalog).unwrap();
        b.add_asset(&source).unwrap();
        let known = vec![a.directory.clone(), b.directory.clone()];
        assert!(
            owner(&source, None, &known)
                .unwrap_err()
                .to_string()
                .contains("--catalog")
        );
        assert_eq!(
            owner(&source, Some(&b.directory), &known).unwrap(),
            Some(b.directory.canonical().unwrap())
        );
        let absent = tmp.path().join("offline");
        assert!(
            owner(&source, None, std::slice::from_ref(&absent))
                .unwrap()
                .is_none()
        );
        assert!(owner(&source, Some(&absent), &[]).is_err());
        let unrelated = tmp.path().join("unmanaged.tif");
        std::fs::write(&unrelated, b"synthetic").unwrap();
        assert!(owner(&unrelated, Some(&a.directory), &[]).is_err());
        let settings = tmp.path().join("settings.json");
        std::fs::write(
            &settings,
            serde_json::json!({"recentLibraries": known}).to_string(),
        )
        .unwrap();
        assert_eq!(registered(&settings).unwrap(), known);
        std::fs::write(&settings, r#"{"recentLibraries": false}"#).unwrap();
        assert!(registered(&settings).is_err());
    }
}
