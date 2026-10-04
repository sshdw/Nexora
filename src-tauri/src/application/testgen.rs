//! Test-generation drafts: template-generated Rust `#[test]` scaffolds for
//! untested public functions detected by the repo audit engine.
//!
//! Substrate decision (Phase 1, binding — no new dependencies, no agent
//! spend): **template-based scaffold generation locally**, not
//! agent-generated via the existing run path
//! ([`crate::application::agent::service::start_run`]). Rationale: drafts are
//! a deterministic function of the audit finding's declaration line (name +
//! parameter names + return shape) plus honest `TODO` markers where intent is
//! unknowable — an LLM run would add latency, approval/budget surface, and
//! hallucinated intent for zero structural gain. No generation on this path
//! is an agent run, so the approval/budget/spend gates are not bypassed —
//! they are simply never entered. If a future slice wants agent-written test
//! bodies, every generation must go through the ordinary run path with its
//! gates intact.
//!
//! Audit-kind map (the only finding kinds usable as testgen targets):
//! - `missing-docs` ([`KIND_MISSING_DOCS`]) — TARGET: the excerpt is the
//!   undocumented `pub` declaration line itself, i.e. a public item with no
//!   doc comment, a classic untested-surface signal.
//! - `dead-code-candidate` ([`KIND_DEAD_CODE`]) — TARGET: the excerpt is the
//!   never-used `pub` declaration line — untested by construction (nothing
//!   calls it, not even tests).
//! - `unwrap-hotspot`, `error-swallowed`, `unchecked-result`,
//!   `suspicious-clone` — NOT targets: excerpts are call-site lines inside
//!   function bodies, not declarations; no name/signature to scaffold from.
//! - `todo-debt` — NOT a target: a debt marker, not a function.
//! - `oversized-file`, `oversized-function` — NOT targets: size signals, not
//!   declarations (the `oversized-function` excerpt is a `fn` line, but the
//!   item may be private and already tested; size alone is not an untested
//!   signal, so it stays out rather than noise up the draft list).
//! - TypeScript findings — NOT targets: TS test generation is out of scope
//!   (Rust only); only `.rs` findings are eligible.
//! - Test-code paths ([`is_test_source_path`]) — NOT targets: scaffolding
//!   tests for tests is noise.
//!
//! Read-only contract: this module only reads via
//! [`audit_workspace`] (which itself only reads directory entries and file
//! bytes); drafts are returned as response data and never written to source
//! files. The user copies them manually — applying is a separate slice.
//! Drafts carry at most the capped declaration excerpt (signatures only, no
//! secrets — see the audit module docs), truncated to [`MAX_DRAFT_CHARS`].
//!
//! Caps: at most [`MAX_TESTGEN_DRAFTS`] drafts per run; the remainder is
//! reported as `drafts_overflow`, never silently dropped.

use std::path::Path;

use serde::Serialize;

use super::repo_audit::{
    audit_workspace, is_test_source_path, RepoAuditError, RepoAuditReport, KIND_DEAD_CODE,
    KIND_MISSING_DOCS,
};

/// Most draft scaffolds returned per run; the remainder is reported in
/// `drafts_overflow`.
pub(crate) const MAX_TESTGEN_DRAFTS: usize = 20;

/// Most characters carried per draft `code` (char-boundary safe truncation).
pub(crate) const MAX_DRAFT_CHARS: usize = 2000;

/// Audit finding kinds usable as testgen targets: declaration-line findings
/// for public Rust items (see the module docs for the full kind map).
pub(crate) const TESTGEN_SOURCE_KINDS: [&str; 2] = [KIND_MISSING_DOCS, KIND_DEAD_CODE];

/// One draft test scaffold: review-buffer data only, never written to disk.
/// The `code` is an arrange/act/assert skeleton derived from the signature
/// with honest `TODO` markers everywhere intent is unknowable statically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TestDraft {
    /// Workspace-relative path with forward slashes (never absolute).
    pub path: String,
    /// 1-based line number of the target declaration.
    pub line: usize,
    /// Bare function name the scaffold tests (`test_<fn_name>`).
    pub fn_name: String,
    /// The audit finding kind this draft was derived from (one of
    /// [`TESTGEN_SOURCE_KINDS`]).
    pub source_kind: String,
    /// The capped declaration line the scaffold was derived from.
    pub signature: String,
    /// The draft `#[test]` scaffold (capped at [`MAX_DRAFT_CHARS`]).
    pub code: String,
}

/// One read-only testgen run: capped drafts plus generation accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TestgenReport {
    /// Draft scaffolds in audit order (`path`, `line`), capped at
    /// [`MAX_TESTGEN_DRAFTS`].
    pub drafts: Vec<TestDraft>,
    /// Eligible targets omitted by the cap (0 when everything fit).
    pub drafts_overflow: usize,
    /// Eligible findings considered (pre-cap, post-dedupe by `path:line`).
    pub targets_considered: usize,
    /// Source files fully scanned by the underlying audit.
    pub files_scanned: usize,
}

/// Parsed declaration shape: bare name, parameter list (raw text between the
/// outer parens, if the excerpt carries a closing paren), and return type
/// (raw text after `->`, if present). Everything is best-effort substring
/// work on the capped excerpt line — never name resolution.
struct ParsedSig {
    name: String,
    params: Option<String>,
    returns: Option<String>,
}

/// Leading identifier of `rest` (ASCII word characters only).
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

/// Parse `fn <name>(<params>) [-> <ret>]` out of a declaration excerpt.
/// Returns `None` when no `fn <name>` shape is present (not a function
/// declaration — e.g. a `pub struct` dead-code hit).
#[must_use]
fn parse_sig(excerpt: &str) -> Option<ParsedSig> {
    let marker = excerpt.find("fn ")?;
    let name = leading_ident(excerpt[marker + 3..].trim_start())?;
    let after_name = &excerpt[marker + 3..];
    let open = after_name.find('(')?;
    let after_open = &after_name[open + 1..];
    // Flat first-`)` cut: nested parens (fn-pointer params) truncate the
    // list, and a truncated excerpt may carry no `)` at all. Both cases
    // surface as `params: None` — the scaffold then says so honestly.
    let params = after_open
        .find(')')
        .map(|close| after_open[..close].to_string());
    let returns = after_open.find(')').and_then(|close| {
        let tail = after_open[close + 1..].trim_start();
        let tail = tail.strip_prefix("->")?;
        let tail = tail.trim_start();
        let end = tail
            .find(['{', ';', '/'])
            .or_else(|| tail.find(" where "))
            .unwrap_or(tail.len());
        let ret = tail[..end].trim();
        if ret.is_empty() {
            None
        } else {
            Some(ret.to_string())
        }
    });
    Some(ParsedSig {
        name,
        params,
        returns,
    })
}

/// Sanitize a function name into a `test_<name>` identifier (non-word
/// characters become `_`; the audit names are already identifiers, so this
/// is defensive).
#[must_use]
fn test_fn_name(name: &str) -> String {
    let mut safe = String::from("test_");
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            safe.push(ch);
        } else {
            safe.push('_');
        }
    }
    safe
}

/// Parameter names usable as call arguments: the identifier before each
/// top-level `:` (destructuring patterns and missing names fall back to a
/// `todo!()` placeholder at the call site — never an invented binding).
fn arg_placeholders(params: &str) -> Vec<String> {
    params
        .split(',')
        .map(str::trim)
        .filter(|param| !param.is_empty())
        .map(|param| {
            let bare = param
                .trim_start_matches(['&', ' '])
                .trim_start_matches("mut ")
                .trim();
            if bare == "self" {
                "todo!(\"provide receiver\")".to_string()
            } else if let Some((name, _)) = bare.split_once(':') {
                let name = name.trim();
                if leading_ident(name).is_some_and(|ident| ident == name) {
                    name.to_string()
                } else {
                    "todo!(\"provide argument\")".to_string()
                }
            } else {
                "todo!(\"provide argument\")".to_string()
            }
        })
        .collect()
}

/// Truncate draft code to [`MAX_DRAFT_CHARS`] characters (char-boundary
/// safe — byte slicing could split multi-byte text).
#[must_use]
fn truncate_code(code: &str) -> String {
    if code.chars().count() <= MAX_DRAFT_CHARS {
        code.to_string()
    } else {
        code.chars().take(MAX_DRAFT_CHARS).collect()
    }
}

/// Build the arrange/act/assert scaffold for one parsed signature. Every
/// unknowable — input values, expected outcome — is a `TODO`/`todo!()`
/// marker; the scaffold documents the call shape and nothing more.
#[must_use]
fn scaffold(path: &str, line: usize, signature: &str, parsed: &ParsedSig) -> String {
    use std::fmt::Write as _;
    let test_name = test_fn_name(&parsed.name);
    let mut code = format!(
        "#[test]\nfn {test_name}() {{\n    // DRAFT scaffold for `{path}:{line}` (`{signature}`).\n    // Template-generated locally from the signature only — no agent run,\n    // no budget spent. Intent is unknowable statically: fill in the TODOs,\n    // review, then copy into a test module manually (nothing is written).\n"
    );
    match parsed.params.as_deref().map(str::trim) {
        None | Some("") => {
            code.push_str("    // TODO: picks up no inputs — the target takes no arguments.\n");
            if parsed.returns.is_some() {
                let _ = writeln!(
                    code,
                    "    let result = {}();\n    // TODO: assert the observable outcome, e.g. `assert_eq!(result, ...)`.\n    let _ = result;",
                    parsed.name
                );
            } else {
                let _ = writeln!(
                    code,
                    "    {}();\n    // TODO: assert the observable outcome (state change, return value, ...).",
                    parsed.name
                );
            }
        }
        Some(params) => {
            let args = arg_placeholders(params);
            let _ = writeln!(
                code,
                "    // TODO: replace each placeholder with a real input for ({params})."
            );
            let call = format!("{}({})", parsed.name, args.join(", "));
            if parsed.returns.is_some() {
                let _ = writeln!(
                    code,
                    "    let result = {call};\n    // TODO: assert the observable outcome, e.g. `assert_eq!(result, ...)`.\n    let _ = result;"
                );
            } else {
                let _ = writeln!(
                    code,
                    "    {call};\n    // TODO: assert the observable outcome (state change, ...)."
                );
            }
            if args.iter().any(|arg| arg.starts_with("todo!")) {
                code.push_str(
                    "    // NOTE: `todo!()` placeholders panic — the draft fails until filled in.\n",
                );
            }
        }
    }
    if parsed.params.is_none() {
        code.push_str(
            "    // NOTE: the source excerpt carries no complete parameter list\n    // (truncated line or unusual shape) — reconstruct the call by hand.\n",
        );
    }
    code.push_str("}\n");
    truncate_code(&code)
}

/// Whether one audit finding is an eligible testgen target (see the module
/// docs for the kind map): a `TESTGEN_SOURCE_KINDS` finding on a Rust
/// non-test path whose excerpt carries a `fn` declaration.
#[must_use]
fn is_target(path: &str, excerpt: &str, kind: &str) -> bool {
    TESTGEN_SOURCE_KINDS.contains(&kind)
        && Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        && !is_test_source_path(path)
        && excerpt.contains("fn ")
}

/// Derive draft scaffolds from an audit report's findings (audit order is
/// already `(path, line, kind)`; dedupe keeps the first hit per
/// `path:line`, e.g. an item that is both undocumented and unused).
#[must_use]
pub(crate) fn drafts_for_report(report: &RepoAuditReport) -> TestgenReport {
    let mut drafts: Vec<TestDraft> = Vec::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    for finding in &report.findings {
        if !is_target(&finding.path, &finding.excerpt, &finding.kind) {
            continue;
        }
        // Declaration excerpts may span lines (`missing-docs` carries up to
        // 3 source lines): the scaffold anchors on the declaration line
        // only, so embedded newlines can never break the draft's comments.
        let decl_line = finding.excerpt.lines().next().unwrap_or(&finding.excerpt);
        let Some(parsed) = parse_sig(decl_line) else {
            continue;
        };
        let key = (finding.path.clone(), finding.line);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let code = scaffold(&finding.path, finding.line, decl_line, &parsed);
        drafts.push(TestDraft {
            path: finding.path.clone(),
            line: finding.line,
            fn_name: parsed.name,
            source_kind: finding.kind.clone(),
            signature: (*decl_line).to_string(),
            code,
        });
    }
    let targets_considered = seen.len();
    let drafts_overflow = targets_considered.saturating_sub(MAX_TESTGEN_DRAFTS);
    drafts.truncate(MAX_TESTGEN_DRAFTS);
    TestgenReport {
        drafts,
        drafts_overflow,
        targets_considered,
        files_scanned: report.files_scanned,
    }
}

/// Run the read-only audit over the workspace `root`, then derive draft
/// test scaffolds from its findings. Reads only; drafts are response data.
///
/// # Errors
///
/// Returns [`RepoAuditError::InvalidRoot`] when `root` is not a directory and
/// [`RepoAuditError::Io`] when the root cannot be listed. Unreadable files
/// below the root are audit skip notices, never errors.
pub(crate) fn generate_testgen_report(root: &Path) -> Result<TestgenReport, RepoAuditError> {
    audit_workspace(root).map(|report| drafts_for_report(&report))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::application::repo_audit::AuditFinding;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("nexora-testgen-test-{}-{id}", std::process::id()));
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

    fn finding(kind: &str, path: &str, line: usize, excerpt: &str) -> AuditFinding {
        AuditFinding {
            kind: kind.to_string(),
            severity: "info".to_string(),
            path: path.to_string(),
            line,
            excerpt: excerpt.to_string(),
        }
    }

    fn report_of(findings: Vec<AuditFinding>) -> RepoAuditReport {
        RepoAuditReport {
            findings,
            findings_overflow: 0,
            files_scanned: 1,
            files_skipped: 0,
            skipped: Vec::new(),
            skipped_overflow: 0,
        }
    }

    #[test]
    fn only_declaration_kinds_become_targets() {
        let report = report_of(vec![
            finding(KIND_MISSING_DOCS, "src/lib.rs", 10, "pub fn bare_api() {}"),
            finding(KIND_DEAD_CODE, "src/lib.rs", 20, "pub fn orphaned_api() {}"),
            finding(
                "unwrap-hotspot",
                "src/lib.rs",
                30,
                "let x = fallible().unwrap();",
            ),
            finding("error-swallowed", "src/lib.rs", 40, "let _ = fallible();"),
            finding("unchecked-result", "src/lib.rs", 50, "#[allow(dead_code)]"),
            finding(
                "suspicious-clone",
                "src/lib.rs",
                60,
                "let c = items.clone();",
            ),
            finding("todo-debt", "src/lib.rs", 70, "// TODO: revisit"),
            finding("oversized-file", "src/lib.rs", 1, "pub fn big() {"),
            finding("oversized-function", "src/lib.rs", 80, "fn huge_api() {"),
            finding(
                KIND_MISSING_DOCS,
                "src/panel.ts",
                5,
                "export function render() {",
            ),
        ]);
        let testgen = drafts_for_report(&report);
        let targets: Vec<&str> = testgen
            .drafts
            .iter()
            .map(|draft| draft.fn_name.as_str())
            .collect();
        assert_eq!(
            targets,
            vec!["bare_api", "orphaned_api"],
            "only missing-docs + dead-code Rust fn declarations scaffold, got {targets:?}"
        );
        assert_eq!(testgen.targets_considered, 2);
        assert_eq!(testgen.drafts_overflow, 0);
    }

    #[test]
    fn test_sources_and_non_fn_decls_are_not_targets() {
        let report = report_of(vec![
            finding(
                KIND_MISSING_DOCS,
                "tests/integration.rs",
                3,
                "pub fn helper_api() {}",
            ),
            finding(
                KIND_MISSING_DOCS,
                "src/parser_test.rs",
                3,
                "pub fn helper_api() {}",
            ),
            finding(
                KIND_MISSING_DOCS,
                "src/lib.rs",
                3,
                "pub struct BareStruct {}",
            ),
            finding(
                KIND_MISSING_DOCS,
                "src/lib.rs",
                4,
                "pub const LIMIT: usize = 1;",
            ),
            finding(
                KIND_DEAD_CODE,
                "src/panel.ts",
                5,
                "export function render() {",
            ),
        ]);
        let testgen = drafts_for_report(&report);
        assert!(
            testgen.drafts.is_empty(),
            "test paths, non-fn items, and TS findings scaffold nothing, got {:?}",
            testgen.drafts
        );
        assert_eq!(testgen.targets_considered, 0);
    }

    #[test]
    fn scaffold_carries_skeleton_and_honest_todos() {
        let report = report_of(vec![finding(
            KIND_MISSING_DOCS,
            "src/math.rs",
            12,
            "pub fn add(left: i32, right: i32) -> i32 {",
        )]);
        let testgen = drafts_for_report(&report);
        assert_eq!(testgen.drafts.len(), 1);
        let draft = &testgen.drafts[0];
        assert_eq!(draft.fn_name, "add");
        assert_eq!(draft.source_kind, KIND_MISSING_DOCS);
        assert_eq!(draft.path, "src/math.rs");
        assert_eq!(draft.line, 12);
        assert!(
            draft.code.contains("fn test_add()"),
            "scaffold names the test, got:\n{}",
            draft.code
        );
        assert!(
            draft.code.contains("add(left, right)"),
            "call shape preserved, got:\n{}",
            draft.code
        );
        assert!(draft.code.contains("TODO"), "honest TODO markers present");
        assert!(
            draft.code.contains("let result = "),
            "return value bound for assertion"
        );
        assert!(
            draft.code.chars().count() <= MAX_DRAFT_CHARS,
            "draft respects the size cap"
        );
    }

    #[test]
    fn scaffold_handles_empty_params_and_no_return() {
        let report = report_of(vec![finding(
            KIND_DEAD_CODE,
            "src/live.rs",
            4,
            "pub fn reset_cache() {",
        )]);
        let testgen = drafts_for_report(&report);
        assert_eq!(testgen.drafts.len(), 1);
        let code = &testgen.drafts[0].code;
        assert!(
            code.contains("reset_cache();"),
            "nullary call scaffolded, got:\n{code}"
        );
        assert!(
            code.contains("TODO"),
            "assertion TODO present even without a return"
        );
    }

    #[test]
    fn truncated_signature_scaffolds_honestly() {
        // A 160-char-capped excerpt can cut the parameter list mid-shape
        // (no closing paren): the draft must say so, not invent arguments.
        let report = report_of(vec![finding(
            KIND_MISSING_DOCS,
            "src/wide.rs",
            7,
            "pub fn configure(endpoint: &str, retries: u32, timeout_ms: u64, backoff: Backoff, tls: Tls",
        )]);
        let testgen = drafts_for_report(&report);
        assert_eq!(testgen.drafts.len(), 1);
        let code = &testgen.drafts[0].code;
        assert!(code.contains("TODO"), "TODO markers present, got:\n{code}");
        assert!(
            code.contains("no complete parameter list"),
            "the draft admits the unknown shape, got:\n{code}"
        );
    }

    #[test]
    fn same_line_dedupes_and_drafts_cap_with_overflow() {
        let mut findings = vec![
            finding(
                KIND_MISSING_DOCS,
                "src/lib.rs",
                10,
                "pub fn shared_api() {}",
            ),
            finding(KIND_DEAD_CODE, "src/lib.rs", 10, "pub fn shared_api() {}"),
        ];
        for index in 0..(MAX_TESTGEN_DRAFTS + 5) {
            findings.push(finding(
                KIND_MISSING_DOCS,
                &format!("src/mod{index}.rs"),
                2,
                &format!("pub fn target_{index}() {{"),
            ));
        }
        let testgen = drafts_for_report(&report_of(findings));
        // 1 shared line + 25 unique = 26 considered; 20 kept, 6 overflow.
        assert_eq!(testgen.targets_considered, MAX_TESTGEN_DRAFTS + 6);
        assert_eq!(testgen.drafts.len(), MAX_TESTGEN_DRAFTS);
        assert_eq!(testgen.drafts_overflow, 6);
        assert_eq!(testgen.drafts[0].fn_name, "shared_api");
        assert_eq!(testgen.drafts[0].path, "src/lib.rs");
    }

    #[test]
    fn multiline_excerpt_anchors_on_declaration_line() {
        // `missing-docs` excerpts carry up to 3 source lines: the draft must
        // anchor on the declaration line so trailing lines never leak into
        // the scaffold as uncommented code.
        let report = report_of(vec![finding(
            KIND_MISSING_DOCS,
            "src/lib.rs",
            37,
            "pub fn run() {\n    // Initialize logging first.\n    infrastructure::logging::init();",
        )]);
        let testgen = drafts_for_report(&report);
        assert_eq!(testgen.drafts.len(), 1);
        let draft = &testgen.drafts[0];
        assert_eq!(draft.signature, "pub fn run() {");
        assert!(
            !draft.code.contains("Initialize logging"),
            "trailing excerpt lines stay out of the draft, got:\n{}",
            draft.code
        );
        assert!(draft.code.contains("fn test_run()"));
    }

    #[test]
    fn generation_is_read_only() {
        let root = test_root();
        write_file(
            &root,
            "src/lib.rs",
            "pub fn bare_api(value: i32) -> i32 {\n    value\n}\n",
        );
        let before: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
            .expect("src lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        let content_before = std::fs::read(root.join("src/lib.rs")).expect("content reads");
        let report = generate_testgen_report(&root).expect("testgen runs");
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.targets_considered, 1, "the bare pub fn is a target");
        assert_eq!(report.drafts.len(), 1);
        assert_eq!(report.drafts[0].fn_name, "bare_api");
        let after: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
            .expect("src lists")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(before, after, "generation creates no files");
        assert_eq!(
            std::fs::read(root.join("src/lib.rs")).expect("content reads"),
            content_before,
            "generation modifies no files"
        );
        with_cleanup(&root);
    }

    #[test]
    fn drafts_are_secret_free_and_signature_only() {
        const SECRET_SENTINELS: [&str; 4] = ["sk-", "secret", "credential", "api_key"];
        let report = report_of(vec![finding(
            KIND_MISSING_DOCS,
            "src/keys.rs",
            9,
            "pub fn rotate_key(old: &str) -> String {",
        )]);
        let testgen = drafts_for_report(&report);
        assert_eq!(testgen.drafts.len(), 1);
        let draft = &testgen.drafts[0];
        for field in [&draft.signature, &draft.code] {
            for sentinel in SECRET_SENTINELS {
                assert!(
                    !field.to_lowercase().contains(sentinel),
                    "drafts carry signatures only, found {sentinel:?} in {field:?}"
                );
            }
        }
    }

    /// Static write-guard: the engine stays a pure workspace read — no
    /// writes and no fix/test application path. Needles are built with
    /// `concat!` so this test's own source never matches them verbatim.
    /// (No process-spawn needle here: the test fixtures name temp
    /// directories with the process id, exactly like the audit engine's own
    /// scaffolding — the command layer below carries that needle instead.)
    #[test]
    fn testgen_engine_stays_a_pure_read_without_write_application() {
        const SOURCE: &str = include_str!("testgen.rs");
        assert!(
            SOURCE.contains("audit_workspace("),
            "testgen must reuse the audit engine detection"
        );
        for needle in [
            concat!("fs", "::write"),
            concat!("File", "::create"),
            concat!("apply", "_fix"),
            concat!("auto", "_fix"),
            concat!("write", "_test"),
        ] {
            assert!(
                !SOURCE.contains(needle),
                "application/testgen.rs must never write, found {needle:?}"
            );
        }
        // NOTE: no directory-creation needle here: the test fixtures build
        // temp directories under `#[cfg(test)]`, exactly like the audit
        // engine's own scaffolding (`application/repo_audit.rs` tests) — the
        // production path above takes no filesystem-write shape at all.
    }
}
