import {
  ArrowLeftRight,
  Bold,
  Check,
  ChevronDown,
  CodeXml,
  Download,
  Ellipsis,
  Folder,
  FolderOpen,
  Image as ImageIcon,
  Inbox,
  Italic,
  Link2,
  List,
  ListTodo,
  LockKeyhole,
  LogIn,
  Pencil,
  Pin,
  Plus,
  RefreshCw,
  Search,
  Settings,
  Trash2,
  Unlink,
  Undo2,
  X,
  type LucideIcon,
} from "lucide-react";
import {
  createContext,
  forwardRef,
  memo,
  useCallback,
  useContext,
  useId,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ChangeEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import ReactMarkdown, { type Components, type UrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { wakeBridge } from "./bridge";
import {
  dialogFocusableElements,
  dialogFocusTargetIndex,
  expandedControlInsideDialog,
  topmostModalDialog,
} from "./dialogFocus";
import { safeImageDisplayName, validateImageFiles } from "./image-input";
import {
  MARKDOWN_LIVE_PREVIEW_MAX_BYTES,
  markdownPreviewRequiresPaging,
  paginateMarkdownEditing,
  paginateMarkdownPreview,
} from "./markdown-preview-pages";
import type {
  Attachment,
  AppStatus,
  CodexIntegrationStatus,
  DefaultIdentityStatus,
  LoginItemStatus,
  LocalDataResetPreview,
  LocalDiagnosticEvent,
  LocalDiagnosticSeverity,
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
  SubmitShortcut,
  ThemePreference,
  UpdateCheckIntervalHours,
  UpdateDownloadProgress,
  UpdateStatus,
  Workspace,
  WorkspaceOpenPreference,
} from "./domain";

export type AppView = "notes" | "trash" | "settings";
export type ToastMessage = {
  id: string;
  tone: "success" | "warning" | "error";
  text: string;
};

const markdownViews: { value: MarkdownLayout; label: string }[] = [
  { value: "edit", label: "原文" },
  { value: "preview", label: "预览" },
  { value: "split", label: "分栏" },
];

type IconName =
  | "bold"
  | "check"
  | "chevronDown"
  | "code"
  | "download"
  | "edit"
  | "folder"
  | "folderOpen"
  | "inbox"
  | "italic"
  | "image"
  | "link"
  | "list"
  | "lock"
  | "login"
  | "more"
  | "move"
  | "pin"
  | "plus"
  | "refresh"
  | "restore"
  | "search"
  | "settings"
  | "sync"
  | "task"
  | "trash"
  | "unlink"
  | "x";

const icons: Record<IconName, LucideIcon> = {
  bold: Bold,
  check: Check,
  chevronDown: ChevronDown,
  code: CodeXml,
  download: Download,
  edit: Pencil,
  folder: Folder,
  folderOpen: FolderOpen,
  inbox: Inbox,
  italic: Italic,
  image: ImageIcon,
  link: Link2,
  list: List,
  lock: LockKeyhole,
  login: LogIn,
  more: Ellipsis,
  move: ArrowLeftRight,
  pin: Pin,
  plus: Plus,
  refresh: RefreshCw,
  restore: Undo2,
  search: Search,
  settings: Settings,
  sync: RefreshCw,
  task: ListTodo,
  trash: Trash2,
  unlink: Unlink,
  x: X,
};

function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  const Component = icons[name];
  return <Component size={size} strokeWidth={1.8} aria-hidden="true" focusable="false" />;
}

export function WakeMark() {
  return (
    <span className="brand-mark" aria-hidden="true">
      <svg viewBox="0 0 28 28" focusable="false">
        <rect x="2" y="2" width="24" height="24" rx="5" fill="#13251f" />
        <rect
          x="11"
          y="6"
          width="11"
          height="15"
          rx="2.2"
          fill="#ffc23a"
          transform="rotate(7 16.5 13.5)"
        />
        <path d="M7 9h9l3 3v9.5A2.5 2.5 0 0 1 16.5 24h-9A2.5 2.5 0 0 1 5 21.5v-10A2.5 2.5 0 0 1 7.5 9Z" fill="#25bfd2" />
        <path d="M16 9v1.6c0 .8.6 1.4 1.4 1.4H19Z" fill="#ffe19a" />
        <path d="M8.2 15.8h6.7M8.2 19h4.5" fill="none" stroke="#10322e" strokeLinecap="round" strokeWidth="1.6" />
      </svg>
    </span>
  );
}

type ImagePreviewSource =
  | {
      kind: "pending";
      token: string;
      mediaType: string;
      available: boolean;
    }
  | {
      kind: "record";
      workspaceId: string;
      recordId: string;
      attachmentId: string;
      mediaType: string;
      available: boolean;
    };

type ImageLightboxState = {
  url: string;
  label: string;
};

function imageBytesBlob(bytes: unknown, mediaType: string): Blob {
  if (bytes instanceof ArrayBuffer) return new Blob([bytes], { type: mediaType });
  if (ArrayBuffer.isView(bytes)) {
    const view = new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return new Blob([Uint8Array.from(view)], { type: mediaType });
  }
  throw new Error("图片预览响应格式无效");
}

export function ImagePreviewButton({
  source,
  label,
  onOpen,
}: {
  source: ImagePreviewSource;
  label: string;
  onOpen: (preview: ImageLightboxState, trigger: HTMLButtonElement) => void;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [url, setUrl] = useState("");
  const [loadState, setLoadState] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const sourceKey = source.kind === "pending"
    ? `pending:${source.token}`
    : `record:${source.workspaceId}:${source.recordId}:${source.attachmentId}`;

  useEffect(() => {
    let active = true;
    let inRange = false;
    let loading = false;
    let failed = false;
    let objectUrl = "";
    let observer: IntersectionObserver | null = null;
    setUrl("");
    setLoadState(source.available ? "idle" : "error");
    if (!source.available) return () => undefined;

    const load = async () => {
      if (!active || !inRange || loading || objectUrl || failed) return;
      loading = true;
      setLoadState("loading");
      try {
        const bytes = source.kind === "pending"
          ? await wakeBridge.readPendingImagePreview(source.token)
          : await wakeBridge.readRecordImagePreview({
              workspaceId: source.workspaceId,
              recordId: source.recordId,
              attachmentId: source.attachmentId,
            });
        const nextUrl = URL.createObjectURL(imageBytesBlob(bytes, source.mediaType));
        if (!active || !inRange) {
          URL.revokeObjectURL(nextUrl);
          return;
        }
        objectUrl = nextUrl;
        setUrl(objectUrl);
        setLoadState("ready");
      } catch {
        failed = true;
        if (active) setLoadState("error");
      } finally {
        loading = false;
      }
    };

    const unload = () => {
      if (!objectUrl) return;
      URL.revokeObjectURL(objectUrl);
      objectUrl = "";
      setUrl("");
      setLoadState(failed ? "error" : "idle");
    };

    const trigger = triggerRef.current;
    if (trigger && "IntersectionObserver" in window) {
      observer = new IntersectionObserver((entries) => {
        const entry = entries.find((candidate) => candidate.target === trigger);
        if (!entry) return;
        inRange = entry.isIntersecting;
        if (inRange) void load();
        else unload();
      }, { rootMargin: "160px" });
      observer.observe(trigger);
    } else {
      inRange = true;
      void load();
    }

    return () => {
      active = false;
      observer?.disconnect();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [sourceKey, source.available, source.mediaType]);

  return (
    <button
      ref={triggerRef}
      className="image-preview-button"
      type="button"
      aria-label={url ? `放大查看 ${label}` : `${label}预览${loadState === "error" ? "不可用" : "加载中"}`}
      aria-busy={loadState === "loading"}
      disabled={!url}
      data-state={loadState}
      onClick={(event) => {
        if (url) onOpen({ url, label }, event.currentTarget);
      }}
    >
      {url ? <img src={url} alt="" draggable="false" /> : <Icon name="image" size={18} />}
      {loadState === "loading" ? <span>读取中</span> : null}
      {loadState === "error" ? <span>不可预览</span> : null}
    </button>
  );
}

function ImageLightbox({
  preview,
  onClose,
}: {
  preview: ImageLightboxState | null;
  onClose: () => void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!preview) return undefined;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    closeRef.current?.focus({ preventScroll: true });
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        onClose();
      } else if (event.key === "Tab") {
        event.preventDefault();
        event.stopPropagation();
        closeRef.current?.focus({ preventScroll: true });
      }
    };
    window.addEventListener("keydown", handleKeyDown, true);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, true);
      document.body.style.overflow = previousOverflow;
    };
  }, [onClose, preview]);

  if (!preview) return null;
  return createPortal(
    <div
      className="image-lightbox-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={`查看 ${preview.label}`}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <figure className="image-lightbox">
        <img src={preview.url} alt={preview.label} draggable="false" />
        <figcaption>{preview.label}</figcaption>
      </figure>
      <button ref={closeRef} type="button" className="image-lightbox-close" aria-label="关闭图片预览" onClick={onClose}>
        <Icon name="x" size={20} />
      </button>
    </div>,
    document.body,
  );
}

function attachmentPreviewAvailable(attachment: Attachment): boolean {
  return attachment.relocationState === "ready"
    && (attachment.fileState === "ready" || attachment.fileState === "preservedShared");
}

type SelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

type SelectPosition = {
  left: number;
  top: number;
  width: number;
  placement: "above" | "below";
};

type AnchorRect = Pick<DOMRect, "left" | "right" | "top" | "bottom" | "width">;
type OverlaySize = Pick<DOMRect, "width" | "height">;

export function anchoredOverlayPosition(
  anchor: AnchorRect,
  overlay: OverlaySize,
  viewport: { width: number; height: number },
  {
    width: requestedWidth = overlay.width,
    align = "start",
    margin = 8,
    gap = 6,
  }: {
    width?: number;
    align?: "start" | "end";
    margin?: number;
    gap?: number;
  } = {},
): SelectPosition {
  const availableWidth = Math.max(1, viewport.width - margin * 2);
  const width = Math.min(Math.max(1, requestedWidth), availableWidth);
  const rawLeft = align === "end" ? anchor.right - width : anchor.left;
  const maximumLeft = Math.max(margin, viewport.width - margin - width);
  const left = Math.min(Math.max(margin, rawLeft), maximumLeft);

  const belowTop = anchor.bottom + gap;
  const aboveTop = anchor.top - gap - overlay.height;
  const belowSpace = Math.max(0, viewport.height - margin - belowTop);
  const aboveSpace = Math.max(0, anchor.top - gap - margin);
  const fitsBelow = overlay.height <= belowSpace;
  const fitsAbove = overlay.height <= aboveSpace;
  const placement = fitsBelow || (!fitsAbove && belowSpace >= aboveSpace) ? "below" : "above";
  const rawTop = placement === "below" ? belowTop : aboveTop;
  const maximumTop = Math.max(margin, viewport.height - margin - overlay.height);
  const top = Math.min(Math.max(margin, rawTop), maximumTop);

  return { left, top, width, placement };
}

const selectTypeaheadTimeout = 700;

function enabledOptionIndex(options: SelectOption[], start: number, direction: 1 | -1): number {
  if (!options.length) return -1;
  for (let offset = 1; offset <= options.length; offset += 1) {
    const index = (start + direction * offset + options.length) % options.length;
    if (!options[index].disabled) return index;
  }
  return -1;
}

function adjacentOptionIndex(options: SelectOption[], start: number, direction: 1 | -1): number {
  for (let index = start + direction; index >= 0 && index < options.length; index += direction) {
    if (!options[index].disabled) return index;
  }
  return start;
}

function edgeOptionIndex(options: SelectOption[], edge: "first" | "last"): number {
  const start = edge === "first" ? -1 : 0;
  return enabledOptionIndex(options, start, edge === "first" ? 1 : -1);
}

export function menuItemIndexAfterKey(
  currentIndex: number,
  itemCount: number,
  key: string,
): number {
  if (itemCount <= 0) return -1;
  if (key === "Home") return 0;
  if (key === "End") return itemCount - 1;
  if (key === "ArrowDown") {
    return currentIndex < 0 ? 0 : (currentIndex + 1) % itemCount;
  }
  if (key === "ArrowUp") {
    return currentIndex < 0 ? itemCount - 1 : (currentIndex - 1 + itemCount) % itemCount;
  }
  return currentIndex;
}

const pageFocusableSelector = [
  "a[href]",
  "area[href]",
  "button:not([disabled])",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[contenteditable='true']",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function pageFocusableElements(excluded: HTMLElement | null): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(pageFocusableSelector)).filter((element) => {
    if (element.tabIndex < 0 || excluded?.contains(element)) return false;
    if (element.closest("[hidden], [inert], [aria-hidden='true']")) return false;
    const style = window.getComputedStyle(element);
    return style.display !== "none" && style.visibility !== "hidden";
  });
}

export function SelectControl({
  ariaLabel,
  value,
  options,
  onChange,
  className = "",
  disabled = false,
  placeholder = "请选择",
  popoverMinWidth = 168,
  tabIndex,
}: {
  ariaLabel: string;
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  className?: string;
  disabled?: boolean;
  placeholder?: string;
  popoverMinWidth?: number;
  tabIndex?: number;
}) {
  const id = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const typeaheadRef = useRef({ text: "", timestamp: 0 });
  const [open, setOpen] = useState(false);
  const [activeValue, setActiveValue] = useState<string | null>(null);
  const [position, setPosition] = useState<SelectPosition | null>(null);
  const selectedIndex = options.findIndex((option) => option.value === value && !option.disabled);
  const firstEnabledIndex = edgeOptionIndex(options, "first");
  const activeIndex = options.findIndex(
    (option) => option.value === activeValue && !option.disabled,
  );
  const selectedLabel = options.find((option) => option.value === value)?.label ?? placeholder;
  const listboxId = `${id}-listbox`;

  const close = (restoreFocus: boolean) => {
    setOpen(false);
    setActiveValue(null);
    setPosition(null);
    typeaheadRef.current = { text: "", timestamp: 0 };
    if (restoreFocus) window.requestAnimationFrame(() => triggerRef.current?.focus());
  };

  const openAt = (index: number) => {
    if (disabled || firstEnabledIndex < 0) return;
    const fallback = selectedIndex >= 0 ? selectedIndex : edgeOptionIndex(options, "first");
    const nextIndex = index >= 0 && !options[index]?.disabled ? index : fallback;
    setActiveValue(nextIndex >= 0 ? options[nextIndex].value : null);
    setOpen(true);
  };

  const commit = (index: number, restoreFocus = true) => {
    const option = options[index];
    if (!option || option.disabled) return;
    if (option.value !== value) onChange(option.value);
    close(restoreFocus);
  };

  const findTypeaheadMatch = (key: string): number => {
    const now = Date.now();
    const previous = typeaheadRef.current;
    const nextText = now - previous.timestamp <= selectTypeaheadTimeout
      ? `${previous.text}${key}`
      : key;
    typeaheadRef.current = { text: nextText, timestamp: now };
    const repeatedCharacter = [...nextText].every((character) => character === nextText[0]);
    const search = (repeatedCharacter ? key : nextText).toLocaleLowerCase();
    const start = activeIndex >= 0 ? activeIndex : selectedIndex;
    for (let offset = 1; offset <= options.length; offset += 1) {
      const index = (Math.max(start, -1) + offset) % options.length;
      const option = options[index];
      if (!option.disabled && option.label.toLocaleLowerCase().startsWith(search)) return index;
    }
    return -1;
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (event.nativeEvent.isComposing) return;
    const printable = event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey;
    if (!open) {
      if (["Enter", " ", "ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        event.preventDefault();
        const index = event.key === "ArrowUp" || event.key === "Home"
          ? edgeOptionIndex(options, "first")
          : event.key === "End"
            ? edgeOptionIndex(options, "last")
            : selectedIndex;
        openAt(index);
      } else if (printable) {
        const match = findTypeaheadMatch(event.key);
        if (match >= 0) {
          event.preventDefault();
          openAt(match);
        }
      }
      return;
    }

    if (event.key === "Escape") {
      event.preventDefault();
      close(true);
    } else if (event.key === "ArrowUp" && event.altKey) {
      event.preventDefault();
      commit(activeIndex);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const nextIndex = adjacentOptionIndex(
        options,
        activeIndex,
        event.key === "ArrowDown" ? 1 : -1,
      );
      setActiveValue(nextIndex >= 0 ? options[nextIndex].value : null);
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      const nextIndex = edgeOptionIndex(options, event.key === "Home" ? "first" : "last");
      setActiveValue(nextIndex >= 0 ? options[nextIndex].value : null);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      commit(activeIndex);
    } else if (event.key === "Tab") {
      if (activeIndex >= 0) commit(activeIndex, false);
      else close(false);
    } else if (printable) {
      const match = findTypeaheadMatch(event.key);
      if (match >= 0) {
        event.preventDefault();
        setActiveValue(options[match].value);
      }
    }
  };

  useLayoutEffect(() => {
    if (!open) return;
    if (disabled || firstEnabledIndex < 0) {
      close(false);
      return;
    }
    if (activeIndex >= 0) return;
    const fallback = selectedIndex >= 0 ? selectedIndex : edgeOptionIndex(options, "first");
    if (fallback >= 0) setActiveValue(options[fallback].value);
    else close(false);
  }, [activeIndex, disabled, firstEnabledIndex, open, options, selectedIndex]);

  useLayoutEffect(() => {
    if (!open) return;
    const trigger = triggerRef.current;
    const popover = popoverRef.current;
    if (!trigger || !popover) return;
    const triggerRect = trigger.getBoundingClientRect();
    const width = Math.min(
      Math.max(triggerRect.width, popoverMinWidth),
      Math.max(120, window.innerWidth - 16),
    );
    popover.style.width = `${width}px`;
    const popoverRect = popover.getBoundingClientRect();
    setPosition(anchoredOverlayPosition(
      triggerRect,
      popoverRect,
      { width: window.innerWidth, height: window.innerHeight },
      { width },
    ));
  }, [open, options.length, popoverMinWidth]);

  useLayoutEffect(() => {
    if (!open || activeIndex < 0) return;
    document.getElementById(`${id}-option-${activeIndex}`)?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, id, open]);

  useEffect(() => {
    if (!open) return;
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (rootRef.current?.contains(target) || popoverRef.current?.contains(target)) return;
      close(false);
    };
    const handleViewportChange = (event: Event) => {
      const target = event.target;
      if (event.type === "scroll" && target instanceof Node && popoverRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("blur", handleViewportChange);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("blur", handleViewportChange);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [open]);

  return (
    <div ref={rootRef} className={`select-control ${className}`.trim()} data-open={open}>
      <button
        ref={triggerRef}
        className="select-trigger"
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-controls={open ? listboxId : undefined}
        aria-expanded={open}
        aria-haspopup="listbox"
        aria-activedescendant={open && activeIndex >= 0 ? `${id}-option-${activeIndex}` : undefined}
        disabled={disabled || firstEnabledIndex < 0}
        tabIndex={tabIndex}
        title={selectedLabel}
        onClick={() => {
          if (open) close(false);
          else openAt(selectedIndex);
        }}
        onKeyDown={handleKeyDown}
      >
        <span className="select-value">{selectedLabel}</span>
        <Icon name="chevronDown" size={14} />
      </button>
      {open ? createPortal(
        <div
          ref={popoverRef}
          id={listboxId}
          className="select-popover"
          role="listbox"
          aria-label={ariaLabel}
          data-placement={position?.placement ?? "below"}
          style={{
            left: position?.left ?? 0,
            top: position?.top ?? 0,
            width: position?.width ?? popoverMinWidth,
            visibility: position ? "visible" : "hidden",
          }}
        >
          {options.map((option, index) => (
            <div
              id={`${id}-option-${index}`}
              key={option.value}
              className="select-option"
              role="option"
              aria-selected={option.value === value}
              aria-disabled={option.disabled || undefined}
              data-active={index === activeIndex}
              data-disabled={Boolean(option.disabled)}
              onPointerDown={(event) => event.preventDefault()}
              onPointerMove={() => {
                if (!option.disabled) setActiveValue(option.value);
              }}
              onClick={() => commit(index)}
            >
              <span>{option.label}</span>
              <Icon name="check" size={14} />
            </div>
          ))}
        </div>,
        document.body,
      ) : null}
    </div>
  );
}

const numberingLabels: Record<NumberingStyle, string> = {
  none: "不添加序号",
  numeric: "连续数字 1、2、3",
  bullet: "项目符号",
  task: "任务复选框",
  timePrefix: "时间前缀",
  dateHeadingNumeric: "日期标题 + 数字",
};

const numberingOptions: SelectOption[] = Object.entries(numberingLabels).map(([value, label]) => ({
  value,
  label,
}));

const numberingSummary = (style: NumberingStyle, start: number) =>
  style === "numeric" || style === "dateHeadingNumeric"
    ? `${numberingLabels[style]} · 从 ${start.toLocaleString()} 开始`
    : numberingLabels[style];

const markdownBlockOptions: SelectOption[] = [
  { value: "paragraph", label: "正文" },
  { value: "heading1", label: "一级标题" },
  { value: "heading2", label: "二级标题" },
  { value: "quote", label: "引用" },
  { value: "code", label: "代码块" },
];

export type MarkdownBlockStyle = "paragraph" | "heading1" | "heading2" | "quote" | "code";

const markdownFencePattern = /^ {0,3}(`{3,}|~{3,})(.*)$/u;
const markdownHeadingOnePattern = /^ {0,3}#(?:[\t ]+|$)/u;
const markdownHeadingTwoPattern = /^ {0,3}##(?:[\t ]+|$)/u;
const markdownQuotePattern = /^ {0,3}>/u;
const markdownOpeningFencePattern = /^ {0,3}(?:`{3,}|~{3,})[^\n]*\n?/u;
const markdownClosingFencePattern = /\n? {0,3}(?:`{3,}|~{3,})[\t ]*$/u;
const markdownHeadingPrefixPattern = /^ {0,3}#{1,6}[\t ]+/gmu;
const markdownQuotePrefixPattern = /^ {0,3}>[\t ]?/gmu;

function normalizeMarkdownSelection(markdown: string, selectionStart: number, selectionEnd: number) {
  const clamp = (position: number) => Math.min(
    markdown.length,
    Math.max(0, Number.isFinite(position) ? Math.trunc(position) : 0),
  );
  const start = clamp(selectionStart);
  const end = clamp(selectionEnd);
  return start <= end ? { start, end } : { start: end, end: start };
}

function selectionIsInsideCodeFence(markdown: string, start: number, end: number): boolean {
  let fenceStart = -1;
  let fenceCharacter = "";
  let fenceLength = 0;
  let lineStart = 0;

  for (const line of markdown.split("\n")) {
    const match = markdownFencePattern.exec(line);
    if (match && fenceStart < 0) {
      fenceStart = lineStart;
      fenceCharacter = match[1][0];
      fenceLength = match[1].length;
    } else if (
      match
      && match[1][0] === fenceCharacter
      && match[1].length >= fenceLength
      && !match[2].trim()
    ) {
      const fenceEnd = lineStart + line.length;
      if (start >= fenceStart && end <= fenceEnd) return true;
      fenceStart = -1;
      fenceCharacter = "";
      fenceLength = 0;
    }
    lineStart += line.length + 1;
  }

  return fenceStart >= 0 && start >= fenceStart && end <= markdown.length;
}

export function detectMarkdownBlockStyle(
  markdown: string,
  selectionStart: number,
  selectionEnd: number,
): MarkdownBlockStyle {
  const { start, end } = normalizeMarkdownSelection(markdown, selectionStart, selectionEnd);
  if (selectionIsInsideCodeFence(markdown, start, end)) return "code";

  const effectiveEnd = end > start && markdown[end - 1] === "\n" ? end - 1 : end;
  const lineStart = markdown.lastIndexOf("\n", Math.max(0, start - 1)) + 1;
  const nextBreak = markdown.indexOf("\n", effectiveEnd);
  const lineEnd = nextBreak < 0 ? markdown.length : nextBreak;
  const lines = markdown.slice(lineStart, lineEnd).split("\n");

  if (lines.every((line) => markdownHeadingOnePattern.test(line))) return "heading1";
  if (lines.every((line) => markdownHeadingTwoPattern.test(line))) return "heading2";
  if (lines.every((line) => markdownQuotePattern.test(line))) return "quote";
  return "paragraph";
}

export function stripMarkdownBlockMarkers(markdown: string): string {
  return markdown
    .replace(markdownOpeningFencePattern, "")
    .replace(markdownClosingFencePattern, "")
    .replace(markdownHeadingPrefixPattern, "")
    .replace(markdownQuotePrefixPattern, "");
}

const submitShortcutOptions: SelectOption[] = [
  { value: "enter", label: "Enter 提交，Shift+Enter 换行" },
  { value: "commandEnter", label: "Command+Enter 提交" },
];

const markdownLayoutOptions: SelectOption[] = [
  { value: "split", label: "原文与预览分栏" },
  { value: "edit", label: "仅原文" },
  { value: "preview", label: "仅预览" },
];

const themeOptions: SelectOption[] = [
  { value: "system", label: "跟随系统" },
  { value: "light", label: "浅色" },
  { value: "dark", label: "深色" },
];

const localDiagnosticsRetentionOptions: SelectOption[] = [
  { value: "1", label: "1 天" },
  { value: "3", label: "3 天" },
  { value: "7", label: "7 天" },
  { value: "14", label: "14 天" },
];

const localDiagnosticsMaxSizeOptions: SelectOption[] = [
  { value: String(1_048_576), label: "1 MiB" },
  { value: String(5_242_880), label: "5 MiB" },
  { value: String(10_485_760), label: "10 MiB" },
  { value: String(20_971_520), label: "20 MiB" },
];

const updateIntervalOptions: SelectOption[] = [
  { value: "6", label: "每 6 小时" },
  { value: "12", label: "每 12 小时" },
  { value: "24", label: "每 24 小时" },
  { value: "48", label: "每 48 小时" },
];

const updatePhaseLabels: Record<UpdateStatus["phase"], string> = {
  unavailable: "正式更新通道尚未配置",
  idle: "尚未检查",
  checking: "正在检查",
  upToDate: "已是最新版本",
  available: "发现新版本",
  downloading: "正在下载",
  verifying: "正在验证签名",
  readyToInstall: "已下载并验证",
  installing: "正在准备安装",
  restarting: "正在重启更新",
  installed: "更新完成",
  rolledBack: "已恢复原版本",
  failed: "最近检查未完成",
};

const updateErrorLabels: Record<string, string> = {
  update_check_failed: "无法连接更新通道",
  update_config_invalid: "更新通道配置无效",
  update_metadata_invalid: "更新元数据未通过安全校验",
  update_signature_invalid: "更新签名验证失败",
  update_download_failed: "下载未完成",
  update_download_incomplete: "下载内容不完整",
  update_download_too_large: "更新包超过安全大小限制",
  update_prepared_changed: "已下载更新发生变化",
  update_write_freeze_failed: "无法安全暂停本地写入",
  update_database_backup_failed: "无法创建更新前数据库备份",
  update_new_app_unhealthy: "新版本未通过启动检查",
  update_rollback_failed: "自动恢复未确认成功",
};

function updateErrorLabel(code: string): string {
  return updateErrorLabels[code] ?? `诊断代码：${code}`;
}

const localDiagnosticSeverityLabels: Record<LocalDiagnosticSeverity, string> = {
  info: "信息",
  warning: "警告",
  error: "错误",
};

const localDiagnosticTimeFormatter = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
});

function formatLocalDiagnosticsBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1_024) return `${bytes} B`;
  const value = bytes / 1_048_576;
  return `${value < 10 ? value.toFixed(2) : value.toFixed(1)} MiB`;
}

function localDataResetDescription(
  preview: LocalDataResetPreview | null,
  unavailable: boolean,
): string {
  if (unavailable) return "无法核对清除范围；WakeGPT 不会启用该操作";
  if (!preview) return "正在核对可清除范围…";
  switch (preview.blockerCode) {
    case "platformUnsupported":
      return "当前平台尚未提供经过验证的系统废纸篓流程";
    case "restartPending":
      return "清除已安排，等待 WakeGPT 重启";
    case "recoveryPending":
      return `请先完成 ${preview.pendingRecoveryOperations} 项待恢复同步`;
    case "defaultIdentityRunning":
    case "defaultIdentityOccupied":
      return "请先关闭默认身份窗口";
    case "defaultIdentityUnavailable":
      return "无法确认默认身份 profile 未被占用";
    default:
      return `将 ${preview.appOwnedItemCount} 类本机项目移入系统废纸篓；工作区文件保持不变`;
  }
}

function safeLocalDiagnosticDate(timestampMs: number): Date | null {
  if (!Number.isFinite(timestampMs)) return null;
  const date = new Date(timestampMs);
  return Number.isNaN(date.getTime()) ? null : date;
}

function formatLocalDiagnosticTime(timestampMs: number): string {
  const date = safeLocalDiagnosticDate(timestampMs);
  return date ? localDiagnosticTimeFormatter.format(date) : "时间不可用";
}

function localDiagnosticDateTime(timestampMs: number): string | undefined {
  return safeLocalDiagnosticDate(timestampMs)?.toISOString();
}

const notebookTargetLabels: Record<Notebook["targetState"], string> = {
  unverified: "正在验证文件",
  ready: "已绑定并持续同步",
  conflict: "文件存在同步冲突",
  unavailable: "文件暂时不可用",
  unbound: "已解除绑定（文件与记录保留）",
};

export function notebookTargetLabel(notebook: Notebook): string {
  if (notebook.attachmentDirectorySyncPending) {
    return "附件目录已保存 · 附件迁移与 Markdown 更新等待恢复";
  }
  if (notebook.numberingSyncPending) {
    return "编号配置已保存 · Markdown 重排等待恢复";
  }
  if (notebook.lastErrorCode === "notebook_converted_to_plain") {
    return "已转为普通 Markdown · 本地记录保留";
  }
  if (notebook.lastErrorCode === "notebook_moved_to_trash") {
    return "文件已移入系统废纸篓 · 本地记录保留";
  }
  return notebookTargetLabels[notebook.targetState];
}

function notebookTabLabel(notebook: Notebook): string {
  return `${notebook.displayName}，${notebookTargetLabel(notebook)}`;
}

const attachmentFileLabels: Record<RecordItem["attachments"][number]["fileState"], string> = {
  ready: "可用",
  trashed: "在系统废纸篓",
  missing: "图片缺失",
  preservedShared: "共享文件已保留",
  modified: "外部修改已保留",
  recoveryRequired: "等待恢复",
};

const attachmentRelocationLabels: Record<
  RecordItem["attachments"][number]["relocationState"],
  string
> = {
  ready: "可用",
  pending: "等待迁移",
  cleanupPending: "等待回收旧副本",
  conflict: "迁移冲突",
};

const codexEndpointLabels: Record<CodexIntegrationStatus["endpointState"], string> = {
  notDetected: "尚未发现调试入口",
  staleEndpoint: "检测到失效的调试入口",
  unmanagedTargetAvailable: "检测到未由 WakeGPT 管理的目标",
  unmanagedTargetIncompatible: "调试目标与当前适配器不兼容",
  connecting: "正在注入 WakeGPT 速记卡",
  connected: "WakeGPT 速记卡已连接",
  partiallyConnected: "部分 ChatGPT 窗口已连接",
  paused: "ChatGPT 速记卡已暂停",
  restartRequired: "需要从 WakeGPT 启动 ChatGPT",
  injectionFailed: "速记卡注入失败，可重试启动",
};

const codexInstanceStateLabels: Record<CodexIntegrationStatus["instances"][number]["state"], string> = {
  unavailable: "未开放安全调试入口",
  incompatible: "版本未验证，已停止注入",
  available: "已发现，等待连接",
  connecting: "正在连接",
  connected: "已连接",
  partiallyConnected: "部分窗口已连接",
  failed: "连接失败",
  paused: "已暂停",
};

const integrationErrorLabels: Record<string, string> = {
  chatgpt_default_target_ambiguous: "检测到多个默认 ChatGPT 实例，WakeGPT 未关闭任何实例",
  chatgpt_home_unavailable: "无法确认当前 macOS 用户目录",
  chatgpt_graceful_termination_failed: "ChatGPT 拒绝正常退出",
  chatgpt_graceful_exit_timeout: "等待 ChatGPT 正常退出超时",
  chatgpt_force_termination_failed: "ChatGPT 关闭窗口后仍未退出，且未接受仅针对该实例的强制退出",
  chatgpt_force_exit_timeout: "仅针对该实例的强制退出仍未完成",
  chatgpt_application_not_found: "未找到 /Applications 中的 ChatGPT",
  chatgpt_launch_failed: "macOS 未能按集成模式启动 ChatGPT",
  chatgpt_instance_arguments_unavailable: "无法安全核对 ChatGPT 实例的启动参数",
  chatgpt_profile_ambiguous: "多个 ChatGPT 进程正在使用同一实例数据",
  chatgpt_profile_claimed_without_debugging: "实例已由其他启动方式重新打开，但未开放安全调试入口",
  chatgpt_replacement_pid_invalid: "重启后仍检测到旧进程标识",
  chatgpt_integration_launch_failed_profile_recovered: "集成启动失败，已恢复普通 ChatGPT",
  chatgpt_integration_start_timeout_profile_recovered: "集成模式未启动，已恢复普通 ChatGPT",
  chatgpt_profile_recovery_launch_failed: "普通 ChatGPT 恢复启动失败",
  chatgpt_profile_recovery_start_timeout: "普通 ChatGPT 恢复启动超时",
  chatgpt_debug_port_unavailable: "WakeGPT 的 ChatGPT 调试端口已被其他程序占用",
  chatgpt_debug_endpoint_start_timeout: "ChatGPT 未在限时内建立调试入口",
  codex_debug_endpoint_unavailable: "检测到的调试端口已经失效",
  codex_version_unverified: "无法验证 ChatGPT 版本与构建号，WakeGPT 未注入卡片",
  codex_version_unsupported: "当前 ChatGPT 版本或构建号尚未经过验证，WakeGPT 未注入卡片",
  codex_context_selector_ambiguous: "ChatGPT 右栏锚点不唯一，WakeGPT 已停止当前窗口的卡片",
  codex_composer_selector_ambiguous: "ChatGPT 输入框不唯一，WakeGPT 未写入任何内容",
  codex_composer_capability_unavailable: "ChatGPT 输入能力尚未就绪，WakeGPT 未写入任何内容",
  codex_image_input_selector_ambiguous: "ChatGPT 图片入口不唯一，WakeGPT 未附加图片",
  codex_instances_unconnectable: "部分 ChatGPT 实例未开放安全回环调试入口",
  codex_all_targets_injection_failed: "所有可连接窗口的速记卡注入均失败",
  codex_card_mount_rejected: "ChatGPT 页面拒绝了速记卡挂载",
  codex_card_host_missing: "速记卡挂载后未能保持在 ChatGPT 页面中",
  codex_adapter_session_panicked: "一个 ChatGPT 窗口的连接会话异常结束",
};

function integrationErrorLabel(code: string): string {
  return integrationErrorLabels[code] ?? `诊断代码：${code}`;
}

const defaultIdentityStateLabels: Record<DefaultIdentityStatus["runtimeState"], string> = {
  stopped: "已配置，当前未运行",
  running: "专用 ChatGPT 正在运行",
  occupied: "检测到重复进程占用，已停止操作",
  unavailable: "本地隔离空间暂时不可用",
};

const defaultIdentityErrorLabels: Record<string, string> = {
  default_identity_profile_occupied: "多个 ChatGPT 进程正在使用同一专用 profile",
  default_identity_process_arguments_unavailable: "无法核对 ChatGPT 进程的 profile 参数",
  default_identity_process_arguments_invalid: "ChatGPT 进程包含重复或无效的 profile 参数",
  default_identity_profile_invalid: "专用 profile 路径不是 WakeGPT 创建的安全目录",
  default_identity_profile_unavailable: "无法读取专用 profile 目录",
  default_identity_profile_permissions_failed: "无法把专用 profile 权限限制为当前用户",
  default_identity_unsupported_platform: "当前平台尚未验证默认身份守护",
};

function defaultIdentityErrorLabel(code: string): string {
  return defaultIdentityErrorLabels[code] ?? `诊断代码：${code}`;
}

function notebookName(notebooks: Notebook[], notebookId: string | null): string {
  if (!notebookId) return "收件箱";
  return notebooks.find((notebook) => notebook.id === notebookId)?.displayName ?? "未知笔记";
}

interface AppSidebarProps {
  searchRef: RefObject<HTMLInputElement | null>;
  workspaces: Workspace[];
  activeWorkspaceId: string;
  activeNotebook: Notebook | null;
  status: AppStatus | null;
  view: AppView;
  search: string;
  busyAddingWorkspace: boolean;
  busyWorkspaceAction: boolean;
  onSearchChange: (value: string) => void;
  onWorkspaceSelect: (workspaceId: string) => void;
  onWorkspaceSettings: () => void;
  onRevealWorkspace: (workspaceId: string) => void;
  onDisconnectWorkspace: (workspace: Workspace) => void;
  onAddWorkspace: () => void;
  onQuickCapture: () => void;
  onViewChange: (view: AppView) => void;
}

export function AppSidebar({
  searchRef,
  workspaces,
  activeWorkspaceId,
  activeNotebook,
  status,
  view,
  search,
  busyAddingWorkspace,
  busyWorkspaceAction,
  onSearchChange,
  onWorkspaceSelect,
  onWorkspaceSettings,
  onRevealWorkspace,
  onDisconnectWorkspace,
  onAddWorkspace,
  onQuickCapture,
  onViewChange,
}: AppSidebarProps) {
  const [workspaceMenuOpen, setWorkspaceMenuOpen] = useState(false);
  const [workspaceMenuPosition, setWorkspaceMenuPosition] = useState<SelectPosition | null>(null);
  const workspaceMenuRef = useRef<HTMLDivElement>(null);
  const workspaceMenuButtonRef = useRef<HTMLButtonElement>(null);
  const workspaceMenuElementRef = useRef<HTMLDivElement>(null);
  const workspaceMenuFocusEdgeRef = useRef<"first" | "last">("first");
  const sidebarSyncPrimary = !activeNotebook
    ? "仅保存在本地收件箱"
    : activeNotebook.targetState === "ready"
      ? `同步到 ${activeNotebook.relativePath}`
      : notebookTargetLabel(activeNotebook);
  const sidebarSyncSecondary = status?.pendingRecoveryOperations
    ? `${status.pendingRecoveryOperations} 项等待恢复`
    : !activeNotebook
      ? "选择笔记后同步 Markdown"
      : activeNotebook.targetState === "ready"
        ? "同步状态正常"
        : activeNotebook.relativePath;
  const closeWorkspaceMenu = useCallback((restoreFocus = false) => {
    setWorkspaceMenuOpen(false);
    setWorkspaceMenuPosition(null);
    if (restoreFocus) {
      window.requestAnimationFrame(() => workspaceMenuButtonRef.current?.focus());
    }
  }, []);

  const openWorkspaceMenu = useCallback((edge: "first" | "last" = "first") => {
    workspaceMenuFocusEdgeRef.current = edge;
    setWorkspaceMenuPosition(null);
    setWorkspaceMenuOpen(true);
  }, []);

  const moveFocusFromWorkspaceMenu = useCallback((backwards: boolean) => {
    const trigger = workspaceMenuButtonRef.current;
    if (!trigger) {
      closeWorkspaceMenu(false);
      return;
    }
    const focusable = pageFocusableElements(workspaceMenuElementRef.current);
    const triggerIndex = focusable.indexOf(trigger);
    const target = triggerIndex < 0
      ? null
      : focusable[triggerIndex + (backwards ? -1 : 1)] ?? null;
    closeWorkspaceMenu(false);
    window.requestAnimationFrame(() => {
      if (target?.isConnected) target.focus({ preventScroll: true });
      else if (trigger.isConnected) trigger.focus({ preventScroll: true });
    });
  }, [closeWorkspaceMenu]);

  useLayoutEffect(() => {
    if (!workspaceMenuOpen) return;
    const trigger = workspaceMenuButtonRef.current;
    const menu = workspaceMenuElementRef.current;
    if (!trigger || !menu) return;
    const menuRect = menu.getBoundingClientRect();
    setWorkspaceMenuPosition(anchoredOverlayPosition(
      trigger.getBoundingClientRect(),
      menuRect,
      { width: window.innerWidth, height: window.innerHeight },
      { width: menuRect.width, align: "end" },
    ));
  }, [workspaceMenuOpen]);

  useEffect(() => {
    closeWorkspaceMenu(false);
  }, [activeWorkspaceId, closeWorkspaceMenu]);

  useEffect(() => {
    if (!workspaceMenuOpen) return undefined;
    window.requestAnimationFrame(() => {
      const items = [...workspaceMenuElementRef.current?.querySelectorAll<HTMLButtonElement>(
        "[role='menuitem']:not(:disabled)",
      ) ?? []];
      const index = workspaceMenuFocusEdgeRef.current === "last" ? items.length - 1 : 0;
      items[index]?.focus({ preventScroll: true });
    });
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (workspaceMenuRef.current?.contains(target) || workspaceMenuElementRef.current?.contains(target)) return;
      closeWorkspaceMenu(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeWorkspaceMenu(true);
        return;
      }
      if (event.key === "Tab" && workspaceMenuElementRef.current?.contains(document.activeElement)) {
        event.preventDefault();
        moveFocusFromWorkspaceMenu(event.shiftKey);
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)
        || !workspaceMenuElementRef.current?.contains(document.activeElement)) return;
      const items = [...workspaceMenuElementRef.current.querySelectorAll<HTMLButtonElement>(
        "[role='menuitem']:not(:disabled)",
      )];
      if (!items.length) return;
      event.preventDefault();
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      const next = menuItemIndexAfterKey(current, items.length, event.key);
      items[next]?.focus();
    };
    const handleViewportChange = (event: Event) => {
      const target = event.target;
      if (event.type === "scroll" && target instanceof Node
        && workspaceMenuElementRef.current?.contains(target)) return;
      closeWorkspaceMenu(false);
    };
    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("blur", handleViewportChange);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("blur", handleViewportChange);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [closeWorkspaceMenu, moveFocusFromWorkspaceMenu, workspaceMenuOpen]);

  return (
    <aside className="app-sidebar">
      <div className="window-drag-region window-drag-region--sidebar" data-tauri-drag-region aria-hidden="true" />
      <div className="sidebar-brand">
        <WakeMark />
        <strong>WakeGPT</strong>
        <button className="icon-button" type="button" aria-label="快速记录" onClick={onQuickCapture}>
          <Icon name="edit" />
        </button>
      </div>

      <label className="search-field">
        <Icon name="search" size={16} />
        <input
          ref={searchRef}
          type="search"
          aria-label="搜索记录"
          value={search}
          placeholder="搜索记录…"
          onChange={(event) => onSearchChange(event.currentTarget.value)}
        />
        <kbd>⌘K</kbd>
      </label>

      <div className="sidebar-section-heading">
        <span>工作区</span>
        <button
          className="icon-button icon-button--small"
          type="button"
          aria-label="添加工作区"
          disabled={busyAddingWorkspace}
          onClick={onAddWorkspace}
        >
          <Icon name="plus" size={16} />
        </button>
      </div>

      <nav className="workspace-list" aria-label="工作区">
        {workspaces.map((workspace) => {
          const active = workspace.id === activeWorkspaceId;
          return (
            <div
              key={workspace.id}
              ref={active ? workspaceMenuRef : undefined}
              className="workspace-row"
              data-active={active}
            >
              <button
                className="sidebar-row workspace-select"
                type="button"
                title={workspace.displayName}
                aria-label={workspace.displayName}
                onClick={() => {
                  closeWorkspaceMenu(false);
                  onWorkspaceSelect(workspace.id);
                }}
              >
                <Icon name="folder" size={17} />
                <span>{workspace.displayName}</span>
              </button>
              {active ? (
                <button
                  ref={workspaceMenuButtonRef}
                  className="workspace-more"
                  type="button"
                  aria-label={`${workspace.displayName} 工作区菜单`}
                  aria-haspopup="menu"
                  aria-expanded={workspaceMenuOpen}
                  aria-controls={workspaceMenuOpen ? "active-workspace-menu" : undefined}
                  disabled={busyWorkspaceAction}
                  onClick={() => {
                    if (workspaceMenuOpen) closeWorkspaceMenu(false);
                    else openWorkspaceMenu();
                  }}
                  onKeyDown={(event) => {
                    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
                    event.preventDefault();
                    openWorkspaceMenu(event.key === "ArrowUp" ? "last" : "first");
                  }}
                >
                  <Icon name="more" size={16} />
                </button>
              ) : null}
              {active && workspaceMenuOpen ? createPortal(
                <div
                  ref={workspaceMenuElementRef}
                  id="active-workspace-menu"
                  className="workspace-menu"
                  role="menu"
                  aria-label={`${workspace.displayName} 工作区操作`}
                  data-placement={workspaceMenuPosition?.placement ?? "below"}
                  style={{
                    left: workspaceMenuPosition?.left ?? 0,
                    top: workspaceMenuPosition?.top ?? 0,
                    width: workspaceMenuPosition?.width,
                    visibility: workspaceMenuPosition ? "visible" : "hidden",
                  }}
                >
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    onClick={() => {
                      closeWorkspaceMenu(false);
                      onRevealWorkspace(workspace.id);
                    }}
                  >
                    <Icon name="folderOpen" size={15} />
                    在 Finder 中显示
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    onClick={() => {
                      closeWorkspaceMenu(false);
                      onWorkspaceSettings();
                    }}
                  >
                    <Icon name="settings" size={15} />
                    工作区设置
                  </button>
                  <span className="workspace-menu__separator" role="separator" />
                  <button
                    type="button"
                    role="menuitem"
                    tabIndex={-1}
                    data-danger="true"
                    onClick={() => {
                      closeWorkspaceMenu(false);
                      onDisconnectWorkspace(workspace);
                    }}
                  >
                    <Icon name="unlink" size={15} />
                    移除工作区
                  </button>
                </div>,
                document.body,
              ) : null}
            </div>
          );
        })}
        {!workspaces.length ? <p className="sidebar-empty">尚未连接工作区</p> : null}
      </nav>

      <div className="sidebar-spacer" />

      <nav className="sidebar-utilities" aria-label="应用">
        <button
          className="sidebar-row"
          data-active={view === "trash"}
          type="button"
          onClick={() => onViewChange("trash")}
        >
          <Icon name="trash" size={17} />
          <span>废纸篓</span>
        </button>
        <div
          className="sidebar-sync"
          role="status"
          aria-label={`${sidebarSyncPrimary}。${sidebarSyncSecondary}`}
        >
          <span className="sync-dot" data-state={activeNotebook?.targetState ?? "unbound"} aria-hidden="true" />
          <div>
            <strong title={sidebarSyncPrimary}>{sidebarSyncPrimary}</strong>
            <small title={sidebarSyncSecondary}>{sidebarSyncSecondary}</small>
          </div>
        </div>
        <button
          className="sidebar-row"
          data-active={view === "settings"}
          type="button"
          onClick={() => onViewChange("settings")}
        >
          <Icon name="settings" size={17} />
          <span>设置</span>
        </button>
      </nav>
    </aside>
  );
}

interface NotebookTabsProps {
  notebooks: Notebook[];
  activeNotebookId: string | null;
  disabled: boolean;
  onSelect: (notebookId: string | null) => void;
  onReorder: (orderedNotebookIds: string[]) => void;
  onPinChange: (notebookId: string, pinned: boolean) => void;
  onCreate: () => void;
}

export function tabScrollLeftToReveal(
  currentScrollLeft: number,
  viewportLeft: number,
  viewportRight: number,
  activeLeft: number,
  activeRight: number,
): number {
  if (activeLeft < viewportLeft) {
    return Math.max(0, currentScrollLeft - (viewportLeft - activeLeft));
  }
  if (activeRight > viewportRight) {
    return currentScrollLeft + activeRight - viewportRight;
  }
  return currentScrollLeft;
}

export function activeNotebookAfterTabRefresh(
  activeNotebookId: string | null,
  notebooks: Pick<Notebook, "id" | "isPinned">[],
): string | null {
  return activeNotebookId
    && notebooks.some((notebook) => notebook.id === activeNotebookId && notebook.isPinned)
    ? activeNotebookId
    : null;
}

export function tabIndexAfterKey(
  currentIndex: number,
  itemCount: number,
  key: string,
): number {
  if (itemCount < 1 || currentIndex < 0 || currentIndex >= itemCount) return currentIndex;
  if (key === "Home") return 0;
  if (key === "End") return itemCount - 1;
  if (key === "ArrowLeft") return (currentIndex - 1 + itemCount) % itemCount;
  if (key === "ArrowRight") return (currentIndex + 1) % itemCount;
  return currentIndex;
}

export function NotebookTabs({
  notebooks,
  activeNotebookId,
  disabled,
  onSelect,
  onReorder,
  onPinChange,
  onCreate,
}: NotebookTabsProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedNotebooks = notebooks.filter((notebook) => notebook.isPinned);
  const unpinnedNotebooks = notebooks.filter((notebook) => !notebook.isPinned);
  const tabIds = [null, ...pinnedNotebooks.map((notebook) => notebook.id)];
  const [focusedTabId, setFocusedTabId] = useState<string | null>(activeNotebookId);
  const focusedTabIndex = Math.max(0, tabIds.indexOf(focusedTabId));
  const pinnedNotebookKey = pinnedNotebooks
    .map((notebook) => `${notebook.id}:${notebook.displayName}`)
    .join("\u0000");

  useLayoutEffect(() => {
    const scrollElement = scrollRef.current;
    if (!scrollElement || disabled) return;

    const revealActiveTab = () => {
      const activeTab = scrollElement.querySelector<HTMLElement>('[data-active="true"]');
      if (!activeTab) return;
      const viewport = scrollElement.getBoundingClientRect();
      const active = activeTab.getBoundingClientRect();
      scrollElement.scrollLeft = tabScrollLeftToReveal(
        scrollElement.scrollLeft,
        viewport.left,
        viewport.right,
        active.left,
        active.right,
      );
    };

    revealActiveTab();
    const resizeObserver = new ResizeObserver(revealActiveTab);
    resizeObserver.observe(scrollElement);
    const activeTab = scrollElement.querySelector<HTMLElement>('[data-active="true"]');
    if (activeTab) resizeObserver.observe(activeTab);
    return () => resizeObserver.disconnect();
  }, [activeNotebookId, disabled, pinnedNotebookKey]);

  useEffect(() => {
    setFocusedTabId(activeNotebookId);
  }, [activeNotebookId, disabled, pinnedNotebookKey]);

  const moveNotebook = (sourceId: string, targetId: string) => {
    if (sourceId === targetId) return;
    const ids = notebooks.map((notebook) => notebook.id);
    const sourceIndex = ids.indexOf(sourceId);
    const targetIndex = ids.indexOf(targetId);
    if (sourceIndex < 0 || targetIndex < 0) return;
    ids.splice(targetIndex, 0, ids.splice(sourceIndex, 1)[0]);
    onReorder(ids);
  };

  const moveNotebookBy = (notebookId: string, delta: -1 | 1) => {
    const ids = pinnedNotebooks.map((notebook) => notebook.id);
    const index = ids.indexOf(notebookId);
    const destination = index + delta;
    if (index < 0 || destination < 0 || destination >= ids.length) return;
    moveNotebook(notebookId, ids[destination]);
  };

  const focusTabAtIndex = (index: number) => {
    if (index < 0 || index >= tabIds.length) return;
    setFocusedTabId(tabIds[index]);
    window.requestAnimationFrame(() => {
      scrollRef.current
        ?.querySelector<HTMLButtonElement>(`[data-tab-index="${index}"]`)
        ?.focus({ preventScroll: true });
    });
  };

  const handleTabKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    notebookId: string | null,
  ) => {
    if (event.metaKey || event.ctrlKey) return;
    if (event.key === "Delete" && notebookId) {
      event.preventDefault();
      const fallbackId = notebookId === activeNotebookId ? null : activeNotebookId;
      focusTabAtIndex(Math.max(0, tabIds.indexOf(fallbackId)));
      onPinChange(notebookId, false);
      return;
    }
    if (disabled || event.altKey) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onSelect(notebookId);
      return;
    }
    const currentIndex = tabIds.indexOf(notebookId);
    const nextIndex = tabIndexAfterKey(currentIndex, tabIds.length, event.key);
    if (nextIndex === currentIndex) return;
    event.preventDefault();
    focusTabAtIndex(nextIndex);
  };

  return (
    <div className="notebook-tabs" data-tauri-drag-region>
      <div
        ref={scrollRef}
        className="notebook-tabs__scroll"
        role={disabled ? undefined : "tablist"}
        aria-label={disabled ? "打开速记位置" : "速记位置"}
        aria-orientation={disabled ? undefined : "horizontal"}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) {
            setFocusedTabId(activeNotebookId);
          }
        }}
      >
        <button
          id="wakegpt-notebook-tab-inbox"
          className="notebook-tab"
          data-active={!disabled && activeNotebookId === null}
          data-tab-index={0}
          type="button"
          role={disabled ? undefined : "tab"}
          aria-selected={disabled ? undefined : activeNotebookId === null}
          aria-controls={disabled ? undefined : "wakegpt-notes-panel"}
          tabIndex={disabled ? 0 : focusedTabIndex === 0 ? 0 : -1}
          onFocus={() => setFocusedTabId(null)}
          onKeyDown={(event) => handleTabKeyDown(event, null)}
          onClick={() => onSelect(null)}
        >
          <Icon name="inbox" size={15} />
          收件箱
        </button>
        {pinnedNotebooks.map((notebook) => (
          <div
            key={notebook.id}
            className="notebook-tab-shell"
            role={disabled ? undefined : "presentation"}
            data-active={!disabled && notebook.id === activeNotebookId}
            draggable={!disabled}
            onDragStart={(event) => {
              event.dataTransfer.effectAllowed = "move";
              event.dataTransfer.setData("text/x-wakegpt-notebook", notebook.id);
            }}
            onDragOver={(event) => {
              if (event.dataTransfer.types.includes("text/x-wakegpt-notebook")) {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
              }
            }}
            onDrop={(event) => {
              event.preventDefault();
              moveNotebook(
                event.dataTransfer.getData("text/x-wakegpt-notebook"),
                notebook.id,
              );
            }}
          >
            <button
              id={`wakegpt-notebook-tab-${notebook.id}`}
              className="notebook-tab notebook-tab--bound"
              data-tab-index={tabIds.indexOf(notebook.id)}
              type="button"
              role={disabled ? undefined : "tab"}
              aria-label={notebookTabLabel(notebook)}
              aria-selected={disabled ? undefined : notebook.id === activeNotebookId}
              aria-controls={disabled ? undefined : "wakegpt-notes-panel"}
              aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight Delete"
              tabIndex={disabled ? 0 : notebook.id === focusedTabId ? 0 : -1}
              onFocus={() => setFocusedTabId(notebook.id)}
              onKeyDown={(event) => {
                if (!event.altKey) {
                  handleTabKeyDown(event, notebook.id);
                } else if (event.key === "ArrowLeft") {
                  event.preventDefault();
                  event.stopPropagation();
                  moveNotebookBy(notebook.id, -1);
                } else if (event.key === "ArrowRight") {
                  event.preventDefault();
                  event.stopPropagation();
                  moveNotebookBy(notebook.id, 1);
                }
              }}
              onClick={(event) => {
                const target = event.target;
                if (target instanceof Element && target.closest(".notebook-tab-close")) {
                  const fallbackId = notebook.id === activeNotebookId ? null : activeNotebookId;
                  focusTabAtIndex(Math.max(0, tabIds.indexOf(fallbackId)));
                  onPinChange(notebook.id, false);
                  return;
                }
                onSelect(notebook.id);
              }}
            >
              <span className="tab-dot" data-state={notebook.targetState} aria-hidden="true" />
              <span className="notebook-tab-label">{notebook.displayName}</span>
              <span
                className="notebook-tab-close"
                title={`关闭 ${notebook.displayName} Tab（保留速记本）`}
                aria-hidden="true"
              >
                <Icon name="x" size={13} />
              </span>
            </button>
          </div>
        ))}
      </div>
      {unpinnedNotebooks.length ? (
        <SelectControl
          className="notebook-tab-overflow"
          ariaLabel="打开未固定的速记本"
          value=""
          placeholder={`更多（${unpinnedNotebooks.length}）`}
          popoverMinWidth={190}
          options={unpinnedNotebooks.map((notebook) => ({
            value: notebook.id,
            label: notebook.displayName,
          }))}
          onChange={(notebookId) => {
            if (notebookId) onSelect(notebookId);
          }}
        />
      ) : null}
      <button className="new-tab-button" type="button" aria-label="新建笔记" onClick={onCreate}>
        <Icon name="plus" size={17} />
      </button>
    </div>
  );
}

interface RecordComposerProps {
  value: string;
  busy: boolean;
  imageBusy: boolean;
  syncEnabled: boolean;
  submitShortcut: SubmitShortcut;
  disabledReason?: string;
  attachments: PendingAttachment[];
  onChange: (value: string) => void;
  onAddImageFiles: (files: File[]) => void;
  onPickImages: () => void;
  onRemoveAttachment: (token: string) => void;
  onSubmit: () => void;
}

function transferFiles(transfer: DataTransfer): File[] {
  const files = Array.from(transfer.files);
  if (files.length) return files;
  return Array.from(transfer.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
}

function transferContainsFiles(transfer: DataTransfer): boolean {
  return Array.from(transfer.types).includes("Files") || transfer.files.length > 0;
}

export function toolbarIndexAfterKey(
  currentIndex: number,
  enabledItems: readonly boolean[],
  key: string,
): number {
  const enabledIndices = enabledItems
    .map((enabled, index) => (enabled ? index : -1))
    .filter((index) => index >= 0);
  if (!enabledIndices.length) return -1;
  if (key === "Home") return enabledIndices[0];
  if (key === "End") return enabledIndices[enabledIndices.length - 1];
  if (key !== "ArrowLeft" && key !== "ArrowRight") return currentIndex;
  const currentPosition = enabledIndices.indexOf(currentIndex);
  if (currentPosition < 0) {
    return key === "ArrowLeft" ? enabledIndices[enabledIndices.length - 1] : enabledIndices[0];
  }
  const direction = key === "ArrowRight" ? 1 : -1;
  return enabledIndices[
    (currentPosition + direction + enabledIndices.length) % enabledIndices.length
  ];
}

function toolbarControlIsVisible(control: HTMLButtonElement): boolean {
  if (control.hidden || control.closest("[hidden], [inert], [aria-hidden='true']")) return false;
  const style = window.getComputedStyle(control);
  return style.display !== "none" && style.visibility !== "hidden";
}

function toolbarControlElements(
  toolbar: HTMLElement | null,
  visibleOnly = false,
): HTMLButtonElement[] {
  if (!toolbar) return [];
  return Array.from(toolbar.querySelectorAll<HTMLButtonElement>("button")).filter(
    (button) => button.closest("[role='toolbar']") === toolbar
      && (!visibleOnly || toolbarControlIsVisible(button)),
  );
}

export const RecordComposer = forwardRef<HTMLTextAreaElement, RecordComposerProps>(
  function RecordComposer(
    {
      value,
      busy,
      imageBusy,
      syncEnabled,
      submitShortcut,
      disabledReason,
      attachments,
      onChange,
      onAddImageFiles,
      onPickImages,
      onRemoveAttachment,
      onSubmit,
    },
    forwardedRef,
  ) {
    const localRef = useRef<HTMLTextAreaElement | null>(null);
    const toolbarRef = useRef<HTMLDivElement | null>(null);
    const dragDepth = useRef(0);
    const [dropActive, setDropActive] = useState(false);
    const [lightbox, setLightbox] = useState<ImageLightboxState | null>(null);
    const [selection, setSelection] = useState({ start: 0, end: 0 });
    const [toolbarFocusIndex, setToolbarFocusIndex] = useState(0);
    const lightboxTrigger = useRef<HTMLButtonElement | null>(null);
    const blockStyle = detectMarkdownBlockStyle(value, selection.start, selection.end);
    const toolbarEnabledItems = [
      !disabledReason,
      !disabledReason,
      !disabledReason,
      !disabledReason,
      !disabledReason,
      !disabledReason && !imageBusy && attachments.length < 10,
      !disabledReason,
      !disabledReason,
    ];
    const toolbarEntryIndex = toolbarEnabledItems[toolbarFocusIndex]
      ? toolbarFocusIndex
      : toolbarEnabledItems.findIndex(Boolean);
    const toolbarTabIndex = (index: number) => (index === toolbarEntryIndex ? 0 : -1);

    const closeLightbox = () => {
      setLightbox(null);
      window.requestAnimationFrame(() => lightboxTrigger.current?.focus({ preventScroll: true }));
    };

    useEffect(() => {
      if (disabledReason || imageBusy) {
        dragDepth.current = 0;
        setDropActive(false);
      }
    }, [disabledReason, imageBusy]);

    useLayoutEffect(() => {
      const allControls = toolbarControlElements(toolbarRef.current);
      const visibleControls = toolbarControlElements(toolbarRef.current, true);
      const currentControl = allControls[toolbarEntryIndex];
      const currentControlAvailable = currentControl
        && toolbarControlIsVisible(currentControl)
        && !currentControl.disabled;
      const fallback = currentControlAvailable
        ? currentControl
        : visibleControls.find((control) => !control.disabled);
      const fallbackIndex = fallback ? allControls.indexOf(fallback) : -1;
      if (fallbackIndex >= 0 && fallbackIndex !== toolbarFocusIndex) {
        setToolbarFocusIndex(fallbackIndex);
      }
      const focusedControl = document.activeElement instanceof HTMLButtonElement
        && allControls.includes(document.activeElement)
        ? document.activeElement
        : null;
      if (focusedControl && (focusedControl.disabled || !toolbarControlIsVisible(focusedControl))) {
        fallback?.focus({ preventScroll: true });
      }
    }, [toolbarEntryIndex, toolbarFocusIndex]);

    const setRef = (element: HTMLTextAreaElement | null) => {
      localRef.current = element;
      if (typeof forwardedRef === "function") forwardedRef(element);
      else if (forwardedRef) forwardedRef.current = element;
    };

    const rememberSelection = (textarea: HTMLTextAreaElement) => {
      const next = { start: textarea.selectionStart, end: textarea.selectionEnd };
      setSelection((current) => (
        current.start === next.start && current.end === next.end ? current : next
      ));
    };

    const insertMarkdown = (prefix: string, suffix: string, fallback: string) => {
      const textarea = localRef.current;
      if (!textarea) return;
      const start = textarea.selectionStart;
      const end = textarea.selectionEnd;
      const selected = value.slice(start, end) || fallback;
      const next = `${value.slice(0, start)}${prefix}${selected}${suffix}${value.slice(end)}`;
      const cursor = start + prefix.length + selected.length + suffix.length;
      onChange(next);
      setSelection({ start: cursor, end: cursor });
      window.requestAnimationFrame(() => {
        textarea.focus();
        textarea.setSelectionRange(cursor, cursor);
      });
    };

    const applyBlockStyle = (style: MarkdownBlockStyle) => {
      const textarea = localRef.current;
      if (!textarea) return;
      const selectionStart = textarea.selectionStart;
      const selectionEnd = textarea.selectionEnd;
      const blockStart = value.lastIndexOf("\n", Math.max(0, selectionStart - 1)) + 1;
      const nextBreak = value.indexOf("\n", selectionEnd);
      const blockEnd = nextBreak < 0 ? value.length : nextBreak;
      const selectedBlock = value.slice(blockStart, blockEnd) || "正文";
      const cleaned = stripMarkdownBlockMarkers(selectedBlock);
      const nextBlock = style === "heading1"
        ? `# ${cleaned}`
        : style === "heading2"
          ? `## ${cleaned}`
          : style === "quote"
            ? cleaned.split("\n").map((line) => `> ${line}`).join("\n")
            : style === "code"
              ? `\`\`\`\n${cleaned}\n\`\`\``
              : cleaned;
      onChange(`${value.slice(0, blockStart)}${nextBlock}${value.slice(blockEnd)}`);
      setSelection({ start: blockStart, end: blockStart + nextBlock.length });
      window.requestAnimationFrame(() => {
        window.requestAnimationFrame(() => {
          textarea.focus();
          textarea.setSelectionRange(blockStart, blockStart + nextBlock.length);
        });
      });
    };

    const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (disabledReason) return;
      if (event.nativeEvent.isComposing || event.key !== "Enter") return;
      const submits = submitShortcut === "commandEnter"
        ? event.metaKey || event.ctrlKey
        : !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey;
      if (submits) {
        event.preventDefault();
        onSubmit();
      }
    };

    const handleToolbarFocus = (event: React.FocusEvent<HTMLDivElement>) => {
      if (!(event.target instanceof HTMLButtonElement)) return;
      const index = toolbarControlElements(event.currentTarget).indexOf(event.target);
      if (index >= 0) setToolbarFocusIndex(index);
    };

    const handleToolbarKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (event.nativeEvent.isComposing || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
      if (!(event.target instanceof HTMLButtonElement)) return;
      const allControls = toolbarControlElements(event.currentTarget);
      const controls = toolbarControlElements(event.currentTarget, true);
      const currentIndex = controls.indexOf(event.target);
      if (currentIndex < 0 || event.target.getAttribute("aria-expanded") === "true") return;
      const nextIndex = toolbarIndexAfterKey(
        currentIndex,
        controls.map((control) => !control.disabled),
        event.key,
      );
      if (nextIndex < 0) return;
      event.preventDefault();
      event.stopPropagation();
      const nextControl = controls[nextIndex];
      setToolbarFocusIndex(allControls.indexOf(nextControl));
      nextControl?.focus({ preventScroll: true });
    };

    const handleDragEnter = (event: React.DragEvent<HTMLElement>) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      dragDepth.current += 1;
      if (!disabledReason && !imageBusy) setDropActive(true);
    };

    const handleDragOver = (event: React.DragEvent<HTMLElement>) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = disabledReason || imageBusy ? "none" : "copy";
    };

    const handleDragLeave = (event: React.DragEvent<HTMLElement>) => {
      event.preventDefault();
      dragDepth.current = Math.max(0, dragDepth.current - 1);
      if (dragDepth.current === 0) setDropActive(false);
    };

    const handleDrop = (event: React.DragEvent<HTMLElement>) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      dragDepth.current = 0;
      setDropActive(false);
      if (disabledReason || imageBusy) return;
      const files = transferFiles(event.dataTransfer);
      if (files.length) onAddImageFiles(files);
    };

    const handlePaste = (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
      if (disabledReason || imageBusy) return;
      const files = transferFiles(event.clipboardData);
      if (files.length) onAddImageFiles(files);
    };

    return (
      <section
        className="composer-card"
        aria-label="新建速记"
        aria-busy={imageBusy}
        data-drop-active={dropActive}
        onDragEnter={handleDragEnter}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
      >
        {dropActive ? (
          <div className="composer-drop-overlay" role="status">
            <Icon name="image" size={24} />
            <strong>松开以添加图片</strong>
            <span>支持 PNG、JPEG、WebP 和 GIF；原图不会被移动</span>
          </div>
        ) : null}
        <div
          ref={toolbarRef}
          className="composer-toolbar"
          role="toolbar"
          aria-label="Markdown 格式"
          aria-orientation="horizontal"
          onFocusCapture={handleToolbarFocus}
          onKeyDownCapture={handleToolbarKeyDown}
        >
          <SelectControl
            className="toolbar-style"
            ariaLabel="Markdown 块类型"
            value={blockStyle}
            options={markdownBlockOptions}
            disabled={Boolean(disabledReason)}
            popoverMinWidth={156}
            tabIndex={toolbarTabIndex(0)}
            onChange={(nextValue) => applyBlockStyle(
              nextValue as MarkdownBlockStyle,
            )}
          />
          <span className="toolbar-divider" />
          <button type="button" tabIndex={toolbarTabIndex(1)} aria-label="粗体" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("**", "**", "粗体文字")}><Icon name="bold" size={16} /></button>
          <button type="button" tabIndex={toolbarTabIndex(2)} aria-label="斜体" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("_", "_", "斜体文字")}><Icon name="italic" size={16} /></button>
          <button type="button" tabIndex={toolbarTabIndex(3)} aria-label="行内代码" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("`", "`", "代码")}><Icon name="code" size={17} /></button>
          <button type="button" tabIndex={toolbarTabIndex(4)} aria-label="链接" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("[", "](https://)", "链接文字")}><Icon name="link" size={17} /></button>
          <button type="button" tabIndex={toolbarTabIndex(5)} aria-label="添加真实图片" disabled={Boolean(disabledReason) || imageBusy || attachments.length >= 10} onClick={onPickImages}><Icon name="image" size={17} /></button>
          <span className="toolbar-divider" />
          <button type="button" tabIndex={toolbarTabIndex(6)} aria-label="项目列表" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("- ", "", "列表项")}><Icon name="list" size={17} /></button>
          <button type="button" tabIndex={toolbarTabIndex(7)} aria-label="任务列表" disabled={Boolean(disabledReason)} onClick={() => insertMarkdown("- [ ] ", "", "待办事项")}><Icon name="task" size={17} /></button>
        </div>
        <textarea
          ref={setRef}
          value={value}
          placeholder="记录想法、粘贴图片或链接，输入 Markdown…"
          maxLength={1_048_576}
          aria-keyshortcuts={submitShortcut === "commandEnter" ? "Meta+Enter Control+Enter" : "Enter"}
          disabled={Boolean(disabledReason)}
          onChange={(event) => {
            onChange(event.currentTarget.value);
            rememberSelection(event.currentTarget);
          }}
          onClick={(event) => rememberSelection(event.currentTarget)}
          onKeyDown={handleKeyDown}
          onKeyUp={(event) => rememberSelection(event.currentTarget)}
          onPaste={handlePaste}
          onSelect={(event) => rememberSelection(event.currentTarget)}
        />
        {attachments.length ? (
          <div className="composer-attachments" aria-label="待提交图片">
            {attachments.map((attachment) => (
              <div key={attachment.token} className="attachment-tile">
                <ImagePreviewButton
                  source={{
                    kind: "pending",
                    token: attachment.token,
                    mediaType: attachment.mediaType,
                    available: true,
                  }}
                  label={attachment.displayName}
                  onOpen={(preview, trigger) => {
                    lightboxTrigger.current = trigger;
                    setLightbox(preview);
                  }}
                />
                <div className="attachment-tile__meta">
                  <span title={attachment.displayName}>{attachment.displayName}</span>
                  <small>{formatBytes(attachment.byteSize)}</small>
                </div>
                <button
                  className="attachment-tile__remove"
                  type="button"
                  aria-label={`移除 ${attachment.displayName}`}
                  disabled={imageBusy}
                  onClick={() => onRemoveAttachment(attachment.token)}
                >
                  <Icon name="x" size={13} />
                </button>
              </div>
            ))}
          </div>
        ) : null}
        <footer className="composer-footer">
          <span>
            {disabledReason ?? (syncEnabled ? "提交后持续追加到当前 Markdown" : "当前记录保存在本地收件箱")}
            {!disabledReason && (submitShortcut === "commandEnter" ? " · ⌘↵ 提交" : " · ↵ 提交，⇧↵ 换行")}
          </span>
          <div>
            <small>{value.length.toLocaleString()} 字符</small>
            <button
              className="primary-button"
              type="button"
              disabled={busy || Boolean(disabledReason) || (!value.trim() && !attachments.length)}
              onClick={onSubmit}
            >
              {busy ? "记录中…" : "记录"}
            </button>
          </div>
        </footer>
        <ImageLightbox preview={lightbox} onClose={closeLightbox} />
      </section>
    );
  },
);

function localCommandMessage(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message) return message;
  }
  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error) as { message?: unknown };
      if (typeof parsed.message === "string" && parsed.message) return parsed.message;
    } catch {
      if (error) return error;
    }
  }
  return "图片未添加，请重试。";
}

interface RecordListProps {
  title: string;
  records: RecordItem[];
  notebooks: Notebook[];
  activeNotebookId: string | null;
  readOnly?: boolean;
  busyRecordId: string;
  loading: boolean;
  mode: "active" | "trash";
  onUpdate: (
    record: RecordItem,
    bodyMarkdown: string,
    retainedAttachmentIds: string[],
    newAttachments: PendingAttachment[],
  ) => Promise<boolean>;
  onTrash: (record: RecordItem) => Promise<boolean>;
  onRestore: (record: RecordItem) => Promise<boolean>;
  onMove: (record: RecordItem, destinationNotebookId: string | null) => Promise<boolean>;
  onPin: (record: RecordItem) => Promise<boolean>;
}

export function RecordList({
  title,
  records,
  notebooks,
  activeNotebookId,
  readOnly = false,
  busyRecordId,
  loading,
  mode,
  onUpdate,
  onTrash,
  onRestore,
  onMove,
  onPin,
}: RecordListProps) {
  const [editing, setEditing] = useState<RecordItem | null>(null);
  const [editBody, setEditBody] = useState("");
  const [editRetainedAttachmentIds, setEditRetainedAttachmentIds] = useState<string[]>([]);
  const [editPendingAttachments, setEditPendingAttachments] = useState<PendingAttachment[]>([]);
  const [editImageBusy, setEditImageBusy] = useState(false);
  const [editImageError, setEditImageError] = useState("");
  const [moving, setMoving] = useState<RecordItem | null>(null);
  const [lightbox, setLightbox] = useState<ImageLightboxState | null>(null);
  const lightboxTrigger = useRef<HTMLButtonElement | null>(null);
  const editTriggerRef = useRef<HTMLButtonElement | null>(null);
  const editingRef = useRef<RecordItem | null>(null);
  const editRetainedAttachmentIdsRef = useRef<string[]>([]);
  const editPendingAttachmentsRef = useRef<PendingAttachment[]>([]);
  editingRef.current = editing;
  editRetainedAttachmentIdsRef.current = editRetainedAttachmentIds;
  editPendingAttachmentsRef.current = editPendingAttachments;

  const restoreEditTriggerFocus = useCallback(() => {
    const trigger = editTriggerRef.current;
    editTriggerRef.current = null;
    window.requestAnimationFrame(() => {
      if (!trigger?.isConnected || trigger.disabled) return;
      if (trigger.closest("[hidden], [inert], [aria-hidden='true']")) return;
      const style = window.getComputedStyle(trigger);
      if (style.display === "none" || style.visibility === "hidden") return;
      trigger.focus({ preventScroll: true });
    });
  }, []);

  const resetEditState = useCallback((discardPending: boolean, restoreFocus: boolean) => {
    const pending = editPendingAttachmentsRef.current;
    if (discardPending && pending.length) {
      void Promise.allSettled(
        pending.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
      );
    }
    editPendingAttachmentsRef.current = [];
    editRetainedAttachmentIdsRef.current = [];
    setEditing(null);
    setEditBody("");
    setEditRetainedAttachmentIds([]);
    setEditPendingAttachments([]);
    setEditImageError("");
    if (restoreFocus) restoreEditTriggerFocus();
  }, [restoreEditTriggerFocus]);
  const blockedNotebookIds = useMemo(
    () => new Set(
      notebooks
        .filter((notebook) => (
          notebook.targetState !== "ready"
          || notebook.numberingSyncPending
          || notebook.attachmentDirectorySyncPending
        ))
        .map((notebook) => notebook.id),
    ),
    [notebooks],
  );
  const ordered = useMemo(() => {
    if (mode !== "active") return records;
    return [...records].sort((left, right) =>
      Number(right.isPinned) - Number(left.isPinned)
      || right.createdAtMs - left.createdAtMs
      || right.id.localeCompare(left.id),
    );
  }, [mode, records]);

  useEffect(() => {
    if (readOnly || (editing?.notebookId && blockedNotebookIds.has(editing.notebookId))) {
      resetEditState(true, true);
    }
    if (readOnly || (moving?.notebookId && blockedNotebookIds.has(moving.notebookId))) {
      setMoving(null);
    }
  }, [blockedNotebookIds, editing?.notebookId, moving?.notebookId, readOnly, resetEditState]);

  useEffect(() => () => {
    for (const attachment of editPendingAttachmentsRef.current) {
      void wakeBridge.discardPendingImage(attachment.token);
    }
  }, []);

  const beginEdit = (record: RecordItem, trigger: HTMLButtonElement) => {
    const pending = editPendingAttachmentsRef.current;
    if (pending.length) {
      void Promise.allSettled(
        pending.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
      );
    }
    editTriggerRef.current = trigger;
    setEditing(record);
    setEditBody(record.bodyMarkdown);
    const retained = record.attachments.map((attachment) => attachment.id);
    editRetainedAttachmentIdsRef.current = retained;
    editPendingAttachmentsRef.current = [];
    setEditRetainedAttachmentIds(retained);
    setEditPendingAttachments([]);
    setEditImageError("");
  };

  const closeEdit = () => resetEditState(true, true);

  const mergeEditPendingAttachments = async (selected: PendingAttachment[]) => {
    const activeEdit = editingRef.current;
    if (!activeEdit || !selected.length) {
      if (selected.length) {
        await Promise.allSettled(
          selected.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
        );
      }
      return;
    }
    const retainedIds = new Set(editRetainedAttachmentIdsRef.current);
    const retained = activeEdit.attachments.filter((attachment) => retainedIds.has(attachment.id));
    const current = editPendingAttachmentsRef.current;
    const knownDigests = new Set([
      ...retained.map((attachment) => attachment.contentSha256),
      ...current.map((attachment) => attachment.contentSha256),
    ]);
    const unique = selected.filter((attachment) => {
      if (knownDigests.has(attachment.contentSha256)) return false;
      knownDigests.add(attachment.contentSha256);
      return true;
    });
    const duplicates = selected.filter((attachment) => !unique.includes(attachment));
    if (duplicates.length) {
      await Promise.allSettled(
        duplicates.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
      );
    }
    if (!unique.length) return;
    const validation = validateImageFiles(
      unique.map((attachment) => ({
        name: attachment.displayName,
        size: attachment.byteSize,
        type: attachment.mediaType,
      })),
      [...retained, ...current],
    );
    if (validation) {
      await Promise.allSettled(
        unique.map((attachment) => wakeBridge.discardPendingImage(attachment.token)),
      );
      setEditImageError(validation);
      return;
    }
    const next = [...current, ...unique];
    editPendingAttachmentsRef.current = next;
    setEditPendingAttachments(next);
    setEditImageError("");
  };

  const pickEditImages = async () => {
    if (editImageBusy || !editingRef.current) return;
    setEditImageBusy(true);
    setEditImageError("");
    try {
      await mergeEditPendingAttachments(await wakeBridge.pickRecordImages());
    } catch (error) {
      setEditImageError(localCommandMessage(error));
    } finally {
      setEditImageBusy(false);
    }
  };

  const stageEditImageFiles = async (files: File[]) => {
    const activeEdit = editingRef.current;
    if (editImageBusy || !activeEdit || !files.length) return;
    const retainedIds = new Set(editRetainedAttachmentIdsRef.current);
    const retained = activeEdit.attachments.filter((attachment) => retainedIds.has(attachment.id));
    const current = editPendingAttachmentsRef.current;
    const validation = validateImageFiles(files, [...retained, ...current]);
    if (validation) {
      setEditImageError(validation);
      return;
    }
    setEditImageBusy(true);
    setEditImageError("");
    const staged: PendingAttachment[] = [];
    const knownDigests = new Set([
      ...retained.map((attachment) => attachment.contentSha256),
      ...current.map((attachment) => attachment.contentSha256),
    ]);
    try {
      for (const file of files) {
        const attachment = await wakeBridge.stageRecordImage(
          new Uint8Array(await file.arrayBuffer()),
          [...knownDigests],
        );
        if (!attachment) continue;
        knownDigests.add(attachment.contentSha256);
        staged.push({
          ...attachment,
          displayName: safeImageDisplayName(file.name, attachment.mediaType),
        });
      }
      await mergeEditPendingAttachments(staged);
    } catch (error) {
      await mergeEditPendingAttachments(staged);
      setEditImageError(localCommandMessage(error));
    } finally {
      setEditImageBusy(false);
    }
  };

  const removeEditPendingAttachment = async (token: string) => {
    if (editImageBusy) return;
    setEditImageBusy(true);
    try {
      await wakeBridge.discardPendingImage(token);
      const next = editPendingAttachmentsRef.current.filter(
        (attachment) => attachment.token !== token,
      );
      editPendingAttachmentsRef.current = next;
      setEditPendingAttachments(next);
      setEditImageError("");
    } catch (error) {
      setEditImageError(localCommandMessage(error));
    } finally {
      setEditImageBusy(false);
    }
  };

  const closeLightbox = () => {
    setLightbox(null);
    window.requestAnimationFrame(() => lightboxTrigger.current?.focus({ preventScroll: true }));
  };

  return (
    <section className="record-section" aria-busy={loading}>
      <header>
        <h2>{title}</h2>
        <span>{records.length} 条</span>
      </header>
      {loading ? <div className="record-state">正在读取记录…</div> : null}
      {!loading && !ordered.length ? (
        <div className="record-state record-state--empty">
          <div className="empty-icon"><Icon name={mode === "trash" ? "trash" : "edit"} /></div>
          <strong>{mode === "trash" ? "废纸篓是空的" : "从上方写下第一条速记"}</strong>
          <p>{mode === "trash" ? "删除的记录会出现在这里。" : "支持 Markdown、链接和多行文本。"}</p>
        </div>
      ) : null}
      <div className="record-list">
        {ordered.map((record) => {
          const recordReadOnly = readOnly
            || (record.notebookId !== null && blockedNotebookIds.has(record.notebookId))
            || record.attachments.some(
              (attachment) => attachment.relocationState !== "ready",
            );
          return (
          <article key={record.id} className="record-card" data-busy={busyRecordId === record.id}>
            <div className="record-meta">
              <time dateTime={new Date(record.createdAtMs).toISOString()}>{formatTime(record.createdAtMs)}</time>
              <span title={notebookName(notebooks, record.notebookId)}>
                {notebookName(notebooks, record.notebookId)}
              </span>
              <span className="record-sync" data-state={record.syncState}>{syncLabel(record.syncState)}</span>
              {record.isPinned ? <span className="record-pin-state">已置顶</span> : null}
            </div>
            {editing?.id === record.id ? (
              <div
                className="record-edit"
                role="group"
                aria-label="编辑记录正文和图片"
                onKeyDown={(event) => {
                  if (
                    event.defaultPrevented
                    || event.key !== "Escape"
                    || editImageBusy
                    || busyRecordId === record.id
                  ) return;
                  event.preventDefault();
                  event.stopPropagation();
                  closeEdit();
                }}
              >
                <textarea
                  aria-label="编辑记录正文"
                  autoFocus
                  value={editBody}
                  onChange={(event) => setEditBody(event.currentTarget.value)}
                  onPaste={(event) => {
                    const files = transferFiles(event.clipboardData);
                    if (!files.length) return;
                    event.preventDefault();
                    void stageEditImageFiles(files);
                  }}
                />
                <div className="record-edit__attachment-heading">
                  <strong>图片</strong>
                  <span>
                    {editRetainedAttachmentIds.length + editPendingAttachments.length}/10
                  </span>
                </div>
                {editRetainedAttachmentIds.length || editPendingAttachments.length ? (
                  <div className="record-edit__attachments">
                    {record.attachments
                      .filter((attachment) => editRetainedAttachmentIds.includes(attachment.id))
                      .map((attachment, index) => (
                        <div key={attachment.id} className="record-edit__attachment">
                          <ImagePreviewButton
                            source={{
                              kind: "record",
                              workspaceId: record.workspaceId,
                              recordId: record.id,
                              attachmentId: attachment.id,
                              mediaType: attachment.mediaType,
                              available: attachmentPreviewAvailable(attachment),
                            }}
                            label={`保留图片 ${index + 1}`}
                            onOpen={(preview, trigger) => {
                              lightboxTrigger.current = trigger;
                              setLightbox(preview);
                            }}
                          />
                          <span>图片 {index + 1}</span>
                          <button
                            type="button"
                            aria-label={`移除图片 ${index + 1}`}
                            disabled={editImageBusy}
                            onClick={() => {
                              const next = editRetainedAttachmentIdsRef.current.filter(
                                (id) => id !== attachment.id,
                              );
                              editRetainedAttachmentIdsRef.current = next;
                              setEditRetainedAttachmentIds(next);
                            }}
                          >
                            <Icon name="x" size={13} />
                          </button>
                        </div>
                      ))}
                    {editPendingAttachments.map((attachment, index) => (
                      <div key={attachment.token} className="record-edit__attachment" data-new="true">
                        <ImagePreviewButton
                          source={{
                            kind: "pending",
                            token: attachment.token,
                            mediaType: attachment.mediaType,
                            available: true,
                          }}
                          label={`新增图片 ${index + 1}`}
                          onOpen={(preview, trigger) => {
                            lightboxTrigger.current = trigger;
                            setLightbox(preview);
                          }}
                        />
                        <span title={attachment.displayName}>{attachment.displayName}</span>
                        <button
                          type="button"
                          aria-label={`移除 ${attachment.displayName}`}
                          disabled={editImageBusy}
                          onClick={() => void removeEditPendingAttachment(attachment.token)}
                        >
                          <Icon name="x" size={13} />
                        </button>
                      </div>
                    ))}
                  </div>
                ) : null}
                {record.attachments.length > editRetainedAttachmentIds.length ? (
                  <div className="record-edit__removed" role="status">
                    保存后移除 {record.attachments.length - editRetainedAttachmentIds.length} 张图片
                    <button
                      type="button"
                      disabled={editImageBusy}
                      onClick={() => {
                        const restored = record.attachments.map((attachment) => attachment.id);
                        editRetainedAttachmentIdsRef.current = restored;
                        setEditRetainedAttachmentIds(restored);
                      }}
                    >撤销移除</button>
                  </div>
                ) : null}
                <div className="record-edit__image-actions">
                  <button
                    className="secondary-button"
                    type="button"
                    disabled={editImageBusy || editRetainedAttachmentIds.length + editPendingAttachments.length >= 10}
                    onClick={() => void pickEditImages()}
                  >
                    <Icon name="image" size={15} />
                    {editImageBusy ? "处理中…" : "添加或替换图片"}
                  </button>
                  <span>也可以直接粘贴图片</span>
                </div>
                {editImageError ? <p className="record-edit__error" role="alert">{editImageError}</p> : null}
                <div className="record-edit__actions">
                  <button
                    className="secondary-button"
                    type="button"
                    disabled={editImageBusy || busyRecordId === record.id}
                    onClick={closeEdit}
                  >取消</button>
                  <button
                    className="primary-button"
                    type="button"
                    disabled={
                      recordReadOnly
                      || editImageBusy
                      || (!editBody.trim()
                        && editRetainedAttachmentIds.length + editPendingAttachments.length === 0)
                      || busyRecordId === record.id
                    }
                    onClick={() => {
                      void onUpdate(
                        record,
                        editBody,
                        editRetainedAttachmentIds,
                        editPendingAttachments,
                      ).then((success) => {
                        if (!success) return;
                        resetEditState(false, true);
                      });
                    }}
                  >保存</button>
                </div>
              </div>
            ) : (
              <>
                {record.bodyMarkdown ? <div className="record-body">{record.bodyMarkdown}</div> : null}
                {record.attachments.length ? (
                  <div className="record-attachments">
                    {record.attachments.map((attachment, index) => (
                      <div key={attachment.id} className="record-attachment">
                        <ImagePreviewButton
                          source={{
                            kind: "record",
                            workspaceId: record.workspaceId,
                            recordId: record.id,
                            attachmentId: attachment.id,
                            mediaType: attachment.mediaType,
                            available: attachmentPreviewAvailable(attachment),
                          }}
                          label={`图片 ${index + 1}`}
                          onOpen={(preview, trigger) => {
                            lightboxTrigger.current = trigger;
                            setLightbox(preview);
                          }}
                        />
                        <div className="record-attachment__meta">
                          <span>图片 {index + 1}</span>
                          <small>{formatBytes(attachment.byteSize)}</small>
                        </div>
                        {attachment.relocationState !== "ready" || attachment.fileState !== "ready" ? (
                          <em data-state={attachment.fileState}>
                            {attachment.relocationState !== "ready"
                              ? attachmentRelocationLabels[attachment.relocationState]
                              : attachmentFileLabels[attachment.fileState]}
                          </em>
                        ) : null}
                      </div>
                    ))}
                  </div>
                ) : null}
              </>
            )}
            <div className="record-actions">
              {mode === "active" ? (
                <>
                  <button type="button" aria-label="编辑记录" disabled={recordReadOnly} onClick={(event) => beginEdit(record, event.currentTarget)}><Icon name="edit" size={16} /></button>
                  <button type="button" aria-label={record.isPinned ? "取消置顶" : "置顶记录"} onClick={() => void onPin(record)}><Icon name="pin" size={16} /></button>
                  <button type="button" aria-label="移动记录" disabled={recordReadOnly} onClick={() => setMoving(record)}><Icon name="move" size={16} /></button>
                  <button className="danger-action" type="button" aria-label="删除记录" disabled={recordReadOnly} onClick={() => void onTrash(record)}><Icon name="trash" size={16} /></button>
                </>
              ) : (
                <button className="restore-button" type="button" disabled={recordReadOnly} onClick={() => void onRestore(record)}>
                  <Icon name="restore" size={16} />恢复
                </button>
              )}
            </div>
          </article>
          );
        })}
      </div>

      <ImageLightbox preview={lightbox} onClose={closeLightbox} />

      {moving ? (
        <MoveDialog
          record={moving}
          notebooks={notebooks}
          activeNotebookId={activeNotebookId}
          busy={busyRecordId === moving.id}
          onClose={() => setMoving(null)}
          onMove={(destination) =>
            onMove(moving, destination).then((success) => {
              if (success) setMoving(null);
              return success;
            })
          }
        />
      ) : null}
    </section>
  );
}

function formatTime(timestamp: number): string {
  const date = new Date(timestamp);
  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  if (sameDay) return `今天 ${new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit" }).format(date)}`;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function syncLabel(state: RecordItem["syncState"]): string {
  const labels: Record<RecordItem["syncState"], string> = {
    local: "本地",
    queued: "等待同步",
    synced: "已同步",
    conflict: "同步冲突",
    targetUnavailable: "目标不可用",
  };
  return labels[state];
}

interface MoveDialogProps {
  record: RecordItem;
  notebooks: Notebook[];
  activeNotebookId: string | null;
  busy: boolean;
  onClose: () => void;
  onMove: (destination: string | null) => Promise<boolean>;
}

function MoveDialog({ record, notebooks, activeNotebookId, busy, onClose, onMove }: MoveDialogProps) {
  const candidates = [
    { id: null, label: "收件箱" },
    ...notebooks
      .filter((notebook) => notebook.targetState === "ready")
      .map((notebook) => ({ id: notebook.id, label: notebook.displayName })),
  ].filter((candidate) => candidate.id !== record.notebookId);
  const [destination, setDestination] = useState<string>(candidates[0]?.id ?? "");

  return (
    <DialogFrame title="移动记录" onClose={onClose}>
      <p className="dialog-copy">记录会从当前笔记移出，并在目标笔记中自动重新编号。</p>
      <label className="form-field">
        <span>目标笔记</span>
        <SelectControl
          className="form-select"
          ariaLabel="目标笔记"
          value={destination}
          options={candidates.map((candidate) => ({
            value: candidate.id ?? "",
            label: candidate.label,
          }))}
          onChange={setDestination}
        />
      </label>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" onClick={onClose}>取消</button>
        <button
          className="primary-button"
          type="button"
          disabled={busy || (!destination && activeNotebookId === null)}
          onClick={() => void onMove(destination || null)}
        >{busy ? "移动中…" : "移动"}</button>
      </div>
    </DialogFrame>
  );
}

interface NewNotebookDialogProps {
  open: boolean;
  busy: boolean;
  onClose: () => void;
  onCreate: (draft: {
    displayName: string;
    relativePath: string;
    numberingStyle: NumberingStyle;
    numberingStart: number;
    attachmentDirectory?: string;
  }) => Promise<boolean>;
  onBind: (
    numberingStyle: NumberingStyle,
    numberingStart: number,
    attachmentDirectory?: string,
  ) => Promise<boolean>;
}

export function NewNotebookDialog({ open, busy, onClose, onCreate, onBind }: NewNotebookDialogProps) {
  const [displayName, setDisplayName] = useState("");
  const [relativePath, setRelativePath] = useState("");
  const [numberingStyle, setNumberingStyle] = useState<NumberingStyle>("numeric");
  const [numberingStart, setNumberingStart] = useState("1");
  const [attachmentDirectory, setAttachmentDirectory] = useState("");

  useEffect(() => {
    if (!open) return;
    setDisplayName("");
    setRelativePath("");
    setNumberingStyle("numeric");
    setNumberingStart("1");
    setAttachmentDirectory("");
  }, [open]);

  if (!open) return null;

  const handleSubmit = (event: FormEvent) => {
    event.preventDefault();
    const name = displayName.trim();
    const path = relativePath.trim() || `${name}.md`;
    const start = Number(numberingStart);
    if (!Number.isInteger(start) || start < 1 || start > 1_000_000_000) return;
    void onCreate({
      displayName: name,
      relativePath: path,
      numberingStyle,
      numberingStart: start,
      attachmentDirectory: attachmentDirectory.trim() || undefined,
    });
  };

  const parsedNumberingStart = Number(numberingStart);
  const numberingStartValid = Number.isInteger(parsedNumberingStart)
    && parsedNumberingStart >= 1
    && parsedNumberingStart <= 1_000_000_000;

  return (
    <DialogFrame title="新建 Markdown 笔记" onClose={onClose}>
      <form onSubmit={handleSubmit}>
        <label className="form-field">
          <span>笔记名称</span>
          <input autoFocus required maxLength={120} value={displayName} placeholder="例如：产品记录" onChange={(event) => setDisplayName(event.currentTarget.value)} />
        </label>
        <label className="form-field">
          <span>起始序号</span>
          <input
            type="number"
            min={1}
            max={1_000_000_000}
            step={1}
            required
            value={numberingStart}
            aria-invalid={!numberingStartValid}
            onChange={(event) => setNumberingStart(event.currentTarget.value)}
          />
          <small>连续数字从此值开始；日期分组会在每天从此值重新开始。</small>
        </label>
        <label className="form-field">
          <span>工作区内路径</span>
          <input value={relativePath} placeholder={displayName ? `${displayName}.md` : "notes/产品记录.md"} onChange={(event) => setRelativePath(event.currentTarget.value)} />
          <small>只创建新的 .md 文件，不覆盖已有文件。</small>
        </label>
        <label className="form-field">
          <span>自动序号</span>
          <SelectControl
            className="form-select"
            ariaLabel="自动序号"
            value={numberingStyle}
            options={numberingOptions}
            onChange={(nextValue) => setNumberingStyle(nextValue as NumberingStyle)}
          />
        </label>
        <label className="form-field">
          <span>附件目录</span>
          <input
            value={attachmentDirectory}
            placeholder="留空则使用 Markdown 旁的 attachments/笔记名"
            onChange={(event) => setAttachmentDirectory(event.currentTarget.value)}
          />
          <small>填写工作区相对目录；WakeGPT 不会移动你选择的原图。</small>
        </label>
        <div className="dialog-actions">
          <button className="secondary-button" type="button" onClick={onClose}>取消</button>
          <button className="primary-button" type="submit" disabled={busy || !displayName.trim() || !numberingStartValid}>{busy ? "创建中…" : "创建笔记"}</button>
        </div>
        <div className="dialog-alternative">
          <span>已有工作区内的 Markdown？</span>
          <button
            className="secondary-button"
            type="button"
            disabled={busy}
            onClick={() => {
              if (numberingStartValid) {
                void onBind(
                  numberingStyle,
                  parsedNumberingStart,
                  attachmentDirectory.trim() || undefined,
                );
              }
            }}
          >
            绑定已有 Markdown
          </button>
        </div>
      </form>
    </DialogFrame>
  );
}

interface MarkdownEditorDialogProps {
  open: boolean;
  notebook: Notebook | null;
  document: NotebookDocument | null;
  preferredLayout: MarkdownLayout;
  busy: boolean;
  readOnly: boolean;
  onDirtyChange: (dirty: boolean) => void;
  onClose: () => void;
  onSave: (markdown: string) => Promise<boolean>;
}

export function restoreMarkdownLineEndings(
  markdown: string,
  lineEnding: NotebookDocument["lineEnding"],
): string {
  const normalized = markdown.replace(/\r\n?|\n/gu, "\n");
  return lineEnding === "\n" ? normalized : normalized.replace(/\n/gu, lineEnding);
}

const LargeMarkdownTextarea = memo(function LargeMarkdownTextarea({
  initialValue,
  pageIdentity: _pageIdentity,
  pageLabel,
  describedBy,
  textareaRef,
  readOnly,
  onChange,
}: {
  initialValue: string;
  pageIdentity: string;
  pageLabel: string;
  describedBy: string;
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  readOnly: boolean;
  onChange: (event: ChangeEvent<HTMLTextAreaElement>) => void;
}) {
  return (
    <textarea
      ref={textareaRef}
      defaultValue={initialValue}
      spellCheck={false}
      aria-label={pageLabel}
      aria-describedby={describedBy}
      readOnly={readOnly}
      onChange={onChange}
    />
  );
// space-seed: The active large-document page owns its DOM value. Parent status updates must not
// copy hundreds of KiB back into the textarea. The document receipt and page index form the
// identity; a different identity remounts the uncontrolled textarea through its key.
}, (previous, next) => (
  previous.pageIdentity === next.pageIdentity
  && previous.readOnly === next.readOnly
));

export function MarkdownEditorDialog({
  open,
  notebook,
  document,
  preferredLayout,
  busy,
  readOnly,
  onDirtyChange,
  onClose,
  onSave,
}: MarkdownEditorDialogProps) {
  const [mode, setMode] = useState<MarkdownLayout>(preferredLayout);
  const [draft, setDraft] = useState("");
  const [previewMarkdown, setPreviewMarkdown] = useState("");
  const [largeDocument, setLargeDocument] = useState(false);
  const [largeDocumentCharacterCount, setLargeDocumentCharacterCount] = useState(0);
  const [draftDirty, setDraftDirty] = useState(false);
  const [previewOutdated, setPreviewOutdated] = useState(false);
  const [largePageIndex, setLargePageIndex] = useState(0);
  const [initializedDocumentIdentity, setInitializedDocumentIdentity] = useState("");
  const [largePreviewRevision, setLargePreviewRevision] = useState(0);
  const [discardConfirmationOpen, setDiscardConfirmationOpen] = useState(false);
  const draftRef = useRef("");
  const largePagesRef = useRef<ReturnType<typeof paginateMarkdownEditing>>([]);
  const largeDraftStatusTimerRef = useRef(0);
  const documentTextareaRef = useRef<HTMLTextAreaElement>(null);
  const markdownViewId = useId();
  const largeEditStatusId = `${markdownViewId}-large-edit-status`;
  const documentIdentity = document
    ? `${document.notebookId}:${document.fileSha256}:${document.receiptGeneration}`
    : "";
  const readyDocument = document && initializedDocumentIdentity === documentIdentity
    ? document
    : null;

  useEffect(() => {
    if (!open) return;
    setMode(preferredLayout);
    if (!document) {
      setInitializedDocumentIdentity("");
      return;
    }
    const markdown = document?.markdown ?? "";
    const isLargeDocument = markdownPreviewRequiresPaging(markdown);
    setDraft(markdown);
    setPreviewMarkdown(markdown);
    draftRef.current = markdown;
    setLargeDocument(isLargeDocument);
    setLargeDocumentCharacterCount(markdown.length);
    largePagesRef.current = isLargeDocument
      ? paginateMarkdownEditing(markdown)
      : [];
    setLargePageIndex(0);
    setLargePreviewRevision(0);
    setDraftDirty(false);
    onDirtyChange(false);
    setPreviewOutdated(false);
    setDiscardConfirmationOpen(false);
    setInitializedDocumentIdentity(documentIdentity);
    return () => {
      if (largeDraftStatusTimerRef.current) {
        window.clearTimeout(largeDraftStatusTimerRef.current);
        largeDraftStatusTimerRef.current = 0;
      }
    };
  }, [document, documentIdentity, onDirtyChange, open]);

  useEffect(() => {
    if (!open || draftDirty) return;
    setMode(preferredLayout);
  }, [draftDirty, open, preferredLayout]);

  useEffect(() => {
    if (!readyDocument) return;
    const focusFrame = window.requestAnimationFrame(() => {
      const active = window.document.activeElement;
      const dialog = topmostModalDialog();
      if (
        active instanceof HTMLButtonElement
        && active.getAttribute("aria-label") === "关闭"
        && active.closest('[role="dialog"]') === dialog
      ) {
        documentTextareaRef.current?.focus({ preventScroll: true });
      }
    });
    return () => window.cancelAnimationFrame(focusFrame);
  }, [documentIdentity, readyDocument]);

  const handleLargeDraftChange = useCallback((event: ChangeEvent<HTMLTextAreaElement>) => {
    const current = largePagesRef.current[largePageIndex];
    if (current) current.markdown = event.currentTarget.value;
    setDraftDirty(true);
    onDirtyChange(true);
    setPreviewOutdated(true);
    if (largeDraftStatusTimerRef.current) window.clearTimeout(largeDraftStatusTimerRef.current);
    largeDraftStatusTimerRef.current = window.setTimeout(() => {
      largeDraftStatusTimerRef.current = 0;
      setLargeDocumentCharacterCount(
        largePagesRef.current.reduce((total, page) => total + page.markdown.length, 0),
      );
    }, 220);
  }, [largePageIndex, onDirtyChange]);

  const selectLargeEditPage = (nextPageIndex: number) => {
    const boundedIndex = Math.max(
      0,
      Math.min(largePagesRef.current.length - 1, nextPageIndex),
    );
    if (boundedIndex === largePageIndex) return;
    setLargePageIndex(boundedIndex);
    window.requestAnimationFrame(() => {
      documentTextareaRef.current?.focus({ preventScroll: true });
    });
  };

  if (!open) return null;

  const fullLargeDraft = () => largePagesRef.current.map((page) => page.markdown).join("");
  const markdownForSave = () => restoreMarkdownLineEndings(
    largeDocument ? fullLargeDraft() : draftRef.current,
    readyDocument?.lineEnding ?? "\n",
  );
  const refreshLargePreview = () => {
    const markdown = fullLargeDraft();
    if (largeDraftStatusTimerRef.current) {
      window.clearTimeout(largeDraftStatusTimerRef.current);
      largeDraftStatusTimerRef.current = 0;
    }
    setLargeDocumentCharacterCount(markdown.length);
    draftRef.current = markdown;
    setPreviewMarkdown(markdown);
    setLargePreviewRevision((current) => current + 1);
    setPreviewOutdated(false);
  };

  const selectMarkdownView = (nextMode: MarkdownLayout, focus: boolean) => {
    if (nextMode !== "edit") {
      if (largeDocument) refreshLargePreview();
      else {
        setPreviewMarkdown(draftRef.current);
        setPreviewOutdated(false);
      }
    }
    setMode(nextMode);
    if (focus) {
      window.requestAnimationFrame(() => {
        window.document
          .getElementById(`${markdownViewId}-${nextMode}-tab`)
          ?.focus({ preventScroll: true });
      });
    }
  };
  const handleMarkdownViewKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    currentMode: MarkdownLayout,
  ) => {
    if (busy) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const currentIndex = markdownViews.findIndex((view) => view.value === currentMode);
    const nextIndex = tabIndexAfterKey(currentIndex, markdownViews.length, event.key);
    if (nextIndex === currentIndex) return;
    event.preventDefault();
    selectMarkdownView(markdownViews[nextIndex].value, true);
  };
  const requestClose = () => {
    if (busy) return;
    if (draftDirty) setDiscardConfirmationOpen(true);
    else onClose();
  };

  return (
    <>
      <DialogFrame
        title={notebook ? `编辑 ${notebook.displayName}` : "编辑 Markdown"}
        onClose={requestClose}
        closeDisabled={busy}
        wide
      >
      <div className="document-toolbar">
        <div className="segmented-control" role="tablist" aria-label="Markdown 视图">
          {markdownViews.map((view) => (
            <button
              id={`${markdownViewId}-${view.value}-tab`}
              key={view.value}
              type="button"
              role="tab"
              aria-selected={mode === view.value}
              aria-controls={`${markdownViewId}-panel`}
              tabIndex={mode === view.value ? 0 : -1}
              disabled={busy}
              onKeyDown={(event) => handleMarkdownViewKeyDown(event, view.value)}
              onClick={() => selectMarkdownView(view.value, true)}
            >
              {view.label}
            </button>
          ))}
        </div>
        <span>{readyDocument ? `${(largeDocument ? largeDocumentCharacterCount : draft.length).toLocaleString()} 字符 · 回执 ${readyDocument.receiptGeneration}` : "正在读取…"}</span>
      </div>
      <div className="managed-region-notice">
        {readOnly
          ? "当前速记本已变为只读。现有内容仍可选择和复制；处理同步状态后再继续编辑。"
          : "WakeGPT 受管速记区会显示在全文中，但需通过记录卡编辑；其他 Markdown 内容可直接修改。"}
      </div>
      {largeDocument && mode === "split" ? (
        <div className="large-markdown-preview-notice">
          <span id={`${markdownViewId}-preview-state`} role="status" aria-live="polite" aria-atomic="true">
            大文档分栏预览不会在每次按键后重新解析；当前预览
            {previewOutdated ? "有未刷新修改" : "已是最新"}。
          </span>
          <button
            className="secondary-button"
            type="button"
            aria-describedby={`${markdownViewId}-preview-state`}
            disabled={busy || !previewOutdated}
            onClick={() => {
              refreshLargePreview();
            }}
          >刷新预览</button>
        </div>
      ) : null}
      {largeDocument && mode !== "preview" ? (
        <div className="large-markdown-edit-pagination" role="group" aria-label="大文档编辑分页">
          <button
            className="secondary-button"
            type="button"
            disabled={busy || largePageIndex === 0}
            aria-disabled={busy || largePageIndex === 0}
            onClick={() => { if (!busy) selectLargeEditPage(largePageIndex - 1); }}
          >上一编辑页</button>
          <span id={largeEditStatusId} role="status">
            编辑第 {largePageIndex + 1} / {largePagesRef.current.length} 页
          </span>
          <button
            className="secondary-button"
            type="button"
            disabled={busy || largePageIndex >= largePagesRef.current.length - 1}
            aria-disabled={busy || largePageIndex >= largePagesRef.current.length - 1}
            onClick={() => { if (!busy) selectLargeEditPage(largePageIndex + 1); }}
          >下一编辑页</button>
        </div>
      ) : null}
      <div
        id={`${markdownViewId}-panel`}
        className="document-editor"
        data-layout={mode}
        role="tabpanel"
        aria-labelledby={`${markdownViewId}-${mode}-tab`}
        aria-busy={!readyDocument}
        tabIndex={!readyDocument || mode === "preview" ? 0 : undefined}
      >
        {!readyDocument ? <div className="document-loading">正在安全读取 Markdown…</div> : null}
        {readyDocument && mode !== "preview" ? (
          largeDocument ? (
            <LargeMarkdownTextarea
              key={`${readyDocument.fileSha256}:${readyDocument.receiptGeneration}:${largePageIndex}`}
              initialValue={largePagesRef.current[largePageIndex]?.markdown ?? ""}
              pageIdentity={`${readyDocument.fileSha256}:${readyDocument.receiptGeneration}:${largePageIndex}`}
              pageLabel={`Markdown 编辑页 ${largePageIndex + 1}，共 ${largePagesRef.current.length} 页`}
              describedBy={largeEditStatusId}
              textareaRef={documentTextareaRef}
              readOnly={busy || readOnly}
              onChange={handleLargeDraftChange}
            />
          ) : (
            <textarea
              ref={documentTextareaRef}
              value={draft}
              spellCheck={false}
              aria-label="Markdown 全文"
              readOnly={busy || readOnly}
              onChange={(event) => {
                const next = event.currentTarget.value;
                draftRef.current = next;
                setDraft(next);
                setDraftDirty(next !== readyDocument.markdown);
                onDirtyChange(next !== readyDocument.markdown);
              }}
            />
          )
        ) : null}
        {readyDocument && mode !== "edit" && notebook ? (
          <MarkdownPreview
            key={largeDocument
              ? `${readyDocument.fileSha256}:${largePreviewRevision}`
              : readyDocument.fileSha256}
            markdown={largeDocument ? previewMarkdown : draft}
            workspaceId={notebook.workspaceId}
            notebookId={notebook.id}
            notebookTargetId={notebook.targetId}
            deferRendering={!largeDocument}
          />
        ) : null}
      </div>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" disabled={busy} onClick={requestClose}>取消</button>
        <button
          className="primary-button"
          type="button"
          disabled={!readyDocument || busy || readOnly || !draftDirty}
          onClick={() => void onSave(markdownForSave())}
        >
          {busy ? "保存中…" : "保存全文"}
        </button>
      </div>
      </DialogFrame>
      <ConfirmDialog
        open={discardConfirmationOpen}
        title="放弃尚未保存的 Markdown 修改？"
        confirmLabel="放弃修改"
        busy={false}
        destructive
        onClose={() => setDiscardConfirmationOpen(false)}
        onConfirm={() => {
          setDiscardConfirmationOpen(false);
          onClose();
        }}
      >
        关闭后，这次对 Markdown 全文的修改将无法从 WakeGPT 恢复。
      </ConfirmDialog>
    </>
  );
}

export function ConflictInspectorDialog({
  inspection,
  notebook,
  busy,
  onClose,
  onRefresh,
  onResolve,
}: {
  inspection: NotebookConflictInspection | null;
  notebook: Notebook | null;
  busy: boolean;
  onClose: () => void;
  onRefresh: () => void;
  onResolve: (action: NotebookConflictResolutionAction) => Promise<void>;
}) {
  const [confirmation, setConfirmation] =
    useState<NotebookConflictResolutionAction | null>(null);
  useEffect(() => setConfirmation(null), [inspection?.conflictToken]);
  if (!inspection || !notebook) return null;
  const labels: Record<NotebookConflictResolutionAction, string> = {
    adoptWakegpt: "采用 WakeGPT 版本",
    adoptFile: "采用文件版本",
    unbind: "解除管理",
  };
  const descriptions: Record<NotebookConflictResolutionAction, string> = {
    adoptWakegpt:
      "只替换 WakeGPT 受管区，保留文件中受管区之外的全部内容；本地记录继续作为准本。",
    adoptFile:
      "只导入文件中可验证的正文改动；记录 ID、顺序、附件和控制结构必须保持不变。",
    unbind:
      "不修改 Markdown 文件；停止持续同步，并保留文件、WakeGPT 标记和本地记录。",
  };
  return (
    <>
      <DialogFrame
        title={`处理 ${notebook.displayName} 的同步冲突`}
        onClose={() => { if (!busy) onClose(); }}
        wide
      >
        <div className="conflict-inspector__notice" role="status">
          <strong>WakeGPT 已暂停这个速记本的写入</strong>
          <span>请比较两侧内容并选择一个明确结果；ChatGPT 速记卡在解决前保持只读。</span>
        </div>
        <div className="conflict-inspector__meta">
          <span className="conflict-inspector__path" title={notebook.relativePath}>
            文件：{notebook.relativePath}
          </span>
          <span>检测：{new Date(inspection.detectedAtMs).toLocaleString()}</span>
          <button className="secondary-button" type="button" disabled={busy} onClick={onRefresh}>
            <Icon name="refresh" size={15} />重新读取
          </button>
        </div>
        <div className="conflict-comparison" aria-label="冲突 Markdown 对比">
          <section>
            <header><strong>当前文件版本</strong><span>磁盘中的实时内容</span></header>
            <pre>{inspection.fileMarkdown}</pre>
          </section>
          <section>
            <header><strong>WakeGPT 版本</strong><span>按本地记录重新生成</span></header>
            <pre>{inspection.wakegptMarkdown}</pre>
          </section>
        </div>
        <details className="conflict-record-diffs">
          <summary>逐条正文差异（{inspection.recordDiffs.filter((diff) => diff.state !== "unchanged").length}）</summary>
          <div>
            {inspection.recordDiffs.map((diff, index) => (
              <article key={diff.recordId} data-state={diff.state}>
                <header><strong>记录 {index + 1}</strong><span>{diff.state === "modified" ? "正文不同" : diff.state === "invalid" ? "文件结构不可采用" : "一致"}</span></header>
                <div>
                  <pre>{diff.fileMarkdown ?? "无法在不改变结构或附件的前提下提取正文"}</pre>
                  <pre>{diff.wakegptMarkdown}</pre>
                </div>
              </article>
            ))}
          </div>
        </details>
        <div className="conflict-actions">
          <button
            className="secondary-button"
            type="button"
            disabled={busy || !inspection.adoptFile.available}
            title={inspection.adoptFile.blockerCode ?? undefined}
            onClick={() => setConfirmation("adoptFile")}
          >采用文件版本</button>
          <button
            className="secondary-button"
            type="button"
            disabled={busy || !inspection.unbind.available}
            title={inspection.unbind.blockerCode ?? undefined}
            onClick={() => setConfirmation("unbind")}
          >解除管理</button>
          <button
            className="primary-button"
            type="button"
            disabled={busy || !inspection.adoptWakegpt.available}
            title={inspection.adoptWakegpt.blockerCode ?? undefined}
            onClick={() => setConfirmation("adoptWakegpt")}
          >采用 WakeGPT 版本</button>
        </div>
      </DialogFrame>
      <ConfirmDialog
        open={confirmation !== null}
        title={confirmation ? `${labels[confirmation]}？` : "确认冲突处理"}
        confirmLabel={confirmation ? labels[confirmation] : "确认"}
        busy={busy}
        destructive={confirmation === "unbind"}
        onClose={() => { if (!busy) setConfirmation(null); }}
        onConfirm={() => {
          if (!confirmation) return;
          void onResolve(confirmation).then(() => setConfirmation(null));
        }}
      >
        {confirmation ? descriptions[confirmation] : ""}
      </ConfirmDialog>
    </>
  );
}

export function NumberingPreviewDialog({
  preview,
  notebook,
  busy,
  onClose,
  onConfirm,
}: {
  preview: NotebookNumberingPreview | null;
  notebook: Notebook | null;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  if (!preview || !notebook) return null;
  return (
    <DialogFrame title={`预览 ${notebook.displayName} 的编号重排`} onClose={onClose} wide>
      <div className="numbering-preview-summary">
        <div>
          <span>当前格式</span>
          <strong>{numberingSummary(preview.currentNumberingStyle, preview.currentNumberingStart)}</strong>
        </div>
        <Icon name="move" size={16} />
        <div>
          <span>确认后</span>
          <strong>{numberingSummary(preview.nextNumberingStyle, preview.nextNumberingStart)}</strong>
        </div>
      </div>
      <p className="managed-region-notice">
        下面是整篇 Markdown 的确认预览；普通正文会原样保留，只重写 WakeGPT 受管区的可见前缀与日期分组。记录身份和逻辑顺序不会改变。
      </p>
      <div className="numbering-preview-document">
        <MarkdownPreview
          markdown={preview.markdown}
          workspaceId={notebook.workspaceId}
          notebookId={notebook.id}
          notebookTargetId={notebook.targetId}
        />
      </div>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" disabled={busy} onClick={onClose}>
          取消
        </button>
        <button className="primary-button" type="button" disabled={busy} onClick={onConfirm}>
          {busy ? "正在重排…" : "确认并重排"}
        </button>
      </div>
    </DialogFrame>
  );
}

export function AttachmentDirectoryPreviewDialog({
  preview,
  notebook,
  busy,
  onClose,
  onConfirm,
}: {
  preview: NotebookAttachmentDirectoryPreview | null;
  notebook: Notebook | null;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  if (!preview || !notebook) return null;
  return (
    <DialogFrame title={`预览 ${notebook.displayName} 的附件迁移`} onClose={onClose} wide>
      <div className="numbering-preview-summary">
        <div>
          <span>当前目录</span>
          <strong className="path-value" title={preview.currentAttachmentDirectory}>{preview.currentAttachmentDirectory}</strong>
        </div>
        <Icon name="move" size={16} />
        <div>
          <span>确认后</span>
          <strong className="path-value" title={preview.nextAttachmentDirectory}>{preview.nextAttachmentDirectory}</strong>
        </div>
      </div>
      <p className="managed-region-notice">
        将准备 {preview.attachmentCount.toLocaleString()} 个图片引用。WakeGPT 会先复制并校验新副本，再写入下面的 Markdown；确认文件回执后，无人引用的旧副本才会移入系统废纸篓。
      </p>
      <div className="numbering-preview-document">
        <MarkdownPreview
          markdown={preview.markdown}
          workspaceId={notebook.workspaceId}
          notebookId={notebook.id}
          notebookTargetId={notebook.targetId}
        />
      </div>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" disabled={busy} onClick={onClose}>
          取消
        </button>
        <button className="primary-button" type="button" disabled={busy} onClick={onConfirm}>
          {busy ? "正在迁移…" : "确认并迁移"}
        </button>
      </div>
    </DialogFrame>
  );
}

type MarkdownAstNode = {
  type: string;
  ordered?: boolean;
  children?: MarkdownAstNode[];
  position?: { start?: { offset?: number } };
  data?: { hProperties?: Record<string, unknown> };
};

function preserveExplicitOrderedListValues() {
  return (tree: MarkdownAstNode, file: { value?: unknown }) => {
    const source = typeof file.value === "string" ? file.value : "";
    const visit = (node: MarkdownAstNode) => {
      if (node.type === "list" && node.ordered) {
        for (const item of node.children ?? []) {
          const offset = item.position?.start?.offset;
          if (!Number.isInteger(offset) || offset === undefined) continue;
          const lineEnd = source.indexOf("\n", offset);
          const firstLine = source.slice(offset, lineEnd < 0 ? source.length : lineEnd);
          const marker = /^ {0,3}(\d{1,10})[.)][\t ]+/u.exec(firstLine);
          if (!marker) continue;
          const value = Number(marker[1]);
          if (!Number.isSafeInteger(value) || value < 1 || value > 1_000_000_000) continue;
          item.data = {
            ...item.data,
            hProperties: { ...item.data?.hProperties, value },
          };
        }
      }
      for (const child of node.children ?? []) visit(child);
    };
    visit(tree);
  };
}

const markdownRemarkPlugins = [remarkGfm, preserveExplicitOrderedListValues];
const safeMarkdownProtocols = new Set(["http:", "https:", "mailto:"]);
const markdownImageTokenPrefix = "wakegpt-markdown-image:";
const markdownRehypeOptions = {
  footnoteLabel: "脚注",
  footnoteBackLabel: "返回正文引用",
};

type MarkdownImageDescriptor =
  | { kind: "relative"; relativePath: string }
  | { kind: "remote" | "unsafe" | "invalid" };

type MarkdownImageRenderContextValue = {
  workspaceId: string;
  notebookId: string;
  notebookTargetId: string;
  onOpen: (preview: ImageLightboxState, trigger: HTMLButtonElement) => void;
  onRelease: (url: string) => void;
};

const MarkdownImageRenderContext = createContext<MarkdownImageRenderContextValue | null>(null);

export function classifyMarkdownImageSource(source: string): MarkdownImageDescriptor {
  if (!source || source.length > 4096 || source !== source.trim()) return { kind: "invalid" };
  let decoded = "";
  try {
    decoded = decodeURIComponent(source);
  } catch {
    return { kind: "invalid" };
  }
  if (!decoded || decoded.length > 4096 || new TextEncoder().encode(decoded).byteLength > 4096) {
    return { kind: "invalid" };
  }
  if (/^https?:/iu.test(decoded) || decoded.startsWith("//")) return { kind: "remote" };
  if (/^[a-z][a-z0-9+.-]*:/iu.test(decoded)) return { kind: "unsafe" };
  if (
    decoded !== decoded.trim()
    || decoded.startsWith("/")
    || decoded.includes("\\")
    || decoded.includes("?")
    || decoded.includes("#")
    || decoded.includes("%")
    || /[\u0000-\u001f\u007f]/u.test(decoded)
    || decoded.split("/").some((component) => (
      !component || component === "." || component === ".."
    ))
  ) {
    return { kind: "unsafe" };
  }
  return { kind: "relative", relativePath: decoded };
}

function markdownImageToken(source: string): string {
  const descriptor = classifyMarkdownImageSource(source);
  return descriptor.kind === "relative"
    ? `${markdownImageTokenPrefix}relative:${encodeURIComponent(descriptor.relativePath)}`
    : `${markdownImageTokenPrefix}${descriptor.kind}`;
}

function markdownImageDescriptorFromToken(source: string | undefined): MarkdownImageDescriptor {
  if (!source?.startsWith(markdownImageTokenPrefix)) return { kind: "invalid" };
  const token = source.slice(markdownImageTokenPrefix.length);
  if (token === "remote" || token === "unsafe" || token === "invalid") return { kind: token };
  if (!token.startsWith("relative:")) return { kind: "invalid" };
  try {
    return classifyMarkdownImageSource(decodeURIComponent(token.slice("relative:".length)));
  } catch {
    return { kind: "invalid" };
  }
}

function markdownImageBlob(response: unknown): Blob {
  const bytes = response instanceof ArrayBuffer
    ? new Uint8Array(response)
    : ArrayBuffer.isView(response)
      ? new Uint8Array(response.buffer, response.byteOffset, response.byteLength)
      : null;
  if (
    !bytes
    || bytes.length <= 6
    || bytes[0] !== 0x57
    || bytes[1] !== 0x47
    || bytes[2] !== 0x4d
    || bytes[3] !== 0x49
    || bytes[4] !== 1
  ) {
    throw new Error("Markdown 图片预览响应格式无效");
  }
  const mediaType = bytes[5] === 1
    ? "image/png"
    : bytes[5] === 2
      ? "image/jpeg"
      : bytes[5] === 3
        ? "image/gif"
        : bytes[5] === 4
          ? "image/webp"
          : "";
  if (!mediaType) throw new Error("Markdown 图片预览媒体类型无效");
  return new Blob([Uint8Array.from(bytes.subarray(6))], { type: mediaType });
}

function markdownImageErrorCode(error: unknown): string {
  if (typeof error === "object" && error !== null && "code" in error) {
    const code = (error as { code?: unknown }).code;
    return typeof code === "string" ? code : "";
  }
  if (typeof error !== "string") return "";
  try {
    const parsed = JSON.parse(error) as { code?: unknown };
    return typeof parsed.code === "string" ? parsed.code : "";
  } catch {
    return "";
  }
}

function markdownImageErrorLabel(error: unknown): string {
  switch (markdownImageErrorCode(error)) {
    case "markdown_image_missing":
      return "图片文件不存在";
    case "markdown_image_path_invalid":
    case "markdown_image_symlink_rejected":
    case "markdown_image_not_regular":
      return "不安全图片路径已阻止";
    case "attachment_type_unsupported":
      return "图片格式不支持";
    case "attachment_too_large":
      return "图片超过 20 MiB";
    case "attachment_file_changed_during_read":
      return "图片读取时已变化";
    case "markdown_image_context_changed":
      return "预览上下文已切换";
    case "markdown_image_notebook_unavailable":
      return "当前笔记不可用";
    default:
      return "图片暂时无法预览";
  }
}

function MarkdownImagePlaceholder({
  label,
  reason,
}: {
  label: string;
  reason: string;
}) {
  return (
    <span className="markdown-image-placeholder" role="img" aria-label={`${reason}：${label}`}>
      <Icon name="image" size={15} />
      <span>{reason} · {label}</span>
    </span>
  );
}

function MarkdownRelativeImage({
  workspaceId,
  notebookId,
  notebookTargetId,
  relativePath,
  label,
  onOpen,
  onRelease,
}: {
  workspaceId: string;
  notebookId: string;
  notebookTargetId: string;
  relativePath: string;
  label: string;
  onOpen: MarkdownImageRenderContextValue["onOpen"];
  onRelease: MarkdownImageRenderContextValue["onRelease"];
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const objectUrlRef = useRef("");
  const failedRef = useRef(false);
  const [url, setUrl] = useState("");
  const [loadState, setLoadState] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [errorLabel, setErrorLabel] = useState("");

  useEffect(() => {
    let active = true;
    let inRange = false;
    let loading = false;
    let observer: IntersectionObserver | null = null;
    failedRef.current = false;
    setUrl("");
    setLoadState("idle");
    setErrorLabel("");

    const releaseObjectUrl = () => {
      const currentUrl = objectUrlRef.current;
      if (!currentUrl) return;
      objectUrlRef.current = "";
      onRelease(currentUrl);
      URL.revokeObjectURL(currentUrl);
    };

    const load = async () => {
      if (!active || !inRange || loading || objectUrlRef.current || failedRef.current) return;
      loading = true;
      setLoadState("loading");
      try {
        const response = await wakeBridge.readMarkdownImagePreview({
          workspaceId,
          notebookId,
          notebookTargetId,
          relativePath,
        });
        const nextUrl = URL.createObjectURL(markdownImageBlob(response));
        if (!active || !inRange) {
          URL.revokeObjectURL(nextUrl);
          return;
        }
        objectUrlRef.current = nextUrl;
        setUrl(nextUrl);
        setLoadState("ready");
      } catch (error) {
        failedRef.current = true;
        if (active) {
          setErrorLabel(markdownImageErrorLabel(error));
          setLoadState("error");
        }
      } finally {
        loading = false;
      }
    };

    const unload = () => {
      releaseObjectUrl();
      setUrl("");
      if (!failedRef.current) setLoadState("idle");
    };

    const trigger = triggerRef.current;
    if (trigger && "IntersectionObserver" in window) {
      observer = new IntersectionObserver((entries) => {
        const entry = entries.find((candidate) => candidate.target === trigger);
        if (!entry) return;
        inRange = entry.isIntersecting;
        if (inRange) void load();
        else unload();
      }, { rootMargin: "160px" });
      observer.observe(trigger);
    } else {
      inRange = true;
      void load();
    }

    return () => {
      active = false;
      observer?.disconnect();
      releaseObjectUrl();
    };
  }, [notebookId, notebookTargetId, onRelease, relativePath, workspaceId]);

  return (
    <button
      ref={triggerRef}
      className="markdown-image-preview"
      type="button"
      disabled={!url}
      data-state={loadState}
      aria-busy={loadState === "loading"}
      aria-label={url ? `放大查看 ${label}` : `${label}：${errorLabel || "正在加载"}`}
      onClick={(event) => {
        if (url) onOpen({ url, label }, event.currentTarget);
      }}
    >
      {url ? (
        <img
          src={url}
          alt=""
          draggable="false"
          onError={() => {
            const currentUrl = objectUrlRef.current;
            if (currentUrl) {
              objectUrlRef.current = "";
              onRelease(currentUrl);
              URL.revokeObjectURL(currentUrl);
            }
            failedRef.current = true;
            setUrl("");
            setErrorLabel("图片内容无法显示");
            setLoadState("error");
          }}
        />
      ) : (
        <MarkdownImagePlaceholder
          label={label}
          reason={loadState === "error" ? errorLabel : "正在读取本地图片"}
        />
      )}
    </button>
  );
}

function MarkdownPreviewImage({
  sourceToken,
  label,
}: {
  sourceToken: string | undefined;
  label: string;
}) {
  const context = useContext(MarkdownImageRenderContext);
  const descriptor = markdownImageDescriptorFromToken(sourceToken);
  if (descriptor.kind === "remote") {
    return <MarkdownImagePlaceholder label={label} reason="远程图片已阻止" />;
  }
  if (descriptor.kind === "unsafe") {
    return <MarkdownImagePlaceholder label={label} reason="不安全图片路径已阻止" />;
  }
  if (descriptor.kind !== "relative") {
    return <MarkdownImagePlaceholder label={label} reason="图片路径无效" />;
  }
  if (!context) {
    return <MarkdownImagePlaceholder label={label} reason="本地图片仅在当前笔记中加载" />;
  }
  return (
    <MarkdownRelativeImage
      workspaceId={context.workspaceId}
      notebookId={context.notebookId}
      notebookTargetId={context.notebookTargetId}
      relativePath={descriptor.relativePath}
      label={label}
      onOpen={context.onOpen}
      onRelease={context.onRelease}
    />
  );
}

const safeMarkdownUrl: UrlTransform = (url, key) => {
  const candidate = url.trim();
  if (key === "src") return markdownImageToken(url);
  if (!candidate || candidate.length > 4096) return undefined;
  if (key === "href" && candidate.startsWith("#")) return candidate;
  if (key !== "href") return undefined;
  try {
    const parsed = new URL(candidate);
    if (parsed.username || parsed.password || !safeMarkdownProtocols.has(parsed.protocol)) {
      return undefined;
    }
    return candidate;
  } catch {
    return undefined;
  }
};

const markdownPreviewComponents: Components = {
  a({ node: _node, href, children, ...props }) {
    if (!href) return <span className="markdown-link-disabled">{children}</span>;
    if (href.startsWith("#")) return <a {...props} href={href}>{children}</a>;
    return (
      <a {...props} href={href} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    );
  },
  img({ node: _node, alt, src }) {
    const label = alt?.trim() || "Markdown 图片";
    return <MarkdownPreviewImage sourceToken={src} label={label} />;
  },
  input({ node: _node, ...props }) {
    return <input {...props} disabled readOnly tabIndex={-1} aria-hidden="true" />;
  },
  table({ node: _node, ...props }) {
    return (
      <div className="markdown-table-scroll" role="region" aria-label="Markdown 表格" tabIndex={0}>
        <table {...props} />
      </div>
    );
  },
};

export const MarkdownPreview = memo(function MarkdownPreview({
  markdown,
  workspaceId = "",
  notebookId = "",
  notebookTargetId = "",
  deferRendering = true,
}: {
  markdown: string;
  workspaceId?: string;
  notebookId?: string;
  notebookTargetId?: string;
  deferRendering?: boolean;
}) {
  const deferredMarkdown = useDeferredValue(markdown);
  const previewMarkdown = deferRendering ? deferredMarkdown : markdown;
  const previewPaged = useMemo(
    () => markdownPreviewRequiresPaging(previewMarkdown),
    [previewMarkdown],
  );
  const previewPages = useMemo(
    () => previewPaged ? paginateMarkdownPreview(previewMarkdown) : null,
    [previewMarkdown, previewPaged],
  );
  const previewVersion = useMemo(() => ({}), [previewMarkdown]);
  const [previewSelection, setPreviewSelection] = useState({
    version: previewVersion,
    index: 0,
  });
  const [lightbox, setLightbox] = useState<ImageLightboxState | null>(null);
  const lightboxTrigger = useRef<HTMLButtonElement | null>(null);
  const previewArticleRef = useRef<HTMLElement>(null);
  const openImage = useCallback<MarkdownImageRenderContextValue["onOpen"]>((preview, trigger) => {
    lightboxTrigger.current = trigger;
    setLightbox(preview);
  }, []);
  const releaseImage = useCallback((url: string) => {
    setLightbox((current) => (current?.url === url ? null : current));
  }, []);
  const closeLightbox = useCallback(() => {
    setLightbox(null);
    window.requestAnimationFrame(() => {
      if (lightboxTrigger.current?.isConnected) {
        lightboxTrigger.current.focus({ preventScroll: true });
      }
    });
  }, []);
  const imageContext = useMemo<MarkdownImageRenderContextValue | null>(() => (
    workspaceId && notebookId && notebookTargetId
      ? {
          workspaceId,
          notebookId,
          notebookTargetId,
          onOpen: openImage,
          onRelease: releaseImage,
        }
      : null
  ), [notebookId, notebookTargetId, openImage, releaseImage, workspaceId]);
  const previewPageIndex = previewSelection.version === previewVersion
    ? previewSelection.index
    : 0;
  const safePreviewPageIndex = previewPages?.length
    ? Math.min(previewPageIndex, previewPages.length - 1)
    : 0;
  const previewPage = previewPages?.[safePreviewPageIndex] ?? null;
  const renderedMarkdown = previewPage?.markdown ?? previewMarkdown;
  const selectPreviewPage = (nextPageIndex: number) => {
    const pageCount = previewPages?.length ?? 1;
    const boundedIndex = Math.max(0, Math.min(pageCount - 1, nextPageIndex));
    if (boundedIndex === safePreviewPageIndex) return;
    setPreviewSelection({ version: previewVersion, index: boundedIndex });
    window.requestAnimationFrame(() => {
      previewArticleRef.current?.focus({ preventScroll: true });
    });
  };

  return (
    <>
      <MarkdownImageRenderContext.Provider value={imageContext}>
        <section className="markdown-preview-surface" aria-label="Markdown 预览">
          {previewPage ? (
            <div className="markdown-preview-pagination" role="group" aria-label="大文档预览分页">
              <p>
                文档超过 {(MARKDOWN_LIVE_PREVIEW_MAX_BYTES / 1024).toLocaleString()} KiB，
                已分页预览。每页独立解析，跨页列表、脚注或引用定义可能无法完整呈现；
                全文编辑和保存不受影响。
              </p>
              <div>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={safePreviewPageIndex === 0}
                  onClick={() => selectPreviewPage(safePreviewPageIndex - 1)}
                >上一页</button>
                <span role="status">
                  第 {safePreviewPageIndex + 1} / {previewPages?.length ?? 1} 页
                </span>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={safePreviewPageIndex >= (previewPages?.length ?? 1) - 1}
                  onClick={() => selectPreviewPage(safePreviewPageIndex + 1)}
                >下一页</button>
              </div>
              {previewPage.oversizedBlock ? (
                <p role="status">当前单个 Markdown 块过大，为避免界面失去响应，本页以纯文本显示。</p>
              ) : null}
            </div>
          ) : null}
          <article
            ref={previewArticleRef}
            className="markdown-preview"
            aria-label={previewPage
              ? `Markdown 预览第 ${safePreviewPageIndex + 1} 页，共 ${previewPages?.length ?? 1} 页`
              : undefined}
            tabIndex={previewPage ? -1 : undefined}
          >
            {previewPage?.oversizedBlock ? (
              <pre className="markdown-preview-oversized-block">{renderedMarkdown}</pre>
            ) : (
              <ReactMarkdown
                skipHtml
                remarkPlugins={markdownRemarkPlugins}
                remarkRehypeOptions={markdownRehypeOptions}
                urlTransform={safeMarkdownUrl}
                components={markdownPreviewComponents}
              >
                {renderedMarkdown}
              </ReactMarkdown>
            )}
          </article>
        </section>
      </MarkdownImageRenderContext.Provider>
      <ImageLightbox preview={lightbox} onClose={closeLightbox} />
    </>
  );
});

function DialogFrame({
  title,
  children,
  onClose,
  wide = false,
  descriptionId,
  closeDisabled = false,
}: {
  title: string;
  children: React.ReactNode;
  onClose: () => void;
  wide?: boolean;
  descriptionId?: string;
  closeDisabled?: boolean;
}) {
  const titleId = useId();
  const closeRef = useRef(onClose);
  const closeDisabledRef = useRef(closeDisabled);
  const dialogRef = useRef<HTMLElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );
  closeRef.current = onClose;
  closeDisabledRef.current = closeDisabled;
  useEffect(() => {
    const previouslyFocused = returnFocusRef.current;
    const initialFocusFrame = window.requestAnimationFrame(() => {
      const dialog = dialogRef.current;
      if (!dialog || topmostModalDialog() !== dialog) return;
      if (document.activeElement instanceof HTMLElement && dialog.contains(document.activeElement)) return;
      (dialogFocusableElements(dialog)[0] ?? dialog).focus({ preventScroll: true });
    });
    const handleKeyDown = (event: KeyboardEvent) => {
      const dialog = dialogRef.current;
      if (event.defaultPrevented || !dialog || topmostModalDialog() !== dialog) return;
      if (event.key === "Escape") {
        if (expandedControlInsideDialog(dialog)) return;
        event.preventDefault();
        event.stopPropagation();
        if (closeDisabledRef.current) return;
        closeRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = dialogFocusableElements(dialog);
      if (!focusable.length) {
        event.preventDefault();
        event.stopPropagation();
        dialog.focus({ preventScroll: true });
        return;
      }
      const currentIndex = document.activeElement instanceof HTMLElement
        ? focusable.indexOf(document.activeElement)
        : -1;
      const targetIndex = dialogFocusTargetIndex(currentIndex, focusable.length, event.shiftKey);
      if (targetIndex === null) return;
      event.preventDefault();
      event.stopPropagation();
      focusable[targetIndex]?.focus({ preventScroll: true });
    };
    window.addEventListener("keydown", handleKeyDown, true);
    return () => {
      window.cancelAnimationFrame(initialFocusFrame);
      window.removeEventListener("keydown", handleKeyDown, true);
      window.requestAnimationFrame(() => {
        if (!previouslyFocused?.isConnected) return;
        if (previouslyFocused.closest("[hidden], [inert], [aria-hidden='true']")) return;
        const remainingDialog = topmostModalDialog();
        if (remainingDialog && !remainingDialog.contains(previouslyFocused)) return;
        previouslyFocused.focus({ preventScroll: true });
      });
    };
  }, []);
  const handleBackdrop = (event: MouseEvent<HTMLDivElement>) => {
    if (!closeDisabled && event.target === event.currentTarget) onClose();
  };
  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={handleBackdrop}>
      <section ref={dialogRef} className="dialog" data-wide={wide} role="dialog" aria-modal="true" aria-labelledby={titleId} aria-describedby={descriptionId} aria-busy={closeDisabled || undefined} tabIndex={-1}>
        <header>
          <h2 id={titleId} title={title}>{title}</h2>
          <button className="icon-button" type="button" aria-label="关闭" disabled={closeDisabled} onClick={onClose}><Icon name="x" size={17} /></button>
        </header>
        {children}
      </section>
    </div>
  );
}

export function ConfirmDialog({
  open,
  title,
  confirmLabel,
  busy,
  confirmDisabled = false,
  destructive = false,
  children,
  onClose,
  onConfirm,
}: {
  open: boolean;
  title: string;
  confirmLabel: string;
  busy: boolean;
  confirmDisabled?: boolean;
  destructive?: boolean;
  children: React.ReactNode;
  onClose: () => void;
  onConfirm: () => void;
}) {
  const descriptionId = useId();
  if (!open) return null;
  return (
    <DialogFrame title={title} descriptionId={descriptionId} closeDisabled={busy} onClose={onClose}>
      <p id={descriptionId} className="dialog-copy">{children}</p>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" disabled={busy} onClick={onClose}>取消</button>
        <button
          className={destructive ? "danger-button" : "primary-button"}
          type="button"
          disabled={busy || confirmDisabled}
          onClick={onConfirm}
        >
          {busy ? "处理中…" : confirmLabel}
        </button>
      </div>
    </DialogFrame>
  );
}

export function LocalDataResetDialog({
  open,
  preview,
  confirmation,
  busy,
  onConfirmationChange,
  onClose,
  onConfirm,
}: {
  open: boolean;
  preview: LocalDataResetPreview | null;
  confirmation: string;
  busy: boolean;
  onConfirmationChange: (value: string) => void;
  onClose: () => void;
  onConfirm: () => void;
}) {
  if (!open || !preview) return null;
  const matches = confirmation === preview.confirmationPhrase;
  return (
    <DialogFrame title="清除 WakeGPT 本机数据？" onClose={onClose} wide>
      <p className="dialog-copy">
        WakeGPT 将暂停速记卡、关闭登录时启动，并在重启前把自己的数据库、草稿、诊断、默认身份 profile、缓存和恢复状态整体移入 macOS 废纸篓。
      </p>
      <dl className="settings-list settings-list--compact local-data-reset-summary">
        <div><dt>本地内容</dt><dd>{preview.workspaceCount} 个工作区映射 · {preview.notebookCount} 个速记本 · {preview.recordCount} 条记录</dd></div>
        <div><dt>图片与草稿</dt><dd>{preview.attachmentCount} 个附件记录 · {preview.draftCount} 份草稿 · {preview.draftAttachmentCount} 张草稿图片</dd></div>
        <div><dt>其他本机状态</dt><dd>{preview.composerReceiptCount} 条 GPT 插入回执 · {preview.diagnosticEventCount} 条诊断事件{preview.defaultIdentityProfilePresent ? " · 默认身份 profile" : ""}</dd></div>
        <div><dt>不会改动</dt><dd>工作区 Markdown、工作区附件、外部导出包和系统备份</dd></div>
      </dl>
      <p className="dialog-copy local-data-reset-warning">
        重启后 WakeGPT 会像首次打开一样开始。废纸篓被清空前，旧的应用数据包仍可由系统恢复；WakeGPT 不会自动覆盖新数据来恢复旧包。
      </p>
      <label className="form-field">
        <span>输入“{preview.confirmationPhrase}”以确认</span>
        <input
          autoFocus
          value={confirmation}
          autoComplete="off"
          spellCheck={false}
          disabled={busy}
          aria-describedby="local-data-reset-confirmation-help"
          onChange={(event) => onConfirmationChange(event.target.value)}
        />
        <small id="local-data-reset-confirmation-help">确认后应用会自动重启；取消不会产生任何数据操作。</small>
      </label>
      <div className="dialog-actions">
        <button className="secondary-button" type="button" disabled={busy} onClick={onClose}>取消</button>
        <button className="danger-button" type="button" disabled={busy || !matches} onClick={onConfirm}>
          {busy ? "正在安排重启…" : "移到废纸篓并重新开始"}
        </button>
      </div>
    </DialogFrame>
  );
}

export function LocalDiagnosticsDialog({
  open,
  events,
  nextCursor,
  loading,
  error,
  onClose,
  onLoadMore,
  onRetry,
}: {
  open: boolean;
  events: LocalDiagnosticEvent[];
  nextCursor: number | null;
  loading: boolean;
  error: string;
  onClose: () => void;
  onLoadMore: () => void;
  onRetry: () => void;
}) {
  if (!open) return null;
  return (
    <DialogFrame title="本地诊断" onClose={onClose} wide>
      <div className="diagnostics-dialog__summary">
        <p>仅显示 WakeGPT 生成的受控诊断字段；不读取笔记正文、图片内容、账号或 Cookie。</p>
        <span>{events.length} 条已加载</span>
      </div>
      {error ? (
        <div className="diagnostics-dialog__error" role="alert">
          <span>{error}</span>
          <button className="secondary-button" type="button" disabled={loading} onClick={onRetry}>
            重试
          </button>
        </div>
      ) : null}
      {!events.length ? (
        <div className="diagnostics-dialog__empty" aria-live="polite">
          <Icon name="list" size={22} />
          <strong>
            {loading ? "正在读取诊断…" : error ? "无法读取诊断" : "暂无诊断事件"}
          </strong>
          {!loading && !error ? <span>新事件会按时间从新到旧显示。</span> : null}
        </div>
      ) : (
        <div className="diagnostics-list" role="list" aria-label="本地诊断事件">
          {events.map((event) => {
            const contextText = JSON.stringify(event.context, null, 2);
            return (
              <article
                className="diagnostics-event"
                data-severity={event.severity}
                role="listitem"
                key={event.id}
              >
                <header>
                  <div>
                    <span className="diagnostics-event__severity">
                      {localDiagnosticSeverityLabels[event.severity]}
                    </span>
                    <strong>{event.code}</strong>
                  </div>
                  <time dateTime={localDiagnosticDateTime(event.lastAtMs)}>
                    {formatLocalDiagnosticTime(event.lastAtMs)}
                  </time>
                </header>
                <div className="diagnostics-event__meta">
                  <code>{event.subsystem}</code>
                  <span>编号 {event.id}</span>
                  {event.occurrenceCount > 1 ? <span>连续发生 {event.occurrenceCount} 次</span> : null}
                  {event.firstAtMs !== event.lastAtMs ? (
                    <span>首次 {formatLocalDiagnosticTime(event.firstAtMs)}</span>
                  ) : null}
                </div>
                {contextText && contextText !== "{}" ? <pre>{contextText}</pre> : null}
              </article>
            );
          })}
        </div>
      )}
      <div className="dialog-actions diagnostics-dialog__actions">
        {loading && events.length ? <span role="status">正在加载更早的事件…</span> : null}
        {nextCursor !== null ? (
          <button className="secondary-button" type="button" disabled={loading} onClick={onLoadMore}>
            {loading ? "正在加载…" : "加载更多"}
          </button>
        ) : events.length && !loading ? <span>已显示全部事件</span> : null}
        <button className="secondary-button" type="button" onClick={onClose}>关闭</button>
      </div>
    </DialogFrame>
  );
}

export function EmptyWorkspace({ busy, onAddWorkspace }: { busy: boolean; onAddWorkspace: () => void }) {
  return (
    <section className="empty-workspace">
      <div className="empty-workspace__icon"><Icon name="folder" size={28} /></div>
      <h1>连接一个工作区</h1>
      <p>WakeGPT 会在你选择的工作区中创建并持续维护 Markdown 笔记。</p>
      <button className="primary-button" type="button" disabled={busy} onClick={onAddWorkspace}>
        <Icon name="plus" size={16} />{busy ? "正在选择…" : "选择工作区"}
      </button>
      <small>不会扫描或修改未绑定的文件。</small>
    </section>
  );
}

interface SettingsViewProps {
  workspace: Workspace | null;
  notebook: Notebook | null;
  notebooks: Notebook[];
  status: AppStatus | null;
  productSettings: ProductSettings | null;
  workspaceOpenPreference: WorkspaceOpenPreference | null;
  integration: CodexIntegrationStatus | null;
  defaultIdentity: DefaultIdentityStatus | null;
  loginItemStatus: LoginItemStatus | null;
  loginItemUnavailable: boolean;
  updateStatus: UpdateStatus | null;
  updateUnavailable: boolean;
  updateAction: string;
  updateProgress: UpdateDownloadProgress | null;
  localDiagnosticsStatus: LocalDiagnosticsStatus | null;
  localDiagnosticsUnavailable: boolean;
  localDataResetPreview: LocalDataResetPreview | null;
  localDataResetUnavailable: boolean;
  theme: ThemePreference;
  submitShortcut: SubmitShortcut;
  markdownLayout: MarkdownLayout;
  busyAction: string;
  integrationRestartNotice: {
    displayName: string;
    tone: "progress" | "error";
    detail: string;
  } | null;
  onToggleIntegration: () => void;
  onRestartCodex: () => void;
  onRestartCodexInstance: (instance: CodexIntegrationStatus["instances"][number]) => void;
  onConfigureDefaultIdentity: (alias: string, locked: boolean) => void;
  onUseDefaultIdentity: () => void;
  onUnbindDefaultIdentity: () => void;
  onRetryRecovery: () => void;
  onExportLocalData: () => void;
  onProductSettingsChange: (
    recordTrashRetentionDays: number | null,
    notebookScanIgnoreDirectories: string[],
  ) => void;
  onPreviewRecordTrashCleanup: () => void;
  onLocalDiagnosticsSettingsChange: (settings: {
    enabled: boolean;
    retentionDays: LocalDiagnosticsRetentionDays;
    maxBytes: LocalDiagnosticsMaxBytes;
  }) => void;
  onOpenLocalDiagnostics: () => void;
  onExportLocalDiagnostics: () => void;
  onRequestClearLocalDiagnostics: () => void;
  onRequestResetLocalData: () => void;
  onLoginItemChange: (enabled: boolean) => void;
  onUpdateSettingsChange: (
    externalNetworkEnabled: boolean,
    automaticChecksEnabled: boolean,
    automaticDownloadsEnabled: boolean,
    checkIntervalHours: UpdateCheckIntervalHours,
  ) => void;
  onCheckForUpdates: () => void;
  onSkipUpdateVersion: (version: string | null) => void;
  onDownloadUpdate: () => void;
  onDiscardDownloadedUpdate: () => void;
  onRequestInstallUpdate: () => void;
  onAcknowledgeUpdateResult: () => void;
  onOpenUpdateRelease: () => void;
  onThemeChange: (theme: ThemePreference) => void;
  onSubmitShortcutChange: (submitShortcut: SubmitShortcut) => void;
  onMarkdownLayoutChange: (markdownLayout: MarkdownLayout) => void;
  onRequestNumberingChange: (
    notebook: Notebook,
    numberingStyle: NumberingStyle,
    numberingStart: number,
  ) => void;
  onRequestAttachmentDirectoryChange: (
    notebook: Notebook,
    attachmentDirectory: string,
  ) => void;
  onNotebookPinChange: (notebookId: string, pinned: boolean) => void;
  onNotebookRename: (notebook: Notebook, displayName: string) => void;
  onWorkspaceOpenPreferenceChange: (
    useLastSelection: boolean,
    defaultNotebookId: string | null,
  ) => void;
  onRequestUnbind: (notebook: Notebook) => void;
  onRebind: (notebook: Notebook) => void;
  onRecoverNotebookTarget: (notebook: Notebook) => void;
  onInspectConflict: (notebook: Notebook) => void;
  onRequestConvertNotebook: (notebook: Notebook) => void;
  onRequestTrashNotebookFile: (notebook: Notebook) => void;
}

export function SettingsView({
  workspace,
  notebook,
  notebooks,
  status,
  productSettings,
  workspaceOpenPreference,
  integration,
  defaultIdentity,
  loginItemStatus,
  loginItemUnavailable,
  updateStatus,
  updateUnavailable,
  updateAction,
  updateProgress,
  localDiagnosticsStatus,
  localDiagnosticsUnavailable,
  localDataResetPreview,
  localDataResetUnavailable,
  theme,
  submitShortcut,
  markdownLayout,
  busyAction,
  integrationRestartNotice,
  onToggleIntegration,
  onRestartCodex,
  onRestartCodexInstance,
  onConfigureDefaultIdentity,
  onUseDefaultIdentity,
  onUnbindDefaultIdentity,
  onRetryRecovery,
  onExportLocalData,
  onProductSettingsChange,
  onPreviewRecordTrashCleanup,
  onLocalDiagnosticsSettingsChange,
  onOpenLocalDiagnostics,
  onExportLocalDiagnostics,
  onRequestClearLocalDiagnostics,
  onRequestResetLocalData,
  onLoginItemChange,
  onUpdateSettingsChange,
  onCheckForUpdates,
  onSkipUpdateVersion,
  onDownloadUpdate,
  onDiscardDownloadedUpdate,
  onRequestInstallUpdate,
  onAcknowledgeUpdateResult,
  onOpenUpdateRelease,
  onThemeChange,
  onSubmitShortcutChange,
  onMarkdownLayoutChange,
  onRequestNumberingChange,
  onRequestAttachmentDirectoryChange,
  onNotebookPinChange,
  onNotebookRename,
  onWorkspaceOpenPreferenceChange,
  onRequestUnbind,
  onRebind,
  onRecoverNotebookTarget,
  onInspectConflict,
  onRequestConvertNotebook,
  onRequestTrashNotebookFile,
}: SettingsViewProps) {
  const [numberingStyleDraft, setNumberingStyleDraft] = useState<NumberingStyle>(
    notebook?.numberingStyle ?? "numeric",
  );
  const [numberingStartDraft, setNumberingStartDraft] = useState(
    String(notebook?.numberingStart ?? 1),
  );
  const [attachmentDirectoryDraft, setAttachmentDirectoryDraft] = useState(
    notebook?.attachmentDirectory ?? "",
  );
  const [notebookDisplayNameDraft, setNotebookDisplayNameDraft] = useState(
    notebook?.displayName ?? "",
  );
  const [defaultIdentityAliasDraft, setDefaultIdentityAliasDraft] = useState(
    defaultIdentity?.alias ?? "默认身份",
  );
  const [recordTrashRetentionDraft, setRecordTrashRetentionDraft] = useState(
    productSettings?.recordTrashRetentionDays === null
      ? "permanent"
      : String(productSettings?.recordTrashRetentionDays ?? 30),
  );
  const [scanIgnoreDirectoriesDraft, setScanIgnoreDirectoriesDraft] = useState(
    productSettings?.notebookScanIgnoreDirectories
      .filter((value) => !productSettings.protectedNotebookScanIgnoreDirectories.includes(value))
      .join("\n") ?? "",
  );
  useEffect(() => {
    setNumberingStyleDraft(notebook?.numberingStyle ?? "numeric");
    setNumberingStartDraft(String(notebook?.numberingStart ?? 1));
    setAttachmentDirectoryDraft(notebook?.attachmentDirectory ?? "");
    setNotebookDisplayNameDraft(notebook?.displayName ?? "");
  }, [
    notebook?.attachmentDirectory,
    notebook?.id,
    notebook?.numberingStart,
    notebook?.numberingStyle,
    notebook?.displayName,
  ]);
  useEffect(() => {
    setDefaultIdentityAliasDraft(defaultIdentity?.alias ?? "默认身份");
  }, [defaultIdentity?.alias, defaultIdentity?.configured]);
  useEffect(() => {
    setRecordTrashRetentionDraft(
      productSettings?.recordTrashRetentionDays === null
        ? "permanent"
        : String(productSettings?.recordTrashRetentionDays ?? 30),
    );
    setScanIgnoreDirectoriesDraft(
      productSettings?.notebookScanIgnoreDirectories
        .filter((value) => !productSettings.protectedNotebookScanIgnoreDirectories.includes(value))
        .join("\n") ?? "",
    );
  }, [
    productSettings?.notebookScanIgnoreDirectories,
    productSettings?.protectedNotebookScanIgnoreDirectories,
    productSettings?.recordTrashRetentionDays,
  ]);
  const parsedNumberingStart = Number(numberingStartDraft);
  const numberingStartValid = Number.isInteger(parsedNumberingStart)
    && parsedNumberingStart >= 1
    && parsedNumberingStart <= 1_000_000_000;
  const numberingChanged = Boolean(
    notebook
    && (notebook.numberingStyle !== numberingStyleDraft
      || notebook.numberingStart !== parsedNumberingStart),
  );
  const attachmentDirectoryChanged = Boolean(
    notebook
    && attachmentDirectoryDraft.trim()
    && notebook.attachmentDirectory !== attachmentDirectoryDraft.trim(),
  );
  const notebookDisplayName = notebookDisplayNameDraft.trim();
  const notebookDisplayNameValid = notebookDisplayName.length > 0
    && [...notebookDisplayName].length <= 120;
  const notebookDisplayNameChanged = Boolean(
    notebook && notebook.displayName !== notebookDisplayName,
  );
  const workspaceOpenPreferenceValue = !workspaceOpenPreference
    ? "last"
    : workspaceOpenPreference.useLastSelection
      ? "last"
      : workspaceOpenPreference.defaultNotebookId ?? "inbox";
  const parsedAdditionalScanIgnoreDirectories = scanIgnoreDirectoriesDraft
    .split("\n")
    .map((value) => value.trim())
    .filter(Boolean);
  const protectedScanIgnoreDirectories = productSettings
    ?.protectedNotebookScanIgnoreDirectories ?? [];
  const parsedScanIgnoreDirectories = [
    ...protectedScanIgnoreDirectories,
    ...parsedAdditionalScanIgnoreDirectories,
  ];
  const scanIgnoreDirectoriesValid = parsedScanIgnoreDirectories.length <= 64
    && new Set(parsedAdditionalScanIgnoreDirectories).size
      === parsedAdditionalScanIgnoreDirectories.length
    && parsedAdditionalScanIgnoreDirectories.every((value) => (
      value.length <= 1024
      && !value.startsWith("/")
      && !value.includes("\\")
      && !value.includes("*")
      && !value.includes("?")
      && value.split("/").every((part) => part && part !== "." && part !== "..")
      && !protectedScanIgnoreDirectories.includes(value)
    ));
  const parsedRecordTrashRetention = recordTrashRetentionDraft === "permanent"
    ? null
    : Number(recordTrashRetentionDraft);
  const productSettingsChanged = Boolean(
    productSettings
    && (
      productSettings.recordTrashRetentionDays !== parsedRecordTrashRetention
      || productSettings.notebookScanIgnoreDirectories.join("\n")
        !== parsedScanIgnoreDirectories.join("\n")
    ),
  );
  const defaultIdentityAlias = defaultIdentityAliasDraft.trim();
  const defaultIdentityAliasValid = defaultIdentityAlias.length > 0
    && [...defaultIdentityAlias].length <= 120;
  const defaultIdentityAliasChanged = Boolean(
    defaultIdentity?.configured && defaultIdentity.alias !== defaultIdentityAlias,
  );
  const connectedTargets = integration?.connectedTargetCount ?? 0;
  const availableTargets = integration?.availableTargetCount ?? 0;
  const detectedInstances = integration?.detectedInstanceCount ?? 0;
  const connectableInstances = integration?.connectableInstanceCount ?? 0;
  const defaultInstance = integration?.instances.find((instance) => instance.isDefault);
  const integrationSummary = !integration
    ? "正在检测…"
    : integration.paused
      ? "自动显示已暂停"
      : connectedTargets > 0
        ? `自动显示已开启 · 已连接 ${connectedTargets}/${Math.max(availableTargets, connectedTargets)} 个窗口`
        : "自动显示已开启 · 当前未连接";
  const canRestartDefault = Boolean(
    integration
    && !integration.paused
    && (!defaultInstance || ["unavailable", "failed"].includes(defaultInstance.state))
    && integration.endpointState !== "connecting",
  );
  const integrationBusy = integrationRestartNotice?.tone === "progress"
    || busyAction === "integration"
    || busyAction === "restart-codex"
    || busyAction.startsWith("restart-codex-instance:");
  const defaultIdentityBusy = busyAction.startsWith("default-identity");
  const defaultIdentitySummary = !defaultIdentity
    ? "正在检测…"
    : !defaultIdentity.platformSupported
      ? "当前平台尚未验证"
      : !defaultIdentity.configured
        ? defaultIdentity.profilePresent
          ? "专用 profile 已保留，当前未绑定"
          : "尚未配置"
        : defaultIdentityStateLabels[defaultIdentity.runtimeState];
  const localDiagnosticsBusy = busyAction.startsWith("diagnostics-");
  const localDiagnosticsControlsDisabled = Boolean(busyAction)
    || localDiagnosticsUnavailable
    || !localDiagnosticsStatus?.available;
  const localDiagnosticsAvailability = localDiagnosticsUnavailable
    ? "诊断服务暂时不可用"
    : !localDiagnosticsStatus
      ? "正在检测…"
      : !localDiagnosticsStatus.available
        ? "诊断存储不可用"
        : "可用";
  const localDiagnosticsRecordingState = !localDiagnosticsStatus
    ? "正在检测…"
    : localDiagnosticsStatus.enabled
      ? "已开启"
      : "已关闭";
  const updateBusy = Boolean(updateAction);
  const updateCheckBusy = updateAction === "check";
  const updateControlsDisabled = updateBusy || updateUnavailable || !updateStatus;
  const updateChannelState = updateUnavailable
    ? "无法读取更新状态"
    : !updateStatus
      ? "正在读取…"
      : updatePhaseLabels[updateStatus.phase];
  const updateLastChecked = updateStatus?.lastCheckedAtMs
    ? localDiagnosticTimeFormatter.format(new Date(updateStatus.lastCheckedAtMs))
    : "尚未检查";
  const visibleUpdateProgress = updateProgress?.version === updateStatus?.availableVersion
    ? updateProgress
    : null;
  const updateDownloadedBytes = visibleUpdateProgress?.downloadedBytes
    ?? updateStatus?.downloadedBytes
    ?? 0;
  const updateTotalBytes = visibleUpdateProgress?.totalBytes
    ?? updateStatus?.downloadTotalBytes
    ?? null;
  const updatePercent = updateTotalBytes && updateTotalBytes > 0
    ? Math.min(100, Math.max(0, Math.round(updateDownloadedBytes / updateTotalBytes * 100)))
    : null;
  const updateDeliveryActive = updateStatus !== null && [
    "downloading",
    "verifying",
    "readyToInstall",
    "installing",
    "restarting",
  ].includes(updateStatus.phase);
  return (
    <div className="settings-view">
      <header className="view-heading">
        <div><p className="eyebrow">WakeGPT</p><h1>设置</h1></div>
      </header>
      {workspace ? <section className="settings-group">
        <header><h2>当前工作区</h2><p>Markdown 文件始终保留在你的工作区中。</p></header>
        <dl className="settings-list">
          <div><dt>名称</dt><dd>{workspace.displayName}</dd></div>
          <div><dt>路径</dt><dd className="path-value" title={workspace.rootPath}>{workspace.rootPath}</dd></div>
          <div><dt>当前笔记</dt><dd>{notebook?.displayName ?? "收件箱"}</dd></div>
          <div>
            <dt>打开工作区时</dt>
            <dd className="workspace-open-setting">
              <SelectControl
                ariaLabel="工作区默认打开位置"
                value={workspaceOpenPreferenceValue}
                options={[
                  { value: "last", label: "上次使用的位置" },
                  { value: "inbox", label: "收件箱" },
                  ...notebooks.map((item) => ({
                    value: item.id,
                    label: item.displayName,
                  })),
                ]}
                disabled={!workspaceOpenPreference || Boolean(busyAction)}
                popoverMinWidth={260}
                onChange={(value) => onWorkspaceOpenPreferenceChange(
                  value === "last",
                  value === "last" || value === "inbox" ? null : value,
                )}
              />
              <small>“上次使用”会恢复该工作区最近的 Tab；选择收件箱或速记本后，仅影响下次打开该工作区。</small>
            </dd>
          </div>
          {notebook ? (
            <div>
              <dt>显示名称</dt>
              <dd className="notebook-display-name-setting">
                <input
                  value={notebookDisplayNameDraft}
                  maxLength={120}
                  aria-label="当前速记本显示名称"
                  aria-invalid={!notebookDisplayNameValid}
                  disabled={Boolean(busyAction)}
                  onChange={(event) => setNotebookDisplayNameDraft(event.currentTarget.value)}
                />
                <button
                  className="secondary-button"
                  type="button"
                  disabled={
                    !notebookDisplayNameChanged
                    || !notebookDisplayNameValid
                    || Boolean(busyAction)
                  }
                  onClick={() => onNotebookRename(notebook, notebookDisplayName)}
                >
                  {busyAction === "rename-notebook" ? "正在保存…" : "保存显示名称"}
                </button>
                <small>只更改 WakeGPT 和速记卡中的名称，不重命名 Markdown 文件。</small>
              </dd>
            </div>
          ) : null}
          {notebook ? (
            <div>
              <dt>自动序号</dt>
              <dd className="numbering-setting">
                <div className="numbering-config-grid">
                  <SelectControl
                    ariaLabel="当前速记本编号格式"
                    value={numberingStyleDraft}
                    options={numberingOptions}
                    disabled={
                      notebook.targetState !== "ready"
                      || notebook.numberingSyncPending
                      || notebook.attachmentDirectorySyncPending
                      || Boolean(busyAction)
                    }
                    onChange={(value) => setNumberingStyleDraft(value as NumberingStyle)}
                  />
                  <label>
                    <span>起始</span>
                    <input
                      type="number"
                      min={1}
                      max={1_000_000_000}
                      step={1}
                      value={numberingStartDraft}
                      aria-label="当前速记本起始序号"
                      aria-invalid={!numberingStartValid}
                      disabled={
                        notebook.targetState !== "ready"
                        || notebook.numberingSyncPending
                        || notebook.attachmentDirectorySyncPending
                        || Boolean(busyAction)
                      }
                      onChange={(event) => setNumberingStartDraft(event.currentTarget.value)}
                    />
                  </label>
                  <button
                    className="secondary-button"
                    type="button"
                    disabled={
                      !numberingChanged
                      || !numberingStartValid
                      || notebook.targetState !== "ready"
                      || notebook.numberingSyncPending
                      || notebook.attachmentDirectorySyncPending
                      || Boolean(busyAction)
                    }
                    onClick={() => onRequestNumberingChange(
                      notebook,
                      numberingStyleDraft,
                      parsedNumberingStart,
                    )}
                  >
                    预览更改
                  </button>
                </div>
                <small className="numbering-help">
                  连续数字从起始值递增；日期分组会在每天从起始值重新开始。
                </small>
                {notebook.numberingSyncPending ? (
                  <small>编号配置已保存，Markdown 重排等待恢复。</small>
                ) : null}
              </dd>
            </div>
          ) : null}
          {notebook ? (
            <div>
              <dt>附件目录</dt>
              <dd className="attachment-directory-setting">
                <div className="attachment-directory-config">
                  <input
                    value={attachmentDirectoryDraft}
                    aria-label="当前速记本附件目录"
                    disabled={
                      notebook.targetState !== "ready"
                      || notebook.numberingSyncPending
                      || notebook.attachmentDirectorySyncPending
                      || Boolean(busyAction)
                    }
                    onChange={(event) => setAttachmentDirectoryDraft(event.currentTarget.value)}
                  />
                  <button
                    className="secondary-button"
                    type="button"
                    disabled={
                      !attachmentDirectoryChanged
                      || notebook.targetState !== "ready"
                      || notebook.numberingSyncPending
                      || notebook.attachmentDirectorySyncPending
                      || Boolean(busyAction)
                    }
                    onClick={() => onRequestAttachmentDirectoryChange(
                      notebook,
                      attachmentDirectoryDraft.trim(),
                    )}
                  >
                    预览迁移
                  </button>
                </div>
                <small>工作区相对目录；文件名按图片内容生成。旧副本会在新链接验证后移入系统废纸篓。</small>
                {notebook.attachmentDirectorySyncPending ? (
                  <small>附件目录已保存，附件迁移与 Markdown 更新等待恢复。</small>
                ) : null}
              </dd>
            </div>
          ) : null}
          {notebook ? <div><dt>文件状态</dt><dd>{notebookTargetLabel(notebook)}</dd></div> : null}
          {notebook ? <div><dt>目标文件</dt><dd className="path-value" title={notebook.relativePath}>{notebook.relativePath}</dd></div> : null}
        </dl>
        {notebook ? (
          <>
            <div className="setting-row">
              <div><strong>固定当前 Tab</strong><span>关闭 Tab 只取消固定，不删除速记本或文件。</span></div>
              <button
                className="switch"
                type="button"
                role="switch"
                aria-label="固定当前速记本 Tab"
                aria-checked={notebook.isPinned}
                disabled={busyAction === "notebook-tab"}
                onClick={() => onNotebookPinChange(notebook.id, !notebook.isPinned)}
              ><span /></button>
            </div>
            {notebook.attachmentDirectorySyncPending ? (
              <div className="setting-row">
                <div>
                  <strong>恢复附件迁移</strong>
                  <span>WakeGPT 会先核对新旧副本，再完成 Markdown 和文件回执；不会永久删除无法验证的文件。</span>
                </div>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={busyAction === "recovery"}
                  onClick={onRetryRecovery}
                >
                  {busyAction === "recovery" ? "正在恢复…" : "重试附件迁移"}
                </button>
              </div>
            ) : notebook.numberingSyncPending ? (
              <div className="setting-row">
                <div>
                  <strong>恢复编号重排</strong>
                  <span>编号配置已经保存在本地；WakeGPT 会重新核对文件摘要后完成受管区重排，不会覆盖冲突内容。</span>
                </div>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={busyAction === "recovery"}
                  onClick={onRetryRecovery}
                >
                  {busyAction === "recovery" ? "正在恢复…" : "重试重排"}
                </button>
              </div>
            ) : notebook.targetState === "ready" || notebook.targetState === "unbound" ? (
              <>
                <div className="setting-row">
                  <div>
                    <strong>Markdown 绑定</strong>
                    <span>
                      {notebook.targetState === "unbound"
                        ? "会先核对原文件、受管区和同步回执；一致时恢复持续同步。"
                        : "解除后停止同步，现有文件、标记和记录都会保留。"}
                    </span>
                  </div>
                  <button
                    className={notebook.targetState === "unbound" ? "secondary-button" : "danger-button"}
                    type="button"
                    disabled={busyAction === "unbind-notebook" || busyAction === "rebind-notebook"}
                    onClick={() => {
                      if (notebook.targetState === "unbound") onRebind(notebook);
                      else onRequestUnbind(notebook);
                    }}
                  >
                    {busyAction === "rebind-notebook"
                      ? "正在重新绑定…"
                      : notebook.targetState === "unbound"
                        ? "重新绑定"
                        : "解除绑定"}
                  </button>
                </div>
                <div className="setting-row">
                  <div><strong>转为普通 Markdown</strong><span>移除 WakeGPT 控制标记，保留文件中的全部可见内容与本地记录。</span></div>
                  <button className="secondary-button" type="button" disabled={busyAction === "convert-notebook"} onClick={() => onRequestConvertNotebook(notebook)}>
                    {busyAction === "convert-notebook" ? "正在转换…" : "转为普通 Markdown"}
                  </button>
                </div>
                <div className="setting-row">
                  <div><strong>速记本文件</strong><span>只把 Markdown 文件移入 macOS 废纸篓；本地记录和目标身份继续保留。</span></div>
                  <button className="danger-button" type="button" disabled={busyAction === "trash-notebook-file"} onClick={() => onRequestTrashNotebookFile(notebook)}>
                    {busyAction === "trash-notebook-file" ? "正在移入废纸篓…" : "移到废纸篓"}
                  </button>
                </div>
              </>
            ) : notebook.targetState === "conflict" ? (
              <div className="setting-row" data-state="conflict">
                <div>
                  <strong>处理同步冲突</strong>
                  <span>WakeGPT 已暂停写入。比较文件与本地记录后，可采用任一版本或解除管理。</span>
                </div>
                <button
                  className="primary-button"
                  type="button"
                  disabled={busyAction === "inspect-conflict" || busyAction === "resolve-conflict"}
                  onClick={() => onInspectConflict(notebook)}
                >
                  {busyAction === "inspect-conflict" ? "正在读取…" : "查看并处理"}
                </button>
              </div>
            ) : (
              <div className="setting-row">
                <div>
                  <strong>{notebook.lastErrorCode === "notebook_converted_to_plain" ? "普通 Markdown" : "恢复文件绑定"}</strong>
                  <span>
                    {notebook.lastErrorCode === "notebook_converted_to_plain"
                      ? "文件已脱离 WakeGPT 管理；可见内容和本地记录均保留。"
                      : "在当前工作区按唯一目标 ID 查找已恢复、移动或重命名的文件。"}
                  </span>
                </div>
                {notebook.lastErrorCode === "notebook_converted_to_plain" ? null : (
                  <button className="secondary-button" type="button" disabled={busyAction === "recover-notebook-target"} onClick={() => onRecoverNotebookTarget(notebook)}>
                    {busyAction === "recover-notebook-target" ? "正在查找…" : "重新查找文件"}
                  </button>
                )}
              </div>
            )}
          </>
        ) : null}
      </section> : null}
      <section className="settings-group">
        <header><h2>编辑与提交</h2><p>快捷键和 Markdown 布局会在主窗口重启后保留。</p></header>
        <div className="setting-row">
          <div><strong>提交快捷键</strong><span>输入法组合文字时不会误提交。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="速记提交快捷键"
            value={submitShortcut}
            options={submitShortcutOptions}
            popoverMinWidth={260}
            onChange={(nextValue) => onSubmitShortcutChange(nextValue as SubmitShortcut)}
          />
        </div>
        <div className="setting-row">
          <div><strong>Markdown 编辑布局</strong><span>打开整篇 Markdown 时使用此布局。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="Markdown 编辑布局"
            value={markdownLayout}
            options={markdownLayoutOptions}
            popoverMinWidth={210}
            onChange={(nextValue) => onMarkdownLayoutChange(nextValue as MarkdownLayout)}
          />
        </div>
      </section>
      <section className="settings-group default-identity-group">
        <header>
          <h2>默认身份</h2>
          <p>固定使用 WakeGPT 专用的 ChatGPT profile；其他应用切换自己的账号时不会改动这里。</p>
        </header>
        <div className="setting-row">
          <div>
            <strong>身份名称</strong>
            <span>仅是本地可读别名，不读取或保存账号邮箱。</span>
          </div>
          <div className="default-identity-alias">
            <input
              value={defaultIdentityAliasDraft}
              maxLength={120}
              aria-label="默认身份本地名称"
              aria-invalid={!defaultIdentityAliasValid}
              disabled={defaultIdentityBusy || !defaultIdentity?.platformSupported}
              onChange={(event) => setDefaultIdentityAliasDraft(event.currentTarget.value)}
            />
            {!defaultIdentity?.configured ? (
              <button
                className="primary-button"
                type="button"
                disabled={
                  !defaultIdentity
                  || !defaultIdentity.platformSupported
                  || !defaultIdentityAliasValid
                  || defaultIdentityBusy
                }
                onClick={() => onConfigureDefaultIdentity(defaultIdentityAlias, true)}
              >
                <Icon name="lock" size={15} />
                {busyAction === "default-identity-configure"
                  ? "正在配置…"
                  : defaultIdentity?.profilePresent
                    ? "重新绑定并锁定"
                    : "创建并锁定"}
              </button>
            ) : (
              <button
                className="secondary-button"
                type="button"
                disabled={!defaultIdentityAliasChanged || !defaultIdentityAliasValid || defaultIdentityBusy}
                onClick={() => onConfigureDefaultIdentity(defaultIdentityAlias, defaultIdentity.locked)}
              >
                保存名称
              </button>
            )}
          </div>
        </div>
        {defaultIdentity?.configured ? (
          <>
            <div className="setting-row">
              <div>
                <strong>锁定默认身份</strong>
                <span>开启后，WakeGPT 始终把这个专用 profile 作为默认入口。</span>
              </div>
              <button
                className="switch"
                type="button"
                role="switch"
                aria-label="锁定默认身份"
                aria-checked={defaultIdentity.locked}
                disabled={!defaultIdentityAliasValid || defaultIdentityBusy}
                onClick={() => onConfigureDefaultIdentity(
                  defaultIdentityAlias,
                  !defaultIdentity.locked,
                )}
              ><span /></button>
            </div>
            <div className="setting-row">
              <div className="default-identity-state">
                <span className="default-identity-state__dot" data-state={defaultIdentity.runtimeState} />
                <div>
                  <strong>{defaultIdentity.alias}</strong>
                  <span>{defaultIdentitySummary}</span>
                  {defaultIdentity.lastErrorCode ? (
                    <small>{defaultIdentityErrorLabel(defaultIdentity.lastErrorCode)}</small>
                  ) : null}
                </div>
              </div>
              <button
                className="primary-button"
                type="button"
                disabled={
                  defaultIdentityBusy
                  || defaultIdentity.runtimeState === "occupied"
                  || defaultIdentity.runtimeState === "unavailable"
                }
                onClick={onUseDefaultIdentity}
              >
                <Icon name="login" size={15} />
                {busyAction === "default-identity-open"
                  ? "正在打开…"
                  : defaultIdentity.runtimeState === "running"
                    ? "切换到此身份"
                    : "打开此身份"}
              </button>
            </div>
            <div className="setting-row">
              <div>
                <strong>解除绑定</strong>
                <span>保留专用 profile、官方登录状态和已打开的 ChatGPT；不会删除账号数据。</span>
              </div>
              <button
                className="secondary-button"
                type="button"
                disabled={defaultIdentityBusy}
                onClick={onUnbindDefaultIdentity}
              >
                <Icon name="unlink" size={15} />
                {busyAction === "default-identity-unbind" ? "正在解除…" : "解除绑定"}
              </button>
            </div>
          </>
        ) : (
          <div className="integration-action">
            <div>
              <strong>{defaultIdentitySummary}</strong>
              <span>首次打开后，请只在 OpenAI 官方登录页面中完成登录；WakeGPT 不接触密码、验证码、Cookie 或令牌。</span>
            </div>
          </div>
        )}
        <dl className="settings-list settings-list--compact">
          <div><dt>隔离边界</dt><dd>WakeGPT 专用 profile</dd></div>
          <div><dt>账号认证</dt><dd>OpenAI 官方登录页</dd></div>
          <div><dt>本地数据库</dt><dd>仅保存别名与锁定状态</dd></div>
        </dl>
      </section>
      <section className="settings-group">
        <header><h2>ChatGPT 中的 Codex</h2><p>控制 ChatGPT 界面中的 WakeGPT 速记卡。</p></header>
        <div className="setting-row">
          <div><strong>显示速记卡</strong><span>{integrationSummary}</span></div>
          <button className="switch" type="button" role="switch" aria-label="显示 ChatGPT 速记卡" aria-checked={!integration?.paused} disabled={!integration || integrationBusy} onClick={onToggleIntegration}><span /></button>
        </div>
        {integrationRestartNotice ? (
          <div
            className="integration-action"
            data-tone={integrationRestartNotice.tone}
            role={integrationRestartNotice.tone === "error" ? "alert" : "status"}
          >
            <div>
              <strong>
                {integrationRestartNotice.tone === "error"
                  ? `${integrationRestartNotice.displayName} 未能启用`
                  : `正在启用 ${integrationRestartNotice.displayName}`}
              </strong>
              <span>{integrationRestartNotice.detail}</span>
            </div>
          </div>
        ) : null}
        {integration && integration.endpointState === "partiallyConnected" ? (
          <div className="integration-action">
            <div>
              <strong>部分实例尚未显示</strong>
              <span>已连接 {connectedTargets}/{Math.max(availableTargets, connectedTargets)} 个窗口；检测到 {detectedInstances} 个实例，其中 {connectableInstances} 个开放了安全回环调试入口。</span>
            </div>
            {canRestartDefault ? (
              <button
                className="secondary-button"
                type="button"
                disabled={integrationBusy}
                onClick={onRestartCodex}
              >
                {busyAction === "restart-codex" ? "正在启动…" : "启动默认 ChatGPT"}
              </button>
            ) : null}
          </div>
        ) : null}
        {integration && !integration.paused && !["connected", "partiallyConnected"].includes(integration.endpointState) ? (
          <div className="integration-action">
            <div>
              <strong>启用 ChatGPT 右侧卡片</strong>
              <span>{codexEndpointLabels[integration.endpointState]}。重启操作只作用于默认 ChatGPT，其他实例保持不动。</span>
            </div>
            {canRestartDefault ? (
              <button
                className="secondary-button"
                type="button"
                disabled={integrationBusy}
                onClick={onRestartCodex}
              >
                {busyAction === "restart-codex" ? "正在重启…" : "启动或重启默认 ChatGPT"}
              </button>
            ) : null}
          </div>
        ) : null}
        {integration?.instances.length ? (
          <div className="integration-instances" role="list" aria-label="ChatGPT 实例连接状态">
            {integration.instances.map((instance) => (
              <div className="integration-instance" role="listitem" key={instance.id}>
                <span className="integration-instance__dot" data-state={instance.state} />
                <div className="integration-instance__body">
                  <strong>{instance.displayName}</strong>
                  <span>
                    {codexInstanceStateLabels[instance.state]}
                    {instance.detectedCodexVersion
                      ? ` · ${instance.detectedCodexVersion}${instance.detectedCodexBuild ? ` (${instance.detectedCodexBuild})` : ""}`
                      : ""}
                    {instance.availableTargetCount > 0
                      ? ` · ${instance.connectedTargetCount}/${instance.availableTargetCount} 个窗口`
                      : ""}
                  </span>
                  {instance.lastErrorCode && !["unavailable", "paused"].includes(instance.state)
                    ? <small>{integrationErrorLabel(instance.lastErrorCode)}</small>
                    : null}
                </div>
                {instance.canRestart
                  && !integration.paused
                  && ["unavailable", "failed"].includes(instance.state) ? (
                    <button
                      className="secondary-button integration-instance__action"
                      type="button"
                      aria-label={`启用 ${instance.displayName} 的速记卡`}
                      disabled={integrationBusy}
                      onClick={() => onRestartCodexInstance(instance)}
                    >
                      {busyAction === `restart-codex-instance:${instance.id}`
                        ? "正在重启…"
                        : "启用此实例"}
                    </button>
                  ) : null}
              </div>
            ))}
          </div>
        ) : null}
        <dl className="settings-list settings-list--compact">
          <div><dt>适配器</dt><dd>{integration?.adapterVersion ?? "—"}</dd></div>
          <div><dt>已验证宿主</dt><dd>{integration ? `${integration.supportedCodexVersion} (${integration.supportedCodexBuild})` : "—"}</dd></div>
          <div><dt>连接状态</dt><dd>{integration ? codexEndpointLabels[integration.endpointState] : "正在检测…"}</dd></div>
          <div><dt>实例覆盖</dt><dd>{integration ? `${connectableInstances}/${detectedInstances} 个实例可连接` : "—"}</dd></div>
          <div><dt>窗口覆盖</dt><dd>{integration ? `${connectedTargets}/${availableTargets} 个窗口已显示` : "—"}</dd></div>
          {integration?.failedTargetCount ? <div><dt>失败窗口</dt><dd>{integration.failedTargetCount} 个</dd></div> : null}
          {integration?.lastErrorCode ? <div><dt>最近诊断</dt><dd>{integrationErrorLabel(integration.lastErrorCode)}</dd></div> : null}
        </dl>
      </section>
      <section className="settings-group">
        <header><h2>应用</h2><p>WakeGPT 保留 Dock 与菜单栏入口；关闭窗口后仍可继续运行。</p></header>
        <div className="setting-row">
          <div>
            <strong>登录时启动</strong>
            <span>
              {loginItemUnavailable
                ? "无法读取系统登录项；WakeGPT 未改变现有设置。"
                : loginItemStatus?.enabled
                  ? "登录后在后台启动；主窗口保持隐藏，可从 Dock 或菜单栏打开。"
                  : loginItemStatus
                    ? "默认关闭；开启后登录系统时在后台启动。"
                    : "正在读取系统登录项…"}
            </span>
          </div>
          <button
            className="switch"
            type="button"
            role="switch"
            aria-label="登录时启动 WakeGPT"
            aria-checked={loginItemStatus?.enabled ?? false}
            disabled={
              !loginItemStatus
              || loginItemUnavailable
              || busyAction === "login-item"
            }
            onClick={() => onLoginItemChange(!loginItemStatus?.enabled)}
          ><span /></button>
        </div>
      </section>
      <section className="settings-group update-settings-group">
        <header>
          <h2>版本与更新</h2>
          <p>只使用带签名的正式稳定版；默认不下载，安装始终需要你明确确认。</p>
        </header>
        <div className="setting-row">
          <div>
            <strong>外部网络</strong>
            <span>
              {updateStatus?.externalNetworkEnabled
                ? "已允许 WakeGPT 访问经过验证的 GitHub 更新地址"
                : updateStatus
                  ? "已关闭；自动检查、下载和 GitHub 发布页均不会联网"
                  : "正在读取网络偏好…"}
            </span>
          </div>
          <button
            className="switch"
            type="button"
            role="switch"
            aria-label="允许 WakeGPT 访问外部更新网络"
            aria-checked={updateStatus?.externalNetworkEnabled ?? false}
            disabled={updateControlsDisabled}
            onClick={() => {
              if (!updateStatus) return;
              onUpdateSettingsChange(
                !updateStatus.externalNetworkEnabled,
                updateStatus.automaticChecksEnabled,
                updateStatus.automaticDownloadsEnabled,
                updateStatus.checkIntervalHours,
              );
            }}
          ><span /></button>
        </div>
        <div className="setting-row">
          <div>
            <strong>自动检查</strong>
            <span>
              {updateStatus?.automaticChecksEnabled
                ? `已开启 · 每 ${updateStatus.checkIntervalHours} 小时最多检查一次`
                : updateStatus
                  ? "已关闭；手动检查只在你点击后进行"
                  : "正在读取更新偏好…"}
            </span>
          </div>
          <button
            className="switch"
            type="button"
            role="switch"
            aria-label="自动检查 WakeGPT 更新"
            aria-checked={updateStatus?.automaticChecksEnabled ?? false}
            disabled={updateControlsDisabled}
            onClick={() => {
              if (!updateStatus) return;
              onUpdateSettingsChange(
                updateStatus.externalNetworkEnabled,
                !updateStatus.automaticChecksEnabled,
                updateStatus.automaticDownloadsEnabled,
                updateStatus.checkIntervalHours,
              );
            }}
          ><span /></button>
        </div>
        <div className="setting-row">
          <div>
            <strong>后台自动下载</strong>
            <span>
              {updateStatus?.automaticDownloadsEnabled
                ? "发现未跳过的新版本后可在后台下载并验签；安装仍需明确确认"
                : updateStatus
                  ? "默认关闭；只在你点击“下载更新”后下载"
                  : "正在读取下载偏好…"}
            </span>
          </div>
          <button
            className="switch"
            type="button"
            role="switch"
            aria-label="后台自动下载 WakeGPT 更新"
            aria-checked={updateStatus?.automaticDownloadsEnabled ?? false}
            disabled={updateControlsDisabled || !updateStatus?.externalNetworkEnabled}
            onClick={() => {
              if (!updateStatus) return;
              onUpdateSettingsChange(
                updateStatus.externalNetworkEnabled,
                updateStatus.automaticChecksEnabled,
                !updateStatus.automaticDownloadsEnabled,
                updateStatus.checkIntervalHours,
              );
            }}
          ><span /></button>
        </div>
        <div className="setting-row">
          <div><strong>检查间隔</strong><span>应用持续运行时仍受同一间隔限制。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="WakeGPT 自动检查更新间隔"
            value={updateStatus ? String(updateStatus.checkIntervalHours) : ""}
            options={updateIntervalOptions}
            placeholder="正在读取…"
            disabled={updateControlsDisabled}
            onChange={(nextValue) => {
              if (!updateStatus) return;
              onUpdateSettingsChange(
                updateStatus.externalNetworkEnabled,
                updateStatus.automaticChecksEnabled,
                updateStatus.automaticDownloadsEnabled,
                Number(nextValue) as UpdateCheckIntervalHours,
              );
            }}
          />
        </div>
        <dl className="settings-list settings-list--compact">
          <div><dt>当前版本</dt><dd>{updateStatus?.currentVersion ?? "—"}</dd></div>
          <div><dt>发布目标</dt><dd>{updateStatus?.target === "darwin-universal" ? "macOS Universal" : updateStatus?.target ?? "—"}</dd></div>
          <div><dt>通道状态</dt><dd>{updateChannelState}</dd></div>
          <div><dt>最近检查</dt><dd>{updateLastChecked}</dd></div>
          {updateStatus?.availableVersion ? (
            <div>
              <dt>可用版本</dt>
              <dd>{updateStatus.availableVersion}{updateStatus.availableIsSkipped ? " · 已跳过" : ""}</dd>
            </div>
          ) : null}
          {updateStatus?.publishedAt ? <div><dt>发布时间</dt><dd>{updateStatus.publishedAt}</dd></div> : null}
          {updateStatus?.configurationErrorCode ? (
            <div><dt>配置诊断</dt><dd>{updateErrorLabel(updateStatus.configurationErrorCode)}</dd></div>
          ) : null}
          {updateStatus?.lastErrorCode ? (
            <div><dt>最近诊断</dt><dd>{updateErrorLabel(updateStatus.lastErrorCode)}</dd></div>
          ) : null}
          {updateStatus?.operationErrorCode ? (
            <div><dt>本次操作</dt><dd>{updateErrorLabel(updateStatus.operationErrorCode)}</dd></div>
          ) : null}
        </dl>
        {updateStatus?.releaseNotes ? (
          <div className="setting-note">
            <strong>版本说明</strong>
            <span>{updateStatus.releaseNotes}</span>
          </div>
        ) : null}
        {updateStatus && ["downloading", "verifying"].includes(updateStatus.phase) ? (
          <div className="update-progress" role="status" aria-live="polite">
            <div>
              <strong>
                {updateStatus.phase === "verifying"
                  ? "下载完成，正在验证签名并安全保存…"
                  : `正在下载 WakeGPT ${updateStatus.availableVersion ?? ""}`}
              </strong>
              <span>
                {updatePercent !== null
                  ? `${updatePercent}% · ${formatLocalDiagnosticsBytes(updateDownloadedBytes)} / ${formatLocalDiagnosticsBytes(updateTotalBytes ?? 0)}`
                  : `已接收 ${formatLocalDiagnosticsBytes(updateDownloadedBytes)}`}
              </span>
            </div>
            <progress
              aria-label={updateStatus.phase === "verifying" ? "正在验证更新签名" : "更新下载进度"}
              max={updateTotalBytes ?? undefined}
              value={updateTotalBytes ? updateDownloadedBytes : undefined}
            />
          </div>
        ) : null}
        {updateStatus?.phase === "readyToInstall" ? (
          <div className="update-result" data-tone="ready" role="status">
            <div>
              <strong>WakeGPT {updateStatus.availableVersion} 已下载并验证</strong>
              <span>更新包保持在 WakeGPT 私有目录；稍后安装或现在移除都不会改变当前版本。</span>
            </div>
          </div>
        ) : null}
        {updateStatus?.installOutcome ? (
          <div
            className="update-result"
            data-tone={updateStatus.installOutcome}
            role={updateStatus.installOutcome === "rollbackFailed" ? "alert" : "status"}
          >
            <div>
              <strong>
                {updateStatus.installOutcome === "installed"
                  ? `更新完成：WakeGPT ${updateStatus.installVersion ?? ""}`
                  : updateStatus.installOutcome === "rolledBack"
                    ? `更新未完成，已恢复到 WakeGPT ${updateStatus.installFromVersion ?? ""}`
                    : "更新未完成，自动恢复也未确认成功"}
              </strong>
              <span>
                {updateStatus.installOutcome === "installed"
                  ? "新版本和数据库迁移已经通过独立启动检查。"
                  : updateStatus.installOutcome === "rolledBack"
                    ? `当前版本和更新前数据库已恢复${updateStatus.installErrorCode ? ` · ${updateErrorLabel(updateStatus.installErrorCode)}` : ""}。`
                    : "请停止再次更新并查看本地诊断。"}
              </span>
            </div>
            <button
              className="secondary-button"
              type="button"
              disabled={updateBusy || updateStatus.installOccurredAtMs === null}
              onClick={onAcknowledgeUpdateResult}
            >
              {updateAction === "acknowledge" ? "正在确认…" : "知道了"}
            </button>
          </div>
        ) : null}
        <div className="setting-row">
          <div>
            <strong>手动检查</strong>
            <span>
              {updateStatus?.configured
                ? "检查不会下载内容；下载完成后仍需单独确认重启安装。"
                : "当前本地构建未嵌入正式公钥和 HTTPS 端点，因此不会联网。"}
            </span>
          </div>
          <div className="diagnostics-settings-actions">
            {updateStatus?.releasePageUrl && updateStatus.availableVersion ? (
              <button
                className="secondary-button"
                type="button"
                disabled={updateBusy || !updateStatus.externalNetworkEnabled}
                onClick={onOpenUpdateRelease}
              >
                GitHub 发布页
              </button>
            ) : null}
            {updateStatus?.availableVersion && updateStatus.phase === "available" ? (
              <button
                className="secondary-button"
                type="button"
                disabled={updateControlsDisabled}
                onClick={() => onSkipUpdateVersion(
                  updateStatus.availableIsSkipped ? null : updateStatus.availableVersion,
                )}
              >
                {updateStatus.availableIsSkipped ? "恢复提示" : "跳过此版本"}
              </button>
            ) : null}
            {updateStatus?.phase === "available"
              && updateStatus.availableVersion
              && !updateStatus.availableIsSkipped ? (
                <button
                  className="primary-button"
                  type="button"
                  disabled={updateControlsDisabled || !updateStatus.externalNetworkEnabled}
                  onClick={onDownloadUpdate}
                >
                  {updateAction === "download" ? "正在下载…" : "下载更新"}
                </button>
              ) : null}
            {updateStatus?.phase === "readyToInstall" ? (
              <>
                <button
                  className="secondary-button"
                  type="button"
                  disabled={updateBusy}
                  onClick={onDiscardDownloadedUpdate}
                >
                  {updateAction === "discard" ? "正在移除…" : "移除下载"}
                </button>
                <button
                  className="primary-button"
                  type="button"
                  disabled={updateBusy}
                  onClick={onRequestInstallUpdate}
                >
                  重启并更新
                </button>
              </>
            ) : null}
            <button
              className="secondary-button"
              type="button"
              disabled={
                updateControlsDisabled
                || !updateStatus?.configured
                || !updateStatus.externalNetworkEnabled
                || updateDeliveryActive
                || Boolean(updateStatus?.installOutcome)
              }
              onClick={onCheckForUpdates}
            >
              {updateCheckBusy ? "正在检查…" : "检查更新"}
            </button>
          </div>
        </div>
        {updateBusy ? <div className="sr-only" role="status">正在处理应用更新</div> : null}
      </section>
      <section className="settings-group">
        <header><h2>外观</h2><p>主题设置会在重启后保留。</p></header>
        <div className="setting-row">
          <div><strong>主题</strong><span>Codex 卡片仍跟随 ChatGPT 外观。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="WakeGPT 主题"
            value={theme}
            options={themeOptions}
            onChange={(nextValue) => onThemeChange(nextValue as ThemePreference)}
          />
        </div>
      </section>
      <section className="settings-group diagnostics-settings-group">
        <header>
          <h2>本地诊断</h2>
          <p>仅在本机保留受控的运行事件，不记录笔记正文、图片内容或账号凭据。</p>
        </header>
        <div className="setting-row">
          <div>
            <strong>记录本地诊断</strong>
            <span>{localDiagnosticsRecordingState} · {localDiagnosticsAvailability}。关闭不会自动清空已有事件。</span>
          </div>
          <button
            className="switch"
            type="button"
            role="switch"
            aria-label="记录本地诊断"
            aria-checked={localDiagnosticsStatus?.enabled ?? false}
            disabled={localDiagnosticsControlsDisabled}
            onClick={() => {
              if (!localDiagnosticsStatus) return;
              onLocalDiagnosticsSettingsChange({
                enabled: !localDiagnosticsStatus.enabled,
                retentionDays: localDiagnosticsStatus.retentionDays,
                maxBytes: localDiagnosticsStatus.maxBytes,
              });
            }}
          ><span /></button>
        </div>
        <div className="setting-row">
          <div><strong>保留时间</strong><span>超过保留期的事件会在本地裁剪。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="本地诊断保留时间"
            value={localDiagnosticsStatus ? String(localDiagnosticsStatus.retentionDays) : ""}
            options={localDiagnosticsRetentionOptions}
            placeholder="正在读取…"
            disabled={localDiagnosticsControlsDisabled}
            onChange={(nextValue) => {
              if (!localDiagnosticsStatus) return;
              onLocalDiagnosticsSettingsChange({
                enabled: localDiagnosticsStatus.enabled,
                retentionDays: Number(nextValue) as LocalDiagnosticsRetentionDays,
                maxBytes: localDiagnosticsStatus.maxBytes,
              });
            }}
          />
        </div>
        <div className="setting-row">
          <div><strong>存储上限</strong><span>达到上限时优先删除最早的诊断事件。</span></div>
          <SelectControl
            className="setting-select"
            ariaLabel="本地诊断存储上限"
            value={localDiagnosticsStatus ? String(localDiagnosticsStatus.maxBytes) : ""}
            options={localDiagnosticsMaxSizeOptions}
            placeholder="正在读取…"
            disabled={localDiagnosticsControlsDisabled}
            onChange={(nextValue) => {
              if (!localDiagnosticsStatus) return;
              onLocalDiagnosticsSettingsChange({
                enabled: localDiagnosticsStatus.enabled,
                retentionDays: localDiagnosticsStatus.retentionDays,
                maxBytes: Number(nextValue) as LocalDiagnosticsMaxBytes,
              });
            }}
          />
        </div>
        <dl className="settings-list settings-list--compact diagnostics-status-list">
          <div><dt>可用性</dt><dd>{localDiagnosticsAvailability}</dd></div>
          <div><dt>已保留事件</dt><dd>{localDiagnosticsStatus ? `${localDiagnosticsStatus.eventCount.toLocaleString()} 条` : "—"}</dd></div>
          <div><dt>当前占用</dt><dd>{localDiagnosticsStatus ? formatLocalDiagnosticsBytes(localDiagnosticsStatus.databaseBytes) : "—"}</dd></div>
          {localDiagnosticsStatus?.lastErrorCode ? (
            <div><dt>最近存储诊断</dt><dd><code>{localDiagnosticsStatus.lastErrorCode}</code></dd></div>
          ) : null}
        </dl>
        <div className="setting-row diagnostics-action-row">
          <div>
            <strong>诊断操作</strong>
            <span>导出只生成独立的脱敏 JSONL 与清单；不会混入常规数据导出。</span>
          </div>
          <div className="diagnostics-settings-actions">
            <button
              className="secondary-button"
              type="button"
              disabled={localDiagnosticsControlsDisabled}
              onClick={onOpenLocalDiagnostics}
            >
              <Icon name="list" size={16} />查看诊断
            </button>
            <button
              className="secondary-button"
              type="button"
              disabled={localDiagnosticsControlsDisabled}
              onClick={onExportLocalDiagnostics}
            >
              <Icon name="download" size={16} />
              {busyAction === "diagnostics-export" ? "正在导出…" : "单独导出"}
            </button>
            <button
              className="danger-button"
              type="button"
              disabled={localDiagnosticsControlsDisabled}
              onClick={onRequestClearLocalDiagnostics}
            >
              <Icon name="trash" size={16} />
              {busyAction === "diagnostics-clear" ? "正在清空…" : "清空"}
            </button>
          </div>
        </div>
        {localDiagnosticsBusy ? <div className="sr-only" role="status">正在更新本地诊断</div> : null}
      </section>
      <section className="settings-group">
        <header>
          <h2>记录与附件安全边界</h2>
          <p>到期记录只在你预览并再次确认后永久删除；图片上限是不可提高的安全边界。</p>
        </header>
        <div className="setting-row">
          <div>
            <strong>记录废纸篓保留期</strong>
            <span>现有用户升级后默认永久保留；新安装默认 30 天。到期不会后台静默删除。</span>
          </div>
          <SelectControl
            className="setting-select"
            ariaLabel="记录废纸篓保留期"
            value={recordTrashRetentionDraft}
            options={[
              { value: "7", label: "7 天" },
              { value: "30", label: "30 天" },
              { value: "90", label: "90 天" },
              { value: "365", label: "1 年" },
              { value: "permanent", label: "永久保留" },
            ]}
            disabled={!productSettings || busyAction === "product-settings"}
            onChange={setRecordTrashRetentionDraft}
          />
        </div>
        <div className="setting-row setting-row--stacked">
          <div>
            <strong>恢复扫描内置忽略目录</strong>
            <span>这些工作区根目录始终受到保护，不能从恢复扫描中移除。</span>
          </div>
          <div className="protected-directory-list" aria-label="恢复扫描内置忽略目录">
            {protectedScanIgnoreDirectories.map((directory) => (
              <code key={directory}>{directory}</code>
            ))}
          </div>
        </div>
        <div className="setting-row setting-row--stacked">
          <div>
            <strong>额外忽略目录</strong>
            <span>每行一个额外的精确工作区相对目录；仅用于找回丢失绑定，不影响已绑定文件，也不接受 glob。</span>
          </div>
          <textarea
            className="settings-textarea"
            aria-label="额外恢复扫描忽略目录"
            value={scanIgnoreDirectoriesDraft}
            rows={6}
            spellCheck={false}
            aria-invalid={!scanIgnoreDirectoriesValid}
            disabled={!productSettings || busyAction === "product-settings"}
            onChange={(event) => setScanIgnoreDirectoriesDraft(event.currentTarget.value)}
          />
        </div>
        <div className="setting-row">
          <div>
            <strong>保存设置</strong>
            <span>{scanIgnoreDirectoriesValid ? `${protectedScanIgnoreDirectories.length} 个内置目录，${parsedAdditionalScanIgnoreDirectories.length} 个额外目录` : "目录格式无效、重复或使用了内置目录"}</span>
          </div>
          <button
            className="primary-button"
            type="button"
            disabled={
              !productSettingsChanged
              || !scanIgnoreDirectoriesValid
              || busyAction === "product-settings"
            }
            onClick={() => onProductSettingsChange(
              parsedRecordTrashRetention,
              parsedScanIgnoreDirectories,
            )}
          >
            {busyAction === "product-settings" ? "正在保存…" : "保存记录设置"}
          </button>
        </div>
        <div className="setting-row">
          <div>
            <strong>清理到期记录</strong>
            <span>先生成实时预览；有未完成 Markdown、附件或 GPT 插入核对的记录会被排除。</span>
          </div>
          <button
            className="danger-button"
            type="button"
            disabled={
              !productSettings
              || productSettings.recordTrashRetentionDays === null
              || busyAction === "trash-cleanup-preview"
            }
            onClick={onPreviewRecordTrashCleanup}
          >
            {busyAction === "trash-cleanup-preview" ? "正在预览…" : "预览到期清理"}
          </button>
        </div>
        <dl className="settings-list settings-list--compact">
          <div><dt>支持格式</dt><dd>PNG、JPEG、WebP、GIF</dd></div>
          <div><dt>单张图片</dt><dd>最多 20 MiB</dd></div>
          <div><dt>单条记录</dt><dd>最多 10 张，合计最多 100 MiB</dd></div>
          <div><dt>上限语义</dt><dd>可在后续版本降低，但任何界面都不能提高原生安全边界</dd></div>
        </dl>
      </section>
      <section className="settings-group">
        <header><h2>数据与恢复</h2><p>本地数据优先保存；文件同步失败时可安全重试。</p></header>
        <div className="setting-row">
          <div>
            <strong>导出本地数据</strong>
            <span>生成一致的 SQLite 快照和已验证图片副本；不包含 ChatGPT profile、Cookie 或令牌，也不是应用恢复备份。</span>
          </div>
          <button
            className="secondary-button"
            type="button"
            disabled={busyAction === "data-export"}
            onClick={onExportLocalData}
          >
            <Icon name="download" size={16} />
            {busyAction === "data-export" ? "正在导出…" : "选择位置并导出"}
          </button>
        </div>
        <div className="setting-row">
          <div><strong>{status?.pendingRecoveryOperations ?? 0} 项等待恢复</strong><span>数据库结构 {status?.schemaVersion ?? "—"} · WakeGPT {status?.appVersion ?? "—"}</span></div>
          <button className="secondary-button" type="button" disabled={busyAction === "recovery"} onClick={onRetryRecovery}><Icon name="refresh" size={16} />{busyAction === "recovery" ? "恢复中…" : "重试同步"}</button>
        </div>
        <div className="setting-row local-data-reset-row">
          <div>
            <strong>清除 WakeGPT 本机数据</strong>
            <span>{localDataResetDescription(localDataResetPreview, localDataResetUnavailable)}</span>
          </div>
          <button
            className="danger-button"
            type="button"
            disabled={Boolean(busyAction) || !localDataResetPreview?.canReset}
            onClick={onRequestResetLocalData}
          >
            <Icon name="trash" size={16} />清除并重新开始
          </button>
        </div>
      </section>
    </div>
  );
}

export function Toast({ message, onClose }: { message: ToastMessage; onClose: () => void }) {
  return (
    <div className="toast" data-tone={message.tone} role="status">
      <span className="toast-icon"><Icon name={message.tone === "success" ? "check" : "sync"} size={16} /></span>
      <p>{message.text}</p>
      <button type="button" aria-label="关闭通知" onClick={onClose}><Icon name="x" size={15} /></button>
    </div>
  );
}
