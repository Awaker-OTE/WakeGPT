use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformTrashError {
    InvalidPath,
    SourceUnavailable,
    SourceNotRegular,
    DestinationCollision,
    TrashFailed,
    ResultingUrlMissing,
    RestoreFailed,
    #[cfg(not(target_os = "macos"))]
    UnsupportedPlatform,
}

impl PlatformTrashError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidPath => "trash_path_invalid",
            Self::SourceUnavailable => "trash_source_unavailable",
            Self::SourceNotRegular => "trash_source_not_regular",
            Self::DestinationCollision => "trash_restore_collision",
            Self::TrashFailed => "trash_move_failed",
            Self::ResultingUrlMissing => "trash_resulting_url_missing",
            Self::RestoreFailed => "trash_restore_failed",
            #[cfg(not(target_os = "macos"))]
            Self::UnsupportedPlatform => "trash_platform_unsupported",
        }
    }
}

impl fmt::Display for PlatformTrashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for PlatformTrashError {}

#[cfg(target_os = "macos")]
pub(crate) fn move_to_system_trash(path: &Path) -> Result<PathBuf, PlatformTrashError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    validate_regular_source(path)?;
    let path_text = path.to_str().ok_or(PlatformTrashError::InvalidPath)?;
    let source = NSURL::fileURLWithPath(&NSString::from_str(path_text));
    let manager = NSFileManager::defaultManager();
    let mut resulting_url = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&source, Some(&mut resulting_url))
        .map_err(|_| PlatformTrashError::TrashFailed)?;
    let resulting_url = resulting_url.ok_or(PlatformTrashError::ResultingUrlMissing)?;
    let resulting_path = resulting_url
        .path()
        .ok_or(PlatformTrashError::ResultingUrlMissing)?;
    let resulting_path = PathBuf::from(resulting_path.to_string());
    validate_regular_source(&resulting_path)?;
    Ok(resulting_path)
}

#[cfg(target_os = "macos")]
pub(crate) fn move_directory_to_system_trash(path: &Path) -> Result<PathBuf, PlatformTrashError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    validate_directory_source(path)?;
    let path_text = path.to_str().ok_or(PlatformTrashError::InvalidPath)?;
    let source = NSURL::fileURLWithPath(&NSString::from_str(path_text));
    let manager = NSFileManager::defaultManager();
    let mut resulting_url = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&source, Some(&mut resulting_url))
        .map_err(|_| PlatformTrashError::TrashFailed)?;
    let resulting_url = resulting_url.ok_or(PlatformTrashError::ResultingUrlMissing)?;
    let resulting_path = resulting_url
        .path()
        .ok_or(PlatformTrashError::ResultingUrlMissing)?;
    let resulting_path = PathBuf::from(resulting_path.to_string());
    validate_directory_source(&resulting_path)?;
    Ok(resulting_path)
}

#[cfg(target_os = "macos")]
pub(crate) fn restore_from_system_trash(
    trashed_path: &Path,
    original_path: &Path,
) -> Result<(), PlatformTrashError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    validate_regular_source(trashed_path)?;
    if !original_path.is_absolute() || original_path.to_str().is_none() {
        return Err(PlatformTrashError::InvalidPath);
    }
    match std::fs::symlink_metadata(original_path) {
        Ok(_) => return Err(PlatformTrashError::DestinationCollision),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(PlatformTrashError::RestoreFailed),
    }
    let parent = original_path
        .parent()
        .ok_or(PlatformTrashError::InvalidPath)?;
    let metadata =
        std::fs::symlink_metadata(parent).map_err(|_| PlatformTrashError::RestoreFailed)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PlatformTrashError::RestoreFailed);
    }
    let trashed_text = trashed_path
        .to_str()
        .ok_or(PlatformTrashError::InvalidPath)?;
    let original_text = original_path
        .to_str()
        .ok_or(PlatformTrashError::InvalidPath)?;
    let source = NSURL::fileURLWithPath(&NSString::from_str(trashed_text));
    let destination = NSURL::fileURLWithPath(&NSString::from_str(original_text));
    NSFileManager::defaultManager()
        .moveItemAtURL_toURL_error(&source, &destination)
        .map_err(|_| PlatformTrashError::RestoreFailed)
}

#[cfg(target_os = "macos")]
fn validate_regular_source(path: &Path) -> Result<(), PlatformTrashError> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(PlatformTrashError::InvalidPath);
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| PlatformTrashError::SourceUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PlatformTrashError::SourceNotRegular);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_directory_source(path: &Path) -> Result<(), PlatformTrashError> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(PlatformTrashError::InvalidPath);
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| PlatformTrashError::SourceUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PlatformTrashError::SourceNotRegular);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn move_to_system_trash(_path: &Path) -> Result<PathBuf, PlatformTrashError> {
    Err(PlatformTrashError::UnsupportedPlatform)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn move_directory_to_system_trash(_path: &Path) -> Result<PathBuf, PlatformTrashError> {
    Err(PlatformTrashError::UnsupportedPlatform)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn restore_from_system_trash(
    _trashed_path: &Path,
    _original_path: &Path,
) -> Result<(), PlatformTrashError> {
    Err(PlatformTrashError::UnsupportedPlatform)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::fs;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("wakegpt-trash-{label}-{}", crate::domain::new_id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_trash_returns_a_restorable_resulting_url() {
        let root = TestRoot::new("restore");
        let original = root.0.join("attachment.png");
        fs::write(&original, b"synthetic-image").unwrap();

        let trashed = move_to_system_trash(&original).unwrap();
        assert!(!original.exists());
        assert!(trashed.exists());

        restore_from_system_trash(&trashed, &original).unwrap();
        assert_eq!(fs::read(original).unwrap(), b"synthetic-image");
    }

    #[test]
    fn native_restore_refuses_to_replace_a_collision() {
        let root = TestRoot::new("collision");
        let original = root.0.join("attachment.png");
        fs::write(&original, b"trashed-version").unwrap();
        let trashed = move_to_system_trash(&original).unwrap();
        fs::write(&original, b"new-version").unwrap();

        assert_eq!(
            restore_from_system_trash(&trashed, &original).unwrap_err(),
            PlatformTrashError::DestinationCollision
        );
        assert_eq!(fs::read(&original).unwrap(), b"new-version");
        fs::remove_file(&original).unwrap();
        restore_from_system_trash(&trashed, &original).unwrap();
    }

    #[test]
    fn native_trash_accepts_an_app_owned_directory() {
        let root = TestRoot::new("directory");
        let directory = root.0.join("reset-package");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("synthetic.json"), b"synthetic").unwrap();

        let trashed = move_directory_to_system_trash(&directory).unwrap();
        assert!(!directory.exists());
        assert_eq!(
            fs::read(trashed.join("synthetic.json")).unwrap(),
            b"synthetic"
        );
        fs::remove_dir_all(trashed).unwrap();
    }
}
