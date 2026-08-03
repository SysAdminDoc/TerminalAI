fn main() {
    let capabilities = if std::env::var_os("CARGO_FEATURE_WDIO_EMBEDDED").is_some() {
        "./capabilities/wdio-embedded.json"
    } else if std::env::var_os("CARGO_FEATURE_WDIO").is_some() {
        "./capabilities/wdio.json"
    } else {
        "./capabilities/default.json"
    };
    tauri_build::try_build(
        tauri_build::Attributes::new().capabilities_path_pattern(capabilities),
    )
    .expect("failed to build Tauri application metadata");
}
