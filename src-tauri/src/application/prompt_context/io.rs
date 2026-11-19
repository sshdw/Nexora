use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, File, OpenOptions};

use super::{ContextError, MAX_PATH_BYTES};

pub(super) fn refused() -> io::Error {
    io::Error::other("context file access refused")
}

pub(super) fn relative_path(value: &str) -> Result<PathBuf, ContextError> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value.chars().any(char::is_control)
        || value.contains(['\\', ':'])
        || value.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
        })
    {
        return Err(ContextError::Invalid);
    }
    let path = PathBuf::from(value);
    if path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(ContextError::Invalid)
    }
}

pub(super) fn path_label(path: &Path) -> Option<String> {
    let parts: Option<Vec<_>> = path
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect();
    let label = parts?.join("/");
    relative_path(&label).ok().map(|_| label)
}

/// Start at the volume root so a raced parent replacement cannot redirect a context read.
pub(super) fn open_absolute(path: &Path) -> io::Result<Dir> {
    if !path.is_absolute() {
        return Err(refused());
    }
    let mut volume = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => volume.push(component.as_os_str()),
            Component::Normal(name) => names.push(name),
            _ => return Err(refused()),
        }
    }
    let mut dir = Dir::open_ambient_dir(volume, cap_std::ambient_authority())?;
    for name in names {
        dir = dir.open_dir_nofollow(name)?;
    }
    Ok(dir)
}

pub(super) fn open_relative(root: &Dir, path: &Path) -> io::Result<Dir> {
    let mut dir = root.try_clone()?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(refused());
        };
        dir = dir.open_dir_nofollow(name)?;
    }
    Ok(dir)
}

pub(super) fn bytes_at(dir: &Dir, name: &OsStr, max_bytes: u64) -> io::Result<Vec<u8>> {
    bytes_from_file(open_file_at(dir, name)?, max_bytes)
}

pub(super) fn open_file_at(dir: &Dir, name: &OsStr) -> io::Result<File> {
    if !dir.symlink_metadata(name)?.is_file() {
        return Err(refused());
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    dir.open_with(name, &options)
}

fn bytes_from_file(file: File, max_bytes: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > max_bytes {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(refused());
    }
    Ok(bytes)
}

pub(super) fn read_at(dir: &Dir, name: &OsStr, max_bytes: u64) -> io::Result<String> {
    text_from_file(open_file_at(dir, name)?, max_bytes)
}

pub(super) fn text_from_file(file: File, max_bytes: u64) -> io::Result<String> {
    let bytes = bytes_from_file(file, max_bytes)?;
    if bytes.contains(&0) {
        return Err(refused());
    }
    String::from_utf8(bytes).map_err(|_| refused())
}

pub(super) fn ignore_rules(
    root: &Dir,
) -> Result<crate::application::project_dir::IgnoreRules, ContextError> {
    let config = Path::new(".nexora/.nexoraignore");
    match read_relative(root, config, 64 * 1024) {
        Ok(rules) => {
            let rules = crate::application::project_dir::parse_ignore_rules(&rules);
            if rules.truncated {
                return Err(ContextError::Limit);
            }
            Ok(rules)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(crate::application::project_dir::IgnoreRules::default())
        }
        Err(_) => Err(ContextError::ReadUnavailable),
    }
}
pub(super) fn read_relative(root: &Dir, path: &Path, max_bytes: u64) -> io::Result<String> {
    let parent = path.parent().ok_or_else(refused)?;
    let name = path.file_name().ok_or_else(refused)?;
    read_at(&open_relative(root, parent)?, name, max_bytes)
}
