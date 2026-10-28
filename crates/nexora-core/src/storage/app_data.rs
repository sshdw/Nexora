//! One app-data resolver for every shell.
//!
//! The desktop used to derive the database path inline from Tauri's
//! `app.path().app_data_dir()`, and the CLI carried its own copy of the same
//! per-OS join. Issue #161 centralizes the rule here so the window app and the
//! terminal can never disagree about where `nexora.db` lives.
//!
//! The identifiers below are the contract:
//!
//! - `com.nexora.app` is the Tauri bundle identifier
//!   (`src-tauri/tauri.conf.json`);
//! - `nexora.db` is the single database filename opened at startup
//!   (`src-tauri/src/lib.rs`).
//!
//! Resolution mirrors Tauri v2's `app_data_dir()`:
//!
//! - Linux: `$XDG_DATA_HOME/<id>`, else `$HOME/.local/share/<id>`;
//! - macOS: `$HOME/Library/Application Support/<id>`;
//! - Windows: `%APPDATA%\<id>`.
//!
//! Inputs are explicit parameters (instead of reading the environment inside
//! the rule) so every branch is unit-testable on any host. `None` means the
//! platform base directory is unknown — callers must not guess a path.

use std::path::PathBuf;

/// Desktop app-data identifier (`src-tauri/tauri.conf.json`).
pub const APP_IDENTIFIER: &str = "com.nexora.app";

/// Single database filename (`src-tauri/src/lib.rs` setup).
pub const DB_FILE_NAME: &str = "nexora.db";

/// OS flavour for app-data resolution (mirrors Tauri's per-OS branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux and other XDG-style Unix desktops.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

impl Platform {
    /// The platform this build targets, or `None` when the path rule is not
    /// defined for it.
    #[must_use]
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Resolve the app-data directory using the same rules Tauri v2 applies for
/// [`APP_IDENTIFIER`]. Returns `None` when the required base is unknown.
#[must_use]
pub fn app_data_dir_with(
    platform: Platform,
    home: Option<&str>,
    xdg_data_home: Option<&str>,
    appdata: Option<&str>,
) -> Option<PathBuf> {
    match platform {
        Platform::Linux => {
            if let Some(xdg) = xdg_data_home.filter(|value| !value.is_empty()) {
                Some(PathBuf::from(xdg).join(APP_IDENTIFIER))
            } else {
                home.filter(|value| !value.is_empty()).map(|value| {
                    PathBuf::from(value)
                        .join(".local/share")
                        .join(APP_IDENTIFIER)
                })
            }
        }
        Platform::Macos => home.filter(|value| !value.is_empty()).map(|value| {
            PathBuf::from(value)
                .join("Library/Application Support")
                .join(APP_IDENTIFIER)
        }),
        Platform::Windows => appdata
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(value).join(APP_IDENTIFIER)),
    }
}

/// Resolve the app-data directory from the process environment.
#[must_use]
pub fn app_data_dir() -> Option<PathBuf> {
    let platform = Platform::current()?;
    let home = std::env::var("HOME").ok();
    let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
    let appdata = std::env::var("APPDATA").ok();
    app_data_dir_with(
        platform,
        home.as_deref(),
        xdg_data_home.as_deref(),
        appdata.as_deref(),
    )
}

/// Resolve the single database file path from the process environment.
#[must_use]
pub fn database_path() -> Option<PathBuf> {
    app_data_dir().map(|dir| dir.join(DB_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_prefers_xdg_data_home() {
        assert_eq!(
            app_data_dir_with(
                Platform::Linux,
                Some("/home/alice"),
                Some("/data/xdg"),
                None
            ),
            Some(PathBuf::from("/data/xdg").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn linux_falls_back_to_home_local_share() {
        assert_eq!(
            app_data_dir_with(Platform::Linux, Some("/home/alice"), None, None),
            Some(PathBuf::from("/home/alice/.local/share").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn linux_empty_xdg_falls_back_to_home() {
        assert_eq!(
            app_data_dir_with(Platform::Linux, Some("/home/alice"), Some(""), None),
            Some(PathBuf::from("/home/alice/.local/share").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn linux_without_home_is_unknown() {
        assert_eq!(app_data_dir_with(Platform::Linux, None, None, None), None);
    }

    #[test]
    fn macos_uses_library_application_support() {
        assert_eq!(
            app_data_dir_with(Platform::Macos, Some("/Users/alice"), None, None),
            Some(PathBuf::from("/Users/alice/Library/Application Support").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn windows_uses_roaming_appdata() {
        assert_eq!(
            app_data_dir_with(
                Platform::Windows,
                None,
                None,
                Some("C:\\Users\\alice\\AppData\\Roaming")
            ),
            Some(PathBuf::from("C:\\Users\\alice\\AppData\\Roaming").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn database_path_joins_the_one_filename() {
        let dir =
            app_data_dir_with(Platform::Linux, Some("/home/alice"), None, None).expect("linux dir");
        assert_eq!(dir.join(DB_FILE_NAME).file_name().unwrap(), "nexora.db");
    }
}
