//! `nexora run` — the phase 4a foundation (issue #163, brief §9.2–9.5/§11/§17).
//!
//! This module is deliberately *only* the part of `run` that does not need an
//! agent backend:
//!
//! - a full Clap flag surface with a bounded stdin-prompt fallback that never
//!   waits on an interactive terminal;
//! - pure validation against the shared catalog/config domains. CLI overrides
//!   are validated into a local [`LaunchSettings`] value and are **never**
//!   written back to settings — no `config::set`/`store::write` reachable here;
//! - a CLI-local [`RunHost`] seam. [`UnavailableRunHost`] is the shipped host:
//!   it returns a fixed, honest failure. [`ScriptedRunHost`] exists only under
//!   `cfg(test)` and supplies deterministic frames (including a terminal one)
//!   so the seam and render ordering are testable without an agent;
//! - versioned machine output. Every JSON object / JSONL line carries
//!   `schema_version: 1`; stdout is machine-only (no prose, ANSI, prompt,
//!   argv, paths, or secrets) and every diagnostic goes through the curated
//!   [`CliError`] path on stderr.
//!
//! The later extraction stage replaces only [`host`] with a real backend and
//! maps richer failures through the same `CliExitCode` table. Nothing here
//! invents agent states, steps, approvals, or a success path.
//!
//! `RunArgs`/`RunHost` keep their contract names even though this module is
//! named `run`; that repetition is intentional API vocabulary, not a smell.
#![allow(clippy::module_name_repetitions)]

use std::fmt::Write as _;
use std::io::{self, IsTerminal, Read};

use nexora_core::catalog;
use nexora_core::config as core_config;
use nexora_core::error::CoreErrorKind;
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{is_secret_shaped, safe_key_label, CliError};

/// Version of the machine-output envelope. Bump only on a breaking schema
/// change; scripts branch on this field.
pub(crate) const MACHINE_SCHEMA_VERSION: u32 = 1;

/// Shared-core default for the step cap (plan §4: "cap 10 steps default").
pub(crate) const DEFAULT_MAX_STEPS: u32 = 10;

/// Hard ceiling on a prompt (argument or piped stdin). The reader takes one
/// byte beyond this so an over-long input is *rejected*, never silently cut.
pub(crate) const MAX_PROMPT_BYTES: u64 = 64 * 1024;

const MICRO_USD_PER_USD: u64 = 1_000_000;
const MAX_BUDGET_FRACTION_DIGITS: usize = 6;

/// Fixed failure text. No host data, no argv, no prompt, no path, no secret.
const UNAVAILABLE_MESSAGE: &str = "agent execution is not available in this CLI build yet; \
     this stage ships parsing/validation/machine output only";
/// Safety hint appended to every host failure. `--quiet` never removes it.
const NO_APPROVAL_HINT: &str = "no approval was granted; no settings were changed";

// ---------------------------------------------------------------------------
// Clap surface. Flag values are `String` on purpose: Clap's own invalid-value
// diagnostic echoes the raw byte string, which would leak a hostile argv.
// Pure parsers below reject unknown values with `safe_key_label` instead.
// ---------------------------------------------------------------------------

/// Terminal capability overrides. Global so they work before/after a command.
#[derive(Debug, clap::Args)]
pub(crate) struct CapabilityArgs {
    /// Colour capability: auto-detect, force on, or force off.
    #[arg(
        long,
        global = true,
        value_name = "auto|always|never",
        default_value = "auto",
        allow_hyphen_values = true
    )]
    pub(crate) color: String,

    /// Use ASCII-only symbols (no line-drawing characters).
    #[arg(long, global = true)]
    pub(crate) ascii: bool,

    /// Decorative motion level; `none` keeps every status and event.
    #[arg(
        long,
        global = true,
        value_name = "quiet|none",
        default_value = "quiet",
        allow_hyphen_values = true
    )]
    pub(crate) motion: String,
}

/// Arguments for `nexora run`.
///
/// The four capability/output booleans are a prescribed part of the Clap flag
/// surface (brief §9.2), not an accidental cluster of configuration toggles.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// Task prompt. If omitted and stdin is piped, read it from stdin (bounded).
    #[arg(value_name = "PROMPT", allow_hyphen_values = true)]
    pub(crate) prompt: Option<String>,

    /// Provider id from the shared catalog. Overrides are never persisted.
    #[arg(long, value_name = "PROVIDER", allow_hyphen_values = true)]
    pub(crate) provider: Option<String>,

    /// Model id (catalog shortlist or a valid custom id). Never persisted.
    #[arg(long, value_name = "MODEL", allow_hyphen_values = true)]
    pub(crate) model: Option<String>,

    /// Autonomy mode: `supervised`, `semi_autonomous`, `full_autonomous`.
    #[arg(
        long,
        value_name = "supervised|semi_autonomous|full_autonomous",
        allow_hyphen_values = true
    )]
    pub(crate) mode: Option<String>,

    /// Maximum agent steps (positive integer, default 10).
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    pub(crate) max_steps: Option<String>,

    /// Spend ceiling in USD decimal (e.g. `1.25` -> `1_250_000` micro-USD).
    #[arg(long, value_name = "AMOUNT", allow_hyphen_values = true)]
    pub(crate) budget_usd: Option<String>,

    /// Workspace root syntax only; a path is NOT a permission grant.
    #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
    pub(crate) workspace: Option<String>,

    /// Output format: text, json, jsonl (default text).
    #[arg(
        long,
        value_name = "text|json|jsonl",
        default_value = "text",
        allow_hyphen_values = true
    )]
    pub(crate) format: String,

    /// Plain output: no styling and no Unicode box drawing.
    #[arg(long)]
    pub(crate) plain: bool,

    /// Disable colour (equivalent to `--color never` for this run).
    #[arg(long)]
    pub(crate) no_color: bool,

    /// Reduce non-essential output. Errors and approval hints are preserved.
    #[arg(long)]
    pub(crate) quiet: bool,

    /// Add fixed diagnostic categories to stderr. Never prints values.
    #[arg(long)]
    pub(crate) verbose: bool,
}

// ---------------------------------------------------------------------------
// Parsed value domains (closed, pure, secret-safe)
// ---------------------------------------------------------------------------

/// Colour capability, parsed from a caller string by [`parse_color_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorMode {
    Auto,
    Always,
    Never,
}

/// Decorative motion level, parsed by [`parse_motion_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MotionMode {
    Quiet,
    None,
}

/// Output format, parsed by [`parse_output_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

fn invalid(label: &str, allowed: &str) -> CliError {
    CliError::validation(format!(
        "{label} is not supported; allowed values: {allowed}"
    ))
}

fn parse_color_mode(raw: &str) -> Result<ColorMode, CliError> {
    match raw.trim() {
        "auto" => Ok(ColorMode::Auto),
        "always" => Ok(ColorMode::Always),
        "never" => Ok(ColorMode::Never),
        other => Err(invalid(
            &format!("color '{}'", safe_key_label(other)),
            "auto, always, never",
        )),
    }
}

fn parse_motion_mode(raw: &str) -> Result<MotionMode, CliError> {
    match raw.trim() {
        "quiet" => Ok(MotionMode::Quiet),
        "none" => Ok(MotionMode::None),
        other => Err(invalid(
            &format!("motion '{}'", safe_key_label(other)),
            "quiet, none",
        )),
    }
}

fn parse_output_format(raw: &str) -> Result<OutputFormat, CliError> {
    match raw.trim() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        "jsonl" => Ok(OutputFormat::Jsonl),
        other => Err(invalid(
            &format!("output format '{}'", safe_key_label(other)),
            "text, json, jsonl",
        )),
    }
}

fn validate_provider(provider: &str) -> Result<(), CliError> {
    if is_secret_shaped(provider) {
        return Err(invalid("provider '[redacted]'", "the shared catalog"));
    }
    if !catalog::is_supported_provider(provider) {
        return Err(invalid(
            &format!("provider '{}'", safe_key_label(provider)),
            "the shared catalog",
        ));
    }
    Ok(())
}

fn validate_model(provider: Option<&str>, model: &str) -> Result<(), CliError> {
    if is_secret_shaped(model) {
        return Err(invalid(
            "model '[redacted]'",
            "the shared catalog or a custom id",
        ));
    }
    let listed = match provider {
        Some(name) => catalog::models_for_provider(name)
            .iter()
            .any(|info| info.name == model),
        None => catalog::is_listed_model(model),
    };
    if listed || core_config::is_valid_custom_model_id(model) {
        Ok(())
    } else {
        Err(invalid(
            &format!("model '{}'", safe_key_label(model)),
            "the shared catalog or a valid custom id",
        ))
    }
}

fn validate_mode(mode: &str) -> Result<(), CliError> {
    if is_secret_shaped(mode) || !core_config::VALID_AUTONOMY.contains(&mode) {
        return Err(invalid(
            &format!("mode '{}'", safe_key_label(mode)),
            "supervised, semi_autonomous, full_autonomous",
        ));
    }
    Ok(())
}

fn resolve_max_steps(raw: Option<&str>) -> Result<u32, CliError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_MAX_STEPS);
    };
    if is_secret_shaped(raw) {
        return Err(invalid("max-steps '[redacted]'", "a positive integer"));
    }
    match raw.trim().parse::<u32>() {
        Ok(steps) if steps > 0 => Ok(steps),
        _ => Err(invalid(
            &format!("max-steps '{}'", safe_key_label(raw)),
            "a positive integer",
        )),
    }
}

/// Parse a USD decimal into integer micro-USD. No float is ever constructed:
/// the fractional digits are accumulated as integers and scaled by a power of
/// ten, so `0.1` is exactly `100_000` and `0.0000001` is a loud rejection.
fn parse_budget_micro_usd(raw: &str) -> Result<u64, CliError> {
    let reject = || {
        invalid(
            "budget '[redacted]'",
            "a positive USD amount with at most 6 decimals",
        )
    };
    if is_secret_shaped(raw) {
        return Err(reject());
    }
    let value = raw.trim();
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (value, ""),
    };
    let whole_digits = whole.as_bytes();
    let fraction_digits = fraction.as_bytes();
    let shape_ok = !whole_digits.is_empty()
        && whole_digits.iter().all(u8::is_ascii_digit)
        && fraction_digits.len() <= MAX_BUDGET_FRACTION_DIGITS
        && fraction_digits.iter().all(u8::is_ascii_digit);
    if !shape_ok {
        return Err(invalid(
            &format!("budget '{}'", safe_key_label(value)),
            "a positive USD amount with at most 6 decimals",
        ));
    }
    let whole_micros = whole
        .parse::<u64>()
        .ok()
        .and_then(|amount| amount.checked_mul(MICRO_USD_PER_USD))
        .ok_or_else(reject)?;
    let mut fraction_micros = 0_u64;
    for digit in fraction_digits {
        fraction_micros = fraction_micros
            .checked_mul(10)
            .and_then(|scaled| scaled.checked_add(u64::from(digit - b'0')))
            .ok_or_else(reject)?;
    }
    let missing_digits = MAX_BUDGET_FRACTION_DIGITS - fraction_digits.len();
    let scale = 10_u64.pow(u32::try_from(missing_digits).unwrap_or_default());
    let fraction_micros = fraction_micros.checked_mul(scale).ok_or_else(reject)?;
    let total = whole_micros
        .checked_add(fraction_micros)
        .ok_or_else(reject)?;
    if total == 0 {
        return Err(reject());
    }
    Ok(total)
}

fn validate_workspace(workspace: &str) -> Result<(), CliError> {
    let valid =
        !is_secret_shaped(workspace) && core_config::is_valid_workspace_root_syntax(workspace);
    if valid {
        Ok(())
    } else {
        // Deliberately no label: a path may carry a user name or key material.
        Err(CliError::validation(format!(
            "workspace path failed syntax validation; this flag grants no permission; \
             {NO_APPROVAL_HINT}"
        )))
    }
}

fn validate_prompt(prompt: &str) -> Result<(), CliError> {
    if prompt.trim().is_empty() {
        return Err(CliError::validation("prompt is empty; no run was started"));
    }
    if u64::try_from(prompt.len()).is_ok_and(|len| len > MAX_PROMPT_BYTES) {
        return Err(CliError::validation(format!(
            "prompt exceeds the {MAX_PROMPT_BYTES}-byte limit; no run was started"
        )));
    }
    Ok(())
}

/// Read at most `MAX_PROMPT_BYTES + 1` bytes: one extra byte lets validation
/// *reject* an over-long prompt instead of silently truncating it.
fn read_bounded_prompt<R: Read>(mut reader: R) -> Result<String, CliError> {
    let mut buffer = String::new();
    reader
        .by_ref()
        .take(MAX_PROMPT_BYTES + 1)
        .read_to_string(&mut buffer)
        .map_err(|_| {
            CliError::validation("prompt could not be read from stdin; no run was started")
        })?;
    validate_prompt(&buffer)?;
    Ok(buffer)
}

fn stdin_prompt_reader() -> Result<String, CliError> {
    read_bounded_prompt(io::stdin().lock())
}

/// Choose the prompt source without ever blocking on an interactive terminal.
///
/// `read_stdin` is injected so this decision — and the bounded-read contract —
/// are unit-testable without a TTY.
fn resolve_prompt(
    positional: Option<String>,
    stdin_is_terminal: bool,
    read_stdin: &dyn Fn() -> Result<String, CliError>,
) -> Result<String, CliError> {
    match positional {
        Some(prompt) => {
            validate_prompt(&prompt)?;
            Ok(prompt)
        }
        None if stdin_is_terminal => Err(CliError::validation(
            "no prompt provided; pass a prompt argument or pipe one on stdin (no run was started)",
        )),
        None => read_stdin(),
    }
}

// ---------------------------------------------------------------------------
// Validated launch settings (local only — never written to the store)
// ---------------------------------------------------------------------------

/// A fully validated, in-memory launch configuration.
///
/// The prompt is held only to hand to a later host; it is never rendered,
/// logged, serialized, or persisted by this stage. `Debug` is intentionally
/// redacted so a diagnostic cannot accidentally dump it. The booleans mirror
/// the prescribed flag surface (brief §9.2) one-for-one.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct LaunchSettings {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mode: String,
    pub(crate) max_steps: u32,
    pub(crate) budget_micro_usd: Option<u64>,
    pub(crate) workspace: Option<String>,
    pub(crate) format: OutputFormat,
    /// Held for the later real host only: never rendered, logged, or persisted.
    #[allow(dead_code)]
    pub(crate) prompt: String,
    pub(crate) plain: bool,
    pub(crate) no_color: bool,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) color_mode: ColorMode,
    pub(crate) ascii: bool,
    pub(crate) motion: MotionMode,
}

/// Debug prints only presence labels, validated enums and numbers — never a
/// provider/model/path value or the prompt. Safe even in a verbose dump.
impl std::fmt::Debug for LaunchSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let presence = |present: bool| if present { "set" } else { "unset" };
        f.debug_struct("LaunchSettings")
            .field("provider", &presence(self.provider.is_some()))
            .field("model", &presence(self.model.is_some()))
            .field("mode", &self.mode)
            .field("max_steps", &self.max_steps)
            .field("budget", &presence(self.budget_micro_usd.is_some()))
            .field("workspace", &presence(self.workspace.is_some()))
            .field("format", &self.format)
            .field("prompt", &"<withheld>")
            .field("plain", &self.plain)
            .field("no_color", &self.no_color)
            .field("quiet", &self.quiet)
            .field("verbose", &self.verbose)
            .field("color_mode", &self.color_mode)
            .field("ascii", &self.ascii)
            .field("motion", &self.motion)
            .finish()
    }
}

/// Validate all flags into local launch settings. Pure: no I/O, no settings
/// write, no network, no agent start.
pub(crate) fn validate(
    args: &RunArgs,
    caps: &CapabilityArgs,
    prompt: String,
) -> Result<LaunchSettings, CliError> {
    if let Some(provider) = &args.provider {
        validate_provider(provider)?;
    }
    if let Some(model) = &args.model {
        validate_model(args.provider.as_deref(), model)?;
    }
    let mode = args.mode.as_deref().unwrap_or(catalog::DEFAULT_AUTONOMY);
    validate_mode(mode)?;
    let max_steps = resolve_max_steps(args.max_steps.as_deref())?;
    let budget_micro_usd = args
        .budget_usd
        .as_deref()
        .map(parse_budget_micro_usd)
        .transpose()?;
    if let Some(workspace) = &args.workspace {
        validate_workspace(workspace)?;
    }
    let format = parse_output_format(&args.format)?;
    let color_mode = parse_color_mode(&caps.color)?;
    let motion = parse_motion_mode(&caps.motion)?;

    Ok(LaunchSettings {
        provider: args.provider.clone(),
        model: args.model.clone(),
        mode: mode.to_string(),
        max_steps,
        budget_micro_usd,
        workspace: args.workspace.clone(),
        format,
        prompt,
        plain: args.plain,
        no_color: args.no_color,
        quiet: args.quiet,
        verbose: args.verbose,
        color_mode,
        ascii: caps.ascii,
        motion,
    })
}

// ---------------------------------------------------------------------------
// Host seam + versioned machine output
// ---------------------------------------------------------------------------

/// One generic JSON object envelope. No `AgentRunEvent` enum is duplicated
/// here: the later runtime extraction supplies the typed events, and this
/// stage only needs an opaque, `snake_case` JSON object with a version stamp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct WireFrame {
    #[serde(flatten)]
    fields: serde_json::Map<String, Value>,
}

// Constructors are the seam's public envelope API: used by the scripted test
// host today and by the real host in the later extraction. The shipped host
// emits no frames at all, so they are unused in this stage's binary.
#[allow(dead_code)]
impl WireFrame {
    /// Create an envelope with a `type` discriminator supplied by the host.
    pub(crate) fn new(event_type: &str) -> Self {
        let mut fields = serde_json::Map::new();
        fields.insert("type".to_string(), json!(event_type));
        Self { fields }
    }

    /// Add one `snake_case` field. Callers supply only host-owned safe values.
    pub(crate) fn with(mut self, key: &str, value: Value) -> Self {
        self.fields.insert(key.to_string(), value);
        self
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    fn versioned(&self) -> Value {
        let mut fields = self.fields.clone();
        // The CLI owns the version stamp: a host cannot forge or omit it.
        fields.insert("schema_version".to_string(), json!(MACHINE_SCHEMA_VERSION));
        Value::Object(fields)
    }
}

/// A curated, secret-free host failure. `message` is fixed vocabulary from
/// the host; `kind` drives the shared `CliExitCode` mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunFailure {
    pub(crate) kind: CoreErrorKind,
    pub(crate) message: String,
}

impl RunFailure {
    fn unavailable() -> Self {
        Self {
            kind: CoreErrorKind::InvalidInput,
            message: UNAVAILABLE_MESSAGE.to_string(),
        }
    }
}

/// The CLI-local seam a real agent backend will implement in a later stage.
pub(crate) trait RunHost {
    fn start(&self, launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure>;
}

/// Shipped host: honest, fixed failure. Starts nothing, grants nothing.
pub(crate) struct UnavailableRunHost;

impl RunHost for UnavailableRunHost {
    fn start(&self, _launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure> {
        Err(RunFailure::unavailable())
    }
}

/// Deterministic test host: returns exactly the supplied frames, in order.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ScriptedRunHost {
    frames: Vec<WireFrame>,
}

#[cfg(test)]
impl ScriptedRunHost {
    fn success() -> Self {
        Self {
            frames: vec![
                WireFrame::new("run_started").with("status", json!("running")),
                WireFrame::new("run_finished").with("status", json!("completed")),
            ],
        }
    }
}

#[cfg(test)]
impl RunHost for ScriptedRunHost {
    fn start(&self, _launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure> {
        Ok(self.frames.clone())
    }
}

/// Convert a host failure into the single curated error carrier. The approval
/// hint is unconditional; verbose adds only a fixed category label.
fn host_failure(failure: RunFailure, launch: &LaunchSettings) -> CliError {
    let mut message = failure.message;
    message.push('\n');
    message.push_str(NO_APPROVAL_HINT);
    if launch.verbose {
        // Presence/shape labels and validated enums only. No provider value,
        // model id, path, amount, or prompt byte is ever part of a diagnostic.
        let presence = |present: bool| if present { "set" } else { "unset" };
        let _ = write!(
            message,
            "\ndiagnostic category: {:?}\nlaunch: provider={} model={} mode={} max_steps={} \
             budget={} workspace={} format={:?} color={:?} motion={:?} plain={} no_color={} \
             ascii={} quiet={}",
            failure.kind,
            presence(launch.provider.is_some()),
            presence(launch.model.is_some()),
            launch.mode,
            launch.max_steps,
            presence(launch.budget_micro_usd.is_some()),
            presence(launch.workspace.is_some()),
            launch.format,
            launch.color_mode,
            launch.motion,
            launch.plain,
            launch.no_color,
            launch.ascii,
            launch.quiet,
        );
    }
    CliError {
        kind: failure.kind,
        message,
    }
}

/// Serialize one envelope. The only render primitive for JSON and JSONL.
fn serialize_versioned(frame: &WireFrame) -> String {
    serde_json::to_string(&frame.versioned()).unwrap_or_default()
}

/// Render host frames. `snake_case` fields, `schema_version` on every machine
/// object, no ANSI, no prose, and no caller/prompt data.
pub(crate) fn render_machine(frames: &[WireFrame], format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => match frames {
            [] => String::new(),
            [only] => serialize_versioned(only),
            many => {
                let objects: Vec<Value> = many.iter().map(WireFrame::versioned).collect();
                serde_json::to_string(&Value::Array(objects)).unwrap_or_default()
            }
        },
        OutputFormat::Jsonl => {
            let mut out = String::new();
            for frame in frames {
                let _ = writeln!(out, "{}", serialize_versioned(frame));
            }
            out
        }
        OutputFormat::Text => frames
            .iter()
            .map(|frame| {
                let event = frame.get("type").and_then(Value::as_str).unwrap_or("event");
                match frame.get("status").and_then(Value::as_str) {
                    Some(status) => format!("{event}: {status}\n"),
                    None => format!("{event}\n"),
                }
            })
            .collect(),
    }
}

/// The only host implementation linked into the shipped binary. A later,
/// separately authorized stage swaps this for the extracted real host.
fn host() -> &'static dyn RunHost {
    static HOST: UnavailableRunHost = UnavailableRunHost;
    &HOST
}

/// Command entry point: resolve prompt, validate, then hand off to the host.
pub(crate) fn execute(args: &RunArgs, caps: &CapabilityArgs) -> Result<String, CliError> {
    let prompt = resolve_prompt(
        args.prompt.clone(),
        io::stdin().is_terminal(),
        &stdin_prompt_reader,
    )?;
    let launch = validate(args, caps, prompt)?;
    let frames = host()
        .start(&launch)
        .map_err(|failure| host_failure(failure, &launch))?;
    Ok(render_machine(&frames, launch.format))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use clap::Parser;
    use nexora_core::error::CliExitCode;

    use super::*;
    use crate::{Cli, Commands};

    const SECRET: &str = "sk-live-sentinel-1234567890";

    fn minimal_args() -> RunArgs {
        RunArgs {
            prompt: None,
            provider: None,
            model: None,
            mode: None,
            max_steps: None,
            budget_usd: None,
            workspace: None,
            format: "text".to_string(),
            plain: false,
            no_color: false,
            quiet: false,
            verbose: false,
        }
    }

    fn minimal_caps() -> CapabilityArgs {
        CapabilityArgs {
            color: "auto".to_string(),
            ascii: false,
            motion: "quiet".to_string(),
        }
    }

    fn launch(args: &RunArgs, caps: &CapabilityArgs) -> LaunchSettings {
        validate(args, caps, "a bounded test prompt".to_string()).expect("valid launch")
    }

    fn quiet_launch(quiet: bool, verbose: bool) -> LaunchSettings {
        let mut args = minimal_args();
        args.quiet = quiet;
        args.verbose = verbose;
        launch(&args, &minimal_caps())
    }

    // ---- parser contracts -------------------------------------------------

    #[test]
    fn run_args_parse_all_flags() {
        let cli = Cli::try_parse_from([
            "nexora",
            "--color",
            "always",
            "--ascii",
            "--motion",
            "none",
            "run",
            "Fix the auth flow",
            "--provider",
            "openai",
            "--model",
            "gpt-5.6-terra",
            "--mode",
            "supervised",
            "--max-steps",
            "7",
            "--budget-usd",
            "1.25",
            "--workspace",
            "/tmp/project",
            "--format",
            "jsonl",
            "--plain",
            "--no-color",
            "--quiet",
            "--verbose",
        ])
        .expect("all flags must parse");
        assert_eq!(cli.capability.color, "always");
        assert!(cli.capability.ascii);
        assert_eq!(cli.capability.motion, "none");
        let crate::Commands::Run(args) = cli.command.expect("run command") else {
            panic!("expected run command");
        };
        assert_eq!(args.prompt.as_deref(), Some("Fix the auth flow"));
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(args.mode.as_deref(), Some("supervised"));
        assert_eq!(args.max_steps.as_deref(), Some("7"));
        assert_eq!(args.budget_usd.as_deref(), Some("1.25"));
        assert_eq!(args.workspace.as_deref(), Some("/tmp/project"));
        assert_eq!(args.format, "jsonl");
        assert!(args.plain && args.no_color && args.quiet && args.verbose);

        let settings = validate(&args, &cli.capability, args.prompt.clone().unwrap())
            .expect("flags must validate");
        assert_eq!(settings.provider.as_deref(), Some("openai"));
        assert_eq!(settings.mode, "supervised");
        assert_eq!(settings.max_steps, 7);
        assert_eq!(settings.budget_micro_usd, Some(1_250_000));
        assert_eq!(settings.format, OutputFormat::Jsonl);
        assert_eq!(settings.color_mode, ColorMode::Always);
        assert_eq!(settings.motion, MotionMode::None);
        assert!(settings.ascii && settings.plain && settings.no_color);
    }

    #[test]
    fn run_args_reject_unknown_mode_and_budget() {
        let mut args = minimal_args();
        args.mode = Some("ghost".to_string());
        let err = validate(&args, &minimal_caps(), "p".to_string()).expect_err("unknown mode");
        assert_eq!(err.exit_code(), CliExitCode::Usage);
        assert!(err.message.contains("mode 'ghost'"));

        args.mode = None;
        args.budget_usd = Some("0.1234567".to_string());
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_err());

        args.budget_usd = Some(SECRET.to_string());
        let err = validate(&args, &minimal_caps(), "p".to_string()).expect_err("hostile budget");
        assert!(err.message.contains("[redacted]"));
        assert!(!err.message.contains(SECRET));

        args.budget_usd = None;
        args.format = "yaml".to_string();
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_err());

        // Clap keeps the raw string; validation is the authoritative gate.
        let parsed = Cli::try_parse_from(["nexora", "run", "p", "--mode", SECRET])
            .expect("raw flag value is accepted by Clap");
        assert!(matches!(parsed.command, Some(Commands::Run(_))));
    }

    #[test]
    fn run_args_defaults_match_core() {
        let settings = launch(&minimal_args(), &minimal_caps());
        assert_eq!(settings.mode, catalog::DEFAULT_AUTONOMY);
        assert!(core_config::VALID_AUTONOMY.contains(&settings.mode.as_str()));
        assert_eq!(settings.max_steps, DEFAULT_MAX_STEPS);
        assert_eq!(settings.format, OutputFormat::Text);
        assert_eq!(settings.color_mode, ColorMode::Auto);
        assert_eq!(settings.motion, MotionMode::Quiet);
        assert!(settings.provider.is_none() && settings.model.is_none());
        assert!(settings.budget_micro_usd.is_none() && settings.workspace.is_none());
        assert!(!settings.ascii && !settings.plain && !settings.no_color);
    }

    #[test]
    fn global_capability_flags_are_global() {
        assert!(Cli::try_parse_from([
            "nexora", "--color", "never", "--ascii", "--motion", "none", "doctor"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "nexora", "doctor", "--color", "always", "--ascii", "--motion", "none"
        ])
        .is_ok());
        let cli = Cli::try_parse_from(["nexora", "--color", "never", "run", "p"])
            .expect("global flag before subcommand");
        assert_eq!(cli.capability.color, "never");
        assert!(matches!(cli.command, Some(Commands::Run(_))));
    }

    #[test]
    fn prompt_from_stdin_is_bounded_and_noninteractive() {
        let called = Cell::new(false);
        let err = resolve_prompt(None, true, &|| {
            called.set(true);
            Ok("must not be read".to_string())
        })
        .expect_err("interactive stdin must never be read");
        assert!(!called.get(), "no blocking read on an interactive terminal");
        assert!(err.message.contains("no prompt provided"));

        let too_long = "x".repeat(usize::try_from(MAX_PROMPT_BYTES).expect("size fits") + 10);
        let err = read_bounded_prompt(too_long.as_bytes()).expect_err("bounded read");
        assert_eq!(err.exit_code(), CliExitCode::Usage);

        let prompt = read_bounded_prompt("  a piped prompt  ".as_bytes()).expect("bounded");
        assert_eq!(prompt.trim(), "a piped prompt");
        assert_eq!(
            resolve_prompt(Some("argument".to_string()), true, &|| unreachable!())
                .expect("positional"),
            "argument"
        );
    }

    // ---- pure validation domains -----------------------------------------

    #[test]
    fn provider_domain_is_checked_against_shared_catalog() {
        let mut args = minimal_args();
        args.provider = Some("openai".to_string());
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_ok());

        for hostile in ["ghost", SECRET, "openai\nsk-live-x"] {
            args.provider = Some(hostile.to_string());
            let err = validate(&args, &minimal_caps(), "p".to_string())
                .expect_err("unsupported provider");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(SECRET));
            if hostile == SECRET {
                assert!(err.message.contains("[redacted]"));
            }
        }
    }

    #[test]
    fn model_domain_uses_catalog_or_custom_id_syntax() {
        let mut args = minimal_args();
        args.provider = Some("openai".to_string());
        args.model = Some("gpt-5.6-terra".to_string());
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_ok());

        args.provider = None;
        args.model = Some("my-finetune/v1".to_string());
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_ok());

        for hostile in ["../../etc/passwd", SECRET, "bad model"] {
            args.model = Some(hostile.to_string());
            let err =
                validate(&args, &minimal_caps(), "p".to_string()).expect_err("unsupported model");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(SECRET));
        }
    }

    #[test]
    fn mode_domain_uses_core_valid_autonomy_and_default() {
        let mut args = minimal_args();
        for mode in core_config::VALID_AUTONOMY {
            args.mode = Some(mode.to_string());
            let settings = validate(&args, &minimal_caps(), "p".to_string()).expect("valid mode");
            assert_eq!(settings.mode, mode);
        }
        args.mode = None;
        assert_eq!(
            validate(&args, &minimal_caps(), "p".to_string())
                .expect("default")
                .mode,
            catalog::DEFAULT_AUTONOMY
        );
        args.mode = Some("full-auto".to_string());
        assert!(validate(&args, &minimal_caps(), "p".to_string()).is_err());
    }

    #[test]
    fn max_steps_must_be_positive() {
        let mut args = minimal_args();
        args.max_steps = Some("3".to_string());
        assert_eq!(
            validate(&args, &minimal_caps(), "p".to_string())
                .expect("valid")
                .max_steps,
            3
        );
        for hostile in ["0", "-1", "abc", SECRET] {
            args.max_steps = Some(hostile.to_string());
            let err =
                validate(&args, &minimal_caps(), "p".to_string()).expect_err("max-steps rejected");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(SECRET));
        }
    }

    #[test]
    fn budget_parses_to_micro_usd_without_float() {
        for (raw, micros) in [
            ("12", 12_000_000),
            ("0.1", 100_000),
            ("0.000001", 1),
            ("1.234567", 1_234_567),
            (" 1.25 ", 1_250_000),
        ] {
            assert_eq!(
                parse_budget_micro_usd(raw).expect("valid budget"),
                micros,
                "wrong micro-USD for {raw}"
            );
        }
        for hostile in ["0.0000001", "0", "1e3", "-1", "1.2.3", SECRET] {
            let err = parse_budget_micro_usd(hostile).expect_err("budget rejected");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(SECRET));
        }
    }

    #[test]
    fn workspace_uses_core_syntax_only() {
        assert!(validate_workspace("/tmp/project").is_ok());
        assert!(validate_workspace("").is_err(), "empty workspace rejected");
        for hostile in ["   ", "/tmp/\0bad", SECRET] {
            let err = validate_workspace(hostile).expect_err("workspace rejected");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(hostile));
            assert!(err.message.contains("grants no permission"));
        }
    }

    #[test]
    fn format_domain_is_closed() {
        assert_eq!(
            parse_output_format("json").expect("json"),
            OutputFormat::Json
        );
        assert_eq!(
            parse_output_format("jsonl").expect("jsonl"),
            OutputFormat::Jsonl
        );
        assert_eq!(
            parse_output_format("text").expect("text"),
            OutputFormat::Text
        );
        for hostile in ["yaml", SECRET] {
            let err = parse_output_format(hostile).expect_err("unknown format");
            assert_eq!(err.exit_code(), CliExitCode::Usage);
            assert!(!err.message.contains(SECRET));
        }
        assert!(parse_color_mode("ghost").is_err());
        assert!(parse_motion_mode("normal").is_err());
    }

    #[test]
    fn hostile_values_are_never_echoed() {
        let mut args = minimal_args();
        args.provider = Some(SECRET.to_string());
        args.model = Some(SECRET.to_string());
        args.mode = Some(SECRET.to_string());
        args.max_steps = Some(SECRET.to_string());
        args.budget_usd = Some(SECRET.to_string());
        args.workspace = Some(SECRET.to_string());
        args.format = SECRET.to_string();
        let mut caps = minimal_caps();
        caps.color = SECRET.to_string();
        caps.motion = SECRET.to_string();

        let err = validate(&args, &caps, "p".to_string()).expect_err("validation must reject");
        assert_eq!(err.exit_code(), CliExitCode::Usage);
        assert!(!err.message.contains(SECRET));
        assert!(err.message.contains("[redacted]"));
        assert!(!err.message.contains('\u{1b}'));
    }

    // ---- rendering / host seam -------------------------------------------

    #[test]
    fn render_json_is_plain_utf8_without_ansi() {
        let settings = quiet_launch(false, false);
        let frames = ScriptedRunHost::success()
            .start(&settings)
            .expect("scripted frames");
        let text = render_machine(&frames[..1], OutputFormat::Json);
        assert!(
            text.starts_with('{'),
            "single frame must be one object: {text}"
        );
        let parsed: Value = serde_json::from_str(&text).expect("valid UTF-8 JSON");
        assert_eq!(parsed["schema_version"], json!(MACHINE_SCHEMA_VERSION));
        assert!(!text.contains('\u{1b}'), "no ANSI in machine output");
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
    }

    #[test]
    fn render_json_shape_is_stable_for_one_and_many_frames() {
        let settings = quiet_launch(false, false);
        let frames = ScriptedRunHost::success()
            .start(&settings)
            .expect("scripted frames");
        assert_eq!(frames.len(), 2);

        // Exactly one frame is a bare object — not a one-element array.
        let single = render_machine(&frames[..1], OutputFormat::Json);
        assert!(
            single.starts_with('{') && single.ends_with('}'),
            "single-frame JSON must be one bare object: {single}"
        );
        let single_value: Value = serde_json::from_str(&single).expect("single JSON object");
        assert_eq!(
            single_value,
            json!({
                "type": "run_started",
                "status": "running",
                "schema_version": MACHINE_SCHEMA_VERSION,
            })
        );

        // Multiple frames are an array of objects in host order, each stamped.
        let many = render_machine(&frames, OutputFormat::Json);
        assert!(
            many.starts_with('[') && many.ends_with(']'),
            "multi-frame JSON must be an array: {many}"
        );
        let many_value: Value = serde_json::from_str(&many).expect("JSON array");
        let objects = many_value.as_array().expect("top-level array");
        assert_eq!(objects.len(), frames.len());
        for (object, frame) in objects.iter().zip(&frames) {
            assert_eq!(object["schema_version"], json!(MACHINE_SCHEMA_VERSION));
            assert_eq!(object["type"], frame.get("type").cloned().expect("type"));
            assert_eq!(
                object["status"],
                frame.get("status").cloned().expect("status")
            );
        }
        assert_eq!(objects[0]["type"], json!("run_started"));
        assert_eq!(objects[1]["type"], json!("run_finished"));

        assert!(!single.contains('\u{1b}') && !many.contains('\u{1b}'));
    }

    #[test]
    fn render_jsonl_has_schema_version_on_every_line() {
        let settings = quiet_launch(true, false);
        let frames = ScriptedRunHost::success()
            .start(&settings)
            .expect("scripted frames");
        let text = render_machine(&frames, OutputFormat::Jsonl);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), frames.len());
        for line in lines {
            let parsed: Value = serde_json::from_str(line).expect("one JSON object per line");
            assert_eq!(parsed["schema_version"], json!(MACHINE_SCHEMA_VERSION));
            assert!(!line.contains('\u{1b}'));
        }
    }

    #[test]
    fn quiet_keeps_errors_and_approval_hints() {
        let settings = quiet_launch(true, false);
        assert!(settings.quiet);
        let failure = UnavailableRunHost
            .start(&settings)
            .expect_err("honest failure");
        let err = host_failure(failure, &settings);
        assert_eq!(err.exit_code(), CliExitCode::Usage);
        assert!(err.message.contains("not available in this CLI build"));
        assert!(
            err.message.contains("no approval was granted"),
            "quiet must not suppress safety hints: {}",
            err.message
        );
        assert!(!err.message.contains('\u{1b}'));
    }

    #[test]
    fn verbose_never_prints_secrets() {
        let settings = quiet_launch(false, true);
        let frames = ScriptedRunHost::success()
            .start(&settings)
            .expect("scripted frames");
        let machine = render_machine(&frames, OutputFormat::Jsonl);
        let failure = UnavailableRunHost
            .start(&settings)
            .expect_err("honest failure");
        let diagnostic = host_failure(failure, &settings).message;
        let combined = format!("{machine}{diagnostic}");
        assert!(diagnostic.contains("diagnostic category: InvalidInput"));
        for secret in [SECRET, "sk-", "ghp_", "AKIA", "Bearer "] {
            assert!(!combined.contains(secret), "verbose leaked {secret}");
        }
        assert!(!combined.contains('\u{1b}'));
    }

    #[test]
    fn scripted_host_emits_terminal_frame_last() {
        let settings = quiet_launch(false, false);
        let host = ScriptedRunHost::success();
        let frames = host.start(&settings).expect("scripted frames");
        let terminal = WireFrame::new("run_finished").with("status", json!("completed"));
        assert_eq!(frames.last(), Some(&terminal));
        let rendered = render_machine(&frames, OutputFormat::Jsonl);
        let last = rendered.lines().last().expect("last rendered line");
        assert!(
            last.contains("\"status\":\"completed\""),
            "terminal frame must render last: {last}"
        );
    }
}
