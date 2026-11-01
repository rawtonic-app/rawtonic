//! The Sky and Subject models from a model folder the app filled (the
//! CLI's `--models`), on the backend the app picks there: on macOS the Core
//! ML program the app assembled from the model pack, else the ONNX model.
//! Nothing is downloaded or assembled here; a model that is not installed
//! is an error that says so.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

use crate::{
    SKYSEG_FILENAME, SKYSEG_SHA256, SUBJECT_FILENAME, SUBJECT_SHA256, SkyModel, SubjectModel,
};

/// Whether the file at `path` has the SHA-256 `expected_hash` (lower-case hex).
pub fn file_has_sha256(path: &Path, expected_hash: &str) -> Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(hex == expected_hash)
}

fn verified(path: &Path, sha: &str) -> bool {
    path.is_file() && file_has_sha256(path, sha).unwrap_or(false)
}

/// The Sky and Subject models of a model folder, each loaded on first use.
pub struct Models {
    dir: PathBuf,
    sky: Mutex<Option<Arc<SkyModel>>>,
    subject: Mutex<Option<Arc<SubjectModel>>>,
}

impl Models {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            sky: Mutex::new(None),
            subject: Mutex::new(None),
        }
    }

    pub fn sky(&self) -> Result<Arc<SkyModel>> {
        let mut slot = self.sky.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(model) = slot.as_ref() {
            return Ok(model.clone());
        }
        #[cfg(target_os = "macos")]
        if let Some(model) = self.core_ml(crate::SKY_CORE_ML_PACKAGE, &crate::SKY_CORE_ML_FILES)? {
            return Ok(slot.insert(Arc::new(SkyModel::core_ml(model))).clone());
        }
        let path = self.onnx(SKYSEG_FILENAME, SKYSEG_SHA256, "Sky")?;
        Ok(slot.insert(Arc::new(SkyModel::onnx(&path)?)).clone())
    }

    pub fn subject(&self) -> Result<Arc<SubjectModel>> {
        let mut slot = self.subject.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(model) = slot.as_ref() {
            return Ok(model.clone());
        }
        #[cfg(target_os = "macos")]
        if let Some(model) = self.core_ml(
            crate::SUBJECT_CORE_ML_PACKAGE,
            &crate::SUBJECT_CORE_ML_FILES,
        )? {
            return Ok(slot.insert(Arc::new(SubjectModel::core_ml(model))).clone());
        }
        let path = self.onnx(SUBJECT_FILENAME, SUBJECT_SHA256, "Subject")?;
        Ok(slot.insert(Arc::new(SubjectModel::onnx(&path)?)).clone())
    }

    fn onnx(&self, file: &str, sha: &str, name: &str) -> Result<PathBuf> {
        let path = self.dir.join(file);
        if !verified(&path, sha) {
            bail!(
                "the {name} model is not installed in {} (open a photo with a {name} selection in the app once, or run scripts/models/install_models.sh)",
                self.dir.display()
            );
        }
        Ok(path)
    }

    /// The Core ML program when the app would run it: its pack files are
    /// installed (`files`: pack file, place in the package, SHA-256). The
    /// app assembles the package on its first use; an unassembled one is an
    /// error here, since the ONNX model's masks differ slightly.
    #[cfg(target_os = "macos")]
    fn core_ml(
        &self,
        package: &str,
        files: &[(&str, &str, &str)],
    ) -> Result<Option<rawtonic_denoise::coreml::CoreMlModel>> {
        if !files
            .iter()
            .all(|&(file, _, sha)| verified(&self.dir.join(file), sha))
        {
            return Ok(None);
        }
        let package = self.dir.join(package);
        if !files
            .iter()
            .all(|&(_, inner, sha)| verified(&package.join(inner), sha))
        {
            bail!(
                "the Core ML program {} is not assembled yet: open a photo with this selection in the app once",
                package.display()
            );
        }
        Ok(Some(rawtonic_denoise::coreml::CoreMlModel::load(
            &package,
            crate::CORE_ML_UNITS,
        )?))
    }
}
