//! `nexora auth set <provider>` — the thin OS-keyring write path (issue #162).
//!
//! The binding plan §6 P3 calls for a minimal keyring touch: presence check
//! (in [`crate::providers`]) plus hidden-input write, with the credential code
//! staying on the adapter side. This module is that write path. It never takes
//! a secret from argv, a plain config key, `SQLite`, a log, or an error message:
//! the value is read from the terminal with echo disabled by `rpassword`,
//! passed straight to the keyring under the shared `"nexora"` service, and
//! dropped as soon as the write returns (brief §3.3, §11, §18).
//!
//! I/O is injected in [`set_with`] so the validation, namespace/account
//! targeting, and redaction discipline are unit-testable without a real TTY
//! or keyring.

use crate::error::{safe_key_label, CliError};
use crate::providers::KEYRING_SERVICE;

/// Fixed hidden-input prompt. It contains no caller data and no secret.
const PROMPT: &str = "Credential: ";

/// Validate a provider id before any secret input or keyring access.
///
/// Only providers from the shared catalog are writeable; hostile argv bytes
/// are rejected loudly and reduced to `[redacted]` before they can reach a
/// diagnostic (never echoed verbatim).
fn validate_provider(provider: &str) -> Result<(), CliError> {
    if !nexora_core::catalog::is_supported_provider(provider) {
        return Err(CliError::validation(format!(
            "provider '{}' is not supported by this build",
            safe_key_label(provider)
        )));
    }
    Ok(())
}

/// Perform a credential write with injected input/output for tests.
///
/// `read_secret` yields the value from hidden input; `write` receives the
/// keyring service, account (the validated provider id), and the value. The
/// returned confirmation contains only the safe provider label — never the
/// secret.
fn set_with(
    provider: &str,
    read_secret: &dyn Fn() -> Result<String, CliError>,
    write: &dyn Fn(&str, &str, &str) -> Result<(), CliError>,
) -> Result<String, CliError> {
    validate_provider(provider)?;
    let secret = read_secret()?;
    write(KEYRING_SERVICE, provider, &secret)?;
    // `secret` goes out of scope here; it is never formatted into output,
    // logged, persisted, or returned.
    Ok(format!("credential set for '{}'", safe_key_label(provider)))
}

/// Read one credential from hidden terminal input and write it to the OS
/// keyring under the shared namespace. Prints only a fixed confirmation.
pub(crate) fn set(provider: &str) -> Result<String, CliError> {
    set_with(
        provider,
        &|| rpassword::prompt_password(PROMPT).map_err(|_| CliError::credential()),
        &|service, account, secret| {
            let entry =
                keyring::Entry::new(service, account).map_err(|_| CliError::credential())?;
            entry
                .set_password(secret)
                .map_err(|_| CliError::credential())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-sentinel-0000000000000042";

    #[test]
    fn only_supported_providers_are_accepted() {
        assert!(validate_provider("openai").is_ok());
        assert!(validate_provider("gemini").is_ok());
        for hostile in ["ghost", SECRET, "openai\nsk-live-x", ""] {
            let err = validate_provider(hostile).expect_err("unsupported provider must be loud");
            assert!(!err.message.contains(SECRET));
            assert_eq!(err.exit_code(), nexora_core::error::CliExitCode::Usage);
        }
        assert!(validate_provider(SECRET)
            .expect_err("hostile id")
            .message
            .contains("[redacted]"));
    }

    #[test]
    fn write_targets_the_shared_namespace_without_echoing_the_secret() {
        use std::cell::RefCell;

        let written = RefCell::new(Vec::new());
        let result = set_with(
            "openai",
            &|| Ok(SECRET.to_string()),
            &|service, account, secret| {
                written.borrow_mut().push((
                    service.to_string(),
                    account.to_string(),
                    secret.to_string(),
                ));
                Ok(())
            },
        )
        .expect("set");
        assert_eq!(
            written.into_inner(),
            vec![(
                KEYRING_SERVICE.to_string(),
                "openai".to_string(),
                SECRET.to_string()
            )]
        );
        assert!(!result.contains(SECRET));
        assert!(result.contains("openai"));
        assert!(!result.contains('\u{1b}'));
    }

    #[test]
    fn hidden_input_failure_is_a_fixed_credential_error() {
        let err = set_with("openai", &|| Err(CliError::credential()), &|_, _, _| {
            unreachable!("no write after a failed read")
        })
        .expect_err("read failure");
        assert_eq!(err.exit_code(), nexora_core::error::CliExitCode::Provider);
        assert!(!err.message.contains(SECRET));
        assert!(!err.message.contains("openai"));
    }
}
