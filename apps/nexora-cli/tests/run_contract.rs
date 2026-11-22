//! Phase 4a contract (issue #163, brief §17 items 3/10/15): the `run`
//! foundation never opens a TUI on redirected output, emits no ANSI or secret
//! bytes in any mode, and exits through the shared 0–5 table.
//!
//! The shipped host is intentionally unavailable, so no frame is fabricated:
//! `run` exits `2` (usage) with a fixed stderr message. Every child uses
//! `env_clear()` and an isolated synthetic app-data base, plus a bounded
//! wait — no real provider, keyring value, personal app data, or hang.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const PLANTED_SECRETS: &[&str] = &[
    "sk-ant-testsecret-0000000000000001",
    "AKIATESTSECRET00000001",
    "xoxb-test-token-000000000001",
    "ghp_testtoken0000000000000000000001",
];

/// Hostile argv used for every new run flag. Must be redacted, never echoed.
const HOSTILE_VALUE: &str = "sk-live-sentinel-9999999999";
const HOSTILE_PROMPT: &str = "sk-live-sentinel-prompt-7777777777";

const MAX_CHILD_RUNTIME: Duration = Duration::from_secs(15);

fn nexora() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nexora"))
}

fn temp_base(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nexora-cli-p4-{name}-{}", std::process::id()));
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

/// Build a child with a scrubbed environment, piped streams, and an isolated
/// app-data base. `plant_secrets` adds hostile key material to the child env.
fn command(args: &[&str], base: &Path, plant_secrets: bool) -> Command {
    let mut command = Command::new(nexora());
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("USER", "nexora-test")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    command
}

/// Run a child with a hard deadline. A TUI or an accidental stdin wait fails
/// the test with a kill instead of hanging the suite forever.
fn run_bounded(mut child: Command) -> Output {
    let mut child = child.spawn().expect("nexora binary must execute");
    let deadline = Instant::now() + MAX_CHILD_RUNTIME;
    loop {
        if child
            .try_wait()
            .expect("child status must be observable")
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline, "run did not terminate in time");
        std::thread::sleep(Duration::from_millis(10));
    }
    child
        .wait_with_output()
        .expect("piped output must be readable")
}

fn run(args: &[&str], base: &Path, plant_secrets: bool) -> Output {
    run_bounded(command(args, base, plant_secrets))
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_no_leak(output: &Output) {
    let combined = format!("{}{}", stdout_text(output), stderr_text(output));
    for secret in PLANTED_SECRETS
        .iter()
        .copied()
        .chain([HOSTILE_VALUE, HOSTILE_PROMPT])
    {
        assert!(
            !combined.contains(secret),
            "run leaked secret bytes into output"
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
        "run output must have no ANSI escapes:\n{combined}"
    );
    assert!(
        !combined.contains("\u{1b}[?1049h"),
        "run must never enter the alternate screen"
    );
}

#[test]
fn run_with_redirected_output_never_starts_a_tui() {
    let (base, _dir) = sandbox("redirect");
    for format in ["text", "json", "jsonl"] {
        let output = run(
            &["run", "Fix the auth flow", "--format", format],
            &base,
            true,
        );
        assert_eq!(output.status.code(), Some(2), "unavailable host -> usage");
        let stdout = stdout_text(&output);
        assert!(
            stdout.is_empty()
                || (stdout.trim_start().starts_with('{') && !stdout.contains('\u{1b}')),
            "machine stdout must be empty or pure JSON, got:\n{stdout}"
        );
        assert_no_leak(&output);
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_unavailable_host_is_honest_and_secret_free() {
    let (base, dir) = sandbox("unavailable");
    let db_path = dir.join(nexora_core::storage::app_data::DB_FILE_NAME);

    let output = run(
        &[
            "run",
            "Fix the auth flow",
            "--format",
            "json",
            "--provider",
            "openai",
            "--budget-usd",
            "1.25",
            "--verbose",
        ],
        &base,
        true,
    );
    assert_eq!(output.status.code(), Some(2), "no fabricated success exit");
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("agent execution is not available in this CLI build yet"),
        "fixed honesty message missing:\n{stderr}"
    );
    assert!(
        stderr.contains("no approval was granted"),
        "safety hint must survive --verbose:\n{stderr}"
    );
    assert!(
        stderr.contains("diagnostic category: InvalidInput"),
        "verbose adds fixed classification only:\n{stderr}"
    );
    assert!(
        stdout_text(&output).is_empty(),
        "no frames means no stdout bytes"
    );
    assert!(
        !db_path.exists(),
        "run overrides must never write settings or create a database"
    );
    assert_no_leak(&output);

    // --quiet keeps the error and the approval hint, and is equally silent.
    let quiet = run(
        &["run", "Explain this error", "--quiet", "--format", "jsonl"],
        &base,
        true,
    );
    assert_eq!(quiet.status.code(), Some(2));
    let quiet_stderr = stderr_text(&quiet);
    assert!(quiet_stderr.contains("not available in this CLI build"));
    assert!(quiet_stderr.contains("no approval was granted"));
    assert!(stdout_text(&quiet).is_empty());
    assert_no_leak(&quiet);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn run_flag_redaction_sweep() {
    let (base, _dir) = sandbox("redaction");
    let cases: &[&[&str]] = &[
        &["run", HOSTILE_PROMPT],
        &["run", "p", "--provider", HOSTILE_VALUE],
        &["run", "p", "--model", HOSTILE_VALUE],
        &["run", "p", "--mode", HOSTILE_VALUE],
        &["run", "p", "--max-steps", HOSTILE_VALUE],
        &["run", "p", "--budget-usd", HOSTILE_VALUE],
        &["run", "p", "--workspace", HOSTILE_VALUE],
        &["run", "p", "--format", HOSTILE_VALUE],
        &["run", "p", "--motion", HOSTILE_VALUE],
        &["--color", HOSTILE_VALUE, "run", "p"],
        &["run", "p", "--verbose", "--provider", HOSTILE_VALUE],
        &[
            "run",
            "p",
            "--quiet",
            "--format",
            "json",
            "--model",
            HOSTILE_VALUE,
        ],
    ];
    for args in cases {
        let output = run(args, &base, true);
        assert_eq!(output.status.code(), Some(2), "flags must be rejected");
        let combined = format!("{}{}", stdout_text(&output), stderr_text(&output));
        // A hostile *prompt* is accepted (it is never echoed); its outcome is
        // the honest unavailable-host failure. Every hostile flag value is
        // rejected with a redacted, loud diagnostic.
        assert!(
            combined.contains("[redacted]")
                || combined.contains("not supported")
                || combined.contains("not available in this CLI build")
                || combined.contains("workspace path failed syntax validation"),
            "rejection must be loud: {args:?}\n{combined}"
        );
        assert_no_leak(&output);
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn bare_invocation_guidance_is_unchanged_by_new_global_flags() {
    let (base, _dir) = sandbox("bare");
    let output = run(&[], &base, false);
    assert_eq!(output.status.code(), Some(2));
    let combined = format!("{}{}", stdout_text(&output), stderr_text(&output));
    assert!(
        combined.contains("nexora doctor"),
        "bare run guidance must survive:\n{combined}"
    );
    assert_no_leak(&output);
    let _ = std::fs::remove_dir_all(&base);
}
