import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [configSource, capabilitySource, lifecycle, storage, commands, library, bridge, main, quickCapture, css] = await Promise.all([
  readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lifecycle.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/main.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/QuickCapture.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
]);

const config = JSON.parse(configSource);
const capability = JSON.parse(capabilitySource);
const quickWindow = config.app.windows.find((window) => window.label === "quick-capture");

assert.ok(quickWindow, "quick capture window must be declared");
assert.equal(quickWindow.url, "index.html?surface=quick-capture");
assert.equal(quickWindow.visible, false);
assert.equal(quickWindow.resizable, false);
assert.equal(quickWindow.decorations, false);
assert.equal(quickWindow.alwaysOnTop, true);
assert.equal(quickWindow.skipTaskbar, true);
assert.equal(quickWindow.dragDropEnabled, false);
assert.ok(capability.windows.includes("quick-capture"));

assert.match(lifecycle, /show_quick_capture_window/u);
assert.match(lifecycle, /position_quick_capture_window/u);
assert.match(lifecycle, /QUICK_CAPTURE_READY_EVENT/u);
assert.match(lifecycle, /QUICK_CAPTURE_WINDOW_LABEL[\s\S]*api\.prevent_close\(\)[\s\S]*window\.hide\(\)/u);
assert.doesNotMatch(
  lifecycle.match(/TrayAction::QuickCapture\s*=>\s*\{([\s\S]*?)\n\s*\}/u)?.[1] ?? "",
  /show_main_window/u,
  "menu bar capture must not reveal the main app",
);

assert.match(storage, /QUICK_CAPTURE_DRAFT_SURFACE/u);
assert.match(storage, /surface_draft_tab_key/u);
assert.match(storage, /quick_capture_drafts_are_isolated_from_main_window_drafts/u);
assert.match(storage, /quick_capture_create_attempts_do_not_collide_with_main_window_attempts/u);
assert.match(storage, /quick_capture_requires_a_connected_workspace/u);
assert.match(commands, /pub fn create_quick_capture_record[\s\S]*get_connected_workspace/u);

for (const command of [
  "load_quick_capture_draft",
  "save_quick_capture_draft",
  "create_quick_capture_record",
  "show_main_window",
  "hide_quick_capture_window",
  "acknowledge_quick_capture_quit",
]) {
  assert.match(commands, new RegExp(`#\\[tauri::command\\]\\s*pub fn ${command}\\b`, "u"));
  assert.match(library, new RegExp(`commands::${command}\\b`, "u"));
  assert.match(bridge, new RegExp(`"${command}"`, "u"));
}

assert.match(main, /get\("surface"\) === "quick-capture"/u);
assert.match(quickCapture, /wakeBridge\.loadQuickCaptureDraft/u);
assert.match(quickCapture, /wakeBridge\.saveQuickCaptureDraft/u);
assert.match(quickCapture, /wakeBridge\.createQuickCaptureRecord/u);
assert.match(quickCapture, /wakeBridge\.stageRecordImage/u);
assert.match(quickCapture, /input\.type = "file"/u);
assert.doesNotMatch(quickCapture, /wakeBridge\.(?:activateWorkspace|setActiveSelection)/u);
assert.match(quickCapture, /最近记录/u);
assert.match(quickCapture, /打开 WakeGPT App/u);
assert.match(quickCapture, /wakegpt:\/\/data-changed/u);
assert.match(quickCapture, /wakegpt:\/\/quit-requested/u);
assert.match(
  quickCapture,
  /persistDraft\([\s\S]*composerRef\.current\?\.value[\s\S]*acknowledgeQuickCaptureQuit/u,
  "interactive quit must persist the current quick-capture DOM value before acknowledgement",
);
assert.match(css, /\.quick-capture-shell/u);
assert.match(css, /\.quick-capture-scroll[^{]*\{[^}]*overflow-y:\s*auto/u);

console.log("independent quick capture surface: ok");
