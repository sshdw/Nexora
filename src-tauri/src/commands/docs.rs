//! Docs-manifest IPC command: the version-anchored manifest behind the
//! in-app Docs panel's migration guide.
//!
//! Thin translation only (ARCHITECTURE.md §5): the command takes no arguments,
//! reads no database state, and opens no files — it projects the
//! compile-time [`MIGRATIONS`](crate::infrastructure::database::MIGRATIONS)
//! list into the sorted, de-duplicated version spine the guide renders
//! against. The frontend owns all prose (EN+RU string catalog, one note per
//! version); the backend owns the version truth, so the guide can neither
//! list a migration that does not exist nor silently miss one that does.
//! There is no setter and no table: docs are content, not state.

use crate::infrastructure::database::MIGRATIONS;

/// One schema milestone in the migration guide: the migration version only.
/// The per-version note renders frontend-side from the string catalog
/// (`docs.migN`); versions without a catalog note fall back to a bare `vN`
/// label, so a catalog lag can never blank the guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DocsMigration {
    pub version: i64,
}

/// The docs manifest: the highest known schema version plus the full ordered
/// migration spine. Field names stay `snake_case` like every other backend
/// struct (the tauri.ts casing rule).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DocsManifest {
    pub schema_version: i64,
    pub migrations: Vec<DocsMigration>,
}

/// Read-only docs manifest (`docs_manifest`): the ordered migration spine
/// computed from the registered migrations. Infallible by construction —
/// the source is a compile-time constant, so there is no I/O to fail.
#[tauri::command]
pub(crate) fn docs_manifest() -> DocsManifest {
    let mut versions: Vec<i64> = MIGRATIONS.iter().map(|(version, _)| *version).collect();
    versions.sort_unstable();
    versions.dedup();
    DocsManifest {
        schema_version: versions.iter().copied().max().unwrap_or(0),
        migrations: versions
            .into_iter()
            .map(|version| DocsMigration { version })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Honesty anchor for the migration guide: the manifest the Docs panel
    /// renders MUST be exactly the registered migration set — same versions,
    /// same order, no gaps, no extras. Any future migration that extends
    /// `MIGRATIONS` flows into the guide automatically; this test fails if
    /// the command ever filters, reorders, or fabricates versions.
    #[test]
    fn docs_manifest_covers_every_migration_version() {
        let manifest = docs_manifest();
        let expected: Vec<i64> = MIGRATIONS.iter().map(|(version, _)| *version).collect();
        let listed: Vec<i64> = manifest
            .migrations
            .iter()
            .map(|entry| entry.version)
            .collect();
        assert_eq!(
            listed, expected,
            "docs manifest must list every migration in order"
        );
        assert_eq!(
            manifest.schema_version,
            expected.iter().copied().max().unwrap_or(0),
            "schema_version must be the highest registered migration"
        );
        assert!(
            expected.windows(2).all(|pair| pair[0] + 1 == pair[1]),
            "migrations must be gap-free from v1, found {expected:?}"
        );
    }

    /// Static wiring check: the docs command stays a thin projection over
    /// the migration registry — no SQL, no filesystem access, no database
    /// handle. Needles are built with `concat!` so this test's own source
    /// never matches them verbatim.
    #[test]
    fn docs_command_stays_thin_over_the_registry() {
        const SOURCE: &str = include_str!("docs.rs");
        assert!(
            SOURCE.contains("MIGRATIONS.iter()"),
            "commands/docs.rs must project the migration registry"
        );
        for needle in [
            concat!("SELECT", " "),
            concat!("INSERT", " "),
            concat!("CREATE", " "),
            concat!("fs", "::write"),
            concat!("fs", "::read"),
            concat!("State<'_, Data", "base>"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "commands/docs.rs must hold no SQL/IO/DB logic, found {needle:?}"
            );
        }
    }
}
