//! Platform-specific helpers: device name, storage locations, dialogs.

#[cfg(not(target_os = "android"))]
pub use desktop::*;

#[cfg(target_os = "android")]
pub use crate::android::*;

#[cfg(not(target_os = "android"))]
mod desktop {
    use std::path::{Path, PathBuf};

    pub fn device_name() -> String {
        let name = gethostname::gethostname().to_string_lossy().into_owned();
        name.strip_suffix(".local").unwrap_or(&name).to_string()
    }

    pub fn default_receive_dir() -> PathBuf {
        dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AirLynx")
    }

    pub fn pick_files() -> Vec<PathBuf> {
        rfd::FileDialog::new().pick_files().unwrap_or_default()
    }

    pub fn pick_folder() -> Option<PathBuf> {
        rfd::FileDialog::new().pick_folder()
    }

    pub fn open_folder(path: &Path) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(path).spawn();
    }
}
