//! `nexora providers list` — a read-only projection of the shared catalog.
//!
//! Provider identities and model lists come from [`nexora_core::catalog`], the
//! same value Desktop's `supported_providers` IPC command returns. The only
//! extra fact is credential *presence*: a `get_password` probe under the
//! shared `"nexora"` keyring service, whose returned value is dropped
//! immediately and never printed, logged, or serialized (brief §3.3, §9.1).

use std::fmt::Write as _;

use nexora_core::config::SELECTED_PROVIDER_KEY;

use crate::error::CliError;
use crate::store;

/// Keyring service namespace shared with the desktop credential store.
pub(crate) const KEYRING_SERVICE: &str = "nexora";

/// Credential presence for one provider — never the value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialState {
    /// A credential exists in the OS keyring.
    Set,
    /// The keyring was reached and no credential exists.
    Missing,
    /// The keyring could not be reached; presence is unknown.
    Unknown,
}

impl CredentialState {
    fn label(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}

/// One rendered provider row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRow {
    /// Internal provider name.
    pub(crate) name: String,
    /// User-facing label from the shared catalog.
    pub(crate) display_name: String,
    /// Credential presence (never a value).
    pub(crate) credential: CredentialState,
    /// Number of hardcoded supported models.
    pub(crate) model_count: usize,
    /// Whether this provider is the persisted default selection.
    pub(crate) selected: bool,
}

/// Probe credential presence under the shared keyring namespace.
///
/// A returned secret is bound to `_value` and dropped here: it never leaves
/// this function, so no output path can leak it.
#[must_use]
pub(crate) fn probe_credential(provider: &str) -> CredentialState {
    let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, provider) else {
        return CredentialState::Unknown;
    };
    match entry.get_password() {
        Ok(_value) => CredentialState::Set,
        Err(keyring::Error::NoEntry) => CredentialState::Missing,
        Err(_) => CredentialState::Unknown,
    }
}

/// Project the shared catalog into rows, injecting credential presence and
/// the persisted selection. Kept free of I/O so render tests are deterministic.
#[must_use]
pub(crate) fn collect(
    selected: Option<&str>,
    probe: &dyn Fn(&str) -> CredentialState,
) -> Vec<ProviderRow> {
    nexora_core::catalog::supported_providers()
        .into_iter()
        .map(|provider| ProviderRow {
            name: provider.name.clone(),
            display_name: provider.display_name,
            credential: probe(&provider.name),
            model_count: provider.models.len(),
            selected: selected == Some(provider.name.as_str()),
        })
        .collect()
}

/// Render rows as a plain, aligned table with no ANSI escapes and no values.
#[must_use]
pub(crate) fn render(rows: &[ProviderRow]) -> String {
    let mut out =
        String::from("PROVIDER           DISPLAY              CREDENTIAL  MODELS  SELECTED\n");
    for row in rows {
        let _ = writeln!(
            out,
            "{:<18} {:<20} {:<11} {:<6} {}",
            row.name,
            row.display_name,
            row.credential.label(),
            row.model_count,
            if row.selected { "yes" } else { "-" },
        );
    }
    out
}

/// Project the catalog with a supplied selection lookup.
///
/// The persisted selection is optional metadata, not a prerequisite for the
/// catalog projection: a NULL marker (used by Desktop to disconnect a
/// provider) or a failed settings read must not hide every provider row.
fn render_with_selection(
    selected: Result<Option<String>, CliError>,
    probe: &dyn Fn(&str) -> CredentialState,
) -> String {
    let selected = selected.ok().flatten();
    let rows = collect(selected.as_deref(), probe);
    render(&rows)
}

/// Gather facts and render the command output.
///
/// Infallible by design: every failure is degraded into optional metadata, so
/// the shared catalog is always renderable.
#[must_use]
pub(crate) fn list() -> String {
    render_with_selection(store::read(SELECTED_PROVIDER_KEY), &probe_credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexora_core::catalog::supported_providers;

    #[test]
    fn rows_are_the_shared_catalog_facts_1_to_1() {
        let rows = collect(None, &|_| CredentialState::Missing);
        let facts = supported_providers();
        assert_eq!(rows.len(), facts.len());
        for (row, fact) in rows.iter().zip(&facts) {
            assert_eq!(row.name, fact.name);
            assert_eq!(row.display_name, fact.display_name);
            assert_eq!(row.model_count, fact.models.len());
            assert!(!row.selected);
        }
    }

    #[test]
    fn render_is_plain_and_never_contains_a_credential_value() {
        const SECRET: &str = "sk-live-sentinel-12345";
        let rows = collect(Some("openai"), &|_| CredentialState::Set);
        let text = render(&rows);
        assert!(!text.contains('\u{1b}'), "no ANSI escapes");
        assert!(!text.contains(SECRET));
        assert!(text.contains("openai"));
        assert!(text.contains("set"));
        assert!(text
            .lines()
            .any(|line| line.contains("openai") && line.contains("yes")));
    }

    #[test]
    fn selection_lookup_failure_still_renders_the_whole_catalog() {
        let text = render_with_selection(Err(CliError::database()), &|_| CredentialState::Missing);
        let row_count = text
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .count();
        assert_eq!(row_count, supported_providers().len());
        assert!(text.contains("openai"));
        assert!(
            !text.lines().any(|line| line.contains("yes")),
            "a failed selection lookup must leave every row unselected:\n{text}"
        );
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn credential_states_render_as_fixed_vocabulary() {
        assert_eq!(CredentialState::Set.label(), "set");
        assert_eq!(CredentialState::Missing.label(), "missing");
        assert_eq!(CredentialState::Unknown.label(), "unknown");
    }
}
