//! Nonrecursive namespace operations. Staging, locking and recovery belong to
//! source libraries. Paths require trusted ancestors; metadata is a snapshot.

use std::{fs, io, path::Path};

fn failure(error: io::Error) -> i64 {
    match error.kind() {
        io::ErrorKind::AlreadyExists => -2,
        io::ErrorKind::NotFound => -3,
        io::ErrorKind::OutOfMemory => super::fault("out of memory"),
        _ => -1,
    }
}

fn status(result: io::Result<()>) -> i64 {
    result.map_or_else(failure, |()| 0)
}

pub(super) fn create_dir(path: &Path) -> i64 {
    // One exclusive operation, not exists() followed by creation.
    status(fs::create_dir(path))
}

pub(super) fn rename(from: &Path, to: &Path) -> i64 {
    // Native replacement only: never delete first or copy across filesystems.
    // This publishes a name; it does not flush files or directories to storage.
    status(fs::rename(from, to))
}

pub(super) fn remove_file(path: &Path) -> i64 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => return failure(error),
        };
        // Windows directory links use RemoveDirectory, not DeleteFile. Never
        // recurse or turn an arbitrary file-removal failure into dir removal.
        if metadata.file_type().is_symlink_dir() {
            return status(fs::remove_dir(path));
        }
    }
    status(fs::remove_file(path))
}

pub(super) fn remove_empty_dir(path: &Path) -> i64 {
    status(fs::remove_dir(path))
}

fn classify(file: bool, directory: bool, link: bool, reparse: bool) -> i64 {
    if link {
        3
    } else if reparse {
        // Unknown reparse points must never invite recursive traversal.
        2
    } else if directory {
        1
    } else if file {
        0
    } else {
        2
    }
}

pub(super) fn entry_kind(path: &Path) -> i64 {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return failure(error),
    };
    let kind = metadata.file_type();
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    #[cfg(not(windows))]
    let reparse = false;
    classify(kind.is_file(), kind.is_dir(), kind.is_symlink(), reparse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_creation_publication_and_nonrecursive_removal() {
        let temporary = tempfile::tempdir().unwrap();
        let staging = temporary.path().join("staging 雪");
        assert_eq!(create_dir(&staging), 0);
        assert_eq!(create_dir(&staging), -2);
        assert_eq!(create_dir(&temporary.path().join("missing/child")), -3);
        assert_eq!(entry_kind(&staging), 1);
        let source = staging.join("source");
        let destination = temporary.path().join("published");
        fs::write(&source, b"first\0bytes").unwrap();
        assert_eq!(create_dir(&source), -2);
        assert_eq!(entry_kind(&source), 0);
        assert_eq!(remove_empty_dir(&staging), -1);
        assert_eq!(rename(&source, &destination), 0);
        assert_eq!(fs::read(&destination).unwrap(), b"first\0bytes");
        fs::write(&source, b"replacement").unwrap();
        assert_eq!(rename(&source, &destination), 0);
        assert_eq!(rename(&source, &destination), -3);
        assert_eq!(fs::read(&destination).unwrap(), b"replacement");
        assert_eq!(remove_file(&destination), 0);
        assert_eq!(remove_file(&destination), -3);
        assert_eq!(entry_kind(&destination), -3);
        assert_eq!(remove_empty_dir(&staging), 0);
        assert_eq!(remove_empty_dir(&staging), -3);
    }

    #[test]
    fn links_and_unknown_reparse_points_are_not_traversable_entries() {
        assert_eq!(classify(false, true, false, true), 2);
        assert_eq!(classify(true, false, false, true), 2);
        assert_eq!(classify(false, true, true, true), 3);
        #[cfg(any(unix, windows))]
        {
            let temporary = tempfile::tempdir().unwrap();
            let target = temporary.path().join("target");
            fs::create_dir(&target).unwrap();
            let content = target.join("kept");
            fs::write(&content, b"keep").unwrap();
            for (index, destination) in [&target, &content, &target.join("absent")]
                .into_iter()
                .enumerate()
            {
                let link = temporary.path().join(format!("link-{index}"));
                #[cfg(unix)]
                let created = std::os::unix::fs::symlink(destination, &link);
                #[cfg(windows)]
                let created = if index == 0 {
                    std::os::windows::fs::symlink_dir(destination, &link)
                } else {
                    std::os::windows::fs::symlink_file(destination, &link)
                };
                #[cfg(windows)]
                if created
                    .as_ref()
                    .is_err_and(|error| error.raw_os_error() == Some(1314))
                {
                    eprintln!("symlink coverage requires Windows symlink privilege");
                    return;
                }
                created.unwrap();
                assert_eq!(entry_kind(&link), 3);
                assert_eq!(remove_file(&link), 0);
                assert_eq!(entry_kind(&link), -3);
            }
            assert_eq!(fs::read(content).unwrap(), b"keep");
        }
    }
}
