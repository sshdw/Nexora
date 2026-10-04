//! Repo audit engine: read-only static analysis of the workspace repository.
//!
//! Substrate decision (Phase 1, binding — no new dependencies): pure-Rust
//! line scanners over [`std::fs`] reads. `Cargo.toml` carries no `tree-sitter`
//! or `regex` dependency and the slice forbids new cargo deps, so every
//! heuristic below is substring / brace-depth matching on UTF-8 lines — no
//! parsing, no name resolution, no type information. TS type-aware analysis is
//! out of scope: TypeScript sources are scanned as text only.
//!
//! Method limits (stated honestly here and surfaced in the panel disclosure):
//! dead-code hits are *candidates, not proof* — re-exports, trait impls,
//! macro-generated names, and dynamic uses are missed by the substring count;
//! brace-depth function lengths miscount braces inside strings and block
//! comments; `.ok()` / `.clone()` tallies cannot tell an intentional
//! conversion from a swallowed error or a wasteful clone.
//! `unwrap-hotspot` skips test code (files under `tests/` or `test/`,
//! `*_test.rs` / `*.test.ts(x)` / `*.spec.ts(x)` names, and `#[cfg(test)]`
//! items) — `unwrap` / `expect` there is idiomatic, not a panic risk.
//!
//! Read-only contract: this module only reads directory entries and file
//! bytes (`read_dir`, `metadata`, `read_to_string`); it never creates,
//! writes, renames, or deletes anything. Findings carry at most
//! [`MAX_EXCERPT_LINES`] source lines, each truncated to
//! [`MAX_EXCERPT_CHARS`] characters — never full files — and errors are
//! fixed-vocabulary with no paths (paths may contain user names).
//!
//! Scope: `.rs` / `.ts` / `.tsx` files under the workspace root, capped at
//! [`MAX_AUDIT_FILES`] files and [`MAX_FILE_BYTES`] bytes per file; anything
//! beyond the caps is reported as a skip notice, never silently dropped.
//! Symlinks are skipped (cycle/escape backstop); `target`, `node_modules`,
//! `.git`, `dist`, `build`, and `.nexora` trees are pruned. There is no
//! watching or live re-audit: the frontend runs the scan manually.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Most workspace source files scanned per run; the remainder is reported as
/// `file-cap` skips (a count, never content).
pub(crate) const MAX_AUDIT_FILES: usize = 2000;

/// Largest single file read before it is reported as a `too-large` skip.
pub(crate) const MAX_FILE_BYTES: usize = 256 * 1024;

/// Most findings returned; the remainder is reported in `findings_overflow`.
pub(crate) const MAX_FINDINGS: usize = 2000;

/// Most skip entries listed; the remainder is reported in `skipped_overflow`.
pub(crate) const MAX_SKIPPED_LISTED: usize = 50;

/// Source lines carried per finding (secret-free excerpt, never full files).
pub(crate) const MAX_EXCERPT_LINES: usize = 3;

/// Characters carried per excerpt line.
pub(crate) const MAX_EXCERPT_CHARS: usize = 160;

/// File length at or above which a file reads `oversized-file`, in lines.
pub(crate) const LARGE_FILE_LINES: usize = 800;

/// Function length at or above which a function reads `oversized-function`,
/// in lines (brace-depth approximation — see the module docs).
pub(crate) const LARGE_FUNCTION_LINES: usize = 100;

/// `.clone()` calls per Rust file at or above which the file reads
/// `suspicious-clone` (one finding per file, not per call).
pub(crate) const CLONE_DENSITY: usize = 5;

/// Item names shorter than this are excluded from the dead-code pass: a
/// one- or two-character substring matches inside unrelated identifiers so
/// often that the candidate list would be noise (and the per-name scan
/// would dominate the run time).
pub(crate) const MIN_DEF_NAME_LEN: usize = 3;

/// Fixed finding-kind vocabulary. The frontend renders labels for exactly
/// these tokens and echoes anything else defensively.
pub(crate) const KIND_DEAD_CODE: &str = "dead-code-candidate";
/// Panic-path hotspot (`.unwrap()` / `.expect(`).
pub(crate) const KIND_UNWRAP: &str = "unwrap-hotspot";
/// `TODO` / `FIXME` marker debt.
pub(crate) const KIND_TODO: &str = "todo-debt";
/// File at or above [`LARGE_FILE_LINES`] lines.
pub(crate) const KIND_LARGE_FILE: &str = "oversized-file";
/// Function at or above [`LARGE_FUNCTION_LINES`] lines.
pub(crate) const KIND_LARGE_FN: &str = "oversized-function";
/// Undocumented `pub` item (Rust only — doc-comment presence, not quality).
pub(crate) const KIND_MISSING_DOCS: &str = "missing-docs";
/// Error-swallowing shape (`let _ =`, `.ok()`, `.unwrap_or_default()`).
pub(crate) const KIND_SWALLOWED: &str = "error-swallowed";
/// Suppressed check (`#[allow(`, `as any`, `ts-ignore`, `eslint-disable`).
pub(crate) const KIND_UNCHECKED: &str = "unchecked-result";
/// Clone-density hotspot (`.clone()` tally or serialize-clone in TS).
pub(crate) const KIND_CLONE: &str = "suspicious-clone";

/// Every [`KIND_*`] token, in panel group order.
pub(crate) const FINDING_KINDS: [&str; 9] = [
    KIND_DEAD_CODE,
    KIND_UNWRAP,
    KIND_TODO,
    KIND_LARGE_FILE,
    KIND_LARGE_FN,
    KIND_MISSING_DOCS,
    KIND_SWALLOWED,
    KIND_UNCHECKED,
    KIND_CLONE,
];

/// Fixed severity vocabulary.
pub(crate) const SEVERITY_INFO: &str = "info";
/// Fixed severity vocabulary.
pub(crate) const SEVERITY_WARNING: &str = "warning";

/// Directories pruned from the walk (build output, vendored trees, VCS, and
/// the workspace project directory — never source).
const SKIP_DIRS: [&str; 6] = ["target", "node_modules", ".git", "dist", "build", ".nexora"];

/// Fixed skip-reason vocabulary.
const SKIP_TOO_LARGE: &str = "too-large";
/// Fixed skip-reason vocabulary.
const SKIP_UNREADABLE: &str = "unreadable";
/// Fixed skip-reason vocabulary.
const SKIP_FILE_CAP: &str = "file-cap";

/// `pub <item>` tokens recognized by [`rs_pub_item`].
const RS_ITEMS: [&str; 8] = [
    "fn", "struct", "enum", "trait", "type", "mod", "const", "static",
];

/// `export <item>` tokens recognized by [`ts_export_item`].
const TS_ITEMS: [&str; 8] = [
    "function",
    "class",
    "interface",
    "type",
    "enum",
    "const",
    "let",
    "var",
];

/// One audit finding: fixed-vocabulary kind + severity with `file:line`
/// evidence and a capped code excerpt (at most [`MAX_EXCERPT_LINES`] lines).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AuditFinding {
    /// One of the [`FINDING_KINDS`] tokens.
    pub kind: String,
    /// [`SEVERITY_INFO`] or [`SEVERITY_WARNING`].
    pub severity: String,
    /// Workspace-relative path with forward slashes (never absolute, so no
    /// user names leak through home-directory prefixes).
    pub path: String,
    /// 1-based line number of the evidence.
    pub line: usize,
    /// Up to [`MAX_EXCERPT_LINES`] source lines from the evidence line.
    pub excerpt: String,
}

/// One file the scan did not read: path plus a fixed-vocabulary reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SkippedFile {
    /// Workspace-relative path with forward slashes.
    pub path: String,
    /// `too-large` | `unreadable` | `file-cap`.
    pub reason: String,
}

/// One read-only audit run: capped findings plus scan accounting. Lists stay
/// capped with overflow counts; totals always cover the whole walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RepoAuditReport {
    /// Findings sorted by `(path, line, kind)`, capped at [`MAX_FINDINGS`].
    pub findings: Vec<AuditFinding>,
    /// Findings omitted by the cap (0 when everything fit).
    pub findings_overflow: usize,
    /// Source files fully scanned.
    pub files_scanned: usize,
    /// Source files not read (listed + overflow combined).
    pub files_skipped: usize,
    /// First [`MAX_SKIPPED_LISTED`] skips, in walk order.
    pub skipped: Vec<SkippedFile>,
    /// Skips omitted from the list (0 when all are listed).
    pub skipped_overflow: usize,
}

/// Secret-free failures for the audit scan. Variants carry no payload, so
/// formatting one can never leak a path, file content, or credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoAuditError {
    /// The workspace root is not a readable directory.
    InvalidRoot,
    /// A filesystem read failed mid-walk.
    Io,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rs,
    Ts,
}

struct Discovered {
    rel: String,
    full: PathBuf,
    lang: Lang,
}

struct PubDef {
    name: String,
    path: String,
    line: usize,
    excerpt: String,
}

/// One source file read exactly once and shared by the declaration pass and
/// the heuristic/use-tally pass ([`audit_workspace`]).
struct CachedFile {
    rel: String,
    lang: Lang,
    text: String,
}

/// Severity for one kind: panic paths, swallowed errors, suppressed checks,
/// and clone density warn; debt, size, docs, and dead-code candidates inform.
#[must_use]
fn severity_for(kind: &str) -> &'static str {
    if kind == KIND_UNWRAP || kind == KIND_SWALLOWED || kind == KIND_UNCHECKED || kind == KIND_CLONE
    {
        SEVERITY_WARNING
    } else {
        SEVERITY_INFO
    }
}

/// Truncate one excerpt line to [`MAX_EXCERPT_CHARS`] characters
/// (char-boundary safe — byte slicing could split multi-byte text).
#[must_use]
fn truncate_chars(line: &str) -> String {
    if line.chars().count() <= MAX_EXCERPT_CHARS {
        line.to_string()
    } else {
        line.chars().take(MAX_EXCERPT_CHARS).collect()
    }
}

/// Up to [`MAX_EXCERPT_LINES`] source lines starting at the 1-based `line`.
#[must_use]
fn excerpt_at(lines: &[String], line: usize) -> String {
    lines
        .iter()
        .skip(line.saturating_sub(1))
        .take(MAX_EXCERPT_LINES)
        .map(|text| truncate_chars(text.trim_end()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Push one finding with no cap: callers collect every candidate, then
/// [`cap_findings`] sorts by `(path, line, kind)` and truncates to
/// [`MAX_FINDINGS`]. Capping only after the sort keeps late passes (dead-code
/// candidates) from starving behind early line findings.
fn push_finding(
    findings: &mut Vec<AuditFinding>,
    kind: &str,
    path: &str,
    line: usize,
    excerpt: String,
) {
    findings.push(AuditFinding {
        kind: kind.to_string(),
        severity: severity_for(kind).to_string(),
        path: path.to_string(),
        line,
        excerpt,
    });
}

/// Sort findings by `(path, line, kind)` and keep the first [`MAX_FINDINGS`].
/// Returns the omitted count (0 when everything fit).
fn cap_findings(findings: &mut Vec<AuditFinding>) -> usize {
    findings.sort_by(|a, b| {
        (a.path.clone(), a.line, a.kind.clone()).cmp(&(b.path.clone(), b.line, b.kind.clone()))
    });
    let overflow = findings.len().saturating_sub(MAX_FINDINGS);
    findings.truncate(MAX_FINDINGS);
    overflow
}

fn push_skipped(skipped: &mut Vec<SkippedFile>, overflow: &mut usize, path: String, reason: &str) {
    if skipped.len() < MAX_SKIPPED_LISTED {
        skipped.push(SkippedFile {
            path,
            reason: reason.to_string(),
        });
    } else {
        *overflow += 1;
    }
}

#[must_use]
fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => Some(Lang::Rs),
        Some("ts" | "tsx") => Some(Lang::Ts),
        _ => None,
    }
}

/// Workspace-relative display path with forward slashes (UI-stable on every
/// OS; never absolute). `None` when the path escapes the root.
#[must_use]
fn rel_path(root: &Path, full: &Path) -> Option<String> {
    full.strip_prefix(root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

/// Walk the workspace for source files in deterministic (sorted) order,
/// pruning [`SKIP_DIRS`] and symlinks. Returns the capped read list plus the
/// skip notices (`file-cap` once the list is full).
fn collect_sources(root: &Path) -> (Vec<Discovered>, Vec<SkippedFile>, usize) {
    let mut stack = vec![root.to_path_buf()];
    let mut found: Vec<Discovered> = Vec::new();
    let mut skipped: Vec<SkippedFile> = Vec::new();
    let mut skipped_overflow: usize = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut ordered: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            ordered.push(entry.path());
        }
        ordered.sort();
        for full in ordered {
            let file_type = match std::fs::symlink_metadata(&full) {
                Ok(meta) => meta.file_type(),
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = full
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if !SKIP_DIRS.contains(&name) {
                    stack.push(full);
                }
                continue;
            }
            let Some(lang) = lang_of(&full) else {
                continue;
            };
            let Some(rel) = rel_path(root, &full) else {
                continue;
            };
            if found.len() < MAX_AUDIT_FILES {
                found.push(Discovered { rel, full, lang });
            } else {
                push_skipped(&mut skipped, &mut skipped_overflow, rel, SKIP_FILE_CAP);
            }
        }
    }
    found.sort_by(|a, b| a.rel.cmp(&b.rel));
    (found, skipped, skipped_overflow)
}

/// Byte-true word character for the dead-code boundary guard.
#[must_use]
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Count whole-word occurrences of `name`, stopping at 3 (the caller only
/// needs to tell 0 / 1 / more-than-1 apart). Substring matches inside longer
/// identifiers do not count.
#[must_use]
fn count_word_occurrences(haystack: &str, name: &str) -> usize {
    if name.is_empty() {
        return 0;
    }
    let text = haystack.as_bytes();
    let needle = name.as_bytes();
    if needle.len() > text.len() {
        return 0;
    }
    let mut count: usize = 0;
    let mut index: usize = 0;
    while index + needle.len() <= text.len() {
        if &text[index..index + needle.len()] == needle
            && (index == 0 || !is_word_byte(text[index - 1]))
            && (index + needle.len() == text.len() || !is_word_byte(text[index + needle.len()]))
        {
            count += 1;
            if count >= 3 {
                return count;
            }
            index += needle.len();
        } else {
            index += 1;
        }
    }
    count
}

/// Leading identifier of `rest` (ASCII word characters only — operator-heavy
/// suffixes like `<T>` or `(` terminate the name).
#[must_use]
fn leading_ident(rest: &str) -> Option<String> {
    let end = rest
        .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let name = rest[..end].to_string();
    if name.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(name)
}

/// A `pub <item> <Name>` declaration line (true public API only — `pub(crate)`
/// and `pub(super)` never start with `pub `). Returns the item name.
#[must_use]
fn rs_pub_item(line: &str) -> Option<String> {
    let mut rest = line.strip_prefix("pub ")?;
    loop {
        if let Some(next) = rest
            .strip_prefix("async ")
            .or_else(|| rest.strip_prefix("unsafe "))
            .or_else(|| rest.strip_prefix("const "))
            .or_else(|| rest.strip_prefix("extern "))
        {
            rest = next;
        } else if let Some(quoted) = rest.strip_prefix('"') {
            // `extern "C" fn`: skip the quoted ABI before the `fn` token.
            let closing = quoted.find('"')?;
            rest = quoted[closing + 1..].trim_start();
        } else {
            break;
        }
    }
    for item in RS_ITEMS {
        if let Some(next) = rest.strip_prefix(item).filter(|next| next.starts_with(' ')) {
            return leading_ident(next.trim_start());
        }
    }
    None
}

/// Rust public-API items that must carry a `///` doc comment. Only true `pub`
/// items are findings (`pub(crate)` / `pub(super)` never start with `pub `,
/// so they are out of scope by construction); `const` and `static` items are
/// excluded — associated constants conventionally document on the parent
/// item, so flagging each would be noise. Common `fn` modifiers (`async`,
/// `unsafe`, `const`, `extern "ABI"`, in any order and combination) are
/// stripped before the item check, mirroring [`rs_pub_item`].
#[must_use]
fn rs_doc_item(line: &str) -> bool {
    let Some(mut rest) = line.strip_prefix("pub ") else {
        return false;
    };
    loop {
        if let Some(next) = rest
            .strip_prefix("async ")
            .or_else(|| rest.strip_prefix("unsafe "))
            .or_else(|| rest.strip_prefix("const "))
            .or_else(|| rest.strip_prefix("extern "))
        {
            rest = next;
        } else if let Some(quoted) = rest.strip_prefix('"') {
            // `extern "C" fn`: skip the quoted ABI before the `fn` token.
            let Some(closing) = quoted.find('"') else {
                return false;
            };
            rest = quoted[closing + 1..].trim_start();
        } else {
            break;
        }
    }
    rest.starts_with("fn ")
        || rest.starts_with("struct ")
        || rest.starts_with("enum ")
        || rest.starts_with("trait ")
        || rest.starts_with("type ")
        || rest.starts_with("mod ")
}

/// Test-code paths whose `.unwrap()` / `.expect(` calls are idiomatic and
/// never flag [`KIND_UNWRAP`]: files under `tests/` or `test/` directories,
/// Rust `*_test.rs` / `test_*.rs` / `tests.rs` names, and TS
/// `*.test.*` / `*.spec.*` names.
#[must_use]
fn is_test_source_path(rel: &str) -> bool {
    let mut parts = rel.split('/');
    let file = parts.next_back().unwrap_or(rel);
    if parts.any(|dir| dir == "tests" || dir == "test") {
        return true;
    }
    if file.contains(".test.") || file.contains(".spec.") {
        return true;
    }
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem == "tests"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
}

/// Whether the `pub` item at `index` carries a `///` doc comment: walk upward
/// past blank lines and `#[...]` attributes; documentation must appear
/// before any other code line.
#[must_use]
fn has_rs_doc(lines: &[String], index: usize) -> bool {
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        let above = lines[cursor].trim();
        if above.is_empty() || above.starts_with("#[") {
            continue;
        }
        return above.starts_with("///");
    }
    false
}

/// An `export <item> <Name>` declaration line. Re-export shapes (`export {`,
/// `export *`, `export =`) declare nothing and return `None`.
#[must_use]
fn ts_export_item(line: &str) -> Option<String> {
    let mut rest = line.strip_prefix("export ")?;
    if let Some(next) = rest.strip_prefix("default ") {
        rest = next;
    }
    if let Some(next) = rest.strip_prefix("async ") {
        rest = next;
    }
    if let Some(next) = rest.strip_prefix("abstract ") {
        rest = next;
    }
    for item in TS_ITEMS {
        if let Some(next) = rest
            .strip_prefix(item)
            .filter(|next| next.is_empty() || next.starts_with(' '))
        {
            return leading_ident(next.trim_start());
        }
    }
    None
}

/// Comment-only lines, per language. In Rust, outer attributes (`#[...]`)
/// are NOT comments — they carry the `#[allow(` suppression check — only
/// inner attributes and shebangs (`#!...`) are skipped. In TS, `#` is
/// excluded: it would false-positive on private `#field` declarations.
#[must_use]
fn is_comment_line(trimmed: &str, lang: Lang) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || (lang == Lang::Rs && trimmed.starts_with("#!"))
}

/// A function-definition line (brace-depth tracking starts here).
/// Approximations, documented in the module docs: TS arrows are only the
/// trailing-`{` shape; one pending function at a time (nesting ignored).
#[must_use]
fn is_fn_def(trimmed: &str, lang: Lang) -> bool {
    match lang {
        Lang::Rs => trimmed.starts_with("fn ") || trimmed.contains(" fn "),
        Lang::Ts => {
            trimmed.starts_with("function")
                || trimmed.contains(" function ")
                || trimmed.contains(" function(")
                || (trimmed.contains("=>") && trimmed.ends_with('{'))
        }
    }
}

/// Brace delta of one line, ignoring braces inside `"..."` strings and after
/// a `//` comment start. Block comments and character literals are NOT
/// handled (documented approximation).
#[must_use]
fn brace_delta(line: &str) -> i32 {
    let mut delta: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == '/' && chars.peek() == Some(&'/') {
            break;
        } else if ch == '{' {
            delta += 1;
        } else if ch == '}' {
            delta -= 1;
        }
    }
    delta
}

/// Scan one file's lines: debt, panic paths, swallowed errors, suppressed
/// checks, missing docs, clone tallies, and brace-depth function lengths.
/// `unwrap-hotspot` skips test code — [`is_test_source_path`] files and lines
/// inside `#[cfg(test)]` items (module scope tracked by brace depth).
#[allow(clippy::too_many_lines)]
fn scan_lines(rel: &str, lang: Lang, text: &str, findings: &mut Vec<AuditFinding>) {
    let owned: Vec<String> = text.lines().map(str::to_string).collect();
    let lines: &[String] = &owned;
    if lines.len() >= LARGE_FILE_LINES {
        push_finding(findings, KIND_LARGE_FILE, rel, 1, excerpt_at(lines, 1));
    }
    let test_file = lang == Lang::Rs && is_test_source_path(rel);
    let mut depth: i32 = 0;
    let mut pending_start: Option<usize> = None;
    let mut pending_depth: i32 = 0;
    let mut pending_opened = false;
    // `#[cfg(test)]` scope: the attribute arms `cfg_pending`; the next code
    // line opens a scope at the current brace depth (covers `mod tests {`
    // blocks and directly-annotated `fn`s alike) that closes when the depth
    // returns. Attribute/blank lines in between do not consume the arm.
    let mut cfg_pending = false;
    let mut cfg_depth: Option<i32> = None;
    let mut clones: usize = 0;
    let mut first_clone_line: usize = 0;
    for (index, line) in lines.iter().enumerate() {
        let lineno = index + 1;
        let trimmed = line.trim();
        if trimmed.contains("TODO") || trimmed.contains("FIXME") {
            push_finding(findings, KIND_TODO, rel, lineno, excerpt_at(lines, lineno));
        }
        if lang == Lang::Rs && trimmed.starts_with("#[cfg(test)]") {
            // Same-line form (`#[cfg(test)] mod tests {`) opens the scope
            // immediately; otherwise the next code line does.
            if trimmed.contains("mod ") {
                cfg_depth = Some(depth);
                cfg_pending = false;
            } else {
                cfg_pending = true;
            }
        } else if cfg_pending
            && !trimmed.is_empty()
            && !trimmed.starts_with("#[")
            && !is_comment_line(trimmed, lang)
        {
            cfg_depth = Some(depth);
            cfg_pending = false;
        }
        let in_cfg_test = cfg_depth.is_some();
        if is_comment_line(trimmed, lang) {
            continue;
        }
        match lang {
            Lang::Rs => {
                if !test_file
                    && !in_cfg_test
                    && (trimmed.contains(".unwrap()") || trimmed.contains(".expect("))
                {
                    push_finding(
                        findings,
                        KIND_UNWRAP,
                        rel,
                        lineno,
                        excerpt_at(lines, lineno),
                    );
                }
                if trimmed.starts_with("let _ =")
                    || trimmed.contains(".ok()")
                    || trimmed.contains(".unwrap_or_default()")
                {
                    push_finding(
                        findings,
                        KIND_SWALLOWED,
                        rel,
                        lineno,
                        excerpt_at(lines, lineno),
                    );
                }
                if trimmed.starts_with("#[allow(") {
                    push_finding(
                        findings,
                        KIND_UNCHECKED,
                        rel,
                        lineno,
                        excerpt_at(lines, lineno),
                    );
                }
                if rs_doc_item(trimmed) && !has_rs_doc(lines, index) {
                    push_finding(
                        findings,
                        KIND_MISSING_DOCS,
                        rel,
                        lineno,
                        excerpt_at(lines, lineno),
                    );
                }
                let file_clones = line.matches(".clone()").count();
                if file_clones > 0 {
                    if clones == 0 {
                        first_clone_line = lineno;
                    }
                    clones += file_clones;
                }
            }
            Lang::Ts => {
                if trimmed.contains("as any")
                    || trimmed.contains("@ts-ignore")
                    || trimmed.contains("@ts-nocheck")
                    || trimmed.contains("eslint-disable")
                {
                    push_finding(
                        findings,
                        KIND_UNCHECKED,
                        rel,
                        lineno,
                        excerpt_at(lines, lineno),
                    );
                }
                if line.contains("JSON.parse(JSON.stringify") {
                    push_finding(findings, KIND_CLONE, rel, lineno, excerpt_at(lines, lineno));
                }
            }
        }
        if pending_start.is_none() && is_fn_def(trimmed, lang) {
            pending_start = Some(index);
            pending_depth = depth;
            pending_opened = false;
        }
        depth += brace_delta(line);
        // A `#[cfg(test)]` scope closes once the brace depth returns to (or
        // below) the depth where it opened — a braceless annotated item
        // clears on its own line.
        if let Some(opened) = cfg_depth {
            if depth <= opened {
                cfg_depth = None;
            }
        }
        if let Some(start) = pending_start {
            if depth > pending_depth {
                pending_opened = true;
            } else if pending_opened && depth <= pending_depth && index > start {
                if index + 1 - start >= LARGE_FUNCTION_LINES {
                    push_finding(
                        findings,
                        KIND_LARGE_FN,
                        rel,
                        start + 1,
                        excerpt_at(lines, start + 1),
                    );
                }
                pending_start = None;
                pending_opened = false;
            } else if !pending_opened && depth < pending_depth {
                // A brace closed above the definition (body-less declaration
                // nearby) — drop the pending start rather than misattributing.
                pending_start = None;
            }
        }
    }
    if let Some(start) = pending_start {
        if pending_opened && lines.len() - start >= LARGE_FUNCTION_LINES {
            push_finding(
                findings,
                KIND_LARGE_FN,
                rel,
                start + 1,
                excerpt_at(lines, start + 1),
            );
        }
    }
    if lang == Lang::Rs && clones >= CLONE_DENSITY {
        push_finding(
            findings,
            KIND_CLONE,
            rel,
            first_clone_line,
            excerpt_at(lines, first_clone_line),
        );
    }
}

/// Yield the identifier tokens of `text`: maximal runs of ASCII word
/// characters. Non-ASCII bytes are boundaries, matching [`is_word_byte`].
fn word_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
}

/// Tally whole-word uses of the declared names in one file, capped at 2 per
/// name (the dead-code pass only needs 0 / 1 / more-than-1). One linear
/// tokenization per file plus hash lookups — no per-name rescan — so the
/// pass scales with workspace size instead of names-times-files.
fn tally_uses(text: &str, name_set: &HashSet<&str>, uses: &mut HashMap<String, usize>) {
    for token in word_tokens(text) {
        let Some(canonical) = name_set.get(token) else {
            continue;
        };
        let capped = uses.get(*canonical).copied().unwrap_or(0);
        if capped < 2 {
            uses.insert((*canonical).to_string(), capped + 1);
        }
    }
}

/// Run the read-only audit over the workspace `root`.
///
/// # Errors
///
/// Returns [`RepoAuditError::InvalidRoot`] when `root` is not a directory and
/// [`RepoAuditError::Io`] when the root cannot be listed. Unreadable files
/// and directories below the root are skip notices, never errors.
pub(crate) fn audit_workspace(root: &Path) -> Result<RepoAuditReport, RepoAuditError> {
    if !root.is_dir() {
        return Err(RepoAuditError::InvalidRoot);
    }
    let (sources, mut skipped, mut skipped_overflow) = collect_sources(root);
    if sources.is_empty() && skipped.is_empty() && root.read_dir().is_err() {
        return Err(RepoAuditError::Io);
    }
    // Single read: each file hits the filesystem once; the cached text is
    // shared by the declaration pass below and the heuristic/use-tally
    // pass. Cached entries are bounded by the walk caps ([`MAX_AUDIT_FILES`]
    // files, [`MAX_FILE_BYTES`] bytes each).
    let mut cached: Vec<CachedFile> = Vec::new();
    for source in &sources {
        let Ok(text) = std::fs::read_to_string(&source.full) else {
            push_skipped(
                &mut skipped,
                &mut skipped_overflow,
                source.rel.clone(),
                SKIP_UNREADABLE,
            );
            continue;
        };
        if text.len() > MAX_FILE_BYTES {
            push_skipped(
                &mut skipped,
                &mut skipped_overflow,
                source.rel.clone(),
                SKIP_TOO_LARGE,
            );
            continue;
        }
        cached.push(CachedFile {
            rel: source.rel.clone(),
            lang: source.lang,
            text,
        });
    }
    // Pass 1: public/exported item declarations (names only; text kept in
    // `cached` for pass 2).
    let mut defs: Vec<PubDef> = Vec::new();
    for file in &cached {
        for (index, line) in file.text.lines().enumerate() {
            let trimmed = line.trim();
            let name = match file.lang {
                Lang::Rs => rs_pub_item(trimmed),
                Lang::Ts => ts_export_item(trimmed),
            };
            if let Some(name) = name {
                if name.len() >= MIN_DEF_NAME_LEN {
                    defs.push(PubDef {
                        name,
                        path: file.rel.clone(),
                        line: index + 1,
                        excerpt: truncate_chars(trimmed),
                    });
                }
            }
        }
    }
    let def_names: Vec<String> = defs.iter().map(|def| def.name.clone()).collect();
    let name_set: HashSet<&str> = def_names.iter().map(String::as_str).collect();
    // Pass 2: line heuristics plus whole-word use tallies for pass 3.
    let mut findings: Vec<AuditFinding> = Vec::new();
    let mut uses: HashMap<String, usize> = HashMap::new();
    let mut files_scanned: usize = 0;
    for file in &cached {
        files_scanned += 1;
        scan_lines(&file.rel, file.lang, &file.text, &mut findings);
        tally_uses(&file.text, &name_set, &mut uses);
    }
    // Pass 3: `pub`-never-used candidates (candidates, not proof — see the
    // module docs). The declaration line itself is the single expected hit.
    for def in &defs {
        if uses.get(&def.name).copied().unwrap_or(0) <= 1 {
            push_finding(
                &mut findings,
                KIND_DEAD_CODE,
                &def.path,
                def.line,
                def.excerpt.clone(),
            );
        }
    }
    // Sort-then-truncate: every pass contributes uncapped, the full set is
    // ordered by `(path, line, kind)`, and only then is the [`MAX_FINDINGS`]
    // cap applied — so dead-code candidates sort in on merit instead of
    // starving behind earlier line findings.
    let findings_overflow = cap_findings(&mut findings);
    let files_skipped = skipped.len() + skipped_overflow;
    Ok(RepoAuditReport {
        findings,
        findings_overflow,
        files_scanned,
        files_skipped,
        skipped,
        skipped_overflow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "nexora-repo-audit-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("test workspace creates");
        root
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        use std::io::Write as _;
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("test parent creates");
        }
        // `OpenOptions` (not the one-call file-writing shorthand) so the
        // read-only static check below keeps meaning "the engine never
        // writes" while the fixtures still need bytes on disk.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&full)
            .expect("test file opens");
        file.write_all(content.as_bytes())
            .expect("test file writes");
    }

    fn with_cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    fn of_kind<'a>(report: &'a RepoAuditReport, kind: &str) -> Vec<&'a AuditFinding> {
        report
            .findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .collect()
    }

    #[test]
    fn finding_kinds_and_severities_stay_in_fixed_vocab() {
        let root = test_root();
        write_file(
            &root,
            "src/lib.rs",
            "pub fn audited_entry() {\n    let _ = fallible();\n    // TODO: revisit\n}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        assert!(
            !report.findings.is_empty(),
            "the fixture must produce findings"
        );
        for finding in &report.findings {
            assert!(
                FINDING_KINDS.contains(&finding.kind.as_str()),
                "kind must be fixed vocabulary, found {:?}",
                finding.kind
            );
            assert!(
                finding.severity == SEVERITY_INFO || finding.severity == SEVERITY_WARNING,
                "severity must be fixed vocabulary, found {:?}",
                finding.severity
            );
            assert!(
                !finding.path.starts_with('/') && !finding.path.contains('\\'),
                "paths stay workspace-relative with forward slashes, found {:?}",
                finding.path
            );
            assert!(finding.line >= 1, "lines stay 1-based");
        }
        with_cleanup(&root);
    }

    #[test]
    fn excerpts_cap_lines_and_width() {
        let root = test_root();
        let long_tail = "x".repeat(MAX_EXCERPT_CHARS + 100);
        write_file(
            &root,
            "src/long.rs",
            &format!("pub fn padded() {{\n    let value = {long_tail}.unwrap();\n}}\n"),
        );
        let report = audit_workspace(&root).expect("audit runs");
        let hits = of_kind(&report, KIND_UNWRAP);
        assert_eq!(hits.len(), 1, "exactly one unwrap hotspot, {hits:?}");
        let excerpt_lines: Vec<&str> = hits[0].excerpt.split('\n').collect();
        assert!(
            excerpt_lines.len() <= MAX_EXCERPT_LINES,
            "excerpts cap lines, found {}",
            excerpt_lines.len()
        );
        for line in excerpt_lines {
            assert!(
                line.chars().count() <= MAX_EXCERPT_CHARS,
                "excerpt lines cap width at {MAX_EXCERPT_CHARS} chars"
            );
        }
        with_cleanup(&root);
    }

    #[test]
    fn dead_code_candidate_marks_never_used_pub_only() {
        let root = test_root();
        write_file(
            &root,
            "src/lib.rs",
            "/// Served entry.\npub fn served_handler() {}\n/// Orphaned entry.\npub fn orphaned_handler() {}\n",
        );
        write_file(
            &root,
            "src/main.rs",
            "fn main() {\n    served_handler();\n}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        let dead = of_kind(&report, KIND_DEAD_CODE);
        assert!(
            dead.iter()
                .any(|finding| finding.excerpt.contains("orphaned_handler")),
            "the never-used pub item is a candidate, found {dead:?}"
        );
        assert!(
            !dead
                .iter()
                .any(|finding| finding.excerpt.contains("served_handler")),
            "the used pub item is not a candidate, found {dead:?}"
        );
        with_cleanup(&root);
    }

    #[test]
    fn missing_docs_flags_undocumented_pub_fn() {
        let root = test_root();
        write_file(
            &root,
            "src/lib.rs",
            "/// Documented entry.\npub fn documented_api() {}\n\npub fn bare_api() {}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        let missing = of_kind(&report, KIND_MISSING_DOCS);
        assert!(
            missing
                .iter()
                .any(|finding| finding.excerpt.contains("bare_api")),
            "the undocumented pub fn is flagged, found {missing:?}"
        );
        assert!(
            !missing
                .iter()
                .any(|finding| finding.excerpt.contains("documented_api")),
            "the documented pub fn is not flagged, found {missing:?}"
        );
        assert!(
            missing
                .iter()
                .all(|finding| finding.severity == SEVERITY_INFO),
            "missing docs inform, never warn"
        );
        with_cleanup(&root);
    }

    #[test]
    fn todo_and_oversized_file_detected() {
        let root = test_root();
        write_file(
            &root,
            "src/small.rs",
            "// TODO: revisit this\nfn tiny() {}\n",
        );
        let mut big = String::new();
        for _ in 0..(LARGE_FILE_LINES + 5) {
            big.push_str("// filler line\n");
        }
        write_file(&root, "src/big.rs", &big);
        let report = audit_workspace(&root).expect("audit runs");
        assert_eq!(of_kind(&report, KIND_TODO).len(), 1);
        let large = of_kind(&report, KIND_LARGE_FILE);
        assert_eq!(large.len(), 1, "one oversized file, {large:?}");
        assert_eq!(large[0].line, 1);
        with_cleanup(&root);
    }

    #[test]
    fn oversized_function_detected_with_brace_tracking() {
        let root = test_root();
        let mut body = String::from("/// Big entry.\npub fn big_api() {\n");
        for _ in 0..(LARGE_FUNCTION_LINES + 10) {
            body.push_str("    let accounted = 1;\n");
        }
        body.push_str("}\n/// Small entry.\npub fn small_api() {\n}\n");
        write_file(&root, "src/functions.rs", &body);
        let report = audit_workspace(&root).expect("audit runs");
        let large = of_kind(&report, KIND_LARGE_FN);
        assert_eq!(large.len(), 1, "only the long function, {large:?}");
        assert_eq!(large[0].line, 2, "the finding points at the fn line");
        with_cleanup(&root);
    }

    #[test]
    fn clone_density_reports_one_finding_per_file() {
        let root = test_root();
        let mut body = String::from("/// Cloning entry.\npub fn cloning_api(items: &[String]) {\n");
        for _ in 0..=CLONE_DENSITY {
            body.push_str("    let _ = items.to_vec().clone();\n");
        }
        body.push_str("}\n");
        write_file(&root, "src/clones.rs", &body);
        let report = audit_workspace(&root).expect("audit runs");
        let clones = of_kind(&report, KIND_CLONE);
        assert_eq!(clones.len(), 1, "one density finding per file, {clones:?}");
        assert_eq!(clones[0].severity, SEVERITY_WARNING);
        with_cleanup(&root);
    }

    #[test]
    fn ts_bug_hunter_shapes_detected() {
        let root = test_root();
        write_file(
            &root,
            "src/panel.ts",
            "export function render(input: unknown): string {\n  // @ts-ignore: legacy shape\n  const loose = input as any;\n  return String(loose);\n}\n",
        );
        write_file(
            &root,
            "src/swallow.rs",
            "/// Swallow entry.\npub fn swallow_api() {\n    let _ = fallible();\n    let kept = fallible().ok();\n    let sank = fallible().unwrap_or_default();\n    let burst = required().unwrap();\n    let trusted = required().expect(\"local invariant\");\n}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        assert!(
            !of_kind(&report, KIND_UNCHECKED).is_empty(),
            "ts suppressions flag"
        );
        assert_eq!(
            of_kind(&report, KIND_SWALLOWED).len(),
            3,
            "let-_/ok/default"
        );
        assert_eq!(of_kind(&report, KIND_UNWRAP).len(), 2, "unwrap + expect");
        with_cleanup(&root);
    }

    #[test]
    fn too_large_file_is_a_skip_notice() {
        let root = test_root();
        let padding = "x".repeat(MAX_FILE_BYTES + 1);
        write_file(&root, "src/huge.rs", &padding);
        write_file(&root, "src/ok.rs", "fn ok() {}\n");
        let report = audit_workspace(&root).expect("audit runs");
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.files_skipped, 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].reason, SKIP_TOO_LARGE);
        assert_eq!(report.skipped_overflow, 0);
        with_cleanup(&root);
    }

    #[test]
    fn skip_and_finding_lists_cap_with_overflow() {
        let mut skipped = Vec::new();
        let mut skipped_overflow = 0;
        for index in 0..(MAX_SKIPPED_LISTED + 7) {
            push_skipped(
                &mut skipped,
                &mut skipped_overflow,
                format!("src/file{index}.rs"),
                SKIP_FILE_CAP,
            );
        }
        assert_eq!(skipped.len(), MAX_SKIPPED_LISTED);
        assert_eq!(skipped_overflow, 7);
        // Findings collect uncapped; the cap applies once, on the sorted set.
        let mut findings = Vec::new();
        for index in (0..(MAX_FINDINGS + 3)).rev() {
            push_finding(
                &mut findings,
                KIND_TODO,
                "src/todo.rs",
                index + 1,
                String::new(),
            );
        }
        let overflow = cap_findings(&mut findings);
        assert_eq!(findings.len(), MAX_FINDINGS);
        assert_eq!(overflow, 3);
        assert!(
            findings.windows(2).all(|pair| {
                (pair[0].path.clone(), pair[0].line, pair[0].kind.clone())
                    <= (pair[1].path.clone(), pair[1].line, pair[1].kind.clone())
            }),
            "kept findings stay sorted by (path, line, kind)"
        );
        assert_eq!(findings[0].line, 1, "truncation keeps the sorted head");
    }

    #[test]
    fn late_dead_code_survives_truncation_on_merit() {
        // Regression: the cap used to fill at push time, so pass-2 line
        // findings starved pass-3 dead-code candidates. Uncapped collection
        // plus sort-then-truncate keeps the sorted head, whatever the pass.
        let root = test_root();
        let mut filler = String::new();
        for _ in 0..MAX_FINDINGS {
            filler.push_str("// TODO: filler debt\n");
        }
        write_file(&root, "mmm/filler.rs", &filler);
        write_file(
            &root,
            "aaa/orphan.rs",
            "/// Orphaned entry.\npub fn orphaned_tail_api() {}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        // 2000 filler TODOs + 1 oversized-file + 1 dead-code candidate.
        assert_eq!(report.findings.len(), MAX_FINDINGS);
        assert_eq!(report.findings_overflow, 2);
        assert!(
            of_kind(&report, KIND_DEAD_CODE)
                .iter()
                .any(|finding| finding.excerpt.contains("orphaned_tail_api")),
            "the late dead-code candidate sorts into the kept head, {report:?}"
        );
        with_cleanup(&root);
    }

    #[test]
    fn unwrap_in_test_code_is_not_a_hotspot() {
        let root = test_root();
        write_file(
            &root,
            "src/live.rs",
            "/// Live entry.\npub fn live_api() {\n    let burst = required().unwrap();\n}\n",
        );
        write_file(
            &root,
            "tests/integration.rs",
            "fn fetch() {\n    let burst = required().unwrap();\n    let trusted = required().expect(\"local invariant\");\n}\n",
        );
        write_file(
            &root,
            "src/lib.rs",
            "#[cfg(test)]\nmod checks {\n    fn graded() {\n        let burst = required().unwrap();\n    }\n}\n/// Live helper.\npub fn helper_api() {}\n",
        );
        let report = audit_workspace(&root).expect("audit runs");
        let hotspots = of_kind(&report, KIND_UNWRAP);
        assert_eq!(
            hotspots.len(),
            1,
            "only production unwrap flags, {hotspots:?}"
        );
        assert_eq!(hotspots[0].path, "src/live.rs");
        with_cleanup(&root);
    }

    #[test]
    fn test_source_paths_cover_common_layouts() {
        for rel in [
            "tests/integration.rs",
            "src/test/helpers.rs",
            "src/parser_test.rs",
            "src/test_parser.rs",
            "src/tests.rs",
            "src/panel.test.ts",
            "src/panel.spec.tsx",
        ] {
            assert!(is_test_source_path(rel), "{rel:?} reads as test code");
        }
        for rel in [
            "src/lib.rs",
            "src/contest.rs",
            "src/latest.rs",
            "src/panel.ts",
            "src/testing_utils.rs",
        ] {
            assert!(
                !is_test_source_path(rel),
                "{rel:?} reads as production code"
            );
        }
    }

    #[test]
    fn doc_check_handles_fn_modifiers_but_ignores_restricted_pub() {
        for line in [
            "pub fn bare_api() {}",
            "pub async fn bare_api() {}",
            "pub unsafe fn bare_api() {}",
            "pub async unsafe fn bare_api() {}",
            "pub unsafe async fn bare_api() {}",
            "pub const fn bare_api() {}",
            "pub async const fn bare_api() {}",
            "pub extern \"C\" fn bare_api() {}",
        ] {
            assert!(rs_doc_item(line), "{line:?} needs a doc comment");
        }
        for line in [
            "pub(crate) fn hidden_api() {}",
            "pub(super) fn hidden_api() {}",
            "pub const LIMIT: usize = 1;",
            "pub static FLAG: bool = true;",
            "fn private_api() {}",
        ] {
            assert!(!rs_doc_item(line), "{line:?} is out of doc scope");
        }
    }

    #[test]
    fn invalid_root_is_secret_free() {
        let missing = std::env::temp_dir().join("nexora-repo-audit-test-missing-dir-xyz");
        let err = audit_workspace(&missing).expect_err("a missing root must fail");
        assert_eq!(err, RepoAuditError::InvalidRoot);
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains("missing-dir-xyz"),
            "errors carry no paths, rendered {rendered:?}"
        );
    }

    #[test]
    fn non_source_files_are_ignored() {
        let root = test_root();
        write_file(&root, "notes.md", "# TODO: not code\n");
        write_file(&root, "data.json", "{\"todo\": true}\n");
        let report = audit_workspace(&root).expect("audit runs");
        assert_eq!(report.files_scanned, 0);
        assert!(report.findings.is_empty());
        with_cleanup(&root);
    }

    #[test]
    fn scan_is_read_only() {
        let root = test_root();
        write_file(&root, "src/lib.rs", "/// Entry.\npub fn steady_api() {}\n");
        let before: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
            .expect("src lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        let content_before = std::fs::read(root.join("src/lib.rs")).expect("content reads");
        let report = audit_workspace(&root).expect("audit runs");
        assert_eq!(report.files_scanned, 1);
        let after: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
            .expect("src lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(before, after, "the scan creates no files");
        assert_eq!(
            std::fs::read(root.join("src/lib.rs")).expect("content reads"),
            content_before,
            "the scan modifies no files"
        );
        with_cleanup(&root);
    }

    #[test]
    fn word_occurrences_respect_identifier_boundaries() {
        assert_eq!(count_word_occurrences("foo foobar foo_bar foo", "foo"), 2);
        assert_eq!(count_word_occurrences("foo", "foo"), 1);
        assert_eq!(count_word_occurrences("foobar", "foo"), 0);
        assert_eq!(count_word_occurrences("", "foo"), 0);
        assert_eq!(count_word_occurrences("foo foo foo", ""), 0);
    }
}
