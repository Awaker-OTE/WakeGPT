use crate::api::{CommandError, CommandResult};
use crate::attachments::{
    AttachmentCoordinator, AttachmentError, PendingAttachment, MAX_ATTACHMENT_BYTES,
};
use crate::data_export::DataExportResult;
use crate::diagnostics::{
    LocalDiagnostics, LocalDiagnosticsError, LocalDiagnosticsExportResult, LocalDiagnosticsPage,
    LocalDiagnosticsStatus,
};
use crate::domain::{
    AttachmentRelocationState, MarkdownLayout, Notebook, NotebookTargetState, NumberingStyle,
    ProductSettings, Record, SubmitShortcut, ThemePreference, UiPreferences, Workspace,
    WorkspaceOpenPreference, INBOX_ATTACHMENT_DIRECTORY,
};
use crate::file_sync::{
    sha256_hex, CoordinatedMutationError, NotebookAttachmentDirectoryChange,
    NotebookAttachmentDirectoryPreview, NotebookConflictInspection,
    NotebookConflictResolutionAction, NotebookDocument, NotebookNumberingConfiguration,
    NotebookNumberingPreview, RecoveryReport, RecoveryState, SyncCoordinator, SyncError,
};
use crate::lifecycle::IntegrationControl;
use crate::local_data_reset::LocalDataResetNotice;
use crate::managed_markdown::synchronize_notebook_for_path;
use crate::storage::{
    DraftAttachment, DraftRecordCreate, RecordAttachmentSetRevision, Store,
    QUICK_CAPTURE_DRAFT_SURFACE, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub app_version: &'static str,
    pub schema_version: u32,
    pub recovered_operations: usize,
    pub initialized_notebooks: usize,
    pub pending_recovery_operations: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginItemStatus {
    pub enabled: bool,
}

const LOCAL_DATA_RESET_CONFIRMATION: &str = "清除 WakeGPT";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDataResetPreview {
    pub platform_supported: bool,
    pub can_reset: bool,
    pub blocker_code: Option<&'static str>,
    pub confirmation_phrase: &'static str,
    pub app_owned_item_count: u32,
    pub workspace_count: u64,
    pub notebook_count: u64,
    pub record_count: u64,
    pub attachment_count: u64,
    pub draft_count: u64,
    pub draft_attachment_count: u64,
    pub composer_receipt_count: u64,
    pub diagnostic_event_count: u64,
    pub default_identity_profile_present: bool,
    pub pending_recovery_operations: usize,
    pub preserves_workspace_files: bool,
    pub reset_scheduled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDataResetRequest {
    pub confirmation_phrase: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownEditorQuitStateRequest {
    pub open: bool,
    pub dirty: bool,
    pub saving: bool,
    pub revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuitRequest {
    pub request_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcknowledgeLocalDataResetNoticeRequest {
    pub occurred_at_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDataResetScheduleResult {
    pub restart_requested: bool,
    pub already_scheduled: bool,
    pub cancelled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalDiagnosticsPageRequest {
    pub before_id: Option<i64>,
    pub limit: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLocalDiagnosticsSettingsRequest {
    pub enabled: bool,
    pub retention_days: u32,
    pub max_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProductSettingsRequest {
    pub record_trash_retention_days: Option<u32>,
    pub notebook_scan_ignore_directories: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordTrashCleanupPreview {
    pub retention_days: Option<u32>,
    pub cutoff_at_ms: Option<i64>,
    pub eligible_record_count: u64,
    pub eligible_attachment_count: u64,
    pub blocked_record_count: u64,
    pub preview_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeRecordTrashRequest {
    pub preview_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordTrashCleanupResult {
    pub deleted_record_count: u64,
    pub deleted_attachment_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexIntegrationStatus {
    pub paused: bool,
    pub adapter_version: &'static str,
    pub supported_codex_version: &'static str,
    pub supported_codex_build: &'static str,
    pub endpoint_state: crate::codex_adapter::AdapterEndpointState,
    pub last_error_code: Option<&'static str>,
    pub card_script_sha256: String,
    pub detected_instance_count: usize,
    pub connectable_instance_count: usize,
    pub available_target_count: usize,
    pub connected_target_count: usize,
    pub failed_target_count: usize,
    pub instances: Vec<crate::codex_adapter::AdapterInstanceProbe>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultIdentityStatus {
    pub configured: bool,
    pub alias: Option<String>,
    pub locked: bool,
    pub platform_supported: bool,
    pub profile_present: bool,
    pub runtime_state: crate::codex_adapter::DefaultIdentityRuntimeState,
    pub last_error_code: Option<&'static str>,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCodexIntegrationPausedRequest {
    pub paused: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoginItemEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartCodexInstanceRequest {
    pub instance_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureDefaultIdentityRequest {
    pub alias: String,
    pub locked: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeWorkspaceRequest {
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRequest {
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNotebookRequest {
    pub workspace_id: String,
    pub display_name: String,
    pub relative_path: String,
    pub numbering_style: NumberingStyle,
    #[serde(default = "crate::domain::default_numbering_start")]
    pub numbering_start: u32,
    pub attachment_directory: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindExistingNotebookRequest {
    pub workspace_id: String,
    pub display_name: Option<String>,
    pub numbering_style: NumberingStyle,
    #[serde(default = "crate::domain::default_numbering_start")]
    pub numbering_start: u32,
    pub attachment_directory: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookDocumentRequest {
    pub workspace_id: String,
    pub notebook_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveNotebookConflictRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub conflict_token: String,
    pub action: NotebookConflictResolutionAction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveNotebookDocumentRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
    pub markdown: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewNotebookNumberingRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub numbering_style: NumberingStyle,
    #[serde(default = "crate::domain::default_numbering_start")]
    pub numbering_start: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeNotebookNumberingRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub numbering_style: NumberingStyle,
    #[serde(default = "crate::domain::default_numbering_start")]
    pub numbering_start: u32,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewNotebookAttachmentDirectoryRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub attachment_directory: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeNotebookAttachmentDirectoryRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub attachment_directory: String,
    pub expected_file_sha256: String,
    pub expected_receipt_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRecordRequest {
    pub mutation_id: String,
    pub mutation_schema_version: u32,
    pub workspace_id: String,
    pub notebook_id: Option<String>,
    pub body_markdown: String,
    #[serde(default)]
    pub attachment_tokens: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRecordRequest {
    pub mutation_id: String,
    pub mutation_schema_version: u32,
    pub workspace_id: String,
    pub record_id: String,
    pub expected_revision: u64,
    pub body_markdown: String,
    #[serde(default)]
    pub retained_attachment_ids: Option<Vec<String>>,
    #[serde(default)]
    pub new_attachment_tokens: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordRevisionRequest {
    pub mutation_id: String,
    pub mutation_schema_version: u32,
    pub workspace_id: String,
    pub record_id: String,
    pub expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrateRecordRequest {
    pub mutation_id: String,
    pub mutation_schema_version: u32,
    pub workspace_id: String,
    pub record_id: String,
    pub expected_revision: u64,
    pub destination_notebook_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinRecordRequest {
    pub workspace_id: String,
    pub record_id: String,
    pub pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSelectionRequest {
    pub workspace_id: String,
    pub notebook_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivateWorkspaceRequest {
    pub workspace_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemePreferenceRequest {
    pub theme: ThemePreference,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitShortcutRequest {
    pub submit_shortcut: SubmitShortcut,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownLayoutRequest {
    pub markdown_layout: MarkdownLayout,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftRequest {
    pub workspace_id: String,
    pub notebook_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDraftRequest {
    pub workspace_id: String,
    pub notebook_id: Option<String>,
    pub body_markdown: String,
    #[serde(default)]
    pub attachments: Vec<PendingAttachment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordImagePreviewRequest {
    pub workspace_id: String,
    pub record_id: String,
    pub attachment_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownImagePreviewRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub notebook_target_id: String,
    pub relative_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftState {
    pub body_markdown: String,
    pub attachments: Vec<PendingAttachment>,
}

#[tauri::command]
pub async fn export_local_data(app: AppHandle) -> CommandResult<Option<DataExportResult>> {
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("选择 WakeGPT 本地数据导出位置")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let parent = selected.into_path().map_err(|_| {
        CommandError::unchanged(
            "data_export_destination_invalid",
            "所选导出位置无效；WakeGPT 未创建导出",
            false,
        )
    })?;
    let app_for_export = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let store = app_for_export.state::<Store>();
        let attachments = app_for_export.state::<AttachmentCoordinator>();
        crate::data_export::export_to_parent(&store, &attachments, &parent)
    })
    .await
    .map_err(|_| {
        CommandError::unchanged(
            "data_export_worker_failed",
            "导出任务意外停止；WakeGPT 未修改现有数据",
            true,
        )
    })?
    .map_err(|error| CommandError::unchanged(error.code(), error.message(), error.retryable()))?;
    Ok(Some(result))
}

fn build_local_data_reset_preview(
    app: &AppHandle,
    store: &Store,
    recovery: &RecoveryState,
    diagnostics: &LocalDiagnostics,
    profile: &crate::codex_adapter::DefaultIdentityProfile,
) -> CommandResult<LocalDataResetPreview> {
    let inventory = store
        .local_data_reset_inventory()
        .map_err(CommandError::from)?;
    let recovery = recovery.snapshot().map_err(|error| {
        CommandError::unchanged(error.code(), "无法核对待恢复操作；本机数据没有改变", true)
    })?;
    let pending_recovery_operations = recovery.pending.len()
        + recovery.pending_notebooks.len()
        + recovery.pending_attachment_operations;
    let diagnostics = diagnostics.status();
    let identity = crate::codex_adapter::default_identity_runtime_probe(profile);
    let platform =
        crate::local_data_reset::platform_preview(&app.config().identifier).map_err(|error| {
            CommandError::unchanged(error.code(), error.message(), error.retryable())
        })?;
    let blocker_code = if !platform.platform_supported {
        Some("platformUnsupported")
    } else if platform.reset_scheduled {
        Some("restartPending")
    } else if pending_recovery_operations > 0 {
        Some("recoveryPending")
    } else {
        match identity.state {
            crate::codex_adapter::DefaultIdentityRuntimeState::Stopped => None,
            crate::codex_adapter::DefaultIdentityRuntimeState::Running => {
                Some("defaultIdentityRunning")
            }
            crate::codex_adapter::DefaultIdentityRuntimeState::Occupied => {
                Some("defaultIdentityOccupied")
            }
            crate::codex_adapter::DefaultIdentityRuntimeState::Unavailable => {
                Some("defaultIdentityUnavailable")
            }
        }
    };
    Ok(LocalDataResetPreview {
        platform_supported: platform.platform_supported,
        can_reset: blocker_code.is_none(),
        blocker_code,
        confirmation_phrase: LOCAL_DATA_RESET_CONFIRMATION,
        app_owned_item_count: platform.app_owned_item_count,
        workspace_count: inventory.workspace_count,
        notebook_count: inventory.notebook_count,
        record_count: inventory.record_count,
        attachment_count: inventory.attachment_count,
        draft_count: inventory.draft_count,
        draft_attachment_count: inventory.draft_attachment_count,
        composer_receipt_count: inventory.composer_receipt_count,
        diagnostic_event_count: diagnostics.event_count,
        default_identity_profile_present: identity.profile_present,
        pending_recovery_operations,
        preserves_workspace_files: true,
        reset_scheduled: platform.reset_scheduled,
    })
}

#[tauri::command]
pub fn local_data_reset_preview(
    app: AppHandle,
    store: State<'_, Store>,
    recovery: State<'_, RecoveryState>,
    diagnostics: State<'_, LocalDiagnostics>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
) -> CommandResult<LocalDataResetPreview> {
    build_local_data_reset_preview(&app, &store, &recovery, &diagnostics, &profile)
}

#[tauri::command]
pub fn local_data_reset_notice(app: AppHandle) -> CommandResult<Option<LocalDataResetNotice>> {
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "local_data_reset_path_invalid",
            "无法验证 WakeGPT 本机数据目录",
            false,
        )
    })?;
    crate::local_data_reset::read_notice(&app_data_dir)
        .map_err(|error| CommandError::unchanged(error.code(), error.message(), error.retryable()))
}

#[tauri::command]
pub fn acknowledge_local_data_reset_notice(
    app: AppHandle,
    request: AcknowledgeLocalDataResetNoticeRequest,
) -> CommandResult<bool> {
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "local_data_reset_path_invalid",
            "无法验证 WakeGPT 本机数据目录",
            false,
        )
    })?;
    crate::local_data_reset::acknowledge_notice(&app_data_dir, request.occurred_at_ms)
        .map_err(|error| CommandError::unchanged(error.code(), error.message(), error.retryable()))
}

#[tauri::command]
pub fn reset_local_data(
    app: AppHandle,
    store: State<'_, Store>,
    recovery: State<'_, RecoveryState>,
    diagnostics: State<'_, LocalDiagnostics>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
    control: State<'_, IntegrationControl>,
    request: LocalDataResetRequest,
) -> CommandResult<LocalDataResetScheduleResult> {
    if request.confirmation_phrase != LOCAL_DATA_RESET_CONFIRMATION {
        return Err(CommandError::unchanged(
            "local_data_reset_confirmation_invalid",
            "确认文字不匹配；WakeGPT 没有清除任何数据",
            false,
        ));
    }
    let app_data_dir = app.path().app_data_dir().map_err(|_| {
        CommandError::unchanged(
            "local_data_reset_path_invalid",
            "无法验证 WakeGPT 本机数据目录",
            false,
        )
    })?;
    let update_installing =
        crate::update_install::install_in_progress(&app_data_dir).map_err(|error| {
            CommandError::unchanged(error.0, "无法确认应用更新未在进行；本机数据没有改变", false)
        })?;
    let update_prepared = crate::update_install::load_prepared(&app_data_dir)
        .map_err(|error| {
            CommandError::unchanged(error.0, "无法验证已下载更新；本机数据没有改变", false)
        })?
        .is_some();
    if update_installing || update_prepared {
        return Err(CommandError::unchanged(
            "local_data_reset_update_pending",
            "请先完成或移除已下载的应用更新，再清除本机数据",
            false,
        ));
    }
    let preview = build_local_data_reset_preview(&app, &store, &recovery, &diagnostics, &profile)?;
    if let Some(blocker) = preview.blocker_code {
        let (code, message, retryable) = match blocker {
            "restartPending" => (
                "local_data_reset_restart_pending",
                "本机数据清除已安排；请等待 WakeGPT 重启",
                false,
            ),
            "recoveryPending" => (
                "local_data_reset_recovery_pending",
                "请先完成全部待恢复同步，再清除本机数据",
                true,
            ),
            "defaultIdentityRunning" | "defaultIdentityOccupied" => (
                "local_data_reset_default_identity_running",
                "请先关闭 WakeGPT 默认身份窗口，再清除本机数据",
                true,
            ),
            "defaultIdentityUnavailable" => (
                "local_data_reset_default_identity_unavailable",
                "无法确认默认身份 profile 未被占用；本机数据没有改变",
                true,
            ),
            _ => (
                "local_data_reset_platform_unsupported",
                "当前平台尚未提供经过验证的本机数据清除流程",
                false,
            ),
        };
        return Err(CommandError::unchanged(code, message, retryable));
    }

    let native_confirmed = app
        .dialog()
        .message(format!(
            "将 {} 条记录、{} 份草稿、诊断、默认身份 profile 和 WakeGPT 自有缓存整体移入 macOS 废纸篓，然后重启应用。\n\n工作区 Markdown、工作区附件、外部导出包和系统备份不会改变。",
            preview.record_count, preview.draft_count
        ))
        .title("确认清除 WakeGPT 本机数据")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "移到废纸篓并重启".to_owned(),
            "取消".to_owned(),
        ))
        .blocking_show();
    if !native_confirmed {
        return Ok(LocalDataResetScheduleResult {
            restart_requested: false,
            already_scheduled: false,
            cancelled: true,
        });
    }

    let previous_login_item = read_login_item_status(&app)?;
    if previous_login_item.enabled {
        app.autolaunch().disable().map_err(|_| {
            CommandError::unchanged(
                "local_data_reset_login_item_disable_failed",
                "无法先关闭登录时启动；本机数据没有改变",
                true,
            )
        })?;
        let disabled_status = match read_login_item_status(&app) {
            Ok(status) => status,
            Err(error) => {
                let _ = app.autolaunch().enable();
                return Err(error);
            }
        };
        if disabled_status.enabled {
            return Err(CommandError::unchanged(
                "local_data_reset_login_item_disable_failed",
                "系统没有确认登录时启动已关闭；本机数据没有改变",
                true,
            ));
        }
    }

    let previous_paused = control
        .is_paused()
        .map_err(|_| CommandError::unchanged("storage_busy", "速记卡状态暂时不可用", true))?;
    if let Err(error) = control.set_paused(true) {
        if previous_login_item.enabled {
            let _ = app.autolaunch().enable();
        }
        return Err(CommandError::unchanged(
            "local_data_reset_pause_failed",
            error,
            true,
        ));
    }
    if app
        .emit("wakegpt://codex-integration-paused", true)
        .is_err()
    {
        let _ = control.set_paused(previous_paused);
        if previous_login_item.enabled {
            let _ = app.autolaunch().enable();
        }
        return Err(CommandError::unchanged(
            "local_data_reset_pause_failed",
            "无法先暂停 ChatGPT 速记卡；本机数据没有改变",
            true,
        ));
    }

    let schedule = match crate::local_data_reset::schedule(&app.config().identifier) {
        Ok(schedule) => schedule,
        Err(error) => {
            let _ = control.set_paused(previous_paused);
            let _ = app.emit("wakegpt://codex-integration-paused", previous_paused);
            if previous_login_item.enabled {
                let _ = app.autolaunch().enable();
            }
            return Err(CommandError::unchanged(
                error.code(),
                error.message(),
                error.retryable(),
            ));
        }
    };

    let restart_app = app.clone();
    let restart_requested = std::thread::Builder::new()
        .name("wakegpt-local-data-reset".to_owned())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            restart_app.request_restart();
        })
        .is_ok();
    Ok(LocalDataResetScheduleResult {
        restart_requested,
        already_scheduled: schedule.already_scheduled,
        cancelled: false,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderNotebooksRequest {
    pub workspace_id: String,
    pub ordered_notebook_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookStateRequest {
    pub workspace_id: String,
    pub notebook_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookPinRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameNotebookRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWorkspaceOpenPreferenceRequest {
    pub workspace_id: String,
    pub use_last_selection: bool,
    pub default_notebook_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewManagedMarkdownRequest {
    pub workspace_id: String,
    pub notebook_id: String,
    pub existing_markdown: String,
}

#[tauri::command]
pub fn app_status(
    store: State<'_, Store>,
    recovery: State<'_, RecoveryState>,
) -> CommandResult<AppStatus> {
    let actual_schema = store.schema_version().map_err(CommandError::from)?;
    if actual_schema != SCHEMA_VERSION {
        return Err(CommandError::storage(crate::storage::StoreError::Conflict(
            format!("database schema mismatch: expected {SCHEMA_VERSION}, got {actual_schema}"),
        )));
    }
    let recovery = recovery.snapshot().map_err(|error| {
        CommandError::coordinated(crate::file_sync::CoordinatedMutationError::Coordination(
            error,
        ))
    })?;
    Ok(AppStatus {
        app_version: env!("CARGO_PKG_VERSION"),
        schema_version: actual_schema,
        recovered_operations: recovery.recovered_operations
            + recovery.recovered_attachment_operations,
        initialized_notebooks: recovery.initialized_notebooks,
        pending_recovery_operations: recovery.pending.len()
            + recovery.pending_notebooks.len()
            + recovery.pending_attachment_operations,
    })
}

#[tauri::command]
pub fn product_settings(store: State<'_, Store>) -> CommandResult<ProductSettings> {
    store.product_settings().map_err(CommandError::from)
}

#[tauri::command]
pub fn set_product_settings(
    store: State<'_, Store>,
    request: SetProductSettingsRequest,
) -> CommandResult<ProductSettings> {
    store
        .set_product_settings(
            request.record_trash_retention_days,
            &request.notebook_scan_ignore_directories,
        )
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn preview_record_trash_cleanup(
    store: State<'_, Store>,
) -> CommandResult<RecordTrashCleanupPreview> {
    let preview = store
        .preview_record_trash_cleanup()
        .map_err(CommandError::from)?;
    let eligible_attachment_count = preview
        .eligible
        .iter()
        .try_fold(0_u64, |total, candidate| {
            total.checked_add(candidate.attachment_count)
        })
        .ok_or_else(|| {
            CommandError::unchanged(
                "trash_cleanup_inventory_invalid",
                "废纸篓清理预览数量异常；没有删除任何数据",
                false,
            )
        })?;
    Ok(RecordTrashCleanupPreview {
        retention_days: preview.retention_days,
        cutoff_at_ms: preview.cutoff_at_ms,
        eligible_record_count: u64::try_from(preview.eligible.len()).map_err(|_| {
            CommandError::unchanged(
                "trash_cleanup_inventory_invalid",
                "废纸篓清理预览数量异常；没有删除任何数据",
                false,
            )
        })?,
        eligible_attachment_count,
        blocked_record_count: preview.blocked_count,
        preview_token: preview.preview_token,
    })
}

#[tauri::command]
pub fn purge_record_trash(
    app: AppHandle,
    store: State<'_, Store>,
    request: PurgeRecordTrashRequest,
) -> CommandResult<RecordTrashCleanupResult> {
    let result = store
        .purge_record_trash(&request.preview_token)
        .map_err(CommandError::from)?;
    crate::codex_adapter::request_card_refresh(&app);
    Ok(RecordTrashCleanupResult {
        deleted_record_count: result.deleted_record_count,
        deleted_attachment_count: result.deleted_attachment_count,
    })
}

fn local_diagnostics_command_error(error: LocalDiagnosticsError) -> CommandError {
    CommandError::unchanged(error.code(), error.message(), error.retryable())
}

#[tauri::command]
pub fn local_diagnostics_status(
    diagnostics: State<'_, LocalDiagnostics>,
) -> CommandResult<LocalDiagnosticsStatus> {
    Ok(diagnostics.status())
}

#[tauri::command]
pub fn list_local_diagnostics(
    diagnostics: State<'_, LocalDiagnostics>,
    request: LocalDiagnosticsPageRequest,
) -> CommandResult<LocalDiagnosticsPage> {
    diagnostics
        .list(request.before_id, request.limit)
        .map_err(local_diagnostics_command_error)
}

#[tauri::command]
pub fn update_local_diagnostics_settings(
    diagnostics: State<'_, LocalDiagnostics>,
    request: UpdateLocalDiagnosticsSettingsRequest,
) -> CommandResult<LocalDiagnosticsStatus> {
    diagnostics
        .update_settings(request.enabled, request.retention_days, request.max_bytes)
        .map_err(local_diagnostics_command_error)
}

#[tauri::command]
pub async fn export_local_diagnostics(
    app: AppHandle,
) -> CommandResult<Option<LocalDiagnosticsExportResult>> {
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("选择 WakeGPT 脱敏诊断导出位置")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let parent = selected
        .into_path()
        .map_err(|_| local_diagnostics_command_error(LocalDiagnosticsError::InvalidDestination))?;
    let app_for_export = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        app_for_export
            .state::<LocalDiagnostics>()
            .export_to_parent(&parent)
    })
    .await
    .map_err(|_| local_diagnostics_command_error(LocalDiagnosticsError::WriteFailed))?
    .map(Some)
    .map_err(local_diagnostics_command_error)
}

#[tauri::command]
pub async fn clear_local_diagnostics(app: AppHandle) -> CommandResult<LocalDiagnosticsStatus> {
    let app_for_clear = app.clone();
    tauri::async_runtime::spawn_blocking(move || app_for_clear.state::<LocalDiagnostics>().clear())
        .await
        .map_err(|_| local_diagnostics_command_error(LocalDiagnosticsError::Unavailable))?
        .map_err(local_diagnostics_command_error)
}

fn read_login_item_status(app: &AppHandle) -> CommandResult<LoginItemStatus> {
    app.autolaunch()
        .is_enabled()
        .map(|enabled| LoginItemStatus { enabled })
        .map_err(|_| {
            CommandError::unchanged(
                "login_item_unavailable",
                "无法读取系统登录启动设置；WakeGPT 未改变现有登录项",
                true,
            )
        })
}

#[tauri::command]
pub fn login_item_status(app: AppHandle) -> CommandResult<LoginItemStatus> {
    read_login_item_status(&app)
}

#[tauri::command]
pub fn set_login_item_enabled(
    app: AppHandle,
    request: SetLoginItemEnabledRequest,
) -> CommandResult<LoginItemStatus> {
    let manager = app.autolaunch();
    let update = if request.enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    update.map_err(|_| {
        CommandError::unchanged(
            "login_item_update_failed",
            "无法修改系统登录启动设置；原设置保持不变",
            true,
        )
    })?;

    let status = read_login_item_status(&app)?;
    if status.enabled != request.enabled {
        return Err(CommandError::unchanged(
            "login_item_verification_failed",
            "系统没有确认登录启动设置；请重试",
            true,
        ));
    }
    Ok(status)
}

#[tauri::command]
pub fn retry_pending_recovery(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    recovery: State<'_, RecoveryState>,
    diagnostics: State<'_, LocalDiagnostics>,
) -> CommandResult<RecoveryReport> {
    let result = (|| {
        store
            .queue_attachment_relocations_for_configured_directories()
            .map_err(CommandError::from)?;
        let attachment_prepare_report = attachments
            .prepare_pending_relocations(&store)
            .map_err(|error| CommandError::unchanged(error.code(), "附件迁移暂时不可用", true))?;
        let mut report = sync.replay_pending(&store).map_err(|error| {
            CommandError::coordinated(crate::file_sync::CoordinatedMutationError::Coordination(
                error,
            ))
        })?;
        let attachment_lifecycle_report = attachments.replay_pending(&store).map_err(|error| {
            CommandError::unchanged(error.code(), "附件恢复队列暂时不可用", true)
        })?;
        let attachment_finish_report = attachments
            .finish_pending_relocations(&store)
            .map_err(|error| CommandError::unchanged(error.code(), "附件清理暂时不可用", true))?;
        report.recovered_attachment_operations = attachment_lifecycle_report.recovered_operations
            + attachment_prepare_report.recovered_operations
            + attachment_finish_report.recovered_operations;
        report.pending_attachment_operations = attachment_lifecycle_report.pending.len()
            + attachment_prepare_report.pending.len()
            + attachment_finish_report.pending.len();
        recovery.replace(report).map_err(|error| {
            CommandError::coordinated(crate::file_sync::CoordinatedMutationError::Coordination(
                error,
            ))
        })
    })();
    match &result {
        Ok(report) => diagnostics.record_recovery(
            report.recovered_operations + report.recovered_attachment_operations,
            report.pending.len()
                + report.pending_notebooks.len()
                + report.pending_attachment_operations,
            None,
        ),
        Err(error) => {
            let pending = recovery
                .snapshot()
                .ok()
                .map(|report| {
                    report.pending.len()
                        + report.pending_notebooks.len()
                        + report.pending_attachment_operations
                })
                .unwrap_or(0);
            diagnostics.record_recovery(0, pending, Some(error.code()));
        }
    }
    result
}

#[tauri::command]
pub fn codex_integration_status(
    control: State<'_, IntegrationControl>,
    runtime: State<'_, crate::codex_adapter::CodexIntegrationRuntime>,
) -> CommandResult<CodexIntegrationStatus> {
    let probe = crate::codex_adapter::runtime_probe(&runtime);
    Ok(CodexIntegrationStatus {
        paused: control
            .is_paused()
            .map_err(|_| CommandError::storage(crate::storage::StoreError::LockPoisoned))?,
        adapter_version: probe.adapter_version,
        supported_codex_version: probe.supported_codex_version,
        supported_codex_build: probe.supported_codex_build,
        endpoint_state: probe.endpoint_state,
        last_error_code: probe.last_error_code,
        card_script_sha256: probe.card_script_sha256,
        detected_instance_count: probe.detected_instance_count,
        connectable_instance_count: probe.connectable_instance_count,
        available_target_count: probe.available_target_count,
        connected_target_count: probe.connected_target_count,
        failed_target_count: probe.failed_target_count,
        instances: probe.instances,
    })
}

#[tauri::command]
pub fn set_codex_integration_paused(
    app: AppHandle,
    store: State<'_, Store>,
    control: State<'_, IntegrationControl>,
    runtime: State<'_, crate::codex_adapter::CodexIntegrationRuntime>,
    request: SetCodexIntegrationPausedRequest,
) -> CommandResult<CodexIntegrationStatus> {
    let previous = control
        .is_paused()
        .map_err(|_| CommandError::storage(crate::storage::StoreError::LockPoisoned))?;
    store
        .set_codex_integration_paused(request.paused)
        .map_err(CommandError::from)?;
    let paused = match control.set_paused(request.paused) {
        Ok(paused) => paused,
        Err(_) => {
            let _ = store.set_codex_integration_paused(previous);
            return Err(CommandError::storage(
                crate::storage::StoreError::LockPoisoned,
            ));
        }
    };
    app.emit("wakegpt://codex-integration-paused", request.paused)
        .map_err(|_| {
            let _ = control.set_paused(previous);
            let _ = store.set_codex_integration_paused(previous);
            CommandError::storage(crate::storage::StoreError::LockPoisoned)
        })?;
    let probe = crate::codex_adapter::runtime_probe(&runtime);
    Ok(CodexIntegrationStatus {
        paused,
        adapter_version: probe.adapter_version,
        supported_codex_version: probe.supported_codex_version,
        supported_codex_build: probe.supported_codex_build,
        endpoint_state: probe.endpoint_state,
        last_error_code: probe.last_error_code,
        card_script_sha256: probe.card_script_sha256,
        detected_instance_count: probe.detected_instance_count,
        connectable_instance_count: probe.connectable_instance_count,
        available_target_count: probe.available_target_count,
        connected_target_count: probe.connected_target_count,
        failed_target_count: probe.failed_target_count,
        instances: probe.instances,
    })
}

#[tauri::command]
pub async fn restart_codex_integration(
    store: State<'_, Store>,
    control: State<'_, IntegrationControl>,
    runtime: State<'_, crate::codex_adapter::CodexIntegrationRuntime>,
) -> CommandResult<CodexIntegrationStatus> {
    store
        .set_codex_integration_paused(false)
        .map_err(CommandError::from)?;
    control.set_paused(false).map_err(|_| {
        CommandError::unchanged(
            "integration_state_unavailable",
            "ChatGPT 速记卡状态暂时不可用",
            true,
        )
    })?;
    let restart_result =
        tauri::async_runtime::spawn_blocking(crate::codex_adapter::restart_codex_with_integration)
            .await
            .map_err(|_| {
                runtime.record_restart_failure("codex_restart_task_failed");
                CommandError::unchanged("codex_restart_task_failed", "ChatGPT 重启任务未完成", true)
            })?;
    if let Err(error) = restart_result {
        runtime.record_restart_failure(error.code());
        return Err(CommandError::unchanged(
            error.code(),
            "无法以 WakeGPT 集成模式重启 ChatGPT",
            true,
        ));
    }
    std::thread::sleep(std::time::Duration::from_millis(900));
    let probe = crate::codex_adapter::runtime_probe(&runtime);
    Ok(CodexIntegrationStatus {
        paused: false,
        adapter_version: probe.adapter_version,
        supported_codex_version: probe.supported_codex_version,
        supported_codex_build: probe.supported_codex_build,
        endpoint_state: probe.endpoint_state,
        last_error_code: probe.last_error_code,
        card_script_sha256: probe.card_script_sha256,
        detected_instance_count: probe.detected_instance_count,
        connectable_instance_count: probe.connectable_instance_count,
        available_target_count: probe.available_target_count,
        connected_target_count: probe.connected_target_count,
        failed_target_count: probe.failed_target_count,
        instances: probe.instances,
    })
}

#[tauri::command]
pub async fn restart_codex_instance_integration(
    store: State<'_, Store>,
    control: State<'_, IntegrationControl>,
    runtime: State<'_, crate::codex_adapter::CodexIntegrationRuntime>,
    request: RestartCodexInstanceRequest,
) -> CommandResult<CodexIntegrationStatus> {
    store
        .set_codex_integration_paused(false)
        .map_err(CommandError::from)?;
    control.set_paused(false).map_err(|_| {
        CommandError::unchanged(
            "integration_state_unavailable",
            "ChatGPT 速记卡状态暂时不可用",
            true,
        )
    })?;
    let instance_id = request.instance_id;
    let restart_result = tauri::async_runtime::spawn_blocking(move || {
        crate::codex_adapter::restart_codex_instance_with_integration(&instance_id)
    })
    .await
    .map_err(|_| {
        runtime.record_restart_failure("codex_instance_restart_task_failed");
        CommandError::unchanged(
            "codex_instance_restart_task_failed",
            "所选 ChatGPT 实例的重启任务未完成",
            true,
        )
    })?;
    if let Err(error) = restart_result {
        runtime.record_restart_failure(error.code());
        let message = match error.code() {
            "chatgpt_force_termination_failed" | "chatgpt_force_exit_timeout" => {
                "所选 ChatGPT 关闭窗口后仍未完全退出；WakeGPT 未启动第二份，请手动退出该实例后重试"
            }
            "chatgpt_profile_claimed_without_debugging" => {
                "所选 ChatGPT 已由其他启动方式重新打开，但未开放安全调试入口；WakeGPT 未重复启动"
            }
            "chatgpt_integration_launch_failed_profile_recovered"
            | "chatgpt_integration_start_timeout_profile_recovered" => {
                "集成模式未能启动；WakeGPT 已恢复普通 ChatGPT，其他实例未受影响"
            }
            "chatgpt_debug_endpoint_start_timeout" => {
                "所选 ChatGPT 已重新打开，但安全调试入口未在限时内就绪；WakeGPT 未重复启动"
            }
            "chatgpt_profile_recovery_launch_failed" | "chatgpt_profile_recovery_start_timeout" => {
                "集成模式与普通恢复启动均未完成；请用原来的启动方式重新打开该实例"
            }
            "chatgpt_profile_ambiguous" => {
                "检测到多个进程同时使用同一实例数据；WakeGPT 已停止操作以避免冲突"
            }
            "chatgpt_instance_arguments_unavailable" => {
                "无法安全核对正在运行实例的启动参数；WakeGPT 未启动第二份"
            }
            _ => "无法安全启用所选 ChatGPT 实例；其他实例未受影响",
        };
        return Err(CommandError::unchanged(error.code(), message, true));
    }
    std::thread::sleep(std::time::Duration::from_millis(900));
    let probe = crate::codex_adapter::runtime_probe(&runtime);
    Ok(CodexIntegrationStatus {
        paused: false,
        adapter_version: probe.adapter_version,
        supported_codex_version: probe.supported_codex_version,
        supported_codex_build: probe.supported_codex_build,
        endpoint_state: probe.endpoint_state,
        last_error_code: probe.last_error_code,
        card_script_sha256: probe.card_script_sha256,
        detected_instance_count: probe.detected_instance_count,
        connectable_instance_count: probe.connectable_instance_count,
        available_target_count: probe.available_target_count,
        connected_target_count: probe.connected_target_count,
        failed_target_count: probe.failed_target_count,
        instances: probe.instances,
    })
}

fn default_identity_status_value(
    store: &Store,
    profile: &crate::codex_adapter::DefaultIdentityProfile,
) -> CommandResult<DefaultIdentityStatus> {
    let slot = store.default_identity_slot().map_err(CommandError::from)?;
    let runtime = crate::codex_adapter::default_identity_runtime_probe(profile);
    Ok(DefaultIdentityStatus {
        configured: slot.is_some(),
        alias: slot.as_ref().map(|slot| slot.alias.clone()),
        locked: slot.as_ref().is_some_and(|slot| slot.locked),
        platform_supported: runtime.platform_supported,
        profile_present: runtime.profile_present,
        runtime_state: runtime.state,
        last_error_code: runtime.last_error_code,
        created_at_ms: slot.as_ref().map(|slot| slot.created_at_ms),
        updated_at_ms: slot.as_ref().map(|slot| slot.updated_at_ms),
    })
}

fn default_identity_action_error(
    error: crate::codex_adapter::AdapterRuntimeError,
    message: &'static str,
) -> CommandError {
    let retryable = !matches!(
        error.code(),
        "default_identity_app_data_invalid"
            | "default_identity_profile_invalid"
            | "default_identity_unsupported_platform"
    );
    CommandError::unchanged(error.code(), message, retryable)
}

fn record_default_identity_result(
    diagnostics: &LocalDiagnostics,
    result: &CommandResult<DefaultIdentityStatus>,
) {
    match result {
        Ok(status) => diagnostics.record_default_identity(
            status.runtime_state.diagnostic_code(),
            status.last_error_code,
        ),
        Err(error) => diagnostics.record_default_identity("unavailable", Some(error.code())),
    }
}

#[tauri::command]
pub fn default_identity_status(
    store: State<'_, Store>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
    diagnostics: State<'_, LocalDiagnostics>,
) -> CommandResult<DefaultIdentityStatus> {
    let result = default_identity_status_value(&store, &profile);
    record_default_identity_result(&diagnostics, &result);
    result
}

#[tauri::command]
pub async fn configure_default_identity(
    store: State<'_, Store>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
    diagnostics: State<'_, LocalDiagnostics>,
    request: ConfigureDefaultIdentityRequest,
) -> CommandResult<DefaultIdentityStatus> {
    let result = async {
        let profile_for_task = profile.inner().clone();
        tauri::async_runtime::spawn_blocking(move || profile_for_task.ensure_ready())
            .await
            .map_err(|_| {
                CommandError::unchanged(
                    "default_identity_profile_task_failed",
                    "默认身份的本地隔离空间未能准备完成",
                    true,
                )
            })?
            .map_err(|error| {
                default_identity_action_error(error, "无法准备默认身份的本地隔离空间")
            })?;
        store
            .configure_default_identity_slot(&request.alias, request.locked)
            .map_err(CommandError::from)?;
        default_identity_status_value(&store, &profile)
    }
    .await;
    record_default_identity_result(&diagnostics, &result);
    result
}

#[tauri::command]
pub async fn use_default_identity(
    app: AppHandle,
    store: State<'_, Store>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
    diagnostics: State<'_, LocalDiagnostics>,
) -> CommandResult<DefaultIdentityStatus> {
    let result = async {
        if store
            .default_identity_slot()
            .map_err(CommandError::from)?
            .is_none()
        {
            return Err(CommandError::unchanged(
                "default_identity_not_configured",
                "请先配置默认身份",
                false,
            ));
        }
        let profile_for_task = profile.inner().clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::codex_adapter::open_or_focus_default_identity(&profile_for_task)
        })
        .await
        .map_err(|_| {
            CommandError::unchanged(
                "default_identity_launch_task_failed",
                "默认身份的 ChatGPT 启动任务未完成",
                true,
            )
        })?
        .map_err(|error| default_identity_action_error(error, "无法打开默认身份的 ChatGPT"))?;
        crate::codex_adapter::request_card_refresh(&app);
        default_identity_status_value(&store, &profile)
    }
    .await;
    record_default_identity_result(&diagnostics, &result);
    result
}

#[tauri::command]
pub fn unbind_default_identity(
    store: State<'_, Store>,
    profile: State<'_, crate::codex_adapter::DefaultIdentityProfile>,
    diagnostics: State<'_, LocalDiagnostics>,
) -> CommandResult<DefaultIdentityStatus> {
    let result = (|| {
        store
            .unbind_default_identity_slot()
            .map_err(CommandError::from)?;
        default_identity_status_value(&store, &profile)
    })();
    record_default_identity_result(&diagnostics, &result);
    result
}

#[tauri::command]
pub async fn authorize_workspace(
    app: AppHandle,
    store: State<'_, Store>,
    request: AuthorizeWorkspaceRequest,
) -> CommandResult<Option<Workspace>> {
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("选择 WakeGPT 工作区")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let selected = selected
        .into_path()
        .map_err(|_| invalid_request("所选工作区路径无效"))?;
    let root_path = validate_selected_workspace(&selected).map_err(invalid_request)?;
    let display_name = request
        .display_name
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            selected
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "工作区".to_owned());
    let workspace = store
        .create_workspace(&display_name, &root_path)
        .map_err(CommandError::from)?;
    crate::codex_adapter::request_card_refresh(&app);
    Ok(Some(workspace))
}

fn validate_selected_workspace(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "无法读取所选工作区".to_owned())?;
    if metadata.file_type().is_symlink() {
        return Err("工作区根目录不能是符号链接".to_owned());
    }
    if !metadata.is_dir() {
        return Err("所选路径不是文件夹".to_owned());
    }
    let canonical = fs::canonicalize(path).map_err(|_| "无法规范化所选工作区".to_owned())?;
    let canonical_text = canonical
        .to_str()
        .ok_or_else(|| "工作区路径必须是有效 UTF-8".to_owned())?;

    let probe = canonical.join(format!(".wakegpt-write-probe-{}", crate::domain::new_id()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|_| "所选工作区不可写".to_owned())?;
    let sync_result = file.sync_all();
    drop(file);
    let remove_result = fs::remove_file(&probe);
    if sync_result.is_err() || remove_result.is_err() {
        return Err("无法验证所选工作区的安全写入能力".to_owned());
    }
    Ok(canonical_text.to_owned())
}

#[tauri::command]
pub fn list_workspaces(store: State<'_, Store>) -> CommandResult<Vec<Workspace>> {
    store.list_workspaces().map_err(CommandError::from)
}

#[tauri::command]
pub fn disconnect_workspace(
    app: AppHandle,
    store: State<'_, Store>,
    request: WorkspaceRequest,
) -> CommandResult<UiPreferences> {
    let preferences = store
        .disconnect_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    crate::codex_adapter::request_card_refresh(&app);
    if let Some(workspace_id) = preferences.active_workspace_id.as_deref() {
        crate::codex_adapter::emit_card_data_changed(
            &app,
            workspace_id,
            preferences.selected_notebook_id.as_deref(),
            "selection",
        );
    }
    Ok(preferences)
}

#[tauri::command]
pub fn reveal_workspace(store: State<'_, Store>, request: WorkspaceRequest) -> CommandResult<()> {
    let workspace = store
        .get_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let root = Path::new(&workspace.root_path);
    if !root.is_dir() {
        return Err(CommandError::unchanged(
            "workspace_root_unavailable",
            "工作区文件夹当前不可用",
            true,
        ));
    }
    reveal_workspace_path(&workspace.root_path)
}

fn require_invoking_window(window: &WebviewWindow, expected_label: &str) -> CommandResult<()> {
    if window.label() == expected_label {
        Ok(())
    } else {
        Err(CommandError::unchanged(
            "window_context_invalid",
            "当前界面不能执行该窗口操作",
            false,
        ))
    }
}

#[tauri::command]
pub fn show_main_window(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    require_invoking_window(&window, "quick-capture")?;
    crate::lifecycle::show_main_window(&app).map_err(|_| {
        CommandError::unchanged("main_window_unavailable", "无法打开 WakeGPT 主窗口", true)
    })
}

#[tauri::command]
pub fn hide_quick_capture_window(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    require_invoking_window(&window, "quick-capture")?;
    crate::lifecycle::hide_quick_capture_window(&app).map_err(|_| {
        CommandError::unchanged(
            "quick_capture_window_unavailable",
            "无法关闭快速记录面板",
            true,
        )
    })
}

#[tauri::command]
pub fn update_markdown_editor_quit_state(
    window: WebviewWindow,
    control: State<'_, crate::lifecycle::QuitControl>,
    request: MarkdownEditorQuitStateRequest,
) -> CommandResult<()> {
    require_invoking_window(&window, "main")?;
    control
        .update_markdown_editor(
            request.open,
            request.dirty,
            request.saving,
            request.revision,
        )
        .map_err(|_| {
            CommandError::unchanged(
                "quit_state_unavailable",
                "无法安全更新 Markdown 退出保护状态",
                true,
            )
        })
}

#[tauri::command]
pub fn confirm_and_quit(
    app: AppHandle,
    window: WebviewWindow,
    control: State<'_, crate::lifecycle::QuitControl>,
    request: QuitRequest,
) -> CommandResult<()> {
    require_invoking_window(&window, "main")?;
    control.confirm_exit(request.request_id).map_err(|code| {
        let (message, retryable) = match code {
            "markdown_save_in_progress" => ("Markdown 正在保存，完成前不能退出 WakeGPT", true),
            "quick_capture_quit_pending" => ("正在保存快速记录草稿，完成后才能退出 WakeGPT", true),
            "quit_request_stale" => ("退出请求已变化，请重新退出 WakeGPT", false),
            _ => ("无法安全确认退出状态", true),
        };
        CommandError::unchanged(code, message, retryable)
    })?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn acknowledge_quick_capture_quit(
    window: WebviewWindow,
    control: State<'_, crate::lifecycle::QuitControl>,
    request: QuitRequest,
) -> CommandResult<()> {
    require_invoking_window(&window, "quick-capture")?;
    control
        .acknowledge_quick_capture(request.request_id)
        .map_err(|_| {
            CommandError::unchanged(
                "quit_request_stale",
                "退出请求已变化，请重新退出 WakeGPT",
                false,
            )
        })
}

#[tauri::command]
pub fn cancel_quit_request(
    window: WebviewWindow,
    control: State<'_, crate::lifecycle::QuitControl>,
    request: QuitRequest,
) -> CommandResult<()> {
    require_invoking_window(&window, "main")?;
    control
        .cancel_request(request.request_id)
        .map_err(|_| CommandError::unchanged("quit_state_unavailable", "无法取消退出请求", true))
}

#[cfg(target_os = "macos")]
fn reveal_workspace_path(root_path: &str) -> CommandResult<()> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    let root_path = NSString::from_str(root_path);
    let revealed =
        NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(None, &root_path);
    if revealed {
        Ok(())
    } else {
        Err(CommandError::unchanged(
            "workspace_reveal_failed",
            "Finder 无法显示该工作区",
            true,
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn reveal_workspace_path(_root_path: &str) -> CommandResult<()> {
    Err(CommandError::unchanged(
        "workspace_reveal_unsupported",
        "当前平台尚未实现工作区定位",
        false,
    ))
}

#[tauri::command]
pub fn ui_preferences(store: State<'_, Store>) -> CommandResult<UiPreferences> {
    store.ui_preferences().map_err(CommandError::from)
}

#[tauri::command]
pub fn set_active_selection(
    app: AppHandle,
    store: State<'_, Store>,
    request: ActiveSelectionRequest,
) -> CommandResult<UiPreferences> {
    let preferences = store
        .set_active_selection(&request.workspace_id, request.notebook_id.as_deref())
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        request.notebook_id.as_deref(),
        "selection",
    );
    Ok(preferences)
}

#[tauri::command]
pub fn activate_workspace(
    app: AppHandle,
    store: State<'_, Store>,
    request: ActivateWorkspaceRequest,
) -> CommandResult<UiPreferences> {
    let preferences = store
        .activate_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        preferences.selected_notebook_id.as_deref(),
        "selection",
    );
    Ok(preferences)
}

#[tauri::command]
pub fn set_theme_preference(
    store: State<'_, Store>,
    request: ThemePreferenceRequest,
) -> CommandResult<UiPreferences> {
    store
        .set_theme_preference(request.theme)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn set_submit_shortcut(
    store: State<'_, Store>,
    request: SubmitShortcutRequest,
) -> CommandResult<UiPreferences> {
    store
        .set_submit_shortcut(request.submit_shortcut)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn set_markdown_layout(
    store: State<'_, Store>,
    request: MarkdownLayoutRequest,
) -> CommandResult<UiPreferences> {
    store
        .set_markdown_layout(request.markdown_layout)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn load_draft(
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: DraftRequest,
) -> CommandResult<DraftState> {
    let saved = store
        .load_draft(&request.workspace_id, request.notebook_id.as_deref())
        .map_err(CommandError::from)?;
    let verified = attachments.restore_pending_draft(&saved.attachments);
    if verified.len() != saved.attachments.len() {
        let cleaned = verified.iter().map(draft_attachment).collect::<Vec<_>>();
        store
            .save_draft(
                &request.workspace_id,
                request.notebook_id.as_deref(),
                &saved.body_markdown,
                &cleaned,
            )
            .map_err(CommandError::from)?;
    }
    Ok(DraftState {
        body_markdown: saved.body_markdown,
        attachments: verified,
    })
}

#[tauri::command]
pub fn save_draft(
    app: AppHandle,
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: SaveDraftRequest,
) -> CommandResult<DraftState> {
    let proposed = request
        .attachments
        .iter()
        .map(draft_attachment)
        .collect::<Vec<_>>();
    let verified = attachments.restore_pending_draft(&proposed);
    if verified.len() != proposed.len() {
        return Err(invalid_request("草稿中的图片已失效，请重新选择"));
    }
    let stored = verified.iter().map(draft_attachment).collect::<Vec<_>>();
    store
        .save_draft(
            &request.workspace_id,
            request.notebook_id.as_deref(),
            &request.body_markdown,
            &stored,
        )
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        request.notebook_id.as_deref(),
        "draft",
    );
    Ok(DraftState {
        body_markdown: request.body_markdown,
        attachments: verified,
    })
}

#[tauri::command]
pub fn load_quick_capture_draft(
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: DraftRequest,
) -> CommandResult<DraftState> {
    store
        .get_connected_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let saved = store
        .load_surface_draft(
            QUICK_CAPTURE_DRAFT_SURFACE,
            &request.workspace_id,
            request.notebook_id.as_deref(),
        )
        .map_err(CommandError::from)?;
    let verified = attachments.restore_pending_draft(&saved.attachments);
    if verified.len() != saved.attachments.len() {
        let cleaned = verified.iter().map(draft_attachment).collect::<Vec<_>>();
        store
            .save_surface_draft(
                QUICK_CAPTURE_DRAFT_SURFACE,
                &request.workspace_id,
                request.notebook_id.as_deref(),
                &saved.body_markdown,
                &cleaned,
            )
            .map_err(CommandError::from)?;
    }
    Ok(DraftState {
        body_markdown: saved.body_markdown,
        attachments: verified,
    })
}

#[tauri::command]
pub fn save_quick_capture_draft(
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: SaveDraftRequest,
) -> CommandResult<DraftState> {
    store
        .get_connected_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let proposed = request
        .attachments
        .iter()
        .map(draft_attachment)
        .collect::<Vec<_>>();
    let verified = attachments.restore_pending_draft(&proposed);
    if verified.len() != proposed.len() {
        return Err(invalid_request("快速记录草稿中的图片已失效，请重新选择"));
    }
    let stored = verified.iter().map(draft_attachment).collect::<Vec<_>>();
    store
        .save_surface_draft(
            QUICK_CAPTURE_DRAFT_SURFACE,
            &request.workspace_id,
            request.notebook_id.as_deref(),
            &request.body_markdown,
            &stored,
        )
        .map_err(CommandError::from)?;
    Ok(DraftState {
        body_markdown: request.body_markdown,
        attachments: verified,
    })
}

#[tauri::command]
pub fn reorder_notebooks(
    app: AppHandle,
    store: State<'_, Store>,
    request: ReorderNotebooksRequest,
) -> CommandResult<Vec<Notebook>> {
    let notebooks = store
        .reorder_notebooks(&request.workspace_id, &request.ordered_notebook_ids)
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(&app, &request.workspace_id, None, "notebooks");
    Ok(notebooks)
}

#[tauri::command]
pub fn rename_notebook(
    app: AppHandle,
    store: State<'_, Store>,
    request: RenameNotebookRequest,
) -> CommandResult<Notebook> {
    let notebook = store
        .rename_notebook(
            &request.workspace_id,
            &request.notebook_id,
            &request.display_name,
        )
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn workspace_open_preference(
    store: State<'_, Store>,
    request: WorkspaceRequest,
) -> CommandResult<WorkspaceOpenPreference> {
    store
        .workspace_open_preference(&request.workspace_id)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn set_workspace_open_preference(
    app: AppHandle,
    store: State<'_, Store>,
    request: SetWorkspaceOpenPreferenceRequest,
) -> CommandResult<WorkspaceOpenPreference> {
    let preference = store
        .set_workspace_open_preference(
            &request.workspace_id,
            request.use_last_selection,
            request.default_notebook_id.as_deref(),
        )
        .map_err(CommandError::from)?;
    if request.default_notebook_id.is_some() {
        crate::codex_adapter::emit_card_data_changed(
            &app,
            &request.workspace_id,
            request.default_notebook_id.as_deref(),
            "notebooks",
        );
    }
    Ok(preference)
}

#[tauri::command]
pub fn set_notebook_pinned(
    app: AppHandle,
    store: State<'_, Store>,
    request: NotebookPinRequest,
) -> CommandResult<Notebook> {
    let notebook = store
        .set_notebook_pinned(&request.workspace_id, &request.notebook_id, request.pinned)
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn unbind_notebook(
    app: AppHandle,
    store: State<'_, Store>,
    request: NotebookStateRequest,
) -> CommandResult<Notebook> {
    let notebook = store
        .unbind_notebook(&request.workspace_id, &request.notebook_id)
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn rebind_notebook(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookStateRequest,
) -> CommandResult<Notebook> {
    let notebook = sync
        .rebind_notebook(&store, &request.workspace_id, &request.notebook_id)
        .map_err(notebook_rebind_error)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn recover_notebook_target(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookStateRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let notebook = sync
        .recover_notebook_target(&store, &request.workspace_id, &request.notebook_id)
        .map_err(|error| CommandError::notebook_lifecycle(error, notebook_id))?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn inspect_notebook_conflict(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookStateRequest,
) -> CommandResult<NotebookConflictInspection> {
    let notebook_id = request.notebook_id.clone();
    sync.inspect_notebook_conflict(&store, &request.workspace_id, &request.notebook_id)
        .map_err(|error| CommandError::notebook_conflict(error, notebook_id))
}

#[tauri::command]
pub fn resolve_notebook_conflict(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: ResolveNotebookConflictRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let notebook = sync
        .resolve_notebook_conflict(
            &store,
            &request.workspace_id,
            &request.notebook_id,
            &request.conflict_token,
            request.action,
        )
        .map_err(|error| CommandError::notebook_conflict(error, notebook_id))?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn convert_notebook_to_plain(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookStateRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let notebook = sync
        .convert_notebook_to_plain(&store, &request.workspace_id, &request.notebook_id)
        .map_err(|error| CommandError::notebook_lifecycle(error, notebook_id))?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn trash_notebook_file(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookStateRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let (notebook, _) = sync
        .trash_notebook_file(&store, &request.workspace_id, &request.notebook_id)
        .map_err(|error| CommandError::notebook_lifecycle(error, notebook_id))?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

fn draft_attachment(attachment: &PendingAttachment) -> DraftAttachment {
    DraftAttachment {
        token: attachment.token.clone(),
        media_type: attachment.media_type.clone(),
        byte_size: attachment.byte_size,
        content_sha256: attachment.content_sha256.clone(),
        display_name: attachment.display_name.clone(),
    }
}

#[tauri::command]
pub fn create_notebook(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: CreateNotebookRequest,
) -> CommandResult<Notebook> {
    let notebook = sync
        .execute_notebook_creation(
            &store,
            &request.workspace_id,
            &request.relative_path,
            |store| {
                store.create_notebook_with_configuration(
                    &request.workspace_id,
                    &request.display_name,
                    &request.relative_path,
                    request.numbering_style,
                    request.numbering_start,
                    request.attachment_directory.as_deref(),
                )
            },
        )
        .map_err(CommandError::coordinated)?;
    crate::codex_adapter::request_card_refresh(&app);
    Ok(notebook)
}

#[tauri::command]
pub async fn bind_existing_notebook(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: BindExistingNotebookRequest,
) -> CommandResult<Option<Notebook>> {
    let workspace = store
        .get_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("绑定已有 Markdown 笔记")
        .add_filter("Markdown", &["md"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let selected = selected
        .into_path()
        .map_err(|_| invalid_request("所选 Markdown 路径无效"))?;
    let relative_path = selected_notebook_relative_path(&workspace, &selected)?;
    let display_name = request
        .display_name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            selected
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Markdown 笔记".to_owned());
    let notebook = sync
        .execute_notebook_binding(
            &store,
            &workspace.id,
            &relative_path,
            request.numbering_style,
            request.numbering_start,
            |store| {
                store.create_notebook_with_configuration(
                    &workspace.id,
                    &display_name,
                    &relative_path,
                    request.numbering_style,
                    request.numbering_start,
                    request.attachment_directory.as_deref(),
                )
            },
        )
        .map_err(CommandError::coordinated)?;
    crate::codex_adapter::request_card_refresh(&app);
    Ok(Some(notebook))
}

#[tauri::command]
pub fn list_notebooks(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    workspace_id: String,
) -> CommandResult<Vec<Notebook>> {
    let detected = sync
        .check_workspace_conflicts(&store, &workspace_id)
        .map_err(|error| CommandError::notebook_conflict(error, workspace_id.clone()))?;
    if detected > 0 {
        crate::codex_adapter::emit_card_data_changed(&app, &workspace_id, None, "notebooks");
    }
    store
        .list_notebooks(&workspace_id)
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn preview_notebook_numbering(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: PreviewNotebookNumberingRequest,
) -> CommandResult<NotebookNumberingPreview> {
    let notebook_id = request.notebook_id.clone();
    sync.preview_notebook_numbering(
        &store,
        &request.workspace_id,
        &request.notebook_id,
        request.numbering_style,
        request.numbering_start,
    )
    .map_err(|error| CommandError::notebook_numbering_preview(error, notebook_id))
}

#[tauri::command]
pub fn change_notebook_numbering(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: ChangeNotebookNumberingRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let notebook = sync
        .change_notebook_numbering(
            &store,
            &request.workspace_id,
            &request.notebook_id,
            NotebookNumberingConfiguration {
                style: request.numbering_style,
                start: request.numbering_start,
            },
            &request.expected_file_sha256,
            request.expected_receipt_generation,
        )
        .map_err(|error| CommandError::notebook_numbering_change(error, notebook_id))?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn preview_notebook_attachment_directory(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: PreviewNotebookAttachmentDirectoryRequest,
) -> CommandResult<NotebookAttachmentDirectoryPreview> {
    let notebook_id = request.notebook_id.clone();
    sync.preview_notebook_attachment_directory(
        &store,
        &request.workspace_id,
        &request.notebook_id,
        &request.attachment_directory,
    )
    .map_err(|error| CommandError::notebook_attachment_directory_preview(error, notebook_id))
}

#[tauri::command]
pub fn change_notebook_attachment_directory(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: ChangeNotebookAttachmentDirectoryRequest,
) -> CommandResult<Notebook> {
    let notebook_id = request.notebook_id.clone();
    let notebook = sync
        .change_notebook_attachment_directory(
            &store,
            &request.workspace_id,
            &request.notebook_id,
            NotebookAttachmentDirectoryChange {
                directory: request.attachment_directory.clone(),
                expected_file_sha256: request.expected_file_sha256.clone(),
                expected_receipt_generation: request.expected_receipt_generation,
            },
            |store, notebook| {
                attachments
                    .prepare_notebook_relocations(store, &notebook.id)
                    .map_err(|error| SyncError::RecoveryRequired(error.code()))
            },
        )
        .map_err(|error| {
            CommandError::notebook_attachment_directory_change(error, notebook_id.clone())
        })?;
    let entity_id = notebook.id.clone();
    attachments
        .finish_pending_relocations(&store)
        .map_err(|error| CommandError::attachment_saved(error, entity_id.clone()))?;
    let notebook = store
        .get_notebook(&request.workspace_id, &notebook.id)
        .map_err(CommandError::from)?;
    if store
        .pending_attachment_relocations()
        .map_err(CommandError::from)?
        .iter()
        .any(|operation| operation.notebook_id.as_deref() == Some(notebook.id.as_str()))
    {
        return Err(CommandError::attachment_saved(
            AttachmentError::Io("attachment_relocation_cleanup_pending"),
            entity_id,
        ));
    }
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        Some(&request.notebook_id),
        "notebooks",
    );
    Ok(notebook)
}

#[tauri::command]
pub fn read_notebook_document(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: NotebookDocumentRequest,
) -> CommandResult<NotebookDocument> {
    sync.read_notebook_document(&store, &request.workspace_id, &request.notebook_id)
        .map_err(notebook_document_read_error)
}

#[tauri::command]
pub fn save_notebook_document(
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    request: SaveNotebookDocumentRequest,
) -> CommandResult<NotebookDocument> {
    let notebook_id = request.notebook_id.clone();
    sync.save_notebook_document(
        &store,
        &request.workspace_id,
        &request.notebook_id,
        &request.expected_file_sha256,
        request.expected_receipt_generation,
        &request.markdown,
    )
    .map_err(|error| CommandError::document_save(error, notebook_id))
}

#[tauri::command]
pub async fn pick_record_images(
    app: AppHandle,
    attachments: State<'_, AttachmentCoordinator>,
) -> CommandResult<Vec<PendingAttachment>> {
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("选择速记图片")
        .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif"])
        .blocking_pick_files()
    else {
        return Ok(Vec::new());
    };
    let paths = selected
        .into_iter()
        .map(|path| {
            path.into_path()
                .map_err(|_| invalid_request("所选图片路径无效"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    attachments
        .stage_selected(&paths)
        .map_err(attachment_command_error)
}

#[tauri::command]
pub fn stage_record_image(
    attachments: State<'_, AttachmentCoordinator>,
    request: tauri::ipc::Request<'_>,
) -> CommandResult<Option<PendingAttachment>> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err(invalid_request("图片暂存请求必须使用二进制内容"));
    };
    if bytes.is_empty() || bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return attachments
            .stage_uploaded(bytes)
            .map(Some)
            .map_err(attachment_command_error);
    }
    let digest = sha256_hex(bytes);
    let is_known = known_digest_header_contains(
        request
            .headers()
            .get("x-wakegpt-known-digests")
            .and_then(|value| value.to_str().ok()),
        &digest,
    );
    if is_known {
        return Ok(None);
    }
    attachments
        .stage_uploaded(bytes)
        .map(Some)
        .map_err(attachment_command_error)
}

#[tauri::command]
pub fn discard_pending_image(
    attachments: State<'_, AttachmentCoordinator>,
    token: String,
) -> CommandResult<()> {
    attachments
        .discard_pending(&token)
        .map_err(attachment_command_error)
}

#[tauri::command]
pub fn read_pending_image_preview(
    attachments: State<'_, AttachmentCoordinator>,
    token: String,
) -> CommandResult<tauri::ipc::Response> {
    let (bytes, _) = attachments
        .read_pending_preview(&token)
        .map_err(attachment_command_error)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub fn read_record_image_preview(
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: RecordImagePreviewRequest,
) -> CommandResult<tauri::ipc::Response> {
    let record = store
        .get_record(&request.workspace_id, &request.record_id)
        .map_err(CommandError::from)?;
    let attachment = record
        .attachments
        .iter()
        .find(|attachment| attachment.id == request.attachment_id)
        .ok_or_else(|| invalid_request("图片不属于这条记录"))?;
    let workspace = store
        .get_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let bytes = attachments
        .read_record_preview(&workspace, attachment)
        .map_err(attachment_command_error)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub fn read_markdown_image_preview(
    store: State<'_, Store>,
    attachments: State<'_, AttachmentCoordinator>,
    request: MarkdownImagePreviewRequest,
) -> CommandResult<tauri::ipc::Response> {
    let (workspace, notebook) = markdown_image_context(&store, &request)?;
    require_markdown_image_notebook_ready(&notebook)?;
    let (bytes, media_type) = attachments
        .read_markdown_preview(&workspace, &notebook, &request.relative_path)
        .map_err(attachment_command_error)?;
    let (_, current_notebook) = markdown_image_context(&store, &request)?;
    require_markdown_image_notebook_ready(&current_notebook)?;
    let media_tag = match media_type {
        "image/png" => 1,
        "image/jpeg" => 2,
        "image/gif" => 3,
        "image/webp" => 4,
        _ => {
            return Err(CommandError::unchanged(
                "attachment_type_unsupported",
                "仅支持 PNG、JPEG、WebP 和 GIF 图片",
                false,
            ));
        }
    };
    let mut response = Vec::with_capacity(6 + bytes.len());
    response.extend_from_slice(b"WGMI");
    response.push(1);
    response.push(media_tag);
    response.extend_from_slice(&bytes);
    Ok(tauri::ipc::Response::new(response))
}

#[tauri::command]
pub fn create_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: CreateRecordRequest,
) -> CommandResult<Record> {
    let attachment_context = if request.attachment_tokens.is_empty() {
        None
    } else {
        let workspace = store
            .get_workspace(&request.workspace_id)
            .map_err(CommandError::from)?;
        let attachment_directory = match request.notebook_id.as_deref() {
            Some(notebook_id) => {
                store
                    .get_notebook(&request.workspace_id, notebook_id)
                    .map_err(CommandError::from)?
                    .attachment_directory
            }
            None => INBOX_ATTACHMENT_DIRECTORY.to_owned(),
        };
        Some((workspace, attachment_directory))
    };
    let result = sync.execute_record_mutation_with_attachments(&store, &attachments, |store| {
        let prepared = match &attachment_context {
            Some((workspace, attachment_directory)) => attachments
                .materialize_locked(workspace, attachment_directory, &request.attachment_tokens)
                .map_err(|_| {
                    crate::storage::StoreError::Conflict(
                        "attachment materialization failed".to_owned(),
                    )
                })?,
            None => Vec::new(),
        };
        store.create_record_from_draft_attempt(
            &request.workspace_id,
            request.notebook_id.as_deref(),
            &request.body_markdown,
            &prepared,
            &request.mutation_id,
            request.mutation_schema_version,
        )
    });
    let record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        request.notebook_id.as_deref(),
        "records",
    );
    Ok(record)
}

#[tauri::command]
pub fn create_quick_capture_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: CreateRecordRequest,
) -> CommandResult<Record> {
    let connected_workspace = store
        .get_connected_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    let attachment_context = if request.attachment_tokens.is_empty() {
        None
    } else {
        let attachment_directory = match request.notebook_id.as_deref() {
            Some(notebook_id) => {
                store
                    .get_notebook(&request.workspace_id, notebook_id)
                    .map_err(CommandError::from)?
                    .attachment_directory
            }
            None => INBOX_ATTACHMENT_DIRECTORY.to_owned(),
        };
        Some((connected_workspace, attachment_directory))
    };
    let result = sync.execute_record_mutation_with_attachments(&store, &attachments, |store| {
        let prepared = match &attachment_context {
            Some((workspace, attachment_directory)) => attachments
                .materialize_locked(workspace, attachment_directory, &request.attachment_tokens)
                .map_err(|_| {
                    crate::storage::StoreError::Conflict(
                        "attachment materialization failed".to_owned(),
                    )
                })?,
            None => Vec::new(),
        };
        store.create_record_from_surface_draft_attempt(
            Some(QUICK_CAPTURE_DRAFT_SURFACE),
            DraftRecordCreate {
                workspace_id: &request.workspace_id,
                notebook_id: request.notebook_id.as_deref(),
                body_markdown: &request.body_markdown,
                attachments: &prepared,
                proposed_mutation_id: &request.mutation_id,
                mutation_schema_version: request.mutation_schema_version,
            },
        )
    });
    let record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        request.notebook_id.as_deref(),
        "records",
    );
    Ok(record)
}

#[tauri::command]
pub fn list_records(
    store: State<'_, Store>,
    workspace_id: String,
    notebook_id: Option<String>,
    include_trashed: Option<bool>,
) -> CommandResult<Vec<Record>> {
    store
        .list_records(
            &workspace_id,
            notebook_id.as_deref(),
            include_trashed.unwrap_or(false),
        )
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn update_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: UpdateRecordRequest,
) -> CommandResult<Record> {
    let current = store
        .get_record(&request.workspace_id, &request.record_id)
        .map_err(CommandError::from)?;
    let revises_attachment_set =
        request.retained_attachment_ids.is_some() || !request.new_attachment_tokens.is_empty();
    let retained_attachment_ids = request.retained_attachment_ids.unwrap_or_else(|| {
        current
            .attachments
            .iter()
            .map(|attachment| attachment.id.clone())
            .collect()
    });
    let attachment_context = if request.new_attachment_tokens.is_empty() {
        None
    } else {
        let workspace = store
            .get_workspace(&request.workspace_id)
            .map_err(CommandError::from)?;
        let attachment_directory = match current.notebook_id.as_deref() {
            Some(notebook_id) => {
                store
                    .get_notebook(&request.workspace_id, notebook_id)
                    .map_err(CommandError::from)?
                    .attachment_directory
            }
            None => INBOX_ATTACHMENT_DIRECTORY.to_owned(),
        };
        Some((workspace, attachment_directory))
    };
    let result = sync.execute_record_mutation_with_attachments(&store, &attachments, |store| {
        let prepared = match &attachment_context {
            Some((workspace, attachment_directory)) => attachments
                .materialize_locked(
                    workspace,
                    attachment_directory,
                    &request.new_attachment_tokens,
                )
                .map_err(|_| {
                    crate::storage::StoreError::Conflict(
                        "attachment materialization failed".to_owned(),
                    )
                })?,
            None => Vec::new(),
        };
        if revises_attachment_set {
            store.revise_record_attachment_set_idempotent(
                &request.workspace_id,
                &request.record_id,
                request.expected_revision,
                RecordAttachmentSetRevision {
                    body_markdown: &request.body_markdown,
                    retained_attachment_ids: &retained_attachment_ids,
                    new_attachments: &prepared,
                    mutation_id: &request.mutation_id,
                    mutation_schema_version: request.mutation_schema_version,
                },
            )
        } else {
            store.update_record_idempotent(
                &request.workspace_id,
                &request.record_id,
                request.expected_revision,
                &request.body_markdown,
                &request.mutation_id,
                request.mutation_schema_version,
            )
        }
    });
    let mut record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        record.notebook_id.as_deref(),
        "records",
    );
    if revises_attachment_set && record.revision != request.expected_revision {
        let entity_id = record.id.clone();
        record = attachments
            .apply_record_lifecycle(&store, record)
            .map_err(|error| CommandError::attachment_saved(error, entity_id))?;
    }
    Ok(record)
}

#[tauri::command]
pub fn trash_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: RecordRevisionRequest,
) -> CommandResult<Record> {
    let result = sync.execute_record_mutation(&store, |store| {
        store.trash_record_idempotent(
            &request.workspace_id,
            &request.record_id,
            request.expected_revision,
            &request.mutation_id,
            request.mutation_schema_version,
        )
    });
    let record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        record.notebook_id.as_deref(),
        "records",
    );
    let entity_id = record.id.clone();
    let record = attachments
        .apply_record_lifecycle(&store, record)
        .map_err(|error| CommandError::attachment_saved(error, entity_id))?;
    Ok(record)
}

#[tauri::command]
pub fn restore_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: RecordRevisionRequest,
) -> CommandResult<Record> {
    let result = sync.execute_record_mutation(&store, |store| {
        store.restore_record_idempotent(
            &request.workspace_id,
            &request.record_id,
            request.expected_revision,
            &request.mutation_id,
            request.mutation_schema_version,
        )
    });
    let record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        record.notebook_id.as_deref(),
        "records",
    );
    let entity_id = record.id.clone();
    let record = attachments
        .apply_record_lifecycle(&store, record)
        .map_err(|error| CommandError::attachment_saved(error, entity_id))?;
    Ok(record)
}

#[tauri::command]
pub fn migrate_record(
    app: AppHandle,
    store: State<'_, Store>,
    sync: State<'_, SyncCoordinator>,
    attachments: State<'_, AttachmentCoordinator>,
    request: MigrateRecordRequest,
) -> CommandResult<Record> {
    let result = sync.execute_record_mutation_with_prepare(
        &store,
        |store| {
            store.migrate_record_idempotent(
                &request.workspace_id,
                &request.record_id,
                request.expected_revision,
                request.destination_notebook_id.as_deref(),
                &request.mutation_id,
                request.mutation_schema_version,
            )
        },
        |store, record| {
            attachments
                .prepare_record_relocations(store, &record.id)
                .map_err(|error| SyncError::RecoveryRequired(error.code()))
        },
    );
    let record = coordinated_record_result(&app, &request.workspace_id, result)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        record.notebook_id.as_deref(),
        "records",
    );
    let entity_id = record.id.clone();
    attachments
        .finish_pending_relocations(&store)
        .map_err(|error| CommandError::attachment_saved(error, entity_id.clone()))?;
    let record = store
        .get_record(&request.workspace_id, &record.id)
        .map_err(CommandError::from)?;
    if record
        .attachments
        .iter()
        .any(|attachment| attachment.relocation_state != AttachmentRelocationState::Ready)
    {
        return Err(CommandError::attachment_saved(
            AttachmentError::Io("attachment_relocation_cleanup_pending"),
            entity_id,
        ));
    }
    Ok(record)
}

#[tauri::command]
pub fn set_record_pinned(
    app: AppHandle,
    store: State<'_, Store>,
    request: PinRecordRequest,
) -> CommandResult<Record> {
    let record = store
        .set_record_pinned(&request.workspace_id, &request.record_id, request.pinned)
        .map_err(CommandError::from)?;
    crate::codex_adapter::emit_card_data_changed(
        &app,
        &request.workspace_id,
        record.notebook_id.as_deref(),
        "records",
    );
    Ok(record)
}

#[tauri::command]
pub fn preview_managed_markdown(
    store: State<'_, Store>,
    request: PreviewManagedMarkdownRequest,
) -> CommandResult<String> {
    const MAX_NOTEBOOK_BYTES: usize = 16 * 1024 * 1024;
    if request.existing_markdown.len() > MAX_NOTEBOOK_BYTES {
        return Err(invalid_request(format!(
            "notebook preview cannot exceed {MAX_NOTEBOOK_BYTES} bytes"
        )));
    }
    let notebook = store
        .get_notebook(&request.workspace_id, &request.notebook_id)
        .map_err(CommandError::from)?;
    let records = store
        .list_records(&request.workspace_id, Some(&request.notebook_id), false)
        .map_err(CommandError::from)?;
    synchronize_notebook_for_path(
        &request.existing_markdown,
        &notebook.target_id,
        &records,
        notebook.numbering_style,
        notebook.numbering_start,
        "\n",
        Some(&notebook.relative_path),
    )
    .map_err(|error| CommandError::storage(crate::storage::StoreError::Conflict(error.to_string())))
}

fn invalid_request(message: impl Into<String>) -> CommandError {
    CommandError::storage(crate::storage::StoreError::Domain(
        crate::domain::DomainError::new(message),
    ))
}

fn coordinated_record_result(
    app: &AppHandle,
    workspace_id: &str,
    result: Result<Record, CoordinatedMutationError>,
) -> CommandResult<Record> {
    match result {
        Ok(record) => Ok(record),
        Err(error) => {
            if matches!(&error, CoordinatedMutationError::Saved { .. }) {
                crate::codex_adapter::request_card_refresh(app);
                crate::codex_adapter::emit_card_data_changed(app, workspace_id, None, "notebooks");
            }
            Err(CommandError::coordinated(error))
        }
    }
}

fn selected_notebook_relative_path(
    workspace: &Workspace,
    selected: &Path,
) -> CommandResult<String> {
    let root = Path::new(&workspace.root_path);
    let relative = selected
        .strip_prefix(root)
        .map_err(|_| invalid_request("只能绑定当前工作区内的 Markdown 文件"))?;
    let relative = relative
        .to_str()
        .ok_or_else(|| invalid_request("Markdown 路径必须是有效 UTF-8"))?;
    crate::domain::validate_notebook_relative_path(relative)
        .map_err(|error| invalid_request(error.to_string()))
}

fn notebook_document_read_error(error: SyncError) -> CommandError {
    let retryable = !matches!(&error, SyncError::Managed(_) | SyncError::Conflict(_));
    let message = match &error {
        SyncError::Managed(message) => message.clone(),
        SyncError::Conflict(_) => "Markdown 受管区与 WakeGPT 记录不一致".to_owned(),
        SyncError::Io(_) => "无法读取 Markdown 文件".to_owned(),
        SyncError::Store(_) | SyncError::LockPoisoned => "本地同步状态暂时不可用".to_owned(),
        SyncError::RecoveryRequired(_) => "Markdown 文件需要先完成恢复".to_owned(),
    };
    CommandError::unchanged(error.code(), message, retryable)
}

fn notebook_rebind_error(error: SyncError) -> CommandError {
    let retryable = matches!(
        &error,
        SyncError::Io(_) | SyncError::Store(_) | SyncError::LockPoisoned
    );
    let message = match &error {
        SyncError::Managed(_) | SyncError::Conflict(_) => {
            "无法恢复绑定：Markdown 受管区或同步回执已发生变化".to_owned()
        }
        SyncError::Io("notebook_rebind_target_missing") => {
            "无法恢复绑定：原 Markdown 文件已不存在".to_owned()
        }
        SyncError::Io(_) => "无法恢复绑定：Markdown 文件暂时不可用".to_owned(),
        SyncError::Store(_) | SyncError::LockPoisoned => {
            "本地同步状态暂时不可用，请稍后重试".to_owned()
        }
        SyncError::RecoveryRequired(_) => "请先完成 Markdown 文件恢复".to_owned(),
    };
    CommandError::unchanged(error.code(), message, retryable)
}

fn attachment_command_error(error: AttachmentError) -> CommandError {
    let retryable = matches!(error, AttachmentError::Io(_) | AttachmentError::Sync(_));
    let message = match error.code() {
        "too_many_attachments" => "每条记录最多添加 10 张图片",
        "attachment_too_large" => "单张图片不能超过 20 MiB",
        "attachments_total_too_large" => "单条记录的图片合计不能超过 100 MiB",
        "attachment_type_unsupported" => "仅支持 PNG、JPEG、WebP 和 GIF 图片",
        "attachment_token_expired" => "待提交图片已失效，请重新选择",
        "attachment_preview_unavailable" => "图片当前不可预览",
        "attachment_preview_missing" => "图片文件已不存在",
        "attachment_preview_changed" | "attachment_preview_media_type_changed" => {
            "图片文件已变化，为安全起见未加载"
        }
        "attachment_file_changed_during_read" => "图片读取期间发生变化，为安全起见未加载",
        "markdown_image_path_invalid" | "markdown_image_symlink_rejected" => {
            "Markdown 图片路径不安全，已阻止加载"
        }
        "markdown_image_not_regular" => "Markdown 图片路径不是普通文件",
        "markdown_image_missing" => "Markdown 图片文件已不存在",
        "markdown_image_unavailable" => "Markdown 图片文件当前不可读取",
        _ => "无法安全处理所选图片",
    };
    CommandError::unchanged(error.code(), message, retryable)
}

fn markdown_image_context(
    store: &Store,
    request: &MarkdownImagePreviewRequest,
) -> CommandResult<(Workspace, Notebook)> {
    let preferences = store.ui_preferences().map_err(CommandError::from)?;
    if preferences.active_workspace_id.as_deref() != Some(request.workspace_id.as_str()) {
        return Err(CommandError::unchanged(
            "markdown_image_context_changed",
            "当前工作区已切换，未加载图片",
            false,
        ));
    }
    let notebook = store
        .get_notebook(&request.workspace_id, &request.notebook_id)
        .map_err(CommandError::from)?;
    if notebook.target_id != request.notebook_target_id {
        return Err(CommandError::unchanged(
            "markdown_image_context_changed",
            "Markdown 笔记身份已变化，未加载图片",
            false,
        ));
    }
    let workspace = store
        .get_workspace(&request.workspace_id)
        .map_err(CommandError::from)?;
    Ok((workspace, notebook))
}

fn require_markdown_image_notebook_ready(notebook: &Notebook) -> CommandResult<()> {
    if notebook.target_state != NotebookTargetState::Ready {
        return Err(CommandError::unchanged(
            "markdown_image_notebook_unavailable",
            "当前 Markdown 笔记不可用，未加载图片",
            false,
        ));
    }
    Ok(())
}

fn known_digest_header_contains(value: Option<&str>, digest: &str) -> bool {
    value.is_some_and(|value| {
        value.split(',').any(|candidate| {
            candidate.len() == 64
                && candidate.bytes().all(|byte| byte.is_ascii_hexdigit())
                && candidate.eq_ignore_ascii_case(digest)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{
        known_digest_header_contains, markdown_image_context, MarkdownImagePreviewRequest,
    };
    use crate::domain::NumberingStyle;
    use crate::storage::Store;

    #[test]
    fn raw_image_digest_deduplication_requires_an_exact_sha256() {
        let digest = "a".repeat(64);
        assert!(known_digest_header_contains(Some(&digest), &digest));
        assert!(known_digest_header_contains(
            Some(&format!("{},{}", "b".repeat(64), digest.to_uppercase())),
            &digest,
        ));
        assert!(!known_digest_header_contains(
            Some(&format!("prefix{digest}")),
            &digest,
        ));
        assert!(!known_digest_header_contains(Some("not-a-digest"), &digest));
        assert!(!known_digest_header_contains(None, &digest));
    }

    #[test]
    fn markdown_image_preview_context_binds_the_workspace_and_notebook_identity() {
        let store = Store::open_in_memory().unwrap();
        let first_workspace = store
            .create_workspace("First", "/tmp/wakegpt-markdown-image-first")
            .unwrap();
        let first_notebook = store
            .create_notebook(
                &first_workspace.id,
                "First",
                "first.md",
                NumberingStyle::None,
            )
            .unwrap();
        let second_notebook = store
            .create_notebook(
                &first_workspace.id,
                "Second",
                "second.md",
                NumberingStyle::None,
            )
            .unwrap();
        let second_workspace = store
            .create_workspace("Second", "/tmp/wakegpt-markdown-image-second")
            .unwrap();
        let foreign_notebook = store
            .create_notebook(
                &second_workspace.id,
                "Foreign",
                "foreign.md",
                NumberingStyle::None,
            )
            .unwrap();
        store
            .set_active_selection(&first_workspace.id, Some(&first_notebook.id))
            .unwrap();

        let second_request = MarkdownImagePreviewRequest {
            workspace_id: first_workspace.id.clone(),
            notebook_id: second_notebook.id.clone(),
            notebook_target_id: second_notebook.target_id.clone(),
            relative_path: "image.png".to_owned(),
        };
        assert!(
            markdown_image_context(&store, &second_request).is_ok(),
            "a settings preview may legitimately target a non-selected notebook"
        );

        let mismatched_notebook = MarkdownImagePreviewRequest {
            notebook_target_id: first_notebook.target_id,
            ..second_request
        };
        assert!(markdown_image_context(&store, &mismatched_notebook).is_err());

        let foreign_workspace = MarkdownImagePreviewRequest {
            workspace_id: second_workspace.id,
            notebook_id: foreign_notebook.id,
            notebook_target_id: foreign_notebook.target_id,
            relative_path: "image.png".to_owned(),
        };
        assert!(markdown_image_context(&store, &foreign_workspace).is_err());
    }
}
