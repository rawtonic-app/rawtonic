//! Portable recipe resources. Only file-valued fields are rewritten; captions
//! and other user text are never interpreted as paths.
use serde_json::Value;
use std::path::{Component, Path};

pub const PREFIX: &str = "rawtonic-resource:";

pub fn visit(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "lutPath" | "cameraProfile" | "raster_path") {
                    if let Value::String(path) = child {
                        f(path);
                    }
                } else {
                    visit(child, f);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| visit(v, f)),
        _ => {}
    }
}

pub fn relative(value: &str) -> Option<&Path> {
    let path = Path::new(value.strip_prefix(PREFIX)?);
    (!path.as_os_str().is_empty()
        && !value.contains('\\')
        && path.components().all(|c| matches!(c, Component::Normal(_))))
    .then_some(path)
}

pub fn resolve(value: &mut Value, root: &Path) {
    visit(value, &mut |path| {
        if let Some(relative) = relative(path) {
            *path = root.join(relative).to_string_lossy().into_owned();
        }
    });
}

/// Store resource paths relative again after an editor saves resolved paths.
pub fn localize(value: &mut Value, root: &Path) {
    visit(value, &mut |path| {
        if let Ok(relative) = Path::new(path).strip_prefix(root.join("resources")) {
            *path = format!(
                "{PREFIX}resources/{}",
                relative.to_string_lossy().replace('\\', "/")
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moving_and_resaving_nested_resources_preserves_user_text() {
        let mut value = json!({"adjustments": {"lutPath": "rawtonic-resource:resources/look.cube",
            "masks": [{"cameraProfile": "rawtonic-resource:resources/camera.dcp"}]},
            "caption": "rawtonic-resource:resources/look.cube"});
        let stored = value.clone();
        resolve(&mut value, Path::new("/moved"));
        assert_eq!(
            value["adjustments"]["lutPath"],
            "/moved/resources/look.cube"
        );
        assert_eq!(
            value["adjustments"]["masks"][0]["cameraProfile"],
            "/moved/resources/camera.dcp"
        );
        assert_eq!(value["caption"], stored["caption"]);
        localize(&mut value, Path::new("/moved"));
        assert_eq!(value, stored);
        for path in [
            "rawtonic-resource:../outside",
            "rawtonic-resource:/outside",
            "rawtonic-resource:",
            "rawtonic-resource:C:\\outside",
        ] {
            assert!(relative(path).is_none());
        }
    }
}
