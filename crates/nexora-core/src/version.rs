//! Three-manifest version parity (`package.json` / `Cargo.toml` /
//! `tauri.conf.json`).
//!
//! Extracted verbatim from the desktop release-readiness surface
//! (`src-tauri/src/application/release.rs`, issue #160). The Desktop
//! `ReleasePanel` badge and the CLI `doctor` readout are now two renderings of
//! the *same* facts, computed by the same functions — they can no longer
//! disagree about whether the three manifests are in parity.
//!
//! ## Pure by construction
//!
//! Everything here is a pure function over text, or over already-read bytes.
//! No filesystem walk, no network, no clock. [`read_manifest_versions`] is the
//! single impure step and it is deliberately thin: read the three files, then
//! hand the text to the pure parsers. A missing file is an honest unknown
//! (`None`), never an error — a production workspace folder is user data, not
//! a checkout (this is the desktop's existing rule, preserved exactly).
//!
//! ## Corrupt manifests must not poison parity
//!
//! A version longer than [`MAX_VERSION_CHARS`] or an unparseable document is
//! reported as *absent*, never as a distinct value. Otherwise a truncated or
//! hostile manifest could masquerade as a third distinct version and turn the
//! parity badge into noise.
//!
//! ## stdlib-only parsing
//!
//! JSON manifests parse through `serde_json`; `Cargo.toml` is read by a line
//! scanner over its `[package]` block — no TOML parser and no new dependency,
//! mirroring the desktop's binding for the dependency inventory.

use std::path::Path;

/// Manifest version caps: versions are short dotted strings; anything longer
/// is treated as absent (a corrupt manifest must not poison parity).
pub const MAX_VERSION_CHARS: usize = 64;

/// The three manifests that must agree, as workspace-root-relative paths.
///
/// Shared so the CLI's `doctor` and the Desktop `ReleasePanel` look at
/// *literally the same three files* — adding a fourth manifest is a
/// deliberate change to this list, not a silent divergence between surfaces.
pub const MANIFEST_PATHS: [&str; 3] = [
    "package.json",
    "src-tauri/Cargo.toml",
    "src-tauri/tauri.conf.json",
];

/// The three parsed manifest versions, in [`MANIFEST_PATHS`] order.
///
/// `None` means "absent or unusable" — a missing file, unreadable file, or a
/// document whose version field is missing / non-string / empty / over-long.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestVersions {
    /// `package.json` `version`.
    pub package_json: Option<String>,
    /// `src-tauri/Cargo.toml` `[package] version`.
    pub cargo: Option<String>,
    /// `src-tauri/tauri.conf.json` `version`.
    pub tauri_conf: Option<String>,
}

impl ManifestVersions {
    /// True only when all three manifest versions are present and equal.
    ///
    /// A missing manifest is a *disagree*: the Desktop panel renders *which*
    /// manifest is unknown from the individual fields, so collapsing it to a
    /// boolean `false` loses no information.
    #[must_use]
    pub fn agree(&self) -> bool {
        versions_agree(
            self.package_json.as_deref(),
            self.cargo.as_deref(),
            self.tauri_conf.as_deref(),
        )
    }
}

/// Parse the `version` string out of a JSON manifest (`package.json`,
/// `tauri.conf.json`). `None` for invalid JSON, a missing key, or a
/// non-string / over-long value.
#[must_use]
pub fn parse_json_manifest_version(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = value.get("version")?.as_str()?;
    if version.is_empty() || version.chars().count() > MAX_VERSION_CHARS {
        return None;
    }
    Some(version.to_string())
}

/// Parse the `[package]` `version` out of `Cargo.toml` with a stdlib-only
/// line scanner (no TOML parser — same binding as the desktop dependency
/// inventory). Only `version = "…"` lines *after* the `[package]` header and
/// *before* the next section header count, so dependency `version` requirements
/// never leak in. `None` when the block or a usable value is absent.
#[must_use]
pub fn parse_cargo_manifest_version(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("version") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = rest.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            let version = &value[1..value.len() - 1];
            if !version.is_empty() && version.chars().count() <= MAX_VERSION_CHARS {
                return Some(version.to_string());
            }
            return None;
        }
    }
    None
}

/// True only when all three manifest versions are present and equal.
#[must_use]
pub fn versions_agree(
    package_json: Option<&str>,
    cargo: Option<&str>,
    tauri_conf: Option<&str>,
) -> bool {
    match (package_json, cargo, tauri_conf) {
        (Some(first), Some(second), Some(third)) => first == second && second == third,
        _ => false,
    }
}

/// Read one manifest's text below `root`, or `None` when the file is missing or
/// unreadable. Best-effort by design: absent manifests are an honest unknown,
/// never an error.
fn read_manifest(root: &Path, relative: &str) -> Option<String> {
    std::fs::read_to_string(root.join(relative)).ok()
}

/// Collect the three manifest versions below `root`.
///
/// The single impure entry point of this module; everything downstream is
/// pure. Missing or unreadable manifests yield `None`, never an error.
#[must_use]
pub fn read_manifest_versions(root: &Path) -> ManifestVersions {
    ManifestVersions {
        package_json: read_manifest(root, MANIFEST_PATHS[0])
            .as_deref()
            .and_then(parse_json_manifest_version),
        cargo: read_manifest(root, MANIFEST_PATHS[1])
            .as_deref()
            .and_then(parse_cargo_manifest_version),
        tauri_conf: read_manifest(root, MANIFEST_PATHS[2])
            .as_deref()
            .and_then(parse_json_manifest_version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexora-core-version-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp root must be creatable");
        dir
    }

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("manifest has a parent"))
            .expect("parent dir must be creatable");
        std::fs::write(path, content).expect("manifest must be writable");
    }

    // ---- fixtures -------------------------------------------------------

    #[test]
    fn json_manifest_versions_parse_or_absent() {
        assert_eq!(
            parse_json_manifest_version(r#"{"name":"nexora","version":"1.5.0"}"#),
            Some("1.5.0".to_string())
        );
        assert_eq!(parse_json_manifest_version(r#"{"name":"nexora"}"#), None);
        assert_eq!(parse_json_manifest_version("not json"), None);
        assert_eq!(
            parse_json_manifest_version(r#"{"version":12}"#),
            None,
            "non-string versions are absent"
        );
        assert_eq!(
            parse_json_manifest_version(r#"{"version":""}"#),
            None,
            "empty versions are absent"
        );
    }

    #[test]
    fn over_long_json_version_is_absent_not_a_distinct_version() {
        let long = "1".repeat(MAX_VERSION_CHARS + 1);
        assert_eq!(
            parse_json_manifest_version(&format!(r#"{{"version":"{long}"}}"#)),
            None
        );
        // Exactly at the cap is still accepted.
        let at_cap = "1".repeat(MAX_VERSION_CHARS);
        assert_eq!(
            parse_json_manifest_version(&format!(r#"{{"version":"{at_cap}"}}"#)),
            Some(at_cap)
        );
    }

    #[test]
    fn cargo_manifest_version_reads_only_the_package_block() {
        let text = "[package]\nname = \"nexora\"\nversion = \"1.5.0\"\n\n\
            [dependencies]\nserde = { version = \"1.0\" }\n";
        assert_eq!(
            parse_cargo_manifest_version(text),
            Some("1.5.0".to_string())
        );
        // A dependency-only manifest must not leak a requirement as the app
        // version.
        let deps_only = "[dependencies]\nserde = { version = \"9.9.9\" }\n";
        assert_eq!(parse_cargo_manifest_version(deps_only), None);
        assert_eq!(
            parse_cargo_manifest_version("[package]\nname = \"x\"\n"),
            None
        );
        assert_eq!(parse_cargo_manifest_version(""), None);
    }

    #[test]
    fn cargo_section_header_resets_the_package_block() {
        // A `version` inside a nested table must never be mistaken for the
        // package version, in either direction.
        let text = "[package]\nname = \"x\"\n\n[package.metadata]\nversion = \"9.9.9\"\n";
        assert_eq!(parse_cargo_manifest_version(text), None);
    }

    // ---- parity ---------------------------------------------------------

    #[test]
    fn version_parity_needs_all_three_present_and_equal() {
        assert!(versions_agree(Some("1.5.0"), Some("1.5.0"), Some("1.5.0")));
        assert!(!versions_agree(Some("1.5.0"), Some("1.5.1"), Some("1.5.0")));
        assert!(!versions_agree(Some("1.5.0"), Some("1.5.0"), None));
        assert!(!versions_agree(None, None, None));
    }

    #[test]
    fn every_missing_manifest_is_a_disagree() {
        // All eight presence combinations, so a partially-readable workspace
        // can never accidentally report parity.
        let present = Some("2.0.0");
        let absent = None;
        for bits in 0..8 {
            let got = versions_agree(
                if bits & 1 == 0 { present } else { absent },
                if bits & 2 == 0 { present } else { absent },
                if bits & 4 == 0 { present } else { absent },
            );
            assert_eq!(got, bits == 0, "parity verdict wrong for mask {bits}");
        }
    }

    #[test]
    fn manifest_versions_agree_matches_the_free_function() {
        let versions = ManifestVersions {
            package_json: Some("2.0.0".into()),
            cargo: Some("2.0.0".into()),
            tauri_conf: Some("2.0.0".into()),
        };
        assert!(versions.agree());
        assert!(versions_agree(
            versions.package_json.as_deref(),
            versions.cargo.as_deref(),
            versions.tauri_conf.as_deref()
        ));
    }

    #[test]
    fn default_manifest_versions_are_absent_and_disagree() {
        let versions = ManifestVersions::default();
        assert_eq!(
            versions,
            ManifestVersions {
                package_json: None,
                cargo: None,
                tauri_conf: None,
            }
        );
        assert!(!versions.agree());
    }

    // ---- reading from disk ---------------------------------------------

    #[test]
    fn reading_an_empty_root_yields_three_absent_versions() {
        let root = temp_root("empty");
        let versions = read_manifest_versions(&root);
        assert_eq!(versions, ManifestVersions::default());
        assert!(!versions.agree());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reading_a_mismatched_workspace_reports_each_version_individually() {
        // The exact Desktop `ReleasePanel` "versions differ" shape: all three
        // present, values disagree. `doctor` and the panel render this same
        // struct, so this fixture pins the shared truth.
        let root = temp_root("mismatch");
        write(
            &root,
            MANIFEST_PATHS[0],
            r#"{"name":"nexora","version":"2.0.0"}"#,
        );
        write(
            &root,
            MANIFEST_PATHS[1],
            "[package]\nname = \"nexora\"\nversion = \"2.0.1\"\n",
        );
        write(&root, MANIFEST_PATHS[2], r#"{"version":"2.0.2"}"#);
        let versions = read_manifest_versions(&root);
        assert_eq!(versions.package_json.as_deref(), Some("2.0.0"));
        assert_eq!(versions.cargo.as_deref(), Some("2.0.1"));
        assert_eq!(versions.tauri_conf.as_deref(), Some("2.0.2"));
        assert!(!versions.agree());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reading_a_parity_workspace_agrees() {
        let root = temp_root("parity");
        write(&root, MANIFEST_PATHS[0], r#"{"version":"2.0.0"}"#);
        write(&root, MANIFEST_PATHS[1], "[package]\nversion = \"2.0.0\"\n");
        write(&root, MANIFEST_PATHS[2], r#"{"version":"2.0.0"}"#);
        let versions = read_manifest_versions(&root);
        assert!(versions.agree());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_manifest_does_not_poison_the_other_two() {
        // Truncated JSON is absent, not a third distinct value — the other two
        // versions are still reported verbatim.
        let root = temp_root("corrupt");
        write(&root, MANIFEST_PATHS[0], r#"{"version":"2.0.0""#);
        write(&root, MANIFEST_PATHS[1], "[package]\nversion = \"2.0.0\"\n");
        write(&root, MANIFEST_PATHS[2], r#"{"version":"2.0.0"}"#);
        let versions = read_manifest_versions(&root);
        assert_eq!(versions.package_json, None);
        assert_eq!(versions.cargo.as_deref(), Some("2.0.0"));
        assert_eq!(versions.tauri_conf.as_deref(), Some("2.0.0"));
        assert!(!versions.agree());
        let _ = std::fs::remove_dir_all(&root);
    }
}
