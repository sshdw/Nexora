//! `nexora-core` — the shared, interface-free core of Nexora (issue #160,
//! phase 1 of the CLI stream).
//!
//! ## Why this crate exists
//!
//! Brief §2.3/§2.4: libraries are not architecture. Tauri, React, Ratatui and
//! Clap are *shells*. The rules they both render must live once, in a core
//! neither shell owns:
//!
//! ```text
//! crates/nexora-core   (this crate — no Tauri/React/Ratatui/Clap)
//!        ▲                    ▲
//!        │ delegates          │ delegates
//! src-tauri/            apps/nexora-cli/
//! (desktop adapter)     (CLI adapter)
//! ```
//!
//! Today Desktop holds these rules inside a Tauri crate, so the CLI can only
//! reach them by linking `src-tauri` — which drags in the whole window
//! application. Phase 1 lifts the spine out; later phases lift the services
//! that sit on top of it.
//!
//! ## What phase 1 contains (and what it deliberately does not)
//!
//! Contained — these are already expressed as *data*, not behavior:
//!
//! - [`error`] — the stable error taxonomy ([`CoreErrorKind`]) and the
//!   versioned exit-code table ([`ExitCode`], brief §9.4). One table, so
//!   Desktop and CLI cannot disagree about what a failure *is*.
//! - [`version`] — three-manifest version parity, the fact the Desktop
//!   `ReleasePanel` badge and CLI `doctor` both render.
//! - [`config`] — the closed allow-list of writable settings keys and their
//!   validation, so a setting written from the terminal is accepted, rejected,
//!   and *worded* exactly as it would be from the window.
//!
//! **Not** contained: business logic. Provider registries, routing-profile and
//! MCP-server parsing, agent execution, storage, and the database all stay with
//! their current owners. Where a value domain is defined by this build rather
//! than by a static rule, [`config::ConfigRegistry`] takes it as an injected
//! predicate instead of importing it.
//!
//! ## Dependency rule (enforced, not aspirational)
//!
//! This crate must never depend on Tauri, React, Ratatui, or Clap. Its only
//! dependency is `serde`/`serde_json`, needed for manifest parsing and for the
//! stable wire shape of the error taxonomy. `cargo tree -p nexora-core` in CI
//! is the check; see the phase-1 PR body.
//!
//! ## Secret hygiene
//!
//! Errors are secret-free by construction ([`error`]), secrets are never
//! reachable through configuration keys ([`config`]), and the version module
//! reports manifest *versions* only — never manifest contents. A new module
//! must preserve all three properties.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod version;

pub use error::{CoreError, CoreErrorKind, ExitCode, EXIT_CODE_TABLE_VERSION};
pub use version::{ManifestVersions, MAX_VERSION_CHARS};
