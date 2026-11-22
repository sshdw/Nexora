use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};

use super::io::{
    ignore_rules, open_absolute, open_file_at, open_relative, path_label, refused, relative_path,
    text_from_file,
};
use super::{digest, secret_free, ContextError};
use crate::application::project_dir::{rules_verdict_with_budget, IgnoreRules};

pub(super) const MAX_FILE_BYTES: u64 = 16 * 1024;
const MAX_FOLDER_ENTRIES: usize = 96;
const MAX_FOLDER_FILES: usize = 8;
const MAX_FOLDER_DEPTH: usize = 2;
const MAX_CONTEXT_BYTES: usize = 24 * 1024;
const MAX_SYMBOL_LINES: usize = 150;
const MAX_IGNORE_WORK: usize = 2_000_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextSelector {
    File { path: String },
    Folder { path: String },
    Symbol { path: String, symbol: String },
    Diff { path: Option<String> },
}

impl ContextSelector {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::File { .. } => "file",
            Self::Folder { .. } => "folder",
            Self::Symbol { .. } => "symbol",
            Self::Diff { .. } => "diff",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRef {
    pub(super) selector: ContextSelector,
    revision: String,
}

#[derive(Serialize)]
pub struct ContextView {
    pub(super) reference: ContextRef,
    pub(super) label: String,
    pub(super) text: String,
    omitted: bool,
}

pub(super) struct Workspace<'a> {
    base: &'a Path,
    pub(super) dir: Dir,
    rules: IgnoreRules,
    ignore_work: Cell<usize>,
}

impl<'a> Workspace<'a> {
    fn open(base: &'a Path) -> Result<Self, ContextError> {
        if nexora_os_exec::private_store_path(base) {
            return Err(ContextError::ReadUnavailable);
        }
        let dir = open_absolute(base).map_err(|_| ContextError::ReadUnavailable)?;
        let rules = ignore_rules(&dir)?;
        Ok(Self {
            base,
            dir,
            rules,
            ignore_work: Cell::new(MAX_IGNORE_WORK),
        })
    }

    pub(super) fn allowed(&self, path: &Path, is_dir: bool) -> bool {
        let Some(label) = path_label(path) else {
            return false;
        };
        let absolute = self.base.join(path);
        if nexora_os_exec::private_store_path(&absolute)
            || nexora_os_exec::sensitive_write_path(path)
            || path.components().any(|part| {
                part.as_os_str().eq_ignore_ascii_case(".git")
                    || part.as_os_str().eq_ignore_ascii_case(".nexora")
            })
        {
            return false;
        }
        let mut work = self.ignore_work.get();
        let verdict = rules_verdict_with_budget(&self.rules, &label, is_dir, &mut work);
        self.ignore_work.set(work);
        verdict == Some(false)
    }

    fn file(&self, name: &str) -> Result<String, ContextError> {
        let path = relative_path(name)?;
        self.read(&path).map_err(|_| ContextError::ReadUnavailable)
    }

    pub(super) fn read(&self, relative: &Path) -> io::Result<String> {
        let parent = relative.parent().ok_or_else(refused)?;
        let name = relative.file_name().ok_or_else(refused)?;
        self.read_entry(&open_relative(&self.dir, parent)?, name, relative)
    }

    fn read_entry(&self, parent: &Dir, name: &OsStr, relative: &Path) -> io::Result<String> {
        let file = open_file_at(parent, name)?;
        #[cfg(windows)]
        let (file, actual) = {
            let _ = relative;
            let file = file.into_std();
            let actual = self.opened_path(&file)?;
            (cap_std::fs::File::from_std(file), actual)
        };
        #[cfg(windows)]
        let path = &actual;
        #[cfg(not(windows))]
        let path = relative;
        if !self.allowed(path, false) {
            return Err(refused());
        }
        text_from_file(file, MAX_FILE_BYTES)
    }

    #[cfg(windows)]
    fn opened_path(&self, file: &std::fs::File) -> io::Result<PathBuf> {
        let root = winx::file::get_file_path(&self.dir.try_clone()?.into_std_file())?;
        // Compare handle-derived paths only. Continue reading the same handle.
        winx::file::get_file_path(file)?
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| refused())
    }

    fn allowed_directory(&self, dir: &Dir, relative: PathBuf) -> io::Result<PathBuf> {
        #[cfg(not(windows))]
        let _ = dir;
        #[cfg(windows)]
        let relative = if relative.as_os_str().is_empty() {
            relative
        } else {
            self.opened_path(&dir.try_clone()?.into_std_file())?
        };
        if relative.as_os_str().is_empty() || self.allowed(&relative, true) {
            Ok(relative)
        } else {
            Err(refused())
        }
    }

    fn folder(&self, name: &str) -> Result<(String, String, bool), ContextError> {
        let path = if name == "." {
            PathBuf::new()
        } else {
            relative_path(name)?
        };
        let dir = open_relative(&self.dir, &path).map_err(|_| ContextError::ReadUnavailable)?;
        let path = self
            .allowed_directory(&dir, path)
            .map_err(|_| ContextError::ReadUnavailable)?;
        let mut folder = Folder {
            workspace: self,
            raw: String::new(),
            text: String::new(),
            examined: 0,
            files: 0,
            omitted: false,
        };
        folder.collect(&dir, &path, 0);
        if folder.files == 0 {
            return Err(ContextError::ReadUnavailable);
        }
        Ok((folder.raw, folder.text, folder.omitted))
    }
}

struct Folder<'a> {
    workspace: &'a Workspace<'a>,
    raw: String,
    text: String,
    examined: usize,
    files: usize,
    omitted: bool,
}

impl Folder<'_> {
    fn collect(&mut self, directory: &Dir, relative: &Path, depth: usize) {
        let Ok(entries) = directory.entries() else {
            self.omitted = true;
            return;
        };
        let mut children = Vec::new();
        for child in entries {
            if self.examined == MAX_FOLDER_ENTRIES {
                self.omitted = true;
                break;
            }
            self.examined += 1;
            let Ok(entry) = child else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = relative.join(entry.file_name());
            if (kind.is_file() || kind.is_dir()) && self.workspace.allowed(&path, kind.is_dir()) {
                children.push((path, entry.file_name(), kind.is_dir()));
            }
        }
        children.sort_unstable();
        for (path, name, is_dir) in children {
            if self.files == MAX_FOLDER_FILES {
                self.omitted = true;
                break;
            }
            if is_dir {
                if depth == MAX_FOLDER_DEPTH {
                    self.omitted = true;
                } else if let Ok(child) = directory.open_dir_nofollow(&name) {
                    if let Ok(actual) = self.workspace.allowed_directory(&child, path) {
                        self.collect(&child, &actual, depth + 1);
                    }
                }
                continue;
            }
            let (Some(label), Ok(text)) = (
                path_label(&path),
                self.workspace.read_entry(directory, &name, &path),
            ) else {
                self.omitted = true;
                continue;
            };
            let raw = format!("\n--- {label} ---\n{text}\n");
            let entry = format!(
                "\n--- {} ---\n{}\n",
                secret_free(&label),
                secret_free(&text)
            );
            if self.raw.len() + raw.len() > MAX_CONTEXT_BYTES
                || self.text.len() + entry.len() > MAX_CONTEXT_BYTES
            {
                self.omitted = true;
                continue;
            }
            self.raw.push_str(&raw);
            self.text.push_str(&entry);
            self.files += 1;
        }
    }
}

fn references(document: &str, symbol: &str) -> Result<(String, bool), ContextError> {
    fn identifier(ch: char) -> bool {
        ch.is_alphanumeric() || matches!(ch, '_' | '$')
    }
    if symbol.is_empty() || symbol.len() > 96 || !symbol.chars().all(identifier) {
        return Err(ContextError::Invalid);
    }
    let lines = document.lines().collect::<Vec<_>>();
    let redacted = secret_free(document);
    let visible = redacted.split('\n').take(lines.len());
    let contains_symbol = |line: &str| line.split(|ch| !identifier(ch)).any(|word| word == symbol);
    let mut chosen = vec![false; lines.len()];
    for (index, (raw, safe)) in lines.iter().zip(visible.clone()).enumerate() {
        if contains_symbol(raw) && contains_symbol(safe) {
            let end = (index + 3).min(lines.len());
            chosen[index.saturating_sub(2)..end].fill(true);
        }
    }
    let mut rendered = String::new();
    let mut count = 0;
    for (index, line) in visible.enumerate().filter(|(index, _)| chosen[*index]) {
        use std::fmt::Write;
        let line = line.strip_suffix('\r').unwrap_or(line);
        let _ = writeln!(rendered, "{}: {line}", index + 1);
        count += 1;
        if count == MAX_SYMBOL_LINES {
            return Ok((
                rendered,
                chosen.iter().filter(|&&chosen| chosen).count() > count,
            ));
        }
    }
    if count == 0 {
        return Err(ContextError::ReadUnavailable);
    }
    Ok((rendered, false))
}

pub(super) fn resolve(
    base: &Path,
    app_data: &Path,
    conversation_id: i64,
    selector: ContextSelector,
) -> Result<ContextView, ContextError> {
    let workspace = Workspace::open(base)?;
    let result: Result<_, ContextError> = (|| match &selector {
        ContextSelector::File { path } => {
            let source = workspace.file(path)?;
            let text = secret_free(&source);
            Ok((path.clone(), digest(source.as_bytes()), text, false))
        }
        ContextSelector::Folder { path } => {
            let (source, text, more) = workspace.folder(path)?;
            Ok((path.clone(), digest(source.as_bytes()), text, more))
        }
        ContextSelector::Symbol { path, symbol } => {
            let source = workspace.file(path)?;
            let (text, more) = references(&source, symbol)?;
            Ok((
                format!("{path}#{symbol}"),
                digest(source.as_bytes()),
                text,
                more,
            ))
        }
        ContextSelector::Diff { path } => {
            let (text, more) = super::git::working_diff(&workspace, app_data, path.as_deref())?;
            Ok((
                path.clone().unwrap_or_else(|| "HEAD → working tree".into()),
                digest(text.as_bytes()),
                text,
                more,
            ))
        }
    })();
    if workspace.ignore_work.get() == 0 {
        return Err(ContextError::Limit);
    }
    let (label, source_revision, text, omitted) = result?;
    let stable_root =
        crate::application::agent::tools::path_safety::path_to_comparable_string(base);
    let text = secret_free(&text);
    let identity = serde_json::to_vec(&(
        stable_root,
        conversation_id,
        &selector,
        omitted,
        &source_revision,
        &text,
    ))
    .map_err(|_| ContextError::Invalid)?;
    Ok(ContextView {
        reference: ContextRef {
            selector,
            revision: digest(&identity),
        },
        label: secret_free(&label),
        text,
        omitted,
    })
}
