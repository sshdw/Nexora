//! Phase-3 contract (issue #162 acceptance): the CLI's provider/model/config
//! surface is a read-only projection of the shared catalog and settings store.
//!
//! - `providers list` / `models list` match `nexora_core::catalog` 1:1 and are
//!   plain (no ANSI) on redirected output;
//! - `config get/set` round-trips through the shared `app_settings` store under
//!   an isolated synthetic app-data base — never the user's real data;
//! - unknown / secret-adjacent keys are rejected loudly and no secret byte
//!   reaches stdout or stderr, including hostile argv and planted env vars.
//!
//! Every child runs with `env_clear()` and an isolated `HOME`/`APPDATA` (plus
//! `XDG_DATA_HOME` on Linux), so app data is never the user's real store. A
//! dead D-Bus address is supplied for Linux, where keyring 4 uses the Secret
//! Service backend: probes then fail fast to `unknown`. On macOS and Windows
//! keyring uses native stores, so a probe may observe real credential
//! *presence* (never values). Assertions therefore use only the fixed
//! vocabulary `{set, missing, unknown}` and zero secret bytes — never an
//! exact credential state.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn nexora() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nexora"))
}

/// Key-like material planted in the child environment. None of it may appear
/// in any command's stdout or stderr.
const PLANTED_SECRETS: &[&str] = &[
    "sk-ant-testsecret-0000000000000001",
    "AKIATESTSECRET00000001",
    "xoxb-test-token-000000000001",
    "ghp_testtoken0000000000000000000001",
];

/// A hostile provider argument: must be rejected and redacted, never echoed.
const HOSTILE_PROVIDER: &str = "sk-live-sentinel-9999999999";

/// A hostile config value: must never be echoed by any rejection diagnostic.
const HOSTILE_VALUE: &str = "sk-live-sentinel-8888888888";

fn temp_base(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nexora-cli-p3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("sandbox base must be creatable");
    dir
}

/// Synthetic app-data sandbox: (env base dir, resolved app-data dir).
fn sandbox(name: &str) -> (PathBuf, PathBuf) {
    let base = temp_base(name);
    #[cfg(target_os = "windows")]
    let app_data_dir = base.join("com.nexora.app");
    #[cfg(target_os = "macos")]
    let app_data_dir = base.join("Library/Application Support/com.nexora.app");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let app_data_dir = base.join(".local/share/com.nexora.app");
    (base, app_data_dir)
}

/// Run a command in a scrubbed, isolated environment. `plant_secrets` adds
/// hostile key material to the child env for the redaction sweep.
fn run(args: &[&str], base: &Path, plant_secrets: bool) -> Output {
    let mut command = Command::new(nexora());
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("USER", "nexora-test")
        // Linux-only: a dead Secret Service bus makes keyring probes fail
        // fast to `unknown`. Native macOS/Windows stores are unaffected, so
        // no test asserts an exact credential state on any platform.
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
    #[cfg(target_os = "windows")]
    command.env("APPDATA", base);
    #[cfg(target_os = "macos")]
    command.env("HOME", base);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    command
        .env("HOME", base)
        .env("XDG_DATA_HOME", base.join(".local/share"));
    if plant_secrets {
        command
            .env("OPENAI_API_KEY", PLANTED_SECRETS[0])
            .env("AWS_SECRET_ACCESS_KEY", PLANTED_SECRETS[1])
            .env("SLACK_TOKEN", PLANTED_SECRETS[2])
            .env("GITHUB_TOKEN", PLANTED_SECRETS[3]);
    }
    command.output().expect("nexora binary must execute")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn combined_text(output: &Output) -> String {
    format!("{}{}", stdout_text(output), stderr_text(output))
}

fn assert_no_leak(output: &Output) {
    let combined = combined_text(output);
    for secret in PLANTED_SECRETS
        .iter()
        .copied()
        .chain([HOSTILE_PROVIDER, HOSTILE_VALUE])
    {
        assert!(
            !combined.contains(secret),
            "command leaked secret bytes into output"
        );
    }
    for pattern in [
        "sk-ant-",
        "sk-live-",
        "AKIA",
        "xoxb-",
        "ghp_",
        "BEGIN PRIVATE KEY",
        "Bearer ",
    ] {
        assert!(
            !combined.contains(pattern),
            "output contains key-like pattern {pattern}:\n{combined}"
        );
    }
    assert!(
        !combined.contains('\u{1b}'),
        "output must have no ANSI escapes"
    );
}

#[test]
fn providers_list_exits_zero_and_is_plain_offline() {
    let (base, _dir) = sandbox("providers-plain");
    let output = run(&["providers", "list"], &base, true);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(text.contains("PROVIDER"), "missing table header:\n{text}");
    assert!(
        text.contains("CREDENTIAL"),
        "missing credential column:\n{text}"
    );
    assert!(text.contains("openai"), "missing provider row:\n{text}");
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn providers_list_rows_match_shared_catalog_counts() {
    let (base, _dir) = sandbox("providers-parity");
    let output = run(&["providers", "list"], &base, false);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    let facts = nexora_core::catalog::supported_providers();

    let rows: Vec<&str> = text
        .lines()
        .skip(1) // header
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        rows.len(),
        facts.len(),
        "one row per catalog provider:\n{text}"
    );

    for fact in &facts {
        let line = rows
            .iter()
            .find(|line| line.split_whitespace().next() == Some(fact.name.as_str()))
            .unwrap_or_else(|| panic!("missing row for '{}':\n{text}", fact.name));
        let columns: Vec<&str> = line.split_whitespace().collect();
        // Columns are NAME, DISPLAY (may contain spaces), CREDENTIAL, MODELS,
        // SELECTED — parse the last three from the right.
        let model_count = columns[columns.len() - 2];
        let credential = columns[columns.len() - 3];
        assert_eq!(
            model_count,
            fact.models.len().to_string(),
            "model count drift for '{}':\n{line}",
            fact.name
        );
        assert!(
            ["set", "missing", "unknown"].contains(&credential),
            "credential must use fixed vocabulary, got '{credential}':\n{line}"
        );
        assert!(
            line.contains(&fact.display_name),
            "display name drift for '{}':\n{line}",
            fact.name
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn models_list_lists_all_catalog_models_without_ansi() {
    let (base, _dir) = sandbox("models-all");
    let output = run(&["models", "list"], &base, true);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(text.contains("CONTEXT"), "missing table header:\n{text}");
    for provider in nexora_core::catalog::supported_providers() {
        for model in provider.models {
            assert!(
                text.contains(&model),
                "missing catalog model '{model}':\n{text}"
            );
        }
    }
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn models_list_filtered_to_one_provider_has_no_cross_provider_rows() {
    let (base, _dir) = sandbox("models-filtered");
    let output = run(&["models", "list", "--provider", "openai"], &base, true);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(
        text.contains("gpt-5.6-terra"),
        "missing openai model:\n{text}"
    );
    assert!(
        !text.contains("claude-sonnet-5"),
        "cross-provider row leaked:\n{text}"
    );
    assert!(
        !text.contains("gemini-3.6-flash"),
        "cross-provider row leaked:\n{text}"
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn models_list_unknown_provider_is_loud_and_never_echoes_hostile_arg() {
    let (base, _dir) = sandbox("models-unknown");
    let output = run(
        &["models", "list", "--provider", HOSTILE_PROVIDER],
        &base,
        true,
    );
    assert_eq!(output.status.code(), Some(2), "usage error expected");
    assert!(
        stderr_text(&output).contains("[redacted]"),
        "hostile provider must be redacted:\n{}",
        stderr_text(&output)
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_get_on_fresh_sandbox_prints_unset_and_creates_no_database() {
    let (base, dir) = sandbox("config-fresh");
    let db_path = dir.join(nexora_core::storage::app_data::DB_FILE_NAME);
    assert!(!db_path.exists());
    let output = run(&["config", "get", "appearance.theme"], &base, true);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(stdout_text(&output).trim(), "<unset>");
    assert!(
        !db_path.exists(),
        "config get must not create the shared database"
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_set_then_get_roundtrip_uses_isolated_appdata() {
    let (base, dir) = sandbox("config-roundtrip");
    std::fs::create_dir_all(&dir).expect("isolated app-data dir");
    let db_path = dir.join(nexora_core::storage::app_data::DB_FILE_NAME);

    let set = run(&["config", "set", "appearance.theme", "dark"], &base, true);
    assert_eq!(set.status.code(), Some(0), "stderr: {}", stderr_text(&set));
    assert!(
        db_path.is_file(),
        "write must land in the isolated app-data db"
    );

    let get = run(&["config", "get", "appearance.theme"], &base, true);
    assert_eq!(get.status.code(), Some(0), "stderr: {}", stderr_text(&get));
    assert_eq!(stdout_text(&get).trim(), "dark");
    assert_no_leak(&set);
    assert_no_leak(&get);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_set_unknown_key_is_loud_and_never_echoes_value() {
    let (base, _dir) = sandbox("config-unknown");
    let output = run(
        &["config", "set", "appearance.mode", HOSTILE_VALUE],
        &base,
        true,
    );
    assert_eq!(output.status.code(), Some(2), "usage error expected");
    assert!(
        stderr_text(&output).contains("not supported"),
        "unknown key must be rejected loudly:\n{}",
        stderr_text(&output)
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_set_secret_adjacent_key_is_rejected_and_redacted() {
    let (base, _dir) = sandbox("config-secret-key");
    // A harmless-looking credential-shaped key is rejected without echoing
    // its hostile value...
    let plain = run(
        &["config", "set", "provider.api_key", HOSTILE_VALUE],
        &base,
        true,
    );
    assert_eq!(plain.status.code(), Some(2));
    assert!(
        stderr_text(&plain).contains("not supported"),
        "secret-adjacent key must be rejected loudly:\n{}",
        stderr_text(&plain)
    );
    assert_no_leak(&plain);

    // ...and a key that itself carries key-like bytes is redacted in the
    // diagnostic rather than echoed verbatim.
    let shaped = run(&["config", "set", HOSTILE_PROVIDER, "dark"], &base, true);
    assert_eq!(shaped.status.code(), Some(2));
    assert!(
        stderr_text(&shaped).contains("[redacted]"),
        "hostile key must be redacted:\n{}",
        stderr_text(&shaped)
    );
    assert_no_leak(&shaped);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn config_set_invalid_theme_value_never_echoes_value() {
    let (base, _dir) = sandbox("config-invalid-value");
    let output = run(
        &["config", "set", "appearance.theme", HOSTILE_VALUE],
        &base,
        true,
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr_text(&output).contains("not valid"),
        "invalid value must be rejected loudly:\n{}",
        stderr_text(&output)
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn all_phase3_commands_emit_no_ansi_and_never_leak_planted_secrets() {
    let (base, dir) = sandbox("sweep");
    std::fs::create_dir_all(&dir).expect("isolated app-data dir");
    let cases: &[&[&str]] = &[
        &["providers", "list"],
        &["models", "list"],
        &["models", "list", "--provider", "openai"],
        &["models", "list", "--provider", HOSTILE_PROVIDER],
        &["config", "get", "appearance.theme"],
        &["config", "get", HOSTILE_PROVIDER],
        &["config", "set", "appearance.mode", HOSTILE_VALUE],
        &["config", "set", "appearance.theme", HOSTILE_VALUE],
        // Validation fails before hidden input is requested, so this never
        // blocks on a TTY in CI.
        &["auth", "set", HOSTILE_PROVIDER],
    ];
    for args in cases {
        let output = run(args, &base, true);
        assert_no_leak(&output);
    }
    let _ = std::fs::remove_dir_all(&base);
}
