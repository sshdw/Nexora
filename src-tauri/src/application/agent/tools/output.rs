//! Tool output shaping: context-budget truncation and diff rendering.
//!
//! Keeps model-visible output within the context budget; no execution logic.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_OUTPUT_BYTES: usize = 20 * 1024; // 20 KB
const TRUNCATE_HEAD: usize = 10 * 1024;
const TRUNCATE_TAIL: usize = 10 * 1024;
const DIFF_CONTEXT: usize = 3;
const MAX_DIFF_HUNKS: usize = 64;
const LCS_CELL_LIMIT: usize = 4_000_000;

// ---------------------------------------------------------------------------
// Helpers: path, truncation, recursion
// ---------------------------------------------------------------------------

pub(crate) fn truncate_output(s: String) -> String {
    truncate_output_for_run(s, "adhoc")
}

/// [`truncate_output`] bucketed under one agent run.
///
/// `run_id` selects `std::env::temp_dir()/nexora-spills/<run_id>/` and is
/// sanitised to a filename-safe bucket so caller-controlled text can never
/// escape the spill root. Spilling is best-effort: any I/O failure degrades
/// to the inline notice, never to an error.
pub(crate) fn truncate_output_for_run(s: String, run_id: &str) -> String {
    truncate_with_spill_dir(s, &spill_root().join(sanitize_bucket(run_id)))
}

/// Test seam: truncate `s`, spilling the full bytes into `spill_dir`.
pub(crate) fn truncate_with_spill_dir(s: String, spill_dir: &Path) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s;
    }
    // Find char boundaries for head/tail
    let head_end = find_char_boundary(&s, TRUNCATE_HEAD);
    let tail_start = find_char_boundary(&s, s.len().saturating_sub(TRUNCATE_TAIL));
    let head = &s[..head_end];
    let tail = &s[tail_start..];
    let kept = head.len() + tail.len();
    let total = s.len();
    let notice = match spill_bytes(spill_dir, s.as_bytes()) {
        Ok(path) => format!(
            "[truncated: {kept}/{total} bytes, full output spilled to {}]",
            path.display()
        ),
        Err(reason) => format!("[truncated: {kept}/{total} bytes, spill unavailable: {reason}]"),
    };
    format!(
        "{head}\n... [output truncated, {total} bytes total, showing first {} and last {} bytes] ...\n{tail}\n{notice}",
        head.len(),
        tail.len()
    )
}

fn spill_root() -> PathBuf {
    std::env::temp_dir().join("nexora-spills")
}

/// Filename-safe bucket: only ASCII alphanumerics, `-`, `_` survive.
fn sanitize_bucket(run_id: &str) -> String {
    let cleaned: String = run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    let short: String = trimmed.chars().take(64).collect();
    if short.is_empty() {
        "adhoc".to_string()
    } else {
        short
    }
}

static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persist the full pre-truncation bytes to a unique file under `spill_dir`.
fn spill_bytes(spill_dir: &Path, full: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(spill_dir).map_err(|e| io_reason(&e))?;
    let id = SPILL_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = spill_dir.join(format!("spill-{}-{id}-{nanos}.txt", std::process::id()));
    std::fs::write(&path, full).map_err(|e| io_reason(&e))?;
    Ok(path)
}

/// Single-line, bounded, content-free I/O reason for the inline notice.
fn io_reason(e: &std::io::Error) -> String {
    let one_line = e.to_string().replace(['\r', '\n'], " ");
    one_line.chars().take(200).collect()
}

fn find_char_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(index) && index > 0 {
        index -= 1;
    }
    // If we moved back, try to go forward to nearest valid near original?
    // Simpler: walk back until boundary, that's valid.
    // If index was inside a char, we backtrack to start of char, slightly less than requested but safe.
    // For tail, we want to start at a boundary at or after desired index.
    // For tail we should walk forward.
    // This function is used for both head (walk back) and tail (should walk forward).
    // For tail we pass s.len() - TAIL, we should walk forward to next boundary.
    // Handle tail specially: if not boundary, walk forward.
    // Our current call for tail uses index that may be inside char; backing up is also safe (shows slightly more).
    // Acceptable.
    index
}

// ---------------------------------------------------------------------------
// Unified diff (Task 5.2): write_file observation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffOp {
    Equal(String),
    Delete(String),
    Insert(String),
}

#[allow(clippy::too_many_lines)]
pub(crate) fn unified_diff(path: &str, old: &str, new: &str) -> String {
    // Split into lines: empty string => no lines (new file / empty file).
    let old_lines: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.lines().collect()
    };

    // Both empty => headers only (sane edge case).
    if old_lines.is_empty() && new_lines.is_empty() && old == new {
        let mut out = String::new();
        let _ = writeln!(&mut out, "--- a/{path}");
        let _ = writeln!(&mut out, "+++ b/{path}");
        return out;
    }

    let ops = diff_ops(&old_lines, &new_lines);

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| match op {
            DiffOp::Equal(_) => None,
            _ => Some(i),
        })
        .collect();

    let mut out = String::new();
    let _ = writeln!(&mut out, "--- a/{path}");
    let _ = writeln!(&mut out, "+++ b/{path}");

    if changed.is_empty() {
        return out;
    }

    // Positions of each op in old/new (1-based).
    let mut pos_old = Vec::with_capacity(ops.len());
    let mut pos_new = Vec::with_capacity(ops.len());
    let mut old_pos: usize = 1;
    let mut new_pos: usize = 1;
    for op in &ops {
        pos_old.push(old_pos);
        pos_new.push(new_pos);
        match op {
            DiffOp::Equal(_) => {
                old_pos += 1;
                new_pos += 1;
            }
            DiffOp::Delete(_) => old_pos += 1,
            DiffOp::Insert(_) => new_pos += 1,
        }
    }

    // Build context-expanded ranges, merging overlaps.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &idx in &changed {
        let start = idx.saturating_sub(DIFF_CONTEXT);
        let end = std::cmp::min(ops.len().saturating_sub(1), idx + DIFF_CONTEXT);
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 {
                if end > last.1 {
                    last.1 = end;
                }
            } else {
                ranges.push((start, end));
            }
        } else {
            ranges.push((start, end));
        }
    }

    let truncated = ranges.len() > MAX_DIFF_HUNKS;
    if truncated {
        ranges.truncate(MAX_DIFF_HUNKS);
    }

    for (start, end) in ranges {
        let mut old_cnt: usize = 0;
        let mut new_cnt: usize = 0;
        for op in &ops[start..=end] {
            match op {
                DiffOp::Equal(_) => {
                    old_cnt += 1;
                    new_cnt += 1;
                }
                DiffOp::Delete(_) => old_cnt += 1,
                DiffOp::Insert(_) => new_cnt += 1,
            }
        }
        let old_start = if old_cnt == 0 { 0 } else { pos_old[start] };
        let new_start = if new_cnt == 0 { 0 } else { pos_new[start] };
        let _ = writeln!(
            &mut out,
            "@@ -{old_start},{old_cnt} +{new_start},{new_cnt} @@"
        );
        for op in &ops[start..=end] {
            match op {
                DiffOp::Equal(s) => {
                    let _ = writeln!(&mut out, " {s}");
                }
                DiffOp::Delete(s) => {
                    let _ = writeln!(&mut out, "-{s}");
                }
                DiffOp::Insert(s) => {
                    let _ = writeln!(&mut out, "+{s}");
                }
            }
        }
    }

    if truncated {
        let _ = writeln!(
            &mut out,
            "... [diff truncated, too many hunks (limit {MAX_DIFF_HUNKS})] ..."
        );
    }

    out
}

fn diff_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    if old.is_empty() && new.is_empty() {
        return Vec::new();
    }
    if old.is_empty() {
        return new
            .iter()
            .map(|l| DiffOp::Insert((*l).to_string()))
            .collect();
    }
    if new.is_empty() {
        return old
            .iter()
            .map(|l| DiffOp::Delete((*l).to_string()))
            .collect();
    }
    if old.len().saturating_mul(new.len()) > LCS_CELL_LIMIT {
        let mut ops = Vec::with_capacity(old.len() + new.len());
        for l in old {
            ops.push(DiffOp::Delete((*l).to_string()));
        }
        for l in new {
            ops.push(DiffOp::Insert((*l).to_string()));
        }
        return ops;
    }
    let m = old.len();
    let n = new.len();
    let mut dp = vec![0usize; (m + 1) * (n + 1)];
    for i in 1..=m {
        for j in 1..=n {
            let idx = i * (n + 1) + j;
            let diag = (i - 1) * (n + 1) + (j - 1);
            let up = (i - 1) * (n + 1) + j;
            let left = i * (n + 1) + (j - 1);
            if old[i - 1] == new[j - 1] {
                dp[idx] = dp[diag] + 1;
            } else if dp[up] >= dp[left] {
                dp[idx] = dp[up];
            } else {
                dp[idx] = dp[left];
            }
        }
    }
    let mut i = m;
    let mut j = n;
    let mut rev: Vec<DiffOp> = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            rev.push(DiffOp::Equal(old[i - 1].to_string()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i * (n + 1) + (j - 1)] >= dp[(i - 1) * (n + 1) + j]) {
            rev.push(DiffOp::Insert(new[j - 1].to_string()));
            j -= 1;
        } else {
            rev.push(DiffOp::Delete(old[i - 1].to_string()));
            i -= 1;
        }
    }
    rev.reverse();
    rev
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::{test_support::*, ToolRegistry};
    use std::fs;
    #[test]
    fn output_truncation_preserves_head_and_tail() {
        let ws = temp_workspace();
        // Generate ~30KB output via a file
        let content = "X".repeat(30 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({"path": "big.txt", "content": content.clone()}),
        );
        ToolRegistry::execute(&write, &ws).expect("write big");

        let cmd_str = if cfg!(windows) {
            "type big.txt"
        } else {
            "cat big.txt"
        };
        let c = call("execute_command", serde_json::json!({"command": cmd_str}));
        let out = ToolRegistry::execute(&c, &ws).expect("cat big");
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500,
            "output should be truncated near 20KB"
        );
        assert!(
            out.contains("output truncated"),
            "should contain truncation marker"
        );
        assert!(out.contains("bytes total"));
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_file_result_is_truncated_at_context_budget() {
        let ws = temp_workspace();
        let content = "z".repeat(40 * 1024);
        let write = call(
            "write_file",
            serde_json::json!({ "path": "wide.txt", "content": content }),
        );
        ToolRegistry::execute(&write, &ws).expect("seed file");

        let read = call("read_file", serde_json::json!({ "path": "wide.txt" }));
        let out = ToolRegistry::execute(&read, &ws).expect("read");
        assert!(
            out.contains("output truncated"),
            "read_file must truncate to 20KB"
        );
        assert!(out.len() <= MAX_OUTPUT_BYTES + 500);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn list_directory_result_is_truncated_at_context_budget() {
        let ws = temp_workspace();
        // ~1200 root entries at roughly 40 bytes each exceed the 20KB budget.
        for i in 0..1200_u32 {
            fs::write(ws.join(format!("f{i:04}.txt")), "x").expect("create entry");
        }
        let list = call("list_directory", serde_json::json!({}));
        let out = ToolRegistry::execute(&list, &ws).expect("list");
        assert!(
            out.contains("output truncated"),
            "listing must truncate to 20KB"
        );
        assert!(out.len() <= MAX_OUTPUT_BYTES + 500);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_new_file_diff_contains_headers_and_additions() {
        let ws = temp_workspace();
        let content = "line1\nline2\nline3";
        let call = call(
            "write_file",
            serde_json::json!({"path": "new.txt", "content": content}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("new file diff");
        assert!(out.contains("--- a/new.txt"), "missing header a: {out}");
        assert!(out.contains("+++ b/new.txt"), "missing header b: {out}");
        assert!(out.contains("@@"), "missing hunk header: {out}");
        assert!(out.contains("+line1"), "missing addition line1: {out}");
        assert!(out.contains("+line2"), "missing addition line2: {out}");
        assert!(out.contains("+line3"), "missing addition line3: {out}");
        // New file is diffed against empty -> old count 0
        assert!(
            out.contains("-0,0 +1,3") || out.contains("-0,0"),
            "new file hunk counts wrong: {out}"
        );
        assert_eq!(fs::read_to_string(ws.join("new.txt")).unwrap(), content);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_modified_file_diff_shows_hunk_with_context() {
        let ws = temp_workspace();
        let old = "line1\nline2\nline3\nline4\nline5";
        fs::write(ws.join("mod.txt"), old).expect("seed old");
        let new = "line1\nchanged\nline3\nline4\nline5";
        let call = call(
            "write_file",
            serde_json::json!({"path": "mod.txt", "content": new}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("modified diff");
        assert!(out.contains("--- a/mod.txt"), "header missing: {out}");
        assert!(out.contains("+++ b/mod.txt"), "header missing: {out}");
        assert!(out.contains("-line2"), "removed line missing: {out}");
        assert!(out.contains("+changed"), "added line missing: {out}");
        // Context lines should appear with leading space
        assert!(
            out.contains(" line1") || out.contains("line1"),
            "context missing: {out}"
        );
        assert_eq!(fs::read_to_string(ws.join("mod.txt")).unwrap(), new);
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_empty_to_empty_produces_sane_diff() {
        let ws = temp_workspace();
        // Create empty file first via direct fs, then overwrite with empty via tool
        fs::write(ws.join("empty.txt"), "").expect("seed empty");
        let call = call(
            "write_file",
            serde_json::json!({"path": "empty.txt", "content": ""}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("empty diff");
        assert!(out.contains("--- a/empty.txt"), "header missing: {out}");
        assert!(out.contains("+++ b/empty.txt"), "header missing: {out}");
        // No hunks for identical empty -> should have only headers
        assert!(
            !out.contains("@@") || out.trim_end().ends_with("+++ b/empty.txt"),
            "empty diff should have no hunks or only headers: {out}"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_diff_truncation_on_huge_content() {
        let ws = temp_workspace();
        let huge = "x\n".repeat(15_000); // ~30KB, diff will exceed 20KB
        let call = call(
            "write_file",
            serde_json::json!({"path": "huge.txt", "content": huge}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("huge diff");
        assert!(
            out.contains("output truncated"),
            "huge diff must be truncated: len={}",
            out.len()
        );
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500,
            "truncated diff too large: {}",
            out.len()
        );
        assert!(
            out.contains("--- a/huge.txt"),
            "header must survive truncation: {out}"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_file_diff_bounded_hunk_count() {
        let ws = temp_workspace();
        // Create old file with many isolated changes (each 10 lines apart) to generate many hunks.
        // With DIFF_CONTEXT=3 and MAX_DIFF_HUNKS=64, we should bound.
        let old_lines: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let old = old_lines.join("\n");
        fs::write(ws.join("many.txt"), &old).expect("seed many");
        let mut new_lines = old_lines.clone();
        for i in (0..500).step_by(7) {
            new_lines[i] = format!("changed {i}");
        }
        let new = new_lines.join("\n");
        let call = call(
            "write_file",
            serde_json::json!({"path": "many.txt", "content": new}),
        );
        let out = ToolRegistry::execute(&call, &ws).expect("many hunks diff");
        let hunk_count = out.lines().filter(|l| l.starts_with("@@")).count();
        assert!(
            hunk_count <= MAX_DIFF_HUNKS,
            "hunk count {hunk_count} exceeds bound {MAX_DIFF_HUNKS}"
        );
        // Verify the diff is bounded and, if truncated, contains marker (hunk limit or output truncate)
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 500 || out.contains("too many hunks"),
            "many hunks diff should be bounded"
        );
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn truncation_appends_spill_notice_with_byte_counts() {
        let total = 30 * 1024;
        let content = "X".repeat(total);
        let bucket = test_spill_bucket("notice");
        let out = truncate_with_spill_dir(content.clone(), &bucket);
        // Machine-readable notice with exact kept/total byte counts.
        let kept = 10 * 1024 + 10 * 1024;
        let prefix = format!("[truncated: {kept}/{total} bytes, full output spilled to ");
        assert!(
            out.contains(&prefix),
            "spill notice with exact counts missing: tail={}",
            out_lines_tail(&out)
        );
        // Legacy silent-form marker is preserved alongside the notice.
        assert!(out.contains("output truncated"));
        assert!(out.contains("bytes total"));
        // The spill path in the notice exists and holds the full bytes.
        let spill_path = notice_spill_path(&out);
        let spilled = fs::read(&spill_path).expect("spill file must exist");
        assert_eq!(spilled.len(), total);
        assert_eq!(spilled, content.as_bytes());
        let _ = fs::remove_dir_all(bucket_root());
    }

    #[test]
    fn spill_failure_falls_back_to_inline_only() {
        // A regular file where the spill directory should be makes
        // `create_dir_all` fail; truncation must still succeed inline.
        let blocker = bucket_root().join("blocker-file");
        fs::create_dir_all(bucket_root()).expect("bucket root");
        fs::write(&blocker, "in the way").expect("blocker file");
        let out = truncate_with_spill_dir("Y".repeat(30 * 1024), &blocker);
        assert!(
            out.contains("[truncated: "),
            "truncation notice missing: tail={}",
            out_lines_tail(&out)
        );
        assert!(
            out.contains("spill unavailable: "),
            "spill failure reason missing: tail={}",
            out_lines_tail(&out)
        );
        assert!(!out.contains("spilled to "), "no spill path expected");
        // Head/tail content still present around the legacy marker.
        assert!(out.contains("output truncated"));
        let _ = fs::remove_dir_all(bucket_root());
    }

    fn bucket_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nexora-tools-spill-test-{}", std::process::id()))
    }

    fn test_spill_bucket(tag: &str) -> std::path::PathBuf {
        let dir = bucket_root().join(tag);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("test spill bucket");
        dir
    }

    fn out_lines_tail(out: &str) -> String {
        out.lines().last().unwrap_or("").to_string()
    }

    /// Extract the spill file path from the `[truncated: ... spilled to <path>]` notice.
    fn notice_spill_path(out: &str) -> std::path::PathBuf {
        let line = out
            .lines()
            .find(|l| l.contains("full output spilled to "))
            .expect("notice line");
        let start = line.find("spilled to ").expect("marker") + "spilled to ".len();
        let end = line.rfind(']').expect("closing bracket");
        std::path::PathBuf::from(line[start..end].trim())
    }
}
