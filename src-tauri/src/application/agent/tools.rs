//! Safe workspace tool execution: shell and filesystem.
//!
//! Provides the six native agent tools behind a central [`ToolRegistry`].
//! All filesystem access is confined to `workspace_root` via path
//! canonicalization / lexical normalisation. Shell execution is bounded by a
//! hard timeout, bounded output capture, a bounded reader-drain grace, and
//! final output truncation to protect the LLM context; no code path can block
//! indefinitely (a detached grandchild holding a pipe cannot stall the tool).

mod definitions;
mod executor;
mod output;

pub(crate) use executor::ToolError;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Central dispatcher for the six native workspace tools.
pub(crate) struct ToolRegistry;

#[cfg(test)]
pub(crate) mod test_support {
    use crate::application::execution::ToolCall;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) static COUNTER: AtomicUsize = AtomicUsize::new(0);

    pub(crate) fn temp_workspace() -> PathBuf {
        let base = std::env::temp_dir();
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = base.join(format!(
            "nexora-tools-test-{}-{}-{}",
            std::process::id(),
            id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp workspace");
        canonical_workspace(&dir)
    }

    /// Canonicalize a freshly created temp workspace so the returned root is
    /// already in the form the file tools compare against. On Windows the
    /// temp dir can sit behind a junction, 8.3 short name, or an alternate
    /// separator/drive-letter/case spelling (notably on CI runners); the
    /// tools' canonical re-check then rejects the non-canonical root with
    /// `PathTraversal`. Resolving once here keeps every downstream
    /// `resolve_path`/`is_within_workspace` comparison canonical-vs-canonical.
    /// The `\\?\` verbatim prefix is stripped so paths stay readable and
    /// comparable with non-verbatim joins.
    pub(crate) fn canonical_workspace(dir: &Path) -> PathBuf {
        let canon = dir.canonicalize().expect("canonicalize temp workspace");
        let text = canon.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        canon
    }

    #[allow(clippy::needless_pass_by_value)] // JSON literals read best at call sites
    pub(crate) fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.to_string(),
            arguments: args.to_string(),
            thought_signature: None,
        }
    }
}
