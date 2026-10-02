//! Per-workspace Nexora home: the `.nexora/` project directory contract.
//!
//! Every agent workspace may carry its own `.nexora/` directory (resolved
//! from the canonical workspace root owned by [`crate::application::workspace`]):
//! a `nexora.json` manifest with a forward-only `version` field, a
//! `.nexoraignore` file with workspace-relative ignore rules, and a
//! `profiles/` scaffold for workspace-scoped profile documents.
//!
//! Contract notes (no business logic moves yet):
//!
//! - `.nexora/` is located from the workspace root and created on explicit
//!   init only ([`init_nexora_dir`], idempotent). It is NOT a second settings
//!   store: the existing settings store keeps app-global keys, while
//!   `.nexora/` holds workspace-scoped files. Where both exist, the
//!   workspace-scoped file takes precedence over the global key.
//! - Every write under `.nexora/` goes through the canonical guard pattern
//!   from the workspace tools (lexical prefix check, `symlink_metadata`
//!   prefix walk, canonicalize-after-create, post-write backstop), reusing
//!   [`crate::application::agent::tools::is_within_workspace`].
//! - Manifest versions are forward-only: a manifest newer than
//!   [`MANIFEST_VERSION`] refuses with [`ProjectDirError::UnsupportedVersion`],
//!   the same downgrade-refuses philosophy as the database.
//! - `.nexora/` itself is always excluded from agent tool reads, and
//!   `.nexoraignore` rules are enforced in `list_directory`, the
//!   `search_files` scope and walk, and `read_file` (fixed, content-free
//!   errors). All failures here are secret-free: fixed category text that
//!   never echoes file content, credentials, SQL, or payloads.
//! - Profile documents reuse the routing profile shape
//!   ([`crate::application::routing::RoutingProfile::from_json`], the
//!   parse-then-validate single source); nothing is duplicated and no profile
//!   logic moves yet.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::application::agent::tools::is_within_workspace;
use crate::application::routing::{RoutingError, RoutingProfile};
use crate::application::workspace::strip_verbatim;

/// Project directory name, joined onto the canonical workspace root.
pub(crate) const NEXORA_DIR_NAME: &str = ".nexora";

/// Manifest file inside [`.nexora/`](NEXORA_DIR_NAME), carrying the version field.
pub(crate) const MANIFEST_FILE_NAME: &str = "nexora.json";

/// Gitignore-style ignore file inside [`.nexora/`](NEXORA_DIR_NAME).
pub(crate) const IGNORE_FILE_NAME: &str = ".nexoraignore";

/// Profile scaffold directory inside [`.nexora/`](NEXORA_DIR_NAME).
pub(crate) const PROFILES_DIR_NAME: &str = "profiles";

/// Current manifest version. Older manifests are accepted; newer ones refuse
/// ([`ProjectDirError::UnsupportedVersion`]).
pub(crate) const MANIFEST_VERSION: u32 = 1;

/// Largest manifest accepted before it is rejected as invalid.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// Largest ignore file read; larger files yield empty rules rather than an error.
const MAX_IGNORE_BYTES: u64 = 256 * 1024;

/// Ignore patterns kept per file; the tail is dropped rather than erroring.
const MAX_IGNORE_PATTERNS: usize = 500;

/// Largest profile document accepted before it is rejected as invalid.
const MAX_PROFILE_BYTES: u64 = 64 * 1024;

/// Default ignore stub written on init when no ignore file exists yet.
/// Comment-only, so it changes no tool behavior by itself.
const DEFAULT_IGNORE_STUB: &str = "# Workspace ignore rules for agent tool reads \
    (gitignore-style, workspace-relative).\n# One pattern per line; `#` starts a comment, \
    `!` negates, a trailing `/` matches directories only.\n# The `.nexora/` directory itself \
    is always excluded.\n";

/// The `.nexora/nexora.json` manifest: a version field only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NexoraManifest {
    /// Manifest schema version (see [`MANIFEST_VERSION`]).
    pub version: u32,
}

/// Secret-free failures for `.nexora/` resolution, init, and reads.
///
/// Every variant renders as fixed category text: formatting a
/// [`ProjectDirError`] can never leak file content, a credential, SQL, or a
/// stored payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectDirError {
    /// A path escaped the workspace root (or the root itself is unusable).
    OutsideWorkspace,
    /// The workspace has no `.nexora/` directory yet.
    NotInitialized,
    /// The manifest names a version newer than [`MANIFEST_VERSION`].
    UnsupportedVersion {
        /// The newer on-disk version (a number, never content).
        version: u32,
    },
    /// The manifest exists but is missing, oversized, or unparsable.
    InvalidManifest,
    /// A profile document is missing, oversized, or fails validation.
    InvalidProfile,
    /// A filesystem operation failed.
    Io,
}

impl std::fmt::Display for ProjectDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutsideWorkspace => write!(f, "path is outside the workspace"),
            Self::NotInitialized => {
                write!(f, "the workspace project directory is not initialized")
            }
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported project directory version: {version}")
            }
            Self::InvalidManifest => write!(f, "the project manifest is invalid"),
            Self::InvalidProfile => write!(f, "the workspace profile is invalid"),
            Self::Io => write!(f, "a project directory operation failed"),
        }
    }
}

impl std::error::Error for ProjectDirError {}

/// Lexical `.nexora/` location for `workspace_root` (no filesystem access).
///
/// Returns the joined path without creating anything or resolving links; use
/// [`resolve_nexora_dir`] for the guarded form.
#[must_use]
pub(crate) fn nexora_dir_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(NEXORA_DIR_NAME)
}

/// Canonicalize `workspace_root`: it must exist and be a directory.
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] when the root does not
/// exist, is not a directory, or cannot be resolved.
fn canonical_workspace_dir(workspace_root: &Path) -> Result<PathBuf, ProjectDirError> {
    let canon = std::fs::canonicalize(workspace_root)
        .map(strip_verbatim)
        .map_err(|_| ProjectDirError::OutsideWorkspace)?;
    if canon.is_dir() {
        Ok(canon)
    } else {
        Err(ProjectDirError::OutsideWorkspace)
    }
}

/// Guarded `.nexora/` location: the canonical workspace root joined with
/// [`.nexora/`](NEXORA_DIR_NAME), refusing an existing symlink escape.
///
/// Nothing is created; use [`init_nexora_dir`] for explicit creation.
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] when the workspace root is
/// unusable or an existing `.nexora` link escapes the workspace.
pub(crate) fn resolve_nexora_dir(workspace_root: &Path) -> Result<PathBuf, ProjectDirError> {
    let canon_ws = canonical_workspace_dir(workspace_root)?;
    let dir = canon_ws.join(NEXORA_DIR_NAME);
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if meta.file_type().is_symlink() && !link_target_within(&canon_ws, &dir) {
            return Err(ProjectDirError::OutsideWorkspace);
        }
    }
    Ok(dir)
}

/// Explicit, idempotent `.nexora/` init: directories, manifest, ignore stub.
///
/// Creates `.nexora/` and `profiles/`, writes `nexora.json` (version
/// [`MANIFEST_VERSION`]) and a comment-only `.nexoraignore` when missing, and
/// then re-reads the manifest so a pre-existing newer version still refuses.
/// Existing files are never overwritten, and every write goes through the
/// canonical guard pattern (lexical check, `symlink_metadata` prefix walk,
/// canonicalize-after-create, post-write backstop).
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] when the root is unusable or
/// any write target escapes the workspace,
/// [`ProjectDirError::UnsupportedVersion`] for a newer pre-existing manifest,
/// [`ProjectDirError::InvalidManifest`] for a corrupt pre-existing manifest,
/// or [`ProjectDirError::Io`] when the filesystem fails.
pub(crate) fn init_nexora_dir(workspace_root: &Path) -> Result<PathBuf, ProjectDirError> {
    let canon_ws = canonical_workspace_dir(workspace_root)?;
    let dir = canon_ws.join(NEXORA_DIR_NAME);
    create_dir_guarded(&canon_ws, &dir)?;
    create_dir_guarded(&canon_ws, &dir.join(PROFILES_DIR_NAME))?;
    ensure_small_file(
        &canon_ws,
        &dir.join(MANIFEST_FILE_NAME),
        &default_manifest_body(),
    )?;
    ensure_small_file(&canon_ws, &dir.join(IGNORE_FILE_NAME), DEFAULT_IGNORE_STUB)?;
    load_manifest_inner(&canon_ws)?;
    Ok(dir)
}

/// Read the workspace manifest, enforcing the forward-only version contract.
///
/// # Errors
///
/// Returns [`ProjectDirError::NotInitialized`] when `.nexora/` or the manifest
/// is absent, [`ProjectDirError::OutsideWorkspace`] on a symlink escape,
/// [`ProjectDirError::UnsupportedVersion`] for a newer version, or
/// [`ProjectDirError::InvalidManifest`] when the manifest is unreadable,
/// oversized, or unparsable.
pub(crate) fn load_manifest(workspace_root: &Path) -> Result<NexoraManifest, ProjectDirError> {
    let canon_ws = canonical_workspace_dir(workspace_root)?;
    load_manifest_inner(&canon_ws)
}

/// [`load_manifest`] over an already-canonical workspace root.
///
/// # Errors
///
/// See [`load_manifest`].
fn load_manifest_inner(canon_ws: &Path) -> Result<NexoraManifest, ProjectDirError> {
    let dir = canon_ws.join(NEXORA_DIR_NAME);
    reject_link_escape(canon_ws, &dir)?;
    if !dir.is_dir() {
        return Err(ProjectDirError::NotInitialized);
    }
    let path = dir.join(MANIFEST_FILE_NAME);
    reject_link_escape(canon_ws, &path)?;
    let meta = std::fs::metadata(&path).map_err(|_| ProjectDirError::NotInitialized)?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(ProjectDirError::InvalidManifest);
    }
    let bytes = std::fs::read(&path).map_err(|_| ProjectDirError::InvalidManifest)?;
    let text = String::from_utf8(bytes).map_err(|_| ProjectDirError::InvalidManifest)?;
    let manifest: NexoraManifest =
        serde_json::from_str(&text).map_err(|_| ProjectDirError::InvalidManifest)?;
    if manifest.version == 0 {
        return Err(ProjectDirError::InvalidManifest);
    }
    if manifest.version > MANIFEST_VERSION {
        return Err(ProjectDirError::UnsupportedVersion {
            version: manifest.version,
        });
    }
    Ok(manifest)
}

/// Reject `path` when it is a link escaping `canon_ws` (live or dangling).
///
/// Missing paths pass: there is no link to follow yet.
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] when a link target escapes,
/// or [`ProjectDirError::Io`] when link metadata cannot be read.
fn reject_link_escape(canon_ws: &Path, path: &Path) -> Result<(), ProjectDirError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !link_target_within(canon_ws, path) {
                return Err(ProjectDirError::OutsideWorkspace);
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ProjectDirError::Io),
    }
}

/// Whether the link at `path` resolves inside `canon_ws`.
///
/// Live targets canonicalize directly; dangling targets resolve against the
/// canonical parent. A link whose target cannot be decided fails closed.
fn link_target_within(canon_ws: &Path, path: &Path) -> bool {
    if let Ok(canon) = path.canonicalize() {
        return is_within_workspace(canon_ws, &canon);
    }
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let target_path = PathBuf::from(&target);
    let parent_canon = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .unwrap_or_else(|| canon_ws.to_path_buf());
    let resolved = if target_path.is_absolute() {
        target_path
    } else {
        parent_canon.join(target_path)
    };
    is_within_workspace(canon_ws, &resolved)
}

/// Whether `path` canonicalizes inside `canon_ws` (fail-closed).
fn canonical_inside(canon_ws: &Path, path: &Path) -> bool {
    path.canonicalize()
        .is_ok_and(|canon| is_within_workspace(canon_ws, &canon))
}

/// Guarded directory creation, one level at a time.
///
/// Each existing level is re-verified (links must resolve inside), each new
/// level is canonicalized after creation and re-verified, so a symlink
/// component pointing outside is rejected *before* anything is created
/// through it — including the depth-2 shape where an intermediate component
/// is a link while deeper parents do not exist yet. A final regular file
/// blocking the directory reports [`ProjectDirError::Io`].
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] on any escape,
/// [`ProjectDirError::Io`] when the filesystem fails or a file blocks the path.
fn create_dir_guarded(canon_ws: &Path, dir: &Path) -> Result<(), ProjectDirError> {
    if !is_within_workspace(canon_ws, dir) {
        return Err(ProjectDirError::OutsideWorkspace);
    }
    let ws_count = canon_ws.components().count();
    let comps: Vec<Component<'_>> = dir.components().collect();
    if comps.len() < ws_count {
        return Err(ProjectDirError::OutsideWorkspace);
    }
    let mut prefix = canon_ws.to_path_buf();
    for comp in &comps[ws_count..] {
        prefix.push(comp.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    if !link_target_within(canon_ws, &prefix) {
                        return Err(ProjectDirError::OutsideWorkspace);
                    }
                } else if !canonical_inside(canon_ws, &prefix) {
                    return Err(ProjectDirError::OutsideWorkspace);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&prefix).map_err(|_| ProjectDirError::Io)?;
                if !canonical_inside(canon_ws, &prefix) {
                    return Err(ProjectDirError::OutsideWorkspace);
                }
            }
            Err(_) => return Err(ProjectDirError::Io),
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(&prefix) {
        if meta.file_type().is_file() {
            return Err(ProjectDirError::Io);
        }
    }
    Ok(())
}

/// Guarded small-file creation for init defaults: never overwrites.
///
/// An existing entry (file, directory, or inside-resolving link) is left
/// untouched so init stays idempotent; an escaping link refuses. The write
/// itself is backstopped by a post-create canonical check.
///
/// # Errors
///
/// Returns [`ProjectDirError::OutsideWorkspace`] on any escape, or
/// [`ProjectDirError::Io`] when the filesystem fails.
fn ensure_small_file(canon_ws: &Path, path: &Path, body: &str) -> Result<(), ProjectDirError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !link_target_within(canon_ws, path) {
                return Err(ProjectDirError::OutsideWorkspace);
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if !is_within_workspace(canon_ws, path) {
                return Err(ProjectDirError::OutsideWorkspace);
            }
            std::fs::write(path, body).map_err(|_| ProjectDirError::Io)?;
            if canonical_inside(canon_ws, path) {
                Ok(())
            } else {
                Err(ProjectDirError::OutsideWorkspace)
            }
        }
        Err(_) => Err(ProjectDirError::Io),
    }
}

/// Serialized default manifest (`{"version": 1}`).
fn default_manifest_body() -> String {
    let manifest = NexoraManifest {
        version: MANIFEST_VERSION,
    };
    match serde_json::to_string_pretty(&manifest) {
        Ok(text) => format!("{text}\n"),
        Err(_) => format!("{{\"version\":{MANIFEST_VERSION}}}\n"),
    }
}

// ---------------------------------------------------------------------------
// Ignore rules (`.nexoraignore`, gitignore-style, workspace-relative)
// ---------------------------------------------------------------------------

/// One parsed ignore pattern: `!` negation, trailing-`/` directory-only, and
/// slash-anchored versus basename matching.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IgnorePattern {
    /// A `!`-prefixed pattern re-includes what earlier patterns excluded.
    negated: bool,
    /// A trailing-`/` pattern matches directories (and their subtrees) only.
    dir_only: bool,
    /// The body contains a slash, so it matches the workspace-relative path;
    /// otherwise it matches the basename at any depth.
    anchored: bool,
    /// Pattern body without the `!` prefix, leading `/`, or trailing `/`.
    /// `*` spans within one segment, `?` is one character, and `**` spans
    /// segments (anchored patterns only).
    body: String,
}

/// Parsed `.nexoraignore` rules for one workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct IgnoreRules {
    /// Patterns in file order; the last match wins.
    patterns: Vec<IgnorePattern>,
}

/// Parse ignore file text into rules (blank lines and `#` comments skipped).
///
/// Simplifications, pinned by tests: no character classes (`[` is literal),
/// trailing spaces are trimmed, and only the first
/// [`MAX_IGNORE_PATTERNS`] patterns are kept.
#[must_use]
fn parse_ignore_rules(text: &str) -> IgnoreRules {
    let mut patterns = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        // A leading backslash quotes a literal `#` or `!`.
        let (negated, rest, quoted) = if let Some(inner) = line.strip_prefix('\\') {
            if inner.starts_with('#') || inner.starts_with('!') {
                (false, inner, true)
            } else {
                (false, line, false)
            }
        } else if let Some(inner) = line.strip_prefix('!') {
            (true, inner, false)
        } else {
            (false, line, false)
        };
        if !quoted && rest.starts_with('#') {
            continue;
        }
        let rest = rest.trim_end_matches([' ', '\t']);
        if rest.is_empty() {
            continue;
        }
        let (dir_only, rest) = match rest.strip_suffix('/') {
            Some(inner) => (true, inner),
            None => (false, rest),
        };
        if rest.is_empty() {
            continue;
        }
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if rest.is_empty() {
            continue;
        }
        let anchored = rest.contains('/');
        patterns.push(IgnorePattern {
            negated,
            dir_only,
            anchored,
            body: rest.to_string(),
        });
        if patterns.len() >= MAX_IGNORE_PATTERNS {
            break;
        }
    }
    IgnoreRules { patterns }
}

/// Match one path segment: `*` spans any run within the segment, `?` is one
/// character, everything else (including `[`) is literal.
#[must_use]
fn segment_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = text.chars().collect();
    let mut pi = 0_usize;
    let mut ti = 0_usize;
    let mut star: Option<usize> = None;
    let mut mark = 0_usize;
    while ti < chars.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == chars[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi + 1);
            mark = ti;
            pi += 1;
        } else if let Some(resume) = star {
            pi = resume;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Match slash-joined segments where `**` eats zero or more segments.
fn match_segments(pat: &[&str], text: &[&str]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat[0] == "**" {
        for eaten in 0..=text.len() {
            if match_segments(&pat[1..], &text[eaten..]) {
                return true;
            }
        }
        return false;
    }
    if text.is_empty() || !segment_match(pat[0], text[0]) {
        return false;
    }
    match_segments(&pat[1..], &text[1..])
}

/// Match an anchored body against a workspace-relative path.
#[must_use]
fn anchored_match(body: &str, rel: &str) -> bool {
    let pat: Vec<&str> = body.split('/').collect();
    let text: Vec<&str> = rel.split('/').collect();
    match_segments(&pat, &text)
}

/// Whether one pattern matches `candidate` (a workspace-relative path).
#[must_use]
fn pattern_matches(pattern: &IgnorePattern, candidate: &str, is_dir: bool) -> bool {
    if pattern.dir_only && !is_dir {
        return false;
    }
    if pattern.anchored {
        anchored_match(&pattern.body, candidate)
    } else {
        let base = candidate.rsplit('/').next().unwrap_or(candidate);
        segment_match(&pattern.body, base)
    }
}

/// Last-match-wins verdict over every ancestor prefix plus the path itself.
///
/// Ancestor prefixes count as directories, so a pattern matching a directory
/// excludes its whole subtree; patterns are applied outermost so a later
/// pattern wins overall. Unlike git there is no parent-exclusion stickiness:
/// a later negation can re-include below an excluded directory.
#[must_use]
fn rules_verdict(rules: &IgnoreRules, rel: &str, is_dir: bool) -> bool {
    let mut prefixes: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for part in rel.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        prefixes.push(prefix.clone());
    }
    let mut ignored = false;
    for pattern in &rules.patterns {
        for (index, candidate) in prefixes.iter().enumerate() {
            let dir = index + 1 < prefixes.len() || is_dir;
            if pattern_matches(pattern, candidate, dir) {
                ignored = !pattern.negated;
            }
        }
    }
    ignored
}

/// Workspace-relative `/`-joined path for `target`, or [`None`] when either
/// side fails to canonicalize or the target escapes the workspace.
fn relative_posix(workspace_root: &Path, target: &Path) -> Option<String> {
    let canon_ws = std::fs::canonicalize(workspace_root)
        .ok()
        .map(strip_verbatim)?;
    let canon_target = std::fs::canonicalize(target).ok().map(strip_verbatim)?;
    if !is_within_workspace(&canon_ws, &canon_target) {
        return None;
    }
    let rel = canon_target.strip_prefix(&canon_ws).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(String::new());
    }
    Some(
        rel.components()
            .map(|comp| comp.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Load the workspace ignore rules; missing, unreadable, oversized, or
/// escaping files yield empty rules rather than an error.
///
/// The `.nexora/` exclusion itself needs no file, so enforcement stays
/// fail-closed even when the rules cannot be read.
#[must_use]
pub(crate) fn load_ignore_rules(workspace_root: &Path) -> IgnoreRules {
    let canon_ws = std::fs::canonicalize(workspace_root)
        .ok()
        .map(strip_verbatim);
    let Some(canon_ws) = canon_ws else {
        return IgnoreRules::default();
    };
    let path = canon_ws.join(NEXORA_DIR_NAME).join(IGNORE_FILE_NAME);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() && !link_target_within(&canon_ws, &path) {
            return IgnoreRules::default();
        }
    }
    let Ok(meta) = std::fs::metadata(&path) else {
        return IgnoreRules::default();
    };
    if meta.len() > MAX_IGNORE_BYTES {
        return IgnoreRules::default();
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return IgnoreRules::default();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return IgnoreRules::default();
    };
    parse_ignore_rules(&text)
}

/// Whether `target` is excluded from agent tool reads: inside `.nexora/`
/// (always) or matched by the workspace ignore rules.
///
/// Listings filter excluded entries silently while direct reads and scopes
/// deny loudly with a fixed content-free error (naming an ignored file in a
/// listing error would leak its existence).
///
/// Unresolvable or escaping targets fail closed to excluded; the workspace
/// root itself is never excluded.
#[must_use]
pub(crate) fn is_excluded(workspace_root: &Path, target: &Path, rules: &IgnoreRules) -> bool {
    let Some(rel) = relative_posix(workspace_root, target) else {
        return true;
    };
    if rel.is_empty() {
        return false;
    }
    if rel == NEXORA_DIR_NAME || rel.starts_with(&format!("{NEXORA_DIR_NAME}/")) {
        return true;
    }
    // Follow links for the dir-only probe so a symlink-to-dir still matches
    // directory patterns; missing or broken targets probe as non-directories
    // (fail-closed: only the pattern verdict is affected, never access).
    let is_dir = std::fs::metadata(target).is_ok_and(|meta| meta.is_dir());
    rules_verdict(rules, &rel, is_dir)
}

/// [`is_excluded`] with freshly loaded rules (one file read per call).
#[must_use]
pub(crate) fn path_is_ignored(workspace_root: &Path, target: &Path) -> bool {
    is_excluded(workspace_root, target, &load_ignore_rules(workspace_root))
}

/// Exclusion verdict for mutating tool calls over a possibly-missing target.
///
/// [`is_excluded`] canonicalizes, so a not-yet-created path always fails
/// closed to excluded — which would deny every create. Existing targets
/// (files, directories, and dangling links) go through [`is_excluded`]
/// unchanged; missing targets get a lexical verdict instead (`.nexora/`
/// denies, otherwise the ignore rules decide over the workspace-relative
/// path). Layouts unrelated to the workspace still fail closed, and the deny
/// error stays the fixed content-free category.
#[must_use]
pub(crate) fn is_excluded_for_write(
    workspace_root: &Path,
    target: &Path,
    rules: &IgnoreRules,
) -> bool {
    if target.exists() || std::fs::symlink_metadata(target).is_ok() {
        return is_excluded(workspace_root, target, rules);
    }
    let Some(rel) = lexical_rel_posix(workspace_root, target) else {
        return true;
    };
    if rel.is_empty() {
        return false;
    }
    if rel == NEXORA_DIR_NAME || rel.starts_with(&format!("{NEXORA_DIR_NAME}/")) {
        return true;
    }
    rules_verdict(rules, &rel, false)
}

/// Workspace-relative `/`-joined path without touching the filesystem.
///
/// [`relative_posix`] needs both sides to exist; creates do not, so this
/// lexical form backs [`is_excluded_for_write`]. Verbatim prefixes are
/// stripped so canonical and joined paths compare equally.
fn lexical_rel_posix(workspace_root: &Path, target: &Path) -> Option<String> {
    let ws = strip_verbatim(workspace_root.to_path_buf());
    let target = strip_verbatim(target.to_path_buf());
    let rel = target.strip_prefix(&ws).ok()?;
    if rel.as_os_str().is_empty() {
        return Some(String::new());
    }
    Some(
        rel.components()
            .map(|comp| comp.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

// ---------------------------------------------------------------------------
// Workspace-scoped profiles (scaffold: validation only, no logic moves)
// ---------------------------------------------------------------------------

/// Validate a profile document against the routing profile shape.
///
/// This is the parse-then-validate single source
/// ([`RoutingProfile::from_json`]) shared with the settings-store path —
/// no bounds are duplicated here.
///
/// # Errors
///
/// Returns [`RoutingError::InvalidProfile`] for malformed or out-of-domain
/// payloads, or [`RoutingError::Serialization`] when encoding fails.
pub(crate) fn validate_profile_document(raw: &str) -> Result<RoutingProfile, RoutingError> {
    RoutingProfile::from_json(raw)
}

/// Load and validate one `profiles/<file_name>` document.
///
/// `file_name` must be a bare `.json` file name (no separators, no parent
/// references); the file must resolve inside the workspace `profiles/`
/// directory. No profile logic moves: this only reuses the routing validation
/// shape for workspace-scoped documents.
///
/// # Errors
///
/// Returns [`ProjectDirError::InvalidProfile`] for a bad name, an
/// unreadable/oversized file, or a document failing routing validation,
/// [`ProjectDirError::NotInitialized`] when `profiles/` is absent,
/// [`ProjectDirError::OutsideWorkspace`] on a symlink escape, or
/// [`ProjectDirError::Io`] when link metadata cannot be read.
pub(crate) fn load_profile_file(
    workspace_root: &Path,
    file_name: &str,
) -> Result<RoutingProfile, ProjectDirError> {
    if file_name.is_empty()
        || file_name == ".json"
        || file_name.contains(['/', '\\', '\0'])
        || file_name.contains("..")
    {
        return Err(ProjectDirError::InvalidProfile);
    }
    let is_json = Path::new(file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    if !is_json {
        return Err(ProjectDirError::InvalidProfile);
    }
    let canon_ws = canonical_workspace_dir(workspace_root)?;
    let dir = canon_ws.join(NEXORA_DIR_NAME).join(PROFILES_DIR_NAME);
    if !dir.is_dir() {
        return Err(ProjectDirError::NotInitialized);
    }
    let path = dir.join(file_name);
    if !is_within_workspace(&canon_ws, &path) {
        return Err(ProjectDirError::OutsideWorkspace);
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() && !link_target_within(&canon_ws, &path) {
                return Err(ProjectDirError::OutsideWorkspace);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProjectDirError::Io);
        }
        Err(_) => return Err(ProjectDirError::Io),
    }
    let meta = std::fs::metadata(&path).map_err(|_| ProjectDirError::Io)?;
    if meta.len() > MAX_PROFILE_BYTES {
        return Err(ProjectDirError::InvalidProfile);
    }
    let bytes = std::fs::read(&path).map_err(|_| ProjectDirError::Io)?;
    let text = String::from_utf8(bytes).map_err(|_| ProjectDirError::InvalidProfile)?;
    validate_profile_document(&text).map_err(|_| ProjectDirError::InvalidProfile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::agent::tools::test_support::{call, temp_workspace};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Canonical scratch workspace that never sits under a guard the init
    /// path cares about (unlike `temp_workspace`, it needs no tool registry).
    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!(
            "nexora-project-dir-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp root");
        strip_verbatim(dir.canonicalize().expect("canonicalize temp root"))
    }

    /// First provider and model listed in this build's registry.
    fn listed_entry() -> (String, String) {
        let registry = crate::infrastructure::providers::supported_providers();
        let first = registry.first().expect("registry must list a provider");
        (
            first.name.clone(),
            first.models.first().cloned().unwrap_or_default(),
        )
    }

    fn profile_json(provider: &str, model: &str) -> String {
        format!(r#"[{{"provider":{provider:?},"model":{model:?}}}]"#)
    }

    #[test]
    fn resolution_stays_inside_workspace() {
        let ws = temp_root();
        assert_eq!(nexora_dir_path(&ws), ws.join(".nexora"));
        let resolved = resolve_nexora_dir(&ws).expect("resolves inside");
        assert_eq!(resolved, ws.join(".nexora"));
        let missing = ws.join("gone-4d2a");
        assert_eq!(
            resolve_nexora_dir(&missing).expect_err("missing root refuses"),
            ProjectDirError::OutsideWorkspace
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn init_is_idempotent_and_scaffolds() {
        let ws = temp_root();
        let dir = init_nexora_dir(&ws).expect("init succeeds");
        assert_eq!(dir, ws.join(".nexora"));
        assert!(dir.join("nexora.json").is_file());
        assert!(dir.join("profiles").is_dir());
        assert!(dir.join(".nexoraignore").is_file());
        let manifest = load_manifest(&ws).expect("manifest loads");
        assert_eq!(manifest.version, MANIFEST_VERSION);
        // Idempotent: a second init returns the same path and never
        // overwrites existing files.
        let before_manifest = std::fs::read(dir.join("nexora.json")).expect("read manifest");
        let before_ignore = std::fs::read(dir.join(".nexoraignore")).expect("read ignore");
        let again = init_nexora_dir(&ws).expect("second init succeeds");
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read(dir.join("nexora.json")).expect("manifest kept"),
            before_manifest
        );
        assert_eq!(
            std::fs::read(dir.join(".nexoraignore")).expect("ignore kept"),
            before_ignore
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn init_refuses_missing_root() {
        let ws = temp_root();
        let missing = ws.join("gone-7e1b");
        assert_eq!(
            init_nexora_dir(&missing).expect_err("missing root refuses"),
            ProjectDirError::OutsideWorkspace
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn manifest_newer_version_refuses_secret_free() {
        const SENTINEL: &str = "sk-test-sentinel-9f3c2a";
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        std::fs::write(
            ws.join(".nexora").join("nexora.json"),
            format!("{{\"version\":999,\"note\":\"{SENTINEL}\"}}"),
        )
        .expect("seed newer manifest");
        let err = load_manifest(&ws).expect_err("newer version refuses");
        assert_eq!(err, ProjectDirError::UnsupportedVersion { version: 999 });
        assert_eq!(
            format!("{err}"),
            "unsupported project directory version: 999"
        );
        assert!(!format!("{err}").contains(SENTINEL));
        // Init re-reads the manifest, so it refuses too — without touching files.
        let init_err = init_nexora_dir(&ws).expect_err("init refuses newer manifest");
        assert_eq!(
            init_err,
            ProjectDirError::UnsupportedVersion { version: 999 }
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn manifest_corrupt_refuses_secret_free() {
        const SENTINEL: &str = "top-secret-payload-51de";
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        std::fs::write(
            ws.join(".nexora").join("nexora.json"),
            format!("not json at all: {SENTINEL}"),
        )
        .expect("seed corrupt manifest");
        let err = load_manifest(&ws).expect_err("corrupt manifest refuses");
        assert_eq!(err, ProjectDirError::InvalidManifest);
        assert!(!format!("{err}").contains(SENTINEL));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn load_before_init_reports_not_initialized() {
        let ws = temp_root();
        assert_eq!(
            load_manifest(&ws).expect_err("no .nexora yet"),
            ProjectDirError::NotInitialized
        );
        assert_eq!(
            load_profile_file(&ws, "chat.json").expect_err("no profiles yet"),
            ProjectDirError::NotInitialized
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn ignore_matching_pins_nested_and_negation() {
        let rules =
            parse_ignore_rules("# comment\n\n*.log\n!important.log\nbuild/\nsrc/*.generated.ts\n");
        // Basename glob at any depth, with a later negation winning.
        assert!(rules_verdict(&rules, "a.log", false));
        assert!(rules_verdict(&rules, "sub/dir/a.log", false));
        assert!(!rules_verdict(&rules, "important.log", false));
        assert!(!rules_verdict(&rules, "sub/important.log", false));
        // Directory-only patterns exclude the directory and its subtree,
        // but never a file of the same name.
        assert!(rules_verdict(&rules, "build", true));
        assert!(rules_verdict(&rules, "build/out.o", false));
        assert!(!rules_verdict(&rules, "build", false));
        // Anchored patterns match the relative path only.
        assert!(rules_verdict(&rules, "src/x.generated.ts", false));
        assert!(!rules_verdict(&rules, "other/x.generated.ts", false));
        assert!(!rules_verdict(&rules, "src/keep.ts", false));
    }

    #[test]
    fn ignore_double_star_spans_segments() {
        let rules = parse_ignore_rules("docs/**/draft.md\n");
        assert!(rules_verdict(&rules, "docs/draft.md", false));
        assert!(rules_verdict(&rules, "docs/a/b/draft.md", false));
        assert!(!rules_verdict(&rules, "docs/a/final.md", false));
    }

    #[test]
    fn nexora_dir_always_excluded_without_any_rules() {
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        std::fs::write(ws.join("visible.txt"), "hello").expect("seed");
        let rules = load_ignore_rules(&ws);
        assert!(is_excluded(&ws, &ws.join(".nexora"), &rules));
        assert!(is_excluded(&ws, &ws.join(".nexora/nexora.json"), &rules));
        assert!(is_excluded(
            &ws,
            &ws.join(".nexora/profiles/chat.json"),
            &rules
        ));
        assert!(!is_excluded(&ws, &ws.join("visible.txt"), &rules));
        // The workspace root itself is never excluded.
        assert!(!is_excluded(&ws, &ws, &rules));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn tool_reads_honor_ignore_rules() {
        const SECRET: &str = "outer-secret-needle-77aa";
        let ws = temp_workspace();
        std::fs::create_dir_all(ws.join(".nexora")).expect("seed .nexora");
        std::fs::write(ws.join(".nexora/nexora.json"), "{\"version\":1}").expect("seed");
        std::fs::write(ws.join(".nexora/.nexoraignore"), "secret.txt\n").expect("seed rules");
        std::fs::write(ws.join("secret.txt"), SECRET).expect("seed secret");
        std::fs::write(ws.join("visible.txt"), "visible-needle here").expect("seed visible");

        // Direct reads are denied with a fixed error that echoes no content.
        let read = call("read_file", serde_json::json!({"path": "secret.txt"}));
        let err = crate::application::agent::tools::ToolRegistry::execute(&read, &ws)
            .expect_err("ignored file denied");
        assert_eq!(
            err.to_string(),
            "Error: path is excluded by workspace ignore rules"
        );
        assert!(!err.to_string().contains(SECRET));

        // `.nexora/` itself is denied too.
        let read_home = call(
            "read_file",
            serde_json::json!({"path": ".nexora/nexora.json"}),
        );
        assert!(crate::application::agent::tools::ToolRegistry::execute(&read_home, &ws).is_err());

        // Listings filter ignored entries and `.nexora/` silently.
        let list = call("list_directory", serde_json::json!({}));
        let out =
            crate::application::agent::tools::ToolRegistry::execute(&list, &ws).expect("list runs");
        assert!(out.contains("visible.txt"), "visible entry missing: {out}");
        assert!(!out.contains("secret.txt"), "ignored entry leaked: {out}");
        assert!(!out.contains(".nexora"), ".nexora leaked: {out}");

        // Search skips ignored files while still finding visible hits.
        let find = call("search_files", serde_json::json!({"pattern": "needle"}));
        let hits = crate::application::agent::tools::ToolRegistry::execute(&find, &ws)
            .expect("search runs");
        assert!(hits.contains("visible.txt"), "visible hit missing: {hits}");
        assert!(!hits.contains(SECRET), "ignored content leaked: {hits}");

        // An ignored scope is denied outright.
        let scoped = call(
            "search_files",
            serde_json::json!({"pattern": "needle", "directory": "secret.txt"}),
        );
        assert!(crate::application::agent::tools::ToolRegistry::execute(&scoped, &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn ignored_dir_list_denied_loudly() {
        // A directly listed ignored directory denies with the same fixed
        // error as direct reads (executor.rs deny branch): the caller named
        // it, so there is nothing to leak by denying loudly.
        let ws = temp_workspace();
        std::fs::create_dir_all(ws.join(".nexora")).expect("seed .nexora");
        std::fs::write(ws.join(".nexora/.nexoraignore"), "secret-dir/\n").expect("seed rules");
        std::fs::create_dir_all(ws.join("secret-dir")).expect("seed dir");
        std::fs::write(ws.join("secret-dir/note.txt"), "x").expect("seed file");
        let list = call("list_directory", serde_json::json!({"path": "secret-dir"}));
        let err = crate::application::agent::tools::ToolRegistry::execute(&list, &ws)
            .expect_err("ignored dir denied");
        assert_eq!(
            err.to_string(),
            "Error: path is excluded by workspace ignore rules"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn negation_reincludes_below_excluded_dir() {
        // Unlike git there is no parent-exclusion stickiness: a later
        // negation re-includes below an excluded directory.
        let rules = parse_ignore_rules("build/\n!build/keep\n");
        assert!(rules_verdict(&rules, "build", true));
        assert!(rules_verdict(&rules, "build/out.o", false));
        assert!(!rules_verdict(&rules, "build/keep", false));
    }

    #[test]
    fn exclusion_fails_closed_for_missing_and_outside() {
        let ws = temp_root();
        let rules = IgnoreRules::default();
        assert!(is_excluded(&ws, &ws.join("no-such-file-38f1"), &rules));
        let outside = temp_root();
        assert!(is_excluded(&ws, &outside, &rules));
        assert!(is_excluded(&ws, &outside.join("no-such-file-9c4e"), &rules));
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    #[cfg(unix)]
    fn symlink_to_dir_matches_dir_only_pattern() {
        // The dir-only probe follows links: a symlink-to-dir matches
        // directory patterns (through its canonical target), while a
        // symlink-to-file still does not.
        use std::os::unix::fs::symlink;
        let ws = temp_root();
        let real = ws.join("real-dir");
        std::fs::create_dir_all(&real).expect("seed dir");
        symlink(&real, ws.join("linked-dir")).expect("link dir");
        std::fs::write(ws.join("real-file.txt"), "x").expect("seed file");
        symlink(ws.join("real-file.txt"), ws.join("linked-file")).expect("link file");
        let dir_rules = parse_ignore_rules("real-dir/\n");
        assert!(is_excluded(&ws, &ws.join("linked-dir"), &dir_rules));
        let file_rules = parse_ignore_rules("real-file.txt/\n");
        assert!(!is_excluded(&ws, &ws.join("linked-file"), &file_rules));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    #[cfg(unix)]
    fn nexora_symlink_escape_refuses_and_writes_nothing_outside() {
        use std::os::unix::fs::symlink;
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        let outside = temp_root();
        std::fs::remove_dir_all(ws.join(".nexora")).expect("clear .nexora");
        symlink(&outside, ws.join(".nexora")).expect("link .nexora -> outside");
        let err = init_nexora_dir(&ws).expect_err("escaping link refuses");
        assert_eq!(err, ProjectDirError::OutsideWorkspace);
        assert!(
            !outside.join("profiles").exists(),
            "nothing may be created through the link"
        );
        assert!(!outside.join("nexora.json").exists());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    #[cfg(unix)]
    fn nested_symlink_escape_under_nexora_blocked_at_depth_two() {
        // Depth-2 shape from the workspace tools: `.nexora/link` points
        // outside while `newsub` does not exist yet. The guarded create must
        // fail at the `link` level without creating anything outside.
        use std::os::unix::fs::symlink;
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        let outside = temp_root();
        let canon_ws = canonical_workspace_dir(&ws).expect("canonical root");
        symlink(&outside, ws.join(".nexora/link")).expect("link -> outside");
        let err = create_dir_guarded(&canon_ws, &ws.join(".nexora/link/newsub"))
            .expect_err("depth-2 escape refuses");
        assert_eq!(err, ProjectDirError::OutsideWorkspace);
        assert!(
            !outside.join("newsub").exists(),
            "outside dir must not gain newsub via the link"
        );
        // A genuinely nested directory still succeeds (no behavior change).
        create_dir_guarded(&canon_ws, &ws.join(".nexora/profiles/extra"))
            .expect("in-workspace nested create succeeds");
        assert!(ws.join(".nexora/profiles/extra").is_dir());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn profile_documents_reuse_routing_validation() {
        let ws = temp_root();
        init_nexora_dir(&ws).expect("init succeeds");
        let (provider, model) = listed_entry();
        std::fs::write(
            ws.join(".nexora/profiles/chat.json"),
            profile_json(&provider, &model),
        )
        .expect("seed valid profile");
        let loaded = load_profile_file(&ws, "chat.json").expect("valid profile loads");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].provider, provider);
        // Unknown providers fail through the same single source.
        std::fs::write(
            ws.join(".nexora/profiles/bad.json"),
            r#"[{"provider":"ghost-provider","model":"x"}]"#,
        )
        .expect("seed invalid profile");
        assert_eq!(
            load_profile_file(&ws, "bad.json").expect_err("invalid profile refuses"),
            ProjectDirError::InvalidProfile
        );
        // Traversal names never reach the filesystem.
        for name in ["../nexora.json", "sub/chat.json", "chat.txt", "", ".json"] {
            assert_eq!(
                load_profile_file(&ws, name).expect_err("bad name refuses"),
                ProjectDirError::InvalidProfile,
                "name {name:?} must be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn errors_stay_secret_free() {
        const SENTINELS: [&str; 3] = ["sk-", "top-secret-value", "credential"];
        let ws = temp_root();
        // Every error surface is exercised with adversarial content nearby;
        // none of it may appear in the rendered message.
        let outside = resolve_nexora_dir(&ws.join("gone-0000")).expect_err("outside");
        let uninit = load_manifest(&ws).expect_err("uninit");
        init_nexora_dir(&ws).expect("init succeeds");
        std::fs::write(
            ws.join(".nexora/nexora.json"),
            "{\"version\":7,\"k\":\"top-secret-value sk-123 credential\"}",
        )
        .expect("seed newer manifest");
        let versioned = load_manifest(&ws).expect_err("versioned");
        std::fs::write(
            ws.join(".nexora/profiles/x.json"),
            "[{\"provider\":\"sk-evil\",\"model\":\"top-secret-value\"}]",
        )
        .expect("seed adversarial profile");
        let profile = load_profile_file(&ws, "x.json").expect_err("profile");
        for err in [
            outside,
            uninit,
            versioned,
            profile,
            ProjectDirError::Io,
            ProjectDirError::InvalidManifest,
            ProjectDirError::InvalidProfile,
        ] {
            let rendered = format!("{err}");
            for sentinel in SENTINELS {
                assert!(
                    !rendered.to_lowercase().contains(sentinel),
                    "project-dir error must stay secret-free, found {sentinel:?} in {rendered:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
}
