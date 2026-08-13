(() => {
  "use strict";

  const adapterVersion = "codex-26.803.81509-v41";
  const hostContractId = "chatgpt-26.803.81509-6415";
  const hostId = "wakegpt-codex-card-v1";
  const exactLabels = {
    context: new Set(["环境信息", "Environment", "输出", "Output", "Outputs", "来源", "Source", "Sources", "输入", "Input"]),
  };
  const supportedImageTypes = new Set(["image/png", "image/jpeg", "image/webp", "image/gif"]);
  const supportedImageName = /\.(?:png|jpe?g|webp|gif)$/iu;
  const composerActionTimeoutMs = 10000;
  const pastedImageBatches = new Map();

  const composerDocumentIdentity = () => (
    `${String(location.href).slice(0, 3072)}\n${String(globalThis.performance?.timeOrigin ?? "unavailable").slice(0, 64)}`
  );

  const isSupportedImageFile = (file) => Boolean(
    file
    && Number.isSafeInteger(file.size)
    && file.size > 0
    && (
      supportedImageTypes.has(String(file.type || "").toLowerCase())
      || supportedImageName.test(String(file.name || ""))
    )
  );

  const imageFilesFromTransfer = (transfer) => {
    if (!transfer) return [];
    const files = [];
    const seen = new Set();
    const append = (file) => {
      if (!isSupportedImageFile(file) || seen.has(file)) return;
      seen.add(file);
      files.push(file);
    };
    for (const file of Array.from(transfer.files || [])) append(file);
    for (const item of Array.from(transfer.items || [])) {
      if (item?.kind === "file" && typeof item.getAsFile === "function") append(item.getAsFile());
    }
    return files;
  };

  const transferContainsFiles = (transfer) => Boolean(
    transfer
    && (
      Array.from(transfer.types || []).includes("Files")
      || Array.from(transfer.files || []).length > 0
    )
  );

  globalThis.__wakegptReadPastedImageChunk = async (
    sessionNonce,
    uploadId,
    fileIndex,
    offset,
    length,
  ) => {
    if (
      sessionNonce !== globalThis.__wakegptActiveSessionNonce
      || typeof uploadId !== "string"
      || !Number.isSafeInteger(fileIndex)
      || !Number.isSafeInteger(offset)
      || !Number.isSafeInteger(length)
      || offset < 0
      || length <= 0
      || length > 512 * 1024
    ) return null;
    const batch = pastedImageBatches.get(uploadId);
    const entry = batch?.sessionNonce === sessionNonce ? batch.files[fileIndex] : null;
    const file = entry?.file || entry;
    if (!file || offset + length > file.size) return null;
    const bytes = new Uint8Array(await file.slice(offset, offset + length).arrayBuffer());
    if (bytes.length !== length) return null;
    const alphabet = "0123456789abcdef";
    const encoded = new Array(bytes.length * 2);
    for (let index = 0; index < bytes.length; index += 1) {
      encoded[index * 2] = alphabet[bytes[index] >>> 4];
      encoded[index * 2 + 1] = alphabet[bytes[index] & 15];
    }
    return encoded.join("");
  };

  globalThis.__wakegptDiscardPastedImages = (sessionNonce, uploadId) => {
    const batch = pastedImageBatches.get(uploadId);
    if (batch?.sessionNonce !== sessionNonce) return false;
    return pastedImageBatches.delete(uploadId);
  };

  const hostComposerResolution = () => {
    const candidates = [...document.querySelectorAll("textarea,[contenteditable='true']")]
      .filter((node) => isVisible(node) && !node.closest(`#${hostId}`))
      .map((node) => ({ node, rect: node.getBoundingClientRect() }))
      .filter(({ rect }) => rect.bottom > window.innerHeight * 0.55 && rect.width > 220)
      .sort((left, right) => right.rect.bottom - left.rect.bottom || right.rect.width - left.rect.width);
    if (candidates.length === 0) return { state: "missing", node: null };
    if (candidates.length !== 1) return { state: "ambiguous", node: null };
    return { state: "ready", node: candidates[0].node };
  };

  const hostComposer = () => {
    const resolution = hostComposerResolution();
    return resolution.state === "ready" ? resolution.node : null;
  };

  const composerAppendText = (current, value) => {
    if (!current) return value;
    const trailing = current.match(/(?:\r\n|\r|\n)+$/)?.[0].match(/\r\n|\r|\n/g)?.length || 0;
    const leading = value.match(/^(?:\r\n|\r|\n)+/)?.[0].match(/\r\n|\r|\n/g)?.length || 0;
    return `${current}${"\n".repeat(Math.max(0, 2 - trailing - leading))}${value}`;
  };

  globalThis.__wakegptInsertIntoComposer = (value) => {
    if (typeof value !== "string" || !value || value.length > 1048576) return false;
    const composer = hostComposer();
    if (!composer) return false;
    composer.focus();
    if (composer instanceof HTMLTextAreaElement) {
      const current = composer.value;
      const next = composerAppendText(current, value);
      const text = next.slice(current.length);
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
      if (!setter) return false;
      setter.call(composer, next);
      composer.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
      return composer.value === next;
    }
    if (composer instanceof HTMLElement && composer.isContentEditable) {
      const selection = window.getSelection();
      if (!selection) return false;
      const current = composer.innerText || composer.textContent || "";
      const next = composerAppendText(current, value);
      const text = next.slice(current.length);
      const beforeContent = composer.textContent || "";
      const range = document.createRange();
      range.selectNodeContents(composer);
      range.collapse(false);
      selection.removeAllRanges();
      selection.addRange(range);
      const inserted = document.execCommand("insertText", false, text);
      if (!inserted) return false;
      composer.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
      const normalizeForMatch = (textValue) => textValue
        .replace(/\r\n?|\n/g, "\n")
        .replace(/\n+/g, "\n")
        .trimEnd();
      const rendered = composer.innerText || composer.textContent || "";
      return (composer.textContent || "") !== beforeContent
        && normalizeForMatch(rendered).endsWith(normalizeForMatch(value));
    }
    return false;
  };

  const hostComposerSurface = () => {
    const composer = hostComposer();
    if (!composer) return null;
    let node = composer;
    let actionSurface = null;
    for (let depth = 0; depth < 9 && node instanceof HTMLElement; depth += 1) {
      const rect = node.getBoundingClientRect();
      if (
        rect.width >= 220
        && rect.bottom > window.innerHeight * 0.55
        && node.querySelector("input[type=file]")
      ) return node;
      if (
        !actionSurface
        && rect.width >= 220
        && rect.bottom > window.innerHeight * 0.55
        && [...node.querySelectorAll("button")].some((button) => isVisible(button) && !button.disabled)
      ) actionSurface = node;
      node = node.parentElement;
    }
    return actionSurface || composer.parentElement;
  };

  const eligibleComposerImageInputs = (surface) => surface instanceof HTMLElement
    ? [...surface.querySelectorAll("input[type=file]")].filter(
      (input) => !input.disabled && (!input.accept || /image|\*/iu.test(input.accept)),
    )
    : [];

  const composerCapability = (requireImageInput = false) => {
    const composer = hostComposerResolution();
    if (composer.state !== "ready") return composer.state === "ambiguous" ? "composerAmbiguous" : "composerMissing";
    if (!requireImageInput) return "ready";
    const inputs = eligibleComposerImageInputs(hostComposerSurface());
    if (inputs.length === 1) return "ready";
    if (inputs.length > 1) return "imageInputAmbiguous";
    const rect = composer.node.getBoundingClientRect();
    return rect.width > 220 && rect.height > 0 ? "ready" : "imageInputMissing";
  };

  globalThis.__wakegptComposerCapability = composerCapability;
  globalThis.__wakegptComposerAttachmentMode = () => {
    if (composerCapability(true) !== "ready") return "unavailable";
    return eligibleComposerImageInputs(hostComposerSurface()).length === 1 ? "fileInput" : "dragDrop";
  };
  globalThis.__wakegptComposerImageInput = () => {
    if (globalThis.__wakegptComposerAttachmentMode?.() !== "fileInput") return null;
    return eligibleComposerImageInputs(hostComposerSurface())[0] || null;
  };
  globalThis.__wakegptComposerDropPoint = () => {
    if (globalThis.__wakegptComposerAttachmentMode?.() !== "dragDrop") return null;
    const composer = hostComposer();
    if (!composer) return null;
    const rect = composer.getBoundingClientRect();
    const x = rect.left + rect.width / 2;
    const y = rect.top + rect.height / 2;
    return Number.isFinite(x) && Number.isFinite(y) && x >= 0 && y >= 0 ? { x, y } : null;
  };

  globalThis.__wakegptAttachmentSignalCount = () => {
    const surface = hostComposerSurface();
    if (!(surface instanceof HTMLElement)) return -1;
    const selectors = [
      "img",
      "[data-testid*='attachment' i]",
      "[data-testid*='upload' i]",
      "[aria-label*='attachment' i]",
      "[aria-label*='image' i]",
      "[aria-label*='附件']",
      "[aria-label*='图片']",
    ];
    const signals = new Set();
    for (const node of surface.querySelectorAll(selectors.join(","))) {
      if (!(node instanceof HTMLElement) || !isVisible(node) || node.closest(`#${hostId}`)) continue;
      const rect = node.getBoundingClientRect();
      if (rect.width >= 18 && rect.height >= 18) signals.add(node);
    }
    return signals.size;
  };

  globalThis.__wakegptWaitForAttachmentSignal = (baseline) => new Promise((resolve) => {
    if (!Number.isInteger(baseline) || baseline < 0) return resolve(false);
    const current = () => Number(globalThis.__wakegptAttachmentSignalCount?.() ?? -1);
    if (current() > baseline) return resolve(true);
    let finished = false;
    const finish = (value) => {
      if (finished) return;
      finished = true;
      observer.disconnect();
      window.clearTimeout(timer);
      resolve(value);
    };
    const observer = new MutationObserver(() => {
      if (current() > baseline) finish(true);
    });
    observer.observe(document.body, { childList: true, subtree: true, attributes: true });
    const timer = window.setTimeout(() => finish(current() > baseline), 1800);
  });

  const isVisible = (element) => {
    if (!(element instanceof HTMLElement)) return false;
    const rect = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    return rect.width > 0 && rect.height > 0 && style.display !== "none" && style.visibility !== "hidden";
  };

  const menuItemIndexAfterKey = (currentIndex, itemCount, key) => {
    if (itemCount <= 0) return -1;
    if (key === "Home") return 0;
    if (key === "End") return itemCount - 1;
    if (key === "ArrowDown") return currentIndex < 0 ? 0 : (currentIndex + 1) % itemCount;
    if (key === "ArrowUp") return currentIndex < 0
      ? itemCount - 1
      : (currentIndex - 1 + itemCount) % itemCount;
    return currentIndex;
  };

  const exactTextElements = (labels) => {
    const matches = [];
    const elements = document.querySelectorAll("h1,h2,h3,h4,span,p,div");
    for (const element of elements) {
      if (element.children.length > 2 || !isVisible(element)) continue;
      if (labels.has((element.textContent || "").trim())) matches.push(element);
    }
    return matches;
  };

  const visiblePageElements = () => [...document.querySelectorAll("body *")]
    .filter((node) => !node.closest(`#${hostId}`) && isVisible(node));

  const sameCandidateGeometry = (left, right) => (
    Math.abs(left.left - right.left) <= 4
    && Math.abs(left.top - right.top) <= 4
    && Math.abs(left.right - right.right) <= 4
    && Math.abs(left.bottom - right.bottom) <= 4
  );

  const resolveContextCandidates = (candidates) => {
    const distinct = [];
    for (const candidate of candidates) {
      if (!distinct.some((existing) => sameCandidateGeometry(existing.rect, candidate.rect))) {
        distinct.push(candidate);
      }
    }
    if (distinct.length === 0) return { state: "missing", node: null };
    if (distinct.length !== 1) return { state: "ambiguous", node: null };
    return { state: "ready", node: distinct[0].node };
  };

  const contextCardResolution = (visibleElements = visiblePageElements()) => {
    const isRightRailCard = (rect) =>
      rect.width >= 240 &&
      rect.width <= 440 &&
      rect.height >= 120 &&
      rect.left >= window.innerWidth * 0.62 &&
      rect.right >= window.innerWidth - 64;
    const surfaces = visibleElements
      .map((node) => ({ node, rect: node.getBoundingClientRect(), style: getComputedStyle(node) }))
      .filter(({ rect, style }) => {
        const background = style.backgroundColor;
        return (
          isRightRailCard(rect) &&
          style.pointerEvents !== "none" &&
          parseFloat(style.borderTopLeftRadius) >= 16 &&
          background !== "transparent" &&
          background !== "rgba(0, 0, 0, 0)" &&
          style.boxShadow !== "none"
        );
      })
      .map(({ node, rect }) => ({ node, rect }));
    const surfaceResolution = resolveContextCandidates(surfaces);
    if (surfaceResolution.state !== "missing") return surfaceResolution;

    const candidates = [];
    for (const label of exactTextElements(exactLabels.context)) {
      let ancestor = label.parentElement;
      for (let depth = 0; depth < 8 && ancestor; depth += 1) {
        const rect = ancestor.getBoundingClientRect();
        if (isRightRailCard(rect) && getComputedStyle(ancestor).pointerEvents !== "none") {
          candidates.push({ node: ancestor, rect });
        }
        ancestor = ancestor.parentElement;
      }
    }
    return resolveContextCandidates(candidates);
  };

  const pageBlockReason = (visibleElements = visiblePageElements()) => {
    const outsideWakeGPT = (element) =>
      element instanceof HTMLElement && !element.closest(`#${hostId}`);
    const visibleOutsideWakeGPT = (element) => outsideWakeGPT(element) && isVisible(element);
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    const coversViewport = (element) => {
      if (!visibleOutsideWakeGPT(element)) return false;
      const rect = element.getBoundingClientRect();
      return (
        rect.left <= viewportWidth * 0.08
        && rect.top <= viewportHeight * 0.08
        && rect.right >= viewportWidth * 0.92
        && rect.bottom >= viewportHeight * 0.92
      );
    };

    if (visibleOutsideWakeGPT(document.fullscreenElement)) return "fullscreen";
    try {
      if (visibleOutsideWakeGPT(document.querySelector(":modal"))) return "modal";
    } catch {
      // Older supported WebViews may not implement the :modal selector.
    }

    const belongsToFixedViewportLayer = (element) => {
      let ancestor = element;
      for (let depth = 0; depth < 10 && ancestor; depth += 1) {
        if (getComputedStyle(ancestor).position === "fixed" && coversViewport(ancestor)) {
          return true;
        }
        ancestor = ancestor.parentElement;
      }
      return false;
    };
    for (const surface of visibleElements) {
      if (!coversViewport(surface)) continue;
      const style = getComputedStyle(surface);
      const background = style.backgroundColor;
      if (
        style.pointerEvents !== "none"
        && parseFloat(style.borderTopLeftRadius) >= 16
        && background !== "transparent"
        && background !== "rgba(0, 0, 0, 0)"
        && style.boxShadow !== "none"
        && belongsToFixedViewportLayer(surface)
      ) return "viewportOverlay";
    }

    for (const dialog of document.querySelectorAll("[aria-modal='true'],[role='dialog']")) {
      if (!visibleOutsideWakeGPT(dialog)) continue;
      if (dialog.getAttribute("aria-modal") === "true") return "modal";
      const rect = dialog.getBoundingClientRect();
      const style = getComputedStyle(dialog);
      if (
        style.position === "fixed"
        && rect.width >= viewportWidth * 0.58
        && rect.height >= viewportHeight * 0.58
      ) return "modal";
    }

    const minimumMediaArea = viewportWidth * viewportHeight * 0.015;
    for (const media of document.querySelectorAll("img,video,canvas")) {
      if (!visibleOutsideWakeGPT(media)) continue;
      const mediaRect = media.getBoundingClientRect();
      if (
        mediaRect.width < 180
        || mediaRect.height < 140
        || mediaRect.width * mediaRect.height < minimumMediaArea
      ) continue;
      let ancestor = media.parentElement;
      for (let depth = 0; depth < 12 && ancestor; depth += 1) {
        const style = getComputedStyle(ancestor);
        if (style.position === "fixed" && coversViewport(ancestor)) return "mediaLightbox";
        ancestor = ancestor.parentElement;
      }
    }
    return null;
  };

  const pageBlocksCard = (visibleElements = visiblePageElements()) => (
    pageBlockReason(visibleElements) !== null
  );

  const collapsedCardMetrics = (contextWidth, fontSize) => ({
    width: Math.min(
      Math.round(contextWidth),
      Math.max(132, Math.round(contextWidth * 0.48), Math.ceil(fontSize * 7.25)),
    ),
    height: Math.max(47, Math.ceil(fontSize * 2.5) + 2),
  });

  const element = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };

  const button = (className, text, action) => {
    const node = element("button", className, text);
    node.type = "button";
    node.dataset.action = action;
    return node;
  };

  // Lucide 1.26.0 icon nodes, ISC licensed; see NOTICE.md.
  const lucideNodes = {
    maximize: [
      ["path", { d: "M15 3h6v6" }],
      ["path", { d: "m21 3-7 7" }],
      ["path", { d: "m3 21 7-7" }],
      ["path", { d: "M9 21H3v-6" }],
    ],
    minimize: [
      ["path", { d: "m14 10 7-7" }],
      ["path", { d: "M20 10h-6V4" }],
      ["path", { d: "m3 21 7-7" }],
      ["path", { d: "M4 14h6v6" }],
    ],
    x: [
      ["path", { d: "M18 6 6 18" }],
      ["path", { d: "m6 6 12 12" }],
    ],
    check: [
      ["path", { d: "M20 6 9 17l-5-5" }],
    ],
    refresh: [
      ["path", { d: "M21 12a9 9 0 0 0-15-6.7L3 8" }],
      ["path", { d: "M3 3v5h5" }],
      ["path", { d: "M3 12a9 9 0 0 0 15 6.7l3-2.7" }],
      ["path", { d: "M21 21v-5h-5" }],
    ],
    chevronDown: [
      ["path", { d: "m6 9 6 6 6-6" }],
    ],
    plus: [
      ["path", { d: "M5 12h14" }],
      ["path", { d: "M12 5v14" }],
    ],
    image: [
      ["rect", { width: "18", height: "18", x: "3", y: "3", rx: "2", ry: "2" }],
      ["circle", { cx: "9", cy: "9", r: "2" }],
      ["path", { d: "m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" }],
    ],
    link: [
      ["path", { d: "M9 17H7A5 5 0 0 1 7 7h2" }],
      ["path", { d: "M15 7h2a5 5 0 1 1 0 10h-2" }],
      ["line", { x1: "8", x2: "16", y1: "12", y2: "12" }],
    ],
    more: [
      ["circle", { cx: "12", cy: "12", r: "1" }],
      ["circle", { cx: "19", cy: "12", r: "1" }],
      ["circle", { cx: "5", cy: "12", r: "1" }],
    ],
    external: [
      ["path", { d: "M15 3h6v6" }],
      ["path", { d: "M10 14 21 3" }],
      ["path", { d: "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" }],
    ],
  };

  const lucideIcon = (name, size = 16) => {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.classList.add("lucide");
    svg.setAttribute("width", String(size));
    svg.setAttribute("height", String(size));
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.8");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    for (const [tag, attributes] of lucideNodes[name] || []) {
      const child = document.createElementNS("http://www.w3.org/2000/svg", tag);
      for (const [attribute, value] of Object.entries(attributes)) child.setAttribute(attribute, value);
      svg.append(child);
    }
    return svg;
  };

  const wakeBrandMark = () => {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 28 28");
    svg.setAttribute("aria-hidden", "true");
    const shape = (tag, attributes) => {
      const node = document.createElementNS("http://www.w3.org/2000/svg", tag);
      for (const [attribute, value] of Object.entries(attributes)) node.setAttribute(attribute, value);
      return node;
    };
    svg.append(
      shape("rect", { x: "2", y: "2", width: "24", height: "24", rx: "5", fill: "#13251f" }),
      shape("rect", {
        x: "11", y: "6", width: "11", height: "15", rx: "2.2", fill: "#ffc23a",
        transform: "rotate(7 16.5 13.5)",
      }),
      shape("path", {
        d: "M7 9h9l3 3v9.5A2.5 2.5 0 0 1 16.5 24h-9A2.5 2.5 0 0 1 5 21.5v-10A2.5 2.5 0 0 1 7.5 9Z",
        fill: "#25bfd2",
      }),
      shape("path", { d: "M16 9v1.6c0 .8.6 1.4 1.4 1.4H19Z", fill: "#ffe19a" }),
      shape("path", {
        d: "M8.2 15.8h6.7M8.2 19h4.5", fill: "none", stroke: "#10322e",
        "stroke-linecap": "round", "stroke-width": "1.6",
      }),
    );
    return svg;
  };

  const setIconButton = (node, iconName, label) => {
    if (node.dataset.iconName !== iconName) {
      node.replaceChildren(lucideIcon(iconName));
      node.dataset.iconName = iconName;
    }
    node.setAttribute("aria-label", label);
    node.title = label;
  };

  const setLabeledButton = (node, iconName, label) => {
    node.replaceChildren(lucideIcon(iconName, 14), element("span", "", label));
  };

  const composerInsertionNeedsReview = (state) => (
    state === "uncertain" || state === "staleUncertain"
  );

  const composerActionExpired = (startedAt, now) => (
    Number.isFinite(startedAt)
    && Number.isFinite(now)
    && now >= startedAt
    && now - startedAt >= composerActionTimeoutMs
  );

  const normalizeState = (value) => {
    const state = value && typeof value === "object" ? value : {};
    return {
      schemaVersion: 1,
      workspaces: Array.isArray(state.workspaces) ? state.workspaces.slice(0, 24) : [],
      selectedWorkspaceId: typeof state.selectedWorkspaceId === "string" ? state.selectedWorkspaceId : null,
      workspaceName: typeof state.workspaceName === "string" ? state.workspaceName.slice(0, 120) : "WakeGPT",
      selectedNotebookId: typeof state.selectedNotebookId === "string" ? state.selectedNotebookId : null,
      notebooks: Array.isArray(state.notebooks)
        ? state.notebooks.slice(0, 48).map((notebook) => ({
            ...notebook,
            isPinned: Boolean(notebook?.isPinned),
            numberingSyncPending: Boolean(notebook?.numberingSyncPending),
            attachmentDirectorySyncPending: Boolean(notebook?.attachmentDirectorySyncPending),
            targetState: typeof notebook?.targetState === "string" ? notebook.targetState : "unverified",
          }))
        : [],
      recentRecords: Array.isArray(state.recentRecords)
        ? state.recentRecords.slice(0, 12).map((record) => ({
            ...record,
            attachments: Array.isArray(record?.attachments)
              ? record.attachments.slice(0, 10).filter((attachment) => (
                  attachment
                  && typeof attachment.id === "string"
                  && typeof attachment.mediaType === "string"
                  && Number.isSafeInteger(attachment.byteSize)
                )).map((attachment) => ({
                  ...attachment,
                  available: attachment.available !== false,
                }))
              : [],
            isPinned: Boolean(record?.isPinned),
            attachmentsReady: record?.attachmentsReady !== false,
            insertionState: ["partial", "uncertain", "staleUncertain", "complete"].includes(record?.insertionState)
              ? record.insertionState
              : "none",
          }))
        : [],
      pendingImageCount: Number.isInteger(state.pendingImageCount) ? Math.max(0, Math.min(10, state.pendingImageCount)) : 0,
      pendingImages: Array.isArray(state.pendingImages)
        ? state.pendingImages.slice(0, 10).filter((attachment) => (
            attachment
            && typeof attachment.token === "string"
            && typeof attachment.mediaType === "string"
            && Number.isSafeInteger(attachment.byteSize)
          ))
        : [],
      draftBodyMarkdown: typeof state.draftBodyMarkdown === "string"
        ? state.draftBodyMarkdown.slice(0, 1048576)
        : "",
      selectionKey: typeof state.selectionKey === "string" ? state.selectionKey.slice(0, 300) : "none:inbox",
      submitShortcut: state.submitShortcut === "commandEnter" ? "commandEnter" : "enter",
      paused: Boolean(state.paused),
    };
  };

  const notebookTabOptions = (notebooks) => [
    { value: "", label: "收件箱（仅本地）" },
    ...(Array.isArray(notebooks) ? notebooks : [])
      .filter((notebook) => (
        notebook?.isPinned === true
        && typeof notebook.id === "string"
        && typeof notebook.displayName === "string"
      ))
      .map((notebook) => ({
        value: notebook.id,
        label: notebook.displayName,
      })),
  ];

  const workspaceSelectOptions = (workspaces) => (Array.isArray(workspaces) ? workspaces : [])
    .filter((workspace) => (
      workspace
      && typeof workspace.id === "string"
      && typeof workspace.displayName === "string"
    ))
    .map((workspace) => ({
      value: workspace.id,
      label: workspace.displayName,
    }));

  const workspaceStateSignature = (workspaces) => JSON.stringify(workspaceSelectOptions(workspaces));

  const notebookStateSignature = (notebooks) => JSON.stringify(
    (Array.isArray(notebooks) ? notebooks : []).map((notebook) => [
      typeof notebook?.id === "string" ? notebook.id : null,
      typeof notebook?.displayName === "string" ? notebook.displayName : null,
      typeof notebook?.targetState === "string" ? notebook.targetState : null,
      typeof notebook?.lastErrorCode === "string" ? notebook.lastErrorCode : null,
      Number.isSafeInteger(notebook?.numberingStart) ? notebook.numberingStart : null,
      Boolean(notebook?.numberingSyncPending),
      typeof notebook?.attachmentDirectory === "string" ? notebook.attachmentDirectory : null,
      Boolean(notebook?.attachmentDirectorySyncPending),
      Boolean(notebook?.isPinned),
    ]),
  );

  const captureInlineEdit = (recent, selectionKey) => {
    const editor = recent?.querySelector?.(".inline-edit");
    const item = editor?.closest?.(".item");
    const textarea = editor?.querySelector?.("textarea");
    const save = editor?.querySelector?.('[data-action="saveEdit"]');
    const recordId = item?.dataset?.recordId;
    const expectedRevision = save?.dataset?.revision;
    const mutationId = editor?.dataset?.mutationId;
    let retainedAttachmentIds;
    if (typeof editor?.dataset?.retainedAttachmentIds === "string") {
      try {
        const parsed = JSON.parse(editor.dataset.retainedAttachmentIds);
        if (Array.isArray(parsed) && parsed.every((id) => typeof id === "string")) {
          retainedAttachmentIds = parsed;
        } else {
          return null;
        }
      } catch {
        return null;
      }
    }
    if (
      typeof selectionKey !== "string"
      || typeof recordId !== "string"
      || typeof textarea?.value !== "string"
      || typeof expectedRevision !== "string"
      || typeof mutationId !== "string"
    ) return null;
    return {
      selectionKey,
      recordId,
      bodyMarkdown: textarea.value,
      selectionStart: Number.isInteger(textarea.selectionStart) ? textarea.selectionStart : textarea.value.length,
      selectionEnd: Number.isInteger(textarea.selectionEnd) ? textarea.selectionEnd : textarea.value.length,
      expectedRevision,
      mutationId,
      ...(retainedAttachmentIds ? { retainedAttachmentIds } : {}),
    };
  };

  const applyInlineEditSnapshot = (textarea, save, snapshot) => {
    if (!textarea || !save || !snapshot) return;
    textarea.value = snapshot.bodyMarkdown;
    save.dataset.revision = snapshot.expectedRevision;
    const end = textarea.value.length;
    const selectionStart = Math.max(0, Math.min(snapshot.selectionStart, end));
    const selectionEnd = Math.max(selectionStart, Math.min(snapshot.selectionEnd, end));
    textarea.focus();
    textarea.setSelectionRange(selectionStart, selectionEnd);
  };

  const canRestoreInlineEdit = (snapshot, state) => Boolean(
    snapshot
    && snapshot.selectionKey === state?.selectionKey
    && Array.isArray(state?.recentRecords)
    && state.recentRecords.some((record) => (
      record?.id === snapshot.recordId
      && Number(record.revision) === Number(snapshot.expectedRevision)
    ))
    && Array.isArray(state?.notebooks)
    && !notebookBlocksChanges(
      state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId),
    )
  );

  const notebookBlocksChanges = (notebook) => Boolean(
    notebook && (
      notebook.targetState !== "ready"
      || notebook.numberingSyncPending
      || notebook.attachmentDirectorySyncPending
    ),
  );

  const mount = (bootstrap) => {
    if (!bootstrap || bootstrap.schemaVersion !== 1 || typeof bootstrap.sessionNonce !== "string") {
      return { ok: false, code: "bootstrap_invalid" };
    }
    if (!/^[a-f0-9]{32}$/.test(bootstrap.sessionNonce)) {
      return { ok: false, code: "session_nonce_invalid" };
    }
    if (!/^[A-Za-z_$][A-Za-z0-9_$]{0,80}$/.test(bootstrap.bindingName || "")) {
      return { ok: false, code: "binding_name_invalid" };
    }
    if (bootstrap.hostContractId !== hostContractId) {
      return { ok: false, code: "host_contract_mismatch" };
    }
    if (typeof globalThis[bootstrap.bindingName] !== "function") {
      return { ok: false, code: "binding_missing" };
    }
    if (
      !document.documentElement
      || !document.body
      || typeof MutationObserver !== "function"
      || typeof ResizeObserver !== "function"
      || typeof requestAnimationFrame !== "function"
      || typeof globalThis.crypto?.randomUUID !== "function"
    ) {
      return { ok: false, code: "host_capability_unavailable" };
    }
    if (contextCardResolution().state === "ambiguous") {
      return { ok: false, code: "context_selector_ambiguous" };
    }

    globalThis.__wakegptActiveSessionNonce = bootstrap.sessionNonce;
    const previous = document.getElementById(hostId);
    if (previous) previous.remove();

    const host = element("section");
    host.id = hostId;
    host.dataset.adapterVersion = adapterVersion;
    host.dataset.hostContractId = hostContractId;
    host.dataset.sessionNonce = bootstrap.sessionNonce;
    host.setAttribute("aria-label", "WakeGPT 速记");
    const shadow = host.attachShadow({ mode: "open" });
    const style = element("style");
    style.textContent = `
      :host { all: initial; display:block; min-width:240px; box-sizing:border-box; color-scheme:light; --wake-text:#202123; --wake-muted:#6f7076; --wake-surface:#fff; --wake-input:#fafafa; --wake-subtle:#f7f7f8; --wake-border:rgba(0,0,0,.14); --wake-hover:rgba(0,0,0,.05); --wake-type-title:.875rem; --wake-type-body:.8125rem; --wake-type-control:.8125rem; --wake-type-label:.75rem; --wake-type-meta:.6875rem; --wake-type-caption:.625rem; --wake-leading-compact:1.35; --wake-leading-body:1.45; font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text","Segoe UI",sans-serif; font-size:var(--wake-type-control); line-height:var(--wake-leading-body); font-weight:400; -webkit-app-region:no-drag; }
      :host([hidden]) { display:none !important; }
      * { box-sizing: border-box; }
      .card { display:flex; flex-direction:column; width:100%; height:100%; max-height:100%; overflow:hidden; color:var(--wake-text,#202123); background:var(--wake-surface,#fff); border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:16px; box-shadow:0 10px 30px rgba(0,0,0,.08),0 1px 3px rgba(0,0,0,.05); font:inherit; -webkit-app-region:no-drag; }
      header { display:flex; align-items:center; gap:1px; min-height:max(45px,2.5em); padding:5px 8px 5px 12px; border-bottom:1px solid var(--wake-border,rgba(0,0,0,.1)); }
      .card-scroll { flex:1; min-height:0; overflow-y:auto; overscroll-behavior:contain; scrollbar-gutter:stable; }
      .card-scroll::-webkit-scrollbar { width:8px; }
      .card-scroll::-webkit-scrollbar-track { background:transparent; }
      .card-scroll::-webkit-scrollbar-thumb { background:var(--wake-border,rgba(0,0,0,.14)); background-clip:padding-box; border:2px solid transparent; border-radius:999px; }
      .mark { display:grid; width:max(22px,1.7em); height:max(22px,1.7em); margin-right:8px; padding:2px; place-items:center; background:linear-gradient(45deg,#25bfd2 0%,#b9d8bd 48%,#ffc23a 100%); border-radius:6px; }
      .mark > svg { display:block; width:100%; height:100%; }
      header strong { flex:1; font-size:var(--wake-type-title); font-weight:650; }
      .lucide { display:block; flex:0 0 auto; pointer-events:none; }
      textarea, button, input { font:inherit; }
      input { min-height:28px; padding:3px 8px; color:inherit; background:transparent; border:1px solid var(--wake-border,rgba(0,0,0,.14)); border-radius:7px; }
      .selectors { display:grid; grid-template-columns:minmax(0,1fr) minmax(0,1fr) auto; gap:7px; padding:8px 11px; border-bottom:1px solid var(--wake-border,rgba(0,0,0,.1)); }
      .selectors .wake-select { width:100%; min-width:0; }
      .selectors .close-tab { width:32px; padding:0; }
      .wake-select { display:flex; min-width:0; color:inherit; }
      .wake-select-trigger { display:flex; width:100%; min-width:0; min-height:28px; align-items:center; justify-content:space-between; gap:6px; padding:3px 7px 3px 8px; color:inherit; background:transparent; border:1px solid var(--wake-border,rgba(0,0,0,.14)); border-radius:7px; text-align:left; }
      .wake-select-trigger > span { min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
      .wake-select-trigger > .lucide { color:var(--wake-muted,#77787c); transition:transform 120ms ease; }
      .wake-select[data-open="true"] .wake-select-trigger > .lucide { transform:rotate(180deg); }
      .wake-select-popover { position:fixed; z-index:2147483647; max-height:min(240px,calc(100vh - 16px)); padding:5px; overflow:auto; color:var(--wake-text,#202123); background:var(--wake-surface,#fff); border:1px solid var(--wake-border,rgba(0,0,0,.14)); border-radius:9px; box-shadow:0 10px 28px rgba(0,0,0,.16),0 1px 2px rgba(0,0,0,.08); }
      .wake-select-popover[hidden] { display:none; }
      .wake-select-option { display:grid; grid-template-columns:minmax(0,1fr) 16px; min-height:31px; align-items:center; gap:7px; padding:6px 8px; border-radius:6px; cursor:default; font-size:var(--wake-type-control); line-height:var(--wake-leading-compact); }
      .wake-select-option > span { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
      .wake-select-option > .lucide { color:#5965e9; opacity:0; }
      .wake-select-option[aria-selected="true"] > .lucide { opacity:1; }
      .wake-select-option[data-active="true"] { background:var(--wake-hover,rgba(0,0,0,.05)); }
      .wake-select-option[data-disabled="true"] { color:var(--wake-muted,#77787c); opacity:.48; }
      .target-hint { display:-webkit-box; max-height:calc(2.7em + 15px); padding:7px 11px; overflow:hidden; color:var(--wake-muted,#77787c); background:var(--wake-input,rgba(0,0,0,.025)); border-bottom:1px solid var(--wake-border,rgba(0,0,0,.1)); font-size:var(--wake-type-meta); line-height:var(--wake-leading-compact); overflow-wrap:anywhere; -webkit-box-orient:vertical; -webkit-line-clamp:2; }
      .target-hint[data-tone="local"] { color:#9a6818; background:rgba(201,139,39,.08); }
      .target-hint[data-tone="ready"] { color:#39754b; background:rgba(57,117,75,.07); }
      .create-panel { display:grid; grid-template-columns:minmax(0,1fr) 94px 72px; gap:7px; padding:9px 11px; background:var(--wake-input,rgba(0,0,0,.025)); border-bottom:1px solid var(--wake-border,rgba(0,0,0,.1)); }
      .create-panel[hidden] { display:none; }
      .create-panel input { min-width:0; padding-right:8px; }
      .create-panel .attachment-directory-input { grid-column:1/-1; }
      .create-panel .panel-actions { display:flex; grid-column:1/-1; gap:7px; }
      .create-panel .panel-actions button:first-child { color:#fff; background:#5965e9; border-color:#5965e9; }
      .composer { position:relative; padding:10px 11px; border-bottom:1px solid var(--wake-border,rgba(0,0,0,.1)); }
      .image-drop-overlay { position:absolute; z-index:4; inset:6px; display:grid; place-content:center; justify-items:center; gap:4px; padding:12px; color:var(--wake-text,#202123); background:color-mix(in srgb,var(--wake-surface,#fff) 94%,transparent); border:1px dashed rgba(89,101,233,.62); border-radius:9px; backdrop-filter:blur(3px); pointer-events:none; text-align:center; }
      .image-drop-overlay[hidden] { display:none; }
      .image-drop-overlay > .lucide { color:#5965e9; }
      .image-drop-overlay strong { font-size:var(--wake-type-control); }
      .image-drop-overlay span { color:var(--wake-muted,#77787c); font-size:var(--wake-type-caption); }
      textarea { display:block; width:100%; min-height:max(72px,5.2em); max-height:max(150px,10em); padding:9px 10px; resize:vertical; color:inherit; background:var(--wake-input,rgba(0,0,0,.025)); border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:9px; outline:none; }
      textarea:focus { border-color:#8b94ec; box-shadow:0 0 0 3px rgba(89,101,233,.12); }
      .composer-actions { display:flex; align-items:center; gap:6px; margin-top:8px; }
      button { min-height:28px; padding:5px 9px; color:inherit; background:transparent; border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:7px; cursor:pointer; -webkit-app-region:no-drag; touch-action:manipulation; }
      button:hover { background:var(--wake-hover,rgba(0,0,0,.05)); }
      button:focus-visible, input:focus-visible, textarea:focus-visible { outline:2px solid #7d86e9; outline-offset:2px; }
      button:disabled { cursor:default; opacity:.45; }
      .icon { position:relative; display:inline-grid; flex:0 0 34px; width:34px; height:34px; min-height:34px; padding:0; place-items:center; line-height:1; background:transparent; border-color:transparent; }
      .icon::before { content:""; position:absolute; inset:4px; pointer-events:none; border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:7px; background:transparent; }
      .icon:hover { background:transparent; }
      .icon:hover::before { background:var(--wake-hover,rgba(0,0,0,.05)); }
      .record { margin-left:auto; color:#fff; background:#5965e9; border-color:#5965e9; font-weight:600; }
      .record:hover:not(:disabled) { color:#fff; background:#4b57da; border-color:#4b57da; }
      .status { min-height:18px; margin-top:6px; color:var(--wake-muted,#77787c); font-size:var(--wake-type-meta); }
      .pending-images,.item-images { display:flex; flex-wrap:wrap; gap:6px; margin-top:7px; }
      .image-tile { display:grid; width:70px; min-width:0; gap:3px; }
      .image-preview { position:relative; display:grid; width:70px; height:54px; min-height:54px; padding:0; place-items:center; overflow:hidden; color:var(--wake-muted,#77787c); background:var(--wake-input,rgba(0,0,0,.025)); border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:7px; cursor:zoom-in; }
      .image-preview:hover:not(:disabled) { background:var(--wake-hover,rgba(0,0,0,.05)); border-color:rgba(89,101,233,.42); }
      .image-preview[data-state="loading"],.image-preview[data-state="error"] { cursor:default; }
      .image-preview[data-state="error"]:not(:disabled) { cursor:pointer; }
      .image-preview img { display:block; width:100%; height:100%; object-fit:contain; }
      .image-preview > span { position:absolute; right:4px; bottom:3px; padding:1px 4px; color:#fff; background:rgba(18,19,22,.62); border-radius:3px; font-size:var(--wake-type-caption); }
      .image-caption { overflow:hidden; color:var(--wake-muted,#77787c); font-size:var(--wake-type-caption); line-height:var(--wake-leading-compact); text-overflow:ellipsis; white-space:nowrap; }
      .image-lightbox { position:fixed; z-index:2147483647; inset:0; display:grid; place-items:center; padding:50px 24px 28px; overflow:auto; color:#fff; background:rgba(12,13,16,.84); backdrop-filter:blur(9px); }
      .image-lightbox[hidden] { display:none; }
      .image-lightbox figure { display:grid; max-width:min(94vw,1440px); max-height:calc(100vh - 82px); gap:8px; margin:auto; place-items:center; }
      .image-lightbox img { display:block; max-width:100%; max-height:calc(100vh - 116px); object-fit:contain; border-radius:8px; box-shadow:0 20px 70px rgba(0,0,0,.4); }
      .image-lightbox figcaption { max-width:min(80vw,720px); overflow:hidden; color:rgba(255,255,255,.78); font-size:var(--wake-type-label); text-overflow:ellipsis; white-space:nowrap; }
      .image-lightbox-close { position:fixed; top:14px; right:16px; display:grid; width:35px; height:35px; min-height:35px; padding:0; place-items:center; color:#fff; background:rgba(255,255,255,.12); border-color:rgba(255,255,255,.2); border-radius:999px; }
      .image-lightbox-close:hover { color:#fff; background:rgba(255,255,255,.2); }
      .recent-heading { display:flex; align-items:center; justify-content:space-between; padding:9px 12px 5px; color:var(--wake-muted,#77787c); font-size:var(--wake-type-label); }
      .recent { min-height:70px; padding:0 8px 8px; }
      .empty { padding:18px 8px; color:var(--wake-muted,#77787c); text-align:center; font-size:var(--wake-type-label); }
      .item { position:relative; padding:8px 4px; border-bottom:1px solid var(--wake-border,rgba(0,0,0,.08)); }
      .item[data-pinned="true"] { padding-left:8px; border-left:2px solid #7d86e9; }
      .item:last-child { border-bottom:0; }
      .item-body { display:-webkit-box; overflow:hidden; color:inherit; font-size:var(--wake-type-body); overflow-wrap:anywhere; -webkit-line-clamp:2; -webkit-box-orient:vertical; }
      .item-meta { display:flex; align-items:center; gap:6px; margin-top:5px; color:var(--wake-muted,#77787c); font-size:var(--wake-type-meta); }
      .item-actions { display:flex; gap:4px; margin-left:auto; }
      .item-meta button { min-height:24px; padding:3px 7px; font-size:var(--wake-type-meta); }
      .item-menu { position:fixed; z-index:2147483646; display:grid; min-width:132px; padding:5px; color:var(--wake-text,#202123); background:var(--wake-surface,#fff); border:1px solid var(--wake-border,rgba(0,0,0,.12)); border-radius:9px; box-shadow:0 10px 28px rgba(0,0,0,.16),0 1px 2px rgba(0,0,0,.08); font-size:var(--wake-type-body); line-height:var(--wake-leading-body); }
      .item-menu[hidden] { display:none; }
      .item-menu button { width:100%; min-height:30px; border:0; text-align:left; }
      .item-menu button[data-action="trashRecord"] { color:#b44242; }
      .target-panel { display:grid; grid-template-columns:minmax(0,1fr) auto; gap:6px; margin-top:7px; padding:7px; background:var(--wake-input,rgba(0,0,0,.025)); border:1px solid var(--wake-border,rgba(0,0,0,.1)); border-radius:8px; }
      .target-panel .wake-select { min-width:0; width:100%; }
      .target-panel-actions { display:flex; grid-column:1/-1; justify-content:flex-end; gap:5px; }
      .composer-insertion-review { margin-top:7px; padding:8px; background:rgba(201,139,39,.08); border:1px solid rgba(201,139,39,.24); border-radius:8px; }
      .composer-insertion-review strong { display:block; color:var(--wake-text,#202123); font-size:var(--wake-type-label); }
      .composer-insertion-review p { margin:3px 0 0; color:var(--wake-muted,#77787c); font-size:var(--wake-type-meta); line-height:var(--wake-leading-body); }
      .composer-insertion-review-actions { display:flex; flex-wrap:wrap; justify-content:flex-end; gap:5px; margin-top:7px; }
      .composer-insertion-review-actions button { display:inline-flex; align-items:center; justify-content:center; gap:4px; }
      .composer-insertion-review-actions .record { margin-left:0; }
      .pin-mark { color:#5965e9; font-weight:650; }
      .item-meta button[data-confirmed="true"] { color:#b44242; background:rgba(180,66,66,.08); border-color:rgba(180,66,66,.28); }
      .item[data-editing="true"] .item-meta { display:none; }
      .inline-edit { position:relative; }
      .inline-edit textarea { min-height:66px; margin-top:2px; font-size:var(--wake-type-body); }
      .inline-edit-toolbar { display:flex; align-items:center; justify-content:space-between; gap:7px; margin-top:6px; }
      .inline-edit-toolbar button { display:inline-flex; align-items:center; gap:4px; }
      .inline-edit-toolbar span { color:var(--wake-muted); font-size:var(--wake-type-meta); }
      .inline-edit-drop-overlay { inset:0; }
      .inline-edit-images { display:flex; flex-wrap:wrap; gap:5px; margin-top:6px; }
      .inline-edit-image { position:relative; width:68px; padding:3px; background:var(--wake-subtle); border:1px solid var(--wake-border); border-radius:7px; }
      .inline-edit-image[data-removed="true"] { opacity:.48; }
      .inline-edit-image .image-preview { height:47px; }
      .inline-edit-image > button:last-child { position:absolute; top:5px; right:5px; display:grid; width:20px; height:20px; padding:0; place-items:center; color:#fff; background:rgba(16,17,20,.68); border:0; border-radius:999px; }
      .inline-edit-image-state { position:absolute; left:5px; bottom:5px; padding:1px 4px; color:#fff; background:rgba(16,17,20,.72); border-radius:3px; font-size:var(--wake-type-caption); }
      .inline-edit-hint { margin-top:5px; color:var(--wake-muted); font-size:var(--wake-type-meta); }
      .inline-edit-error { margin-top:5px; color:#b44242; font-size:var(--wake-type-meta); }
      .inline-edit-error[hidden] { display:none; }
      .inline-edit-actions { display:flex; justify-content:flex-end; gap:5px; margin-top:6px; }
      .inline-edit[aria-busy="true"] { opacity:.72; }
      :host([data-layout="compact"]) .composer { padding:8px 11px; border-bottom:0; }
      :host([data-layout="compact"]) .composer textarea { min-height:max(50px,5.2em); max-height:max(76px,7em); resize:none; }
      :host([data-layout="compact"]) .recent-heading { padding-top:6px; }
      :host([data-layout="compact"]) .recent { min-height:0; }
      :host([data-layout="compact"]) .status:empty { display:none; }
      :host([data-layout="collapsed"]) { min-width:0; }
      :host([data-layout="collapsed"]) .card { height:max(45px,2.5em); }
      :host([data-layout="collapsed"]) .card > :not(header) { display:none; }
      :host([data-layout="collapsed"]) header { min-height:max(45px,2.5em); padding:5px 5px 5px 9px; border-bottom:0; }
      :host([data-layout="collapsed"]) .app-launcher { display:none; }
      :host([data-layout="collapsed"]) .notebook-menu { display:none; }
      :host([data-layout="drawer"]) .card { height:100%; }
      @media (prefers-color-scheme:dark) { :host { color-scheme:dark; --wake-text:#ececef; --wake-muted:#a5a6aa; --wake-surface:#202124; --wake-input:#27282b; --wake-subtle:#242529; --wake-border:rgba(255,255,255,.14); --wake-hover:rgba(255,255,255,.07); } }
      @media (prefers-contrast:more) { :host { --wake-muted:#505157; --wake-border:rgba(0,0,0,.32); --wake-hover:rgba(0,0,0,.1); } button:focus-visible,input:focus-visible,textarea:focus-visible { outline-width:3px; } }
      @media (prefers-color-scheme:dark) and (prefers-contrast:more) { :host { --wake-muted:#d1d2d6; --wake-border:rgba(255,255,255,.34); --wake-hover:rgba(255,255,255,.13); } }
      @media (prefers-reduced-motion:reduce) { *,*::before,*::after { scroll-behavior:auto !important; transition-duration:0.01ms !important; animation-duration:0.01ms !important; animation-iteration-count:1 !important; } }
      @media (forced-colors:active) { .wake-select-trigger,.wake-select-popover,.image-drop-overlay { color:CanvasText; background:Canvas; border-color:ButtonText; } .wake-select-option[data-active="true"] { color:HighlightText; background:Highlight; forced-color-adjust:none; } .wake-select-option > .lucide { color:currentColor; } .inline-edit-image-state { color:HighlightText; background:Highlight; forced-color-adjust:none; } }
    `;

    const selectPopover = element("div", "wake-select-popover");
    selectPopover.id = `${hostId}-select-listbox`;
    selectPopover.hidden = true;
    selectPopover.setAttribute("role", "listbox");
    let selectSequence = 0;
    let openSelectControl = null;
    let selectActiveIndex = -1;
    let selectTypeaheadText = "";
    let selectTypeaheadAt = 0;

    const updateSelectTrigger = (control) => {
      const selected = control.options.find((option) => option.value === control.value);
      const label = selected?.label || control.placeholder;
      control.valueNode.textContent = label;
      control.trigger.title = label;
      control.trigger.disabled = control.disabled || !control.options.some((option) => !option.disabled);
    };

    const createWakeSelect = (className, ariaLabel, onChange, minWidth = 168) => {
      const root = element("div", `wake-select ${className || ""}`.trim());
      const trigger = element("button", "wake-select-trigger");
      trigger.type = "button";
      trigger.setAttribute("role", "combobox");
      trigger.setAttribute("aria-label", ariaLabel);
      trigger.setAttribute("aria-haspopup", "listbox");
      trigger.setAttribute("aria-expanded", "false");
      const valueNode = element("span", "", "请选择");
      trigger.append(valueNode, lucideIcon("chevronDown", 14));
      root.append(trigger);
      const control = {
        id: ++selectSequence,
        root,
        trigger,
        valueNode,
        ariaLabel,
        options: [],
        value: "",
        placeholder: "请选择",
        disabled: false,
        minWidth,
        onChange,
        setOptions(nextOptions, nextValue, placeholder = "请选择") {
          this.options = Array.isArray(nextOptions)
            ? nextOptions
                .filter((option) => option && typeof option.value === "string" && typeof option.label === "string")
                .slice(0, 64)
                .map((option) => ({
                  value: option.value.slice(0, 160),
                  label: option.label.slice(0, 120),
                  disabled: Boolean(option.disabled),
                }))
            : [];
          this.value = typeof nextValue === "string" ? nextValue : "";
          this.placeholder = typeof placeholder === "string" ? placeholder.slice(0, 120) : "请选择";
          if (openSelectControl === this) closeWakeSelect(false);
          updateSelectTrigger(this);
        },
        setDisabled(nextDisabled) {
          this.disabled = Boolean(nextDisabled);
          if (this.disabled && openSelectControl === this) closeWakeSelect(false);
          updateSelectTrigger(this);
        },
      };
      trigger.addEventListener("click", () => {
        if (openSelectControl === control) closeWakeSelect(false);
        else openWakeSelect(control);
      });
      trigger.addEventListener("keydown", (event) => handleWakeSelectKeyDown(event, control));
      updateSelectTrigger(control);
      return control;
    };

    const selectEdgeIndex = (control, edge) => {
      if (!control.options.length) return -1;
      const start = edge === "first" ? -1 : 0;
      const direction = edge === "first" ? 1 : -1;
      for (let offset = 1; offset <= control.options.length; offset += 1) {
        const index = (start + direction * offset + control.options.length) % control.options.length;
        if (!control.options[index].disabled) return index;
      }
      return -1;
    };

    const selectMoveIndex = (control, currentIndex, direction) => {
      if (!control.options.length) return -1;
      for (
        let index = currentIndex + direction;
        index >= 0 && index < control.options.length;
        index += direction
      ) {
        if (!control.options[index].disabled) return index;
      }
      return currentIndex;
    };

    const renderSelectPopover = () => {
      selectPopover.replaceChildren();
      const control = openSelectControl;
      if (!control) return;
      control.options.forEach((option, index) => {
        const optionNode = element("div", "wake-select-option");
        optionNode.id = `${hostId}-select-${control.id}-option-${index}`;
        optionNode.dataset.index = String(index);
        optionNode.dataset.active = String(index === selectActiveIndex);
        optionNode.dataset.disabled = String(option.disabled);
        optionNode.setAttribute("role", "option");
        optionNode.setAttribute("aria-selected", String(option.value === control.value));
        if (option.disabled) optionNode.setAttribute("aria-disabled", "true");
        optionNode.append(element("span", "", option.label), lucideIcon("check", 14));
        selectPopover.append(optionNode);
      });
      control.trigger.setAttribute(
        "aria-activedescendant",
        selectActiveIndex >= 0 ? `${hostId}-select-${control.id}-option-${selectActiveIndex}` : "",
      );
      selectPopover.querySelector(`[data-index="${selectActiveIndex}"]`)?.scrollIntoView({ block: "nearest" });
    };

    const positionSelectPopover = () => {
      const control = openSelectControl;
      if (!control) return;
      const triggerRect = control.trigger.getBoundingClientRect();
      const margin = 8;
      const gap = 6;
      const width = Math.min(
        Math.max(triggerRect.width, control.minWidth),
        Math.max(120, Math.min(260, window.innerWidth - margin * 2)),
      );
      selectPopover.style.width = `${Math.round(width)}px`;
      const popoverRect = selectPopover.getBoundingClientRect();
      const left = Math.min(
        Math.max(margin, triggerRect.left),
        Math.max(margin, window.innerWidth - width - margin),
      );
      const fitsBelow = triggerRect.bottom + gap + popoverRect.height <= window.innerHeight - margin;
      const top = fitsBelow
        ? triggerRect.bottom + gap
        : Math.max(margin, triggerRect.top - gap - popoverRect.height);
      selectPopover.dataset.placement = fitsBelow ? "below" : "above";
      selectPopover.style.left = `${Math.round(left)}px`;
      selectPopover.style.top = `${Math.round(top)}px`;
    };

    const closeWakeSelect = (restoreFocus = false) => {
      const control = openSelectControl;
      selectPopover.hidden = true;
      selectPopover.removeAttribute("style");
      selectPopover.removeAttribute("aria-label");
      selectPopover.replaceChildren();
      if (control) {
        control.root.dataset.open = "false";
        control.trigger.setAttribute("aria-expanded", "false");
        control.trigger.removeAttribute("aria-controls");
        control.trigger.removeAttribute("aria-activedescendant");
        if (restoreFocus && control.trigger.isConnected) control.trigger.focus({ preventScroll: true });
      }
      openSelectControl = null;
      selectActiveIndex = -1;
      selectTypeaheadText = "";
      selectTypeaheadAt = 0;
    };

    const openWakeSelect = (control, requestedIndex = -1) => {
      if (!control || control.disabled || !control.options.some((option) => !option.disabled)) return;
      closeItemMenu();
      closeWakeSelect(false);
      openSelectControl = control;
      const selectedIndex = control.options.findIndex(
        (option) => option.value === control.value && !option.disabled,
      );
      selectActiveIndex = requestedIndex >= 0 && !control.options[requestedIndex]?.disabled
        ? requestedIndex
        : selectedIndex >= 0
          ? selectedIndex
          : selectEdgeIndex(control, "first");
      control.root.dataset.open = "true";
      control.trigger.setAttribute("aria-expanded", "true");
      control.trigger.setAttribute("aria-controls", selectPopover.id);
      selectPopover.setAttribute("aria-label", control.ariaLabel);
      selectPopover.hidden = false;
      renderSelectPopover();
      positionSelectPopover();
    };

    const commitWakeSelect = (index, restoreFocus = true) => {
      const control = openSelectControl;
      const option = control?.options[index];
      if (!control || !option || option.disabled) return;
      control.value = option.value;
      updateSelectTrigger(control);
      const onChange = control.onChange;
      closeWakeSelect(restoreFocus);
      onChange?.(option.value);
    };

    const findWakeSelectTypeahead = (control, key) => {
      const now = Date.now();
      selectTypeaheadText = now - selectTypeaheadAt <= 700 ? `${selectTypeaheadText}${key}` : key;
      selectTypeaheadAt = now;
      const repeated = [...selectTypeaheadText].every((character) => character === selectTypeaheadText[0]);
      const search = (repeated ? key : selectTypeaheadText).toLocaleLowerCase();
      const selectedIndex = control.options.findIndex((option) => option.value === control.value);
      const start = selectActiveIndex >= 0 ? selectActiveIndex : selectedIndex;
      for (let offset = 1; offset <= control.options.length; offset += 1) {
        const index = (Math.max(start, -1) + offset) % control.options.length;
        const option = control.options[index];
        if (!option.disabled && option.label.toLocaleLowerCase().startsWith(search)) return index;
      }
      return -1;
    };

    const handleWakeSelectKeyDown = (event, control) => {
      if (event.isComposing) return;
      const printable = event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey;
      if (openSelectControl !== control) {
        if (["Enter", " ", "ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
          event.preventDefault();
          const index = event.key === "ArrowUp" || event.key === "Home"
            ? selectEdgeIndex(control, "first")
            : event.key === "End"
              ? selectEdgeIndex(control, "last")
              : -1;
          openWakeSelect(control, index);
        } else if (printable) {
          const match = findWakeSelectTypeahead(control, event.key);
          if (match >= 0) {
            event.preventDefault();
            openWakeSelect(control, match);
          }
        }
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        closeWakeSelect(true);
      } else if (event.key === "ArrowUp" && event.altKey) {
        event.preventDefault();
        commitWakeSelect(selectActiveIndex);
      } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        selectActiveIndex = selectMoveIndex(control, selectActiveIndex, event.key === "ArrowDown" ? 1 : -1);
        renderSelectPopover();
      } else if (event.key === "Home" || event.key === "End") {
        event.preventDefault();
        selectActiveIndex = selectEdgeIndex(control, event.key === "Home" ? "first" : "last");
        renderSelectPopover();
      } else if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        commitWakeSelect(selectActiveIndex);
      } else if (event.key === "Tab") {
        if (selectActiveIndex >= 0) commitWakeSelect(selectActiveIndex, false);
        else closeWakeSelect(false);
      } else if (printable) {
        const match = findWakeSelectTypeahead(control, event.key);
        if (match >= 0) {
          event.preventDefault();
          selectActiveIndex = match;
          renderSelectPopover();
        }
      }
    };

    selectPopover.addEventListener("pointerdown", (event) => event.preventDefault());
    selectPopover.addEventListener("pointermove", (event) => {
      const option = event.target instanceof Element ? event.target.closest(".wake-select-option") : null;
      if (!option || option.dataset.disabled === "true") return;
      const index = Number(option.dataset.index);
      if (!Number.isInteger(index) || index === selectActiveIndex) return;
      selectActiveIndex = index;
      renderSelectPopover();
    });
    selectPopover.addEventListener("click", (event) => {
      const option = event.target instanceof Element ? event.target.closest(".wake-select-option") : null;
      if (!option || option.dataset.disabled === "true") return;
      const index = Number(option.dataset.index);
      if (Number.isInteger(index)) commitWakeSelect(index);
    });

    const card = element("div", "card");
    const header = element("header");
    const brandMark = element("span", "mark");
    brandMark.append(wakeBrandMark());
    header.append(brandMark, element("strong", "", "WakeGPT"));
    const appLauncherButton = button("icon app-launcher", "", "openApp");
    setIconButton(appLauncherButton, "external", "打开 WakeGPT App");
    const expandButton = button("icon expand-toggle", "", "toggleExpanded");
    setIconButton(expandButton, "maximize", "展开速记面板");
    const notebookMenuButton = button("icon notebook-menu", "", "toggleNotebookPanel");
    setIconButton(notebookMenuButton, "plus", "新建或绑定速记本");
    header.append(appLauncherButton, expandButton, notebookMenuButton);
    const selectors = element("div", "selectors");
    const workspaceSelect = createWakeSelect("workspace-select", "工作区", (value) => {
      if (recordEditSession) {
        actionStatus = "请先保存或取消当前记录修改";
        status.textContent = actionStatus;
        workspaceSelect.value = state.selectedWorkspaceId || "";
        updateSelectTrigger(workspaceSelect);
        return;
      }
      state.selectedWorkspaceId = value || null;
      send("selectWorkspace", {
        workspaceId: state.selectedWorkspaceId,
        bodyMarkdown: input.value,
      });
    }, 190);
    const notebookSelect = createWakeSelect("notebook-select", "速记本", (value) => {
      if (recordEditSession) {
        actionStatus = "请先保存或取消当前记录修改";
        status.textContent = actionStatus;
        notebookSelect.value = state.selectedNotebookId || "";
        updateSelectTrigger(notebookSelect);
        return;
      }
      state.selectedNotebookId = value || null;
      send("selectNotebook", {
        notebookId: state.selectedNotebookId,
        bodyMarkdown: input.value,
      });
    }, 190);
    const closeTabButton = button("close-tab", "", "unpinNotebook");
    setIconButton(closeTabButton, "x", "关闭当前 Tab（保留速记本）");
    selectors.append(workspaceSelect.root, notebookSelect.root, closeTabButton);

    const createPanel = element("div", "create-panel");
    createPanel.hidden = true;
    const notebookNameInput = element("input");
    notebookNameInput.maxLength = 120;
    notebookNameInput.placeholder = "新速记本名称";
    notebookNameInput.setAttribute("aria-label", "新速记本名称");
    const numberingStartInput = element("input");
    numberingStartInput.type = "number";
    numberingStartInput.min = "1";
    numberingStartInput.max = "1000000000";
    numberingStartInput.step = "1";
    numberingStartInput.value = "1";
    numberingStartInput.placeholder = "起始";
    numberingStartInput.setAttribute("aria-label", "起始序号");
    const numberingSelect = createWakeSelect("numbering-select", "自动序号", (value) => {
      numberingStartInput.disabled = !["numeric", "dateHeadingNumeric"].includes(value);
    }, 150);
    numberingSelect.setOptions([
      { value: "numeric", label: "数字" },
      { value: "bullet", label: "项目符号" },
      { value: "task", label: "任务" },
      { value: "timePrefix", label: "时间前缀" },
      { value: "dateHeadingNumeric", label: "日期＋数字" },
      { value: "none", label: "无序号" },
    ], "numeric");
    const attachmentDirectoryInput = element("input", "attachment-directory-input");
    attachmentDirectoryInput.maxLength = 1024;
    attachmentDirectoryInput.placeholder = "附件目录（留空使用 Markdown 旁的默认目录）";
    attachmentDirectoryInput.setAttribute("aria-label", "附件目录");
    const panelActions = element("div", "panel-actions");
    panelActions.append(
      button("", "新建 Markdown", "newNotebook"),
      button("", "绑定已有 Markdown", "bindNotebook"),
    );
    createPanel.append(
      notebookNameInput,
      numberingSelect.root,
      numberingStartInput,
      attachmentDirectoryInput,
      panelActions,
    );

    const targetHint = element("div", "target-hint");
    targetHint.setAttribute("role", "status");

    const composer = element("div", "composer");
    const imageDropOverlay = element("div", "image-drop-overlay");
    imageDropOverlay.hidden = true;
    imageDropOverlay.setAttribute("role", "status");
    imageDropOverlay.append(
      lucideIcon("image", 22),
      element("strong", "", "松开以添加图片"),
      element("span", "", "支持 PNG、JPEG、WebP 和 GIF；原图不会被移动"),
    );
    const input = element("textarea");
    input.maxLength = 1048576;
    input.placeholder = "记录想法、粘贴图片或链接，输入 Markdown…";
    input.setAttribute("aria-label", "速记内容");
    const composerActions = element("div", "composer-actions");
    const imageButton = button("icon", "", "chooseImages");
    setIconButton(imageButton, "image", "添加图片");
    const imageInput = element("input", "image-input");
    imageInput.type = "file";
    imageInput.accept = "image/png,image/jpeg,image/webp,image/gif,.png,.jpg,.jpeg,.webp,.gif";
    imageInput.multiple = true;
    imageInput.hidden = true;
    imageInput.tabIndex = -1;
    const linkButton = button("icon", "", "insertLink");
    setIconButton(linkButton, "link", "插入链接");
    const recordButton = button("record", "记录", "createRecord");
    const status = element("div", "status");
    status.setAttribute("role", "status");
    const pendingImages = element("div", "pending-images");
    pendingImages.setAttribute("aria-label", "待提交图片");
    composerActions.append(imageButton, linkButton, recordButton);
    composer.append(imageDropOverlay, input, imageInput, composerActions, pendingImages, status);

    const recentHeading = element("div", "recent-heading");
    recentHeading.append(element("span", "", "最近记录"), element("span", "count", "0 条"));
    const recent = element("div", "recent");
    const itemMenu = element("div", "item-menu");
    itemMenu.id = `${hostId}-record-menu`;
    itemMenu.hidden = true;
    itemMenu.setAttribute("role", "menu");
    itemMenu.setAttribute("aria-label", "记录操作");
    const itemMenuEdit = button("", "编辑", "beginEdit");
    const itemMenuPin = button("", "置顶", "pinRecord");
    const itemMenuMigrate = button("", "迁移到…", "beginTargetAction");
    const itemMenuRemove = button("", "删除", "trashRecord");
    for (const menuButton of [itemMenuEdit, itemMenuPin, itemMenuMigrate, itemMenuRemove]) {
      menuButton.setAttribute("role", "menuitem");
      menuButton.tabIndex = -1;
    }
    itemMenu.append(itemMenuEdit, itemMenuPin, itemMenuMigrate, itemMenuRemove);
    const imageLightbox = element("div", "image-lightbox");
    imageLightbox.hidden = true;
    imageLightbox.setAttribute("role", "dialog");
    imageLightbox.setAttribute("aria-modal", "true");
    imageLightbox.setAttribute("aria-label", "图片预览");
    const imageLightboxFigure = element("figure");
    const imageLightboxImage = element("img");
    imageLightboxImage.alt = "";
    imageLightboxImage.draggable = false;
    const imageLightboxCaption = element("figcaption");
    imageLightboxFigure.append(imageLightboxImage, imageLightboxCaption);
    const imageLightboxClose = button("image-lightbox-close", "", "closeImagePreview");
    setIconButton(imageLightboxClose, "x", "关闭图片预览");
    imageLightbox.append(imageLightboxFigure, imageLightboxClose);
    const cardScroll = element("div", "card-scroll");
    cardScroll.append(selectors, targetHint, createPanel, composer, recentHeading, recent);
    card.append(header, cardScroll);
    shadow.append(style, card, itemMenu, selectPopover, imageLightbox);
    document.body.append(host);

    let state = normalizeState(bootstrap.initialState);
    let pendingCreate = null;
    let pendingCreateAck = null;
    let createRequestTimer = 0;
    let createAckTimer = 0;
    let observedContext = null;
    let resizeObserver = null;
    let layoutFrame = 0;
    let lastHeartbeatAt = Date.now();
    let lastVisibilityDiagnosticKey = "";
    let expandedOverlay = false;
    let actionStatus = "";
    let draftSaveTimer = 0;
    let imageRequestTimer = 0;
    let pendingImageRequest = null;
    // space-seed: Saved-record edit files stay in renderer memory until the user presses Save.
    // This keeps them isolated from composer drafts and leaves Cancel as zero backend writes. If
    // edits must survive a card remount, replace this with a persisted record-edit attempt journal.
    let recordEditSession = null;
    let pendingRecordEditRequest = null;
    let imageDropDepth = 0;
    let itemMenuTrigger = null;
    let imageLightboxTrigger = null;
    let activeComposerUncertaintyRecordId = null;
    let pendingComposerResolution = null;
    let activeMount = true;
    let compatibilityFailureSent = false;
    const pendingActions = new Map();
    const pendingComposerInsertions = new Map();
    const imagePreviewUrls = new Map();
    const imagePreviewBuffers = new Map();
    const imagePreviewLoading = new Map();
    const imagePreviewFailures = new Set();
    const imagePreviewRequests = new Map();
    let imagePreviewObserver = null;

    const send = (action, data, requestId = crypto.randomUUID()) => {
      const payload = {
        schemaVersion: 1,
        adapterVersion,
        sessionNonce: bootstrap.sessionNonce,
        requestId,
        action,
        data,
      };
      globalThis[bootstrap.bindingName](JSON.stringify(payload));
      pendingActions.set(requestId, action);
      return requestId;
    };

    const notify = (action, data) => {
      try {
        globalThis[bootstrap.bindingName](JSON.stringify({
          schemaVersion: 1,
          adapterVersion,
          sessionNonce: bootstrap.sessionNonce,
          requestId: crypto.randomUUID(),
          action,
          data,
        }));
        return true;
      } catch {
        return false;
      }
    };

    const reportCardVisibility = (visibility, layout = null) => {
      const key = `${visibility}:${layout || ""}`;
      if (key === lastVisibilityDiagnosticKey) return;
      if (notify("cardVisibilityChanged", { visibility, layout })) {
        lastVisibilityDiagnosticKey = key;
      }
    };

    const canAcceptImageDrop = () => {
      const selectedNotebook = state.notebooks.find(
        (notebook) => notebook?.id === state.selectedNotebookId,
      );
      return !state.paused && !notebookBlocksChanges(selectedNotebook) && !pendingImageRequest;
    };

    const setImageDropActive = (active) => {
      imageDropOverlay.hidden = !active;
      composer.dataset.imageDropActive = active ? "true" : "false";
    };

    const queueImageFiles = (files) => {
      const candidates = Array.from(files || []).filter(isSupportedImageFile);
      if (!candidates.length) return;
      const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
      const targetBlocked = notebookBlocksChanges(selectedNotebook);
      if (state.paused || targetBlocked) return;
      if (pendingImageRequest) {
        actionStatus = "上一批图片仍在读取，请稍候";
        status.textContent = actionStatus;
        return;
      }
      if (state.pendingImageCount + candidates.length > 10) {
        actionStatus = "每条记录最多 10 张图片";
        status.textContent = actionStatus;
        return;
      }
      if (candidates.some((file) => file.size > 20 * 1024 * 1024)) {
        actionStatus = "单张图片不能超过 20 MiB";
        status.textContent = actionStatus;
        return;
      }
      const total = candidates.reduce((sum, file) => sum + file.size, 0);
      if (!Number.isSafeInteger(total) || total > 100 * 1024 * 1024) {
        actionStatus = "单条记录的图片合计不能超过 100 MiB";
        status.textContent = actionStatus;
        return;
      }
      const uploadId = crypto.randomUUID();
      pastedImageBatches.set(uploadId, {
        sessionNonce: bootstrap.sessionNonce,
        files: candidates,
      });
      pendingImageRequest = uploadId;
      actionStatus = `正在读取 ${candidates.length} 张图片…`;
      try {
        send("stagePastedImages", {
          uploadId,
          bodyMarkdown: input.value,
          images: candidates.map((file, index) => ({
            index,
            byteSize: file.size,
          })),
        }, uploadId);
      } catch {
        globalThis.__wakegptDiscardPastedImages?.(bootstrap.sessionNonce, uploadId);
        pendingImageRequest = null;
        actionStatus = "图片读取请求未发送，请重试";
      }
      if (pendingImageRequest) {
        imageRequestTimer = window.setTimeout(() => {
          if (pendingImageRequest !== uploadId) return;
          globalThis.__wakegptDiscardPastedImages?.(bootstrap.sessionNonce, uploadId);
          pendingActions.delete(uploadId);
          pendingImageRequest = null;
          imageRequestTimer = 0;
          actionStatus = "图片读取超时，请重试";
          render();
          scheduleLayout();
        }, 60000);
      }
      render();
      scheduleLayout();
    };

    const retainedIdsFromEditor = (editor, record) => {
      const validIds = new Set(record?.attachments?.map((attachment) => attachment.id) || []);
      try {
        const parsed = JSON.parse(editor?.dataset.retainedAttachmentIds || "[]");
        return Array.isArray(parsed)
          ? parsed.filter((id, index) => (
              typeof id === "string" && validIds.has(id) && parsed.indexOf(id) === index
            ))
          : [];
      } catch {
        return [];
      }
    };

    const setInlineEditError = (editor, message) => {
      const alert = editor?.querySelector?.(".inline-edit-error");
      if (!alert) return;
      alert.textContent = typeof message === "string" ? message.slice(0, 240) : "";
      alert.hidden = !alert.textContent;
    };

    const clearRecordEditSession = () => {
      const session = recordEditSession;
      if (!session) return;
      pastedImageBatches.delete(session.uploadId);
      if (imageInput.dataset.editRecordId === session.recordId) {
        delete imageInput.dataset.editRecordId;
        imageInput.value = "";
      }
      for (const entry of session.files) {
        const preview = imagePreviewUrls.get(entry.previewKey);
        if (preview) URL.revokeObjectURL(preview.url);
        imagePreviewUrls.delete(entry.previewKey);
        imagePreviewFailures.delete(entry.previewKey);
        if (imageLightbox.dataset.previewKey === entry.previewKey) closeImageLightbox(false);
      }
      recordEditSession = null;
    };

    const ensureRecordEditSession = (record, mutationId = "") => {
      if (!record || typeof record.id !== "string") return null;
      const expectedRevision = Number(record.revision || 0);
      if (
        recordEditSession
        && (
          recordEditSession.selectionKey !== state.selectionKey
          || recordEditSession.recordId !== record.id
          || recordEditSession.expectedRevision !== expectedRevision
        )
      ) clearRecordEditSession();
      if (!recordEditSession) {
        recordEditSession = {
          selectionKey: state.selectionKey,
          recordId: record.id,
          expectedRevision,
          mutationId: typeof mutationId === "string" && mutationId ? mutationId : crypto.randomUUID(),
          uploadId: crypto.randomUUID(),
          files: [],
        };
      }
      return recordEditSession;
    };

    const restoreInlineEditAfterLocalChange = () => {
      const snapshot = captureInlineEdit(recent, state.selectionKey);
      render();
      if (snapshot && canRestoreInlineEdit(snapshot, state)) {
        beginInlineEdit(snapshot.recordId, snapshot);
      }
      scheduleLayout();
    };

    const queueRecordEditImageFiles = (files, editor) => {
      const item = editor?.closest?.(".item");
      const record = state.recentRecords.find((candidate) => candidate?.id === item?.dataset.recordId);
      if (!record || !editor || pendingRecordEditRequest) return;
      const candidates = Array.from(files || []).filter(isSupportedImageFile);
      if (!candidates.length) {
        setInlineEditError(editor, "仅支持 PNG、JPEG、WebP 和 GIF 图片");
        return;
      }
      if (candidates.some((file) => file.size > 20 * 1024 * 1024)) {
        setInlineEditError(editor, "单张图片不能超过 20 MiB");
        return;
      }
      const session = ensureRecordEditSession(record, editor.dataset.mutationId || "");
      if (!session) return;
      const uniqueCandidates = candidates.filter((file) => (
        !session.files.some((entry) => entry.file === file)
      ));
      const retainedIds = new Set(retainedIdsFromEditor(editor, record));
      const retained = record.attachments.filter((attachment) => retainedIds.has(attachment.id));
      if (retained.length + session.files.length + uniqueCandidates.length > 10) {
        setInlineEditError(editor, "每条记录最多 10 张图片");
        return;
      }
      const totalBytes = [...retained.map((attachment) => attachment.byteSize),
        ...session.files.map((entry) => entry.file.size),
        ...uniqueCandidates.map((file) => file.size)]
        .reduce((sum, value) => sum + value, 0);
      if (!Number.isSafeInteger(totalBytes) || totalBytes > 100 * 1024 * 1024) {
        setInlineEditError(editor, "单条记录的图片合计不能超过 100 MiB");
        return;
      }
      for (const file of uniqueCandidates) {
        const id = crypto.randomUUID();
        const previewKey = `record-edit:${session.uploadId}:${id}`;
        const label = String(file.name || "待添加图片").slice(0, 160);
        const url = URL.createObjectURL(file);
        session.files.push({ id, file, previewKey, label });
        imagePreviewUrls.set(previewKey, {
          url,
          mediaType: String(file.type || "image/png").slice(0, 64),
        });
      }
      pastedImageBatches.set(session.uploadId, {
        sessionNonce: bootstrap.sessionNonce,
        files: session.files,
      });
      setInlineEditError(editor, "");
      restoreInlineEditAfterLocalChange();
    };

    const removeRecordEditImage = (previewKey) => {
      const session = recordEditSession;
      if (!session || pendingRecordEditRequest) return;
      const index = session.files.findIndex((entry) => entry.previewKey === previewKey);
      if (index < 0) return;
      const [entry] = session.files.splice(index, 1);
      const preview = imagePreviewUrls.get(entry.previewKey);
      if (preview) URL.revokeObjectURL(preview.url);
      imagePreviewUrls.delete(entry.previewKey);
      imagePreviewFailures.delete(entry.previewKey);
      if (imageLightbox.dataset.previewKey === entry.previewKey) closeImageLightbox(false);
      if (session.files.length) {
        pastedImageBatches.set(session.uploadId, {
          sessionNonce: bootstrap.sessionNonce,
          files: session.files,
        });
      } else {
        pastedImageBatches.delete(session.uploadId);
      }
      restoreInlineEditAfterLocalChange();
    };

    const closeItemMenu = (restoreFocus = false) => {
      itemMenu.hidden = true;
      itemMenu.removeAttribute("style");
      itemMenu.removeAttribute("data-record-id");
      itemMenuRemove.dataset.confirmed = "false";
      itemMenuRemove.textContent = "删除";
      for (const trigger of recent.querySelectorAll("button[data-action='toggleItemMenu']")) {
        trigger.setAttribute("aria-expanded", "false");
        trigger.removeAttribute("aria-controls");
      }
      if (restoreFocus && itemMenuTrigger?.isConnected) itemMenuTrigger.focus({ preventScroll: true });
      itemMenuTrigger = null;
    };

    const positionItemMenu = (trigger) => {
      const triggerRect = trigger.getBoundingClientRect();
      const hostRect = host.getBoundingClientRect();
      const menuRect = itemMenu.getBoundingClientRect();
      const margin = 8;
      const leftBoundary = Math.max(margin, hostRect.left + 4);
      const rightBoundary = Math.min(window.innerWidth - margin, hostRect.right - 4);
      const topBoundary = Math.max(margin, hostRect.top + 4);
      const bottomBoundary = Math.min(window.innerHeight - margin, hostRect.bottom - 4);
      const left = Math.min(
        Math.max(leftBoundary, triggerRect.right - menuRect.width),
        Math.max(leftBoundary, rightBoundary - menuRect.width),
      );
      const below = triggerRect.bottom + 5;
      const above = triggerRect.top - menuRect.height - 5;
      const top = below + menuRect.height <= bottomBoundary
        ? below
        : Math.max(topBoundary, Math.min(above, bottomBoundary - menuRect.height));
      itemMenu.style.left = `${Math.round(left)}px`;
      itemMenu.style.top = `${Math.round(top)}px`;
    };

    const openItemMenu = (trigger, recordId, focusEdge = "first") => {
      const record = state.recentRecords.find((item) => item?.id === recordId);
      if (!record) return;
      closeWakeSelect(false);
      const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
      const targetBlocked = notebookBlocksChanges(selectedNotebook);
      const wasOpen = !itemMenu.hidden && itemMenu.dataset.recordId === recordId;
      closeItemMenu();
      if (wasOpen) return;
      itemMenu.dataset.recordId = recordId;
      itemMenuEdit.dataset.recordId = recordId;
      itemMenuPin.dataset.recordId = recordId;
      itemMenuPin.dataset.revision = String(record.revision || 0);
      itemMenuPin.dataset.pinned = String(!record.isPinned);
      itemMenuPin.textContent = record.isPinned ? "取消置顶" : "置顶";
      itemMenuMigrate.dataset.recordId = recordId;
      itemMenuMigrate.dataset.mode = "migrate";
      itemMenuRemove.dataset.recordId = recordId;
      itemMenuRemove.dataset.revision = String(record.revision || 0);
      itemMenuEdit.disabled = targetBlocked || !record.attachmentsReady;
      itemMenuMigrate.disabled = targetBlocked || !record.attachmentsReady;
      itemMenuRemove.disabled = targetBlocked || !record.attachmentsReady;
      itemMenu.hidden = false;
      itemMenuTrigger = trigger;
      trigger.setAttribute("aria-expanded", "true");
      trigger.setAttribute("aria-controls", itemMenu.id);
      positionItemMenu(trigger);
      const enabledItems = [...itemMenu.querySelectorAll("button:not(:disabled)")];
      const focusIndex = focusEdge === "last" ? enabledItems.length - 1 : 0;
      enabledItems[focusIndex]?.focus({ preventScroll: true });
    };

    const cardFocusableSelector = [
      "a[href]",
      "button:not([disabled])",
      "input:not([disabled]):not([type='hidden'])",
      "textarea:not([disabled])",
      "[contenteditable='true']",
      "[tabindex]:not([tabindex='-1'])",
    ].join(",");

    const moveFocusFromItemMenu = (backwards) => {
      const trigger = itemMenuTrigger;
      if (!trigger?.isConnected) {
        closeItemMenu(false);
        return;
      }
      const focusable = [...shadow.querySelectorAll(cardFocusableSelector)].filter((control) => (
        control instanceof HTMLElement
        && control.tabIndex >= 0
        && !itemMenu.contains(control)
        && !control.closest("[hidden],[inert],[aria-hidden='true']")
        && isVisible(control)
      ));
      const triggerIndex = focusable.indexOf(trigger);
      let target = triggerIndex < 0
        ? null
        : focusable[triggerIndex + (backwards ? -1 : 1)] || null;
      if (!target) {
        const pageFocusable = [...document.querySelectorAll(cardFocusableSelector)].filter((control) => (
          control instanceof HTMLElement
          && control.tabIndex >= 0
          && !control.closest(`#${hostId}`)
          && !control.closest("[hidden],[inert],[aria-hidden='true']")
          && isVisible(control)
        ));
        const relation = backwards ? Node.DOCUMENT_POSITION_PRECEDING : Node.DOCUMENT_POSITION_FOLLOWING;
        const candidates = pageFocusable.filter((control) => (
          Boolean(host.compareDocumentPosition(control) & relation)
        ));
        target = backwards
          ? candidates[candidates.length - 1] || pageFocusable[pageFocusable.length - 1] || null
          : candidates[0] || pageFocusable[0] || null;
      }
      closeItemMenu(false);
      requestAnimationFrame(() => {
        if (target?.isConnected) target.focus({ preventScroll: true });
        else if (trigger.isConnected) trigger.focus({ preventScroll: true });
      });
    };

    const queueDraftSave = () => {
      if (draftSaveTimer) window.clearTimeout(draftSaveTimer);
      draftSaveTimer = window.setTimeout(() => {
        draftSaveTimer = 0;
        send("saveDraft", {
          workspaceId: state.selectedWorkspaceId,
          notebookId: state.selectedNotebookId,
          bodyMarkdown: input.value,
        });
      }, 350);
    };

    const formatImageBytes = (value) => {
      if (!Number.isSafeInteger(value) || value < 0) return "";
      if (value < 1024) return `${value} B`;
      if (value < 1024 * 1024) return `${Math.max(1, Math.round(value / 1024))} KiB`;
      return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
    };

    const closeImageLightbox = (restoreFocus = true) => {
      if (imageLightbox.hidden) return;
      imageLightbox.hidden = true;
      imageLightbox.removeAttribute("data-preview-key");
      imageLightboxImage.removeAttribute("src");
      imageLightboxCaption.textContent = "";
      if (restoreFocus && imageLightboxTrigger?.isConnected) {
        imageLightboxTrigger.focus({ preventScroll: true });
      }
      imageLightboxTrigger = null;
    };

    const openImageLightbox = (previewKey, label, trigger) => {
      const preview = imagePreviewUrls.get(previewKey);
      if (!preview) return;
      imageLightbox.dataset.previewKey = previewKey;
      imageLightboxImage.src = preview.url;
      imageLightboxImage.alt = label;
      imageLightboxCaption.textContent = label;
      imageLightbox.setAttribute("aria-label", `查看 ${label}`);
      imageLightbox.hidden = false;
      imageLightboxTrigger = trigger;
      imageLightboxClose.focus({ preventScroll: true });
    };

    const previewDescriptorFromButton = (node) => {
      const scope = node?.dataset.scope;
      const previewKey = node?.dataset.previewKey;
      if (!previewKey || !["pending", "record"].includes(scope)) return null;
      if (scope === "pending") {
        const token = node.dataset.token;
        return token ? { scope, token, previewKey } : null;
      }
      const recordId = node.dataset.recordId;
      const attachmentId = node.dataset.attachmentId;
      return recordId && attachmentId
        ? { scope, recordId, attachmentId, previewKey }
        : null;
    };

    const requestImagePreview = (descriptor, retry = false) => {
      if (
        !descriptor
        || imagePreviewUrls.has(descriptor.previewKey)
        || imagePreviewLoading.has(descriptor.previewKey)
        || (!retry && imagePreviewFailures.has(descriptor.previewKey))
      ) return;
      imagePreviewFailures.delete(descriptor.previewKey);
      const requestId = send("loadImagePreview", descriptor);
      imagePreviewLoading.set(descriptor.previewKey, requestId);
      imagePreviewRequests.set(requestId, descriptor.previewKey);
    };

    const imagePreviewButton = ({
      previewKey,
      scope,
      label,
      available,
      token = "",
      recordId = "",
      attachmentId = "",
    }) => {
      const preview = imagePreviewUrls.get(previewKey);
      const loading = imagePreviewLoading.has(previewKey);
      const failed = imagePreviewFailures.has(previewKey) || !available;
      const node = button("image-preview", "", preview ? "openImagePreview" : "loadImagePreview");
      node.dataset.previewKey = previewKey;
      node.dataset.scope = scope;
      node.dataset.label = label.slice(0, 160);
      if (token) node.dataset.token = token;
      if (recordId) node.dataset.recordId = recordId;
      if (attachmentId) node.dataset.attachmentId = attachmentId;
      node.dataset.state = preview ? "ready" : loading ? "loading" : failed ? "error" : "idle";
      node.disabled = !preview && !available;
      node.setAttribute("aria-label", preview
        ? `放大查看 ${label}`
        : failed
          ? `${label}预览不可用${available ? "，点击重试" : ""}`
          : `${label}预览加载中`);
      if (preview) {
        const image = element("img");
        image.src = preview.url;
        image.alt = "";
        image.draggable = false;
        node.append(image);
      } else {
        node.append(lucideIcon("image", 17));
        if (loading) node.append(element("span", "", "读取中"));
        else if (failed) node.append(element("span", "", available ? "重试" : "不可用"));
      }
      return node;
    };

    const observeImagePreviews = () => {
      imagePreviewObserver?.disconnect();
      const nodes = [...shadow.querySelectorAll('.image-preview[data-state="idle"]')];
      if (!nodes.length) return;
      if (!("IntersectionObserver" in globalThis)) {
        for (const node of nodes) requestImagePreview(previewDescriptorFromButton(node));
        return;
      }
      imagePreviewObserver = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          imagePreviewObserver?.unobserve(entry.target);
          requestImagePreview(previewDescriptorFromButton(entry.target));
        }
      }, { rootMargin: "120px" });
      for (const node of nodes) imagePreviewObserver.observe(node);
    };

    let observedComposerContext = "";
    const syncComposerContext = () => {
      const current = composerDocumentIdentity();
      if (current === observedComposerContext) return;
      observedComposerContext = current;
      activeComposerUncertaintyRecordId = null;
      state.recentRecords = state.recentRecords.map((record) => ({ ...record, insertionState: "none" }));
      send("composerContextChanged", {});
    };

    const render = () => {
      imagePreviewObserver?.disconnect();
      closeItemMenu();
      closeWakeSelect(false);
      const activeComposerUncertainty = state.recentRecords.find((record) => (
        record?.id === activeComposerUncertaintyRecordId
        && composerInsertionNeedsReview(record?.insertionState)
      ));
      if (!activeComposerUncertainty) activeComposerUncertaintyRecordId = null;
      workspaceSelect.setOptions(
        workspaceSelectOptions(state.workspaces),
        state.selectedWorkspaceId || "",
        "选择工作区",
      );
      const notebookOptions = notebookTabOptions(state.notebooks);
      notebookSelect.setOptions(
        notebookOptions,
        state.selectedNotebookId || "",
        "收件箱（仅本地）",
      );
      workspaceSelect.setDisabled(state.paused || Boolean(recordEditSession));
      notebookSelect.setDisabled(state.paused || Boolean(recordEditSession));
      const selectedNotebook = state.notebooks.find(
        (notebook) =>
          notebook &&
          typeof notebook.id === "string" &&
          typeof notebook.displayName === "string" &&
          notebook.id === state.selectedNotebookId,
      );
      closeTabButton.hidden = state.selectedNotebookId === null;
      if (!state.selectedNotebookId) {
        targetHint.dataset.tone = "local";
        targetHint.textContent = notebookOptions.length > 1
          ? "当前仅保存到 WakeGPT；选择速记本后立即同步 Markdown。"
          : state.notebooks.length
            ? "当前没有已固定速记本；请在 WakeGPT 的“更多”中重新打开 Tab，或点击 + 新建/绑定。"
            : "收件箱仅本地保存。点击 + 新建或绑定 Markdown。";
      } else if (selectedNotebook?.attachmentDirectorySyncPending) {
        targetHint.dataset.tone = "local";
        targetHint.textContent = "附件目录已保存，附件迁移与 Markdown 更新等待 WakeGPT 完成恢复。";
      } else if (selectedNotebook?.numberingSyncPending) {
        targetHint.dataset.tone = "local";
        targetHint.textContent = "编号配置已保存，Markdown 重排等待 WakeGPT 完成恢复。";
      } else if (selectedNotebook?.targetState === "ready") {
        targetHint.dataset.tone = "ready";
        targetHint.textContent = `提交后立即同步到 ${selectedNotebook.displayName}.md`;
      } else if (selectedNotebook?.targetState === "unbound") {
        targetHint.dataset.tone = "local";
        targetHint.textContent = "该速记本已解除绑定；文件与记录保留，请在 WakeGPT 中重新绑定。";
      } else if (selectedNotebook?.lastErrorCode === "notebook_converted_to_plain") {
        targetHint.dataset.tone = "local";
        targetHint.textContent = "已转为普通 Markdown；本地记录保留，当前速记本已停止同步。";
      } else if (selectedNotebook?.lastErrorCode === "notebook_moved_to_trash") {
        targetHint.dataset.tone = "local";
        targetHint.textContent = "文件已移入系统废纸篓；本地记录保留，恢复后可在 WakeGPT 中重新查找。";
      } else {
        targetHint.dataset.tone = "local";
        targetHint.textContent = selectedNotebook?.targetState === "conflict"
          ? "该速记本存在同步冲突；请在 WakeGPT 中处理。"
          : "目标路径不可用或仍在验证；请在 WakeGPT 中重新查找文件。";
      }
      targetHint.title = targetHint.textContent;
      const validPreviewKeys = new Set();
      if (recordEditSession?.selectionKey === state.selectionKey) {
        for (const entry of recordEditSession.files) validPreviewKeys.add(entry.previewKey);
      }
      pendingImages.replaceChildren();
      for (const attachment of state.pendingImages) {
        const previewKey = `pending:${attachment.token}`;
        validPreviewKeys.add(previewKey);
        const tile = element("div", "image-tile");
        const label = typeof attachment.displayName === "string"
          ? attachment.displayName.slice(0, 160)
          : "待提交图片";
        tile.append(
          imagePreviewButton({
            previewKey,
            scope: "pending",
            label,
            available: true,
            token: attachment.token,
          }),
          element("span", "image-caption", `${label} · ${formatImageBytes(attachment.byteSize)}`),
        );
        pendingImages.append(tile);
      }
      pendingImages.hidden = state.pendingImages.length === 0;
      recent.replaceChildren();
      const records = state.recentRecords;
      recentHeading.querySelector(".count").textContent = `${records.length} 条`;
      if (!records.length) {
        recent.append(element("div", "empty", "还没有速记"));
      } else {
        for (const record of records) {
          if (!record || typeof record.id !== "string" || typeof record.bodyMarkdown !== "string") continue;
          const item = element("article", "item");
          item.dataset.recordId = record.id;
          item.dataset.pinned = String(Boolean(record.isPinned));
          const body = record.bodyMarkdown.slice(0, 1200) || (record.attachmentCount ? `${record.attachmentCount} 张图片` : "空记录");
          item.append(element("div", "item-body", body));
          if (record.attachments.length) {
            const gallery = element("div", "item-images");
            record.attachments.forEach((attachment, index) => {
              const previewKey = `record:${attachment.id}`;
              validPreviewKeys.add(previewKey);
              const label = `图片 ${index + 1}`;
              const tile = element("div", "image-tile");
              tile.append(
                imagePreviewButton({
                  previewKey,
                  scope: "record",
                  label,
                  available: attachment.available,
                  recordId: record.id,
                  attachmentId: attachment.id,
                }),
                element("span", "image-caption", `${label} · ${formatImageBytes(attachment.byteSize)}`),
              );
              gallery.append(tile);
            });
            item.append(gallery);
          }
          const meta = element("div", "item-meta");
          meta.append(element("span", "", typeof record.createdLabel === "string" ? record.createdLabel.slice(0, 40) : ""));
          if (record.isPinned) meta.append(element("span", "pin-mark", "已置顶"));
          const actions = element("div", "item-actions");
          const insertLabel = record.insertionState === "complete"
            ? "已插入"
            : record.insertionState === "staleUncertain"
              ? "核对已修改记录"
              : record.insertionState === "uncertain"
                ? "核对插入"
                : record.insertionState === "partial"
                  ? "重试失败项"
                  : "插入";
          const insert = button("", insertLabel, "insertRecord");
          insert.dataset.recordId = record.id;
          insert.disabled = record.insertionState === "complete"
            || !record.attachmentsReady
            || [...pendingComposerInsertions.values()].some((pending) => pending.recordId === record.id)
            || pendingComposerResolution?.recordId === record.id;
          const copy = button("", "复制", "beginTargetAction");
          copy.dataset.recordId = record.id;
          copy.dataset.mode = "copy";
          copy.disabled = !record.attachmentsReady;
          const more = button("icon", "", "toggleItemMenu");
          more.dataset.recordId = record.id;
          more.setAttribute("aria-haspopup", "menu");
          more.setAttribute("aria-expanded", "false");
          setIconButton(more, "more", "更多记录操作");
          actions.append(insert, copy, more);
          meta.append(actions);
          item.append(meta);
          if (
            composerInsertionNeedsReview(record.insertionState)
            && record.id === activeComposerUncertaintyRecordId
          ) {
            const stale = record.insertionState === "staleUncertain";
            const review = element("div", "composer-insertion-review");
            review.dataset.recordId = record.id;
            review.tabIndex = -1;
            review.setAttribute("role", "group");
            review.setAttribute("aria-label", "核对插入结果");
            review.setAttribute(
              "aria-busy",
              pendingComposerResolution?.recordId === record.id ? "true" : "false",
            );
            review.append(
              element("strong", "", stale
                ? "记录已在上次插入后修改"
                : "请先查看 ChatGPT 输入框"),
              element("p", "", stale
                ? "请核对当前版本是否已出现；重试只在你明确选择后插入当前版本。"
                : "确认上次内容是否已经出现。WakeGPT 不会自动重试，以免重复插入。"),
            );
            const reviewActions = element("div", "composer-insertion-review-actions");
            const confirm = button("record", "", "resolveComposerInsertion");
            confirm.dataset.recordId = record.id;
            confirm.dataset.resolution = "confirm";
            setLabeledButton(confirm, "check", stale ? "当前版本已看到" : "已看到");
            const retry = button("", "", "resolveComposerInsertion");
            retry.dataset.recordId = record.id;
            retry.dataset.resolution = "retry";
            setLabeledButton(retry, "refresh", stale ? "没有，插入当前版本" : "没有，重试");
            const cancel = button("", "取消", "cancelComposerInsertionReview");
            cancel.dataset.recordId = record.id;
            const resolving = pendingComposerResolution?.recordId === record.id;
            confirm.disabled = resolving;
            retry.disabled = resolving;
            cancel.disabled = resolving;
            reviewActions.append(confirm, retry, cancel);
            review.append(reviewActions);
            item.append(review);
          }
          recent.append(item);
        }
      }
      for (const [previewKey, preview] of imagePreviewUrls) {
        if (validPreviewKeys.has(previewKey)) continue;
        URL.revokeObjectURL(preview.url);
        imagePreviewUrls.delete(previewKey);
        imagePreviewFailures.delete(previewKey);
        if (imageLightbox.dataset.previewKey === previewKey) closeImageLightbox(false);
      }
      const targetBlocked = notebookBlocksChanges(selectedNotebook);
      if (state.paused || targetBlocked || pendingImageRequest) {
        imageDropDepth = 0;
        setImageDropActive(false);
      }
      recordButton.disabled = state.paused || targetBlocked || (!input.value.trim() && !state.pendingImageCount) || pendingCreate !== null || pendingCreateAck !== null || pendingImageRequest !== null;
      input.disabled = state.paused || targetBlocked || pendingImageRequest !== null;
      imageButton.disabled = state.paused || targetBlocked || pendingImageRequest !== null;
      linkButton.disabled = state.paused || targetBlocked;
      status.textContent = state.paused
        ? "WakeGPT 集成已暂停"
        : targetBlocked
          ? selectedNotebook?.attachmentDirectorySyncPending
            ? "附件迁移恢复完成后可继续记录"
          : selectedNotebook?.numberingSyncPending
            ? "编号重排恢复完成后可继续记录"
            : selectedNotebook?.lastErrorCode === "notebook_converted_to_plain"
            ? "普通 Markdown 已停止同步"
            : selectedNotebook?.lastErrorCode === "notebook_moved_to_trash"
              ? "从废纸篓恢复文件后可重新查找"
              : selectedNotebook?.targetState === "unbound"
                ? "重新绑定后可继续记录"
                : "恢复目标文件后可继续记录"
        : pendingImageRequest
          ? actionStatus
        : state.pendingImageCount
          ? `已选择 ${state.pendingImageCount} 张真实图片`
          : actionStatus;
      observeImagePreviews();
    };

    const focusComposerInsertionControl = (recordId, selector) => {
      requestAnimationFrame(() => {
        if (!activeMount) return;
        const item = recent.querySelector(`.item[data-record-id="${CSS.escape(recordId)}"]`);
        item?.querySelector(selector)?.focus({ preventScroll: true });
      });
    };

    const openComposerInsertionReview = (recordId) => {
      if (pendingComposerResolution) return;
      const record = state.recentRecords.find((item) => item?.id === recordId);
      if (!record || !composerInsertionNeedsReview(record.insertionState)) return;
      activeComposerUncertaintyRecordId = recordId;
      render();
      scheduleLayout();
      focusComposerInsertionControl(
        recordId,
        '[data-action="resolveComposerInsertion"][data-resolution="confirm"]',
      );
    };

    const closeComposerInsertionReview = (restoreFocus = true) => {
      const recordId = activeComposerUncertaintyRecordId;
      if (!recordId || pendingComposerResolution?.recordId === recordId) return;
      activeComposerUncertaintyRecordId = null;
      render();
      scheduleLayout();
      if (restoreFocus) {
        focusComposerInsertionControl(recordId, '[data-action="insertRecord"]');
      }
    };

    const resolveComposerInsertion = (recordId, resolution) => {
      if (pendingComposerResolution || !["confirm", "retry"].includes(resolution)) return;
      const record = state.recentRecords.find((item) => item?.id === recordId);
      if (!record || !composerInsertionNeedsReview(record.insertionState)) return;
      const requestId = crypto.randomUUID();
      pendingComposerResolution = { requestId, recordId, resolution, startedAt: Date.now() };
      actionStatus = resolution === "confirm"
        ? "正在确认插入结果…"
        : "正在清除不确定回执并重试…";
      render();
      scheduleLayout();
      try {
        send("resolveComposerInsertion", { recordId, resolution }, requestId);
      } catch {
        pendingComposerResolution = null;
        actionStatus = "核对请求未发送，请重试";
        render();
        scheduleLayout();
        focusComposerInsertionControl(
          recordId,
          `[data-action="resolveComposerInsertion"][data-resolution="${resolution}"]`,
        );
      }
    };

    const reconcileComposerActionTimeouts = (now = Date.now()) => {
      const expiredRecordIds = [];
      for (const [requestId, pending] of pendingComposerInsertions) {
        if (!composerActionExpired(pending.startedAt, now)) continue;
        pendingComposerInsertions.delete(requestId);
        pendingActions.delete(requestId);
        expiredRecordIds.push(pending.recordId);
      }
      if (
        pendingComposerResolution
        && composerActionExpired(pendingComposerResolution.startedAt, now)
      ) {
        pendingActions.delete(pendingComposerResolution.requestId);
        expiredRecordIds.push(pendingComposerResolution.recordId);
        pendingComposerResolution = null;
      }
      if (!expiredRecordIds.length) return false;
      const reviewRecord = expiredRecordIds
        .map((recordId) => state.recentRecords.find((record) => record?.id === recordId))
        .find((record) => composerInsertionNeedsReview(record?.insertionState));
      activeComposerUncertaintyRecordId = reviewRecord?.id || null;
      actionStatus = "未收到上次操作回应；已按当前耐久回执恢复，不会自动重试";
      render();
      scheduleLayout();
      if (reviewRecord) {
        focusComposerInsertionControl(
          reviewRecord.id,
          '[data-action="resolveComposerInsertion"][data-resolution="confirm"]',
        );
      }
      return true;
    };

    const beginInlineEdit = (recordId, snapshot = null) => {
      const record = state.recentRecords.find((item) => item?.id === recordId);
      const item = recent.querySelector(`.item[data-record-id="${CSS.escape(recordId)}"]`);
      if (!record || !(item instanceof HTMLElement)) return;
      const body = item.querySelector(".item-body");
      if (!body) return;
      const editSession = ensureRecordEditSession(record, snapshot?.mutationId || "");
      if (!editSession) return;
      workspaceSelect.setDisabled(true);
      notebookSelect.setDisabled(true);
      item.dataset.editing = "true";
      const editor = element("div", "inline-edit");
      editor.dataset.mutationId = editSession.mutationId;
      editor.setAttribute("role", "group");
      editor.setAttribute("aria-label", "编辑记录正文和图片");
      const saving = pendingRecordEditRequest?.mutationId === editSession.mutationId;
      editor.setAttribute("aria-busy", String(saving));
      const validAttachmentIds = new Set(record.attachments.map((attachment) => attachment.id));
      const retainedAttachmentIds = new Set(
        Array.isArray(snapshot?.retainedAttachmentIds)
          ? snapshot.retainedAttachmentIds.filter((id) => validAttachmentIds.has(id))
          : [...validAttachmentIds],
      );
      editor.dataset.retainedAttachmentIds = JSON.stringify([...retainedAttachmentIds]);
      const dropOverlay = element("div", "image-drop-overlay inline-edit-drop-overlay");
      dropOverlay.hidden = true;
      dropOverlay.setAttribute("role", "status");
      dropOverlay.append(
        lucideIcon("image", 20),
        element("strong", "", "松开以添加到本次修改"),
        element("span", "", "图片会与正文和移除操作一起保存"),
      );
      const textarea = element("textarea");
      textarea.value = typeof record.bodyMarkdown === "string" ? record.bodyMarkdown : "";
      textarea.maxLength = 1048576;
      textarea.setAttribute("aria-label", "编辑速记");
      const toolbar = element("div", "inline-edit-toolbar");
      const addImages = button("", "", "addEditImages");
      setLabeledButton(addImages, "image", "添加图片");
      addImages.dataset.recordId = record.id;
      addImages.dataset.revision = String(record.revision || 0);
      const imageCount = element("span");
      toolbar.append(addImages, imageCount);
      const actions = element("div", "inline-edit-actions");
      const cancel = button("", "取消", "cancelEdit");
      const save = button("record", saving ? "正在保存…" : "保存", "saveEdit");
      save.dataset.recordId = record.id;
      save.dataset.revision = String(record.revision || 0);
      const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
      const targetBlocked = state.paused || notebookBlocksChanges(selectedNotebook);
      textarea.readOnly = targetBlocked || saving;
      const updateSaveState = () => {
        const retainedCount = retainedIdsFromEditor(editor, record).length;
        const pendingCount = recordEditSession?.recordId === record.id
          ? recordEditSession.files.length
          : 0;
        const blocked = targetBlocked || saving;
        save.disabled = blocked || (!textarea.value.trim() && retainedCount + pendingCount === 0);
        cancel.disabled = saving;
        addImages.disabled = blocked || retainedCount + pendingCount >= 10;
        imageCount.textContent = `${retainedCount + pendingCount}/10 张`;
      };
      textarea.addEventListener("input", updateSaveState);
      editor.append(dropOverlay, textarea, toolbar);
      if (record.attachments.length || editSession.files.length) {
        const gallery = element("div", "inline-edit-images");
        gallery.setAttribute("aria-label", "当前与待添加图片");
        record.attachments.forEach((attachment, index) => {
          const previewKey = `record:${attachment.id}`;
          const tile = element("div", "inline-edit-image");
          tile.dataset.attachmentId = attachment.id;
          tile.dataset.removed = String(!retainedAttachmentIds.has(attachment.id));
          const toggle = button("", "", "toggleEditAttachment");
          toggle.dataset.attachmentId = attachment.id;
          toggle.setAttribute("aria-pressed", String(!retainedAttachmentIds.has(attachment.id)));
          setIconButton(
            toggle,
            retainedAttachmentIds.has(attachment.id) ? "x" : "refresh",
            retainedAttachmentIds.has(attachment.id)
              ? `移除图片 ${index + 1}`
              : `撤销移除图片 ${index + 1}`,
          );
          tile.append(
            imagePreviewButton({
              previewKey,
              scope: "record",
              label: `图片 ${index + 1}`,
              available: attachment.available,
              recordId: record.id,
              attachmentId: attachment.id,
            }),
            ...(!retainedAttachmentIds.has(attachment.id)
              ? [element("span", "inline-edit-image-state", "待移除")]
              : []),
            toggle,
          );
          gallery.append(tile);
        });
        editSession.files.forEach((entry, index) => {
          const tile = element("div", "inline-edit-image");
          tile.dataset.previewKey = entry.previewKey;
          const remove = button("", "", "removeEditPendingImage");
          remove.dataset.previewKey = entry.previewKey;
          setIconButton(remove, "x", `移除待添加图片 ${index + 1}`);
          tile.append(
            imagePreviewButton({
              previewKey: entry.previewKey,
              scope: "recordEdit",
              label: entry.label,
              available: true,
            }),
            element("span", "inline-edit-image-state", "待添加"),
            remove,
          );
          gallery.append(tile);
        });
        editor.append(gallery);
      }
      editor.append(element("div", "inline-edit-hint", "可点击、粘贴或拖放图片；移除与新增会在保存时作为同一次修改生效。"));
      const error = element("div", "inline-edit-error");
      error.setAttribute("role", "alert");
      error.textContent = typeof snapshot?.errorMessage === "string" ? snapshot.errorMessage.slice(0, 240) : "";
      error.hidden = !error.textContent;
      editor.append(error);
      actions.append(cancel, save);
      editor.append(actions);
      body.replaceWith(editor);
      let editDropDepth = 0;
      textarea.addEventListener("paste", (event) => {
        const files = imageFilesFromTransfer(event.clipboardData);
        if (!files.length) return;
        event.preventDefault();
        queueRecordEditImageFiles(files, editor);
      });
      editor.addEventListener("dragenter", (event) => {
        if (!transferContainsFiles(event.dataTransfer)) return;
        event.preventDefault();
        editDropDepth += 1;
        dropOverlay.hidden = targetBlocked || saving;
      });
      editor.addEventListener("dragover", (event) => {
        if (!transferContainsFiles(event.dataTransfer)) return;
        event.preventDefault();
        const accepts = !targetBlocked && !saving;
        event.dataTransfer.dropEffect = accepts ? "copy" : "none";
        dropOverlay.hidden = !accepts;
      });
      editor.addEventListener("dragleave", (event) => {
        if (!transferContainsFiles(event.dataTransfer)) return;
        event.preventDefault();
        editDropDepth = Math.max(0, editDropDepth - 1);
        if (!editDropDepth) dropOverlay.hidden = true;
      });
      editor.addEventListener("drop", (event) => {
        if (!transferContainsFiles(event.dataTransfer)) return;
        event.preventDefault();
        editDropDepth = 0;
        dropOverlay.hidden = true;
        if (targetBlocked || saving) return;
        queueRecordEditImageFiles(imageFilesFromTransfer(event.dataTransfer), editor);
      });
      if (snapshot) applyInlineEditSnapshot(textarea, save, snapshot);
      else textarea.focus();
      updateSaveState();
      observeImagePreviews();
    };

    const beginTargetAction = (recordId, mode) => {
      const record = state.recentRecords.find((item) => item?.id === recordId);
      const item = recent.querySelector(`.item[data-record-id="${CSS.escape(recordId)}"]`);
      if (!record || !(item instanceof HTMLElement) || !["copy", "migrate"].includes(mode)) return;
      closeItemMenu();
      closeWakeSelect(false);
      item.querySelector(".target-panel")?.remove();
      const panel = element("div", "target-panel");
      panel.dataset.mode = mode;
      const targetSelect = createWakeSelect(
        "target-select",
        mode === "copy" ? "复制到" : "迁移到",
        null,
        190,
      );
      const targetOptions = [
        { value: "", label: "收件箱（仅本地）" },
        ...state.notebooks
          .filter((notebook) => notebook && typeof notebook.id === "string" && typeof notebook.displayName === "string")
          .filter((notebook) => (
            notebook.targetState === "ready"
            && !notebook.numberingSyncPending
            && !notebook.attachmentDirectorySyncPending
          ))
          .map((notebook) => ({
            value: notebook.id,
            label: notebook.displayName,
          })),
      ];
      const preferredTarget = targetOptions.some((option) => option.value === state.selectedNotebookId)
        ? state.selectedNotebookId
        : "";
      targetSelect.setOptions(targetOptions, preferredTarget || "");
      panel.__wakegptSelectControl = targetSelect;
      const label = element("span", "", mode === "copy" ? "选择副本目标" : "选择迁移目标");
      const actions = element("div", "target-panel-actions");
      const cancel = button("", "取消", "cancelTargetAction");
      const confirm = button("record", mode === "copy" ? "复制" : "迁移", "confirmTargetAction");
      confirm.dataset.recordId = record.id;
      confirm.dataset.revision = String(record.revision || 0);
      confirm.dataset.mode = mode;
      actions.append(cancel, confirm);
      panel.append(label, targetSelect.root, actions);
      item.append(panel);
      targetSelect.trigger.focus();
    };

    const layout = () => {
      layoutFrame = 0;
      if (!activeMount) return;
      if (globalThis.__wakegptActiveSessionNonce !== bootstrap.sessionNonce) {
        cleanup();
        return;
      }
      if (!host.isConnected && document.body) document.body.append(host);
      const visibleElements = visiblePageElements();
      const blockReason = pageBlockReason(visibleElements);
      if (blockReason) {
        closeItemMenu();
        closeWakeSelect(false);
        host.hidden = true;
        reportCardVisibility(blockReason);
        return;
      }
      const contextResolution = contextCardResolution(visibleElements);
      if (contextResolution.state === "ambiguous") {
        closeItemMenu();
        closeWakeSelect(false);
        host.hidden = true;
        reportCardVisibility("contextAmbiguous");
        if (!compatibilityFailureSent) {
          compatibilityFailureSent = true;
          try {
            send("adapterIncompatible", { code: "contextSelectorAmbiguous" });
          } finally {
            cleanup();
          }
        }
        return;
      }
      const context = contextResolution.node;
      if (!context) {
        closeItemMenu();
        closeWakeSelect(false);
        host.hidden = true;
        reportCardVisibility("contextMissing");
        return;
      }
      host.hidden = false;
      const rect = context.getBoundingClientRect();
      const top = Math.max(12, Math.round(rect.bottom + 14));
      const available = Math.max(0, Math.floor(window.innerHeight - top - 14));
      host.style.position = "fixed";
      host.style.zIndex = "2147483000";
      if (expandedOverlay) {
        const drawerWidth = Math.min(360, Math.max(280, Math.round(rect.width)));
        const drawerTop = Math.max(48, Math.round(rect.top));
        host.dataset.layout = "drawer";
        host.style.left = `${Math.max(12, Math.round(rect.left - drawerWidth - 12))}px`;
        host.style.top = `${drawerTop}px`;
        host.style.width = `${drawerWidth}px`;
        host.style.height = `${Math.max(240, window.innerHeight - drawerTop - 14)}px`;
        setIconButton(expandButton, "minimize", "收起速记面板");
      } else if (available >= 430) {
        host.dataset.layout = "full";
        host.style.left = `${Math.round(rect.left)}px`;
        host.style.top = `${top}px`;
        host.style.width = `${Math.round(rect.width)}px`;
        host.style.height = `${available}px`;
        setIconButton(expandButton, "maximize", "展开速记面板");
      } else if (available >= 220) {
        host.dataset.layout = "compact";
        host.style.left = `${Math.round(rect.left)}px`;
        host.style.top = `${top}px`;
        host.style.width = `${Math.round(rect.width)}px`;
        host.style.height = `${available}px`;
        setIconButton(expandButton, "maximize", "展开速记面板");
      } else {
        const fontSize = Number.parseFloat(getComputedStyle(host).fontSize) || 13;
        const collapsed = collapsedCardMetrics(rect.width, fontSize);
        host.dataset.layout = "collapsed";
        host.style.left = `${Math.max(12, Math.round(rect.left - collapsed.width - 12))}px`;
        host.style.top = `${Math.min(
          Math.max(12, Math.round(rect.top)),
          window.innerHeight - collapsed.height - 14,
        )}px`;
        host.style.width = `${collapsed.width}px`;
        host.style.height = `${collapsed.height}px`;
        setIconButton(expandButton, "maximize", "展开速记面板");
      }
      reportCardVisibility("visible", host.dataset.layout);
      if (context !== observedContext) {
        resizeObserver?.disconnect();
        observedContext = context;
        resizeObserver = new ResizeObserver(scheduleLayout);
        resizeObserver.observe(context);
        resizeObserver.observe(document.documentElement);
      }
      if (!itemMenu.hidden && itemMenuTrigger?.isConnected) positionItemMenu(itemMenuTrigger);
      if (!selectPopover.hidden && openSelectControl?.trigger.isConnected) positionSelectPopover();
    };

    const scheduleLayout = () => {
      if (!layoutFrame) layoutFrame = requestAnimationFrame(layout);
    };

    const setExpanded = (expanded) => {
      if (expandedOverlay === expanded) return;
      expandedOverlay = expanded;
      if (!expandedOverlay) {
        createPanel.hidden = true;
        closeWakeSelect(false);
      }
      scheduleLayout();
    };

    const submitRecord = () => {
      const bodyMarkdown = input.value.trimEnd();
      const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
      const targetBlocked = notebookBlocksChanges(selectedNotebook);
      if (state.paused || targetBlocked || (!bodyMarkdown && !state.pendingImageCount) || pendingCreate !== null || pendingCreateAck !== null) return;
      pendingCreate = send("createRecord", { notebookId: state.selectedNotebookId, bodyMarkdown });
      if (createRequestTimer) window.clearTimeout(createRequestTimer);
      createRequestTimer = window.setTimeout(() => {
        if (!pendingCreate) return;
        pendingActions.delete(pendingCreate);
        pendingCreate = null;
        createRequestTimer = 0;
        actionStatus = "未确认记录结果；再次点击会安全核对同一提交";
        render();
      }, 10000);
      actionStatus = "正在记录…";
      status.textContent = actionStatus;
      recordButton.disabled = true;
    };

    input.addEventListener("input", () => {
      const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
      const targetBlocked = notebookBlocksChanges(selectedNotebook);
      recordButton.disabled = state.paused || targetBlocked || (!input.value.trim() && !state.pendingImageCount) || pendingCreate !== null || pendingCreateAck !== null;
      queueDraftSave();
    });
    input.addEventListener("paste", (event) => {
      const files = imageFilesFromTransfer(event.clipboardData);
      if (!files.length) return;
      event.preventDefault();
      queueImageFiles(files);
    });
    composer.addEventListener("dragenter", (event) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      imageDropDepth += 1;
      setImageDropActive(canAcceptImageDrop());
    });
    composer.addEventListener("dragover", (event) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = canAcceptImageDrop() ? "copy" : "none";
      setImageDropActive(canAcceptImageDrop());
    });
    composer.addEventListener("dragleave", (event) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      imageDropDepth = Math.max(0, imageDropDepth - 1);
      if (!imageDropDepth) setImageDropActive(false);
    });
    composer.addEventListener("drop", (event) => {
      if (!transferContainsFiles(event.dataTransfer)) return;
      event.preventDefault();
      imageDropDepth = 0;
      setImageDropActive(false);
      const files = imageFilesFromTransfer(event.dataTransfer);
      if (!files.length) {
        actionStatus = "仅支持 PNG、JPEG、WebP 和 GIF 图片";
        status.textContent = actionStatus;
        return;
      }
      if (!canAcceptImageDrop()) return;
      queueImageFiles(files);
    });
    imageInput.addEventListener("change", () => {
      const files = imageFilesFromTransfer({ files: imageInput.files, items: [] });
      const editRecordId = imageInput.dataset.editRecordId || "";
      imageInput.value = "";
      delete imageInput.dataset.editRecordId;
      if (!files.length) return;
      if (editRecordId) {
        const editor = recent.querySelector(
          `.item[data-record-id="${CSS.escape(editRecordId)}"] .inline-edit`,
        );
        if (editor) queueRecordEditImageFiles(files, editor);
      } else {
        queueImageFiles(files);
      }
    });
    input.addEventListener("keydown", (event) => {
      if (event.isComposing || event.key !== "Enter") return;
      const submits = state.submitShortcut === "commandEnter"
        ? event.metaKey || event.ctrlKey
        : !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey;
      if (!submits) return;
      event.preventDefault();
      submitRecord();
    });
    shadow.addEventListener("click", (event) => {
      const target = event.target instanceof Element ? event.target.closest("button[data-action]") : null;
      if (!(target instanceof HTMLButtonElement)) return;
      const action = target.dataset.action;
      if (pendingImageRequest && action !== "toggleExpanded") return;
      if (
        recordEditSession
        && ![
          "beginEdit", "cancelEdit", "toggleEditAttachment", "saveEdit", "addEditImages",
          "removeEditPendingImage", "loadImagePreview", "openImagePreview", "closeImagePreview",
          "toggleExpanded", "openApp",
        ].includes(action)
      ) {
        actionStatus = "请先保存或取消当前记录修改";
        status.textContent = actionStatus;
        return;
      }
      if (action === "unpinNotebook") {
        send("unpinNotebook", { bodyMarkdown: input.value });
        actionStatus = "正在关闭 Tab…";
        status.textContent = actionStatus;
      } else if (action === "createRecord") {
        submitRecord();
      } else if (action === "insertRecord") {
        const recordId = target.dataset.recordId || "";
        const record = state.recentRecords.find((item) => item?.id === recordId);
        if (composerInsertionNeedsReview(record?.insertionState)) {
          openComposerInsertionReview(recordId);
          return;
        }
        const requestId = send("insertRecord", { recordId });
        pendingComposerInsertions.set(requestId, { recordId, startedAt: Date.now() });
        actionStatus = "正在插入到对话框…";
        render();
        scheduleLayout();
      } else if (action === "resolveComposerInsertion") {
        resolveComposerInsertion(
          target.dataset.recordId || "",
          target.dataset.resolution || "",
        );
      } else if (action === "cancelComposerInsertionReview") {
        closeComposerInsertionReview(true);
      } else if (action === "beginTargetAction") {
        beginTargetAction(target.dataset.recordId || "", target.dataset.mode || "");
      } else if (action === "cancelTargetAction") {
        closeWakeSelect(false);
        target.closest(".target-panel")?.remove();
      } else if (action === "confirmTargetAction") {
        const panel = target.closest(".target-panel");
        const targetSelect = panel?.__wakegptSelectControl;
        if (!targetSelect || typeof targetSelect.value !== "string") return;
        closeWakeSelect(false);
        const mode = target.dataset.mode;
        if (mode === "copy") {
          send("copyRecord", {
            recordId: target.dataset.recordId || "",
            destinationNotebookId: targetSelect.value || null,
          });
          actionStatus = "正在复制记录…";
        } else if (mode === "migrate") {
          send("migrateRecord", {
            recordId: target.dataset.recordId || "",
            expectedRevision: Number(target.dataset.revision || 0),
            destinationNotebookId: targetSelect.value || null,
          });
          actionStatus = "正在迁移记录…";
        }
        status.textContent = actionStatus;
      } else if (action === "toggleItemMenu") {
        openItemMenu(target, target.dataset.recordId || "");
      } else if (action === "pinRecord") {
        closeItemMenu();
        send("pinRecord", {
          recordId: target.dataset.recordId || "",
          expectedRevision: Number(target.dataset.revision || 0),
          pinned: target.dataset.pinned === "true",
        });
        actionStatus = target.dataset.pinned === "true" ? "正在置顶…" : "正在取消置顶…";
        status.textContent = actionStatus;
      } else if (action === "beginEdit") {
        const recordId = target.dataset.recordId || "";
        closeItemMenu();
        render();
        beginInlineEdit(recordId);
      } else if (action === "cancelEdit") {
        if (pendingRecordEditRequest) return;
        const recordId = recordEditSession?.recordId || "";
        clearRecordEditSession();
        render();
        requestAnimationFrame(() => {
          recent.querySelector(
            `.item[data-record-id="${CSS.escape(recordId)}"] [data-action="toggleItemMenu"]`,
          )?.focus({ preventScroll: true });
        });
      } else if (action === "toggleEditAttachment") {
        const editor = target.closest(".inline-edit");
        const item = editor?.closest(".item");
        const record = state.recentRecords.find((candidate) => candidate?.id === item?.dataset.recordId);
        const attachmentId = target.dataset.attachmentId || "";
        if (!editor || !record || !record.attachments.some((attachment) => attachment.id === attachmentId)) return;
        let retained = [];
        try {
          const parsed = JSON.parse(editor.dataset.retainedAttachmentIds || "[]");
          if (Array.isArray(parsed)) retained = parsed.filter((id) => typeof id === "string");
        } catch {}
        const retainedSet = new Set(retained);
        if (retainedSet.has(attachmentId)) retainedSet.delete(attachmentId);
        else retainedSet.add(attachmentId);
        retained = record.attachments
          .map((attachment) => attachment.id)
          .filter((id) => retainedSet.has(id));
        editor.dataset.retainedAttachmentIds = JSON.stringify(retained);
        const removed = !retainedSet.has(attachmentId);
        const tile = target.closest(".inline-edit-image");
        if (tile) {
          tile.dataset.removed = String(removed);
          tile.querySelector(".inline-edit-image-state")?.remove();
          if (removed) {
            tile.insertBefore(element("span", "inline-edit-image-state", "待移除"), target);
          }
        }
        const index = record.attachments.findIndex((attachment) => attachment.id === attachmentId);
        target.setAttribute("aria-pressed", String(removed));
        setIconButton(
          target,
          removed ? "refresh" : "x",
          removed ? `撤销移除图片 ${index + 1}` : `移除图片 ${index + 1}`,
        );
        const textarea = editor.querySelector("textarea");
        const save = editor.querySelector('[data-action="saveEdit"]');
        const addImages = editor.querySelector('[data-action="addEditImages"]');
        const imageCount = editor.querySelector(".inline-edit-toolbar span");
        const selectedNotebook = state.notebooks.find((notebook) => notebook?.id === state.selectedNotebookId);
        if (textarea && save) {
          const pendingCount = recordEditSession?.recordId === record.id
            ? recordEditSession.files.length
            : 0;
          save.disabled = state.paused
            || notebookBlocksChanges(selectedNotebook)
            || (!textarea.value.trim() && retained.length + pendingCount === 0);
          if (addImages) addImages.disabled = retained.length + pendingCount >= 10;
          if (imageCount) imageCount.textContent = `${retained.length + pendingCount}/10 张`;
        }
      } else if (action === "addEditImages") {
        if (pendingRecordEditRequest) return;
        imageInput.dataset.editRecordId = target.dataset.recordId || "";
        imageInput.click();
      } else if (action === "removeEditPendingImage") {
        removeRecordEditImage(target.dataset.previewKey || "");
      } else if (action === "saveEdit") {
        const editor = target.closest(".inline-edit");
        const textarea = editor?.querySelector("textarea");
        const recordId = target.dataset.recordId || "";
        const record = state.recentRecords.find((candidate) => candidate?.id === recordId);
        const editSession = recordEditSession;
        if (
          !(textarea instanceof HTMLTextAreaElement)
          || !editor
          || !record
          || !editSession
          || pendingRecordEditRequest
          || editSession.recordId !== recordId
          || editSession.expectedRevision !== Number(target.dataset.revision || 0)
        ) return;
        const retainedAttachmentIds = retainedIdsFromEditor(editor, record);
        const payload = {
          recordId,
          expectedRevision: editSession.expectedRevision,
          bodyMarkdown: textarea.value,
          retainedAttachmentIds,
          ...(editSession.files.length ? {
            uploadId: editSession.uploadId,
            images: editSession.files.map((entry, index) => ({
              index,
              byteSize: entry.file.size,
            })),
          } : {}),
        };
        try {
          send("editRecord", payload, editSession.mutationId);
          pendingRecordEditRequest = {
            mutationId: editSession.mutationId,
            recordId,
            expectedRevision: editSession.expectedRevision,
            startedAt: Date.now(),
          };
          editor.setAttribute("aria-busy", "true");
          textarea.readOnly = true;
          for (const control of editor.querySelectorAll("button")) control.disabled = true;
          target.textContent = "正在保存…";
        } catch {
          setInlineEditError(editor, "保存请求未发送，修改内容仍保留");
        }
        actionStatus = "正在保存修改…";
        status.textContent = actionStatus;
      } else if (action === "trashRecord") {
        if (target.dataset.confirmed !== "true") {
          target.dataset.confirmed = "true";
          target.textContent = "确认删除";
          window.setTimeout(() => {
            if (target.isConnected) {
              target.dataset.confirmed = "false";
              target.textContent = "删除";
            }
          }, 3200);
          return;
        }
        send("trashRecord", {
          recordId: target.dataset.recordId || "",
          expectedRevision: Number(target.dataset.revision || 0),
        });
        closeItemMenu();
        actionStatus = "正在移到废纸篓…";
        status.textContent = actionStatus;
      } else if (action === "chooseImages") {
        delete imageInput.dataset.editRecordId;
        imageInput.click();
      } else if (action === "loadImagePreview") {
        requestImagePreview(previewDescriptorFromButton(target), true);
        restoreInlineEditAfterLocalChange();
      } else if (action === "openImagePreview") {
        openImageLightbox(
          target.dataset.previewKey || "",
          target.dataset.label || "图片",
          target,
        );
      } else if (action === "closeImagePreview") {
        closeImageLightbox(true);
      } else if (action === "toggleExpanded") {
        setExpanded(!expandedOverlay);
      } else if (action === "toggleNotebookPanel") {
        if (host.dataset.layout === "compact" || host.dataset.layout === "collapsed") {
          setExpanded(true);
        }
        createPanel.hidden = !createPanel.hidden;
        if (createPanel.hidden) closeWakeSelect(false);
        if (!createPanel.hidden) notebookNameInput.focus();
      } else if (action === "newNotebook") {
        const displayName = notebookNameInput.value.trim();
        if (!displayName) {
          actionStatus = "请输入新速记本名称";
          status.textContent = actionStatus;
          notebookNameInput.focus();
          return;
        }
        const numberingStart = Number(numberingStartInput.value);
        if (!Number.isInteger(numberingStart) || numberingStart < 1 || numberingStart > 1000000000) {
          actionStatus = "起始序号需为 1–1,000,000,000 的整数";
          status.textContent = actionStatus;
          numberingStartInput.focus();
          return;
        }
        send("newNotebook", {
          displayName,
          numberingStyle: numberingSelect.value,
          numberingStart,
          attachmentDirectory: attachmentDirectoryInput.value.trim() || null,
          bodyMarkdown: input.value,
        });
        actionStatus = "正在新建速记本…";
        status.textContent = actionStatus;
      } else if (action === "bindNotebook") {
        const numberingStart = Number(numberingStartInput.value);
        if (!Number.isInteger(numberingStart) || numberingStart < 1 || numberingStart > 1000000000) {
          actionStatus = "起始序号需为 1–1,000,000,000 的整数";
          status.textContent = actionStatus;
          numberingStartInput.focus();
          return;
        }
        send("bindNotebook", {
          numberingStyle: numberingSelect.value,
          numberingStart,
          attachmentDirectory: attachmentDirectoryInput.value.trim() || null,
          bodyMarkdown: input.value,
        });
        actionStatus = "请选择工作区内的 Markdown…";
        status.textContent = actionStatus;
      } else if (action === "insertLink") {
        const start = input.selectionStart;
        const end = input.selectionEnd;
        const selection = input.value.slice(start, end) || "链接文字";
        input.setRangeText(`[${selection}](https://)`, start, end, "end");
        input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText" }));
        input.focus();
      } else if (action === "openApp") {
        send("openApp", {});
      }
    });
    imageLightbox.addEventListener("pointerdown", (event) => {
      if (event.target === imageLightbox) closeImageLightbox(true);
    });

    const handleItemMenuPointerDown = (event) => {
      if (itemMenu.hidden) return;
      const path = event.composedPath();
      if (path.includes(itemMenu) || (itemMenuTrigger && path.includes(itemMenuTrigger))) return;
      closeItemMenu();
    };
    const handleItemMenuKeyDown = (event) => {
      if (itemMenu.hidden) return;
      const menuHasFocus = itemMenu.contains(shadow.activeElement);
      if (event.key === "Escape") {
        event.preventDefault();
        closeItemMenu(true);
        return;
      }
      if (event.key === "Tab" && menuHasFocus) {
        event.preventDefault();
        event.stopImmediatePropagation();
        moveFocusFromItemMenu(event.shiftKey);
        return;
      }
      if (!menuHasFocus) return;
      const enabledItems = [...itemMenu.querySelectorAll("button:not(:disabled)")];
      if (!enabledItems.length || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      event.preventDefault();
      const currentIndex = enabledItems.indexOf(shadow.activeElement);
      const nextIndex = menuItemIndexAfterKey(currentIndex, enabledItems.length, event.key);
      enabledItems[nextIndex].focus({ preventScroll: true });
    };
    const handleItemMenuTriggerKeyDown = (event) => {
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const trigger = event.target instanceof Element
        ? event.target.closest("button[data-action='toggleItemMenu']")
        : null;
      if (!(trigger instanceof HTMLButtonElement)) return;
      event.preventDefault();
      event.stopPropagation();
      openItemMenu(
        trigger,
        trigger.dataset.recordId || "",
        event.key === "ArrowUp" ? "last" : "first",
      );
    };
    const handleItemMenuViewportChange = () => closeItemMenu();
    const handleSelectPointerDown = (event) => {
      if (selectPopover.hidden || !openSelectControl) return;
      const path = event.composedPath();
      if (path.includes(selectPopover) || path.includes(openSelectControl.root)) return;
      closeWakeSelect(false);
    };
    const handleSelectViewportChange = (event) => {
      if (selectPopover.hidden) return;
      if (event.type === "scroll" && event.target instanceof Node && selectPopover.contains(event.target)) return;
      closeWakeSelect(false);
    };
    const handleImageLightboxKeyDown = (event) => {
      if (imageLightbox.hidden) return;
      if (event.key === "Escape") {
        event.preventDefault();
        closeImageLightbox(true);
      } else if (event.key === "Tab") {
        event.preventDefault();
        imageLightboxClose.focus({ preventScroll: true });
      }
    };
    const handleComposerInsertionReviewKeyDown = (event) => {
      if (
        event.defaultPrevented
        || event.key !== "Escape"
        || !activeComposerUncertaintyRecordId
        || pendingComposerResolution
        || !imageLightbox.hidden
        || !itemMenu.hidden
        || !selectPopover.hidden
      ) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      closeComposerInsertionReview(true);
    };
    const handleInlineEditKeyDown = (event) => {
      if (
        event.defaultPrevented
        || event.key !== "Escape"
        || !recordEditSession
        || pendingRecordEditRequest
        || !imageLightbox.hidden
        || !itemMenu.hidden
        || !selectPopover.hidden
      ) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      const recordId = recordEditSession.recordId;
      clearRecordEditSession();
      render();
      requestAnimationFrame(() => {
        recent.querySelector(
          `.item[data-record-id="${CSS.escape(recordId)}"] [data-action="toggleItemMenu"]`,
        )?.focus({ preventScroll: true });
      });
    };
    const handlePanelKeyDown = (event) => {
      if (
        !event.defaultPrevented
        && event.key === "Escape" && expandedOverlay
        && imageLightbox.hidden
        && itemMenu.hidden
        && selectPopover.hidden
      ) {
        event.preventDefault();
        setExpanded(false);
        expandButton.focus({ preventScroll: true });
      }
    };
    document.addEventListener("pointerdown", handleItemMenuPointerDown, true);
    document.addEventListener("pointerdown", handleSelectPointerDown, true);
    document.addEventListener("scroll", handleSelectViewportChange, true);
    window.addEventListener("keydown", handleItemMenuKeyDown);
    recent.addEventListener("keydown", handleItemMenuTriggerKeyDown);
    window.addEventListener("keydown", handleImageLightboxKeyDown);
    window.addEventListener("keydown", handleComposerInsertionReviewKeyDown);
    window.addEventListener("keydown", handleInlineEditKeyDown);
    window.addEventListener("keydown", handlePanelKeyDown);
    window.addEventListener("blur", handleItemMenuViewportChange);
    window.addEventListener("blur", handleSelectViewportChange);
    window.addEventListener("resize", handleSelectViewportChange, { passive: true });
    cardScroll.addEventListener("scroll", handleItemMenuViewportChange, { passive: true });
    cardScroll.addEventListener("scroll", handleSelectViewportChange, { passive: true });

    const receiverName = `__wakegptReceive_${bootstrap.sessionNonce}`;
    const imageReceiverName = `__wakegptReceiveImage_${bootstrap.sessionNonce}`;
    globalThis[imageReceiverName] = (message) => {
      if (
        !message
        || message.schemaVersion !== 1
        || message.sessionNonce !== bootstrap.sessionNonce
        || typeof message.requestId !== "string"
        || typeof message.previewKey !== "string"
        || imagePreviewRequests.get(message.requestId) !== message.previewKey
      ) return false;
      lastHeartbeatAt = Date.now();
      if (message.phase === "start") {
        if (
          !Number.isSafeInteger(message.byteSize)
          || message.byteSize <= 0
          || message.byteSize > 20 * 1024 * 1024
          || !supportedImageTypes.has(message.mediaType)
        ) return false;
        imagePreviewBuffers.set(message.requestId, {
          previewKey: message.previewKey,
          mediaType: message.mediaType,
          bytes: new Uint8Array(message.byteSize),
          nextOffset: 0,
        });
        return true;
      }
      const transfer = imagePreviewBuffers.get(message.requestId);
      if (!transfer || transfer.previewKey !== message.previewKey) return false;
      if (message.phase === "chunk") {
        if (
          !Number.isSafeInteger(message.offset)
          || message.offset !== transfer.nextOffset
          || typeof message.hex !== "string"
          || message.hex.length === 0
          || message.hex.length > 1024 * 1024
          || message.hex.length % 2 !== 0
          || !/^[0-9a-f]+$/u.test(message.hex)
        ) return false;
        const byteLength = message.hex.length / 2;
        if (message.offset + byteLength > transfer.bytes.length) return false;
        for (let index = 0; index < byteLength; index += 1) {
          const value = Number.parseInt(message.hex.slice(index * 2, index * 2 + 2), 16);
          if (!Number.isInteger(value)) return false;
          transfer.bytes[message.offset + index] = value;
        }
        transfer.nextOffset += byteLength;
        return true;
      }
      if (message.phase !== "complete" || transfer.nextOffset !== transfer.bytes.length) return false;
      const prior = imagePreviewUrls.get(message.previewKey);
      if (prior) URL.revokeObjectURL(prior.url);
      const url = URL.createObjectURL(new Blob([transfer.bytes], { type: transfer.mediaType }));
      imagePreviewUrls.set(message.previewKey, { url, mediaType: transfer.mediaType });
      imagePreviewBuffers.delete(message.requestId);
      imagePreviewLoading.delete(message.previewKey);
      imagePreviewFailures.delete(message.previewKey);
      restoreInlineEditAfterLocalChange();
      return true;
    };
    globalThis[receiverName] = (message) => {
      if (!message || message.schemaVersion !== 1 || message.sessionNonce !== bootstrap.sessionNonce) return false;
      lastHeartbeatAt = Date.now();
      const completedAction = message.requestId ? pendingActions.get(message.requestId) : undefined;
      const completedComposerInsertionRecordId = message.requestId
        ? pendingComposerInsertions.get(message.requestId)?.recordId
        : undefined;
      const completedComposerResolution = message.requestId === pendingComposerResolution?.requestId
        ? pendingComposerResolution
        : null;
      const completedRecordEdit = message.requestId === pendingRecordEditRequest?.mutationId
        ? pendingRecordEditRequest
        : null;
      if (message.requestId) pendingActions.delete(message.requestId);
      if (message.requestId) pendingComposerInsertions.delete(message.requestId);
      const previewKey = message.requestId ? imagePreviewRequests.get(message.requestId) : undefined;
      if (message.requestId && previewKey) {
        imagePreviewRequests.delete(message.requestId);
        imagePreviewLoading.delete(previewKey);
        imagePreviewBuffers.delete(message.requestId);
        if (!message.ok) imagePreviewFailures.add(previewKey);
      }
      const inlineEditSnapshot = !message.requestId || completedRecordEdit || completedAction === "loadImagePreview"
        ? captureInlineEdit(recent, state.selectionKey)
        : null;
      const preserveTransientUi = !message.requestId && (
        recent.querySelector(".inline-edit,.target-panel") !== null || !itemMenu.hidden || !selectPopover.hidden
      );
      const previousSelectionKey = state.selectionKey;
      const previousWorkspaceStateSignature = workspaceStateSignature(state.workspaces);
      const previousNotebookStateSignature = notebookStateSignature(state.notebooks);
      if (message.state) state = normalizeState(message.state);
      const selectionChanged = state.selectionKey !== previousSelectionKey;
      const recordEditStale = Boolean(
        recordEditSession
        && (
          recordEditSession.selectionKey !== state.selectionKey
          || !state.recentRecords.some((record) => (
            record?.id === recordEditSession.recordId
            && Number(record.revision) === recordEditSession.expectedRevision
          ))
        )
      );
      const completedComposerInsertionUncertain = Boolean(
        completedComposerInsertionRecordId
        && state.recentRecords.some((record) => (
          record?.id === completedComposerInsertionRecordId
          && composerInsertionNeedsReview(record?.insertionState)
        )),
      );
      if (completedComposerInsertionUncertain) {
        activeComposerUncertaintyRecordId = completedComposerInsertionRecordId;
      }
      const composerUncertaintyInvalidated = Boolean(
        activeComposerUncertaintyRecordId
        && (
          selectionChanged
          || !state.recentRecords.some((record) => (
            record?.id === activeComposerUncertaintyRecordId
            && composerInsertionNeedsReview(record?.insertionState)
          ))
        )
      );
      if (composerUncertaintyInvalidated) activeComposerUncertaintyRecordId = null;
      const workspaceStateChanged = workspaceStateSignature(state.workspaces)
        !== previousWorkspaceStateSignature;
      const notebookStateChanged = notebookStateSignature(state.notebooks)
        !== previousNotebookStateSignature;
      if (message.requestId && message.requestId === pendingCreate) {
        if (createRequestTimer) {
          window.clearTimeout(createRequestTimer);
          createRequestTimer = 0;
        }
        if (message.ok) {
          input.value = "";
          pendingCreateAck = send("ackCreateRecord", {});
          if (createAckTimer) window.clearTimeout(createAckTimer);
          createAckTimer = window.setTimeout(() => {
            if (!pendingCreateAck) return;
            pendingActions.delete(pendingCreateAck);
            pendingCreateAck = null;
            createAckTimer = 0;
            input.value = state.draftBodyMarkdown;
            actionStatus = "正在核对记录完成状态；必要时可安全重试";
            render();
          }, 10000);
        }
        pendingCreate = null;
      }
      if (message.requestId && message.requestId === pendingCreateAck) {
        if (createAckTimer) {
          window.clearTimeout(createAckTimer);
          createAckTimer = 0;
        }
        if (!message.ok) input.value = state.draftBodyMarkdown;
        pendingCreateAck = null;
      }
      if (message.requestId && message.requestId === pendingImageRequest) {
        if (imageRequestTimer) {
          window.clearTimeout(imageRequestTimer);
          imageRequestTimer = 0;
        }
        globalThis.__wakegptDiscardPastedImages?.(
          bootstrap.sessionNonce,
          pendingImageRequest,
        );
        pendingImageRequest = null;
      }
      const responseStatus = typeof message.message === "string"
        ? message.message.slice(0, 240)
        : "";
      const restoreCompletedRecordEdit = Boolean(
        completedRecordEdit
        && !message.ok
        && !recordEditStale
        && canRestoreInlineEdit(inlineEditSnapshot, state)
      );
      if (completedRecordEdit) pendingRecordEditRequest = null;
      if (recordEditStale || completedRecordEdit && !restoreCompletedRecordEdit) {
        if (recordEditStale && pendingRecordEditRequest) {
          pendingActions.delete(pendingRecordEditRequest.mutationId);
          pendingRecordEditRequest = null;
        }
        clearRecordEditSession();
      }
      if (completedComposerResolution) {
        pendingComposerResolution = null;
        const uncertaintyRemains = state.recentRecords.some((record) => (
          record?.id === completedComposerResolution.recordId
          && composerInsertionNeedsReview(record?.insertionState)
        ));
        if (uncertaintyRemains) {
          activeComposerUncertaintyRecordId = completedComposerResolution.recordId;
        } else if (message.ok) {
          activeComposerUncertaintyRecordId = null;
        }
      }
      if (
        message.ok
        && ["selectWorkspace", "selectNotebook", "unpinNotebook", "newNotebook", "bindNotebook"].includes(completedAction)
      ) {
        input.value = state.draftBodyMarkdown;
      }
      if (!message.requestId && selectionChanged) {
        if (draftSaveTimer) {
          window.clearTimeout(draftSaveTimer);
          draftSaveTimer = 0;
        }
        send("saveDraft", {
          workspaceId: previousSelectionKey.split(":", 1)[0] === "none"
            ? null
            : previousSelectionKey.split(":", 1)[0],
          notebookId: previousSelectionKey.endsWith(":inbox")
            ? null
            : previousSelectionKey.slice(previousSelectionKey.indexOf(":") + 1),
          bodyMarkdown: input.value,
        });
        input.value = state.draftBodyMarkdown;
      }
      if (message.ok && message.requestId) {
        createPanel.hidden = true;
        notebookNameInput.value = "";
      }
      if (message.requestId) {
        if (completedAction !== "ackCreateRecord" || responseStatus) actionStatus = responseStatus;
      }
      if (
        !preserveTransientUi || selectionChanged || workspaceStateChanged || notebookStateChanged
        || composerUncertaintyInvalidated || recordEditStale
      ) {
        render();
        if (
          !notebookStateChanged
          && canRestoreInlineEdit(inlineEditSnapshot, state)
          && (!completedRecordEdit || restoreCompletedRecordEdit)
        ) {
          beginInlineEdit(inlineEditSnapshot.recordId, {
            ...inlineEditSnapshot,
            ...(completedRecordEdit && responseStatus ? { errorMessage: responseStatus } : {}),
          });
        } else if (completedRecordEdit && !restoreCompletedRecordEdit) {
          requestAnimationFrame(() => {
            recent.querySelector(
              `.item[data-record-id="${CSS.escape(completedRecordEdit.recordId)}"] [data-action="toggleItemMenu"]`,
            )?.focus({ preventScroll: true });
          });
        }
        if (completedComposerResolution && !message.ok && activeComposerUncertaintyRecordId) {
          focusComposerInsertionControl(
            completedComposerResolution.recordId,
            `[data-action="resolveComposerInsertion"][data-resolution="${completedComposerResolution.resolution}"]`,
          );
        } else if (
          completedComposerInsertionUncertain
          || completedComposerResolution
            && activeComposerUncertaintyRecordId === completedComposerResolution.recordId
        ) {
          focusComposerInsertionControl(
            activeComposerUncertaintyRecordId,
            '[data-action="resolveComposerInsertion"][data-resolution="confirm"]',
          );
        }
      }
      scheduleLayout();
      return true;
    };

    const observer = new MutationObserver((mutations) => {
      if (mutations.every((mutation) => mutation.target === host || host.contains(mutation.target))) {
        return;
      }
      scheduleLayout();
      syncComposerContext();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["aria-modal", "class", "data-state", "hidden", "open", "style"],
      childList: true,
      subtree: true,
    });
    window.addEventListener("resize", scheduleLayout, { passive: true });
    let heartbeatTimer = 0;
    const cleanup = () => {
      if (!activeMount) return;
      activeMount = false;
      activeComposerUncertaintyRecordId = null;
      pendingComposerResolution = null;
      pendingComposerInsertions.clear();
      pendingRecordEditRequest = null;
      clearRecordEditSession();
      observer.disconnect();
      resizeObserver?.disconnect();
      if (layoutFrame) cancelAnimationFrame(layoutFrame);
      if (heartbeatTimer) window.clearInterval(heartbeatTimer);
      if (draftSaveTimer) window.clearTimeout(draftSaveTimer);
      if (createRequestTimer) window.clearTimeout(createRequestTimer);
      if (createAckTimer) window.clearTimeout(createAckTimer);
      if (imageRequestTimer) window.clearTimeout(imageRequestTimer);
      if (pendingImageRequest) {
        globalThis.__wakegptDiscardPastedImages?.(
          bootstrap.sessionNonce,
          pendingImageRequest,
        );
      }
      window.removeEventListener("resize", scheduleLayout);
      document.removeEventListener("pointerdown", handleItemMenuPointerDown, true);
      document.removeEventListener("pointerdown", handleSelectPointerDown, true);
      document.removeEventListener("scroll", handleSelectViewportChange, true);
      window.removeEventListener("keydown", handleItemMenuKeyDown);
      recent.removeEventListener("keydown", handleItemMenuTriggerKeyDown);
      window.removeEventListener("keydown", handleImageLightboxKeyDown);
      window.removeEventListener("keydown", handleComposerInsertionReviewKeyDown);
      window.removeEventListener("keydown", handleInlineEditKeyDown);
      window.removeEventListener("keydown", handlePanelKeyDown);
      window.removeEventListener("blur", handleItemMenuViewportChange);
      window.removeEventListener("blur", handleSelectViewportChange);
      window.removeEventListener("resize", handleSelectViewportChange);
      cardScroll.removeEventListener("scroll", handleItemMenuViewportChange);
      cardScroll.removeEventListener("scroll", handleSelectViewportChange);
      imagePreviewObserver?.disconnect();
      closeImageLightbox(false);
      for (const preview of imagePreviewUrls.values()) URL.revokeObjectURL(preview.url);
      imagePreviewUrls.clear();
      imagePreviewBuffers.clear();
      imagePreviewLoading.clear();
      imagePreviewRequests.clear();
      delete globalThis[receiverName];
      delete globalThis[imageReceiverName];
      if (globalThis.__wakegptStopCodexCard === stopCard) {
        delete globalThis.__wakegptStopCodexCard;
      }
      if (globalThis.__wakegptActiveSessionNonce === bootstrap.sessionNonce) {
        delete globalThis.__wakegptActiveSessionNonce;
      }
      closeItemMenu();
      closeWakeSelect(false);
      host.remove();
    };
    const stopCard = (sessionNonce) => {
      if (sessionNonce !== bootstrap.sessionNonce) return false;
      cleanup();
      return true;
    };
    globalThis.__wakegptStopCodexCard = stopCard;
    heartbeatTimer = window.setInterval(() => {
      if (globalThis.__wakegptActiveSessionNonce !== bootstrap.sessionNonce) {
        cleanup();
        return;
      }
      syncComposerContext();
      const now = Date.now();
      reconcileComposerActionTimeouts(now);
      const heartbeatAge = now - lastHeartbeatAt;
      if (
        !pendingImageRequest
        && !pendingRecordEditRequest
        && heartbeatAge > 7500
        && (pendingActions.size === 0 || heartbeatAge > 15000)
      ) cleanup();
    }, 2500);
    window.addEventListener("pagehide", cleanup, { once: true });

    input.value = state.draftBodyMarkdown;
    render();
    syncComposerContext();
    scheduleLayout();
    return {
      ok: true,
      adapterVersion,
      hostContractId,
      sessionNonce: bootstrap.sessionNonce,
      bindingName: bootstrap.bindingName,
      receiverName,
    };
  };

  globalThis.__wakegptMountCodexCard = mount;
})();
