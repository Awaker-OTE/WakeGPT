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
const profile = await mkdtemp(join(tmpdir(), "wakegpt-dialog-large-text-"));
const cdpPort = await availablePort();
const vitePort = await availablePort();
const previewUrl = `http://127.0.0.1:${vitePort}/`;
const scenarios = [
  { name: "new notebook", actions: [{ aria: "新建笔记" }] },
  { name: "Markdown editor", actions: [{ text: "编辑 Markdown" }] },
  { name: "move record", actions: [{ aria: "移动记录" }] },
  { name: "local diagnostics", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "查看诊断" }] },
  { name: "local data reset", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "清除并重新开始" }] },
  { name: "numbering preview", actions: [{ text: "设置", waitForText: "本地诊断" }, { input: { aria: "当前速记本起始序号", value: "2" }, waitForEnabled: "预览更改" }, { text: "预览更改" }] },
  { name: "attachment preview", actions: [{ text: "设置", waitForText: "本地诊断" }, { input: { aria: "当前速记本附件目录", value: "notes/assets-v2" }, waitForEnabled: "预览迁移" }, { text: "预览迁移" }] },
  { name: "conflict inspector", conflict: true, actions: [{ text: "设置", waitForText: "处理同步冲突" }, { text: "查看并处理" }] },
  { name: "conflict confirmation", conflict: true, actions: [{ text: "设置", waitForText: "处理同步冲突" }, { text: "查看并处理" }, { text: "采用 WakeGPT 版本" }] },
  { name: "unbind confirmation", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "解除绑定" }] },
  { name: "plain Markdown confirmation", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "转为普通 Markdown" }] },
  { name: "trash notebook confirmation", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "移到废纸篓" }] },
  { name: "clear diagnostics confirmation", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "清空" }] },
  { name: "record trash cleanup confirmation", actions: [{ text: "设置", waitForText: "本地诊断" }, { text: "预览到期清理" }] },
];

if (process.platform !== "darwin") {
  throw new Error("The large-text dialog verifier currently requires the verified macOS platform");
}
if (typeof WebSocket === "undefined") {
  throw new Error("The large-text dialog verifier requires a Node.js runtime with WebSocket support");
}

function availablePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}

async function waitFor(probe, label, extra = "", timeoutMs = 12_000) {
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
  throw new Error(`${label} did not become ready${lastError ? `: ${lastError}` : ""}${extra ? `\n${extra}` : ""}`);
}

function waitForExit(child) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", resolve));
}

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
    const response = await this.call("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  }

  async navigate(url) {
    await this.call("Page.navigate", { url });
  }

  waitFor(expression, timeoutMs = 8_000) {
    return waitFor(() => this.evaluate(expression), expression, "", timeoutMs);
  }

  waitForButton({ text = null, aria = null }) {
    return this.waitFor(`[...document.querySelectorAll("button")].some((button) =>
      (${JSON.stringify(text)} === null || button.textContent.trim() === ${JSON.stringify(text)})
      && (${JSON.stringify(aria)} === null || button.getAttribute("aria-label") === ${JSON.stringify(aria)})
      && !button.disabled
    )`);
  }

  clickButton({ text = null, aria = null, nth = 0 }) {
    return this.evaluate(`(() => {
      const matches = [...document.querySelectorAll("button")].filter((button) =>
        (${JSON.stringify(text)} === null || button.textContent.trim() === ${JSON.stringify(text)})
        && (${JSON.stringify(aria)} === null || button.getAttribute("aria-label") === ${JSON.stringify(aria)})
        && !button.disabled
      );
      const button = matches[${nth}];
      if (!button) return false;
      button.scrollIntoView({ block: "center", inline: "nearest" });
      button.click();
      return true;
    })()`);
  }

  setInput(aria, value) {
    return this.evaluate(`(() => {
      const input = [...document.querySelectorAll("input")].find((item) => item.getAttribute("aria-label") === ${JSON.stringify(aria)});
      if (!input) return false;
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(input, ${JSON.stringify(value)});
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
      return true;
    })()`);
  }

  measureDialog() {
    return this.evaluate(`(() => {
      const dialog = [...document.querySelectorAll('[role="dialog"]')].at(-1);
      const rect = dialog.getBoundingClientRect();
      const clippedControls = [...dialog.querySelectorAll('button,input,textarea,select,a[href],[tabindex]:not([tabindex="-1"])')]
        .filter((control) => {
          const controlRect = control.getBoundingClientRect();
          return controlRect.width > 0 && (controlRect.left < rect.left - 1 || controlRect.right > rect.right + 1);
        })
        .map((control) => control.getAttribute("aria-label") || control.textContent.trim());
      dialog.scrollTop = dialog.scrollHeight;
      return {
        bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
        dialogOverflow: dialog.scrollWidth - dialog.clientWidth,
        clippedControls,
        atBottom: Math.abs(dialog.scrollHeight - dialog.clientHeight - dialog.scrollTop) <= 2,
        errorOverlay: Boolean(document.querySelector("vite-error-overlay, .vite-error-overlay")),
      };
    })()`);
  }

  close() {
    this.socket.close();
  }
}

const vite = spawn(process.platform === "win32" ? "npm.cmd" : "npm", ["run", "--silent", "dev", "--", "--host", "127.0.0.1", "--port", String(vitePort)], {
  cwd: appRoot,
  stdio: ["ignore", "pipe", "pipe"],
});
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
  await waitFor(() => fetch(`${previewUrl}?preview`).then((response) => response.ok), "Vite preview", viteLog);
  const target = await waitFor(async () => {
    const targets = await fetch(`http://127.0.0.1:${cdpPort}/json/list`).then((response) => response.json());
    return targets.find((item) => item.type === "page");
  }, "Chrome DevTools page");
  const cdp = new CdpClient(target.webSocketDebuggerUrl);
  await cdp.open();
  await cdp.call("Emulation.setDeviceMetricsOverride", {
    width: 960,
    height: 640,
    deviceScaleFactor: 1,
    mobile: false,
  });

  for (const scenario of scenarios) {
    await cdp.navigate(`${previewUrl}?preview&long-content${scenario.conflict ? "&conflict" : ""}`);
    await cdp.waitFor("document.readyState === 'complete' && document.querySelectorAll('button').length > 20");
    await cdp.evaluate("document.documentElement.style.fontSize = '200%'; true");
    for (const action of scenario.actions) {
      if (action.input) {
        assert.equal(
          await cdp.setInput(action.input.aria, action.input.value),
          true,
          `${scenario.name}: missing ${action.input.aria}`,
        );
        if (action.waitForEnabled) await cdp.waitForButton({ text: action.waitForEnabled });
        continue;
      }
      await cdp.waitForButton(action);
      assert.equal(await cdp.clickButton(action), true, `${scenario.name}: missing ${JSON.stringify(action)}`);
      if (action.waitForText) await cdp.waitFor(`document.body.innerText.includes(${JSON.stringify(action.waitForText)})`);
    }
    await cdp.waitFor("!!document.querySelector('[role=dialog]')");
    const result = await cdp.measureDialog();
    assert.equal(result.bodyOverflow, 0, `${scenario.name}: document overflows horizontally`);
    assert.equal(result.dialogOverflow, 0, `${scenario.name}: dialog overflows horizontally`);
    assert.deepEqual(result.clippedControls, [], `${scenario.name}: controls escape the dialog horizontally`);
    assert.equal(result.atBottom, true, `${scenario.name}: dialog cannot reach its bottom`);
    assert.equal(result.errorOverlay, false, `${scenario.name}: Vite error overlay is visible`);
  }

  await cdp.close();
  console.log(`large-text dialog matrix: ${scenarios.length}/${scenarios.length} passed`);
} finally {
  vite.kill("SIGTERM");
  chrome.kill("SIGTERM");
  await Promise.allSettled([waitForExit(vite), waitForExit(chrome)]);
  await rm(profile, { recursive: true, force: true });
}
