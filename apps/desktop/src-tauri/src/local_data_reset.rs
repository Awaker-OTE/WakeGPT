use crate::domain::new_id;
use crate::platform_trash;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INTENT_SCHEMA_VERSION: u32 = 1;
const NOTICE_SCHEMA_VERSION: u32 = 1;
const NOTICE_FILE_NAME: &str = "local-data-reset-notice-v1.json";
const PACKAGE_MANIFEST_FILE_NAME: &str = "wakegpt-local-data-reset-manifest-v1.json";
const MAX_CONTROL_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalDataResetError(&'static str);

impl LocalDataResetError {
    pub(crate) const fn code(self) -> &'static str {
        self.0
    }

    pub(crate) fn message(self) -> &'static str {
        match self.0 {
            "local_data_reset_platform_unsupported" => "当前平台尚未提供经过验证的本机数据清除流程",
            "local_data_reset_identifier_invalid" => "应用身份无法安全验证；本机数据没有改变",
            "local_data_reset_home_unavailable" => "无法验证当前用户目录；本机数据没有改变",
            "local_data_reset_path_invalid" => "检测到不安全的数据路径；本机数据没有改变",
            "local_data_reset_intent_invalid" => "清除请求已损坏；WakeGPT 没有继续删除数据",
            "local_data_reset_target_collision" => "清除暂存位置发生冲突；WakeGPT 没有覆盖任何文件",
            "local_data_reset_stage_failed" => "无法安全暂存全部本机数据；原数据已保留",
            "local_data_reset_rollback_failed" => {
                "无法完整回滚本机数据清除；请不要再次启动 WakeGPT"
            }
            "local_data_reset_trash_failed" => "无法把本机数据移入系统废纸篓；原数据已恢复",
            "local_data_reset_notice_invalid" => "本机数据清除结果回执无法验证",
            _ => "本机数据清除未完成；现有数据保持不变",
        }
    }

    pub(crate) fn retryable(self) -> bool {
        matches!(
            self.0,
            "local_data_reset_stage_failed" | "local_data_reset_trash_failed"
        )
    }
}

impl fmt::Display for LocalDataResetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for LocalDataResetError {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum ResetIntentState {
    Collecting,
    Staged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum ResetTargetKind {
    Cache,
    Logs,
    WebKit,
    Preferences,
    HttpStorage,
    SavedState,
    Cookies,
    AppData,
}

impl ResetTargetKind {
    const ALL: [Self; 8] = [
        Self::AppData,
        Self::Cache,
        Self::Logs,
        Self::WebKit,
        Self::Preferences,
        Self::HttpStorage,
        Self::SavedState,
        Self::Cookies,
    ];

    const fn staged_name(self) -> &'static str {
        match self {
            Self::Cache => "01-cache",
            Self::Logs => "02-logs",
            Self::WebKit => "03-webkit",
            Self::Preferences => "04-preferences.plist",
            Self::HttpStorage => "05-http-storage",
            Self::SavedState => "06-saved-state",
            Self::Cookies => "07-cookies.binarycookies",
            Self::AppData => "08-app-data",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetIntent {
    schema_version: u32,
    reset_id: String,
    state: ResetIntentState,
    targets: Vec<ResetTargetKind>,
    moved_target_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetPackageManifest {
    schema_version: u32,
    purpose: String,
    reset_id: String,
    created_at_ms: u64,
    bundle_identifier: String,
    restore_requires_wakegpt_to_be_closed: bool,
    targets: Vec<ResetPackageTarget>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetPackageTarget {
    staged_name: String,
    original_location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalDataResetNotice {
    schema_version: u32,
    pub outcome: LocalDataResetOutcome,
    pub occurred_at_ms: u64,
    pub moved_item_count: u32,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LocalDataResetOutcome {
    Completed,
    RolledBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDataResetPlatformPreview {
    pub platform_supported: bool,
    pub app_owned_item_count: u32,
    pub reset_scheduled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDataResetSchedule {
    pub already_scheduled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupResetOutcome {
    Completed { moved_item_count: u32 },
    RolledBack { error_code: &'static str },
}

#[derive(Debug, Clone)]
struct ResetLayout {
    home: PathBuf,
    identifier: String,
    intent_path: PathBuf,
}

impl ResetLayout {
    fn for_home(home: &Path, identifier: &str) -> Result<Self, LocalDataResetError> {
        validate_identifier(identifier)?;
        let home = canonical_private_directory(home, "local_data_reset_home_unavailable")?;
        let application_support = home.join("Library").join("Application Support");
        validate_ancestor_directories(&home, &application_support)?;
        validate_existing_directory(&application_support)?;
        Ok(Self {
            home,
            identifier: identifier.to_owned(),
            intent_path: application_support
                .join(format!(".{identifier}.local-data-reset-v1.json")),
        })
    }

    fn app_data_dir(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("Application Support")
            .join(&self.identifier)
    }

    fn target_path(&self, kind: ResetTargetKind) -> PathBuf {
        let library = self.home.join("Library");
        match kind {
            ResetTargetKind::Cache => library.join("Caches").join(&self.identifier),
            ResetTargetKind::Logs => library.join("Logs").join(&self.identifier),
            ResetTargetKind::WebKit => library.join("WebKit").join(&self.identifier),
            ResetTargetKind::Preferences => library
                .join("Preferences")
                .join(format!("{}.plist", self.identifier)),
            ResetTargetKind::HttpStorage => library.join("HTTPStorages").join(&self.identifier),
            ResetTargetKind::SavedState => library
                .join("Saved Application State")
                .join(format!("{}.savedState", self.identifier)),
            ResetTargetKind::Cookies => library
                .join("Cookies")
                .join(format!("{}.binarycookies", self.identifier)),
            ResetTargetKind::AppData => self.app_data_dir(),
        }
    }

    fn staging_root(&self, reset_id: &str) -> PathBuf {
        self.intent_path
            .parent()
            .expect("reset intent has a parent")
            .join(format!("WakeGPT Local Data Reset {reset_id}"))
    }

    fn notice_path(&self) -> PathBuf {
        self.app_data_dir().join(NOTICE_FILE_NAME)
    }
}

pub fn platform_preview(
    identifier: &str,
) -> Result<LocalDataResetPlatformPreview, LocalDataResetError> {
    #[cfg(target_os = "macos")]
    {
        let layout = ResetLayout::for_home(&platform_home()?, identifier)?;
        let mut count = 0_u32;
        for kind in ResetTargetKind::ALL {
            validate_ancestor_directories(&layout.home, &layout.target_path(kind))?;
            match fs::symlink_metadata(layout.target_path(kind)) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(LocalDataResetError("local_data_reset_path_invalid"));
                }
                Ok(_) => count = count.saturating_add(1),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(LocalDataResetError("local_data_reset_path_invalid")),
            }
        }
        Ok(LocalDataResetPlatformPreview {
            platform_supported: true,
            app_owned_item_count: count,
            reset_scheduled: layout.intent_path.exists(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        Ok(LocalDataResetPlatformPreview {
            platform_supported: false,
            app_owned_item_count: 0,
            reset_scheduled: false,
        })
    }
}

pub fn schedule(identifier: &str) -> Result<LocalDataResetSchedule, LocalDataResetError> {
    #[cfg(target_os = "macos")]
    {
        schedule_for_layout(&ResetLayout::for_home(&platform_home()?, identifier)?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        Err(LocalDataResetError("local_data_reset_platform_unsupported"))
    }
}

pub fn apply_pending(identifier: &str) -> Result<Option<StartupResetOutcome>, LocalDataResetError> {
    #[cfg(target_os = "macos")]
    {
        let layout = ResetLayout::for_home(&platform_home()?, identifier)?;
        apply_pending_for_layout(&layout, |path| {
            platform_trash::move_directory_to_system_trash(path)
                .map(|_| ())
                .map_err(|_| LocalDataResetError("local_data_reset_trash_failed"))
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        Ok(None)
    }
}

pub fn collecting_app_data_dir(identifier: &str) -> Result<Option<PathBuf>, LocalDataResetError> {
    #[cfg(target_os = "macos")]
    {
        let layout = ResetLayout::for_home(&platform_home()?, identifier)?;
        collecting_app_data_for_layout(&layout)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = identifier;
        Ok(None)
    }
}

fn collecting_app_data_for_layout(
    layout: &ResetLayout,
) -> Result<Option<PathBuf>, LocalDataResetError> {
    let Some(intent) = read_json_if_present::<ResetIntent>(
        &layout.intent_path,
        "local_data_reset_intent_invalid",
    )?
    else {
        return Ok(None);
    };
    validate_intent(&intent)?;
    if intent.state != ResetIntentState::Collecting
        || layout.staging_root(&intent.reset_id).exists()
    {
        return Ok(None);
    }
    let app_data_dir = layout.app_data_dir();
    match fs::symlink_metadata(&app_data_dir) {
        Ok(_) => Ok(Some(app_data_dir)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(LocalDataResetError("local_data_reset_path_invalid")),
    }
}

pub fn cancel_collecting(
    identifier: &str,
    error_code: &'static str,
) -> Result<bool, LocalDataResetError> {
    #[cfg(target_os = "macos")]
    {
        let layout = ResetLayout::for_home(&platform_home()?, identifier)?;
        cancel_collecting_for_layout(&layout, error_code)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (identifier, error_code);
        Ok(false)
    }
}

fn cancel_collecting_for_layout(
    layout: &ResetLayout,
    error_code: &'static str,
) -> Result<bool, LocalDataResetError> {
    let Some(intent) = read_json_if_present::<ResetIntent>(
        &layout.intent_path,
        "local_data_reset_intent_invalid",
    )?
    else {
        return Ok(false);
    };
    validate_intent(&intent)?;
    if intent.state != ResetIntentState::Collecting {
        return Err(LocalDataResetError("local_data_reset_rollback_failed"));
    }
    write_notice(
        layout,
        LocalDataResetOutcome::RolledBack,
        0,
        Some(error_code),
    )?;
    remove_intent(layout)?;
    Ok(true)
}

pub fn read_notice(
    app_data_dir: &Path,
) -> Result<Option<LocalDataResetNotice>, LocalDataResetError> {
    let notice_path = app_data_dir.join(NOTICE_FILE_NAME);
    let notice = read_json_if_present::<LocalDataResetNotice>(
        &notice_path,
        "local_data_reset_notice_invalid",
    )?;
    if notice
        .as_ref()
        .is_some_and(|value| value.schema_version != NOTICE_SCHEMA_VERSION)
    {
        return Err(LocalDataResetError("local_data_reset_notice_invalid"));
    }
    Ok(notice)
}

pub fn acknowledge_notice(
    app_data_dir: &Path,
    occurred_at_ms: u64,
) -> Result<bool, LocalDataResetError> {
    let notice_path = app_data_dir.join(NOTICE_FILE_NAME);
    let Some(notice) = read_notice(app_data_dir)? else {
        return Ok(false);
    };
    if notice.occurred_at_ms != occurred_at_ms {
        return Ok(false);
    }
    fs::remove_file(&notice_path)
        .map_err(|_| LocalDataResetError("local_data_reset_notice_invalid"))?;
    sync_directory(app_data_dir)
        .map_err(|_| LocalDataResetError("local_data_reset_notice_invalid"))?;
    Ok(true)
}

fn schedule_for_layout(
    layout: &ResetLayout,
) -> Result<LocalDataResetSchedule, LocalDataResetError> {
    if layout.intent_path.exists() {
        let intent = read_intent(layout)?;
        validate_intent(&intent)?;
        return Ok(LocalDataResetSchedule {
            already_scheduled: true,
        });
    }
    let intent = ResetIntent {
        schema_version: INTENT_SCHEMA_VERSION,
        reset_id: new_id(),
        state: ResetIntentState::Collecting,
        targets: ResetTargetKind::ALL.to_vec(),
        moved_target_count: 0,
    };
    write_json_atomic(&layout.intent_path, &intent, false)?;
    Ok(LocalDataResetSchedule {
        already_scheduled: false,
    })
}

fn apply_pending_for_layout<F>(
    layout: &ResetLayout,
    mut move_to_trash: F,
) -> Result<Option<StartupResetOutcome>, LocalDataResetError>
where
    F: FnMut(&Path) -> Result<(), LocalDataResetError>,
{
    if !layout.intent_path.exists() {
        return Ok(None);
    }
    let mut intent = read_intent(layout)?;
    validate_intent(&intent)?;
    let staging_root = layout.staging_root(&intent.reset_id);

    if intent.state == ResetIntentState::Collecting {
        if let Err(error) = stage_targets(layout, &intent, &staging_root) {
            rollback_targets(layout, &intent, &staging_root)?;
            write_notice(
                layout,
                LocalDataResetOutcome::RolledBack,
                0,
                Some(error.code()),
            )?;
            remove_intent(layout)?;
            return Ok(Some(StartupResetOutcome::RolledBack {
                error_code: error.code(),
            }));
        }
        if layout.app_data_dir().exists() {
            rollback_targets(layout, &intent, &staging_root)?;
            return Err(LocalDataResetError("local_data_reset_target_collision"));
        }
        if let Err(error) = write_package_manifest(layout, &intent, &staging_root) {
            rollback_targets(layout, &intent, &staging_root)?;
            write_notice(
                layout,
                LocalDataResetOutcome::RolledBack,
                0,
                Some(error.code()),
            )?;
            remove_intent(layout)?;
            return Ok(Some(StartupResetOutcome::RolledBack {
                error_code: error.code(),
            }));
        }
        intent.moved_target_count = count_staged_targets(&intent, &staging_root)?;
        intent.state = ResetIntentState::Staged;
        write_json_atomic(&layout.intent_path, &intent, true)?;
    }

    if staging_root.exists() {
        if let Err(error) = validate_package_manifest(layout, &intent, &staging_root) {
            rollback_targets(layout, &intent, &staging_root)?;
            write_notice(
                layout,
                LocalDataResetOutcome::RolledBack,
                0,
                Some(error.code()),
            )?;
            remove_intent(layout)?;
            return Ok(Some(StartupResetOutcome::RolledBack {
                error_code: error.code(),
            }));
        }
        if let Err(error) = move_to_trash(&staging_root) {
            rollback_targets(layout, &intent, &staging_root)?;
            write_notice(
                layout,
                LocalDataResetOutcome::RolledBack,
                0,
                Some(error.code()),
            )?;
            remove_intent(layout)?;
            return Ok(Some(StartupResetOutcome::RolledBack {
                error_code: error.code(),
            }));
        }
    }

    ensure_private_directory(&layout.app_data_dir())?;
    write_notice(
        layout,
        LocalDataResetOutcome::Completed,
        intent.moved_target_count,
        None,
    )?;
    remove_intent(layout)?;
    Ok(Some(StartupResetOutcome::Completed {
        moved_item_count: intent.moved_target_count,
    }))
}

fn stage_targets(
    layout: &ResetLayout,
    intent: &ResetIntent,
    staging_root: &Path,
) -> Result<(), LocalDataResetError> {
    ensure_private_directory(staging_root)?;
    for kind in &intent.targets {
        let source = layout.target_path(*kind);
        let staged = staging_root.join(kind.staged_name());
        validate_ancestor_directories(&layout.home, &source)?;
        let source_metadata = fs::symlink_metadata(&source);
        let staged_metadata = fs::symlink_metadata(&staged);
        match (source_metadata, staged_metadata) {
            (Ok(_), Ok(_)) => {
                return Err(LocalDataResetError("local_data_reset_target_collision"));
            }
            (Ok(metadata), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir())
                {
                    return Err(LocalDataResetError("local_data_reset_path_invalid"));
                }
                fs::rename(&source, &staged)
                    .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
                sync_parent(&source)?;
                sync_directory(staging_root)?;
            }
            (Err(error), Ok(metadata)) if error.kind() == std::io::ErrorKind::NotFound => {
                if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir())
                {
                    return Err(LocalDataResetError("local_data_reset_path_invalid"));
                }
            }
            (Err(source_error), Err(staged_error))
                if source_error.kind() == std::io::ErrorKind::NotFound
                    && staged_error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(LocalDataResetError("local_data_reset_path_invalid")),
        }
    }
    Ok(())
}

fn rollback_targets(
    layout: &ResetLayout,
    intent: &ResetIntent,
    staging_root: &Path,
) -> Result<(), LocalDataResetError> {
    for kind in intent.targets.iter().rev() {
        let source = layout.target_path(*kind);
        let staged = staging_root.join(kind.staged_name());
        if !staged.exists() {
            continue;
        }
        if source.exists() {
            return Err(LocalDataResetError("local_data_reset_rollback_failed"));
        }
        let parent = source
            .parent()
            .ok_or(LocalDataResetError("local_data_reset_rollback_failed"))?;
        validate_ancestor_directories(&layout.home, &source)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        validate_existing_directory(parent)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        fs::rename(&staged, &source)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        sync_parent(&source)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        sync_directory(staging_root)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
    }
    if staging_root.exists() {
        let manifest = staging_root.join(PACKAGE_MANIFEST_FILE_NAME);
        if manifest.exists() {
            fs::remove_file(&manifest)
                .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
            sync_directory(staging_root)
                .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        }
        fs::remove_dir(staging_root)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
        sync_parent(staging_root)
            .map_err(|_| LocalDataResetError("local_data_reset_rollback_failed"))?;
    }
    Ok(())
}

fn write_package_manifest(
    layout: &ResetLayout,
    intent: &ResetIntent,
    staging_root: &Path,
) -> Result<(), LocalDataResetError> {
    let targets = intent
        .targets
        .iter()
        .filter(|kind| staging_root.join(kind.staged_name()).exists())
        .map(|kind| ResetPackageTarget {
            staged_name: kind.staged_name().to_owned(),
            original_location: original_location(&layout.identifier, *kind),
        })
        .collect();
    let manifest = ResetPackageManifest {
        schema_version: 1,
        purpose: "wakegpt-local-data-reset-recovery-package".to_owned(),
        reset_id: intent.reset_id.clone(),
        created_at_ms: current_time_ms(),
        bundle_identifier: layout.identifier.clone(),
        restore_requires_wakegpt_to_be_closed: true,
        targets,
    };
    write_json_atomic(
        &staging_root.join(PACKAGE_MANIFEST_FILE_NAME),
        &manifest,
        true,
    )
}

fn validate_package_manifest(
    layout: &ResetLayout,
    intent: &ResetIntent,
    staging_root: &Path,
) -> Result<(), LocalDataResetError> {
    for kind in &intent.targets {
        match fs::symlink_metadata(staging_root.join(kind.staged_name())) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || (!metadata.is_file() && !metadata.is_dir()) =>
            {
                return Err(LocalDataResetError("local_data_reset_path_invalid"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(LocalDataResetError("local_data_reset_path_invalid")),
        }
    }
    let manifest_path = staging_root.join(PACKAGE_MANIFEST_FILE_NAME);
    let manifest = read_json_if_present::<ResetPackageManifest>(
        &manifest_path,
        "local_data_reset_intent_invalid",
    )?
    .ok_or(LocalDataResetError("local_data_reset_intent_invalid"))?;
    let expected_targets = intent
        .targets
        .iter()
        .filter(|kind| staging_root.join(kind.staged_name()).exists())
        .map(|kind| ResetPackageTarget {
            staged_name: kind.staged_name().to_owned(),
            original_location: original_location(&layout.identifier, *kind),
        })
        .collect::<Vec<_>>();
    if manifest.schema_version != 1
        || manifest.purpose != "wakegpt-local-data-reset-recovery-package"
        || manifest.reset_id != intent.reset_id
        || manifest.bundle_identifier != layout.identifier
        || !manifest.restore_requires_wakegpt_to_be_closed
        || manifest.targets.len() != expected_targets.len()
        || manifest
            .targets
            .iter()
            .zip(expected_targets.iter())
            .any(|(actual, expected)| {
                actual.staged_name != expected.staged_name
                    || actual.original_location != expected.original_location
            })
    {
        return Err(LocalDataResetError("local_data_reset_intent_invalid"));
    }
    Ok(())
}

fn original_location(identifier: &str, kind: ResetTargetKind) -> String {
    match kind {
        ResetTargetKind::Cache => format!("Library/Caches/{identifier}"),
        ResetTargetKind::Logs => format!("Library/Logs/{identifier}"),
        ResetTargetKind::WebKit => format!("Library/WebKit/{identifier}"),
        ResetTargetKind::Preferences => format!("Library/Preferences/{identifier}.plist"),
        ResetTargetKind::HttpStorage => format!("Library/HTTPStorages/{identifier}"),
        ResetTargetKind::SavedState => {
            format!("Library/Saved Application State/{identifier}.savedState")
        }
        ResetTargetKind::Cookies => format!("Library/Cookies/{identifier}.binarycookies"),
        ResetTargetKind::AppData => format!("Library/Application Support/{identifier}"),
    }
}

fn count_staged_targets(
    intent: &ResetIntent,
    staging_root: &Path,
) -> Result<u32, LocalDataResetError> {
    let mut count = 0_u32;
    for kind in &intent.targets {
        match fs::symlink_metadata(staging_root.join(kind.staged_name())) {
            Ok(metadata) if !metadata.file_type().is_symlink() => count = count.saturating_add(1),
            Ok(_) => return Err(LocalDataResetError("local_data_reset_path_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(LocalDataResetError("local_data_reset_path_invalid")),
        }
    }
    Ok(count)
}

fn validate_intent(intent: &ResetIntent) -> Result<(), LocalDataResetError> {
    if intent.schema_version != INTENT_SCHEMA_VERSION
        || intent.reset_id.is_empty()
        || intent.reset_id.len() > 128
        || !intent
            .reset_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || intent.targets != ResetTargetKind::ALL
        || intent.moved_target_count > intent.targets.len() as u32
        || (intent.state == ResetIntentState::Collecting && intent.moved_target_count != 0)
    {
        return Err(LocalDataResetError("local_data_reset_intent_invalid"));
    }
    Ok(())
}

fn read_intent(layout: &ResetLayout) -> Result<ResetIntent, LocalDataResetError> {
    read_json_if_present(&layout.intent_path, "local_data_reset_intent_invalid")?
        .ok_or(LocalDataResetError("local_data_reset_intent_invalid"))
}

fn write_notice(
    layout: &ResetLayout,
    outcome: LocalDataResetOutcome,
    moved_item_count: u32,
    error_code: Option<&str>,
) -> Result<(), LocalDataResetError> {
    ensure_private_directory(&layout.app_data_dir())?;
    let notice = LocalDataResetNotice {
        schema_version: NOTICE_SCHEMA_VERSION,
        outcome,
        occurred_at_ms: current_time_ms(),
        moved_item_count,
        error_code: error_code.map(str::to_owned),
    };
    write_json_atomic(&layout.notice_path(), &notice, true)
}

fn remove_intent(layout: &ResetLayout) -> Result<(), LocalDataResetError> {
    match fs::remove_file(&layout.intent_path) {
        Ok(()) => sync_parent(&layout.intent_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(LocalDataResetError("local_data_reset_intent_invalid")),
    }
}

fn read_json_if_present<T: for<'de> Deserialize<'de>>(
    path: &Path,
    error_code: &'static str,
) -> Result<Option<T>, LocalDataResetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(LocalDataResetError(error_code)),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_CONTROL_FILE_BYTES
    {
        return Err(LocalDataResetError(error_code));
    }
    let file = File::open(path).map_err(|_| LocalDataResetError(error_code))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONTROL_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LocalDataResetError(error_code))?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_CONTROL_FILE_BYTES {
        return Err(LocalDataResetError(error_code));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| LocalDataResetError(error_code))
}

fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
    replace: bool,
) -> Result<(), LocalDataResetError> {
    let parent = path
        .parent()
        .ok_or(LocalDataResetError("local_data_reset_path_invalid"))?;
    validate_existing_directory(parent)?;
    if !replace && path.exists() {
        return Err(LocalDataResetError("local_data_reset_target_collision"));
    }
    let bytes = serde_json::to_vec(value)
        .map_err(|_| LocalDataResetError("local_data_reset_intent_invalid"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_CONTROL_FILE_BYTES {
        return Err(LocalDataResetError("local_data_reset_intent_invalid"));
    }
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or(LocalDataResetError("local_data_reset_path_invalid"))?,
        new_id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
        file.sync_all()
            .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
        fs::rename(&temporary, path)
            .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn ensure_private_directory(path: &Path) -> Result<(), LocalDataResetError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LocalDataResetError("local_data_reset_path_invalid"));
        }
    } else {
        let parent = path
            .parent()
            .ok_or(LocalDataResetError("local_data_reset_path_invalid"))?;
        validate_existing_directory(parent)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(path)
            .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))?;
    }
    sync_parent(path)
}

fn validate_identifier(identifier: &str) -> Result<(), LocalDataResetError> {
    if identifier.is_empty()
        || identifier.len() > 128
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || identifier.starts_with('.')
        || identifier.ends_with('.')
    {
        return Err(LocalDataResetError("local_data_reset_identifier_invalid"));
    }
    Ok(())
}

fn canonical_private_directory(
    path: &Path,
    error_code: &'static str,
) -> Result<PathBuf, LocalDataResetError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LocalDataResetError(error_code))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalDataResetError(error_code));
    }
    fs::canonicalize(path).map_err(|_| LocalDataResetError(error_code))
}

fn validate_existing_directory(path: &Path) -> Result<(), LocalDataResetError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| LocalDataResetError("local_data_reset_path_invalid"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalDataResetError("local_data_reset_path_invalid"));
    }
    Ok(())
}

fn validate_ancestor_directories(home: &Path, target: &Path) -> Result<(), LocalDataResetError> {
    let parent = target
        .parent()
        .ok_or(LocalDataResetError("local_data_reset_path_invalid"))?;
    let relative = parent
        .strip_prefix(home)
        .map_err(|_| LocalDataResetError("local_data_reset_path_invalid"))?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(LocalDataResetError("local_data_reset_path_invalid"));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(LocalDataResetError("local_data_reset_path_invalid"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(LocalDataResetError("local_data_reset_path_invalid")),
        }
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), LocalDataResetError> {
    let parent = path
        .parent()
        .ok_or(LocalDataResetError("local_data_reset_path_invalid"))?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), LocalDataResetError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| LocalDataResetError("local_data_reset_stage_failed"))
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(target_os = "macos")]
fn platform_home() -> Result<PathBuf, LocalDataResetError> {
    use objc2_foundation::NSFileManager;

    let home = NSFileManager::defaultManager().homeDirectoryForCurrentUser();
    let path = home
        .path()
        .ok_or(LocalDataResetError("local_data_reset_home_unavailable"))?;
    canonical_private_directory(
        Path::new(&path.to_string()),
        "local_data_reset_home_unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wakegpt-local-reset-{label}-{}",
                crate::domain::new_id()
            ));
            fs::create_dir(&path).unwrap();
            for relative in [
                "Library/Application Support",
                "Library/Caches",
                "Library/Logs",
                "Library/WebKit",
                "Library/Preferences",
                "Library/HTTPStorages",
                "Library/Saved Application State",
                "Library/Cookies",
            ] {
                fs::create_dir_all(path.join(relative)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(layout: &ResetLayout) -> PathBuf {
        fs::create_dir(layout.app_data_dir()).unwrap();
        fs::write(layout.app_data_dir().join("wakegpt.sqlite3"), b"database").unwrap();
        let cache = layout.target_path(ResetTargetKind::Cache);
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("cache.bin"), b"cache").unwrap();
        let preferences = layout.target_path(ResetTargetKind::Preferences);
        fs::write(&preferences, b"preferences").unwrap();
        let workspace = layout.home.join("Workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("Notes.md"), b"user-owned").unwrap();
        workspace
    }

    #[test]
    fn scheduling_is_non_destructive_and_idempotent() {
        let root = TestRoot::new("schedule");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);

        assert!(!schedule_for_layout(&layout).unwrap().already_scheduled);
        assert!(schedule_for_layout(&layout).unwrap().already_scheduled);
        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
    }

    #[test]
    fn a_late_profile_blocker_cancels_before_staging() {
        let root = TestRoot::new("preflight-cancel");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);
        schedule_for_layout(&layout).unwrap();

        assert!(
            cancel_collecting_for_layout(&layout, "local_data_reset_default_identity_running")
                .unwrap()
        );

        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
        assert!(!layout.intent_path.exists());
        let notice = read_notice(&layout.app_data_dir()).unwrap().unwrap();
        assert_eq!(notice.outcome, LocalDataResetOutcome::RolledBack);
        assert_eq!(
            notice.error_code.as_deref(),
            Some("local_data_reset_default_identity_running")
        );
    }

    #[test]
    fn reset_moves_only_app_owned_roots_and_leaves_a_notice() {
        let root = TestRoot::new("complete");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);
        schedule_for_layout(&layout).unwrap();
        let trash = root.0.join("Synthetic Trash");

        let outcome = apply_pending_for_layout(&layout, |staging| {
            fs::rename(staging, &trash)
                .map_err(|_| LocalDataResetError("local_data_reset_trash_failed"))
        })
        .unwrap();

        assert!(matches!(
            outcome,
            Some(StartupResetOutcome::Completed {
                moved_item_count: 3
            })
        ));
        assert!(layout.app_data_dir().is_dir());
        assert!(!layout.app_data_dir().join("wakegpt.sqlite3").exists());
        assert!(trash.join("08-app-data/wakegpt.sqlite3").is_file());
        assert!(trash.join("01-cache/cache.bin").is_file());
        assert!(trash.join("04-preferences.plist").is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(trash.join(PACKAGE_MANIFEST_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(
            manifest["purpose"],
            "wakegpt-local-data-reset-recovery-package"
        );
        assert!(manifest
            .to_string()
            .contains("Library/Application Support/com.wakegpt.test"));
        assert!(!manifest.to_string().contains(root.0.to_str().unwrap()));
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
        assert!(!layout.intent_path.exists());
        let notice = read_notice(&layout.app_data_dir()).unwrap().unwrap();
        assert_eq!(notice.outcome, LocalDataResetOutcome::Completed);
        assert_eq!(notice.moved_item_count, 3);
        assert!(!acknowledge_notice(
            &layout.app_data_dir(),
            notice.occurred_at_ms.saturating_add(1)
        )
        .unwrap());
        assert!(read_notice(&layout.app_data_dir()).unwrap().is_some());
        assert!(acknowledge_notice(&layout.app_data_dir(), notice.occurred_at_ms).unwrap());
        assert!(read_notice(&layout.app_data_dir()).unwrap().is_none());
    }

    #[test]
    fn partial_staging_is_resumed_without_overwriting() {
        let root = TestRoot::new("resume");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        fixture(&layout);
        schedule_for_layout(&layout).unwrap();
        assert_eq!(
            collecting_app_data_for_layout(&layout).unwrap(),
            Some(layout.app_data_dir())
        );
        let intent = read_intent(&layout).unwrap();
        let staging = layout.staging_root(&intent.reset_id);
        ensure_private_directory(&staging).unwrap();
        fs::rename(
            layout.target_path(ResetTargetKind::Cache),
            staging.join(ResetTargetKind::Cache.staged_name()),
        )
        .unwrap();
        assert!(collecting_app_data_for_layout(&layout).unwrap().is_none());
        let trash = root.0.join("Synthetic Trash");

        apply_pending_for_layout(&layout, |source| {
            fs::rename(source, &trash)
                .map_err(|_| LocalDataResetError("local_data_reset_trash_failed"))
        })
        .unwrap();

        assert!(trash.join("01-cache/cache.bin").is_file());
        assert!(trash.join("08-app-data/wakegpt.sqlite3").is_file());
    }

    #[test]
    fn trash_failure_rolls_every_target_back() {
        let root = TestRoot::new("rollback");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);
        schedule_for_layout(&layout).unwrap();

        let outcome = apply_pending_for_layout(&layout, |_| {
            Err(LocalDataResetError("local_data_reset_trash_failed"))
        })
        .unwrap();

        assert!(matches!(
            outcome,
            Some(StartupResetOutcome::RolledBack {
                error_code: "local_data_reset_trash_failed"
            })
        ));
        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert!(layout
            .target_path(ResetTargetKind::Cache)
            .join("cache.bin")
            .is_file());
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
        assert!(!layout.intent_path.exists());
        let notice = read_notice(&layout.app_data_dir()).unwrap().unwrap();
        assert_eq!(notice.outcome, LocalDataResetOutcome::RolledBack);
        assert_eq!(
            notice.error_code.as_deref(),
            Some("local_data_reset_trash_failed")
        );
    }

    #[test]
    fn a_tampered_recovery_manifest_is_rolled_back_before_trash() {
        let root = TestRoot::new("manifest-tamper");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        fixture(&layout);
        schedule_for_layout(&layout).unwrap();
        let mut intent = read_intent(&layout).unwrap();
        let staging = layout.staging_root(&intent.reset_id);
        stage_targets(&layout, &intent, &staging).unwrap();
        write_package_manifest(&layout, &intent, &staging).unwrap();
        intent.moved_target_count = count_staged_targets(&intent, &staging).unwrap();
        intent.state = ResetIntentState::Staged;
        write_json_atomic(&layout.intent_path, &intent, true).unwrap();
        fs::write(staging.join(PACKAGE_MANIFEST_FILE_NAME), b"{}").unwrap();

        let outcome = apply_pending_for_layout(&layout, |_| {
            panic!("a tampered manifest must fail before Trash")
        })
        .unwrap();

        assert!(matches!(
            outcome,
            Some(StartupResetOutcome::RolledBack {
                error_code: "local_data_reset_intent_invalid"
            })
        ));
        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert!(!layout.intent_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_fails_closed_and_preserves_other_roots() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new("symlink");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);
        let webkit = layout.target_path(ResetTargetKind::WebKit);
        symlink(&workspace, &webkit).unwrap();
        schedule_for_layout(&layout).unwrap();

        let outcome = apply_pending_for_layout(&layout, |_| {
            panic!("an unsafe target must fail before Trash")
        })
        .unwrap();

        assert!(matches!(
            outcome,
            Some(StartupResetOutcome::RolledBack {
                error_code: "local_data_reset_path_invalid"
            })
        ));
        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
        assert!(fs::symlink_metadata(webkit)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_fails_before_any_system_trash_call() {
        use std::os::unix::fs::symlink;

        let root = TestRoot::new("parent-symlink");
        let layout = ResetLayout::for_home(&root.0, "com.wakegpt.test").unwrap();
        let workspace = fixture(&layout);
        let webkit_parent = root.0.join("Library/WebKit");
        fs::remove_dir(&webkit_parent).unwrap();
        symlink(&workspace, &webkit_parent).unwrap();
        schedule_for_layout(&layout).unwrap();

        let outcome = apply_pending_for_layout(&layout, |_| {
            panic!("an unsafe parent must fail before Trash")
        })
        .unwrap();

        assert!(matches!(
            outcome,
            Some(StartupResetOutcome::RolledBack {
                error_code: "local_data_reset_path_invalid"
            })
        ));
        assert_eq!(
            fs::read(layout.app_data_dir().join("wakegpt.sqlite3")).unwrap(),
            b"database"
        );
        assert_eq!(fs::read(workspace.join("Notes.md")).unwrap(), b"user-owned");
    }
}
