//! Build-time toolchain capture for `nexora doctor`.
//!
//! The stub reports the toolchain that BUILT the binary (compile-time
//! capture), not whatever `rustc`/`cargo` happen to be on the end-user's
//! `PATH` at runtime (usually nothing — users are not developers). Values
//! are baked in via `cargo:rustc-env`, so the binary stays fully offline.

use std::process::Command;

/// Run `program --version`, returning the trimmed first line or "unknown".
fn tool_version(program: &str) -> String {
    Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .map_or_else(
            || "unknown".to_owned(),
            |output| {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let first_line = text.lines().next().unwrap_or("unknown").trim();
                    if first_line.is_empty() {
                        "unknown".to_owned()
                    } else {
                        first_line.to_owned()
                    }
                } else {
                    "unknown".to_owned()
                }
            },
        )
}

fn main() {
    // `CARGO` is set by cargo itself to the invoking cargo binary's path —
    // more reliable than a `PATH` lookup.
    let cargo_bin = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    println!(
        "cargo:rustc-env=NEXORA_BUILD_RUSTC_VERSION={}",
        tool_version("rustc")
    );
    println!(
        "cargo:rustc-env=NEXORA_BUILD_CARGO_VERSION={}",
        tool_version(&cargo_bin)
    );
    // `TARGET` is always set by cargo for build scripts.
    println!(
        "cargo:rustc-env=NEXORA_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned())
    );
    // Re-run only when the toolchain selection changes, not on every build.
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
}
