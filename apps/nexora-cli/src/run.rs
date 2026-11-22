//! `nexora run` — machine output over the real shared agent backend
//! (issue #163 4b, brief §9.2–9.5/§11/§17).
//!
//! The Clap surface, the bounded stdin-prompt fallback and the pure validation
//! domain are unchanged from the foundation stage. What changed is the host:
//!
//! - [`CliAgentHost`] is the shipped [`AgentRunHost`] implementation. It emits
//!   each real [`RunFrame`] verbatim (serde `tag = "type"`, `snake_case`) into
//!   a std `mpsc` channel, which the [`RunHost`] seam drains on the calling
//!   thread until the forwarder's terminal `Finished` frame. The agent loop
//!   and the forwarder still live on plain std threads owned by the runtime;
//! - CLI overrides are validated into a local [`LaunchSettings`] value and
//!   are **never** written back to `app_settings`. The launch is recorded
//!   through the existing `agent_runs` repository by the runtime bridge;
//! - non-interactive approval is fail-closed: the first
//!   `ApprovalRequested`/`QuestionRequested` governance frame cancels the run
//!   through the shared registry, never auto-approves, and exits `4` with the
//!   fixed safety hint on stderr;
//! - versioned machine output. Every JSON object / JSONL line carries
//!   `schema_version: 1`; stdout is machine-only (no prose, ANSI, prompt,
//!   argv, paths, or secrets) and every diagnostic goes through the curated
//!   [`CliError`] path on stderr.
//!
//! `RunArgs`/`RunHost` keep their contract names even though this module is
//! named `run`; that repetition is intentional API vocabulary, not a smell.
#![allow(clippy::module_name_repetitions)]

use std::fmt::Write as _;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use nexora_core::catalog;
use nexora_core::config as core_config;
use nexora_core::error::{CliExitCode, CliRunOutcome, CoreErrorKind};
use nexora_runtime::application::agent::approval::AutonomyMode;
use nexora_runtime::application::agent::control::AgentRunEvent;
use nexora_runtime::application::agent::service::{
    resolve_preset_for_workspace, resolve_spend_limit_for_workspace, start_run_with_options,
    AgentRunHost, AgentRunRegistry, AgentRunRequest, RunFrame, RunLaunchOptions,
};
use nexora_runtime::application::conversations::ConversationService;
use nexora_runtime::application::execution::{
    ProviderExecutor, RequestError, RequestExecutionService,
};
use nexora_runtime::infrastructure::database::Database;
use nexora_runtime::infrastructure::providers::registry::execution_sources;
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{is_secret_shaped, safe_key_label, CliError};
use crate::provider_reactor::TokioSpawner;

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

/// Fixed failure texts. No host data, no argv, no prompt, no path, no secret.
const PROVIDER_MESSAGE: &str = "the selected provider could not be used; no approval was granted";
const CREDENTIAL_MESSAGE: &str =
    "a provider credential was missing or unavailable; no secret was read";
const DATABASE_MESSAGE: &str = "a database operation failed; no setting was changed";
const INTERNAL_MESSAGE: &str = "the agent run could not be started; no approval was granted";
const VALIDATION_MESSAGE: &str = "the requested workspace cannot be used as an agent workspace";
const NO_PROVIDER_MESSAGE: &str =
    "no provider is selected; pass --provider or set one with `nexora config set`";
const NO_MODEL_MESSAGE: &str =
    "no model is selected for this provider; pass --model or set one with `nexora config set`";
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

// Constructors are the seam's envelope API: used by the scripted test host and
// by the real host's verbatim frame mapping.
impl WireFrame {
    /// Serialize a real runtime frame verbatim. `RunFrame` is internally
    /// tagged (`"type"`) with `snake_case` fields; `to_value` preserves that
    /// shape exactly. No field is renamed, dropped, or reclassified here.
    pub(crate) fn from_run_frame(frame: &RunFrame) -> Self {
        let serde_json::Value::Object(fields) = serde_json::to_value(frame).unwrap_or_default()
        else {
            return Self::new("event");
        };
        Self { fields }
    }

    /// Create an envelope with a `type` discriminator supplied by the host.
    pub(crate) fn new(event_type: &str) -> Self {
        let mut fields = serde_json::Map::new();
        fields.insert("type".to_string(), json!(event_type));
        Self { fields }
    }

    /// Add one `snake_case` field. Callers supply only host-owned safe values.
    #[cfg(test)]
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
    #[cfg(test)]
    fn validation(message: &str) -> Self {
        Self {
            kind: CoreErrorKind::InvalidInput,
            message: message.to_string(),
        }
    }

    fn provider() -> Self {
        Self {
            kind: CoreErrorKind::Request,
            message: PROVIDER_MESSAGE.to_string(),
        }
    }

    fn credential() -> Self {
        Self {
            kind: CoreErrorKind::Credential,
            message: CREDENTIAL_MESSAGE.to_string(),
        }
    }

    fn database() -> Self {
        Self {
            kind: CoreErrorKind::Database,
            message: DATABASE_MESSAGE.to_string(),
        }
    }

    fn internal() -> Self {
        Self {
            kind: CoreErrorKind::Io,
            message: INTERNAL_MESSAGE.to_string(),
        }
    }

    /// A pre-spawn runtime failure, mapped without ever formatting the error:
    /// classified error payloads can carry provider text, so only the variant
    /// decides the (fixed) CLI message.
    fn from_agent_run_error(
        error: &nexora_runtime::application::agent::service::AgentRunError,
    ) -> Self {
        use nexora_runtime::application::agent::service::AgentRunError as E;
        match error {
            E::WorkspaceProtected => Self {
                kind: CoreErrorKind::InvalidInput,
                message: VALIDATION_MESSAGE.to_string(),
            },
            E::Request(_) => Self::provider(),
            E::Database(_) | E::RunNotPersisted | E::ThreadSpawn(_) | E::Attachment(_) => {
                Self::database()
            }
            E::ConversationNotFound { .. }
            | E::RunAlreadyActive { .. }
            | E::RunNotFound { .. }
            | E::RunNotInterrupted { .. } => Self::internal(),
        }
    }
}

/// Map a classified request-execution failure without formatting its payload.
fn from_request_error(error: &RequestError) -> RunFailure {
    match error {
        RequestError::Credential(_)
        | RequestError::MissingCredentials { .. }
        | RequestError::ProviderUnavailable { .. } => RunFailure::credential(),
        RequestError::UnknownProvider { .. }
        | RequestError::ExecutorUnavailable { .. }
        | RequestError::InvalidCompatEndpoint { .. }
        | RequestError::Execution { .. }
        | RequestError::Database(_) => RunFailure::provider(),
    }
}

/// The CLI-local seam over the real agent backend.
///
/// `Send + Sync` lets the TUI call it from its run thread through a shared
/// `&'static dyn RunHost`. `request_cancel` reaches the live run through the
/// process-wide registry, so a cancel gesture is a real core cancellation, not
/// UI-only feedback.
pub(crate) trait RunHost: Send + Sync {
    fn start(&self, launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure>;

    /// Ask the active run to cancel. Idempotent: repeats are swallowed by the
    /// UI before reaching here, and the registry tolerates them regardless.
    fn request_cancel(&self) {}
}

/// Process-wide run registry. Shared with the TUI's approval/pause/resume/cancel
/// paths so every surface governs the same live run.
static REGISTRY: OnceLock<Arc<AgentRunRegistry>> = OnceLock::new();

pub(crate) fn registry() -> Arc<AgentRunRegistry> {
    Arc::clone(REGISTRY.get_or_init(|| Arc::new(AgentRunRegistry::default())))
}

/// The run id started by the most recent `start` (0 when none). Atomic because
/// `request_cancel` may be called from the TUI input loop on another thread.
static ACTIVE_RUN_ID: AtomicI64 = AtomicI64::new(0);

/// Conversation created for the most recent run. The TUI's approval view reads
/// the live run state of this conversation through the runtime inspector.
static ACTIVE_CONVERSATION_ID: AtomicI64 = AtomicI64::new(0);

pub(crate) fn active_run_id() -> Option<i64> {
    let id = ACTIVE_RUN_ID.load(Ordering::Acquire);
    (id > 0).then_some(id)
}

pub(crate) fn active_conversation_id() -> Option<i64> {
    let id = ACTIVE_CONVERSATION_ID.load(Ordering::Acquire);
    (id > 0).then_some(id)
}

fn install_provider_sources() {
    // The composition root installs the HTTP reactor + keyring credential
    // source exactly once per process. A repeated call (several `run`
    // invocations in one process, or a test that already installed sources)
    // is harmless: the already-installed sources are the correct ones.
    let (executors, credentials) = execution_sources(Arc::new(TokioSpawner));
    let _ =
        nexora_runtime::application::execution::install_execution_sources(executors, credentials);
}

/// Whether a governance frame is an interactive gate the CLI must never answer
/// on the user's behalf. Returns the owning run id.
fn gated_run_id(frame: &RunFrame) -> Option<i64> {
    match frame {
        RunFrame::Governance { run_id, event, .. } if gated_event(event) => Some(*run_id),
        _ => None,
    }
}

fn gated_event(event: &AgentRunEvent) -> bool {
    matches!(
        event,
        AgentRunEvent::ApprovalRequested { .. } | AgentRunEvent::QuestionRequested { .. }
    )
}

/// Which shell drives the run. The distinction is load-bearing for approvals:
/// only a non-interactive `nexora run` may fail closed by cancelling the run;
/// the interactive TUI must leave the park alive so A/D/Ctrl+C can decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostMode {
    /// Machine output, no user present: a gate cancels immediately.
    NonInteractive,
    /// Interactive TUI: a gate stays parked for the user to answer.
    Interactive,
}

/// The shipped host: a thin adapter from the runtime's `AgentRunHost` frames to
/// the CLI's versioned machine-output seam. Per-run instances carry a clone of
/// the frame sender (so one can be handed to the runtime's run/forwarder
/// threads) and the owning shell mode.
#[derive(Clone)]
pub(crate) struct CliAgentHost {
    mode: HostMode,
    registry: Arc<AgentRunRegistry>,
    tx: mpsc::Sender<WireFrame>,
}

impl CliAgentHost {
    fn new(mode: HostMode) -> (Self, mpsc::Receiver<WireFrame>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                mode,
                registry: registry(),
                tx,
            },
            rx,
        )
    }
}

impl AgentRunHost for CliAgentHost {
    fn emit(&self, frame: &RunFrame) {
        // Approval fail-closed applies ONLY to `nexora run`: with no user to
        // answer, the first gate frame cancels through the shared registry and
        // the runner emits an honest `Cancelled` terminal outcome. In the
        // interactive TUI the park stays alive for A/D/Ctrl+C; never auto-grant
        // and never auto-cancel there.
        if self.mode == HostMode::NonInteractive {
            if let Some(run_id) = gated_run_id(frame) {
                let _ = self.registry.cancel(run_id);
            }
        }
        // Best effort: a disconnected receiver means the caller stopped
        // draining, which never affects the run itself.
        let _ = self.tx.send(WireFrame::from_run_frame(frame));
    }

    fn persist_assistant_message(
        &self,
        conversation_id: i64,
        content: &str,
        provider: &str,
        model: &str,
    ) {
        let Ok(db) = crate::store::open_run_db() else {
            return;
        };
        let _ = ConversationService::new(&db).persist_assistant_message(
            conversation_id,
            content,
            provider,
            model,
        );
    }

    fn checkpoint_store(
        &self,
    ) -> Option<nexora_runtime::application::agent::checkpoints::CheckpointStore> {
        // Fail-closed `PerCommandConsent` gating: no verified checkpoint store
        // from the CLI, so dangerous commands still require approval.
        None
    }

    // `on_run_completed` stays at its no-op default: the CLI owns no work queue.
}

/// Deterministic failure host for the validation/diagnostic tests.
#[cfg(test)]
pub(crate) struct ScriptedFailureHost;

#[cfg(test)]
impl RunHost for ScriptedFailureHost {
    fn start(&self, _launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure> {
        Err(RunFailure::validation("scripted host failure"))
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

/// Resolve the provider from a validated CLI override, else from the shared
/// settings store. Never writes settings: an override stays in this process.
fn resolve_provider(launch: &LaunchSettings) -> Result<String, RunFailure> {
    if let Some(provider) = &launch.provider {
        return Ok(provider.clone());
    }
    match crate::store::read(core_config::SELECTED_PROVIDER_KEY) {
        Ok(Some(provider)) if catalog::is_supported_provider(&provider) => Ok(provider),
        _ => Err(RunFailure {
            kind: CoreErrorKind::InvalidInput,
            message: NO_PROVIDER_MESSAGE.to_string(),
        }),
    }
}

/// Resolve the model from a validated CLI override, else the persisted
/// selection, else the provider's first catalog model. Never writes settings.
fn resolve_model(launch: &LaunchSettings, provider: &str) -> Result<String, RunFailure> {
    if let Some(model) = &launch.model {
        return Ok(model.clone());
    }
    if let Ok(Some(model)) = crate::store::read(core_config::SELECTED_MODEL_KEY) {
        if catalog::is_listed_model(&model)
            || catalog::models_for_provider(provider)
                .iter()
                .any(|info| info.name == model)
            || core_config::is_valid_custom_model_id(&model)
        {
            return Ok(model);
        }
    }
    catalog::models_for_provider(provider)
        .into_iter()
        .next()
        .map(|info| info.name)
        .ok_or_else(|| RunFailure {
            kind: CoreErrorKind::InvalidInput,
            message: NO_MODEL_MESSAGE.to_string(),
        })
}

/// Map a validated autonomy string onto the runtime's ladder. The CLI's
/// validation domain and the core domain are the same three values.
fn autonomy_mode(mode: &str) -> AutonomyMode {
    match mode {
        "supervised" => AutonomyMode::Supervised,
        "full_autonomous" => AutonomyMode::FullAutonomous,
        _ => AutonomyMode::SemiAutonomous,
    }
}

impl RunHost for CliAgentHost {
    fn start(&self, launch: &LaunchSettings) -> Result<Vec<WireFrame>, RunFailure> {
        // Composition root for the CLI process: installs the scoped Tokio HTTP
        // reactor and the OS-keyring credential source exactly once.
        install_provider_sources();
        Self::start_real(self.mode, launch, None, None, None)
    }

    fn request_cancel(&self) {
        let run_id = ACTIVE_RUN_ID.load(Ordering::Acquire);
        if run_id > 0 {
            let _ = self.registry.cancel(run_id);
        }
    }
}

impl CliAgentHost {
    /// The real run path. Every injected argument is a test seam: `db` uses an
    /// in-memory/shared synthetic database, `executor` is a scripted
    /// `ProviderExecutor`, and `credential` is a stub value. None of them is
    /// reachable from `RunHost::start`, which always uses the real DB, the real
    /// provider registry and the real keyring seam.
    fn start_real(
        mode: HostMode,
        launch: &LaunchSettings,
        db: Option<Database>,
        executor: Option<Arc<dyn ProviderExecutor + Send + Sync>>,
        credential: Option<String>,
    ) -> Result<Vec<WireFrame>, RunFailure> {
        Self::start_real_with_observer(mode, launch, db, executor, credential, &|_| {})
    }

    /// Full real-host path with a frame observer. Production passes a no-op;
    /// tests observe a live gate and drive the real registry (approve/pause/
    /// resume) without a TTY or network.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_real_with_observer(
        mode: HostMode,
        launch: &LaunchSettings,
        db: Option<Database>,
        executor: Option<Arc<dyn ProviderExecutor + Send + Sync>>,
        credential: Option<String>,
        on_frame: &dyn Fn(&WireFrame),
    ) -> Result<Vec<WireFrame>, RunFailure> {
        let db = match db {
            Some(db) => db,
            None => crate::store::open_run_db().map_err(|_| RunFailure::database())?,
        };
        let provider = resolve_provider(launch)?;
        let model = resolve_model(launch, &provider)?;

        let executor = match executor {
            Some(executor) => executor,
            None => RequestExecutionService::new(&db)
                .resolve_executor_for(&provider)
                .map_err(|err| from_request_error(&err))?,
        };
        let secret = match credential {
            Some(secret) => secret,
            None => RequestExecutionService::new(&db)
                .resolve_credential(&provider)
                .map_err(|err| from_request_error(&err))?,
        };

        let workspace = launch.workspace.as_ref().map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            PathBuf::from,
        );
        let autonomy = autonomy_mode(&launch.mode);
        // Read-only resolution from the shared settings tiers: a CLI override
        // never reaches any settings writer.
        let preset = resolve_preset_for_workspace(&db, Some(&workspace));
        let spend_limit = launch
            .budget_micro_usd
            .or_else(|| resolve_spend_limit_for_workspace(&db, Some(&workspace)));
        let conversation_id = ConversationService::new(&db)
            .create("CLI run")
            .map_err(|_| RunFailure::database())?;
        ACTIVE_CONVERSATION_ID.store(conversation_id, Ordering::Release);

        let request = AgentRunRequest {
            conversation_id,
            user_request: launch.prompt.clone(),
            provider,
            model,
            credential: secret,
            max_iterations: Some(usize::try_from(launch.max_steps).unwrap_or(usize::MAX)),
            spend_limit_micro_usd: spend_limit,
        };
        let (host, rx) = Self::new(mode);
        let run_id = start_run_with_options(
            &db,
            Arc::clone(&host.registry),
            Arc::new(host.clone()) as Arc<dyn AgentRunHost>,
            executor,
            workspace,
            request,
            autonomy,
            preset,
            RunLaunchOptions::default(),
        )
        .map_err(|err| RunFailure::from_agent_run_error(&err))?;
        ACTIVE_RUN_ID.store(run_id, Ordering::Release);

        // std-only drain on the calling thread: `recv` ends when the runtime's
        // forwarder drops its sender, and the forwarder guarantees `Finished`
        // is the last frame. No Tokio, no busy loop, no fabricated terminal.
        let mut frames = Vec::new();
        for frame in rx {
            let terminal = frame.get("type").and_then(Value::as_str) == Some("finished");
            on_frame(&frame);
            frames.push(frame);
            if terminal {
                break;
            }
        }
        ACTIVE_RUN_ID.store(0, Ordering::Release);
        Ok(frames)
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

/// Host for non-interactive `nexora run` (`HostMode::NonInteractive`): no user
/// is present, so the first approval/question gate fails closed — the run is
/// cancelled, the fixed hint goes to stderr and the process exits 4. It never
/// auto-approves.
pub(crate) fn host() -> &'static dyn RunHost {
    static HOST: OnceLock<(CliAgentHost, Mutex<mpsc::Receiver<WireFrame>>)> = OnceLock::new();
    &HOST
        .get_or_init(|| {
            let (host, rx) = CliAgentHost::new(HostMode::NonInteractive);
            (host, Mutex::new(rx))
        })
        .0
}

/// Host for the interactive TUI (`HostMode::Interactive`): the same seam and
/// registry, but a governance gate stays parked for `A`/`D`/`Ctrl+C` through
/// the real registry/`resolve_approval` path — no auto-cancel, no
/// auto-approve. Only core-emitted frames set the local decision and the
/// approval return point.
pub(crate) fn interactive_host() -> &'static dyn RunHost {
    static HOST: OnceLock<(CliAgentHost, Mutex<mpsc::Receiver<WireFrame>>)> = OnceLock::new();
    &HOST
        .get_or_init(|| {
            let (host, rx) = CliAgentHost::new(HostMode::Interactive);
            (host, Mutex::new(rx))
        })
        .0
}

/// The result of a real `run`: machine stdout, the shared exit code, and an
/// optional fixed stderr notice (the non-interactive approval hint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunOutcome {
    pub(crate) text: String,
    pub(crate) code: CliExitCode,
    pub(crate) notice: Option<String>,
}

/// Whether the stream carried a governance gate that the CLI could not answer.
fn saw_gate(frames: &[WireFrame]) -> bool {
    frames.iter().any(|frame| {
        frame.get("type").and_then(Value::as_str) == Some("governance")
            && matches!(
                frame
                    .get("event")
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str),
                Some("approval_requested" | "question_requested")
            )
    })
}

/// Classify the secret-free, fixed-vocabulary error text carried by a
/// `RunFinished` frame. `AgentError` is crate-private to the runtime, so the
/// classified `Display` text is the wire contract: internal categories
/// (context exhaustion, persistence, empty response) must NOT be reported as
/// provider/network/auth failures. Unknown text fails closed as `Failed` (1),
/// never as a provider failure.
fn classify_run_error(error: &str) -> CliRunOutcome {
    if error.starts_with("the AI provider") || error.starts_with("provider reported") {
        return CliRunOutcome::Provider;
    }
    if error.contains("context window exhausted") {
        return CliRunOutcome::ContextExhausted;
    }
    if error.contains("run history could not be saved") {
        return CliRunOutcome::PersistenceFailure;
    }
    CliRunOutcome::Failed
}

/// Map the terminal frame's persisted status onto the shared 0–5 table. A
/// missing non-terminal frame fails closed as `Internal` — never a fake zero.
fn exit_code_for_frames(frames: &[WireFrame]) -> CliExitCode {
    let Some(last) = frames.last() else {
        return CliExitCode::Internal;
    };
    if last.get("type").and_then(Value::as_str) != Some("finished") {
        return CliExitCode::Internal;
    }
    let event = last.get("event");
    let status = event
        .and_then(|event| event.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("error");
    let outcome = match status {
        "completed" => CliRunOutcome::Completed,
        "cancelled" => CliRunOutcome::Cancelled,
        "budget_exhausted" => CliRunOutcome::BudgetExhausted,
        "spend_limit_exceeded" => CliRunOutcome::SpendLimitExceeded,
        "error" => classify_run_error(
            event
                .and_then(|event| event.get("error"))
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        _ => CliRunOutcome::Failed,
    };
    CliExitCode::for_run_outcome(outcome)
}

/// Turn a drained frame stream into the process outcome. Shared by `execute`
/// and by the full-path tests so exit codes are pinned end to end.
fn finish_run(frames: &[WireFrame], format: OutputFormat) -> RunOutcome {
    let code = exit_code_for_frames(frames);
    let notice =
        (code == CliExitCode::Cancelled && saw_gate(frames)).then(|| NO_APPROVAL_HINT.to_string());
    RunOutcome {
        text: render_machine(frames, format),
        code,
        notice,
    }
}

/// Build validated launch settings for a task submitted from the TUI. Uses the
/// same defaults as a bare `nexora run <prompt>` and never persists overrides.
pub(crate) fn tui_launch(
    prompt: String,
    color: String,
    ascii: bool,
    motion: String,
) -> Result<LaunchSettings, CliError> {
    let args = RunArgs {
        prompt: Some(prompt.clone()),
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
    };
    let caps = CapabilityArgs {
        color,
        ascii,
        motion,
    };
    validate(&args, &caps, prompt)
}

/// Command entry point: resolve prompt, validate, then hand off to the real
/// host. Validation and pre-spawn failures keep the `CliError` path; terminal
/// run statuses map onto the shared exit table via [`RunOutcome`].
pub(crate) fn execute(args: &RunArgs, caps: &CapabilityArgs) -> Result<RunOutcome, CliError> {
    let prompt = resolve_prompt(
        args.prompt.clone(),
        io::stdin().is_terminal(),
        &stdin_prompt_reader,
    )?;
    let launch = validate(args, caps, prompt)?;
    let frames = host()
        .start(&launch)
        .map_err(|failure| host_failure(failure, &launch))?;
    Ok(finish_run(&frames, launch.format))
}

/// Process-wide registry claims one conversation id; every in-memory test DB
/// starts at id 1, so all real-host tests across CLI and TUI serialize here.
#[cfg(test)]
pub(crate) static REAL_HOST_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::VecDeque;

    use clap::Parser;
    use nexora_core::error::CliExitCode;
    use nexora_core::storage::database::in_memory_database;
    use nexora_core::storage::repository::agent_runs::AgentRunRepository;
    use nexora_core::storage::repository::settings::SettingsRepository;
    use nexora_runtime::application::execution::{AiResponse, ExecutorError, TokenUsage, ToolCall};

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
        let failure = ScriptedFailureHost
            .start(&settings)
            .expect_err("scripted failure");
        let err = host_failure(failure, &settings);
        assert_eq!(err.exit_code(), CliExitCode::Usage);
        assert!(err.message.contains("scripted host failure"));
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
        let failure = ScriptedFailureHost
            .start(&settings)
            .expect_err("scripted failure");
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

    // ---- real host over the shared runtime (scripted provider, no network) --

    struct ScriptedExecutor {
        steps: Mutex<VecDeque<Result<AiResponse, ExecutorError>>>,
    }

    impl ScriptedExecutor {
        fn new(steps: Vec<Result<AiResponse, ExecutorError>>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
            }
        }
    }

    impl ProviderExecutor for ScriptedExecutor {
        fn execute(
            &self,
            _request: &nexora_runtime::application::execution::AiRequest,
            _credential: &str,
            _token: &nexora_runtime::application::agent::control::CancellationToken,
        ) -> Result<AiResponse, ExecutorError> {
            self.steps
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(Err(ExecutorError::Failure))
        }
    }

    fn text_response(content: &str) -> AiResponse {
        AiResponse {
            content: content.to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    fn tool_response(name: &str, tool: &str, arguments: &str) -> AiResponse {
        AiResponse {
            content: String::new(),
            model: "test-model".to_string(),
            tool_calls: vec![ToolCall {
                id: format!("{name}-1"),
                name: tool.to_string(),
                arguments: arguments.to_string(),
                thought_signature: None,
            }],
            usage: None,
        }
    }

    fn real_launch(
        mode: &str,
        prompt: &str,
        workspace: Option<&std::path::Path>,
    ) -> LaunchSettings {
        let mut args = minimal_args();
        args.provider = Some("openai".to_string());
        args.model = Some("gpt-5.6-terra".to_string());
        args.mode = Some(mode.to_string());
        args.workspace = workspace.map(|path| path.to_string_lossy().into_owned());
        validate(&args, &minimal_caps(), prompt.to_string()).expect("valid real launch")
    }

    fn synthetic_workspace(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nexora-cli-real-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("synthetic workspace");
        dir
    }

    #[test]
    fn real_host_emits_verbatim_frames_and_terminal_last() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let db = in_memory_database();
        let executor: Arc<dyn ProviderExecutor + Send + Sync> =
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("all done"))]));
        let launch = real_launch("semi_autonomous", "do work", None);
        let frames = CliAgentHost::start_real(
            HostMode::NonInteractive,
            &launch,
            Some(db),
            Some(executor),
            Some("stub-credential-value".to_string()),
        )
        .expect("real run over scripted provider");

        let rendered = render_machine(&frames, OutputFormat::Jsonl);
        let objects: Vec<Value> = rendered
            .lines()
            .map(|line| serde_json::from_str(line).expect("one object per line"))
            .collect();
        assert!(!objects.is_empty(), "the real bridge must emit frames");
        assert!(
            objects
                .iter()
                .all(|object| object["schema_version"] == json!(MACHINE_SCHEMA_VERSION)),
            "every real frame carries the version stamp"
        );
        assert!(
            objects.iter().any(|object| object["type"] == "step"),
            "the persisted step must reach stdout verbatim: {rendered}"
        );
        let last = objects.last().expect("terminal frame");
        assert_eq!(last["type"], json!("finished"));
        assert_eq!(last["event"]["status"], json!("completed"));
        assert!(!rendered.contains('\u{1b}'), "no ANSI in machine output");
        assert!(!rendered.contains("stub-credential-value"));
        assert_eq!(exit_code_for_frames(&frames), CliExitCode::Ok);
    }

    #[test]
    fn real_host_plain_text_is_no_ansi_and_carries_no_credential() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let db = in_memory_database();
        let executor: Arc<dyn ProviderExecutor + Send + Sync> = Arc::new(ScriptedExecutor::new(
            vec![Err(ExecutorError::Authentication)],
        ));
        let launch = real_launch("supervised", "plain text run", None);
        let frames = CliAgentHost::start_real(
            HostMode::NonInteractive,
            &launch,
            Some(db),
            Some(executor),
            Some(SECRET.to_string()),
        )
        .expect("classified provider failure still emits frames");
        for format in [OutputFormat::Text, OutputFormat::Json, OutputFormat::Jsonl] {
            let rendered = render_machine(&frames, format);
            assert!(!rendered.contains('\u{1b}'), "{format:?} emitted ANSI");
            assert!(
                !rendered.contains(SECRET),
                "{format:?} leaked the credential"
            );
        }
        assert_eq!(exit_code_for_frames(&frames), CliExitCode::Provider);
    }

    #[test]
    fn launch_settings_are_recorded_without_writing_overrides() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let db = in_memory_database();
        let executor: Arc<dyn ProviderExecutor + Send + Sync> =
            Arc::new(ScriptedExecutor::new(vec![Ok(text_response("done"))]));
        let launch = real_launch("full_autonomous", "record the launch", None);
        let _ = CliAgentHost::start_real(
            HostMode::NonInteractive,
            &launch,
            Some(db.clone()),
            Some(executor),
            Some("stub".to_string()),
        )
        .expect("run");

        let runs = AgentRunRepository::new(&db)
            .list_runs_by_started_at_desc()
            .expect("read runs");
        let run = runs.first().expect("one persisted run");
        assert_eq!(run.model, "gpt-5.6-terra", "model recorded on the run row");
        assert_eq!(run.mode, "full_autonomous", "mode recorded on the run row");
        assert_eq!(run.status, "completed");
        // A CLI override is never persisted into app_settings.
        for key in [
            nexora_core::config::AUTONOMY_KEY,
            nexora_core::config::SELECTED_PROVIDER_KEY,
            nexora_core::config::SELECTED_MODEL_KEY,
            nexora_core::config::SPEND_LIMIT_KEY,
        ] {
            assert_eq!(
                SettingsRepository::new(&db)
                    .get(key)
                    .expect("settings read"),
                None,
                "CLI must not write {key}"
            );
        }
    }

    #[test]
    fn approval_request_fails_closed_and_exits_distinct_from_provider_failure() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = synthetic_workspace("approval");
        let db = in_memory_database();
        let executor: Arc<dyn ProviderExecutor + Send + Sync> =
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response(
                    "gate",
                    "write_file",
                    r#"{"path":"a.txt","content":"ok"}"#,
                )),
                Ok(text_response("must never run")),
            ]));
        let launch = real_launch("supervised", "needs approval", Some(&workspace));
        let frames = CliAgentHost::start_real(
            HostMode::NonInteractive,
            &launch,
            Some(db),
            Some(executor),
            Some("stub".to_string()),
        )
        .expect("run is cancelled, not a pre-spawn failure");

        assert!(saw_gate(&frames), "the gate frame must be emitted");
        // Full run-path outcome, not just the mapping helper: exit 4 + hint.
        let outcome = finish_run(&frames, OutputFormat::Jsonl);
        assert_eq!(outcome.code, CliExitCode::Cancelled);
        assert_eq!(outcome.notice.as_deref(), Some(NO_APPROVAL_HINT));
        assert!(
            outcome.text.contains("approval_requested"),
            "honest governance event: {}",
            outcome.text
        );
        assert!(
            !outcome.text.contains("\"approved\":true"),
            "no auto-approval may be fabricated: {}",
            outcome.text
        );
        assert!(
            !workspace.join("a.txt").exists(),
            "the gated tool must never dispatch"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn interactive_host_leaves_the_approval_parked_for_the_user() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workspace = synthetic_workspace("interactive");
        let db = in_memory_database();
        let executor: Arc<dyn ProviderExecutor + Send + Sync> =
            Arc::new(ScriptedExecutor::new(vec![
                Ok(tool_response(
                    "gate",
                    "write_file",
                    r#"{"path":"a.txt","content":"ok"}"#,
                )),
                Ok(text_response("finished after approval")),
            ]));
        let launch = real_launch("supervised", "needs a human", Some(&workspace));
        let decision_db = db.clone();
        let responded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_responded = Arc::clone(&responded);
        let frames = CliAgentHost::start_real_with_observer(
            HostMode::Interactive,
            &launch,
            Some(db),
            Some(executor),
            Some("stub".to_string()),
            &move |frame| {
                let governance = frame.get("type").and_then(Value::as_str) == Some("governance");
                let requested = frame
                    .get("event")
                    .and_then(|event| event.get("type"))
                    .and_then(Value::as_str)
                    == Some("approval_requested");
                if governance && requested {
                    let call_id = frame
                        .get("event")
                        .and_then(|event| event.get("call_id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let run_id = frame.get("run_id").and_then(Value::as_i64).unwrap_or(0);
                    let registry = crate::run::registry();
                    let ok = nexora_runtime::application::agent::permissions::resolve_approval(
                        registry.as_ref(),
                        &decision_db,
                        run_id,
                        &call_id,
                        true,
                        None,
                    )
                    .is_ok();
                    saw_responded.store(ok, Ordering::SeqCst);
                }
            },
        )
        .expect("interactive run with a scripted provider");

        assert!(
            responded.load(Ordering::SeqCst),
            "the park must survive until the user answers it"
        );
        let outcome = finish_run(&frames, OutputFormat::Jsonl);
        assert_eq!(outcome.code, CliExitCode::Ok, "no auto-cancel in TUI mode");
        assert_eq!(outcome.notice, None, "no non-interactive hint in TUI mode");
        assert!(outcome.text.contains("\"approved\":true"));

        // Cross-platform proof of dispatch: after the core-emitted resolution,
        // the recorder emitted a `tool_call` step for the approved tool. An
        // OS-specific file check would fail on Windows where the synthetic
        // workspace path is subject to different ignore rules, without saying
        // anything about whether the core dispatched the tool.
        let resolved = frames
            .iter()
            .position(|frame| {
                frame.get("type").and_then(Value::as_str) == Some("governance")
                    && frame
                        .get("event")
                        .and_then(|event| event.get("type"))
                        .and_then(Value::as_str)
                        == Some("approval_resolved")
            })
            .expect("core emitted an approval resolution");
        assert!(
            frames[resolved + 1..].iter().any(|frame| {
                frame.get("type").and_then(Value::as_str) == Some("step")
                    && frame
                        .get("event")
                        .and_then(|event| event.get("tool_name"))
                        .and_then(Value::as_str)
                        == Some("write_file")
            }),
            "the approved tool was dispatched after core confirmation: {}",
            outcome.text
        );
        let terminal = frames.last().expect("terminal frame");
        assert_eq!(
            terminal.get("type").and_then(Value::as_str),
            Some("finished"),
            "Finished is last"
        );
        assert_eq!(
            terminal
                .get("event")
                .and_then(|event| event.get("status"))
                .and_then(Value::as_str),
            Some("completed"),
            "the run completed after the approved tool dispatched"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn spend_limit_exhaustion_exits_five_over_the_full_run_path() {
        let _serial = REAL_HOST_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let db = in_memory_database();
        let usage = AiResponse {
            content: "never final".to_string(),
            model: "test-model".to_string(),
            tool_calls: Vec::new(),
            usage: Some(TokenUsage {
                input_tokens: 400_000,
                output_tokens: 0,
                cached_tokens: 0,
            }),
        };
        let executor: Arc<dyn ProviderExecutor + Send + Sync> =
            Arc::new(ScriptedExecutor::new(vec![Ok(usage)]));
        let mut launch = real_launch("semi_autonomous", "spend test", None);
        launch.budget_micro_usd = Some(1_000_000);
        let frames = CliAgentHost::start_real(
            HostMode::NonInteractive,
            &launch,
            Some(db),
            Some(executor),
            Some("stub".to_string()),
        )
        .expect("spend-limited run");
        let outcome = finish_run(&frames, OutputFormat::Jsonl);
        assert_eq!(outcome.code, CliExitCode::Budget);
        assert_eq!(outcome.notice, None, "budget is not an approval hint");
        assert!(outcome.text.contains("spend_limit_exceeded"));
    }

    #[test]
    fn classified_error_text_is_not_always_a_provider_failure() {
        let cases = [
            (
                "the AI provider could not be reached (network or timeout)",
                CliExitCode::Provider,
            ),
            (
                "the AI provider rejected the stored credential (HTTP 401/403)",
                CliExitCode::Provider,
            ),
            (
                "provider reported insufficient credits/quota (HTTP 402)",
                CliExitCode::Provider,
            ),
            (
                "agent stopped: context window exhausted after recovery attempts",
                CliExitCode::Internal,
            ),
            (
                "agent stopped: run history could not be saved",
                CliExitCode::Internal,
            ),
            (
                "agent stopped: the model returned an empty response",
                CliExitCode::Internal,
            ),
            ("unclassified failure", CliExitCode::Internal),
        ];
        for (error, expected) in cases {
            assert_eq!(
                CliExitCode::for_run_outcome(classify_run_error(error)),
                expected,
                "error text {error}"
            );
            let frames = vec![WireFrame::new("finished")
                .with("event", json!({"status": "error", "error": error}))];
            assert_eq!(exit_code_for_frames(&frames), expected, "frame {error}");
        }
    }

    #[test]
    fn terminal_status_exit_codes_are_pinned() {
        let cases = [
            ("completed", CliExitCode::Ok),
            ("cancelled", CliExitCode::Cancelled),
            ("budget_exhausted", CliExitCode::Budget),
            ("spend_limit_exceeded", CliExitCode::Budget),
        ];
        for (status, expected) in cases {
            let frames = vec![WireFrame::new("finished").with("event", json!({"status": status}))];
            assert_eq!(exit_code_for_frames(&frames), expected, "status {status}");
        }
        assert_eq!(exit_code_for_frames(&[]), CliExitCode::Internal);
        let non_terminal = vec![WireFrame::new("run_started")];
        assert_eq!(exit_code_for_frames(&non_terminal), CliExitCode::Internal);
    }
}
