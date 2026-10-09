//! Server-side destructive-action confirmations (NEX-SEC-004).
//!
//! Destructive IPC operations used to be gated on caller-controlled values —
//! `terminal_run(confirmed: bool)` and the `data_management` `"confirm"`
//! phrase. A constant phrase or boolean is not a user-presence proof: any
//! code able to invoke IPC (compromised renderer, malicious plugin, future
//! XSS) passes them trivially, so `terminal_run(confirmed: true)` was a
//! one-call arbitrary-shell primitive.
//!
//! The trust anchor now lives server-side, in two halves:
//!
//! 1. **Rust-side user presence.** An id is minted only by
//!    [`request_confirmation`](crate::commands::confirmations::request_confirmation),
//!    which first shows a *blocking native OS dialog* (`tauri-plugin-dialog`,
//!    registered in `lib.rs`) carrying the operation identity and only mints
//!    after the user accepts. Cancelling mints nothing and the destructive
//!    command never runs. A compromised renderer can therefore no longer
//!    obtain an id unattended: it can only raise a prompt the user sees.
//! 2. **Operation binding.** Each minted id records a `u64` binding over
//!    `scope ‖ operation ‖ cwd ‖ target_id` ([`ConfirmationTarget`]), and the
//!    consuming operation recomputes that binding from *its own* arguments.
//!    An id minted while the user looked at `ls` cannot be spent on
//!    `rm -rf`, and one minted for `delete conversation 5` cannot be spent on
//!    `clear()`. The binding is a hash, so a command that carries secrets is
//!    never stored in the clear.
//!
//! The dialog text is composed **server-side** from the same
//! [`ConfirmationTarget`] the binding covers ([`prompt_body`]), never from a
//! free-form caller-supplied summary: the user always reads the identity that
//! the id will actually authorize, so a renderer cannot show a benign string
//! and spend the id on something else.
//!
//! Other preserved properties:
//!
//! - every id is bound to one scope ([`SCOPE_TERMINAL`] or
//!   [`SCOPE_DATA_MANAGEMENT`]) and consumed **atomically on first use**:
//!   reuse, cross-scope replay, and unknown ids are refused with no execution;
//! - ids expire after [`CONFIRMATION_TTL`] (120 s) on a monotonic clock;
//! - the pending map is bounded by [`MAX_PENDING`] — expired entries are
//!   purged and, if that is not enough, the oldest entries are evicted, so a
//!   mint flood inside one TTL window still cannot grow it without limit;
//! - ids are unguessable because the server-side map is the only source of
//!   truth: an id the renderer did not receive from this process can never
//!   resolve to a pending entry, whatever it hashes to.
//!
//! Errors stay fixed-vocabulary and secret-free: scopes, operations, command
//! text, and target ids are all caller-influenced, so no variant echoes them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Scope vocabulary
// ---------------------------------------------------------------------------

/// How long a minted confirmation id stays valid: short enough that a leaked
/// id is useless quickly, long enough that the honest prompt → accept → run
/// flow never races it.
pub(crate) const CONFIRMATION_TTL: Duration = Duration::from_mins(2);

/// Scope binding terminal runs (`terminal_run`).
pub(crate) const SCOPE_TERMINAL: &str = "terminal";

/// Scope binding destructive data-management operations (permanent deletes,
/// clear-all).
pub(crate) const SCOPE_DATA_MANAGEMENT: &str = "data_management";

/// Operation identity for permanently deleting one prompt row. The frontend
/// mints with this exact string (mirrored in `src/lib/tauri.ts` and pinned by
/// a source check in `commands/confirmations.rs`).
pub(crate) const OP_DELETE_PROMPT: &str = "delete_prompt";

/// Operation identity for permanently deleting one conversation row.
pub(crate) const OP_DELETE_CONVERSATION: &str = "delete_conversation";

/// Operation identity for clearing every locally stored table.
pub(crate) const OP_CLEAR_ALL_DATA: &str = "clear_application_data";

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// Longest operation identity shown in the native confirmation dialog, in
/// characters. Display-only: the *binding* always hashes the full, uncapped
/// string on both the mint and the consume side, so truncating the prompt can
/// never make two different operations share a binding.
pub(crate) const MAX_OPERATION_CHARS: usize = 512;

/// Cap on pending (unconsumed) ids: `request_confirmation` purges expired
/// entries past this bound and then evicts the oldest ones, so
/// request-without-consume spam cannot grow the map without limit even inside
/// a single TTL window.
const MAX_PENDING: usize = 512;

/// Cap on the `target_id` rendered into the dialog body. Row ids are `i64`
/// (at most 20 characters); the bound is a belt-and-braces guard on the
/// dialog text length.
const MAX_TARGET_CHARS: usize = 20;

// ---------------------------------------------------------------------------
// Operation target
// ---------------------------------------------------------------------------

/// The exact operation a confirmation id authorizes.
///
/// Minted by [`ConfirmationRegistry::request`] and recomputed independently by
/// the consuming operation from its own arguments; the two must hash to the
/// same [`ConfirmationBinding`] or the id is refused.
///
/// - `operation` — the operation identity: the literal command text for the
///   terminal scope, or one of the `OP_*` constants above;
/// - `cwd` — the workspace-relative working directory (`None` = workspace
///   root), so an id minted for a run in one directory cannot authorize a run
///   in another;
/// - `target_id` — the affected row for row-scoped data-management deletes,
///   `None` for terminal runs and clear-all.
///
/// No field is stored in the clear: [`ConfirmationBinding`] is a hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConfirmationTarget<'a> {
    pub(crate) operation: &'a str,
    pub(crate) cwd: Option<&'a str>,
    pub(crate) target_id: Option<i64>,
}

impl<'a> ConfirmationTarget<'a> {
    /// A target with no working-directory override and no row id.
    pub(crate) const fn new(operation: &'a str) -> Self {
        Self {
            operation,
            cwd: None,
            target_id: None,
        }
    }

    /// A target scoped to a workspace-relative working directory.
    pub(crate) const fn in_cwd(operation: &'a str, cwd: Option<&'a str>) -> Self {
        Self {
            operation,
            cwd,
            target_id: None,
        }
    }

    /// A target naming the row a row-scoped operation acts on.
    pub(crate) const fn for_target(operation: &'a str, target_id: i64) -> Self {
        Self {
            operation,
            cwd: None,
            target_id: Some(target_id),
        }
    }

    /// The hash stored at mint time and recomputed at consume time. Every
    /// component is length-delimited through `Hash for str`, so no two
    /// distinct tuples can collide by concatenation (`"ab" + "c"` never
    /// equals `"a" + "bc"`).
    fn binding(&self) -> ConfirmationBinding {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.operation.hash(&mut hasher);
        self.cwd.unwrap_or_default().hash(&mut hasher);
        self.target_id.hash(&mut hasher);
        hasher.finish()
    }
}

/// The stored form of a [`ConfirmationTarget`]: a hash, never the identity
/// itself, so no command text or path is retained in process memory.
type ConfirmationBinding = u64;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// One minted, not-yet-consumed confirmation.
#[derive(Debug)]
struct PendingConfirmation {
    scope: &'static str,
    binding: ConfirmationBinding,
    minted_at: Instant,
}

/// Resolve a caller-supplied scope to its internal `'static` form, or `None`
/// for an unknown one. Exposed so the command layer can reject an unknown
/// scope *before* raising the native prompt (a prompt for an unrecognised
/// scope would be a confusing way to say "no").
pub(crate) fn scope_of_public(scope: &str) -> Option<&'static str> {
    scope_of(scope)
}

fn scope_of(scope: &str) -> Option<&'static str> {
    match scope {
        SCOPE_TERMINAL => Some(SCOPE_TERMINAL),
        SCOPE_DATA_MANAGEMENT => Some(SCOPE_DATA_MANAGEMENT),
        _ => None,
    }
}

/// Process-wide server-side confirmation state: pending single-use ids.
/// Held as managed Tauri state (`Arc` over this) so IPC commands can mint
/// on the calling thread and consume from `spawn_blocking` workers.
#[derive(Debug, Default)]
pub(crate) struct ConfirmationRegistry {
    state: Mutex<HashMap<String, PendingConfirmation>>,
    counter: AtomicU64,
}

/// Managed confirmation registry state is an [`std::sync::Arc`] so commands
/// can clone an owned handle into `spawn_blocking` without borrowing the
/// managed value (the same shape as `ManagedTerminal`).
pub(crate) type ManagedConfirmations = std::sync::Arc<ConfirmationRegistry>;

/// Classified confirmation failure. Carries no caller content: scopes,
/// operations, command text, and target ids may all be attacker-chosen, so
/// errors stay fixed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmationError {
    /// The requested scope is not a known confirmation scope.
    InvalidScope,
}

impl std::fmt::Display for ConfirmationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidScope => write!(f, "the confirmation scope is invalid"),
        }
    }
}

impl std::error::Error for ConfirmationError {}

impl ConfirmationRegistry {
    /// Create an empty registry (no pending ids).
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mint a single-use confirmation id authorizing `target` inside `scope`.
    /// The caller must have obtained the user's acceptance at the native
    /// prompt first — this layer only records what was accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ConfirmationError::InvalidScope`] for an unknown scope.
    pub(crate) fn request(
        &self,
        scope: &str,
        target: &ConfirmationTarget<'_>,
    ) -> Result<String, ConfirmationError> {
        let scope = scope_of(scope).ok_or(ConfirmationError::InvalidScope)?;
        let binding = target.binding();
        let id = self.mint_id(scope, binding);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.len() >= MAX_PENDING {
            state.retain(|_, pending| pending.minted_at.elapsed() <= CONFIRMATION_TTL);
        }
        // Still at the cap after the purge means a mint flood inside one TTL
        // window: evict oldest-first until the new entry fits, so the map
        // stays bounded rather than growing past `MAX_PENDING`.
        while state.len() >= MAX_PENDING {
            let Some(oldest) = state
                .iter()
                .min_by_key(|(_, pending)| pending.minted_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            state.remove(&oldest);
        }
        state.insert(
            id.clone(),
            PendingConfirmation {
                scope,
                binding,
                minted_at: Instant::now(),
            },
        );
        Ok(id)
    }

    /// Atomically consume the id for `target` in `scope`: returns `true` (and
    /// removes the id so it can never be reused) only for a known, unexpired
    /// id minted for this exact scope *and* this exact operation binding.
    /// Forged, expired, already-consumed, cross-scope, and
    /// different-operation ids return `false` — always without side effects.
    pub(crate) fn consume(&self, scope: &str, id: &str, target: &ConfirmationTarget<'_>) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = state.remove(id) else {
            return false;
        };
        if pending.scope != scope {
            return false;
        }
        if pending.minted_at.elapsed() > CONFIRMATION_TTL {
            return false;
        }
        pending.binding == target.binding()
    }

    /// Mint one 128-bit-shaped random hex id. No `rand`/`uuid` dependency
    /// exists in this crate (the `tasks::mint_lock_token` precedent), so the
    /// id hashes the scope, the operation binding, wall-clock nanos, a
    /// process-wide monotonic counter, and this registry's address through two
    /// independently-salted hash rounds.
    ///
    /// Truth about the hasher: `DefaultHasher::new()` is **fixed-key and
    /// deterministic** — there is no OS-seeded per-call key here, and the
    /// salts below are constant bytes, not entropy. Unguessability does not
    /// come from the hash function: it comes from the server-side map lookup.
    /// Only a string this process actually inserted can ever resolve to a
    /// pending entry, so a renderer that guesses, replays, or recomputes a
    /// plausible id offline still fails the map check in `consume`. The mixed
    /// inputs exist to make accidental collisions between distinct mints
    /// negligible, not to make the output cryptographically unguessable.
    fn mint_id(&self, scope: &str, binding: ConfirmationBinding) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let address = std::ptr::from_ref(self).addr();
        let mut first = DefaultHasher::new();
        scope.hash(&mut first);
        binding.hash(&mut first);
        counter.hash(&mut first);
        nanos.hash(&mut first);
        address.hash(&mut first);
        0_u8.hash(&mut first);
        let mut second = DefaultHasher::new();
        scope.hash(&mut second);
        binding.hash(&mut second);
        counter.hash(&mut second);
        nanos.hash(&mut second);
        address.hash(&mut second);
        1_u8.hash(&mut second);
        format!("{:016x}{:016x}", first.finish(), second.finish())
    }

    /// Test-only mint with a backdated timestamp: exercises expiry without
    /// sleeping (no timing test — no `elapsed >=` assert, no clock wait).
    #[cfg(test)]
    fn request_aged_for_test(
        &self,
        scope: &'static str,
        target: &ConfirmationTarget<'_>,
        age: Duration,
    ) -> String {
        let binding = target.binding();
        let id = self.mint_id(scope, binding);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // `checked_sub` saturates to now on a machine booted more recently
        // than `age` (then the entry is simply fresh — the expiry tests use
        // ages far below any real uptime).
        let minted_at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        state.insert(
            id.clone(),
            PendingConfirmation {
                scope,
                binding,
                minted_at,
            },
        );
        id
    }

    /// Number of pending (unconsumed) ids. Test-only: pins the `MAX_PENDING`
    /// bound under a mint flood.
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

// ---------------------------------------------------------------------------
// Native prompt text
// ---------------------------------------------------------------------------

/// Compose the body of the native confirmation dialog for `target` inside
/// `scope`.
///
/// Server-side on purpose: the prompt a user reads is derived from the exact
/// [`ConfirmationTarget`] the id will be bound to, so the text cannot disagree
/// with what the id authorizes. Display-only bounds
/// ([`MAX_OPERATION_CHARS`], [`MAX_TARGET_CHARS`]) cap the *rendered* text;
/// the binding itself always covers the full, uncapped values.
///
/// The operation text is user-authored (their own command, their own row), so
/// showing it here is the point of the prompt. It is never logged and never
/// crosses IPC.
pub(crate) fn prompt_body(scope: &str, target: &ConfirmationTarget<'_>) -> String {
    let operation: String = target.operation.chars().take(MAX_OPERATION_CHARS).collect();
    let mut body = if scope == SCOPE_TERMINAL {
        let cwd = target
            .cwd
            .filter(|dir| !dir.trim().is_empty())
            .unwrap_or("the workspace root");
        format!(
            "Nexora will run this shell command in {cwd}:\n\n{operation}\n\n\
             This runs the command directly on your machine."
        )
    } else {
        let mut body = format!("Nexora will run this data-management operation:\n\n{operation}");
        if let Some(id) = target.target_id {
            let row: String = id.to_string().chars().take(MAX_TARGET_CHARS).collect();
            body.push_str("\n\nTarget row: #");
            body.push_str(&row);
        }
        body.push_str("\n\nThis cannot be undone.");
        body
    };
    if operation.chars().count() < target.operation.chars().count() {
        body.push_str("\n\n(…truncated for display)");
    }
    body
}

// ---------------------------------------------------------------------------
// Coverage (NEX-SEC-004 is partial — read this before claiming coverage)
// ---------------------------------------------------------------------------
//
// Rust-verified today (minted only after the native prompt is accepted, bound
// to the exact operation, consumed atomically once):
//
// - `terminal_run`
// - `delete_conversation_permanently` — the ONLY conversation-delete command:
//   the ungated `delete_conversation` sibling was removed, so the live sidebar
//   path (`useConversations.remove`) mints through the native prompt first.
// - `delete_prompt_permanently` — the ONLY prompt-delete command: the ungated
//   `delete_prompt` sibling was removed, so the live library path
//   (`usePrompts.remove`) mints through the native prompt first.
// - `clear_application_data`
//
// Still renderer-attested: these destructive paths take a caller-supplied
// `confirmed: bool` (or, for the write-to-disk exports, no gate at all) and
// therefore have **no** Rust-side user-presence proof — any IPC caller passes
// them:
//
// - `git_stage`, `git_unstage`, `git_commit`, `git_push`
// - `refactor_apply`
// - `privacy_wipe`
// - `create_issue_for_finding`
// - `export_conversation_to_file` (and the other write-to-disk export paths:
//   `export_conversation`, `export_setup`, `export_setup_to_file`)
//
// Tracked as open audit items — #137 (SEC-005, export-to-file confirmation)
// and the remaining boolean gates listed above. Other destructive commands
// without any gate (`delete_task`, `remove_attachment`, `delete_setting`,
// `remove_provider`, `delete_debt_item`, `remove_permission_rule`) are outside
// this confirmation gate's scope and are tracked separately. Do not describe
// the confirmation gate as covering those paths until they are migrated to
// the same mint-and-consume flow.

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_target(command: &str) -> ConfirmationTarget<'_> {
        ConfirmationTarget::new(command)
    }

    #[test]
    fn minted_id_consumes_once_for_its_target() {
        let registry = ConfirmationRegistry::new();
        let target = terminal_target("echo hi");
        let id = registry
            .request(SCOPE_TERMINAL, &target)
            .expect("mint succeeds");
        assert_eq!(id.len(), 32, "id must be 128-bit-shaped hex");
        assert!(
            registry.consume(SCOPE_TERMINAL, &id, &target),
            "first consume must succeed"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &target),
            "replay of a consumed id must be refused"
        );
    }

    #[test]
    fn forged_and_cross_scope_ids_are_refused() {
        let registry = ConfirmationRegistry::new();
        let target = terminal_target("echo hi");
        assert!(
            !registry.consume(SCOPE_TERMINAL, "forged-id", &target),
            "unknown id must be refused"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, "", &target),
            "empty id must be refused"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, "confirm", &target),
            "the old phrase shape must not pass as an id"
        );
        let clear = ConfirmationTarget::new(OP_CLEAR_ALL_DATA);
        let id = registry
            .request(SCOPE_DATA_MANAGEMENT, &clear)
            .expect("mint succeeds");
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &terminal_target("echo hi")),
            "cross-scope replay must be refused"
        );
        // The cross-scope attempt burned the id: the owning scope refuses too.
        assert!(
            !registry.consume(SCOPE_DATA_MANAGEMENT, &id, &clear),
            "burned id must stay single-use"
        );
    }

    #[test]
    fn id_minted_for_one_operation_cannot_be_spent_on_another() {
        let registry = ConfirmationRegistry::new();
        let cheap = terminal_target("ls");
        let destructive = terminal_target("rm -rf /");
        let id = registry
            .request(SCOPE_TERMINAL, &cheap)
            .expect("mint succeeds");
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &destructive),
            "an id minted for a cheap command must not authorize a destructive one"
        );
        // The mismatched attempt burned it, exactly like a replay.
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &cheap),
            "burned id must stay single-use"
        );
    }

    #[test]
    fn id_minted_for_one_target_row_cannot_be_spent_on_another() {
        let registry = ConfirmationRegistry::new();
        let five = ConfirmationTarget::for_target(OP_DELETE_CONVERSATION, 5);
        let id = registry
            .request(SCOPE_DATA_MANAGEMENT, &five)
            .expect("mint succeeds");
        assert!(
            !registry.consume(
                SCOPE_DATA_MANAGEMENT,
                &id,
                &ConfirmationTarget::for_target(OP_DELETE_CONVERSATION, 6),
            ),
            "row 5's id must not delete row 6"
        );
    }

    #[test]
    fn id_minted_for_a_row_delete_cannot_be_spent_on_clear_all() {
        let registry = ConfirmationRegistry::new();
        let delete = ConfirmationTarget::for_target(OP_DELETE_PROMPT, 5);
        let id = registry
            .request(SCOPE_DATA_MANAGEMENT, &delete)
            .expect("mint succeeds");
        assert!(
            !registry.consume(
                SCOPE_DATA_MANAGEMENT,
                &id,
                &ConfirmationTarget::new(OP_CLEAR_ALL_DATA),
            ),
            "a row-delete id must not authorize clear-all"
        );
    }

    #[test]
    fn id_minted_for_one_working_directory_cannot_be_spent_on_another() {
        let registry = ConfirmationRegistry::new();
        let root = ConfirmationTarget::in_cwd("npm test", None);
        let id = registry
            .request(SCOPE_TERMINAL, &root)
            .expect("mint succeeds");
        assert!(
            !registry.consume(
                SCOPE_TERMINAL,
                &id,
                &ConfirmationTarget::in_cwd("npm test", Some("apps/web")),
            ),
            "a cwd-scoped id must not run in a different directory"
        );
    }

    #[test]
    fn expired_id_is_refused() {
        let registry = ConfirmationRegistry::new();
        let target = terminal_target("echo hi");
        let id = registry.request_aged_for_test(
            SCOPE_TERMINAL,
            &target,
            CONFIRMATION_TTL + Duration::from_secs(1),
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &target),
            "expired id must be refused"
        );
    }

    #[test]
    fn fresh_id_is_not_expired() {
        let registry = ConfirmationRegistry::new();
        let target = terminal_target("echo hi");
        let id = registry.request_aged_for_test(SCOPE_TERMINAL, &target, Duration::from_secs(0));
        assert!(
            registry.consume(SCOPE_TERMINAL, &id, &target),
            "fresh id must consume"
        );
    }

    #[test]
    fn unknown_scope_is_rejected_secret_free() {
        let registry = ConfirmationRegistry::new();
        let err = registry
            .request(
                "terminal; DROP TABLE x",
                &ConfirmationTarget::new("echo hi"),
            )
            .expect_err("unknown scope must be rejected");
        assert_eq!(err, ConfirmationError::InvalidScope);
        assert_eq!(format!("{err}"), "the confirmation scope is invalid");
        assert!(
            !format!("{err}").contains("DROP"),
            "scope must never be echoed"
        );
    }

    #[test]
    fn minted_ids_are_unique() {
        let registry = ConfirmationRegistry::new();
        let target = terminal_target("echo hi");
        let first = registry.request(SCOPE_TERMINAL, &target).expect("mint");
        let second = registry.request(SCOPE_TERMINAL, &target).expect("mint");
        assert_ne!(first, second, "back-to-back mints must differ");
    }

    /// The pending map must stay bounded by [`MAX_PENDING`] even when every
    /// mint lands inside one TTL window (so the expiry purge cannot help).
    #[test]
    fn mint_flood_stays_within_the_pending_cap() {
        let registry = ConfirmationRegistry::new();
        let target = ConfirmationTarget::new("echo hi");
        for _ in 0..(MAX_PENDING + 64) {
            registry
                .request(SCOPE_TERMINAL, &target)
                .expect("mint succeeds");
        }
        assert!(
            registry.pending_len() <= MAX_PENDING,
            "pending map grew past the cap: {} > {MAX_PENDING}",
            registry.pending_len(),
        );
    }

    #[test]
    fn prompt_body_shows_the_bound_operation_not_a_free_form_summary() {
        let body = prompt_body(SCOPE_TERMINAL, &ConfirmationTarget::new("rm -rf build"));
        assert!(
            body.contains("rm -rf build"),
            "the prompt must show the exact command it authorizes: {body}"
        );
        assert!(
            body.contains("workspace root"),
            "the prompt must name the working directory: {body}"
        );

        let body = prompt_body(
            SCOPE_DATA_MANAGEMENT,
            &ConfirmationTarget::for_target(OP_DELETE_CONVERSATION, 5),
        );
        assert!(body.contains(OP_DELETE_CONVERSATION), "{body}");
        assert!(body.contains("#5"), "the prompt must name the row: {body}");
    }

    #[test]
    fn prompt_body_truncates_only_the_displayed_text() {
        let long = "x".repeat(MAX_OPERATION_CHARS + 100);
        let body = prompt_body(SCOPE_TERMINAL, &ConfirmationTarget::new(&long));
        assert!(body.contains("truncated for display"), "{body}");
        assert!(
            body.chars().count() < long.chars().count() + 200,
            "the displayed body must stay bounded"
        );
        // Truncation is display-only: two long operations sharing a prefix
        // still get distinct bindings.
        let registry = ConfirmationRegistry::new();
        let a = ConfirmationTarget::new(&long);
        let longer = format!("{long}y");
        let b = ConfirmationTarget::new(&longer);
        let id = registry.request(SCOPE_TERMINAL, &a).expect("mint");
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id, &b),
            "display truncation must not merge distinct operation bindings"
        );
    }
}
