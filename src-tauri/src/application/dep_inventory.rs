//! Dependency inventory: read-only cargo + npm tables for the workspace.
//!
//! Parse-substrate decision (Phase 1, binding — no new dependencies):
//! `Cargo.toml` carries no TOML parser and the slice forbids new cargo deps,
//! so `src-tauri/Cargo.lock` is read with a stdlib-only line scanner over its
//! `[[package]]` blocks (`name` / `version` / optional `source` lines) — the
//! same substring discipline as the repo-audit engine
//! ([`crate::application::repo_audit`]). `package-lock.json` is real JSON, so
//! it parses through the existing `serde_json` dependency (no new dep). The
//! manifests (`Cargo.toml`, `package.json`) are NOT parsed: the tables report
//! locked (resolved) versions from the lockfiles only — declared ranges never
//! appear, and the panel says so.
//!
//! Read-only contract: this module only reads two lockfiles below the
//! workspace root (`read_to_string`); it never creates, writes, renames, or
//! deletes anything. A missing or unreadable lockfile yields an empty table,
//! never an error — the inventory is best-effort, and the panel renders the
//! empty state. Errors are fixed-vocabulary with no paths (paths may contain
//! user names).
//!
//! Source vocabulary (fixed, per ecosystem): cargo entries report
//! `"crates.io"` (registry source), `"git"` (`git+` source), `"local"` (no
//! source line — a path/workspace member), or `"unknown"`; npm entries report
//! `"registry"`, `"git"`, `"local"` (a `link:` entry), or `"unknown"`. The
//! frontend renders labels for exactly these tokens and echoes anything else
//! defensively.

use std::path::Path;

use serde::Serialize;

/// Most entries kept per ecosystem table; the remainder is reported in the
/// overflow count (a count, never content).
pub(crate) const MAX_DEP_ENTRIES: usize = 500;

/// One locked dependency: resolved name + version plus a fixed-vocabulary
/// source label (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DepEntry {
    /// Resolved package name, as written in the lockfile.
    pub name: String,
    /// Resolved version (`"unknown"` when the lockfile carries none).
    pub version: String,
    /// Fixed-vocabulary source label (ecosystem-specific, see module docs).
    pub source: String,
}

/// One read-only inventory run: capped cargo + npm tables with totals.
/// Totals always cover the whole lockfile; lists stay capped with overflow
/// counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DepInventory {
    /// Cargo packages sorted by name, capped at [`MAX_DEP_ENTRIES`].
    pub cargo: Vec<DepEntry>,
    /// Cargo packages in the lockfile (listed + overflow combined).
    pub cargo_total: usize,
    /// Cargo entries omitted by the cap (0 when everything fit).
    pub cargo_overflow: usize,
    /// npm packages sorted by name, capped at [`MAX_DEP_ENTRIES`].
    pub npm: Vec<DepEntry>,
    /// npm packages in the lockfile (listed + overflow combined).
    pub npm_total: usize,
    /// npm entries omitted by the cap (0 when everything fit).
    pub npm_overflow: usize,
}

/// Secret-free failures for the inventory scan. Variants carry no payload, so
/// formatting one can never leak a path, file content, or credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DepInventoryError {
    /// The workspace root is not a readable directory.
    InvalidRoot,
    /// A filesystem read failed for the workspace root itself.
    Io,
}

impl std::fmt::Display for DepInventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRoot => write!(f, "the workspace folder is not available for inventory"),
            Self::Io => write!(f, "the dependency inventory could not read the workspace"),
        }
    }
}

impl std::error::Error for DepInventoryError {}

/// Unquote a `key = "value"` line: the text between the first and last
/// double-quote. `None` when the line carries no quoted value.
#[must_use]
fn unquote(line: &str) -> Option<&str> {
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    if end <= start {
        return None;
    }
    Some(&line[start + 1..end])
}

/// Label one cargo `source` line value into the fixed vocabulary.
#[must_use]
fn cargo_source_label(source: Option<&str>) -> &'static str {
    match source {
        None => "local",
        Some(raw) if raw.contains("registry+") => "crates.io",
        Some(raw) if raw.starts_with("git+") => "git",
        Some(_) => "unknown",
    }
}

/// Scan `Cargo.lock` text: one entry per `[[package]]` block. Blocks without
/// a `name` line are skipped (never invented); a missing `version` reads
/// `"unknown"`.
#[must_use]
fn parse_cargo_lock(text: &str) -> Vec<DepEntry> {
    let mut entries: Vec<DepEntry> = Vec::new();
    let mut name: Option<&str> = None;
    let mut version: Option<&str> = None;
    let mut source: Option<&str> = None;
    let mut in_package = false;
    let flush = |entries: &mut Vec<DepEntry>,
                 name: Option<&str>,
                 version: Option<&str>,
                 source: Option<&str>| {
        if let Some(name) = name {
            entries.push(DepEntry {
                name: name.to_string(),
                version: version.unwrap_or("unknown").to_string(),
                source: cargo_source_label(source).to_string(),
            });
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            if in_package {
                flush(&mut entries, name, version, source);
            }
            in_package = true;
            name = None;
            version = None;
            source = None;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name =") {
            name = unquote(rest);
        } else if let Some(rest) = trimmed.strip_prefix("version =") {
            version = unquote(rest);
        } else if let Some(rest) = trimmed.strip_prefix("source =") {
            source = unquote(rest);
        }
    }
    if in_package {
        flush(&mut entries, name, version, source);
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Label one npm `resolved` value into the fixed vocabulary. `None` (no
/// `resolved` field and not a link) reads `"unknown"`.
#[must_use]
fn npm_source_label(resolved: Option<&str>, linked: bool) -> &'static str {
    if linked {
        return "local";
    }
    match resolved {
        Some(raw) if raw.contains("registry.npmjs.org") || raw.starts_with("https://") => {
            "registry"
        }
        Some(raw) if raw.starts_with("git+") || raw.starts_with("git://") => "git",
        None | Some(_) => "unknown",
    }
}

/// Read the npm `packages` map: one entry per top-level `node_modules/<name>`
/// key (nested `node_modules/<a>/node_modules/<b>` duplicates are skipped —
/// the table lists each install root once). A missing `version` reads
/// `"unknown"`.
#[must_use]
fn parse_npm_packages(packages: &serde_json::Map<String, serde_json::Value>) -> Vec<DepEntry> {
    let mut entries: Vec<DepEntry> = Vec::new();
    for (key, meta) in packages {
        if key.is_empty() {
            continue;
        }
        let Some(rest) = key.strip_prefix("node_modules/") else {
            continue;
        };
        if rest.is_empty() || rest.contains("/node_modules/") {
            continue;
        }
        let version = meta
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let resolved = meta.get("resolved").and_then(serde_json::Value::as_str);
        let linked = meta
            .get("link")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        entries.push(DepEntry {
            name: rest.to_string(),
            version: version.to_string(),
            source: npm_source_label(resolved, linked).to_string(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Cap one sorted table at [`MAX_DEP_ENTRIES`], returning the kept list plus
/// the omitted count (0 when everything fit).
fn cap_entries(entries: &mut Vec<DepEntry>) -> usize {
    let overflow = entries.len().saturating_sub(MAX_DEP_ENTRIES);
    entries.truncate(MAX_DEP_ENTRIES);
    overflow
}

/// Run the read-only dependency inventory over the workspace `root`: the
/// cargo table from `<root>/src-tauri/Cargo.lock` and the npm table from
/// `<root>/package-lock.json`. Missing, unreadable, or unparsable lockfiles
/// yield empty tables — never errors.
///
/// # Errors
///
/// Returns [`DepInventoryError::InvalidRoot`] when `root` is not a directory
/// and [`DepInventoryError::Io`] when the root cannot be listed.
pub(crate) fn inventory_workspace(root: &Path) -> Result<DepInventory, DepInventoryError> {
    if !root.is_dir() {
        return Err(DepInventoryError::InvalidRoot);
    }
    if root.read_dir().is_err() {
        return Err(DepInventoryError::Io);
    }
    let cargo_lock = root.join("src-tauri").join("Cargo.lock");
    let mut cargo = if cargo_lock.is_file() {
        let text = std::fs::read_to_string(&cargo_lock).map_err(|_| DepInventoryError::Io)?;
        parse_cargo_lock(&text)
    } else {
        Vec::new()
    };
    let npm_lock = root.join("package-lock.json");
    let mut npm = if npm_lock.is_file() {
        let text = std::fs::read_to_string(&npm_lock).map_err(|_| DepInventoryError::Io)?;
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| value.get("packages").cloned())
            .and_then(|packages| packages.as_object().cloned())
            .map_or_else(Vec::new, |packages| parse_npm_packages(&packages))
    } else {
        Vec::new()
    };
    let cargo_total = cargo.len();
    let npm_total = npm.len();
    let cargo_overflow = cap_entries(&mut cargo);
    let npm_overflow = cap_entries(&mut npm);
    Ok(DepInventory {
        cargo,
        cargo_total,
        cargo_overflow,
        npm,
        npm_total,
        npm_overflow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "nexora-dep-inventory-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("test workspace creates");
        root
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        use std::io::Write as _;
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("test parent creates");
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&full)
            .expect("test file opens");
        file.write_all(content.as_bytes())
            .expect("test file writes");
    }

    fn with_cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cargo_lock_parses_packages_with_source_labels() {
        let entries = parse_cargo_lock(
            "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"local-crate\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"git-dep\"\nversion = \"0.2.0\"\nsource = \"git+https://example.com/repo.git\"\n",
        );
        assert_eq!(entries.len(), 3);
        // Sorted by name.
        assert_eq!(entries[0].name, "git-dep");
        assert_eq!(entries[0].source, "git");
        assert_eq!(entries[1].name, "local-crate");
        assert_eq!(entries[1].source, "local");
        assert_eq!(entries[2].name, "serde");
        assert_eq!(entries[2].version, "1.0.0");
        assert_eq!(entries[2].source, "crates.io");
    }

    #[test]
    fn cargo_lock_skips_nameless_blocks_and_defaults_version() {
        let entries = parse_cargo_lock("[[package]]\nversion = \"1.0.0\"\n");
        assert!(
            entries.is_empty(),
            "nameless blocks are skipped, {entries:?}"
        );
        let entries = parse_cargo_lock("[[package]]\nname = \"bare\"\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].version, "unknown");
        assert_eq!(entries[0].source, "local");
    }

    #[test]
    fn npm_packages_map_parses_top_level_only() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"packages": {"": {"version": "1.0.0"}, "node_modules/react": {"version": "19.0.0", "resolved": "https://registry.npmjs.org/react/-/react-19.0.0.tgz"}, "node_modules/a/node_modules/b": {"version": "1.0.0"}, "node_modules/linked": {"version": "1.0.0", "link": true}, "node_modules/gitter": {"version": "1.0.0", "resolved": "git+https://example.com/r.git"}}}"#,
        )
        .expect("fixture parses");
        let packages = value
            .get("packages")
            .and_then(serde_json::Value::as_object)
            .expect("packages is a map")
            .clone();
        let entries = parse_npm_packages(&packages);
        assert_eq!(entries.len(), 3, "root + nested dup skipped, {entries:?}");
        assert_eq!(entries[0].name, "gitter");
        assert_eq!(entries[0].source, "git");
        assert_eq!(entries[1].name, "linked");
        assert_eq!(entries[1].source, "local");
        assert_eq!(entries[2].name, "react");
        assert_eq!(entries[2].version, "19.0.0");
        assert_eq!(entries[2].source, "registry");
    }

    #[test]
    fn tables_cap_with_overflow() {
        let mut entries: Vec<DepEntry> = (0..(MAX_DEP_ENTRIES + 4))
            .map(|index| DepEntry {
                name: format!("dep-{index:05}"),
                version: "1.0.0".to_string(),
                source: "crates.io".to_string(),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let overflow = cap_entries(&mut entries);
        assert_eq!(entries.len(), MAX_DEP_ENTRIES);
        assert_eq!(overflow, 4);
        assert_eq!(
            entries[0].name, "dep-00000",
            "truncation keeps the sorted head"
        );
    }

    #[test]
    fn missing_lockfiles_yield_empty_tables() {
        let root = test_root();
        let report = inventory_workspace(&root).expect("inventory runs");
        assert_eq!(report.cargo_total, 0);
        assert_eq!(report.npm_total, 0);
        assert!(report.cargo.is_empty());
        assert!(report.npm.is_empty());
        with_cleanup(&root);
    }

    #[test]
    fn inventory_reads_both_lockfiles() {
        let root = test_root();
        write_file(
            &root,
            "src-tauri/Cargo.lock",
            "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        );
        write_file(
            &root,
            "package-lock.json",
            r#"{"packages": {"node_modules/react": {"version": "19.0.0"}}}"#,
        );
        let report = inventory_workspace(&root).expect("inventory runs");
        assert_eq!(report.cargo_total, 1);
        assert_eq!(report.cargo[0].name, "serde");
        assert_eq!(report.npm_total, 1);
        assert_eq!(report.npm[0].name, "react");
        assert_eq!(report.npm[0].source, "unknown");
        with_cleanup(&root);
    }

    #[test]
    fn malformed_npm_lock_yields_an_empty_npm_table() {
        let root = test_root();
        write_file(&root, "package-lock.json", "{not json");
        let report = inventory_workspace(&root).expect("inventory runs");
        assert!(report.npm.is_empty());
        with_cleanup(&root);
    }

    #[test]
    fn invalid_root_is_secret_free() {
        let missing = std::env::temp_dir().join("nexora-dep-inventory-test-missing-dir-xyz");
        let err = inventory_workspace(&missing).expect_err("a missing root must fail");
        assert_eq!(err, DepInventoryError::InvalidRoot);
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("missing-dir-xyz"),
            "errors carry no paths, rendered {rendered:?}"
        );
    }

    #[test]
    fn scan_is_read_only() {
        let root = test_root();
        write_file(
            &root,
            "src-tauri/Cargo.lock",
            "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n",
        );
        let before: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
            .expect("root lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        let report = inventory_workspace(&root).expect("inventory runs");
        assert_eq!(report.cargo_total, 1);
        let after: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
            .expect("root lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(before, after, "the scan creates no files");
        with_cleanup(&root);
    }
}
