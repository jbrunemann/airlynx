//! Platform-specific helpers: device name, storage locations, dialogs.

#[cfg(not(target_os = "android"))]
pub use desktop::*;

#[cfg(target_os = "android")]
pub use crate::android::*;

#[cfg(not(target_os = "android"))]
mod desktop {
    use airlynx_core::DeviceKind;
    use std::path::{Path, PathBuf};

    pub fn device_kind() -> DeviceKind {
        DeviceKind::Desktop
    }

    pub fn device_name() -> String {
        let name = gethostname::gethostname().to_string_lossy().into_owned();
        name.strip_suffix(".local").unwrap_or(&name).to_string()
    }

    /// Where the device id and favorites live. Shared with the CLI.
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AirLynx")
    }

    pub fn default_receive_dir() -> PathBuf {
        dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AirLynx")
    }

    /// Android only: runs a foreground service and holds wake/Wi-Fi locks so receiving works with the screen locked.
    pub fn set_keep_awake(_on: bool) {}

    /// Android TV only: whether the app runs on a television, driven by a remote.
    pub fn is_tv() -> bool {
        false
    }

    /// Android only: sends the app to the background, like the Home button.
    pub fn go_home() {}

    /// Android only: adds a saved file to the media index.
    pub fn scan_file(_path: &Path) {}

    /// Android only: deletes the copy made of a file shared from another app.
    pub fn discard_copy(_path: &Path) {}

    pub fn pick_files() -> Vec<PathBuf> {
        rfd::FileDialog::new().pick_files().unwrap_or_default()
    }

    pub fn pick_folder() -> Option<PathBuf> {
        rfd::FileDialog::new().pick_folder()
    }

    pub fn open_folder(path: &Path) {
        let _ = open_path(path);
    }

    /// Opens a received file in the default app for its type.
    pub fn open_file(path: &Path) -> Result<(), String> {
        open_path(path).map_err(|e| format!("Could not open the file: {e}"))
    }

    /// The system opener launches files as well as folders.
    fn open_path(path: &Path) -> std::io::Result<()> {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener).arg(path).spawn().map(|_| ())
    }
}
