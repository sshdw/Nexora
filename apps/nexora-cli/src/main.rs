//! `nexora` — the Nexora CLI adapter (phase 0 stub, phase 3 provider surface,
//! phase 4a `run` foundation).
//!
//! Offline and TUI-free by construction: argument parsing is pure [`clap`],
//! `doctor` performs local read-only checks, and the phase-3 commands
//! (`providers`, `models`, `config`) project shared facts from
//! [`nexora_core`]. `run` (phase 4a) parses and validates a launch and renders
//! versioned machine output; the shipped host honestly reports that agent
//! execution is not available in this build yet — no step, tool call,
//! approval, or success is fabricated. The phase-5 interactive TUI opens only
//! for a bare `nexora` on three real terminals; redirected/piped invocations
//! still exit with usage guidance and emit zero escape bytes. No branch here
//! touches the network or prints a secret.
//!
//! Process exit codes come from [`nexora_core::error::CliExitCode`] — the
//! versioned 0–5 CLI contract (brief §9.4, plan §5) — so the CLI cannot invent
//! its own numbers and a DB/config failure maps to `Internal` (`1`) rather than
//! `Budget` (`5`). Clap's own usage failures already exit `2`, which is
//! `CliExitCode::Usage`.

mod auth;
mod config;
mod doctor;
mod error;
mod models;
mod providers;
mod run;
mod store;
mod tui;

use std::io::IsTerminal;

use clap::{Parser, Subcommand};
use nexora_core::error::CliExitCode;

use error::CliError;

#[derive(Debug, Parser)]
#[command(
    name = "nexora",
    version,
    about = "Nexora CLI: offline diagnostics and shared provider/model/config facts",
    long_about = "Nexora CLI.\n\n\
        Works offline with no provider configured. `doctor` performs local\n\
        read-only checks; `providers list`, `models list`, and `config get/set`\n\
        read the SAME catalog and settings as Nexora Desktop. `auth set` stores\n\
        a credential in the OS keyring from hidden input (never from argv).\n\
        Nothing here opens a fullscreen UI or prints a credential. The\n\
        interactive TUI is not part of this build: a bare `nexora` exits with\n\
        usage guidance, including when output is redirected.",
    after_help = "Commands: run, doctor, providers list, models list, config get|set, auth set.\n\
        `sessions`, `diff`, and the interactive TUI land in later phases."
)]
struct Cli {
    /// Terminal capability overrides (global: valid before or after a command).
    #[command(flatten)]
    capability: run::CapabilityArgs,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Parse and validate a task launch, then emit its machine output.
    ///
    /// This build ships the parsing/validation/machine-output foundation only:
    /// the shipped host honestly reports that agent execution is unavailable.
    Run(run::RunArgs),
    /// Print offline diagnostics: versions, app-data dir, database
    /// migrations state, and keyring availability (never secret values).
    Doctor,
    /// Inspect the shared provider registry (read-only).
    Providers {
        #[command(subcommand)]
        command: ProviderCommands,
    },
    /// Inspect the shared model catalog (read-only).
    Models {
        #[command(subcommand)]
        command: ModelCommands,
    },
    /// Read or write allow-listed settings in the shared store.
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Manage provider credentials in the shared OS keyring (never in config).
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
}

#[derive(Debug, Subcommand)]
enum ProviderCommands {
    /// List supported providers, credential presence, and model counts.
    List,
}

#[derive(Debug, Subcommand)]
enum ModelCommands {
    /// List models from the shared catalog, optionally for one provider.
    List {
        /// Internal provider id (for example `openai`).
        #[arg(long)]
        provider: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommands {
    /// Store a credential read from hidden terminal input in the OS keyring.
    Set {
        /// Supported provider id (for example `openai`). The secret is NEVER
        /// an argument: it is prompted for with echo disabled.
        provider: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
    /// Print one setting value, or `<unset>` when it has never been set.
    Get {
        /// Allow-listed setting key (for example `appearance.theme`).
        key: String,
    },
    /// Validate and persist one allow-listed setting value.
    Set {
        /// Allow-listed setting key (for example `appearance.theme`).
        key: String,
        /// New value. Credential-shaped keys are always rejected.
        value: String,
    },
}

/// Execute a parsed command, returning its stdout payload on success.
fn run(cli: &Cli) -> Result<String, CliError> {
    match &cli.command {
        Some(Commands::Run(args)) => run::execute(args, &cli.capability),
        Some(Commands::Doctor) => Ok(doctor::render(&doctor::collect())),
        Some(Commands::Providers {
            command: ProviderCommands::List,
        }) => Ok(providers::list()),
        Some(Commands::Models {
            command: ModelCommands::List { provider },
        }) => models::list(provider.as_deref()),
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Get { key } => config::get(key),
            ConfigCommands::Set { key, value } => config::set(key, value),
        },
        Some(Commands::Auth { command }) => match command {
            AuthCommands::Set { provider } => auth::set(provider),
        },
        None => {
            // Non-TTY fallback: a piped/redirected invocation keeps the plain
            // usage guidance and exits 2 (CliExitCode::Usage). The TUI is
            // entered only on an interactive terminal, via `tui::run` in
            // `main`, never from this arm.
            Err(CliError::validation(
                "interactive mode is not available in this build; try `nexora --help` or `nexora doctor`",
            ))
        }
    }
}

fn main() {
    let cli = Cli::parse();

    // The TUI is entered ONLY for a bare command on three real terminals.
    // `--color always` never overrides TTY detection, and `run --format
    // json|jsonl` always has a subcommand so it can never land here. Every
    // redirected/piped bare invocation falls through to the unchanged `None`
    // arm below: exit 2, `nexora doctor` guidance, and zero ESC bytes.
    if cli.command.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
    {
        let outcome = tui::run(&cli.capability);
        match outcome {
            Ok(code) => std::process::exit(code.code()),
            Err(err) => {
                // The TerminalGuard was dropped inside `tui::run`, so the
                // alternate screen is already gone before this report.
                err.report();
                std::process::exit(err.exit_code().code());
            }
        }
    }

    match run(&cli) {
        Ok(text) => {
            print!("{text}");
            std::process::exit(CliExitCode::Ok.code());
        }
        Err(err) => {
            err.report();
            std::process::exit(err.exit_code().code());
        }
    }
}
