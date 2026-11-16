//! Bounded, no-follow attachment I/O and app-owned copy lifecycle.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};

use crate::infrastructure::database::{Database, DatabaseError};

pub(crate) const MAX_FILES: usize = 10;
pub(crate) const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
static LIFECYCLE: Mutex<()> = Mutex::new(());

/// Lock order: path lookup (released), process mutex, OS lease, then `SQLite`.
/// This is separate from the lifetime shared instance lease. The stable lock
/// lives beside the database, outside every managed-copy purge directory.
pub(crate) struct LifecycleGuard {
    _process: MutexGuard<'static, ()>,
    _file: Option<std::fs::File>,
}

fn lifecycle_file(db: &Database) -> Result<Option<std::fs::File>, DatabaseError> {
    let path = {
        let conn = db.read().map_err(|_| DatabaseError::Io(invalid()))?;
        conn.path()
            .filter(|path| !path.is_empty() && *path != ":memory:")
            .map(PathBuf::from)
    };
    let Some(path) = path else {
        // In-memory connections cannot be shared by independent processes.
        return Ok(None);
    };
    let open = || -> std::io::Result<std::fs::File> {
        let path = std::fs::canonicalize(path)?;
        let parent = directory(path.parent().ok_or_else(invalid)?)?;
        let mut name = path.file_name().ok_or_else(invalid)?.to_os_string();
        name.push(".attachments.lock");
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .follow(FollowSymlinks::No);
        let file = parent.open_with(name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        Ok(file.into_std())
    };
    // Keep filesystem paths and OS details out of all propagated errors.
    open().map(Some).map_err(|_| DatabaseError::Io(invalid()))
}

/// Serialize admission, byte preparation/linking, removal and delete/clear
/// through post-commit purge across every process sharing this database.
/// Dropping the guard also releases the OS lease on error or unwind.
pub(crate) fn lifecycle(db: &Database) -> Result<LifecycleGuard, DatabaseError> {
    let file = lifecycle_file(db)?;
    let process = LIFECYCLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(file) = &file {
        file.lock().map_err(|_| DatabaseError::Io(invalid()))?;
    }
    Ok(LifecycleGuard {
        _process: process,
        _file: file,
    })
}

#[cfg(test)]
pub(super) fn assert_lifecycle_contended(db: &Database) {
    let file = lifecycle_file(db)
        .unwrap()
        .expect("file-backed synthetic DB");
    assert!(
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "another process must retain the lifecycle lease after deletion commit"
    );
}

fn invalid() -> std::io::Error {
    std::io::Error::other("attachment I/O was refused")
}

/// Walk from the volume root, refusing every symlink/reparse component.
fn directory(path: &Path) -> std::io::Result<Dir> {
    directory_with_create(path, false)
}

fn directory_with_create(path: &Path, create: bool) -> std::io::Result<Dir> {
    let mut root = PathBuf::new();
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => root.push(component.as_os_str()),
            Component::Normal(name) => names.push(name),
            _ => return Err(invalid()),
        }
    }
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    for name in names {
        dir = match dir.open_dir_nofollow(name) {
            Ok(next) => next,
            Err(err) if create && err.kind() == std::io::ErrorKind::NotFound => {
                match dir.create_dir(name) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err),
                }
                dir.open_dir_nofollow(name)?
            }
            Err(err) => return Err(err),
        };
    }
    Ok(dir)
}

fn read_bounded(dir: &Dir, name: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(invalid());
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn managed_name(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
        s.get(..36)
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && s.as_bytes().get(36) == Some(&b'-')
    })
}

/// Opens a validated canonical path through handles; no path re-resolution is
/// performed after opening its parent. Managed copies require a hash sidecar.
pub(crate) fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    let require_hash = managed_name(path)
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "attachments");
    let canonical = super::attachments::resolve_attachment_file_path(&path.to_string_lossy())
        .map_err(|_| invalid())?;
    if require_hash && canonical != path {
        return Err(invalid());
    }
    let dir = directory(canonical.parent().ok_or_else(invalid)?)?;
    let name = Path::new(canonical.file_name().ok_or_else(invalid)?);
    let bytes = read_bounded(&dir, name, MAX_FILE_BYTES)?;
    if require_hash {
        let sidecar = format!("{}.sha256", name.to_string_lossy());
        let expected = read_bounded(&dir, Path::new(&sidecar), 64)?;
        if expected != digest(&bytes).as_bytes() {
            return Err(invalid());
        }
    }
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub(crate) struct AttachmentStore {
    root: PathBuf,
    directory: std::sync::Arc<Dir>,
}

impl AttachmentStore {
    pub(crate) fn open(root: PathBuf) -> std::io::Result<Self> {
        let directory = std::sync::Arc::new(directory_with_create(&root, true)?);
        let root = super::workspace::strip_verbatim(std::fs::canonicalize(root)?);
        Ok(Self { root, directory })
    }

    pub(crate) fn copy(
        &self,
        conversation: i64,
        name: &str,
        bytes: &[u8],
    ) -> std::io::Result<PathBuf> {
        if bytes.len() as u64 > MAX_FILE_BYTES
            || conversation <= 0
            || name.is_empty()
            || name.contains(['/', '\\', ':', '\0'])
            || name == "."
            || name == ".."
        {
            return Err(invalid());
        }
        let root = self.directory.try_clone()?;
        let folder = conversation.to_string();
        match root.create_dir(&folder) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        let dir = root.open_dir_nofollow(&folder)?;
        // Keep the generated component below filesystem limits while the full
        // original display name remains in SQLite.
        let mut end = name.len().min(200);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let filename = format!("{}-{}", uuid::Uuid::new_v4(), &name[..end]);
        let sidecar = format!("{filename}.sha256");
        let outcome = (|| {
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            let mut file = dir.open_with(&filename, &options)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            if read_bounded(&dir, Path::new(&filename), MAX_FILE_BYTES)? != bytes {
                return Err(invalid());
            }
            let mut hash = dir.open_with(&sidecar, &options)?;
            hash.write_all(digest(bytes).as_bytes())?;
            hash.sync_all()
        })();
        if let Err(err) = outcome {
            let _ = dir.remove_file(&filename);
            let _ = dir.remove_file(&sidecar);
            return Err(err);
        }
        Ok(self.root.join(folder).join(filename))
    }

    /// Deletes only a generated direct child of this store/conversation.
    /// Legacy references and all paths outside the owned directory are ignored.
    pub(crate) fn remove(&self, conversation: i64, path: &Path) -> std::io::Result<()> {
        if path.parent() != Some(self.root.join(conversation.to_string()).as_path())
            || !managed_name(path)
        {
            return Ok(());
        }
        let root = self.directory.try_clone()?;
        let dir = match root.open_dir_nofollow(conversation.to_string()) {
            Ok(dir) => dir,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let name = path.file_name().ok_or_else(invalid)?;
        for entry in [
            name.to_string_lossy().into_owned(),
            format!("{}.sha256", name.to_string_lossy()),
        ] {
            if let Err(err) = dir.remove_file(entry) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// Non-recursive cleanup confined to generated files in numeric direct
    /// children. A failed cleanup is retryable even after rows have cascaded.
    pub(crate) fn purge(&self, conversation: Option<i64>) -> std::io::Result<()> {
        let root = self.directory.try_clone()?;
        let folders = if let Some(id) = conversation {
            if id <= 0 {
                return Ok(());
            }
            vec![id.to_string()]
        } else {
            let mut folders = Vec::new();
            for entry in root.entries()? {
                let name = entry?.file_name().to_string_lossy().into_owned();
                if name.parse::<i64>().is_ok_and(|n| n > 0) {
                    folders.push(name);
                }
            }
            folders
        };
        for folder in folders {
            let dir = match root.open_dir_nofollow(&folder) {
                Ok(dir) => dir,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            for entry in dir.entries()? {
                let name = entry?.file_name();
                if managed_name(Path::new(&name)) {
                    dir.remove_file(name)?;
                }
            }
            // Only remove an empty directory, never an unexpected child tree.
            drop(dir);
            match root.remove_dir(&folder) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nexora-copy-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            // Match the store's canonical path spelling for ownership assertions.
            let root = super::super::workspace::strip_verbatim(root.canonicalize().unwrap());
            Self(root)
        }
        fn store(&self) -> AttachmentStore {
            AttachmentStore::open(self.0.join("attachments")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn managed_copy_is_stable_after_original_changes_and_detects_tampering() {
        let fixture = Fixture::new();
        let original = fixture.0.join("note.txt");
        std::fs::write(&original, "original").unwrap();
        let store = fixture.store();
        let copy = store
            .copy(7, "note.txt", &read(&original).unwrap())
            .unwrap();
        assert_ne!(copy, original);
        assert!(copy.starts_with(fixture.0.join("attachments/7")));
        assert!(managed_name(&copy));
        std::fs::write(&original, "edited").unwrap();
        assert_eq!(read(&copy).unwrap(), b"original");
        std::fs::write(&copy, "tampered").unwrap();
        assert!(read(&copy).is_err());
        store.remove(7, &copy).unwrap();
        assert!(!copy.exists());
        assert_eq!(std::fs::read(&original).unwrap(), b"edited");
    }

    #[test]
    fn cleanup_never_mutates_originals_or_another_conversation() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let original = fixture.0.join("original.txt");
        std::fs::write(&original, "source").unwrap();
        let first = store.copy(1, "note.txt", b"first").unwrap();
        let other = store.copy(2, "note.txt", b"other").unwrap();
        store.remove(1, &original).unwrap();
        store.remove(1, &other).unwrap();
        store.purge(Some(1)).unwrap();
        assert!(!first.exists());
        assert!(other.exists());
        assert!(original.exists());
        store.purge(Some(1)).unwrap();
        store.purge(None).unwrap();
        assert!(!other.exists());
        assert_eq!(std::fs::read(original).unwrap(), b"source");
    }

    #[test]
    fn oversized_reads_and_missing_hashes_fail_closed() {
        let fixture = Fixture::new();
        let large = fixture.0.join("large.txt");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(read(&large).is_err());
        let copy = fixture.store().copy(1, "note.txt", b"content").unwrap();
        std::fs::remove_file(format!("{}.sha256", copy.display())).unwrap();
        assert!(read(&copy).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nofollow_parent_and_file_swaps_cannot_escape_the_store() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let store = fixture.store();
        let outside = fixture.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let original = outside.join("secret.txt");
        std::fs::write(&original, "outside").unwrap();
        symlink(&outside, store.root.join("7")).unwrap();
        assert!(store.copy(7, "note.txt", b"safe").is_err());
        assert!(store.purge(Some(7)).is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"outside");
        let dir = directory(&outside).unwrap();
        symlink("secret.txt", outside.join("alias.txt")).unwrap();
        assert!(read_bounded(&dir, Path::new("alias.txt"), MAX_FILE_BYTES).is_err());
    }

    #[test]
    fn lifecycle_lease_releases_on_unwind_and_purge_preserves_lock_identity() {
        let fixture = Fixture::new();
        let db = Database::new(
            crate::infrastructure::database::open(fixture.0.join("nexora.db")).unwrap(),
        );
        let store = fixture.store();
        let existing = lifecycle_file(&db).unwrap().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = lifecycle(&db).unwrap();
            assert!(matches!(
                existing.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            store.purge(None).unwrap();
            panic!("synthetic lifecycle unwind");
        }));
        assert!(result.is_err());
        existing.try_lock().unwrap();
        existing.unlock().unwrap();
        // The handle opened before purge still contends with the next lease:
        // no operation unlinked/replaced the lock inode.
        let _lease = lifecycle(&db).unwrap();
        assert!(matches!(
            existing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_lock_refuses_symlinks_without_touching_the_target() {
        let fixture = Fixture::new();
        let db = Database::new(
            crate::infrastructure::database::open(fixture.0.join("nexora.db")).unwrap(),
        );
        let original = fixture.0.join("original.txt");
        std::fs::write(&original, b"original").unwrap();
        std::os::unix::fs::symlink(&original, fixture.0.join("nexora.db.attachments.lock"))
            .unwrap();
        assert!(lifecycle(&db).is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"original");
    }
}
