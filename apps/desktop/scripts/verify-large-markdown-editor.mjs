import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const chromeExecutable = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const profile = await mkdtemp(join(tmpdir(), "wakegpt-large-markdown-editor-"));

if (process.platform !== "darwin") {
  throw new Error("The large Markdown editor verifier currently requires the verified macOS platform");
}

const availablePort = () => new Promise((resolve, reject) => {
  const server = createServer();
  server.once("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    server.close(() => resolve(address.port));
  });
});

const waitFor = async (probe, label, timeoutMs = 20_000) => {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await delay(80);
  }
  throw new Error(`${label} did not become ready${lastError ? `: ${lastError}` : ""}`);
};

const waitForExit = (child) => child.exitCode !== null || child.signalCode !== null
  ? Promise.resolve()
  : new Promise((resolve) => child.once("exit", resolve));

class CdpClient {
  constructor(url) {
    this.url = url;
    this.nextId = 0;
    this.pending = new Map();
  }
  async open() {
    this.socket = new WebSocket(this.url);
    this.socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
      else pending.resolve(message.result);
    });
    await new Promise((resolve, reject) => {
      this.socket.addEventListener("open", resolve, { once: true });
      this.socket.addEventListener("error", reject, { once: true });
    });
  }
  call(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.nextId;
      this.pending.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }
  async evaluate(expression) {
    const response = await this.call("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  }
  waitFor(expression, timeoutMs) {
    return waitFor(() => this.evaluate(expression), expression, timeoutMs);
  }
  close() { this.socket.close(); }
}

const cdpPort = await availablePort();
const vitePort = await availablePort();
const previewUrl = `http://127.0.0.1:${vitePort}/?preview&large-markdown&delayed-markdown-save&crlf-markdown`;
const crPreviewUrl = `http://127.0.0.1:${vitePort}/?preview&large-markdown&cr-markdown`;
const previewNotebookId = "018f2222-2222-7222-8222-222222222222";
const alternateNotebookId = "018f3333-3333-7333-8333-333333333333";
const secondPageMarker = "WAKEGPT_SECOND_EDIT_PAGE_REGRESSION";
const vite = spawn(process.platform === "win32" ? "npm.cmd" : "npm", [
  "run", "--silent", "dev", "--", "--host", "127.0.0.1", "--port", String(vitePort),
], { cwd: appRoot, stdio: ["ignore", "pipe", "pipe"] });
const chrome = spawn(chromeExecutable, [
  "--headless=new",
  "--disable-gpu",
  "--no-first-run",
  "--no-default-browser-check",
  "--remote-debugging-address=127.0.0.1",
  `--remote-debugging-port=${cdpPort}`,
  `--user-data-dir=${profile}`,
  "about:blank",
], { stdio: "ignore" });

let viteLog = "";
vite.stdout.on("data", (chunk) => { viteLog += chunk; });
vite.stderr.on("data", (chunk) => { viteLog += chunk; });

try {
  await waitFor(() => fetch(previewUrl).then((response) => response.ok), `Vite preview\n${viteLog}`);
  const target = await waitFor(async () => {
    const targets = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then((response) => response.json());
    return targets.find((item) => item.type === "page");
  }, "Chrome DevTools page");
  const cdp = new CdpClient(target.webSocketDebuggerUrl);
  await cdp.open();
  const browserConsole = [];
  await cdp.call("Runtime.enable");
  cdp.socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.method === "Runtime.consoleAPICalled") {
      browserConsole.push(message.params.args.map((argument) => argument.value ?? argument.description).join(" "));
    }
    if (message.method === "Runtime.exceptionThrown") {
      browserConsole.push(message.params.exceptionDetails?.exception?.description ?? message.params.exceptionDetails?.text);
    }
  });
  await cdp.call("Emulation.setDeviceMetricsOverride", {
    width: 960,
    height: 640,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await cdp.call("Page.navigate", { url: previewUrl });
  await cdp.waitFor("document.readyState === 'complete' && [...document.querySelectorAll('button')].some((button) => button.textContent.trim() === '编辑 Markdown')");

  const openStarted = performance.now();
  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find((item) => item.textContent.trim() === '编辑 Markdown');
    button?.click();
    return Boolean(button);
  })()`), true);
  try {
  await cdp.waitFor("Boolean(document.querySelector('textarea[aria-label^=\"Markdown 编辑页\"]')?.value.length > 100_000 && document.querySelector('.large-markdown-edit-pagination'))", 20_000);
  } catch (error) {
    const state = await cdp.evaluate(`(() => ({
      bodyText: document.body.innerText.slice(0, 500),
      bodyHtml: document.body.innerHTML.slice(0, 500),
      dialogCount: document.querySelectorAll('[role=dialog]').length,
      textareaLength: document.querySelector('textarea[aria-label^="Markdown 编辑页"]')?.value.length ?? null,
      loading: document.querySelector('.document-loading')?.textContent ?? null,
      errorOverlay: Boolean(document.querySelector('vite-error-overlay, .vite-error-overlay')),
      readyState: document.readyState,
      url: location.href,
      console: ${JSON.stringify([])},
    }))()`);
    state.console = browserConsole;
    throw new Error(`${error}; state=${JSON.stringify(state)}`);
  }
  const openMs = performance.now() - openStarted;
  assert.ok(openMs < 8_000, `opening the 16 MiB editor took ${Math.round(openMs)} ms`);

  const initial = await cdp.evaluate(`(() => {
    const dialog = document.querySelector('[role=dialog]');
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const tabs = [...document.querySelectorAll('[role=tab]')].map((tab) => ({
      text: tab.textContent.trim(), selected: tab.getAttribute('aria-selected'),
    }));
    return {
      valueLength: textarea?.value.length ?? 0,
      dialogOverflow: dialog ? dialog.scrollWidth - dialog.clientWidth : null,
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      tabs,
      errorOverlay: Boolean(document.querySelector('vite-error-overlay, .vite-error-overlay')),
    };
  })()`);
  assert.ok(initial.valueLength > 100_000 && initial.valueLength < 500_000);
  assert.equal(initial.dialogOverflow, 0);
  assert.equal(initial.documentOverflow, 0);
  assert.equal(initial.errorOverlay, false);

  const previewStarted = performance.now();
  assert.equal(await cdp.evaluate(`(() => {
    const tab = [...document.querySelectorAll('[role=tab]')].find((item) => item.textContent.trim() === '预览');
    tab?.click();
    return Boolean(tab);
  })()`), true);
  await cdp.waitFor("Boolean(document.querySelector('.markdown-preview-pagination') && document.querySelector('.markdown-preview h2'))", 12_000);
  const previewMs = performance.now() - previewStarted;
  assert.ok(previewMs < 4_000, `opening the first paged preview took ${Math.round(previewMs)} ms`);
  const preview = await cdp.evaluate(`(() => ({
    pageStatus: document.querySelector('.markdown-preview-pagination [role=status]')?.textContent.trim(),
    headings: document.querySelectorAll('.markdown-preview h2').length,
    nextEnabled: ![...document.querySelectorAll('.markdown-preview-pagination button')]
      .find((button) => button.textContent.trim() === '下一页')?.disabled,
  }))()`);
  assert.match(preview.pageStatus, /^第 1 \/ \d+ 页$/u);
  assert.ok(preview.headings > 100);
  assert.equal(preview.nextEnabled, true);

  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('.markdown-preview-pagination button')]
      .find((item) => item.textContent.trim() === '下一页');
    button?.focus();
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('.markdown-preview-pagination [role=status]')?.textContent.trim().startsWith('第 2 /')");
  assert.equal(await cdp.evaluate(`document.activeElement?.matches('article[aria-label^="Markdown 预览第 2 页"]')`), true);

  assert.equal(await cdp.evaluate(`(() => {
    const tab = [...document.querySelectorAll('[role=tab]')].find((item) => item.textContent.trim() === '分栏');
    tab?.click();
    return Boolean(tab);
  })()`), true);
  await cdp.waitFor("Boolean(document.querySelector('.large-markdown-preview-notice') && document.querySelector('textarea[aria-label^=\"Markdown 编辑页\"]'))");
  await delay(1_000);

  const baseline = await cdp.evaluate(`(async () => {
    const documentState = await window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
      request: { notebookId: ${JSON.stringify(previewNotebookId)} },
    });
    const firstPage = document.querySelector('textarea[aria-label^="Markdown 编辑页"]').value;
    window.__wakegptLargeMarkdownRegression = {
      baseline: documentState.markdown,
      firstPage,
    };
    return {
      fullLength: documentState.markdown.length,
      firstPageLength: firstPage.length,
      firstPageMatches: documentState.markdown.split('\\r\\n').join('\\n').startsWith(firstPage),
      lineEnding: documentState.lineEnding,
      hadUtf8Bom: documentState.hadUtf8Bom,
    };
  })()`);
  assert.ok(baseline.fullLength > 15 * 1024 * 1024);
  assert.ok(baseline.firstPageLength > 100_000 && baseline.firstPageLength < 500_000);
  assert.equal(baseline.firstPageMatches, true);
  assert.equal(baseline.lineEnding, "\r\n");
  assert.equal(baseline.hadUtf8Bom, true);

  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('.large-markdown-edit-pagination button')]
      .find((item) => item.textContent.trim() === '下一编辑页');
    button?.focus();
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('.large-markdown-edit-pagination [role=status]')?.textContent.trim().startsWith('编辑第 2 /')");
  assert.equal(await cdp.evaluate(`document.activeElement?.matches('textarea[aria-label^="Markdown 编辑页 2"]')`), true);

  const inputMeasurement = await cdp.evaluate(`(() => new Promise((resolve) => {
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const regression = window.__wakegptLargeMarkdownRegression;
    const originalPage = textarea.value;
    const insertionOffset = Math.min(192, Math.max(1, originalPage.length - 1));
    const normalizedBaseline = regression.baseline.split('\\r\\n').join('\\n');
    const boundary = regression.firstPage.length;
    const pageMatchesBaseline = normalizedBaseline.slice(
      boundary,
      boundary + originalPage.length,
    ) === originalPage;
    const nextPage = originalPage.slice(0, insertionOffset)
      + ${JSON.stringify(secondPageMarker)}
      + originalPage.slice(insertionOffset);
    regression.boundary = boundary;
    regression.insertionOffset = insertionOffset;
    regression.originalSecondPage = originalPage;
    const expectedNormalized = normalizedBaseline.slice(0, boundary)
      + nextPage
      + normalizedBaseline.slice(boundary + originalPage.length);
    regression.expected = expectedNormalized.split('\\n').join('\\r\\n');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    const start = performance.now();
    setter.call(textarea, nextPage);
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    const dispatchMs = performance.now() - start;
    requestAnimationFrame(() => requestAnimationFrame(() => resolve({
      dispatchMs,
      settledMs: performance.now() - start,
      pageMatchesBaseline,
    })));
  }))()`);
  assert.equal(inputMeasurement.pageMatchesBaseline, true);
  assert.ok(
    inputMeasurement.dispatchMs < 300,
    `large-document input dispatch took ${Math.round(inputMeasurement.dispatchMs)} ms`,
  );
  assert.ok(
    inputMeasurement.settledMs < 2_500,
    `large-document UI settlement took ${Math.round(inputMeasurement.settledMs)} ms`,
  );

  assert.equal(await cdp.evaluate(`(() => {
    const tab = document.querySelector('#wakegpt-notebook-tab-${alternateNotebookId}');
    tab?.click();
    return Boolean(tab);
  })()`), true);
  await cdp.waitFor(`document.querySelector('#wakegpt-notebook-tab-${alternateNotebookId}')?.getAttribute('aria-selected') === 'true'`);
  const frozenSession = await cdp.evaluate(`(() => ({
    title: document.querySelector('[role=dialog] h2')?.textContent.trim(),
    editorStillOpen: Boolean(document.querySelector('textarea[aria-label^="Markdown 编辑页 2"]')),
  }))()`);
  assert.equal(frozenSession.title, "编辑 产品记录");
  assert.equal(frozenSession.editorStillOpen, true);
  await cdp.waitFor("document.querySelector('.large-markdown-preview-notice')?.textContent.includes('有未刷新修改')");
  const stale = await cdp.evaluate(`(() => ({
    refreshEnabled: ![...document.querySelectorAll('.large-markdown-preview-notice button')][0]?.disabled,
    pageStillRendered: Boolean(document.querySelector('.markdown-preview h2')),
    saveEnabled: ![...document.querySelectorAll('[role=dialog] button')]
      .find((button) => button.textContent.trim() === '保存全文')?.disabled,
    editPageStatus: document.querySelector('.large-markdown-edit-pagination [role=status]')?.textContent.trim(),
  }))()`);
  assert.equal(stale.refreshEnabled, true);
  assert.equal(stale.pageStillRendered, true);
  assert.equal(stale.saveEnabled, true);
  assert.match(stale.editPageStatus, /^编辑第 2 \/ \d+ 页$/u);

  assert.equal(await cdp.evaluate(`(() => {
    const button = document.querySelector('.large-markdown-preview-notice button');
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('.large-markdown-preview-notice')?.textContent.includes('已是最新') && document.querySelector('.markdown-preview-pagination [role=status]')?.textContent.trim().startsWith('第 1 /')");
  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('.markdown-preview-pagination button')]
      .find((item) => item.textContent.trim() === '下一页');
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('.markdown-preview')?.textContent.includes('WAKEGPT_SECOND_EDIT_PAGE_REGRESSION')");

  const dialogBottom = await cdp.evaluate(`(() => {
    const dialog = document.querySelector('[role=dialog]');
    dialog.scrollTop = dialog.scrollHeight;
    return {
      atBottom: Math.abs(dialog.scrollHeight - dialog.clientHeight - dialog.scrollTop) <= 2,
      saveVisible: (() => {
        const button = [...dialog.querySelectorAll('button')].find((item) => item.textContent.trim() === '保存全文');
        const a = button.getBoundingClientRect();
        const b = dialog.getBoundingClientRect();
        return a.top >= b.top && a.bottom <= b.bottom;
      })(),
    };
  })()`);
  assert.equal(dialogBottom.atBottom, true);
  assert.equal(dialogBottom.saveVisible, true);

  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('[role=dialog] button')]
      .find((item) => item.textContent.trim() === '保存全文');
    button?.click();
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('[role=dialog] button[aria-label=\"关闭\"]')?.disabled === true");
  const savingLock = await cdp.evaluate(`(() => {
    const dialog = document.querySelector('[role=dialog]');
    const close = dialog?.querySelector('button[aria-label="关闭"]');
    const cancel = [...dialog.querySelectorAll('button')]
      .find((button) => button.textContent.trim() === '取消');
    const textarea = dialog?.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const valueBefore = textarea?.value;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(textarea, valueBefore + 'SHOULD_NOT_SAVE_DURING_LOCK');
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    close?.click();
    cancel?.click();
    return {
      closeDisabled: close?.disabled,
      cancelDisabled: cancel?.disabled,
      editorReadOnly: textarea?.readOnly,
      valueChangedBySyntheticInput: textarea?.value !== valueBefore,
      stillOpen: Boolean(document.querySelector('[role=dialog]')),
    };
  })()`);
  assert.equal(savingLock.closeDisabled, true);
  assert.equal(savingLock.cancelDisabled, true);
  assert.equal(savingLock.editorReadOnly, true);
  assert.equal(savingLock.valueChangedBySyntheticInput, true);
  assert.equal(savingLock.stillOpen, true);
  const crossOperationUnlock = await cdp.evaluate(`(async () => {
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const invoke = window.__TAURI_INTERNALS__.invoke;
    const recordWrite = invoke('create_record', {
      request: {
        mutationId: crypto.randomUUID(),
        mutationSchemaVersion: 1,
        workspaceId: ${JSON.stringify("018f1111-1111-7111-8111-111111111111")},
        notebookId: ${JSON.stringify(previewNotebookId)},
        bodyMarkdown: 'INDEPENDENT_BUSY_REGRESSION',
        attachmentTokens: [],
      },
    });
    await recordWrite;
    await new Promise((resolve) => setTimeout(resolve, 0));
    const dialog = document.querySelector('[role=dialog]');
    return {
      closeDisabled: dialog?.querySelector('button[aria-label="关闭"]')?.disabled,
      cancelDisabled: [...dialog.querySelectorAll('button')]
        .find((button) => button.textContent.trim() === '取消')?.disabled,
      editorReadOnly: textarea?.readOnly,
    };
  })()`);
  assert.deepEqual(crossOperationUnlock, {
    closeDisabled: true,
    cancelDisabled: true,
    editorReadOnly: true,
  });
  await cdp.waitFor("!document.querySelector('[role=dialog]') && [...document.querySelectorAll('button')].some((button) => button.textContent.trim() === '编辑 Markdown')");

  const saved = await cdp.evaluate(`(async () => {
    const regression = window.__wakegptLargeMarkdownRegression;
    const documentState = await window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
      request: { notebookId: ${JSON.stringify(previewNotebookId)} },
    });
    return {
      exact: documentState.markdown === regression.expected,
      length: documentState.markdown.length,
      expectedLength: regression.expected.length,
      markerCount: documentState.markdown.split(${JSON.stringify(secondPageMarker)}).length - 1,
      receiptGeneration: documentState.receiptGeneration,
      preservesCrlf: documentState.markdown.split('\\r\\n').join('').includes('\\n') === false,
      lineEnding: documentState.lineEnding,
      hadUtf8Bom: documentState.hadUtf8Bom,
    };
  })()`);
  assert.equal(saved.exact, true);
  assert.equal(saved.length, saved.expectedLength);
  assert.equal(saved.markerCount, 1);
  assert.equal(saved.receiptGeneration, 2);
  assert.equal(saved.preservesCrlf, true);
  assert.equal(saved.lineEnding, "\r\n");
  assert.equal(saved.hadUtf8Bom, true);
  assert.equal(await cdp.evaluate(`window.__TAURI_INTERNALS__.invoke('preview_markdown_save_count')`), 1);
  assert.equal(await cdp.evaluate(`window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
    request: { notebookId: ${JSON.stringify(previewNotebookId)} },
  }).then((documentState) => documentState.markdown.includes('SHOULD_NOT_SAVE_DURING_LOCK'))`), false);
  const alternateDocument = await cdp.evaluate(`window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
    request: { notebookId: ${JSON.stringify(alternateNotebookId)} },
  })`);
  assert.equal(alternateDocument.receiptGeneration, 1);
  assert.equal(alternateDocument.markdown.includes(secondPageMarker), false);

  assert.equal(await cdp.evaluate(`(() => {
    const tab = document.querySelector('#wakegpt-notebook-tab-${previewNotebookId}');
    tab?.click();
    return Boolean(tab);
  })()`), true);
  await cdp.waitFor(`document.querySelector('#wakegpt-notebook-tab-${previewNotebookId}')?.getAttribute('aria-selected') === 'true'`);

  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')]
      .find((item) => item.textContent.trim() === '编辑 Markdown');
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("Boolean(document.querySelector('textarea[aria-label^=\"Markdown 编辑页\"]')?.value.length > 100_000 && document.querySelector('.large-markdown-edit-pagination'))", 20_000);

  const reopenedFirstPage = await cdp.evaluate(`(() => {
    const regression = window.__wakegptLargeMarkdownRegression;
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    return textarea.value === regression.firstPage;
  })()`);
  assert.equal(reopenedFirstPage, true);

  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('.large-markdown-edit-pagination button')]
      .find((item) => item.textContent.trim() === '下一编辑页');
    button?.focus();
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("document.querySelector('.large-markdown-edit-pagination [role=status]')?.textContent.trim().startsWith('编辑第 2 /')");
  assert.equal(await cdp.evaluate(`document.activeElement?.matches('textarea[aria-label^="Markdown 编辑页 2"]')`), true);

  const reopened = await cdp.evaluate(`(async () => {
    const regression = window.__wakegptLargeMarkdownRegression;
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const expectedSecondPagePrefix = regression.originalSecondPage.slice(0, regression.insertionOffset)
      + ${JSON.stringify(secondPageMarker)};
    const documentState = await window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
      request: { notebookId: ${JSON.stringify(previewNotebookId)} },
    });
    return {
      secondPageStartsWithExpectedEdit: textarea.value.startsWith(expectedSecondPagePrefix),
      secondPageContainsMarker: textarea.value.includes(${JSON.stringify(secondPageMarker)}),
      fullDocumentStillExact: documentState.markdown === regression.expected,
    };
  })()`);
  assert.equal(reopened.secondPageStartsWithExpectedEdit, true);
  assert.equal(reopened.secondPageContainsMarker, true);
  assert.equal(reopened.fullDocumentStillExact, true);

  assert.equal(await cdp.evaluate(`(() => {
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页 2"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(textarea, textarea.value + 'UNSAVED_DISCARD_GUARD');
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    return Boolean(textarea);
  })()`), true);
  await cdp.waitFor("![...document.querySelectorAll('[role=dialog] button')].find((button) => button.textContent.trim() === '保存全文')?.disabled");
  assert.equal(await cdp.evaluate(`(() => {
    const cancel = [...document.querySelectorAll('[role=dialog] button')]
      .find((button) => button.textContent.trim() === '取消');
    cancel?.click();
    return Boolean(cancel);
  })()`), true);
  await cdp.waitFor("[...document.querySelectorAll('[role=dialog] h2')].some((heading) => heading.textContent.includes('放弃尚未保存'))");
  assert.equal(await cdp.evaluate(`document.querySelectorAll('[role=dialog]').length`), 2);
  assert.equal(await cdp.evaluate(`(() => {
    const dialogs = [...document.querySelectorAll('[role=dialog]')];
    const keepEditing = [...dialogs.at(-1).querySelectorAll('button')]
      .find((button) => button.textContent.trim() === '取消');
    keepEditing?.click();
    return Boolean(keepEditing);
  })()`), true);
  await cdp.waitFor("document.querySelectorAll('[role=dialog]').length === 1 && Boolean(document.querySelector('textarea[aria-label^=\"Markdown 编辑页 2\"]'))");

  await cdp.call("Page.navigate", { url: crPreviewUrl });
  await cdp.waitFor("document.readyState === 'complete' && [...document.querySelectorAll('button')].some((button) => button.textContent.trim() === '编辑 Markdown')");
  assert.equal(await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')]
      .find((item) => item.textContent.trim() === '编辑 Markdown');
    button?.click();
    return Boolean(button);
  })()`), true);
  await cdp.waitFor("Boolean(document.querySelector('textarea[aria-label^=\"Markdown 编辑页\"]') && document.querySelector('.large-markdown-edit-pagination'))", 20_000);
  const crBaseline = await cdp.evaluate(`(async () => {
    const documentState = await window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
      request: { notebookId: ${JSON.stringify(previewNotebookId)} },
    });
    return {
      lineEnding: documentState.lineEnding,
      noLf: !documentState.markdown.includes('\\n'),
      hasCr: documentState.markdown.includes('\\r'),
    };
  })()`);
  assert.deepEqual(crBaseline, { lineEnding: "\r", noLf: true, hasCr: true });
  assert.equal(await cdp.evaluate(`(() => {
    const textarea = document.querySelector('textarea[aria-label^="Markdown 编辑页"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(textarea, textarea.value + ${JSON.stringify("CR_ONLY_EDITOR_REGRESSION")});
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    const save = [...document.querySelectorAll('[role=dialog] button')]
      .find((button) => button.textContent.trim() === '保存全文');
    save?.click();
    return Boolean(textarea && save);
  })()`), true);
  await cdp.waitFor("!document.querySelector('[role=dialog]')");
  const crSaved = await cdp.evaluate(`(async () => {
    const documentState = await window.__TAURI_INTERNALS__.invoke('read_notebook_document', {
      request: { notebookId: ${JSON.stringify(previewNotebookId)} },
    });
    return {
      lineEnding: documentState.lineEnding,
      noLf: !documentState.markdown.includes('\\n'),
      hasCr: documentState.markdown.includes('\\r'),
      markerCount: documentState.markdown.split('CR_ONLY_EDITOR_REGRESSION').length - 1,
      hadUtf8Bom: documentState.hadUtf8Bom,
    };
  })()`);
  assert.deepEqual(crSaved, {
    lineEnding: "\r",
    noLf: true,
    hasCr: true,
    markerCount: 1,
    hadUtf8Bom: true,
  });
  await cdp.close();
  console.log("large Markdown editor browser matrix: ok");
} finally {
  vite.kill("SIGTERM");
  chrome.kill("SIGTERM");
  await Promise.allSettled([waitForExit(vite), waitForExit(chrome)]);
  await rm(profile, { recursive: true, force: true });
}
