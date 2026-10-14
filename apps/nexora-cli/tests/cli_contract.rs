//! Phase-0 stub contract (issue #159 acceptance):
//!
//! - `nexora --help` / `--version` / `doctor` work offline with no provider
//!   configured and exit 0;
//! - no fullscreen UI on redirected/piped output (no ANSI escapes, no
//!   alternate-screen sequences);
//! - `doctor` prints every required field and zero secret values, even with
//!   hostile key-like material planted in the environment;
//! - `doctor` never creates the desktop database, and reports a real
//!   `schema_version` read-only when one is present.

use std::path::PathBuf;
use std::process::{Command, Output};

fn nexora() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nexora"))
}

/// Run the stub with a scrubbed environment: no provider config, no
/// proxies (offline proof), and an isolated `HOME` so the real desktop
/// database is never even looked at.
fn run_offline(args: &[&str], home: &std::path::Path) -> Output {
    Command::new(nexora())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("USER", "nexora-test")
        .output()
        .expect("stub binary must execute")
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nexora-cli-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp HOME must be creatable");
    dir
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Key-like material that must never appear in any stub output.
const PLANTED_SECRETS: &[&str] = &[
    "sk-ant-testsecret-0000000000000001",
    "AKIATESTSECRET00000001",
    "xoxb-test-token-000000000001",
    "ghp_testtoken0000000000000000000001",
];

#[test]
fn help_exits_zero_offline_with_no_provider() {
    let home = temp_home("help");
    let output = run_offline(&["--help"], &home);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert!(
        text.contains("doctor"),
        "help must advertise doctor:\n{text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "piped help must not contain ANSI escapes"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn version_exits_zero_offline() {
    let home = temp_home("version");
    let output = run_offline(&["--version"], &home);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert!(
        text.contains("nexora"),
        "version must name the binary:\n{text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "piped version must not contain ANSI escapes"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn doctor_prints_every_required_field_offline() {
    let home = temp_home("doctor-fields");
    let output = run_offline(&["doctor"], &home);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    for field in [
        "binary:",
        "rustc:",
        "cargo:",
        "target:",
        "app_data_dir:",
        "database:",
        "keyring:",
    ] {
        assert!(text.contains(field), "doctor missing {field}:\n{text}");
    }
    assert!(text.contains("database: not found") || text.contains("schema_version="));
    assert!(
        text.contains("keyring: AVAILABLE") || text.contains("keyring: UNAVAILABLE"),
        "keyring must be a bare availability label:\n{text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "piped doctor must not contain ANSI escapes"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn doctor_output_contains_zero_secret_values() {
    let home = temp_home("doctor-secrets");
    // Plant hostile material where a careless implementation could echo
    // it: environment variables visible to the child process.
    let output = Command::new(nexora())
        .arg("doctor")
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("OPENAI_API_KEY", PLANTED_SECRETS[0])
        .env("AWS_SECRET_ACCESS_KEY", PLANTED_SECRETS[1])
        .env("SLACK_TOKEN", PLANTED_SECRETS[2])
        .env("GITHUB_TOKEN", PLANTED_SECRETS[3])
        .output()
        .expect("stub binary must execute");
    assert_eq!(output.status.code(), Some(0));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in PLANTED_SECRETS {
        assert!(
            !combined.contains(secret),
            "doctor leaked a secret value into its output"
        );
    }
    // Generic key-material shapes must not appear either.
    for pattern in [
        "sk-ant-",
        "AKIA",
        "xoxb-",
        "ghp_",
        "BEGIN PRIVATE KEY",
        "Bearer ",
    ] {
        assert!(
            !combined.contains(pattern),
            "doctor output contains key-like pattern {pattern}:\n{combined}"
        );
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn doctor_never_creates_the_database() {
    let home = temp_home("doctor-nocreate");
    let db_path = home.join(".local/share/com.nexora.app/nexora.db");
    assert!(!db_path.exists());
    let output = run_offline(&["doctor"], &home);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout_text(&output).contains("database: not found"),
        "fresh HOME must report honestly, not invent state"
    );
    assert!(
        !db_path.exists(),
        "doctor must never create the desktop database"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn doctor_reports_real_schema_version_read_only() {
    let home = temp_home("doctor-realdb");
    let dir = home.join(".local/share/com.nexora.app");
    std::fs::create_dir_all(&dir).expect("fake app-data dir must be creatable");
    let db_path = dir.join("nexora.db");
    // Minimal desktop-shaped database: a schema_version bookkeeping table.
    let connection = rusqlite::Connection::open(&db_path).expect("fixture must open");
    connection
        .execute_batch(
            "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES (7), (14);",
        )
        .expect("fixture setup must succeed");
    drop(connection);

    let size_before = std::fs::metadata(&db_path)
        .expect("fixture must exist")
        .len();
    let output = run_offline(&["doctor"], &home);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert!(
        text.contains("schema_version=14"),
        "doctor must read MAX(schema_version):\n{text}"
    );
    let size_after = std::fs::metadata(&db_path)
        .expect("fixture must survive")
        .len();
    assert_eq!(
        size_before, size_after,
        "doctor must not modify the database"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn bare_invocation_never_opens_a_tui() {
    let home = temp_home("bare");
    // Piped stdout (captured): the stub must not emit fullscreen control
    // sequences and must exit non-zero with guidance, not hang in a TUI.
    let output = run_offline(&[], &home);
    assert_eq!(output.status.code(), Some(2));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains('\u{1b}'),
        "stub must not emit terminal escapes"
    );
    assert!(
        combined.contains("nexora doctor"),
        "bare run must point at real commands"
    );
    let _ = std::fs::remove_dir_all(&home);
}
