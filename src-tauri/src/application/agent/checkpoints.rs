//! Disk-backed content-addressed pre-mutation file checkpoints & revert
//! engine (Stage 2 PR 2B, SRS §18, ARCHITECTURE.md, DATABASE.md).
//!
//! Raw pre-mutation file bytes live exclusively under
//! `<app_data>/checkpoints/<sha256>` (never inside the user workspace and never
//! stored inline in `SQLite`). Pre-mutation capture is fail-closed: if reading
//! the existing target, writing its verified backup blob, or inserting the
//! `file_checkpoints` row fails, the workspace write/edit is aborted before any
//! bytes touch disk.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::tools::path_safety::{
    canonical_root, check_prefixes_for_escape, check_private, path_to_comparable_string,
    resolve_path, secure_dir, secure_open, verify_existing_within,
};
use super::tools::ToolError;
use crate::infrastructure::database::{Database, DatabaseError};
use crate::infrastructure::repository::agent_runs::AgentRunRepository;
use crate::infrastructure::repository::file_checkpoints::{
    FileCheckpoint, FileCheckpointRepository, NewFileCheckpoint,
};

static TMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// Secret-free typed error returned by checkpoint capture, GC, and revert.
#[derive(Debug)]
pub(crate) enum CheckpointError {
    NotFound,
    ExternalConflict,
    BlobCorrupt,
    BlobMissing,
    InvalidPath,
    PathEscape,
    WorkspaceUnavailable,
    StoreInsideWorkspace,
    Database(DatabaseError),
    Io(std::io::ErrorKind),
}

impl From<DatabaseError> for CheckpointError {
    fn from(value: DatabaseError) -> Self {
        Self::Database(value)
    }
}

impl From<std::io::Error> for CheckpointError {
    fn from(value: std::io::Error) -> Self {
        match value.kind() {
            std::io::ErrorKind::NotFound => Self::BlobMissing,
            kind => Self::Io(kind),
        }
    }
}

impl From<ToolError> for CheckpointError {
    fn from(value: ToolError) -> Self {
        match value {
            ToolError::PathTraversal(_) => Self::PathEscape,
            ToolError::InvalidArguments(_) => Self::InvalidPath,
            ToolError::UnknownTool(_)
            | ToolError::Io(_)
            | ToolError::Timeout(_)
            | ToolError::Cancelled
            | ToolError::ConsentInvalidated => Self::Io(std::io::ErrorKind::Other),
        }
    }
}

impl CheckpointError {
    pub(crate) const fn summary(&self) -> &'static str {
        match self {
            Self::NotFound => "checkpoint not found",
            Self::ExternalConflict => "target file modified externally since checkpoint",
            Self::BlobCorrupt => "checkpoint backup failed integrity check",
            Self::BlobMissing => "checkpoint backup blob missing",
            Self::InvalidPath => "invalid checkpoint target path",
            Self::PathEscape => "checkpoint target escapes workspace boundaries",
            Self::WorkspaceUnavailable => "checkpoint workspace directory unavailable",
            Self::StoreInsideWorkspace => "checkpoint store cannot reside inside workspace",
            Self::Database(_) => "checkpoint persistence error",
            Self::Io(_) => "checkpoint file system error",
        }
    }
}

/// Content-addressed backup store rooted at `<app_data>/checkpoints/`.
#[derive(Debug, Clone)]
pub(crate) struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    /// Opens (and creates if needed) the content-addressed checkpoint
    /// directory, verifying that the directory itself is not a symlink.
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self, CheckpointError> {
        let raw = root.as_ref();
        if raw.as_os_str().is_empty() {
            return Err(CheckpointError::InvalidPath);
        }
        match std::fs::symlink_metadata(raw) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(CheckpointError::PathEscape);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(raw).map_err(|e| CheckpointError::Io(e.kind()))?;
                let meta =
                    std::fs::symlink_metadata(raw).map_err(|e| CheckpointError::Io(e.kind()))?;
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(CheckpointError::PathEscape);
                }
            }
            Err(err) => return Err(CheckpointError::Io(err.kind())),
        }
        let canon = std::fs::canonicalize(raw).map_err(|e| CheckpointError::Io(e.kind()))?;
        Ok(Self { root: canon })
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Verifies that the checkpoint store directory exists (non-symlink) and
    /// is disjoint from `workspace_root` (neither inside it nor enclosing it).
    pub(crate) fn verify_for_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<(), CheckpointError> {
        let store_meta = std::fs::symlink_metadata(&self.root)
            .map_err(|_| CheckpointError::WorkspaceUnavailable)?;
        if store_meta.file_type().is_symlink() || !store_meta.is_dir() {
            return Err(CheckpointError::PathEscape);
        }
        let canon_ws = std::fs::canonicalize(workspace_root)
            .map_err(|_| CheckpointError::WorkspaceUnavailable)?;
        if !canon_ws.is_dir() {
            return Err(CheckpointError::WorkspaceUnavailable);
        }

        let store_cmp = comparable_posix(&self.root);
        let ws_cmp = comparable_posix(&canon_ws);
        if paths_overlap(&store_cmp, &ws_cmp) || paths_overlap(&ws_cmp, &store_cmp) {
            return Err(CheckpointError::StoreInsideWorkspace);
        }
        Ok(())
    }

    /// Writes `bytes` content-addressed under `<root>/<sha256>`, deduplicating
    /// and verifying integrity if the digest already exists.
    pub(crate) fn write_blob(&self, bytes: &[u8]) -> Result<(String, i64), CheckpointError> {
        let sha256 = sha256_hex(bytes);
        let size = i64::try_from(bytes.len()).map_err(|_| CheckpointError::InvalidPath)?;
        let target = self.root.join(&sha256);

        match std::fs::symlink_metadata(&target) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_file() {
                    return Err(CheckpointError::PathEscape);
                }
                let existing = std::fs::read(&target).map_err(|e| CheckpointError::Io(e.kind()))?;
                if sha256_hex(&existing) != sha256 {
                    return Err(CheckpointError::BlobCorrupt);
                }
                return Ok((sha256, size));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(CheckpointError::Io(err.kind())),
        }

        let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(".{sha256}.{}.{nonce}.tmp", std::process::id());
        let tmp_path = self.root.join(tmp_name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        let write_result = (|| -> std::io::Result<()> {
            let mut file = opts.open(&tmp_path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(err) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(CheckpointError::Io(err.kind()));
        }

        if let Err(rename_err) = std::fs::rename(&tmp_path, &target) {
            let _ = std::fs::remove_file(&tmp_path);
            // Concurrent writers may have raced to write the exact same digest.
            if let Ok(meta) = std::fs::symlink_metadata(&target) {
                if !meta.file_type().is_symlink() && meta.is_file() {
                    let existing =
                        std::fs::read(&target).map_err(|e| CheckpointError::Io(e.kind()))?;
                    if sha256_hex(&existing) == sha256 {
                        return Ok((sha256, size));
                    }
                    return Err(CheckpointError::BlobCorrupt);
                }
            }
            return Err(CheckpointError::Io(rename_err.kind()));
        }

        Ok((sha256, size))
    }

    /// Reads `<root>/<expected_sha256>` and verifies its digest before returning
    /// bytes.
    pub(crate) fn read_verified_blob(
        &self,
        expected_sha256: &str,
    ) -> Result<Vec<u8>, CheckpointError> {
        if !is_valid_sha256_hex(expected_sha256) {
            return Err(CheckpointError::BlobCorrupt);
        }
        let target = self.root.join(expected_sha256);
        let meta = std::fs::symlink_metadata(&target).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                CheckpointError::BlobMissing
            } else {
                CheckpointError::Io(err.kind())
            }
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(CheckpointError::PathEscape);
        }
        let bytes = std::fs::read(&target).map_err(|e| CheckpointError::Io(e.kind()))?;
        if sha256_hex(&bytes) != expected_sha256 {
            return Err(CheckpointError::BlobCorrupt);
        }
        Ok(bytes)
    }

    /// Garbage-collects unreferenced backup blobs from `<root>`.
    ///
    /// When `candidates` is `Some`, each candidate digest is checked via
    /// `is_blob_referenced` and deleted only if no remaining `file_checkpoints`
    /// row points to it. When `None`, scans the store directory against all
    /// remaining referenced digests (used after clearing all data).
    pub(crate) fn gc_unreferenced_blobs(
        &self,
        db: &Database,
        candidates: Option<&[String]>,
    ) -> Result<usize, CheckpointError> {
        let repo = FileCheckpointRepository::new(db);
        let mut removed = 0usize;

        if let Some(hashes) = candidates {
            for sha in hashes {
                if !is_valid_sha256_hex(sha) {
                    continue;
                }
                if !repo.is_blob_referenced(sha)? && remove_regular_blob(&self.root.join(sha)) {
                    removed += 1;
                }
            }
        } else {
            let referenced: HashSet<String> = repo.all_pre_blobs()?.into_iter().collect();
            let entries = match std::fs::read_dir(&self.root) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(err) => return Err(CheckpointError::Io(err.kind())),
            };
            for entry in entries {
                let Ok(entry) = entry else { continue };
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_symlink() || !file_type.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                if is_valid_sha256_hex(name_str)
                    && !referenced.contains(name_str)
                    && remove_regular_blob(&entry.path())
                {
                    removed += 1;
                }
            }
        }

        Ok(removed)
    }
}

/// Active run binding used by tool dispatch & executor to capture pre- and
/// post-mutation file checkpoints and link them to `agent_steps`.
#[derive(Clone)]
pub(crate) struct CheckpointRunContext {
    db: Arc<Database>,
    store: CheckpointStore,
    conversation_id: i64,
    run_id: i64,
}

impl CheckpointRunContext {
    pub(crate) const fn new(
        db: Arc<Database>,
        store: CheckpointStore,
        conversation_id: i64,
        run_id: i64,
    ) -> Self {
        Self {
            db,
            store,
            conversation_id,
            run_id,
        }
    }

    pub(crate) const fn store(&self) -> &CheckpointStore {
        &self.store
    }

    /// Captures pre-mutation state BEFORE any workspace directory or file is
    /// modified. Fails closed if the store is unverified, blob write fails, or
    /// `SQLite` insert fails.
    pub(crate) fn capture_pre(
        &self,
        turn_index: usize,
        workspace_root: &Path,
        requested_path: &str,
        tool_name: &str,
        pre_existed: bool,
        pre_bytes: &[u8],
    ) -> Result<i64, CheckpointError> {
        self.store.verify_for_workspace(workspace_root)?;
        let (ws_display, rel_posix) =
            normalized_workspace_and_relative(workspace_root, requested_path)?;
        let turn_i64 = i64::try_from(turn_index).unwrap_or(i64::MAX);

        let (pre_sha, pre_size) = if pre_existed {
            let (sha, size) = self.store.write_blob(pre_bytes)?;
            (Some(sha), Some(size))
        } else {
            (None, None)
        };

        let id =
            FileCheckpointRepository::new(&self.db).insert_pre_mutation(&NewFileCheckpoint {
                conversation_id: self.conversation_id,
                run_id: self.run_id,
                step_id: None,
                turn_index: turn_i64,
                workspace_root: &ws_display,
                relative_path: &rel_posix,
                tool_name,
                pre_existed,
                pre_blob_sha256: pre_sha.as_deref(),
                pre_size_bytes: pre_size,
            })?;
        Ok(id)
    }

    /// Records the post-mutation SHA-256 and size for a captured checkpoint.
    pub(crate) fn complete_post(
        &self,
        checkpoint_id: i64,
        post_bytes: &[u8],
    ) -> Result<(), CheckpointError> {
        let sha = sha256_hex(post_bytes);
        let size = i64::try_from(post_bytes.len()).map_err(|_| CheckpointError::InvalidPath)?;
        FileCheckpointRepository::new(&self.db).record_post_mutation(checkpoint_id, &sha, size)?;
        Ok(())
    }

    /// Associates a checkpoint row with the persisted `agent_steps.id` row.
    pub(crate) fn link_step(
        &self,
        checkpoint_id: i64,
        step_id: i64,
    ) -> Result<(), CheckpointError> {
        FileCheckpointRepository::new(&self.db).link_step(checkpoint_id, step_id)?;
        Ok(())
    }
}

/// Per-tool-call capture hook passed into `ToolRegistry` so pre-mutation
/// capture happens immediately before disk changes and returns the captured row
/// ID for `agent_steps` linkage.
pub(crate) struct CheckpointHook<'a> {
    ctx: &'a CheckpointRunContext,
    turn_index: usize,
    captured_id: Option<i64>,
}

impl<'a> CheckpointHook<'a> {
    pub(crate) const fn new(ctx: &'a CheckpointRunContext, turn_index: usize) -> Self {
        Self {
            ctx,
            turn_index,
            captured_id: None,
        }
    }

    pub(crate) fn capture_pre(
        &mut self,
        workspace_root: &Path,
        requested_path: &str,
        tool_name: &str,
        pre_existed: bool,
        pre_bytes: &[u8],
    ) -> Result<i64, CheckpointError> {
        let id = self.ctx.capture_pre(
            self.turn_index,
            workspace_root,
            requested_path,
            tool_name,
            pre_existed,
            pre_bytes,
        )?;
        self.captured_id = Some(id);
        Ok(id)
    }

    pub(crate) fn complete_post(
        &self,
        checkpoint_id: i64,
        post_bytes: &[u8],
    ) -> Result<(), CheckpointError> {
        self.ctx.complete_post(checkpoint_id, post_bytes)
    }

    pub(crate) const fn captured_id(&self) -> Option<i64> {
        self.captured_id
    }
}

/// Lists all recorded file checkpoints for a run ordered `(turn_index ASC, id ASC)`.
pub(crate) fn list_run_checkpoints(
    db: &Database,
    run_id: i64,
) -> Result<Vec<FileCheckpoint>, CheckpointError> {
    if AgentRunRepository::new(db).read_run(run_id)?.is_none() {
        return Err(CheckpointError::NotFound);
    }
    Ok(FileCheckpointRepository::new(db).list_by_run(run_id)?)
}

/// Reverts a single file checkpoint. Fails closed with
/// [`CheckpointError::ExternalConflict`] if current disk content diverges from
/// `post_blob_sha256`, unless `force` is explicitly true.
pub(crate) fn revert_file_checkpoint(
    db: &Database,
    store: &CheckpointStore,
    checkpoint_id: i64,
    force: bool,
) -> Result<FileCheckpoint, CheckpointError> {
    let repo = FileCheckpointRepository::new(db);
    let row = repo.read(checkpoint_id)?.ok_or(CheckpointError::NotFound)?;
    if row.reverted_at.is_some() {
        return Ok(row);
    }
    revert_rows_atomically(db, store, std::slice::from_ref(&row), force)?;
    repo.read(checkpoint_id)?.ok_or(CheckpointError::NotFound)
}

/// Reverts all un-reverted checkpoints in `(run_id, turn_index)` in reverse
/// creation order (`id DESC`). Validates all workspace targets, backup blobs,
/// and external-modification checks before modifying any disk file.
pub(crate) fn revert_turn_checkpoints(
    db: &Database,
    store: &CheckpointStore,
    run_id: i64,
    turn_index: i64,
    force: bool,
) -> Result<Vec<FileCheckpoint>, CheckpointError> {
    if AgentRunRepository::new(db).read_run(run_id)?.is_none() {
        return Err(CheckpointError::NotFound);
    }
    if turn_index < 0 {
        return Err(CheckpointError::InvalidPath);
    }
    let repo = FileCheckpointRepository::new(db);
    let rows = repo.list_by_turn_desc(run_id, turn_index)?;
    let active: Vec<FileCheckpoint> = rows
        .into_iter()
        .filter(|row| row.reverted_at.is_none())
        .collect();
    if !active.is_empty() {
        revert_rows_atomically(db, store, &active, force)?;
    }
    Ok(repo.list_by_run(run_id)?)
}

struct PreparedRevert {
    row: FileCheckpoint,
    canon_ws: PathBuf,
    resolved: PathBuf,
    pre_bytes: Option<Vec<u8>>,
}

/// Phase 1 validates every target, symlink boundary, backup blob digest, and
/// external modification conflict BEFORE Phase 2 mutates any file on disk.
fn revert_rows_atomically(
    db: &Database,
    store: &CheckpointStore,
    active_desc: &[FileCheckpoint],
    force: bool,
) -> Result<(), CheckpointError> {
    let mut prepared = Vec::with_capacity(active_desc.len());
    let mut checked_latest_targets: HashSet<(String, String)> = HashSet::new();
    let mut verified_workspaces: HashSet<PathBuf> = HashSet::new();

    for row in active_desc {
        let ws_raw = Path::new(&row.workspace_root);
        let canon_ws =
            std::fs::canonicalize(ws_raw).map_err(|_| CheckpointError::WorkspaceUnavailable)?;
        if verified_workspaces.insert(canon_ws.clone()) {
            store.verify_for_workspace(&canon_ws)?;
        }

        let resolved = resolve_target_for_revert(&canon_ws, &row.relative_path)?;
        let pre_bytes = if row.pre_existed {
            let expected_sha = row
                .pre_blob_sha256
                .as_deref()
                .ok_or(CheckpointError::BlobCorrupt)?;
            Some(store.read_verified_blob(expected_sha)?)
        } else {
            None
        };

        let target_key = (
            comparable_posix(&canon_ws),
            row.relative_path.replace('\\', "/"),
        );
        // Only the latest un-reverted checkpoint for a given target reflects
        // the current on-disk file state when unwinding a multi-step turn.
        if checked_latest_targets.insert(target_key) {
            verify_no_external_conflict(&canon_ws, &resolved, row, force)?;
        }

        prepared.push(PreparedRevert {
            row: row.clone(),
            canon_ws,
            resolved,
            pre_bytes,
        });
    }

    let repo = FileCheckpointRepository::new(db);
    let mut blobs_by_hash: HashMap<String, Vec<u8>> = HashMap::new();
    for item in &prepared {
        if let (Some(sha), Some(bytes)) = (&item.row.pre_blob_sha256, &item.pre_bytes) {
            blobs_by_hash.insert(sha.clone(), bytes.clone());
        }
    }

    for item in prepared {
        // Re-check right before writing/removing in case of concurrent symlink swap.
        let resolved = resolve_target_for_revert(&item.canon_ws, &item.row.relative_path)?;
        if item.row.pre_existed {
            let bytes = item
                .pre_bytes
                .as_deref()
                .or_else(|| {
                    item.row
                        .pre_blob_sha256
                        .as_deref()
                        .and_then(|s| blobs_by_hash.get(s).map(Vec::as_slice))
                })
                .ok_or(CheckpointError::BlobCorrupt)?;
            let parent = resolved.parent().ok_or(CheckpointError::InvalidPath)?;
            let _ = secure_dir(&item.canon_ws, parent, true)?;
            let mut file = secure_open(&item.canon_ws, &resolved, true)?;
            file.set_len(0).map_err(|e| CheckpointError::Io(e.kind()))?;
            file.write_all(bytes)
                .map_err(|e| CheckpointError::Io(e.kind()))?;
            file.sync_all().map_err(|e| CheckpointError::Io(e.kind()))?;
        } else {
            match std::fs::symlink_metadata(&resolved) {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(CheckpointError::PathEscape);
                    }
                    if !meta.is_file() {
                        return Err(CheckpointError::InvalidPath);
                    }
                    let parent = resolved.parent().ok_or(CheckpointError::InvalidPath)?;
                    let leaf = resolved.file_name().ok_or(CheckpointError::InvalidPath)?;
                    let dir = secure_dir(&item.canon_ws, parent, false)?;
                    match dir.remove_file(Path::new(leaf)) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound && force => {}
                        Err(err) => return Err(CheckpointError::Io(err.kind())),
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    if !force && item.row.post_blob_sha256.is_some() {
                        return Err(CheckpointError::ExternalConflict);
                    }
                }
                Err(err) => return Err(CheckpointError::Io(err.kind())),
            }
        }
        repo.mark_reverted(item.row.id, None)?;
    }

    Ok(())
}

fn verify_no_external_conflict(
    canon_ws: &Path,
    resolved: &Path,
    row: &FileCheckpoint,
    force: bool,
) -> Result<(), CheckpointError> {
    match std::fs::symlink_metadata(resolved) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(CheckpointError::PathEscape);
            }
            if !meta.is_file() {
                return Err(CheckpointError::InvalidPath);
            }
            if force {
                return Ok(());
            }
            let Some(expected_post) = row.post_blob_sha256.as_deref() else {
                return Err(CheckpointError::ExternalConflict);
            };
            let mut file = secure_open(canon_ws, resolved, false)?;
            let mut current = Vec::new();
            file.read_to_end(&mut current)
                .map_err(|e| CheckpointError::Io(e.kind()))?;
            if sha256_hex(&current) != expected_post {
                return Err(CheckpointError::ExternalConflict);
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if force {
                Ok(())
            } else {
                Err(CheckpointError::ExternalConflict)
            }
        }
        Err(err) => Err(CheckpointError::Io(err.kind())),
    }
}

fn resolve_target_for_revert(
    canon_ws: &Path,
    relative_path: &str,
) -> Result<PathBuf, CheckpointError> {
    check_private(canon_ws)?;
    let trimmed = relative_path.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
    {
        return Err(CheckpointError::InvalidPath);
    }

    let normalized_rel = trimmed.replace('\\', "/");
    let mut joined = canon_ws.to_path_buf();
    let mut component_count = 0usize;
    for part in normalized_rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(CheckpointError::PathEscape),
            seg => {
                // Reject Windows drive-letter prefix masquerading as relative.
                if component_count == 0 && seg.as_bytes().get(1) == Some(&b':') {
                    return Err(CheckpointError::InvalidPath);
                }
                joined.push(seg);
                component_count += 1;
            }
        }
    }
    if component_count == 0 {
        return Err(CheckpointError::InvalidPath);
    }

    check_private(&joined)?;
    check_prefixes_for_escape(canon_ws, canon_ws, &joined, relative_path)?;
    match std::fs::symlink_metadata(&joined) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(CheckpointError::PathEscape),
        Ok(_) => verify_existing_within(canon_ws, canon_ws, &joined, relative_path)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(CheckpointError::Io(err.kind())),
    }
    Ok(joined)
}

fn normalized_workspace_and_relative(
    workspace_root: &Path,
    requested_path: &str,
) -> Result<(String, String), CheckpointError> {
    let canon_ws = canonical_root(workspace_root);
    let resolved = resolve_path(workspace_root, requested_path)?;
    check_prefixes_for_escape(workspace_root, &canon_ws, &resolved, requested_path)?;
    if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
        if meta.file_type().is_symlink() {
            return Err(CheckpointError::PathEscape);
        }
        verify_existing_within(workspace_root, &canon_ws, &resolved, requested_path)?;
    }

    // Resolve the deepest existing ancestor to its canonical path so any inner
    // symlink within the workspace aligns directly with `canon_ws`.
    let canonical_resolved = canonicalize_with_tail(&resolved)?;
    let rel_path = canonical_resolved
        .strip_prefix(&canon_ws)
        .map_err(|_| CheckpointError::PathEscape)?;

    let mut parts = Vec::new();
    for comp in rel_path.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_str().ok_or(CheckpointError::InvalidPath)?;
                parts.push(s);
            }
            _ => return Err(CheckpointError::InvalidPath),
        }
    }
    if parts.is_empty() {
        return Err(CheckpointError::InvalidPath);
    }

    Ok((display_canonical(&canon_ws), parts.join("/")))
}

fn canonicalize_with_tail(path: &Path) -> Result<PathBuf, CheckpointError> {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or(CheckpointError::InvalidPath)?
            .to_os_string();
        tail.push(name);
        if !existing.pop() {
            return Err(CheckpointError::WorkspaceUnavailable);
        }
    }
    let mut base = std::fs::canonicalize(&existing).map_err(|e| CheckpointError::Io(e.kind()))?;
    for seg in tail.into_iter().rev() {
        base.push(seg);
    }
    Ok(base)
}

fn display_canonical(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.into_owned()
    }
}

fn comparable_posix(path: &Path) -> String {
    path_to_comparable_string(path)
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn paths_overlap(inner: &str, outer: &str) -> bool {
    inner == outer
        || inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn remove_regular_blob(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_symlink() && meta.is_file() => {
            std::fs::remove_file(path).is_ok()
        }
        _ => false,
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn is_valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::database::in_memory_database;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> std::io::Result<Self> {
            let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("nexora-cp-test-{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn setup_env() -> (TempDir, TempDir, Arc<Database>, CheckpointStore, i64, i64) {
        let app_data = TempDir::new().expect("app_data temp dir");
        let workspace = TempDir::new().expect("workspace temp dir");
        let store = CheckpointStore::open(app_data.path().join("checkpoints")).expect("open store");
        let db = Arc::new(in_memory_database());
        let conv_id = {
            let conn = db.lock().expect("lock db");
            conn.execute(
                "INSERT INTO conversations (title, status) VALUES ('cp-test', 'active')",
                [],
            )
            .expect("insert conversation");
            conn.last_insert_rowid()
        };
        let run_id = AgentRunRepository::new(&db)
            .create_run(Some(conv_id), "test-model", "full_autonomous")
            .expect("create run");
        (app_data, workspace, db, store, conv_id, run_id)
    }

    #[test]
    fn blob_store_rejects_store_inside_workspace_and_deduplicates_bytes() {
        let (_app, ws, _db, store, _conv, _run) = setup_env();
        store
            .verify_for_workspace(ws.path())
            .expect("disjoint checkpoint store passes verification");

        let nested = CheckpointStore::open(ws.path().join(".nexora-checkpoints"))
            .expect("open nested store");
        assert!(matches!(
            nested.verify_for_workspace(ws.path()),
            Err(CheckpointError::StoreInsideWorkspace)
        ));

        let payload = b"hello\x00world\xffbinary";
        let (sha1, size1) = store.write_blob(payload).expect("write first blob");
        let (sha2, size2) = store.write_blob(payload).expect("write duplicate blob");
        assert_eq!(sha1, sha2);
        assert_eq!(size1, size2);
        assert_eq!(
            store.read_verified_blob(&sha1).expect("read verified"),
            payload
        );

        // Corrupting the blob fails closed on read and re-write.
        std::fs::write(store.root().join(&sha1), b"tampered").expect("tamper blob");
        assert!(matches!(
            store.read_verified_blob(&sha1),
            Err(CheckpointError::BlobCorrupt)
        ));
        assert!(matches!(
            store.write_blob(payload),
            Err(CheckpointError::BlobCorrupt)
        ));
    }

    #[test]
    fn revert_single_and_turn_handles_creation_overwrite_non_utf8_and_conflicts() {
        let (_app, ws, db, store, conv_id, run_id) = setup_env();
        let ctx = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv_id, run_id);

        // Turn 0, step A: overwrite existing non-UTF8 file.
        let existing_path = ws.path().join("bin.dat");
        let original_bytes = vec![0x00, 0xff, 0x10, 0x41, 0x42];
        std::fs::write(&existing_path, &original_bytes).expect("seed existing");
        let cp_existing = ctx
            .capture_pre(0, ws.path(), "bin.dat", "write_file", true, &original_bytes)
            .expect("capture existing");
        let updated_bytes = vec![0x43, 0x44, 0xff];
        std::fs::write(&existing_path, &updated_bytes).expect("mutate existing");
        ctx.complete_post(cp_existing, &updated_bytes)
            .expect("record post");

        // Turn 0, step B: create brand-new nested file.
        let created_rel = "sub/created.txt";
        let cp_created = ctx
            .capture_pre(0, ws.path(), created_rel, "write_file", false, &[])
            .expect("capture created");
        std::fs::create_dir_all(ws.path().join("sub")).expect("mkdir sub");
        std::fs::write(ws.path().join(created_rel), b"new file body").expect("write created");
        ctx.complete_post(cp_created, b"new file body")
            .expect("post created");

        // External modification on existing_path blocks revert when force=false.
        std::fs::write(&existing_path, b"external edit").expect("external edit");
        assert!(matches!(
            revert_turn_checkpoints(&db, &store, run_id, 0, false),
            Err(CheckpointError::ExternalConflict)
        ));
        // Because turn revert is conflict-atomic in phase 1, created.txt is still present.
        assert!(ws.path().join(created_rel).exists());

        // Passing force=true reverts both files cleanly in reverse order.
        let rows = revert_turn_checkpoints(&db, &store, run_id, 0, true).expect("force revert");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.reverted_at.is_some()));
        assert_eq!(
            std::fs::read(&existing_path).expect("read restored"),
            original_bytes
        );
        assert!(!ws.path().join(created_rel).exists());
    }

    #[test]
    fn revert_turn_handles_multiple_edits_to_same_file_and_partial_single_revert() {
        let (_app, ws, db, store, conv_id, run_id) = setup_env();
        let ctx = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv_id, run_id);
        let file = ws.path().join("app.txt");
        std::fs::write(&file, b"v0").expect("v0");

        let cp1 = ctx
            .capture_pre(0, ws.path(), "app.txt", "write_file", true, b"v0")
            .expect("cp1");
        std::fs::write(&file, b"v1").expect("v1");
        ctx.complete_post(cp1, b"v1").expect("post1");

        let cp2 = ctx
            .capture_pre(0, ws.path(), "app.txt", "edit_file", true, b"v1")
            .expect("cp2");
        std::fs::write(&file, b"v2").expect("v2");
        ctx.complete_post(cp2, b"v2").expect("post2");

        // Trying to revert cp1 directly while file is still v2 fails with conflict.
        assert!(matches!(
            revert_file_checkpoint(&db, &store, cp1, false),
            Err(CheckpointError::ExternalConflict)
        ));

        // Reverting cp2 puts file back at v1.
        let r2 = revert_file_checkpoint(&db, &store, cp2, false).expect("revert cp2");
        assert!(r2.reverted_at.is_some());
        assert_eq!(std::fs::read(&file).expect("read v1"), b"v1");

        // Now Turn revert finishes unwinding cp1 from v1 back to v0 cleanly.
        let all = revert_turn_checkpoints(&db, &store, run_id, 0, false).expect("revert turn");
        assert!(all.iter().all(|r| r.reverted_at.is_some()));
        assert_eq!(std::fs::read(&file).expect("read v0"), b"v0");
    }

    #[test]
    fn gc_deletes_unreferenced_blobs_and_retains_shared_blobs() {
        let (_app, ws, db, store, conv1, run1) = setup_env();
        let conv2 = {
            let conn = db.lock().expect("lock");
            conn.execute(
                "INSERT INTO conversations (title, status) VALUES ('c2', 'active')",
                [],
            )
            .expect("conv2");
            conn.last_insert_rowid()
        };
        let run2 = AgentRunRepository::new(&db)
            .create_run(Some(conv2), "m", "full_autonomous")
            .expect("run2");

        let shared_ctx1 = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv1, run1);
        let shared_ctx2 = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv2, run2);

        let shared_bytes = b"shared pre-mutation content";
        let unique_bytes = b"unique conv1 content";
        let (shared_sha, _) = store.write_blob(shared_bytes).expect("shared sha");
        let (unique_sha, _) = store.write_blob(unique_bytes).expect("unique sha");

        std::fs::write(ws.path().join("s.txt"), shared_bytes).expect("seed");
        std::fs::write(ws.path().join("u.txt"), unique_bytes).expect("seed");

        let _ = shared_ctx1
            .capture_pre(0, ws.path(), "s.txt", "write_file", true, shared_bytes)
            .expect("cp shared 1");
        let _ = shared_ctx1
            .capture_pre(0, ws.path(), "u.txt", "write_file", true, unique_bytes)
            .expect("cp unique 1");
        let _ = shared_ctx2
            .capture_pre(0, ws.path(), "s.txt", "write_file", true, shared_bytes)
            .expect("cp shared 2");

        let candidates = FileCheckpointRepository::new(&db)
            .collect_pre_blobs_for_conversation(conv1)
            .expect("candidates");
        {
            let conn = db.lock().expect("lock");
            conn.execute("DELETE FROM conversations WHERE id = ?1", [conv1])
                .expect("delete conv1");
        }
        let deleted = store
            .gc_unreferenced_blobs(&db, Some(&candidates))
            .expect("gc");
        assert_eq!(deleted, 1);
        assert!(!store.root().join(&unique_sha).exists());
        assert!(store.root().join(&shared_sha).exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn revert_rejects_symlink_targets_even_with_force() {
        let (_app, ws, db, store, conv_id, run_id) = setup_env();
        let outside_dir = TempDir::new().expect("outside dir");
        let outside_file = outside_dir.path().join("secret.txt");
        std::fs::write(&outside_file, b"keep intact").expect("seed outside");

        let ctx = CheckpointRunContext::new(Arc::clone(&db), store.clone(), conv_id, run_id);
        let link_target = ws.path().join("link.txt");
        std::fs::write(&link_target, b"original").expect("seed original");
        let cp = ctx
            .capture_pre(0, ws.path(), "link.txt", "write_file", true, b"original")
            .expect("capture");
        std::fs::write(&link_target, b"edited").expect("edit");
        ctx.complete_post(cp, b"edited").expect("post");

        std::fs::remove_file(&link_target).expect("remove regular");

        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside_file, &link_target);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&outside_file, &link_target);

        let Ok(()) = linked else {
            // Unprivileged Windows environments without Developer Mode skip symlink creation.
            return;
        };

        assert!(matches!(
            revert_file_checkpoint(&db, &store, cp, true),
            Err(CheckpointError::PathEscape)
        ));
        assert_eq!(
            std::fs::read(&outside_file).expect("outside untouched"),
            b"keep intact"
        );
    }

    #[test]
    fn approval_matrix_honors_verified_checkpoints_and_sensitive_guards() {
        use crate::application::agent::access::{AccessContext, AccessProfile};
        use crate::application::agent::approval::{ApprovalGate, AutonomyMode};
        use crate::application::execution::ToolCall;

        let dir = TempDir::new().expect("temp dir");
        let ws_raw = dir.path().join("ws");
        std::fs::create_dir_all(&ws_raw).expect("ws dir");
        let ws = std::fs::canonicalize(&ws_raw).expect("canon ws");
        let store = CheckpointStore::open(dir.path().join("checkpoints")).expect("open store");
        let access = AccessContext::fixed(AccessProfile::Standard);
        let gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(access.clone())
            .with_checkpoints(Some(store.clone()));

        // Before bind_workspace verifies the store, checkpoints are not yet trusted.
        let ordinary_write = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs", "content": "fn main() {}"})
                .to_string(),
            thought_signature: None,
        };
        assert!(!gate.has_verified_checkpoints());
        assert!(gate.needs_approval(&ordinary_write));

        // After workspace binding verifies the external store, ordinary workspace writes auto-run.
        gate.bind_workspace(&ws);
        assert!(gate.has_verified_checkpoints());
        assert!(access.checkpoints_enabled());
        assert!(!gate.needs_approval(&ordinary_write));

        let ordinary_edit = ToolCall {
            id: "c2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"})
                .to_string(),
            thought_signature: None,
        };
        assert!(!gate.needs_approval(&ordinary_edit));

        // Sensitive paths and shell commands still strictly require approval.
        for sensitive in [
            ".env",
            ".env.local",
            "config/.env.production",
            "certs/server.pem",
            "keys/private.key",
            ".ssh/id_rsa",
            ".git/hooks/pre-commit",
            ".git/config",
            ".nexora/nexora.json",
            "../outside.txt",
        ] {
            let call = ToolCall {
                id: format!("sens-{sensitive}"),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": sensitive, "content": "x"}).to_string(),
                thought_signature: None,
            };
            assert!(
                gate.needs_approval(&call),
                "sensitive/outside path {sensitive} must still require approval"
            );
        }

        let shell = ToolCall {
            id: "sh".into(),
            name: "execute_command".into(),
            arguments: serde_json::json!({"command": "echo hi"}).to_string(),
            thought_signature: None,
        };
        assert!(gate.needs_approval(&shell));

        // SemiAutonomous and Supervised modes still prompt even with verified checkpoints.
        let semi_gate = ApprovalGate::new(AutonomyMode::SemiAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard))
            .with_checkpoints(Some(store.clone()));
        semi_gate.bind_workspace(&ws);
        assert!(semi_gate.needs_approval(&ordinary_write));

        let sup_access = AccessContext::fixed(AccessProfile::Supervised);
        let sup_gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(sup_access)
            .with_checkpoints(Some(store.clone()));
        sup_gate.bind_workspace(&ws);
        assert!(sup_gate.needs_approval(&ordinary_write));

        let ro_access = AccessContext::fixed(AccessProfile::ReadOnly);
        let ro_gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(ro_access.clone())
            .with_checkpoints(Some(store));
        ro_gate.bind_workspace(&ws);
        assert!(!ro_access.allows(&ordinary_write));

        // A checkpoint store placed inside the workspace fails verification.
        let inside_store = CheckpointStore::open(ws.join("ckpt-in-ws")).expect("inside store");
        let inside_gate = ApprovalGate::new(AutonomyMode::FullAutonomous)
            .with_access(AccessContext::fixed(AccessProfile::Standard))
            .with_checkpoints(Some(inside_store));
        inside_gate.bind_workspace(&ws);
        assert!(!inside_gate.has_verified_checkpoints());
        assert!(inside_gate.needs_approval(&ordinary_write));
    }
}
