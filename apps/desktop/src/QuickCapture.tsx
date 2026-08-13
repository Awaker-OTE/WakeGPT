import { listen } from "@tauri-apps/api/event";
import { ExternalLink, Image as ImageIcon, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import "./App.css";
import { wakeBridge } from "./bridge";
import { safeImageDisplayName, validateImageFiles } from "./image-input";
import type {
  Notebook,
  PendingAttachment,
  RecordItem,
  SubmitShortcut,
  ThemePreference,
  Workspace,
} from "./domain";
import {
  ImagePreviewButton,
  RecordComposer,
  SelectControl,
  Toast,
  WakeMark,
  type ToastMessage,
} from "./ui";

interface CommandFailure {
  message?: string;
  dataState?: "unchanged" | "savedLocal" | "recoveryRequired";
}

interface WakeDataChanged {
  workspaceId: string;
  notebookId: string | null;
  changeKind: "records" | "notebooks" | "selection" | "draft";
  source?: "backend";
}

interface QuitRequested {
  requestId: number;
}

const mutationSchemaVersion = 1 as const;

function mutationId(): string {
  return crypto.randomUUID();
}

function parseCommandFailure(error: unknown): CommandFailure {
  if (typeof error === "object" && error !== null) return error as CommandFailure;
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as unknown;
      if (typeof parsed === "object" && parsed !== null) return parsed as CommandFailure;
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

function notebookWritable(notebook: Notebook): boolean {
  return notebook.targetState === "ready"
    && !notebook.numberingSyncPending
    && !notebook.attachmentDirectorySyncPending;
}

function notebookDisabledReason(notebook: Notebook | null): string | undefined {
  if (!notebook || notebookWritable(notebook)) return undefined;
  if (notebook.attachmentDirectorySyncPending) return "附件迁移完成后可继续记录";
  if (notebook.numberingSyncPending) return "编号重排完成后可继续记录";
  if (notebook.targetState === "conflict") return "该速记本存在同步冲突，请在主应用中处理";
  if (notebook.targetState === "unavailable") return "目标文件不可用，请在主应用中重新查找";
  if (notebook.targetState === "unbound") return "该速记本已解除绑定";
  return "正在验证目标文件";
}

function formatRecentTime(timestamp: number): string {
  const date = new Date(timestamp);
  const today = date.toDateString() === new Date().toDateString();
  return new Intl.DateTimeFormat("zh-CN", today
    ? { hour: "2-digit", minute: "2-digit" }
    : { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }
  ).format(date);
}

function recentSummary(record: RecordItem): string {
  const firstLine = record.bodyMarkdown.trim().split(/\r?\n/u)[0] ?? "";
  if (firstLine) return firstLine;
  return `${record.attachments.length} 张图片`;
}

function syncSummary(record: RecordItem): string {
  const labels: Record<RecordItem["syncState"], string> = {
    local: "本地",
    queued: "等待同步",
    synced: "已同步",
    conflict: "同步冲突",
    targetUnavailable: "目标不可用",
  };
  return labels[record.syncState];
}

function RecentRecord({ record }: { record: RecordItem }) {
  const [preview, setPreview] = useState<{ url: string; label: string } | null>(null);
  const lightboxTrigger = useRef<HTMLButtonElement | null>(null);

  const closePreview = useCallback(() => {
    setPreview(null);
    window.requestAnimationFrame(() => lightboxTrigger.current?.focus({ preventScroll: true }));
  }, []);

  useEffect(() => {
    if (!preview) return undefined;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closePreview();
      } else if (event.key === "Tab") {
        event.preventDefault();
        event.stopPropagation();
        document.querySelector<HTMLButtonElement>(".quick-capture-lightbox button")?.focus();
      }
    };
    window.addEventListener("keydown", close, true);
    return () => window.removeEventListener("keydown", close, true);
  }, [closePreview, preview]);

  return (
    <article className="quick-capture-record">
      <div className="quick-capture-record__meta">
        <time dateTime={new Date(record.createdAtMs).toISOString()}>
          {formatRecentTime(record.createdAtMs)}
        </time>
        <span>{syncSummary(record)}</span>
      </div>
      <p title={record.bodyMarkdown || undefined}>{recentSummary(record)}</p>
      {record.attachments.length ? (
        <div className="quick-capture-record__images" aria-label="记录图片">
          {record.attachments.slice(0, 3).map((attachment, index) => (
            <ImagePreviewButton
              key={attachment.id}
              source={{
                kind: "record",
                workspaceId: record.workspaceId,
                recordId: record.id,
                attachmentId: attachment.id,
                mediaType: attachment.mediaType,
                available: attachment.relocationState === "ready"
                  && (attachment.fileState === "ready" || attachment.fileState === "preservedShared"),
              }}
              label={`图片 ${index + 1}`}
              onOpen={(next, trigger) => {
                lightboxTrigger.current = trigger;
                setPreview(next);
              }}
            />
          ))}
          {record.attachments.length > 3 ? <span>+{record.attachments.length - 3}</span> : null}
        </div>
      ) : null}
      {preview ? (
        <div
          className="quick-capture-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label={`查看 ${preview.label}`}
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) closePreview();
          }}
        >
          <img src={preview.url} alt={preview.label} draggable="false" />
          <button autoFocus type="button" aria-label="关闭图片预览" onClick={closePreview}>
            <X size={18} aria-hidden="true" />
          </button>
        </div>
      ) : null}
    </article>
  );
}

export default function QuickCapture() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [notebooks, setNotebooks] = useState<Notebook[]>([]);
  const [records, setRecords] = useState<RecordItem[]>([]);
  const [workspaceId, setWorkspaceId] = useState("");
  const [notebookId, setNotebookId] = useState<string | null>(null);
  const [composer, setComposer] = useState("");
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const [loadedDraftKey, setLoadedDraftKey] = useState("");
  const [submitShortcut, setSubmitShortcut] = useState<SubmitShortcut>("enter");
  const [theme, setTheme] = useState<ThemePreference>("system");
  const [busy, setBusy] = useState("");
  const [loading, setLoading] = useState(true);
  const [startupError, setStartupError] = useState("");
  const [toast, setToast] = useState<ToastMessage | null>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const selectionRef = useRef({ workspaceId: "", notebookId: null as string | null });
  const attachmentsRef = useRef<PendingAttachment[]>([]);
  const draftGenerationRef = useRef(0);
  const selectionVersionRef = useRef(0);
  const activeNotebook = useMemo(
    () => notebooks.find((notebook) => notebook.id === notebookId) ?? null,
    [notebookId, notebooks],
  );
  const draftKey = workspaceId ? `${workspaceId}:${notebookId ?? "inbox"}` : "";
  selectionRef.current = { workspaceId, notebookId };
  attachmentsRef.current = attachments;

  const showToast = useCallback((message: Omit<ToastMessage, "id">) => {
    setToast({ ...message, id: mutationId() });
  }, []);

  const loadNotebooks = useCallback(async (nextWorkspaceId: string) => {
    const next = await wakeBridge.listNotebooks(nextWorkspaceId);
    setNotebooks(next);
    setNotebookId((current) => (
      current && next.some((notebook) => notebook.id === current) ? current : null
    ));
  }, []);

  const loadRecords = useCallback(async (nextWorkspaceId: string, nextNotebookId: string | null) => {
    const next = await wakeBridge.listRecords(nextWorkspaceId, nextNotebookId, false);
    next.sort((left, right) => right.createdAtMs - left.createdAtMs || right.id.localeCompare(left.id));
    setRecords(next.slice(0, 5));
  }, []);

  const persistDraft = useCallback(async (
    nextWorkspaceId: string,
    nextNotebookId: string | null,
    bodyMarkdown: string,
    pending: PendingAttachment[],
  ) => {
    if (!nextWorkspaceId) return;
    await wakeBridge.saveQuickCaptureDraft({
      workspaceId: nextWorkspaceId,
      notebookId: nextNotebookId,
      bodyMarkdown,
      attachments: pending,
    });
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<QuitRequested>("wakegpt://quit-requested", (event) => {
      const requestId = event.payload.requestId;
      const selection = selectionRef.current;
      void persistDraft(
        selection.workspaceId,
        selection.notebookId,
        composerRef.current?.value ?? "",
        attachmentsRef.current,
      ).then(
        () => wakeBridge.acknowledgeQuickCaptureQuit({ requestId }),
        () => undefined,
      );
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [persistDraft]);

  useEffect(() => {
    let active = true;
    Promise.all([wakeBridge.listWorkspaces(), wakeBridge.uiPreferences()]).then(
      ([nextWorkspaces, preferences]) => {
        if (!active) return;
        setWorkspaces(nextWorkspaces);
        setTheme(preferences.theme);
        setSubmitShortcut(preferences.submitShortcut);
        const preferred = nextWorkspaces.find((workspace) => workspace.id === preferences.activeWorkspaceId);
        setWorkspaceId(preferred?.id ?? nextWorkspaces[0]?.id ?? "");
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
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    if (!workspaceId) {
      setNotebooks([]);
      setRecords([]);
      setLoadedDraftKey("");
      return;
    }
    void loadNotebooks(workspaceId).catch((error) => {
      showToast({ tone: "error", text: commandMessage(error) });
    });
  }, [loadNotebooks, showToast, workspaceId]);

  useEffect(() => {
    if (!draftKey) {
      setComposer("");
      setAttachments([]);
      setLoadedDraftKey("");
      return;
    }
    const generation = ++draftGenerationRef.current;
    setLoadedDraftKey("");
    wakeBridge.loadQuickCaptureDraft({ workspaceId, notebookId }).then(
      (draft) => {
        if (draftGenerationRef.current !== generation) return;
        setComposer(draft.bodyMarkdown);
        setAttachments(draft.attachments);
        attachmentsRef.current = draft.attachments;
        setLoadedDraftKey(draftKey);
      },
      (error: unknown) => {
        if (draftGenerationRef.current !== generation) return;
        setLoadedDraftKey(draftKey);
        showToast({ tone: "error", text: commandMessage(error) });
      },
    );
    return () => {
      draftGenerationRef.current += 1;
    };
  }, [draftKey, notebookId, showToast, workspaceId]);

  useEffect(() => {
    if (!draftKey || loadedDraftKey !== draftKey) return undefined;
    const timer = window.setTimeout(() => {
      void persistDraft(workspaceId, notebookId, composer, attachments).catch(() => undefined);
    }, 350);
    return () => window.clearTimeout(timer);
  }, [attachments, composer, draftKey, loadedDraftKey, notebookId, persistDraft, workspaceId]);

  useEffect(() => {
    if (!workspaceId) return;
    void loadRecords(workspaceId, notebookId).catch((error) => {
      showToast({ tone: "error", text: commandMessage(error) });
    });
  }, [loadRecords, notebookId, showToast, workspaceId]);

  useEffect(() => {
    const unlistenPromise = listen("wakegpt://quick-capture-ready", () => {
      window.requestAnimationFrame(() => composerRef.current?.focus());
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<WakeDataChanged>("wakegpt://data-changed", (event) => {
      const change = event.payload;
      if (change.source && change.source !== "backend") return;
      if (change.changeKind === "selection" || change.changeKind === "draft") return;
      if (change.workspaceId !== selectionRef.current.workspaceId) return;
      if (change.changeKind === "notebooks") {
        void loadNotebooks(change.workspaceId).catch(() => undefined);
      }
      if (change.notebookId === selectionRef.current.notebookId || change.notebookId === null) {
        void loadRecords(
          selectionRef.current.workspaceId,
          selectionRef.current.notebookId,
        ).catch(() => undefined);
      }
    });
    return () => {
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, [loadNotebooks, loadRecords]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (document.querySelector(".select-popover, .quick-capture-lightbox")) return;
      event.preventDefault();
      void wakeBridge.hideQuickCaptureWindow();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  const switchWorkspace = async (nextWorkspaceId: string) => {
    if (busy || nextWorkspaceId === workspaceId) return;
    const version = ++selectionVersionRef.current;
    setBusy("switch");
    try {
      await persistDraft(workspaceId, notebookId, composer, attachmentsRef.current);
      if (selectionVersionRef.current !== version) return;
      setWorkspaceId(nextWorkspaceId);
      setNotebookId(null);
      setRecords([]);
    } catch (error) {
      showToast({ tone: "error", text: commandMessage(error) });
    } finally {
      setBusy("");
    }
  };

  const switchNotebook = async (nextValue: string) => {
    const nextNotebookId = nextValue || null;
    if (busy || nextNotebookId === notebookId) return;
    const version = ++selectionVersionRef.current;
    setBusy("switch");
    try {
      await persistDraft(workspaceId, notebookId, composer, attachmentsRef.current);
      if (selectionVersionRef.current !== version) return;
      setNotebookId(nextNotebookId);
      setRecords([]);
    } catch (error) {
      showToast({ tone: "error", text: commandMessage(error) });
    } finally {
      setBusy("");
    }
  };

  const mergeStagedAttachments = async (selected: PendingAttachment[]) => {
    const current = attachmentsRef.current;
    const known = new Set(current.map((attachment) => attachment.contentSha256));
    const unique = selected.filter((attachment) => {
      if (known.has(attachment.contentSha256)) return false;
      known.add(attachment.contentSha256);
      return true;
    });
    const duplicates = selected.filter((attachment) => !unique.includes(attachment));
    if (duplicates.length) {
      await Promise.allSettled(
        duplicates.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
      );
    }
    const next = [...current, ...unique];
    attachmentsRef.current = next;
    setAttachments(next);
    return unique.length;
  };

  const addImageFiles = async (files: File[]) => {
    if (busy || !workspaceId || !files.length) return;
    const validation = validateImageFiles(files, attachmentsRef.current);
    if (validation) {
      showToast({ tone: "error", text: validation });
      return;
    }
    const staged: PendingAttachment[] = [];
    const known = new Set(attachmentsRef.current.map((attachment) => attachment.contentSha256));
    setBusy("images");
    try {
      for (const file of files) {
        const attachment = await wakeBridge.stageRecordImage(
          new Uint8Array(await file.arrayBuffer()),
          [...known],
        );
        if (!attachment) continue;
        known.add(attachment.contentSha256);
        staged.push({
          ...attachment,
          displayName: safeImageDisplayName(file.name, attachment.mediaType),
        });
      }
      await mergeStagedAttachments(staged);
    } catch (error) {
      const added = await mergeStagedAttachments(staged);
      showToast({
        tone: added ? "warning" : "error",
        text: added
          ? `已添加 ${added} 张图片；其余图片未完成：${commandMessage(error)}`
          : commandMessage(error),
      });
    } finally {
      setBusy("");
    }
  };

  const pickImages = () => {
    if (busy || attachmentsRef.current.length >= 10) return;
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/png,image/jpeg,image/webp,image/gif";
    input.multiple = true;
    input.addEventListener("change", () => {
      const files = Array.from(input.files ?? []);
      if (files.length) void addImageFiles(files);
    }, { once: true });
    input.click();
  };

  const removeAttachment = async (token: string) => {
    if (busy) return;
    setBusy("images");
    try {
      await wakeBridge.discardPendingImage(token);
      const next = attachmentsRef.current.filter((attachment) => attachment.token !== token);
      attachmentsRef.current = next;
      setAttachments(next);
    } catch (error) {
      showToast({ tone: "error", text: commandMessage(error) });
    } finally {
      setBusy("");
    }
  };

  const finishSuccessfulSubmission = async (
    submittedWorkspaceId: string,
    submittedNotebookId: string | null,
    submittedAttachments: PendingAttachment[],
  ) => {
    await Promise.allSettled(
      submittedAttachments.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
    );
    if (
      selectionRef.current.workspaceId === submittedWorkspaceId
      && selectionRef.current.notebookId === submittedNotebookId
    ) {
      setComposer("");
      setAttachments([]);
      attachmentsRef.current = [];
    }
    await persistDraft(submittedWorkspaceId, submittedNotebookId, "", []);
    await loadRecords(submittedWorkspaceId, submittedNotebookId);
  };

  const createRecord = async () => {
    const bodyMarkdown = composer.trimEnd();
    if (!workspaceId || busy || (!bodyMarkdown.trim() && !attachmentsRef.current.length)) return;
    const submittedWorkspaceId = workspaceId;
    const submittedNotebookId = notebookId;
    const submittedAttachments = [...attachmentsRef.current];
    setBusy("submit");
    try {
      await wakeBridge.createQuickCaptureRecord({
        mutationId: mutationId(),
        mutationSchemaVersion,
        workspaceId: submittedWorkspaceId,
        notebookId: submittedNotebookId,
        bodyMarkdown,
        attachmentTokens: submittedAttachments.map((attachment) => attachment.token),
      });
      await finishSuccessfulSubmission(
        submittedWorkspaceId,
        submittedNotebookId,
        submittedAttachments,
      );
      showToast({
        tone: "success",
        text: submittedNotebookId ? "已记录并同步到 Markdown" : "已保存到收件箱（仅本地）",
      });
      window.requestAnimationFrame(() => composerRef.current?.focus());
    } catch (error) {
      const failure = parseCommandFailure(error);
      if (failure.dataState === "savedLocal" || failure.dataState === "recoveryRequired") {
        await finishSuccessfulSubmission(
          submittedWorkspaceId,
          submittedNotebookId,
          submittedAttachments,
        );
        showToast({ tone: "warning", text: failure.message ?? "记录已保存在本地，等待同步恢复。" });
      } else {
        showToast({ tone: "error", text: failure.message ?? "记录失败" });
      }
    } finally {
      setBusy("");
    }
  };

  const openMainApp = async () => {
    try {
      await wakeBridge.showMainWindow();
      await wakeBridge.hideQuickCaptureWindow();
    } catch (error) {
      showToast({ tone: "error", text: commandMessage(error) });
    }
  };

  if (loading) {
    return <main className="quick-capture-shell quick-capture-state">正在打开快速记录…</main>;
  }
  if (startupError) {
    return (
      <main className="quick-capture-shell quick-capture-state" role="alert">
        <WakeMark />
        <strong>快速记录暂时不可用</strong>
        <p>{startupError}</p>
      </main>
    );
  }
  if (!workspaces.length || !workspaceId) {
    return (
      <main className="quick-capture-shell quick-capture-state">
        <WakeMark />
        <strong>尚未连接工作区</strong>
        <p>请先在 WakeGPT App 中选择工作区。</p>
        <button className="primary-button" type="button" onClick={() => void openMainApp()}>
          打开 WakeGPT App
        </button>
      </main>
    );
  }

  const disabledReason = notebookDisabledReason(activeNotebook);
  return (
    <main className="quick-capture-shell" aria-label="WakeGPT 快速记录">
      <header className="quick-capture-header">
        <div><WakeMark /><strong>快速记录</strong></div>
        <button className="quick-capture-close" type="button" aria-label="关闭快速记录" onClick={() => void wakeBridge.hideQuickCaptureWindow()}>
          <X size={17} aria-hidden="true" />
        </button>
      </header>

      <div className="quick-capture-scroll">
        <section className="quick-capture-targets" aria-label="记录位置">
          <label>
            <span>工作区</span>
            <SelectControl
              ariaLabel="快速记录工作区"
              value={workspaceId}
              options={workspaces.map((workspace) => ({ value: workspace.id, label: workspace.displayName }))}
              disabled={Boolean(busy)}
              popoverMinWidth={220}
              onChange={(value) => void switchWorkspace(value)}
            />
          </label>
          <label>
            <span>保存到</span>
            <SelectControl
              ariaLabel="快速记录速记本"
              value={notebookId ?? ""}
              options={[
                { value: "", label: "收件箱（仅本地）" },
                ...notebooks.map((notebook) => ({
                  value: notebook.id,
                  label: notebook.displayName,
                  disabled: !notebookWritable(notebook),
                })),
              ]}
              disabled={Boolean(busy)}
              popoverMinWidth={220}
              onChange={(value) => void switchNotebook(value)}
            />
          </label>
        </section>

        <RecordComposer
          ref={composerRef}
          value={composer}
          busy={Boolean(busy) || loadedDraftKey !== draftKey}
          imageBusy={busy === "images"}
          syncEnabled={Boolean(activeNotebook && notebookWritable(activeNotebook))}
          submitShortcut={submitShortcut}
          disabledReason={disabledReason}
          attachments={attachments}
          onChange={setComposer}
          onAddImageFiles={(files) => void addImageFiles(files)}
          onPickImages={pickImages}
          onRemoveAttachment={(token) => void removeAttachment(token)}
          onSubmit={() => void createRecord()}
        />

        <section className="quick-capture-recent" aria-busy={busy === "switch"}>
          <header><strong>最近记录</strong><span>{records.length} 条</span></header>
          {records.length ? records.map((record) => (
            <RecentRecord key={record.id} record={record} />
          )) : (
            <p className="quick-capture-empty">当前位置还没有记录。</p>
          )}
        </section>
      </div>

      <footer className="quick-capture-footer">
        <span><ImageIcon size={14} aria-hidden="true" />支持粘贴真实图片</span>
        <button type="button" onClick={() => void openMainApp()}>
          打开 WakeGPT App <ExternalLink size={14} aria-hidden="true" />
        </button>
      </footer>
      {toast ? <Toast message={toast} onClose={() => setToast(null)} /> : null}
    </main>
  );
}
