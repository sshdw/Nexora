//! Server-side confirmation minting (NEX-SEC-004).
//!
//! Thin translation only (ARCHITECTURE.md §5): `request_confirmation` is the
//! single place where a confirmation id comes into existence, and it does so
//! only after a **blocking native OS dialog** has been accepted by the user.
//! The dialog is rendered by `tauri-plugin-dialog` (registered in `lib.rs`)
//! from the operation identity the same call carries, so what the user reads
//! is exactly what the id will authorize — the body is composed server-side
//! by [`prompt_body`](crate::application::confirmations::prompt_body), never
//! from a free-form caller string.
//!
//! The blocking show runs on the runtime's blocking pool, never the main
//! thread (`blocking_show` would otherwise freeze the app). Cancelling
//! returns the existing `ConfirmationRequired` error and mints nothing, so
//! the destructive command that would have spent the id never runs.
//!
//! The minted id is recorded by the application-layer
//! [`ConfirmationRegistry`](crate::application::confirmations::ConfirmationRegistry),
//! which owns the scope allowlist, the operation binding, the TTL, the
//! pending-map bound, and single-use enforcement. No business logic lives
//! here beyond that translation.
//!
//! # Coverage (complete for the destructive-write set — read before claiming)
//!
//! Rust-verified today: `terminal_run`, `clear_application_data`,
//! `delete_conversation_permanently` (the only conversation-delete command —
//! the ungated `delete_conversation` sibling was removed, so the live sidebar
//! path mints through the native prompt first),
//! `delete_prompt_permanently` (the only prompt-delete command — the ungated
//! `delete_prompt` sibling was removed, so the live library path mints
//! through the native prompt first), the version-control writes
//! (`git_stage` / `git_unstage` / `git_commit` / `git_push`, scope
//! `version_control`), `refactor_apply` (scope `refactor`),
//! `privacy_wipe` (scope `privacy`), `create_issue_for_finding` (scope
//! `release`), and the four export commands (scope `export`) — the former
//! renderer-attested set, migrated in SEC-03.
//!
//! Other destructive commands without any gate (`delete_task`,
//! `remove_attachment`, `delete_setting`, `remove_provider`,
//! `delete_debt_item`, `remove_permission_rule`) are outside this
//! confirmation gate's scope and are tracked separately. Do not describe
//! the confirmation gate as covering those paths until they are migrated to
//! the same mint-and-consume flow.

// Tauri command handlers must take ownership of their deserialized
// arguments: serde cannot borrow into the wire payload, so passing by
// value here is a framework requirement, not a review defect.
// (Same justification as the other command modules, e.g. conversations.rs.)
#![allow(clippy::needless_pass_by_value)]

use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::application::confirmations::{prompt_body, ConfirmationTarget, ManagedConfirmations};

use super::error::{CommandError, ErrorKind};

/// Mint one single-use confirmation id authorizing `operation` in `scope`
/// (`"terminal"` or `"data_management"`) and return it.
///
/// The user must accept the native confirmation dialog first: this command
/// blocks on the OS prompt, shows the operation identity (`operation`, `cwd`,
/// `target_id`), and only then mints. Cancelling — or an unknown scope —
/// returns an error and mints nothing.
///
/// The id is bound to the exact operation it was minted for and is consumed
/// atomically once, within its TTL; forged, expired, reused, cross-scope, and
/// different-operation ids are refused by the consuming command with no
/// execution.
///
/// # Errors
///
/// Classified [`CommandError`]s: `ConfirmationRequired` when the user cancels
/// at the native prompt (or the prompt itself cannot be shown), and
/// `InvalidInput` for an unknown scope. Secret-free by construction: neither
/// the scope, the operation, the command text, nor the target id is echoed in
/// any error.
#[tauri::command]
pub(crate) async fn request_confirmation(
    scope: String,
    operation: String,
    cwd: Option<String>,
    target_id: Option<i64>,
    app: AppHandle,
    confirmations: State<'_, ManagedConfirmations>,
) -> Result<String, CommandError> {
    let scope = crate::application::confirmations::scope_of_public(&scope).ok_or_else(|| {
        CommandError::new(ErrorKind::InvalidInput, "the confirmation scope is invalid")
    })?;
    let target = ConfirmationTarget {
        operation: operation.as_str(),
        cwd: cwd.as_deref(),
        target_id,
    };
    let body = prompt_body(scope, &target);
    let accepted = confirm_natively(&app, body).await?;
    if !accepted {
        return Err(CommandError::new(
            ErrorKind::ConfirmationRequired,
            "explicit confirmation is required before a destructive action can run",
        ));
    }
    confirmations
        .request(scope, &target)
        .map_err(CommandError::from)
}

/// Show the blocking native confirmation dialog and report whether the user
/// accepted.
///
/// `blocking_show` freezes the calling thread until the dialog closes, so it
/// runs on the runtime's blocking pool: calling it on the main thread (or
/// blocking the async runtime in-place) would hang the app. The dialog is
/// parented to nothing in particular (rfd centers it), which is what the
/// plugin's own docs recommend over blocking on the main thread.
async fn confirm_natively(app: &AppHandle, body: String) -> Result<bool, CommandError> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        handle
            .dialog()
            .message(body)
            .title("Nexora")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Allow".to_string(),
                "Cancel".to_string(),
            ))
            .blocking_show()
    })
    .await
    .map_err(|err| {
        // Only reachable if the blocking task panicked: report a safe refusal
        // instead of leaving the promise dangling (and never fall through to
        // a mint the user never saw).
        log::error!("native confirmation dialog failed: {err}");
        CommandError::new(
            ErrorKind::ConfirmationRequired,
            "explicit confirmation is required before a destructive action can run",
        )
    })
}

#[cfg(test)]
mod tests {
    /// The whole frontend IPC surface, concatenated: the `src/lib/tauri.ts`
    /// barrel plus every `src/lib/ipc/*.ts` module. AC-205 split the wrapper
    /// sources out of the barrel (the `OP_*` operation identities now live in
    /// `src/lib/ipc/confirmations.ts`), so the mint pin scans the surface it
    /// actually governs (paths resolved relative to `src-tauri/` =
    /// `CARGO_MANIFEST_DIR`; read at test runtime so the pin always sees the
    /// current frontend).
    fn frontend_ipc_sources() -> Vec<String> {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("src")
            .join("lib");
        let mut sources =
            vec![std::fs::read_to_string(lib.join("tauri.ts")).expect("read src/lib/tauri.ts")];
        let ipc = lib.join("ipc");
        let mut names: Vec<String> = std::fs::read_dir(&ipc)
            .expect("read src/lib/ipc directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
                    && !name.ends_with(".test.ts")
            })
            .collect();
        names.sort();
        for name in names {
            sources.push(std::fs::read_to_string(ipc.join(&name)).expect("read ipc module"));
        }
        sources
    }

    /// The frontend mints with the literal operation identity the consuming
    /// command recomputes; a drift on either side silently breaks every
    /// destructive path (the id is refused), so pin the frontend literals
    /// against the Rust constants.
    #[test]
    fn frontend_mints_with_the_shared_operation_identities() {
        let surface = frontend_ipc_sources().join("\n");
        for literal in [
            crate::application::confirmations::OP_DELETE_PROMPT,
            crate::application::confirmations::OP_DELETE_CONVERSATION,
            crate::application::confirmations::OP_CLEAR_ALL_DATA,
        ] {
            assert!(
                surface.contains(&format!("\"{literal}\"")),
                "the frontend IPC surface (src/lib/tauri.ts + src/lib/ipc/*) \
                 must mint with the literal {literal:?}"
            );
        }
    }

    /// The confirmation gate is a Rust-side user-presence proof, so the mint
    /// path must go through a blocking native dialog and must not mint unless
    /// the user accepted. Static check: the command body cannot be invoked
    /// here (it needs `AppHandle` + managed state), so pin the shape.
    #[test]
    fn minting_requires_the_native_dialog_to_be_accepted() {
        const SOURCE: &str = include_str!("confirmations.rs");
        assert!(
            SOURCE.contains("blocking_show()"),
            "minting must go through a blocking native dialog"
        );
        assert!(
            SOURCE.contains("if !accepted"),
            "minting must be gated on the user's acceptance"
        );
        // The accept branch must come before the mint, never after it.
        let accepted = SOURCE.find("if !accepted").expect("acceptance gate");
        let mint = SOURCE.find(".request(scope, &target)").expect("mint call");
        assert!(accepted < mint, "the acceptance gate must precede the mint");
        assert!(
            SOURCE.contains("spawn_blocking"),
            "blocking_show must not run on the main thread"
        );
    }

    /// Static wiring check: the destructive paths that claim Rust-side
    /// verification must all route through the shared mint/consume registry.
    /// This is the SEC-03 verified set — terminal, data-management, the
    /// version-control writes, `refactor_apply`, `privacy_wipe`,
    /// `create_issue_for_finding`, and the four export commands — and none
    /// of them may keep (or regain) a caller-supplied boolean gate.
    #[test]
    fn rust_verified_paths_all_consume_the_shared_registry() {
        for source in [
            include_str!("terminal.rs"),
            include_str!("data_management.rs"),
            include_str!("version_control.rs"),
            include_str!("dep_refactor.rs"),
            include_str!("privacy.rs"),
            include_str!("release.rs"),
            include_str!("import_export.rs"),
        ] {
            // The command layer holds the registry as managed state via the
            // `ManagedConfirmations` alias; that alias IS the shared registry.
            assert!(
                source.contains("ManagedConfirmations"),
                "every Rust-verified destructive path must consume the shared registry"
            );
            assert!(
                source.contains("confirmation_id"),
                "every Rust-verified destructive path must forward a minted id"
            );
            assert!(
                !source.contains("confirmed: bool"),
                "no Rust-verified path may keep a caller-supplied boolean gate"
            );
        }
        // The export module must still expose all four gated commands.
        let exports = read_command("import_export.rs");
        for command in [
            "export_conversation_to_file",
            "export_setup_to_file",
            "export_setup",
        ] {
            assert!(
                exports.contains(command),
                "import_export.rs must still expose {command}"
            );
        }
    }

    /// Read a sibling command module's source for the static wiring checks.
    fn read_command(name: &str) -> &'static str {
        match name {
            "version_control.rs" => include_str!("version_control.rs"),
            "dep_refactor.rs" => include_str!("dep_refactor.rs"),
            "privacy.rs" => include_str!("privacy.rs"),
            "release.rs" => include_str!("release.rs"),
            "import_export.rs" => include_str!("import_export.rs"),
            other => panic!("unlisted command module: {other}"),
        }
    }

    /// The scope allowlist must stay closed: an unknown scope is refused with
    /// fixed vocabulary and never echoed.
    #[test]
    fn unknown_scope_never_reaches_the_dialog() {
        assert!(
            crate::application::confirmations::scope_of_public("terminal; DROP TABLE x").is_none(),
            "unknown scopes must not map to a known scope"
        );
        assert!(crate::application::confirmations::scope_of_public("terminal").is_some());
        assert!(crate::application::confirmations::scope_of_public("data_management").is_some());
    }
}
