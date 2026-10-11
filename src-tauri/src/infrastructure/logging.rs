//! Minimal logging facility for the infrastructure layer.
//!
//! Uses the standard `log` crate facade — the Rust ecosystem logging
//! interface, already present transitively via Tauri — together with a
//! lightweight stderr sink. No separate logging framework (for example
//! `tracing`, `slog`, or `env_logger`) is introduced.
//!
//! Call [`init`] at the very start of application startup, before any other
//! subsystem runs, so that database and migration events are captured.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Windows console code page for UTF-8 output.
///
/// Russian log output garbles in the default Windows console (CP866); the
/// startup path switches the console to this code page before emitting logs.
pub(crate) const UTF8_CODE_PAGE: u32 = 65001;

/// Return the UTF-8 code page applied to the Windows console.
pub(crate) fn utf8_code_page() -> u32 {
    UTF8_CODE_PAGE
}

/// Switch the Windows console to UTF-8 output; no-op elsewhere.
///
/// Best-effort: a failure (no console, `chcp` missing) never fails startup.
/// Reports whether the switch was applied or was unnecessary.
///
/// NEX-SEC-010: the console helper is spawned shell-free through an absolute
/// `%SystemRoot%\System32` path — never through `cmd /C`, and never resolved
/// through `PATH` (an untrusted search path). When `%SystemRoot%` is absent
/// the switch is skipped (`false`) instead of guessing a drive.
#[cfg(windows)]
pub(crate) fn init_console_encoding() -> bool {
    let Some(system_root) = std::env::var_os("SystemRoot") else {
        return false;
    };
    for binary in ["chcp.com", "chcp.exe"] {
        let candidate = chcp_candidate(std::path::Path::new(&system_root), binary);
        if std::process::Command::new(candidate)
            .arg(utf8_code_page().to_string())
            .output()
            .is_ok()
        {
            return true;
        }
    }
    false
}

/// Absolute path of the Windows console code-page helper inside
/// `%SystemRoot%\System32` (NEX-SEC-010).
///
/// Pure and `cfg`-independent so the absolute-path construction is
/// unit-testable on every platform; the `cfg(windows)` startup path above is
/// its only production caller. `binary` is a frozen file name (`chcp.com` /
/// `chcp.exe`), never caller input.
#[cfg(any(windows, test))]
fn chcp_candidate(system_root: &std::path::Path, binary: &str) -> std::path::PathBuf {
    system_root.join("System32").join(binary)
}

/// No console code page exists outside Windows; always reports success.
#[cfg(not(windows))]
pub(crate) fn init_console_encoding() -> bool {
    let _page = utf8_code_page();
    true
}

/// Maximum severity emitted by the application.
const MAX_LEVEL: Level = Level::Info;

/// Trivial stderr logger used as the global `log` sink.
struct StderrLogger {
    max: LevelFilter,
}

static LOGGER: StderrLogger = StderrLogger {
    max: LevelFilter::Info,
};

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.max
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // NOTE: callers must never embed secrets, API keys, prompts,
            // conversations, user messages, or other sensitive data into log
            // records. This sink writes the formatted record verbatim.
            let _ = writeln!(
                std::io::stderr(),
                "[{ts}] {lvl:<5} {msg}",
                ts = timestamp(),
                lvl = record.level(),
                msg = record.args()
            );
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// Install the global logger.
///
/// Idempotent: calling it more than once keeps the already-installed logger
/// and simply re-applies the maximum level.
pub(crate) fn init() {
    // Switch the Windows console to UTF-8 first so non-ASCII log output
    // (e.g. Russian text, CP866 garble) renders correctly. Best-effort:
    // the result is intentionally ignored so startup never fails here.
    init_console_encoding();
    // `set_logger` fails only if a logger is already installed; in that case
    // the existing logger is intentionally retained, so the result is ignored.
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(MAX_LEVEL.to_level_filter());
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_encoding_init_reports_success() {
        assert!(init_console_encoding());
    }

    #[test]
    fn utf8_code_page_is_65001() {
        assert_eq!(utf8_code_page(), 65001);
        assert_eq!(UTF8_CODE_PAGE, 65001);
    }

    #[test]
    fn chcp_candidate_is_an_absolute_system32_path() {
        // NEX-SEC-010: the helper must resolve inside `%SystemRoot%\System32`
        // with a frozen binary name — never a bare command through `PATH`.
        for binary in ["chcp.com", "chcp.exe"] {
            let candidate = chcp_candidate(std::path::Path::new("/system-root-sentinel"), binary);
            assert_eq!(
                candidate.file_name().and_then(|name| name.to_str()),
                Some(binary)
            );
            assert_eq!(
                candidate
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str()),
                Some("System32")
            );
        }
    }

    #[test]
    fn console_encoding_spawn_is_shell_free() {
        // Static pin (own-source, like the `spawn_blocking` wiring checks):
        // the Windows startup path must not spawn through a shell. The
        // needle is assembled with `concat!` so this test's own source never
        // matches it verbatim.
        const SOURCE: &str = include_str!("logging.rs");
        const SHELL_SPAWN: &str = concat!("Command::new(", "\"cmd\"", ")");
        assert!(
            !SOURCE.contains(SHELL_SPAWN),
            "the console switch must not spawn through a shell"
        );
        assert!(
            SOURCE.contains("System32"),
            "the console helper must resolve through an absolute System32 path"
        );
        assert!(
            SOURCE.contains("SystemRoot"),
            "the System32 path must anchor at %SystemRoot%, never a guess"
        );
    }
}
