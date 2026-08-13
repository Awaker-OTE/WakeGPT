import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import "./App.css";
import { wakeBridge } from "./bridge";
import { safeImageDisplayName, validateImageFiles } from "./image-input";
import type {
  AppStatus,
  CodexIntegrationStatus,
  DefaultIdentityStatus,
  LoginItemStatus,
  LocalDataResetNotice,
  LocalDataResetPreview,
  LocalDiagnosticEvent,
  LocalDiagnosticsMaxBytes,
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
  SubmitShortcut,
  ThemePreference,
  UpdateCheckIntervalHours,
  UpdateDownloadProgress,
  UpdateStatus,
  Workspace,
  WorkspaceOpenPreference,
} from "./domain";
import {
  AppSidebar,
  ConfirmDialog,
  ConflictInspectorDialog,
  LocalDataResetDialog,
  LocalDiagnosticsDialog,
  EmptyWorkspace,
  MarkdownEditorDialog,
  NewNotebookDialog,
  AttachmentDirectoryPreviewDialog,
  NumberingPreviewDialog,
  NotebookTabs,
  RecordComposer,
  RecordList,
  SettingsView,
  Toast,
  WakeMark,
  activeNotebookAfterTabRefresh,
  notebookTargetLabel,
  type AppView,
  type ToastMessage,
} from "./ui";

interface CommandFailure {
  code?: string;
  message?: string;
  retryable?: boolean;
  dataState?: "unchanged" | "savedLocal" | "recoveryRequired";
}

interface WakeDataChanged {
  workspaceId: string;
  notebookId: string | null;
  changeKind: "records" | "notebooks" | "selection" | "draft";
  source?: "backend";
}

interface NotebookFileAction {
  kind: "convert" | "trash";
  notebook: Notebook;
}

interface MarkdownEditorSession {
  workspaceId: string;
  notebook: Notebook;
  document: NotebookDocument | null;
  requestId: number;
}

interface IntegrationRestartNotice {
  displayName: string;
  tone: "progress" | "error";
  detail: string;
}

interface QuitRequested {
  requestId: number;
}

const isPreviewMode =
  import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview");

const mutationSchemaVersion = 1 as const;

function mutationId(): string {
  return crypto.randomUUID();
}

function parseCommandFailure(error: unknown): CommandFailure {
  if (typeof error === "object" && error !== null) {
    return error as CommandFailure;
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as unknown;
      if (typeof parsed === "object" && parsed !== null) {
        return parsed as CommandFailure;
      }
    } catch {
      return { message: error };
    }
    return { message: error };
  }
  return { message: "操作未完成，请重试。" };
}

function commandMessage(error: unknown): string {
  return parseCommandFailure(error).message ?? "操作未完成，请重试。";
}

function attachmentResultMessage(record: RecordItem, action: "trash" | "restore"): string {
  const states = new Set(record.attachments.map((attachment) => attachment.fileState));
  if (states.has("modified")) {
    return action === "trash"
      ? "记录已删除；已修改的图片为安全起见保留在工作区"
      : "记录已恢复；同路径图片未被覆盖";
  }
  if (states.has("missing")) {
    return action === "trash"
      ? "记录已删除；部分图片文件此前已不存在"
      : "记录已恢复，但部分图片已不在系统废纸篓";
  }
  if (states.has("preservedShared")) return "记录已删除；共享图片仍安全保留";
  return action === "trash" ? "已移到废纸篓" : "记录已恢复";
}

function App() {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [productSettings, setProductSettings] = useState<ProductSettings | null>(null);
  const [recordTrashCleanupPreview, setRecordTrashCleanupPreview] =
    useState<RecordTrashCleanupPreview | null>(null);
  const [recordTrashCleanupOpen, setRecordTrashCleanupOpen] = useState(false);
  const [integration, setIntegration] =
    useState<CodexIntegrationStatus | null>(null);
  const [defaultIdentity, setDefaultIdentity] =
    useState<DefaultIdentityStatus | null>(null);
  const [loginItemStatus, setLoginItemStatus] = useState<LoginItemStatus | null>(null);
  const [loginItemUnavailable, setLoginItemUnavailable] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus | null>(null);
  const [updateUnavailable, setUpdateUnavailable] = useState(false);
  const [updateAction, setUpdateAction] = useState("");
  const [updateProgress, setUpdateProgress] = useState<UpdateDownloadProgress | null>(null);
  const [updateInstallOpen, setUpdateInstallOpen] = useState(false);
  const [localDiagnosticsStatus, setLocalDiagnosticsStatus] =
    useState<LocalDiagnosticsStatus | null>(null);
  const [localDiagnosticsUnavailable, setLocalDiagnosticsUnavailable] = useState(false);
  const [localDiagnosticsOpen, setLocalDiagnosticsOpen] = useState(false);
  const [localDiagnosticsEvents, setLocalDiagnosticsEvents] =
    useState<LocalDiagnosticEvent[]>([]);
  const [localDiagnosticsNextCursor, setLocalDiagnosticsNextCursor] =
    useState<number | null>(null);
  const [localDiagnosticsLoading, setLocalDiagnosticsLoading] = useState(false);
  const [localDiagnosticsError, setLocalDiagnosticsError] = useState("");
  const [localDiagnosticsClearOpen, setLocalDiagnosticsClearOpen] = useState(false);
  const [localDataResetPreview, setLocalDataResetPreview] =
    useState<LocalDataResetPreview | null>(null);
  const [localDataResetUnavailable, setLocalDataResetUnavailable] = useState(false);
  const [localDataResetOpen, setLocalDataResetOpen] = useState(false);
  const [localDataResetConfirmation, setLocalDataResetConfirmation] = useState("");
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspaceOpenPreference, setWorkspaceOpenPreference] =
    useState<WorkspaceOpenPreference | null>(null);
  const [notebooks, setNotebooks] = useState<Notebook[]>([]);
  const [records, setRecords] = useState<RecordItem[]>([]);
  const [activeWorkspaceId, setActiveWorkspaceId] = useState("");
  const [activeNotebookId, setActiveNotebookId] = useState<string | null>(null);
  const [view, setView] = useState<AppView>("notes");
  const [search, setSearch] = useState("");
  const [composer, setComposer] = useState("");
  const [pendingAttachments, setPendingAttachments] = useState<PendingAttachment[]>([]);
  const [theme, setTheme] = useState<ThemePreference>("system");
  const [submitShortcut, setSubmitShortcut] = useState<SubmitShortcut>("enter");
  const [markdownLayout, setMarkdownLayout] = useState<MarkdownLayout>("split");
  const [loadedDraftKey, setLoadedDraftKey] = useState("");
  const [startupError, setStartupError] = useState("");
  const [loading, setLoading] = useState(true);
  const [recordsLoading, setRecordsLoading] = useState(false);
  const [busyAction, setBusyAction] = useState("");
  const [integrationRestartNotice, setIntegrationRestartNotice] =
    useState<IntegrationRestartNotice | null>(null);
  const [newNotebookOpen, setNewNotebookOpen] = useState(false);
  const [workspaceToDisconnect, setWorkspaceToDisconnect] = useState<Workspace | null>(null);
  const [notebookToUnbind, setNotebookToUnbind] = useState<Notebook | null>(null);
  const [notebookFileAction, setNotebookFileAction] = useState<NotebookFileAction | null>(null);
  const [markdownEditorOpen, setMarkdownEditorOpen] = useState(false);
  const [markdownEditorDirty, setMarkdownEditorDirty] = useState(false);
  const [markdownEditorOperation, setMarkdownEditorOperation] =
    useState<"idle" | "reading" | "saving">("idle");
  const [quitConfirmationOpen, setQuitConfirmationOpen] = useState(false);
  const [pendingQuitRequestId, setPendingQuitRequestId] = useState<number | null>(null);
  const [markdownEditorSession, setMarkdownEditorSession] =
    useState<MarkdownEditorSession | null>(null);
  const [numberingPreview, setNumberingPreview] = useState<NotebookNumberingPreview | null>(null);
  const [attachmentDirectoryPreview, setAttachmentDirectoryPreview] =
    useState<NotebookAttachmentDirectoryPreview | null>(null);
  const [conflictInspection, setConflictInspection] =
    useState<NotebookConflictInspection | null>(null);
  const [toast, setToast] = useState<ToastMessage | null>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const preferredNotebookRef = useRef<string | null | undefined>(undefined);
  const activeWorkspaceIdRef = useRef("");
  const activeNotebookIdRef = useRef<string | null>(null);
  const currentDraftKeyRef = useRef("");
  const composerValueRef = useRef("");
  const pendingAttachmentsRef = useRef<PendingAttachment[]>([]);
  const localDiagnosticsStatusGenerationRef = useRef(0);
  const localDiagnosticsLoadGenerationRef = useRef(0);
  const notifiedUpdateVersionRef = useRef<string | null>(null);
  const activeDownloadVersionRef = useRef<string | null>(null);
  const automaticDownloadVersionRef = useRef<string | null>(null);
  const markdownEditorRequestIdRef = useRef(0);
  const markdownOperationIdRef = useRef(0);
  const markdownSaveInFlightRef = useRef(false);
  const markdownEditorOperationRef = useRef<"idle" | "reading" | "saving">("idle");
  const quitRequestInFlightRef = useRef(false);
  const markdownQuitStateRevisionRef = useRef(0);

  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === activeWorkspaceId) ?? null,
    [activeWorkspaceId, workspaces],
  );
  const activeNotebook = useMemo(
    () => notebooks.find((notebook) => notebook.id === activeNotebookId) ?? null,
    [activeNotebookId, notebooks],
  );
  const activeNotebookReadOnly = activeNotebook !== null
    && (
      activeNotebook.targetState !== "ready"
      || activeNotebook.numberingSyncPending
      || activeNotebook.attachmentDirectorySyncPending
    );
  const markdownEditorLiveNotebook = markdownEditorSession
    ? notebooks.find((notebook) => notebook.id === markdownEditorSession.notebook.id) ?? null
    : null;
  const markdownEditorReadOnly = markdownEditorSession !== null
    && (
      markdownEditorLiveNotebook === null
      || markdownEditorLiveNotebook.workspaceId !== markdownEditorSession.workspaceId
      || markdownEditorLiveNotebook.targetState !== "ready"
      || markdownEditorLiveNotebook.numberingSyncPending
      || markdownEditorLiveNotebook.attachmentDirectorySyncPending
    );
  const numberingPreviewNotebook = useMemo(
    () => notebooks.find((notebook) => notebook.id === numberingPreview?.notebookId) ?? null,
    [notebooks, numberingPreview?.notebookId],
  );
  const attachmentDirectoryPreviewNotebook = useMemo(
    () => notebooks.find(
      (notebook) => notebook.id === attachmentDirectoryPreview?.notebookId,
    ) ?? null,
    [attachmentDirectoryPreview?.notebookId, notebooks],
  );
  const currentDraftKey = activeWorkspaceId
    ? `${activeWorkspaceId}:${activeNotebookId ?? "inbox"}`
    : "";
  activeWorkspaceIdRef.current = activeWorkspaceId;
  activeNotebookIdRef.current = activeNotebookId;
  currentDraftKeyRef.current = currentDraftKey;
  composerValueRef.current = composer;
  pendingAttachmentsRef.current = pendingAttachments;

  const showToast = useCallback((next: ToastMessage) => {
    setToast(next);
    window.setTimeout(() => {
      setToast((current) => (current?.id === next.id ? null : current));
    }, 4200);
  }, []);

  useEffect(() => {
    if (conflictInspection && conflictInspection.notebookId !== activeNotebookId) {
      setConflictInspection(null);
    }
  }, [activeNotebookId, conflictInspection]);

  useEffect(() => {
    if (!activeNotebookReadOnly) return;
    if (
      markdownEditorSession
      && markdownEditorSession.workspaceId === activeWorkspaceId
      && markdownEditorSession.notebook.id === activeNotebookId
    ) {
      showToast({
        id: mutationId(),
        tone: "warning",
        text: "当前速记本已变为只读；编辑窗口保留，请复制未保存内容后关闭并处理同步状态",
      });
    }
    setNumberingPreview(null);
    setAttachmentDirectoryPreview(null);
  }, [
    activeNotebookId,
    activeNotebookReadOnly,
    activeWorkspaceId,
    markdownEditorSession,
    showToast,
  ]);

  useEffect(() => {
    if (!activeWorkspaceId) {
      setWorkspaceOpenPreference(null);
      return;
    }
    let active = true;
    void wakeBridge.workspaceOpenPreference(activeWorkspaceId).then(
      (preference) => {
        if (active) setWorkspaceOpenPreference(preference);
      },
      (error: unknown) => {
        if (active) showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      },
    );
    return () => {
      active = false;
    };
  }, [activeWorkspaceId, showToast]);

  const persistCurrentDraft = useCallback(async () => {
    if (!activeWorkspaceId) return;
    await wakeBridge.saveDraft({
      workspaceId: activeWorkspaceId,
      notebookId: activeNotebookId,
      bodyMarkdown: composer,
      attachments: pendingAttachments,
    });
  }, [activeNotebookId, activeWorkspaceId, composer, pendingAttachments]);

  const confirmQuitWhenReady = useCallback(async (requestId: number) => {
    const deadline = Date.now() + 1_500;
    while (true) {
      try {
        await wakeBridge.confirmAndQuit({ requestId });
        return;
      } catch (error) {
        const failure = parseCommandFailure(error);
        if (failure.code !== "quick_capture_quit_pending" || Date.now() >= deadline) {
          throw error;
        }
        await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
      }
    }
  }, []);

  useEffect(() => {
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    if (isPreviewMode) return undefined;
    void wakeBridge.updateMarkdownEditorQuitState({
      open: markdownEditorOpen,
      dirty: markdownEditorDirty,
      saving: markdownEditorOperation === "saving",
      revision: ++markdownQuitStateRevisionRef.current,
    });
    return undefined;
  }, [markdownEditorDirty, markdownEditorOpen, markdownEditorOperation]);

  useEffect(() => {
    if (isPreviewMode) return undefined;
    const unlistenPromise = listen<QuitRequested>("wakegpt://quit-requested", (event) => {
      if (quitRequestInFlightRef.current) return;
      const requestId = event.payload.requestId;
      quitRequestInFlightRef.current = true;
      void (async () => {
        try {
          await persistCurrentDraft();
          if (markdownEditorOperationRef.current === "saving") {
            showToast({
              id: mutationId(),
              tone: "warning",
              text: "Markdown 正在保存，完成前不能退出 WakeGPT",
            });
            return;
          }
          if (markdownEditorOpen && markdownEditorDirty) {
            setPendingQuitRequestId(requestId);
            setQuitConfirmationOpen(true);
            return;
          }
          await confirmQuitWhenReady(requestId);
        } catch (error) {
          showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
        } finally {
          quitRequestInFlightRef.current = false;
        }
      })();
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [confirmQuitWhenReady, markdownEditorDirty, markdownEditorOpen, persistCurrentDraft, showToast]);

  const loadWorkspaceRecords = useCallback(
    async (
      workspaceId: string,
      notebookId: string | null,
      currentView: AppView,
      workspaceNotebooks: Notebook[],
    ) => {
      if (currentView === "settings") return;
      setRecordsLoading(true);
      try {
        if (currentView === "trash") {
          const groups = await Promise.all(
            [null, ...workspaceNotebooks.map((notebook) => notebook.id)].map(
              (targetId) => wakeBridge.listRecords(workspaceId, targetId, true),
            ),
          );
          const trashed = groups
            .flat()
            .filter((record) => record.state === "trashed")
            .sort((left, right) => (right.trashedAtMs ?? 0) - (left.trashedAtMs ?? 0));
          setRecords(trashed);
        } else {
          const active = await wakeBridge.listRecords(workspaceId, notebookId, false);
          setRecords(active.filter((record) => record.state === "active"));
        }
      } catch (error) {
        showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      } finally {
        setRecordsLoading(false);
      }
    },
    [showToast],
  );

  const refreshRecords = useCallback(async () => {
    if (!activeWorkspaceId) return;
    await loadWorkspaceRecords(activeWorkspaceId, activeNotebookId, view, notebooks);
  }, [activeNotebookId, activeWorkspaceId, loadWorkspaceRecords, notebooks, view]);

  const refreshNotebooks = useCallback(async () => {
    if (!activeWorkspaceId) return;
    setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
  }, [activeWorkspaceId]);

  useEffect(() => {
    let active = true;
    const generation = localDiagnosticsStatusGenerationRef.current + 1;
    localDiagnosticsStatusGenerationRef.current = generation;
    void wakeBridge.localDiagnosticsStatus().then(
      (nextStatus) => {
        if (!active || localDiagnosticsStatusGenerationRef.current !== generation) return;
        setLocalDiagnosticsStatus(nextStatus);
        setLocalDiagnosticsUnavailable(false);
      },
      () => {
        if (!active || localDiagnosticsStatusGenerationRef.current !== generation) return;
        setLocalDiagnosticsUnavailable(true);
      },
    );
    return () => {
      active = false;
    };
  }, [view]);

  useEffect(() => {
    let active = true;
    void wakeBridge.localDataResetPreview().then(
      (preview) => {
        if (active) {
          setLocalDataResetPreview(preview);
          setLocalDataResetUnavailable(false);
        }
      },
      () => {
        if (active) {
          setLocalDataResetPreview(null);
          setLocalDataResetUnavailable(true);
        }
      },
    );
    return () => {
      active = false;
    };
  }, [defaultIdentity?.runtimeState, status?.pendingRecoveryOperations, view]);

  useEffect(() => {
    let active = true;
    void wakeBridge.localDataResetNotice().then(
      (notice: LocalDataResetNotice | null) => {
        if (!active || !notice) return;
        showToast({
          id: mutationId(),
          tone: notice.outcome === "completed" ? "success" : "error",
          text: notice.outcome === "completed"
            ? `WakeGPT 已重新开始；${notice.movedItemCount} 类旧本机数据已移入系统废纸篓`
            : "本机数据清除没有完成；原数据已安全恢复",
        });
        void wakeBridge.acknowledgeLocalDataResetNotice(notice.occurredAtMs);
      },
      () => undefined,
    );
    return () => {
      active = false;
    };
  }, [showToast]);

  useEffect(() => {
    let active = true;
    Promise.all([
      wakeBridge.appStatus(),
      wakeBridge.productSettings(),
      wakeBridge.listWorkspaces(),
      wakeBridge.codexIntegrationStatus(),
      wakeBridge.defaultIdentityStatus(),
      wakeBridge.loginItemStatus().catch(() => null),
      wakeBridge.updateStatus().catch(() => null),
      wakeBridge.uiPreferences(),
    ]).then(
      ([
        nextStatus,
        nextProductSettings,
        nextWorkspaces,
        nextIntegration,
        nextDefaultIdentity,
        nextLoginItemStatus,
        nextUpdateStatus,
        preferences,
      ]) => {
        if (!active) return;
        setStatus(nextStatus);
        setProductSettings(nextProductSettings);
        setWorkspaces(nextWorkspaces);
        setIntegration(nextIntegration);
        setDefaultIdentity(nextDefaultIdentity);
        setLoginItemStatus(nextLoginItemStatus);
        setLoginItemUnavailable(nextLoginItemStatus === null);
        setUpdateStatus(nextUpdateStatus);
        setUpdateUnavailable(nextUpdateStatus === null);
        setTheme(preferences.theme);
        setSubmitShortcut(preferences.submitShortcut);
        setMarkdownLayout(preferences.markdownLayout);
        const preferredWorkspace = nextWorkspaces.find(
          (workspace) => workspace.id === preferences.activeWorkspaceId,
        );
        const workspaceId = preferredWorkspace?.id ?? nextWorkspaces[0]?.id ?? "";
        preferredNotebookRef.current = preferredWorkspace
          ? preferences.selectedNotebookId
          : undefined;
        setActiveWorkspaceId(workspaceId);
        if (workspaceId && !preferredWorkspace) {
          void wakeBridge.activateWorkspace(workspaceId);
        }
        setLoading(false);
      },
      (error: unknown) => {
        if (!active) return;
        setStartupError(commandMessage(error));
        setLoading(false);
      },
    );
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (loading) return undefined;
    let active = true;
    const check = async () => {
      try {
        const next = await wakeBridge.checkForUpdates(false);
        if (!active) return;
        setUpdateStatus(next);
        setUpdateUnavailable(false);
        if (
          next.phase === "available"
          && next.availableVersion
          && !next.availableIsSkipped
          && notifiedUpdateVersionRef.current !== next.availableVersion
        ) {
          notifiedUpdateVersionRef.current = next.availableVersion;
          showToast({
            id: mutationId(),
            tone: "success",
            text: `WakeGPT ${next.availableVersion} 已发布，可在设置中查看`,
          });
        }
        if (
          next.phase === "available"
          && next.availableVersion
          && next.externalNetworkEnabled
          && next.automaticDownloadsEnabled
          && !next.availableIsSkipped
          && automaticDownloadVersionRef.current !== next.availableVersion
        ) {
          automaticDownloadVersionRef.current = next.availableVersion;
          activeDownloadVersionRef.current = next.availableVersion;
          setUpdateProgress(null);
          setUpdateAction("download");
          try {
            const downloaded = await wakeBridge.downloadUpdate(next.availableVersion);
            if (!active) return;
            setUpdateStatus(downloaded);
            showToast({
              id: mutationId(),
              tone: "success",
              text: `WakeGPT ${next.availableVersion} 已在后台下载并通过签名验证`,
            });
          } catch (error) {
            if (active) {
              const latest = await wakeBridge.updateStatus().catch(() => null);
              if (latest) setUpdateStatus(latest);
              showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
            }
          } finally {
            activeDownloadVersionRef.current = null;
            setUpdateProgress(null);
            setUpdateAction("");
          }
        }
      } catch {
        void wakeBridge.updateStatus().then(
          (next) => {
            if (!active) return;
            setUpdateStatus(next);
            setUpdateUnavailable(false);
          },
          () => {
            if (active) setUpdateUnavailable(true);
          },
        );
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 60 * 60 * 1_000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [loading, showToast]);

  useEffect(() => {
    if (isPreviewMode) return undefined;
    const unlistenPromise = listen<UpdateDownloadProgress>(
      "wakegpt://update-download-progress",
      (event) => {
        const progress = event.payload;
        if (
          progress.schemaVersion !== 1
          || activeDownloadVersionRef.current !== progress.version
        ) return;
        setUpdateProgress(progress);
        setUpdateStatus((current) => {
          if (!current || current.availableVersion !== progress.version) return current;
          if (![
            "available",
            "downloading",
            "verifying",
          ].includes(current.phase)) return current;
          return {
            ...current,
            phase: progress.verifying ? "verifying" : "downloading",
            downloadedBytes: progress.downloadedBytes,
            downloadTotalBytes: progress.totalBytes,
          };
        });
      },
    );
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (!activeWorkspaceId) {
      setNotebooks([]);
      setRecords([]);
      return;
    }
    let active = true;
    wakeBridge.listNotebooks(activeWorkspaceId).then(
      (nextNotebooks) => {
        if (!active) return;
        setNotebooks(nextNotebooks);
        const preferred = preferredNotebookRef.current;
        preferredNotebookRef.current = undefined;
        setActiveNotebookId((current) => {
          if (preferred !== undefined) {
            return activeNotebookAfterTabRefresh(preferred, nextNotebooks);
          }
          return activeNotebookAfterTabRefresh(current, nextNotebooks);
        });
      },
      (error: unknown) => {
        if (active) {
          showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
        }
      },
    );
    return () => {
      active = false;
    };
  }, [activeWorkspaceId, showToast]);

  useEffect(() => {
    if (!activeWorkspaceId) {
      setComposer("");
      setPendingAttachments([]);
      setLoadedDraftKey("");
      return;
    }
    let active = true;
    const draftKey = currentDraftKey;
    setLoadedDraftKey("");
    wakeBridge.loadDraft({
      workspaceId: activeWorkspaceId,
      notebookId: activeNotebookId,
    }).then(
      (draft) => {
        if (!active) return;
        setComposer(draft.bodyMarkdown);
        setPendingAttachments(draft.attachments);
        setLoadedDraftKey(draftKey);
      },
      (error: unknown) => {
        if (!active) return;
        setLoadedDraftKey(draftKey);
        showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      },
    );
    return () => {
      active = false;
    };
  }, [activeNotebookId, activeWorkspaceId, currentDraftKey, showToast]);

  useEffect(() => {
    if (!currentDraftKey || loadedDraftKey !== currentDraftKey) return undefined;
    const timer = window.setTimeout(() => {
      void persistCurrentDraft().catch(() => undefined);
    }, 350);
    return () => window.clearTimeout(timer);
  }, [currentDraftKey, loadedDraftKey, persistCurrentDraft]);

  useEffect(() => {
    if (!activeWorkspaceId) return;
    void loadWorkspaceRecords(activeWorkspaceId, activeNotebookId, view, notebooks);
  }, [activeNotebookId, activeWorkspaceId, loadWorkspaceRecords, notebooks, view]);

  useEffect(() => {
    if (isPreviewMode) return undefined;
    const unlistenPromise = listen("wakegpt://quick-capture", () => {
      setView("notes");
      window.requestAnimationFrame(() => composerRef.current?.focus());
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const handleShortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "k") {
        event.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", handleShortcut);
    return () => window.removeEventListener("keydown", handleShortcut);
  }, []);

  useEffect(() => {
    if (isPreviewMode) return undefined;
    const unlistenPromise = listen<boolean>(
      "wakegpt://codex-integration-paused",
      (event) =>
        setIntegration((current) => (current ? { ...current, paused: event.payload } : current)),
    );
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (isPreviewMode || !activeWorkspaceId) return undefined;
    const unlistenPromise = listen<WakeDataChanged>("wakegpt://data-changed", (event) => {
      const change = event.payload;
      if (change.source && change.source !== "backend") return;
      if (change.changeKind === "selection") {
        if (change.workspaceId !== activeWorkspaceId) {
          preferredNotebookRef.current = change.notebookId;
          setActiveWorkspaceId(change.workspaceId);
        } else {
          setActiveNotebookId(change.notebookId);
        }
        setView("notes");
        return;
      }
      if (change.changeKind === "draft") return;
      if (change.workspaceId !== activeWorkspaceId) return;
      if (change.changeKind === "notebooks") {
        const workspaceId = activeWorkspaceId;
        const activeNotebookAtEvent = activeNotebookId;
        const draftBodyAtEvent = composerValueRef.current;
        const draftAttachmentsAtEvent = pendingAttachmentsRef.current;
        void (async () => {
          try {
            const nextNotebooks = await wakeBridge.listNotebooks(workspaceId);
            if (
              activeWorkspaceIdRef.current !== workspaceId
              || activeNotebookIdRef.current !== activeNotebookAtEvent
            ) return;
            const nextActiveNotebookId = activeNotebookAfterTabRefresh(
              activeNotebookAtEvent,
              nextNotebooks,
            );
            if (activeNotebookAtEvent && !nextActiveNotebookId) {
              await wakeBridge.saveDraft({
                workspaceId,
                notebookId: activeNotebookAtEvent,
                bodyMarkdown: draftBodyAtEvent,
                attachments: draftAttachmentsAtEvent,
              });
            }
            setNotebooks(nextNotebooks);
            if (!nextActiveNotebookId) {
              setActiveNotebookId((current) =>
                current === activeNotebookAtEvent ? null : current
              );
            }
            await loadWorkspaceRecords(
              workspaceId,
              nextActiveNotebookId,
              view,
              nextNotebooks,
            );
          } catch (error) {
            showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
          }
        })();
        return;
      }
      if (change.notebookId === activeNotebookId) {
        void loadWorkspaceRecords(activeWorkspaceId, activeNotebookId, view, notebooks);
      }
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [
    activeNotebookId,
    activeWorkspaceId,
    loadWorkspaceRecords,
    notebooks,
    showToast,
    view,
  ]);

  useEffect(() => {
    const interval = window.setInterval(() => {
      void wakeBridge.codexIntegrationStatus().then(setIntegration, () => undefined);
      void wakeBridge.defaultIdentityStatus().then(setDefaultIdentity, () => undefined);
    }, 2_500);
    return () => window.clearInterval(interval);
  }, []);

  const handleWorkspaceSelect = (workspaceId: string) => {
    if (workspaceId === activeWorkspaceId) return;
    void (async () => {
      try {
        await persistCurrentDraft();
        const preferences = await wakeBridge.activateWorkspace(workspaceId);
        preferredNotebookRef.current = preferences.selectedNotebookId;
        setActiveWorkspaceId(workspaceId);
        setActiveNotebookId(null);
        setView("notes");
        setSearch("");
      } catch (error) {
        showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      }
    })();
  };

  const handleRevealWorkspace = async (workspaceId: string) => {
    try {
      await wakeBridge.revealWorkspace(workspaceId);
      showToast({ id: mutationId(), tone: "success", text: "已在 Finder 中显示工作区" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleDisconnectWorkspace = async () => {
    const workspace = workspaceToDisconnect;
    if (!workspace) return;
    setBusyAction("disconnect-workspace");
    try {
      await persistCurrentDraft();
      const preferences = await wakeBridge.disconnectWorkspace(workspace.id);
      const nextWorkspaces = await wakeBridge.listWorkspaces();
      setWorkspaces(nextWorkspaces);
      preferredNotebookRef.current = preferences.selectedNotebookId;
      setActiveWorkspaceId(preferences.activeWorkspaceId ?? "");
      setActiveNotebookId(null);
      setRecords([]);
      setSearch("");
      setView("notes");
      setWorkspaceToDisconnect(null);
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已移除“${workspace.displayName}”的连接；本地记录和工作区文件均已保留`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleNotebookSelect = (notebookId: string | null) => {
    if (!activeWorkspaceId || notebookId === activeNotebookId) {
      setView("notes");
      return;
    }
    void (async () => {
      try {
        await persistCurrentDraft();
        await wakeBridge.setActiveSelection({
          workspaceId: activeWorkspaceId,
          notebookId,
        });
        if (notebookId) {
          setNotebooks((current) => current.map((notebook) =>
            notebook.id === notebookId ? { ...notebook, isPinned: true } : notebook
          ));
        }
        setActiveNotebookId(notebookId);
        setView("notes");
      } catch (error) {
        showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      }
    })();
  };

  const handleNotebookPinChange = async (notebookId: string, pinned: boolean) => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("notebook-tab");
    try {
      if (!pinned && notebookId === activeNotebookId) {
        await persistCurrentDraft();
      }
      const updated = await wakeBridge.setNotebookPinned({
        workspaceId: activeWorkspaceId,
        notebookId,
        pinned,
      });
      setNotebooks((current) => current.map((notebook) =>
        notebook.id === notebookId ? updated : notebook
      ));
      if (!pinned && notebookId === activeNotebookId) {
        setActiveNotebookId(null);
      }
      if (!pinned && workspaceOpenPreference?.defaultNotebookId === notebookId) {
        setWorkspaceOpenPreference((current) => current ? {
          ...current,
          defaultNotebookId: null,
          useLastSelection: false,
          updatedAtMs: Date.now(),
        } : current);
      }
      showToast({
        id: mutationId(),
        tone: "success",
        text: pinned ? `已固定“${updated.displayName}”Tab` : `已关闭“${updated.displayName}”Tab；速记本和文件仍保留`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleNotebookRename = async (notebook: Notebook, displayName: string) => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("rename-notebook");
    try {
      const updated = await wakeBridge.renameNotebook({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
        displayName,
      });
      setNotebooks((current) => current.map((item) => item.id === updated.id ? updated : item));
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已将速记本显示名称改为“${updated.displayName}”；Markdown 文件名未改变`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleWorkspaceOpenPreferenceChange = async (
    useLastSelection: boolean,
    defaultNotebookId: string | null,
  ) => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("workspace-open-preference");
    try {
      const next = await wakeBridge.setWorkspaceOpenPreference({
        workspaceId: activeWorkspaceId,
        useLastSelection,
        defaultNotebookId,
      });
      setWorkspaceOpenPreference(next);
      if (next.defaultNotebookId) {
        setNotebooks((current) => current.map((notebook) =>
          notebook.id === next.defaultNotebookId ? { ...notebook, isPinned: true } : notebook
        ));
      }
      showToast({ id: mutationId(), tone: "success", text: "工作区打开方式已保存" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleUnbindNotebook = async (): Promise<void> => {
    const notebook = notebookToUnbind;
    if (!activeWorkspaceId || !notebook || busyAction) return;
    setBusyAction("unbind-notebook");
    try {
      const updated = await wakeBridge.unbindNotebook({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setNotebooks((current) => current.map((item) =>
        item.id === updated.id ? updated : item
      ));
      setNotebookToUnbind(null);
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已解除“${updated.displayName}”的同步绑定；文件、标记和记录均已保留`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleRebindNotebook = async (notebook: Notebook): Promise<void> => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("rebind-notebook");
    try {
      const updated = await wakeBridge.rebindNotebook({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setNotebooks((current) => current.map((item) =>
        item.id === updated.id ? updated : item
      ));
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已重新绑定“${updated.displayName}”；持续同步已恢复`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleRecoverNotebookTarget = async (notebook: Notebook): Promise<void> => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("recover-notebook-target");
    try {
      const updated = await wakeBridge.recoverNotebookTarget({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setNotebooks((current) => current.map((item) =>
        item.id === updated.id ? updated : item
      ));
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已按目标 ID 找回“${updated.displayName}”：${updated.relativePath}`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleOpenConflictInspector = async (notebook: Notebook): Promise<void> => {
    if (!activeWorkspaceId || busyAction) return;
    setBusyAction("inspect-conflict");
    try {
      const inspection = await wakeBridge.inspectNotebookConflict({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setConflictInspection(inspection);
      setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleRefreshConflictInspector = async (): Promise<void> => {
    if (!activeWorkspaceId || !conflictInspection || busyAction) return;
    const notebookId = conflictInspection.notebookId;
    setBusyAction("inspect-conflict");
    try {
      setConflictInspection(await wakeBridge.inspectNotebookConflict({
        workspaceId: activeWorkspaceId,
        notebookId,
      }));
      setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleResolveNotebookConflict = async (
    action: NotebookConflictResolutionAction,
  ): Promise<void> => {
    if (!activeWorkspaceId || !conflictInspection || busyAction) return;
    const inspection = conflictInspection;
    setBusyAction("resolve-conflict");
    try {
      const resolved = await wakeBridge.resolveNotebookConflict({
        workspaceId: activeWorkspaceId,
        notebookId: inspection.notebookId,
        conflictToken: inspection.conflictToken,
        action,
      });
      const [nextNotebooks, nextRecords] = await Promise.all([
        wakeBridge.listNotebooks(activeWorkspaceId),
        wakeBridge.listRecords(activeWorkspaceId, inspection.notebookId, false),
      ]);
      setNotebooks(nextNotebooks);
      setRecords(nextRecords.filter((record) => record.state === "active"));
      setConflictInspection(null);
      const result = action === "adoptWakegpt"
        ? "已采用 WakeGPT 版本并恢复持续同步"
        : action === "adoptFile"
          ? "已采用文件正文并恢复持续同步"
          : `已解除“${resolved.displayName}”的管理；Markdown 文件未被修改`;
      showToast({ id: mutationId(), tone: "success", text: result });
    } catch (error) {
      const failure = parseCommandFailure(error);
      try {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
      } catch {
        // Preserve the original conflict resolution failure for the user.
      }
      if (failure.code === "conflict_evidence_stale") {
        try {
          setConflictInspection(await wakeBridge.inspectNotebookConflict({
            workspaceId: activeWorkspaceId,
            notebookId: inspection.notebookId,
          }));
        } catch {
          setConflictInspection(null);
        }
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "recoveryRequired" ? "warning" : "error",
        text: failure.message ?? "冲突处理未完成",
      });
    } finally {
      setBusyAction("");
    }
  };

  const handleNotebookFileAction = async (): Promise<void> => {
    if (!activeWorkspaceId || !notebookFileAction || busyAction) return;
    const { kind, notebook } = notebookFileAction;
    const action = kind === "convert" ? "convert-notebook" : "trash-notebook-file";
    setBusyAction(action);
    try {
      const request = { workspaceId: activeWorkspaceId, notebookId: notebook.id };
      const updated = kind === "convert"
        ? await wakeBridge.convertNotebookToPlain(request)
        : await wakeBridge.trashNotebookFile(request);
      setNotebooks((current) => current.map((item) =>
        item.id === updated.id ? updated : item
      ));
      setNotebookFileAction(null);
      showToast({
        id: mutationId(),
        tone: "success",
        text: kind === "convert"
          ? `“${updated.displayName}”已转为普通 Markdown；可见内容和本地记录均保留`
          : `“${updated.displayName}”文件已移入 macOS 废纸篓；本地记录仍保留`,
      });
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState === "recoveryRequired") {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
      }
      showToast({ id: mutationId(), tone: "error", text: failure.message ?? "文件操作未完成" });
    } finally {
      setBusyAction("");
    }
  };

  const handleReorderNotebooks = async (orderedNotebookIds: string[]) => {
    if (!activeWorkspaceId) return;
    try {
      setNotebooks(await wakeBridge.reorderNotebooks({
        workspaceId: activeWorkspaceId,
        orderedNotebookIds,
      }));
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleRequestNumberingChange = async (
    notebook: Notebook,
    numberingStyle: NumberingStyle,
    numberingStart: number,
  ) => {
    if (
      !activeWorkspaceId
      || busyAction
      || (notebook.numberingStyle === numberingStyle
        && notebook.numberingStart === numberingStart)
    ) return;
    setBusyAction("preview-numbering");
    try {
      const preview = await wakeBridge.previewNotebookNumbering({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
        numberingStyle,
        numberingStart,
      });
      setNumberingPreview(preview);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleConfirmNumberingChange = async () => {
    const preview = numberingPreview;
    if (!activeWorkspaceId || !preview || busyAction) return;
    setBusyAction("change-numbering");
    try {
      const updated = await wakeBridge.changeNotebookNumbering({
        workspaceId: activeWorkspaceId,
        notebookId: preview.notebookId,
        numberingStyle: preview.nextNumberingStyle,
        numberingStart: preview.nextNumberingStart,
        expectedFileSha256: preview.expectedFileSha256,
        expectedReceiptGeneration: preview.expectedReceiptGeneration,
      });
      setNotebooks((current) => current.map((notebook) =>
        notebook.id === updated.id ? updated : notebook
      ));
      setNumberingPreview(null);
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已更新“${updated.displayName}”的编号配置并重排 Markdown`,
      });
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
        setNumberingPreview(null);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法更新编号格式",
      });
    } finally {
      setBusyAction("");
    }
  };

  const handleRequestAttachmentDirectoryChange = async (
    notebook: Notebook,
    attachmentDirectory: string,
  ) => {
    if (
      !activeWorkspaceId
      || busyAction
      || notebook.attachmentDirectory === attachmentDirectory
    ) return;
    setBusyAction("preview-attachment-directory");
    try {
      const preview = await wakeBridge.previewNotebookAttachmentDirectory({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
        attachmentDirectory,
      });
      setAttachmentDirectoryPreview(preview);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleConfirmAttachmentDirectoryChange = async () => {
    const preview = attachmentDirectoryPreview;
    if (!activeWorkspaceId || !preview || busyAction) return;
    setBusyAction("change-attachment-directory");
    try {
      const updated = await wakeBridge.changeNotebookAttachmentDirectory({
        workspaceId: activeWorkspaceId,
        notebookId: preview.notebookId,
        attachmentDirectory: preview.nextAttachmentDirectory,
        expectedFileSha256: preview.expectedFileSha256,
        expectedReceiptGeneration: preview.expectedReceiptGeneration,
      });
      setNotebooks((current) => current.map((notebook) =>
        notebook.id === updated.id ? updated : notebook
      ));
      setAttachmentDirectoryPreview(null);
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已更新“${updated.displayName}”的附件目录`,
      });
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
        setAttachmentDirectoryPreview(null);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法更新附件目录",
      });
    } finally {
      setBusyAction("");
    }
  };

  const handleThemeChange = async (nextTheme: ThemePreference) => {
    const previous = theme;
    setTheme(nextTheme);
    try {
      const preferences = await wakeBridge.setThemePreference(nextTheme);
      setTheme(preferences.theme);
    } catch (error) {
      setTheme(previous);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleLoginItemChange = async (nextEnabled: boolean) => {
    if (!loginItemStatus || loginItemUnavailable) return;
    setBusyAction("login-item");
    try {
      const nextStatus = await wakeBridge.setLoginItemEnabled(nextEnabled);
      setLoginItemStatus(nextStatus);
      showToast({
        id: mutationId(),
        tone: "success",
        text: nextStatus.enabled
          ? "已开启登录时启动；下次登录后将在后台运行"
          : "已关闭登录时启动",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleUpdateSettingsChange = async (
    externalNetworkEnabled: boolean,
    automaticChecksEnabled: boolean,
    automaticDownloadsEnabled: boolean,
    checkIntervalHours: UpdateCheckIntervalHours,
  ) => {
    if (updateAction) return;
    setUpdateAction("settings");
    try {
      if (!automaticDownloadsEnabled) automaticDownloadVersionRef.current = null;
      const next = await wakeBridge.setUpdateSettings({
        externalNetworkEnabled,
        automaticChecksEnabled,
        automaticDownloadsEnabled,
        checkIntervalHours,
      });
      setUpdateStatus(next);
      setUpdateUnavailable(false);
      if (
        next.externalNetworkEnabled
        && next.automaticDownloadsEnabled
        && next.phase === "available"
        && next.availableVersion
        && !next.availableIsSkipped
      ) {
        automaticDownloadVersionRef.current = next.availableVersion;
        activeDownloadVersionRef.current = next.availableVersion;
        setUpdateProgress(null);
        setUpdateAction("download");
        const downloaded = await wakeBridge.downloadUpdate(next.availableVersion);
        setUpdateStatus(downloaded);
        showToast({
          id: mutationId(),
          tone: "success",
          text: `WakeGPT ${next.availableVersion} 已在后台下载并通过签名验证`,
        });
        return;
      }
      showToast({
        id: mutationId(),
        tone: "success",
        text: !externalNetworkEnabled
          ? "已关闭 WakeGPT 外部网络；不会访问 GitHub"
          : automaticDownloadsEnabled
            ? `已开启后台下载；仍只在你确认后安装`
            : automaticChecksEnabled
              ? `已开启自动检查，间隔 ${checkIntervalHours} 小时`
              : "已关闭自动检查；手动检查仍由你明确触发",
      });
    } catch (error) {
      const latest = await wakeBridge.updateStatus().catch(() => null);
      if (latest) setUpdateStatus(latest);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      activeDownloadVersionRef.current = null;
      setUpdateProgress(null);
      setUpdateAction("");
    }
  };

  const handleProductSettingsChange = async (
    recordTrashRetentionDays: number | null,
    notebookScanIgnoreDirectories: string[],
  ) => {
    setBusyAction("product-settings");
    try {
      const next = await wakeBridge.setProductSettings({
        recordTrashRetentionDays,
        notebookScanIgnoreDirectories,
      });
      setProductSettings(next);
      setIntegration((current) => current
        ? { ...current, paused: next.codexIntegrationPaused }
        : current
      );
      setRecordTrashCleanupPreview(null);
      showToast({ id: mutationId(), tone: "success", text: "记录与恢复扫描设置已保存" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handlePreviewRecordTrashCleanup = async () => {
    setBusyAction("trash-cleanup-preview");
    try {
      const preview = await wakeBridge.previewRecordTrashCleanup();
      setRecordTrashCleanupPreview(preview);
      setRecordTrashCleanupOpen(true);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handlePurgeRecordTrash = async () => {
    const preview = recordTrashCleanupPreview;
    if (!preview) return;
    setBusyAction("trash-cleanup");
    try {
      const result = await wakeBridge.purgeRecordTrash(preview.previewToken);
      setRecordTrashCleanupOpen(false);
      setRecordTrashCleanupPreview(null);
      await refreshRecords();
      showToast({
        id: mutationId(),
        tone: "success",
        text: result.deletedRecordCount
          ? `已永久删除 ${result.deletedRecordCount} 条到期记录`
          : "没有符合安全条件的到期记录",
      });
    } catch (error) {
      setRecordTrashCleanupOpen(false);
      setRecordTrashCleanupPreview(null);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleCheckForUpdates = async () => {
    if (updateAction || !updateStatus?.configured) return;
    setUpdateAction("check");
    try {
      const next = await wakeBridge.checkForUpdates(true);
      setUpdateStatus(next);
      setUpdateUnavailable(false);
      showToast({
        id: mutationId(),
        tone: "success",
        text: next.phase === "available" && next.availableVersion
          ? `已找到 WakeGPT ${next.availableVersion}`
          : "WakeGPT 已是最新版本",
      });
    } catch (error) {
      const latest = await wakeBridge.updateStatus().catch(() => null);
      if (latest) setUpdateStatus(latest);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setUpdateAction("");
    }
  };

  const handleSkipUpdateVersion = async (version: string | null) => {
    if (updateAction) return;
    setUpdateAction("skip");
    try {
      const next = await wakeBridge.skipUpdateVersion(version);
      setUpdateStatus(next);
      setUpdateUnavailable(false);
      showToast({
        id: mutationId(),
        tone: "success",
        text: version ? `已跳过 ${version}；后续版本仍会提示` : "已恢复该版本的更新提示",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setUpdateAction("");
    }
  };

  const handleDownloadUpdate = async () => {
    const version = updateStatus?.availableVersion;
    if (!version || updateAction || updateStatus.availableIsSkipped) return;
    activeDownloadVersionRef.current = version;
    setUpdateProgress(null);
    setUpdateAction("download");
    try {
      const next = await wakeBridge.downloadUpdate(version);
      setUpdateStatus(next);
      setUpdateUnavailable(false);
      showToast({
        id: mutationId(),
        tone: "success",
        text: `WakeGPT ${version} 已下载并通过签名验证`,
      });
    } catch (error) {
      const latest = await wakeBridge.updateStatus().catch(() => null);
      if (latest) setUpdateStatus(latest);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      activeDownloadVersionRef.current = null;
      setUpdateProgress(null);
      setUpdateAction("");
    }
  };

  const handleDiscardDownloadedUpdate = async () => {
    const transactionId = updateStatus?.preparedTransactionId;
    if (!transactionId || updateAction) return;
    setUpdateAction("discard");
    try {
      const next = await wakeBridge.discardDownloadedUpdate(transactionId);
      setUpdateStatus(next);
      setUpdateInstallOpen(false);
      showToast({ id: mutationId(), tone: "success", text: "已移除下载的更新包" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setUpdateAction("");
    }
  };

  const handleOpenUpdateRelease = async () => {
    const version = updateStatus?.availableVersion;
    if (!version || updateAction) return;
    try {
      await wakeBridge.openUpdateRelease(version);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleInstallDownloadedUpdate = async () => {
    const transactionId = updateStatus?.preparedTransactionId;
    const version = updateStatus?.availableVersion;
    if (!transactionId || !version || updateAction) return;
    setUpdateAction("install");
    try {
      await persistCurrentDraft();
      await wakeBridge.installDownloadedUpdate(transactionId, version);
    } catch (error) {
      const latest = await wakeBridge.updateStatus().catch(() => null);
      if (latest) setUpdateStatus(latest);
      setUpdateInstallOpen(false);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      setUpdateAction("");
    }
  };

  const handleAcknowledgeUpdateResult = async () => {
    const occurredAtMs = updateStatus?.installOccurredAtMs;
    if (occurredAtMs === null || occurredAtMs === undefined || updateAction) return;
    setUpdateAction("acknowledge");
    try {
      const next = await wakeBridge.acknowledgeUpdateResult(occurredAtMs);
      setUpdateStatus(next);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setUpdateAction("");
    }
  };

  const handleSubmitShortcutChange = async (nextShortcut: SubmitShortcut) => {
    const previous = submitShortcut;
    setSubmitShortcut(nextShortcut);
    try {
      const preferences = await wakeBridge.setSubmitShortcut(nextShortcut);
      setSubmitShortcut(preferences.submitShortcut);
    } catch (error) {
      setSubmitShortcut(previous);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleMarkdownLayoutChange = async (nextLayout: MarkdownLayout) => {
    const previous = markdownLayout;
    setMarkdownLayout(nextLayout);
    try {
      const preferences = await wakeBridge.setMarkdownLayout(nextLayout);
      setMarkdownLayout(preferences.markdownLayout);
    } catch (error) {
      setMarkdownLayout(previous);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    }
  };

  const handleAddWorkspace = async () => {
    setBusyAction("workspace");
    try {
      const workspace = await wakeBridge.authorizeWorkspace();
      if (!workspace) return;
      setWorkspaces((current) => [workspace, ...current.filter((item) => item.id !== workspace.id)]);
      setActiveWorkspaceId(workspace.id);
      setActiveNotebookId(null);
      preferredNotebookRef.current = null;
      await wakeBridge.setActiveSelection({ workspaceId: workspace.id, notebookId: null });
      setView("notes");
      showToast({ id: mutationId(), tone: "success", text: `已连接“${workspace.displayName}”` });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleCreateNotebook = async (draft: {
    displayName: string;
    relativePath: string;
    numberingStyle: NumberingStyle;
    numberingStart: number;
    attachmentDirectory?: string;
  }): Promise<boolean> => {
    if (!activeWorkspaceId) return false;
    setBusyAction("notebook");
    try {
      const notebook = await wakeBridge.createNotebook({
        workspaceId: activeWorkspaceId,
        ...draft,
      });
      setNotebooks((current) => [...current, notebook]);
      setActiveNotebookId(notebook.id);
      await wakeBridge.setActiveSelection({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setView("notes");
      setNewNotebookOpen(false);
      showToast({ id: mutationId(), tone: "success", text: `已创建“${notebook.displayName}”` });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState === "savedLocal" || failure.dataState === "recoveryRequired") {
        const current = await wakeBridge.listNotebooks(activeWorkspaceId);
        setNotebooks(current);
      }
      showToast({ id: mutationId(), tone: "error", text: failure.message ?? "无法创建笔记" });
      return false;
    } finally {
      setBusyAction("");
    }
  };

  const handleBindNotebook = async (
    numberingStyle: NumberingStyle,
    numberingStart: number,
    attachmentDirectory?: string,
  ): Promise<boolean> => {
    if (!activeWorkspaceId) return false;
    setBusyAction("notebook");
    try {
      const notebook = await wakeBridge.bindExistingNotebook({
        workspaceId: activeWorkspaceId,
        numberingStyle,
        numberingStart,
        attachmentDirectory,
      });
      if (!notebook) return false;
      setNotebooks((current) => [...current, notebook]);
      setActiveNotebookId(notebook.id);
      await wakeBridge.setActiveSelection({
        workspaceId: activeWorkspaceId,
        notebookId: notebook.id,
      });
      setView("notes");
      setNewNotebookOpen(false);
      showToast({ id: mutationId(), tone: "success", text: `已绑定“${notebook.displayName}”` });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState && failure.dataState !== "unchanged") {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
      }
      showToast({ id: mutationId(), tone: "error", text: failure.message ?? "无法绑定 Markdown" });
      return false;
    } finally {
      setBusyAction("");
    }
  };

  const discardPendingImages = async (attachments: PendingAttachment[]) => {
    const results = await Promise.allSettled(
      attachments.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
    );
    return results.filter((result) => result.status === "rejected").length;
  };

  const mergePendingImages = async (
    draftKey: string,
    selected: PendingAttachment[],
  ): Promise<number> => {
    if (!selected.length) return 0;
    if (currentDraftKeyRef.current !== draftKey) {
      await discardPendingImages(selected);
      showToast({
        id: mutationId(),
        tone: "warning",
        text: "当前速记已切换，所选图片未添加。",
      });
      return 0;
    }
    const current = pendingAttachmentsRef.current;
    const knownDigests = new Set(current.map((attachment) => attachment.contentSha256));
    const unique = selected.filter((attachment) => {
      if (knownDigests.has(attachment.contentSha256)) return false;
      knownDigests.add(attachment.contentSha256);
      return true;
    });
    const duplicates = selected.filter((attachment) => !unique.includes(attachment));
    if (duplicates.length) await discardPendingImages(duplicates);
    if (!unique.length) return 0;
    const validation = validateImageFiles(
      unique.map((attachment) => ({
        name: attachment.displayName,
        size: attachment.byteSize,
        type: attachment.mediaType,
      })),
      current,
    );
    if (validation) {
      await discardPendingImages(unique);
      showToast({ id: mutationId(), tone: "error", text: validation });
      return 0;
    }
    const next = [...current, ...unique];
    pendingAttachmentsRef.current = next;
    setPendingAttachments(next);
    return unique.length;
  };

  const handlePickImages = async () => {
    if (busyAction || pendingAttachmentsRef.current.length >= 10) return;
    const draftKey = currentDraftKeyRef.current;
    setBusyAction("pick-images");
    try {
      const selected = await wakeBridge.pickRecordImages();
      if (!selected.length) return;
      await mergePendingImages(draftKey, selected);
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleAddImageFiles = async (files: File[]) => {
    if (busyAction || !currentDraftKeyRef.current) return;
    const current = pendingAttachmentsRef.current;
    const validation = validateImageFiles(files, current);
    if (validation) {
      showToast({ id: mutationId(), tone: "error", text: validation });
      return;
    }
    const draftKey = currentDraftKeyRef.current;
    const staged: PendingAttachment[] = [];
    const knownDigests = new Set(current.map((attachment) => attachment.contentSha256));
    setBusyAction("stage-images");
    try {
      for (const file of files) {
        const bytes = new Uint8Array(await file.arrayBuffer());
        const attachment = await wakeBridge.stageRecordImage(bytes, [...knownDigests]);
        if (!attachment) continue;
        knownDigests.add(attachment.contentSha256);
        staged.push({
          ...attachment,
          displayName: safeImageDisplayName(file.name, attachment.mediaType),
        });
      }
      await mergePendingImages(draftKey, staged);
    } catch (error) {
      const added = await mergePendingImages(draftKey, staged);
      showToast({
        id: mutationId(),
        tone: added ? "warning" : "error",
        text: added
          ? `已添加 ${added} 张图片；其余图片未完成：${commandMessage(error)}`
          : commandMessage(error),
      });
    } finally {
      setBusyAction("");
    }
  };

  const handleRemovePendingImage = async (token: string) => {
    if (busyAction) return;
    setBusyAction("discard-image");
    try {
      await wakeBridge.discardPendingImage(token);
      setPendingAttachments((current) => {
        const next = current.filter((attachment) => attachment.token !== token);
        pendingAttachmentsRef.current = next;
        return next;
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleCreateRecord = async () => {
    const bodyMarkdown = composer.trimEnd();
    if (
      !activeWorkspaceId
      || (!bodyMarkdown.trim() && !pendingAttachments.length)
      || busyAction
    ) return;
    const submittedAttachments = pendingAttachmentsRef.current;
    setBusyAction("create-record");
    try {
      const record = await wakeBridge.createRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: activeWorkspaceId,
        notebookId: activeNotebookId,
        bodyMarkdown,
        attachmentTokens: submittedAttachments.map((attachment) => attachment.token),
      });
      const cleanupFailures = await discardPendingImages(submittedAttachments);
      setRecords((current) => [...current, record]);
      setComposer("");
      pendingAttachmentsRef.current = [];
      setPendingAttachments([]);
      void wakeBridge.saveDraft({
        workspaceId: activeWorkspaceId,
        notebookId: activeNotebookId,
        bodyMarkdown: "",
        attachments: [],
      }).catch(() => undefined);
      showToast({
        id: mutationId(),
        tone: cleanupFailures ? "warning" : "success",
        text: cleanupFailures
          ? "记录已保存；部分临时图片未能清理"
          : activeNotebookId
            ? "已记录并同步到 Markdown"
            : "已保存到收件箱（仅本地）",
      });
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState === "savedLocal" || failure.dataState === "recoveryRequired") {
        await discardPendingImages(submittedAttachments);
        setComposer("");
        pendingAttachmentsRef.current = [];
        setPendingAttachments([]);
        void wakeBridge.saveDraft({
          workspaceId: activeWorkspaceId,
          notebookId: activeNotebookId,
          bodyMarkdown: "",
          attachments: [],
        }).catch(() => undefined);
        await Promise.allSettled([refreshRecords(), refreshNotebooks()]);
        showToast({
          id: mutationId(),
          tone: "warning",
          text: failure.message ?? "记录已保存在本地，等待同步恢复。",
        });
      } else {
        showToast({ id: mutationId(), tone: "error", text: failure.message ?? "记录失败" });
      }
    } finally {
      setBusyAction("");
    }
  };

  const handleOpenMarkdownEditor = async () => {
    if (!activeNotebook || busyAction || markdownEditorOperationRef.current !== "idle") return;
    const requestId = markdownEditorRequestIdRef.current + 1;
    const operationId = markdownOperationIdRef.current + 1;
    markdownEditorRequestIdRef.current = requestId;
    markdownOperationIdRef.current = operationId;
    const session = { workspaceId: activeWorkspaceId, notebook: activeNotebook, requestId };
    setMarkdownEditorOpen(true);
    setMarkdownEditorDirty(false);
    setMarkdownEditorSession({ ...session, document: null });
    markdownEditorOperationRef.current = "reading";
    setMarkdownEditorOperation("reading");
    try {
      const document = await wakeBridge.readNotebookDocument({
        workspaceId: session.workspaceId,
        notebookId: session.notebook.id,
      });
      if (document.notebookId !== session.notebook.id) {
        throw new Error("读取到的 Markdown 与当前编辑会话不一致");
      }
      setMarkdownEditorSession((current) => (
        current
        && current.requestId === requestId
        && current.workspaceId === session.workspaceId
        && current.notebook.id === session.notebook.id
          ? { ...current, document }
          : current
      ));
    } catch (error) {
      if (markdownEditorRequestIdRef.current !== requestId) return;
      setMarkdownEditorOpen(false);
      setMarkdownEditorDirty(false);
      setMarkdownEditorSession(null);
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      if (markdownOperationIdRef.current === operationId) {
        markdownEditorOperationRef.current = "idle";
        setMarkdownEditorOperation("idle");
      }
    }
  };

  const handleSaveMarkdownDocument = async (markdown: string): Promise<boolean> => {
    const session = markdownEditorSession;
    if (!session?.document || session.document.notebookId !== session.notebook.id) {
      showToast({ id: mutationId(), tone: "error", text: "Markdown 编辑会话已失效，请重新打开" });
      return false;
    }
    if (markdownSaveInFlightRef.current) return false;
    markdownSaveInFlightRef.current = true;
    markdownEditorOperationRef.current = "saving";
    setMarkdownEditorOperation("saving");
    const requestId = session.requestId;
    const operationId = markdownOperationIdRef.current + 1;
    markdownOperationIdRef.current = operationId;
    try {
      const saved = await wakeBridge.saveNotebookDocument({
        workspaceId: session.workspaceId,
        notebookId: session.notebook.id,
        expectedFileSha256: session.document.fileSha256,
        expectedReceiptGeneration: session.document.receiptGeneration,
        markdown,
      });
      if (saved.notebookId !== session.notebook.id) {
        throw new Error("保存回执与当前 Markdown 编辑会话不一致");
      }
      if (markdownEditorRequestIdRef.current !== requestId) return true;
      setMarkdownEditorOpen(false);
      setMarkdownEditorDirty(false);
      setMarkdownEditorSession(null);
      markdownEditorRequestIdRef.current += 1;
      showToast({ id: mutationId(), tone: "success", text: "Markdown 全文已保存" });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (markdownEditorRequestIdRef.current !== requestId) return false;
      showToast({
        id: mutationId(),
        tone: failure.dataState === "recoveryRequired" ? "warning" : "error",
        text: failure.message ?? "无法保存 Markdown",
      });
      if (failure.dataState === "recoveryRequired") {
        setMarkdownEditorOpen(false);
        setMarkdownEditorDirty(false);
        setMarkdownEditorSession(null);
        markdownEditorRequestIdRef.current += 1;
        return true;
      }
      return false;
    } finally {
      markdownSaveInFlightRef.current = false;
      if (markdownOperationIdRef.current === operationId) {
        markdownEditorOperationRef.current = "idle";
        setMarkdownEditorOperation("idle");
      }
    }
  };

  const handleUpdateRecord = async (
    record: RecordItem,
    bodyMarkdown: string,
    retainedAttachmentIds: string[],
    newAttachments: PendingAttachment[],
  ): Promise<boolean> => {
    setBusyAction(record.id);
    try {
      const updated = await wakeBridge.updateRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: record.workspaceId,
        recordId: record.id,
        expectedRevision: record.revision,
        bodyMarkdown: bodyMarkdown.trimEnd(),
        retainedAttachmentIds,
        newAttachmentTokens: newAttachments.map((attachment) => attachment.token),
      });
      const cleanupFailures = await discardPendingImages(newAttachments);
      setRecords((current) => current.map((item) => (item.id === updated.id ? updated : item)));
      showToast({
        id: mutationId(),
        tone: cleanupFailures ? "warning" : "success",
        text: cleanupFailures
          ? "修改已保存；部分临时图片未能清理"
          : record.notebookId
            ? "正文与附件已同步到 Markdown"
            : "收件箱正文与附件已修改（仅本地）",
      });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        await discardPendingImages(newAttachments);
        await Promise.allSettled([refreshRecords(), refreshNotebooks()]);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法修改记录",
      });
      return failure.dataState !== "unchanged";
    } finally {
      setBusyAction("");
    }
  };

  const handleTrashRecord = async (record: RecordItem): Promise<boolean> => {
    setBusyAction(record.id);
    try {
      const result = await wakeBridge.trashRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: record.workspaceId,
        recordId: record.id,
        expectedRevision: record.revision,
      });
      setRecords((current) => current.filter((item) => item.id !== record.id));
      showToast({
        id: mutationId(),
        tone: result.attachments.some((attachment) =>
          ["modified", "missing", "preservedShared"].includes(attachment.fileState),
        )
          ? "warning"
          : "success",
        text: attachmentResultMessage(result, "trash"),
      });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        await Promise.allSettled([refreshRecords(), refreshNotebooks()]);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法删除记录",
      });
      return failure.dataState !== "unchanged";
    } finally {
      setBusyAction("");
    }
  };

  const handleRestoreRecord = async (record: RecordItem): Promise<boolean> => {
    setBusyAction(record.id);
    try {
      const result = await wakeBridge.restoreRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: record.workspaceId,
        recordId: record.id,
        expectedRevision: record.revision,
      });
      setRecords((current) => current.filter((item) => item.id !== record.id));
      showToast({
        id: mutationId(),
        tone: result.attachments.some((attachment) =>
          ["modified", "missing"].includes(attachment.fileState),
        )
          ? "warning"
          : "success",
        text: attachmentResultMessage(result, "restore"),
      });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        await Promise.allSettled([refreshRecords(), refreshNotebooks()]);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法恢复记录",
      });
      return failure.dataState !== "unchanged";
    } finally {
      setBusyAction("");
    }
  };

  const handlePinRecord = async (record: RecordItem): Promise<boolean> => {
    setBusyAction(record.id);
    try {
      const updated = await wakeBridge.setRecordPinned({
        workspaceId: record.workspaceId,
        recordId: record.id,
        pinned: !record.isPinned,
      });
      setRecords((current) =>
        current.map((item) => (item.id === updated.id ? updated : item)),
      );
      showToast({
        id: mutationId(),
        tone: "success",
        text: updated.isPinned ? "已置顶" : "已取消置顶",
      });
      return true;
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      return false;
    } finally {
      setBusyAction("");
    }
  };

  const handleMoveRecord = async (
    record: RecordItem,
    destinationNotebookId: string | null,
  ): Promise<boolean> => {
    setBusyAction(record.id);
    try {
      await wakeBridge.migrateRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: record.workspaceId,
        recordId: record.id,
        expectedRevision: record.revision,
        destinationNotebookId,
      });
      setRecords((current) => current.filter((item) => item.id !== record.id));
      showToast({ id: mutationId(), tone: "success", text: "记录已移动并自动重排" });
      return true;
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState !== "unchanged") {
        await Promise.allSettled([refreshRecords(), refreshNotebooks()]);
      }
      showToast({
        id: mutationId(),
        tone: failure.dataState === "unchanged" ? "error" : "warning",
        text: failure.message ?? "无法移动记录",
      });
      return failure.dataState !== "unchanged";
    } finally {
      setBusyAction("");
    }
  };

  const handleDefaultIdentityConfigure = async (alias: string, locked: boolean) => {
    setBusyAction("default-identity-configure");
    try {
      const next = await wakeBridge.configureDefaultIdentity({ alias, locked });
      setDefaultIdentity(next);
      showToast({
        id: mutationId(),
        tone: "success",
        text: next.locked ? "默认身份已锁定到专用 ChatGPT" : "默认身份锁定已暂停",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleUseDefaultIdentity = async () => {
    setBusyAction("default-identity-open");
    try {
      const next = await wakeBridge.useDefaultIdentity();
      setDefaultIdentity(next);
      showToast({
        id: mutationId(),
        tone: next.runtimeState === "running" ? "success" : "warning",
        text: next.runtimeState === "running"
          ? "已切换到 WakeGPT 的默认身份"
          : "专用 ChatGPT 已启动，正在确认运行状态",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleUnbindDefaultIdentity = async () => {
    const confirmed = window.confirm(
      "解除绑定只会移除 WakeGPT 中的默认身份配置；专用 ChatGPT profile 和其中的官方登录状态会保留，正在运行的 ChatGPT 也不会被关闭。是否继续？",
    );
    if (!confirmed) return;
    setBusyAction("default-identity-unbind");
    try {
      const next = await wakeBridge.unbindDefaultIdentity();
      setDefaultIdentity(next);
      showToast({ id: mutationId(), tone: "success", text: "默认身份已解除绑定；专用 profile 已保留" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleIntegrationToggle = async () => {
    if (!integration) return;
    setBusyAction("integration");
    try {
      const next = await wakeBridge.setCodexIntegrationPaused(!integration.paused);
      setIntegration(next);
      setProductSettings((current) => current
        ? { ...current, codexIntegrationPaused: next.paused }
        : current
      );
      showToast({
        id: mutationId(),
        tone: "success",
        text: next.paused ? "ChatGPT 速记卡已暂停" : "ChatGPT 速记卡已恢复",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleRestartCodexIntegration = async () => {
    const confirmed = window.confirm(
      "这会正常退出并重新打开默认 ChatGPT，以启用 Codex 界面中的 WakeGPT 右侧速记卡；其他 ChatGPT 实例不会被关闭。请先确认默认实例中没有尚未提交的输入。是否继续？",
    );
    if (!confirmed) return;
    setBusyAction("restart-codex");
    try {
      const next = await wakeBridge.restartCodexIntegration();
      setIntegration(next);
      setProductSettings((current) => current
        ? { ...current, codexIntegrationPaused: false }
        : current
      );
      showToast({
        id: mutationId(),
        tone: next.endpointState === "connected" ? "success" : "warning",
        text:
          next.endpointState === "connected"
            ? "ChatGPT 右侧速记卡已连接"
            : "ChatGPT 已按集成模式启动，正在连接速记卡。",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleRestartCodexInstanceIntegration = async (
    instance: CodexIntegrationStatus["instances"][number],
  ) => {
    const confirmed = window.confirm(
      `这会先正常退出并重新打开“${instance.displayName}”，为它启用独立的本机回环调试入口；若窗口关闭后进程仍不结束，WakeGPT 会在等待后仅强制结束这个实例。其他 ChatGPT 实例不会被关闭。请先确认该实例中没有尚未提交的输入。是否继续？`,
    );
    if (!confirmed) return;
    const actionKey = `restart-codex-instance:${instance.id}`;
    setBusyAction(actionKey);
    setIntegrationRestartNotice({
      displayName: instance.displayName,
      tone: "progress",
      detail: "正在退出旧进程、重新启动并验证安全调试入口；其他实例保持不动。",
    });
    try {
      const next = await wakeBridge.restartCodexInstanceIntegration(instance.id);
      setIntegration(next);
      setProductSettings((current) => current
        ? { ...current, codexIntegrationPaused: false }
        : current
      );
      setIntegrationRestartNotice(null);
      const connected = ["connected", "partiallyConnected"].includes(next.endpointState);
      showToast({
        id: mutationId(),
        tone: connected ? "success" : "warning",
        text: connected
          ? `${instance.displayName} 已按集成模式重新打开并连接速记卡`
          : `${instance.displayName} 已按集成模式重新打开，正在连接速记卡`,
      });
    } catch (error) {
      const message = commandMessage(error);
      setIntegrationRestartNotice({
        displayName: instance.displayName,
        tone: "error",
        detail: message,
      });
      showToast({ id: mutationId(), tone: "error", text: message });
      void wakeBridge.codexIntegrationStatus().then(setIntegration, () => undefined);
    } finally {
      setBusyAction((current) => current === actionKey ? "" : current);
    }
  };

  const handleRecoveryRetry = async () => {
    setBusyAction("recovery");
    try {
      const report = await wakeBridge.retryPendingRecovery();
      setStatus((current) =>
        current
          ? {
              ...current,
              recoveredOperations:
                current.recoveredOperations
                + report.recoveredOperations
                + report.recoveredAttachmentOperations,
              initializedNotebooks:
                current.initializedNotebooks + report.initializedNotebooks,
              pendingRecoveryOperations:
                report.pending.length
                + report.pendingNotebooks.length
                + report.pendingAttachmentOperations,
            }
          : current,
      );
      if (activeWorkspaceId) {
        setNotebooks(await wakeBridge.listNotebooks(activeWorkspaceId));
      }
      await refreshRecords();
      showToast({
        id: mutationId(),
        tone:
          report.pending.length
          || report.pendingNotebooks.length
          || report.pendingAttachmentOperations
            ? "warning"
            : "success",
        text:
          report.pending.length
          || report.pendingNotebooks.length
          || report.pendingAttachmentOperations
            ? "仍有同步项目需要处理"
            : "同步恢复已完成",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleDataExport = async () => {
    setBusyAction("data-export");
    try {
      await persistCurrentDraft();
      const result = await wakeBridge.exportLocalData();
      if (!result) return;
      showToast({
        id: mutationId(),
        tone: result.unavailableAttachments ? "warning" : "success",
        text: result.unavailableAttachments
          ? `已导出 ${result.folderName}；${result.unavailableAttachments} 项图片不可用，已在清单中标记`
          : `已导出本地数据：${result.folderName}`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleOpenLocalDataReset = () => {
    if (!localDataResetPreview?.canReset || busyAction) return;
    setLocalDataResetConfirmation("");
    setLocalDataResetOpen(true);
  };

  const handleLocalDataReset = async () => {
    if (!localDataResetPreview?.canReset || busyAction) return;
    setBusyAction("local-data-reset");
    try {
      const result = await wakeBridge.resetLocalData({
        confirmationPhrase: localDataResetConfirmation,
      });
      if (result.cancelled) {
        setLocalDataResetOpen(false);
        setLocalDataResetConfirmation("");
        setBusyAction("");
        return;
      }
      showToast({
        id: mutationId(),
        tone: result.restartRequested ? "warning" : "error",
        text: result.restartRequested
          ? "已安排清除，WakeGPT 正在重启"
          : "清除已安全安排；自动重启未启动，请手动重新打开 WakeGPT",
      });
      if (!result.restartRequested) setBusyAction("");
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
      setBusyAction("");
    }
  };

  const handleLocalDiagnosticsSettingsChange = async (nextSettings: {
    enabled: boolean;
    retentionDays: LocalDiagnosticsRetentionDays;
    maxBytes: LocalDiagnosticsMaxBytes;
  }) => {
    if (busyAction) return;
    localDiagnosticsStatusGenerationRef.current += 1;
    setBusyAction("diagnostics-settings");
    try {
      const nextStatus = await wakeBridge.updateLocalDiagnosticsSettings(nextSettings);
      setLocalDiagnosticsStatus(nextStatus);
      setLocalDiagnosticsUnavailable(false);
      showToast({
        id: mutationId(),
        tone: "success",
        text: nextStatus.enabled
          ? "本地诊断设置已更新"
          : "本地诊断已关闭；现有事件仍按保留期保存",
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const loadLocalDiagnosticsPage = useCallback(async (
    beforeId: number | null,
    replace: boolean,
    generation: number,
  ) => {
    setLocalDiagnosticsLoading(true);
    setLocalDiagnosticsError("");
    try {
      const page = await wakeBridge.listLocalDiagnostics({ beforeId, limit: 50 });
      if (localDiagnosticsLoadGenerationRef.current !== generation) return;
      setLocalDiagnosticsEvents((current) => {
        if (replace) return page.events;
        const existingIds = new Set(current.map((event) => event.id));
        return [...current, ...page.events.filter((event) => !existingIds.has(event.id))];
      });
      setLocalDiagnosticsNextCursor(page.nextCursor);
    } catch (error) {
      if (localDiagnosticsLoadGenerationRef.current !== generation) return;
      setLocalDiagnosticsError(commandMessage(error));
    } finally {
      if (localDiagnosticsLoadGenerationRef.current === generation) {
        setLocalDiagnosticsLoading(false);
      }
    }
  }, []);

  const handleOpenLocalDiagnostics = () => {
    const generation = localDiagnosticsLoadGenerationRef.current + 1;
    localDiagnosticsLoadGenerationRef.current = generation;
    setLocalDiagnosticsOpen(true);
    setLocalDiagnosticsEvents([]);
    setLocalDiagnosticsNextCursor(null);
    setLocalDiagnosticsError("");
    void loadLocalDiagnosticsPage(null, true, generation);
  };

  const handleCloseLocalDiagnostics = () => {
    localDiagnosticsLoadGenerationRef.current += 1;
    setLocalDiagnosticsLoading(false);
    setLocalDiagnosticsOpen(false);
  };

  const handleLoadMoreLocalDiagnostics = () => {
    if (localDiagnosticsLoading || localDiagnosticsNextCursor === null) return;
    void loadLocalDiagnosticsPage(
      localDiagnosticsNextCursor,
      false,
      localDiagnosticsLoadGenerationRef.current,
    );
  };

  const handleRetryLocalDiagnostics = () => {
    if (localDiagnosticsLoading) return;
    const replace = localDiagnosticsEvents.length === 0;
    void loadLocalDiagnosticsPage(
      replace ? null : localDiagnosticsNextCursor,
      replace,
      localDiagnosticsLoadGenerationRef.current,
    );
  };

  const handleExportLocalDiagnostics = async () => {
    if (busyAction) return;
    setBusyAction("diagnostics-export");
    try {
      const result = await wakeBridge.exportLocalDiagnostics();
      if (!result) return;
      showToast({
        id: mutationId(),
        tone: "success",
        text: `已单独导出本地诊断：${result.folderName}（${result.eventCount} 条事件）`,
      });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const handleClearLocalDiagnostics = async () => {
    if (busyAction) return;
    localDiagnosticsStatusGenerationRef.current += 1;
    setBusyAction("diagnostics-clear");
    try {
      const nextStatus = await wakeBridge.clearLocalDiagnostics();
      localDiagnosticsLoadGenerationRef.current += 1;
      setLocalDiagnosticsStatus(nextStatus);
      setLocalDiagnosticsUnavailable(false);
      setLocalDiagnosticsEvents([]);
      setLocalDiagnosticsNextCursor(null);
      setLocalDiagnosticsLoading(false);
      setLocalDiagnosticsError("");
      setLocalDiagnosticsClearOpen(false);
      showToast({ id: mutationId(), tone: "success", text: "本地诊断已清空" });
    } catch (error) {
      showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
    } finally {
      setBusyAction("");
    }
  };

  const visibleRecords = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return records;
    return records.filter((record) => record.bodyMarkdown.toLocaleLowerCase().includes(query));
  }, [records, search]);

  const activeNotebookDisabledReason = !activeNotebookReadOnly
    ? undefined
    : activeNotebook.attachmentDirectorySyncPending
      ? "附件目录已保存，附件迁移与 Markdown 更新完成后可继续记录"
      : activeNotebook.numberingSyncPending
      ? "编号格式已保存，Markdown 重排恢复完成后可继续记录"
      : activeNotebook.lastErrorCode === "notebook_converted_to_plain"
      ? "已转为普通 Markdown；本地记录保留，当前速记本已停止同步"
      : activeNotebook.lastErrorCode === "notebook_moved_to_trash"
        ? "文件已移入系统废纸篓；本地记录保留，恢复文件后可重新查找"
        : activeNotebook.targetState === "unbound"
          ? "该速记本已解除绑定；重新绑定后可继续记录"
          : activeNotebook.targetState === "conflict"
            ? "该速记本存在同步冲突；请在设置中处理后继续记录"
            : activeNotebook.targetState === "unavailable"
              ? "目标路径不可用；请在设置中重新查找文件"
              : "正在验证目标文件；完成后可继续记录";
  const activeNotebookTargetText = !activeNotebook
    ? "仅保存在本地收件箱"
    : activeNotebook.attachmentDirectorySyncPending
      ? `附件迁移等待恢复 · ${activeNotebook.attachmentDirectory}`
      : activeNotebook.numberingSyncPending
        ? `编号重排等待恢复 · ${activeNotebook.relativePath}`
        : activeNotebook.targetState === "ready"
          ? `已同步到 ${activeNotebook.relativePath}`
          : activeNotebook.lastErrorCode === "notebook_converted_to_plain"
              || activeNotebook.lastErrorCode === "notebook_moved_to_trash"
            ? notebookTargetLabel(activeNotebook)
            : activeNotebook.targetState === "unbound"
              ? "已解除绑定 · 文件与记录仍保留"
              : activeNotebook.targetState === "conflict"
                ? `同步冲突 · ${activeNotebook.relativePath}`
                : activeNotebook.targetState === "unavailable"
                  ? `目标路径不可用 · ${activeNotebook.relativePath}`
                  : `正在验证 ${activeNotebook.relativePath}`;

  if (loading) {
    return (
      <main className="startup-screen" aria-busy="true">
        <WakeMark />
        <p>正在打开 WakeGPT…</p>
      </main>
    );
  }

  if (startupError) {
    return (
      <main className="startup-screen startup-screen--error">
        <WakeMark />
        <h1>WakeGPT 无法启动</h1>
        <p>{startupError}</p>
      </main>
    );
  }

  return (
    <main id="wakegpt-app" className="app-shell" data-ready={status !== null}>
      <AppSidebar
        searchRef={searchRef}
        workspaces={workspaces}
        activeWorkspaceId={activeWorkspaceId}
        activeNotebook={activeNotebook}
        status={status}
        view={view}
        search={search}
        busyAddingWorkspace={busyAction === "workspace"}
        busyWorkspaceAction={busyAction === "disconnect-workspace"}
        onSearchChange={setSearch}
        onWorkspaceSelect={handleWorkspaceSelect}
        onWorkspaceSettings={() => setView("settings")}
        onRevealWorkspace={(workspaceId) => void handleRevealWorkspace(workspaceId)}
        onDisconnectWorkspace={setWorkspaceToDisconnect}
        onAddWorkspace={() => void handleAddWorkspace()}
        onQuickCapture={() => {
          setView("notes");
          window.requestAnimationFrame(() => composerRef.current?.focus());
        }}
        onViewChange={setView}
      />

      <section className="app-content" aria-label="WakeGPT 内容">
        {activeWorkspace ? (
          <NotebookTabs
            notebooks={notebooks}
            activeNotebookId={activeNotebookId}
            disabled={view !== "notes"}
            onSelect={handleNotebookSelect}
            onReorder={(ids) => void handleReorderNotebooks(ids)}
            onPinChange={(notebookId, pinned) => void handleNotebookPinChange(notebookId, pinned)}
            onCreate={() => setNewNotebookOpen(true)}
          />
        ) : null}
        {view === "settings" ? (
          <SettingsView
            workspace={activeWorkspace}
            notebook={activeNotebook}
            notebooks={notebooks}
            status={status}
            productSettings={productSettings}
            workspaceOpenPreference={workspaceOpenPreference}
            integration={integration}
            defaultIdentity={defaultIdentity}
            loginItemStatus={loginItemStatus}
            loginItemUnavailable={loginItemUnavailable}
            updateStatus={updateStatus}
            updateUnavailable={updateUnavailable}
            updateAction={updateAction}
            updateProgress={updateProgress}
            localDiagnosticsStatus={localDiagnosticsStatus}
            localDiagnosticsUnavailable={localDiagnosticsUnavailable}
            localDataResetPreview={localDataResetPreview}
            localDataResetUnavailable={localDataResetUnavailable}
            theme={theme}
            submitShortcut={submitShortcut}
            markdownLayout={markdownLayout}
            busyAction={busyAction}
            integrationRestartNotice={integrationRestartNotice}
            onNotebookPinChange={(notebookId, pinned) =>
              void handleNotebookPinChange(notebookId, pinned)
            }
            onNotebookRename={(notebook, displayName) =>
              void handleNotebookRename(notebook, displayName)
            }
            onWorkspaceOpenPreferenceChange={(useLastSelection, defaultNotebookId) =>
              void handleWorkspaceOpenPreferenceChange(useLastSelection, defaultNotebookId)
            }
            onRequestUnbind={(notebook) => setNotebookToUnbind(notebook)}
            onRebind={(notebook) => void handleRebindNotebook(notebook)}
            onRecoverNotebookTarget={(notebook) => void handleRecoverNotebookTarget(notebook)}
            onInspectConflict={(notebook) => void handleOpenConflictInspector(notebook)}
            onRequestConvertNotebook={(notebook) =>
              setNotebookFileAction({ kind: "convert", notebook })
            }
            onRequestTrashNotebookFile={(notebook) =>
              setNotebookFileAction({ kind: "trash", notebook })
            }
            onToggleIntegration={() => void handleIntegrationToggle()}
            onRestartCodex={() => void handleRestartCodexIntegration()}
            onRestartCodexInstance={(instance) =>
              void handleRestartCodexInstanceIntegration(instance)
            }
            onConfigureDefaultIdentity={(alias, locked) =>
              void handleDefaultIdentityConfigure(alias, locked)
            }
            onUseDefaultIdentity={() => void handleUseDefaultIdentity()}
            onUnbindDefaultIdentity={() => void handleUnbindDefaultIdentity()}
            onRetryRecovery={() => void handleRecoveryRetry()}
            onExportLocalData={() => void handleDataExport()}
            onProductSettingsChange={(retentionDays, ignoreDirectories) =>
              void handleProductSettingsChange(retentionDays, ignoreDirectories)
            }
            onPreviewRecordTrashCleanup={() => void handlePreviewRecordTrashCleanup()}
            onLocalDiagnosticsSettingsChange={(nextSettings) =>
              void handleLocalDiagnosticsSettingsChange(nextSettings)
            }
            onOpenLocalDiagnostics={handleOpenLocalDiagnostics}
            onExportLocalDiagnostics={() => void handleExportLocalDiagnostics()}
            onRequestClearLocalDiagnostics={() => setLocalDiagnosticsClearOpen(true)}
            onRequestResetLocalData={handleOpenLocalDataReset}
            onLoginItemChange={(nextEnabled) => void handleLoginItemChange(nextEnabled)}
            onUpdateSettingsChange={(externalNetworkEnabled, automaticChecksEnabled, automaticDownloadsEnabled, checkIntervalHours) =>
              void handleUpdateSettingsChange(externalNetworkEnabled, automaticChecksEnabled, automaticDownloadsEnabled, checkIntervalHours)
            }
            onCheckForUpdates={() => void handleCheckForUpdates()}
            onSkipUpdateVersion={(version) => void handleSkipUpdateVersion(version)}
            onDownloadUpdate={() => void handleDownloadUpdate()}
            onDiscardDownloadedUpdate={() => void handleDiscardDownloadedUpdate()}
            onRequestInstallUpdate={() => setUpdateInstallOpen(true)}
            onAcknowledgeUpdateResult={() => void handleAcknowledgeUpdateResult()}
            onOpenUpdateRelease={() => void handleOpenUpdateRelease()}
            onThemeChange={(nextTheme) => void handleThemeChange(nextTheme)}
            onSubmitShortcutChange={(nextShortcut) =>
              void handleSubmitShortcutChange(nextShortcut)
            }
            onMarkdownLayoutChange={(nextLayout) =>
              void handleMarkdownLayoutChange(nextLayout)
            }
            onRequestNumberingChange={(notebook, numberingStyle, numberingStart) =>
              void handleRequestNumberingChange(notebook, numberingStyle, numberingStart)
            }
            onRequestAttachmentDirectoryChange={(notebook, attachmentDirectory) =>
              void handleRequestAttachmentDirectoryChange(notebook, attachmentDirectory)
            }
          />
        ) : activeWorkspace ? (
          <>
            {view === "notes" ? (
              <div
                id="wakegpt-notes-panel"
                className="notes-view"
                role="tabpanel"
                aria-labelledby={activeNotebookId
                  ? `wakegpt-notebook-tab-${activeNotebookId}`
                  : "wakegpt-notebook-tab-inbox"}
              >
                <header className="view-heading">
                  <div className="view-heading__identity">
                    <p className="eyebrow" title={activeWorkspace.displayName}>{activeWorkspace.displayName}</p>
                    <h1 title={activeNotebook?.displayName ?? "收件箱"}>
                      {activeNotebook?.displayName ?? "收件箱"}
                    </h1>
                  </div>
                  <div className="view-heading__actions">
                    <div
                      className="target-status"
                      role="status"
                      aria-label={activeNotebookTargetText}
                      title={activeNotebookTargetText}
                      data-state={activeNotebook?.numberingSyncPending
                        || activeNotebook?.attachmentDirectorySyncPending
                        ? "conflict"
                        : activeNotebook?.targetState ?? "unbound"}
                    >
                      <span className="target-status__dot" aria-hidden="true" />
                      <span className="target-status__text" aria-hidden="true">
                        {activeNotebookTargetText}
                      </span>
                    </div>
                    {activeNotebook?.targetState === "conflict" ? (
                      <button
                        className="primary-button"
                        type="button"
                        disabled={busyAction === "inspect-conflict" || busyAction === "resolve-conflict"}
                        onClick={() => void handleOpenConflictInspector(activeNotebook)}
                      >
                        {busyAction === "inspect-conflict" ? "读取冲突…" : "处理冲突"}
                      </button>
                    ) : activeNotebook ? (
                      <button
                        className="secondary-button"
                        type="button"
                        disabled={
                          activeNotebook.targetState !== "ready"
                          || activeNotebook.numberingSyncPending
                          || activeNotebook.attachmentDirectorySyncPending
                          || Boolean(busyAction)
                          || markdownEditorOperation !== "idle"
                        }
                        onClick={() => void handleOpenMarkdownEditor()}
                      >
                        {markdownEditorOperation === "reading" ? "读取中…" : "编辑 Markdown"}
                      </button>
                    ) : null}
                  </div>
                </header>

                <RecordComposer
                  ref={composerRef}
                  value={composer}
                  busy={
                    ["create-record", "pick-images", "stage-images", "discard-image"]
                      .includes(busyAction)
                    || loadedDraftKey !== currentDraftKey
                  }
                  imageBusy={
                    ["pick-images", "stage-images", "discard-image"].includes(busyAction)
                  }
                  syncEnabled={
                    activeNotebook?.targetState === "ready"
                    && !activeNotebook.numberingSyncPending
                    && !activeNotebook.attachmentDirectorySyncPending
                  }
                  submitShortcut={submitShortcut}
                  disabledReason={activeNotebookDisabledReason}
                  attachments={pendingAttachments}
                  onChange={setComposer}
                  onAddImageFiles={(files) => void handleAddImageFiles(files)}
                  onPickImages={() => void handlePickImages()}
                  onRemoveAttachment={(token) => void handleRemovePendingImage(token)}
                  onSubmit={() => void handleCreateRecord()}
                />

                <RecordList
                  title="快速记录"
                  records={visibleRecords}
                  notebooks={notebooks}
                  activeNotebookId={activeNotebookId}
                  readOnly={activeNotebookReadOnly}
                  busyRecordId={busyAction}
                  loading={recordsLoading}
                  mode="active"
                  onUpdate={handleUpdateRecord}
                  onTrash={handleTrashRecord}
                  onRestore={handleRestoreRecord}
                  onMove={handleMoveRecord}
                  onPin={handlePinRecord}
                />
              </div>
            ) : null}

            {view === "trash" ? (
              <div className="utility-view">
                <header className="view-heading">
                  <div>
                    <p className="eyebrow">{activeWorkspace.displayName}</p>
                    <h1>废纸篓</h1>
                  </div>
                  <p className="view-note">恢复后会回到原笔记并自动重排。</p>
                </header>
                <RecordList
                  title="已删除记录"
                  records={visibleRecords}
                  notebooks={notebooks}
                  activeNotebookId={activeNotebookId}
                  busyRecordId={busyAction}
                  loading={recordsLoading}
                  mode="trash"
                  onUpdate={handleUpdateRecord}
                  onTrash={handleTrashRecord}
                  onRestore={handleRestoreRecord}
                  onMove={handleMoveRecord}
                  onPin={handlePinRecord}
                />
              </div>
            ) : null}

          </>
        ) : (
          <EmptyWorkspace
            busy={busyAction === "workspace"}
            onAddWorkspace={() => void handleAddWorkspace()}
          />
        )}
      </section>

      <ConfirmDialog
        open={recordTrashCleanupOpen}
        title="永久删除到期记录？"
        confirmLabel="永久删除"
        busy={busyAction === "trash-cleanup"}
        confirmDisabled={!recordTrashCleanupPreview?.eligibleRecordCount}
        destructive
        onClose={() => {
          if (busyAction !== "trash-cleanup") {
            setRecordTrashCleanupOpen(false);
            setRecordTrashCleanupPreview(null);
          }
        }}
        onConfirm={() => void handlePurgeRecordTrash()}
      >
        {recordTrashCleanupPreview
          ? recordTrashCleanupPreview.eligibleRecordCount
            ? `将永久删除 ${recordTrashCleanupPreview.eligibleRecordCount} 条到期记录和 ${recordTrashCleanupPreview.eligibleAttachmentCount} 个本地附件条目。${recordTrashCleanupPreview.blockedRecordCount ? `另有 ${recordTrashCleanupPreview.blockedRecordCount} 条记录因恢复、附件或 GPT 插入核对尚未完成而保留。` : ""}此操作不能从 WakeGPT 恢复。`
            : recordTrashCleanupPreview.blockedRecordCount
              ? `没有可安全删除的记录；${recordTrashCleanupPreview.blockedRecordCount} 条到期记录仍有未完成状态，因此会全部保留。`
              : "当前没有达到保留期限的记录。"
          : "正在生成实时清理预览。"}
      </ConfirmDialog>

      <ConfirmDialog
        open={updateInstallOpen}
        title={`重启并更新到 WakeGPT ${updateStatus?.availableVersion ?? ""}？`}
        confirmLabel="保存并重启更新"
        busy={updateAction === "install"}
        onClose={() => {
          if (updateAction !== "install") setUpdateInstallOpen(false);
        }}
        onConfirm={() => void handleInstallDownloadedUpdate()}
      >
        WakeGPT 会先保存当前草稿，暂停并排空本地记录、Markdown 与附件写入，备份当前 App 和数据库，再替换自身并重新打开。ChatGPT / Codex 不会退出；若新版本或数据库迁移未通过独立启动检查，WakeGPT 会尝试恢复当前版本和更新前数据库。
      </ConfirmDialog>
      <LocalDiagnosticsDialog
        open={localDiagnosticsOpen}
        events={localDiagnosticsEvents}
        nextCursor={localDiagnosticsNextCursor}
        loading={localDiagnosticsLoading}
        error={localDiagnosticsError}
        onClose={handleCloseLocalDiagnostics}
        onLoadMore={handleLoadMoreLocalDiagnostics}
        onRetry={handleRetryLocalDiagnostics}
      />
      <NewNotebookDialog
        open={newNotebookOpen}
        busy={busyAction === "notebook"}
        onClose={() => setNewNotebookOpen(false)}
        onCreate={handleCreateNotebook}
        onBind={handleBindNotebook}
      />
      <MarkdownEditorDialog
        open={markdownEditorOpen}
        notebook={markdownEditorSession?.notebook ?? null}
        document={markdownEditorSession?.document ?? null}
        preferredLayout={markdownLayout}
        busy={markdownEditorOperation !== "idle"}
        readOnly={markdownEditorReadOnly}
        onDirtyChange={setMarkdownEditorDirty}
        onClose={() => {
          if (markdownEditorOperationRef.current !== "idle") return;
          setMarkdownEditorOpen(false);
          setMarkdownEditorDirty(false);
          setMarkdownEditorSession(null);
          markdownEditorRequestIdRef.current += 1;
          markdownOperationIdRef.current += 1;
        }}
        onSave={handleSaveMarkdownDocument}
      />
      <ConfirmDialog
        open={quitConfirmationOpen}
        title="放弃尚未保存的 Markdown 修改并退出 WakeGPT？"
        confirmLabel="放弃修改并退出"
        busy={false}
        destructive
        onClose={() => {
          const requestId = pendingQuitRequestId;
          setQuitConfirmationOpen(false);
          setPendingQuitRequestId(null);
          if (requestId !== null) {
            void wakeBridge.cancelQuitRequest({ requestId }).catch(() => undefined);
          }
        }}
        onConfirm={() => {
          const requestId = pendingQuitRequestId;
          setQuitConfirmationOpen(false);
          setPendingQuitRequestId(null);
          if (requestId === null) return;
          void confirmQuitWhenReady(requestId).catch((error) => {
            showToast({ id: mutationId(), tone: "error", text: commandMessage(error) });
          });
        }}
      >
        退出后，这次对 Markdown 全文的修改将无法从 WakeGPT 恢复。
      </ConfirmDialog>
      <ConflictInspectorDialog
        inspection={conflictInspection}
        notebook={notebooks.find((notebook) => notebook.id === conflictInspection?.notebookId) ?? null}
        busy={busyAction === "inspect-conflict" || busyAction === "resolve-conflict"}
        onClose={() => {
          if (busyAction !== "inspect-conflict" && busyAction !== "resolve-conflict") {
            setConflictInspection(null);
          }
        }}
        onRefresh={() => void handleRefreshConflictInspector()}
        onResolve={handleResolveNotebookConflict}
      />
      <NumberingPreviewDialog
        preview={numberingPreview}
        notebook={numberingPreviewNotebook}
        busy={busyAction === "change-numbering"}
        onClose={() => {
          if (busyAction !== "change-numbering") setNumberingPreview(null);
        }}
        onConfirm={() => void handleConfirmNumberingChange()}
      />
      <AttachmentDirectoryPreviewDialog
        preview={attachmentDirectoryPreview}
        notebook={attachmentDirectoryPreviewNotebook}
        busy={busyAction === "change-attachment-directory"}
        onClose={() => {
          if (busyAction !== "change-attachment-directory") {
            setAttachmentDirectoryPreview(null);
          }
        }}
        onConfirm={() => void handleConfirmAttachmentDirectoryChange()}
      />
      <ConfirmDialog
        open={workspaceToDisconnect !== null}
        title="移除工作区连接？"
        confirmLabel="移除工作区"
        busy={busyAction === "disconnect-workspace"}
        destructive
        onClose={() => {
          if (busyAction !== "disconnect-workspace") setWorkspaceToDisconnect(null);
        }}
        onConfirm={() => void handleDisconnectWorkspace()}
      >
        {workspaceToDisconnect
          ? `WakeGPT 会从侧边栏移除“${workspaceToDisconnect.displayName}”，但保留它的收件箱记录、速记本、草稿、设置以及工作区中的全部 Markdown 和图片。以后重新选择同一文件夹，会恢复原工作区身份。`
          : ""}
      </ConfirmDialog>
      <ConfirmDialog
        open={notebookToUnbind !== null}
        title="解除 Markdown 绑定？"
        confirmLabel="解除绑定"
        busy={busyAction === "unbind-notebook"}
        destructive
        onClose={() => {
          if (busyAction !== "unbind-notebook") setNotebookToUnbind(null);
        }}
        onConfirm={() => void handleUnbindNotebook()}
      >
        {notebookToUnbind
          ? `将停止向“${notebookToUnbind.displayName}”持续同步。工作区中的 Markdown 文件、WakeGPT 标记和全部记录都会保留。`
          : ""}
      </ConfirmDialog>
      <ConfirmDialog
        open={notebookFileAction !== null}
        title={notebookFileAction?.kind === "convert"
          ? "转为普通 Markdown？"
          : "将速记本文件移到废纸篓？"}
        confirmLabel={notebookFileAction?.kind === "convert"
          ? "转为普通 Markdown"
          : "移到废纸篓"}
        busy={busyAction === "convert-notebook" || busyAction === "trash-notebook-file"}
        destructive={notebookFileAction?.kind === "trash"}
        onClose={() => {
          if (busyAction !== "convert-notebook" && busyAction !== "trash-notebook-file") {
            setNotebookFileAction(null);
          }
        }}
        onConfirm={() => void handleNotebookFileAction()}
      >
        {notebookFileAction?.kind === "convert"
          ? `WakeGPT 会移除“${notebookFileAction.notebook.displayName}”中的控制标记，保留全部可见内容和本地记录，并停止持续同步。`
          : notebookFileAction
            ? `只把“${notebookFileAction.notebook.displayName}”的 Markdown 文件移入 macOS 废纸篓；本地记录和目标身份继续保留。`
          : ""}
      </ConfirmDialog>
      <ConfirmDialog
        open={localDiagnosticsClearOpen}
        title="清空本地诊断？"
        confirmLabel="清空诊断"
        busy={busyAction === "diagnostics-clear"}
        destructive
        onClose={() => {
          if (busyAction !== "diagnostics-clear") setLocalDiagnosticsClearOpen(false);
        }}
        onConfirm={() => void handleClearLocalDiagnostics()}
      >
        将永久删除 WakeGPT 当前诊断库中的全部事件并回收本地空间。笔记、图片、工作区文件和普通数据导出不受影响；已有的系统备份或文件系统快照不会被删除。
      </ConfirmDialog>
      <LocalDataResetDialog
        open={localDataResetOpen}
        preview={localDataResetPreview}
        confirmation={localDataResetConfirmation}
        busy={busyAction === "local-data-reset"}
        onConfirmationChange={setLocalDataResetConfirmation}
        onClose={() => {
          if (busyAction !== "local-data-reset") {
            setLocalDataResetOpen(false);
            setLocalDataResetConfirmation("");
          }
        }}
        onConfirm={() => void handleLocalDataReset()}
      />
      {toast ? <Toast message={toast} onClose={() => setToast(null)} /> : null}
    </main>
  );
}

export default App;
