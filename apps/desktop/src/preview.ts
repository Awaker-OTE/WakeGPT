import type { InvokeArgs } from "@tauri-apps/api/core";
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
  LocalDiagnosticEvent,
  LocalDiagnosticsExportResult,
  LocalDiagnosticsMaxBytes,
  LocalDiagnosticsPage,
  LocalDiagnosticsRetentionDays,
  LocalDiagnosticsStatus,
  Notebook,
  NotebookAttachmentDirectoryPreview,
  NotebookConflictInspection,
  NotebookConflictResolutionAction,
  NotebookDocument,
  NotebookNumberingPreview,
  PendingAttachment,
  ProductSettings,
  RecordItem,
  RecordTrashCleanupPreview,
  RecordTrashCleanupResult,
  RecoveryReport,
  UiPreferences,
  WorkspaceOpenPreference,
  Workspace,
} from "./domain";
import type {
  CreateNotebookRequest,
  CreateRecordRequest,
  BindExistingNotebookRequest,
  MigrateRecordRequest,
  RecordRevisionRequest,
  UpdateRecordRequest,
} from "./bridge";

const now = Date.now();
const previewParameters = new URLSearchParams(window.location.search);
const longContentPreviewEnabled = previewParameters.has("long-content");
const largeMarkdownPreviewEnabled = previewParameters.has("large-markdown");
const delayedMarkdownSaveEnabled = previewParameters.has("delayed-markdown-save");
const crlfMarkdownPreviewEnabled = previewParameters.has("crlf-markdown");
const crMarkdownPreviewEnabled = previewParameters.has("cr-markdown");
const longPreviewWorkspaceName = `超长工作区名称_${"产品规划与研发协同记录".repeat(10)}`.slice(0, 120);
const longPreviewNotebookName = `超长速记本名称_${"跨团队产品需求讨论与技术验证".repeat(10)}`.slice(0, 120);
const longPreviewPath = `notes/${Array.from(
  { length: 8 },
  (_, index) => `segment-${index}-${"x".repeat(72)}`,
).join("/")}/document.md`;
let previewProductSettings: ProductSettings = {
  recordTrashRetentionDays: 30,
  notebookScanIgnoreDirectories: [
    ".git", ".hg", ".svn", ".wakegpt", ".next", "node_modules", "target", "dist", "build",
  ],
  protectedNotebookScanIgnoreDirectories: [
    ".git", ".hg", ".svn", ".wakegpt", ".next", "node_modules", "target", "dist", "build",
  ],
  codexIntegrationPaused: false,
  updatedAtMs: now,
};
const workspace: Workspace = {
  id: "018f1111-1111-7111-8111-111111111111",
  displayName: longContentPreviewEnabled ? longPreviewWorkspaceName : "产品工作区",
  rootPath: longContentPreviewEnabled ? `/example/${longPreviewPath}` : "/example/产品工作区",
  createdAtMs: now - 86_400_000,
  updatedAtMs: now,
};

let notebooks: Notebook[] = [
  previewNotebook(
    "018f2222-2222-7222-8222-222222222222",
    longContentPreviewEnabled ? longPreviewNotebookName : "产品记录",
    longContentPreviewEnabled ? longPreviewPath : "notes/产品记录.md",
    "numeric",
    1,
  ),
  previewNotebook("018f3333-3333-7333-8333-333333333333", "灵感", "notes/灵感.md", "bullet", 2),
  previewNotebook("018f4444-4444-7444-8444-444444444444", "待办", "notes/待办.md", "task", 3),
];

let records: RecordItem[] = [
  previewRecord(
    "018f5555-5555-7555-8555-555555555555",
    longContentPreviewEnabled
      ? `超长记录正文：${"中文内容与 Markdown 链接 https://example.invalid/path。".repeat(24)}\n\n${"UNBROKEN0123456789".repeat(32)}`
      : "用户希望支持导出为 PNG 与 PDF，优先级较高。",
    now - 1_800_000,
    1024,
  ),
  previewRecord(
    "018f6666-6666-7666-8666-666666666666",
    "考虑在筛选条件中加入时间范围快捷选项（今天 / 近 7 天 / 近 30 天）。",
    now - 68_000_000,
    2048,
  ),
  previewRecord(
    "018f7777-7777-7777-8777-777777777777",
    "灵感：引入「模板中心」，让新建页面更高效。",
    now - 108_000_000,
    3072,
  ),
];
records[0].attachments = [{
  id: "018f9999-9999-7999-8999-999999999999",
  recordId: records[0].id,
  mediaType: "image/png",
  managedRelativePath: `notes/attachments/产品记录/bb/${"b".repeat(64)}.png`,
  contentSha256: "b".repeat(64),
  byteSize: 184_320,
  createdAtMs: now - 1_790_000,
  fileState: "ready",
  previousManagedRelativePath: null,
  relocationState: "ready",
  relocationErrorCode: null,
}];

const conflictPreviewEnabled = previewParameters.has("conflict");
if (conflictPreviewEnabled) {
  notebooks[0].targetState = "conflict";
  notebooks[0].lastErrorCode = "managed_region_changed";
}

let integrationPaused = false;
const quickCaptureDrafts = new Map<string, DraftState>();
let defaultPreviewIntegrationEnabled = false;
let customPreviewIntegrationEnabled = false;
let previewLoginItemEnabled = false;
let previewMarkdownSaveCount = 0;
let previewQuitCount = 0;
let workspaceConnected = true;
let previewLocalDiagnosticsEnabled = true;
let previewLocalDiagnosticsRetentionDays: LocalDiagnosticsRetentionDays = 14;
let previewLocalDiagnosticsMaxBytes: LocalDiagnosticsMaxBytes = 20_971_520;
let previewLocalDiagnosticEvents: LocalDiagnosticEvent[] = Array.from(
  { length: 56 },
  (_, index) => {
    const id = 56 - index;
    const lastAtMs = now - index * 90_000;
    const templates = [
      {
        severity: "info",
        subsystem: "app",
        code: "app_started",
        context: { schemaVersion: 21, recoveredOperations: 0, pendingOperations: 0 },
      },
      {
        severity: "info",
        subsystem: "codex",
        code: "codex_state_changed",
        context: {
          state: "connected",
          detectedInstances: 3,
          connectableInstances: 2,
          availableTargets: 3,
          connectedTargets: 3,
          failedTargets: 0,
        },
      },
      {
        severity: "info",
        subsystem: "card",
        code: "card_visibility_changed",
        context: { visibility: "visible", layout: "compact" },
      },
      {
        severity: "warning",
        subsystem: "recovery",
        code: "recovery_pending",
        context: { recoveredOperations: 1, pendingOperations: 1 },
      },
    ] as const;
    const template = templates[index % templates.length];
    const occurrenceCount = index % 9 === 0 ? 3 : 1;
    return {
      id,
      firstAtMs: lastAtMs - (occurrenceCount - 1) * 650,
      lastAtMs,
      severity: template.severity,
      subsystem: template.subsystem,
      code: template.code,
      context: template.context,
      occurrenceCount,
    };
  },
);
let previewDefaultIdentity: DefaultIdentityStatus = {
  configured: true,
  alias: "官方工作身份",
  locked: true,
  platformSupported: true,
  profilePresent: true,
  runtimeState: "stopped",
  lastErrorCode: null,
  createdAtMs: now - 43_200_000,
  updatedAtMs: now - 43_200_000,
};
const pendingPreviewAttachment: PendingAttachment = {
  token: "018f8888-8888-7888-8888-888888888888",
  mediaType: "image/png",
  byteSize: 184_320,
  contentSha256: "a".repeat(64),
  displayName: "界面参考.png",
};
const previewImageBytes = Uint8Array.from(
  atob("iVBORw0KGgoAAAANSUhEUgAAAUAAAAC0CAYAAADl5PURAAAETElEQVR42u3U5UEbYBAAULase4PbBEUKtEBwW69Yko8II1zXuEvej7fCm3vTaUF+v7pBEf+eqGJOLgJEgAJEgAhQgAgQAQoQASJAASJABChABIgABYgAEaAAESACFCACRIACRIACRIAIUIAkD/BtpwX5bXWDIu6fqEKAAkSAAkSACHD2ApxvQX5bJ0ER989UIUABIkABIkAEKEAEiAAFiAARoAARIAIUIAJEgAJEgAhQgAgQARYO8N18C/LbPgmKeHimCgEKEAEKEAEiQAEiQAQoQASIAAWIABHglAa40IL8tk+DIh56VCFAASJAASJABChABIgABYgAEaAAESACnM4A3y+0IL+d06CIxx5VCFCACFCACBABChABIkABIkAEKEAEiAAFiAARoAARIAIUIAJEgJUDXGxBfjtnQRGPfaoQoAARoAARIAKcuQA/LLYgv92zoIinPlUIUIAIUIAIEAEKEAEiQAEiQAQoQASIAAWIABGgABEgAhQgAkSAAkSACFCACFCACBABCpDsAX5cakF+v8+DIp4HVCFAASJAASJABChABIgABYgAEaAAESACFCACRIACRIAIUIAIEAEKEAEiQAEiQAEiQAQoQLIH+GmpBfntnQdF9AZUIUABIkABIkAEOHsBLrcgv72LoIheowoBChABChABIkABIkAEKEAEiAAFiAARoAARIAIUIAJEgAJEgAiwcICfl1uQ3/5FUES/UYUABYgABYgAEaAAESACFCACRIACRIAIcEoDXGlBfvuXQRH9F6oQoAARoAARIAIUIAJEgAJEgAhQgAgQAU5ngF9WWpDfwWVQxOCFKgQoQAQoQASIAAWIABGgABEgAhQgAkSAAkSACFCACBABChABIsDKAa62IL+Dq6CIwZAqBChABChABIgAZy7Ar6styO/PVVBEG1KFAAWIAAWIABGgABEgAhQgAkSAAkSACFCACBABChABIkABIkAEKEAEiAAFiAAFiAARoADJHuC3tRbk9/c6KOJlRBUCFCACFCACRIACRIAIUIAIEAEKEAEiQAEiQAQoQASIAAWIABGgABEgAhQgAhQgAkSAAiR7gN/XWpDf4XVQxHBEFQIUIAIUIAJEgLMX4HoL8ju8CYoYjqlCgAJEgAJEgAhQgAgQAQoQASJAASJABChABIgABYgAEaAAESACLBzgj/UW5Hd0ExQxGlOFAAWIAAWIABGgABEgAhQgAkSAAkSACHBKA9xoQX5Ht0ERowlVCFCACFCACBABChABIkABIkAEKEAEiACnM8CfGy3I7/g2KGI8oQoBChABChABIkABIkAEKEAEiAAFiAARoAARIAIUIAJEgAJEgAiwcoCbLcjv+C4oYvxKFQIUIAIUIAJEgDMXYGezBfl174IiJq9UIUABIkABIkAEKEAEiAAFiAARoAARIAIUIAJEgAJEgAhQgAgQAQoQASJAASJAASJABChAkvsPIeu7C1dWdkwAAAAASUVORK5CYII="),
  (character) => character.charCodeAt(0),
).buffer;
const previewMarkdownImageBytes = (() => {
  const image = new Uint8Array(previewImageBytes);
  const framed = new Uint8Array(6 + image.byteLength);
  framed.set([0x57, 0x47, 0x4d, 0x49, 1, 1]);
  framed.set(image, 6);
  return framed.buffer;
})();
const previewStagedImages = new Map<string, string>();
const notebookDocuments = new Map<string, NotebookDocument>();
const drafts = new Map<string, DraftState>();
let uiPreferences: UiPreferences = {
  activeWorkspaceId: workspace.id,
  selectedNotebookId: notebooks[0].id,
  theme: "system",
  submitShortcut: "enter",
  markdownLayout: "split",
};
const workspaceOpenPreferences = new Map<string, WorkspaceOpenPreference>();

function previewNotebook(
  id: string,
  displayName: string,
  relativePath: string,
  numberingStyle: Notebook["numberingStyle"],
  ordinal: number,
  numberingStart = 1,
  attachmentDirectory?: string,
): Notebook {
  const parent = relativePath.includes("/")
    ? relativePath.slice(0, relativePath.lastIndexOf("/"))
    : "";
  const stem = relativePath.split("/").pop()?.replace(/\.md$/i, "") || "notebook";
  return {
    id,
    workspaceId: workspace.id,
    targetId: crypto.randomUUID(),
    displayName,
    relativePath,
    ordinal,
    isPinned: true,
    numberingStyle,
    numberingStart,
    numberingSyncPending: false,
    attachmentDirectory: attachmentDirectory || `${parent ? `${parent}/` : ""}attachments/${stem}`,
    previousAttachmentDirectory: null,
    attachmentDirectorySyncPending: false,
    targetState: "ready",
    lastErrorCode: null,
    createdAtMs: now - 80_000_000,
    updatedAtMs: now,
  };
}

function previewRecord(
  id: string,
  bodyMarkdown: string,
  createdAtMs: number,
  logicalOrder: number,
): RecordItem {
  return {
    id,
    workspaceId: workspace.id,
    notebookId: notebooks[0].id,
    bodyMarkdown,
    createdAtMs,
    updatedAtMs: createdAtMs,
    logicalOrder,
    state: "active",
    syncState: "synced",
    revision: 1,
    appliedRevision: 1,
    trashedAtMs: null,
    isPinned: false,
    attachments: [],
  };
}

function payload(args?: InvokeArgs): Record<string, unknown> {
  if (!args || Array.isArray(args)) return {};
  return args as Record<string, unknown>;
}

function previewImageDigest(args?: InvokeArgs): string {
  const bytes = args instanceof ArrayBuffer
    ? new Uint8Array(args)
    : args instanceof Uint8Array
      ? args
      : new Uint8Array();
  // space-seed: preview-only deterministic byte key; production SHA-256 remains Rust-owned.
  let value = 2_166_136_261;
  for (const byte of bytes) value = Math.imul(value ^ byte, 16_777_619) >>> 0;
  return value.toString(16).padStart(8, "0").repeat(8);
}

function request<T>(args?: InvokeArgs): T {
  return payload(args).request as T;
}

function findRecord(id: string): RecordItem {
  const record = records.find((item) => item.id === id);
  if (!record) throw new Error("Preview record not found");
  return record;
}

export function previewInvoke(command: string, args?: InvokeArgs): unknown {
  switch (command) {
    case "update_markdown_editor_quit_state":
      return undefined;
    case "acknowledge_quick_capture_quit":
    case "cancel_quit_request":
      return undefined;
    case "confirm_and_quit":
      previewQuitCount += 1;
      return undefined;
    case "preview_quit_count":
      return previewQuitCount;
    case "app_status":
      return {
        appVersion: "0.1.0",
        schemaVersion: 21,
        recoveredOperations: 0,
        initializedNotebooks: 3,
        pendingRecoveryOperations: 0,
      } satisfies AppStatus;
    case "product_settings":
      return structuredClone(previewProductSettings);
    case "set_product_settings": {
      const next = request<{
        recordTrashRetentionDays: number | null;
        notebookScanIgnoreDirectories: string[];
      }>(args);
      previewProductSettings = {
        ...previewProductSettings,
        recordTrashRetentionDays: next.recordTrashRetentionDays,
        notebookScanIgnoreDirectories: [...next.notebookScanIgnoreDirectories],
        updatedAtMs: Date.now(),
      };
      return structuredClone(previewProductSettings);
    }
    case "workspace_open_preference": {
      const draft = request<{ workspaceId: string }>(args);
      return structuredClone(workspaceOpenPreferences.get(draft.workspaceId) ?? {
        workspaceId: draft.workspaceId,
        defaultNotebookId: null,
        useLastSelection: true,
        updatedAtMs: 0,
      } satisfies WorkspaceOpenPreference);
    }
    case "set_workspace_open_preference": {
      const draft = request<{
        workspaceId: string;
        defaultNotebookId: string | null;
        useLastSelection: boolean;
      }>(args);
      const preference: WorkspaceOpenPreference = {
        ...draft,
        updatedAtMs: Date.now(),
      };
      workspaceOpenPreferences.set(draft.workspaceId, preference);
      return structuredClone(preference);
    }
    case "preview_record_trash_cleanup": {
      const eligible = records.filter((record) => record.state === "trashed");
      return {
        retentionDays: previewProductSettings.recordTrashRetentionDays,
        cutoffAtMs: Date.now() - 30 * 86_400_000,
        eligibleRecordCount: eligible.length,
        eligibleAttachmentCount: eligible.reduce(
          (total, record) => total + record.attachments.length,
          0,
        ),
        blockedRecordCount: 0,
        previewToken: "a".repeat(64),
      } satisfies RecordTrashCleanupPreview;
    }
    case "purge_record_trash": {
      const previous = records.filter((record) => record.state === "trashed");
      records = records.filter((record) => record.state !== "trashed");
      return {
        deletedRecordCount: previous.length,
        deletedAttachmentCount: previous.reduce(
          (total, record) => total + record.attachments.length,
          0,
        ),
      } satisfies RecordTrashCleanupResult;
    }
    case "local_diagnostics_status":
      return previewLocalDiagnosticsStatus();
    case "list_local_diagnostics": {
      const { beforeId, limit } = request<{ beforeId: number | null; limit: number }>(args);
      if (!Number.isInteger(limit) || limit < 1 || limit > 100) {
        throw new Error("Preview local diagnostics page is invalid");
      }
      const candidates = beforeId === null
        ? previewLocalDiagnosticEvents
        : previewLocalDiagnosticEvents.filter((event) => event.id < beforeId);
      const hasMore = candidates.length > limit;
      const events = candidates.slice(0, limit);
      return structuredClone({
        events,
        nextCursor: hasMore ? events[events.length - 1]?.id ?? null : null,
      } satisfies LocalDiagnosticsPage);
    }
    case "update_local_diagnostics_settings": {
      const next = request<{
        enabled: boolean;
        retentionDays: LocalDiagnosticsRetentionDays;
        maxBytes: LocalDiagnosticsMaxBytes;
      }>(args);
      if (![1, 3, 7, 14].includes(next.retentionDays)
        || ![1_048_576, 5_242_880, 10_485_760, 20_971_520].includes(next.maxBytes)) {
        throw new Error("Preview local diagnostics settings are invalid");
      }
      previewLocalDiagnosticsEnabled = next.enabled;
      previewLocalDiagnosticsRetentionDays = next.retentionDays;
      previewLocalDiagnosticsMaxBytes = next.maxBytes;
      return previewLocalDiagnosticsStatus();
    }
    case "export_local_diagnostics":
      return {
        folderName: "WakeGPT-diagnostics-preview",
        eventCount: previewLocalDiagnosticEvents.length,
      } satisfies LocalDiagnosticsExportResult;
    case "clear_local_diagnostics":
      previewLocalDiagnosticEvents = [];
      return previewLocalDiagnosticsStatus();
    case "export_local_data":
      return {
        folderName: "WakeGPT-export-preview",
        databaseFileName: "wakegpt.sqlite3",
        workspaceCount: 1,
        notebookCount: notebooks.length,
        recordCount: records.length,
        exportedAttachmentFiles: 1,
        unavailableAttachments: 0,
      } satisfies DataExportResult;
    case "local_data_reset_preview":
      return {
        platformSupported: true,
        canReset: previewDefaultIdentity.runtimeState === "stopped",
        blockerCode: previewDefaultIdentity.runtimeState === "stopped"
          ? null
          : "defaultIdentityRunning",
        confirmationPhrase: "清除 WakeGPT",
        appOwnedItemCount: 6,
        workspaceCount: workspaceConnected ? 1 : 0,
        notebookCount: notebooks.length,
        recordCount: records.length,
        attachmentCount: records.reduce(
          (count, record) => count + record.attachments.length,
          0,
        ),
        draftCount: drafts.size,
        draftAttachmentCount: [...drafts.values()].reduce(
          (count, draft) => count + draft.attachments.length,
          0,
        ),
        composerReceiptCount: 1,
        diagnosticEventCount: previewLocalDiagnosticEvents.length,
        defaultIdentityProfilePresent: previewDefaultIdentity.profilePresent,
        pendingRecoveryOperations: 0,
        preservesWorkspaceFiles: true,
        resetScheduled: false,
      } satisfies LocalDataResetPreview;
    case "local_data_reset_notice":
      return null satisfies LocalDataResetNotice | null;
    case "acknowledge_local_data_reset_notice":
      return true;
    case "reset_local_data": {
      const reset = request<{ confirmationPhrase: string }>(args);
      if (reset.confirmationPhrase !== "清除 WakeGPT") {
        throw new Error("确认文字不匹配");
      }
      return {
        restartRequested: false,
        alreadyScheduled: false,
        cancelled: true,
      } satisfies LocalDataResetScheduleResult;
    }
    case "login_item_status":
      return { enabled: previewLoginItemEnabled } satisfies LoginItemStatus;
    case "set_login_item_enabled":
      previewLoginItemEnabled = request<{ enabled: boolean }>(args).enabled;
      return { enabled: previewLoginItemEnabled } satisfies LoginItemStatus;
    case "codex_integration_status":
      return previewIntegrationStatus();
    case "set_codex_integration_paused": {
      integrationPaused = request<{ paused: boolean }>(args).paused;
      previewProductSettings = {
        ...previewProductSettings,
        codexIntegrationPaused: integrationPaused,
      };
      return previewIntegrationStatus();
    }
    case "restart_codex_integration":
      integrationPaused = false;
      previewProductSettings = {
        ...previewProductSettings,
        codexIntegrationPaused: false,
      };
      defaultPreviewIntegrationEnabled = true;
      return previewIntegrationStatus();
    case "restart_codex_instance_integration": {
      integrationPaused = false;
      const { instanceId } = request<{ instanceId: string }>(args);
      if (instanceId === "chatgpt-preview-custom-unavailable") {
        customPreviewIntegrationEnabled = true;
      }
      return previewIntegrationStatus();
    }
    case "default_identity_status":
      return structuredClone(previewDefaultIdentity);
    case "configure_default_identity": {
      const draft = request<{ alias: string; locked: boolean }>(args);
      previewDefaultIdentity = {
        ...previewDefaultIdentity,
        configured: true,
        alias: draft.alias.trim(),
        locked: draft.locked,
        profilePresent: true,
        updatedAtMs: Date.now(),
      };
      return structuredClone(previewDefaultIdentity);
    }
    case "use_default_identity":
      previewDefaultIdentity.runtimeState = "running";
      return structuredClone(previewDefaultIdentity);
    case "unbind_default_identity":
      previewDefaultIdentity = {
        ...previewDefaultIdentity,
        configured: false,
        alias: null,
        locked: false,
        createdAtMs: null,
        updatedAtMs: null,
      };
      return structuredClone(previewDefaultIdentity);
    case "list_workspaces":
      return structuredClone(workspaceConnected ? [workspace] : []);
    case "disconnect_workspace":
      workspaceConnected = false;
      uiPreferences.activeWorkspaceId = null;
      uiPreferences.selectedNotebookId = null;
      return structuredClone(uiPreferences);
    case "reveal_workspace":
    case "show_main_window":
    case "hide_quick_capture_window":
      return undefined;
    case "ui_preferences":
      return structuredClone(uiPreferences);
    case "activate_workspace": {
      const workspaceId = request<{ workspaceId: string }>(args).workspaceId;
      const preference = workspaceOpenPreferences.get(workspaceId);
      uiPreferences.activeWorkspaceId = workspaceId;
      if (preference && !preference.useLastSelection) {
        uiPreferences.selectedNotebookId = preference.defaultNotebookId;
      }
      return structuredClone(uiPreferences);
    }
    case "set_active_selection": {
      const selection = request<{ workspaceId: string; notebookId: string | null }>(args);
      uiPreferences.activeWorkspaceId = selection.workspaceId;
      uiPreferences.selectedNotebookId = selection.notebookId;
      if (selection.notebookId) {
        const selected = notebooks.find((notebook) => notebook.id === selection.notebookId);
        if (selected) selected.isPinned = true;
      }
      return structuredClone(uiPreferences);
    }
    case "set_theme_preference":
      uiPreferences.theme = request<{ theme: UiPreferences["theme"] }>(args).theme;
      return structuredClone(uiPreferences);
    case "set_submit_shortcut":
      uiPreferences.submitShortcut = request<{
        submitShortcut: UiPreferences["submitShortcut"];
      }>(args).submitShortcut;
      return structuredClone(uiPreferences);
    case "set_markdown_layout":
      uiPreferences.markdownLayout = request<{
        markdownLayout: UiPreferences["markdownLayout"];
      }>(args).markdownLayout;
      return structuredClone(uiPreferences);
    case "load_draft": {
      const selection = request<{ workspaceId: string; notebookId: string | null }>(args);
      const key = `${selection.workspaceId}:${selection.notebookId ?? "inbox"}`;
      return structuredClone(drafts.get(key) ?? { bodyMarkdown: "", attachments: [] });
    }
    case "save_draft": {
      const draft = request<{
        workspaceId: string;
        notebookId: string | null;
        bodyMarkdown: string;
        attachments: PendingAttachment[];
      }>(args);
      const key = `${draft.workspaceId}:${draft.notebookId ?? "inbox"}`;
      const saved = { bodyMarkdown: draft.bodyMarkdown, attachments: draft.attachments };
      drafts.set(key, structuredClone(saved));
      return structuredClone(saved);
    }
    case "load_quick_capture_draft": {
      const selection = request<{ workspaceId: string; notebookId: string | null }>(args);
      const key = `${selection.workspaceId}:${selection.notebookId ?? "inbox"}`;
      return structuredClone(quickCaptureDrafts.get(key) ?? { bodyMarkdown: "", attachments: [] });
    }
    case "save_quick_capture_draft": {
      const draft = request<{
        workspaceId: string;
        notebookId: string | null;
        bodyMarkdown: string;
        attachments: PendingAttachment[];
      }>(args);
      const key = `${draft.workspaceId}:${draft.notebookId ?? "inbox"}`;
      const saved = { bodyMarkdown: draft.bodyMarkdown, attachments: draft.attachments };
      quickCaptureDrafts.set(key, structuredClone(saved));
      return structuredClone(saved);
    }
    case "authorize_workspace":
      workspaceConnected = true;
      return structuredClone(workspace);
    case "list_notebooks":
      return structuredClone(notebooks);
    case "reorder_notebooks": {
      const ordered = request<{ orderedNotebookIds: string[] }>(args).orderedNotebookIds;
      notebooks = ordered
        .map((id) => notebooks.find((notebook) => notebook.id === id))
        .filter((notebook): notebook is Notebook => Boolean(notebook))
        .map((notebook, index) => ({ ...notebook, ordinal: (index + 1) * 1024 }));
      return structuredClone(notebooks);
    }
    case "set_notebook_pinned": {
      const draft = request<{ notebookId: string; pinned: boolean }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.isPinned = draft.pinned;
      if (!draft.pinned && uiPreferences.selectedNotebookId === notebook.id) {
        uiPreferences.selectedNotebookId = null;
      }
      const preference = workspaceOpenPreferences.get(notebook.workspaceId);
      if (!draft.pinned && preference?.defaultNotebookId === notebook.id) {
        workspaceOpenPreferences.set(notebook.workspaceId, {
          ...preference,
          defaultNotebookId: null,
          useLastSelection: false,
          updatedAtMs: Date.now(),
        });
      }
      return structuredClone(notebook);
    }
    case "preview_notebook_numbering": {
      const draft = request<{
        notebookId: string;
        numberingStyle: Notebook["numberingStyle"];
        numberingStart: number;
      }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      const preview: NotebookNumberingPreview = {
        notebookId: notebook.id,
        currentNumberingStyle: notebook.numberingStyle,
        currentNumberingStart: notebook.numberingStart,
        nextNumberingStyle: draft.numberingStyle,
        nextNumberingStart: draft.numberingStart,
        markdown: previewNumberedMarkdown(notebook, draft.numberingStyle, draft.numberingStart),
        expectedFileSha256: "0".repeat(64),
        expectedReceiptGeneration: 1,
      };
      return structuredClone(preview);
    }
    case "change_notebook_numbering": {
      const draft = request<{
        notebookId: string;
        numberingStyle: Notebook["numberingStyle"];
        numberingStart: number;
      }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.numberingStyle = draft.numberingStyle;
      notebook.numberingStart = draft.numberingStart;
      notebook.numberingSyncPending = false;
      notebook.updatedAtMs = Date.now();
      const document: NotebookDocument = {
        notebookId: notebook.id,
        markdown: previewNumberedMarkdown(notebook, draft.numberingStyle, draft.numberingStart),
        fileSha256: "1".repeat(64),
        receiptGeneration: 2,
        lineEnding: "\n",
        hadUtf8Bom: false,
      };
      notebookDocuments.set(notebook.id, document);
      return structuredClone(notebook);
    }
    case "preview_notebook_attachment_directory": {
      const draft = request<{ notebookId: string; attachmentDirectory: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      const preview: NotebookAttachmentDirectoryPreview = {
        notebookId: notebook.id,
        currentAttachmentDirectory: notebook.attachmentDirectory,
        nextAttachmentDirectory: draft.attachmentDirectory,
        attachmentCount: records
          .filter((record) => record.notebookId === notebook.id && record.state === "active")
          .reduce((count, record) => count + record.attachments.length, 0),
        markdown: previewNumberedMarkdown(
          notebook,
          notebook.numberingStyle,
          notebook.numberingStart,
        ),
        expectedFileSha256: "2".repeat(64),
        expectedReceiptGeneration: 2,
      };
      return structuredClone(preview);
    }
    case "change_notebook_attachment_directory": {
      const draft = request<{ notebookId: string; attachmentDirectory: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.attachmentDirectory = draft.attachmentDirectory;
      notebook.previousAttachmentDirectory = null;
      notebook.attachmentDirectorySyncPending = false;
      notebook.updatedAtMs = Date.now();
      return structuredClone(notebook);
    }
    case "rename_notebook": {
      const draft = request<{ notebookId: string; displayName: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.displayName = draft.displayName.trim();
      notebook.updatedAtMs = Date.now();
      return structuredClone(notebook);
    }
    case "unbind_notebook": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.targetState = "unbound";
      notebook.lastErrorCode = null;
      return structuredClone(notebook);
    }
    case "rebind_notebook": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.targetState = "ready";
      notebook.lastErrorCode = null;
      return structuredClone(notebook);
    }
    case "recover_notebook_target": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.targetState = "ready";
      notebook.lastErrorCode = null;
      return structuredClone(notebook);
    }
    case "inspect_notebook_conflict": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook || notebook.targetState !== "conflict") {
        throw new Error("Preview notebook conflict not found");
      }
      return structuredClone({
        notebookId: notebook.id,
        reasonCode: "managed_region_changed",
        detectedAtMs: now - 120_000,
        conflictToken: "f".repeat(64),
        fileMarkdown: "# 产品记录\n\n1. 文件中修改的正文\n\n文件外部备注保持不变。\n",
        wakegptMarkdown: previewNumberedMarkdown(
          notebook,
          notebook.numberingStyle,
          notebook.numberingStart,
        ),
        recordDiffs: records
          .filter((record) => record.notebookId === notebook.id && record.state === "active")
          .map((record, index) => ({
            recordId: record.id,
            wakegptMarkdown: record.bodyMarkdown,
            fileMarkdown: index === 0 ? "文件中修改的正文" : record.bodyMarkdown,
            state: index === 0 ? "modified" : "unchanged",
          })),
        adoptWakegpt: { available: true, blockerCode: null },
        adoptFile: { available: true, blockerCode: null },
        unbind: { available: true, blockerCode: null },
      } satisfies NotebookConflictInspection);
    }
    case "resolve_notebook_conflict": {
      const draft = request<{
        notebookId: string;
        conflictToken: string;
        action: NotebookConflictResolutionAction;
      }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook || draft.conflictToken !== "f".repeat(64)) {
        throw new Error("Preview conflict evidence is stale");
      }
      if (draft.action === "adoptFile") {
        const record = records.find((item) => item.notebookId === notebook.id && item.state === "active");
        if (record) {
          record.bodyMarkdown = "文件中修改的正文";
          record.revision += 1;
          record.appliedRevision = record.revision;
          record.syncState = "synced";
        }
      }
      notebook.targetState = draft.action === "unbind" ? "unbound" : "ready";
      notebook.lastErrorCode = null;
      return structuredClone(notebook);
    }
    case "convert_notebook_to_plain": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.targetState = "unavailable";
      notebook.lastErrorCode = "notebook_converted_to_plain";
      return structuredClone(notebook);
    }
    case "trash_notebook_file": {
      const draft = request<{ notebookId: string }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (!notebook) throw new Error("Preview notebook not found");
      notebook.targetState = "unavailable";
      notebook.lastErrorCode = "notebook_moved_to_trash";
      return structuredClone(notebook);
    }
    case "create_notebook": {
      const draft = request<CreateNotebookRequest>(args);
      const notebook = previewNotebook(
        crypto.randomUUID(),
        draft.displayName,
        draft.relativePath,
        draft.numberingStyle,
        notebooks.length + 1,
        draft.numberingStart,
        draft.attachmentDirectory,
      );
      notebooks = [...notebooks, notebook];
      return structuredClone(notebook);
    }
    case "bind_existing_notebook": {
      const draft = request<BindExistingNotebookRequest>(args);
      const notebook = previewNotebook(
        crypto.randomUUID(),
        draft.displayName || "已绑定笔记",
        "notes/已绑定笔记.md",
        draft.numberingStyle,
        notebooks.length + 1,
        draft.numberingStart,
        draft.attachmentDirectory,
      );
      notebooks = [...notebooks, notebook];
      return structuredClone(notebook);
    }
    case "read_notebook_document": {
      const draft = request<{ notebookId: string }>(args);
      const existing = notebookDocuments.get(draft.notebookId);
      if (existing) return structuredClone(existing);
      const document = previewDocument(draft.notebookId);
      notebookDocuments.set(draft.notebookId, document);
      return structuredClone(document);
    }
    case "save_notebook_document": {
      previewMarkdownSaveCount += 1;
      const draft = request<{
        notebookId: string;
        markdown: string;
        expectedReceiptGeneration: number;
      }>(args);
      const document: NotebookDocument = {
        notebookId: draft.notebookId,
        markdown: draft.markdown,
        fileSha256: crypto.randomUUID().replace(/-/g, "").padEnd(64, "0"),
        receiptGeneration: draft.expectedReceiptGeneration + 1,
        lineEnding: draft.markdown.includes("\r\n")
          ? "\r\n"
          : draft.markdown.includes("\r")
            ? "\r"
            : "\n",
        hadUtf8Bom: notebookDocuments.get(draft.notebookId)?.hadUtf8Bom ?? false,
      };
      const persist = () => {
        notebookDocuments.set(draft.notebookId, document);
        return structuredClone(document);
      };
      return delayedMarkdownSaveEnabled
        ? new Promise((resolve) => window.setTimeout(() => resolve(persist()), 350))
        : persist();
    }
    case "preview_markdown_save_count":
      return previewMarkdownSaveCount;
    case "pick_record_images":
      return structuredClone([pendingPreviewAttachment]);
    case "stage_record_image": {
      const contentSha256 = previewImageDigest(args);
      if ([...previewStagedImages.values()].includes(contentSha256)) return null;
      const token = crypto.randomUUID();
      previewStagedImages.set(token, contentSha256);
      return {
        token,
        mediaType: "image/png",
        byteSize: args instanceof ArrayBuffer || args instanceof Uint8Array
          ? args.byteLength
          : 1024,
        contentSha256,
        displayName: "图片.png",
      } satisfies PendingAttachment;
    }
    case "discard_pending_image": {
      const token = payload(args).token;
      if (typeof token === "string") previewStagedImages.delete(token);
      return null;
    }
    case "read_pending_image_preview":
    case "read_record_image_preview":
      return previewImageBytes.slice(0);
    case "read_markdown_image_preview": {
      const draft = request<{
        workspaceId: string;
        notebookId: string;
        notebookTargetId: string;
        relativePath: string;
      }>(args);
      const notebook = notebooks.find((item) => item.id === draft.notebookId);
      if (
        draft.workspaceId !== uiPreferences.activeWorkspaceId
        || !notebook
        || draft.notebookTargetId !== notebook.targetId
        || draft.relativePath !== "assets/preview.png"
      ) {
        throw new Error(JSON.stringify({ code: "markdown_image_context_changed" }));
      }
      return previewMarkdownImageBytes.slice(0);
    }
    case "list_records": {
      const values = payload(args);
      const notebookId = (values.notebookId as string | null) ?? null;
      const includeTrashed = Boolean(values.includeTrashed);
      return structuredClone(
        records.filter(
          (record) =>
            record.notebookId === notebookId && (includeTrashed || record.state === "active"),
        ),
      );
    }
    case "create_record":
    case "create_quick_capture_record": {
      const draft = request<CreateRecordRequest>(args);
      const record: RecordItem = {
        id: crypto.randomUUID(),
        workspaceId: draft.workspaceId,
        notebookId: draft.notebookId,
        bodyMarkdown: draft.bodyMarkdown,
        createdAtMs: Date.now(),
        updatedAtMs: Date.now(),
        logicalOrder: records.length * 1024 + 1024,
        state: "active",
        syncState: draft.notebookId ? "synced" : "local",
        revision: 1,
        appliedRevision: draft.notebookId ? 1 : 0,
        trashedAtMs: null,
        isPinned: false,
        attachments: draft.attachmentTokens.map((_, index) => ({
          id: crypto.randomUUID(),
          recordId: "preview-pending",
          mediaType: pendingPreviewAttachment.mediaType,
          managedRelativePath: `.wakegpt/attachments/aa/${"a".repeat(64)}.png`,
          contentSha256: pendingPreviewAttachment.contentSha256,
          byteSize: pendingPreviewAttachment.byteSize,
          createdAtMs: Date.now() + index,
          fileState: "ready",
          previousManagedRelativePath: null,
          relocationState: "ready",
          relocationErrorCode: null,
        })),
      };
      records = [...records, record];
      return structuredClone(record);
    }
    case "update_record": {
      const draft = request<UpdateRecordRequest>(args);
      const record = findRecord(draft.recordId);
      record.bodyMarkdown = draft.bodyMarkdown;
      if (draft.retainedAttachmentIds || draft.newAttachmentTokens?.length) {
        const retained = new Set(
          draft.retainedAttachmentIds ?? record.attachments.map((attachment) => attachment.id),
        );
        const current = record.attachments.filter((attachment) => retained.has(attachment.id));
        const added = (draft.newAttachmentTokens ?? []).map((token, index) => {
          const contentSha256 = previewStagedImages.get(token) ?? pendingPreviewAttachment.contentSha256;
          return {
            id: crypto.randomUUID(),
            recordId: record.id,
            mediaType: "image/png",
            managedRelativePath: `notes/attachments/产品记录/${contentSha256.slice(0, 2)}/${contentSha256}.png`,
            contentSha256,
            byteSize: pendingPreviewAttachment.byteSize,
            createdAtMs: Date.now() + index,
            fileState: "ready" as const,
            previousManagedRelativePath: null,
            relocationState: "ready" as const,
            relocationErrorCode: null,
          };
        });
        record.attachments = [...current, ...added];
      }
      if (!record.bodyMarkdown.trim() && !record.attachments.length) {
        throw new Error("记录需要正文或至少一张图片");
      }
      record.updatedAtMs = Date.now();
      record.revision += 1;
      record.appliedRevision = record.revision;
      return structuredClone(record);
    }
    case "trash_record": {
      const draft = request<RecordRevisionRequest>(args);
      const record = findRecord(draft.recordId);
      record.state = "trashed";
      record.trashedAtMs = Date.now();
      record.revision += 1;
      return structuredClone(record);
    }
    case "restore_record": {
      const draft = request<RecordRevisionRequest>(args);
      const record = findRecord(draft.recordId);
      record.state = "active";
      record.trashedAtMs = null;
      record.revision += 1;
      return structuredClone(record);
    }
    case "migrate_record": {
      const draft = request<MigrateRecordRequest>(args);
      const record = findRecord(draft.recordId);
      record.notebookId = draft.destinationNotebookId;
      record.updatedAtMs = Date.now();
      record.revision += 1;
      return structuredClone(record);
    }
    case "set_record_pinned": {
      const draft = request<{ recordId: string; pinned: boolean }>(args);
      const record = findRecord(draft.recordId);
      record.isPinned = draft.pinned;
      return structuredClone(record);
    }
    case "retry_pending_recovery":
      return {
        recoveredOperations: 0,
        recoveredAttachmentOperations: 0,
        initializedNotebooks: 0,
        pending: [],
        pendingNotebooks: [],
        pendingAttachmentOperations: 0,
      } satisfies RecoveryReport;
    default:
      throw new Error(`Unhandled preview command: ${command}`);
  }
}

function previewLocalDiagnosticsStatus(): LocalDiagnosticsStatus {
  const timestamps = previewLocalDiagnosticEvents.flatMap((event) => [
    event.firstAtMs,
    event.lastAtMs,
  ]);
  return {
    available: true,
    enabled: previewLocalDiagnosticsEnabled,
    retentionDays: previewLocalDiagnosticsRetentionDays,
    maxBytes: previewLocalDiagnosticsMaxBytes,
    eventCount: previewLocalDiagnosticEvents.length,
    databaseBytes: 4_096 + previewLocalDiagnosticEvents.length * 512,
    earliestAtMs: timestamps.length ? Math.min(...timestamps) : null,
    latestAtMs: timestamps.length ? Math.max(...timestamps) : null,
    lastErrorCode: null,
  };
}

function previewDocument(notebookId: string): NotebookDocument {
  const notebook = notebooks.find((item) => item.id === notebookId);
  const targetId = notebook?.targetId ?? crypto.randomUUID();
  const managed = `<!-- wakegpt:target id="${targetId}" schema="2" -->\n<!-- wakegpt:managed -->\n<!-- /wakegpt:managed -->\n`;
  if (largeMarkdownPreviewEnabled) {
    const block = "## Large document sample\n\nParagraph with **bold**, [safe link](https://example.com), and `code`.\n\n";
    const targetLength = 16 * 1024 * 1024 - managed.length - 1;
    const body = block.repeat(Math.ceil(targetLength / block.length)).slice(0, targetLength);
    const markdown = `${body}\n${managed}`;
    const lineEnding: NotebookDocument["lineEnding"] = crlfMarkdownPreviewEnabled
      ? "\r\n"
      : crMarkdownPreviewEnabled
        ? "\r"
        : "\n";
    return {
      notebookId,
      markdown: lineEnding === "\n" ? markdown : markdown.replace(/\n/gu, lineEnding),
      fileSha256: "0".repeat(64),
      receiptGeneration: 1,
      lineEnding,
      hadUtf8Bom: crlfMarkdownPreviewEnabled || crMarkdownPreviewEnabled,
    };
  }
  return {
    notebookId,
    markdown: `# ${notebook?.displayName ?? "Markdown 笔记"}\n\n这一部分可以直接编辑。\n\n![工作区图片](assets/preview.png)\n\n![远程图片](https://example.invalid/tracker.png)\n\n${managed}`,
    fileSha256: "0".repeat(64),
    receiptGeneration: 1,
    lineEnding: "\n",
    hadUtf8Bom: false,
  };
}

function previewNumberedMarkdown(
  notebook: Notebook,
  style: Notebook["numberingStyle"],
  numberingStart: number,
): string {
  const notebookRecords = records.filter(
    (record) => record.notebookId === notebook.id && record.state === "active",
  );
  const visible = notebookRecords.map((record, index) => {
    const prefix = style === "none"
      ? ""
      : style === "numeric" || style === "dateHeadingNumeric"
        ? `${numberingStart + index}. `
        : style === "bullet"
          ? "- "
          : style === "task"
            ? "- [ ] "
            : "[21:55] ";
    return `<!-- wakegpt:record id="${record.id}" revision="${record.revision}" sequence="${index + 1}" -->\n${prefix}${record.bodyMarkdown}\n<!-- /wakegpt:record -->`;
  }).join("\n\n");
  const dateHeading = style === "dateHeadingNumeric" && visible ? "\n\n### 2026-07-25" : "";
  return `# ${notebook.displayName}\n\n这一部分可以直接编辑。\n\n<!-- wakegpt:target id="${notebook.targetId}" schema="2" -->\n<!-- wakegpt:managed -->${dateHeading}${visible ? `\n\n${visible}` : ""}\n<!-- /wakegpt:managed -->\n`;
}

function previewIntegrationStatus(): CodexIntegrationStatus {
  const instances: CodexIntegrationStatus["instances"] = [
    {
      id: "chatgpt-preview-default",
      displayName: "默认 ChatGPT",
      isDefault: true,
      canRestart: false,
      detectedCodexVersion: "26.803.81509",
      detectedCodexBuild: "6415",
      state: integrationPaused
        ? "paused"
        : defaultPreviewIntegrationEnabled
          ? "connected"
          : "unavailable",
      availableTargetCount: defaultPreviewIntegrationEnabled ? 1 : 0,
      connectedTargetCount: defaultPreviewIntegrationEnabled ? 1 : 0,
      failedTargetCount: 0,
      lastErrorCode: defaultPreviewIntegrationEnabled ? null : "codex_active_port_unavailable",
    },
    {
      id: "chatgpt-preview-custom",
      displayName: "ChatGPT 实例 1",
      isDefault: false,
      canRestart: true,
      detectedCodexVersion: "26.803.81509",
      detectedCodexBuild: "6415",
      state: integrationPaused ? "paused" : "connected",
      availableTargetCount: 2,
      connectedTargetCount: 2,
      failedTargetCount: 0,
      lastErrorCode: null,
    },
    {
      id: "chatgpt-preview-custom-unavailable",
      displayName: "ChatGPT 实例 2",
      isDefault: false,
      canRestart: true,
      detectedCodexVersion: "26.803.81509",
      detectedCodexBuild: "6415",
      state: integrationPaused
        ? "paused"
        : customPreviewIntegrationEnabled
          ? "connected"
          : "unavailable",
      availableTargetCount: customPreviewIntegrationEnabled ? 1 : 0,
      connectedTargetCount: customPreviewIntegrationEnabled ? 1 : 0,
      failedTargetCount: 0,
      lastErrorCode: customPreviewIntegrationEnabled ? null : "codex_active_port_unavailable",
    },
  ];
  const detectedInstanceCount = instances.length;
  const connectableInstanceCount = instances.filter(
    (instance) => instance.availableTargetCount > 0,
  ).length;
  const availableTargetCount = instances.reduce(
    (count, instance) => count + instance.availableTargetCount,
    0,
  );
  const connectedTargetCount = instances.reduce(
    (count, instance) => count + instance.connectedTargetCount,
    0,
  );
  const connectedInstanceCount = instances.filter(
    (instance) => instance.state === "connected",
  ).length;
  const endpointState: CodexIntegrationStatus["endpointState"] = integrationPaused
    ? "paused"
    : connectedInstanceCount === instances.length
      ? "connected"
      : connectedInstanceCount > 0
        ? "partiallyConnected"
        : "notDetected";
  return {
    paused: integrationPaused,
    adapterVersion: "codex-26.803.81509-v41",
    supportedCodexVersion: "26.803.81509",
    supportedCodexBuild: "6415",
    endpointState,
    lastErrorCode: endpointState === "connected"
      ? null
      : endpointState === "partiallyConnected"
        ? "codex_instances_unconnectable"
        : "codex_debug_endpoint_unavailable",
    cardScriptSha256: "0".repeat(64),
    detectedInstanceCount,
    connectableInstanceCount,
    availableTargetCount,
    connectedTargetCount,
    failedTargetCount: 0,
    instances,
  };
}
