use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use git2::{ObjectType, Oid, Patch, Repository, Tree};

use super::io::{bytes_at, open_absolute, path_label, relative_path};
use super::workspace::{Workspace, MAX_FILE_BYTES};
use super::{secret_free, ContextError};

const MAX_PACK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LOOSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PATHS: usize = 1024;
const MAX_DIFF_FILES: usize = 12;
const MAX_DIFF_BYTES: usize = 24 * 1024;
const MAX_TREE_DEPTH: usize = 16;

struct Scratch {
    path: PathBuf,
    dir: Option<Dir>,
}

impl Scratch {
    fn new(app_data: &Path) -> Result<Self, ContextError> {
        let base = open_absolute(app_data).map_err(unavailable)?;
        let name = format!("prompt-git-{}", uuid::Uuid::new_v4());
        let builder = cap_std::fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use cap_std::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        base.create_dir_with(&name, &builder).map_err(unavailable)?;
        let dir = base.open_dir_nofollow(&name).map_err(unavailable)?;
        Ok(Self {
            path: app_data.join(name),
            dir: Some(dir),
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            let _ = dir.remove_open_dir_all();
        }
    }
}

fn unavailable<T>(_: T) -> ContextError {
    ContextError::ReadUnavailable
}

fn absent<T>(result: io::Result<T>) -> Result<Option<T>, ContextError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ContextError::ReadUnavailable),
    }
}

struct Snapshot {
    repo: Repository,
    git: Dir,
    source: Dir,
    dest: Dir,
    scratch: Scratch,
    loose_budget: Cell<u64>,
}

impl Snapshot {
    fn new(workspace: &Workspace<'_>, app_data: &Path) -> Result<Self, ContextError> {
        let git = workspace
            .dir
            .open_dir_nofollow(".git")
            .map_err(unavailable)?;
        let source = git.open_dir_nofollow("objects").map_err(unavailable)?;
        if absent(git.symlink_metadata("commondir"))?.is_some() {
            return Err(ContextError::ReadUnavailable);
        }
        if let Some(info) = absent(source.open_dir_nofollow("info"))? {
            for name in ["alternates", "http-alternates"] {
                if absent(info.symlink_metadata(name))?.is_some() {
                    return Err(ContextError::ReadUnavailable);
                }
            }
        }

        // Libgit2 only opens a private app-owned object database. Repository
        // config, attributes, worktree pointers and alternate files are never
        // handed to it. Every admitted input is copied by a no-follow handle.
        let scratch = Scratch::new(app_data)?;
        let mut options = git2::RepositoryInitOptions::new();
        options.bare(true).external_template(false).no_reinit(true);
        let repo = Repository::init_opts(&scratch.path, &options).map_err(unavailable)?;
        let scratch_dir = scratch.dir.as_ref().ok_or(ContextError::ReadUnavailable)?;
        let dest = scratch_dir
            .open_dir_nofollow("objects")
            .map_err(unavailable)?;
        let snapshot = Self {
            repo,
            git,
            source,
            dest,
            scratch,
            loose_budget: Cell::new(MAX_LOOSE_BYTES),
        };
        snapshot.copy_packs()?;
        Ok(snapshot)
    }

    fn index(&self) -> Result<Option<git2::Index>, ContextError> {
        let Some(index) = absent(bytes_at(&self.git, OsStr::new("index"), MAX_METADATA_BYTES))?
        else {
            return Ok(None);
        };
        self.scratch
            .dir
            .as_ref()
            .ok_or(ContextError::ReadUnavailable)?
            .write("index", index)
            .map_err(unavailable)?;
        git2::Index::open(&self.scratch.path.join("index"))
            .map(Some)
            .map_err(unavailable)
    }

    fn copy_packs(&self) -> Result<(), ContextError> {
        let Some(source) = absent(self.source.open_dir_nofollow("pack"))? else {
            return Ok(());
        };
        let dest = self.dest.open_dir_nofollow("pack").map_err(unavailable)?;
        let mut budget = MAX_PACK_BYTES;
        for (count, entry) in source.entries().map_err(unavailable)?.enumerate() {
            if count >= 32 {
                return Err(ContextError::Limit);
            }
            let name = entry.map_err(unavailable)?.file_name();
            let value = name.to_str().ok_or(ContextError::ReadUnavailable)?;
            let Some((base, extension)) = value.split_once('.') else {
                continue;
            };
            if !matches!(extension, "pack" | "idx") {
                continue;
            }
            if base.strip_prefix("pack-").is_none_or(|hash| {
                hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) {
                return Err(ContextError::ReadUnavailable);
            }
            let bytes = bytes_at(&source, &name, budget).map_err(unavailable)?;
            budget -= bytes.len() as u64;
            dest.write(&name, bytes).map_err(unavailable)?;
        }
        Ok(())
    }

    fn object(&self, oid: Oid, kind: ObjectType, max: u64) -> Result<(), ContextError> {
        let odb = self.repo.odb().map_err(unavailable)?;
        if !odb.exists(oid) {
            let hex = oid.to_string();
            let (directory, file) = hex.split_at(2);
            let source = self
                .source
                .open_dir_nofollow(directory)
                .map_err(unavailable)?;
            let budget = self.loose_budget.get();
            let bytes = bytes_at(&source, OsStr::new(file), (max + 1024).min(budget))
                .map_err(unavailable)?;
            self.loose_budget.set(budget - bytes.len() as u64);
            match self.dest.create_dir(directory) {
                Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
                    return Err(unavailable(error))
                }
                _ => {}
            }
            let dest = self
                .dest
                .open_dir_nofollow(directory)
                .map_err(unavailable)?;
            dest.create(file)
                .and_then(|mut f| f.write_all(&bytes))
                .map_err(unavailable)?;
            odb.refresh().map_err(unavailable)?;
        }
        let (length, actual) = odb.read_header(oid).map_err(unavailable)?;
        if actual != kind || length as u64 > max {
            return Err(ContextError::ReadUnavailable);
        }
        Ok(())
    }

    fn tree(&self, oid: Oid) -> Result<Tree<'_>, ContextError> {
        self.object(oid, ObjectType::Tree, MAX_METADATA_BYTES)?;
        self.repo.find_tree(oid).map_err(unavailable)
    }

    fn base_object<'a>(
        &'a self,
        mut tree: Tree<'a>,
        path: &Path,
    ) -> Result<Option<String>, ContextError> {
        let mut remaining = path.components().peekable();
        while let Some(component) = remaining.next() {
            let name = component
                .as_os_str()
                .to_str()
                .ok_or(ContextError::Invalid)?;
            let Some(entry) = tree.get_name(name) else {
                return Ok(None);
            };
            let id = entry.id();
            let mode = entry.filemode();
            drop(entry);
            if remaining.peek().is_some() {
                if mode != 0o040_000 {
                    return Err(ContextError::ReadUnavailable);
                }
                tree = self.tree(id)?;
                continue;
            }

            if !matches!(mode, 0o100_644 | 0o100_755) {
                return Err(ContextError::ReadUnavailable);
            }
            self.object(id, ObjectType::Blob, MAX_FILE_BYTES)?;
            let blob = self.repo.find_blob(id).map_err(unavailable)?;
            let raw = blob.content();
            if raw.contains(&0) {
                return Err(ContextError::ReadUnavailable);
            }
            return std::str::from_utf8(raw)
                .map(str::to_owned)
                .map(Some)
                .map_err(unavailable);
        }
        Ok(None)
    }

    fn collect_tree(
        &self,
        workspace: &Workspace<'_>,
        tree: &Tree<'_>,
        prefix: &Path,
        paths: &mut BTreeSet<PathBuf>,
        remaining: &mut usize,
        depth: usize,
    ) -> Result<(), ContextError> {
        if depth >= MAX_TREE_DEPTH {
            return Err(ContextError::DiffLimit);
        }
        for entry in tree {
            *remaining = remaining.checked_sub(1).ok_or(ContextError::DiffLimit)?;
            let Ok(name) = entry.name() else { continue };
            // Tree entries must be one filename. Reject malformed separators
            // before joining, including Windows alternate path forms.
            if name.contains('/') || relative_path(name).is_err() {
                continue;
            }
            let path = prefix.join(name);
            match entry.filemode() {
                0o040_000 if workspace.allowed(&path, true) => {
                    let child = self.tree(entry.id())?;
                    self.collect_tree(workspace, &child, &path, paths, remaining, depth + 1)?;
                }
                0o100_644 | 0o100_755 if workspace.allowed(&path, false) => {
                    paths.insert(path);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub(super) fn working_diff(
    workspace: &Workspace<'_>,
    app_data: &Path,
    file: Option<&str>,
) -> Result<(String, bool), ContextError> {
    let pin = super::skills::git_pin(&workspace.dir)
        .map_err(unavailable)?
        .ok_or(ContextError::ReadUnavailable)?;
    let snapshot = Snapshot::new(workspace, app_data)?;
    let tree = if pin == "unborn" {
        None
    } else {
        let id = Oid::from_str(&pin).map_err(unavailable)?;
        snapshot.object(id, ObjectType::Commit, MAX_METADATA_BYTES)?;
        let commit = snapshot.repo.find_commit(id).map_err(unavailable)?;
        Some(snapshot.tree(commit.tree_id())?)
    };

    let paths = if let Some(path) = file {
        let path = relative_path(path)?;
        if !workspace.allowed(&path, false) {
            return Err(ContextError::ReadUnavailable);
        }
        BTreeSet::from([path])
    } else {
        let mut paths = BTreeSet::new();
        let mut budget = MAX_PATHS;
        if let Some(tree) = tree.as_ref() {
            snapshot.collect_tree(workspace, tree, Path::new(""), &mut paths, &mut budget, 0)?;
        }
        if let Some(index) = snapshot.index()? {
            if index.len() > MAX_PATHS {
                return Err(ContextError::DiffLimit);
            }
            for entry in index.iter() {
                if !matches!(entry.mode, 0o100_644 | 0o100_755) {
                    continue;
                }
                let Some(path) = std::str::from_utf8(&entry.path)
                    .ok()
                    .and_then(|path| relative_path(path).ok())
                else {
                    continue;
                };
                if workspace.allowed(&path, false) {
                    paths.insert(path);
                }
            }
        }
        paths
    };

    let mut result = format!("HEAD {pin} → working tree\n");
    let mut emitted = 0;
    let mut omitted = false;
    for path in paths {
        let combined = file_patch(workspace, &snapshot, tree.clone(), &path, file.is_some());
        let patch = match combined {
            Ok(patch) => patch,
            Err(_) if file.is_none() => {
                omitted = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        if patch.is_empty() {
            continue;
        }
        if emitted == MAX_DIFF_FILES || result.len() + patch.len() > MAX_DIFF_BYTES {
            if file.is_some() {
                return Err(ContextError::Limit);
            }
            omitted = true;
            continue;
        }
        result.push_str(&patch);
        emitted += 1;
    }
    if emitted == 0 {
        if omitted {
            return Err(ContextError::DiffLimit);
        }
        return Err(ContextError::NoDiff);
    }
    Ok((result, omitted))
}

fn file_patch(
    workspace: &Workspace<'_>,
    snapshot: &Snapshot,
    tree: Option<Tree<'_>>,
    path: &Path,
    verify_tracked: bool,
) -> Result<String, ContextError> {
    let old = tree
        .map(|tree| snapshot.base_object(tree, path))
        .transpose()?
        .flatten();
    if verify_tracked && old.is_none() {
        let tracked = snapshot
            .index()?
            .and_then(|index| index.get_path(path, 0))
            .is_some_and(|entry| matches!(entry.mode, 0o100_644 | 0o100_755));
        if !tracked {
            return Err(ContextError::NoDiff);
        }
    }
    let new = absent(workspace.read(path))?;
    if old == new {
        return Ok(String::new());
    }
    let label = secret_free(&path_label(path).ok_or(ContextError::Invalid)?);
    let label = Path::new(&label);
    let old_text = old.as_deref().map(secret_free).unwrap_or_default();
    let new_text = new.as_deref().map(secret_free).unwrap_or_default();
    if old_text == new_text {
        return Ok(String::new());
    }
    let mut options = git2::DiffOptions::new();
    options.context_lines(3);
    let mut diff = Patch::from_buffers(
        old_text.as_bytes(),
        old.as_ref().map(|_| label),
        new_text.as_bytes(),
        new.as_ref().map(|_| label),
        Some(&mut options),
    )
    .map_err(unavailable)?;
    let bytes = diff.to_buf().map_err(unavailable)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
