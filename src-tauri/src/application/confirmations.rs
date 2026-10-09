//! Server-side destructive-action confirmations (NEX-SEC-004).
//!
//! Destructive IPC operations used to be gated on caller-controlled values —
//! `terminal_run(confirmed: bool)` and the `data_management` `"confirm"`
//! phrase. A constant phrase or boolean is not a user-presence proof: any
//! code able to invoke IPC (compromised renderer, malicious plugin, future
//! XSS) passes them trivially, so `terminal_run(confirmed: true)` was a
//! one-call arbitrary-shell primitive.
//!
//! The trust anchor now lives server-side. The honest frontend flow is
//! unchanged (dialog → confirm click → execute), but the confirm click first
//! calls the [`request_confirmation`](crate::commands::confirmations::request_confirmation)
//! command, which mints an unguessable single-use id, and the destructive
//! command consumes that id:
//!
//! - ids are 128-bit-shaped random hex minted backend-side; a bare IPC caller
//!   cannot forge one (no `rand`/`uuid` dependency exists in this crate, so
//!   minting mixes per-call randomly-keyed hashing with process-private
//!   state — the same precedent as `tasks::mint_lock_token` — which the
//!   renderer process cannot observe);
//! - every id is bound to one scope ([`SCOPE_TERMINAL`] or
//!   [`SCOPE_DATA_MANAGEMENT`]) and consumed atomically on first use: reuse,
//!   cross-scope replay, and unknown ids are refused with no execution;
//! - ids expire after [`CONFIRMATION_TTL`] (120 s); an expired id is refused
//!   with no execution.
//!
//! The recorded `summary` (command text, operation name) is audit context
//! only — never a gate — and is length-capped. It may carry user secrets, so
//! it is never echoed in errors or logs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a minted confirmation id stays valid: short enough that a leaked
/// id is useless quickly, long enough that the honest dialog → execute flow
/// never races it.
pub(crate) const CONFIRMATION_TTL: Duration = Duration::from_mins(2);

/// Scope binding terminal runs (`terminal_run`).
pub(crate) const SCOPE_TERMINAL: &str = "terminal";

/// Scope binding destructive data-management operations (permanent deletes,
/// clear-all).
pub(crate) const SCOPE_DATA_MANAGEMENT: &str = "data_management";

/// Longest confirmation summary kept, in characters. The summary is audit
/// context only (never echoed), so overlong input is cut, never an error.
pub(crate) const MAX_SUMMARY_CHARS: usize = 512;

/// Cap on pending (unconsumed) ids: `request_confirmation` purges expired
/// entries past this bound so request-without-consume spam cannot grow the
/// map without limit.
const MAX_PENDING: usize = 512;

/// One minted, not-yet-consumed confirmation.
#[derive(Debug)]
struct PendingConfirmation {
    scope: &'static str,
    minted_at: Instant,
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

/// Classified confirmation failure. Carries no caller content: scopes and
/// summaries may be attacker-chosen, so errors stay fixed vocabulary.
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

    /// Mint a single-use confirmation id for `scope`, recording `summary` as
    /// audit context. Returns the id the destructive command must present.
    ///
    /// # Errors
    ///
    /// Returns [`ConfirmationError::InvalidScope`] for an unknown scope.
    pub(crate) fn request(&self, scope: &str, summary: &str) -> Result<String, ConfirmationError> {
        let scope = scope_of(scope).ok_or(ConfirmationError::InvalidScope)?;
        let kept: String = summary.chars().take(MAX_SUMMARY_CHARS).collect();
        let id = self.mint_id(scope, &kept);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.len() >= MAX_PENDING {
            state.retain(|_, pending| pending.minted_at.elapsed() <= CONFIRMATION_TTL);
        }
        state.insert(
            id.clone(),
            PendingConfirmation {
                scope,
                minted_at: Instant::now(),
            },
        );
        Ok(id)
    }

    /// Atomically consume the id for `scope`: returns `true` (and removes the
    /// id so it can never be reused) only for a known, unexpired id minted
    /// for this exact scope. Forged, expired, already-consumed, and
    /// cross-scope ids return `false` — always without side effects.
    pub(crate) fn consume(&self, scope: &str, id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = state.remove(id) else {
            return false;
        };
        if pending.scope != scope {
            return false;
        }
        pending.minted_at.elapsed() <= CONFIRMATION_TTL
    }

    /// Mint one 128-bit-shaped random hex id. No `rand`/`uuid` dependency
    /// exists in this crate (the `tasks::mint_lock_token` precedent), so the
    /// id hashes the scope, the summary, wall-clock nanos, a process-wide
    /// monotonic counter, and this registry's address through two
    /// independently-keyed hash rounds. The per-call hasher keys are
    /// OS-seeded and process-private — a renderer-side IPC caller cannot
    /// observe or predict them — which is the exact threat model here (the
    /// webview is a separate process that cannot read backend memory).
    fn mint_id(&self, scope: &str, summary: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let address = std::ptr::from_ref(self).addr();
        let mut first = DefaultHasher::new();
        scope.hash(&mut first);
        summary.hash(&mut first);
        counter.hash(&mut first);
        nanos.hash(&mut first);
        address.hash(&mut first);
        0_u8.hash(&mut first);
        let mut second = DefaultHasher::new();
        scope.hash(&mut second);
        summary.hash(&mut second);
        counter.hash(&mut second);
        nanos.hash(&mut second);
        address.hash(&mut second);
        1_u8.hash(&mut second);
        format!("{:016x}{:016x}", first.finish(), second.finish())
    }

    /// Test-only mint with a backdated timestamp: exercises expiry without
    /// sleeping (no timing test — no `elapsed >=` assert, no clock wait).
    #[cfg(test)]
    fn request_aged_for_test(&self, scope: &'static str, summary: &str, age: Duration) -> String {
        let kept: String = summary.chars().take(MAX_SUMMARY_CHARS).collect();
        let id = self.mint_id(scope, &kept);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // `checked_sub` saturates to now on a machine booted more recently
        // than `age` (then the entry is simply fresh — the expiry tests use
        // ages far below any real uptime).
        let minted_at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        state.insert(id.clone(), PendingConfirmation { scope, minted_at });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_id_consumes_once_for_its_scope() {
        let registry = ConfirmationRegistry::new();
        let id = registry
            .request(SCOPE_TERMINAL, "echo hi")
            .expect("mint succeeds");
        assert_eq!(id.len(), 32, "id must be 128-bit-shaped hex");
        assert!(
            registry.consume(SCOPE_TERMINAL, &id),
            "first consume must succeed"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id),
            "replay of a consumed id must be refused"
        );
    }

    #[test]
    fn forged_and_cross_scope_ids_are_refused() {
        let registry = ConfirmationRegistry::new();
        assert!(
            !registry.consume(SCOPE_TERMINAL, "forged-id"),
            "unknown id must be refused"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, ""),
            "empty id must be refused"
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, "confirm"),
            "the old phrase shape must not pass as an id"
        );
        let id = registry
            .request(SCOPE_DATA_MANAGEMENT, "clear")
            .expect("mint succeeds");
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id),
            "cross-scope replay must be refused"
        );
        // The cross-scope attempt burned the id: the owning scope refuses too.
        assert!(
            !registry.consume(SCOPE_DATA_MANAGEMENT, &id),
            "burned id must stay single-use"
        );
    }

    #[test]
    fn expired_id_is_refused() {
        let registry = ConfirmationRegistry::new();
        let id = registry.request_aged_for_test(
            SCOPE_TERMINAL,
            "echo hi",
            CONFIRMATION_TTL + Duration::from_secs(1),
        );
        assert!(
            !registry.consume(SCOPE_TERMINAL, &id),
            "expired id must be refused"
        );
    }

    #[test]
    fn fresh_id_is_not_expired() {
        let registry = ConfirmationRegistry::new();
        let id = registry.request_aged_for_test(SCOPE_TERMINAL, "echo hi", Duration::from_secs(0));
        assert!(
            registry.consume(SCOPE_TERMINAL, &id),
            "fresh id must consume"
        );
    }

    #[test]
    fn unknown_scope_is_rejected_secret_free() {
        let registry = ConfirmationRegistry::new();
        let err = registry
            .request("terminal; DROP TABLE x", "echo hi")
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
        let first = registry.request(SCOPE_TERMINAL, "a").expect("mint");
        let second = registry.request(SCOPE_TERMINAL, "a").expect("mint");
        assert_ne!(first, second, "back-to-back mints must differ");
    }
}
