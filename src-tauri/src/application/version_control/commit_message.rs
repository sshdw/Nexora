//! AI-generated conventional-commit messages and commit explanations.
//!
//! Split from `version_control.rs` (AC-207): the model-facing prompt
//! builders, sanitizers, and the generate/explain entry points live here;
//! the guarded writes they describe are siblings.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::application::agent::runner::DEFAULT_REQUEST_TIMEOUT;
use crate::application::execution::{AiMessage, AiRequest, AiRole, RequestError};
use crate::infrastructure::database::Database;

use super::mutate::{is_conventional_message, validate_commit_message};
use super::status::{find_commit_by_hash, git_commit_diff};
use super::{
    open_workspace_repo, VersionControlError, MAX_COMMIT_EXPLAIN_BYTES,
    MAX_COMMIT_EXPLANATION_CHARS, MAX_COMMIT_MESSAGE_LEN, MAX_COMMIT_PROMPT_BYTES,
    MAX_COMMIT_SUBJECT_LEN,
};

// ---------------------------------------------------------------------------
// AI-generated conventional-commit message
// ---------------------------------------------------------------------------

/// Narrow prompt turning a staged-diff summary into a conventional-commit
/// message. The model must reply with only the message: a
/// `type(scope): subject` first line plus an optional body.
pub(crate) fn build_commit_prompt(summary: &str, truncated: bool) -> String {
    let mut prompt = String::from(
        "Write a conventional commit message for the staged git changes summarized below.\n\
         Reply with ONLY the commit message, no code fences, no explanation.\n\
         The first line must be `type(scope): subject` where type is one of \
         feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert; \
         scope is a short lowercase area name; subject is an imperative short summary \
         under 72 characters. After a blank line, add 1-3 short body lines describing \
         what changed and why.\n",
    );
    if truncated {
        prompt.push_str(
            "Note: the change summary was truncated to fit; describe only what is shown.\n",
        );
    }
    prompt.push_str("Staged changes:\n");
    prompt.push_str(summary);
    prompt
}

/// Coerce raw model output into a valid conventional-commit message: strip
/// code fences, take the first conventional subject line as the subject plus
/// the following body lines, and fall back to a `chore(workspace):` subject
/// when no conventional line is present. The result always satisfies
/// [`validate_commit_message`] and [`is_conventional_message`].
pub(crate) fn sanitize_ai_message(raw: &str) -> String {
    // Strip NULs upfront: `validate_commit_message` rejects them, so the
    // sanitized output must never carry one through from model output.
    let without_nul = raw.replace('\0', "");
    let stripped = without_nul.trim().replace("\r\n", "\n");
    let all: Vec<&str> = stripped
        .lines()
        .map(str::trim_end)
        .filter(|line| {
            let line = line.trim();
            line != "```" && !line.starts_with("```")
        })
        .collect();
    // Keep interior blank lines (subject/body separator) while trimming
    // leading and trailing empties.
    let start = all
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(all.len());
    let end = all
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let lines = if start < end { &all[start..end] } else { &[] };
    let conventional_at = lines.iter().position(|line| is_conventional_message(line));
    let mut message = if let Some(index) = conventional_at {
        let mut out = vec![lines[index].trim().to_string()];
        for line in lines.iter().skip(index + 1) {
            out.push((*line).to_string());
        }
        out.join("\n").trim().to_string()
    } else {
        let first = lines
            .iter()
            .find(|line| !line.trim().is_empty())
            .map_or("", |line| *line)
            .trim();
        let subject: String = first.chars().take(72).collect();
        let subject = subject.trim();
        if subject.is_empty() {
            "chore(workspace): update files".to_string()
        } else {
            format!("chore(workspace): {subject}")
        }
    };
    // Enforce the backend bounds so the sanitized message always validates.
    if message.chars().count() > MAX_COMMIT_MESSAGE_LEN {
        let mut end = MAX_COMMIT_MESSAGE_LEN;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message = message.trim_end().to_string();
    }
    let mut split: Vec<String> = message.split('\n').map(str::to_string).collect();
    if let Some(subject) = split.first() {
        if subject.chars().count() > MAX_COMMIT_SUBJECT_LEN {
            let mut end = MAX_COMMIT_SUBJECT_LEN;
            while !subject.is_char_boundary(end) {
                end -= 1;
            }
            split[0] = subject[..end].trim_end().to_string();
            // Truncating the subject could only break conventional shape by
            // cutting the scope/type; re-check and fall back if needed.
            let rejoined = split.join("\n");
            if is_conventional_message(&rejoined) {
                return rejoined;
            }
            let fallback_subject: String = rejoined
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(72)
                .collect();
            return format!("chore(workspace): {}", fallback_subject.trim());
        }
    }
    debug_assert!(validate_commit_message(&message).is_ok());
    debug_assert!(is_conventional_message(&message));
    message
}

/// AI-generated commit message plus whether the staged summary was truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeneratedCommitMessage {
    /// Sanitized conventional-commit message (subject plus optional body).
    pub message: String,
    /// Whether the staged summary fed to the model was truncated at
    /// [`MAX_COMMIT_PROMPT_BYTES`].
    pub truncated_input: bool,
}

/// Failures for AI commit-message generation: workspace/git problems or the
/// shared AI execution failure. Both sides stay secret-free.
#[derive(Debug)]
pub(crate) enum CommitMessageError {
    /// The staged summary could not be read (not a repository, git failure).
    VersionControl(VersionControlError),
    /// The AI request failed (unknown provider, missing credentials,
    /// provider failure). Carries no prompt or diff content.
    Request(RequestError),
}

impl std::fmt::Display for CommitMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionControl(inner) => write!(f, "{inner}"),
            Self::Request(_) => write!(f, "the commit message could not be generated"),
        }
    }
}

impl std::error::Error for CommitMessageError {}

impl From<VersionControlError> for CommitMessageError {
    fn from(err: VersionControlError) -> Self {
        Self::VersionControl(err)
    }
}

impl From<RequestError> for CommitMessageError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// Summarize the staged (index vs `HEAD`) diff, capped at
/// [`MAX_COMMIT_PROMPT_BYTES`] with a truncation flag. An empty summary means
/// nothing is staged.
///
/// # Errors
///
/// See [`open_workspace_repo`].
pub(super) fn staged_diff_summary(
    workspace_root: &Path,
) -> Result<(String, bool), VersionControlError> {
    let (_, repo) = open_workspace_repo(workspace_root)?;
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let index = repo.index().map_err(|_| VersionControlError::GitFailed)?;
    let mut opts = git2::DiffOptions::new();
    opts.context_lines(3);
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut opts))
        .map_err(|_| VersionControlError::GitFailed)?;
    let mut text = String::new();
    // `Deltas` walks `0..count` in order, so the enumerated index is the
    // delta index `Patch::from_diff` needs.
    for (delta_index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path_bytes()
            .or_else(|| delta.old_file().path_bytes())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        text.push_str("file: ");
        text.push_str(&path);
        text.push('\n');
        if let Some(mut patch) = git2::Patch::from_diff(&diff, delta_index)
            .map_err(|_| VersionControlError::GitFailed)?
        {
            let buf = patch.to_buf().map_err(|_| VersionControlError::GitFailed)?;
            let bytes: &[u8] = &buf;
            if bytes.contains(&0) {
                text.push_str("(binary file, content omitted)\n");
            } else {
                text.push_str(&String::from_utf8_lossy(bytes));
            }
        }
        text.push('\n');
        if text.len() >= MAX_COMMIT_PROMPT_BYTES {
            break;
        }
    }
    let truncated = text.len() > MAX_COMMIT_PROMPT_BYTES;
    if truncated {
        let mut end = MAX_COMMIT_PROMPT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok((text, truncated))
}

/// Generate a conventional-commit message for the staged changes through the
/// existing AI execution path: the staged diff summary (capped at
/// [`MAX_COMMIT_PROMPT_BYTES`]) feeds a narrow prompt to a single text-only
/// [`AiRequest`] executed by the shared
/// [`RequestExecutionService`](crate::application::execution::RequestExecutionService)
/// — the same execution boundary `send_message` and the agent-run bridge use.
/// Credentials resolve from the OS keyring only; no new key input exists on
/// this path, and nothing is persisted (no conversation message is created).
/// The model output is coerced through [`sanitize_ai_message`], so the
/// returned message always validates.
///
/// # Errors
///
/// Returns [`CommitMessageError::VersionControl`] when the workspace is not a
/// repository or nothing is staged, [`CommitMessageError::Request`] when AI
/// execution fails.
pub(crate) fn generate_commit_message(
    db: &Database,
    workspace_root: &Path,
    provider: &str,
    model: &str,
) -> Result<GeneratedCommitMessage, CommitMessageError> {
    use crate::application::execution::RequestExecutionService;
    let (summary, truncated) = staged_diff_summary(workspace_root)?;
    if summary.trim().is_empty() {
        return Err(VersionControlError::GitFailed.into());
    }
    let prompt = build_commit_prompt(&summary, truncated);
    let request = AiRequest {
        provider: provider.to_string(),
        model: model.to_string(),
        messages: std::sync::Arc::new(vec![AiMessage {
            role: AiRole::User,
            content: prompt,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }]),
        tools: std::sync::Arc::new(Vec::new()),
        model_config: None,
        request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
    };
    let response = RequestExecutionService::new(db).execute(&request)?;
    let message = sanitize_ai_message(&response.content);
    validate_commit_message(&message).map_err(CommitMessageError::from)?;
    Ok(GeneratedCommitMessage {
        message,
        truncated_input: truncated,
    })
}

// ---------------------------------------------------------------------------
// AI-generated commit explanation (timeline detail)
// ---------------------------------------------------------------------------

/// AI explanation of one historical commit plus whether the diff excerpt was
/// truncated. Copy-only: the text is rendered for copying, never applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CommitExplanation {
    /// What the commit changed and why it matters, in plain language.
    pub explanation: String,
    /// Whether the commit diff fed to the model was cut at
    /// [`MAX_COMMIT_EXPLAIN_BYTES`].
    pub truncated_input: bool,
}

/// Failures for AI commit explanation: repository/commit problems or the
/// shared AI execution failure. Both sides stay secret-free: the diff may
/// carry user secrets, so no variant echoes caller content.
#[derive(Debug)]
pub(crate) enum CommitExplainError {
    /// The commit hash was malformed or unknown, or the commit changed no
    /// files (nothing to explain).
    InvalidInput,
    /// The AI request failed (unknown provider, missing credentials,
    /// provider failure). Carries no prompt or diff content.
    Request(RequestError),
}

impl std::fmt::Display for CommitExplainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput => write!(f, "the commit diff to explain is invalid"),
            Self::Request(_) => write!(f, "the commit explanation could not be generated"),
        }
    }
}

impl std::error::Error for CommitExplainError {}

impl From<RequestError> for CommitExplainError {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// Narrow prompt turning one commit's diff into a plain-language
/// explanation. The model must reply with ONLY a short explanation: what
/// changed and why it matters. No commands, no fixes, nothing to run.
pub(crate) fn build_commit_explain_prompt(
    summary_line: &str,
    excerpt: &str,
    truncated: bool,
) -> String {
    let mut prompt = String::from(
        "Explain the git commit below in plain language.\n\
         Reply with ONLY a short explanation: what changed and why it matters. \
         Do not suggest commands or edits; describe only.\n",
    );
    if truncated {
        prompt
            .push_str("Note: the commit diff was truncated to fit; explain only what is shown.\n");
    }
    prompt.push_str("Commit: ");
    prompt.push_str(summary_line);
    prompt.push('\n');
    prompt.push_str("Diff:\n");
    prompt.push_str(excerpt);
    prompt
}

/// Cut `text` to `max_chars` characters on a char boundary.
fn truncate_explain_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_string()
}

/// Coerce raw model output into a copy-only explanation: strip NULs and code
/// fences, trim blank edges, and fall back to fixed vocabulary when nothing
/// remains. The result is capped at [`MAX_COMMIT_EXPLANATION_CHARS`].
pub(crate) fn sanitize_commit_explanation(raw: &str) -> String {
    // Strip NULs upfront so the result can never carry one through.
    let without_nul = raw.replace('\0', "");
    let lines: Vec<&str> = without_nul
        .lines()
        .map(str::trim_end)
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != "```" && !trimmed.starts_with("```")
        })
        .collect();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let explanation = if start < end {
        lines[start..end].join("\n").trim().to_string()
    } else {
        String::new()
    };
    if explanation.is_empty() {
        return "The commit changes could not be summarized from the diff.".to_string();
    }
    truncate_explain_chars(&explanation, MAX_COMMIT_EXPLANATION_CHARS)
}

/// Explain one historical commit through the existing AI execution path: the
/// commit diff (capped at [`MAX_COMMIT_EXPLAIN_BYTES`]) feeds a narrow
/// prompt to a single text-only [`AiRequest`] executed by the shared
/// [`RequestExecutionService`](crate::application::execution::RequestExecutionService)
/// — the same execution boundary `send_message`, the agent-run bridge, the
/// commit-message path, and the terminal explain path use. Credentials
/// resolve from the OS keyring only; no new key input exists on this path,
/// and nothing is persisted.
///
/// The commit diff may contain secrets: it is capped and truncated, never
/// logged, and never echoed by [`CommitExplainError`]. The returned
/// explanation is copy-only — no caller may feed it back into a write.
///
/// # Errors
///
/// Returns [`CommitExplainError::InvalidInput`] when the hash is malformed
/// or unknown, or the commit changed no files;
/// [`CommitExplainError::Request`] when AI execution fails.
pub(crate) fn explain_commit(
    db: &Database,
    workspace_root: &Path,
    raw_hash: &str,
    provider: &str,
    model: &str,
) -> Result<CommitExplanation, CommitExplainError> {
    use crate::application::execution::RequestExecutionService;
    let (_, repo) =
        open_workspace_repo(workspace_root).map_err(|_| CommitExplainError::InvalidInput)?;
    let commit =
        find_commit_by_hash(&repo, raw_hash).map_err(|_| CommitExplainError::InvalidInput)?;
    let detail = git_commit_diff(workspace_root, &commit.id().to_string())
        .map_err(|_| CommitExplainError::InvalidInput)?;
    if detail.files.is_empty() {
        return Err(CommitExplainError::InvalidInput);
    }
    let summary = commit
        .summary_bytes()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        "(no message)"
    } else {
        summary
    };
    let author_sig = commit.author();
    let author = String::from_utf8_lossy(author_sig.name_bytes());
    let summary_line = format!("{summary} by {author}");
    let (excerpt, truncated) = if detail.diff.len() > MAX_COMMIT_EXPLAIN_BYTES {
        let mut end = MAX_COMMIT_EXPLAIN_BYTES;
        while !detail.diff.is_char_boundary(end) {
            end -= 1;
        }
        (detail.diff[..end].to_string(), true)
    } else {
        (detail.diff.clone(), detail.truncated)
    };
    if excerpt.trim().is_empty() {
        return Err(CommitExplainError::InvalidInput);
    }
    let prompt = build_commit_explain_prompt(&summary_line, &excerpt, truncated);
    let request = AiRequest {
        provider: provider.to_string(),
        model: model.to_string(),
        messages: std::sync::Arc::new(vec![AiMessage {
            role: AiRole::User,
            content: prompt,
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
        }]),
        tools: std::sync::Arc::new(Vec::new()),
        model_config: None,
        request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
    };
    let response = RequestExecutionService::new(db).execute(&request)?;
    Ok(CommitExplanation {
        explanation: sanitize_commit_explanation(&response.content),
        truncated_input: truncated,
    })
}
