//! `nexora` — Nexora CLI stub (phase 0, issue #159; shared spine: #160).
//!
//! Offline, instant, and TUI-free by construction: argument parsing is pure
//! [`clap`], `doctor` performs local read-only checks only, and no branch
//! in this binary touches the network, a provider, or terminal fullscreen
//! modes. The interactive TUI (Ratatui/Crossterm) lands in a later phase;
//! until then a bare `nexora` explains that and exits non-zero instead of
//! guessing.
//!
//! Exit codes come from [`nexora_core::error::ExitCode`] — the shared,
//! versioned table (brief §9.4) that Desktop's error taxonomy uses too — so
//! the two surfaces cannot invent divergent codes. Clap's own usage failures
//! already exit `2`, which is [`ExitCode::Usage`].

mod doctor;

use clap::{Parser, Subcommand};
use nexora_core::error::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "nexora",
    version,
    about = "Nexora CLI (phase 0 stub): offline diagnostics; interactive agent TUI not yet included",
    long_about = "Nexora CLI, phase 0 stub.\n\n\
        Works fully offline with no provider configured. `doctor` performs\n\
        local read-only checks only and never prints secret values. The\n\
        interactive TUI is not part of this build: a bare `nexora` exits\n\
        non-zero rather than opening a fullscreen UI, including when output\n\
        is redirected.",
    after_help = "PHASE 0 STUB: `run`, `sessions`, `models`, `providers`, and the\n\
        interactive TUI are not implemented yet. See issue #159."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Print offline diagnostics: versions, app-data dir, database
    /// migrations state, and keyring availability (never secret values).
    Doctor,
}

fn run(cli: &Cli) -> ExitCode {
    match &cli.command {
        Some(Commands::Doctor) => {
            print!("{}", doctor::render(&doctor::collect()));
            ExitCode::Ok
        }
        None => {
            // No TUI in the stub — say so on stderr, point at real commands,
            // and fail loudly rather than silently succeeding at nothing.
            eprintln!(
                "nexora: interactive mode is not available in this stub build (phase 0).\n\
                 Try `nexora --help` or `nexora doctor`."
            );
            ExitCode::Usage
        }
    }
}

fn main() {
    std::process::exit(run(&Cli::parse()).code());
}
