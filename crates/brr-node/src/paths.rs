use std::path::PathBuf;

pub fn default_data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("BRR_DATA_DIR").filter(|path| !path.is_empty()) {
        return path.into();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::new();
    };
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(home).join("Library/Application Support/dev.brr.desktop")
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
            .join("brr")
    }
}

/// Resolve only a packaged or explicitly source-built Brr runtime, never PATH.
pub fn owned_runtime_path() -> PathBuf {
    let packaged = std::env::current_exe()
        .unwrap_or_default()
        .with_file_name("brr-runtime");
    if packaged.is_file() {
        return packaged;
    }
    #[cfg(debug_assertions)]
    {
        let development =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/binaries/brr-runtime");
        if development.is_file() {
            return development;
        }
    }
    packaged
}

pub fn owned_browser_path() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    let sibling = executable
        .with_file_name("browser")
        .join("chrome-headless-shell");
    if sibling.is_file() {
        return sibling;
    }
    #[cfg(target_os = "macos")]
    {
        let resource = executable
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("../Resources/browser/chrome-headless-shell");
        if resource.is_file() {
            return resource;
        }
    }
    #[cfg(debug_assertions)]
    {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/binaries/browser/chrome-headless-shell");
        if development.is_file() {
            return development;
        }
    }
    sibling
}
