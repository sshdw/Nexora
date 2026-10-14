//! `nexora doctor`: offline, read-only diagnostics for the Nexora CLI stub.
//!
//! Design notes (binding for later phases):
//!
//! - The app-data directory rules MIRROR the desktop backend: Tauri v2
//!   `app.path().app_data_dir()` for the identifier `com.nexora.app`
//!   (`src-tauri/tauri.conf.json`) holding the `nexora.db` file
//!   (`src-tauri/src/lib.rs` setup). There is exactly one path scheme —
//!   [`resolve_app_data_dir`] reimplements Tauri's per-OS join using only
//!   `std::env`, because the CLI must not depend on `src-tauri` as a
//!   library (issue #159).
//! - The migrations readout is a READ-ONLY `MAX(schema_version)` query over
//!   a `SQLITE_OPEN_READ_ONLY` handle — the stub never creates, writes, or
//!   migrates the database.
//! - The keyring probe uses the same service namespace (`"nexora"`) as the
//!   desktop credential store
//!   (`src-tauri/src/infrastructure/providers/credentials.rs`) but a
//!   dedicated probe account that can never collide with a provider name,
//!   and prints only `AVAILABLE`/`UNAVAILABLE` — never values.
//! - Everything printed here is secret-free by construction: fixed labels,
//!   versions, counts, and fixed-vocabulary failure reasons. Raw OS/SQLite
//!   error text is never propagated (it may quote paths, which may carry
//!   user names).

use std::path::PathBuf;

use rusqlite::OpenFlags;

/// Desktop app-data identifier (`src-tauri/tauri.conf.json`).
const APP_IDENTIFIER: &str = "com.nexora.app";

/// Desktop database file name (`src-tauri/src/lib.rs` setup).
const DB_FILE_NAME: &str = "nexora.db";

/// Keyring service namespace shared with the desktop credential store.
const KEYRING_SERVICE: &str = "nexora";

/// Probe account that can never collide with a provider internal name.
const KEYRING_PROBE_ACCOUNT: &str = "__nexora_doctor_probe__";

/// Toolchain facts baked in at compile time by `build.rs`.
const BUILD_RUSTC_VERSION: &str = env!("NEXORA_BUILD_RUSTC_VERSION");
const BUILD_CARGO_VERSION: &str = env!("NEXORA_BUILD_CARGO_VERSION");
const BUILD_TARGET: &str = env!("NEXORA_BUILD_TARGET");

/// OS flavour for app-data resolution (mirrors Tauri's per-OS branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Resolve the desktop app-data directory using the same rules Tauri v2
/// applies for [`APP_IDENTIFIER`]:
///
/// - Linux: `$XDG_DATA_HOME/<id>`, else `$HOME/.local/share/<id>`;
/// - macOS: `$HOME/Library/Application Support/<id>`;
/// - Windows: `%APPDATA%\<id>`.
///
/// Inputs are explicit parameters (instead of reading the environment
/// directly) so every branch is unit-testable on any host. Returns `None`
/// when the required base directory is unknown.
fn resolve_app_data_dir_with(
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

/// Resolve the desktop app-data directory from the process environment.
fn resolve_app_data_dir() -> Option<PathBuf> {
    let platform = Platform::current()?;
    let home = std::env::var("HOME").ok();
    let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
    let appdata = std::env::var("APPDATA").ok();
    resolve_app_data_dir_with(
        platform,
        home.as_deref(),
        xdg_data_home.as_deref(),
        appdata.as_deref(),
    )
}

/// Migrations-state readout: `MAX(schema_version)` over a read-only handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatabaseState {
    /// No database file at the expected desktop location. The stub reports
    /// this honestly and never creates one.
    NotFound,
    /// The file exists; `schema_version` is the applied migration ceiling
    /// and `size_bytes` the on-disk size.
    Found {
        schema_version: i64,
        size_bytes: u64,
    },
    /// The file exists but could not be inspected read-only. The reason is
    /// fixed vocabulary — raw SQLite/OS error text is never surfaced (it
    /// may quote user-name-carrying paths).
    Unreadable { reason: &'static str },
}

/// Keyring availability: presence of a working OS backend, never values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyringStatus {
    Available,
    Unavailable,
}

impl std::fmt::Display for KeyringStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => write!(f, "AVAILABLE"),
            Self::Unavailable => write!(f, "UNAVAILABLE"),
        }
    }
}

/// Probe the OS keyring without ever exposing a secret value.
///
/// A dedicated probe account under the shared `"nexora"` service namespace
/// is touched: `NoEntry` (backend reachable, probe absent) and a readable
/// probe both mean the backend works; any other failure means it does not.
/// A returned secret — practically impossible for the probe account — is
/// dropped immediately and never printed, logged, or returned.
fn probe_keyring() -> KeyringStatus {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_PROBE_ACCOUNT) {
        Err(_) => KeyringStatus::Unavailable,
        // Both arms mean the backend works: a readable probe (practically
        // impossible — the value is dropped here, never printed) and
        // `NoEntry` (backend reachable, probe absent).
        Ok(entry) => match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => KeyringStatus::Available,
            Err(_) => KeyringStatus::Unavailable,
        },
    }
}

/// Read-only inspection of the desktop database. Never creates files.
fn inspect_database(app_data_dir: Option<&PathBuf>) -> DatabaseState {
    let db_path = app_data_dir.map(|dir| dir.join(DB_FILE_NAME));
    let Some(db_path) = db_path else {
        return DatabaseState::NotFound;
    };
    let size_bytes = match std::fs::metadata(&db_path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return DatabaseState::NotFound;
            }
            metadata.len()
        }
        Err(_) => return DatabaseState::NotFound,
    };
    let Ok(connection) =
        rusqlite::Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return DatabaseState::Unreadable {
            reason: "database file could not be opened read-only",
        };
    };
    match connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(schema_version) => DatabaseState::Found {
            schema_version,
            size_bytes,
        },
        Err(_) => DatabaseState::Unreadable {
            reason: "schema_version could not be read",
        },
    }
}

/// The collected doctor facts (plain data — rendering is separate so tests
/// can assert on the output contract without parsing internals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorReport {
    pub(crate) binary_name: &'static str,
    pub(crate) binary_version: &'static str,
    pub(crate) rustc_version: &'static str,
    pub(crate) cargo_version: &'static str,
    pub(crate) build_target: &'static str,
    pub(crate) app_data_dir: Option<PathBuf>,
    pub(crate) app_data_dir_exists: bool,
    pub(crate) database: DatabaseState,
    pub(crate) keyring: KeyringStatus,
}

/// Collect the doctor facts. Performs no network I/O, opens no TUI, and
/// writes nothing to disk.
pub(crate) fn collect() -> DoctorReport {
    let app_data_dir = resolve_app_data_dir();
    let app_data_dir_exists = app_data_dir
        .as_ref()
        .is_some_and(|dir| std::fs::metadata(dir).is_ok_and(|meta| meta.is_dir()));
    let database = inspect_database(app_data_dir.as_ref());
    DoctorReport {
        binary_name: "nexora",
        binary_version: env!("CARGO_PKG_VERSION"),
        rustc_version: BUILD_RUSTC_VERSION,
        cargo_version: BUILD_CARGO_VERSION,
        build_target: BUILD_TARGET,
        app_data_dir,
        app_data_dir_exists,
        database,
        keyring: probe_keyring(),
    }
}

/// Render the report as stable plain text: one `key: value` line per fact,
/// no ANSI escapes, no secrets. Safe to pipe, redirect, or scrape in CI.
pub(crate) fn render(report: &DoctorReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("nexora doctor\n");
    let _ = writeln!(
        out,
        "binary: {} {}",
        report.binary_name, report.binary_version
    );
    let _ = writeln!(out, "rustc: {} [build-time]", report.rustc_version);
    let _ = writeln!(out, "cargo: {} [build-time]", report.cargo_version);
    let _ = writeln!(out, "target: {}", report.build_target);
    match &report.app_data_dir {
        Some(dir) => {
            let state = if report.app_data_dir_exists {
                "exists"
            } else {
                "missing"
            };
            let _ = writeln!(out, "app_data_dir: {} ({state})", dir.display());
        }
        None => out.push_str("app_data_dir: unknown (platform base directory not set)\n"),
    }
    match &report.database {
        DatabaseState::NotFound => out.push_str(
            "database: not found (no database file at the desktop location; \
             this command never creates one)\n",
        ),
        DatabaseState::Found {
            schema_version,
            size_bytes,
        } => {
            let _ = writeln!(
                out,
                "database: found schema_version={schema_version} size_bytes={size_bytes}"
            );
        }
        DatabaseState::Unreadable { reason } => {
            let _ = writeln!(out, "database: unreadable ({reason})");
        }
    }
    let _ = writeln!(out, "keyring: {}", report.keyring);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_prefers_xdg_data_home() {
        assert_eq!(
            resolve_app_data_dir_with(
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
            resolve_app_data_dir_with(Platform::Linux, Some("/home/alice"), None, None),
            Some(PathBuf::from("/home/alice/.local/share").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn linux_empty_xdg_falls_back_to_home() {
        assert_eq!(
            resolve_app_data_dir_with(Platform::Linux, Some("/home/alice"), Some(""), None),
            Some(PathBuf::from("/home/alice/.local/share").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn linux_without_home_is_unknown() {
        assert_eq!(
            resolve_app_data_dir_with(Platform::Linux, None, None, None),
            None
        );
    }

    #[test]
    fn macos_uses_library_application_support() {
        assert_eq!(
            resolve_app_data_dir_with(Platform::Macos, Some("/Users/alice"), None, None),
            Some(PathBuf::from("/Users/alice/Library/Application Support").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn windows_uses_roaming_appdata() {
        assert_eq!(
            resolve_app_data_dir_with(
                Platform::Windows,
                None,
                None,
                Some("C:\\Users\\alice\\AppData\\Roaming")
            ),
            Some(PathBuf::from("C:\\Users\\alice\\AppData\\Roaming").join(APP_IDENTIFIER))
        );
    }

    #[test]
    fn render_covers_every_required_field() {
        let report = DoctorReport {
            binary_name: "nexora",
            binary_version: "0.1.0",
            rustc_version: "rustc 1.97.1",
            cargo_version: "cargo 1.97.1",
            build_target: "x86_64-unknown-linux-gnu",
            app_data_dir: Some(PathBuf::from("/tmp/nexora-test")),
            app_data_dir_exists: false,
            database: DatabaseState::NotFound,
            keyring: KeyringStatus::Unavailable,
        };
        let text = render(&report);
        for field in [
            "binary:",
            "rustc:",
            "cargo:",
            "target:",
            "app_data_dir:",
            "database:",
            "keyring: UNAVAILABLE",
        ] {
            assert!(text.contains(field), "missing field {field} in:\n{text}");
        }
        assert!(
            !text.contains('\u{1b}'),
            "doctor output must not contain ANSI escapes"
        );
    }

    #[test]
    fn render_found_database_reports_schema_version() {
        let report = DoctorReport {
            binary_name: "nexora",
            binary_version: "0.1.0",
            rustc_version: "rustc 1.97.1",
            cargo_version: "cargo 1.97.1",
            build_target: "x86_64-unknown-linux-gnu",
            app_data_dir: Some(PathBuf::from("/tmp/nexora-test")),
            app_data_dir_exists: true,
            database: DatabaseState::Found {
                schema_version: 14,
                size_bytes: 42,
            },
            keyring: KeyringStatus::Available,
        };
        let text = render(&report);
        assert!(text.contains("schema_version=14"));
        assert!(text.contains("keyring: AVAILABLE"));
    }

    #[test]
    fn render_never_propagates_raw_error_text() {
        // `Unreadable` carries fixed vocabulary only — synthesising it here
        // proves no caller can smuggle a path-bearing SQLite error through.
        let report = DoctorReport {
            binary_name: "nexora",
            binary_version: "0.1.0",
            rustc_version: "rustc 1.97.1",
            cargo_version: "cargo 1.97.1",
            build_target: "x86_64-unknown-linux-gnu",
            app_data_dir: None,
            app_data_dir_exists: false,
            database: DatabaseState::Unreadable {
                reason: "schema_version could not be read",
            },
            keyring: KeyringStatus::Unavailable,
        };
        let text = render(&report);
        assert!(text.contains("database: unreadable (schema_version could not be read)"));
    }
}
