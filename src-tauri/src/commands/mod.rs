//! Tauri command layer (Phase 10.2 — Tauri Command Layer).
//!
//! This module translates Tauri IPC inputs/outputs to and from the existing
//! application-layer services and infrastructure. Its responsibilities are
//! deliberately narrow (ARCHITECTURE.md §5):
//!
//! - declare `#[tauri::command]` functions over the existing services;
//! - convert classified application errors into safe, serializable
//!   [`error::CommandError`] values without leaking credentials or secrets;
//! - provide no business logic, no repository access, and no new behavior.
//!
//! The commands are wired into the application through
//! `tauri::Builder::invoke_handler` in [`crate::run`].

pub mod agent;
pub mod attachments;
pub mod compat;
pub mod confirmations;
pub mod context;
pub mod conversations;
pub mod credentials;
pub mod data_management;
pub mod debt;
pub mod dep_refactor;
pub mod docs;
pub mod error;
pub mod flags;
pub mod github;
pub mod import_export;
pub mod mcp;
pub mod privacy;
pub mod prompt_context;
pub mod prompts;
pub mod providers;
pub mod release;
pub mod repo_audit;
pub mod search;
pub mod settings;
pub mod system;
pub mod tasks;
pub mod terminal;
pub mod testgen;
pub mod version_control;
pub mod workspace;

pub(crate) mod sandbox;

/// Cross-layer static wiring assertions.
///
/// These checks read shell-owned source files (`commands/*`, the webview
/// capability manifest), so they live with the command layer after the
/// application/infrastructure extraction. They are moved verbatim from
/// `application::workspace` and `application::import::conversations` — no
/// assertion was dropped or weakened.
#[cfg(test)]
mod wiring_tests {
    /// The git panel and the audit command stay on the shared resolver, so
    /// switching the active root moves both features with no per-feature
    /// changes. Needles use `concat!` so this test's own source never
    /// matches them verbatim.
    #[test]
    fn root_aware_features_share_the_active_root_resolver() {
        const VCS: &str = include_str!("version_control.rs");
        const AUDIT: &str = include_str!("repo_audit.rs");
        const RESOLVER: &str = concat!("resolve", "_workspace_root");
        assert!(
            VCS.contains(RESOLVER),
            "version_control commands must resolve the shared active root"
        );
        assert!(
            AUDIT.contains(RESOLVER),
            "repo_audit must resolve the shared active root"
        );
        for source in [VCS, AUDIT] {
            for needle in [
                concat!("C", ":\\"),
                concat!("/home", "/"),
                concat!("canonical", "ize"),
            ] {
                assert!(
                    !source.contains(needle),
                    "root-aware commands must not hardcode roots, found {needle:?}"
                );
            }
        }
    }

    #[test]
    fn frontend_capability_has_no_home_read_scope_nor_opener() {
        // SEC-01 (NEX-SEC-012 closed): the `fs` plugin carries **no read
        // scope at all**. The former `$HOME` / `$HOME/**/*` pair
        // (`fs:allow-read-text-file` + `fs:scope`) is gone — the import
        // document is read backend-side (`read_import_document`) and the
        // attachment size is derived server-side, so no renderer JS reads
        // the home tree through the `fs` plugin. Only `fs:allow-stat`
        // remains (a stat needs no path scope beyond the picked file).
        // SEC-08: the `opener` grant is gone too, so renderer JS cannot ask
        // the OS to open URLs or handlers.
        const CAPABILITY: &str = include_str!("../../capabilities/default.json");
        let document: serde_json::Value =
            serde_json::from_str(CAPABILITY).expect("capability is valid JSON");
        let permissions = document
            .get("permissions")
            .and_then(serde_json::Value::as_array)
            .expect("permissions array present");
        for forbidden in [
            "fs:allow-read-text-file",
            "fs:scope",
            "fs:allow-read",
            "opener:default",
        ] {
            assert!(
                !permissions.iter().any(|entry| entry == forbidden
                    || entry.get("identifier") == Some(&serde_json::json!(forbidden))),
                "the {forbidden} grant must be gone"
            );
        }
        // No `$HOME` scope survives anywhere in the capability file.
        let rendered = serde_json::to_string(&document).expect("serialize capability");
        assert!(
            !rendered.contains("$HOME"),
            "no home-tree scope may remain in the capability file: {rendered:?}"
        );
    }
}
