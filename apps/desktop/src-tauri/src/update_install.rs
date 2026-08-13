use crate::storage::{Store, StoreUpdateGuard};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri_plugin_updater::Update;
use uuid::Uuid;

#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

const UPDATE_DIRECTORY: &str = "updates-v1";
const PREPARED_BUNDLE: &str = "WakeGPT_universal.app.tar.gz";
const PREPARED_RECEIPT: &str = "prepared-update-v1.json";
const INSTALL_INTENT: &str = "install-intent-v1.json";
const INSTALL_HEALTH: &str = "install-health-v1.json";
const INSTALL_RESULT: &str = "install-result-v1.json";
const DATABASE_BACKUP: &str = "wakegpt-before-update.sqlite3";
const DATABASE_NAME: &str = "wakegpt.sqlite3";
const CURRENT_APP: &str = "/Applications/WakeGPT.app";
const EXECUTABLE_RELATIVE: &str = "Contents/MacOS/wakegpt-desktop";
const MAX_CONTROL_BYTES: u64 = 128 * 1024;
pub(crate) const MAX_UPDATE_BYTES: u64 = 256 * 1024 * 1024;
const GUARDIAN_ARGUMENT: &str = "--wakegpt-update-guardian";
const HEALTH_PROBE_ARGUMENT: &str = "--wakegpt-update-health-probe";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreparedUpdateReceipt {
    pub schema_version: u32,
    pub transaction_id: String,
    pub from_version: String,
    pub version: String,
    pub download_url: String,
    pub signature: String,
    pub sha256: String,
    pub byte_size: u64,
    pub downloaded_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallIntent {
    schema_version: u32,
    transaction_id: String,
    phase: String,
    from_version: String,
    version: String,
    old_executable_sha256: String,
    new_bundle_sha256: String,
    database_backup_sha256: String,
    database_backup_bytes: u64,
    team_identifier: String,
    rollback_reason: Option<String>,
    started_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstallHealth {
    schema_version: u32,
    transaction_id: String,
    version: String,
    database_schema_version: u32,
    healthy_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateInstallResult {
    pub schema_version: u32,
    pub outcome: String,
    pub from_version: String,
    pub version: String,
    pub error_code: Option<String>,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone)]
struct AppIdentity {
    version: String,
    team_identifier: String,
    executable_sha256: String,
}

#[derive(Debug)]
pub(crate) struct UpdateInstallError(pub(crate) &'static str);

impl std::fmt::Display for UpdateInstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for UpdateInstallError {}

type InstallResult<T> = Result<T, UpdateInstallError>;

fn now_ms() -> i64 {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn valid_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok() && value.len() == 36
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn update_root(app_data_dir: &Path) -> InstallResult<PathBuf> {
    let app_metadata = fs::symlink_metadata(app_data_dir)
        .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    if app_metadata.file_type().is_symlink() || !app_metadata.is_dir() {
        return Err(UpdateInstallError("update_storage_unavailable"));
    }
    #[cfg(unix)]
    if app_metadata.uid() != unsafe { libc::geteuid() } || app_metadata.mode() & 0o022 != 0 {
        return Err(UpdateInstallError("update_storage_unsafe"));
    }
    let root = app_data_dir.join(UPDATE_DIRECTORY);
    match fs::symlink_metadata(&root) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => return Err(UpdateInstallError("update_storage_unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder
                    .create(&root)
                    .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
            }
            #[cfg(not(unix))]
            fs::create_dir(&root).map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
            sync_directory(app_data_dir)?;
        }
        Err(_) => return Err(UpdateInstallError("update_storage_unavailable")),
    }
    #[cfg(unix)]
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    #[cfg(unix)]
    {
        let root_metadata = fs::symlink_metadata(&root)
            .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
        if root_metadata.uid() != unsafe { libc::geteuid() }
            || root_metadata.mode() & 0o777 != 0o700
        {
            return Err(UpdateInstallError("update_storage_unsafe"));
        }
    }
    Ok(root)
}

fn secure_regular_metadata(path: &Path, max_bytes: u64) -> InstallResult<fs::Metadata> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(UpdateInstallError("update_state_invalid"));
    }
    Ok(metadata)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> InstallResult<Option<T>> {
    let before = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(UpdateInstallError("update_state_unavailable")),
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_CONTROL_BYTES =>
        {
            return Err(UpdateInstallError("update_state_invalid"));
        }
        Ok(metadata) => metadata,
    };
    #[cfg(unix)]
    if before.uid() != unsafe { libc::geteuid() }
        || before.mode() & 0o077 != 0
        || before.nlink() != 1
    {
        return Err(UpdateInstallError("update_state_invalid"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_CONTROL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        return Err(UpdateInstallError("update_state_invalid"));
    }
    let after = file
        .metadata()
        .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    #[cfg(unix)]
    if after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.nlink() != 1
        || after.uid() != unsafe { libc::geteuid() }
    {
        return Err(UpdateInstallError("update_state_changed"));
    }
    if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
        return Err(UpdateInstallError("update_state_changed"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| UpdateInstallError("update_state_invalid"))
}

fn write_json(path: &Path, value: &impl Serialize) -> InstallResult<()> {
    let parent = path
        .parent()
        .ok_or(UpdateInstallError("update_storage_unavailable"))?;
    let bytes =
        serde_json::to_vec(value).map_err(|_| UpdateInstallError("update_state_invalid"))?;
    if bytes.len() as u64 > MAX_CONTROL_BYTES {
        return Err(UpdateInstallError("update_state_invalid"));
    }
    let temporary = parent.join(format!(
        ".{}.{}.partial",
        path.file_name().and_then(OsStr::to_str).unwrap_or("update"),
        Uuid::now_v7()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options
            .open(&temporary)
            .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
        file.write_all(&bytes)
            .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
        sync_file(&file)?;
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(UpdateInstallError("update_state_invalid"));
            }
        }
        fs::rename(&temporary, path)
            .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn sync_file(file: &File) -> InstallResult<()> {
    file.sync_all()
        .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    #[cfg(target_os = "macos")]
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
        return Err(UpdateInstallError("update_storage_unavailable"));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> InstallResult<()> {
    let file = File::open(path).map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    file.sync_all()
        .map_err(|_| UpdateInstallError("update_storage_unavailable"))
}

fn sha256_file(path: &Path, max_bytes: u64) -> InstallResult<(String, u64)> {
    let metadata = secure_regular_metadata(path, max_bytes)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(UpdateInstallError("update_state_invalid"))?;
        if total > max_bytes {
            return Err(UpdateInstallError("update_state_invalid"));
        }
        hasher.update(&buffer[..read]);
    }
    let after = file
        .metadata()
        .map_err(|_| UpdateInstallError("update_state_unavailable"))?;
    if total != metadata.len()
        || after.len() != metadata.len()
        || after.modified().ok() != metadata.modified().ok()
    {
        return Err(UpdateInstallError("update_state_changed"));
    }
    #[cfg(unix)]
    if after.dev() != metadata.dev() || after.ino() != metadata.ino() {
        return Err(UpdateInstallError("update_state_changed"));
    }
    let digest = hasher.finalize();
    let mut sha256 = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(sha256, "{byte:02x}");
    }
    Ok((sha256, total))
}

fn validate_prepared(receipt: &PreparedUpdateReceipt) -> InstallResult<()> {
    if receipt.schema_version != 1
        || !valid_id(&receipt.transaction_id)
        || !valid_version(&receipt.from_version)
        || !valid_version(&receipt.version)
        || receipt.version == receipt.from_version
        || !valid_sha256(&receipt.sha256)
        || receipt.byte_size == 0
        || receipt.byte_size > MAX_UPDATE_BYTES
        || receipt.download_url.len() > 2_048
        || !receipt.download_url.starts_with("https://github.com/")
        || !(32..=4_096).contains(&receipt.signature.len())
        || receipt.downloaded_at_ms < 0
    {
        return Err(UpdateInstallError("update_prepared_invalid"));
    }
    Ok(())
}

fn validate_install_intent(intent: &InstallIntent) -> InstallResult<()> {
    if intent.schema_version != 1
        || !valid_id(&intent.transaction_id)
        || ![
            "prepared",
            "installing",
            "installed_pending_health",
            "rollback_requested",
            "rollback_app_restored",
            "rollback_database_restored",
        ]
        .contains(&intent.phase.as_str())
        || !valid_version(&intent.from_version)
        || !valid_version(&intent.version)
        || intent.version == intent.from_version
        || !valid_sha256(&intent.old_executable_sha256)
        || !valid_sha256(&intent.new_bundle_sha256)
        || !valid_sha256(&intent.database_backup_sha256)
        || intent.database_backup_bytes == 0
        || intent.database_backup_bytes > MAX_UPDATE_BYTES
        || intent.team_identifier.is_empty()
        || intent.team_identifier.len() > 64
        || !intent
            .team_identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        || intent.rollback_reason.as_deref().is_some_and(|value| {
            ![
                "update_install_failed",
                "update_installed_app_invalid",
                "update_guardian_start_failed",
                "update_new_app_start_failed",
                "update_new_app_unhealthy",
                "update_install_interrupted",
            ]
            .contains(&value)
        })
        || intent.started_at_ms < 0
    {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }
    Ok(())
}

pub(crate) fn load_prepared(app_data_dir: &Path) -> InstallResult<Option<PreparedUpdateReceipt>> {
    let root = update_root(app_data_dir)?;
    let Some(receipt) = read_json::<PreparedUpdateReceipt>(&root.join(PREPARED_RECEIPT))? else {
        return Ok(None);
    };
    validate_prepared(&receipt)?;
    let (sha256, byte_size) = sha256_file(&root.join(PREPARED_BUNDLE), MAX_UPDATE_BYTES)?;
    if sha256 != receipt.sha256 || byte_size != receipt.byte_size {
        return Err(UpdateInstallError("update_prepared_changed"));
    }
    Ok(Some(receipt))
}

pub(crate) fn prepared_bundle_path(app_data_dir: &Path) -> InstallResult<PathBuf> {
    Ok(update_root(app_data_dir)?.join(PREPARED_BUNDLE))
}

pub(crate) fn create_download_stage(
    app_data_dir: &Path,
    transaction_id: &str,
) -> InstallResult<(File, PathBuf)> {
    if !valid_id(transaction_id) {
        return Err(UpdateInstallError("update_request_invalid"));
    }
    let root = update_root(app_data_dir)?;
    if root.join(INSTALL_INTENT).exists() {
        return Err(UpdateInstallError("update_install_in_progress"));
    }
    discard_prepared(app_data_dir)?;
    let path = root.join(format!(".download-{transaction_id}.partial"));
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(&path)
        .map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    Ok((file, path))
}

pub(crate) fn sync_download_stage(file: &File) -> InstallResult<()> {
    sync_file(file)
}

pub(crate) fn abort_download_stage(app_data_dir: &Path, stage: &Path) -> InstallResult<()> {
    let root = update_root(app_data_dir)?;
    let Some(name) = stage.file_name().and_then(OsStr::to_str) else {
        return Err(UpdateInstallError("update_stage_invalid"));
    };
    if stage.parent() != Some(root.as_path())
        || !name.starts_with(".download-")
        || !name.ends_with(".partial")
    {
        return Err(UpdateInstallError("update_stage_invalid"));
    }
    remove_regular_if_present(stage)?;
    sync_directory(&root)
}

pub(crate) fn commit_download(
    app_data_dir: &Path,
    stage: &Path,
    receipt: &PreparedUpdateReceipt,
) -> InstallResult<()> {
    validate_prepared(receipt)?;
    let root = update_root(app_data_dir)?;
    let expected_stage_name = format!(".download-{}.partial", receipt.transaction_id);
    if stage.parent() != Some(root.as_path())
        || stage.file_name().and_then(OsStr::to_str) != Some(expected_stage_name.as_str())
    {
        return Err(UpdateInstallError("update_stage_invalid"));
    }
    let (sha256, byte_size) = sha256_file(stage, MAX_UPDATE_BYTES)?;
    if sha256 != receipt.sha256 || byte_size != receipt.byte_size {
        return Err(UpdateInstallError("update_stage_changed"));
    }
    let target = root.join(PREPARED_BUNDLE);
    if target.exists() || root.join(PREPARED_RECEIPT).exists() {
        return Err(UpdateInstallError("update_prepared_collision"));
    }
    fs::rename(stage, &target).map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
    sync_directory(&root)?;
    if let Err(error) = write_json(&root.join(PREPARED_RECEIPT), receipt) {
        let _ = fs::remove_file(target);
        return Err(error);
    }
    Ok(())
}

fn remove_regular_if_present(path: &Path) -> InstallResult<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(UpdateInstallError("update_storage_unavailable")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(UpdateInstallError("update_state_invalid"))
        }
        Ok(_) => {
            fs::remove_file(path).map_err(|_| UpdateInstallError("update_storage_unavailable"))
        }
    }
}

pub(crate) fn discard_prepared(app_data_dir: &Path) -> InstallResult<()> {
    let root = update_root(app_data_dir)?;
    if root.join(INSTALL_INTENT).exists() {
        return Err(UpdateInstallError("update_install_in_progress"));
    }
    remove_regular_if_present(&root.join(PREPARED_RECEIPT))?;
    remove_regular_if_present(&root.join(PREPARED_BUNDLE))?;
    for entry in
        fs::read_dir(&root).map_err(|_| UpdateInstallError("update_storage_unavailable"))?
    {
        let entry = entry.map_err(|_| UpdateInstallError("update_storage_unavailable"))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".download-") {
            remove_regular_if_present(&entry.path())?;
        }
    }
    sync_directory(&root)
}

pub(crate) fn load_install_result(
    app_data_dir: &Path,
) -> InstallResult<Option<UpdateInstallResult>> {
    let root = update_root(app_data_dir)?;
    let result = read_json::<UpdateInstallResult>(&root.join(INSTALL_RESULT))?;
    if let Some(value) = &result {
        if value.schema_version != 1
            || !["installed", "rolledBack", "rollbackFailed"].contains(&value.outcome.as_str())
            || !valid_version(&value.from_version)
            || !valid_version(&value.version)
            || value.occurred_at_ms < 0
        {
            return Err(UpdateInstallError("update_result_invalid"));
        }
    }
    Ok(result)
}

pub(crate) fn install_in_progress(app_data_dir: &Path) -> InstallResult<bool> {
    let root = update_root(app_data_dir)?;
    match read_json::<InstallIntent>(&root.join(INSTALL_INTENT))? {
        Some(intent) => {
            validate_install_intent(&intent)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub(crate) fn acknowledge_install_result(app_data_dir: &Path) -> InstallResult<()> {
    let root = update_root(app_data_dir)?;
    if install_in_progress(app_data_dir)? {
        return Err(UpdateInstallError("update_install_in_progress"));
    }
    if let Some(result) = read_json::<UpdateInstallResult>(&root.join(INSTALL_RESULT))? {
        if result.schema_version != 1
            || !["installed", "rolledBack", "rollbackFailed"].contains(&result.outcome.as_str())
        {
            return Err(UpdateInstallError("update_result_invalid"));
        }
    }
    remove_regular_if_present(&root.join(INSTALL_RESULT))?;
    sync_directory(&root)
}

fn backup_app_path(transaction_id: &str) -> PathBuf {
    Path::new("/Applications").join(format!(".WakeGPT-update-backup-{transaction_id}.app"))
}

fn spawn_guardian(intent: &InstallIntent, parent_pid: u32) -> InstallResult<()> {
    validate_install_intent(intent)?;
    if parent_pid == 0 {
        return Err(UpdateInstallError("update_guardian_argument_invalid"));
    }
    let backup = backup_app_path(&intent.transaction_id);
    let backup_identity =
        verify_release_app(&backup, &intent.from_version, Some(&intent.team_identifier))?;
    if backup_identity.executable_sha256 != intent.old_executable_sha256 {
        return Err(UpdateInstallError("update_backup_changed"));
    }
    Command::new(backup.join(EXECUTABLE_RELATIVE))
        .arg(GUARDIAN_ARGUMENT)
        .arg(&intent.transaction_id)
        .arg(parent_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| UpdateInstallError("update_guardian_start_failed"))
}

fn run_checked(command: &str, arguments: &[&OsStr]) -> InstallResult<String> {
    let output = Command::new(command)
        .args(arguments)
        .output()
        .map_err(|_| UpdateInstallError("update_platform_command_failed"))?;
    if !output.status.success() {
        return Err(UpdateInstallError("update_platform_command_failed"));
    }
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn plist_value(info: &Path, key: &str) -> InstallResult<String> {
    Ok(run_checked(
        "/usr/bin/plutil",
        &[
            OsStr::new("-extract"),
            OsStr::new(key),
            OsStr::new("raw"),
            OsStr::new("-o"),
            OsStr::new("-"),
            info.as_os_str(),
        ],
    )?
    .trim()
    .to_owned())
}

#[cfg(target_os = "macos")]
fn verify_release_app(
    app: &Path,
    expected_version: &str,
    expected_team: Option<&str>,
) -> InstallResult<AppIdentity> {
    let metadata =
        fs::symlink_metadata(app).map_err(|_| UpdateInstallError("update_app_unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(UpdateInstallError("update_app_invalid"));
    }
    run_checked(
        "/usr/bin/codesign",
        &[
            OsStr::new("--verify"),
            OsStr::new("--deep"),
            OsStr::new("--strict"),
            OsStr::new("--verbose=2"),
            app.as_os_str(),
        ],
    )?;
    let signature = run_checked(
        "/usr/bin/codesign",
        &[
            OsStr::new("-dv"),
            OsStr::new("--verbose=4"),
            app.as_os_str(),
        ],
    )?;
    if !signature
        .lines()
        .any(|line| line.starts_with("Authority=Developer ID Application:"))
        || !signature.lines().any(|line| line.starts_with("Timestamp="))
        || !signature
            .lines()
            .any(|line| line.starts_with("flags=") && line.contains("runtime"))
    {
        return Err(UpdateInstallError("update_app_signature_invalid"));
    }
    let team_identifier = signature
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|value| !value.is_empty() && *value != "not set")
        .ok_or(UpdateInstallError("update_app_signature_invalid"))?
        .to_owned();
    if expected_team.is_some_and(|value| value != team_identifier) {
        return Err(UpdateInstallError("update_app_team_mismatch"));
    }
    run_checked(
        "/usr/bin/xcrun",
        &[
            OsStr::new("stapler"),
            OsStr::new("validate"),
            app.as_os_str(),
        ],
    )?;
    let info = app.join("Contents/Info.plist");
    if plist_value(&info, "CFBundleIdentifier")? != "com.wakegpt.desktop"
        || plist_value(&info, "CFBundleExecutable")? != "wakegpt-desktop"
        || plist_value(&info, "CFBundleShortVersionString")? != expected_version
        || plist_value(&info, "CFBundleVersion")? != expected_version
    {
        return Err(UpdateInstallError("update_app_identity_invalid"));
    }
    let executable = app.join(EXECUTABLE_RELATIVE);
    let architectures = run_checked(
        "/usr/bin/lipo",
        &[OsStr::new("-archs"), executable.as_os_str()],
    )?;
    let architectures = architectures
        .split_whitespace()
        .collect::<std::collections::HashSet<_>>();
    if architectures.len() != 2
        || !architectures.contains("arm64")
        || !architectures.contains("x86_64")
    {
        return Err(UpdateInstallError("update_app_architecture_invalid"));
    }
    Ok(AppIdentity {
        version: expected_version.to_owned(),
        team_identifier,
        executable_sha256: sha256_file(&executable, MAX_UPDATE_BYTES)?.0,
    })
}

#[cfg(not(target_os = "macos"))]
fn verify_release_app(
    _app: &Path,
    _expected_version: &str,
    _expected_team: Option<&str>,
) -> InstallResult<AppIdentity> {
    Err(UpdateInstallError("update_platform_unsupported"))
}

#[cfg(target_os = "macos")]
fn current_release_app(expected_version: &str) -> InstallResult<AppIdentity> {
    let executable =
        std::env::current_exe().map_err(|_| UpdateInstallError("update_app_unavailable"))?;
    let expected = Path::new(CURRENT_APP).join(EXECUTABLE_RELATIVE);
    if executable != expected {
        return Err(UpdateInstallError("update_install_location_unsupported"));
    }
    verify_release_app(Path::new(CURRENT_APP), expected_version, None)
}

#[cfg(not(target_os = "macos"))]
fn current_release_app(_expected_version: &str) -> InstallResult<AppIdentity> {
    Err(UpdateInstallError("update_platform_unsupported"))
}

fn remove_app_directory(path: &Path, transaction_id: &str) -> InstallResult<()> {
    let expected = backup_app_path(transaction_id);
    if path != expected {
        return Err(UpdateInstallError("update_backup_path_invalid"));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(UpdateInstallError("update_backup_unavailable")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(UpdateInstallError("update_backup_invalid"))
        }
        Ok(_) => {
            fs::remove_dir_all(path).map_err(|_| UpdateInstallError("update_backup_unavailable"))
        }
    }
}

#[cfg(target_os = "macos")]
fn rename_swap(left: &Path, right: &Path) -> InstallResult<()> {
    let parent = left
        .parent()
        .filter(|parent| Some(*parent) == right.parent())
        .ok_or(UpdateInstallError("update_atomic_swap_failed"))?;
    let left = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| UpdateInstallError("update_path_invalid"))?;
    let right = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| UpdateInstallError("update_path_invalid"))?;
    if unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    } != 0
    {
        return Err(UpdateInstallError("update_atomic_swap_failed"));
    }
    sync_directory(parent)
}

#[cfg(not(target_os = "macos"))]
fn rename_swap(_left: &Path, _right: &Path) -> InstallResult<()> {
    Err(UpdateInstallError("update_platform_unsupported"))
}

fn copy_current_app(transaction_id: &str, current: &AppIdentity) -> InstallResult<PathBuf> {
    let backup = backup_app_path(transaction_id);
    if backup.exists() {
        return Err(UpdateInstallError("update_backup_collision"));
    }
    let copied = (|| {
        run_checked(
            "/usr/bin/ditto",
            &[Path::new(CURRENT_APP).as_os_str(), backup.as_os_str()],
        )?;
        let identity =
            verify_release_app(&backup, &current.version, Some(&current.team_identifier))?;
        if identity.executable_sha256 != current.executable_sha256 {
            return Err(UpdateInstallError("update_backup_verification_failed"));
        }
        sync_directory(Path::new("/Applications"))
    })();
    if let Err(error) = copied {
        let _ = remove_app_directory(&backup, transaction_id);
        return Err(error);
    }
    Ok(backup)
}

fn identity_matches_old(identity: &AppIdentity, intent: &InstallIntent) -> bool {
    identity.version == intent.from_version
        && identity.team_identifier == intent.team_identifier
        && identity.executable_sha256 == intent.old_executable_sha256
}

fn identity_matches_new(identity: &AppIdentity, intent: &InstallIntent) -> bool {
    identity.version == intent.version && identity.team_identifier == intent.team_identifier
}

fn restore_app_from_backup(intent: &InstallIntent) -> InstallResult<()> {
    let current = Path::new(CURRENT_APP);
    let backup = backup_app_path(&intent.transaction_id);
    if verify_release_app(current, &intent.from_version, Some(&intent.team_identifier))
        .is_ok_and(|identity| identity_matches_old(&identity, intent))
    {
        match fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(UpdateInstallError("update_backup_unavailable")),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(UpdateInstallError("update_backup_invalid"));
            }
            Ok(_) => {}
        }
        let backup_is_old =
            verify_release_app(&backup, &intent.from_version, Some(&intent.team_identifier))
                .is_ok_and(|identity| identity_matches_old(&identity, intent));
        let backup_is_new =
            verify_release_app(&backup, &intent.version, Some(&intent.team_identifier))
                .is_ok_and(|identity| identity_matches_new(&identity, intent));
        if !backup_is_old && !backup_is_new {
            return Err(UpdateInstallError("update_backup_changed"));
        }
        remove_app_directory(&backup, &intent.transaction_id)?;
        return Ok(());
    }
    if matches!(fs::symlink_metadata(&backup), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        let restored =
            verify_release_app(current, &intent.from_version, Some(&intent.team_identifier))?;
        return if identity_matches_old(&restored, intent) {
            Ok(())
        } else {
            Err(UpdateInstallError("update_backup_unavailable"))
        };
    }
    let backup_identity =
        verify_release_app(&backup, &intent.from_version, Some(&intent.team_identifier))?;
    if !identity_matches_old(&backup_identity, intent) {
        return Err(UpdateInstallError("update_backup_changed"));
    }
    match fs::symlink_metadata(current) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
            rename_swap(current, &backup)?;
            let restored =
                verify_release_app(current, &intent.from_version, Some(&intent.team_identifier))?;
            if !identity_matches_old(&restored, intent) {
                let _ = rename_swap(current, &backup);
                return Err(UpdateInstallError("update_rollback_verification_failed"));
            }
            remove_app_directory(&backup, &intent.transaction_id)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(&backup, current)
                .map_err(|_| UpdateInstallError("update_rollback_failed"))?;
            sync_directory(Path::new("/Applications"))?;
            let restored =
                verify_release_app(current, &intent.from_version, Some(&intent.team_identifier))?;
            if !identity_matches_old(&restored, intent) {
                return Err(UpdateInstallError("update_rollback_verification_failed"));
            }
        }
        _ => return Err(UpdateInstallError("update_app_invalid")),
    }
    Ok(())
}

fn restore_database(root: &Path, intent: &InstallIntent) -> InstallResult<()> {
    let backup = root.join(DATABASE_BACKUP);
    let (sha256, bytes) = sha256_file(&backup, MAX_UPDATE_BYTES)?;
    if sha256 != intent.database_backup_sha256 || bytes != intent.database_backup_bytes {
        return Err(UpdateInstallError("update_database_backup_changed"));
    }
    let app_data_dir = root
        .parent()
        .ok_or(UpdateInstallError("update_storage_unavailable"))?;
    let database = app_data_dir.join(DATABASE_NAME);
    let stage = app_data_dir.join(format!(
        ".wakegpt-update-rollback-{}.sqlite3",
        intent.transaction_id
    ));
    if stage.exists() {
        remove_regular_if_present(&stage)?;
    }
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = OsString::from(database.as_os_str());
        sidecar.push(suffix);
        match fs::symlink_metadata(Path::new(&sidecar)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(UpdateInstallError("update_database_sidecar_present")),
        }
    }
    let mut source_options = OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    source_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut source = source_options
        .open(&backup)
        .map_err(|_| UpdateInstallError("update_database_restore_failed"))?;
    let mut stage_options = OpenOptions::new();
    stage_options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    stage_options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut stage_file = stage_options
        .open(&stage)
        .map_err(|_| UpdateInstallError("update_database_restore_failed"))?;
    let copy_result = io::copy(&mut source, &mut stage_file)
        .map_err(|_| UpdateInstallError("update_database_restore_failed"));
    if copy_result.is_err() {
        drop(stage_file);
        let _ = remove_regular_if_present(&stage);
        return copy_result.map(|_| ());
    }
    sync_file(&stage_file)?;
    drop(stage_file);
    let (stage_sha256, stage_bytes) = sha256_file(&stage, MAX_UPDATE_BYTES)?;
    if stage_sha256 != sha256 || stage_bytes != bytes {
        let _ = remove_regular_if_present(&stage);
        return Err(UpdateInstallError("update_database_restore_failed"));
    }
    if database.exists() {
        let metadata = fs::symlink_metadata(&database)
            .map_err(|_| UpdateInstallError("update_database_restore_failed"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(UpdateInstallError("update_database_restore_failed"));
        }
        rename_swap(&stage, &database)?;
        remove_regular_if_present(&stage)?;
    } else {
        fs::rename(&stage, &database)
            .map_err(|_| UpdateInstallError("update_database_restore_failed"))?;
        sync_directory(app_data_dir)?;
    }
    Ok(())
}

fn write_result(
    root: &Path,
    intent: &InstallIntent,
    outcome: &str,
    error: Option<&str>,
) -> InstallResult<()> {
    write_json(
        &root.join(INSTALL_RESULT),
        &UpdateInstallResult {
            schema_version: 1,
            outcome: outcome.to_owned(),
            from_version: intent.from_version.clone(),
            version: intent.version.clone(),
            error_code: error.map(str::to_owned),
            occurred_at_ms: now_ms(),
        },
    )
}

fn cleanup_transaction(
    root: &Path,
    transaction_id: &str,
    remove_backup: bool,
) -> InstallResult<()> {
    for name in [
        INSTALL_HEALTH,
        DATABASE_BACKUP,
        PREPARED_BUNDLE,
        PREPARED_RECEIPT,
    ] {
        remove_regular_if_present(&root.join(name))?;
    }
    if remove_backup {
        remove_app_directory(&backup_app_path(transaction_id), transaction_id)?;
    }
    remove_regular_if_present(&root.join(INSTALL_INTENT))?;
    sync_directory(root)
}

fn cleanup_unstarted_install(root: &Path, transaction_id: &str) -> InstallResult<()> {
    for name in [INSTALL_HEALTH, DATABASE_BACKUP] {
        remove_regular_if_present(&root.join(name))?;
    }
    remove_app_directory(&backup_app_path(transaction_id), transaction_id)?;
    remove_regular_if_present(&root.join(INSTALL_INTENT))?;
    sync_directory(root)
}

pub(crate) fn recover_after_install_worker_failure(
    app_data_dir: &Path,
    transaction_id: &str,
) -> InstallResult<bool> {
    if !valid_id(transaction_id) {
        return Err(UpdateInstallError("update_request_invalid"));
    }
    let root = update_root(app_data_dir)?;
    match read_json::<InstallIntent>(&root.join(INSTALL_INTENT))? {
        Some(intent) => {
            validate_install_intent(&intent)?;
            if intent.transaction_id != transaction_id {
                return Err(UpdateInstallError("update_install_intent_invalid"));
            }
            Ok(true)
        }
        None => {
            cleanup_unstarted_install(&root, transaction_id)?;
            Ok(false)
        }
    }
}

pub(crate) fn install_prepared(
    app_data_dir: &Path,
    store: &Store,
    store_guard: &mut StoreUpdateGuard<'_>,
    receipt: &PreparedUpdateReceipt,
    update: &Update,
    bytes: &[u8],
) -> InstallResult<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app_data_dir, store, store_guard, receipt, update, bytes);
        return Err(UpdateInstallError("update_platform_unsupported"));
    }
    #[cfg(target_os = "macos")]
    {
        validate_prepared(receipt)?;
        if bytes.len() as u64 != receipt.byte_size {
            return Err(UpdateInstallError("update_prepared_changed"));
        }
        let mut bundle_hasher = Sha256::new();
        bundle_hasher.update(bytes);
        let bundle_sha256 = bundle_hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if bundle_sha256 != receipt.sha256 {
            return Err(UpdateInstallError("update_prepared_changed"));
        }
        if update.current_version != receipt.from_version
            || update.version != receipt.version
            || update.download_url.as_str() != receipt.download_url
            || update.signature != receipt.signature
        {
            return Err(UpdateInstallError("update_metadata_changed"));
        }
        let root = update_root(app_data_dir)?;
        if root.join(INSTALL_INTENT).exists() {
            return Err(UpdateInstallError("update_install_in_progress"));
        }
        let current = current_release_app(&receipt.from_version)?;
        let backup = copy_current_app(&receipt.transaction_id, &current)?;
        let database_backup_path = root.join(DATABASE_BACKUP);
        let database_backup =
            match store.backup_database_for_update(store_guard, &database_backup_path) {
                Ok(value) => value,
                Err(_) => {
                    remove_app_directory(&backup, &receipt.transaction_id)?;
                    return Err(UpdateInstallError("update_database_backup_failed"));
                }
            };
        let intent = InstallIntent {
            schema_version: 1,
            transaction_id: receipt.transaction_id.clone(),
            phase: "installing".to_owned(),
            from_version: receipt.from_version.clone(),
            version: receipt.version.clone(),
            old_executable_sha256: current.executable_sha256.clone(),
            new_bundle_sha256: receipt.sha256.clone(),
            database_backup_sha256: database_backup.sha256,
            database_backup_bytes: database_backup.byte_size,
            team_identifier: current.team_identifier.clone(),
            rollback_reason: None,
            started_at_ms: now_ms(),
        };
        if let Err(error) = write_json(&root.join(INSTALL_INTENT), &intent) {
            cleanup_unstarted_install(&root, &intent.transaction_id)?;
            return Err(error);
        }
        if let Err(error) = spawn_guardian(&intent, std::process::id()) {
            cleanup_unstarted_install(&root, &intent.transaction_id)?;
            return Err(error);
        }

        let mut next_intent = intent.clone();
        if update.install(bytes).is_err() {
            next_intent.phase = "rollback_requested".to_owned();
            next_intent.rollback_reason = Some("update_install_failed".to_owned());
        } else if verify_release_app(
            Path::new(CURRENT_APP),
            &receipt.version,
            Some(&current.team_identifier),
        )
        .is_err()
        {
            next_intent.phase = "rollback_requested".to_owned();
            next_intent.rollback_reason = Some("update_installed_app_invalid".to_owned());
        } else {
            next_intent.phase = "installed_pending_health".to_owned();
        }
        let _ = write_json(&root.join(INSTALL_INTENT), &next_intent);
        Ok(())
    }
}

fn record_startup_health(
    app_data_dir: &Path,
    transaction_id: &str,
    version: &str,
    database_schema_version: u32,
) -> InstallResult<()> {
    if !valid_id(transaction_id) || !valid_version(version) {
        return Err(UpdateInstallError("update_launch_argument_invalid"));
    }
    let root = update_root(app_data_dir)?;
    let intent = read_json::<InstallIntent>(&root.join(INSTALL_INTENT))?
        .ok_or(UpdateInstallError("update_install_intent_missing"))?;
    if intent.schema_version != 1
        || intent.transaction_id != transaction_id
        || intent.version != version
        || intent.phase != "installed_pending_health"
    {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }
    write_json(
        &root.join(INSTALL_HEALTH),
        &InstallHealth {
            schema_version: 1,
            transaction_id: transaction_id.to_owned(),
            version: version.to_owned(),
            database_schema_version,
            healthy_at_ms: now_ms(),
        },
    )
}

fn run_health_probe(transaction_id: &str) -> InstallResult<()> {
    if !valid_id(transaction_id) {
        return Err(UpdateInstallError("update_launch_argument_invalid"));
    }
    let (app_data_dir, root) = guardian_paths()?;
    let intent = read_json::<InstallIntent>(&root.join(INSTALL_INTENT))?
        .ok_or(UpdateInstallError("update_install_intent_missing"))?;
    validate_install_intent(&intent)?;
    if intent.transaction_id != transaction_id
        || intent.phase != "installed_pending_health"
        || intent.version != env!("CARGO_PKG_VERSION")
    {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }
    verify_release_app(
        Path::new(CURRENT_APP),
        &intent.version,
        Some(&intent.team_identifier),
    )?;
    let store = Store::open(&app_data_dir.join(DATABASE_NAME))
        .map_err(|_| UpdateInstallError("update_database_health_failed"))?;
    let schema_version = store
        .verify_update_health()
        .map_err(|_| UpdateInstallError("update_database_health_failed"))?;
    record_startup_health(
        &app_data_dir,
        transaction_id,
        env!("CARGO_PKG_VERSION"),
        schema_version,
    )
}

pub fn run_health_probe_if_requested() -> Option<i32> {
    let arguments = std::env::args().collect::<Vec<_>>();
    let index = arguments
        .iter()
        .position(|value| value == HEALTH_PROBE_ARGUMENT)?;
    if index != 1 || arguments.len() != 3 {
        return Some(2);
    }
    Some(if run_health_probe(&arguments[2]).is_ok() {
        0
    } else {
        1
    })
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn wait_for_parent_exit(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    while process_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    !process_alive(pid)
}

fn guardian_paths() -> InstallResult<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(UpdateInstallError("update_guardian_home_unavailable"))?;
    let metadata = fs::symlink_metadata(&home)
        .map_err(|_| UpdateInstallError("update_guardian_home_unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(UpdateInstallError("update_guardian_home_unavailable"));
    }
    let app_data = home
        .join("Library")
        .join("Application Support")
        .join("com.wakegpt.desktop");
    let root = update_root(&app_data)?;
    Ok((app_data, root))
}

pub(crate) fn recover_interrupted_before_start(
    identifier: &str,
    current_version: &str,
) -> InstallResult<bool> {
    if identifier != "com.wakegpt.desktop" || !valid_version(current_version) {
        return Err(UpdateInstallError("update_app_identity_invalid"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(UpdateInstallError("update_guardian_home_unavailable"))?;
    let app_data = home
        .join("Library")
        .join("Application Support")
        .join(identifier);
    match fs::symlink_metadata(&app_data) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(UpdateInstallError("update_storage_unavailable")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(UpdateInstallError("update_storage_unsafe"));
        }
        Ok(_) => {}
    }
    let root_path = app_data.join(UPDATE_DIRECTORY);
    match fs::symlink_metadata(&root_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(UpdateInstallError("update_storage_unavailable")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(UpdateInstallError("update_storage_unsafe"));
        }
        Ok(_) => {}
    }
    let root = update_root(&app_data)?;
    let Some(mut intent) = read_json::<InstallIntent>(&root.join(INSTALL_INTENT))? else {
        return Ok(false);
    };
    validate_install_intent(&intent)?;

    let current = current_release_app(current_version)?;
    if current.team_identifier != intent.team_identifier {
        return Err(UpdateInstallError("update_app_team_mismatch"));
    }
    if let Some(result) = load_install_result(&app_data)? {
        if result.outcome == "installed" && current_version == intent.version {
            cleanup_transaction(&root, &intent.transaction_id, true)?;
            return Ok(false);
        }
        if result.outcome == "rolledBack"
            && current_version == intent.from_version
            && current.executable_sha256 == intent.old_executable_sha256
        {
            cleanup_transaction(&root, &intent.transaction_id, false)?;
            return Ok(false);
        }
    }

    if current_version == intent.from_version {
        if current.executable_sha256 != intent.old_executable_sha256 {
            return Err(UpdateInstallError("update_rollback_verification_failed"));
        }
        if matches!(intent.phase.as_str(), "prepared" | "installing") {
            let _ = write_result(
                &root,
                &intent,
                "rolledBack",
                Some("update_install_interrupted"),
            );
            cleanup_transaction(&root, &intent.transaction_id, true)?;
            return Ok(false);
        }
        return if finish_guardian_rollback(&root, &intent, "update_install_interrupted", false) {
            Ok(false)
        } else {
            Err(UpdateInstallError("update_rollback_failed"))
        };
    }

    if current_version == intent.version {
        if matches!(
            intent.phase.as_str(),
            "rollback_app_restored" | "rollback_database_restored"
        ) {
            return Err(UpdateInstallError("update_install_intent_invalid"));
        }
        if matches!(intent.phase.as_str(), "prepared" | "installing") {
            intent.phase = "installed_pending_health".to_owned();
            write_json(&root.join(INSTALL_INTENT), &intent)?;
        }
        spawn_guardian(&intent, std::process::id())?;
        return Ok(true);
    }

    Err(UpdateInstallError("update_app_version_mismatch"))
}

fn finish_guardian_success(root: &Path, intent: &InstallIntent) -> InstallResult<()> {
    verify_release_app(
        Path::new(CURRENT_APP),
        &intent.version,
        Some(&intent.team_identifier),
    )?;
    write_result(root, intent, "installed", None)?;
    cleanup_transaction(root, &intent.transaction_id, true)?;
    Command::new("/usr/bin/open")
        .arg(CURRENT_APP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| UpdateInstallError("update_new_app_start_failed"))
}

fn finish_guardian_rollback(
    root: &Path,
    intent: &InstallIntent,
    code: &'static str,
    reopen: bool,
) -> bool {
    let mut progress = intent.clone();
    let restored = (|| {
        if !matches!(
            progress.phase.as_str(),
            "rollback_app_restored" | "rollback_database_restored"
        ) {
            restore_app_from_backup(&progress)?;
            progress.phase = "rollback_app_restored".to_owned();
            write_json(&root.join(INSTALL_INTENT), &progress)?;
        }
        if progress.phase != "rollback_database_restored" {
            restore_database(root, &progress)?;
            progress.phase = "rollback_database_restored".to_owned();
            write_json(&root.join(INSTALL_INTENT), &progress)?;
        }
        write_result(root, &progress, "rolledBack", Some(code))?;
        cleanup_transaction(root, &progress.transaction_id, false)
    })()
    .is_ok();
    if !restored {
        let _ = write_result(
            root,
            &progress,
            "rollbackFailed",
            Some("update_rollback_failed"),
        );
    }
    if restored && reopen {
        let _ = Command::new("/usr/bin/open")
            .arg(CURRENT_APP)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    restored
}

fn rollback_reason(intent: &InstallIntent, fallback: &'static str) -> &'static str {
    match intent.rollback_reason.as_deref() {
        Some("update_install_failed") => "update_install_failed",
        Some("update_installed_app_invalid") => "update_installed_app_invalid",
        Some("update_guardian_start_failed") => "update_guardian_start_failed",
        Some("update_new_app_start_failed") => "update_new_app_start_failed",
        Some("update_new_app_unhealthy") => "update_new_app_unhealthy",
        Some("update_install_interrupted") => "update_install_interrupted",
        _ => fallback,
    }
}

fn run_guardian(transaction_id: &str, parent_pid: u32) -> InstallResult<()> {
    if !valid_id(transaction_id) || parent_pid == 0 {
        return Err(UpdateInstallError("update_guardian_argument_invalid"));
    }
    let (_, root) = guardian_paths()?;
    let initial_intent = read_json::<InstallIntent>(&root.join(INSTALL_INTENT))?
        .ok_or(UpdateInstallError("update_install_intent_missing"))?;
    validate_install_intent(&initial_intent)?;
    if initial_intent.transaction_id != transaction_id || initial_intent.phase == "prepared" {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }
    if !wait_for_parent_exit(parent_pid) {
        return Err(UpdateInstallError("update_parent_exit_timeout"));
    }
    let mut intent = read_json::<InstallIntent>(&root.join(INSTALL_INTENT))?
        .ok_or(UpdateInstallError("update_install_intent_missing"))?;
    validate_install_intent(&intent)?;
    if intent.transaction_id != transaction_id {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }
    if matches!(
        intent.phase.as_str(),
        "rollback_requested" | "rollback_app_restored" | "rollback_database_restored"
    ) {
        let code = rollback_reason(&intent, "update_install_failed");
        return if finish_guardian_rollback(&root, &intent, code, true) {
            Ok(())
        } else {
            Err(UpdateInstallError("update_rollback_failed"))
        };
    }

    if intent.phase == "installing" {
        if verify_release_app(
            Path::new(CURRENT_APP),
            &intent.version,
            Some(&intent.team_identifier),
        )
        .is_err()
        {
            intent.phase = "rollback_requested".to_owned();
            intent.rollback_reason = Some("update_installed_app_invalid".to_owned());
            let _ = write_json(&root.join(INSTALL_INTENT), &intent);
            return if finish_guardian_rollback(&root, &intent, "update_installed_app_invalid", true)
            {
                Ok(())
            } else {
                Err(UpdateInstallError("update_rollback_failed"))
            };
        }
        intent.phase = "installed_pending_health".to_owned();
        intent.rollback_reason = None;
        write_json(&root.join(INSTALL_INTENT), &intent)?;
    }
    if intent.phase != "installed_pending_health" {
        return Err(UpdateInstallError("update_install_intent_invalid"));
    }

    let mut child = Command::new(Path::new(CURRENT_APP).join(EXECUTABLE_RELATIVE))
        .arg(HEALTH_PROBE_ARGUMENT)
        .arg(transaction_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(ref mut child) = child else {
        return if finish_guardian_rollback(&root, &intent, "update_new_app_start_failed", true) {
            Ok(())
        } else {
            Err(UpdateInstallError("update_rollback_failed"))
        };
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut healthy = false;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    healthy = read_json::<InstallHealth>(&root.join(INSTALL_HEALTH))
                        .ok()
                        .flatten()
                        .is_some_and(|health| {
                            health.schema_version == 1
                                && health.transaction_id == transaction_id
                                && health.version == intent.version
                                && health.database_schema_version == crate::storage::SCHEMA_VERSION
                                && health.healthy_at_ms >= intent.started_at_ms
                        });
                }
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    if healthy {
        return finish_guardian_success(&root, &intent);
    }

    let _ = child.kill();
    let _ = child.wait();
    if finish_guardian_rollback(&root, &intent, "update_new_app_unhealthy", true) {
        Ok(())
    } else {
        Err(UpdateInstallError("update_rollback_failed"))
    }
}

pub fn run_guardian_if_requested() -> Option<i32> {
    let arguments = std::env::args().collect::<Vec<_>>();
    let index = arguments
        .iter()
        .position(|value| value == GUARDIAN_ARGUMENT)?;
    if index != 1 || arguments.len() != 4 {
        return Some(2);
    }
    let transaction_id = &arguments[2];
    let parent_pid = match arguments[3].parse::<u32>() {
        Ok(value) => value,
        Err(_) => return Some(2),
    };
    Some(if run_guardian(transaction_id, parent_pid).is_ok() {
        0
    } else {
        1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let serial = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "wakegpt-update-install-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn receipt(transaction_id: String) -> PreparedUpdateReceipt {
        PreparedUpdateReceipt {
            schema_version: 1,
            transaction_id,
            from_version: "0.1.0".to_owned(),
            version: "0.2.0".to_owned(),
            download_url: "https://github.com/wakegpt/wakegpt/releases/download/v0.2.0/WakeGPT_universal.app.tar.gz".to_owned(),
            signature: "A".repeat(64),
            sha256: String::new(),
            byte_size: 0,
            downloaded_at_ms: 1,
        }
    }

    fn intent(transaction_id: String) -> InstallIntent {
        InstallIntent {
            schema_version: 1,
            transaction_id,
            phase: "installed_pending_health".to_owned(),
            from_version: "0.1.0".to_owned(),
            version: "0.2.0".to_owned(),
            old_executable_sha256: "a".repeat(64),
            new_bundle_sha256: "b".repeat(64),
            database_backup_sha256: "c".repeat(64),
            database_backup_bytes: 1,
            team_identifier: "TEAM123456".to_owned(),
            rollback_reason: None,
            started_at_ms: 1,
        }
    }

    #[test]
    fn prepared_bundle_is_durable_and_rejects_tampering() {
        let root = TestRoot::new();
        let app_data = root.0.join("data");
        fs::create_dir(&app_data).unwrap();
        let transaction_id = Uuid::now_v7().to_string();
        let (mut stage, stage_path) = create_download_stage(&app_data, &transaction_id).unwrap();
        stage.write_all(b"signed update fixture").unwrap();
        sync_file(&stage).unwrap();
        drop(stage);
        let (sha256, byte_size) = sha256_file(&stage_path, MAX_UPDATE_BYTES).unwrap();
        let mut receipt = receipt(transaction_id);
        receipt.sha256 = sha256;
        receipt.byte_size = byte_size;
        commit_download(&app_data, &stage_path, &receipt).unwrap();
        assert_eq!(load_prepared(&app_data).unwrap(), Some(receipt.clone()));

        let bundle = prepared_bundle_path(&app_data).unwrap();
        OpenOptions::new()
            .append(true)
            .open(bundle)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        assert_eq!(
            load_prepared(&app_data).unwrap_err().0,
            "update_prepared_changed"
        );
    }

    #[test]
    fn prepared_state_rejects_symlink_and_install_intent_blocks_discard() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new();
        let app_data = root.0.join("data");
        fs::create_dir(&app_data).unwrap();
        let update = update_root(&app_data).unwrap();
        let outside = root.0.join("outside");
        fs::write(&outside, "fixture").unwrap();
        symlink(&outside, update.join(PREPARED_BUNDLE)).unwrap();
        let mut value = receipt(Uuid::now_v7().to_string());
        value.sha256 = "a".repeat(64);
        value.byte_size = 7;
        write_json(&update.join(PREPARED_RECEIPT), &value).unwrap();
        assert!(load_prepared(&app_data).is_err());
        fs::remove_file(update.join(PREPARED_BUNDLE)).unwrap();
        fs::write(update.join(INSTALL_INTENT), "{}").unwrap();
        assert_eq!(
            discard_prepared(&app_data).unwrap_err().0,
            "update_install_in_progress"
        );
    }

    #[test]
    fn cleanup_keeps_the_intent_until_every_other_artifact_is_safe() {
        let root = TestRoot::new();
        let transaction_id = Uuid::now_v7().to_string();
        write_json(
            &root.0.join(INSTALL_INTENT),
            &intent(transaction_id.clone()),
        )
        .unwrap();
        fs::create_dir(root.0.join(PREPARED_BUNDLE)).unwrap();

        assert!(cleanup_transaction(&root.0, &transaction_id, false).is_err());
        assert!(root.0.join(INSTALL_INTENT).is_file());
    }

    #[test]
    fn worker_failure_returns_only_when_no_install_intent_exists() {
        let root = TestRoot::new();
        let app_data = root.0.join("data");
        fs::create_dir(&app_data).unwrap();
        let update = update_root(&app_data).unwrap();
        let transaction_id = Uuid::now_v7().to_string();
        fs::write(update.join(DATABASE_BACKUP), b"stale backup").unwrap();
        assert!(!recover_after_install_worker_failure(&app_data, &transaction_id).unwrap());
        assert!(!update.join(DATABASE_BACKUP).exists());

        write_json(
            &update.join(INSTALL_INTENT),
            &intent(transaction_id.clone()),
        )
        .unwrap();
        assert!(recover_after_install_worker_failure(&app_data, &transaction_id).unwrap());
        assert!(update.join(INSTALL_INTENT).is_file());
        assert!(
            recover_after_install_worker_failure(&app_data, &Uuid::now_v7().to_string()).is_err()
        );
    }

    #[test]
    fn database_restore_refuses_live_sidecars_before_touching_the_database() {
        let root = TestRoot::new();
        let app_data = root.0.join("data");
        fs::create_dir(&app_data).unwrap();
        let update = update_root(&app_data).unwrap();
        let backup = update.join(DATABASE_BACKUP);
        let database = app_data.join(DATABASE_NAME);
        let wal = app_data.join(format!("{DATABASE_NAME}-wal"));
        fs::write(&backup, b"backup database").unwrap();
        fs::write(&database, b"live database").unwrap();
        fs::write(&wal, b"live wal").unwrap();
        let (backup_sha256, backup_bytes) = sha256_file(&backup, MAX_UPDATE_BYTES).unwrap();
        let mut install_intent = intent(Uuid::now_v7().to_string());
        install_intent.database_backup_sha256 = backup_sha256;
        install_intent.database_backup_bytes = backup_bytes;

        assert_eq!(
            restore_database(&update, &install_intent).unwrap_err().0,
            "update_database_sidecar_present"
        );
        assert_eq!(fs::read(&database).unwrap(), b"live database");
        assert_eq!(fs::read(&wal).unwrap(), b"live wal");
    }

    #[test]
    fn startup_health_requires_the_exact_installed_intent() {
        let root = TestRoot::new();
        let app_data = root.0.join("data");
        fs::create_dir(&app_data).unwrap();
        let update = update_root(&app_data).unwrap();
        let transaction_id = Uuid::now_v7().to_string();
        write_json(
            &update.join(INSTALL_INTENT),
            &intent(transaction_id.clone()),
        )
        .unwrap();
        record_startup_health(&app_data, &transaction_id, "0.2.0", 19).unwrap();
        let health = read_json::<InstallHealth>(&update.join(INSTALL_HEALTH))
            .unwrap()
            .unwrap();
        assert_eq!(health.transaction_id, transaction_id);
        assert_eq!(health.database_schema_version, 19);
        assert!(
            record_startup_health(&app_data, &Uuid::now_v7().to_string(), "0.2.0", 19).is_err()
        );
    }
}
