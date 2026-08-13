use crate::api::{CommandError, CommandResult};
use crate::domain::UpdateSettings;
use crate::storage::Store;
use crate::update_install::{self, PreparedUpdateReceipt, UpdateInstallResult};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::StreamExt as _;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter as _, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt as _};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

pub const UPDATER_TARGET: &str = "darwin-universal";
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_RELEASE_NOTES_CHARS: usize = 8_000;
const UPDATE_PROGRESS_EVENT: &str = "wakegpt://update-download-progress";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Unavailable,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Verifying,
    ReadyToInstall,
    Installing,
    Restarting,
    Installed,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub configured: bool,
    pub configuration_error_code: Option<&'static str>,
    pub current_version: &'static str,
    pub target: &'static str,
    pub external_network_enabled: bool,
    pub automatic_checks_enabled: bool,
    pub automatic_downloads_enabled: bool,
    pub check_interval_hours: u32,
    pub skipped_version: Option<String>,
    pub last_checked_at_ms: Option<i64>,
    pub last_error_code: Option<String>,
    pub phase: UpdatePhase,
    pub available_version: Option<String>,
    pub available_is_skipped: bool,
    pub release_notes: Option<String>,
    pub published_at: Option<String>,
    pub operation_error_code: Option<String>,
    pub downloaded_bytes: u64,
    pub download_total_bytes: Option<u64>,
    pub prepared_transaction_id: Option<String>,
    pub install_outcome: Option<String>,
    pub install_from_version: Option<String>,
    pub install_version: Option<String>,
    pub install_error_code: Option<String>,
    pub install_occurred_at_ms: Option<i64>,
    pub release_page_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetUpdateSettingsRequest {
    pub external_network_enabled: bool,
    pub automatic_checks_enabled: bool,
    pub automatic_downloads_enabled: bool,
    pub check_interval_hours: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckForUpdatesRequest {
    pub manual: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkipUpdateVersionRequest {
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadUpdateRequest {
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscardDownloadedUpdateRequest {
    pub transaction_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallDownloadedUpdateRequest {
    pub transaction_id: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcknowledgeUpdateResultRequest {
    pub occurred_at_ms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenUpdateReleaseRequest {
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadProgress {
    pub schema_version: u32,
    pub transaction_id: String,
    pub version: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub verifying: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseUpdateConfig {
    public_key: String,
    endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReleaseConfigState {
    Unconfigured,
    Invalid,
    Configured(ReleaseUpdateConfig),
}

#[derive(Debug, Clone)]
struct RuntimeSnapshot {
    phase: UpdatePhase,
    available_version: Option<String>,
    release_notes: Option<String>,
    published_at: Option<String>,
    available_download_url: Option<String>,
    available_signature: Option<String>,
    operation_error_code: Option<String>,
    downloaded_bytes: u64,
    download_total_bytes: Option<u64>,
    prepared: Option<PreparedUpdateReceipt>,
    install_result: Option<UpdateInstallResult>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            available_version: None,
            release_notes: None,
            published_at: None,
            available_download_url: None,
            available_signature: None,
            operation_error_code: None,
            downloaded_bytes: 0,
            download_total_bytes: None,
            prepared: None,
            install_result: None,
        }
    }
}

pub struct UpdateRuntime {
    inner: Mutex<RuntimeSnapshot>,
}

impl UpdateRuntime {
    pub(crate) fn load(app_data_dir: &Path) -> Result<Self, update_install::UpdateInstallError> {
        let prepared = update_install::load_prepared(app_data_dir)?;
        let install_result = update_install::load_install_result(app_data_dir)?;
        let mut snapshot = RuntimeSnapshot::default();
        if let Some(result) = install_result {
            snapshot.phase = phase_for_install_result(&result);
            snapshot.operation_error_code = result.error_code.clone();
            snapshot.available_version = Some(result.version.clone());
            snapshot.install_result = Some(result);
        } else if let Some(receipt) = prepared {
            snapshot.phase = UpdatePhase::ReadyToInstall;
            snapshot.available_version = Some(receipt.version.clone());
            snapshot.available_download_url = Some(receipt.download_url.clone());
            snapshot.available_signature = Some(receipt.signature.clone());
            snapshot.downloaded_bytes = receipt.byte_size;
            snapshot.download_total_bytes = Some(receipt.byte_size);
            snapshot.prepared = Some(receipt);
        }
        Ok(Self {
            inner: Mutex::new(snapshot),
        })
    }

    fn refresh_from_disk(&self, app_data_dir: &Path) -> CommandResult<()> {
        let install_result =
            update_install::load_install_result(app_data_dir).map_err(|error| {
                CommandError::unchanged(
                    error.0,
                    "无法验证更新结果回执；WakeGPT 没有继续安装",
                    false,
                )
            })?;
        let prepared = update_install::load_prepared(app_data_dir).map_err(|error| {
            CommandError::unchanged(error.0, "无法验证已下载的更新；WakeGPT 没有继续安装", false)
        })?;
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法刷新更新状态；WakeGPT 没有继续安装",
                true,
            )
        })?;
        if let Some(result) = install_result {
            state.phase = phase_for_install_result(&result);
            state.operation_error_code = result.error_code.clone();
            state.available_version = Some(result.version.clone());
            state.install_result = Some(result);
            state.prepared = prepared;
            return Ok(());
        }
        if matches!(
            state.phase,
            UpdatePhase::Checking
                | UpdatePhase::Downloading
                | UpdatePhase::Verifying
                | UpdatePhase::Installing
                | UpdatePhase::Restarting
        ) {
            return Ok(());
        }
        state.install_result = None;
        match prepared {
            Some(receipt) => {
                state.phase = UpdatePhase::ReadyToInstall;
                state.available_version = Some(receipt.version.clone());
                state.available_download_url = Some(receipt.download_url.clone());
                state.available_signature = Some(receipt.signature.clone());
                state.downloaded_bytes = receipt.byte_size;
                state.download_total_bytes = Some(receipt.byte_size);
                state.prepared = Some(receipt);
            }
            None if state.phase == UpdatePhase::ReadyToInstall => {
                *state = RuntimeSnapshot::default();
            }
            None => state.prepared = None,
        }
        Ok(())
    }

    fn snapshot(&self) -> CommandResult<RuntimeSnapshot> {
        self.inner.lock().map(|state| state.clone()).map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法读取更新状态；WakeGPT 没有发起网络请求",
                true,
            )
        })
    }

    fn begin_check(&self) -> CommandResult<bool> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法读取更新状态；WakeGPT 没有发起网络请求",
                true,
            )
        })?;
        if matches!(
            state.phase,
            UpdatePhase::Checking
                | UpdatePhase::Downloading
                | UpdatePhase::Verifying
                | UpdatePhase::ReadyToInstall
                | UpdatePhase::Installing
                | UpdatePhase::Restarting
        ) {
            return Ok(false);
        }
        state.phase = UpdatePhase::Checking;
        state.operation_error_code = None;
        Ok(true)
    }

    fn finish_up_to_date(&self) -> CommandResult<()> {
        self.replace(RuntimeSnapshot {
            phase: UpdatePhase::UpToDate,
            ..RuntimeSnapshot::default()
        })
    }

    fn finish_available(
        &self,
        update: &Update,
        release_notes: Option<String>,
        published_at: Option<String>,
    ) -> CommandResult<()> {
        self.replace(RuntimeSnapshot {
            phase: UpdatePhase::Available,
            available_version: Some(update.version.clone()),
            release_notes,
            published_at,
            available_download_url: Some(update.download_url.to_string()),
            available_signature: Some(update.signature.clone()),
            ..RuntimeSnapshot::default()
        })
    }

    fn finish_check_failed(&self, code: &'static str) -> CommandResult<()> {
        let mut current = self.snapshot()?;
        current.phase = UpdatePhase::Failed;
        current.operation_error_code = Some(code.to_owned());
        self.replace(current)
    }

    fn begin_download(&self, version: &str) -> CommandResult<(String, String)> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法开始下载；WakeGPT 没有下载任何内容",
                true,
            )
        })?;
        if state.phase != UpdatePhase::Available
            || state.available_version.as_deref() != Some(version)
        {
            return Err(CommandError::unchanged(
                "update_version_stale",
                "可用版本已经变化；请重新检查更新",
                true,
            ));
        }
        let url = state.available_download_url.clone().ok_or_else(|| {
            CommandError::unchanged(
                "update_metadata_invalid",
                "更新元数据不完整；WakeGPT 没有下载任何内容",
                false,
            )
        })?;
        let signature = state.available_signature.clone().ok_or_else(|| {
            CommandError::unchanged(
                "update_metadata_invalid",
                "更新元数据不完整；WakeGPT 没有下载任何内容",
                false,
            )
        })?;
        state.phase = UpdatePhase::Downloading;
        state.operation_error_code = None;
        state.downloaded_bytes = 0;
        state.download_total_bytes = None;
        Ok((url, signature))
    }

    fn set_download_progress(&self, downloaded: u64, total: Option<u64>) -> CommandResult<()> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法保存下载进度；下载已经停止",
                true,
            )
        })?;
        if state.phase != UpdatePhase::Downloading {
            return Err(CommandError::unchanged(
                "update_operation_stale",
                "下载状态已经变化；下载已经停止",
                true,
            ));
        }
        state.downloaded_bytes = downloaded;
        state.download_total_bytes = total;
        Ok(())
    }

    fn begin_verifying(&self, downloaded: u64, total: Option<u64>) -> CommandResult<()> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法进入验签阶段；WakeGPT 没有保存该更新",
                true,
            )
        })?;
        if state.phase != UpdatePhase::Downloading {
            return Err(CommandError::unchanged(
                "update_operation_stale",
                "下载状态已经变化；WakeGPT 没有保存该更新",
                true,
            ));
        }
        state.phase = UpdatePhase::Verifying;
        state.downloaded_bytes = downloaded;
        state.download_total_bytes = total;
        Ok(())
    }

    fn finish_ready(&self, receipt: PreparedUpdateReceipt) -> CommandResult<()> {
        let mut state = self.snapshot()?;
        state.phase = UpdatePhase::ReadyToInstall;
        state.available_version = Some(receipt.version.clone());
        state.available_download_url = Some(receipt.download_url.clone());
        state.available_signature = Some(receipt.signature.clone());
        state.operation_error_code = None;
        state.downloaded_bytes = receipt.byte_size;
        state.download_total_bytes = Some(receipt.byte_size);
        state.prepared = Some(receipt);
        state.install_result = None;
        self.replace(state)
    }

    fn fail_delivery(&self, code: &'static str) -> CommandResult<()> {
        let mut state = self.snapshot()?;
        state.phase = if state.prepared.is_some() {
            UpdatePhase::ReadyToInstall
        } else if state.available_version.is_some() {
            UpdatePhase::Available
        } else {
            UpdatePhase::Failed
        };
        state.operation_error_code = Some(code.to_owned());
        state.downloaded_bytes = state.prepared.as_ref().map_or(0, |value| value.byte_size);
        state.download_total_bytes = state.prepared.as_ref().map(|value| value.byte_size);
        self.replace(state)
    }

    fn begin_install(
        &self,
        transaction_id: &str,
        version: &str,
    ) -> CommandResult<PreparedUpdateReceipt> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法开始安装；当前版本和本地数据没有改变",
                true,
            )
        })?;
        let receipt = state.prepared.clone().ok_or_else(|| {
            CommandError::unchanged(
                "update_prepared_missing",
                "已下载的更新不存在；请重新下载",
                true,
            )
        })?;
        if state.phase != UpdatePhase::ReadyToInstall
            || receipt.transaction_id != transaction_id
            || receipt.version != version
        {
            return Err(CommandError::unchanged(
                "update_prepared_stale",
                "已下载的更新已经变化；请重新确认",
                true,
            ));
        }
        state.phase = UpdatePhase::Installing;
        state.operation_error_code = None;
        Ok(receipt)
    }

    fn finish_restarting(&self) -> CommandResult<()> {
        let mut state = self.snapshot()?;
        state.phase = UpdatePhase::Restarting;
        state.operation_error_code = None;
        self.replace(state)
    }

    fn finish_discard(&self) -> CommandResult<()> {
        let mut state = self.snapshot()?;
        state.phase = if state.available_download_url.is_some() {
            UpdatePhase::Available
        } else {
            UpdatePhase::Idle
        };
        state.prepared = None;
        state.downloaded_bytes = 0;
        state.download_total_bytes = None;
        state.operation_error_code = None;
        self.replace(state)
    }

    fn finish_acknowledge(&self) -> CommandResult<()> {
        let mut state = self.snapshot()?;
        state.install_result = None;
        state.operation_error_code = None;
        state.phase = if state.prepared.is_some() {
            UpdatePhase::ReadyToInstall
        } else {
            UpdatePhase::Idle
        };
        self.replace(state)
    }

    fn replace(&self, next: RuntimeSnapshot) -> CommandResult<()> {
        let mut state = self.inner.lock().map_err(|_| {
            CommandError::unchanged(
                "update_state_unavailable",
                "无法保存更新状态；WakeGPT 没有安装任何内容",
                true,
            )
        })?;
        *state = next;
        Ok(())
    }
}

fn phase_for_install_result(result: &UpdateInstallResult) -> UpdatePhase {
    match result.outcome.as_str() {
        "installed" => UpdatePhase::Installed,
        "rolledBack" => UpdatePhase::RolledBack,
        _ => UpdatePhase::Failed,
    }
}

fn compiled_release_config() -> ReleaseConfigState {
    validate_release_config(
        option_env!("WAKEGPT_UPDATER_PUBLIC_KEY"),
        option_env!("WAKEGPT_UPDATER_ENDPOINT"),
    )
}

fn validate_release_config(public_key: Option<&str>, endpoint: Option<&str>) -> ReleaseConfigState {
    let public_key = public_key.map(str::trim).filter(|value| !value.is_empty());
    let endpoint = endpoint.map(str::trim).filter(|value| !value.is_empty());
    let (Some(public_key), Some(endpoint)) = (public_key, endpoint) else {
        return if public_key.is_none() && endpoint.is_none() {
            ReleaseConfigState::Unconfigured
        } else {
            ReleaseConfigState::Invalid
        };
    };
    let public_key_valid = decode_public_key(public_key).is_ok();
    let endpoint_suffix = endpoint.strip_prefix("https://");
    let endpoint_valid = endpoint.len() <= 2_048
        && endpoint_suffix.is_some_and(|suffix| {
            !suffix.is_empty()
                && !suffix.starts_with('/')
                && !suffix
                    .bytes()
                    .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
                && !suffix
                    .split('/')
                    .next()
                    .is_some_and(|host| host.contains('@'))
                && !endpoint.contains('#')
        });
    if !public_key_valid || !endpoint_valid {
        return ReleaseConfigState::Invalid;
    }
    ReleaseConfigState::Configured(ReleaseUpdateConfig {
        public_key: public_key.to_owned(),
        endpoint: endpoint.to_owned(),
    })
}

fn decode_outer_text(value: &str, max_encoded_bytes: usize) -> Result<String, &'static str> {
    if value.is_empty()
        || value.len() > max_encoded_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return Err("update_signature_invalid");
    }
    let decoded = BASE64_STANDARD
        .decode(value)
        .map_err(|_| "update_signature_invalid")?;
    String::from_utf8(decoded).map_err(|_| "update_signature_invalid")
}

fn decode_public_key(value: &str) -> Result<PublicKey, &'static str> {
    let decoded = decode_outer_text(value, 512)?;
    PublicKey::decode(&decoded).map_err(|_| "update_signature_invalid")
}

fn decode_signature(value: &str) -> Result<Signature, &'static str> {
    let decoded = decode_outer_text(value, 4_096)?;
    Signature::decode(&decoded).map_err(|_| "update_signature_invalid")
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn automatic_check_due(settings: &UpdateSettings, now: i64) -> bool {
    if !settings.automatic_checks_enabled {
        return false;
    }
    let Some(last_checked_at_ms) = settings.last_checked_at_ms else {
        return true;
    };
    if last_checked_at_ms > now {
        return true;
    }
    let interval_ms = i64::from(settings.check_interval_hours)
        .saturating_mul(60)
        .saturating_mul(60)
        .saturating_mul(1_000);
    now.saturating_sub(last_checked_at_ms) >= interval_ms
}

fn truncate_release_notes(value: String) -> String {
    value.chars().take(MAX_RELEASE_NOTES_CHARS).collect()
}

fn valid_signature(value: &str) -> bool {
    decode_signature(value).is_ok()
}

fn valid_update_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

fn immutable_github_asset_url(version: &str, value: &str) -> bool {
    let Some(path) = value.strip_prefix("https://github.com/") else {
        return false;
    };
    if value.contains('?') || value.contains('#') || path.contains("/releases/latest/download/") {
        return false;
    }
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() != 6
        || segments[2] != "releases"
        || segments[3] != "download"
        || segments[4] != format!("v{version}")
        || segments[5].is_empty()
    {
        return false;
    }
    let safe_repository_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    };
    safe_repository_segment(segments[0])
        && safe_repository_segment(segments[1])
        && segments[5].starts_with("WakeGPT")
        && segments[5].ends_with(".app.tar.gz")
}

fn release_page_url(version: &str, download_url: &str) -> Option<String> {
    if !immutable_github_asset_url(version, download_url) {
        return None;
    }
    let path = download_url.strip_prefix("https://github.com/")?;
    let segments = path.split('/').collect::<Vec<_>>();
    Some(format!(
        "https://github.com/{}/{}/releases/tag/v{version}",
        segments[0], segments[1]
    ))
}

fn update_status_from_parts(
    settings: UpdateSettings,
    runtime: RuntimeSnapshot,
    config: ReleaseConfigState,
) -> UpdateStatus {
    let (configured, configuration_error_code) = match config {
        ReleaseConfigState::Configured(_) => (true, None),
        ReleaseConfigState::Unconfigured => (false, None),
        ReleaseConfigState::Invalid => (false, Some("update_config_invalid")),
    };
    let phase = if configured || runtime.prepared.is_some() || runtime.install_result.is_some() {
        runtime.phase
    } else {
        UpdatePhase::Unavailable
    };
    let available_is_skipped = runtime.available_version.is_some()
        && runtime.available_version == settings.skipped_version;
    let prepared_transaction_id = runtime
        .prepared
        .as_ref()
        .map(|receipt| receipt.transaction_id.clone());
    let release_page_url = runtime.available_version.as_deref().and_then(|version| {
        runtime
            .available_download_url
            .as_deref()
            .and_then(|url| release_page_url(version, url))
    });
    let install_outcome = runtime
        .install_result
        .as_ref()
        .map(|result| result.outcome.clone());
    let install_from_version = runtime
        .install_result
        .as_ref()
        .map(|result| result.from_version.clone());
    let install_version = runtime
        .install_result
        .as_ref()
        .map(|result| result.version.clone());
    let install_error_code = runtime
        .install_result
        .as_ref()
        .and_then(|result| result.error_code.clone());
    let install_occurred_at_ms = runtime
        .install_result
        .as_ref()
        .map(|result| result.occurred_at_ms);
    UpdateStatus {
        configured,
        configuration_error_code,
        current_version: env!("CARGO_PKG_VERSION"),
        target: UPDATER_TARGET,
        external_network_enabled: settings.external_network_enabled,
        automatic_checks_enabled: settings.automatic_checks_enabled,
        automatic_downloads_enabled: settings.automatic_downloads_enabled,
        check_interval_hours: settings.check_interval_hours,
        skipped_version: settings.skipped_version,
        last_checked_at_ms: settings.last_checked_at_ms,
        last_error_code: settings.last_error_code,
        phase,
        available_version: runtime.available_version,
        available_is_skipped,
        release_notes: runtime.release_notes,
        published_at: runtime.published_at,
        operation_error_code: runtime.operation_error_code,
        downloaded_bytes: runtime.downloaded_bytes,
        download_total_bytes: runtime.download_total_bytes,
        prepared_transaction_id,
        install_outcome,
        install_from_version,
        install_version,
        install_error_code,
        install_occurred_at_ms,
        release_page_url,
    }
}

fn current_status(
    app: &AppHandle,
    store: &Store,
    runtime: &UpdateRuntime,
) -> CommandResult<UpdateStatus> {
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "update_storage_unavailable",
            "无法读取更新存储；WakeGPT 没有继续安装",
            true,
        )
    })?;
    runtime.refresh_from_disk(&app_data_dir)?;
    let settings = store.update_settings().map_err(CommandError::from)?;
    let snapshot = runtime.snapshot()?;
    Ok(update_status_from_parts(
        settings,
        snapshot,
        compiled_release_config(),
    ))
}

fn valid_update_metadata(update: &Update) -> bool {
    update.current_version == env!("CARGO_PKG_VERSION")
        && update.target == UPDATER_TARGET
        && valid_update_version(&update.version)
        && valid_signature(&update.signature)
        && immutable_github_asset_url(&update.version, update.download_url.as_str())
}

async fn fetch_checked_update(
    app: &AppHandle,
    config: &ReleaseUpdateConfig,
) -> Result<Option<Update>, &'static str> {
    let endpoint = config
        .endpoint
        .parse()
        .map_err(|_| "update_config_invalid")?;
    let updater = app
        .updater_builder()
        .target(UPDATER_TARGET)
        .pubkey(config.public_key.clone())
        .timeout(UPDATE_CHECK_TIMEOUT)
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.build())
        .map_err(|_| "update_config_invalid")?;
    let update = updater.check().await.map_err(|_| "update_check_failed")?;
    if update
        .as_ref()
        .is_some_and(|value| !valid_update_metadata(value))
    {
        return Err("update_metadata_invalid");
    }
    Ok(update)
}

fn check_error_message(code: &'static str) -> &'static str {
    match code {
        "update_config_invalid" => "更新通道配置无效；WakeGPT 没有下载任何内容",
        "update_metadata_invalid" => "更新元数据未通过安全校验；WakeGPT 没有下载任何内容",
        _ => "无法连接 WakeGPT 更新通道；没有下载或安装任何内容",
    }
}

#[tauri::command]
pub fn update_status(
    app: AppHandle,
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
) -> CommandResult<UpdateStatus> {
    current_status(&app, &store, &runtime)
}

#[tauri::command]
pub fn set_update_settings(
    app: AppHandle,
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
    request: SetUpdateSettingsRequest,
) -> CommandResult<UpdateStatus> {
    store
        .set_update_settings(
            request.external_network_enabled,
            request.automatic_checks_enabled,
            request.automatic_downloads_enabled,
            request.check_interval_hours,
        )
        .map_err(CommandError::from)?;
    current_status(&app, &store, &runtime)
}

#[tauri::command]
pub fn skip_update_version(
    app: AppHandle,
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
    request: SkipUpdateVersionRequest,
) -> CommandResult<UpdateStatus> {
    if let Some(version) = request.version.as_deref() {
        let snapshot = runtime.snapshot()?;
        if snapshot.available_version.as_deref() != Some(version) {
            return Err(CommandError::unchanged(
                "update_version_stale",
                "可用版本已经变化；请重新检查更新",
                true,
            ));
        }
        if snapshot.prepared.is_some() {
            return Err(CommandError::unchanged(
                "update_prepared_must_discard",
                "该版本已经下载；请先移除已下载更新，再选择跳过",
                false,
            ));
        }
    }
    store
        .set_skipped_update_version(request.version.as_deref())
        .map_err(CommandError::from)?;
    current_status(&app, &store, &runtime)
}

#[tauri::command]
pub async fn check_for_updates(
    app: AppHandle,
    request: CheckForUpdatesRequest,
) -> CommandResult<UpdateStatus> {
    let store = app.state::<Store>();
    let runtime = app.state::<UpdateRuntime>();
    let config = match compiled_release_config() {
        ReleaseConfigState::Configured(config) => config,
        ReleaseConfigState::Unconfigured => return current_status(&app, &store, &runtime),
        ReleaseConfigState::Invalid => {
            return Err(CommandError::unchanged(
                "update_config_invalid",
                "更新通道配置无效；WakeGPT 没有发起网络请求",
                false,
            ));
        }
    };
    let settings = store.update_settings().map_err(CommandError::from)?;
    if !settings.external_network_enabled {
        if request.manual {
            return Err(CommandError::unchanged(
                "update_network_disabled",
                "外部网络已关闭；WakeGPT 没有发起更新请求",
                false,
            ));
        }
        return current_status(&app, &store, &runtime);
    }
    if !request.manual && !automatic_check_due(&settings, now_ms()) {
        return current_status(&app, &store, &runtime);
    }
    if !runtime.begin_check()? {
        if request.manual {
            return Err(CommandError::unchanged(
                "update_check_in_progress",
                "WakeGPT 正在检查更新",
                true,
            ));
        }
        return current_status(&app, &store, &runtime);
    }

    match fetch_checked_update(&app, &config).await {
        Ok(None) => {
            runtime.finish_up_to_date()?;
            store
                .record_update_check(None)
                .map_err(CommandError::from)?;
        }
        Ok(Some(update)) => {
            let release_notes = update.body.clone().map(truncate_release_notes);
            let published_at = update.date.map(|date| date.to_string());
            runtime.finish_available(&update, release_notes, published_at)?;
            store
                .record_update_check(None)
                .map_err(CommandError::from)?;
        }
        Err(code) => {
            runtime.finish_check_failed(code)?;
            store
                .record_update_check(Some(code))
                .map_err(CommandError::from)?;
            return Err(CommandError::unchanged(
                code,
                check_error_message(code),
                code == "update_check_failed",
            ));
        }
    }
    current_status(&app, &store, &runtime)
}

fn ensure_rustls_provider() -> Result<(), &'static str> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        Ok(())
    } else {
        Err("update_download_tls_unavailable")
    }
}

fn allowed_asset_host(host: Option<&str>) -> bool {
    matches!(
        host,
        Some("release-assets.githubusercontent.com") | Some("objects.githubusercontent.com")
    )
}

fn allowed_download_url(initial: &str, url: &reqwest::Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    url.as_str() == initial || allowed_asset_host(url.host_str())
}

fn exact_update_matches(
    update: &Update,
    version: &str,
    download_url: &str,
    signature: &str,
) -> bool {
    valid_update_metadata(update)
        && update.version == version
        && update.download_url.as_str() == download_url
        && update.signature == signature
}

fn emit_download_progress(
    app: &AppHandle,
    transaction_id: &str,
    version: &str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    verifying: bool,
) {
    let _ = app.emit(
        UPDATE_PROGRESS_EVENT,
        UpdateDownloadProgress {
            schema_version: 1,
            transaction_id: transaction_id.to_owned(),
            version: version.to_owned(),
            downloaded_bytes,
            total_bytes,
            verifying,
        },
    );
}

async fn download_and_prepare(
    app: &AppHandle,
    runtime: &UpdateRuntime,
    config: &ReleaseUpdateConfig,
    update: &Update,
    transaction_id: &str,
) -> Result<PreparedUpdateReceipt, &'static str> {
    ensure_rustls_provider()?;
    let public_key = decode_public_key(&config.public_key)?;
    let signature = decode_signature(&update.signature)?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|_| "update_signature_invalid")?;
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "update_storage_unavailable")?;
    let (mut stage, stage_path) =
        update_install::create_download_stage(&app_data_dir, transaction_id)
            .map_err(|error| error.0)?;
    let initial_url = update.download_url.to_string();
    let policy_initial = initial_url.clone();
    let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
        let chain_is_valid = attempt.previous().len() <= 3
            && attempt
                .previous()
                .first()
                .is_some_and(|url| url.as_str() == policy_initial)
            && attempt
                .previous()
                .iter()
                .all(|url| allowed_download_url(&policy_initial, url));
        if chain_is_valid && allowed_download_url(&policy_initial, attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });

    let result = async {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(UPDATE_DOWNLOAD_CONNECT_TIMEOUT)
            .timeout(UPDATE_DOWNLOAD_TIMEOUT)
            .redirect(redirect_policy)
            .user_agent("WakeGPT-Updater/1")
            .build()
            .map_err(|_| "update_download_failed")?;
        let response = client
            .get(update.download_url.clone())
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| "update_download_failed")?;
        if !response.status().is_success() || !allowed_download_url(&initial_url, response.url()) {
            return Err("update_download_failed");
        }
        let total_bytes = response.content_length().filter(|value| *value > 0);
        if total_bytes.is_some_and(|value| value > update_install::MAX_UPDATE_BYTES) {
            return Err("update_download_too_large");
        }
        let mut downloaded_bytes = 0_u64;
        let mut last_emitted_bytes = 0_u64;
        let mut hasher = Sha256::new();
        let mut stream = response.bytes_stream();
        emit_download_progress(app, transaction_id, &update.version, 0, total_bytes, false);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "update_download_failed")?;
            downloaded_bytes = downloaded_bytes
                .checked_add(chunk.len() as u64)
                .ok_or("update_download_too_large")?;
            if downloaded_bytes > update_install::MAX_UPDATE_BYTES {
                return Err("update_download_too_large");
            }
            stage
                .write_all(&chunk)
                .map_err(|_| "update_storage_unavailable")?;
            verifier.update(&chunk);
            hasher.update(&chunk);
            runtime
                .set_download_progress(downloaded_bytes, total_bytes)
                .map_err(|_| "update_state_unavailable")?;
            if downloaded_bytes.saturating_sub(last_emitted_bytes) >= 256 * 1024
                || total_bytes == Some(downloaded_bytes)
            {
                emit_download_progress(
                    app,
                    transaction_id,
                    &update.version,
                    downloaded_bytes,
                    total_bytes,
                    false,
                );
                last_emitted_bytes = downloaded_bytes;
            }
        }
        if downloaded_bytes == 0 {
            return Err("update_download_empty");
        }
        if total_bytes.is_some_and(|total| total != downloaded_bytes) {
            return Err("update_download_incomplete");
        }
        runtime
            .begin_verifying(downloaded_bytes, total_bytes)
            .map_err(|_| "update_state_unavailable")?;
        emit_download_progress(
            app,
            transaction_id,
            &update.version,
            downloaded_bytes,
            total_bytes,
            true,
        );
        verifier
            .finalize()
            .map_err(|_| "update_signature_invalid")?;
        update_install::sync_download_stage(&stage).map_err(|error| error.0)?;
        drop(stage);
        let digest = hasher.finalize();
        let mut sha256 = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(sha256, "{byte:02x}");
        }
        let receipt = PreparedUpdateReceipt {
            schema_version: 1,
            transaction_id: transaction_id.to_owned(),
            from_version: update.current_version.clone(),
            version: update.version.clone(),
            download_url: update.download_url.to_string(),
            signature: update.signature.clone(),
            sha256,
            byte_size: downloaded_bytes,
            downloaded_at_ms: now_ms(),
        };
        update_install::commit_download(&app_data_dir, &stage_path, &receipt)
            .map_err(|error| error.0)?;
        let verified = update_install::load_prepared(&app_data_dir)
            .map_err(|error| error.0)?
            .ok_or("update_prepared_missing")?;
        if verified != receipt {
            return Err("update_prepared_changed");
        }
        Ok(verified)
    }
    .await;

    if result.is_err() {
        let _ = update_install::abort_download_stage(&app_data_dir, &stage_path);
        let _ = update_install::discard_prepared(&app_data_dir);
    }
    result
}

fn delivery_error_message(code: &'static str) -> &'static str {
    match code {
        "update_signature_invalid" => "更新签名验证失败；已删除下载内容，当前版本没有改变",
        "update_download_too_large" => "更新包超过安全大小限制；已停止并删除下载内容",
        "update_download_empty" | "update_download_incomplete" => {
            "更新下载不完整；已删除临时内容，可以重试"
        }
        "update_metadata_changed" | "update_version_stale" => {
            "更新元数据已经变化；请重新检查后再下载"
        }
        _ => "更新下载未完成；当前版本和本地数据没有改变，可以重试",
    }
}

#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    request: DownloadUpdateRequest,
) -> CommandResult<UpdateStatus> {
    if !valid_update_version(&request.version) {
        return Err(CommandError::unchanged(
            "update_request_invalid",
            "请求的更新版本无效；WakeGPT 没有下载任何内容",
            false,
        ));
    }
    let store = app.state::<Store>();
    let runtime = app.state::<UpdateRuntime>();
    let settings = store.update_settings().map_err(CommandError::from)?;
    if !settings.external_network_enabled {
        return Err(CommandError::unchanged(
            "update_network_disabled",
            "外部网络已关闭；WakeGPT 没有下载任何内容",
            false,
        ));
    }
    if settings.skipped_version.as_deref() == Some(request.version.as_str()) {
        return Err(CommandError::unchanged(
            "update_version_skipped",
            "该版本已被跳过；请先恢复提示",
            false,
        ));
    }
    let config = match compiled_release_config() {
        ReleaseConfigState::Configured(config) => config,
        _ => {
            return Err(CommandError::unchanged(
                "update_config_invalid",
                "正式更新通道尚未配置；WakeGPT 没有下载任何内容",
                false,
            ));
        }
    };
    let (expected_url, expected_signature) = runtime.begin_download(&request.version)?;
    let operation = async {
        let update = fetch_checked_update(&app, &config)
            .await?
            .ok_or("update_version_stale")?;
        if !exact_update_matches(
            &update,
            &request.version,
            &expected_url,
            &expected_signature,
        ) {
            return Err("update_metadata_changed");
        }
        let transaction_id = uuid::Uuid::now_v7().to_string();
        download_and_prepare(&app, &runtime, &config, &update, &transaction_id).await
    }
    .await;
    match operation {
        Ok(receipt) => runtime.finish_ready(receipt)?,
        Err(code) => {
            runtime.fail_delivery(code)?;
            return Err(CommandError::unchanged(
                code,
                delivery_error_message(code),
                !matches!(
                    code,
                    "update_signature_invalid"
                        | "update_metadata_changed"
                        | "update_download_too_large"
                ),
            ));
        }
    }
    current_status(&app, &store, &runtime)
}

fn load_verified_prepared_bytes(
    path: &Path,
    receipt: &PreparedUpdateReceipt,
    config: &ReleaseUpdateConfig,
) -> Result<Vec<u8>, &'static str> {
    let public_key = decode_public_key(&config.public_key)?;
    let signature = decode_signature(&receipt.signature)?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|_| "update_signature_invalid")?;
    let before = std::fs::symlink_metadata(path).map_err(|_| "update_prepared_missing")?;
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.len() != receipt.byte_size
        || before.len() == 0
        || before.len() > update_install::MAX_UPDATE_BYTES
    {
        return Err("update_prepared_changed");
    }
    #[cfg(unix)]
    if before.nlink() != 1 || before.mode() & 0o022 != 0 {
        return Err("update_prepared_changed");
    }
    let capacity = usize::try_from(before.len()).map_err(|_| "update_download_too_large")?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options.open(path).map_err(|_| "update_prepared_missing")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "update_storage_unavailable")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() as u64 > update_install::MAX_UPDATE_BYTES {
            return Err("update_download_too_large");
        }
        verifier.update(&buffer[..read]);
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata().map_err(|_| "update_storage_unavailable")?;
    if bytes.len() as u64 != receipt.byte_size
        || after.len() != before.len()
        || after.modified().ok() != before.modified().ok()
    {
        return Err("update_prepared_changed");
    }
    #[cfg(unix)]
    if after.dev() != before.dev() || after.ino() != before.ino() || after.nlink() != 1 {
        return Err("update_prepared_changed");
    }
    verifier
        .finalize()
        .map_err(|_| "update_signature_invalid")?;
    let digest = hasher.finalize();
    let mut sha256 = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(sha256, "{byte:02x}");
    }
    if sha256 != receipt.sha256 {
        return Err("update_prepared_changed");
    }
    Ok(bytes)
}

#[tauri::command]
pub fn discard_downloaded_update(
    app: AppHandle,
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
    request: DiscardDownloadedUpdateRequest,
) -> CommandResult<UpdateStatus> {
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "update_storage_unavailable",
            "无法读取更新存储；没有移除任何内容",
            true,
        )
    })?;
    let receipt = update_install::load_prepared(&app_data_dir)
        .map_err(|error| {
            CommandError::unchanged(error.0, "无法验证已下载的更新；没有移除任何内容", false)
        })?
        .ok_or_else(|| {
            CommandError::unchanged("update_prepared_missing", "没有可移除的已下载更新", false)
        })?;
    if receipt.transaction_id != request.transaction_id {
        return Err(CommandError::unchanged(
            "update_prepared_stale",
            "已下载的更新已经变化；请刷新状态",
            true,
        ));
    }
    update_install::discard_prepared(&app_data_dir).map_err(|error| {
        CommandError::unchanged(error.0, "无法移除已下载的更新；现有内容保持不变", true)
    })?;
    runtime.finish_discard()?;
    current_status(&app, &store, &runtime)
}

#[tauri::command]
pub fn acknowledge_update_result(
    app: AppHandle,
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
    request: AcknowledgeUpdateResultRequest,
) -> CommandResult<UpdateStatus> {
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "update_storage_unavailable",
            "无法读取更新结果；没有确认任何内容",
            true,
        )
    })?;
    let result = update_install::load_install_result(&app_data_dir)
        .map_err(|error| {
            CommandError::unchanged(error.0, "无法验证更新结果；没有确认任何内容", false)
        })?
        .ok_or_else(|| {
            CommandError::unchanged("update_result_missing", "没有待确认的更新结果", false)
        })?;
    if result.occurred_at_ms != request.occurred_at_ms {
        return Err(CommandError::unchanged(
            "update_result_stale",
            "更新结果已经变化；请刷新后再确认",
            true,
        ));
    }
    update_install::acknowledge_install_result(&app_data_dir).map_err(|error| {
        CommandError::unchanged(error.0, "无法确认更新结果；结果回执仍保留", true)
    })?;
    runtime.finish_acknowledge()?;
    current_status(&app, &store, &runtime)
}

#[tauri::command]
pub fn open_update_release(
    store: State<'_, Store>,
    runtime: State<'_, UpdateRuntime>,
    request: OpenUpdateReleaseRequest,
) -> CommandResult<()> {
    if !store
        .update_settings()
        .map_err(CommandError::from)?
        .external_network_enabled
    {
        return Err(CommandError::unchanged(
            "update_network_disabled",
            "外部网络已关闭；WakeGPT 没有打开 GitHub",
            false,
        ));
    }
    let snapshot = runtime.snapshot()?;
    if snapshot.available_version.as_deref() != Some(request.version.as_str()) {
        return Err(CommandError::unchanged(
            "update_version_stale",
            "可用版本已经变化；请刷新更新状态",
            true,
        ));
    }
    let page = snapshot
        .available_download_url
        .as_deref()
        .and_then(|url| release_page_url(&request.version, url))
        .ok_or_else(|| {
            CommandError::unchanged(
                "update_metadata_invalid",
                "无法验证 GitHub 发布页地址；WakeGPT 没有打开链接",
                false,
            )
        })?;
    #[cfg(target_os = "macos")]
    {
        Command::new("/usr/bin/open")
            .arg(page)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                CommandError::unchanged(
                    "update_release_open_failed",
                    "无法打开 GitHub 发布页；WakeGPT 没有改变任何内容",
                    true,
                )
            })?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = page;
        Err(CommandError::unchanged(
            "update_platform_unsupported",
            "当前平台尚未验证 GitHub 发布页入口",
            false,
        ))
    }
}

#[tauri::command]
pub async fn install_downloaded_update(
    app: AppHandle,
    request: InstallDownloadedUpdateRequest,
) -> CommandResult<UpdateStatus> {
    let config = match compiled_release_config() {
        ReleaseConfigState::Configured(config) => config,
        _ => {
            return Err(CommandError::unchanged(
                "update_config_invalid",
                "正式更新通道尚未配置；WakeGPT 没有安装任何内容",
                false,
            ));
        }
    };
    let store = app.state::<Store>();
    let runtime = app.state::<UpdateRuntime>();
    if !store
        .update_settings()
        .map_err(CommandError::from)?
        .external_network_enabled
    {
        return Err(CommandError::unchanged(
            "update_network_disabled",
            "外部网络已关闭；WakeGPT 没有重新验证更新元数据",
            false,
        ));
    }
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "update_storage_unavailable",
            "无法读取已下载更新；WakeGPT 没有安装任何内容",
            true,
        )
    })?;
    runtime.refresh_from_disk(&app_data_dir)?;
    let receipt = runtime.begin_install(&request.transaction_id, &request.version)?;
    let preview =
        crate::local_data_reset::platform_preview(&app.config().identifier).map_err(|error| {
            CommandError::unchanged(error.code(), error.message(), error.retryable())
        })?;
    if preview.reset_scheduled {
        runtime.fail_delivery("update_local_data_reset_pending")?;
        return Err(CommandError::unchanged(
            "update_local_data_reset_pending",
            "本机数据清除正在等待重启；WakeGPT 没有安装更新",
            false,
        ));
    }
    let update = match fetch_checked_update(&app, &config).await {
        Ok(Some(update))
            if exact_update_matches(
                &update,
                &receipt.version,
                &receipt.download_url,
                &receipt.signature,
            ) =>
        {
            update
        }
        Ok(_) => {
            runtime.fail_delivery("update_metadata_changed")?;
            return Err(CommandError::unchanged(
                "update_metadata_changed",
                "更新元数据已经变化；WakeGPT 没有安装任何内容",
                true,
            ));
        }
        Err(code) => {
            runtime.fail_delivery(code)?;
            return Err(CommandError::unchanged(
                code,
                check_error_message(code),
                code == "update_check_failed",
            ));
        }
    };
    let bundle_path = update_install::prepared_bundle_path(&app_data_dir).map_err(|error| {
        CommandError::unchanged(
            error.0,
            "无法读取已下载更新；WakeGPT 没有安装任何内容",
            true,
        )
    })?;
    let bytes = load_verified_prepared_bytes(&bundle_path, &receipt, &config).map_err(|code| {
        let _ = runtime.fail_delivery(code);
        CommandError::unchanged(code, delivery_error_message(code), false)
    })?;

    let app_for_install = app.clone();
    let worker_transaction_id = receipt.transaction_id.clone();
    let worker_app_data_dir = app_data_dir.clone();
    let install_worker =
        tauri::async_runtime::spawn_blocking(move || -> Result<(), &'static str> {
            let sync = app_for_install.state::<crate::file_sync::SyncCoordinator>();
            let attachments = app_for_install.state::<crate::attachments::AttachmentCoordinator>();
            let store = app_for_install.state::<Store>();
            let runtime = app_for_install.state::<UpdateRuntime>();
            let control = app_for_install.state::<crate::lifecycle::IntegrationControl>();
            let previous_paused = control
                .is_paused()
                .map_err(|_| "update_write_freeze_failed")?;
            control
                .set_paused(true)
                .map_err(|_| "update_write_freeze_failed")?;
            let _ = app_for_install.emit("wakegpt://codex-integration-paused", true);
            let result = (|| {
                if !app_for_install
                    .state::<crate::codex_adapter::CodexIntegrationRuntime>()
                    .wait_until_paused(Duration::from_secs(10))
                {
                    return Err("update_codex_pause_timeout");
                }
                let _sync_guard = sync
                    .freeze_for_update()
                    .map_err(|_| "update_write_freeze_failed")?;
                let _attachment_guard = attachments
                    .freeze_for_update()
                    .map_err(|_| "update_write_freeze_failed")?;
                let mut store_guard = store
                    .freeze_for_update()
                    .map_err(|_| "update_write_freeze_failed")?;
                let reset =
                    crate::local_data_reset::platform_preview(&app_for_install.config().identifier)
                        .map_err(|_| "update_local_data_reset_pending")?;
                if reset.reset_scheduled {
                    return Err("update_local_data_reset_pending");
                }
                runtime
                    .finish_restarting()
                    .map_err(|_| "update_state_unavailable")?;
                update_install::install_prepared(
                    &app_data_dir,
                    &store,
                    &mut store_guard,
                    &receipt,
                    &update,
                    &bytes,
                )
                .map_err(|error| error.0)?;
                app_for_install.exit(0);
                thread::sleep(Duration::from_millis(500));
                std::process::exit(0);
            })();
            if result.is_err() {
                let _ = control.set_paused(previous_paused);
                let _ = app_for_install.emit("wakegpt://codex-integration-paused", previous_paused);
            }
            result
        })
        .await;
    let install_result = match install_worker {
        Ok(result) => result,
        Err(_) => {
            match update_install::recover_after_install_worker_failure(
                &worker_app_data_dir,
                &worker_transaction_id,
            ) {
                Ok(false) => {
                    runtime.fail_delivery("update_install_worker_failed")?;
                    return Err(CommandError::unchanged(
                        "update_install_worker_failed",
                        "更新安装线程意外停止；当前版本没有改变，可以重试",
                        true,
                    ));
                }
                Ok(true) | Err(_) => {
                    app.exit(1);
                    thread::sleep(Duration::from_millis(500));
                    std::process::exit(1);
                }
            }
        }
    };
    if let Err(code) = install_result {
        runtime.fail_delivery(code)?;
        return Err(CommandError::unchanged(
            code,
            "安装前安全检查未通过；当前版本和本地数据没有改变",
            true,
        ));
    }
    current_status(&app, &store, &runtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_test_public_key() -> String {
        BASE64_STANDARD.encode(
            "untrusted comment: minisign public key fixture\n\
             RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n",
        )
    }

    fn encoded_test_signature() -> String {
        BASE64_STANDARD.encode(
            "untrusted comment: signature from minisign secret key\n\
             RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
             trusted comment: timestamp:1556193335\tfile:test\n\
             y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==",
        )
    }

    fn settings() -> UpdateSettings {
        UpdateSettings {
            external_network_enabled: true,
            automatic_checks_enabled: true,
            automatic_downloads_enabled: false,
            check_interval_hours: 24,
            skipped_version: None,
            last_checked_at_ms: None,
            last_error_code: None,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn release_configuration_is_all_or_nothing_and_https_only() {
        let public_key = encoded_test_public_key();
        assert_eq!(
            validate_release_config(None, None),
            ReleaseConfigState::Unconfigured
        );
        assert_eq!(
            validate_release_config(Some(&public_key), None),
            ReleaseConfigState::Invalid
        );
        assert_eq!(
            validate_release_config(
                Some(&public_key),
                Some("http://updates.example.test/latest.json"),
            ),
            ReleaseConfigState::Invalid
        );
        assert_eq!(
            validate_release_config(
                Some("not a minisign public key"),
                Some("https://updates.example.test/latest.json"),
            ),
            ReleaseConfigState::Invalid
        );
        assert!(matches!(
            validate_release_config(
                Some(&public_key),
                Some("https://updates.example.test/{{target}}/{{current_version}}.json"),
            ),
            ReleaseConfigState::Configured(_)
        ));
    }

    #[test]
    fn tauri_outer_base64_signature_stream_verifies_before_ready_state() {
        let public_key = decode_public_key(&encoded_test_public_key()).unwrap();
        let signature = decode_signature(&encoded_test_signature()).unwrap();
        let mut verifier = public_key.verify_stream(&signature).unwrap();
        verifier.update(b"te");
        verifier.update(b"st");
        verifier.finalize().unwrap();

        let mut tampered = public_key.verify_stream(&signature).unwrap();
        tampered.update(b"changed");
        assert!(tampered.finalize().is_err());
    }

    #[test]
    fn download_redirects_are_https_and_limited_to_github_asset_hosts() {
        let initial = "https://github.com/wakegpt/wakegpt/releases/download/v1.2.3/WakeGPT_universal.app.tar.gz";
        assert!(allowed_download_url(initial, &initial.parse().unwrap()));
        assert!(allowed_download_url(
            initial,
            &"https://release-assets.githubusercontent.com/github-production-release-asset/file?sig=test"
                .parse()
                .unwrap(),
        ));
        let credentialed = ["https://user", "release-assets.githubusercontent.com/file"].join("@");
        for rejected in [
            "http://release-assets.githubusercontent.com/file",
            "https://example.test/file",
            credentialed.as_str(),
        ] {
            assert!(!allowed_download_url(initial, &rejected.parse().unwrap()));
        }
    }

    #[test]
    fn automatic_checks_are_disabled_and_rate_bounded() {
        let now = 1_000_000_000_i64;
        let mut value = settings();
        assert!(automatic_check_due(&value, now));
        value.automatic_checks_enabled = false;
        assert!(!automatic_check_due(&value, now));
        value.automatic_checks_enabled = true;
        value.last_checked_at_ms = Some(now - 23 * 60 * 60 * 1_000);
        assert!(!automatic_check_due(&value, now));
        value.last_checked_at_ms = Some(now - 24 * 60 * 60 * 1_000);
        assert!(automatic_check_due(&value, now));
        value.last_checked_at_ms = Some(now + 1);
        assert!(automatic_check_due(&value, now));
    }

    #[test]
    fn update_assets_require_an_exact_immutable_github_release() {
        assert!(immutable_github_asset_url(
            "1.2.3",
            "https://github.com/wakegpt/wakegpt/releases/download/v1.2.3/WakeGPT_1.2.3_universal.app.tar.gz",
        ));
        for rejected in [
            "http://github.com/wakegpt/wakegpt/releases/download/v1.2.3/WakeGPT_1.2.3_universal.app.tar.gz",
            "https://github.com/wakegpt/wakegpt/releases/download/1.2.3/WakeGPT_1.2.3_universal.app.tar.gz",
            "https://github.com/wakegpt/wakegpt/releases/latest/download/WakeGPT_1.2.3_universal.app.tar.gz",
            "https://github.com/wakegpt/wakegpt/releases/download/v1.2.4/WakeGPT_1.2.3_universal.app.tar.gz",
            "https://example.test/wakegpt/releases/download/v1.2.3/WakeGPT_1.2.3_universal.app.tar.gz",
            "https://github.com/wakegpt/wakegpt/releases/download/v1.2.3/Other.app.tar.gz",
            "https://github.com/wakegpt/wakegpt/releases/download/v1.2.3/WakeGPT.app.tar.gz?token=secret",
        ] {
            assert!(!immutable_github_asset_url("1.2.3", rejected), "{rejected}");
        }
    }

    #[test]
    fn status_never_exposes_the_endpoint_or_public_key() {
        let config = ReleaseConfigState::Configured(ReleaseUpdateConfig {
            public_key: "A".repeat(44),
            endpoint: "https://updates.example.test/latest.json".to_owned(),
        });
        let status = update_status_from_parts(settings(), RuntimeSnapshot::default(), config);
        let json = serde_json::to_string(&status).unwrap();
        assert!(status.configured);
        assert!(!json.contains("updates.example.test"));
        assert!(!json.contains(&"A".repeat(44)));
    }
}
