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
#[cfg(windows)]
pub(crate) fn init_console_encoding() -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "chcp"])
        .arg(utf8_code_page().to_string())
        .output()
        .is_ok()
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
}
