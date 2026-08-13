import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const chromeExecutable = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const profile = await mkdtemp(join(tmpdir(), "wakegpt-long-content-"));
const cardSource = await readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8");
const cardStyles = cardSource.match(/style\.textContent = `([\s\S]*?)`;/u)?.[1];
assert.ok(cardStyles, "Codex card styles are missing");

if (process.platform !== "darwin") {
  throw new Error("The long-content verifier currently requires the verified macOS platform");
}
if (typeof WebSocket === "undefined") {
  throw new Error("The long-content verifier requires a Node.js runtime with WebSocket support");
}

const availablePort = () => new Promise((resolve, reject) => {
  const server = createServer();
  server.unref();
  server.once("error", reject);
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    server.close(() => resolve(address.port));
  });
});

const waitFor = async (probe, label, timeoutMs = 12_000) => {
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

  async navigate(url) {
    await this.call("Page.navigate", { url });
  }

  waitFor(expression, timeoutMs = 8_000) {
    return waitFor(() => this.evaluate(expression), expression, timeoutMs);
  }

  close() {
    this.socket.close();
  }
}

const cdpPort = await availablePort();
const vitePort = await availablePort();
const previewUrl = `http://127.0.0.1:${vitePort}/`;
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
  await waitFor(
    () => fetch(`${previewUrl}?preview&long-content`).then((response) => response.ok),
    `Vite preview\n${viteLog}`,
  );
  const target = await waitFor(async () => {
    const targets = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then((response) => response.json());
    return targets.find((item) => item.type === "page");
  }, "Chrome DevTools page");
  const cdp = new CdpClient(target.webSocketDebuggerUrl);
  await cdp.open();

  const measure = async ({ url, width, height, ready }) => {
    await cdp.call("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await cdp.navigate(url);
    await cdp.waitFor(`document.readyState === "complete" && ${ready}`);
    await cdp.evaluate("document.documentElement.style.fontSize = '200%'; true");
    await cdp.waitFor("getComputedStyle(document.documentElement).fontSize === '32px'");
    return cdp.evaluate(`(() => {
      const scrollOwner = document.querySelector('.notes-view, .quick-capture-scroll');
      const composer = document.querySelector('.composer-card');
      const heading = document.querySelector('.view-heading');
      const firstCard = document.querySelector('.record-card');
      const firstActions = firstCard?.querySelector('.record-actions');
      const title = document.querySelector('.view-heading h1');
      const status = document.querySelector('.target-status');
      const titledElements = [title, status].filter(Boolean);
      const ownerRect = scrollOwner?.getBoundingClientRect();
      const horizontalOffenders = scrollOwner && ownerRect
        ? [...scrollOwner.querySelectorAll('*')]
          .map((element) => ({
            element,
            rect: element.getBoundingClientRect(),
          }))
          .filter(({ element, rect }) => element.scrollWidth > element.clientWidth + 1
            || rect.left < ownerRect.left - 1
            || rect.right > ownerRect.right + 1)
          .slice(0, 12)
          .map(({ element, rect }) => ({
            name: element.className || element.tagName,
            clientWidth: element.clientWidth,
            scrollWidth: element.scrollWidth,
            left: Math.round(rect.left),
            right: Math.round(rect.right),
          }))
        : [];
      if (scrollOwner) scrollOwner.scrollTop = scrollOwner.scrollHeight;
      const within = (child, parent) => {
        const childRect = child?.getBoundingClientRect();
        const parentRect = parent?.getBoundingClientRect();
        return Boolean(childRect && parentRect
          && childRect.left >= parentRect.left - 1
          && childRect.right <= parentRect.right + 1
          && childRect.top >= parentRect.top - 1
          && childRect.bottom <= parentRect.bottom + 1);
      };
      return {
        documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
        scrollOverflow: scrollOwner ? scrollOwner.scrollWidth - scrollOwner.clientWidth : null,
        atBottom: scrollOwner ? Math.abs(scrollOwner.scrollHeight - scrollOwner.clientHeight - scrollOwner.scrollTop) <= 2 : false,
        composerTop: composer?.getBoundingClientRect().top ?? null,
        headingHeight: heading?.getBoundingClientRect().height ?? null,
        actionsInCard: firstCard && firstActions ? within(firstActions, firstCard) : null,
        completeLabels: titledElements.every((element) => element.title && element.getAttribute('aria-label') !== ''),
        horizontalOffenders,
        errorOverlay: Boolean(document.querySelector('vite-error-overlay, .vite-error-overlay')),
      };
    })()`);
  };

  const main = await measure({
    url: `${previewUrl}?preview&long-content`,
    width: 960,
    height: 640,
    ready: "document.querySelectorAll('.record-card').length >= 3",
  });
  assert.equal(main.documentOverflow, 0, "main app document overflows horizontally");
  assert.equal(main.bodyOverflow, 0, "main app body overflows horizontally");
  assert.equal(
    main.scrollOverflow,
    0,
    `main app scroll owner overflows horizontally: ${JSON.stringify(main.horizontalOffenders)}`,
  );
  assert.equal(main.atBottom, true, "main app cannot reach the final long record");
  assert.ok(
    main.headingHeight > 0 && main.headingHeight <= 180,
    `main app heading is not bounded: ${main.headingHeight}`,
  );
  assert.ok(main.composerTop !== null && main.composerTop < 520, "main app composer is pushed out of the first viewport");
  assert.equal(main.actionsInCard, true, "long record actions escape their card");
  assert.equal(main.completeLabels, true, "bounded main app labels lose complete text metadata");
  assert.equal(main.errorOverlay, false, "main app Vite error overlay is visible");

  const quickCapture = await measure({
    url: `${previewUrl}?preview&long-content&surface=quick-capture`,
    width: 432,
    height: 520,
    ready: "Boolean(document.querySelector('.quick-capture-shell .composer-card'))",
  });
  assert.equal(quickCapture.documentOverflow, 0, "quick capture document overflows horizontally");
  assert.equal(quickCapture.bodyOverflow, 0, "quick capture body overflows horizontally");
  assert.equal(quickCapture.scrollOverflow, 0, "quick capture scroll owner overflows horizontally");
  assert.equal(quickCapture.atBottom, true, "quick capture cannot reach its final record");
  assert.equal(quickCapture.errorOverlay, false, "quick capture Vite error overlay is visible");

  await cdp.call("Emulation.setDeviceMetricsOverride", {
    width: 640,
    height: 480,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await cdp.navigate("about:blank");
  await cdp.waitFor("document.readyState === 'complete'");
  const targetHint = await cdp.evaluate(`(() => {
    document.documentElement.style.fontSize = "32px";
    const host = document.createElement("section");
    host.style.width = "600px";
    const shadow = host.attachShadow({ mode: "open" });
    const style = document.createElement("style");
    style.textContent = ${JSON.stringify(cardStyles)};
    const hint = document.createElement("div");
    hint.className = "target-hint";
    hint.dataset.tone = "local";
    hint.textContent = "当前没有已固定速记本；请在 WakeGPT 的“更多”中重新打开 Tab，或点击 + 新建/绑定。";
    shadow.append(style, hint);
    document.body.append(host);
    const computed = getComputedStyle(hint);
    return {
      clientHeight: hint.clientHeight,
      contentHeight: hint.clientHeight
        - Number.parseFloat(computed.paddingTop)
        - Number.parseFloat(computed.paddingBottom),
      lineHeight: Number.parseFloat(computed.lineHeight),
      scrollHeight: hint.scrollHeight,
    };
  })()`);
  assert.ok(
    targetHint.contentHeight + 1 >= targetHint.lineHeight * 2,
    `Codex target hint has less than two visible text lines: ${JSON.stringify(targetHint)}`,
  );
  assert.ok(
    targetHint.scrollHeight <= targetHint.clientHeight + 1,
    `Codex target hint clips the fixed two-line status: ${JSON.stringify(targetHint)}`,
  );

  await cdp.close();
  console.log("large-text long-content surfaces: 3/3 passed");
} finally {
  vite.kill("SIGTERM");
  chrome.kill("SIGTERM");
  await Promise.allSettled([waitForExit(vite), waitForExit(chrome)]);
  await rm(profile, { recursive: true, force: true });
}
