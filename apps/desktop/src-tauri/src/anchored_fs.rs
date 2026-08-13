//! Descriptor-anchored filesystem primitives for authorized Unix workspaces.
//!
//! Every relative lookup starts at an already-authorized directory descriptor.
//! Parent directories are opened one component at a time with `openat(2)`,
//! `O_NOFOLLOW`, and `O_DIRECTORY`; no operation reconstructs an absolute path.
//! Windows is intentionally unsupported here and needs a separate handle-based
//! implementation before this module can be used there.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchoredFsErrorCode {
    InvalidRelativePath,
    NameContainsNul,
    EntryNotFound,
    EntryAlreadyExists,
    SymlinkEncountered,
    NotDirectory,
    NotRegularFile,
    PermissionDenied,
    StageIdentityChanged,
    TargetIdentityChanged,
    FileSyncFailed,
    DirectorySyncFailed,
    Io,
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    UnsupportedPlatform,
}

impl AnchoredFsErrorCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRelativePath => "anchored_path_invalid",
            Self::NameContainsNul => "anchored_name_contains_nul",
            Self::EntryNotFound => "anchored_entry_not_found",
            Self::EntryAlreadyExists => "anchored_entry_already_exists",
            Self::SymlinkEncountered => "anchored_symlink_rejected",
            Self::NotDirectory => "anchored_not_directory",
            Self::NotRegularFile => "anchored_not_regular_file",
            Self::PermissionDenied => "anchored_permission_denied",
            Self::StageIdentityChanged => "anchored_stage_identity_changed",
            Self::TargetIdentityChanged => "anchored_target_identity_changed",
            Self::FileSyncFailed => "anchored_file_sync_failed",
            Self::DirectorySyncFailed => "anchored_directory_sync_failed",
            Self::Io => "anchored_io_error",
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            Self::UnsupportedPlatform => "anchored_platform_unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnchoredFsError {
    code: AnchoredFsErrorCode,
    raw_os_error: Option<i32>,
}

impl AnchoredFsError {
    const fn new(code: AnchoredFsErrorCode) -> Self {
        Self {
            code,
            raw_os_error: None,
        }
    }

    const fn with_errno(code: AnchoredFsErrorCode, raw_os_error: Option<i32>) -> Self {
        Self { code, raw_os_error }
    }

    pub(crate) const fn code(self) -> AnchoredFsErrorCode {
        self.code
    }

    pub(crate) const fn code_str(self) -> &'static str {
        self.code.as_str()
    }
}

impl fmt::Display for AnchoredFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code.as_str())
    }
}

impl std::error::Error for AnchoredFsError {}

pub(crate) type AnchoredFsResult<T> = Result<T, AnchoredFsError>;

#[cfg(unix)]
mod unix {
    use super::{AnchoredFsError, AnchoredFsErrorCode, AnchoredFsResult};
    use sha2::{Digest, Sha256};
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io;
    use std::io::Read;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path};

    pub(crate) struct AnchoredRoot {
        fd: OwnedFd,
    }

    impl AnchoredRoot {
        /// Takes ownership of an authorization-scoped directory descriptor.
        pub(crate) fn from_owned_fd(fd: OwnedFd) -> AnchoredFsResult<Self> {
            require_directory(fd.as_raw_fd())?;
            Ok(Self { fd })
        }

        /// Opens a regular file for reading without following its final component.
        pub(crate) fn open_regular_file_read(&self, relative: &Path) -> AnchoredFsResult<File> {
            let components = relative_components(relative)?;
            let (parent, name) = resolve_parent(self.fd.as_raw_fd(), &components, false)?;
            reject_known_symlink(parent.as_raw_fd(), name)?;

            // O_NONBLOCK prevents a hostile FIFO from blocking before fstat can reject it.
            let raw_fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                )
            };
            if raw_fd < 0 {
                return Err(classify_file_open_error(
                    parent.as_raw_fd(),
                    name,
                    io::Error::last_os_error(),
                ));
            }
            let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
            require_regular_file(fd.as_raw_fd())?;
            Ok(File::from(fd))
        }

        /// Creates a mode-0600 stage beside `target_relative` using O_EXCL.
        ///
        /// The caller supplies a single opaque stage filename. `create_parents`
        /// controls whether missing target parents are created with `mkdirat(2)`.
        pub(crate) fn create_exclusive_stage(
            &self,
            target_relative: &Path,
            stage_file_name: &OsStr,
            create_parents: bool,
        ) -> AnchoredFsResult<ExclusiveStage> {
            let components = relative_components(target_relative)?;
            let (parent, target_name) =
                resolve_parent(self.fd.as_raw_fd(), &components, create_parents)?;
            let stage_name = single_name(stage_file_name)?;
            if stage_name.as_bytes() == target_name.as_bytes() {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::InvalidRelativePath,
                ));
            }

            let raw_fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    stage_name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0o600,
                )
            };
            if raw_fd < 0 {
                return Err(error_from_io(
                    io::Error::last_os_error(),
                    AnchoredFsErrorCode::Io,
                ));
            }
            let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
            require_regular_file(fd.as_raw_fd())?;
            let identity = file_identity(fd.as_raw_fd())?;

            Ok(ExclusiveStage {
                parent,
                file: File::from(fd),
                stage_name,
                target_name: target_name.clone(),
                identity,
                installed: false,
                remove_on_drop: true,
            })
        }

        /// Reopens a persisted stage beside `target_relative` without following links.
        pub(crate) fn open_existing_stage(
            &self,
            target_relative: &Path,
            stage_file_name: &OsStr,
        ) -> AnchoredFsResult<ExclusiveStage> {
            let components = relative_components(target_relative)?;
            let (parent, target_name) = resolve_parent(self.fd.as_raw_fd(), &components, false)?;
            let stage_name = single_name(stage_file_name)?;
            if stage_name.as_bytes() == target_name.as_bytes() {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::InvalidRelativePath,
                ));
            }
            let raw_fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    stage_name.as_ptr(),
                    libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                )
            };
            if raw_fd < 0 {
                return Err(classify_file_open_error(
                    parent.as_raw_fd(),
                    &stage_name,
                    io::Error::last_os_error(),
                ));
            }
            let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
            require_regular_file(fd.as_raw_fd())?;
            let identity = file_identity(fd.as_raw_fd())?;
            Ok(ExclusiveStage {
                parent,
                file: File::from(fd),
                stage_name,
                target_name: target_name.clone(),
                identity,
                installed: false,
                remove_on_drop: false,
            })
        }

        pub(crate) fn remove_stage_if_present(
            &self,
            target_relative: &Path,
            stage_file_name: &OsStr,
        ) -> AnchoredFsResult<()> {
            let components = relative_components(target_relative)?;
            let (parent, target_name) = resolve_parent(self.fd.as_raw_fd(), &components, false)?;
            let stage_name = single_name(stage_file_name)?;
            if stage_name.as_bytes() == target_name.as_bytes() {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::InvalidRelativePath,
                ));
            }
            match entry_kind(parent.as_raw_fd(), &stage_name)? {
                None => return Ok(()),
                Some(EntryKind::Symlink) => {
                    return Err(AnchoredFsError::new(
                        AnchoredFsErrorCode::SymlinkEncountered,
                    ));
                }
                Some(EntryKind::Directory) => {
                    return Err(AnchoredFsError::new(AnchoredFsErrorCode::NotRegularFile));
                }
                Some(EntryKind::Regular | EntryKind::Other) => {}
            }
            if unsafe { libc::unlinkat(parent.as_raw_fd(), stage_name.as_ptr(), 0) } != 0 {
                return Err(error_from_io(
                    io::Error::last_os_error(),
                    AnchoredFsErrorCode::Io,
                ));
            }
            sync_directory(parent.as_raw_fd())
        }

        pub(crate) fn sync_target_parent(&self, target_relative: &Path) -> AnchoredFsResult<()> {
            let components = relative_components(target_relative)?;
            let (parent, _) = resolve_parent(self.fd.as_raw_fd(), &components, false)?;
            sync_directory(parent.as_raw_fd())
        }
    }

    pub(crate) struct ExclusiveStage {
        parent: OwnedFd,
        file: File,
        stage_name: CString,
        target_name: CString,
        identity: FileIdentity,
        installed: bool,
        remove_on_drop: bool,
    }

    impl ExclusiveStage {
        pub(crate) fn file_mut(&mut self) -> &mut File {
            &mut self.file
        }

        /// Flushes a newly-created stage and leaves it in place for crash recovery.
        pub(crate) fn persist(mut self) -> AnchoredFsResult<()> {
            self.sync_and_verify()?;
            sync_directory(self.parent.as_raw_fd())?;
            self.installed = true;
            Ok(())
        }

        /// Installs the stage only when the target name does not exist.
        ///
        /// macOS uses `renameatx_np(RENAME_EXCL)`. Linux uses
        /// `renameat2(RENAME_NOREPLACE)`. Other Unix targets deliberately return
        /// `anchored_platform_unsupported` until equivalent behavior is verified.
        pub(crate) fn install_new(mut self) -> AnchoredFsResult<()> {
            self.sync_and_verify()?;
            rename_no_replace(self.parent.as_raw_fd(), &self.stage_name, &self.target_name)?;
            self.installed = true;
            sync_directory(self.parent.as_raw_fd())
        }

        /// Atomically exchanges the stage with an existing target, verifies the displaced target
        /// digest, and rolls the exchange back if the compare-and-swap precondition changed.
        pub(crate) fn replace_existing_cas(
            mut self,
            expected_target_sha256: &str,
            max_bytes: u64,
        ) -> AnchoredFsResult<()> {
            self.sync_and_verify()?;
            require_regular_entry(self.parent.as_raw_fd(), &self.target_name)?;
            rename_exchange(self.parent.as_raw_fd(), &self.stage_name, &self.target_name)?;
            self.installed = true;
            let verification =
                read_entry_bytes(self.parent.as_raw_fd(), &self.stage_name, max_bytes).and_then(
                    |bytes| {
                        if sha256_hex(&bytes) == expected_target_sha256 {
                            Ok(())
                        } else {
                            Err(AnchoredFsError::new(
                                AnchoredFsErrorCode::TargetIdentityChanged,
                            ))
                        }
                    },
                );
            if let Err(error) = verification {
                if rename_exchange(self.parent.as_raw_fd(), &self.stage_name, &self.target_name)
                    .is_err()
                {
                    return Err(AnchoredFsError::new(
                        AnchoredFsErrorCode::TargetIdentityChanged,
                    ));
                }
                self.installed = false;
                let _ = sync_directory(self.parent.as_raw_fd());
                return Err(error);
            }
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.stage_name.as_ptr(), 0) } != 0
            {
                return Err(error_from_io(
                    io::Error::last_os_error(),
                    AnchoredFsErrorCode::Io,
                ));
            }
            sync_directory(self.parent.as_raw_fd())
        }

        fn sync_and_verify(&self) -> AnchoredFsResult<()> {
            sync_file(&self.file).map_err(|error| {
                AnchoredFsError::with_errno(
                    AnchoredFsErrorCode::FileSyncFailed,
                    error.raw_os_error(),
                )
            })?;
            let current = entry_identity(self.parent.as_raw_fd(), &self.stage_name)?;
            if current != self.identity {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::StageIdentityChanged,
                ));
            }
            Ok(())
        }
    }

    impl Drop for ExclusiveStage {
        fn drop(&mut self) {
            if !self.installed && self.remove_on_drop {
                unsafe {
                    libc::unlinkat(self.parent.as_raw_fd(), self.stage_name.as_ptr(), 0);
                }
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        device: libc::dev_t,
        inode: libc::ino_t,
    }

    fn relative_components(relative: &Path) -> AnchoredFsResult<Vec<CString>> {
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::InvalidRelativePath,
            ));
        }

        let mut output = Vec::new();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::InvalidRelativePath,
                ));
            };
            output.push(c_string(name)?);
        }
        if output.is_empty() {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::InvalidRelativePath,
            ));
        }
        Ok(output)
    }

    fn single_name(name: &OsStr) -> AnchoredFsResult<CString> {
        let mut components = Path::new(name).components();
        let Some(Component::Normal(component)) = components.next() else {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::InvalidRelativePath,
            ));
        };
        if components.next().is_some() {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::InvalidRelativePath,
            ));
        }
        c_string(component)
    }

    fn c_string(name: &OsStr) -> AnchoredFsResult<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| AnchoredFsError::new(AnchoredFsErrorCode::NameContainsNul))
    }

    fn resolve_parent(
        root_fd: RawFd,
        components: &[CString],
        create_missing: bool,
    ) -> AnchoredFsResult<(OwnedFd, &CString)> {
        let (name, parents) = components
            .split_last()
            .ok_or_else(|| AnchoredFsError::new(AnchoredFsErrorCode::InvalidRelativePath))?;
        let parent = walk_directories(root_fd, parents, create_missing)?;
        Ok((parent, name))
    }

    fn walk_directories(
        root_fd: RawFd,
        components: &[CString],
        create_missing: bool,
    ) -> AnchoredFsResult<OwnedFd> {
        let mut current = duplicate_fd(root_fd)?;
        for component in components {
            current = open_directory_at(current.as_raw_fd(), component, create_missing)?;
        }
        Ok(current)
    }

    fn open_directory_at(
        parent_fd: RawFd,
        name: &CString,
        create_missing: bool,
    ) -> AnchoredFsResult<OwnedFd> {
        match entry_kind(parent_fd, name)? {
            Some(EntryKind::Symlink) => {
                return Err(AnchoredFsError::new(
                    AnchoredFsErrorCode::SymlinkEncountered,
                ));
            }
            Some(EntryKind::Directory) => {}
            Some(EntryKind::Regular | EntryKind::Other) => {
                return Err(AnchoredFsError::new(AnchoredFsErrorCode::NotDirectory));
            }
            None if create_missing => {
                let result = unsafe { libc::mkdirat(parent_fd, name.as_ptr(), 0o700) };
                if result != 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EEXIST) {
                        return Err(error_from_io(error, AnchoredFsErrorCode::Io));
                    }
                } else {
                    sync_directory(parent_fd)?;
                }
            }
            None => {
                return Err(AnchoredFsError::new(AnchoredFsErrorCode::EntryNotFound));
            }
        }

        let raw_fd = unsafe {
            libc::openat(
                parent_fd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            )
        };
        if raw_fd < 0 {
            return Err(classify_directory_open_error(
                parent_fd,
                name,
                io::Error::last_os_error(),
            ));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        require_directory(fd.as_raw_fd())?;
        Ok(fd)
    }

    fn reject_known_symlink(parent_fd: RawFd, name: &CString) -> AnchoredFsResult<()> {
        if entry_kind(parent_fd, name)? == Some(EntryKind::Symlink) {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::SymlinkEncountered,
            ));
        }
        Ok(())
    }

    fn require_regular_entry(parent_fd: RawFd, name: &CString) -> AnchoredFsResult<()> {
        match entry_kind(parent_fd, name)? {
            None => Err(AnchoredFsError::new(AnchoredFsErrorCode::EntryNotFound)),
            Some(EntryKind::Symlink) => Err(AnchoredFsError::new(
                AnchoredFsErrorCode::SymlinkEncountered,
            )),
            Some(EntryKind::Regular) => Ok(()),
            Some(EntryKind::Other | EntryKind::Directory) => {
                Err(AnchoredFsError::new(AnchoredFsErrorCode::NotRegularFile))
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EntryKind {
        Directory,
        Symlink,
        Regular,
        Other,
    }

    fn entry_kind(parent_fd: RawFd, name: &CString) -> AnchoredFsResult<Option<EntryKind>> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent_fd,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            return Err(error_from_io(error, AnchoredFsErrorCode::Io));
        }
        let stat = unsafe { stat.assume_init() };
        let file_type = stat.st_mode & libc::S_IFMT;
        Ok(Some(if file_type == libc::S_IFDIR {
            EntryKind::Directory
        } else if file_type == libc::S_IFLNK {
            EntryKind::Symlink
        } else if file_type == libc::S_IFREG {
            EntryKind::Regular
        } else {
            EntryKind::Other
        }))
    }

    fn require_directory(fd: RawFd) -> AnchoredFsResult<()> {
        let stat = file_stat(fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(AnchoredFsError::new(AnchoredFsErrorCode::NotDirectory));
        }
        Ok(())
    }

    fn require_regular_file(fd: RawFd) -> AnchoredFsResult<()> {
        let stat = file_stat(fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(AnchoredFsError::new(AnchoredFsErrorCode::NotRegularFile));
        }
        Ok(())
    }

    fn file_identity(fd: RawFd) -> AnchoredFsResult<FileIdentity> {
        let stat = file_stat(fd)?;
        Ok(FileIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }

    fn entry_identity(parent_fd: RawFd, name: &CString) -> AnchoredFsResult<FileIdentity> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                parent_fd,
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::StageIdentityChanged,
            ));
        }
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::StageIdentityChanged,
            ));
        }
        Ok(FileIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }

    fn file_stat(fd: RawFd) -> AnchoredFsResult<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(unsafe { stat.assume_init() })
    }

    fn read_entry_bytes(
        parent_fd: RawFd,
        name: &CString,
        max_bytes: u64,
    ) -> AnchoredFsResult<Vec<u8>> {
        let raw_fd = unsafe {
            libc::openat(
                parent_fd,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        };
        if raw_fd < 0 {
            return Err(classify_file_open_error(
                parent_fd,
                name,
                io::Error::last_os_error(),
            ));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        require_regular_file(fd.as_raw_fd())?;
        let stat = file_stat(fd.as_raw_fd())?;
        if stat.st_size < 0 || stat.st_size as u64 > max_bytes {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::TargetIdentityChanged,
            ));
        }
        let mut file = File::from(fd);
        let mut bytes = Vec::with_capacity(stat.st_size as usize);
        file.by_ref()
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                AnchoredFsError::with_errno(AnchoredFsErrorCode::Io, error.raw_os_error())
            })?;
        if bytes.len() as u64 > max_bytes {
            return Err(AnchoredFsError::new(
                AnchoredFsErrorCode::TargetIdentityChanged,
            ));
        }
        Ok(bytes)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let mut output = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }

    fn sync_file(file: &File) -> io::Result<()> {
        file.sync_all()?;
        #[cfg(target_os = "macos")]
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn duplicate_fd(fd: RawFd) -> AnchoredFsResult<OwnedFd> {
        let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated < 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
    }

    fn sync_directory(fd: RawFd) -> AnchoredFsResult<()> {
        if unsafe { libc::fsync(fd) } != 0 {
            let error = io::Error::last_os_error();
            return Err(AnchoredFsError::with_errno(
                AnchoredFsErrorCode::DirectorySyncFailed,
                error.raw_os_error(),
            ));
        }
        Ok(())
    }

    fn classify_directory_open_error(
        parent_fd: RawFd,
        name: &CString,
        original: io::Error,
    ) -> AnchoredFsError {
        if let Ok(Some(kind)) = entry_kind(parent_fd, name) {
            return match kind {
                EntryKind::Symlink => AnchoredFsError::new(AnchoredFsErrorCode::SymlinkEncountered),
                EntryKind::Regular | EntryKind::Other => {
                    AnchoredFsError::new(AnchoredFsErrorCode::NotDirectory)
                }
                EntryKind::Directory => error_from_io(original, AnchoredFsErrorCode::Io),
            };
        }
        error_from_io(original, AnchoredFsErrorCode::Io)
    }

    fn classify_file_open_error(
        parent_fd: RawFd,
        name: &CString,
        original: io::Error,
    ) -> AnchoredFsError {
        if let Ok(Some(kind)) = entry_kind(parent_fd, name) {
            return match kind {
                EntryKind::Symlink => AnchoredFsError::new(AnchoredFsErrorCode::SymlinkEncountered),
                EntryKind::Directory => AnchoredFsError::new(AnchoredFsErrorCode::NotRegularFile),
                EntryKind::Regular | EntryKind::Other => {
                    error_from_io(original, AnchoredFsErrorCode::Io)
                }
            };
        }
        error_from_io(original, AnchoredFsErrorCode::Io)
    }

    fn error_from_io(error: io::Error, fallback: AnchoredFsErrorCode) -> AnchoredFsError {
        let raw = error.raw_os_error();
        let code = match raw {
            Some(libc::ENOENT) => AnchoredFsErrorCode::EntryNotFound,
            Some(libc::EEXIST) => AnchoredFsErrorCode::EntryAlreadyExists,
            Some(libc::ELOOP) => AnchoredFsErrorCode::SymlinkEncountered,
            Some(libc::ENOTDIR) => AnchoredFsErrorCode::NotDirectory,
            Some(libc::EACCES) | Some(libc::EPERM) => AnchoredFsErrorCode::PermissionDenied,
            _ => fallback,
        };
        AnchoredFsError::with_errno(code, raw)
    }

    #[cfg(target_os = "macos")]
    fn rename_no_replace(
        parent_fd: RawFd,
        stage_name: &CString,
        target_name: &CString,
    ) -> AnchoredFsResult<()> {
        let result = unsafe {
            libc::renameatx_np(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn rename_exchange(
        parent_fd: RawFd,
        stage_name: &CString,
        target_name: &CString,
    ) -> AnchoredFsResult<()> {
        let result = unsafe {
            libc::renameatx_np(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                libc::RENAME_SWAP,
            )
        };
        if result != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn rename_no_replace(
        parent_fd: RawFd,
        stage_name: &CString,
        target_name: &CString,
    ) -> AnchoredFsResult<()> {
        let result = unsafe {
            libc::renameat2(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn rename_exchange(
        parent_fd: RawFd,
        stage_name: &CString,
        target_name: &CString,
    ) -> AnchoredFsResult<()> {
        let result = unsafe {
            libc::renameat2(
                parent_fd,
                stage_name.as_ptr(),
                parent_fd,
                target_name.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        if result != 0 {
            return Err(error_from_io(
                io::Error::last_os_error(),
                AnchoredFsErrorCode::Io,
            ));
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn rename_no_replace(
        _parent_fd: RawFd,
        _stage_name: &CString,
        _target_name: &CString,
    ) -> AnchoredFsResult<()> {
        Err(AnchoredFsError::new(
            AnchoredFsErrorCode::UnsupportedPlatform,
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn rename_exchange(
        _parent_fd: RawFd,
        _stage_name: &CString,
        _target_name: &CString,
    ) -> AnchoredFsResult<()> {
        Err(AnchoredFsError::new(
            AnchoredFsErrorCode::UnsupportedPlatform,
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs::{self, OpenOptions};
        use std::io::{Read, Write};
        use std::os::unix::fs::{symlink, OpenOptionsExt};
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

        struct TestRoot {
            path: PathBuf,
        }

        impl TestRoot {
            fn new() -> Self {
                let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "wakegpt-anchored-fs-{}-{serial}",
                    std::process::id()
                ));
                fs::create_dir(&path).expect("create isolated test root");
                Self { path }
            }

            fn anchored(&self) -> AnchoredRoot {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                    .open(&self.path)
                    .expect("open test root");
                AnchoredRoot::from_owned_fd(file.into()).expect("accept test root descriptor")
            }

            fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
                self.path.join(relative)
            }
        }

        impl Drop for TestRoot {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.path).expect("remove isolated test root");
            }
        }

        #[test]
        fn rejects_symlink_parent() {
            let root = TestRoot::new();
            fs::create_dir(root.join("real")).unwrap();
            fs::write(root.join("real/note.md"), b"safe").unwrap();
            symlink("real", root.join("linked")).unwrap();

            let error = root
                .anchored()
                .open_regular_file_read(Path::new("linked/note.md"))
                .unwrap_err();

            assert_eq!(error.code(), AnchoredFsErrorCode::SymlinkEncountered);
        }

        #[test]
        fn rejects_symlink_target() {
            let root = TestRoot::new();
            fs::write(root.join("real.md"), b"safe").unwrap();
            symlink("real.md", root.join("linked.md")).unwrap();

            let error = root
                .anchored()
                .open_regular_file_read(Path::new("linked.md"))
                .unwrap_err();

            assert_eq!(error.code(), AnchoredFsErrorCode::SymlinkEncountered);
        }

        #[test]
        fn rejects_parent_traversal() {
            let root = TestRoot::new();
            let error = root
                .anchored()
                .open_regular_file_read(Path::new("../outside.md"))
                .unwrap_err();

            assert_eq!(error.code(), AnchoredFsErrorCode::InvalidRelativePath);
        }

        #[test]
        fn stage_creation_is_exclusive_and_installs_without_overwrite() {
            let root = TestRoot::new();
            let anchored = root.anchored();
            let mut stage = anchored
                .create_exclusive_stage(Path::new("notes/note.md"), OsStr::new(".note.stage"), true)
                .unwrap();
            stage.file_mut().write_all(b"first").unwrap();

            let duplicate_error = anchored
                .create_exclusive_stage(
                    Path::new("notes/note.md"),
                    OsStr::new(".note.stage"),
                    false,
                )
                .err()
                .expect("second stage name must be rejected");
            assert_eq!(
                duplicate_error.code(),
                AnchoredFsErrorCode::EntryAlreadyExists
            );

            stage.install_new().unwrap();
            let mut installed = anchored
                .open_regular_file_read(Path::new("notes/note.md"))
                .unwrap();
            let mut text = String::new();
            installed.read_to_string(&mut text).unwrap();
            assert_eq!(text, "first");

            let mut replacement_stage = anchored
                .create_exclusive_stage(
                    Path::new("notes/note.md"),
                    OsStr::new(".note.stage-2"),
                    false,
                )
                .unwrap();
            replacement_stage.file_mut().write_all(b"second").unwrap();
            let error = replacement_stage.install_new().unwrap_err();
            assert_eq!(error.code(), AnchoredFsErrorCode::EntryAlreadyExists);
            assert_eq!(fs::read(root.join("notes/note.md")).unwrap(), b"first");

            let mut replacement_stage = anchored
                .create_exclusive_stage(
                    Path::new("notes/note.md"),
                    OsStr::new(".note.stage-3"),
                    false,
                )
                .unwrap();
            replacement_stage.file_mut().write_all(b"second").unwrap();
            replacement_stage
                .replace_existing_cas(&sha256_hex(b"first"), 1024)
                .unwrap();
            assert_eq!(fs::read(root.join("notes/note.md")).unwrap(), b"second");
        }

        #[test]
        fn compare_and_swap_rolls_back_when_the_target_changed() {
            let root = TestRoot::new();
            fs::create_dir(root.join("notes")).unwrap();
            fs::write(root.join("notes/note.md"), b"expected").unwrap();
            let anchored = root.anchored();
            let mut stage = anchored
                .create_exclusive_stage(
                    Path::new("notes/note.md"),
                    OsStr::new(".note.stage"),
                    false,
                )
                .unwrap();
            stage.file_mut().write_all(b"replacement").unwrap();
            fs::write(root.join("notes/note.md"), b"external change").unwrap();

            let error = stage
                .replace_existing_cas(&sha256_hex(b"expected"), 1024)
                .unwrap_err();

            assert_eq!(error.code(), AnchoredFsErrorCode::TargetIdentityChanged);
            assert_eq!(
                fs::read(root.join("notes/note.md")).unwrap(),
                b"external change"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::{AnchoredRoot, ExclusiveStage};

#[cfg(not(unix))]
pub(crate) fn unsupported_platform() -> AnchoredFsError {
    AnchoredFsError::new(AnchoredFsErrorCode::UnsupportedPlatform)
}
