//! `nexora doctor`: offline, read-only diagnostics for the Nexora CLI stub.
//!
//! Design notes (binding for later phases):
//!
//! - The app-data directory comes from `nexora_core::storage::app_data`, the
//!   SAME resolver the desktop startup uses (issue #161). There is exactly
//!   one path scheme and one database filename in the workspace; the CLI
//!   never guesses either.
//! - The migrations readout is a READ-ONLY `MAX(schema_version)` query over
//!   a `SQLITE_OPEN_READ_ONLY` handle — the stub never creates, writes, or
//!   migrates the database.
//! - The keyring probe uses the same service namespace (`"nexora"`) as the
//!   desktop credential store
//!   (`src-tauri/src/infrastructure/providers/credentials.rs`) but a
//!   dedicated probe account that can never collide with a provider name,
//!   and prints only `AVAILABLE`/`UNAVAILABLE` — never values.
//! - The version-parity readout consumes `nexora_core::version` (issue #160),
//!   the SAME functions the desktop `ReleasePanel` badge is computed from, so
//!   the two surfaces cannot report different verdicts about the manifests.
//!   An installed CLI usually has no checkout to inspect; the search walks up
//!   from the working directory and honestly reports `unknown` when there is
//!   none, exactly as Desktop renders an absent manifest.
//! - Everything printed here is secret-free by construction: fixed labels,
//!   versions, counts, and fixed-vocabulary failure reasons. Raw OS/SQLite
//!   error text is never propagated (it may quote paths, which may carry
//!   user names).

use std::path::PathBuf;

use nexora_core::storage::app_data;
use nexora_core::version::ManifestVersions;
use rusqlite::OpenFlags;

/// Keyring service namespace shared with the desktop credential store.
const KEYRING_SERVICE: &str = "nexora";

/// Probe account that can never collide with a provider internal name.
const KEYRING_PROBE_ACCOUNT: &str = "__nexora_doctor_probe__";

/// Toolchain facts baked in at compile time by `build.rs`.
const BUILD_RUSTC_VERSION: &str = env!("NEXORA_BUILD_RUSTC_VERSION");
const BUILD_CARGO_VERSION: &str = env!("NEXORA_BUILD_CARGO_VERSION");
const BUILD_TARGET: &str = env!("NEXORA_BUILD_TARGET");

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
    let db_path = app_data_dir.map(|dir| dir.join(app_data::DB_FILE_NAME));
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
    pub(crate) manifests: ManifestVersions,
    pub(crate) app_data_dir: Option<PathBuf>,
    pub(crate) app_data_dir_exists: bool,
    pub(crate) database: DatabaseState,
    pub(crate) keyring: KeyringStatus,
}

/// Walk up from `start` looking for a directory holding all three manifests.
///
/// An installed CLI normally has no checkout, so this honestly returns `None`
/// and `doctor` reports the versions as unknown — the same way Desktop's
/// `ReleasePanel` renders an absent manifest. The walk is bounded by the
/// filesystem root and touches only the shared manifest paths.
fn find_manifest_root(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(candidate) = dir {
        let holds_all = nexora_core::version::MANIFEST_PATHS
            .iter()
            .all(|relative| candidate.join(relative).is_file());
        if holds_all {
            return Some(candidate.to_path_buf());
        }
        dir = candidate.parent();
    }
    None
}

/// Collect the doctor facts. Performs no network I/O, opens no TUI, and
/// writes nothing to disk.
pub(crate) fn collect() -> DoctorReport {
    let app_data_dir = app_data::app_data_dir();
    let app_data_dir_exists = app_data_dir
        .as_ref()
        .is_some_and(|dir| std::fs::metadata(dir).is_ok_and(|meta| meta.is_dir()));
    let database = inspect_database(app_data_dir.as_ref());
    let manifests = std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(find_manifest_root)
        .map_or_else(ManifestVersions::default, |root| {
            nexora_core::version::read_manifest_versions(&root)
        });
    DoctorReport {
        binary_name: "nexora",
        binary_version: env!("CARGO_PKG_VERSION"),
        rustc_version: BUILD_RUSTC_VERSION,
        cargo_version: BUILD_CARGO_VERSION,
        build_target: BUILD_TARGET,
        manifests,
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
    // Version-manifest parity: the exact fact the Desktop `ReleasePanel`
    // "versions" badge is computed from, read through `nexora_core::version`.
    // An absent manifest prints `unknown` (never an invented value), which is
    // what Desktop renders for a missing manifest too.
    let versions = &report.manifests;
    let _ = writeln!(
        out,
        "versions: package.json={} src-tauri/Cargo.toml={} src-tauri/tauri.conf.json={} agree={}",
        versions.package_json.as_deref().unwrap_or("unknown"),
        versions.cargo.as_deref().unwrap_or("unknown"),
        versions.tauri_conf.as_deref().unwrap_or("unknown"),
        versions.agree()
    );
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

    fn report_with(manifests: ManifestVersions) -> DoctorReport {
        DoctorReport {
            binary_name: "nexora",
            binary_version: "0.1.0",
            rustc_version: "rustc 1.97.1",
            cargo_version: "cargo 1.97.1",
            build_target: "x86_64-unknown-linux-gnu",
            manifests,
            app_data_dir: Some(PathBuf::from("/tmp/nexora-test")),
            app_data_dir_exists: false,
            database: DatabaseState::NotFound,
            keyring: KeyringStatus::Unavailable,
        }
    }

    #[test]
    fn render_covers_every_required_field() {
        let report = report_with(ManifestVersions::default());
        let text = render(&report);
        for field in [
            "binary:",
            "rustc:",
            "cargo:",
            "target:",
            "versions:",
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
        let mut report = report_with(ManifestVersions::default());
        report.app_data_dir_exists = true;
        report.database = DatabaseState::Found {
            schema_version: 14,
            size_bytes: 42,
        };
        report.keyring = KeyringStatus::Available;
        let text = render(&report);
        assert!(text.contains("schema_version=14"));
        assert!(text.contains("keyring: AVAILABLE"));
    }

    #[test]
    fn render_never_propagates_raw_error_text() {
        // `Unreadable` carries fixed vocabulary only — synthesising it here
        // proves no caller can smuggle a path-bearing SQLite error through.
        let mut report = report_with(ManifestVersions::default());
        report.app_data_dir = None;
        report.database = DatabaseState::Unreadable {
            reason: "schema_version could not be read",
        };
        let text = render(&report);
        assert!(text.contains("database: unreadable (schema_version could not be read)"));
    }

    // ---- ReleasePanel parity (issue #160) --------------------------------

    #[test]
    fn version_line_matches_release_panel_facts_when_in_parity() {
        // The Desktop `ReleasePanel` "versions" badge is `pass` exactly when
        // `versions_agree` is true and `package_json_version` is present.
        // `doctor` must render the same verdict from the same struct.
        let report = report_with(ManifestVersions {
            package_json: Some("2.0.0".into()),
            cargo: Some("2.0.0".into()),
            tauri_conf: Some("2.0.0".into()),
        });
        assert!(report.manifests.agree(), "fixture must be in parity");
        let text = render(&report);
        assert!(
            text.contains(
                "versions: package.json=2.0.0 src-tauri/Cargo.toml=2.0.0 \
                 src-tauri/tauri.conf.json=2.0.0 agree=true"
            ),
            "doctor must report the ReleasePanel parity facts:\n{text}"
        );
    }

    #[test]
    fn version_line_reports_a_mismatch_as_disagree() {
        // Desktop renders `versionsDiffer` when all three are present but
        // unequal; `doctor` must say `agree=false` and name every version.
        let report = report_with(ManifestVersions {
            package_json: Some("2.0.0".into()),
            cargo: Some("2.0.1".into()),
            tauri_conf: Some("2.0.2".into()),
        });
        let text = render(&report);
        assert!(
            text.contains("agree=false"),
            "mismatch must not agree:\n{text}"
        );
        for version in ["2.0.0", "2.0.1", "2.0.2"] {
            assert!(text.contains(version), "missing {version} in:\n{text}");
        }
    }

    #[test]
    fn version_line_reports_an_absent_manifest_as_unknown_never_invented() {
        // Desktop renders `versionsUnknown` (a warning, not a failure) when a
        // manifest is missing. `doctor` must print `unknown`, not a guess.
        let report = report_with(ManifestVersions {
            package_json: Some("2.0.0".into()),
            cargo: Some("2.0.0".into()),
            tauri_conf: None,
        });
        assert!(!report.manifests.agree());
        let text = render(&report);
        assert!(
            text.contains("src-tauri/tauri.conf.json=unknown"),
            "absent manifest must render unknown:\n{text}"
        );
        assert!(
            text.contains("agree=false"),
            "absent manifest cannot agree:\n{text}"
        );
    }

    #[test]
    fn version_line_with_no_checkout_is_all_unknown() {
        let text = render(&report_with(ManifestVersions::default()));
        assert_eq!(
            text.lines()
                .find(|line| line.starts_with("versions:"))
                .expect("versions line"),
            "versions: package.json=unknown src-tauri/Cargo.toml=unknown \
             src-tauri/tauri.conf.json=unknown agree=false"
        );
    }

    #[test]
    fn manifest_root_search_stops_at_a_workspace_boundary() {
        let root = std::env::temp_dir().join(format!("nexora-doctor-root-{}", std::process::id()));
        let nested = root.join("apps/nexora-cli");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src-tauri")).expect("fixture tree");
        std::fs::create_dir_all(&nested).expect("fixture tree");
        std::fs::write(root.join("package.json"), r#"{"version":"9.9.9"}"#).expect("write");
        std::fs::write(
            root.join("src-tauri/Cargo.toml"),
            "[package]\nversion = \"9.9.9\"\n",
        )
        .expect("write");
        std::fs::write(
            root.join("src-tauri/tauri.conf.json"),
            r#"{"version":"9.9.9"}"#,
        )
        .expect("write");

        let found = find_manifest_root(&nested).expect("root must be discovered by walking up");
        assert_eq!(
            found, root,
            "the search must walk up to the workspace root, not stop at the crate"
        );
        let versions = nexora_core::version::read_manifest_versions(&found);
        assert!(versions.agree(), "fixture manifests must agree");
        let _ = std::fs::remove_dir_all(&root);
    }
}
