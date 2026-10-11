//! Ratings, labels and keywords exchanged with XMP sidecars.
//! Part of `file_management`, split by concern (ADR 0009).
use crate::image_processing::ImageMetadata;
use crate::tagging::COLOR_TAG_PREFIX;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

pub fn extract_xmp_rating(content: &str) -> Option<u8> {
    if let Some(idx) = content.find("xmp:Rating=\"") {
        let start = idx + 12;
        let end = content[start..].find('"').map(|i| start + i)?;
        return content[start..end].parse().ok();
    }
    if let Some(idx) = content.find("<xmp:Rating>") {
        let start = idx + 12;
        let end = content[start..].find('<').map(|i| start + i)?;
        return content[start..end].parse().ok();
    }
    None
}

pub fn extract_xmp_label(content: &str) -> Option<String> {
    if let Some(idx) = content.find("xmp:Label=\"") {
        let start = idx + 11;
        let end = content[start..].find('"').map(|i| start + i)?;
        return Some(content[start..end].to_string());
    }
    if let Some(idx) = content.find("<xmp:Label>") {
        let start = idx + 11;
        let end = content[start..].find('<').map(|i| start + i)?;
        return Some(content[start..end].to_string());
    }
    None
}

pub fn extract_xmp_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(start_idx) = content.find("<dc:subject>")
        && let Some(end_idx) = content[start_idx..].find("</dc:subject>")
    {
        let subject_block = &content[start_idx..start_idx + end_idx];
        let mut current_idx = 0;
        while let Some(li_start) = subject_block[current_idx..].find("<rdf:li>") {
            let val_start = current_idx + li_start + 8;
            if let Some(li_end) = subject_block[val_start..].find("</rdf:li>") {
                tags.push(subject_block[val_start..val_start + li_end].to_string());
                current_idx = val_start + li_end + 9;
            } else {
                break;
            }
        }
    }
    tags
}

pub fn resolve_xmp_path(image_path: &Path) -> Option<PathBuf> {
    let xmp_path = image_path.with_extension("xmp");
    let xmp_path_upper = image_path.with_extension("XMP");
    if xmp_path.exists() {
        Some(xmp_path)
    } else if xmp_path_upper.exists() {
        Some(xmp_path_upper)
    } else {
        None
    }
}

pub fn sync_metadata_from_xmp(source_path: &Path, metadata: &mut ImageMetadata) -> bool {
    if crate::session_bridge::is_managed_source(source_path) {
        return false;
    }
    let actual_xmp = resolve_xmp_path(source_path);

    let mut changed = false;

    if let Some(xmp_file) = actual_xmp
        && let Ok(content) = fs::read_to_string(&xmp_file)
    {
        if metadata.rating == 0
            && let Some(rating) = extract_xmp_rating(&content)
            && rating != 0
        {
            metadata.rating = rating;
            if let Some(obj) = metadata.adjustments.as_object_mut() {
                obj.insert("rating".to_string(), serde_json::json!(rating));
            } else {
                metadata.adjustments = serde_json::json!({"rating": rating});
            }
            changed = true;
        }

        let xmp_label = extract_xmp_label(&content);
        let xmp_tags = extract_xmp_tags(&content);

        let mut current_tags = metadata.tags.clone().unwrap_or_default();
        let original_len = current_tags.len();
        let had_no_tags = metadata.tags.is_none();

        for tag in xmp_tags {
            if !current_tags.contains(&tag) {
                current_tags.push(tag);
            }
        }

        if let Some(label) = xmp_label {
            let label_tag = format!("{}{}", COLOR_TAG_PREFIX, label.to_lowercase());
            if !current_tags.contains(&label_tag) {
                current_tags.retain(|t| !t.starts_with(COLOR_TAG_PREFIX));
                current_tags.push(label_tag);
            }
        }

        if current_tags.len() != original_len || (had_no_tags && !current_tags.is_empty()) {
            metadata.tags = Some(current_tags);
            changed = true;
        }
    }
    changed
}

pub fn sync_metadata_to_xmp(source_path: &Path, metadata: &ImageMetadata, create_if_missing: bool) {
    // Managed libraries already commit and synchronize through the DB adapter.
    // A second writer would invalidate its conflict hash and overwrite its recipe.
    if crate::session_bridge::is_managed_source(source_path) {
        return;
    }
    let path = resolve_xmp_path(source_path).unwrap_or_else(|| source_path.with_extension("xmp"));
    if !path.exists() && !create_if_missing {
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        let existing = if path.exists() {
            Some(fs::read_to_string(&path)?)
        } else {
            None
        };
        let (mut meta, recipe) = existing
            .as_deref()
            .map(rawtonic_metadata::read_xmp)
            .transpose()?
            .unwrap_or_default();
        meta.rating = metadata.rating;
        meta.label.clear();
        meta.keywords.clear();
        for tag in metadata.tags.as_deref().unwrap_or_default() {
            if let Some(color) = tag.strip_prefix(COLOR_TAG_PREFIX) {
                let mut chars = color.chars();
                meta.label = chars
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default();
            } else {
                // XMP keywords without the editor's marker for typed keywords.
                let keyword = tag
                    .strip_prefix(crate::tagging::USER_TAG_PREFIX)
                    .unwrap_or(tag);
                if !meta.keywords.iter().any(|k| k == keyword) {
                    meta.keywords.push(keyword.to_string());
                }
            }
        }
        let expected = existing
            .as_ref()
            .map(|s| rawtonic_metadata::digest(s.as_bytes()));
        rawtonic_metadata::write_xmp(&path, &meta, recipe.as_ref(), expected.as_deref())?;
        Ok(())
    })();
    if let Err(error) = result {
        log::error!("XMP sync failed for {}: {error}", path.display());
    }
}
