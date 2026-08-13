export type NumberingStyle =
  | "none"
  | "numeric"
  | "bullet"
  | "task"
  | "timePrefix"
  | "dateHeadingNumeric";

export type RecordState = "active" | "trashed";

export type SyncState =
  | "local"
  | "queued"
  | "synced"
  | "conflict"
  | "targetUnavailable";

export type NotebookTargetState =
  | "unverified"
  | "ready"
  | "conflict"
  | "unavailable"
  | "unbound";

export type ThemePreference = "system" | "light" | "dark";
export type SubmitShortcut = "enter" | "commandEnter";
export type MarkdownLayout = "edit" | "preview" | "split";

export interface UiPreferences {
  activeWorkspaceId: string | null;
  selectedNotebookId: string | null;
  theme: ThemePreference;
  submitShortcut: SubmitShortcut;
  markdownLayout: MarkdownLayout;
}

export interface WorkspaceOpenPreference {
  workspaceId: string;
  defaultNotebookId: string | null;
  useLastSelection: boolean;
  updatedAtMs: number;
}

export interface AppStatus {
  appVersion: string;
  schemaVersion: number;
  recoveredOperations: number;
  initializedNotebooks: number;
  pendingRecoveryOperations: number;
}

export interface ProductSettings {
  recordTrashRetentionDays: number | null;
  notebookScanIgnoreDirectories: string[];
  protectedNotebookScanIgnoreDirectories: string[];
  codexIntegrationPaused: boolean;
  updatedAtMs: number;
}

export interface RecordTrashCleanupPreview {
  retentionDays: number | null;
  cutoffAtMs: number | null;
  eligibleRecordCount: number;
  eligibleAttachmentCount: number;
  blockedRecordCount: number;
  previewToken: string;
}

export interface RecordTrashCleanupResult {
  deletedRecordCount: number;
  deletedAttachmentCount: number;
}

export interface DataExportResult {
  folderName: string;
  databaseFileName: string;
  workspaceCount: number;
  notebookCount: number;
  recordCount: number;
  exportedAttachmentFiles: number;
  unavailableAttachments: number;
}

export type LocalDataResetBlockerCode =
  | "platformUnsupported"
  | "restartPending"
  | "recoveryPending"
  | "defaultIdentityRunning"
  | "defaultIdentityOccupied"
  | "defaultIdentityUnavailable";

export interface LocalDataResetPreview {
  platformSupported: boolean;
  canReset: boolean;
  blockerCode: LocalDataResetBlockerCode | null;
  confirmationPhrase: string;
  appOwnedItemCount: number;
  workspaceCount: number;
  notebookCount: number;
  recordCount: number;
  attachmentCount: number;
  draftCount: number;
  draftAttachmentCount: number;
  composerReceiptCount: number;
  diagnosticEventCount: number;
  defaultIdentityProfilePresent: boolean;
  pendingRecoveryOperations: number;
  preservesWorkspaceFiles: boolean;
  resetScheduled: boolean;
}

export interface LocalDataResetScheduleResult {
  restartRequested: boolean;
  alreadyScheduled: boolean;
  cancelled: boolean;
}

export interface LocalDataResetNotice {
  outcome: "completed" | "rolledBack";
  occurredAtMs: number;
  movedItemCount: number;
  errorCode: string | null;
}

export type LocalDiagnosticsRetentionDays = 1 | 3 | 7 | 14;
export type LocalDiagnosticsMaxBytes =
  | 1_048_576
  | 5_242_880
  | 10_485_760
  | 20_971_520;
export type LocalDiagnosticSeverity = "info" | "warning" | "error";
export type LocalDiagnosticSubsystem =
  | "app"
  | "recovery"
  | "codex"
  | "card"
  | "default_identity"
  | "lifecycle";
export type LocalDiagnosticContextValue =
  | string
  | number
  | boolean
  | null
  | LocalDiagnosticContextValue[]
  | { [key: string]: LocalDiagnosticContextValue };

export interface LocalDiagnosticsStatus {
  available: boolean;
  enabled: boolean;
  retentionDays: LocalDiagnosticsRetentionDays;
  maxBytes: LocalDiagnosticsMaxBytes;
  eventCount: number;
  databaseBytes: number;
  earliestAtMs: number | null;
  latestAtMs: number | null;
  lastErrorCode: string | null;
}

export interface LocalDiagnosticEvent {
  id: number;
  firstAtMs: number;
  lastAtMs: number;
  severity: LocalDiagnosticSeverity;
  subsystem: LocalDiagnosticSubsystem;
  code: string;
  context: LocalDiagnosticContextValue;
  occurrenceCount: number;
}

export interface LocalDiagnosticsPage {
  events: LocalDiagnosticEvent[];
  nextCursor: number | null;
}

export interface LocalDiagnosticsExportResult {
  folderName: string;
  eventCount: number;
}

export interface LoginItemStatus {
  enabled: boolean;
}

export type UpdateCheckIntervalHours = 6 | 12 | 24 | 48;
export type UpdatePhase =
  | "unavailable"
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "downloading"
  | "verifying"
  | "readyToInstall"
  | "installing"
  | "restarting"
  | "installed"
  | "rolledBack"
  | "failed";

export type UpdateInstallOutcome = "installed" | "rolledBack" | "rollbackFailed";

export interface UpdateDownloadProgress {
  schemaVersion: 1;
  transactionId: string;
  version: string;
  downloadedBytes: number;
  totalBytes: number | null;
  verifying: boolean;
}

export interface UpdateStatus {
  configured: boolean;
  configurationErrorCode: string | null;
  currentVersion: string;
  target: string;
  externalNetworkEnabled: boolean;
  automaticChecksEnabled: boolean;
  automaticDownloadsEnabled: boolean;
  checkIntervalHours: UpdateCheckIntervalHours;
  skippedVersion: string | null;
  lastCheckedAtMs: number | null;
  lastErrorCode: string | null;
  phase: UpdatePhase;
  availableVersion: string | null;
  availableIsSkipped: boolean;
  releaseNotes: string | null;
  publishedAt: string | null;
  operationErrorCode: string | null;
  downloadedBytes: number;
  downloadTotalBytes: number | null;
  preparedTransactionId: string | null;
  installOutcome: UpdateInstallOutcome | null;
  installFromVersion: string | null;
  installVersion: string | null;
  installErrorCode: string | null;
  installOccurredAtMs: number | null;
  releasePageUrl: string | null;
}

export interface RecoveryIssue {
  operationId: string;
  recordId: string;
  code: string;
  attemptCount: number;
  previousErrorCode: string | null;
}

export interface RecoveryReport {
  recoveredOperations: number;
  recoveredAttachmentOperations: number;
  initializedNotebooks: number;
  pending: RecoveryIssue[];
  pendingNotebooks: NotebookRecoveryIssue[];
  pendingAttachmentOperations: number;
}

export interface NotebookRecoveryIssue {
  notebookId: string;
  code: string;
}

export interface CodexIntegrationStatus {
  paused: boolean;
  adapterVersion: string;
  supportedCodexVersion: string;
  supportedCodexBuild: string;
  endpointState:
    | "notDetected"
    | "staleEndpoint"
    | "unmanagedTargetAvailable"
    | "unmanagedTargetIncompatible"
    | "connecting"
    | "connected"
    | "partiallyConnected"
    | "paused"
    | "restartRequired"
    | "injectionFailed";
  lastErrorCode: string | null;
  cardScriptSha256: string;
  detectedInstanceCount: number;
  connectableInstanceCount: number;
  availableTargetCount: number;
  connectedTargetCount: number;
  failedTargetCount: number;
  instances: CodexInstanceStatus[];
}

export interface DefaultIdentityStatus {
  configured: boolean;
  alias: string | null;
  locked: boolean;
  platformSupported: boolean;
  profilePresent: boolean;
  runtimeState: "stopped" | "running" | "occupied" | "unavailable";
  lastErrorCode: string | null;
  createdAtMs: number | null;
  updatedAtMs: number | null;
}

export interface CodexInstanceStatus {
  id: string;
  displayName: string;
  isDefault: boolean;
  canRestart: boolean;
  detectedCodexVersion: string | null;
  detectedCodexBuild: string | null;
  state:
    | "unavailable"
    | "incompatible"
    | "available"
    | "connecting"
    | "connected"
    | "partiallyConnected"
    | "failed"
    | "paused";
  availableTargetCount: number;
  connectedTargetCount: number;
  failedTargetCount: number;
  lastErrorCode: string | null;
}

export interface Workspace {
  id: string;
  displayName: string;
  rootPath: string;
  createdAtMs: number;
  updatedAtMs: number;
}

export interface Notebook {
  id: string;
  workspaceId: string;
  targetId: string;
  displayName: string;
  relativePath: string;
  ordinal: number;
  isPinned: boolean;
  numberingStyle: NumberingStyle;
  numberingStart: number;
  numberingSyncPending: boolean;
  attachmentDirectory: string;
  previousAttachmentDirectory: string | null;
  attachmentDirectorySyncPending: boolean;
  targetState: NotebookTargetState;
  lastErrorCode: string | null;
  createdAtMs: number;
  updatedAtMs: number;
}

export interface NotebookNumberingPreview {
  notebookId: string;
  currentNumberingStyle: NumberingStyle;
  currentNumberingStart: number;
  nextNumberingStyle: NumberingStyle;
  nextNumberingStart: number;
  markdown: string;
  expectedFileSha256: string;
  expectedReceiptGeneration: number;
}

export interface NotebookAttachmentDirectoryPreview {
  notebookId: string;
  currentAttachmentDirectory: string;
  nextAttachmentDirectory: string;
  attachmentCount: number;
  markdown: string;
  expectedFileSha256: string;
  expectedReceiptGeneration: number;
}

export interface Attachment {
  id: string;
  recordId: string;
  mediaType: string;
  managedRelativePath: string;
  contentSha256: string;
  byteSize: number;
  createdAtMs: number;
  fileState:
    | "ready"
    | "trashed"
    | "missing"
    | "preservedShared"
    | "modified"
    | "recoveryRequired";
  previousManagedRelativePath: string | null;
  relocationState: "ready" | "pending" | "cleanupPending" | "conflict";
  relocationErrorCode: string | null;
}

export interface PendingAttachment {
  token: string;
  mediaType: string;
  byteSize: number;
  contentSha256: string;
  displayName: string;
}

export interface DraftState {
  bodyMarkdown: string;
  attachments: PendingAttachment[];
}

export interface NotebookDocument {
  notebookId: string;
  markdown: string;
  fileSha256: string;
  receiptGeneration: number;
  lineEnding: "\n" | "\r\n" | "\r";
  hadUtf8Bom: boolean;
}

export type NotebookConflictResolutionAction =
  | "adoptWakegpt"
  | "adoptFile"
  | "unbind";

export interface NotebookConflictActionAvailability {
  available: boolean;
  blockerCode: string | null;
}

export interface NotebookConflictRecordDiff {
  recordId: string;
  wakegptMarkdown: string;
  fileMarkdown: string | null;
  state: "unchanged" | "modified" | "invalid";
}

export interface NotebookConflictInspection {
  notebookId: string;
  reasonCode: string;
  detectedAtMs: number;
  conflictToken: string;
  fileMarkdown: string;
  wakegptMarkdown: string;
  recordDiffs: NotebookConflictRecordDiff[];
  adoptWakegpt: NotebookConflictActionAvailability;
  adoptFile: NotebookConflictActionAvailability;
  unbind: NotebookConflictActionAvailability;
}

export interface RecordItem {
  id: string;
  workspaceId: string;
  notebookId: string | null;
  bodyMarkdown: string;
  createdAtMs: number;
  updatedAtMs: number;
  logicalOrder: number;
  state: RecordState;
  syncState: SyncState;
  revision: number;
  appliedRevision: number;
  trashedAtMs: number | null;
  isPinned: boolean;
  attachments: Attachment[];
}
