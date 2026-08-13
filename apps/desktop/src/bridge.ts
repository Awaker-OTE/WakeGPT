import { invoke } from "@tauri-apps/api/core";
import type {
  AppStatus,
  CodexIntegrationStatus,
  DataExportResult,
  DefaultIdentityStatus,
  DraftState,
  LoginItemStatus,
  LocalDataResetNotice,
  LocalDataResetPreview,
  LocalDataResetScheduleResult,
  LocalDiagnosticsExportResult,
  LocalDiagnosticsMaxBytes,
  LocalDiagnosticsPage,
  LocalDiagnosticsRetentionDays,
  LocalDiagnosticsStatus,
  MarkdownLayout,
  Notebook,
  NotebookAttachmentDirectoryPreview,
  NotebookConflictInspection,
  NotebookConflictResolutionAction,
  NotebookDocument,
  NotebookNumberingPreview,
  NumberingStyle,
  PendingAttachment,
  ProductSettings,
  RecordItem,
  RecordTrashCleanupPreview,
  RecordTrashCleanupResult,
  RecoveryReport,
  SubmitShortcut,
  ThemePreference,
  UiPreferences,
  UpdateCheckIntervalHours,
  UpdateDownloadProgress,
  UpdateStatus,
  Workspace,
  WorkspaceOpenPreference,
} from "./domain";

export interface AuthorizeWorkspaceRequest {
  displayName?: string;
}

export interface ConfigureDefaultIdentityRequest {
  alias: string;
  locked: boolean;
}

export interface LocalDiagnosticsPageRequest {
  beforeId: number | null;
  limit: number;
}

export interface UpdateLocalDiagnosticsSettingsRequest {
  enabled: boolean;
  retentionDays: LocalDiagnosticsRetentionDays;
  maxBytes: LocalDiagnosticsMaxBytes;
}

export interface SetUpdateSettingsRequest {
  externalNetworkEnabled: boolean;
  automaticChecksEnabled: boolean;
  automaticDownloadsEnabled: boolean;
  checkIntervalHours: UpdateCheckIntervalHours;
}

export interface SetProductSettingsRequest {
  recordTrashRetentionDays: number | null;
  notebookScanIgnoreDirectories: string[];
}

export type { UpdateDownloadProgress };

export interface LocalDataResetRequest {
  confirmationPhrase: string;
}

export interface MarkdownEditorQuitStateRequest {
  open: boolean;
  dirty: boolean;
  saving: boolean;
  revision: number;
}

export interface QuitRequest {
  requestId: number;
}

export interface CreateNotebookRequest {
  workspaceId: string;
  displayName: string;
  relativePath: string;
  numberingStyle: NumberingStyle;
  numberingStart: number;
  attachmentDirectory?: string;
}

export interface CreateRecordRequest {
  mutationId: string;
  mutationSchemaVersion: 1;
  workspaceId: string;
  notebookId: string | null;
  bodyMarkdown: string;
  attachmentTokens: string[];
}

export interface BindExistingNotebookRequest {
  workspaceId: string;
  displayName?: string;
  numberingStyle: NumberingStyle;
  numberingStart: number;
  attachmentDirectory?: string;
}

export interface NotebookDocumentRequest {
  workspaceId: string;
  notebookId: string;
}

export interface ResolveNotebookConflictRequest extends NotebookDocumentRequest {
  conflictToken: string;
  action: NotebookConflictResolutionAction;
}

export interface SaveNotebookDocumentRequest extends NotebookDocumentRequest {
  expectedFileSha256: string;
  expectedReceiptGeneration: number;
  markdown: string;
}

export interface PreviewNotebookNumberingRequest extends NotebookDocumentRequest {
  numberingStyle: NumberingStyle;
  numberingStart: number;
}

export interface ChangeNotebookNumberingRequest extends PreviewNotebookNumberingRequest {
  expectedFileSha256: string;
  expectedReceiptGeneration: number;
}

export interface PreviewNotebookAttachmentDirectoryRequest extends NotebookDocumentRequest {
  attachmentDirectory: string;
}

export interface ChangeNotebookAttachmentDirectoryRequest
  extends PreviewNotebookAttachmentDirectoryRequest {
  expectedFileSha256: string;
  expectedReceiptGeneration: number;
}

export interface UpdateRecordRequest {
  mutationId: string;
  mutationSchemaVersion: 1;
  workspaceId: string;
  recordId: string;
  expectedRevision: number;
  bodyMarkdown: string;
  retainedAttachmentIds?: string[];
  newAttachmentTokens?: string[];
}

export interface RecordRevisionRequest {
  mutationId: string;
  mutationSchemaVersion: 1;
  workspaceId: string;
  recordId: string;
  expectedRevision: number;
}

export interface MigrateRecordRequest extends RecordRevisionRequest {
  destinationNotebookId: string | null;
}

export interface PinRecordRequest {
  workspaceId: string;
  recordId: string;
  pinned: boolean;
}

export interface ActiveSelectionRequest {
  workspaceId: string;
  notebookId: string | null;
}

export interface DraftRequest extends ActiveSelectionRequest {}

export interface SaveDraftRequest extends DraftRequest {
  bodyMarkdown: string;
  attachments: PendingAttachment[];
}

export type CreateQuickCaptureRecordRequest = CreateRecordRequest;

export interface RecordImagePreviewRequest {
  workspaceId: string;
  recordId: string;
  attachmentId: string;
}

export interface MarkdownImagePreviewRequest {
  workspaceId: string;
  notebookId: string;
  notebookTargetId: string;
  relativePath: string;
}

export interface ReorderNotebooksRequest {
  workspaceId: string;
  orderedNotebookIds: string[];
}

export interface NotebookStateRequest {
  workspaceId: string;
  notebookId: string;
}

export interface NotebookPinRequest extends NotebookStateRequest {
  pinned: boolean;
}

export interface RenameNotebookRequest extends NotebookStateRequest {
  displayName: string;
}

export interface SetWorkspaceOpenPreferenceRequest {
  workspaceId: string;
  useLastSelection: boolean;
  defaultNotebookId: string | null;
}

export interface PreviewManagedMarkdownRequest {
  workspaceId: string;
  notebookId: string;
  existingMarkdown: string;
}

export const wakeBridge = {
  appStatus: () => invoke<AppStatus>("app_status"),
  productSettings: () => invoke<ProductSettings>("product_settings"),
  setProductSettings: (request: SetProductSettingsRequest) =>
    invoke<ProductSettings>("set_product_settings", { request }),
  previewRecordTrashCleanup: () =>
    invoke<RecordTrashCleanupPreview>("preview_record_trash_cleanup"),
  purgeRecordTrash: (previewToken: string) =>
    invoke<RecordTrashCleanupResult>("purge_record_trash", {
      request: { previewToken },
    }),
  exportLocalData: () => invoke<DataExportResult | null>("export_local_data"),
  localDataResetPreview: () =>
    invoke<LocalDataResetPreview>("local_data_reset_preview"),
  localDataResetNotice: () =>
    invoke<LocalDataResetNotice | null>("local_data_reset_notice"),
  acknowledgeLocalDataResetNotice: (occurredAtMs: number) =>
    invoke<boolean>("acknowledge_local_data_reset_notice", {
      request: { occurredAtMs },
    }),
  resetLocalData: (request: LocalDataResetRequest) =>
    invoke<LocalDataResetScheduleResult>("reset_local_data", { request }),
  localDiagnosticsStatus: () =>
    invoke<LocalDiagnosticsStatus>("local_diagnostics_status"),
  listLocalDiagnostics: (request: LocalDiagnosticsPageRequest) =>
    invoke<LocalDiagnosticsPage>("list_local_diagnostics", { request }),
  updateLocalDiagnosticsSettings: (
    request: UpdateLocalDiagnosticsSettingsRequest,
  ) =>
    invoke<LocalDiagnosticsStatus>("update_local_diagnostics_settings", {
      request,
    }),
  exportLocalDiagnostics: () =>
    invoke<LocalDiagnosticsExportResult | null>("export_local_diagnostics"),
  clearLocalDiagnostics: () =>
    invoke<LocalDiagnosticsStatus>("clear_local_diagnostics"),
  loginItemStatus: () => invoke<LoginItemStatus>("login_item_status"),
  setLoginItemEnabled: (enabled: boolean) =>
    invoke<LoginItemStatus>("set_login_item_enabled", { request: { enabled } }),
  updateStatus: () => invoke<UpdateStatus>("update_status"),
  setUpdateSettings: (request: SetUpdateSettingsRequest) =>
    invoke<UpdateStatus>("set_update_settings", { request }),
  checkForUpdates: (manual: boolean) =>
    invoke<UpdateStatus>("check_for_updates", { request: { manual } }),
  skipUpdateVersion: (version: string | null) =>
    invoke<UpdateStatus>("skip_update_version", { request: { version } }),
  downloadUpdate: (version: string) =>
    invoke<UpdateStatus>("download_update", { request: { version } }),
  discardDownloadedUpdate: (transactionId: string) =>
    invoke<UpdateStatus>("discard_downloaded_update", {
      request: { transactionId },
    }),
  installDownloadedUpdate: (transactionId: string, version: string) =>
    invoke<UpdateStatus>("install_downloaded_update", {
      request: { transactionId, version },
    }),
  acknowledgeUpdateResult: (occurredAtMs: number) =>
    invoke<UpdateStatus>("acknowledge_update_result", {
      request: { occurredAtMs },
    }),
  openUpdateRelease: (version: string) =>
    invoke<void>("open_update_release", { request: { version } }),
  retryPendingRecovery: () => invoke<RecoveryReport>("retry_pending_recovery"),
  codexIntegrationStatus: () =>
    invoke<CodexIntegrationStatus>("codex_integration_status"),
  setCodexIntegrationPaused: (paused: boolean) =>
    invoke<CodexIntegrationStatus>("set_codex_integration_paused", {
      request: { paused },
    }),
  restartCodexIntegration: () =>
    invoke<CodexIntegrationStatus>("restart_codex_integration"),
  restartCodexInstanceIntegration: (instanceId: string) =>
    invoke<CodexIntegrationStatus>("restart_codex_instance_integration", {
      request: { instanceId },
    }),
  defaultIdentityStatus: () =>
    invoke<DefaultIdentityStatus>("default_identity_status"),
  configureDefaultIdentity: (request: ConfigureDefaultIdentityRequest) =>
    invoke<DefaultIdentityStatus>("configure_default_identity", { request }),
  useDefaultIdentity: () =>
    invoke<DefaultIdentityStatus>("use_default_identity"),
  unbindDefaultIdentity: () =>
    invoke<DefaultIdentityStatus>("unbind_default_identity"),
  authorizeWorkspace: (request: AuthorizeWorkspaceRequest = {}) =>
    invoke<Workspace | null>("authorize_workspace", { request }),
  listWorkspaces: () => invoke<Workspace[]>("list_workspaces"),
  disconnectWorkspace: (workspaceId: string) =>
    invoke<UiPreferences>("disconnect_workspace", { request: { workspaceId } }),
  revealWorkspace: (workspaceId: string) =>
    invoke<void>("reveal_workspace", { request: { workspaceId } }),
  uiPreferences: () => invoke<UiPreferences>("ui_preferences"),
  activateWorkspace: (workspaceId: string) =>
    invoke<UiPreferences>("activate_workspace", { request: { workspaceId } }),
  showMainWindow: () => invoke<void>("show_main_window"),
  hideQuickCaptureWindow: () => invoke<void>("hide_quick_capture_window"),
  updateMarkdownEditorQuitState: (request: MarkdownEditorQuitStateRequest) =>
    invoke<void>("update_markdown_editor_quit_state", { request }),
  confirmAndQuit: (request: QuitRequest) => invoke<void>("confirm_and_quit", { request }),
  acknowledgeQuickCaptureQuit: (request: QuitRequest) =>
    invoke<void>("acknowledge_quick_capture_quit", { request }),
  cancelQuitRequest: (request: QuitRequest) =>
    invoke<void>("cancel_quit_request", { request }),
  setActiveSelection: (request: ActiveSelectionRequest) =>
    invoke<UiPreferences>("set_active_selection", { request }),
  setThemePreference: (theme: ThemePreference) =>
    invoke<UiPreferences>("set_theme_preference", { request: { theme } }),
  setSubmitShortcut: (submitShortcut: SubmitShortcut) =>
    invoke<UiPreferences>("set_submit_shortcut", { request: { submitShortcut } }),
  setMarkdownLayout: (markdownLayout: MarkdownLayout) =>
    invoke<UiPreferences>("set_markdown_layout", { request: { markdownLayout } }),
  loadDraft: (request: DraftRequest) =>
    invoke<DraftState>("load_draft", { request }),
  saveDraft: (request: SaveDraftRequest) =>
    invoke<DraftState>("save_draft", { request }),
  loadQuickCaptureDraft: (request: DraftRequest) =>
    invoke<DraftState>("load_quick_capture_draft", { request }),
  saveQuickCaptureDraft: (request: SaveDraftRequest) =>
    invoke<DraftState>("save_quick_capture_draft", { request }),
  createNotebook: (request: CreateNotebookRequest) =>
    invoke<Notebook>("create_notebook", { request }),
  bindExistingNotebook: (request: BindExistingNotebookRequest) =>
    invoke<Notebook | null>("bind_existing_notebook", { request }),
  listNotebooks: (workspaceId: string) =>
    invoke<Notebook[]>("list_notebooks", { workspaceId }),
  previewNotebookNumbering: (request: PreviewNotebookNumberingRequest) =>
    invoke<NotebookNumberingPreview>("preview_notebook_numbering", { request }),
  changeNotebookNumbering: (request: ChangeNotebookNumberingRequest) =>
    invoke<Notebook>("change_notebook_numbering", { request }),
  previewNotebookAttachmentDirectory: (
    request: PreviewNotebookAttachmentDirectoryRequest,
  ) =>
    invoke<NotebookAttachmentDirectoryPreview>(
      "preview_notebook_attachment_directory",
      { request },
    ),
  changeNotebookAttachmentDirectory: (
    request: ChangeNotebookAttachmentDirectoryRequest,
  ) =>
    invoke<Notebook>("change_notebook_attachment_directory", { request }),
  reorderNotebooks: (request: ReorderNotebooksRequest) =>
    invoke<Notebook[]>("reorder_notebooks", { request }),
  renameNotebook: (request: RenameNotebookRequest) =>
    invoke<Notebook>("rename_notebook", { request }),
  workspaceOpenPreference: (workspaceId: string) =>
    invoke<WorkspaceOpenPreference>("workspace_open_preference", {
      request: { workspaceId },
    }),
  setWorkspaceOpenPreference: (request: SetWorkspaceOpenPreferenceRequest) =>
    invoke<WorkspaceOpenPreference>("set_workspace_open_preference", { request }),
  setNotebookPinned: (request: NotebookPinRequest) =>
    invoke<Notebook>("set_notebook_pinned", { request }),
  unbindNotebook: (request: NotebookStateRequest) =>
    invoke<Notebook>("unbind_notebook", { request }),
  rebindNotebook: (request: NotebookStateRequest) =>
    invoke<Notebook>("rebind_notebook", { request }),
  recoverNotebookTarget: (request: NotebookStateRequest) =>
    invoke<Notebook>("recover_notebook_target", { request }),
  inspectNotebookConflict: (request: NotebookStateRequest) =>
    invoke<NotebookConflictInspection>("inspect_notebook_conflict", { request }),
  resolveNotebookConflict: (request: ResolveNotebookConflictRequest) =>
    invoke<Notebook>("resolve_notebook_conflict", { request }),
  convertNotebookToPlain: (request: NotebookStateRequest) =>
    invoke<Notebook>("convert_notebook_to_plain", { request }),
  trashNotebookFile: (request: NotebookStateRequest) =>
    invoke<Notebook>("trash_notebook_file", { request }),
  readNotebookDocument: (request: NotebookDocumentRequest) =>
    invoke<NotebookDocument>("read_notebook_document", { request }),
  saveNotebookDocument: (request: SaveNotebookDocumentRequest) =>
    invoke<NotebookDocument>("save_notebook_document", { request }),
  pickRecordImages: () => invoke<PendingAttachment[]>("pick_record_images"),
  stageRecordImage: (bytes: Uint8Array, knownDigests: string[]) =>
    invoke<PendingAttachment | null>("stage_record_image", bytes, {
      headers: { "x-wakegpt-known-digests": knownDigests.join(",") },
    }),
  discardPendingImage: (token: string) =>
    invoke<void>("discard_pending_image", { token }),
  readPendingImagePreview: (token: string) =>
    invoke<ArrayBuffer>("read_pending_image_preview", { token }),
  readRecordImagePreview: (request: RecordImagePreviewRequest) =>
    invoke<ArrayBuffer>("read_record_image_preview", { request }),
  readMarkdownImagePreview: (request: MarkdownImagePreviewRequest) =>
    invoke<ArrayBuffer>("read_markdown_image_preview", { request }),
  createRecord: (request: CreateRecordRequest) =>
    invoke<RecordItem>("create_record", { request }),
  createQuickCaptureRecord: (request: CreateQuickCaptureRecordRequest) =>
    invoke<RecordItem>("create_quick_capture_record", { request }),
  listRecords: (
    workspaceId: string,
    notebookId: string | null,
    includeTrashed = false,
  ) =>
    invoke<RecordItem[]>("list_records", {
      workspaceId,
      notebookId,
      includeTrashed,
    }),
  updateRecord: (request: UpdateRecordRequest) =>
    invoke<RecordItem>("update_record", { request }),
  trashRecord: (request: RecordRevisionRequest) =>
    invoke<RecordItem>("trash_record", { request }),
  restoreRecord: (request: RecordRevisionRequest) =>
    invoke<RecordItem>("restore_record", { request }),
  migrateRecord: (request: MigrateRecordRequest) =>
    invoke<RecordItem>("migrate_record", { request }),
  setRecordPinned: (request: PinRecordRequest) =>
    invoke<RecordItem>("set_record_pinned", { request }),
  previewManagedMarkdown: (request: PreviewManagedMarkdownRequest) =>
    invoke<string>("preview_managed_markdown", { request }),
};
