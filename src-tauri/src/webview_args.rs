use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder, Wry, utils::config::WindowConfig};

// Tauri puts both windows in the app's local data directory. WebView2 requires
// identical arguments for that shared environment. Custom arguments replace
// wry's defaults, so retain its three disabled features.
#[cfg(any(windows, test))]
const WEBVIEW2_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                            --disable-background-networking --disable-component-update";

pub(crate) fn from_config<'a>(
    app: &'a AppHandle,
    config: &WindowConfig,
) -> tauri::Result<WebviewWindowBuilder<'a, Wry, AppHandle>> {
    WebviewWindowBuilder::from_config(app, config).map(apply_browser_args)
}

pub(crate) fn new_window<'a>(
    app: &'a AppHandle,
    label: &str,
    url: WebviewUrl,
) -> WebviewWindowBuilder<'a, Wry, AppHandle> {
    apply_browser_args(WebviewWindowBuilder::new(app, label, url))
}

fn apply_browser_args(
    builder: WebviewWindowBuilder<'_, Wry, AppHandle>,
) -> WebviewWindowBuilder<'_, Wry, AppHandle> {
    #[cfg(windows)]
    let builder = builder.additional_browser_args(WEBVIEW2_ARGS);
    builder
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    #[test]
    fn webview2_arguments_preserve_wry_features_and_disable_background_services() {
        assert_eq!(
            super::WEBVIEW2_ARGS,
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-background-networking --disable-component-update"
        );
    }

    #[test]
    fn desktop_webviews_use_the_shared_builders() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let source_dir = root.join("src");
        let builder_module = source_dir.join("webview_args.rs");
        for entry in walkdir::WalkDir::new(&source_dir) {
            let entry = entry.unwrap();
            let path = entry.path();
            if path == builder_module || path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = fs::read_to_string(path).unwrap();
            for symbol in [
                "WebviewWindowBuilder",
                "WebviewBuilder",
                "additional_browser_args",
            ] {
                assert!(
                    !source.contains(symbol),
                    "{} uses {symbol} outside webview_args.rs",
                    path.display()
                );
            }
        }

        // Tauri would create a configured window automatically if create were
        // omitted or true, bypassing the shared builders.
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if !name.starts_with("tauri.") || !name.ends_with("conf.json") {
                continue;
            }
            let config: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            for window in config["app"]["windows"].as_array().into_iter().flatten() {
                assert_eq!(
                    window["create"].as_bool(),
                    Some(false),
                    "{} window {} must use the shared builders",
                    path.display(),
                    window["label"]
                );
            }
        }
    }
}
