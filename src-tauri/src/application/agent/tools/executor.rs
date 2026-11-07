//! Workspace tool execution: dispatch, timeouts, and workspace guards.
//!
//! [`ToolRegistry`] request handling for shell and filesystem tools. All
//! filesystem access is confined to the workspace root; shell execution is
//! bounded by timeout, capture, and drain limits.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use super::command::CommandLimits;
use super::output::{truncate_output, unified_diff};
use super::path_safety::{
    canonical_root, ensure_parent_dirs, is_within_workspace, reject_final_symlink_escape,
    resolve_path, verify_existing_within,
};
use super::search::{
    walk_search, LiteRegex, MAX_SEARCH_PATTERN_BYTES, SEARCH_DEFAULT_MAX_MATCHES,
    SEARCH_HARD_CAP_MATCHES,
};
use super::ToolRegistry;
use crate::application::agent::control::CancellationToken;
use crate::application::execution::ToolCall;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Classified tool execution error. Display is always prefixed with `Error:`
/// so the LLM can observe failures without panicking the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolError {
    UnknownTool(String),
    InvalidArguments(String),
    Io(String),
    PathTraversal(String),
    Timeout(String),
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "Error: unknown tool '{name}'"),
            Self::InvalidArguments(msg) => write!(f, "Error: invalid arguments: {msg}"),
            Self::PathTraversal(p) => {
                write!(
                    f,
                    "Error: path traversal not allowed: '{p}' is outside workspace"
                )
            }
            Self::Io(msg) | Self::Timeout(msg) => write!(f, "Error: {msg}"),
            Self::Cancelled => write!(f, "Error: tool execution was cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

impl ToolRegistry {
    /// Dispatch a [`ToolCall`] to its implementation.
    ///
    /// `workspace_root` is the absolute path that bounds all filesystem
    /// access. Tool failures are returned as `Err(ToolError)` with an
    /// `Error:` prefix; they never panic.
    pub(crate) fn execute(call: &ToolCall, workspace_root: &Path) -> Result<String, ToolError> {
        // No control attached: never cancelled (Task 3.1 semantics exactly).
        let token = CancellationToken::new();
        Self::execute_with_cancellation(call, workspace_root, &token)
    }

    /// [`execute`](Self::execute) that honours cooperative cancellation
    /// (Task 3.2): returns `Err(ToolError::Cancelled)` immediately when the
    /// token has fired, and polls `token` inside long-running command
    /// execution so the child process is killed promptly.
    pub(crate) fn execute_with_cancellation(
        call: &ToolCall,
        workspace_root: &Path,
        token: &CancellationToken,
    ) -> Result<String, ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: Value = serde_json::from_str(&call.arguments).map_err(|e| {
            ToolError::InvalidArguments(format!("arguments are not valid JSON: {e}"))
        })?;
        match call.name.as_str() {
            "execute_command" => Self::execute_command_with_limits(
                &args,
                workspace_root,
                &CommandLimits::default(),
                token,
            ),
            "read_file" => Self::read_file(&args, workspace_root),
            "write_file" => Self::write_file(&args, workspace_root),
            "edit_file" => Self::edit_file(&args, workspace_root),
            "search_files" => Self::search_files(&args, workspace_root),
            "list_directory" => Self::list_directory(&args, workspace_root),
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }

    // -----------------------------------------------------------------------
    fn read_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let offset = args
            .get("offset_lines")
            .and_then(Value::as_u64)
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX));
        let limit = args
            .get("limit_lines")
            .and_then(Value::as_u64)
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX));
        if let Some(l) = limit {
            if l == 0 {
                return Err(ToolError::InvalidArguments(
                    "limit_lines must be >= 1".to_string(),
                ));
            }
        }

        let resolved = resolve_path(workspace_root, path)?;
        if !resolved.exists() {
            return Err(ToolError::Io(format!("file not found: '{path}'")));
        }
        // Workspace ignore contract (`application/project_dir`): `.nexora/`
        // itself and every `.nexoraignore` match stay unreadable through the
        // agent tools. The error is a fixed category with no content echo.
        if crate::application::project_dir::path_is_ignored(workspace_root, &resolved) {
            return Err(ToolError::Io(
                "path is excluded by workspace ignore rules".to_string(),
            ));
        }
        if resolved.is_dir() {
            return Err(ToolError::Io(format!(
                "path is a directory, not a file: '{path}'"
            )));
        }
        // TOCTOU guard, deliberately last: the canonical target is re-verified
        // immediately before the read it gates, so a swap between the ignore
        // verdict above (which re-canonicalizes internally) and the read
        // cannot redirect it.
        verify_existing_within(
            workspace_root,
            &canonical_root(workspace_root),
            &resolved,
            path,
        )?;

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| ToolError::Io(format!("failed to read file '{path}': {e}")))?;

        // Chunk by lines
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = offset.unwrap_or(0);
        if start >= total && total != 0 {
            // Return empty with hint; not an error that panics
            return Ok(String::new());
        }
        let start = std::cmp::min(start, total);
        let end = match limit {
            Some(l) => std::cmp::min(start.saturating_add(l), total),
            None => total,
        };
        let slice = &lines[start..end];
        Ok(truncate_output(slice.join("\n")))
    }

    fn write_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let content = args.get("content").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'content'".to_string())
        })?;

        let resolved = resolve_path(workspace_root, path)?;
        // Workspace ignore contract (same fixed error as reads): mutating
        // tools must not create or modify excluded paths. The for-write
        // verdict handles not-yet-existing targets lexically, so creates
        // under `.nexora/` or matching `.nexoraignore` deny before any read
        // or write happens. No `profiles/` allowlist: every write under
        // `.nexora/` denies, without exception.
        let ignore = crate::application::project_dir::load_ignore_rules(workspace_root);
        if crate::application::project_dir::is_excluded_for_write(
            workspace_root,
            &resolved,
            &ignore,
        ) {
            return Err(ToolError::Io(
                "path is excluded by workspace ignore rules".to_string(),
            ));
        }
        let canon_ws = canonical_root(workspace_root);

        // Read old content before writing (same path validation as read_file). For
        // new files the diff is against empty; for existing files the bytes are
        // read lossily so binary files still produce a diff without panicking.
        // A pre-existing symlink escaping the workspace is rejected first so the
        // read itself never follows an outside link.
        reject_final_symlink_escape(&canon_ws, &resolved, path)?;
        let old_content = if resolved.exists() {
            verify_existing_within(workspace_root, &canon_ws, &resolved, path)?;
            match std::fs::read(&resolved) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        };

        if let Some(parent) = resolved.parent() {
            if !parent.as_os_str().is_empty() {
                // Lexical guard (unchanged behavior for in-workspace paths).
                if !is_within_workspace(workspace_root, parent) {
                    return Err(ToolError::PathTraversal(path.to_string()));
                }
                // Create parents one level at a time so a symlink component
                // pointing outside is rejected *before* anything is created
                // through it; each created level is canonicalized and
                // re-verified (canonicalize-after-create).
                ensure_parent_dirs(workspace_root, &canon_ws, parent, path)?;
            }
        }

        // The final component itself may be a symlink pointing outside
        // (single-level escape): re-check after the parents exist, just before
        // the write that would follow the link.
        reject_final_symlink_escape(&canon_ws, &resolved, path)?;
        std::fs::write(&resolved, content)
            .map_err(|e| ToolError::Io(format!("failed to write file '{path}': {e}")))?;

        // Backstop for a link swapped in between the pre-write check and the
        // write (TOCTOU): the written file must canonicalize inside the
        // workspace, otherwise report traversal instead of success.
        if let Ok(canon_file) = resolved.canonicalize() {
            if !is_within_workspace(&canon_ws, &canon_file) {
                return Err(ToolError::PathTraversal(path.to_string()));
            }
        } else if let Some(parent) = resolved.parent() {
            if let Ok(canon_parent) = parent.canonicalize() {
                if !is_within_workspace(&canon_ws, &canon_parent) {
                    return Err(ToolError::PathTraversal(path.to_string()));
                }
            }
        }

        let diff = unified_diff(path, &old_content, content);
        Ok(truncate_output(diff))
    }

    /// Exact-once in-place edit: `old_text`/`new_text` replace mode or
    /// `insert_after`/`new_text` insert mode (exactly one of the two anchors).
    ///
    /// The anchor must occur exactly once: zero matches fail with
    /// `no exact match found`, two or more fail with
    /// `pattern matches N locations, refusing to guess` — the tool never
    /// guesses. New text is spliced verbatim (callers include newlines).
    /// Files must be valid UTF-8; the workspace guard is shared with
    /// `read_file`/`write_file`.
    fn edit_file(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'path'".to_string())
        })?;
        if path.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'path' must not be empty".to_string(),
            ));
        }
        let new_text = args
            .get("new_text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ToolError::InvalidArguments("missing required field 'new_text'".to_string())
            })?;
        let old_text = args.get("old_text").and_then(Value::as_str);
        let insert_after = args.get("insert_after").and_then(Value::as_str);
        let anchor = match (old_text, insert_after) {
            (Some(_), Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "supply exactly one of 'old_text' or 'insert_after'".to_string(),
                ));
            }
            (None, None) => {
                return Err(ToolError::InvalidArguments(
                    "missing required field 'old_text' or 'insert_after'".to_string(),
                ));
            }
            (Some(old), None) => EditAnchor::Replace(old),
            (None, Some(after)) => EditAnchor::InsertAfter(after),
        };
        if anchor.text().is_empty() {
            return Err(ToolError::InvalidArguments(
                "anchor text must not be empty".to_string(),
            ));
        }

        let resolved = resolve_path(workspace_root, path)?;
        if !resolved.exists() {
            return Err(ToolError::Io(format!("file not found: '{path}'")));
        }
        // Workspace ignore contract (same fixed error as reads): excluded
        // paths deny before any content is touched.
        let ignore = crate::application::project_dir::load_ignore_rules(workspace_root);
        if crate::application::project_dir::is_excluded_for_write(
            workspace_root,
            &resolved,
            &ignore,
        ) {
            return Err(ToolError::Io(
                "path is excluded by workspace ignore rules".to_string(),
            ));
        }
        // Re-verify through symlink_metadata (TOCTOU guard): the link may have
        // been swapped between `resolve_path` and use.
        let canon_ws = canonical_root(workspace_root);
        verify_existing_within(workspace_root, &canon_ws, &resolved, path)?;
        if resolved.is_dir() {
            return Err(ToolError::Io(format!(
                "path is a directory, not a file: '{path}'"
            )));
        }
        let bytes = std::fs::read(&resolved)
            .map_err(|e| ToolError::Io(format!("failed to read file '{path}': {e}")))?;
        let content = String::from_utf8(bytes).map_err(|_| {
            ToolError::Io(format!("file is not valid UTF-8, cannot edit: '{path}'"))
        })?;

        let occurrences = content.match_indices(anchor.text()).count();
        if occurrences == 0 {
            return Err(ToolError::Io("no exact match found".to_string()));
        }
        if occurrences > 1 {
            return Err(ToolError::Io(format!(
                "pattern matches {occurrences} locations, refusing to guess"
            )));
        }
        let updated = match anchor {
            EditAnchor::Replace(old) => content.replacen(old, new_text, 1),
            EditAnchor::InsertAfter(after) => {
                content.replacen(after, &format!("{after}{new_text}"), 1)
            }
        };

        // Re-verify just before the write that follows the (possibly linked)
        // path, so a swap between read and write cannot redirect the edit.
        verify_existing_within(workspace_root, &canon_ws, &resolved, path)?;
        std::fs::write(&resolved, &updated)
            .map_err(|e| ToolError::Io(format!("failed to write file '{path}': {e}")))?;

        // Backstop for a link swapped in between the pre-write check and the
        // write (TOCTOU): the edited file must canonicalize inside the
        // workspace, otherwise report traversal instead of success.
        if let Ok(canon_file) = resolved.canonicalize() {
            if !is_within_workspace(&canon_ws, &canon_file) {
                return Err(ToolError::PathTraversal(path.to_string()));
            }
        } else if let Some(parent) = resolved.parent() {
            if let Ok(canon_parent) = parent.canonicalize() {
                if !is_within_workspace(&canon_ws, &canon_parent) {
                    return Err(ToolError::PathTraversal(path.to_string()));
                }
            }
        }

        Ok(truncate_output(unified_diff(path, &content, &updated)))
    }

    /// Regex-lite grep over the workspace: one `path:line:text` hit per
    /// matching line, honouring `max_matches` (default 50, hard cap 200).
    ///
    /// Binary files (NUL-byte heuristic), files over 5 MiB, unreadable files,
    /// and symbolic links are skipped; the walk never leaves the workspace.
    /// Long lines are middle-truncated with an edge-kept notice.
    fn search_files(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let pattern = args.get("pattern").and_then(Value::as_str).ok_or_else(|| {
            ToolError::InvalidArguments("missing required field 'pattern'".to_string())
        })?;
        if pattern.is_empty() {
            return Err(ToolError::InvalidArguments(
                "field 'pattern' must not be empty".to_string(),
            ));
        }
        if pattern.len() > MAX_SEARCH_PATTERN_BYTES {
            return Err(ToolError::InvalidArguments(format!(
                "field 'pattern' exceeds {MAX_SEARCH_PATTERN_BYTES} bytes"
            )));
        }
        let regex = LiteRegex::compile(pattern);
        let scope_arg = args
            .get("directory")
            .and_then(Value::as_str)
            .or_else(|| args.get("path").and_then(Value::as_str));
        let scope = match scope_arg {
            Some(dir) if !dir.trim().is_empty() => resolve_path(workspace_root, dir)?,
            _ => workspace_root.to_path_buf(),
        };
        if !scope.exists() {
            return Err(ToolError::Io(format!(
                "directory not found: '{}'",
                scope_arg.unwrap_or("")
            )));
        }
        if !scope.is_dir() {
            return Err(ToolError::Io(format!(
                "search scope is not a directory: '{}'",
                scope_arg.unwrap_or("")
            )));
        }
        if !is_within_workspace(workspace_root, &scope) {
            return Err(ToolError::PathTraversal(
                scope_arg.unwrap_or("").to_string(),
            ));
        }
        // Canonical re-verification (TOCTOU guard before the walk starts).
        verify_existing_within(
            workspace_root,
            &canonical_root(workspace_root),
            &scope,
            scope_arg.unwrap_or(""),
        )?;
        // Workspace ignore contract: an ignored scope is denied, and ignored
        // paths are skipped during the walk so matches never leak.
        let ignore = crate::application::project_dir::load_ignore_rules(workspace_root);
        if crate::application::project_dir::is_excluded(workspace_root, &scope, &ignore) {
            return Err(ToolError::Io(
                "path is excluded by workspace ignore rules".to_string(),
            ));
        }
        let max_matches = match args.get("max_matches") {
            None => SEARCH_DEFAULT_MAX_MATCHES,
            Some(value) => {
                let raw = value.as_u64().ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "field 'max_matches' must be a non-negative integer".to_string(),
                    )
                })?;
                usize::try_from(raw)
                    .unwrap_or(usize::MAX)
                    .clamp(1, SEARCH_HARD_CAP_MATCHES)
            }
        };

        let mut hits: Vec<String> = Vec::new();
        let mut capped = false;
        walk_search(
            &scope,
            workspace_root,
            &ignore,
            &regex,
            max_matches,
            &mut hits,
            &mut capped,
        )?;
        if hits.is_empty() {
            return Ok("(no matches)".to_string());
        }
        let mut out = hits.join("\n");
        if capped {
            let _ = writeln!(
                out,
                "\n[match cap reached: showing first {max_matches} matches (cap {SEARCH_HARD_CAP_MATCHES})]"
            );
        }
        Ok(truncate_output(out))
    }

    fn list_directory(args: &Value, workspace_root: &Path) -> Result<String, ToolError> {
        let path_opt = args.get("path").and_then(Value::as_str);
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let target = if let Some(p) = path_opt {
            if p.trim().is_empty() {
                workspace_root.to_path_buf()
            } else {
                resolve_path(workspace_root, p)?
            }
        } else {
            workspace_root.to_path_buf()
        };

        if !target.exists() {
            return Err(ToolError::Io(format!(
                "directory not found: '{}'",
                path_opt.unwrap_or("")
            )));
        }
        if !target.is_dir() {
            return Err(ToolError::Io(format!(
                "path is not a directory: '{}'",
                path_opt.unwrap_or("")
            )));
        }
        // Ensure target is within workspace
        if !is_within_workspace(workspace_root, &target) {
            return Err(ToolError::PathTraversal(path_opt.unwrap_or("").to_string()));
        }
        // Canonical re-verification (TOCTOU guard before the directory is read).
        verify_existing_within(
            workspace_root,
            &canonical_root(workspace_root),
            &target,
            path_opt.unwrap_or(""),
        )?;
        // Workspace ignore contract: an ignored scope is denied with the
        // same fixed, content-free error as `read_file`; ignored entries are
        // filtered silently below so listings never leak their names.
        let ignore = crate::application::project_dir::load_ignore_rules(workspace_root);
        if crate::application::project_dir::is_excluded(workspace_root, &target, &ignore) {
            return Err(ToolError::Io(
                "path is excluded by workspace ignore rules".to_string(),
            ));
        }

        let mut entries = Vec::new();
        if recursive {
            walk_recursive(&target, workspace_root, &ignore, &mut entries)?;
        } else {
            let read = std::fs::read_dir(&target)
                .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
            for entry in read {
                let entry =
                    entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
                if crate::application::project_dir::is_excluded(
                    workspace_root,
                    &entry.path(),
                    &ignore,
                ) {
                    continue;
                }
                let ft = entry
                    .file_type()
                    .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?;
                let name = entry.file_name().to_string_lossy().to_string();
                let kind = if ft.is_dir() { "dir" } else { "file" };
                // Show relative to workspace for LLM clarity
                let rel = entry
                    .path()
                    .strip_prefix(workspace_root)
                    .unwrap_or(entry.path().as_path())
                    .to_string_lossy()
                    .to_string();
                entries.push(format!("{kind}: {rel} (name: {name})"));
            }
        }
        entries.sort();
        if entries.is_empty() {
            Ok("(empty directory)".to_string())
        } else {
            Ok(truncate_output(entries.join("\n")))
        }
    }
}
/// Edit mode selected by which anchor argument was supplied.
#[derive(Debug, Clone, Copy)]
enum EditAnchor<'a> {
    Replace(&'a str),
    InsertAfter(&'a str),
}

impl<'a> EditAnchor<'a> {
    fn text(self) -> &'a str {
        match self {
            Self::Replace(old) | Self::InsertAfter(old) => old,
        }
    }
}
fn walk_recursive(
    dir: &Path,
    workspace_root: &Path,
    ignore: &crate::application::project_dir::IgnoreRules,
    out: &mut Vec<String>,
) -> Result<(), ToolError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ToolError::Io(format!("failed to read directory: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| ToolError::Io(format!("failed to read entry: {e}")))?;
        let path = entry.path();
        // Ensure each visited path stays within workspace (defense against symlink escapes)
        if !is_within_workspace(workspace_root, &path) {
            continue;
        }
        // Workspace ignore contract: ignored paths (including `.nexora/`)
        // are neither listed nor descended into.
        if crate::application::project_dir::is_excluded(workspace_root, &path, ignore) {
            continue;
        }
        let ft = entry
            .file_type()
            .map_err(|e| ToolError::Io(format!("failed to get file type: {e}")))?;
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .to_string();
        let kind = if ft.is_dir() { "dir" } else { "file" };
        let name = entry.file_name().to_string_lossy().to_string();
        out.push(format!("{kind}: {rel} (name: {name})"));
        if ft.is_dir() {
            // Never descend through a symlink, even one swapped in after the
            // `read_dir` above; symlinks stay listed as files, never followed.
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }
            if let Ok(canon) = path.canonicalize() {
                if !is_within_workspace(workspace_root, &canon) {
                    continue;
                }
            }
            walk_recursive(&path, workspace_root, ignore, out)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::command::{COMMAND_TIMEOUT, MAX_CAPTURE_BYTES};
    use crate::application::agent::tools::test_support::*;
    use std::fs;
    use std::time::{Duration, Instant};
    #[test]
    fn execute_command_echo_captures_stdout() {
        let ws = temp_workspace();
        let c = call(
            "execute_command",
            serde_json::json!({"command": "echo hello"}),
        );
        let out = ToolRegistry::execute(&c, &ws).expect("execute_command succeeds");
        assert!(out.contains("hello"), "output was: {out}");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_creates_and_read_file_reads_back() {
        let ws = temp_workspace();
        let write = call(
            "write_file",
            serde_json::json!({"path": "hello.txt", "content": "hello world"}),
        );
        let res = ToolRegistry::execute(&write, &ws).expect("write succeeds");
        assert!(
            res.contains("--- a/hello.txt"),
            "diff header missing: {res}"
        );
        assert!(
            res.contains("+++ b/hello.txt"),
            "diff header missing: {res}"
        );
        assert!(res.contains("+hello world"), "diff addition missing: {res}");
        assert!(res.contains("@@"), "diff hunk header missing: {res}");

        let read = call("read_file", serde_json::json!({"path": "hello.txt"}));
        let content = ToolRegistry::execute(&read, &ws).expect("read succeeds");
        assert_eq!(content, "hello world");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_file_with_offset_and_limit() {
        let ws = temp_workspace();
        let content = "line1\nline2\nline3\nline4\nline5";
        let write = call(
            "write_file",
            serde_json::json!({"path": "multi.txt", "content": content}),
        );
        ToolRegistry::execute(&write, &ws).expect("write");

        let read = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 1, "limit_lines": 2}),
        );
        let out = ToolRegistry::execute(&read, &ws).expect("read with offset");
        assert_eq!(out, "line2\nline3");

        let read2 = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 0, "limit_lines": 1}),
        );
        let out2 = ToolRegistry::execute(&read2, &ws).expect("read first line");
        assert_eq!(out2, "line1");

        let read3 = call(
            "read_file",
            serde_json::json!({"path": "multi.txt", "offset_lines": 10}),
        );
        let out3 = ToolRegistry::execute(&read3, &ws).expect("offset beyond end");
        assert_eq!(out3, "");

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn path_traversal_is_rejected() {
        let ws = temp_workspace();
        // Try various traversal payloads
        for payload in &[
            "../../etc/passwd",
            "../outside.txt",
            "a/../../b/../../etc/passwd",
        ] {
            let read = call("read_file", serde_json::json!({"path": payload}));
            let res = ToolRegistry::execute(&read, &ws);
            assert!(res.is_err(), "traversal '{payload}' should be rejected");
            let err = res.unwrap_err().to_string();
            assert!(
                err.starts_with("Error:"),
                "error should be prefixed with Error: got {err}"
            );
            assert!(
                err.to_lowercase().contains("outside") || err.to_lowercase().contains("traversal"),
                "err: {err}"
            );
        }

        let write = call(
            "write_file",
            serde_json::json!({"path": "../../evil.txt", "content": "bad"}),
        );
        let res = ToolRegistry::execute(&write, &ws);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().starts_with("Error:"));

        // Absolute path outside workspace should also be rejected
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let read_abs = call("read_file", serde_json::json!({"path": abs_outside}));
        let res_abs = ToolRegistry::execute(&read_abs, &ws);
        assert!(res_abs.is_err(), "absolute outside path should be rejected");

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_failure_does_not_crash() {
        let ws = temp_workspace();
        // Command that exits with non-zero should not panic, should return Ok with status
        let fail = if cfg!(windows) {
            call("execute_command", serde_json::json!({"command": "exit 1"}))
        } else {
            call("execute_command", serde_json::json!({"command": "false"}))
        };
        let res = ToolRegistry::execute(&fail, &ws);
        // Should be Ok (process ran) with exit status reported, not Err/panic
        assert!(res.is_ok(), "non-zero exit should not be Err, got {res:?}");
        let out = res.unwrap();
        // Should contain exit status or be non-empty
        assert!(!out.is_empty());

        // Invalid command should be captured as Ok with stderr or Err but not panic
        let invalid = call(
            "execute_command",
            serde_json::json!({"command": "nonexistent_command_xyz_12345"}),
        );
        let res2 = ToolRegistry::execute(&invalid, &ws);
        // Must not panic; may be Ok with error output or Err
        assert!(res2.is_ok() || res2.is_err());
        if let Err(e) = res2 {
            assert!(e.to_string().starts_with("Error:") || e.to_string().contains("Error"));
        }

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn command_timeout_is_handled() {
        // We test timeout handling by using a command that sleeps.
        // To avoid waiting 30s, we test the truncation and timeout infrastructure
        // indirectly: ensure that a quick command does not timeout, and that the
        // timeout mechanism exists and does not panic on normal commands.
        // This test verifies the tool does not hang on a fast command and that
        // the 30s constant is present.
        assert_eq!(COMMAND_TIMEOUT, Duration::from_secs(30));
        let ws = temp_workspace();
        let c = call(
            "execute_command",
            serde_json::json!({"command": "echo quick"}),
        );
        let out = ToolRegistry::execute(&c, &ws).expect("quick command should not timeout");
        assert!(out.contains("quick"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn list_directory_works() {
        let ws = temp_workspace();
        // Create structure
        fs::create_dir_all(ws.join("a/b")).unwrap();
        fs::write(ws.join("a/file1.txt"), "x").unwrap();
        fs::write(ws.join("a/b/file2.txt"), "y").unwrap();
        fs::write(ws.join("root.txt"), "z").unwrap();

        let list = call("list_directory", serde_json::json!({}));
        let out = ToolRegistry::execute(&list, &ws).expect("list root");
        assert!(out.contains("root.txt") || out.contains('a'));

        let list_a = call("list_directory", serde_json::json!({"path": "a"}));
        let out_a = ToolRegistry::execute(&list_a, &ws).expect("list a");
        assert!(out_a.contains("file1.txt"));

        let rec = call(
            "list_directory",
            serde_json::json!({"path": "a", "recursive": true}),
        );
        let out_r = ToolRegistry::execute(&rec, &ws).expect("recursive");
        assert!(out_r.contains("file2.txt"));

        // Traversal via list should be rejected
        let bad = call("list_directory", serde_json::json!({"path": "../"}));
        assert!(ToolRegistry::execute(&bad, &ws).is_err());

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn unknown_tool_is_error() {
        let ws = temp_workspace();
        let c = call("unknown_tool", serde_json::json!({}));
        let res = ToolRegistry::execute(&c, &ws);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("unknown tool"));
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Sandbox hardening (Task 2.1.1): bounded timeout / capture / drain
    // -----------------------------------------------------------------------

    /// Injected limits for the real execution path with short waits.
    fn hardening_limits(timeout_secs: u64, drain_grace_ms: u64) -> CommandLimits {
        CommandLimits {
            timeout: Duration::from_secs(timeout_secs),
            drain_grace: Duration::from_millis(drain_grace_ms),
            capture_cap: MAX_CAPTURE_BYTES,
        }
    }

    fn run_cmd(command: &str, ws: &std::path::Path) -> Result<String, ToolError> {
        let args = serde_json::json!({ "command": command });
        let token = CancellationToken::new();
        ToolRegistry::execute_command_with_limits(&args, ws, &hardening_limits(5, 500), &token)
    }

    #[test]
    fn command_exceeding_injected_timeout_is_killed_within_bound() {
        let ws = temp_workspace();
        // ~10 s of sleep; the injected 1 s timeout must kill it and return.
        let sleep_cmd = if cfg!(windows) {
            "ping -n 11 127.0.0.1 >nul"
        } else {
            "sleep 10"
        };
        let args = serde_json::json!({ "command": sleep_cmd });
        let token = CancellationToken::new();
        let start = Instant::now();
        let res = ToolRegistry::execute_command_with_limits(
            &args,
            &ws,
            &hardening_limits(1, 500),
            &token,
        );
        assert!(
            matches!(res, Err(ToolError::Timeout(_))),
            "expected Timeout, got {res:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "timeout path blocked too long: {:?}",
            start.elapsed()
        );
        let _ = fs::remove_dir_all(&ws);
    }

    // -----------------------------------------------------------------------
    // Cooperative cancellation (Task 3.2)
    // -----------------------------------------------------------------------

    #[test]
    fn pre_cancelled_token_rejects_execution_without_running_anything() {
        let ws = temp_workspace();
        let token = CancellationToken::new();
        token.cancel();
        let call = call(
            "execute_command",
            serde_json::json!({ "command": "echo should-not-run" }),
        );
        let res = ToolRegistry::execute_with_cancellation(&call, &ws, &token);
        assert_eq!(res.unwrap_err(), ToolError::Cancelled);
        assert_eq!(
            res_err_string(&call, &ws, &token),
            "Error: tool execution was cancelled"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    /// Render the Display of an already-cancelled execution for the assertion
    /// above without a second execution path divergence.
    fn res_err_string(call: &ToolCall, ws: &std::path::Path, token: &CancellationToken) -> String {
        match ToolRegistry::execute_with_cancellation(call, ws, token) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("must stay cancelled"),
        }
    }

    #[test]
    fn cancel_during_long_running_command_kills_child_within_about_a_second() {
        let ws = temp_workspace();
        // ~15 s of work; cancellation must kill it well inside a second of
        // being requested (plus one poll interval), building directly on the
        // hardened Task 2.1.1 bounded wait loop.
        let long_cmd = if cfg!(windows) {
            "ping -n 16 127.0.0.1 >nul"
        } else {
            "sleep 15"
        };
        let call = call(
            "execute_command",
            serde_json::json!({ "command": long_cmd }),
        );
        let token = CancellationToken::new();

        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| ToolRegistry::execute_with_cancellation(&call, &ws, &token));
            // Let the child reach its steady state before cancelling.
            std::thread::sleep(Duration::from_millis(300));
            let start = Instant::now();
            token.cancel();
            let res = worker.join().expect("worker must not panic");
            assert_eq!(res.unwrap_err(), ToolError::Cancelled);
            assert!(
                start.elapsed() <= Duration::from_secs(1),
                "cancellation took too long: {:?}",
                start.elapsed()
            );
        });

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn grandchild_holding_pipe_does_not_hang_and_reports_partial_capture() {
        let ws = temp_workspace();
        // The backgrounded `sleep` inherits stdout and keeps the pipe open
        // long after the shell exits; only the bounded drain makes this
        // return. Exercises the REAL grace-expiry leak path.
        let start = Instant::now();
        let out = run_cmd("echo begin; sleep 5 &", &ws).expect("must complete despite open pipe");
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "drain grace not honored"
        );
        assert!(out.contains("begin"), "early stdout must be captured");
        assert!(
            out.contains("output stream still open after grace period"),
            "partial-capture warning marker missing"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn stdin_reading_command_returns_promptly_because_stdin_is_null() {
        let ws = temp_workspace();
        // A bare `cat` blocks forever on stdin; with Stdio::null() it sees EOF
        // immediately and exits.
        let start = Instant::now();
        let out = run_cmd("cat", &ws).expect("cat must return via null stdin");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "stdin was not detached"
        );
        assert_eq!(out.trim(), "(no output)");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn runaway_output_is_memory_bounded_by_capture_cap() {
        let ws = temp_workspace();
        // Produce ~2 MB of output through a file; capture must stay far below
        // the produced volume while still truncating to the context budget.
        let content = "y".repeat(2 * 1024 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({ "path": "big.bin", "content": content }),
        );
        ToolRegistry::execute(&write, &ws).expect("seed big file");

        let read_cmd = if cfg!(windows) {
            "type big.bin"
        } else {
            "cat big.bin"
        };
        let out = run_cmd(read_cmd, &ws).expect("read big file back");
        assert!(
            out.len() < 100 * 1024,
            "capture leaked past bounds: {} bytes",
            out.len()
        );
        assert!(out.contains("output truncated"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_path_validation_still_rejects_escapes_after_diff_change() {
        let ws = temp_workspace();
        for payload in &[
            "../../etc/passwd",
            "../outside.txt",
            "a/../../b/../../etc/passwd",
        ] {
            let call = call(
                "write_file",
                serde_json::json!({"path": payload, "content": "bad"}),
            );
            let res = ToolRegistry::execute(&call, &ws);
            assert!(res.is_err(), "traversal '{payload}' should be rejected");
            assert!(res.unwrap_err().to_string().starts_with("Error:"));
        }
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let call = call(
            "write_file",
            serde_json::json!({"path": abs_outside, "content": "bad"}),
        );
        assert!(
            ToolRegistry::execute(&call, &ws).is_err(),
            "absolute outside must be rejected"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_cancelled_leaves_file_untouched() {
        let ws = temp_workspace();
        let existing = "original content";
        fs::write(ws.join("keep.txt"), existing).expect("seed");
        let token = CancellationToken::new();
        token.cancel();
        let c = call(
            "write_file",
            serde_json::json!({"path": "keep.txt", "content": "new content"}),
        );
        let res = ToolRegistry::execute_with_cancellation(&c, &ws, &token);
        assert_eq!(res.unwrap_err(), ToolError::Cancelled);
        // File must remain untouched on cancelled path (existing semantics preserved)
        assert_eq!(fs::read_to_string(ws.join("keep.txt")).unwrap(), existing);
        // Also new file path cancelled should not be created
        let new_c = call(
            "write_file",
            serde_json::json!({"path": "new_keep.txt", "content": "hello"}),
        );
        let res2 = ToolRegistry::execute_with_cancellation(&new_c, &ws, &token);
        assert_eq!(res2.unwrap_err(), ToolError::Cancelled);
        assert!(
            !ws.join("new_keep.txt").exists(),
            "cancelled write must not create file"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_replaces_exact_once() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "hello brave world").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "brave", "new_text": "cold"}),
        );
        let out = ToolRegistry::execute(&edit, &ws).expect("exact-once replace");
        assert!(
            out.contains("-hello brave world"),
            "diff must show removal: {out}"
        );
        assert!(
            out.contains("+hello cold world"),
            "diff must show addition: {out}"
        );
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "hello cold world"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_no_match_errors() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "hello world").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "absent", "new_text": "x"}),
        );
        let res = ToolRegistry::execute(&edit, &ws);
        assert!(res.is_err(), "zero matches must fail");
        assert_eq!(res.unwrap_err().to_string(), "Error: no exact match found");
        // File untouched.
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "hello world"
        );
        // Insert mode with a missing anchor fails the same way.
        let insert = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "insert_after": "absent", "new_text": "x"}),
        );
        let res_insert = ToolRegistry::execute(&insert, &ws);
        assert!(res_insert.is_err());
        assert_eq!(
            res_insert.unwrap_err().to_string(),
            "Error: no exact match found"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_multi_match_refuses() {
        let ws = temp_workspace();
        fs::write(ws.join("note.txt"), "aaa bbb aaa").expect("seed");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "note.txt", "old_text": "aaa", "new_text": "zzz"}),
        );
        let res = ToolRegistry::execute(&edit, &ws);
        assert!(res.is_err(), "ambiguous matches must fail");
        assert_eq!(
            res.unwrap_err().to_string(),
            "Error: pattern matches 2 locations, refusing to guess"
        );
        // No fuzzy repair: the file is byte-identical afterwards.
        assert_eq!(
            fs::read_to_string(ws.join("note.txt")).unwrap(),
            "aaa bbb aaa"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_insert_after_anchor() {
        let ws = temp_workspace();
        fs::write(ws.join("list.txt"), "line1\nline2\nline3").expect("seed");
        let insert = call(
            "edit_file",
            serde_json::json!({"path": "list.txt", "insert_after": "line2", "new_text": "\ninserted"}),
        );
        let out = ToolRegistry::execute(&insert, &ws).expect("insert");
        assert!(out.contains("+inserted"), "diff must show insertion: {out}");
        assert_eq!(
            fs::read_to_string(ws.join("list.txt")).unwrap(),
            "line1\nline2\ninserted\nline3"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_rejects_outside_workspace() {
        let ws = temp_workspace();
        fs::write(ws.join("inner.txt"), "unique-inner-content").expect("seed");
        // Traversal payload.
        let traversal = call(
            "edit_file",
            serde_json::json!({"path": "../outside.txt", "old_text": "a", "new_text": "b"}),
        );
        let res = ToolRegistry::execute(&traversal, &ws);
        assert!(res.is_err(), "traversal must be rejected");
        assert!(res.unwrap_err().to_string().starts_with("Error:"));
        // Absolute path outside the workspace.
        let abs_outside = if cfg!(windows) {
            "C:\\Windows\\System32\\drivers\\etc\\hosts"
        } else {
            "/etc/passwd"
        };
        let absolute = call(
            "edit_file",
            serde_json::json!({"path": abs_outside, "old_text": "a", "new_text": "b"}),
        );
        assert!(
            ToolRegistry::execute(&absolute, &ws).is_err(),
            "absolute outside path must be rejected"
        );
        // Symlink escape: a link inside the workspace pointing outside must
        // not become an editing backdoor.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside_dir = temp_workspace();
            fs::write(outside_dir.join("secret.txt"), "outer-secret").expect("outside seed");
            let link = ws.join("escape.txt");
            symlink(outside_dir.join("secret.txt"), &link).expect("symlink");
            let via_link = call(
                "edit_file",
                serde_json::json!({"path": "escape.txt", "old_text": "outer-secret", "new_text": "pwned"}),
            );
            let res_link = ToolRegistry::execute(&via_link, &ws);
            assert!(res_link.is_err(), "symlink escape must be rejected");
            assert_eq!(
                fs::read_to_string(outside_dir.join("secret.txt")).unwrap(),
                "outer-secret",
                "outside file must be untouched"
            );
            let _ = fs::remove_dir_all(&outside_dir);
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn search_files_finds_pattern_with_caps() {
        let ws = temp_workspace();
        let mut body = String::new();
        for i in 0..10 {
            let _ = writeln!(body, "needle in haystack {i}");
        }
        body.push_str("no match here\n");
        fs::write(ws.join("hay.txt"), &body).expect("seed");

        // Literal match finds every matching line.
        let find = call("search_files", serde_json::json!({"pattern": "needle"}));
        let out = ToolRegistry::execute(&find, &ws).expect("search");
        assert_eq!(out.lines().filter(|l| l.contains("needle")).count(), 10);
        assert!(
            out.contains("hay.txt:1:"),
            "hit shape is path:line:text: {out}"
        );

        // Regex subset: `.+`, `\d`, anchors.
        let digits = call(
            "search_files",
            serde_json::json!({"pattern": "^needle.+\\d$"}),
        );
        let out_digits = ToolRegistry::execute(&digits, &ws).expect("anchored class search");
        assert_eq!(out_digits.lines().count(), 10);

        // max_matches is honoured and the cap notice is honest.
        let capped = call(
            "search_files",
            serde_json::json!({"pattern": "needle", "max_matches": 4}),
        );
        let out_capped = ToolRegistry::execute(&capped, &ws).expect("capped search");
        assert_eq!(
            out_capped.lines().filter(|l| l.contains("needle")).count(),
            4
        );
        assert!(out_capped.contains("[match cap reached: showing first 4 matches"));

        // Hard cap: a request above 200 is clamped, never honoured literally.
        let mut big = String::new();
        for _ in 0..300 {
            big.push_str("capped-needle\n");
        }
        fs::write(ws.join("big.txt"), &big).expect("seed big");
        let hard = call(
            "search_files",
            serde_json::json!({"pattern": "capped-needle", "max_matches": 5000}),
        );
        let out_hard = ToolRegistry::execute(&hard, &ws).expect("hard-cap search");
        assert_eq!(
            out_hard
                .lines()
                .filter(|l| l.contains("capped-needle"))
                .count(),
            200
        );
        assert!(out_hard.contains("(cap 200)"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn search_files_skips_binary_and_stays_in_workspace() {
        let ws = temp_workspace();
        fs::write(ws.join("plain.txt"), "visible-needle here").expect("seed text");
        // Binary file containing the same pattern plus NUL bytes is skipped.
        let mut binary = b"visible-needle here".to_vec();
        binary.extend_from_slice(&[0, 1, 2, 3]);
        binary.extend_from_slice(b"visible-needle again");
        fs::write(ws.join("blob.bin"), &binary).expect("seed binary");

        let find = call(
            "search_files",
            serde_json::json!({"pattern": "visible-needle"}),
        );
        let out = ToolRegistry::execute(&find, &ws).expect("search");
        assert!(out.contains("plain.txt"), "text hit missing: {out}");
        assert!(
            !out.contains("blob.bin"),
            "binary hit must be skipped: {out}"
        );

        // Traversal scope is rejected.
        let bad_scope = call(
            "search_files",
            serde_json::json!({"pattern": "visible-needle", "directory": "../"}),
        );
        assert!(
            ToolRegistry::execute(&bad_scope, &ws).is_err(),
            "scope outside workspace must be rejected"
        );

        // Symlink escape: a link inside the workspace pointing at an outside
        // file must not leak the outside file's contents.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside_dir = temp_workspace();
            fs::write(outside_dir.join("secret.txt"), "escaped-needle-xyz").expect("outside seed");
            symlink(outside_dir.join("secret.txt"), ws.join("leak.txt")).expect("symlink");
            let leak = call(
                "search_files",
                serde_json::json!({"pattern": "escaped-needle-xyz"}),
            );
            let out_leak = ToolRegistry::execute(&leak, &ws).expect("search runs");
            assert!(
                !out_leak.contains("escaped-needle-xyz"),
                "symlink target outside workspace must stay excluded: {out_leak}"
            );
            let _ = fs::remove_dir_all(&outside_dir);
        }
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn write_file_blocks_nested_symlink_escape_at_depth_two() {
        // H-1 regression: `ws/link` is a symlink to the outside and `newsub`
        // does not exist yet. The old code skipped the canonical check because
        // the immediate parent (`ws/link/newsub`) was missing, then
        // `create_dir_all` followed the link and the write landed outside.
        use std::os::unix::fs::symlink;
        let ws = temp_workspace();
        let outside_dir = temp_workspace();
        symlink(&outside_dir, ws.join("link")).expect("symlink ws/link -> outside");

        let write = call(
            "write_file",
            serde_json::json!({"path": "link/newsub/file.txt", "content": "pwned"}),
        );
        let res = ToolRegistry::execute(&write, &ws);
        assert!(
            matches!(res, Err(ToolError::PathTraversal(_))),
            "depth-2 symlink escape must be PathTraversal, got {res:?}"
        );

        // Nothing may have been created through the link: the outside
        // directory must be untouched.
        assert!(
            !outside_dir.join("newsub").exists(),
            "outside dir must not gain newsub via the link"
        );
        assert!(
            !outside_dir.join("newsub/file.txt").exists(),
            "outside file must not be created"
        );
        let _ = fs::remove_dir_all(&ws);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn write_file_allows_legit_nested_write() {
        // The hardening must not change behavior for in-workspace paths:
        // a genuinely nested write still creates its parents and reads back.
        let ws = temp_workspace();
        let write = call(
            "write_file",
            serde_json::json!({"path": "a/b/c.txt", "content": "nested ok"}),
        );
        ToolRegistry::execute(&write, &ws).expect("nested in-workspace write succeeds");
        assert_eq!(
            fs::read_to_string(ws.join("a/b/c.txt")).expect("nested file exists"),
            "nested ok"
        );
        let read = call("read_file", serde_json::json!({"path": "a/b/c.txt"}));
        assert_eq!(
            ToolRegistry::execute(&read, &ws).expect("read back"),
            "nested ok"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_and_edit_deny_excluded_paths() {
        const FIXED: &str = "Error: path is excluded by workspace ignore rules";
        let ws = temp_workspace();
        fs::create_dir_all(ws.join(".nexora")).expect("seed .nexora");
        fs::write(ws.join(".nexora/nexora.json"), "{\"version\":1}").expect("seed manifest");
        fs::write(ws.join(".nexora/.nexoraignore"), "*.log\nsecret.txt\n").expect("seed rules");
        fs::write(ws.join("secret.txt"), "original-secret").expect("seed secret");
        fs::write(ws.join("visible.txt"), "original-visible").expect("seed visible");

        // Excluded targets deny with the fixed content-free error: the
        // manifest, an ignored file, and a not-yet-existing ignored file.
        for target in [".nexora/nexora.json", "secret.txt", "fresh.log"] {
            let write = call(
                "write_file",
                serde_json::json!({"path": target, "content": "pwned"}),
            );
            let err = ToolRegistry::execute(&write, &ws).expect_err("write denied");
            assert_eq!(err.to_string(), FIXED, "write target {target:?}");
        }
        for target in [".nexora/nexora.json", "secret.txt"] {
            let edit = call(
                "edit_file",
                serde_json::json!({"path": target, "old_text": "a", "new_text": "b"}),
            );
            let err = ToolRegistry::execute(&edit, &ws).expect_err("edit denied");
            assert_eq!(err.to_string(), FIXED, "edit target {target:?}");
        }
        // Denied writes change nothing on disk.
        assert_eq!(
            fs::read_to_string(ws.join("secret.txt")).expect("secret kept"),
            "original-secret"
        );
        assert!(
            !ws.join("fresh.log").exists(),
            "ignored create wrote nothing"
        );

        // Non-excluded writes, edits, and creates still succeed (no behavior
        // change outside the deny).
        let write = call(
            "write_file",
            serde_json::json!({"path": "visible.txt", "content": "changed-visible"}),
        );
        ToolRegistry::execute(&write, &ws).expect("visible write succeeds");
        let create = call(
            "write_file",
            serde_json::json!({"path": "fresh.txt", "content": "new"}),
        );
        ToolRegistry::execute(&create, &ws).expect("non-ignored create succeeds");
        let edit = call(
            "edit_file",
            serde_json::json!({"path": "visible.txt", "old_text": "changed", "new_text": "edited"}),
        );
        ToolRegistry::execute(&edit, &ws).expect("visible edit succeeds");
        assert_eq!(
            fs::read_to_string(ws.join("visible.txt")).expect("visible read"),
            "edited-visible"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn write_file_blocks_single_level_symlink_escape() {
        // Single-level escapes were already rejected; they must stay rejected
        // after the H-1 rework (no behavior change, same error class).
        use std::os::unix::fs::symlink;
        let ws = temp_workspace();
        let outside_dir = temp_workspace();
        fs::write(outside_dir.join("secret.txt"), "outer-secret").expect("outside seed");
        symlink(outside_dir.join("secret.txt"), ws.join("escape.txt")).expect("symlink");

        let write = call(
            "write_file",
            serde_json::json!({"path": "escape.txt", "content": "pwned"}),
        );
        let res = ToolRegistry::execute(&write, &ws);
        assert!(
            matches!(res, Err(ToolError::PathTraversal(_))),
            "single-level symlink escape must stay PathTraversal, got {res:?}"
        );
        assert_eq!(
            fs::read_to_string(outside_dir.join("secret.txt")).expect("outside seed"),
            "outer-secret",
            "outside file must be untouched"
        );
        let _ = fs::remove_dir_all(&ws);
        let _ = fs::remove_dir_all(&outside_dir);
    }
}
