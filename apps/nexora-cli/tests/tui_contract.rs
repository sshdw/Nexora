//! Phase-5 no-TUI-on-redirect contract (issue #164, brief §17 items 3/15).
//!
//! The TUI is gated on three real terminals. Every child here uses piped
//! streams and an isolated synthetic app-data base, so a bare invocation must
//! fall through to the unchanged usage arm: exit 2, `nexora doctor` guidance,
//! and ZERO escape bytes (in particular no alternate-screen `\x1b[?1049h`).
//! `--color always` never overrides the TTY gate, and `run --format json`
//! always has a subcommand so it can never enter the TUI.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAX_CHILD_RUNTIME: Duration = Duration::from_secs(15);

fn nexora() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nexora"))
}

fn sandbox(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("nexora-cli-tui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("sandbox base must be creatable");
    base
}

/// Child with piped input/output, a scrubbed environment and an isolated base.
fn command(args: &[&str], base: &Path, extra_env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(nexora());
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("USER", "nexora-test")
        .stdin(Stdio::piped())
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
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command
}

fn run_bounded(mut command: Command) -> Output {
    let mut child = command.spawn().expect("nexora binary must execute");
    let deadline = Instant::now() + MAX_CHILD_RUNTIME;
    loop {
        if child
            .try_wait()
            .expect("child status must be observable")
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "bare nexora must not hang in a TUI"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    child
        .wait_with_output()
        .expect("piped output must be readable")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_alternate_screen(output: &Output) {
    let bytes = [&output.stdout[..], &output.stderr[..]].concat();
    assert!(
        !bytes.contains(&0x1b),
        "no ESC byte on either stream: {output:?}"
    );
    let text = combined(output);
    assert!(
        !text.contains("\u{1b}[?1049h"),
        "never enter alternate screen"
    );
    assert!(!text.contains("\u{1b}[?25l"), "never hide the cursor");
}

#[test]
fn bare_nexora_with_piped_stdout_exits_usage_without_alternate_screen() {
    let base = sandbox("piped-stdout");
    let output = run_bounded(command(&[], &base, &[]));
    assert_eq!(output.status.code(), Some(2), "bare piped run -> usage");
    assert!(
        combined(&output).contains("nexora doctor"),
        "bare run must point at real commands"
    );
    assert_no_alternate_screen(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn bare_nexora_with_piped_stdin_never_enters_tui() {
    let base = sandbox("piped-stdin");
    // Piped stdin alone is enough to fail the three-terminal gate.
    let output = run_bounded(command(&[], &base, &[]));
    assert_eq!(output.status.code(), Some(2));
    assert!(combined(&output).contains("nexora doctor"));
    assert_no_alternate_screen(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn color_always_with_json_format_never_emits_ansi() {
    let base = sandbox("always-json");
    let output = run_bounded(command(
        &["--color", "always", "run", "Fix auth", "--format", "json"],
        &base,
        &[("NO_COLOR", "")],
    ));
    assert_eq!(
        output.status.code(),
        Some(2),
        "no provider configured -> usage"
    );
    assert!(
        combined(&output).contains("no provider is selected"),
        "honest fixed failure"
    );
    assert_no_alternate_screen(&output);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn no_color_env_does_not_change_usage_error_text() {
    let base = sandbox("no-color-env");
    let plain = run_bounded(command(&[], &base, &[]));
    let no_color = run_bounded(command(&[], &base, &[("NO_COLOR", "1")]));
    assert_eq!(plain.status.code(), Some(2));
    assert_eq!(no_color.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&plain.stderr),
        String::from_utf8_lossy(&no_color.stderr),
        "usage guidance must be byte-identical with NO_COLOR set"
    );
    assert!(combined(&no_color).contains("nexora doctor"));
    assert_no_alternate_screen(&no_color);
    let _ = std::fs::remove_dir_all(&base);
}

/// Phase 6a/6b (#165): a piped child still exits 2 through the unchanged usage
/// path, and no synthetic sessions, diffs, approvals or resumed agents appear
/// on redirected output. Scripted sources exist only under `#[cfg(test)]` and
/// cannot be linked into `main`.
#[test]
fn no_fabricated_backend_data_in_shipped_binary() {
    let base = sandbox("no-fabricated-data");

    // Bare piped invocation: unchanged exit 2 + honest guidance, no TUI data.
    let piped = run_bounded(command(&[], &base, &[]));
    assert_eq!(piped.status.code(), Some(2), "bare piped run -> usage");
    assert_no_alternate_screen(&piped);
    let text = combined(&piped);
    for fabricated in [
        "Fix authentication",
        "18m ago",
        "+24 -8",
        "Approval required",
        "file 3/8",
        "Copied",
        "live output paused",
    ] {
        assert!(
            !text.contains(fabricated),
            "piped output fabricated '{fabricated}': {text}"
        );
    }

    // With no provider configured the real host fails before any agent starts:
    // no fabricated tool frames, diff or approval is emitted.
    let run = run_bounded(command(
        &["run", "Fix auth", "--format", "json"],
        &base,
        &[],
    ));
    assert_eq!(
        run.status.code(),
        Some(2),
        "no provider configured -> usage"
    );
    let run_text = combined(&run);
    for fabricated in [
        "tool_call_started",
        "approval_required",
        "run_finished",
        "session_id",
        "schema_version",
    ] {
        assert!(
            !run_text.contains(fabricated),
            "unavailable host emitted '{fabricated}': {run_text}"
        );
    }
    assert!(
        run_text.contains("no provider is selected"),
        "honest fixed failure"
    );

    // Source-level proof: scripted projection types are behind `#[cfg(test)]`.
    // Normalize CRLF from Windows checkouts: the multi-line marker below uses
    // `\n`, and `include_str!` preserves the on-disk line endings verbatim.
    let sources = include_str!("../src/tui/sources.rs").replace("\r\n", "\n");
    for scripted in [
        "ScriptedSessionSource",
        "ScriptedDiffSource",
        "ScriptedApprovalView",
        "ScriptedClipboardSink",
    ] {
        assert!(
            sources.contains(scripted),
            "scripted source '{scripted}' must exist for tests"
        );
    }
    assert!(
        sources.contains("#[cfg(test)]"),
        "scripted sources must be cfg(test)-gated"
    );
    let shipped_markers = [
        "Readiness::Unavailable",
        "fn copy(&self, _text: &str) -> bool {\n        false",
    ];
    for marker in shipped_markers {
        assert!(
            sources.contains(marker),
            "shipped source must be honestly unavailable: {marker}"
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}
